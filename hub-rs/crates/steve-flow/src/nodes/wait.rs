//! `wait`: `{mode:"delay"|"until"|"event", delay:"30m", until:"next_workday 09:00"|ts,
//! match:{source,type,when}, timeout:"3d", timeout_label}`. Writes `flow_waits` and parks the run;
//! the scheduler wakes `delay`/`until` waits and event timeouts, [`crate::ingest`] hands matching
//! events to [`crate::engine::GraphEngine::resume_event`]. A timeout continues on the
//! `timeout_label` edge.

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::{Value, json};

use super::{NodeKind, NodeOutcome, RunCx, WaitSpec, opt_str};
use crate::cron::parse_tz;
use crate::expr;

pub struct Wait;

/// Longest wait or timeout.
pub const MAX_MS: i64 = 90 * 24 * 3_600_000;

/// `30m`, `2h`, `3d`, `1h30m`, `45s`, also `30 分钟`, `2 小时`, `3 天`. Milliseconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut unit = String::new();
    let mut parts: Vec<(i64, String)> = Vec::new();
    let flush =
        |num: &mut String, unit: &mut String, parts: &mut Vec<(i64, String)>| -> Option<()> {
            if num.is_empty() {
                return if unit.trim().is_empty() {
                    Some(())
                } else {
                    None
                };
            }
            parts.push((num.parse().ok()?, unit.trim().to_string()));
            num.clear();
            unit.clear();
            Some(())
        };
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !unit.trim().is_empty() {
                flush(&mut num, &mut unit, &mut parts)?;
            }
            num.push(c);
        } else if !c.is_whitespace() {
            if num.is_empty() {
                return None;
            }
            unit.push(c);
        }
    }
    flush(&mut num, &mut unit, &mut parts)?;
    if parts.is_empty() {
        return None;
    }
    for (n, u) in parts {
        let ms = match u.as_str() {
            "s" | "秒" => 1_000,
            "m" | "min" | "分" | "分钟" => 60_000,
            "h" | "小时" | "时" => 3_600_000,
            "d" | "天" | "日" => 86_400_000,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(ms)?)?;
    }
    (total > 0 && total <= MAX_MS).then_some(total)
}

fn hm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// `until` as a time: a timestamp, `HH:MM` (the next one), `tomorrow HH:MM`, `next_workday HH:MM`
/// (the next Monday–Friday after today), in `tz`.
pub fn parse_until(v: &Value, now: i64, tz: Tz) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    let s = v.as_str()?.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    let local = DateTime::<Utc>::from_timestamp_millis(now)?.with_timezone(&tz);
    let (day_offset, t): (Option<&str>, &str) = match s.split_once(char::is_whitespace) {
        Some((w, t)) => (Some(w), t),
        None => (None, s),
    };
    let t = hm(t)?;
    let today = local.date_naive();
    let date = match day_offset {
        None => {
            if local.time() < t {
                today
            } else {
                today.succ_opt()?
            }
        }
        Some("tomorrow" | "明天") => today.succ_opt()?,
        Some("next_workday" | "下个工作日") => {
            let mut d = today.succ_opt()?;
            while matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
                d = d.succ_opt()?;
            }
            d
        }
        Some(_) => return None,
    };
    tz.from_local_datetime(&date.and_time(t))
        .earliest()
        .map(|d| d.timestamp_millis())
}

fn check_until(v: &Value) -> bool {
    parse_until(v, 0, chrono_tz::UTC).is_some()
}

#[async_trait]
impl NodeKind for Wait {
    fn kind(&self) -> &'static str {
        "wait"
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        match opt_str(cfg, "mode")?.unwrap_or("") {
            "delay" => {
                parse_duration(opt_str(cfg, "delay")?.unwrap_or(""))
                    .ok_or("等待时长写得不对，例如 30m、2h、3d（最多 90 天）")?;
            }
            "until" => {
                if !check_until(&cfg["until"]) {
                    return Err("等到的时间写得不对，例如 next_workday 09:00".into());
                }
            }
            "event" => {
                let m = &cfg["match"];
                let source = m["source"].as_str().unwrap_or("");
                let has_type = match &m["type"] {
                    Value::String(t) => !t.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    _ => false,
                };
                if source.is_empty() && !has_type {
                    return Err("等事件要写清来源或事件类型".into());
                }
                if let Some(w) = m["when"].as_str().filter(|w| !w.trim().is_empty()) {
                    expr::parse(w).map_err(|e| format!("等事件的条件：{e}"))?;
                }
            }
            _ => return Err("mode 只能是 delay、until 或 event".into()),
        }
        if let Some(t) = opt_str(cfg, "timeout")?.filter(|t| !t.trim().is_empty()) {
            parse_duration(t).ok_or("超时写得不对，例如 3d")?;
        }
        opt_str(cfg, "timeout_label")?;
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let now = cx.now();
        let mode = cfg["mode"].as_str().unwrap_or("");
        let timeout = cfg["timeout"].as_str().and_then(parse_duration);
        let (mode, until) = match mode {
            "delay" => match cfg["delay"].as_str().and_then(parse_duration) {
                Some(ms) => ("delay", Some(now + ms)),
                None => return NodeOutcome::Fail("等待时长写得不对".into()),
            },
            "until" => {
                let ws = cx.ws().to_string();
                let tz = cx
                    .flow
                    .store()
                    .reader(|c| crate::schedules::workspace_tz(c, &ws))
                    .ok()
                    .and_then(|t| parse_tz(&t).ok())
                    .unwrap_or(chrono_tz::Asia::Taipei);
                match parse_until(&cx.render_value(&cfg["until"], input), now, tz) {
                    Some(t) => ("until", Some(t.max(now))),
                    None => return NodeOutcome::Fail("等到的时间写得不对".into()),
                }
            }
            _ => ("event", timeout.map(|t| now + t)),
        };
        let mut matcher = cfg.get("match").cloned().unwrap_or(Value::Null);
        if mode == "event" && matcher.is_object() {
            matcher["timeout_label"] = cfg.get("timeout_label").cloned().unwrap_or(Value::Null);
        }
        let waiting = match mode {
            "event" => {
                let t = &cfg["match"]["type"];
                if t.is_null() {
                    cfg["match"]["source"].clone()
                } else {
                    t.clone()
                }
            }
            m => json!(m),
        };
        NodeOutcome::Wait(Box::new(WaitSpec {
            mode,
            until,
            matcher,
            decision: None,
            output: json!({ "waiting": waiting, "until": until }),
        }))
    }
}

/// The edge a finished wait continues on: `None` (normal) or the timeout label.
pub fn timeout_branch(matcher: &Value, cfg: &Value) -> Option<String> {
    matcher["timeout_label"]
        .as_str()
        .or_else(|| cfg["timeout_label"].as_str())
        .filter(|l| !l.is_empty())
        .map(String::from)
}

/// The first edge of a wait that is not its timeout edge (dry runs pass straight through).
pub fn pass_label(g: &crate::graph::Graph, node: &crate::graph::Node) -> Option<String> {
    let t = node.config["timeout_label"].as_str().unwrap_or("");
    g.outs(&node.id)
        .find(|e| t.is_empty() || e.label != t)
        .map(|e| e.label.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m"), Some(30 * 60_000));
        assert_eq!(parse_duration("1h30m"), Some(90 * 60_000));
        assert_eq!(parse_duration("3d"), Some(3 * 86_400_000));
        assert_eq!(parse_duration("30 分钟"), Some(30 * 60_000));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("100d"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn until() {
        let tz = chrono_tz::Asia::Taipei;
        // Friday 2026-10-30 10:00 Taipei
        let fri = tz
            .with_ymd_and_hms(2026, 10, 30, 10, 0, 0)
            .unwrap()
            .timestamp_millis();
        let mon9 = tz
            .with_ymd_and_hms(2026, 11, 2, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            parse_until(&json!("next_workday 09:00"), fri, tz),
            Some(mon9)
        );
        let fri18 = tz
            .with_ymd_and_hms(2026, 10, 30, 18, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(parse_until(&json!("18:00"), fri, tz), Some(fri18));
        assert_eq!(parse_until(&json!(5), fri, tz), Some(5));
        assert_eq!(parse_until(&json!("someday"), fri, tz), None);
    }
}

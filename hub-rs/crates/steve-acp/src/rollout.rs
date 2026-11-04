//! Codex token usage from its rollout file. codex-acp 0.16 answers `session/prompt` with a bare
//! `{stopReason}` and its `usage_update`s carry only the context size, so a Codex turn reached the
//! statistics without tokens (ST-21, 2026-11-02). Codex itself writes every model response's
//! `token_count` (running totals, cached input included) to
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<session id>.jsonl`; the turn's share is the
//! difference of those totals across the turn.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

/// Rollout path per Codex session id, so the directory walk happens once per session.
static PATHS: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);

/// Token figures of one `token_count` event, OpenAI convention (`input` includes `cached`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    input: i64,
    cached: i64,
    output: i64,
    reasoning: i64,
}

impl Counts {
    fn of(v: &Value) -> Option<Counts> {
        let n = |k: &str| v.get(k).and_then(Value::as_i64);
        Some(Counts {
            input: n("input_tokens")?,
            cached: n("cached_input_tokens").unwrap_or(0),
            output: n("output_tokens").unwrap_or(0),
            reasoning: n("reasoning_output_tokens").unwrap_or(0),
        })
    }
    fn minus(self, o: Counts) -> Option<Counts> {
        let c = Counts {
            input: self.input - o.input,
            cached: self.cached - o.cached,
            output: self.output - o.output,
            reasoning: self.reasoning - o.reasoning,
        };
        (c.input >= 0 && c.cached >= 0 && c.output >= 0 && c.reasoning >= 0).then_some(c)
    }
    fn plus(self, o: Counts) -> Counts {
        Counts {
            input: self.input + o.input,
            cached: self.cached + o.cached,
            output: self.output + o.output,
            reasoning: self.reasoning + o.reasoning,
        }
    }
}

/// One `token_count` event: when, the session's running total, and this response's own figures.
#[derive(Debug, Clone, Copy)]
struct Tick {
    ms: i64,
    total: Counts,
    last: Option<Counts>,
}

/// Milliseconds since the epoch of an RFC 3339 UTC time as Codex writes it (`2026-11-01T16:59:20.548Z`).
fn parse_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.splitn(3, ':').map(|x| x.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    let digits: String = frac.chars().take(3).collect();
    let ms = format!("{digits:0<3}").parse::<i64>().ok()?;
    // days from civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
}

fn ticks(text: &str) -> Vec<Tick> {
    text.lines()
        .filter(|l| l.contains("\"token_count\""))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let p = v.get("payload")?;
            if p.get("type").and_then(Value::as_str) != Some("token_count") {
                return None;
            }
            let info = p.get("info").filter(|i| i.is_object())?;
            Some(Tick {
                ms: parse_ms(v.get("timestamp")?.as_str()?)?,
                total: Counts::of(info.get("total_token_usage")?)?,
                last: info.get("last_token_usage").and_then(Counts::of),
            })
        })
        .collect()
}

/// The tokens of the responses between `started` and `ended` (ms): the change of the running total,
/// or the sum of the per-response figures when the total went backwards (a resumed session).
fn window(ticks: &[Tick], started: i64, ended: i64) -> Option<Counts> {
    let inside: Vec<&Tick> = ticks
        .iter()
        .filter(|t| t.ms >= started && t.ms <= ended)
        .collect();
    let last = inside.last()?;
    let before = ticks
        .iter()
        .rev()
        .find(|t| t.ms < started)
        .map_or(Counts::default(), |t| t.total);
    last.total.minus(before).or_else(|| {
        Some(
            inside
                .iter()
                .filter_map(|t| t.last)
                .fold(Counts::default(), Counts::plus),
        )
    })
}

/// The rollout file of a Codex session, newest day first; remembered once found.
pub fn find(codex_home: &Path, session_id: &str) -> Option<PathBuf> {
    if let Some(p) = PATHS
        .lock()
        .ok()?
        .as_ref()
        .and_then(|m| m.get(session_id))
        .filter(|p| p.exists())
    {
        return Some(p.clone());
    }
    let suffix = format!("-{session_id}.jsonl");
    // newest day first: a session's file sits in the directory of the day it began
    let sorted = |p: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(p)
            .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        v.sort();
        v.reverse();
        v
    };
    let found = sorted(&codex_home.join("sessions"))
        .into_iter()
        .flat_map(|y| sorted(&y))
        .flat_map(|m| sorted(&m))
        .flat_map(|d| sorted(&d))
        .find(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix))
        })?;
    if let Ok(mut m) = PATHS.lock() {
        m.get_or_insert_with(HashMap::new)
            .insert(session_id.to_owned(), found.clone());
    }
    Some(found)
}

/// The turn's tokens as a usage object in Codex's own field names (read by `steve_stats::RawUsage`),
/// or `None` when the session has no rollout or no response in the window. The file is written
/// asynchronously, so a turn whose last response is not there yet is read again for up to `wait`.
pub async fn turn_usage(
    codex_home: &Path,
    session_id: &str,
    started: i64,
    ended: i64,
    wait: Duration,
) -> Option<Value> {
    let home = codex_home.to_path_buf();
    let sid = session_id.to_owned();
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let (home, sid) = (home.clone(), sid.clone());
        // a response that ended the turn is logged at most a moment after the prompt answered
        let read = tokio::task::spawn_blocking(move || {
            let path = find(&home, &sid)?;
            let ticks = ticks(&fs::read_to_string(path).ok()?);
            let done = ticks.iter().any(|t| t.ms >= ended - 2_000);
            Some((window(&ticks, started, ended + 2_000), done))
        })
        .await
        .ok()
        .flatten();
        match read {
            Some((c, true)) => return c.map(to_json),
            Some((c, false)) if tokio::time::Instant::now() >= deadline => return c.map(to_json),
            Some(_) => tokio::time::sleep(Duration::from_millis(250)).await,
            // no rollout: not a Codex that keeps one (or another CODEX_HOME)
            None => return None,
        }
    }
}

fn to_json(c: Counts) -> Value {
    json!({
        "input_tokens": c.input,
        "cached_input_tokens": c.cached,
        "output_tokens": c.output,
        "reasoning_output_tokens": c.reasoning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ts: &str, total: (i64, i64, i64), last: (i64, i64, i64)) -> String {
        json!({"timestamp": ts, "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": total.0, "cached_input_tokens": total.1, "output_tokens": total.2, "reasoning_output_tokens": 0, "total_tokens": 0},
            "last_token_usage": {"input_tokens": last.0, "cached_input_tokens": last.1, "output_tokens": last.2, "reasoning_output_tokens": 0, "total_tokens": 0},
            "model_context_window": 475000}}})
        .to_string()
    }

    #[test]
    fn rfc3339_to_ms() {
        assert_eq!(parse_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_ms("2026-11-01T16:59:20.548Z"),
            Some(1_793_552_360_548)
        );
        assert_eq!(parse_ms("2026-11-01T16:59:20Z"), Some(1_793_552_360_000));
        assert_eq!(parse_ms("2026-11-01T16:59:20.5Z"), Some(1_793_552_360_500));
        assert_eq!(parse_ms("garbage"), None);
    }

    #[tokio::test]
    async fn a_turn_is_the_change_of_the_running_total() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/11/02");
        fs::create_dir_all(&day).unwrap();
        let sid = "01a1980d-22b8-7d33-82c7-873835c051e3";
        let text = [
            r#"{"timestamp":"2026-11-01T16:59:20.548Z","type":"session_meta","payload":{"id":"x"}}"#.to_string(),
            line("2026-11-01T16:59:29.360Z", (16143, 0, 100), (16143, 0, 100)),
            line("2026-11-01T17:00:00.348Z", (82432, 60000, 500), (16885, 16000, 400)),
            // second turn: two responses
            line("2026-11-01T17:00:27.866Z", (99363, 76000, 600), (16931, 16000, 100)),
            line("2026-11-01T17:00:56.801Z", (168666, 140000, 900), (17000, 16900, 300)),
            "not json".to_string(),
        ]
        .join("\n");
        fs::write(
            day.join(format!("rollout-2026-11-02T00-59-20-{sid}.jsonl")),
            text,
        )
        .unwrap();
        let ms = |s| parse_ms(s).unwrap();
        let u = turn_usage(
            home.path(),
            sid,
            ms("2026-11-01T17:00:20.579Z"),
            ms("2026-11-01T17:00:56.804Z"),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(
            u,
            json!({"input_tokens": 86234, "cached_input_tokens": 80000, "output_tokens": 400, "reasoning_output_tokens": 0})
        );
        // the first turn: nothing before it
        let u = turn_usage(
            home.path(),
            sid,
            ms("2026-11-01T16:59:20.489Z"),
            ms("2026-11-01T17:00:00.401Z"),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(u["input_tokens"], 82432);
        assert_eq!(u["cached_input_tokens"], 60000);
        // a window without responses, and an unknown session
        assert_eq!(
            turn_usage(
                home.path(),
                sid,
                ms("2026-11-01T18:00:00.000Z"),
                ms("2026-11-01T18:01:00.000Z"),
                Duration::ZERO
            )
            .await,
            None
        );
        assert_eq!(
            turn_usage(home.path(), "nope", 0, i64::MAX / 2, Duration::ZERO).await,
            None
        );
    }

    #[test]
    fn a_total_that_went_backwards_sums_the_responses() {
        let t = |ms, total: i64, last: i64| Tick {
            ms,
            total: Counts {
                input: total,
                cached: 0,
                output: 0,
                reasoning: 0,
            },
            last: Some(Counts {
                input: last,
                cached: 0,
                output: 0,
                reasoning: 0,
            }),
        };
        let ticks = [t(10, 500, 500), t(30, 100, 100), t(40, 250, 150)];
        assert_eq!(window(&ticks, 20, 50).map(|c| c.input), Some(250));
    }
}

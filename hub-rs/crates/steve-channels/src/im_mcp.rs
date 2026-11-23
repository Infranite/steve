//! The MCP tool `steve_im_read`: an agent reads a chat conversation's messages when its task needs
//! them, choosing the conversation and the time range ([`Channels::read_history`]). Visibility is
//! the session's user's, as for knowledge ([`steve_knowledge::tools::session_viewer`]); the answer
//! carries the space's disclosure level and the same level legend as `steve_knowledge_search`, a
//! space at `never` or set to 屏蔽 is not read, and the answer is at most [`READ_MAX`] characters.
//! It is context only: nothing is stored, no rule, flow or action runs. It does not end the turn.
//!
//! Serving it is the Hub's (`mcp_extra`), like the other modules' tools: list [`tools`] and route
//! names for which [`handles`] is true to [`call`]. Tool descriptions are always English; errors follow the request language and messages
//! remain as they were written.

use chrono::TimeZone;
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_store::base::Viewer;

use crate::history::{self, Page, Source};
use crate::{ChannelResult, Channels, spaces};

pub const READ: &str = "steve_im_read";
/// Characters of one answer.
pub const READ_MAX: usize = 20_000;
/// Characters of one message in an answer.
pub const LINE_MAX: usize = 2_000;
/// Candidates named when a space name is ambiguous or unknown.
const CANDIDATES: usize = 5;

/// Model-facing definitions stay English, independent of the user interface language.
pub fn tools() -> Vec<Value> {
    steve_i18n::with(steve_i18n::EN, || {
        vec![json!({
            "name": READ,
            "description": steve_i18n::t!("channels.im_read.description"),
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "space": { "type": "string", "description": steve_i18n::t!("channels.im_read.arg.space") },
                    "from": { "type": ["string", "number"], "description": steve_i18n::t!("channels.im_read.arg.from") },
                    "to": { "type": ["string", "number"], "description": steve_i18n::t!("channels.im_read.arg.to") },
                    "limit": { "type": "number", "description": steve_i18n::t!("channels.im_read.arg.limit", max = history::MAX_LIMIT, n = history::DEFAULT_LIMIT) }
                },
                "required": ["space"]
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "openWorldHint": true }
        })]
    })
}

/// Is `name` this module's tool?
pub fn handles(name: &str) -> bool {
    name == READ
}

/// Runs the tool. `Ok` is the result text, `Err` the `isError` text.
pub async fn call(ch: &Channels, cx: &McpCx, name: &str, args: &Value) -> Result<String, String> {
    if name != READ {
        return Err(format!("MCP error -32602: Tool {name} not found"));
    }
    let r = read(ch, cx, args).await.map_err(|e| e.to_string());
    if let Err(e) = &r {
        tracing::warn!(tool = name, error = %e, "im tool failed");
    }
    r
}

/// The workspace's time zone.
pub(crate) fn tz_of(r: &mut impl steve_store::Read, ws: &str) -> chrono_tz::Tz {
    steve_modules::zone::workspace_tz(r, ws).unwrap_or_else(|_| steve_modules::zone::default_tz())
}

/// A time argument: unix ms (or seconds), RFC 3339, or `YYYY-MM-DD[ HH:MM[:SS]]` in the
/// workspace's time zone.
pub fn parse_time(v: &Value, tz: chrono_tz::Tz) -> Option<i64> {
    match v {
        Value::Number(n) => {
            let x = n.as_f64()? as i64;
            Some(if x.abs() < 100_000_000_000 {
                x * 1000
            } else {
                x
            })
        }
        Value::String(s) => {
            let s = s.trim();
            if let Ok(x) = s.parse::<i64>() {
                return parse_time(&json!(x), tz);
            }
            if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
                return Some(d.timestamp_millis());
            }
            let naive = [
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%d %H:%M",
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%dT%H:%M",
            ]
            .iter()
            .find_map(|f| chrono::NaiveDateTime::parse_from_str(s, f).ok())
            .or_else(|| {
                chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
            })?;
            steve_runtime::tz::Zone::parse(tz.name())
                .ok()?
                .instant(naive)
                .ok()
        }
        _ => None,
    }
}

pub(crate) fn shown(at: i64, tz: chrono_tz::Tz) -> String {
    tz.timestamp_millis_opt(at)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| at.to_string())
}

/// `space`: a space id, else the one space the viewer sees with exactly this name.
fn resolve(r: &mut impl steve_store::Read, viewer: &Viewer, q: &str) -> ChannelResult<String> {
    let q = q.trim();
    if q.is_empty() {
        return Err(steve_modules::ApiError::rule(steve_i18n::t!(
            "channels.im_read.error.no_space_arg"
        )));
    }
    if steve_store::channel_spaces::rows::get_in_workspace(r, &viewer.ws, q)?.is_some() {
        return Ok(q.to_string());
    }
    let found = spaces::list(r, viewer, None, Some(q))?;
    let key = q.to_lowercase();
    let mut exact = Vec::new();
    for s in &found {
        if s.name.trim().to_lowercase() == key
            || spaces::display_title(r, s)?.trim().to_lowercase() == key
        {
            exact.push(s);
        }
    }
    if exact.len() == 1 {
        return Ok(exact[0].id.clone());
    }
    let pool: Vec<_> = if exact.is_empty() {
        found.iter().collect()
    } else {
        exact
    };
    let names: Vec<String> = pool
        .iter()
        .take(CANDIDATES)
        .map(|s| Ok(format!("{}（{}）", spaces::display_title(r, s)?, s.id)))
        .collect::<ChannelResult<_>>()?;
    Err(steve_modules::ApiError::rule(if names.is_empty() {
        steve_i18n::t!("channels.im_read.error.no_space", space = q)
    } else {
        steve_i18n::t!(
            "channels.im_read.error.which_space",
            space = q,
            candidates = names.join("、")
        )
    }))
}

async fn read(ch: &Channels, cx: &McpCx, args: &Value) -> ChannelResult<String> {
    let bad = |k: &str| {
        steve_modules::ApiError::rule(steve_i18n::t!("channels.im_read.error.arg", arg = k))
    };
    let space_q = match args.get("space") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(bad("space")),
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => history::DEFAULT_LIMIT,
        Some(v) => v
            .as_f64()
            .filter(|x| *x >= 1.0)
            .map(|x| x as usize)
            .ok_or_else(|| bad("limit"))?,
    };
    let viewer = steve_knowledge::tools::session_viewer(ch.modules(), cx)?;
    let now = ch.now();
    let (space, tz) = ch.store().read(|r| -> ChannelResult<_> {
        Ok((resolve(r, &viewer, &space_q)?, tz_of(r, &viewer.ws)))
    })?;
    let time = |k: &str| -> ChannelResult<Option<i64>> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => parse_time(v, tz).map(Some).ok_or_else(|| bad(k)),
        }
    };
    let to = time("to")?.unwrap_or(now);
    let from = time("from")?.unwrap_or(to - history::DEFAULT_RANGE_MS);
    let page = ch.read_history(&viewer, &space, from, to, limit).await?;
    Ok(render(&page, tz))
}

/// The answer: a header (space, range, level, where it came from), one line per message, whether
/// there is more, and the level legend; at most [`READ_MAX`] characters.
pub fn render(p: &Page, tz: chrono_tz::Tz) -> String {
    let source = match p.source {
        Source::Remote => "平台实时读取",
        Source::Stored => "Steve 已存的消息",
    };
    let mut out = format!(
        "「{}」（{}）· {} – {} · level: {} · 来源：{} · {} 条\n仅作上下文：这些消息不会被回复，也不会触发规则或流程。\n",
        p.space_name,
        p.space_id,
        shown(p.from, tz),
        shown(p.to, tz),
        p.level.code(),
        source,
        p.lines.len()
    );
    let mut cut: Option<i64> = None;
    for l in &p.lines {
        let who = if l.out {
            format!("{}（Steve 发出）", l.sender)
        } else if l.sender.trim().is_empty() {
            "某人".to_string()
        } else {
            l.sender.clone()
        };
        let mut text = match &l.text {
            None => "（原文已过保留期限）".to_string(),
            Some(t) => t.split_whitespace().collect::<Vec<_>>().join(" "),
        };
        if text.chars().count() > LINE_MAX {
            text = text.chars().take(LINE_MAX).collect::<String>() + "…";
        }
        for k in &l.attachments {
            text.push_str(match k.as_str() {
                "image" => " [图片]",
                _ => " [文件]",
            });
        }
        let mark = match l.history.as_deref() {
            Some(_) => "（接入时的旧消息）",
            None => "",
        };
        let line = format!("[{}] {who}{mark}：{}\n", shown(l.at, tz), text.trim());
        if out.chars().count() + line.chars().count() > READ_MAX {
            cut = Some(l.at);
            break;
        }
        out.push_str(&line);
    }
    match (cut, p.more) {
        (Some(at), _) => out.push_str(&format!(
            "（输出已到上限，还有更多：用 from={at} 接着读）\n"
        )),
        (None, true) => {
            let next = p.lines.last().map_or(p.to, |l| l.at + 1);
            out.push_str(&format!("（还有更多：用 from={next} 接着读）\n"));
        }
        (None, false) => {}
    }
    out.push('\n');
    out.push_str(steve_knowledge::tools::LEVELS);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_dst_table() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
        ] {
            let z = steve_runtime::tz::Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let ms = z.instant(wall).unwrap();
            assert_eq!(
                parse_time(&serde_json::json!(text), name.parse().unwrap()),
                Some(ms)
            );
            let mut f = crate::rules::Facts::default();
            f.set_time(ms, name);
            let local = z.local(ms).unwrap();
            assert_eq!(
                f.minute,
                chrono::Timelike::hour(&local) * 60 + chrono::Timelike::minute(&local)
            );
        }
    }

    use super::*;

    #[test]
    fn times_parse_in_the_workspace_zone() {
        let tz = "Asia/Shanghai".parse().expect("tz");
        assert_eq!(
            parse_time(&json!(1_762_400_000_000_i64), tz),
            Some(1_762_400_000_000)
        );
        assert_eq!(
            parse_time(&json!(1_762_400_000), tz),
            Some(1_762_400_000_000)
        );
        assert_eq!(
            parse_time(&json!("1762400000000"), tz),
            Some(1_762_400_000_000)
        );
        let local = parse_time(&json!("2026-11-06 10:07"), tz).expect("local");
        assert_eq!(parse_time(&json!("2026-11-06T02:07:00Z"), tz), Some(local));
        assert_eq!(
            parse_time(&json!("2026-11-06"), tz),
            parse_time(&json!("2026-11-06 00:00"), tz)
        );
        assert_eq!(parse_time(&json!("昨天"), tz), None);
        assert_eq!(parse_time(&json!(true), tz), None);
    }

    #[test]
    fn the_answer_is_capped_and_says_how_to_go_on() {
        let tz = chrono_tz::UTC;
        let line = |i: i64| history::Line {
            at: 1_000 + i,
            sender: "王小丽".into(),
            out: false,
            text: Some("长".repeat(1_500)),
            attachments: vec![],
            history: None,
        };
        let p = Page {
            space_id: "spc_1".into(),
            space_name: "运维群".into(),
            level: steve_store::base::Level::Conclusion,
            from: 0,
            to: 10_000,
            source: Source::Stored,
            lines: (0..40).map(line).collect(),
            more: false,
        };
        let out = render(&p, tz);
        assert!(
            out.chars().count() <= READ_MAX + 400,
            "{}",
            out.chars().count()
        );
        assert!(out.contains("level: conclusion"));
        assert!(out.contains("用 from=10"), "{out}");
        assert!(out.ends_with(steve_knowledge::tools::LEVELS));
    }
}

//! Back to a trigger word while its item runs: an item handed off from your trigger word (an
//! answer said as you, [`crate::command_skill`]) reports to the message it came from without the
//! agent having to remember to. A failure is said at once (the same reason at most every
//! [`ERROR_REPEAT_MS`], as its runs are retried); what the item is doing now is said when it
//! changed and [`PROGRESS_EVERY_MS`] passed since the last report. Its end says nothing: the
//! message's reaction turns to done (`react_item_ended`). All of it goes out as you, `[AI] `-marked, where the trigger's
//! settings say; the reaction stays until the end. What was said last is kept per item in
//! `settings` (`trigger.report.<item>`).

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::deliver::{Auth, OutRequest, Purpose};
use crate::outbound::Outbox;
use crate::{ChannelResult, Channels};

/// How often open items are looked at.
pub const REPORT_EVERY: Duration = Duration::from_secs(60);
/// The least time between two progress reports of one item.
pub const PROGRESS_EVERY_MS: i64 = 30 * 60_000;
/// The least time before the same failure is said again.
pub const ERROR_REPEAT_MS: i64 = 30 * 60_000;

/// An open item handed off from a trigger word.
#[derive(Debug, Clone, PartialEq)]
struct Open {
    ws: String,
    item: String,
    /// The chat message it came from.
    message: String,
    /// What it is doing now (`items.now`).
    now: String,
}

/// What was said of an item last.
#[derive(Debug, Clone, Default, PartialEq)]
struct Said {
    /// The last failure looked at (`diagnostic_events.seq`).
    diag_seq: i64,
    error: String,
    error_at: i64,
    now: String,
    at: i64,
}

impl Said {
    fn from(v: &Value) -> Self {
        Self {
            diag_seq: v["diag_seq"].as_i64().unwrap_or(0),
            error: v["error"].as_str().unwrap_or("").to_string(),
            error_at: v["error_at"].as_i64().unwrap_or(0),
            now: v["now"].as_str().unwrap_or("").to_string(),
            at: v["at"].as_i64().unwrap_or(0),
        }
    }

    fn to_value(&self) -> Value {
        json!({"diag_seq": self.diag_seq, "error": self.error, "error_at": self.error_at, "now": self.now, "at": self.at})
    }
}

fn key(item: &str) -> String {
    format!("trigger.report.{item}")
}

/// Open items made from a chat message whose trigger word was answered as you.
fn open_items(c: &Connection) -> ChannelResult<Vec<Open>> {
    let mut st = c.prepare(
        "SELECT i.workspace_id, i.id, l.object_id, coalesce(i.now, '') FROM items i
           JOIN links l ON l.workspace_id = i.workspace_id AND l.subject_type = 'item' AND l.subject_id = i.id
                       AND l.predicate = 'from' AND l.object_type = 'chan_message'
          WHERE i.status NOT IN ('done', 'drop')
            AND EXISTS (SELECT 1 FROM messages t WHERE t.workspace_id = i.workspace_id
                          AND json_extract(t.meta, '$.chan.message') = l.object_id
                          AND json_extract(t.meta, '$.chan.command') = 1
                          AND json_extract(t.meta, '$.chan.mode') = 'self')",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(Open {
                ws: r.get(0)?,
                item: r.get(1)?,
                message: r.get(2)?,
                now: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn said(c: &Connection, ws: &str, item: &str) -> ChannelResult<Option<Said>> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
            params![ws, key(item)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| Said::from(&v)))
}

fn keep(c: &Connection, ws: &str, item: &str, s: &Said, now: i64) -> ChannelResult<()> {
    c.execute(
        "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT (workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
        params![ws, key(item), s.to_value().to_string(), now],
    )?;
    Ok(())
}

/// The newest failure of the item after `after`: `(seq, what went wrong)`, the reason as its
/// session said it (`events`, kind `system`), else the failure's kind.
fn failure(c: &Connection, item: &str, after: i64) -> ChannelResult<Option<(i64, String)>> {
    let f: Option<(i64, Option<String>, String)> = c
        .query_row(
            "SELECT seq, session_id, kind FROM diagnostic_events
              WHERE item_id = ? AND severity = 'error' AND seq > ? ORDER BY seq DESC LIMIT 1",
            params![item, after],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((seq, session, kind)) = f else {
        return Ok(None);
    };
    let reason: Option<String> = match &session {
        Some(s) => c
            .query_row(
                "SELECT json_extract(data, '$.text') FROM events WHERE session_id = ? AND kind = 'system'
                  ORDER BY seq DESC LIMIT 1",
                [s],
                |r| r.get(0),
            )
            .optional()?
            .flatten(),
        None => None,
    };
    let reason = reason
        .map(|r| r.trim().trim_end_matches('。').to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| format!("执行出错（{kind}）"));
    Ok(Some((seq, reason)))
}

/// What to say of `o` now, given what was said last; the new record either way.
fn next(
    o: &Open,
    last: Option<Said>,
    fail: Option<(i64, String)>,
    now: i64,
) -> (Option<String>, Said) {
    // first look: what it is doing now is the starting point, nothing is said of it yet
    let mut s = last.clone().unwrap_or(Said {
        now: o.now.clone(),
        at: now,
        ..Said::default()
    });
    if let Some((seq, reason)) = fail {
        s.diag_seq = seq;
        let repeat = reason == s.error && now - s.error_at < ERROR_REPEAT_MS;
        if !repeat {
            s.error = reason.clone();
            s.error_at = now;
            return (
                Some(format!("{} 执行遇到问题：{reason}，正在重试。", o.item)),
                s,
            );
        }
    }
    let changed = !o.now.trim().is_empty() && o.now != s.now;
    if last.is_some() && changed && now - s.at >= PROGRESS_EVERY_MS {
        s.now = o.now.clone();
        s.at = now;
        return (Some(format!("{} 进展：{}", o.item, o.now.trim())), s);
    }
    (None, s)
}

/// One look at every open item handed off from a trigger word.
pub async fn sweep(ch: &Channels, outbox: &Outbox) -> ChannelResult<usize> {
    let now = ch.now();
    let open = ch.store().reader(open_items)?;
    let mut n = 0;
    for o in open {
        let (say, s) = ch.store().reader(|c| -> ChannelResult<_> {
            let last = said(c, &o.ws, &o.item)?;
            let after = last.as_ref().map_or(0, |l| l.diag_seq);
            let fail = failure(c, &o.item, after)?;
            Ok(next(&o, last, fail, now))
        })?;
        ch.store()
            .transaction(|tx| keep(tx, &o.ws, &o.item, &s, now))?;
        let Some(text) = say else { continue };
        let plan = ch
            .store()
            .reader(|c| crate::command_skill::trigger_plan(c, &o.ws, &o.message))?;
        let Some(plan) = plan else { continue };
        let req = OutRequest {
            target: crate::command_skill::target(ch, &plan)?,
            text,
            // not the end: the reaction stays
            purpose: Purpose::Source,
            approve: false,
            generation: None,
            auth: Auth::Trigger {
                source: plan.message_id.clone(),
            },
        };
        match outbox.deliver_checked(ch, &req).await {
            Ok((d, _)) => {
                ch.after_trigger_send(&o.ws, &plan.connection_id, &d);
                n += 1;
            }
            Err(e) => tracing::warn!(item = o.item, "trigger report not sent: {e}"),
        }
    }
    Ok(n)
}

/// The job: [`sweep`] every [`REPORT_EVERY`].
pub async fn run(ch: Channels, outbox: Arc<Outbox>) {
    loop {
        if let Err(e) = sweep(&ch, &outbox).await {
            tracing::warn!("trigger reports: {e}");
        }
        tokio::time::sleep(REPORT_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(now: &str) -> Open {
        Open {
            ws: "w".into(),
            item: "ST-1".into(),
            message: "cm".into(),
            now: now.into(),
        }
    }

    #[test]
    fn a_failure_is_said_at_once_and_not_again_while_it_repeats() {
        let fail = Some((
            7,
            "Node 没能打开这个会话（Authentication required）".to_string(),
        ));
        let (say, s) = next(&open("开始"), None, fail.clone(), 1_000);
        assert_eq!(
            say.as_deref(),
            Some("ST-1 执行遇到问题：Node 没能打开这个会话（Authentication required），正在重试。")
        );
        // retried: the same reason again soon is not said
        let (say, s) = next(
            &open("开始"),
            Some(s),
            Some((9, fail.clone().unwrap().1)),
            60_000,
        );
        assert_eq!((say, s.diag_seq), (None, 9));
        // much later, or another reason: said
        let (say, _) = next(
            &open("开始"),
            Some(s.clone()),
            Some((10, fail.unwrap().1)),
            1_000 + ERROR_REPEAT_MS,
        );
        assert!(say.is_some());
        let (say, _) = next(
            &open("开始"),
            Some(s),
            Some((11, "别的问题".into())),
            70_000,
        );
        assert!(say.unwrap().contains("别的问题"));
    }

    #[test]
    fn progress_is_said_when_it_changed_and_enough_time_passed() {
        let (say, s) = next(&open("开始"), None, None, 0);
        assert_eq!(say, None, "the first look is the starting point");
        let (say, s) = next(&open("在查日志"), Some(s), None, PROGRESS_EVERY_MS - 1);
        assert_eq!(say, None, "too soon");
        let (say, s) = next(&open("在查日志"), Some(s), None, PROGRESS_EVERY_MS);
        assert_eq!(say.as_deref(), Some("ST-1 进展：在查日志"));
        let (say, _) = next(&open("在查日志"), Some(s), None, 3 * PROGRESS_EVERY_MS);
        assert_eq!(say, None, "nothing new");
    }
}

//! The one-off backfill: turns, session-level costs and item marks reconstructed from the
//! tables that existed before migration 15 (`events`, `sessions`, `items`, `audit`, `timeline`,
//! `decisions`). Every row it writes is marked (`backfill = 1`, or `source = 'backfill'` for token
//! records), so a run first deletes the previous run's rows: running it again gives the same result.
//!
//! Live rows win: for a session with live turns only the events before its first live turn are read,
//! and for an item with live marks only what happened before its first live mark.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use steve_store::{Result, Store, fail_reason, now, uid};

use crate::refresh::{flag_workspace, rebuild_in, refresh_all, tz_of};

/// The setting that records that the backfill ran (`stats.backfill`).
pub const KEY: &str = "stats.backfill";
/// The current backfill version; a different stored value runs it again (v2: unfinished turns end at
/// the session's last event, not at the restart or `sessions.updated`).
pub const VERSION: &str = "v2";

/// What one workspace's backfill wrote.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub turns: usize,
    pub usage: usize,
    pub marks: usize,
}

/// Backfills every workspace not yet at [`VERSION`] (all of them with `force`), rebuilds their
/// `stat_daily` and recomputes `item_stats`. Returns what was written per workspace.
pub fn run(store: &Store, force: bool) -> Result<BTreeMap<String, Report>> {
    let wss: Vec<String> = store.reader(|c| {
        Ok::<_, steve_store::StoreError>(
            c.prepare("SELECT id FROM workspaces ORDER BY id")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?,
        )
    })?;
    let mut out = BTreeMap::new();
    for ws in wss {
        let done = store.reader(|c| {
            Ok::<_, steve_store::StoreError>(
                c.query_row(
                    "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
                    params![ws, KEY],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .and_then(|v| serde_json::from_str::<String>(&v).ok()),
            )
        })?;
        if !force && done.as_deref() == Some(VERSION) {
            continue;
        }
        let r = store.transaction(|c| {
            let r = backfill_in(c, &ws)?;
            rebuild_in(c, &ws)?;
            c.execute(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)
                 ON CONFLICT (workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                params![ws, KEY, json!(VERSION).to_string(), now()],
            )?;
            Ok::<_, steve_store::StoreError>(r)
        })?;
        tracing::info!(
            "stats backfill {ws}: {} turns, {} usage rows, {} marks",
            r.turns,
            r.usage,
            r.marks
        );
        out.insert(ws, r);
    }
    refresh_all(store)?;
    for ws in out.keys() {
        store.publish(steve_store::stats_changed(ws, None));
    }
    Ok(out)
}

/// Starts [`run`] on a blocking thread (Hub boot); errors are logged.
pub fn spawn(store: Store) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = run(&store, false) {
            tracing::error!("stats backfill {e}");
        }
    })
}

/// Deletes the previous backfill of `ws` and writes it again (no `stat_daily`, no `item_stats`).
pub fn backfill_in(c: &Connection, ws: &str) -> Result<Report> {
    c.execute(
        "DELETE FROM agent_turns WHERE workspace_id = ? AND backfill = 1",
        [ws],
    )?;
    c.execute(
        "DELETE FROM usage_records WHERE workspace_id = ? AND source = 'backfill'",
        [ws],
    )?;
    c.execute(
        "DELETE FROM item_marks WHERE workspace_id = ? AND backfill = 1",
        [ws],
    )?;
    let tz = tz_of(c, ws)?;
    let mut r = Report::default();
    for s in sessions(c, ws)? {
        r.turns += turns(c, ws, &s)?;
        r.usage += usage(c, ws, &s, tz)?;
    }
    let items: Vec<(String, i64, i64, String)> = c
        .prepare("SELECT id, created, updated, status FROM items WHERE workspace_id = ? AND kind = 'task' ORDER BY created")?
        .query_map([ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, created, updated, status) in items {
        let marks = item_marks(c, &id, created, updated, &status)?;
        let cutoff: Option<i64> = c.query_row(
            "SELECT MIN(ts) FROM item_marks WHERE item_id = ? AND backfill = 0",
            [&id],
            |r| r.get(0),
        )?;
        for m in marks
            .into_iter()
            .filter(|m| cutoff.is_none_or(|t| m.ts < t))
        {
            c.prepare_cached(
                "INSERT INTO item_marks (workspace_id, item_id, ts, kind, from_v, to_v, actor, ref, detail, backfill) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
            )?
            .execute(params![ws, id, m.ts, m.kind, m.from_v, m.to_v, m.actor, m.r#ref, m.detail.to_string()])?;
            r.marks += 1;
        }
    }
    flag_workspace(c, ws)?;
    Ok(r)
}

/* ── turns ── */

struct Sess {
    id: String,
    item: Option<String>,
    bot: Option<String>,
    project: Option<String>,
    harness: String,
    model: String,
    cost: Option<f64>,
    updated: i64,
}

/// The workspace's own (not imported) sessions.
fn sessions(c: &Connection, ws: &str) -> Result<Vec<Sess>> {
    Ok(c
        .prepare(
            "SELECT s.id, s.item_id, s.bot_id, i.project_id, s.harness, COALESCE(NULLIF(s.model, ''), NULLIF(b.model, ''), ''), s.cost, s.updated
             FROM sessions s LEFT JOIN items i ON i.id = s.item_id LEFT JOIN bots b ON b.id = s.bot_id
             WHERE s.workspace_id = ? AND s.source = 'Steve' ORDER BY s.created",
        )?
        .query_map([ws], |r| {
            Ok(Sess {
                id: r.get(0)?,
                item: r.get(1)?,
                bot: r.get(2)?,
                project: r.get(3)?,
                harness: r.get(4)?,
                model: r.get(5)?,
                cost: r.get(6)?,
                updated: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// A turn being reconstructed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Turn {
    pub user_seq: Option<i64>,
    pub attempt: i64,
    pub started: i64,
    pub ended: i64,
    pub stop_reason: String,
    pub error: Option<String>,
    pub transient: bool,
    pub tools: i64,
    pub tool_errors: i64,
    pub tool_kinds: BTreeMap<String, i64>,
    pub permissions: i64,
}

/// An `events` row: (seq, ts, kind, data).
pub type EventRow = (i64, i64, String, Value);

/// The retry notice the runner writes before sending a prompt again ("…（第 k 次）").
fn retry_attempt(text: &str) -> Option<i64> {
    if !text.starts_with("推理网关暂时不可用") {
        return None;
    }
    let tail = text.rsplit("第 ").next()?;
    tail.split(' ').next()?.parse().ok()
}

/// Cuts a session's events into turns: a run of `user` events opens one, `turn_end` closes it with its
/// `stopReason`, a "Hub 重启…中断" notice closes it as `interrupted`, a gateway retry notice closes it
/// as a transient error and opens the next attempt. An interrupted turn ends at the session's last
/// event before the restart notice (the notice itself is written at the next boot, maybe hours later),
/// and a turn still open at the end ends at the session's last event, never later than `last`, both
/// as `interrupted`. `events` are all of the session's events, in order (kinds not listed below only
/// move the clock). Tool calls, failed tool calls and permission requests
/// in between are counted; events outside a turn (autonomous work) are not.
pub fn cut_turns(events: &[EventRow], last: i64) -> Vec<Turn> {
    let mut out = Vec::new();
    let mut cur: Option<Turn> = None;
    let mut failed: HashSet<String> = HashSet::new();
    let mut seen_tools: HashSet<String> = HashSet::new();
    // the time of the last event seen before the current one
    let mut seen: Option<i64> = None;
    for (seq, ts, kind, data) in events {
        let before = seen;
        seen = Some(*ts);
        let text = data.get("text").and_then(Value::as_str).unwrap_or_default();
        match kind.as_str() {
            "user" => {
                if cur.is_none() {
                    cur = Some(Turn {
                        user_seq: Some(*seq),
                        started: *ts,
                        ..Turn::default()
                    });
                    failed.clear();
                    seen_tools.clear();
                }
            }
            "turn_end" => {
                if let Some(mut t) = cur.take() {
                    t.ended = *ts;
                    t.stop_reason = data
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .unwrap_or("end_turn")
                        .into();
                    out.push(t);
                }
            }
            "system" => {
                let Some(t) = cur.as_mut() else { continue };
                if let Some(k) = retry_attempt(text) {
                    let mut done = cur.take().unwrap_or_default();
                    done.ended = *ts;
                    done.stop_reason = "error".into();
                    done.transient = true;
                    done.error = done.error.or_else(|| Some(text.into()));
                    out.push(done);
                    cur = Some(Turn {
                        attempt: k,
                        started: *ts,
                        ..Turn::default()
                    });
                    failed.clear();
                    seen_tools.clear();
                } else if text.starts_with("Hub 重启") && text.contains("中断") {
                    let mut done = cur.take().unwrap_or_default();
                    done.ended = before.unwrap_or(done.started).max(done.started);
                    done.stop_reason = "interrupted".into();
                    out.push(done);
                } else if let Some(e) = text.strip_prefix("这一回合出错：") {
                    t.error = Some(e.into());
                }
            }
            "tool_call" => {
                let Some(t) = cur.as_mut() else { continue };
                t.tools += 1;
                let name = data
                    .pointer("/_meta/claudeCode/toolName")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let bucket = match name.strip_prefix("mcp__steve__") {
                    Some(tool) => format!("mcp:{tool}"),
                    None => data
                        .get("kind")
                        .and_then(Value::as_str)
                        .filter(|k| !k.is_empty())
                        .unwrap_or("other")
                        .to_owned(),
                };
                *t.tool_kinds.entry(bucket).or_default() += 1;
                if !name.is_empty() && !name.starts_with("mcp__steve__") {
                    *t.tool_kinds.entry(format!("tool:{name}")).or_default() += 1;
                }
                if data.get("status").and_then(Value::as_str) == Some("failed") {
                    t.tool_errors += 1;
                }
            }
            "tool_call_update" => {
                let Some(t) = cur.as_mut() else { continue };
                let id = data
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if data.get("status").and_then(Value::as_str) == Some("failed")
                    && failed.insert(id.to_owned())
                {
                    t.tool_errors += 1;
                }
            }
            // a transcript tool entry (steve.v1): its whole view, counted once by id
            "tool" => {
                let Some(t) = cur.as_mut() else { continue };
                let tool = &data["tool"];
                let id = tool["id"].as_str().unwrap_or_default();
                if seen_tools.insert(id.to_owned()) {
                    t.tools += 1;
                    let name = tool["name"].as_str().unwrap_or_default();
                    let bucket = match name.strip_prefix("mcp__steve__") {
                        Some(tool) => format!("mcp:{tool}"),
                        None => tool["category"].as_str().unwrap_or("other").to_owned(),
                    };
                    *t.tool_kinds.entry(bucket).or_default() += 1;
                    if !name.is_empty() && !name.starts_with("mcp__steve__") {
                        *t.tool_kinds.entry(format!("tool:{name}")).or_default() += 1;
                    }
                }
                if tool["phase"] == "failed" && failed.insert(id.to_owned()) {
                    t.tool_errors += 1;
                }
            }
            "permission" => {
                if let Some(t) = cur.as_mut() {
                    t.permissions += 1;
                }
            }
            _ => {}
        }
    }
    if let Some(mut t) = cur {
        t.ended = seen.unwrap_or(t.started).min(last).max(t.started);
        t.stop_reason = "interrupted".into();
        out.push(t);
    }
    out
}

fn turns(c: &Connection, ws: &str, s: &Sess) -> Result<usize> {
    let cutoff: Option<i64> = c.query_row(
        "SELECT MIN(started) FROM agent_turns WHERE session_id = ? AND backfill = 0",
        [&s.id],
        |r| r.get(0),
    )?;
    let events: Vec<EventRow> = c
        .prepare_cached(
            "SELECT seq, ts, kind, CASE WHEN kind IN ('user', 'turn_end', 'system', 'tool_call', 'tool_call_update', 'tool', 'permission')
               THEN data ELSE 'null' END FROM events WHERE session_id = ?1 AND ts < ?2 ORDER BY seq",
        )?
        .query_map(params![s.id, cutoff.unwrap_or(i64::MAX)], |r| {
            let data: String = r.get(3)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                serde_json::from_str(&data).unwrap_or(Value::Null),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let last = cutoff.map_or(s.updated, |t| t.min(s.updated));
    let ts = cut_turns(&events, last);
    for t in &ts {
        c.prepare_cached(
            "INSERT INTO agent_turns (id, workspace_id, session_id, item_id, bot_id, project_id, harness, user_seq, attempt, started, ended,
               stop_reason, error, transient, tools, tool_errors, tool_kinds, permissions, backfill)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
        )?
        .execute(params![
            uid("tn"),
            ws,
            s.id,
            s.item,
            s.bot,
            s.project,
            s.harness,
            t.user_seq,
            t.attempt,
            t.started,
            t.ended,
            t.stop_reason,
            t.error,
            t.transient,
            t.tools,
            t.tool_errors,
            serde_json::to_string(&t.tool_kinds)?,
            t.permissions
        ])?;
    }
    Ok(ts.len())
}

/// One `source=backfill` record per session: the adapter's own dollar figure (`sessions.cost`, a lower
/// bound: it restarts from 0 with the adapter), less what live records already booked for it.
fn usage(c: &Connection, ws: &str, s: &Sess, tz: Option<chrono_tz::Tz>) -> Result<usize> {
    let live: f64 = c.query_row(
        "SELECT COALESCE(SUM(cost_reported), 0) FROM usage_records WHERE session_id = ? AND source != 'backfill'",
        [&s.id],
        |r| r.get(0),
    )?;
    let cost = s.cost.map(|v| (v - live).max(0.0));
    c.prepare_cached(
        "INSERT INTO usage_records (workspace_id, ts, day, source, purpose, harness, model, session_id, item_id, bot_id, project_id, cost_reported, detail)
         VALUES (?, ?, ?, 'backfill', 'turn', ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        ws,
        s.updated,
        crate::day::day(s.updated, tz),
        s.harness,
        s.model,
        s.id,
        s.item,
        s.bot,
        s.project,
        cost,
        json!({"scope": "session"}).to_string()
    ])?;
    Ok(1)
}

/* ── marks ── */

/// A mark to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub ts: i64,
    pub kind: &'static str,
    pub from_v: Option<String>,
    pub to_v: Option<String>,
    pub actor: String,
    pub r#ref: Option<String>,
    pub detail: Value,
}

impl Mark {
    fn new(ts: i64, kind: &'static str, to: &str, actor: &str) -> Self {
        Mark {
            ts,
            kind,
            from_v: None,
            to_v: Some(to.into()),
            actor: actor.into(),
            r#ref: None,
            detail: json!({}),
        }
    }
}

/// Timeline texts that close an item: accepted by the user or by itself, released, wrapped up.
fn finishes(text: &str) -> bool {
    text == "验收通过"
        || text.contains("自动验收")
        || text.starts_with("已发布")
        || text.contains("本事项收尾")
}

/// The "发布… 失败" / "已发布…" line a release ended with.
fn release_line(text: &str) -> bool {
    text.starts_with("已发布") || (text.starts_with("发布") && text.contains("失败"))
}

fn release_value(text: &str) -> &'static str {
    if text.starts_with("已发布") {
        "released"
    } else if text.contains("没有报告结果") {
        "unknown"
    } else if text.contains("已回到") {
        "rolled_back"
    } else {
        "failed"
    }
}

/// The marks of one item, oldest first, from its row, audit entries, timeline and decisions.
pub fn item_marks(
    c: &Connection,
    id: &str,
    created: i64,
    updated: i64,
    status: &str,
) -> Result<Vec<Mark>> {
    let timeline: Vec<(i64, String, String)> = c
        .prepare_cached("SELECT ts, actor, text FROM timeline WHERE item_id = ? ORDER BY id")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let audit: Vec<(i64, String, String)> = c
        .prepare_cached(
            "SELECT ts, actor, action FROM audit WHERE target = ? AND action IN ('item.drop', 'item.pause') ORDER BY ts",
        )?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let decisions = crate::items::decisions_of(c, id)?;
    let requested: HashMap<String, String> = c
        .prepare_cached("SELECT id, question FROM decisions WHERE item_id = ?")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(build_marks(
        created, updated, status, &timeline, &audit, &decisions, &requested,
    ))
}

/// [`item_marks`] over loaded rows: timeline `(ts, actor, text)` in order, audit `(ts, actor, action)`,
/// decisions with their questions by id.
pub fn build_marks(
    created: i64,
    updated: i64,
    status: &str,
    timeline: &[(i64, String, String)],
    audit: &[(i64, String, String)],
    decisions: &[crate::items::DecisionRow],
    questions: &HashMap<String, String>,
) -> Vec<Mark> {
    let mut out = Vec::new();

    // status: created → (paused / dropped from the audit) → done from the timeline
    let mut status_marks = vec![Mark {
        from_v: None,
        ..Mark::new(created, "status", "run", "")
    }];
    for (ts, actor, action) in audit {
        let to = if action == "item.drop" {
            "drop"
        } else {
            "pause"
        };
        status_marks.push(Mark::new(*ts, "status", to, actor));
    }
    if status == "done" {
        let at = timeline.iter().rev().find(|t| finishes(&t.2));
        status_marks.push(match at {
            Some((ts, actor, _)) => Mark::new(*ts, "status", "done", actor),
            None => Mark {
                detail: json!({"approx": true}),
                ..Mark::new(updated, "status", "done", "")
            },
        });
    } else if status == "drop" && !audit.iter().any(|a| a.2 == "item.drop") {
        status_marks.push(Mark {
            detail: json!({"approx": true}),
            ..Mark::new(updated, "status", "drop", "")
        });
    }
    status_marks.sort_by_key(|m| m.ts);
    let mut prev: Option<String> = None;
    for mut m in status_marks {
        if prev.is_some() && prev == m.to_v {
            continue;
        }
        m.from_v = prev.clone();
        prev = m.to_v.clone();
        out.push(m);
    }

    // deliveries: "交付，等你验收", automatic when followed by "自检全部通过，自动验收"
    let accepts: Vec<&crate::items::DecisionRow> =
        decisions.iter().filter(|d| d.kind == "accept").collect();
    let mut used = HashSet::new();
    for (i, (ts, actor, text)) in timeline.iter().enumerate() {
        if text != "交付，等你验收" {
            continue;
        }
        let auto = timeline
            .get(i + 1)
            .is_some_and(|n| n.2.contains("自动验收") && n.0 - ts < 60_000);
        let mut m = Mark::new(*ts, "deliver", if auto { "auto" } else { "manual" }, actor);
        if !auto {
            m.r#ref = accepts
                .iter()
                .find(|d| {
                    !used.contains(&d.id) && d.created >= ts - 2_000 && d.created < ts + 60_000
                })
                .map(|d| d.id.clone());
            if let Some(r) = &m.r#ref {
                used.insert(r.clone());
            }
        }
        out.push(m);
    }

    // fail cards, by the text they were opened with
    for d in decisions.iter().filter(|d| d.kind == "fail") {
        let why = questions.get(&d.id).map(String::as_str).unwrap_or_default();
        out.push(Mark {
            r#ref: Some(d.id.clone()),
            detail: json!({ "why": why }),
            ..Mark::new(d.created, "fail", fail_reason(why), "steve")
        });
    }

    // releases: the outcome on the approved decision, timed by the line that reported it
    let releases: Vec<&crate::items::DecisionRow> = decisions
        .iter()
        .filter(|d| {
            d.kind == "approve"
                && questions
                    .get(&d.id)
                    .is_some_and(|q| q.starts_with("发布新版本"))
        })
        .filter(|d| d.status == "resolved" && d.resolved.is_some())
        .collect();
    for (k, d) in releases.iter().enumerate() {
        let from = d.resolved.unwrap_or(d.created);
        let until = releases
            .get(k + 1)
            .and_then(|n| n.resolved)
            .unwrap_or(i64::MAX);
        let line = timeline
            .iter()
            .find(|t| t.0 >= from && t.0 < until && release_line(&t.2));
        let outcome = d.result.get("outcome").and_then(Value::as_str);
        let approved = d.result.get("approved").and_then(Value::as_bool) != Some(false)
            && d.result.get("choice").and_then(Value::as_str) != Some("no");
        let (to, ts, approx) = match (outcome, line) {
            (Some("not_released"), _) => ("not_released", from, false),
            (Some("releasing"), None) => continue,
            (_, Some((ts, _, text))) if approved => (release_value(text), *ts, false),
            (Some("released"), None) => ("released", from, true),
            (Some("failed"), None) => ("failed", from, true),
            _ => continue,
        };
        let mut m = Mark::new(ts, "release", to, "steve");
        m.r#ref = Some(d.id.clone());
        if approx {
            m.detail = json!({"approx": true});
        }
        out.push(m);
    }

    out.sort_by_key(|m| m.ts);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, ts: i64, kind: &str, data: Value) -> EventRow {
        (seq, ts, kind.into(), data)
    }

    #[test]
    fn retry_notice_parses() {
        assert_eq!(
            retry_attempt("推理网关暂时不可用，30 秒后自动重试（第 2 次）"),
            Some(2)
        );
        assert_eq!(retry_attempt("这一回合出错：x"), None);
    }

    #[test]
    fn cuts_turns() {
        let evs = vec![
            ev(1, 100, "user", json!({"text": "do"})),
            ev(2, 101, "user", json!({"text": "more"})),
            ev(
                3,
                110,
                "tool_call",
                json!({"kind": "execute", "_meta": {"claudeCode": {"toolName": "Bash"}}, "toolCallId": "a"}),
            ),
            ev(
                4,
                111,
                "tool_call_update",
                json!({"toolCallId": "a", "status": "failed"}),
            ),
            ev(
                5,
                112,
                "tool_call_update",
                json!({"toolCallId": "a", "status": "failed"}),
            ),
            ev(
                6,
                113,
                "tool_call",
                json!({"kind": "other", "_meta": {"claudeCode": {"toolName": "mcp__steve__steve_deliver"}}}),
            ),
            ev(7, 114, "permission", json!({})),
            ev(
                8,
                120,
                "system",
                json!({"text": "推理网关暂时不可用，5 秒后自动重试（第 1 次）"}),
            ),
            ev(9, 130, "turn_end", json!({"stopReason": "end_turn"})),
            // autonomous work between turns is not counted
            ev(10, 140, "tool_call", json!({"kind": "read"})),
            ev(11, 200, "user", json!({"text": "again"})),
            ev(
                12,
                210,
                "system",
                json!({"text": "Hub 重启，这一回合被中断。不会自动重放；需要时让它继续。"}),
            ),
            ev(13, 300, "user", json!({"text": "last"})),
            ev(14, 310, "system", json!({"text": "这一回合出错：boom"})),
        ];
        let t = cut_turns(&evs, 400);
        assert_eq!(t.len(), 4);
        assert_eq!(
            (t[0].started, t[0].ended, t[0].user_seq, t[0].transient),
            (100, 120, Some(1), true)
        );
        assert_eq!((t[0].tools, t[0].tool_errors, t[0].permissions), (2, 1, 1));
        assert_eq!(t[0].tool_kinds.get("execute"), Some(&1));
        assert_eq!(t[0].tool_kinds.get("tool:Bash"), Some(&1));
        assert_eq!(t[0].tool_kinds.get("mcp:steve_deliver"), Some(&1));
        assert_eq!(
            (t[1].attempt, t[1].user_seq, t[1].stop_reason.as_str()),
            (1, None, "end_turn")
        );
        assert_eq!(
            (t[2].stop_reason.as_str(), t[2].ended),
            ("interrupted", 200)
        );
        assert_eq!(
            (t[3].stop_reason.as_str(), t[3].ended),
            ("interrupted", 310)
        );
        assert_eq!(t[3].error.as_deref(), Some("boom"));
    }

    #[test]
    fn a_long_gap_before_the_restart_is_not_turn_time() {
        let h = 3_600_000;
        let evs = vec![
            ev(1, 0, "user", json!({"text": "go"})),
            ev(2, 60_000, "tool_call", json!({"kind": "execute"})),
            ev(3, 120_000, "agent_message_chunk", Value::Null),
            // the Hub was down for 5 hours; recover writes the notice at boot
            ev(
                4,
                5 * h,
                "system",
                json!({"text": "Hub 重启时这个会话正在运行，这一回合被中断。"}),
            ),
            ev(5, 5 * h + 1_000, "user", json!({"text": "continue"})),
            ev(6, 5 * h + 30_000, "tool_call", json!({"kind": "read"})),
        ];
        // the session was touched again much later (updated), with no event after the last tool call
        let t = cut_turns(&evs, 9 * h);
        assert_eq!(t.len(), 2);
        assert_eq!(
            (t[0].started, t[0].ended, t[0].stop_reason.as_str()),
            (0, 120_000, "interrupted")
        );
        assert_eq!(
            (t[1].started, t[1].ended, t[1].stop_reason.as_str()),
            (5 * h + 1_000, 5 * h + 30_000, "interrupted")
        );
    }
}

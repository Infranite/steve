//! The per-conversation cursor and the window cut.
//!
//! Each conversation keeps its own cursor (`digest_cursors`: the last message cut into a window). A window is
//! cut when
//! - `size`: the window size of messages wait after the cursor (a busy conversation cuts several at once);
//! - `quiet`: some wait and none came for the quiet time;
//! - `max_wait`: the oldest waiting one waited the maximum wait (so every conversation is digested at least
//!   once a day by default);
//! - `manual`: asked for (the page, a test, the dev report).
//!
//! A window carries the last few messages before it as context (`context`), never written back. A conversation
//! without a cursor starts with all stored messages (`backlog_days = 0`), or a bounded lookback: only what Steve stored, nothing is fetched (history is pulled
//! on demand elsewhere and is then digested like any other message). Depth `record` moves the cursor without
//! queueing anything, `paused` leaves the messages waiting, `alerts` aggregates by rule and queues only the
//! anomalies. A day's spend over the daily budget defers the rest to the next day.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::json;
use steve_store::{Store, StoreError, Tx};

use crate::conv::{self, ConvInfo, ConvRef, Msg};
use crate::profile::{self, Profile};
use crate::queue::{self, Batching, NewPacket};
use crate::{alerts, candidates, chunk, facts, summary};

type Result<T> = std::result::Result<T, StoreError>;

pub const DAY_MS: i64 = 24 * 3600 * 1000;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Cursor {
    pub at: i64,
    /// Fixed initial history floor, independent of the advancing high-water mark.
    pub since: i64,
    pub msg_id: String,
    pub windows: i64,
    pub day: String,
    pub spent: i64,
}

pub fn cursor(c: &Connection, ws: &str, conv: &ConvRef) -> Result<Option<Cursor>> {
    Ok(c
        .prepare_cached("SELECT at, msg_id, windows, day, spent, since FROM digest_cursors WHERE workspace_id = ? AND conv = ?")?
        .query_row(params![ws, conv.key()], |r| {
            Ok(Cursor {
                at: r.get(0)?,
                msg_id: r.get(1)?,
                windows: r.get(2)?,
                day: r.get(3)?,
                spent: r.get(4)?,
                since: r.get(5)?,
            })
        })
        .optional()?)
}

fn save_cursor(tx: &Tx, ws: &str, conv: &ConvRef, k: &Cursor, now: i64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO digest_cursors (workspace_id, conv, at, msg_id, windows, day, spent, updated, since) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![ws, conv.key(), k.at, k.msg_id, k.windows, k.day, k.spent, now, k.since],
    )?;
    Ok(())
}

fn receipt(tx: &Tx, ws: &str, conv: &ConvRef, msgs: &[Msg], seq: i64, now: i64) -> Result<()> {
    for m in msgs {
        tx.execute("INSERT INTO digest_receipts (workspace_id, conv, message_id, seq, created) VALUES (?, ?, ?, ?, ?)", params![ws, conv.key(), m.id, seq, now])?;
    }
    Ok(())
}

/// What one tick did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub conversations: usize,
    pub windows: usize,
    pub packets: Vec<String>,
    /// Alert windows that had no anomaly (rolled up only).
    pub rolled: usize,
    /// Conversations whose windows wait for tomorrow's budget.
    pub deferred: usize,
    /// Messages passed by depth `record`.
    pub recorded: usize,
}

/// The kind as the packet's header says it.
fn kind_label(k: &str) -> &'static str {
    match k {
        "project" => "专项群",
        "team" => "团队群",
        "feed" => "信息归类群",
        "dm" => "私聊",
        "mail" => "邮件线程",
        "pr" => "PR",
        "issue" => "Issue",
        _ => "会话",
    }
}

fn trigger(
    n: usize,
    window: i64,
    oldest: i64,
    newest: i64,
    now: i64,
    p: &Profile,
    manual: bool,
) -> Option<&'static str> {
    if n == 0 {
        None
    } else if n as i64 >= window {
        Some("size")
    } else if now - newest >= p.settings.quiet_ms {
        Some("quiet")
    } else if now - oldest >= p.settings.max_wait_ms {
        Some("max_wait")
    } else if manual {
        Some("manual")
    } else {
        None
    }
}

fn initial_since(now: i64, b: &Batching) -> i64 {
    if b.backlog_days <= 0 {
        i64::MIN
    } else {
        now.saturating_sub(b.backlog_days.saturating_mul(DAY_MS))
    }
}

/// Cuts every window that is due in `conv`. `manual` cuts what waits even when nothing is due.
pub fn run_conv(
    tx: &Tx,
    ws: &str,
    conv: &ConvRef,
    now: i64,
    b: &Batching,
    manual: bool,
    rep: &mut Report,
) -> Result<()> {
    let Some(info) = conv::info(tx, ws, conv)? else {
        return Ok(());
    };
    let p = profile::profile(tx, ws, &info)?;
    let mut k = cursor(tx, ws, conv)?.unwrap_or_else(|| Cursor {
        at: initial_since(now, b),
        since: initial_since(now, b),
        ..Default::default()
    });
    let day = crate::day_of(now);
    if k.day != day {
        k.day = day;
        k.spent = 0;
    }
    // Persist the initial floor even when paused or budget-deferred.
    save_cursor(tx, ws, conv, &k, now)?;
    rep.conversations += 1;
    match p.settings.depth.as_str() {
        "paused" => return Ok(()),
        "record" => {
            loop {
                let msgs = conv::unread(tx, ws, conv, k.since, 500)?;
                let Some(last) = msgs.last() else {
                    break;
                };
                receipt(tx, ws, conv, &msgs, k.windows, now)?;
                rep.recorded += msgs.len();
                if (last.at, last.id.as_str()) > (k.at, k.msg_id.as_str()) {
                    k.at = last.at;
                    k.msg_id = last.id.clone();
                }
                save_cursor(tx, ws, conv, &k, now)?;
            }
            return Ok(());
        }
        _ => {}
    }
    loop {
        let (n, oldest, newest) = conv::unread_stats(tx, ws, conv, k.since)?;
        let Some(why) = trigger(
            n as usize,
            p.settings.window,
            oldest,
            newest,
            now,
            &p,
            manual,
        ) else {
            break;
        };
        if p.settings.daily_budget > 0 && k.spent >= p.settings.daily_budget && !manual {
            rep.deferred += 1;
            break;
        }
        let msgs = conv::unread(tx, ws, conv, k.since, p.settings.window.max(1) as usize)?;
        let Some(last) = msgs.last().cloned() else {
            break;
        };
        let context = if k.msg_id.is_empty() {
            Vec::new()
        } else {
            let first = &msgs[0];
            conv::read_before(
                tx,
                ws,
                conv,
                first.at,
                &first.id,
                b.context.saturating_add(1),
            )?
            .into_iter()
            .filter(|m| m.id != first.id)
            .collect()
        };
        let seq = k.windows + 1;
        let tokens = cut_window(tx, ws, &info, &p, seq, why, &context, &msgs, b, now, rep)?;
        rep.windows += 1;
        receipt(tx, ws, conv, &msgs, seq, now)?;
        if (last.at, last.id.as_str()) > (k.at, k.msg_id.as_str()) {
            k.at = last.at;
            k.msg_id = last.id;
        }
        k.windows = seq;
        k.spent += tokens;
        save_cursor(tx, ws, conv, &k, now)?;
        if why != "size" {
            break;
        }
    }
    Ok(())
}

/// One window: rolled up (alerts) and/or queued. Returns the tokens queued.
#[allow(clippy::too_many_arguments)]
fn cut_window(
    tx: &Tx,
    ws: &str,
    info: &ConvInfo,
    p: &Profile,
    seq: i64,
    why: &str,
    context: &[Msg],
    msgs: &[Msg],
    b: &Batching,
    now: i64,
    rep: &mut Report,
) -> Result<i64> {
    let conv = &info.conv;
    let (kind, msgs, context, alert_facts): (&str, Vec<Msg>, Vec<Msg>, Option<serde_json::Value>) =
        if p.settings.depth == "alerts" {
            let o = alerts::aggregate(tx, ws, conv, msgs)?;
            summary::write(tx, ws, conv, None, Some(&o.rollup), None, "steve", now)?;
            if o.anomalies.is_empty() {
                rep.rolled += 1;
                return Ok(0);
            }
            let picked: Vec<Msg> = o.anomalies.iter().map(|i| msgs[*i].clone()).collect();
            let f = json!({"window_messages": msgs.len(), "window_alerts": o.alerts, "rollup": o.rollup,
                           "window_first_id": msgs.first().map(|m| m.id.clone()), "window_last_id": msgs.last().map(|m| m.id.clone())});
            ("alerts", picked, Vec::new(), Some(f))
        } else {
            ("window", msgs.to_vec(), context.to_vec(), None)
        };
    let built = facts::build(tx, ws, info, p, seq, why, &context, &msgs)?;
    let mut facts = built.facts.clone();
    if let Some(a) = alert_facts {
        facts["alerts"] = a;
    }
    let cands = if p.settings.depth == "summary" {
        Vec::new()
    } else {
        candidates::find(tx, ws, conv, &built, now)?
    };
    let role = if info.role.trim().is_empty() {
        String::new()
    } else {
        format!(" · 职责：{}", crate::cut(info.role.trim(), 80))
    };
    let count = msgs.len();
    let chunks = chunk::split(&built.lines, b.chunk_tokens, b.overlap, |n, of, a, z| {
        format!(
            "会话：{}（{}{}）· 第 {seq} 个窗口{} · 第 {n}/{of} 块 · 位置 [{a}]–[{z}]，共 {count} 条",
            info.name,
            kind_label(&p.kind),
            role,
            if kind == "alerts" {
                "（告警异常）"
            } else {
                ""
            },
        )
    });
    let priority = (p.settings.priority + i64::from(built.mentions_me || p.kind == "dm")).min(3);
    let (first, last) = (&msgs[0], &msgs[msgs.len() - 1]);
    let tokens: i64 = chunks.iter().map(|c| c.tokens).sum();
    let id = queue::insert(
        tx,
        &NewPacket {
            ws,
            conv: &conv.key(),
            seq,
            kind,
            trigger: why,
            first_id: &first.id,
            last_id: &last.id,
            first_at: first.at,
            last_at: last.at,
            count: count as i64,
            priority,
            facts: &facts,
            candidates: &candidates::json(&cands),
        },
        &chunks,
        now,
    )?;
    rep.packets.push(id);
    Ok(tokens)
}

/// One tick over every conversation of `ws` with new messages; one transaction per conversation.
pub fn tick(store: &Store, ws: &str, now: i64) -> Result<Report> {
    let b = store.reader(|c| queue::batching(c, ws))?;
    let mut rep = Report::default();
    if !b.enabled {
        return Ok(rep);
    }
    let since = initial_since(now, &b);
    let convs = store.reader(|c| conv::with_news(c, ws, since))?;
    for (conv, _) in convs {
        store.transaction(|tx| run_conv(tx, ws, &conv, now, &b, false, &mut rep))?;
    }
    store.transaction(|tx| queue::release_expired(tx, ws, now).map(|_| ()))?;
    Ok(rep)
}

/// Cuts what waits in one conversation now, due or not.
pub fn cut_now(store: &Store, ws: &str, conv: &ConvRef, now: i64) -> Result<Report> {
    let b = store.reader(|c| queue::batching(c, ws))?;
    let mut rep = Report::default();
    store.transaction(|tx| run_conv(tx, ws, conv, now, &b, true, &mut rep))?;
    Ok(rep)
}

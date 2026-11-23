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

use serde::Serialize;
use serde_json::json;
use steve_store::digest::{Cursor, NewSummary};
use steve_store::{Store, StoreError, W};

use crate::conv::{self, ConvInfo, ConvRef, Msg};
use crate::profile::{self, Profile};
use crate::queue::{Batching, NewPacket};
use crate::{alerts, candidates, chunk, facts};

type Result<T> = std::result::Result<T, StoreError>;

pub const DAY_MS: i64 = 24 * 3600 * 1000;

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

/// Cuts every window that is due in `conv`. `manual` cuts what waits even when nothing is due.
pub fn run_conv(
    w: &mut W<'_>,
    ws: &str,
    conv: &ConvRef,
    now: i64,
    b: &Batching,
    manual: bool,
    rep: &mut Report,
) -> Result<()> {
    run_conv_step(w, ws, conv, now, b, manual, rep, &mut |_| {})
}

fn backlog_floor(now: i64, days: i64) -> i64 {
    if days > 0 {
        now.saturating_sub(days.saturating_mul(DAY_MS))
    } else {
        0
    }
}

#[allow(clippy::too_many_arguments)]
fn run_conv_step(
    w: &mut W<'_>,
    ws: &str,
    conv: &ConvRef,
    now: i64,
    b: &Batching,
    manual: bool,
    rep: &mut Report,
    checkpoint: &mut impl FnMut(&mut W<'_>),
) -> Result<()> {
    let key = conv.key();
    let Some(info) = conv::info(w, ws, conv)? else {
        return Ok(());
    };
    let stored = w.digest_profiles().get(ws, &key)?;
    let p = profile::profile(&info, stored, || conv::guess_kind(w, ws, &info))?;
    let initial_since = backlog_floor(now, b.backlog_days);
    let mut k = if let Some(k) = w.digest_windows().cursor(ws, &key)? {
        k
    } else {
        // Apply lookback once. Later insertions are eligible even with older provider timestamps.
        if b.backlog_days > 0 {
            w.digest_windows().exclude_before(ws, &key, initial_since)?;
        }
        Cursor {
            at: i64::MIN,
            ..Default::default()
        }
    };
    // the budget day, on the clock of the person the digest is for
    let day = steve_modules::zone::Zone::for_owner(w, ws)?.day(now);
    if k.day != day {
        k.day = day;
        k.spent = 0;
    }
    // Persist initialization even when paused or budget-deferred.
    w.digest_windows()
        .save_cursor(ws, &key, &k, now, initial_since)?;
    rep.conversations += 1;
    match p.settings.depth.as_str() {
        "paused" => return Ok(()),
        "record" => {
            loop {
                let msgs = conv::unread(w, ws, conv, 500)?;
                let Some(last) = msgs.last() else {
                    break;
                };
                w.digest_windows().acknowledge(
                    ws,
                    &key,
                    msgs.iter().map(|m| &m.id),
                    k.windows,
                    now,
                )?;
                rep.recorded += msgs.len();
                if (last.at, last.id.as_str()) > (k.at, k.msg_id.as_str()) {
                    k.at = last.at;
                    k.msg_id = last.id.clone();
                }
                w.digest_windows()
                    .save_cursor(ws, &key, &k, now, initial_since)?;
            }
            return Ok(());
        }
        _ => {}
    }
    loop {
        let (n, oldest, newest) = w.digest_windows().pending_stats(ws, &key)?;
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
        let msgs = conv::unread(w, ws, conv, p.settings.window.max(1) as usize)?;
        let Some(last) = msgs.last().cloned() else {
            break;
        };
        let context = if k.msg_id.is_empty() {
            Vec::new()
        } else {
            let first = &msgs[0];
            conv::read_before(
                w,
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
        let tokens = cut_window(w, ws, &info, &p, seq, why, &context, &msgs, b, now, rep)?;
        rep.windows += 1;
        // The explicit receipt acknowledgement consumes the inbox positions. Packets, acknowledgement and the
        // display cursor share this transaction, so a crash here leaves the entire window pending.
        checkpoint(w);
        w.digest_windows()
            .acknowledge(ws, &key, msgs.iter().map(|m| &m.id), seq, now)?;
        checkpoint(w);
        if (last.at, last.id.as_str()) > (k.at, k.msg_id.as_str()) {
            k.at = last.at;
            k.msg_id = last.id;
        }
        k.windows = seq;
        k.spent += tokens;
        w.digest_windows()
            .save_cursor(ws, &key, &k, now, initial_since)?;
        if why != "size" {
            break;
        }
    }
    Ok(())
}

/// One window: rolled up (alerts) and/or queued. Returns the tokens queued.
#[allow(clippy::too_many_arguments)]
fn cut_window(
    w: &mut W<'_>,
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
            let o = alerts::aggregate(w, ws, conv, msgs)?;
            w.summaries().append(
                ws,
                &conv.key(),
                NewSummary {
                    text: None,
                    rules: Some(&o.rollup),
                    packet: None,
                    writer: "steve",
                    now,
                },
            )?;
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
    let built = facts::build(w, ws, info, p, seq, why, &context, &msgs)?;
    let mut facts = built.facts.clone();
    if let Some(a) = alert_facts {
        facts["alerts"] = a;
    }
    let cands = if p.settings.depth == "summary" {
        Vec::new()
    } else {
        candidates::find(w, ws, conv, &built, now)?
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
    let id = w.digest_packets().insert(
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
    let b = store.read(|r| r.digest_packets().batching(ws))?;
    let mut rep = Report::default();
    if !b.enabled {
        return Ok(rep);
    }
    let convs = conv::with_news(store, ws)?;
    for (conv, _) in convs {
        store.write(|w| run_conv(w, ws, &conv, now, &b, false, &mut rep))?;
    }
    store.write(|w| w.digest_packets().release_expired(ws, now))?;
    Ok(rep)
}

/// Cuts what waits in one conversation now, due or not.
pub fn cut_now(store: &Store, ws: &str, conv: &ConvRef, now: i64) -> Result<Report> {
    let b = store.read(|r| r.digest_packets().batching(ws))?;
    let mut rep = Report::default();
    store.write(|w| run_conv(w, ws, conv, now, &b, true, &mut rep))?;
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use steve_modules::testkit::{TestEnv, WS};

    use steve_store::testkit::digest as fixture;

    #[test]
    fn five_hundred_receipts_aggregate_the_conversation_once() {
        let env = TestEnv::new(|_| {});
        env.store()
            .write(|w| {
                fixture::bot_space(w)?;
                let conv = ConvRef::new("space", "s1");
                for n in 0..500 {
                    fixture::seed_message(
                        w,
                        fixture::ChannelInput {
                            id: &format!("batch-{n}"),
                            space: "s1",
                            at: n,
                            reply: None,
                            thread: None,
                            text: "body",
                            mentions: &serde_json::json!([]),
                        },
                    )?;
                }
                let ids: Vec<_> = conv::unread(w, WS, &conv, 500)?
                    .into_iter()
                    .map(|m| m.id)
                    .collect();
                assert_eq!(ids.len(), 500);
                assert_eq!(fixture::acknowledge_counted(w, WS, &conv.key(), &ids)?, 1);
                assert_eq!(fixture::receipt_count_in(w, WS)?, 500);
                Ok::<_, StoreError>(())
            })
            .unwrap();
        fixture::audit(env.store()).unwrap();
    }

    #[test]
    fn panic_before_or_after_acknowledgement_replays_once_after_reopen() {
        for fail_at in 1..=2 {
            let e = TestEnv::new(|_| {});
            e.store()
                .write(|w| {
                    fixture::bot_space(w)?;
                    fixture::seed_message(
                        w,
                        fixture::ChannelInput {
                            id: "m1",
                            space: "s1",
                            at: 1,
                            reply: None,
                            thread: None,
                            text: "backfilled message",
                            mentions: &serde_json::json!([]),
                        },
                    )
                })
                .unwrap();
            let conv = ConvRef::new("space", "s1");
            let mut checkpoints = 0;
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                let _: Result<()> = e.store().write(|w| {
                    run_conv_step(
                        w,
                        WS,
                        &conv,
                        DAY_MS,
                        &Batching::default(),
                        true,
                        &mut Report::default(),
                        &mut |w| {
                            checkpoints += 1;
                            assert_eq!(
                                w.digest_packets().list(WS, None, None, 10).unwrap().len(),
                                1
                            );
                            assert_eq!(
                                fixture::receipt_count_in(w, WS).unwrap(),
                                i64::from(checkpoints == 2)
                            );
                            assert_eq!(
                                w.digest_windows().pending_stats(WS, &conv.key()).unwrap().0,
                                i64::from(checkpoints == 1)
                            );
                            if checkpoints == fail_at {
                                panic!("crash at acknowledgement boundary {fail_at}");
                            }
                        },
                    )
                });
            }));
            assert!(panicked.is_err());
            let reopened = Store::open(
                e.dir.path().join("steve.sqlite3"),
                steve_store::FnPublisher(|_| {}),
            )
            .unwrap();
            reopened
                .read::<_, steve_store::StoreError>(|r| {
                    assert!(r.digest_packets().list(WS, None, None, 50)?.is_empty());
                    assert_eq!(r.digest_windows().pending_stats(WS, &conv.key())?.0, 1);
                    assert!(r.digest_windows().cursor(WS, &conv.key())?.is_none());
                    Ok(())
                })
                .unwrap();
            assert_eq!(cut_now(&reopened, WS, &conv, DAY_MS).unwrap().windows, 1);
            assert_eq!(cut_now(&reopened, WS, &conv, DAY_MS).unwrap().windows, 0);
            reopened
                .read::<_, steve_store::StoreError>(|r| {
                    assert_eq!(r.digest_packets().list(WS, None, None, 50)?.len(), 1);
                    assert_eq!(
                        r.digest_windows().pending_stats(WS, &conv.key())?,
                        (0, 0, 0)
                    );
                    Ok(())
                })
                .unwrap();
            assert_eq!(fixture::receipt_count(&reopened, WS).unwrap(), 1);
            fixture::audit(&reopened).unwrap();
        }
    }
}

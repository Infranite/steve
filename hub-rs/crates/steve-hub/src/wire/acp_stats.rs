//! Agent turns into the statistics tables (stats.md §2.2): `agent_turns`, `usage_records`
//! and the `stat_daily` cube, through `steve_store::Stats`. Token figures are brought to one convention
//! with `steve_stats::normalize`; days are cut in the workspace's `stats.tz`.

use serde_json::json;
use steve_store::{ModelUsage, NewUsage, Session, Store, Tokens};

/// Session attribution of a turn: workspace, item, bot, project, and the fallback model.
struct Owner {
    s: Session,
    project: Option<String>,
    bot_model: Option<String>,
}

fn owner(store: &Store, session_id: &str) -> Option<Owner> {
    let s = store.sessions().get(session_id).ok().flatten()?;
    // imported read-only sessions are not Agent work
    if s.readonly != 0 {
        return None;
    }
    let project = s
        .item_id
        .as_deref()
        .and_then(|i| store.items().get(i).ok().flatten())
        .and_then(|i| i.project_id);
    let bot_model = s
        .bot_id
        .as_deref()
        .and_then(|b| store.bots().get(b).ok().flatten())
        .and_then(|b| b.model)
        .filter(|m| !m.is_empty());
    Some(Owner {
        s,
        project,
        bot_model,
    })
}

impl Owner {
    /// The adapter's model, else the session's, else the bot's, else `""`.
    fn model(&self, reported: Option<&str>) -> String {
        reported
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .or_else(|| self.s.model.clone().filter(|m| !m.is_empty()))
            .or_else(|| self.bot_model.clone())
            .unwrap_or_default()
    }
}

fn day(store: &Store, ws: &str, ts: i64) -> String {
    let tz: String = store
        .settings()
        .get(ws, "stats.tz", String::new())
        .unwrap_or_default();
    steve_stats::day(ts, steve_stats::tz(Some(&tz)))
}

/// Boot: Codex turns that ended without tokens get them from Codex's rollout file. codex-acp never
/// reports usage, so every Codex turn before the runner read the rollout itself (ST-21, 2026-11-02)
/// was in the statistics with no tokens; the turn's rows are added once, like at its end.
pub async fn repair_codex(store: &Store, codex_home: &std::path::Path) -> usize {
    let turns: Vec<(String, String, i64, i64, String)> = store
        .reader(|c| -> steve_store::Result<Vec<_>> {
            let mut st = c.prepare(
                "SELECT t.id, t.session_id, t.started, t.ended, s.acp_session_id FROM agent_turns t
                 JOIN sessions s ON s.id = t.session_id
                 WHERE t.harness = 'codex' AND t.backfill = 0 AND t.ended IS NOT NULL AND s.acp_session_id IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM usage_records u WHERE u.turn_id = t.id)
                 ORDER BY t.started",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap_or_default();
    let mut added = 0;
    for (turn, sid, started, ended, acp) in turns {
        let Some(raw) = steve_acp::codex_turn_usage(
            codex_home,
            &acp,
            started,
            ended,
            std::time::Duration::ZERO,
        )
        .await
        else {
            continue;
        };
        let Some(o) = owner(store, &sid) else {
            continue;
        };
        let ws = o.s.workspace_id.clone();
        let codex_cache: bool = store
            .settings()
            .get(&ws, "stats.codex_input_includes_cache", true)
            .unwrap_or(true);
        let r = store.stats().usage_add(&NewUsage {
            day: day(store, &ws, ended),
            workspace_id: ws,
            ts: ended,
            source: "agent".into(),
            purpose: "turn".into(),
            harness: o.s.harness.clone(),
            turn_id: Some(turn.clone()),
            session_id: Some(o.s.id.clone()),
            item_id: o.s.item_id.clone(),
            bot_id: o.s.bot_id.clone(),
            project_id: o.project.clone(),
            usage: ModelUsage {
                model: o.model(None),
                tokens: steve_stats::normalize(
                    "codex",
                    steve_stats::RawUsage::from_json(&raw),
                    codex_cache,
                ),
                cost_reported: None,
                detail: json!({ "scope": "main", "from": "rollout" }),
            },
            duration_ms: None,
            ok: true,
            attempts: 1,
        });
        match r {
            Ok(_) => added += 1,
            Err(e) => tracing::error!("stats codex repair {turn}: {e}"),
        }
    }
    if added > 0 {
        tracing::info!("stats: tokens of {added} codex turns read from their rollout files");
    }
    added
}

/// Boot: turns still open were cut by the restart (`stop_reason = interrupted`).
pub fn interrupt_open(store: &Store, at: i64) {
    match store.stats().interrupt_open(at, |ws, t| day(store, ws, t)) {
        Ok(0) => {}
        Ok(n) => tracing::info!("stats: {n} open turns interrupted by the restart"),
        Err(e) => tracing::error!("stats interrupt open turns: {e}"),
    }
}

// ── attempts on a Node ──

/// A Node attempt began: its statistics row, found again by the attempt at its end.
pub fn node_attempt_started(
    store: &Store,
    session: &str,
    attempt: &str,
    ordinal: i64,
    user_seq: Option<i64>,
    at: i64,
) {
    let Some(o) = owner(store, session) else {
        return;
    };
    let r = store.stats().turn_begin(&steve_store::TurnBegin {
        workspace_id: o.s.workspace_id.clone(),
        session_id: o.s.id.clone(),
        item_id: o.s.item_id.clone(),
        bot_id: o.s.bot_id.clone(),
        project_id: o.project.clone(),
        harness: o.s.harness.clone(),
        user_seq,
        attempt: ordinal,
        started: at,
    });
    let linked = r.and_then(|id| {
        store.with_conn(|c| {
            c.execute(
                "UPDATE agent_turns SET node_attempt_id = ? WHERE id = ?",
                rusqlite::params![attempt, id],
            )?;
            Ok(())
        })
    });
    if let Err(e) = linked {
        tracing::error!("stats attempt begin {session}: {e}");
    }
}

/// A Node attempt ended: its row gets what it did and what it cost.
pub fn node_attempt_finished(
    store: &Store,
    session: &str,
    attempt: &str,
    c: &steve_node_api::Completion,
    f: &super::node_runtime::AttemptFacts,
    at: i64,
) {
    let Some(o) = owner(store, session) else {
        return;
    };
    let turn: Option<String> = store
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT id FROM agent_turns WHERE node_attempt_id = ?",
                [attempt],
                |r| r.get(0),
            )
            .ok())
        })
        .ok()
        .flatten();
    let Some(turn_id) = turn else {
        return;
    };
    let ws = &o.s.workspace_id;
    // a final figure replaces the attempt's deltas; deltas add up
    let tokens_of = |t: &steve_node_api::Tokens| Tokens {
        input: t.input.map(|x| x as i64),
        output: t.output.map(|x| x as i64),
        cache_read: t.cache_read.map(|x| x as i64),
        cache_write: t.cache_write.map(|x| x as i64),
        reasoning: None,
    };
    // the per-model figures for everything (scope `all`) when there are any, else the main
    // loop's: `all` already holds `main`, the two never add up
    let scoped = |m: steve_node_api::Measurement| -> Vec<&steve_node_api::UsageReport> {
        let of: Vec<_> = f.usage.iter().filter(|u| u.measurement == m).collect();
        if of
            .iter()
            .any(|u| u.scope == steve_node_api::UsageScope::All)
        {
            of.into_iter()
                .filter(|u| u.scope == steve_node_api::UsageScope::All)
                .collect()
        } else {
            of
        }
    };
    let finals = scoped(steve_node_api::Measurement::Final);
    let mut usage: Vec<ModelUsage> = if finals.is_empty() {
        scoped(steve_node_api::Measurement::Delta)
            .into_iter()
            .map(|u| ModelUsage {
                model: o.model(u.model.as_deref().or(f.model.as_deref())),
                tokens: tokens_of(&u.tokens),
                cost_reported: None,
                detail: json!({ "scope": u.scope }),
            })
            .collect()
    } else {
        finals
            .iter()
            .map(|u| ModelUsage {
                model: o.model(u.model.as_deref().or(f.model.as_deref())),
                tokens: tokens_of(&u.tokens),
                cost_reported: None,
                detail: json!({ "scope": u.scope }),
            })
            .collect()
    };
    if let Some(cost) = f.cost.filter(|c| *c > 0.0) {
        if usage.is_empty() {
            usage.push(ModelUsage {
                model: o.model(f.model.as_deref()),
                tokens: Tokens::default(),
                cost_reported: None,
                detail: json!({ "scope": "main" }),
            });
        }
        usage[0].cost_reported = Some(cost);
    }
    let model = usage
        .first()
        .map(|u| u.model.clone())
        .unwrap_or_else(|| o.model(f.model.as_deref()));
    let r = store.stats().turn_finish(&steve_store::TurnFinish {
        turn_id,
        ended: at,
        day: day(store, ws, at),
        model,
        stop_reason: Some(c.stop_reason.clone()),
        error: c.error.as_ref().map(|e| e.safe_detail.clone()),
        transient: c.transient,
        tools: f.tools,
        tool_errors: f.tool_errors,
        tool_kinds: f.tool_kinds.clone(),
        permissions: f.permissions,
        usage,
    });
    if let Err(e) = r {
        tracing::error!("stats attempt finish {session}: {e}");
    }
}

#[cfg(test)]
#[allow(clippy::type_complexity)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use steve_store::{NewBot, NewSession, NoPublisher, WS};

    struct T {
        dir: tempfile::TempDir,
        store: Store,
    }

    fn fresh() -> T {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .bots()
            .seed(
                "steve",
                true,
                &NewBot {
                    name: "Steve".into(),
                    harness: "claude-code".into(),
                    model: Some("claude-opus-5-5".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        T { dir, store }
    }

    fn session(t: &T, item: Option<&str>, harness: &str) -> String {
        t.store
            .sessions()
            .create(&NewSession {
                workspace_id: WS.into(),
                item_id: item.map(Into::into),
                bot_id: Some("steve".into()),
                harness: harness.into(),
                model: None,
                reasoning_effort: None,
                cwd: t.dir.path().display().to_string(),
                title: "t".into(),
                token: "tok".into(),
            })
            .unwrap()
            .id
    }

    fn db(t: &T) -> Connection {
        Connection::open(t.dir.path().join("steve.sqlite3")).unwrap()
    }

    fn begin(t: &T, sid: &str) -> String {
        let s = t.store.sessions().get(sid).unwrap().unwrap();
        t.store
            .stats()
            .turn_begin(&steve_store::TurnBegin {
                workspace_id: s.workspace_id,
                session_id: s.id,
                item_id: s.item_id,
                bot_id: s.bot_id,
                project_id: None,
                harness: s.harness,
                user_seq: None,
                attempt: 0,
                started: 1_000,
            })
            .unwrap()
    }

    fn finish(t: &T, id: &str, cost: Option<f64>) {
        let usage = cost
            .map(|c| ModelUsage {
                model: "m".into(),
                tokens: Tokens::default(),
                cost_reported: Some(c),
                detail: json!({ "scope": "main" }),
            })
            .into_iter()
            .collect();
        t.store
            .stats()
            .turn_finish(&steve_store::TurnFinish {
                turn_id: id.into(),
                ended: 4_000,
                day: "1970-01-01".into(),
                model: "m".into(),
                stop_reason: Some("end_turn".into()),
                error: None,
                transient: false,
                tools: 2,
                tool_errors: 1,
                tool_kinds: BTreeMap::from([("execute".into(), 2)]),
                permissions: 0,
                usage,
            })
            .unwrap();
    }

    #[tokio::test]
    async fn codex_turns_without_tokens_get_them_from_the_rollout_once() {
        let t = fresh();
        let sid = session(&t, None, "codex");
        db(&t)
            .execute(
                "UPDATE sessions SET acp_session_id = 'cx-1' WHERE id = ?",
                [&sid],
            )
            .unwrap();
        let id = begin(&t, &sid);
        finish(&t, &id, None);
        let home = t.dir.path().join("codex");
        let day = home.join("sessions/1970/01/01");
        std::fs::create_dir_all(&day).unwrap();
        let tick = json!({"timestamp": "1970-01-01T00:00:02.000Z", "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": 1000, "cached_input_tokens": 900, "output_tokens": 50, "reasoning_output_tokens": 10},
            "last_token_usage": {"input_tokens": 1000, "cached_input_tokens": 900, "output_tokens": 50, "reasoning_output_tokens": 10}}}});
        std::fs::write(
            day.join("rollout-1970-01-01T00-00-00-cx-1.jsonl"),
            tick.to_string(),
        )
        .unwrap();
        assert_eq!(repair_codex(&t.store, &home).await, 1);
        assert_eq!(repair_codex(&t.store, &home).await, 0);
        let row: (i64, i64, Option<i64>, i64, i64) = db(&t)
            .query_row(
                "SELECT input, cache_read, cache_write, output, reasoning FROM usage_records WHERE turn_id = ?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        // OpenAI's input includes the cached part; the stats keep Anthropic's convention
        assert_eq!(row, (100, 900, None, 50, 10));
        let (hit_base, cache_read): (i64, i64) = db(&t)
            .query_row(
                "SELECT SUM(hit_base), SUM(cache_read) FROM stat_daily WHERE harness = 'codex'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((hit_base, cache_read), (1000, 900));
    }

    #[test]
    fn open_turns_are_interrupted_at_boot() {
        let t = fresh();
        let sid = session(&t, None, "claude-code");
        let id = begin(&t, &sid);
        // a turn run on a Node: the restart did not cut it
        let node = begin(&t, &sid);
        let c = db(&t);
        c.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        c.execute(
            "UPDATE agent_turns SET node_attempt_id = 'att_1' WHERE id = ?",
            [&node],
        )
        .unwrap();
        drop(c);
        interrupt_open(&t.store, 9_000);
        let open: Option<i64> = db(&t)
            .query_row("SELECT ended FROM agent_turns WHERE id = ?", [&node], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(open, None, "a Node turn is left to its attempt's end");
        let (ended, stop): (i64, String) = db(&t)
            .query_row(
                "SELECT ended, stop_reason FROM agent_turns WHERE id = ?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // no event of the session before the boot: it ends where it started, not at the boot
        assert_eq!((ended, stop.as_str()), (1_000, "interrupted"));
        // a late finish of the cut turn books nothing twice
        let _ = t.store.stats().turn_finish(&steve_store::TurnFinish {
            turn_id: id.clone(),
            ended: 9_500,
            day: "1970-01-01".into(),
            model: "m".into(),
            stop_reason: Some("end_turn".into()),
            error: None,
            transient: false,
            tools: 0,
            tool_errors: 0,
            tool_kinds: BTreeMap::new(),
            permissions: 0,
            usage: vec![ModelUsage {
                model: "m".into(),
                tokens: Tokens::default(),
                cost_reported: Some(1.0),
                detail: json!({ "scope": "main" }),
            }],
        });
        let n: i64 = db(&t)
            .query_row("SELECT count(*) FROM usage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}

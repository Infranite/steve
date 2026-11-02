//! The runner's turn reports into the statistics tables (stats.md §2.2): `agent_turns`, `usage_records`
//! and the `stat_daily` cube, through `steve_store::Stats`. Token figures are brought to one convention
//! with `steve_stats::normalize`; days are cut in the workspace's `stats.tz`.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;
use steve_store::{ModelUsage, NewUsage, Session, Store, Tokens};

/// Per session, the `events.seq` of the first `user` event written since the last turn began.
#[derive(Default)]
pub struct UserSeqs(Mutex<HashMap<String, i64>>);

impl UserSeqs {
    pub fn user_event(&self, session_id: &str, seq: i64) {
        self.lock().entry(session_id.to_owned()).or_insert(seq);
    }
    fn take(&self, session_id: &str) -> Option<i64> {
        self.lock().remove(session_id)
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, i64>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

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

pub fn turn_begin(store: &Store, seqs: &UserSeqs, b: steve_acp::TurnBegin) -> Option<String> {
    let seq = seqs.take(&b.session_id);
    let o = owner(store, &b.session_id)?;
    let r = store.stats().turn_begin(&steve_store::TurnBegin {
        workspace_id: o.s.workspace_id.clone(),
        session_id: o.s.id.clone(),
        item_id: o.s.item_id.clone(),
        bot_id: o.s.bot_id.clone(),
        project_id: o.project.clone(),
        harness: o.s.harness.clone(),
        user_seq: if b.user_events > 0 { seq } else { None },
        attempt: i64::from(b.attempt),
        started: b.started,
    });
    match r {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::error!("stats turn begin {}: {e}", b.session_id);
            None
        }
    }
}

pub fn turn_finish(store: &Store, f: steve_acp::TurnFinish) {
    let Some(o) = owner(store, &f.session_id) else {
        return;
    };
    let ws = &o.s.workspace_id;
    let codex_cache: bool = store
        .settings()
        .get(ws, "stats.codex_input_includes_cache", true)
        .unwrap_or(true);
    let mut usage: Vec<ModelUsage> = f
        .usage
        .iter()
        .map(|u| ModelUsage {
            model: o.model(u.model.as_deref().or(f.model.as_deref())),
            tokens: steve_stats::normalize(
                &o.s.harness,
                steve_stats::RawUsage::from_json(&u.raw),
                codex_cache,
            ),
            cost_reported: None,
            detail: json!({ "scope": u.scope }),
        })
        .collect();
    // the adapter's dollar increment goes on the turn's first row; a cost without tokens still gets one
    if let Some(c) = f.cost.filter(|c| *c > 0.0) {
        if usage.is_empty() {
            usage.push(ModelUsage {
                model: o.model(f.model.as_deref()),
                tokens: Tokens::default(),
                cost_reported: None,
                detail: json!({ "scope": "main" }),
            });
        }
        usage[0].cost_reported = Some(c);
    }
    let model = usage
        .first()
        .map(|u| u.model.clone())
        .unwrap_or_else(|| o.model(f.model.as_deref()));
    let r = store.stats().turn_finish(&steve_store::TurnFinish {
        turn_id: f.turn_id,
        ended: f.ended,
        day: day(store, ws, f.ended),
        model,
        stop_reason: Some(f.stop_reason),
        error: f.error,
        transient: f.transient,
        tools: f.tools,
        tool_errors: f.tool_errors,
        tool_kinds: f.tool_kinds,
        permissions: f.permissions,
        usage,
    });
    if let Err(e) = r {
        tracing::error!("stats turn finish {}: {e}", f.session_id);
    }
}

pub fn autonomous(store: &Store, a: steve_acp::AutonomousUsage) {
    if a.cost <= 0.0 {
        return;
    }
    let Some(o) = owner(store, &a.session_id) else {
        return;
    };
    let ws = o.s.workspace_id.clone();
    let r = store.stats().usage_add(&NewUsage {
        day: day(store, &ws, a.ts),
        workspace_id: ws,
        ts: a.ts,
        source: "agent".into(),
        purpose: "autonomous".into(),
        harness: o.s.harness.clone(),
        turn_id: None,
        session_id: Some(o.s.id.clone()),
        item_id: o.s.item_id.clone(),
        bot_id: o.s.bot_id.clone(),
        project_id: o.project.clone(),
        usage: ModelUsage {
            model: o.model(a.model.as_deref()),
            tokens: Tokens::default(),
            cost_reported: Some(a.cost),
            detail: json!({ "origin": a.origin }),
        },
        duration_ms: None,
        ok: true,
        attempts: 1,
    });
    if let Err(e) = r {
        tracing::error!("stats autonomous {}: {e}", a.session_id);
    }
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

#[cfg(test)]
#[allow(clippy::type_complexity)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use steve_acp::{AutonomousUsage, TurnBegin, TurnFinish, TurnUsage};
    use steve_store::{NewBot, NewItem, NewSession, NoPublisher, WS};

    struct T {
        dir: tempfile::TempDir,
        store: Store,
        seqs: UserSeqs,
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
        T {
            dir,
            store,
            seqs: UserSeqs::default(),
        }
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

    fn begin(t: &T, sid: &str, attempt: u32, user_events: usize) -> String {
        turn_begin(
            &t.store,
            &t.seqs,
            TurnBegin {
                session_id: sid.into(),
                attempt,
                user_events,
                started: 1_000,
            },
        )
        .unwrap()
    }

    fn finish(id: &str, sid: &str, usage: Vec<TurnUsage>, cost: Option<f64>) -> TurnFinish {
        TurnFinish {
            turn_id: id.into(),
            session_id: sid.into(),
            ended: 4_000,
            stop_reason: "end_turn".into(),
            error: None,
            transient: false,
            tools: 2,
            tool_errors: 1,
            tool_kinds: BTreeMap::from([("execute".into(), 2)]),
            permissions: 0,
            usage,
            model: None,
            cost,
        }
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
        let id = begin(&t, &sid, 0, 1);
        turn_finish(&t.store, finish(&id, &sid, vec![], None));
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
    fn a_chat_session_turn_is_recorded_with_the_bot_model_and_the_cost() {
        let t = fresh();
        let sid = session(&t, None, "claude-code");
        let seq = t
            .store
            .events()
            .add(&sid, "user", json!({"text": "hi"}))
            .unwrap();
        t.seqs.user_event(&sid, seq);
        let id = begin(&t, &sid, 0, 1);
        let raw = json!({"inputTokens": 3, "outputTokens": 4, "cachedReadTokens": 100, "cachedWriteTokens": 20});
        let u = vec![TurnUsage {
            model: None,
            raw,
            scope: "main",
        }];
        turn_finish(&t.store, finish(&id, &sid, u, Some(0.25)));
        let c = db(&t);
        let (item, user_seq, stop, tools): (Option<String>, Option<i64>, String, i64) = c
            .query_row(
                "SELECT item_id, user_seq, stop_reason, tools FROM agent_turns WHERE id = ?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (item, user_seq, stop.as_str(), tools),
            (None, Some(seq), "end_turn", 2)
        );
        let row: (String, i64, i64, i64, i64, f64, String) = c
            .query_row(
                "SELECT model, input, output, cache_read, cache_write, cost_reported, detail FROM usage_records WHERE turn_id = ?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "claude-opus-5-5".into(),
                3,
                4,
                100,
                20,
                0.25,
                r#"{"scope":"main"}"#.into()
            )
        );
        let (turns, hit, model): (i64, i64, String) = c
            .query_row(
                "SELECT sum(turns), sum(hit_base), max(model) FROM stat_daily",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((turns, hit, model.as_str()), (1, 123, "claude-opus-5-5"));
    }

    #[test]
    fn codex_input_loses_the_cache_and_retries_have_no_user_seq() {
        let t = fresh();
        let i = t
            .store
            .items()
            .create(
                WS,
                &NewItem {
                    title: "x".into(),
                    goal: "g".into(),
                    bot_id: "steve".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
            .id;
        let sid = session(&t, Some(&i), "codex");
        t.seqs.user_event(&sid, 7);
        let id = begin(&t, &sid, 1, 0);
        let u = vec![TurnUsage {
            model: Some("gpt-5".into()),
            raw: json!({"inputTokens": 1000, "outputTokens": 10, "cachedReadTokens": 800}),
            scope: "main",
        }];
        turn_finish(&t.store, finish(&id, &sid, u, None));
        let c = db(&t);
        let (item, seq, attempt): (Option<String>, Option<i64>, i64) = c
            .query_row(
                "SELECT item_id, user_seq, attempt FROM agent_turns WHERE id = ?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((item.as_deref(), seq, attempt), (Some(i.as_str()), None, 1));
        let (input, cr, cost): (i64, i64, Option<f64>) = c
            .query_row(
                "SELECT input, cache_read, cost_reported FROM usage_records WHERE turn_id = ?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((input, cr, cost), (200, 800, None));
    }

    #[test]
    fn a_cost_without_tokens_and_autonomous_cost_get_rows() {
        let t = fresh();
        let sid = session(&t, None, "claude-code");
        let id = begin(&t, &sid, 0, 0);
        turn_finish(&t.store, finish(&id, &sid, vec![], Some(0.1)));
        autonomous(
            &t.store,
            AutonomousUsage {
                session_id: sid.clone(),
                ts: 5_000,
                model: Some("claude-haiku-5-5".into()),
                origin: "task-notification".into(),
                cost: 0.05,
            },
        );
        let c = db(&t);
        let rows: Vec<(String, String, Option<i64>, f64)> = c
            .prepare("SELECT purpose, model, input, cost_reported FROM usage_records ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            rows,
            [
                ("turn".into(), "claude-opus-5-5".into(), None, 0.1),
                ("autonomous".into(), "claude-haiku-5-5".into(), None, 0.05)
            ]
        );
    }

    #[test]
    fn open_turns_are_interrupted_at_boot() {
        let t = fresh();
        let sid = session(&t, None, "claude-code");
        let id = begin(&t, &sid, 0, 0);
        interrupt_open(&t.store, 9_000);
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
        turn_finish(&t.store, finish(&id, &sid, vec![], Some(1.0)));
        let n: i64 = db(&t)
            .query_row("SELECT count(*) FROM usage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}

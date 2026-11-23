//! Agent turns into the statistics tables (stats.md §2.2): `agent_turns`, `usage_records`
//! and the `stat_daily` cube, through `steve_store::Stats`. Token figures are brought to one convention
//! with `steve_stats::normalize`; days are cut in the workspace's `stats.tz`.

#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use steve_store::{ModelUsage, Tokens};
use steve_store::{Session, Store};

/// Session attribution of a turn: workspace, item, bot, project, and the fallback model.
struct Owner {
    s: Session,
    project: Option<String>,
    bot_model: Option<String>,
}

fn owner(store: &Store, session_id: &str) -> steve_store::Result<Option<Owner>> {
    let Some(s) = store.sessions().get(session_id)? else {
        return Ok(None);
    };
    if s.readonly != 0 {
        return Ok(None);
    }
    let project = match s.item_id.as_deref() {
        Some(id) => store.items().get(id)?.and_then(|i| i.project_id),
        None => None,
    };
    let bot_model = match s.bot_id.as_deref() {
        Some(id) => store
            .bots()
            .get(id)?
            .and_then(|b| b.model)
            .filter(|m| !m.is_empty()),
        None => None,
    };
    Ok(Some(Owner {
        s,
        project,
        bot_model,
    }))
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
    if let Err(e) = try_node_attempt_started(store, session, attempt, ordinal, user_seq, at) {
        tracing::error!("stats attempt begin {session}: {e}");
    }
}

pub(crate) fn try_node_attempt_started(
    store: &Store,
    session: &str,
    attempt: &str,
    ordinal: i64,
    user_seq: Option<i64>,
    at: i64,
) -> steve_store::Result<()> {
    let Some(o) = owner(store, session)? else {
        return Ok(());
    };
    store.stats().node_turn_begin(
        &steve_store::TurnBegin {
            workspace_id: o.s.workspace_id,
            session_id: o.s.id,
            item_id: o.s.item_id,
            bot_id: o.s.bot_id,
            project_id: o.project,
            harness: o.s.harness,
            user_seq,
            attempt: ordinal,
            started: at,
        },
        attempt,
    )?;
    Ok(())
}

fn ensure_node_turn(store: &Store, session: &str, attempt: &str) -> steve_store::Result<()> {
    let missing: Option<(i64,i64)> = store.reader(|db| {
        use rusqlite::OptionalExtension;
        Ok::<_,steve_store::StoreError>(db.query_row(
            "SELECT a.ordinal,a.created FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.attempt_id=?1 AND t.session_id=?2 AND NOT EXISTS(SELECT 1 FROM agent_turns WHERE node_attempt_id=?1)",
            [attempt,session], |r|Ok((r.get(0)?,r.get(1)?))).optional()?)
    })?;
    if let Some((ordinal, started)) = missing {
        try_node_attempt_started(store, session, attempt, ordinal, None, started)?;
    }
    Ok(())
}

/// Persist one report before its Node event is marked applied. Used in the coordinator's write
/// transaction; replays and the eventual finish use the same report identity.
pub(crate) fn node_usage(
    store: &Store,
    session: &str,
    attempt: &str,
    report: &steve_node_api::UsageReport,
) -> steve_store::Result<()> {
    let Some(o) = owner(store, session)? else {
        return Ok(());
    };
    ensure_node_turn(store, session, attempt)?;
    let turn: Option<(String, i64)> = store.reader(|c| {
        use rusqlite::OptionalExtension;
        Ok::<_, steve_store::StoreError>(
            c.query_row(
                "SELECT id,started FROM agent_turns WHERE node_attempt_id=?",
                [attempt],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
        )
    })?;
    if let Some((id, started)) = turn {
        // Admission already rebased context-only meters. Keep refusal/currency evidence
        // for the shared reducer; filtering it here would hide an unknown monetary sum.
        store.stats().usage_report(
            &id,
            report,
            &o.model(None),
            &day(store, &o.s.workspace_id, started),
        )?;
    }
    Ok(())
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
    if let Err(e) = try_node_attempt_finished(store, session, attempt, c, f, at)
        .map(|m| m.inspect(steve_store::instrumentation::ModelCall::record))
    {
        tracing::error!("stats attempt finish {session}: {e}");
    }
}

/// Books the end of a Node attempt; what it is for `steve_model_*` (recorded once the caller's
/// transaction committed), when it had a statistics row.
pub(crate) fn try_node_attempt_finished(
    store: &Store,
    session: &str,
    attempt: &str,
    c: &steve_node_api::Completion,
    f: &super::node_runtime::AttemptFacts,
    at: i64,
) -> steve_store::Result<Option<steve_store::instrumentation::ModelCall>> {
    let Some(o) = owner(store, session)? else {
        return Ok(None);
    };
    let turn: Option<(String, i64)> = store.reader(|db| {
        use rusqlite::OptionalExtension;
        Ok::<_, steve_store::StoreError>(
            db.query_row(
                "SELECT id, started FROM agent_turns WHERE node_attempt_id=?",
                [attempt],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?,
        )
    })?;
    let Some((turn_id, started)) = turn else {
        return Ok(None);
    };
    let ws = &o.s.workspace_id;
    let model = o.model(f.model.as_deref());
    let measure = steve_store::instrumentation::ModelCall {
        source: "agent",
        harness: o.s.harness.clone(),
        model: model.clone(),
        result: turn_result(c),
        seconds: at.saturating_sub(started).max(0) as f64 / 1000.0,
        usage: turn_usage(store, &turn_id)?,
    };
    store.stats().turn_finish(&steve_store::TurnFinish {
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
        usage: vec![],
    })?;
    Ok(Some(measure))
}

/// `ok`, or why the attempt did not succeed: its error's code, else its outcome.
fn turn_result(c: &steve_node_api::Completion) -> &'static str {
    use steve_node_api::Outcome;
    match (&c.outcome, &c.error) {
        (Outcome::Succeeded, _) => "ok",
        (_, Some(e)) => e.code.as_str(),
        (Outcome::Failed, None) => "failed",
        (Outcome::Cancelled, None) => "cancelled",
        (Outcome::TimedOut, None) => "timed_out",
        (Outcome::Interrupted, None) => "interrupted",
    }
}

/// The turn's tokens and cost by model, as its usage reports left them.
fn turn_usage(store: &Store, turn: &str) -> steve_store::Result<Vec<steve_store::ModelUsage>> {
    store.reader(|db| {
        let mut st = db.prepare_cached(
            "SELECT model, input, output, cache_read, cache_write, reasoning, cost_reported FROM usage_records WHERE live_turn_id=?",
        )?;
        let rows = st
            .query_map([turn], |r| {
                Ok(steve_store::ModelUsage {
                    model: r.get(0)?,
                    tokens: steve_store::Tokens {
                        input: r.get(1)?,
                        output: r.get(2)?,
                        cache_read: r.get(3)?,
                        cache_write: r.get(4)?,
                        reasoning: r.get(5)?,
                    },
                    cost_reported: r.get(6)?,
                    detail: serde_json::Value::Null,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, steve_store::StoreError>(rows)
    })
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

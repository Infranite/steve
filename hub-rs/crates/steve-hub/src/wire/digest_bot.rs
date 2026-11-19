//! Queue-driven starts reuse the ordinary schedule engine and item/ACP execution path. No model
//! gateway, process, file or backend-specific call belongs here.
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_digest::{bot, conv, queue};
use steve_flow::{FlowCx, policy::OverlapPolicy, runs, schedules};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};

pub fn routes(flow: Arc<FlowCx>) -> Router<Modules> {
    Router::new()
        .route("/digest/bot", get(read).put(bind))
        .layer(Extension(flow))
}
fn owner(m: &Modules, who: &Caller) -> ApiResult<()> {
    if m.store.reader(|c| conv::owner(c, &who.ws))?.as_deref() != Some(who.me.as_str()) {
        return Err(ApiError::Forbidden(
            "Only the digest data owner can configure its Bot".into(),
        ));
    }
    Ok(())
}
async fn read(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        Ok(Json(json!({"schedule_id": m.store.reader(|c| bot::schedule(c, &who.ws))?, "template": bot::template()})))
    }).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bind {
    schedule_id: Option<String>,
}
async fn bind(
    State(m): State<Modules>,
    Extension(flow): Extension<Arc<FlowCx>>,
    who: Caller,
    Body(b): Body<Bind>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        m.store.transaction(|tx| {
            if let Some(id) = &b.schedule_id {
                let s = schedules::own(tx, &who.ws, id)?;
                schedules::check_refs(tx, &who.ws, &s.spec)?;
                if !matches!(s.spec.mode, schedules::Mode::Item | schedules::Mode::ItemAuto)
                    || s.spec.requester.as_deref() != Some(who.me.as_str()) || s.spec.overlap != OverlapPolicy::Skip {
                    return Err(ApiError::rule("Bind an item-mode schedule with requester set to your user id and overlap=skip"));
                }
                tx.execute("INSERT OR REPLACE INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)", params![who.ws, bot::SCHEDULE, id, m.clock.now()])?;
            } else {
                tx.execute("DELETE FROM settings WHERE workspace_id = ? AND key = ?", params![who.ws, bot::SCHEDULE])?;
            }
            Ok::<_, ApiError>(())
        })?;
        flow.wake();
        Ok(Json(json!({"schedule_id": b.schedule_id})))
    }).await
}

/// The ordinary flow scheduler remains the Bot's execution and policy boundary.
struct Runner(Arc<FlowCx>);

fn runnable(c: &rusqlite::Connection, ws: &str) -> ApiResult<Option<(String, i64)>> {
    let Some(id) = bot::schedule(c, ws)? else {
        return Ok(None);
    };
    let Some(s) = schedules::get(c, ws, &id)? else {
        return Ok(None);
    };
    if !s.enabled
        || !matches!(
            s.spec.mode,
            schedules::Mode::Item | schedules::Mode::ItemAuto
        )
        || s.spec.overlap != OverlapPolicy::Skip
        || s.spec.requester.is_none()
        || s.spec.requester != conv::owner(c, ws)?
    {
        return Ok(None);
    }
    if let Err(e) = schedules::check_refs(c, ws, &s.spec) {
        tracing::debug!(workspace = ws, "digest Bot binding: {e}");
        return Ok(None);
    }
    let busy: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM flow_runs r LEFT JOIN items i ON i.id = r.work_item_id
        WHERE r.workspace_id = ? AND r.flow_id = ? AND (r.status IN ('running', 'waiting', 'retry')
          OR (i.id IS NOT NULL AND i.archived = 0 AND i.status NOT IN ('done', 'drop'))))",
        params![ws, id],
        |r| r.get(0),
    )?;
    if busy {
        return Ok(None);
    }
    // A skipped/failed/empty run must not immediately start again while packets still wait.
    // All runs (including cron/manual starts) share this minute, using durable run history.
    let last: Option<i64> = c.query_row(
        "SELECT max(started) FROM flow_runs WHERE workspace_id = ? AND flow_id = ?",
        params![ws, id],
        |r| r.get(0),
    )?;
    Ok(Some((
        id,
        last.map_or(i64::MIN, |at| at.saturating_add(60_000)),
    )))
}

impl Runner {
    fn failed_start(
        &self,
        ws: &str,
        id: &str,
        now: i64,
        error: ApiError,
    ) -> Result<(), steve_store::StoreError> {
        // ApiError::Internal is the flow API's storage/bug boundary, not a row rejection.
        if let ApiError::Internal(reason) = error {
            return Err(steve_store::StoreError::Hook(reason));
        }
        let reason = error.to_string();
        self.0
            .store()
            .transaction(|tx| -> ApiResult<()> {
                // A concurrent removal makes this binding unavailable; there is no start to retry.
                let Some(schedule) = schedules::get(tx, ws, id)? else {
                    return Ok(());
                };
                let run = runs::insert(
                    tx,
                    &runs::NewRun {
                        ws,
                        flow_id: id,
                        flow_version: schedule.version,
                        trigger_kind: "manual",
                        status: "failed",
                        outcome_label: &reason,
                        cursor: None,
                        context: &json!({"at":now,"trigger":"manual","source":"digest.bot"}),
                        now,
                    },
                )?;
                tx.execute(
                    "UPDATE flow_runs SET error = ? WHERE id = ?",
                    params![reason, run],
                )?;
                Ok(())
            })
            .map_err(|e| steve_store::StoreError::Hook(e.to_string()))?;
        tracing::warn!(
            workspace = ws,
            schedule = id,
            "digest Bot start failed: {reason}"
        );
        Ok(())
    }
}

impl queue::BotRunner for Runner {
    fn ready(
        &self,
        c: &rusqlite::Connection,
        ws: &str,
    ) -> Result<Option<i64>, steve_store::StoreError> {
        runnable(c, ws)
            .map(|s| s.map(|(_, at)| at))
            .map_err(|e| steve_store::StoreError::Hook(e.to_string()))
    }
    fn start(&self, ws: &str, now: i64) -> Result<(), steve_store::StoreError> {
        let due = self
            .0
            .store()
            .reader(|c| runnable(c, ws))
            .map_err(|e| steve_store::StoreError::Hook(e.to_string()))?;
        if let Some((id, at)) = due
            && at <= now
        {
            match steve_flow::scheduler::start_manual(&self.0, ws, &id) {
                Ok((run, true)) => {
                    drop(steve_flow::scheduler::spawn_drive(self.0.clone(), run));
                }
                Ok((_, false)) => {}
                Err(error) => self.failed_start(ws, &id, now, error)?,
            }
        }
        Ok(())
    }
}

/// Tables read by BotSource and runnable/check_refs, including schedule reference checks.
/// This is only a recheck hint; queue and policy decisions always read the store again.
fn wake(store: &steve_store::Store) -> Arc<tokio::sync::Notify> {
    store.commit_wake(queue::BOT_WAKE_TABLES)
}

/// Packet-count/age and lease deadlines, woken by committed changes to their input tables.
pub async fn run(m: Modules, flow: Arc<FlowCx>) {
    let wake = wake(&m.store);
    let source = Arc::new(queue::BotSource::new(&m, Runner(flow)));
    steve_runtime::due::due_loop(
        source,
        m.clock.clone(),
        wake,
        steve_runtime::policy::Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
            factor: 2,
            jitter: steve_runtime::policy::Jitter::None,
            attempts: None,
            reset_after: None,
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use steve_flow::testkit::FlowEnv;
    use steve_modules::testkit::{ME, OTHER, WS, call, caller};

    async fn bound() -> (FlowEnv, String) {
        let e = FlowEnv::new();
        let (st, schedule) = call(
            &e.router(),
            Some(&caller(WS, ME)),
            "POST",
            "/schedules",
            Some(&json!({
                "name":"Digest", "cron":"*/30 * * * *", "what":"Organize", "bot":"steve",
                "mode":"item_auto", "overlap":"skip", "requester":ME,
            })),
        )
        .await;
        assert_eq!(st, 200, "{schedule}");
        let id = schedule["id"].as_str().unwrap().to_string();
        let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
        let changed = wake(e.env.store());
        let (st, v) = call(
            &api,
            Some(&caller(WS, ME)),
            "PUT",
            "/digest/bot",
            Some(&json!({"schedule_id":id})),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        assert!(changed.notified().now_or_never().is_some());
        (e, id)
    }

    fn packet(e: &FlowEnv, ws: &str, at: i64) -> String {
        e.env.store().transaction(|tx| -> Result<_, steve_store::StoreError> {
            tx.execute("INSERT OR IGNORE INTO workspaces (id,name,created) VALUES (?, 'digest test', 1)", [ws])?;
            tx.execute("INSERT OR REPLACE INTO settings (workspace_id,key,value,updated) VALUES (?, 'digest', '{\"every_min\":1,\"packets\":1000}', 1)", [ws])?;
            queue::insert(tx, &queue::NewPacket {
                ws, conv: "space:fixture", seq: 1, kind: "window", trigger: "quiet",
                first_id: "m", last_id: "m", first_at: at, last_at: at,
                count: 1, priority: 1, facts: &json!({}), candidates: &json!([]),
            }, &[], at - 60_000)
        }).unwrap()
    }

    // WS uses the production runner against a graph that always rejects startup. Other
    // workspaces replace only the consumer, keeping the same source and durable packet queue.
    struct PartlyBroken(Runner);
    impl queue::BotRunner for PartlyBroken {
        fn ready(
            &self,
            c: &rusqlite::Connection,
            ws: &str,
        ) -> Result<Option<i64>, steve_store::StoreError> {
            if ws == WS {
                self.0.ready(c, ws)
            } else {
                Ok(Some(i64::MIN))
            }
        }
        fn start(&self, ws: &str, now: i64) -> Result<(), steve_store::StoreError> {
            if ws == WS {
                return self.0.start(ws, now);
            }
            self.0
                .0
                .store()
                .transaction(|tx| -> Result<(), steve_store::StoreError> {
                    let b = queue::batching(tx, ws)?;
                    let (_, ids) = queue::lease(tx, ws, now, &b, 10)?;
                    for id in ids {
                        let p = queue::get(tx, ws, &id)?.unwrap();
                        queue::packet_done(tx, &p, true, None, &b, now)?;
                    }
                    Ok(())
                })
        }
    }

    #[tokio::test]
    async fn a_broken_schedule_defers_only_itself_and_healthy_workspaces_keep_their_deadlines() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        let now = e.env.clock.now();
        let bad = packet(&e, WS, now);
        let healthy = packet(&e, "ws_digest_healthy", now);
        let future = packet(&e, "ws_digest_future", now + 17_123);
        e.env
            .store()
            .with_conn(|c| {
                c.execute("UPDATE flows SET graph = 'broken json' WHERE id = ?", [&id])?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            steve_flow::scheduler::start_manual(&e.flow, WS, &id),
            Err(ApiError::Rule(_))
        ));
        let source = queue::BotSource::new(&e.env.modules, PartlyBroken(Runner(e.flow.clone())));
        let changed = wake(e.env.store());
        assert_eq!(source.next_due().await.unwrap(), Some(now));
        source
            .run_due(now)
            .await
            .expect("a row rejection must not back off the source");
        assert!(
            changed.notified().now_or_never().is_some(),
            "the failed run writes a production wake table"
        );
        let history = || {
            e.env
                .store()
                .reader(|c| runs::page(c, WS, &id, None, 10))
                .unwrap()
        };
        let rows = history();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[0].ended, Some(now));
        assert!(
            rows[0]
                .error
                .as_deref()
                .unwrap()
                .contains("Invalid stored flow graph")
        );
        assert_eq!(
            e.env.store().reader(|c| runnable(c, WS)).unwrap(),
            Some((id.clone(), now + 60_000))
        );
        let state = |ws: &str, packet: &str| {
            e.env
                .store()
                .reader(|c| queue::get(c, ws, packet))
                .unwrap()
                .unwrap()
                .state
        };
        assert_eq!(state(WS, &bad), "queued");
        assert_eq!(state("ws_digest_healthy", &healthy), "done");
        assert_eq!(source.next_due().await.unwrap(), Some(now + 17_123));
        source.run_due(now).await.unwrap();
        assert_eq!(
            history().len(),
            1,
            "own commit hints cannot cause another failed start"
        );
        e.env.clock.advance(17_123);
        source.run_due(now + 17_123).await.unwrap();
        assert_eq!(state("ws_digest_future", &future), "done");
        assert_eq!(source.next_due().await.unwrap(), Some(now + 60_000));
        e.env.clock.advance(60_000 - 17_123);
        source.run_due(now + 60_000).await.unwrap();
        assert_eq!(
            history().len(),
            2,
            "the still-broken graph gets one new attempt per minute"
        );
        assert_eq!(source.next_due().await.unwrap(), Some(now + 120_000));
    }

    #[tokio::test]
    async fn failure_to_persist_a_rejected_start_remains_a_source_error() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        let now = e.env.clock.now();
        packet(&e, WS, now);
        e.env.store().with_conn(|c| {
            c.execute("UPDATE flows SET graph = 'broken json' WHERE id = ?", [&id])?;
            c.execute_batch("CREATE TRIGGER reject_failed_run BEFORE INSERT ON flow_runs WHEN NEW.status = 'failed' BEGIN SELECT RAISE(ABORT, 'cannot persist failed start'); END")?;
            Ok(())
        }).unwrap();
        let source = queue::BotSource::new(&e.env.modules, Runner(e.flow.clone()));
        let error = source.run_due(now).await.unwrap_err();
        assert!(error.0.contains("cannot persist failed start"));
        assert!(
            e.env
                .store()
                .reader(|c| runs::page(c, WS, &id, None, 10))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn runnable_excludes_disabled_busy_and_ownerless_bindings_and_spaces_restarts() {
        let (e, id) = bound().await;
        let changed = wake(e.env.store());
        let assert_changed = || assert!(changed.notified().now_or_never().is_some());
        let read = || e.env.store().reader(|c| runnable(c, WS)).unwrap();
        assert_eq!(read(), Some((id.clone(), i64::MIN)));
        e.env
            .store()
            .with_conn(|c| {
                c.execute("UPDATE flows SET enabled = 0 WHERE id = ?", [&id])?;
                Ok(())
            })
            .unwrap();
        assert_changed();
        assert!(read().is_none());
        e.env.store().with_conn(|c| {
            c.execute("UPDATE flows SET enabled = 1 WHERE id = ?", [&id])?;
            c.execute("INSERT INTO flow_runs (id,workspace_id,flow_id,flow_version,trigger_kind,status,started)
                VALUES ('digest_busy',?, ?,1,'manual','running',1000)", params![WS,id])?;
            Ok(())
        }).unwrap();
        assert_changed();
        assert!(read().is_none());
        e.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "UPDATE flow_runs SET status = 'done' WHERE id = 'digest_busy'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_changed();
        assert_eq!(
            read(),
            Some((id, 61_000)),
            "an empty/failed start cannot spin on queued packets"
        );
        e.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "DELETE FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                    params![WS, ME],
                )?;
                Ok(())
            })
            .unwrap();
        assert_changed();
        assert!(read().is_none());
    }

    #[tokio::test]
    async fn binding_requires_owner_and_explicit_personal_item_schedule() {
        let e = FlowEnv::new();
        let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
        let (st, v) = call(&api, Some(&caller(WS, ME)), "GET", "/digest/bot", None).await;
        assert_eq!(st, 200);
        assert!(
            v["template"]["bot"]["rules"]
                .as_str()
                .unwrap()
                .contains("steve_tool_call")
        );
        assert_eq!(
            call(&api, Some(&caller(WS, OTHER)), "GET", "/digest/bot", None)
                .await
                .0,
            403
        );
        for requester in [None, Some(ME)] {
            let mut body = json!({"name": "Digest", "cron": "*/30 * * * *", "what": "Organize", "bot": "steve", "mode": "item_auto", "overlap": "skip"});
            if let Some(user) = requester {
                body["requester"] = json!(user);
            }
            let (st, schedule) = call(
                &e.router(),
                Some(&caller(WS, ME)),
                "POST",
                "/schedules",
                Some(&body),
            )
            .await;
            assert_eq!(st, 200, "{schedule}");
            let bind = json!({"schedule_id": schedule["id"]});
            let (st, _) = call(
                &api,
                Some(&caller(WS, ME)),
                "PUT",
                "/digest/bot",
                Some(&bind),
            )
            .await;
            assert_eq!(st, if requester.is_some() { 200 } else { 400 });
            assert_eq!(
                call(
                    &api,
                    Some(&caller(WS, OTHER)),
                    "PUT",
                    "/digest/bot",
                    Some(&bind)
                )
                .await
                .0,
                403
            );
        }
        assert!(
            e.env
                .store()
                .reader(|c| bot::schedule(c, WS))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            call(
                &api,
                Some(&caller(WS, ME)),
                "PUT",
                "/digest/bot",
                Some(&json!({"schedule_id": null}))
            )
            .await
            .0,
            200
        );
        assert!(
            e.env
                .store()
                .reader(|c| bot::schedule(c, WS))
                .unwrap()
                .is_none()
        );
    }
}

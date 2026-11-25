//! Queue-driven starts reuse the ordinary schedule engine and item/ACP execution path. No model
//! gateway, process, file or backend-specific call belongs here.
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_digest::{bot, queue};
use steve_flow::{FlowCx, policy::OverlapPolicy, runs, schedules};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};

pub fn routes(flow: Arc<FlowCx>) -> Router<Modules> {
    Router::new()
        .route("/digest/bot", get(read).put(bind))
        .layer(Extension(flow))
}
fn owner(m: &Modules, who: &Caller) -> ApiResult<()> {
    if m.store.read(|r| r.members().owner(&who.ws))?.as_deref() != Some(who.me.as_str()) {
        return Err(ApiError::Forbidden(
            "Only the digest data owner can configure its Bot".into(),
        ));
    }
    Ok(())
}
async fn read(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let id = m.store.read(|r| r.digest_packets().schedule(&who.ws))?;
        let diagnostic = match &id {
            Some(id) => m.store.read::<_, steve_store::StoreError>(|r| Ok(runs::startup_diagnostic(r, &who.ws, id, BOT_SOURCE)))??,
            None => None,
        };
        Ok(Json(json!({"schedule_id": id, "template": bot::template(), "diagnostics": diagnostic.into_iter().collect::<Vec<_>>()})))
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
        m.store.write(|w| {
            let now = m.clock.now();
            let previous = w.digest_packets().schedule(&who.ws)?;
            if let Some(id) = &b.schedule_id {
                let s = schedules::own(w, &who.ws, id)?;
                schedules::check_refs(w, &who.ws, &s.spec)?;
                if !matches!(s.spec.mode, schedules::Mode::Item | schedules::Mode::ItemAuto)
                    || s.spec.requester.as_deref() != Some(who.me.as_str()) || s.spec.overlap != OverlapPolicy::Skip {
                    return Err(ApiError::rule("Bind an item-mode schedule with requester set to your user id and overlap=skip"));
                }
            }
            w.digest_packets().bind_schedule(&who.ws, b.schedule_id.as_deref(), now)?;
            if let Some(prev) = previous.as_deref()
                && previous.as_deref() != b.schedule_id.as_deref()
            {
                schedules::set_time_trigger_enabled(w, &who.ws, prev, true, now)?;
            }
            if let Some(id) = b.schedule_id.as_deref() {
                // Queue-driven starts via `digest.bot` replace blind wall-clock cron polling.
                schedules::set_time_trigger_enabled(w, &who.ws, id, false, now)?;
            }
            Ok::<_, ApiError>(())
        })?;
        flow.wake();
        Ok(Json(json!({"schedule_id": b.schedule_id})))
    }).await
}

/// The ordinary flow scheduler remains the Bot's execution and policy boundary.
struct Runner(
    Arc<FlowCx>,
    std::sync::Mutex<std::collections::HashSet<String>>,
);
const BOT_SOURCE: &str = "digest.bot";
const START_LIMIT: i64 = 3;
/// Minimum cooldown between consecutive Digest Bot starts so failed/empty runs cannot spin.
const MIN_START_GAP_MS: i64 = 60_000;

impl Runner {
    fn new(flow: Arc<FlowCx>) -> Self {
        Self(flow, Default::default())
    }

    fn runnable(
        &self,
        c: &mut impl steve_store::Read,
        ws: &str,
        id: &str,
        owner: Option<&str>,
    ) -> ApiResult<Option<i64>> {
        let Some(s) = schedules::get(c, ws, id)? else {
            return Ok(None);
        };
        if !s.enabled
            || !matches!(
                s.spec.mode,
                schedules::Mode::Item | schedules::Mode::ItemAuto
            )
            || s.spec.overlap != OverlapPolicy::Skip
            || s.spec.requester.is_none()
            || s.spec.requester.as_deref() != owner
        {
            return Ok(None);
        }
        if let Err(e) = schedules::check_refs(c, ws, &s.spec) {
            tracing::debug!(workspace = ws, "digest Bot binding: {e}");
            return Ok(None);
        }
        use steve_store::{ItemReads, RunRows};
        let items = RunRows(c).work_items(ws, id)?;
        if RunRows(c).active(ws, id)? || ItemReads(c).unfinished(ws, &items)? {
            return Ok(None);
        }
        let Some(revision) = steve_flow::scheduler::startup_revision(c, ws, id)? else {
            return Ok(None);
        };
        let result = runs::startup_check(c, ws, id, BOT_SOURCE, &revision)?;
        match result {
            runs::StartupCheck::State(Some(state)) if state.state == runs::StartupStatus::Dead => {
                return Ok(None);
            }
            runs::StartupCheck::Invalid(diagnostic) => {
                // Observability only: never use this set as source liveness or retry state.
                let first = self
                    .1
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(diagnostic.run_id.clone());
                if first {
                    tracing::warn!(workspace = ws, flow = %id, run = %diagnostic.run_id,
                    code = diagnostic.code, "digest Bot startup metadata is damaged");
                }
                return Ok(None);
            }
            runs::StartupCheck::State(_) => {}
        }
        // A skipped/failed/empty run must not immediately start again while packets still wait.
        // All runs (including cron/manual starts) share this minute, using durable run history.
        let last = RunRows(c).latest_start(ws, id)?;
        Ok(Some(last.map_or(i64::MIN, |at| {
            at.saturating_add(MIN_START_GAP_MS)
        })))
    }
}

impl queue::BotRunner for Runner {
    fn ready(&self, ws: &str) -> Result<Option<i64>, steve_store::StoreError> {
        let store = self.0.store();
        store.read(|r| {
            let id = r.digest_packets().schedule(ws)?;
            let owner = r.members().owner(ws)?;
            let Some(id) = id else {
                return Ok(None);
            };
            self.runnable(r, ws, &id, owner.as_deref())
                .map_err(|e| steve_store::StoreError::Hook(e.to_string()))
        })
    }
    fn start(&self, ws: &str, now: i64) -> Result<(), steve_store::StoreError> {
        let store = self.0.store();
        let Some(id) = store.read(|r| r.digest_packets().schedule(ws))? else {
            return Ok(());
        };
        let outcome = steve_flow::scheduler::start_from_source(
            &self.0,
            ws,
            &id,
            BOT_SOURCE,
            START_LIMIT,
            now,
            |w| {
                let bound = w.digest_packets().schedule(ws)?;
                let owner = w.members().owner(ws)?;
                Ok(bound.as_deref() == Some(id.as_str())
                    && self
                        .runnable(w, ws, &id, owner.as_deref())?
                        .is_some_and(|at| at <= now))
            },
        )?;
        if let Some((run, true)) = outcome {
            drop(steve_flow::scheduler::spawn_drive(self.0.clone(), run));
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
    let backoff = steve_runtime::policy::Backoff {
        base: Duration::from_secs(1),
        max: Duration::from_secs(60),
        factor: 2,
        jitter: steve_runtime::policy::Jitter::None,
        attempts: None,
        reset_after: None,
    };
    let source = Arc::new(queue::BotSource::new(&m, Runner::new(flow)));
    steve_runtime::due::due_loop(source, m.clock.clone(), wake, backoff).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use steve_digest::queue::BotRunner;
    use steve_flow::testkit::FlowEnv;
    use steve_modules::testkit::{ME, OTHER, WS, call, caller};
    use steve_store::testkit::{flow as flow_fixture, hub::digest as fixtures};

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
        e.env
            .store()
            .write(|w| -> steve_store::Result<_> {
                fixtures::configure_packet(w, ws)?;
                w.digest_packets().insert(
                    &queue::NewPacket {
                        ws,
                        conv: "space:fixture",
                        seq: 1,
                        kind: "window",
                        trigger: "quiet",
                        first_id: "m",
                        last_id: "m",
                        first_at: at,
                        last_at: at,
                        count: 1,
                        priority: 1,
                        facts: &json!({}),
                        candidates: &json!([]),
                    },
                    &[],
                    at - 60_000,
                )
            })
            .unwrap()
    }

    // WS uses the production runner against a graph that always rejects startup. Other
    // workspaces replace only the consumer, keeping the same source and durable packet queue.
    struct PartlyBroken(Runner);
    impl queue::BotRunner for PartlyBroken {
        fn ready(&self, ws: &str) -> Result<Option<i64>, steve_store::StoreError> {
            if ws == WS {
                self.0.ready(ws)
            } else {
                Ok(Some(i64::MIN))
            }
        }
        fn start(&self, ws: &str, now: i64) -> Result<(), steve_store::StoreError> {
            if ws == WS {
                return self.0.start(ws, now);
            }
            self.0.0.store().write(|w| -> steve_store::Result<()> {
                let b = w.digest_packets().batching(ws)?;
                let (_, ids) = w
                    .digest_packets()
                    .lease_matching(ws, now, &b, 10, |_, _| Ok(true))?;
                for id in ids {
                    let p = w.digest_packets().get(ws, &id)?.unwrap();
                    w.digest_packets().packet_done(&p, true, None, &b, now)?;
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
        flow_fixture::set_raw_flow_graph(e.env.store(), &id, "broken json").unwrap();
        assert!(matches!(
            steve_flow::scheduler::start_manual(&e.flow, WS, &id),
            Err(ApiError::Rule(_))
        ));
        let source =
            queue::BotSource::new(&e.env.modules, PartlyBroken(Runner::new(e.flow.clone())));
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
                .read(|c| runs::page(c, WS, &id, None, 10))
                .unwrap()
        };
        let rows = history();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[0].ended, Some(now));
        assert_eq!(rows[0].error.as_deref(), Some(runs::START_REJECTED_CODE));
        assert_eq!(
            rows[0].context["start_error"],
            json!({"code":runs::START_REJECTED_CODE,"params":{}})
        );
        assert_eq!(
            Runner::new(e.flow.clone()).ready(WS).unwrap(),
            Some(now + 60_000)
        );
        let state = |ws: &str, packet: &str| {
            e.env
                .store()
                .read(|r| r.digest_packets().get(ws, packet))
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
        let repeated = history();
        assert_eq!(
            repeated.len(),
            1,
            "consecutive failures share one history entry"
        );
        assert_eq!(repeated[0].id, rows[0].id);
        assert_eq!(repeated[0].context["start_failures"], 2);
        assert_eq!(repeated[0].started, now + 60_000);
        assert_eq!(steve_flow::runs::view(&repeated[0]).at, now + 60_000);
        assert_eq!(source.next_due().await.unwrap(), Some(now + 120_000));
    }

    #[tokio::test]
    async fn a_day_of_broken_schedule_starts_keeps_one_counted_history_entry() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        let now = e.env.clock.now();
        packet(&e, WS, now);
        flow_fixture::set_raw_flow_graph(e.env.store(), &id, "broken json").unwrap();
        let source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
        source.run_due(now).await.unwrap();
        let history = || {
            e.env
                .store()
                .read(|c| runs::page(c, WS, &id, None, 50))
                .unwrap()
        };
        let first = history().pop().unwrap();
        for minute in 1..1440 {
            e.env.clock.advance(60_000);
            source.run_due(now + minute * 60_000).await.unwrap();
        }
        let rows = history();
        assert_eq!(
            rows.len(),
            1,
            "1440 failed starts must not fill the run list"
        );
        let last = &rows[0];
        assert_eq!(last.id, first.id);
        assert_eq!(last.context["start_failures"], START_LIMIT);
        assert_eq!(last.context["first_at"], now);
        let view = steve_flow::runs::view(last);
        assert_eq!(view.start_failures, Some(START_LIMIT));
        assert_eq!(
            serde_json::to_value(&view).unwrap()["start_failures"],
            START_LIMIT
        );
        assert_eq!(last.error.as_deref(), Some(runs::START_REJECTED_CODE));
        assert_eq!(
            last.context["start_error"],
            json!({"code":runs::START_REJECTED_CODE,"params":{}})
        );
        assert_eq!(
            view.error.as_deref(),
            Some(steve_flow::scheduler::start_rejected().as_str())
        );
        assert_eq!(
            last.attempts, 0,
            "flow action retry attempts are independent"
        );
        assert_eq!(last.started, now + (START_LIMIT - 1) * 60_000);
        assert_eq!(last.ended, Some(last.started));
        assert_eq!(
            steve_flow::runs::view(last).at,
            last.started,
            "the run list shows the latest failure time"
        );
        assert_eq!(source.next_due().await.unwrap(), None);
        source.run_due(e.env.clock.now()).await.unwrap();
        assert_eq!(
            history(),
            rows,
            "an own commit at the same wall time cannot add a failure"
        );
    }

    #[tokio::test]
    async fn failure_to_update_a_coalesced_start_remains_a_source_error_and_rolls_back() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        let now = e.env.clock.now();
        packet(&e, WS, now);
        flow_fixture::set_raw_flow_graph(e.env.store(), &id, "broken json").unwrap();
        let source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
        source.run_due(now).await.unwrap();
        let before = e
            .env
            .store()
            .read(|c| runs::page(c, WS, &id, None, 50))
            .unwrap();
        steve_store::testkit::hub::mutations::create_reject_failed_update(e.env.store()).unwrap();
        e.env.clock.advance(60_000);
        let error = source.run_due(now + 60_000).await.unwrap_err();
        assert!(error.0.contains("cannot update failed start"));
        assert_eq!(
            e.env
                .store()
                .read(|c| runs::page(c, WS, &id, None, 50))
                .unwrap(),
            before,
            "failed updates cannot partly change the count, timestamp or history"
        );
    }

    #[tokio::test]
    async fn failure_to_persist_a_rejected_start_remains_a_source_error() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        let now = e.env.clock.now();
        packet(&e, WS, now);
        fixtures::reject_failed_start(e.env.store(), &id).unwrap();
        let source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
        let error = source.run_due(now).await.unwrap_err();
        assert!(error.0.contains("cannot persist failed start"));
        assert!(
            e.env
                .store()
                .read(|c| runs::page(c, WS, &id, None, 10))
                .unwrap()
                .is_empty()
        );
    }

    struct StartupRequests {
        env: FlowEnv,
        source: Arc<queue::BotSource<Runner>>,
    }
    impl StartupRequests {
        fn new() -> Self {
            let env = FlowEnv::new();
            env.env.clock.set(1_800_000_000_000);
            let source = Arc::new(queue::BotSource::new(
                &env.env.modules,
                Runner::new(env.flow.clone()),
            ));
            Self { env, source }
        }
        fn recorded(&self, state: &str) -> Vec<String> {
            fixtures::recorded_workspaces(self.env.env.store(), state).unwrap()
        }
    }
    impl steve_runtime::due::contract::Fixture for StartupRequests {
        type Source = queue::BotSource<Runner>;
        fn source(&self) -> &Self::Source {
            &self.source
        }
        fn batch(&self) -> usize {
            queue::BOT_BATCH
        }
        fn add(&self, at: i64, poison: bool) -> String {
            let ws = steve_store::uid("ws");
            self.env.env.store().write(|w| -> ApiResult<()> {
                fixtures::seed_owner_workspace(w, &ws, ME)?;
                let spec: schedules::Spec=serde_json::from_value(json!({"when":"fixture","cron":"*/30 * * * *","tz":"UTC","what":"Organize","bot":"steve","mode":"item_auto","overlap":"skip","requester":ME})).unwrap();
                let id=schedules::insert(w,&ws,ME,&schedules::Fields{name:"startup contract",spec:&spec,enabled:true},&schedules::Origin::default(),at-60_000)?;
                w.digest_packets().bind_schedule(&ws, Some(&id), 1)?;
                if poison { fixtures::break_graph(w, &id)?; }
                Ok(())
            }).unwrap();
            packet(&self.env, &ws, at);
            ws
        }
        fn done(&self) -> Vec<String> {
            self.recorded("started")
        }
        fn dead(&self) -> Vec<String> {
            self.recorded("dead")
        }
    }

    #[tokio::test]
    async fn startup_failures_pass_the_bounded_due_contract_with_real_flow_history() {
        steve_runtime::due::contract::check(
            StartupRequests::new,
            1_800_000_000_000,
            START_LIMIT as u32,
        )
        .await;
    }

    #[tokio::test]
    async fn exhausted_startups_revive_only_after_schedule_or_bot_edits() {
        use steve_runtime::{Clock, due::DueSource};
        for repair in ["schedule", "name", "bot", "binding"] {
            let (e, id) = bound().await;
            let now = e.env.clock.now();
            let original = fixtures::flow_graph(e.env.store(), &id).unwrap();
            packet(&e, WS, now);
            fixtures::set_graph_blob(e.env.store(), &id).unwrap();
            let source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
            for _ in 0..START_LIMIT {
                source
                    .run_due(e.env.clock.now())
                    .await
                    .expect("a corrupt graph column is a row failure");
                e.env.clock.advance(60_000);
            }
            assert_eq!(source.next_due().await.unwrap(), None);
            let history = || {
                e.env
                    .store()
                    .read(|c| runs::page(c, WS, &id, None, 50))
                    .unwrap()
            };
            let before = history();
            assert_eq!(before[0].context["startup"]["attempts"], START_LIMIT);
            assert_eq!(before[0].context["startup"]["state"], "dead");
            source
                .run_due(e.env.clock.now() + 86_400_000)
                .await
                .unwrap();
            assert_eq!(
                history(),
                before,
                "elapsed time and direct calls do not revive dead starts"
            );
            flow_fixture::set_raw_flow_graph(e.env.store(), &id, &original).unwrap();
            assert_eq!(
                source.next_due().await.unwrap(),
                None,
                "an unversioned write is not an owner revision"
            );
            match repair {
                "schedule" | "name" => {
                    e.env
                        .store()
                        .write(|tx| -> ApiResult<()> {
                            let old = schedules::own(tx, WS, &id)?;
                            let mut spec = old.spec.clone();
                            if repair == "schedule" {
                                spec.what = "Repaired digest".into();
                            }
                            // Any deliberate save, including a name-only edit, reopens the budget.
                            let name = if repair == "name" {
                                "Renamed digest"
                            } else {
                                &old.name
                            };
                            schedules::update(
                                tx,
                                &old,
                                &schedules::Fields {
                                    name,
                                    spec: &spec,
                                    enabled: true,
                                },
                                ME,
                                "owner repair",
                                e.env.clock.now(),
                            )?;
                            if repair == "name" {
                                let changed = schedules::own(tx, WS, &id)?;
                                assert_eq!(
                                    changed.version, old.version,
                                    "name-only edit keeps the graph version"
                                );
                                assert!(changed.updated > old.updated);
                            }
                            Ok(())
                        })
                        .unwrap();
                }
                "bot" => {
                    let patch =
                        serde_json::from_value(json!({"role":"Repaired organizer"})).unwrap();
                    e.env.store().bots().patch("steve", &patch, ME).unwrap();
                }
                _ => {
                    let (status,schedule)=call(&e.router(),Some(&caller(WS,ME)),"POST","/schedules",Some(&json!({"name":"replacement","cron":"*/30 * * * *","what":"Organize","bot":"steve","mode":"item_auto","overlap":"skip","requester":ME}))).await;
                    assert_eq!(status, 200, "{schedule}");
                    let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
                    let (status, value) = call(
                        &api,
                        Some(&caller(WS, ME)),
                        "PUT",
                        "/digest/bot",
                        Some(&json!({"schedule_id":schedule["id"]})),
                    )
                    .await;
                    assert_eq!(status, 200, "{value}");
                }
            }
            assert!(
                source.next_due().await.unwrap().is_some(),
                "{repair}: new configuration is eligible"
            );
            source.run_due(e.env.clock.now()).await.unwrap();
            let started: i64 = fixtures::started_count(e.env.store(), WS).unwrap();
            assert_eq!(
                started, 1,
                "{repair}: a real ordinary schedule start is persisted"
            );
        }
    }

    #[tokio::test]
    async fn runnable_excludes_disabled_busy_and_ownerless_bindings_and_spaces_restarts() {
        let (e, id) = bound().await;
        let changed = wake(e.env.store());
        let assert_changed = || assert!(changed.notified().now_or_never().is_some());
        let read = || Runner::new(e.flow.clone()).ready(WS).unwrap();
        assert_eq!(read(), Some(i64::MIN));
        fixtures::disable_flow(e.env.store(), &id).unwrap();
        assert_changed();
        assert!(read().is_none());
        fixtures::seed_busy_flow(e.env.store(), WS, &id).unwrap();
        assert_changed();
        assert!(read().is_none());
        fixtures::finish_busy_flow(e.env.store()).unwrap();
        assert_changed();
        assert_eq!(
            read(),
            Some(61_000),
            "an empty/failed start cannot spin on queued packets"
        );
        steve_store::testkit::hub::authority::remove_member(e.env.store(), WS, ME).unwrap();
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
                .read(|r| r.digest_packets().schedule(WS))
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
                .read(|r| r.digest_packets().schedule(WS))
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_running_insert_storage_fault_returns_source_error_without_charging_a_row() {
        use steve_runtime::{Clock, due::DueSource};
        for seed_failure in [false, true] {
            let (e, id) = bound().await;
            let now = e.env.clock.now();
            packet(&e, WS, now);
            let revision = e
                .env
                .store()
                .read::<_, steve_store::StoreError>(|c| {
                    Ok(steve_flow::scheduler::startup_revision(c, WS, &id))
                })
                .unwrap()
                .unwrap()
                .unwrap();
            if seed_failure {
                e.env
                    .store()
                    .write(|tx| {
                        runs::record_start_failure(
                            tx,
                            &runs::FailedStart {
                                ws: WS,
                                flow_id: &id,
                                flow_version: 1,
                                source: BOT_SOURCE,
                                error: &steve_flow::scheduler::start_rejected(),
                                now: now - 60_000,
                                revision: &revision,
                                limit: START_LIMIT,
                            },
                        )
                    })
                    .unwrap();
            }
            let history = || {
                e.env
                    .store()
                    .read(|c| runs::page(c, WS, &id, None, 50))
                    .unwrap()
            };
            let budget = || {
                e.env
                    .store()
                    .read::<_, steve_store::StoreError>(|c| {
                        Ok(runs::startup_state(c, WS, &id, BOT_SOURCE, &revision))
                    })
                    .unwrap()
                    .unwrap()
            };
            let (before_rows, before_budget) = (history(), budget());
            steve_store::testkit::hub::mutations::create_reject_running_start(e.env.store())
                .unwrap();
            let source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
            let error = source.run_due(now).await.unwrap_err();
            assert!(error.0.contains("running insert unavailable"));
            assert_eq!(
                history(),
                before_rows,
                "a storage failure creates no failed run"
            );
            assert_eq!(
                budget(),
                before_budget,
                "a storage failure spends no row budget"
            );
        }
    }

    #[tokio::test]
    async fn stable_start_rejections_keep_distinct_internal_error_fingerprints() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        let now = e.env.clock.now();
        packet(&e, WS, now);
        flow_fixture::set_raw_flow_graph(e.env.store(), &id, "broken json").unwrap();
        let source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
        source.run_due(now).await.unwrap();
        e.env.clock.advance(60_000);
        fixtures::set_graph_blob(e.env.store(), &id).unwrap();
        source.run_due(now + 60_000).await.unwrap();
        let rows = e
            .env
            .store()
            .read(|c| runs::page(c, WS, &id, None, 50))
            .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "distinct decoder errors retain separate history"
        );
        for row in &rows {
            assert_eq!(
                steve_flow::runs::view(row).error.as_deref(),
                Some(steve_flow::scheduler::start_rejected().as_str())
            );
            assert_eq!(steve_flow::runs::view(row).start_failures, Some(1));
            assert!(!row.context.to_string().contains("Blob"));
            assert_eq!(row.context["error_fingerprint"].as_str().unwrap().len(), 64);
        }
        assert_ne!(
            rows[0].context["error_fingerprint"],
            rows[1].context["error_fingerprint"]
        );
        assert_eq!(
            steve_flow::runs::view(&rows[0])
                .startup
                .as_ref()
                .unwrap()
                .attempts,
            2
        );
    }

    #[tokio::test]
    async fn only_the_owner_can_read_corrupt_startup_metadata_diagnostics() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        let now = e.env.clock.now();
        packet(&e, WS, now);
        flow_fixture::set_raw_flow_graph(e.env.store(), &id, "broken json").unwrap();
        let source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
        source.run_due(now).await.unwrap();
        let run = e
            .env
            .store()
            .read(|c| runs::page(c, WS, &id, None, 50))
            .unwrap()[0]
            .id
            .clone();
        flow_fixture::set_run_context_field(
            e.env.store(),
            &run,
            "$.startup.attempts",
            &json!("broken"),
        )
        .unwrap();
        e.env.clock.advance(60_000);
        assert_eq!(source.next_due().await.unwrap(), None);
        let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
        assert_eq!(
            call(&api, Some(&caller(WS, OTHER)), "GET", "/digest/bot", None)
                .await
                .0,
            403
        );
        // ME owns both fixture workspaces; the other scope is readable but must contain no
        // diagnostic or binding from WS. Non-owner access above still returns 403.
        let (other_status, other) =
            call(&api, Some(&caller("w2", ME)), "GET", "/digest/bot", None).await;
        assert_eq!(other_status, 200);
        assert!(other["diagnostics"].as_array().unwrap().is_empty());
        assert!(other["schedule_id"].is_null());
        assert!(!other.to_string().contains(&run));
        let (status, body) = call(&api, Some(&caller(WS, ME)), "GET", "/digest/bot", None).await;
        assert_eq!(status, 200);
        assert_eq!(body["diagnostics"].as_array().unwrap().len(), 1);
        let d = &body["diagnostics"][0];
        assert_eq!(d["code"], "startup_metadata_invalid");
        assert_eq!(d["flow_id"], id);
        assert_eq!(d["run_id"], run);
        assert!(
            d["message"]
                .as_str()
                .unwrap()
                .contains("修改调度或 Bot 即可恢复")
        );
        assert!(!body.to_string().contains("invalid type"));
    }

    #[tokio::test]
    async fn damaged_metadata_warns_once_and_real_schedule_or_bot_edits_recover() {
        use std::io::Write;
        use steve_runtime::{Clock, due::DueSource};
        #[derive(Clone)]
        struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for repair in ["schedule", "bot"] {
            let (e, id) = bound().await;
            let now = e.env.clock.now();
            packet(&e, WS, now);
            flow_fixture::set_raw_flow_graph(e.env.store(), &id, "broken json").unwrap();
            let runner = Runner::new(e.flow.clone());
            queue::BotRunner::start(&runner, WS, now).unwrap();
            let old = e
                .env
                .store()
                .read(|c| runs::page(c, WS, &id, None, 50))
                .unwrap()[0]
                .id
                .clone();
            flow_fixture::damage_startup_run(
                e.env.store(),
                &old,
                flow_fixture::DamagedRunColumn::Cost,
            )
            .unwrap();
            e.env.clock.advance(60_000);
            let buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = buffer.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::WARN)
                .without_time()
                .with_ansi(false)
                .with_writer(move || Capture(sink.clone()))
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                for _ in 0..5 {
                    assert_eq!(runner.ready(WS).unwrap(), None);
                }
            });
            let log = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
            assert_eq!(
                log.matches("digest Bot startup metadata is damaged")
                    .count(),
                1,
                "{log}"
            );
            assert!(log.contains(&old));
            let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
            let (status, body) =
                call(&api, Some(&caller(WS, ME)), "GET", "/digest/bot", None).await;
            assert_eq!(status, 200);
            assert_eq!(body["diagnostics"][0]["run_id"], old);
            assert!(
                body["diagnostics"][0]["message"]
                    .as_str()
                    .unwrap()
                    .contains("修改调度或 Bot 即可恢复")
            );
            if repair == "schedule" {
                e.env
                    .store()
                    .write(|tx| -> ApiResult<()> {
                        let old = schedules::own(tx, WS, &id)?;
                        schedules::update(
                            tx,
                            &old,
                            &schedules::Fields {
                                name: "owner saved",
                                spec: &old.spec,
                                enabled: true,
                            },
                            ME,
                            "repair",
                            e.env.clock.now(),
                        )?;
                        Ok(())
                    })
                    .unwrap();
            } else {
                let patch = serde_json::from_value(json!({"role":"owner saved"})).unwrap();
                e.env.store().bots().patch("steve", &patch, ME).unwrap();
            }
            let source = queue::BotSource::new(&e.env.modules, runner);
            assert!(source.next_due().await.unwrap().is_some());
            source.run_due(e.env.clock.now()).await.unwrap();
            let revision = e
                .env
                .store()
                .read::<_, steve_store::StoreError>(|c| {
                    Ok(steve_flow::scheduler::startup_revision(c, WS, &id))
                })
                .unwrap()
                .unwrap()
                .unwrap();
            let state = e
                .env
                .store()
                .read::<_, steve_store::StoreError>(|c| {
                    Ok(runs::startup_state(c, WS, &id, BOT_SOURCE, &revision))
                })
                .unwrap()
                .unwrap()
                .unwrap();
            assert_ne!(state.run_id, old);
            assert_eq!(state.attempts, if repair == "schedule" { 0 } else { 1 });
            let (status, body) =
                call(&api, Some(&caller(WS, ME)), "GET", "/digest/bot", None).await;
            assert_eq!(status, 200);
            assert_eq!(
                body["diagnostics"][0]["run_id"], old,
                "old damaged run remains diagnosable after recovery"
            );
            let original: (String, String) =
                fixtures::damaged_run_columns(e.env.store(), &old).unwrap();
            assert_eq!(original, ("broken".into(), "blob".into()));
        }
    }

    #[tokio::test]
    async fn bound_digest_schedule_suppresses_cron_and_runs_only_when_packets_are_due() {
        use steve_runtime::{Clock, due::DueSource};
        let (e, id) = bound().await;
        // Binding disables the wall-clock time trigger immediately.
        assert_eq!(
            flow_fixture::enabled_trigger_count(e.env.store(), &id).unwrap(),
            0
        );
        assert_eq!(
            flow_fixture::trigger_next_at(e.env.store(), &id).unwrap(),
            None
        );
        // Advancing across multiple 30-minute intervals starts zero idle runs.
        let triggers = steve_flow::scheduler::Triggers::new(e.flow.clone());
        let bot_source = queue::BotSource::new(&e.env.modules, Runner::new(e.flow.clone()));
        for _ in 0..4 {
            e.env.clock.advance(30 * 60_000);
            let now = e.env.clock.now();
            triggers.run_due(now).await.unwrap();
            bot_source.run_due(now).await.unwrap();
        }
        assert!(
            e.env
                .store()
                .read(|c| runs::page(c, WS, &id, None, 10))
                .unwrap()
                .is_empty(),
            "idle digest bot must not start runs on wall-clock cron"
        );
        // Editing the bound schedule keeps its wall-clock trigger disabled.
        let (st, v) = call(
            &e.router(),
            Some(&caller(WS, ME)),
            "PATCH",
            &format!("/schedules/{id}"),
            Some(&json!({"what": "Organize updated"})),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(
            flow_fixture::enabled_trigger_count(e.env.store(), &id).unwrap(),
            0
        );
        // Even if an upgraded installation still has an enabled cron trigger on the bound
        // schedule, the scheduler suppresses and disables it instead of launching an empty run.
        let now = e.env.clock.now();
        e.env
            .store()
            .write(|w| schedules::set_time_trigger_enabled(w, WS, &id, true, now))
            .unwrap();
        assert_eq!(
            flow_fixture::enabled_trigger_count(e.env.store(), &id).unwrap(),
            1
        );
        e.env.clock.advance(30 * 60_000);
        triggers.run_due(e.env.clock.now()).await.unwrap();
        assert!(
            e.env
                .store()
                .read(|c| runs::page(c, WS, &id, None, 10))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            flow_fixture::enabled_trigger_count(e.env.store(), &id).unwrap(),
            0
        );
        // Queueing a real digest packet starts the Digest Bot via BotSource.
        let now = e.env.clock.now();
        packet(&e, WS, now);
        assert!(bot_source.next_due().await.unwrap().is_some());
        bot_source.run_due(now).await.unwrap();
        assert_eq!(
            e.env
                .store()
                .read(|c| runs::page(c, WS, &id, None, 10))
                .unwrap()
                .len(),
            1
        );
        // Unbinding restores the schedule's normal wall-clock time trigger.
        let api = routes(e.flow.clone()).with_state(e.env.modules.clone());
        let (st, v) = call(
            &api,
            Some(&caller(WS, ME)),
            "PUT",
            "/digest/bot",
            Some(&json!({"schedule_id": null})),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        assert_eq!(
            flow_fixture::enabled_trigger_count(e.env.store(), &id).unwrap(),
            1
        );
    }
}

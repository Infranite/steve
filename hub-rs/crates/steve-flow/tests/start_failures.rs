//! Startup history belongs to Flow; fixtures use its real migrated store and public run API.
use steve_flow::{runs, testkit::FlowEnv};
use steve_modules::{ApiResult, testkit::WS};

fn record(e: &FlowEnv, error: &str, version: i64, now: i64) -> String {
    e.env
        .store()
        .transaction(|tx| {
            runs::record_start_failure(
                tx,
                &runs::FailedStart {
                    ws: WS,
                    flow_id: "flow_start",
                    flow_version: version,
                    source: "fixture.start",
                    revision: &serde_json::json!({"flow":version}),
                    limit: 10,
                    error,
                    now,
                },
            )
        })
        .unwrap()
}
fn history(e: &FlowEnv) -> Vec<runs::RunRow> {
    e.env
        .store()
        .reader(|c| runs::page(c, WS, "flow_start", None, 50))
        .unwrap()
}

#[test]
fn failure_insert_contains_error_and_repeats_only_the_latest_matching_record() {
    let e = FlowEnv::new();
    e.env.store().with_conn(|c| {
        c.execute_batch("CREATE TRIGGER require_start_error BEFORE INSERT ON flow_runs WHEN NEW.status='failed' AND NEW.error IS NULL BEGIN SELECT RAISE(ABORT,'missing initial error'); END")?;
        Ok(())
    }).unwrap();
    let first = record(&e, "bad graph", 1, 1000);
    assert_eq!(record(&e, "bad graph", 1, 61_000), first);
    let repeated = history(&e).pop().unwrap();
    assert_eq!(repeated.error.as_deref(), Some("bad graph"));
    assert_eq!(repeated.context["start_failures"], 2);
    assert_eq!(repeated.context["first_at"], 1000);
    assert_eq!(repeated.started, 61_000);
    assert_eq!(repeated.ended, Some(61_000));
    assert_ne!(record(&e, "other graph", 1, 121_000), first);
    assert_ne!(record(&e, "bad graph", 1, 181_000), first);
    let before = history(&e);
    let new_version = record(&e, "bad graph", 2, 241_000);
    let rows = history(&e);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].id, new_version);
    assert_eq!(rows[0].context["start_failures"], 1);
    assert_eq!(rows[1..], before);
}

#[test]
fn a_rejected_history_write_rolls_back_the_whole_operation() {
    let e = FlowEnv::new();
    record(&e, "bad graph", 1, 1000);
    let before = history(&e);
    e.env.store().with_conn(|c| {
        c.execute_batch("CREATE TRIGGER reject_start_update BEFORE UPDATE ON flow_runs BEGIN SELECT RAISE(ABORT,'history unavailable'); END")?;
        Ok(())
    }).unwrap();
    let error = e
        .env
        .store()
        .transaction(|tx| -> ApiResult<_> {
            runs::record_start_failure(
                tx,
                &runs::FailedStart {
                    ws: WS,
                    flow_id: "flow_start",
                    flow_version: 1,
                    source: "fixture.start",
                    revision: &serde_json::json!({"flow":1}),
                    limit: 10,
                    error: "new error",
                    now: 61_000,
                },
            )
        })
        .unwrap_err();
    assert!(error.to_string().contains("history unavailable"));
    assert_eq!(history(&e), before);
}

fn bounded(e: &FlowEnv, error: &str, epoch: i64, now: i64) -> String {
    e.env
        .store()
        .transaction(|tx| {
            runs::record_start_failure(
                tx,
                &runs::FailedStart {
                    ws: WS,
                    flow_id: "flow_start",
                    flow_version: 1,
                    source: "fixture.start",
                    error,
                    now,
                    revision: &serde_json::json!({"epoch":epoch}),
                    limit: 3,
                },
            )
        })
        .unwrap()
}
fn state(e: &FlowEnv, epoch: i64) -> Option<runs::StartupState> {
    e.env
        .store()
        .reader(|c| {
            runs::startup_state(
                c,
                WS,
                "flow_start",
                "fixture.start",
                &serde_json::json!({"epoch":epoch}),
            )
        })
        .unwrap()
}
fn ordinary(e: &FlowEnv, at: i64) -> String {
    e.env
        .store()
        .transaction(|tx| {
            runs::insert(
                tx,
                &runs::NewRun {
                    ws: WS,
                    flow_id: "flow_start",
                    flow_version: 1,
                    trigger_kind: "manual",
                    status: "done",
                    error: None,
                    outcome_label: "ordinary",
                    cursor: None,
                    context: &serde_json::json!({"at":at}),
                    now: at,
                },
            )
        })
        .unwrap()
}

#[test]
fn changing_errors_or_ordinary_history_cannot_reset_the_persisted_budget() {
    let e = FlowEnv::new();
    bounded(&e, "A", 1, 1000);
    ordinary(&e, 2000);
    bounded(&e, "B", 1, 500); // causal order, even when wall time moves backwards
    assert_eq!(state(&e, 1).unwrap().attempts, 2);
    let dead = bounded(&e, "A", 1, 400);
    assert_eq!(state(&e, 1).unwrap().state, runs::StartupStatus::Dead);
    ordinary(&e, 3000);
    let before = history(&e);
    assert_eq!(bounded(&e, "C", 1, 4000), dead);
    assert_eq!(history(&e), before);
    let reopened =
        steve_store::Store::open(e.env.store().path(), steve_store::NoPublisher).unwrap();
    assert_eq!(
        reopened
            .reader(|c| runs::startup_state(
                c,
                WS,
                "flow_start",
                "fixture.start",
                &serde_json::json!({"epoch":1})
            ))
            .unwrap()
            .unwrap()
            .state,
        runs::StartupStatus::Dead
    );
    assert!(state(&e, 2).is_none());
    bounded(&e, "A", 2, 5000);
    assert_eq!(state(&e, 2).unwrap().attempts, 1);
}

#[test]
fn a_successful_source_start_resets_the_budget_but_pruning_cannot_resurrect_old_state() {
    let e = FlowEnv::new();
    e.env.store().with_conn(|c| {
        c.execute("INSERT INTO flows (id,workspace_id,kind,name,graph,origin,created_by,created,updated) VALUES ('flow_start',?,'schedule','fixture','{}','me','usr_dev',0,0)",[WS])?;
        Ok(())
    }).unwrap();
    for n in 0..3 {
        bounded(&e, "bad", 1, 1000 + n);
    }
    let dead = state(&e, 1).unwrap().run_id;
    for n in 0..205 {
        ordinary(&e, 2000 + n);
    }
    let pruned = e
        .env
        .store()
        .transaction(|tx| runs::prune(tx, 2 * runs::KEEP_MS))
        .unwrap();
    assert!(pruned > 0, "the test must exercise real retention");
    assert_eq!(state(&e, 1).unwrap().state, runs::StartupStatus::Dead);
    assert!(
        e.env
            .store()
            .reader(|c| runs::get(c, &dead))
            .unwrap()
            .is_some()
    );
    let success = ordinary(&e, 3000);
    e.env
        .store()
        .transaction(|tx| {
            runs::record_started(
                tx,
                &success,
                "fixture.start",
                &serde_json::json!({"epoch":1}),
                3,
            )
        })
        .unwrap();
    for n in 0..205 {
        ordinary(&e, 4000 + n);
    }
    e.env
        .store()
        .transaction(|tx| runs::prune(tx, 2 * runs::KEEP_MS))
        .unwrap();
    assert!(
        e.env
            .store()
            .reader(|c| runs::get(c, &dead))
            .unwrap()
            .is_none()
    );
    let current = state(&e, 1).unwrap();
    assert_eq!(current.run_id, success);
    assert_eq!(current.state, runs::StartupStatus::Started);
    bounded(&e, "bad again", 1, 5000);
    assert_eq!(state(&e, 1).unwrap().attempts, 1);
}

#[test]
fn execution_failure_of_a_started_run_is_not_a_startup_rejection_to_coalesce() {
    let e = FlowEnv::new();
    let id = ordinary(&e, 1000);
    e.env
        .store()
        .transaction(|tx| -> ApiResult<()> {
            runs::record_started(tx, &id, "fixture.start", &serde_json::json!({"epoch":1}), 3)?;
            let mut row = runs::get(tx, &id)?.unwrap();
            row.status = "failed".into();
            row.error = Some("bad".into());
            runs::save(tx, &row)
        })
        .unwrap();
    let failed = bounded(&e, "bad", 1, 2000);
    assert_ne!(failed, id);
    assert_eq!(state(&e, 1).unwrap().attempts, 1);
    assert_eq!(history(&e)[0].context["start_failures"], 1);
}

#[test]
fn run_view_exposes_historical_counts_and_budget_without_internal_revision() {
    let e = FlowEnv::new();
    bounded(&e, "bad graph", 1, 1_795_180_000_000);
    bounded(&e, "bad graph", 1, 1_795_180_060_000);
    let repeated = history(&e)[0].view();
    assert_eq!(repeated.start_failures, Some(2));
    assert_eq!(repeated.first_at, Some(1_795_180_000_000));
    assert_eq!(repeated.started, 1_795_180_060_000);
    bounded(&e, "missing owner", 1, 1_795_180_120_000);
    let dead = history(&e)[0].view();
    assert_eq!(dead.start_failures, Some(1));
    assert_eq!(dead.first_at, Some(dead.started));
    assert_eq!(
        dead.startup.as_ref().unwrap(),
        &runs::StartupSummary {
            source: "fixture.start".into(),
            state: runs::StartupStatus::Dead,
            attempts: 3,
            limit: 3,
        }
    );
    let json = serde_json::to_value(dead).unwrap();
    assert_eq!(json["startup"]["state"], "dead");
    assert!(json["startup"].get("revision").is_none());
    let id = ordinary(&e, 1_795_180_180_000);
    let plain = e
        .env
        .store()
        .reader(|c| runs::get(c, &id))
        .unwrap()
        .unwrap();
    let json = serde_json::to_value(plain.view()).unwrap();
    for key in ["first_at", "start_failures", "startup"] {
        assert!(json.get(key).is_none(), "ordinary runs omit {key}");
    }
}

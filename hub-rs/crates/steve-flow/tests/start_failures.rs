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
                    error: "bad graph",
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

#[test]
fn ordinary_runs_and_new_versions_split_error_history_without_rewriting_earlier_rows() {
    for barrier in ["version", "done", "failed", "skipped"] {
        let e = FlowEnv::new();
        let first = record(&e, "first failure", 1, 1000);
        let before = history(&e)[0].clone();
        if barrier != "version" {
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
                            status: barrier,
                            error: None,
                            outcome_label: "other run",
                            cursor: None,
                            context: &serde_json::json!({"at":61_000}),
                            now: 61_000,
                        },
                    )
                })
                .unwrap();
        }
        let version = 1 + i64::from(barrier == "version");
        let next = record(&e, "first failure", version, 121_000);
        let rows = history(&e);
        assert_ne!(first, next, "{barrier}");
        assert_eq!(rows.len(), if barrier == "version" { 2 } else { 3 });
        assert_eq!(rows[0].view().start_failures, Some(1));
        assert_eq!(rows[0].view().first_at, Some(121_000));
        assert_eq!(rows[0].flow_version, version);
        assert_eq!(rows.last().unwrap(), &before);
    }
}

#[test]
fn inserting_a_new_failure_never_issues_a_redundant_update() {
    let e = FlowEnv::new();
    e.env.store().with_conn(|c| {
        c.execute_batch("CREATE TRIGGER deny_any_run_update BEFORE UPDATE ON flow_runs BEGIN SELECT RAISE(ABORT,'updates unavailable'); END")?;
        Ok(())
    }).unwrap();
    let first = record(&e, "bad graph", 1, 1000);
    assert_eq!(history(&e)[0].id, first);
    assert_eq!(history(&e)[0].view().start_failures, Some(1));
}

#[test]
fn changed_errors_start_new_history_with_view_counts_and_first_times() {
    let e = FlowEnv::new();
    let first = record(&e, "first error", 1, 1000);
    assert_eq!(record(&e, "first error", 1, 61_000), first);
    let repeated = history(&e)[0].clone();
    assert_eq!(repeated.view().start_failures, Some(2));
    assert_eq!(repeated.view().first_at, Some(1000));
    record(&e, "changed error", 1, 121_000);
    let changed = history(&e)[0].clone();
    assert_eq!(changed.view().start_failures, Some(1));
    assert_eq!(changed.view().error.as_deref(), Some("changed error"));
    assert_eq!(changed.view().first_at, Some(121_000));
    record(&e, "first error", 1, 181_000);
    let rows = history(&e);
    assert_eq!(rows.len(), 3);
    assert_ne!(rows[0].id, first);
    assert_eq!(rows[0].view().start_failures, Some(1));
    assert_eq!(rows[1], changed);
    assert_eq!(rows[2], repeated);
}

#[test]
fn corrupt_source_metadata_is_diagnosable_and_cannot_spend_or_erase_budget() {
    for (key, value) in [
        ("attempts", serde_json::json!("bad")),
        ("limit", serde_json::json!(0)),
        ("format", serde_json::json!(99)),
        ("state", serde_json::json!("unknown")),
    ] {
        let e = FlowEnv::new();
        let id = bounded(&e, "bad graph", 1, 1000);
        e.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "UPDATE flow_runs SET context=json_set(context,?1,json(?2)) WHERE id=?3",
                    rusqlite::params![format!("$.startup.{key}"), value.to_string(), id],
                )?;
                Ok(())
            })
            .unwrap();
        let before = history(&e);
        let error = e
            .env
            .store()
            .reader(|c| {
                runs::startup_state(
                    c,
                    WS,
                    "flow_start",
                    "fixture.start",
                    &serde_json::json!({"epoch":1}),
                )
            })
            .unwrap_err();
        assert!(error.to_string().contains("Invalid startup"));
        let diagnostic = e
            .env
            .store()
            .reader(|c| runs::startup_diagnostic(c, WS, "flow_start", "fixture.start"))
            .unwrap()
            .unwrap();
        assert_eq!(diagnostic.code, "startup_metadata_invalid");
        assert_eq!(diagnostic.run_id, id);
        assert_eq!(diagnostic.flow_id, "flow_start");
        assert!(!diagnostic.message.contains("invalid type"));
        assert_eq!(history(&e), before);
    }
}

#[test]
fn an_edited_configuration_starts_at_zero_without_overwriting_damaged_history() {
    let e = FlowEnv::new();
    let id = bounded(&e, "bad", 1, 1000);
    e.env.store().with_conn(|c|{c.execute("UPDATE flow_runs SET context=json_set(context,'$.startup.attempts','broken') WHERE id=?",[&id])?;Ok(())}).unwrap();
    let before = e
        .env
        .store()
        .reader(|c| runs::get(c, &id))
        .unwrap()
        .unwrap();
    assert!(state(&e, 2).is_none());
    let next = bounded(&e, "still bad", 2, 61_000);
    assert_ne!(next, id);
    assert_eq!(state(&e, 2).unwrap().attempts, 1);
    assert_eq!(
        e.env
            .store()
            .reader(|c| runs::get(c, &id))
            .unwrap()
            .unwrap(),
        before
    );
    let diagnostic = e
        .env
        .store()
        .reader(|c| runs::startup_diagnostic(c, WS, "flow_start", "fixture.start"))
        .unwrap()
        .unwrap();
    assert_eq!(
        diagnostic.run_id, id,
        "newer valid metadata does not hide the old damage"
    );
    assert!(diagnostic.message.contains("修改调度或 Bot 即可恢复"));
}

#[test]
fn diagnostics_decode_columns_independently_instead_of_loading_a_full_run() {
    for change in [
        "cost=x'ff'",
        "started=x'ff'",
        "status=x'ff'",
        "error=x'ff'",
        "id=x'ff'",
        "context=CAST(context AS BLOB)",
    ] {
        let e = FlowEnv::new();
        let id = bounded(&e, "bad", 1, 1000);
        e.env.store().with_conn(|c|{
            c.execute("UPDATE flow_runs SET context=json_set(context,'$.startup.attempts','broken') WHERE id=?",[&id])?;
            c.execute(&format!("UPDATE flow_runs SET {change} WHERE id=?"),[&id])?;Ok(())
        }).unwrap();
        let diagnostic = e
            .env
            .store()
            .reader(|c| runs::startup_diagnostic(c, WS, "flow_start", "fixture.start"))
            .unwrap()
            .unwrap();
        assert_eq!(diagnostic.code, "startup_metadata_invalid", "{change}");
        assert_eq!(diagnostic.flow_id, "flow_start");
        assert!(!diagnostic.run_id.is_empty());
        if change != "id=x'ff'" {
            assert_eq!(diagnostic.run_id, id);
        }
    }
}

#[test]
fn unreadable_unrelated_history_cannot_replace_a_known_dead_source_budget() {
    let e = FlowEnv::new();
    for n in 0..3 {
        bounded(&e, "bad", 1, 1000 + n);
    }
    let dead = state(&e, 1).unwrap();
    let unrelated = ordinary(&e, 2000);
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE flow_runs SET context='not json' WHERE id=?",
                [&unrelated],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(state(&e, 1).unwrap(), dead);
    let diagnostic = e
        .env
        .store()
        .reader(|c| runs::startup_diagnostic(c, WS, "flow_start", "fixture.start"))
        .unwrap()
        .unwrap();
    assert_eq!(diagnostic.run_id, unrelated);
}

#[test]
fn startup_rejection_and_recovery_diagnostics_follow_the_readers_language() {
    for (locale, rejected, recovery) in [
        (
            steve_i18n::ZH_CN,
            "此调度无法启动",
            "修改调度或 Bot 即可恢复",
        ),
        (
            steve_i18n::EN,
            "This schedule could not start",
            "Edit the schedule or Bot to recover",
        ),
    ] {
        steve_i18n::with(locale, || {
            assert!(steve_flow::scheduler::start_rejected().contains(rejected));
            let e = FlowEnv::new();
            let id = bounded(&e, "bad", 1, 1000);
            e.env.store().with_conn(|c|{c.execute("UPDATE flow_runs SET context=json_set(context,'$.startup.limit',0) WHERE id=?",[&id])?;Ok(())}).unwrap();
            let d = e
                .env
                .store()
                .reader(|c| runs::startup_diagnostic(c, WS, "flow_start", "fixture.start"))
                .unwrap()
                .unwrap();
            assert_eq!(d.code, "startup_metadata_invalid");
            assert!(d.message.contains(recovery));
        });
    }
}

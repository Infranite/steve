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
                    error: "new error",
                    now: 61_000,
                },
            )
        })
        .unwrap_err();
    assert!(error.to_string().contains("history unavailable"));
    assert_eq!(history(&e), before);
}

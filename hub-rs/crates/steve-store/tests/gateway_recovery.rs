//! Result scheduling uses the production 128 batch, real SQLite faults and durable call identities.
use rusqlite::params;
use serde_json::{Value, json};
use steve_node_api::{Epoch, Measurement, Money, Tokens, UsageReport, UsageScope};
use steve_store::testkit::SqliteFixtures;
use steve_store::{
    NoPublisher, Store, StoreError, WS, db,
    gateway_ledger::{CallBegin, CallFinish},
};

const AT: i64 = 1_793_491_200_000;
const BATCH: usize = 128;
fn open(path: &std::path::Path) -> Store {
    let s = Store::open(path, NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s
}
fn sql(s: &Store, text: &str) {
    s.write::<_, StoreError>(|tx| {
        steve_store::testkit::sqlite_writer(tx).execute_batch(text)?;
        Ok(())
    })
    .unwrap();
}
fn n(s: &Store, text: &str) -> i64 {
    s.fixture_read::<_, StoreError>(|c| Ok(c.query_row(text, [], |r| r.get(0))?))
        .unwrap()
}
fn begin(s: &Store, call: &str) {
    s.stats()
        .gateway_begin(&CallBegin {
            id: call.into(),
            workspace: WS.into(),
            purpose: "test".into(),
            started: AT,
            day: "2026-11-01".into(),
            ..Default::default()
        })
        .unwrap();
}
fn report(id: &str, amount: u64, measurement: Measurement) -> UsageReport {
    UsageReport {
        report_id: id.into(),
        model: Some("m".into()),
        meter_epoch: Epoch::new("meter").unwrap(),
        scope: UsageScope::Main,
        measurement,
        tokens: Tokens {
            input: Some(amount),
            output: Some(0),
            cache_read: Some(0),
            cache_write: Some(0),
            reasoning: None,
        },
        cost: Some(Money {
            decimal: if amount == 10 { "0.25" } else { "0.5" }.into(),
            currency: "USD".into(),
        }),
        context_used: None,
        context_limit: None,
    }
}
fn seed(s: &Store, count: usize) {
    for i in 0..count {
        let call = format!("call-{i:04}");
        begin(s, &call);
        s.stats()
            .gateway_attempt(&call, &call, "fixture", "m")
            .unwrap();
        s.stats()
            .gateway_report(&call, &report("live", 10, Measurement::Cumulative), AT + 1)
            .unwrap();
    }
}
fn outage(s: &Store, healthy: Option<&str>) {
    sql(s, "DROP TRIGGER IF EXISTS fail_projection");
    let condition = healthy
        .map(|id| format!("WHEN json_extract(NEW.detail,'$.call_id') != '{id}'"))
        .unwrap_or_default();
    sql(
        s,
        &format!(
            "CREATE TRIGGER fail_projection BEFORE INSERT ON usage_records {condition} BEGIN SELECT RAISE(ABORT,'isolated projection failure'); END"
        ),
    );
}
fn end(s: &Store, call: &str, finish: bool) {
    let result = if finish {
        s.stats().gateway_finish(
            call,
            &CallFinish {
                ok: false,
                model: "m".into(),
                harness: "fixture".into(),
                attempts: 1,
                duration_ms: 88,
                failures: vec!["failed".into()],
                ..Default::default()
            },
        )
    } else {
        s.stats().gateway_cancel(call, "cancelled")
    };
    assert!(result.is_err());
}
fn bill(s: &Store, call: &str) -> Value {
    s.fixture_read::<_,StoreError>(|c|Ok(c.query_row("SELECT input,cost_reported,ok,json_extract(detail,'$.pending'),attempts,duration_ms,json_extract(detail,'$.failures'),day FROM usage_records WHERE live_call_id=?",[call],|r|Ok(json!({"input":r.get::<_,i64>(0)?,"cost":r.get::<_,f64>(1)?,"ok":r.get::<_,bool>(2)?,"pending":r.get::<_,bool>(3)?,"attempts":r.get::<_,i64>(4)?,"duration":r.get::<_,i64>(5)?,"failures":r.get::<_,Option<String>>(6)?,"day":r.get::<_,String>(7)?})))?)).unwrap()
}
fn settled(s: &Store, call: &str, finish: bool) {
    assert_eq!(
        n(
            s,
            &format!("SELECT projection_dirty FROM gateway_calls WHERE call_id='{call}'")
        ),
        0
    );
    let b = bill(s, call);
    assert_eq!(b["input"], 10);
    assert_eq!(b["cost"], 0.25);
    assert_eq!(b["ok"], false);
    assert_eq!(b["pending"], false);
    assert_eq!(b["attempts"], 1);
    assert_eq!(b["day"], "2026-11-01");
    if finish {
        assert_eq!(b["duration"], 88);
        assert_eq!(b["failures"], "[\"failed\"]");
    }
    assert_eq!(
        n(
            s,
            &format!("SELECT COUNT(*) FROM gateway_result_ready WHERE call_id='{call}'")
        ),
        0
    );
}

#[test]
fn finish_and_cancel_progress_with_multiple_failed_batches_and_continuous_arrivals() {
    for finish in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let s = open(&dir.path().join("steve.sqlite3"));
        seed(&s, 385);
        outage(&s, None);
        for i in 0..385 {
            end(&s, &format!("call-{i:04}"), finish);
        }
        outage(&s, Some("call-0384"));
        let first = n(&s, "SELECT MIN(ticket) FROM gateway_result_ready");
        assert!(s.stats().gateway_apply_local(0).unwrap() == 0);
        assert_eq!(n(&s, "SELECT MIN(ticket) FROM gateway_result_ready"), first);
        for round in 0..4 {
            // New calls have no Usage: every dirty outcome still enters the SAME tail.
            for i in 0..40 {
                let call = format!("new-{round}-{i}");
                begin(&s, &call);
                end(&s, &call, finish);
            }
            assert!(s.stats().gateway_apply_local(BATCH).is_err());
            if round == 0 {
                // Exactly 128 scheduling slots consumed, never an unbounded dirty scan.
                assert_eq!(
                    n(&s, "SELECT MIN(ticket) FROM gateway_result_ready"),
                    first + 128
                );
            }
            if round < 3 {
                assert_eq!(
                    n(
                        &s,
                        "SELECT projection_dirty FROM gateway_calls WHERE call_id='call-0384'"
                    ),
                    1
                );
            }
        }
        settled(&s, "call-0384", finish);
        assert_eq!(
            n(
                &s,
                "SELECT COUNT(*) FROM usage_records WHERE live_call_id LIKE 'call-%'"
            ),
            385
        );
        assert_eq!(n(&s, "SELECT SUM(input) FROM usage_records"), 3850);
        // A late successful caller result cannot erase the first failure; Final replaces live10.
        s.stats()
            .gateway_finish(
                "call-0384",
                &CallFinish {
                    ok: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let final_report = report("final", 20, Measurement::Final);
        assert!(
            s.stats()
                .gateway_report("call-0384", &final_report, AT + 2)
                .unwrap()
        );
        assert!(
            !s.stats()
                .gateway_report("call-0384", &final_report, AT + 3)
                .unwrap()
        );
        let b = bill(&s, "call-0384");
        assert_eq!(b["input"], 20);
        assert_eq!(b["cost"], 0.5);
        assert_eq!(b["ok"], false);
        assert_eq!(
            n(
                &s,
                "SELECT COUNT(*) FROM usage_records WHERE live_call_id='call-0384'"
            ),
            1
        );
        assert_eq!(n(&s, "SELECT SUM(input) FROM usage_records"), 3860);
    }
}

#[test]
fn restart_retains_rotation_and_projection_outage_does_not_block_startup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let s = open(&path);
    seed(&s, 257);
    outage(&s, None);
    for i in 0..257 {
        end(&s, &format!("call-{i:04}"), i % 2 == 0);
    }
    assert!(s.stats().gateway_apply_local(BATCH).is_err());
    let head = n(&s, "SELECT MIN(ticket) FROM gateway_result_ready");
    // Two callers were still running at crash time; no new usage is needed to settle them.
    begin(&s, "orphan-1");
    begin(&s, "orphan-2");
    drop(s);
    let s = open(&path);
    assert_eq!(n(&s, "SELECT MIN(ticket) FROM gateway_result_ready"), head);
    outage(&s, Some("call-0256"));
    assert_eq!(s.stats().gateway_recover().unwrap(), 2);
    settled(&s, "call-0256", true);
    assert_eq!(
        n(
            &s,
            "SELECT COUNT(*) FROM gateway_calls WHERE call_id LIKE 'orphan-%' AND ended IS NOT NULL AND ok=0 AND projection_dirty=1"
        ),
        2
    );
    assert_eq!(
        n(&s, "SELECT COUNT(*) FROM gateway_calls WHERE ended IS NULL"),
        0
    );
    sql(&s, "DROP TRIGGER fail_projection");
    for _ in 0..3 {
        s.stats().gateway_apply_local(BATCH).unwrap();
    }
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_result_ready"), 0);
    assert_eq!(
        n(
            &s,
            "SELECT COUNT(*) FROM gateway_calls WHERE projection_dirty=1"
        ),
        0
    );
    assert_eq!(
        n(
            &s,
            "SELECT COUNT(*) FROM usage_records WHERE ok=1 OR json_extract(detail,'$.pending')=1"
        ),
        0
    );
}

#[test]
fn projection_rollback_keeps_bill_cube_inbox_and_queue_then_recovers_same_call() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir.path().join("steve.sqlite3"));
    seed(&s, 1);
    outage(&s, None);
    end(&s, "call-0000", true);
    let before = bill(&s, "call-0000");
    let ticket = n(&s, "SELECT ticket FROM gateway_result_ready");
    sql(
        &s,
        "DROP TRIGGER fail_projection; CREATE TRIGGER fail_clean AFTER UPDATE OF projection_dirty ON gateway_calls WHEN NEW.projection_dirty=0 BEGIN SELECT RAISE(ABORT,'rollback after bill and queue cleanup'); END",
    );
    s.stats()
        .gateway_local_receive(
            "call-0000",
            &report("final", 20, Measurement::Final),
            AT + 5,
        )
        .unwrap();
    assert!(s.stats().gateway_apply_local(BATCH).is_err());
    assert_eq!(bill(&s, "call-0000"), before);
    assert_eq!(n(&s, "SELECT SUM(input) FROM stat_daily"), 10);
    assert_eq!(n(&s, "SELECT applied FROM gateway_local_inbox"), 0);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_usage_reports"), 1);
    assert_eq!(n(&s, "SELECT projection_dirty FROM gateway_calls"), 1);
    assert!(n(&s, "SELECT ticket FROM gateway_result_ready") > ticket);
    sql(&s, "DROP TRIGGER fail_clean");
    assert_eq!(s.stats().gateway_apply_local(BATCH).unwrap(), 1);
    assert_eq!(n(&s, "SELECT applied FROM gateway_local_inbox"), 1);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_usage_reports"), 2);
    assert_eq!(n(&s, "SELECT SUM(input) FROM stat_daily"), 20);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM usage_records"), 1);
    assert_eq!(bill(&s, "call-0000")["ok"], false);
    assert_eq!(bill(&s, "call-0000")["input"], 20);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_result_ready"), 0);
}

#[test]
fn concurrent_workers_and_producer_cannot_move_new_work_ahead_of_healthy_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let s = open(&path);
    seed(&s, 385);
    outage(&s, None);
    for i in 0..385 {
        end(&s, &format!("call-{i:04}"), i % 2 == 0);
    }
    outage(&s, Some("call-0384"));
    // Independent Store write locks/connections exercise SQLite's IMMEDIATE serialization too.
    let a = open(&path);
    let b = open(&path);
    let producer = open(&path);
    let gate = std::sync::Arc::new(std::sync::Barrier::new(3));
    std::thread::scope(|scope| {
        for worker in [a, b] {
            let gate = gate.clone();
            scope.spawn(move || {
                gate.wait();
                for _ in 0..4 {
                    assert!(worker.stats().gateway_apply_local(BATCH).is_err());
                }
            });
        }
        let gate = gate.clone();
        scope.spawn(move || {
            gate.wait();
            for i in 0..160 {
                let call = format!("new-{i}");
                begin(&producer, &call);
                end(&producer, &call, false);
            }
        });
    });
    settled(&s, "call-0384", true);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_calls"), 545);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM usage_records"), 385);
    assert_eq!(n(&s, "SELECT SUM(input) FROM usage_records"), 3850);
    assert_eq!(
        n(&s, "SELECT COUNT(*) FROM gateway_calls WHERE ended IS NULL"),
        0
    );
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_result_ready"), 544);
}

#[test]
fn migration_seeds_old_dirty_calls_and_preserves_fifo_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = rusqlite::Connection::open(&path).unwrap();
    // Other branches can publish migrations before this one is merged.
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("CREATE TABLE gateway_result_ready"))
        .unwrap();
    for m in &db::MIGRATIONS[..before] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL)")
        .unwrap();
    c.execute("INSERT INTO schema_version VALUES(?)", [before as i64])
        .unwrap();
    for (id, dirty) in [("old-finish", 1), ("old-clean", 0), ("old-cancel", 1)] {
        c.execute("INSERT INTO gateway_calls(call_id,workspace_id,purpose,started,day,ended,ok,projection_dirty) VALUES(?,'default','test',?,'2026-11-01',?,0,?)",params![id,AT,AT+1,dirty]).unwrap();
    }
    drop(c);
    let s = open(&path);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_result_ready"), 2);
    assert_eq!(
        n(
            &s,
            "SELECT COUNT(*) FROM gateway_result_ready WHERE call_id='old-clean'"
        ),
        0
    );
    outage(&s, None);
    assert!(s.stats().gateway_apply_local(1).is_err());
    let head = s
        .fixture_read::<_, StoreError>(|c| {
            Ok(c.query_row(
                "SELECT call_id FROM gateway_result_ready ORDER BY ticket LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )?)
        })
        .unwrap();
    assert_eq!(head, "old-cancel");
    drop(s);
    let s = open(&path);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_result_ready"), 2);
    sql(&s, "DROP TRIGGER fail_projection");
    s.stats().gateway_apply_local(1).unwrap();
    assert_eq!(
        n(
            &s,
            "SELECT projection_dirty FROM gateway_calls WHERE call_id='old-cancel'"
        ),
        0
    );
    assert_eq!(
        n(
            &s,
            "SELECT projection_dirty FROM gateway_calls WHERE call_id='old-finish'"
        ),
        1
    );
    assert_eq!(n(&s, "SELECT COUNT(*) FROM gateway_calls"), 3);
}

#[test]
fn successful_finish_uses_the_same_fair_queue_as_failed_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir.path().join("steve.sqlite3"));
    seed(&s, 129);
    outage(&s, None);
    for i in 0..128 {
        end(&s, &format!("call-{i:04}"), false);
    }
    assert!(
        s.stats()
            .gateway_finish(
                "call-0128",
                &CallFinish {
                    ok: true,
                    model: "m".into(),
                    harness: "fixture".into(),
                    attempts: 1,
                    duration_ms: 55,
                    ..Default::default()
                }
            )
            .is_err()
    );
    outage(&s, Some("call-0128"));
    assert!(s.stats().gateway_apply_local(BATCH).is_err());
    assert_eq!(
        n(
            &s,
            "SELECT projection_dirty FROM gateway_calls WHERE call_id='call-0128'"
        ),
        1
    );
    assert!(s.stats().gateway_apply_local(BATCH).is_err());
    let b = bill(&s, "call-0128");
    assert_eq!(b["ok"], true);
    assert_eq!(b["pending"], false);
    assert_eq!(b["duration"], 55);
    assert_eq!(b["input"], 10);
    assert_eq!(b["cost"], 0.25);
    assert_eq!(
        n(
            &s,
            "SELECT projection_dirty FROM gateway_calls WHERE call_id='call-0128'"
        ),
        0
    );
    assert_eq!(n(&s, "SELECT SUM(input) FROM usage_records"), 1290);
    assert_eq!(n(&s, "SELECT COUNT(*) FROM usage_records"), 129);
}

#[test]
fn startup_still_refuses_a_failure_to_persist_orphan_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir.path().join("steve.sqlite3"));
    begin(&s, "orphan");
    sql(
        &s,
        "CREATE TRIGGER fail_result BEFORE UPDATE OF ended ON gateway_calls BEGIN SELECT RAISE(ABORT,'result persistence unavailable'); END",
    );
    assert!(s.stats().gateway_recover().is_err());
    assert_eq!(
        n(&s, "SELECT COUNT(*) FROM gateway_calls WHERE ended IS NULL"),
        1
    );
    sql(&s, "DROP TRIGGER fail_result");
    assert_eq!(s.stats().gateway_recover().unwrap(), 1);
    assert_eq!(
        n(
            &s,
            "SELECT COUNT(*) FROM gateway_calls WHERE ended IS NULL OR ok<>0 OR projection_dirty<>0"
        ),
        0
    );
}

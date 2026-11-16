//! Durable one-shot bills use real SQLite and the same summary/item consumers as turns.
use serde_json::json;
use steve_node_api::{Epoch, Measurement, Tokens, UsageReport, UsageScope};
use steve_stats::{metrics, views};
use steve_store::{
    NewBot, NewItem, NoPublisher, Store, WS,
    gateway_ledger::{CallBegin, CallFinish},
};
const AT: i64 = 1_793_491_200_000;
fn setup() -> (tempfile::TempDir, Store, String) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("bill.sqlite"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s.bots()
        .seed(
            "bill",
            false,
            &NewBot {
                name: "Bill".into(),
                harness: "fixture".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    let i = s
        .items()
        .create(
            WS,
            &NewItem {
                title: "bill".into(),
                goal: "bill".into(),
                bot_id: "bill".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id;
    (dir, s, i)
}
fn begin(s: &Store, item: &str, call: &str, attempt: &str, model: &str) {
    s.stats()
        .gateway_begin(&CallBegin {
            id: call.into(),
            workspace: WS.into(),
            item: Some(item.into()),
            purpose: "test".into(),
            started: AT,
            day: "2026-11-01".into(),
            ..Default::default()
        })
        .unwrap();
    s.stats()
        .gateway_attempt(call, attempt, "fixture", model)
        .unwrap();
}
fn r(id: &str, model: &str, n: u64, measurement: Measurement) -> UsageReport {
    UsageReport {
        report_id: id.into(),
        model: Some(model.into()),
        meter_epoch: Epoch::new("meter").unwrap(),
        scope: UsageScope::Main,
        measurement,
        tokens: Tokens {
            input: Some(n),
            output: Some(0),
            cache_read: Some(0),
            cache_write: Some(0),
            reasoning: None,
        },
        cost: None,
        context_used: None,
        context_limit: None,
    }
}
fn total(s: &Store) -> i64 {
    s.reader::<_, steve_store::StoreError>(|c| {
        Ok(c.query_row(
            "SELECT COALESCE(SUM(input),0) FROM usage_records",
            [],
            |r| r.get(0),
        )?)
    })
    .unwrap()
}
#[test]
fn failed_transaction_rolls_back_identity_meter_projection_and_retry_recovers() {
    let (_dir, s, item) = setup();
    begin(&s, &item, "call", "try", "m");
    let live = r("live", "m", 10, Measurement::Cumulative);
    let result: std::result::Result<(), steve_store::StoreError> = s.transaction(|_| {
        s.stats().gateway_report("try", &live, AT + 5)?;
        Err(steve_store::StoreError::Hook(
            "injected apply failure".into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(total(&s), 0);
    s.reader::<_, steve_store::StoreError>(|c| {
        for table in ["gateway_usage_reports", "gateway_usage_meters"] {
            assert_eq!(
                c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
        }
        Ok(())
    })
    .unwrap();
    assert!(s.stats().gateway_report("try", &live, AT + 5).unwrap());
    assert_eq!(total(&s), 10);
    assert!(!s.stats().gateway_report("try", &live, AT + 99).unwrap());
    s.stats().gateway_cancel("call", "timeout").unwrap();
    s.stats()
        .gateway_report("try", &r("final", "m", 20, Measurement::Final), AT + 8)
        .unwrap();
    assert_eq!(total(&s), 20);
    s.reader::<_,steve_store::StoreError>(|c| {let (ok,n,source):(bool,i64,i64)=c.query_row("SELECT ok,(SELECT COUNT(*) FROM usage_records),(SELECT source_at FROM gateway_usage_reports WHERE report_id='live') FROM gateway_calls",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;assert!(!ok);assert_eq!((n,source),(1,AT+5));Ok(())}).unwrap();
}
#[test]
fn same_model_and_cross_model_retries_reopen_replay_and_rebuild_keep_one_call() {
    for model in ["m", "other"] {
        let (dir, s, item) = setup();
        begin(&s, &item, "call", "first", "m");
        s.stats()
            .gateway_report(
                "first",
                &r("same-report-id", "m", 10, Measurement::Final),
                AT + 1,
            )
            .unwrap();
        s.stats()
            .gateway_attempt_end("first", false, Some("503"), AT + 2)
            .unwrap();
        s.stats()
            .gateway_attempt("call", "retry", "fixture", model)
            .unwrap();
        s.stats()
            .gateway_report(
                "retry",
                &r("same-report-id", model, 20, Measurement::Final),
                AT + 3,
            )
            .unwrap();
        s.stats()
            .gateway_finish(
                "call",
                &CallFinish {
                    ok: true,
                    model: model.into(),
                    harness: "fixture".into(),
                    attempts: 2,
                    duration_ms: 88,
                    failures: vec!["503".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(total(&s), 30);
        let check = |s: &Store| {
            s.reader::<_, steve_store::StoreError>(|c| {
                let rows = c
                    .prepare("SELECT detail,attempts,duration_ms FROM usage_records ORDER BY id")?
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(rows.len(), if model == "m" { 1 } else { 2 });
                assert_eq!((rows[0].1, rows[0].2), (2, Some(88)));
                for (i, (detail, _, _)) in rows.iter().enumerate() {
                    let d: serde_json::Value = serde_json::from_str(detail)?;
                    assert_eq!(d["call_id"], "call");
                    if i == 0 {
                        assert_eq!(d["failures"], json!(["503"]));
                    }
                }
                let previous: String = c.query_row(
                    "SELECT previous_attempt FROM gateway_attempts WHERE attempt_id='retry'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(previous, "first");
                let q = metrics::Query {
                    workspace_id: WS.into(),
                    from: "2026-11-01".into(),
                    to: "2026-11-01".into(),
                    ..Default::default()
                };
                assert_eq!(
                    metrics::summary(c, &q)?.unwrap().usage.coverage.total,
                    Some(30)
                );
                assert_eq!(
                    views::item_detail(c, &item, AT + 100, false)?
                        .unwrap()
                        .1
                        .usage
                        .coverage
                        .total,
                    Some(30)
                );
                Ok(())
            })
            .unwrap()
        };
        check(&s);
        // Rebuild inside the actual store transaction, then reopen the same database.
        s.transaction(|tx| steve_stats::refresh::rebuild_in(tx, WS))
            .unwrap();
        check(&s);
        drop(s);
        let s = Store::open(dir.path().join("bill.sqlite"), NoPublisher).unwrap();
        assert!(
            !s.stats()
                .gateway_report(
                    "retry",
                    &r("same-report-id", model, 20, Measurement::Final),
                    AT + 999
                )
                .unwrap()
        );
        check(&s);
    }
}
#[test]
fn parallel_calls_keep_ownership_filters_and_source_day_after_timezone_rebuild() {
    let (_dir, s, item) = setup();
    let jobs = (0..8)
        .map(|i| {
            let s = s.clone();
            let item = item.clone();
            std::thread::spawn(move || {
                let call = format!("c{i}");
                let attempt = format!("a{i}");
                begin(&s, &item, &call, &attempt, &format!("m{}", i % 2));
                s.stats()
                    .gateway_report(
                        &attempt,
                        &r("f", &format!("m{}", i % 2), 10, Measurement::Final),
                        AT + 86400_000,
                    )
                    .unwrap();
                s.stats()
                    .gateway_finish(
                        &call,
                        &CallFinish {
                            ok: i % 2 == 0,
                            model: format!("m{}", i % 2),
                            harness: "fixture".into(),
                            attempts: 1,
                            ..Default::default()
                        },
                    )
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for j in jobs {
        j.join().unwrap();
    }
    s.reader::<_, steve_store::StoreError>(|c| {
        for (bot, model, expected) in [
            (Some("bill"), None, 80),
            (Some("other"), None, 0),
            (None, Some("m1"), 40),
        ] {
            let q = metrics::Query {
                workspace_id: WS.into(),
                from: "2026-11-01".into(),
                to: "2026-11-01".into(),
                bot: bot.map(String::from),
                model: model.map(String::from),
                ..Default::default()
            };
            assert_eq!(
                metrics::summary(c, &q)?
                    .unwrap()
                    .usage
                    .coverage
                    .total
                    .unwrap_or(0),
                expected
            );
        }
        assert_eq!(
            c.query_row(
                "SELECT COUNT(DISTINCT json_extract(detail,'$.call_id')) FROM usage_records",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            8
        );
        assert_eq!(
            c.query_row("SELECT MIN(ts) FROM usage_records", [], |r| r
                .get::<_, i64>(0))?,
            AT
        );
        Ok(())
    })
    .unwrap();
    s.stats()
        .gateway_begin(&CallBegin {
            id: "not-dispatched".into(),
            workspace: WS.into(),
            started: AT,
            day: "2026-11-01".into(),
            ..Default::default()
        })
        .unwrap();
    s.settings()
        .set(
            WS,
            &serde_json::Map::from_iter([("stats.tz".into(), json!("America/Los_Angeles"))]),
            "test",
        )
        .unwrap();
    s.transaction(|tx| steve_stats::refresh::rebuild_in(tx, WS))
        .unwrap();
    s.reader::<_, steve_store::StoreError>(|c| {
        assert_eq!(
            c.query_row(
                "SELECT day FROM gateway_calls WHERE call_id='not-dispatched'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "2026-10-31"
        );
        Ok(())
    })
    .unwrap();
    s.stats()
        .gateway_report(
            "a0",
            &r("correction", "m0", 20, Measurement::Final),
            AT + 2 * 86400_000,
        )
        .unwrap();
    s.reader::<_, steve_store::StoreError>(|c| {
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM usage_records WHERE day='2026-10-31'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            8
        );
        assert_eq!(
            c.query_row("SELECT SUM(input) FROM stat_daily", [], |r| r
                .get::<_, i64>(0))?,
            90
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn ownership_is_frozen_at_dispatch_and_workspace_project_bot_filters_agree() {
    use steve_store::{NewProject, NewWorkspace};
    let (_dir, s, item) = setup();
    let project = s
        .projects()
        .create(
            WS,
            &NewProject {
                name: "project".into(),
                path: ".".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .id;
    s.transaction(|tx| {
        tx.execute(
            "UPDATE items SET project_id=? WHERE id=?",
            rusqlite::params![project, item],
        )?;
        Ok::<_, steve_store::StoreError>(())
    })
    .unwrap();
    begin(&s, &item, "original", "attempt", "m");
    s.bots()
        .seed(
            "changed",
            false,
            &NewBot {
                name: "Changed".into(),
                harness: "fixture".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    // Later reassignment cannot move already spent usage to a different project/bot.
    s.transaction(|tx| {
        tx.execute(
            "UPDATE items SET project_id=NULL,bot_id='changed' WHERE id=?",
            [&item],
        )?;
        Ok::<_, steve_store::StoreError>(())
    })
    .unwrap();
    s.stats()
        .gateway_report("attempt", &r("f", "m", 23, Measurement::Final), AT + 90000)
        .unwrap();
    let user = s
        .users()
        .create("test-user", "Test", None, "admin")
        .unwrap();
    let other = s
        .workspaces()
        .create(
            &NewWorkspace {
                name: "other".into(),
                ..Default::default()
            },
            &user,
        )
        .unwrap()
        .id;
    let foreign = s.stats().gateway_begin(&CallBegin {
        id: "foreign-item".into(),
        workspace: other.clone(),
        item: Some(item.clone()),
        purpose: "test".into(),
        started: AT,
        day: "2026-11-01".into(),
        ..Default::default()
    });
    assert!(
        foreign.is_err(),
        "an item cannot be billed to another workspace"
    );
    s.stats()
        .gateway_begin(&CallBegin {
            id: "other".into(),
            workspace: other.clone(),
            purpose: "test".into(),
            started: AT,
            day: "2026-11-01".into(),
            ..Default::default()
        })
        .unwrap();
    s.stats()
        .gateway_attempt("other", "other-a", "fixture", "m")
        .unwrap();
    s.stats()
        .gateway_report("other-a", &r("f", "m", 41, Measurement::Final), AT)
        .unwrap();
    s.reader::<_, steve_store::StoreError>(|c| {
        for (ws, prj, bot, n) in [
            (WS, Some(project.as_str()), Some("bill"), 23),
            (WS, Some("no-project"), None, 0),
            (other.as_str(), None, None, 41),
        ] {
            let q = metrics::Query {
                workspace_id: ws.into(),
                from: "2026-11-01".into(),
                to: "2026-11-01".into(),
                project: prj.map(String::from),
                bot: bot.map(String::from),
                ..Default::default()
            };
            assert_eq!(
                metrics::summary(c, &q)?
                    .unwrap()
                    .usage
                    .coverage
                    .total
                    .unwrap_or(0),
                n
            );
        }
        assert_eq!(
            views::item_detail(c, &item, AT + 100000, false)?
                .unwrap()
                .1
                .usage
                .coverage
                .total,
            Some(23)
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn local_inbox_and_caller_failure_recover_after_projection_failure_and_reopen() {
    let (dir, s, item) = setup();
    begin(&s, &item, "local-call", "local-attempt", "m");
    let report = r("final", "m", 20, Measurement::Final);
    assert!(
        s.stats()
            .gateway_local_receive("local-attempt", &report, AT + 5)
            .unwrap()
    );
    s.transaction(|tx|{tx.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON usage_records BEGIN SELECT RAISE(ABORT,'projection fault'); END")?;Ok::<_,steve_store::StoreError>(())}).unwrap();
    assert!(s.stats().gateway_apply_local(128).is_err());
    assert!(
        s.stats()
            .gateway_cancel("local-call", "caller dropped")
            .is_err()
    );
    s.reader::<_, steve_store::StoreError>(|c| {
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM gateway_usage_reports", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            c.query_row("SELECT applied FROM gateway_local_inbox", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            c.query_row(
                "SELECT ended IS NOT NULL AND ok=0 AND projection_dirty=1 FROM gateway_calls",
                [],
                |r| r.get::<_, bool>(0)
            )?,
            true
        );
        Ok(())
    })
    .unwrap();
    drop(s);
    let s = Store::open(dir.path().join("bill.sqlite"), NoPublisher).unwrap();
    s.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER fail_projection")?;
        Ok::<_, steve_store::StoreError>(())
    })
    .unwrap();
    s.stats().gateway_recover().unwrap();
    assert_eq!(total(&s), 20);
    assert!(
        !s.stats()
            .gateway_local_receive("local-attempt", &report, AT + 99)
            .unwrap()
    );
    assert_eq!(s.stats().gateway_apply_local(128).unwrap(), 0);
    s.reader::<_,steve_store::StoreError>(|c| {
        let row:(i64,i64,String,i64)=c.query_row("SELECT ok,(SELECT applied FROM gateway_local_inbox),json_extract(detail,'$.call_id'),(SELECT observed_at FROM gateway_local_inbox) FROM usage_records",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!(row,(0,1,"local-call".into(),AT+5));Ok(())
    }).unwrap();
}

#[test]
fn local_replay_keeps_attempt_order_after_a_fault_without_blocking_other_calls() {
    let (_dir, s, item) = setup();
    begin(&s, &item, "ordered", "a", "m");
    begin(&s, &item, "other", "b", "m");
    s.stats()
        .gateway_local_receive("a", &r("c", "m", 10, Measurement::Cumulative), AT + 1)
        .unwrap();
    s.stats()
        .gateway_local_receive("a", &r("d", "m", 5, Measurement::Delta), AT + 2)
        .unwrap();
    s.stats()
        .gateway_local_receive("b", &r("ok", "m", 2, Measurement::Final), AT + 3)
        .unwrap();
    s.transaction(|tx| {tx.execute_batch("CREATE TRIGGER fail_first BEFORE INSERT ON gateway_usage_reports WHEN NEW.report_id='c' BEGIN SELECT RAISE(ABORT,'first report fault'); END")?;Ok::<_,steve_store::StoreError>(())}).unwrap();
    assert!(s.stats().gateway_apply_local(1).is_err());
    assert_eq!(
        s.stats().gateway_apply_local(1).unwrap(),
        1,
        "another attempt proceeds before retrying the broken one"
    );
    assert_eq!(
        total(&s),
        2,
        "the later delta must wait for its own cumulative report; other calls continue"
    );
    s.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER fail_first")?;
        Ok::<_, steve_store::StoreError>(())
    })
    .unwrap();
    assert_eq!(s.stats().gateway_apply_local(128).unwrap(), 2);
    assert_eq!(
        total(&s),
        17,
        "Cumulative10 then Delta5 must remain15 after recovery, plus the independent2"
    );
}

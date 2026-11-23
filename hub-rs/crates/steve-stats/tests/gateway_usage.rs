//! Durable one-shot bills use real SQLite and the same summary/item consumers as turns.
use steve_store::testkit::stats as fixture;

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
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    // AT is 2026-11-01 00:00 UTC and the queries ask for that day: say so, not the host's zone
    s.settings()
        .set(
            WS,
            &serde_json::Map::from_iter([("stats.tz".into(), json!("UTC"))]),
            "test",
        )
        .unwrap();
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
                tags: vec![
                    s.tags()
                        .kinds(WS)
                        .unwrap()
                        .into_iter()
                        .find(|tag| tag.name == "feat")
                        .unwrap()
                        .id,
                ],
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
#[test]
fn failed_transaction_rolls_back_identity_meter_projection_and_retry_recovers() {
    let (_dir, s, item) = setup();
    begin(&s, &item, "call", "try", "m");
    let live = r("live", "m", 10, Measurement::Cumulative);
    let result: std::result::Result<(), steve_store::StoreError> = s.write(|_| {
        s.stats().gateway_report("try", &live, AT + 5)?;
        Err(steve_store::StoreError::Hook(
            "injected apply failure".into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(fixture::input_total(&s).unwrap(), 0);
    assert_eq!(fixture::gateway_identity_count(&s).unwrap(), (0, 0));
    assert!(s.stats().gateway_report("try", &live, AT + 5).unwrap());
    assert_eq!(fixture::input_total(&s).unwrap(), 10);
    assert!(!s.stats().gateway_report("try", &live, AT + 99).unwrap());
    s.stats().gateway_cancel("call", "timeout").unwrap();
    s.stats()
        .gateway_report("try", &r("final", "m", 20, Measurement::Final), AT + 8)
        .unwrap();
    assert_eq!(fixture::input_total(&s).unwrap(), 20);
    assert_eq!(
        fixture::gateway_cancellation(&s).unwrap(),
        (false, 1, AT + 5)
    );
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
        assert_eq!(fixture::input_total(&s).unwrap(), 30);
        let check = |s: &Store| {
            let rows = fixture::gateway_usage(s).unwrap();
            assert_eq!(rows.len(), if model == "m" { 1 } else { 2 });
            assert_eq!((rows[0].1, rows[0].2), (2, Some(88)));
            for (i, (detail, _, _)) in rows.iter().enumerate() {
                let d: serde_json::Value = serde_json::from_str(detail).unwrap();
                assert_eq!(d["call_id"], "call");
                if i == 0 {
                    assert_eq!(d["failures"], json!(["503"]));
                }
            }
            let previous = fixture::gateway_previous(s).unwrap();
            assert_eq!(previous, "first");
            s.read::<_, steve_store::StoreError>(|c| {
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
        s.write(|tx| steve_stats::refresh::rebuild_in(tx, WS))
            .unwrap();
        check(&s);
        drop(s);
        let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
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
    s.read::<_, steve_store::StoreError>(|c| {
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
        Ok(())
    })
    .unwrap();
    assert_eq!(fixture::gateway_call_count(&s).unwrap(), (8, AT));
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
    s.write(|tx| steve_stats::refresh::rebuild_in(tx, WS))
        .unwrap();
    assert_eq!(fixture::gateway_source_day(&s).unwrap(), "2026-10-31");
    s.stats()
        .gateway_report(
            "a0",
            &r("correction", "m0", 20, Measurement::Final),
            AT + 2 * 86400_000,
        )
        .unwrap();
    assert_eq!(fixture::gateway_daily(&s).unwrap(), (8, 90));
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
    fixture::assign_project(&s, &item, &project).unwrap();
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
    fixture::reassign_bot(&s, &item).unwrap();
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
    s.read::<_, steve_store::StoreError>(|c| {
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
    fixture::install_fail_projection(&s).unwrap();
    assert!(s.stats().gateway_apply_local(128).is_err());
    assert!(
        s.stats()
            .gateway_cancel("local-call", "caller dropped")
            .is_err()
    );
    assert_eq!(fixture::gateway_unapplied(&s).unwrap(), (0, 0, true));
    drop(s);
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    fixture::remove_fail_projection(&s).unwrap();
    s.stats().gateway_recover().unwrap();
    assert_eq!(fixture::input_total(&s).unwrap(), 20);
    assert!(
        !s.stats()
            .gateway_local_receive("local-attempt", &report, AT + 99)
            .unwrap()
    );
    assert_eq!(s.stats().gateway_apply_local(128).unwrap(), 0);
    assert_eq!(
        fixture::gateway_recovered(&s).unwrap(),
        (0, 1, "local-call".into(), AT + 5)
    );
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
    fixture::install_fail_first(&s).unwrap();
    assert!(s.stats().gateway_apply_local(1).is_err());
    assert_eq!(
        s.stats().gateway_apply_local(1).unwrap(),
        1,
        "another attempt proceeds before retrying the broken one"
    );
    assert_eq!(
        fixture::input_total(&s).unwrap(),
        2,
        "the later delta must wait for its own cumulative report; other calls continue"
    );
    fixture::remove_fail_first(&s).unwrap();
    assert_eq!(s.stats().gateway_apply_local(128).unwrap(), 2);
    assert_eq!(
        fixture::input_total(&s).unwrap(),
        17,
        "Cumulative10 then Delta5 must remain15 after recovery, plus the independent2"
    );
}

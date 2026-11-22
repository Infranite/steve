//! Real SQLite ledger -> session/item/workspace APIs, including reopening the database. No harness,
//! model credentials or installed Hub are needed for accounting acceptance.
use steve_node_api::{Epoch, Measurement, Tokens, UsageReport, UsageScope};
use steve_stats::{coverage::CoverageState, metrics, views};
use steve_store::{NewBot, NewItem, NewSession, NoPublisher, Store, TurnBegin, TurnFinish, WS};

const AT: i64 = 1_793_491_200_000;
const DAY: &str = "2026-11-01";

fn fixture() -> (tempfile::TempDir, Store, String, Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s.bots()
        .seed(
            "dev",
            false,
            &NewBot {
                name: "Dev".into(),
                harness: "test".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    s.settings()
        .set(
            WS,
            &serde_json::Map::from_iter([("stats.tz".into(), serde_json::json!("UTC"))]),
            "test",
        )
        .unwrap();
    let item = s
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
                title: "Live usage".into(),
                goal: "g".into(),
                bot_id: "dev".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id;
    let mut sessions = vec![];
    let mut turns = vec![];
    for _ in 0..2 {
        let session = s
            .sessions()
            .create(&NewSession {
                workspace_id: WS.into(),
                item_id: Some(item.clone()),
                bot_id: Some("dev".into()),
                harness: "test".into(),
                model: Some("m".into()),
                reasoning_effort: None,
                cwd: ".".into(),
                title: "usage".into(),
                token: "test".into(),
            })
            .unwrap()
            .id;
        turns.push(
            s.stats()
                .turn_begin(&TurnBegin {
                    workspace_id: WS.into(),
                    session_id: session.clone(),
                    item_id: Some(item.clone()),
                    bot_id: Some("dev".into()),
                    harness: "test".into(),
                    started: AT,
                    ..Default::default()
                })
                .unwrap(),
        );
        sessions.push(session);
    }
    (dir, s, item, sessions, turns)
}

fn report(id: &str, m: Measurement, n: Option<u64>) -> UsageReport {
    UsageReport {
        report_id: id.into(),
        meter_epoch: Epoch::new("epoch1").unwrap(),
        model: Some("m".into()),
        scope: UsageScope::Main,
        measurement: m,
        tokens: Tokens {
            input: n,
            output: n.map(|_| 0),
            cache_read: n.map(|_| 0),
            cache_write: n.map(|_| 0),
            reasoning: None,
        },
        context_used: None,
        context_limit: None,
        cost: None,
    }
}

fn totals(s: &Store, item: &str, sessions: &[String], want: i64) {
    s.reader::<_, steve_store::StoreError>(|c| {
        let session_total: i64 = sessions
            .iter()
            .map(|id| views::session(c, id).unwrap().unwrap().1.usage.total)
            .sum();
        let detail = views::item_detail(c, item, AT + 1000, false)?.unwrap().1;
        let q = metrics::Query {
            workspace_id: WS.into(),
            from: DAY.into(),
            to: DAY.into(),
            ..Default::default()
        };
        let summary = metrics::summary(c, &q)?.unwrap();
        assert_eq!(
            (session_total, detail.usage.total, summary.usage.total),
            (want, want, want)
        );
        assert_eq!(detail.usage.coverage, summary.usage.coverage);
        assert_eq!(
            metrics::breakdown(
                &metrics::daily(c, &q)?,
                metrics::By::Model,
                &metrics::book_of(c, WS)?
            )
            .iter()
            .map(|r| r.usage.total)
            .sum::<i64>(),
            want
        );
        assert_eq!(
            metrics::hourly(c, &q)?
                .iter()
                .map(|r| r.total())
                .sum::<i64>(),
            want
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn live_snapshots_final_cancel_restart_and_replay_share_one_total() {
    let (dir, s, item, sessions, turns) = fixture();
    let a = report("d1", Measurement::Delta, Some(10));
    assert!(s.stats().usage_report(&turns[0], &a, "m", DAY).unwrap());
    totals(&s, &item, &sessions, 10);
    assert!(!s.stats().usage_report(&turns[0], &a, "m", DAY).unwrap());
    s.stats()
        .usage_report(
            &turns[0],
            &report("snap", Measurement::Cumulative, Some(25)),
            "m",
            DAY,
        )
        .unwrap();
    totals(&s, &item, &sessions, 25);
    drop(s);
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    assert!(!s.stats().usage_report(&turns[0], &a, "m", DAY).unwrap());
    // The other session runs concurrently; its report id can be identical without collision.
    s.stats().usage_report(&turns[1], &a, "m", DAY).unwrap();
    let final_ = report("final", Measurement::Final, Some(30));
    s.stats()
        .usage_report(&turns[0], &final_, "m", DAY)
        .unwrap();
    s.stats()
        .usage_report(
            &turns[0],
            &report("late-delta", Measurement::Delta, Some(500)),
            "m",
            DAY,
        )
        .unwrap();
    totals(&s, &item, &sessions, 40);
    for (i, id) in turns.iter().enumerate() {
        let f = TurnFinish {
            turn_id: id.clone(),
            ended: AT + 1000,
            day: DAY.into(),
            stop_reason: Some(if i == 0 { "end_turn" } else { "cancelled" }.into()),
            ..Default::default()
        };
        assert!(s.stats().turn_finish(&f).unwrap());
        assert!(!s.stats().turn_finish(&f).unwrap());
    }
    assert!(
        !s.stats()
            .usage_report(&turns[0], &final_, "m", DAY)
            .unwrap()
    );
    totals(&s, &item, &sessions, 40);
    steve_stats::rebuild(&s, WS).unwrap();
    totals(&s, &item, &sessions, 40);
}

#[test]
fn epochs_models_and_all_main_scope_are_isolated_and_replaceable() {
    let (_dir, s, item, sessions, turns) = fixture();
    s.stats()
        .usage_report(
            &turns[0],
            &report("main", Measurement::Delta, Some(10)),
            "m",
            DAY,
        )
        .unwrap();
    let mut all = report("all", Measurement::Final, Some(13));
    all.scope = UsageScope::All;
    s.stats().usage_report(&turns[0], &all, "m", DAY).unwrap();
    let mut other = report("other-model", Measurement::Final, Some(7));
    other.model = Some("another".into());
    other.scope = UsageScope::All;
    s.stats().usage_report(&turns[0], &other, "m", DAY).unwrap();
    let mut epoch = report("other-epoch", Measurement::Cumulative, Some(5));
    epoch.meter_epoch = Epoch::new("epoch2").unwrap();
    s.stats().usage_report(&turns[0], &epoch, "m", DAY).unwrap();
    totals(&s, &item, &sessions, 25);
    let mut corrected = all;
    corrected.report_id = "corrected".into();
    corrected.tokens.input = Some(12);
    s.stats()
        .usage_report(&turns[0], &corrected, "m", DAY)
        .unwrap();
    totals(&s, &item, &sessions, 24);
}

#[test]
fn unreported_unavailable_partial_and_true_zero_are_distinct() {
    let (_dir, s, item, sessions, turns) = fixture();
    let cov = |id: &str| {
        s.reader::<_, steve_store::StoreError>(|c| {
            Ok(views::session(c, id)?.unwrap().1.usage.coverage)
        })
        .unwrap()
    };
    assert_eq!(cov(&sessions[0]).state, CoverageState::NotReported);
    assert_eq!(cov(&sessions[0]).total, None);
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: turns[0].clone(),
            ended: AT + 1,
            day: DAY.into(),
            stop_reason: Some("interrupted".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(cov(&sessions[0]).state, CoverageState::Unavailable);
    s.stats()
        .usage_report(
            &turns[1],
            &report("zero", Measurement::Final, Some(0)),
            "m",
            DAY,
        )
        .unwrap();
    assert_eq!(cov(&sessions[1]).state, CoverageState::Complete);
    assert_eq!(cov(&sessions[1]).total, Some(0));
    let whole = s
        .reader::<_, steve_store::StoreError>(|c| {
            Ok(views::item_detail(c, &item, AT + 2, true)?
                .unwrap()
                .1
                .usage
                .coverage)
        })
        .unwrap();
    assert_eq!(whole.state, CoverageState::Partial);
    assert_eq!(whole.total, Some(0));
    assert_eq!(whole.unavailable, 1);
    // A late recovery can repair the missing result, without inventing its unknown output/cache.
    let mut recovered = report("recovered", Measurement::Final, None);
    recovered.tokens.input = Some(4);
    s.stats()
        .usage_report(&turns[0], &recovered, "m", DAY)
        .unwrap();
    assert_eq!(cov(&sessions[0]).state, CoverageState::Partial);
    assert_eq!(cov(&sessions[0]).output, None);
    totals(&s, &item, &sessions, 4);
}

#[test]
fn rollback_does_not_consume_a_report_id_or_publish_a_projection() {
    let (_dir, s, item, sessions, turns) = fixture();
    let r = report("rollback", Measurement::Delta, Some(11));
    let failed = s.transaction::<(), steve_store::StoreError>(|_| {
        s.stats().usage_report(&turns[0], &r, "m", DAY)?;
        Err(steve_store::StoreError::Hook("rollback".into()))
    });
    assert!(failed.is_err());
    totals(&s, &item, &sessions, 0);
    assert!(s.stats().usage_report(&turns[0], &r, "m", DAY).unwrap());
    totals(&s, &item, &sessions, 11);
}

#[test]
fn concurrent_sessions_do_not_share_report_ids_or_meter_heads() {
    let (_dir, s, item, sessions, turns) = fixture();
    std::thread::scope(|scope| {
        for turn in &turns {
            let s = s.clone();
            scope.spawn(move || {
                for i in 0..20 {
                    let r = report(&format!("delta-{i}"), Measurement::Delta, Some(1));
                    assert!(s.stats().usage_report(turn, &r, "m", DAY).unwrap());
                    assert!(!s.stats().usage_report(turn, &r, "m", DAY).unwrap());
                }
            });
        }
    });
    totals(&s, &item, &sessions, 40);
}

#[test]
fn standalone_cost_only_and_other_models_stay_missing() {
    for other_model in [false, true] {
        let (_dir, s, _, _, _) = fixture();
        if other_model {
            s.stats()
                .usage_add(&steve_store::NewUsage {
                    workspace_id: WS.into(),
                    ts: AT,
                    day: DAY.into(),
                    source: "gateway".into(),
                    purpose: "missing".into(),
                    harness: "test".into(),
                    turn_id: None,
                    session_id: None,
                    item_id: None,
                    bot_id: None,
                    project_id: None,
                    usage: steve_store::ModelUsage {
                        model: "B".into(),
                        tokens: steve_store::Tokens {
                            input: Some(10),
                            output: Some(5),
                            cache_read: Some(0),
                            cache_write: Some(0),
                            reasoning: None,
                        },
                        cost_reported: None,
                        detail: serde_json::json!({"scope":"all","call_id":"group"}),
                    },
                    duration_ms: None,
                    ok: true,
                    attempts: 1,
                })
                .unwrap();
        }
        s.stats()
            .usage_add(&steve_store::NewUsage {
                workspace_id: WS.into(),
                ts: AT,
                day: DAY.into(),
                source: "gateway".into(),
                purpose: "missing".into(),
                harness: "test".into(),
                turn_id: None,
                session_id: None,
                item_id: None,
                bot_id: None,
                project_id: None,
                usage: steve_store::ModelUsage {
                    model: "A".into(),
                    tokens: Default::default(),
                    cost_reported: Some(0.1),
                    detail: serde_json::json!({"scope":"main","call_id":"group"}),
                },
                duration_ms: None,
                ok: true,
                attempts: 1,
            })
            .unwrap();
        s.reader::<_, steve_store::StoreError>(|c| {
            let q = metrics::Query {
                workspace_id: WS.into(),
                from: DAY.into(),
                to: DAY.into(),
                source: Some("gateway".into()),
                ..Default::default()
            };
            let cov = metrics::summary(c, &q)?.unwrap().usage.coverage;
            assert_eq!(cov.unavailable, 1);
            assert_eq!(
                cov.state,
                if other_model {
                    CoverageState::Partial
                } else {
                    CoverageState::Unavailable
                }
            );
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn old_meter_heads_are_rebuilt_from_durable_reports_without_losing_identity() {
    let (_dir, s, item, sessions, turns) = fixture();
    let first = report("first", Measurement::Delta, Some(10));
    s.stats().usage_report(&turns[0], &first, "m", DAY).unwrap();
    // The unreleased pre-shared-primitive payload was a UsageReport, without field state.
    s.with_conn(|c| {
        c.execute(
            "UPDATE usage_meters SET payload=? WHERE turn_id=?",
            rusqlite::params![serde_json::to_string(&first).unwrap(), turns[0]],
        )?;
        Ok(())
    })
    .unwrap();
    let mut context = report("context", Measurement::Final, None);
    context.context_used = Some(100);
    s.stats()
        .usage_report(&turns[0], &context, "m", DAY)
        .unwrap();
    s.stats()
        .usage_report(
            &turns[0],
            &report("next", Measurement::Delta, Some(5)),
            "m",
            DAY,
        )
        .unwrap();
    assert!(!s.stats().usage_report(&turns[0], &first, "m", DAY).unwrap());
    totals(&s, &item, &sessions, 15);
}

#[test]
fn review_cross_model_main_all_preserves_both_models() {
    let (_dir, s, item, sessions, turns) = fixture();
    let mut a = report("main-A", Measurement::Final, Some(10));
    a.model = Some("A".into());
    a.tokens.output = Some(5);
    let mut b = report("all-B", Measurement::Final, Some(20));
    b.model = Some("B".into());
    b.scope = UsageScope::All;
    b.tokens.output = Some(7);
    s.stats().usage_report(&turns[0], &a, "m", DAY).unwrap();
    s.stats().usage_report(&turns[0], &b, "m", DAY).unwrap();
    totals(&s, &item, &sessions, 42);
}
#[test]
fn review_partial_all_preserves_known_main_input() {
    let (_dir, s, item, sessions, turns) = fixture();
    let a = report("main", Measurement::Final, Some(10));
    let mut b = report("all", Measurement::Final, None);
    b.scope = UsageScope::All;
    b.tokens.output = Some(7);
    s.stats().usage_report(&turns[0], &a, "m", DAY).unwrap();
    s.stats().usage_report(&turns[0], &b, "m", DAY).unwrap();
    totals(&s, &item, &sessions, 17);
}
#[test]
fn review_context_only_final_does_not_seal_tokens() {
    let (_dir, s, item, sessions, turns) = fixture();
    let a = report("delta1", Measurement::Delta, Some(10));
    let mut b = report("context", Measurement::Final, None);
    b.context_used = Some(100);
    let c = report("delta2", Measurement::Delta, Some(5));
    for r in [a, b, c] {
        s.stats().usage_report(&turns[0], &r, "m", DAY).unwrap();
    }
    totals(&s, &item, &sessions, 15);
}

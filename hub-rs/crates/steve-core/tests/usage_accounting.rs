//! Identical inputs through the actual persistent ledger, Tried::of and recording gateway.
//! Contract fixtures, never evidence of a paid provider call.
use serde::Deserialize;
use serde_json::json;
use steve_core::gateway::{Attempt, Printed, Tried, merged_usage};
use steve_core::stats::{CallTag, record};
use steve_node_api::{Tokens, UsageReport};
use steve_stats::{coverage::Coverage, metrics, views};
use steve_store::{NewBot, NewItem, NewSession, NoPublisher, Store, TurnBegin, TurnFinish, WS};

const AT: i64 = 1_793_491_200_000;
const DAY: &str = "2026-11-01";
#[derive(Deserialize)]
struct Case {
    name: String,
    fallback_model: String,
    reports: Vec<UsageReport>,
    tokens: Tokens,
    cost_usd: Option<f64>,
}

fn fixture() -> (tempfile::TempDir, Store, String, String, String) {
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
            &serde_json::Map::from_iter([("stats.tz".into(), json!("UTC"))]),
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
                title: "accounting probe".into(),
                goal: "g".into(),
                bot_id: "dev".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id;
    let session = s
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: Some(item.clone()),
            bot_id: Some("dev".into()),
            harness: "test".into(),
            model: None,
            reasoning_effort: None,
            cwd: ".".into(),
            title: "accounting".into(),
            token: "test".into(),
        })
        .unwrap()
        .id;
    let turn = s
        .stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: session.clone(),
            item_id: Some(item.clone()),
            bot_id: Some("dev".into()),
            harness: "test".into(),
            started: AT,
            ..Default::default()
        })
        .unwrap();
    (dir, s, item, session, turn)
}
// Fixture statistics use UTC. Query that workspace day, including when the machine has crossed
// local midnight while UTC has not; amounts/coverage must not depend on the test host's timezone.
fn gateway_day(s: &Store) -> String {
    let zone: Option<String> = s.settings().get(WS, "stats.tz", None).unwrap();
    steve_stats::day(steve_store::now(), steve_stats::tz(zone.as_deref()))
}
fn coverage(s: &Store, item: &str, session: &str) -> Coverage {
    s.read::<_, steve_store::StoreError>(|c| {
        let a = views::session(c, session)?.unwrap().1.usage;
        let b = views::item_detail(c, item, AT + 1000, false)?
            .unwrap()
            .1
            .usage;
        let q = metrics::Query {
            workspace_id: WS.into(),
            from: DAY.into(),
            to: DAY.into(),
            source: Some("agent".into()),
            ..Default::default()
        };
        let d = metrics::summary(c, &q)?.unwrap().usage;
        assert_eq!((a.total, a.total), (b.total, d.total));
        assert_eq!(a.coverage, b.coverage);
        assert_eq!(a.coverage, d.coverage);
        Ok(a.coverage)
    })
    .unwrap()
}
fn known_tokens(c: &Coverage) -> Tokens {
    let n = |v: Option<i64>| v.map(|n| u64::try_from(n).unwrap());
    Tokens {
        input: n(c.input),
        output: n(c.output),
        cache_read: n(c.cache_read),
        cache_write: n(c.cache_write),
        reasoning: None,
    }
}
#[test]
fn same_inputs_match_after_every_durable_restart_and_one_shot_projection() {
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../steve-node-api/tests/fixtures/accounting.json"
    ))
    .unwrap();
    for case in cases {
        let (dir, mut s, item, session, turn) = fixture();
        for end in 1..=case.reports.len() {
            let r = &case.reports[end - 1];
            s.stats()
                .usage_report(&turn, r, &case.fallback_model, DAY)
                .unwrap();
            assert!(
                !s.stats()
                    .usage_report(&turn, r, &case.fallback_model, DAY)
                    .unwrap()
            );
            drop(s);
            s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
            let reports = merged_usage(case.reports[..end].iter().cloned(), &case.fallback_model);
            let raw = Tried::of(Ok(("ok".into(), None)), reports.clone());
            let twice = Tried::of(
                Ok(("ok".into(), None)),
                merged_usage(reports, &case.fallback_model),
            );
            assert_eq!(raw.usage, twice.usage, "{} prefix {end}", case.name);
            let sum = |get: fn(steve_store::Tokens) -> Option<i64>| {
                raw.usage
                    .iter()
                    .filter_map(|u| get(u.tokens))
                    .reduce(i64::saturating_add)
                    .map(|n| n as u64)
            };
            let tokens = Tokens {
                input: sum(|t| t.input),
                output: sum(|t| t.output),
                cache_read: sum(|t| t.cache_read),
                cache_write: sum(|t| t.cache_write),
                reasoning: None,
            };
            let c = coverage(&s, &item, &session);
            assert_eq!(known_tokens(&c), tokens, "{} prefix {end}", case.name);
            let costs = raw
                .usage
                .iter()
                .filter_map(|u| u.cost_reported)
                .reduce(|a, b| a + b);
            assert_eq!(c.cost_reported, costs, "{} cost prefix {end}", case.name);
        }
        let c = coverage(&s, &item, &session);
        assert_eq!(known_tokens(&c), case.tokens, "{} final tokens", case.name);
        assert_eq!(c.cost_reported, case.cost_usd, "{} final cost", case.name);
        let finish = TurnFinish {
            turn_id: turn,
            ended: AT + 1000,
            day: DAY.into(),
            stop_reason: Some("cancelled".into()),
            ..Default::default()
        };
        assert!(s.stats().turn_finish(&finish).unwrap());
        assert!(!s.stats().turn_finish(&finish).unwrap());
        let c = coverage(&s, &item, &session);
        steve_stats::rebuild(&s, WS).unwrap();
        assert_eq!(coverage(&s, &item, &session), c, "{} rebuild", case.name);
        let tried = Tried::of(
            Ok(("ok".into(), None)),
            merged_usage(case.reports, &case.fallback_model),
        );
        let model = tried
            .usage
            .first()
            .map(|u| u.model.clone())
            .unwrap_or_default();
        record(
            &s,
            &Attempt {
                result: Ok(Printed::default()),
                attempts: 1,
                duration_ms: 123,
                usage: tried.usage,
                model,
                failures: vec![],
                harness: "test".into(),
            },
            &CallTag {
                workspace_id: WS.into(),
                ..Default::default()
            },
        )
        .unwrap();
        if case.name == "independent_zero_and_cost_only_model" {
            s.read::<_, steve_store::StoreError>(|c| {
                let calls = steve_store::core_fixture::usage::usage(&s, Some("gateway")).len();
                assert_eq!(
                    calls, 1,
                    "zero evidence must not become another request row"
                );
                let d = gateway_day(&s);
                let cov = metrics::summary(
                    c,
                    &metrics::Query {
                        workspace_id: WS.into(),
                        from: d.clone(),
                        to: d,
                        source: Some("gateway".into()),
                        model: Some("A".into()),
                        ..Default::default()
                    },
                )?
                .unwrap()
                .usage
                .coverage;
                assert_eq!((cov.total, cov.complete, cov.unavailable), (Some(0), 1, 0));
                Ok(())
            })
            .unwrap();
        }
        let gw = s
            .read::<_, steve_store::StoreError>(|c| {
                let d = gateway_day(&s);
                Ok(metrics::summary(
                    c,
                    &metrics::Query {
                        workspace_id: WS.into(),
                        from: d.clone(),
                        to: d,
                        source: Some("gateway".into()),
                        ..Default::default()
                    },
                )?
                .unwrap()
                .usage
                .coverage)
            })
            .unwrap();
        assert_eq!(
            known_tokens(&gw),
            case.tokens,
            "{} recorded gateway",
            case.name
        );
        assert_eq!(
            gw.cost_reported, case.cost_usd,
            "{} recorded cost",
            case.name
        );
        println!("{}", json!({"case":case.name,"ledger":c,"gateway":gw}));
    }
}
#[test]
fn real_normalizer_response_shape_keeps_default_and_alias_canonical_coverage() {
    use steve_acp::normalize::{Fact, Normalizer};
    use steve_node_api::{Epoch, EventBody};
    // Adapter session/prompt shape: usage has no model; quota carries the canonical name.
    let response = json!({"usage":{"inputTokens":10,"outputTokens":5,"cachedReadTokens":0,"cachedWriteTokens":0},"_meta":{"quota":{"model_usage":[{"model":"canonical","token_count":{"inputTokens":10,"outputTokens":5,"cachedReadTokens":0,"cachedWriteTokens":0}}]}}});
    let mut n = Normalizer::new(".", "adapter-response", Epoch::new("meter").unwrap());
    let reports: Vec<_> = n
        .prompt_response(&response)
        .into_iter()
        .filter_map(|f| match f {
            Fact::Event(EventBody::Usage { report }) => Some(report),
            _ => None,
        })
        .collect();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].model, None);
    for selected in ["", "requested-alias"] {
        let folded = merged_usage(reports.clone(), selected);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].model.as_deref(), Some("canonical"));
        let tried = Tried::of(Ok(("ok".into(), None)), folded);
        assert_eq!(
            (tried.usage[0].tokens.input, tried.usage[0].tokens.output),
            (Some(10), Some(5))
        );
    }
}

#[test]
fn concurrent_calls_have_stable_separate_ownership_without_extra_request_rows() {
    use steve_store::{ModelUsage, Tokens};
    let (_dir, s, _, _, _) = fixture();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let s = s.clone();
            scope.spawn(move || {
                record(
                    &s,
                    &Attempt {
                        result: Ok(Printed::default()),
                        attempts: 2,
                        duration_ms: 123,
                        model: "A".into(),
                        harness: "test".into(),
                        failures: vec!["failed retry".into()],
                        usage: vec![
                            ModelUsage {
                                model: "A".into(),
                                tokens: Tokens {
                                    input: Some(10),
                                    output: Some(5),
                                    cache_read: Some(0),
                                    cache_write: Some(0),
                                    reasoning: None,
                                },
                                cost_reported: None,
                                detail: json!({"scope":"all"}),
                            },
                            ModelUsage {
                                model: "B".into(),
                                tokens: Default::default(),
                                cost_reported: Some(0.1),
                                detail: json!({"scope":"main"}),
                            },
                        ],
                    },
                    &CallTag {
                        workspace_id: WS.into(),
                        ..Default::default()
                    },
                )
                .unwrap();
            });
        }
    });
    s.read::<_, steve_store::StoreError>(|c| {
        let groups = steve_store::core_fixture::usage::gateway_groups(&s);
        assert_eq!(groups.len(), 4);
        for (id, rows, attempts, duration, failures) in groups {
            assert!(uuid::Uuid::parse_str(&id).is_ok());
            assert_eq!((rows, attempts, duration, failures), (2, 3, 123, 1));
        }
        let d = gateway_day(&s);
        let cov = metrics::summary(
            c,
            &metrics::Query {
                workspace_id: WS.into(),
                from: d.clone(),
                to: d,
                source: Some("gateway".into()),
                ..Default::default()
            },
        )?
        .unwrap()
        .usage
        .coverage;
        assert_eq!((cov.expected, cov.unavailable), (8, 4));
        Ok(())
    })
    .unwrap();
}

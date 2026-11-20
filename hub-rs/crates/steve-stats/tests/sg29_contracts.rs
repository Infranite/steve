//! SG29 contracts: explicit coverage, chart branches, rebuild boundaries and persisted realtime.
#[path = "sg29/mod.rs"]
mod f;
use f::{AT, DAY, DAY_MS, DAY2, F, U, close};
use rusqlite::params;
use serde_json::json;
use std::collections::HashMap;
use steve_stats::{
    coverage::{self, CoverageState},
    metrics, refresh,
    views::{self, Bucket, Metric, Series},
};
use steve_store::WS;

fn chart(f: &F, m: Metric, group: &str) -> Series {
    f.store
        .reader(|c| views::series(c, &f.q(), m, group, Bucket::Day))
        .unwrap()
}
fn line<'a>(s: &'a Series, key: &str) -> &'a views::Line {
    s.series
        .iter()
        .find(|l| l.key == key)
        .unwrap_or_else(|| panic!("missing {key}: {s:?}"))
}

#[test]
fn sg29_entity_coverage_keeps_known_zero_unknown_attempts_and_owned_cost_peers_distinct() {
    let f = F::new();
    f.store
        .reader(|c| {
            let x = coverage::entity(c, "item", "ST-1")?;
            assert_eq!(
                (x.state, x.expected, x.reported, x.complete, x.uncertain),
                (CoverageState::Complete, 2, 2, 2, 0)
            );
            assert_eq!(
                (x.total, x.input, x.output, x.cache_read, x.cache_write),
                (Some(135), Some(100), Some(20), Some(10), Some(5))
            );
            close(x.cost_reported.unwrap(), 0.9);
            let zero = views::series(
                c,
                &metrics::Query {
                    source: Some("gateway".into()),
                    ..f.q()
                },
                Metric::Tokens,
                "none",
                Bucket::Day,
            )?;
            assert_eq!(line(&zero, "total").values, [Some(0.0), None]);
            let missing = coverage::entity(c, "item", "ST-2")?;
            assert_eq!(missing.state, CoverageState::Unavailable);
            assert_eq!(
                (missing.expected, missing.unavailable, missing.total),
                (1, 1, None)
            );
            for (kind, id) in [("session", "s1"), ("turn", "t1")] {
                let x = coverage::entity(c, kind, id)?;
                assert_eq!((x.expected, x.complete, x.total), (1, 1, Some(135)));
            }
            assert_eq!(coverage::entity(c, "unknown", "ST-1")?, Default::default());
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}

#[test]
fn sg29_legacy_uncertain_partial_and_unreported_open_attempts_preserve_field_knowledge() {
    let f = F::new();
    let mut part = U::new("m-a", [Some(5), None, None, None]);
    part.turn = None;
    part.session = None;
    part.ts = AT + 3_000;
    f.usage(part);
    let mut cost = U::new("m-a", [None; 4]);
    cost.turn = None;
    cost.session = None;
    cost.ts = AT + 3_000;
    cost.cost = Some(0.1);
    cost.detail = json!({"scope":"main"});
    f.usage(cost);
    f.store.with_conn(|c|{c.execute("INSERT INTO agent_turns(id,workspace_id,session_id,item_id,bot_id,harness,attempt,started) VALUES('open','default','s1','ST-1','dev','custom',0,?)",[AT+9_000])?;Ok(())}).unwrap();
    let x = f
        .store
        .reader(|c| coverage::entity(c, "item", "ST-1"))
        .unwrap();
    assert_eq!(x.state, CoverageState::Partial);
    assert_eq!(
        (x.expected, x.reported, x.complete, x.uncertain, x.pending),
        (5, 3, 2, 1, 1)
    );
    assert_eq!(x.total, Some(140));
    assert_eq!(x.output, Some(20));
    close(x.cost_reported.unwrap(), 1.0);
}

#[test]
fn sg29_unspent_zero_models_are_explicit_evidence_and_nonzero_payloads_are_ignored() {
    let f = F::new();
    let mut u = U::new("m-a", [Some(0); 4]);
    u.detail = json!({"accounting_tokens":1,"unspent_accounting":1,"unspent_usage":[
 {"model":"unused-zero","tokens":{"input":0,"output":0,"cache_read":0,"cache_write":0,"reasoning":0}},
 {"model":"not-zero","tokens":{"input":0,"output":0,"cache_read":0,"cache_write":0,"reasoning":1}}]});
    f.usage(u);
    f.rebuild();
    let q = metrics::Query {
        model: Some("unused-zero".into()),
        ..f.q()
    };
    let s = f
        .store
        .reader(|c| views::series(c, &q, Metric::Tokens, "none", Bucket::Day))
        .unwrap();
    let x = &s.coverage.as_ref().unwrap()[0];
    assert_eq!((x.expected, x.complete, x.total), (1, 1, Some(0)));
    assert_eq!(line(&s, "total").values[0], Some(0.0));
    let q = metrics::Query {
        model: Some("not-zero".into()),
        ..f.q()
    };
    let s = f
        .store
        .reader(|c| views::series(c, &q, Metric::Tokens, "none", Bucket::Day))
        .unwrap();
    assert_eq!(s.coverage.unwrap()[0].expected, 0);
}

#[test]
fn sg29_all_series_metric_families_keep_hand_calculated_daily_values_and_percentiles() {
    let f = F::new();
    for (m, expected) in [
        (Metric::Tokens, [Some(135.0), None]),
        (Metric::Cost, [Some(0.00019), None]),
        (Metric::Done, [Some(1.0), Some(0.0)]),
        (Metric::Created, [Some(1.0), Some(1.0)]),
        (Metric::Wip, [Some(0.0), Some(1.0)]),
        (Metric::HitRate, [Some(10.0 / 115.0), None]),
        (Metric::Turns, [Some(1.0), Some(1.0)]),
        (Metric::TurnMs, [Some(4_000.0), Some(1_000.0)]),
        (Metric::WaitMs, [Some(7_000.0), Some(0.0)]),
        (Metric::Decisions, [Some(4.0), Some(0.0)]),
        (Metric::Active, [Some(1.0), Some(1.0)]),
    ] {
        let s = chart(&f, m, "none");
        assert_eq!(s.buckets, [DAY, DAY2]);
        assert_eq!(s.series.len(), 1, "{m:?}: {s:?}");
        for (a, b) in s.series[0].values.iter().zip(expected) {
            match (a, b) {
                (Some(a), Some(b)) => assert!((*a - b).abs() < 1e-9, "{m:?}: {a} != {b}"),
                _ => assert_eq!(*a, b, "{m:?}"),
            }
        }
    }
    let s = chart(&f, Metric::Lead, "none");
    assert_eq!(line(&s, "p50").values, [Some(12_000.0), None]);
    assert_eq!(line(&s, "p85").values, [Some(12_000.0), None]);
}

#[test]
fn sg29_token_stacks_model_gaps_harness_labels_and_top_six_folding_keep_their_identity() {
    let f = F::new();
    let stack = chart(&f, Metric::Tokens, "token_type");
    assert_eq!(
        stack
            .series
            .iter()
            .map(|x| x.key.as_str())
            .collect::<Vec<_>>(),
        ["input", "output", "cache_read", "cache_write"]
    );
    for (key, value) in [
        ("input", 100.0),
        ("output", 20.0),
        ("cache_read", 10.0),
        ("cache_write", 5.0),
    ] {
        assert_eq!(line(&stack, key).values, [Some(value), None]);
    }
    let models = chart(&f, Metric::Tokens, "model");
    assert_eq!(line(&models, "m-a").values, [Some(135.0), None]);
    assert_eq!(line(&models, "unpriced-sg29").values, [Some(0.0), None]);
    assert_eq!(line(&models, "session-fallback").values, [None, None]);
    let mapped = f
        .store
        .reader(|c| {
            views::series_with_harnesses(
                c,
                &f.q(),
                Metric::Tokens,
                "harness",
                Bucket::Day,
                &HashMap::from([("custom".into(), "Friendly".into())]),
            )
        })
        .unwrap();
    assert_eq!(line(&mapped, "custom").label, "Friendly");
    for i in 0..7 {
        let model = format!("extra-{i}");
        let mut u = U::new(&model, [Some(i + 1), Some(0), Some(0), Some(0)]);
        u.source = "gateway";
        u.purpose = "route";
        u.turn = None;
        u.session = None;
        f.usage(u);
    }
    f.rebuild();
    let s = chart(&f, Metric::Tokens, "model");
    assert_eq!(s.series.len(), 6);
    assert_eq!(s.series[0].key, "m-a");
    assert_eq!(s.series.last().unwrap().key, "other");
    assert_eq!(line(&s, "other").values, [Some(6.0), None]);
    close(s.series.iter().filter_map(|l| l.values[0]).sum(), 163.0);
}

#[test]
fn sg29_hourly_future_masking_filtered_cards_and_invalid_bounds_preserve_early_returns() {
    let f = F::new();
    let q = metrics::Query {
        to: DAY.into(),
        ..f.q()
    };
    let s = f
        .store
        .reader(|c| {
            views::series(
                c,
                &q,
                Metric::Tokens,
                "none",
                Bucket::Hour {
                    now: AT + 2 * 3_600_000 + 5,
                },
            )
        })
        .unwrap();
    assert_eq!(s.buckets.len(), 24);
    assert_eq!(s.current, Some(2));
    assert_eq!(s.series[0].values[0], Some(135.0));
    assert!(s.series[0].values[3..].iter().all(Option::is_none));
    let filtered = metrics::Query {
        bot: Some("dev".into()),
        ..f.q()
    };
    let wait = f
        .store
        .reader(|c| views::series(c, &filtered, Metric::WaitMs, "none", Bucket::Day))
        .unwrap();
    assert_eq!(line(&wait, "total").values[0], Some(6_000.0));
    let count = f
        .store
        .reader(|c| views::series(c, &filtered, Metric::Decisions, "none", Bucket::Day))
        .unwrap();
    assert_eq!(line(&count, "total").values[0], Some(3.0));
    let bad = metrics::Query {
        from: "bad".into(),
        to: "bad".into(),
        ..f.q()
    };
    for m in [
        Metric::Done,
        Metric::Created,
        Metric::Active,
        Metric::WaitMs,
        Metric::Decisions,
        Metric::Lead,
    ] {
        let s = f
            .store
            .reader(|c| views::series(c, &bad, m, "none", Bucket::Day))
            .unwrap();
        assert!(s.series.is_empty(), "{m:?}: {s:?}");
    }
}

#[test]
fn sg29_fixed_label_families_builtin_precedence_and_unknown_values_are_preserved() {
    for key in [
        "agent",
        "gateway",
        "turn",
        "autonomous",
        "route",
        "chat",
        "research",
        "title",
        "draft",
        "extract",
        "compose",
        "judge",
        "mail",
        "disclosure",
        "intent",
        "flow",
        "yolo",
        "digest",
        "other",
        "stall",
        "error",
        "restart",
        "release",
        "accept",
        "ask",
        "approve",
        "fail",
        "propose",
        "confirm",
    ] {
        assert!(!views::fixed_label(key).unwrap().is_empty(), "{key}");
    }
    assert_eq!(views::fixed_label("sg29-no-label"), None);
    assert_eq!(
        views::fixed_label("claude-code").as_deref(),
        Some("Claude Code")
    );
}

#[test]
fn sg29_item_brief_full_usage_turns_marks_and_decision_audiences_agree() {
    let f = F::new();
    let (_, brief) = f
        .store
        .reader(|c| views::item_detail(c, "ST-1", AT + 20_000, true))
        .unwrap()
        .unwrap();
    let (ws, full) = f
        .store
        .reader(|c| views::item_detail(c, "ST-1", AT + 20_000, false))
        .unwrap()
        .unwrap();
    assert_eq!(ws, WS);
    assert_eq!(brief.usage, full.usage);
    assert_eq!(brief.age_ms, 12_000);
    assert_eq!(full.usage.total, 135);
    close(full.usage.reported, 0.9);
    close(full.usage.est, 0.00019);
    close(full.usage.hit_rate.unwrap(), 10.0 / 115.0);
    assert!(
        brief.turns.is_none()
            && brief.marks.is_none()
            && brief.decisions.is_none()
            && brief.by_model.is_none()
    );
    let turns = full.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, "t1");
    assert_eq!(turns[0].usage.total, 135);
    assert_eq!(turns[0].usage.coverage.expected, 1);
    let marks = full.marks.unwrap();
    assert_eq!(
        marks.iter().map(|m| m.ts - AT).collect::<Vec<_>>(),
        [0, 10_000, 11_000, 12_000]
    );
    let ds = full.decisions.unwrap();
    assert_eq!(ds.len(), 3);
    assert_eq!(
        ds.iter().find(|d| d.id == "auto").unwrap().by.as_deref(),
        Some("yolo")
    );
    assert_eq!(
        ds.iter().find(|d| d.id == "q1").unwrap().by.as_deref(),
        Some("me")
    );
    let models = full.by_model.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model, "m-a");
    assert_eq!(models[0].usage.coverage.total, Some(135));
    assert_eq!(models[1].usage.coverage.total, Some(0));
    assert!(
        f.store
            .reader(|c| views::item_detail(c, "missing", AT, false))
            .unwrap()
            .is_none()
    );
}

#[test]
fn sg29_rebuild_recuts_usage_and_pending_calls_flags_only_its_workspace_and_is_idempotent() {
    let f = F::new();
    let mut u = U::new("m-a", [Some(2), Some(0), Some(0), Some(0)]);
    u.ts = AT + DAY_MS - 1_000;
    u.turn = None;
    u.session = None;
    u.source = "gateway";
    u.purpose = "late";
    let row = f.usage(u);
    f.store
        .stats()
        .gateway_begin(&steve_store::gateway_ledger::CallBegin {
            id: "late-call".into(),
            workspace: WS.into(),
            started: AT + DAY_MS - 1_000,
            day: DAY.into(),
            ..Default::default()
        })
        .unwrap();
    f.store.with_conn(|c|{c.execute("UPDATE settings SET value='\"Asia/Tokyo\"' WHERE workspace_id='default' AND key='stats.tz'",[])?;c.execute("INSERT INTO stat_daily(workspace_id,day,source,purpose,harness,model,bot_id,project_id) VALUES('other','sentinel','x','x','x','x','','')",[])?;Ok(())}).unwrap();
    let other = f.cube("other");
    let rows = f.store.transaction(|c| refresh::rebuild_in(c, WS)).unwrap();
    assert_eq!(rows, 4);
    f.store
        .reader(|c| {
            let day: String =
                c.query_row("SELECT day FROM usage_records WHERE id=?", [row], |r| {
                    r.get(0)
                })?;
            assert_eq!(day, DAY2);
            let day: String = c.query_row(
                "SELECT day FROM gateway_calls WHERE call_id='late-call'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(day, DAY2);
            let dirty: i64 = c.query_row(
                "SELECT count(*) FROM item_stats WHERE workspace_id='default' AND dirty=1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(dirty, 2);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    assert_eq!(f.cube("other"), other);
    let before = f.cube(WS);
    assert_eq!(
        f.store.transaction(|c| refresh::rebuild_in(c, WS)).unwrap(),
        rows
    );
    assert_eq!(f.cube(WS), before);
}

#[test]
fn sg29_rebuild_model_precedence_preserves_live_empty_and_first_usage_fallback() {
    for (model, want) in [
        (None, "session-fallback"),
        (Some(""), ""),
        (Some("live"), "live"),
    ] {
        let f = F::new();
        f.store
            .with_conn(|c| {
                c.execute("UPDATE agent_turns SET model=NULL WHERE id='t1'", [])?;
                c.execute("UPDATE agent_turns SET model=? WHERE id='t2'", [model])?;
                Ok(())
            })
            .unwrap();
        f.store.transaction(|c| refresh::rebuild_in(c, WS)).unwrap();
        f.store
            .reader(|c| {
                let model: String = c.query_row(
                    "SELECT model FROM stat_daily WHERE source='agent' AND day=?",
                    [DAY],
                    |r| r.get(0),
                )?;
                assert_eq!(model, "m-a");
                let model: String = c.query_row(
                    "SELECT model FROM stat_daily WHERE source='agent' AND day=?",
                    [DAY2],
                    |r| r.get(0),
                )?;
                assert_eq!(model, want);
                Ok::<_, steve_store::StoreError>(())
            })
            .unwrap();
    }
}

#[test]
fn sg29_rebuild_write_failure_rolls_back_cube_and_day_corrections() {
    let f = F::new();
    let before = f.cube(WS);
    f.store.with_conn(|c|{c.execute("UPDATE usage_records SET day='wrong'",[])?;c.execute_batch("CREATE TEMP TRIGGER sg29_fail BEFORE INSERT ON stat_daily BEGIN SELECT RAISE(ABORT,'sg29 rebuild rollback'); END;")?;Ok(())}).unwrap();
    let error = f
        .store
        .transaction(|c| refresh::rebuild_in(c, WS))
        .unwrap_err();
    assert!(error.to_string().contains("sg29 rebuild rollback"));
    assert_eq!(f.cube(WS), before);
    f.store
        .reader(|c| {
            let n: i64 = c.query_row(
                "SELECT count(*) FROM usage_records WHERE day='wrong'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 3);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}

const RT: i64 = 1_800_000_000_000;
fn node_attempt(f: &F, id: &str, ws: &str, project: &str, tracked: bool) {
    f.store.with_conn(|c|{
  c.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES(?,?,'custom','.','running',?,?)",params![id,ws,RT-100_000,RT])?;
  c.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES(?,'n','{}',?,'r','busy',?)",params![id,id,RT])?;
  c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES(?,?,'finished',?,?)",params![id,id,RT-100_000,RT])?;
  c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES(?,?,0,'n',?,'{}','{}','d','running',?,?)",params![id,id,id,RT-100_000,RT])?;
  if tracked {c.execute("INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,project_id,bot_id,node_id,started,baseline) VALUES(?,?,?,?,'dev','n',?,1)",params![id,id,ws,project,RT-100_000])?;}
  Ok(())
 }).unwrap();
}
fn sample(f: &F, id: &str, seq: i64, at: i64, units: i64, kind: &str) {
    f.store.with_conn(|c|{c.execute("INSERT INTO realtime_output_samples(node_id,stream,epoch,seq,event_id,attempt_id,at,source_at,received_at,kind,units) VALUES('n',?,'e',?,?,?,?,?,?,?,?)",params![id,seq,format!("{id}-{seq}"),id,at,at,RT,kind,units])?;Ok(())}).unwrap();
}
fn realtime(f: &F, ws: &str, project: Option<&str>, at: i64) -> steve_stats::realtime::Realtime {
    f.store
        .reader(|c| {
            steve_stats::realtime::read(
                c,
                steve_stats::realtime::Scope {
                    workspace: ws,
                    project,
                    bot: None,
                },
                at,
            )
        })
        .unwrap()
}

#[test]
fn sg29_realtime_snapshot_rounds_clock_loads_bounded_samples_and_preserves_confirmed_end_history() {
    use steve_stats::realtime::Quality;
    let f = F::new();
    node_attempt(&f, "rt-a", WS, "p", true);
    sample(&f, "rt-a", 1, RT - 80_000, 1_000_000, "text");
    sample(&f, "rt-a", 2, RT - 10_000, 400, "text");
    sample(&f, "rt-a", 3, RT - 1_000, 80, "text");
    sample(&f, "rt-a", 4, RT, 120, "text");
    sample(&f, "rt-a", 5, RT + 1_000, 400, "text");
    f.store
        .with_conn(|c| {
            c.execute(
                "UPDATE realtime_output_attempts SET ended=? WHERE attempt_id='rt-a'",
                [RT + 1_000],
            )?;
            Ok(())
        })
        .unwrap();
    let r = realtime(&f, WS, None, RT + 999);
    assert_eq!(r.at, RT);
    assert_eq!(r.tps, Some(5.0));
    assert_eq!(r.state, Quality::Estimated);
    assert_eq!(r.output_sessions, Some(1));
    assert_eq!(
        (
            r.coverage.measured,
            r.coverage.estimated,
            r.coverage.unavailable
        ),
        (0, 1, 0)
    );
    assert_eq!(r.series.len(), 60);
    assert_eq!(r.series[0].at, RT - 59_000);
    assert_eq!(r.series.last().unwrap().tps, r.tps);
    assert!(r.series.windows(2).all(|p| p[1].at - p[0].at == 1_000));
    let ended = realtime(&f, WS, None, RT + 11_000);
    assert_eq!(
        (ended.state, ended.tps, ended.output_sessions),
        (Quality::Idle, Some(0.0), Some(0))
    );
    assert!(ended.series.iter().any(|p| p.state == Quality::Estimated));
}

#[test]
fn sg29_realtime_pending_gap_untracked_and_scope_filters_do_not_turn_missing_into_zero() {
    use steve_stats::realtime::Quality;
    let f = F::new();
    for (id, ws, p, tracked, units) in [
        ("rt-a", WS, "p1", true, 80),
        ("rt-b", WS, "p2", true, 160),
        ("rt-c", WS, "p1", false, 0),
        ("rt-other", "other", "p1", true, 400_000),
    ] {
        node_attempt(&f, id, ws, p, tracked);
        if tracked {
            sample(&f, id, 1, RT - 1_000, units, "text");
        }
    }
    f.store.with_conn(|c|{c.execute("INSERT INTO node_streams(node_id,stream,epoch,durable,applied) VALUES('n','pending','e',1,0)",[])?;c.execute("INSERT INTO node_event_inbox(node_id,stream,epoch,seq,event_id,payload,applied,received) VALUES('n','pending','e',1,'pending',?,0,?)",params![json!({"subject":{"attempt":"rt-a"}}).to_string(),RT])?;Ok(())}).unwrap();
    let all = realtime(&f, WS, None, RT);
    assert_eq!(
        (all.state, all.tps, all.output_sessions),
        (Quality::Partial, None, None)
    );
    assert_eq!(all.estimated_tps, Some(6.0));
    assert_eq!((all.coverage.estimated, all.coverage.unavailable), (1, 2));
    assert_eq!(realtime(&f, WS, Some("p2"), RT).tps, Some(4.0));
    assert_eq!(realtime(&f, "other", None, RT).tps, Some(10_000.0));
    sample(&f, "rt-b", 2, RT, 0, "gap");
    let gap = realtime(&f, WS, Some("p2"), RT);
    assert_eq!(
        (gap.state, gap.tps, gap.estimated_tps),
        (Quality::Partial, None, Some(4.0))
    );
}

#[test]
fn sg29_legacy_ended_sessions_bound_unknown_intervals_but_keep_their_historical_points() {
    use steve_stats::realtime::Quality;
    let f = F::new();
    f.store.with_conn(|c|{c.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES('legacy','default','custom','.','ended',?,?)",params![RT-20_000,RT-5_000])?;Ok(())}).unwrap();
    let unknown = realtime(&f, WS, None, RT);
    assert_eq!((unknown.state, unknown.tps), (Quality::Unavailable, None));
    let later = realtime(&f, WS, None, RT + 5_000);
    assert_eq!((later.state, later.tps), (Quality::Idle, Some(0.0)));
    assert!(later.series.iter().any(|p| p.state == Quality::Unavailable));
}

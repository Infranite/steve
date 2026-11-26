//! Conservation and attribution contracts for `node` and `session` dimensions across
//! `summary`, `breakdown`, `series`, `node_costs`, and `session_summaries` (including
//! multi-node sessions, gateway calls, research runs, and legacy unattributed records).

use serde_json::json;
use steve_stats::metrics::{self, By, Query};
use steve_stats::views::{self, Bucket, Metric};
use steve_store::gateway_ledger::{CallBegin, CallFinish};
use steve_store::testkit::stats::{bind_session_node, seed_active_node};
use steve_store::{
    ModelUsage, NewBot, NewSession, NewUsage, NoPublisher, Store, Tokens, TurnBegin, TurnFinish, WS,
};

const DAY1: &str = "2026-11-01";
const DAY2: &str = "2026-11-02";
const DAY1_TS: i64 = 1_793_520_000_000; // 2026-11-01T08:00:00Z
const DAY2_TS: i64 = 1_793_606_400_000; // 2026-11-02T08:00:00Z

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s.settings()
        .set(
            WS,
            &serde_json::Map::from_iter([("stats.tz".into(), json!("UTC"))]),
            "test",
        )
        .unwrap();
    s.bots()
        .seed(
            "dev",
            true,
            &NewBot {
                name: "Dev".into(),
                harness: "claude-code".into(),
                model: Some("claude-opus-5-5".into()),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    // Register a named remote Node so Names::label resolves it.
    seed_active_node(&s, "node_gpu", "owner", "GPU Box").unwrap();
    (dir, s)
}

fn make_session(s: &Store, title: &str, node_id: &str) -> String {
    let sid = s
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            model: Some("claude-opus-5-5".into()),
            cwd: ".".into(),
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    bind_session_node(s, &sid, node_id, "idle").unwrap();
    sid
}

fn tok(input: i64, output: i64, cache_read: i64) -> Tokens {
    Tokens {
        input: Some(input),
        output: Some(output),
        cache_read: Some(cache_read),
        cache_write: Some(0),
        reasoning: None,
    }
}

#[test]
fn node_and_session_breakdowns_and_series_conserve_exact_totals_across_sessions_gateway_and_research()
 {
    let (_dir, s) = setup();
    let s_local = make_session(&s, "Local Session", "local");
    let s_remote = make_session(&s, "Remote GPU Session", "node_gpu");

    // 1. Session turn on local Node (automatically resolved via runtime_sessions).
    let t_local = s
        .stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: s_local.clone(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            started: DAY1_TS,
            ..Default::default()
        })
        .unwrap();
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: t_local,
            ended: DAY1_TS + 12_000,
            day: DAY1.into(),
            model: "claude-opus-5-5".into(),
            stop_reason: Some("end_turn".into()),
            usage: vec![ModelUsage {
                model: "claude-opus-5-5".into(),
                tokens: tok(100_000, 20_000, 50_000),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();

    // 2. Session turn on remote Node (day 2).
    let t_remote = s
        .stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: s_remote.clone(),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            started: DAY2_TS,
            ..Default::default()
        })
        .unwrap();
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: t_remote,
            ended: DAY2_TS + 30_000,
            day: DAY2.into(),
            model: "claude-opus-5-5".into(),
            stop_reason: Some("end_turn".into()),
            usage: vec![ModelUsage {
                model: "claude-opus-5-5".into(),
                tokens: tok(200_000, 40_000, 10_000),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();

    // 3. Gateway call on local Node (routing).
    s.stats()
        .gateway_begin(&CallBegin {
            id: "gw_local".into(),
            workspace: WS.into(),
            purpose: "route".into(),
            node_id: Some("local".into()),
            started: DAY1_TS + 60_000,
            day: DAY1.into(),
            ..Default::default()
        })
        .unwrap();
    s.stats()
        .gateway_finish(
            "gw_local",
            &CallFinish {
                ok: true,
                model: "claude-haiku-5-5".into(),
                harness: "claude-code".into(),
                attempts: 1,
                duration_ms: 800,
                failures: vec![],
                usage: vec![ModelUsage {
                    model: "claude-haiku-5-5".into(),
                    tokens: tok(30_000, 5_000, 0),
                    ..Default::default()
                }],
            },
        )
        .unwrap();

    // 4. Research gateway call attributed to remote Node.
    s.stats()
        .gateway_begin(&CallBegin {
            id: "gw_research".into(),
            workspace: WS.into(),
            purpose: "research".into(),
            node_id: Some("node_gpu".into()),
            started: DAY2_TS + 60_000,
            day: DAY2.into(),
            ..Default::default()
        })
        .unwrap();
    s.stats()
        .gateway_finish(
            "gw_research",
            &CallFinish {
                ok: true,
                model: "claude-sonnet-4-5".into(),
                harness: "claude-code".into(),
                attempts: 1,
                duration_ms: 4_000,
                failures: vec![],
                usage: vec![ModelUsage {
                    model: "claude-sonnet-4-5".into(),
                    tokens: tok(80_000, 15_000, 20_000),
                    ..Default::default()
                }],
            },
        )
        .unwrap();

    // 5. Legacy unattributed gateway call (node_id = "").
    s.stats()
        .usage_add(&NewUsage {
            workspace_id: WS.into(),
            ts: DAY1_TS + 120_000,
            day: DAY1.into(),
            source: "gateway".into(),
            purpose: "chat".into(),
            harness: "claude-code".into(),
            node_id: None,
            usage: ModelUsage {
                model: "claude-haiku-5-5".into(),
                tokens: tok(10_000, 2_000, 0),
                ..Default::default()
            },
            ok: true,
            attempts: 1,
            ..Default::default()
        })
        .unwrap();

    let q = Query {
        workspace_id: WS.into(),
        from: DAY1.into(),
        to: DAY2.into(),
        ..Default::default()
    };

    s.read::<_, steve_store::StoreError>(|c| {
        let sum = metrics::summary(c, &q)?.expect("summary");
        let expected_tokens = 170_000 + 250_000 + 35_000 + 115_000 + 12_000;
        assert_eq!(sum.usage.total, expected_tokens);
        assert!(sum.spend.est > 0.0);

        let daily_rows = metrics::daily(c, &q)?;
        let session_rows = metrics::by_session(c, &q, false)?;
        let prices = metrics::book_of(c, WS, daily_rows.iter().map(|r| r.model.as_str()))?;
        let names = views::Names::load(c, WS)?;

        // Breakdown by Node: exact 0 error vs summary.
        let by_node = metrics::breakdown(&daily_rows, By::Node, &prices);
        assert_eq!(by_node.len(), 3, "local, node_gpu, and unattributed ''");
        let node_tok_sum: i64 = by_node.iter().map(|r| r.usage.total).sum();
        let node_est_sum: f64 = by_node.iter().map(|r| r.usage.est).sum();
        assert_eq!(node_tok_sum, sum.usage.total);
        assert!(
            (node_est_sum - sum.spend.est).abs() < 1e-12,
            "node cost conservation error: {node_est_sum} vs {}",
            sum.spend.est
        );
        assert_eq!(names.label("node", "node_gpu"), "GPU Box");
        assert_eq!(names.label("node", "local"), "本机 Node");
        assert_eq!(names.label("node", ""), "未归属 Node");

        // Breakdown by Session: exact 0 error vs summary (non-session gateway/research under "").
        let by_session = metrics::breakdown(&session_rows, By::Session, &prices);
        assert_eq!(by_session.len(), 3, "s_local, s_remote, and ''");
        let sess_tok_sum: i64 = by_session.iter().map(|r| r.usage.total).sum();
        let sess_est_sum: f64 = by_session.iter().map(|r| r.usage.est).sum();
        assert_eq!(sess_tok_sum, sum.usage.total);
        assert!(
            (sess_est_sum - sum.spend.est).abs() < 1e-12,
            "session cost conservation error: {sess_est_sum} vs {}",
            sum.spend.est
        );
        assert_eq!(names.label("session", &s_local), "Local Session");
        assert_eq!(names.label("session", &s_remote), "Remote GPU Session");
        assert_eq!(names.label("session", ""), "非会话调用（网关 / 研究）");

        // Series grouped by node and session: exact 0 error for both tokens and cost.
        for group in ["node", "session"] {
            let tok_series = views::series(c, &q, Metric::Tokens, group, Bucket::Day)?;
            let tok_total: f64 = tok_series
                .series
                .iter()
                .flat_map(|l| l.values.iter().flatten())
                .sum();
            assert_eq!(
                tok_total.round() as i64,
                sum.usage.total,
                "series tokens group={group}"
            );

            let cost_series = views::series(c, &q, Metric::Cost, group, Bucket::Day)?;
            let cost_total: f64 = cost_series
                .series
                .iter()
                .flat_map(|l| l.values.iter().flatten())
                .sum();
            assert!(
                (cost_total - sum.spend.est).abs() < 1e-12,
                "series cost group={group}: {cost_total} vs {}",
                sum.spend.est
            );
        }

        // Node cumulative & today costs for /api/vitals.
        let costs = metrics::node_costs(c, WS, DAY2)?;
        assert!(costs["local"].0 > 0.0);
        assert_eq!(costs["local"].1, 0.0, "local had usage only on DAY1");
        assert!(costs["node_gpu"].0 > 0.0);
        assert!(
            (costs["node_gpu"].0 - costs["node_gpu"].1).abs() < 1e-12,
            "node_gpu usage was all on DAY2"
        );

        // Batched session_summaries for SessionView enrichment.
        let summaries = views::session_summaries(c, WS, &[s_local.clone(), s_remote.clone()])?;
        assert_eq!(summaries[&s_local].0, 170_000);
        assert!(summaries[&s_local].1.unwrap() > 0.0);
        assert_eq!(summaries[&s_remote].0, 250_000);
        assert!(summaries[&s_remote].1.unwrap() > 0.0);

        Ok(())
    })
    .unwrap();

    // Also verify rebuild preserves node attribution identically.
    steve_stats::refresh::rebuild(&s, WS).unwrap();
    s.read::<_, steve_store::StoreError>(|c| {
        let sum = metrics::summary(c, &q)?.expect("summary after rebuild");
        let daily_rows = metrics::daily(c, &q)?;
        let prices = metrics::book_of(c, WS, daily_rows.iter().map(|r| r.model.as_str()))?;
        let by_node = metrics::breakdown(&daily_rows, By::Node, &prices);
        assert_eq!(by_node.len(), 3);
        assert_eq!(
            by_node.iter().map(|r| r.usage.total).sum::<i64>(),
            sum.usage.total
        );
        Ok(())
    })
    .unwrap();
}

//! The metrics on a small database shaped like production, with the expected figures worked out
//! by hand below.
//!
//! All times are minutes after 2026-11-01 00:00 UTC; the workspace's `stats.tz` is UTC.

use steve_stats::metrics::{By, Query, breakdown, daily, prices_of, summary, wip_by_day};
use steve_stats::{refresh, views};
use steve_store::testkit::stats::{
    dropped_status_marks, item_totals, resolve_question, seed_fixture, seed_tree, seed_tree_chain,
    snapshot,
};
use steve_store::{NewBot, NoPublisher, Store, WS};

const T0: i64 = 1_793_491_200_000;
const MIN: i64 = 60_000;

fn m(x: i64) -> i64 {
    T0 + x * MIN
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s.bots()
        .seed(
            "dev",
            false,
            &NewBot {
                name: "开发".into(),
                harness: "claude-code".into(),
                model: Some("claude-opus-5-5".into()),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    seed_fixture(&s, m).unwrap();
    refresh::rebuild(&s, WS).unwrap();
    (dir, s)
}

fn day_query() -> Query {
    Query {
        workspace_id: WS.into(),
        from: "2026-11-01".into(),
        to: "2026-11-01".into(),
        ..Query::default()
    }
}

#[test]
fn metrics_match_the_hand_count() {
    // turns: ST-1 one, ST-2 three (the retry splits one), ST-3 one, ST-5 one
    let (_d, s) = store();

    let sm = s.read(|c| summary(c, &day_query())).unwrap().unwrap();
    assert_eq!(sm.tz, "UTC");

    let th = &sm.throughput;
    assert_eq!(
        (th.created, th.done, th.dropped, th.wip, th.net),
        (5, 3, 1, 1, 1)
    );
    assert_eq!(th.drop_rate, Some(0.25));
    assert_eq!(
        (th.delivered, th.delivered_auto, th.delivered_manual),
        (4, 1, 3)
    );

    // D1: ST-1 20 min, ST-2 55 min, ST-3 5 min 1 s
    let lead = &sm.time.lead;
    assert_eq!(lead.n, 3);
    assert_eq!(lead.p50, Some(20 * MIN));
    assert_eq!(lead.p85, Some(55 * MIN));
    assert_eq!(lead.avg, Some((75 * MIN + 5 * MIN + 1_000) as f64 / 3.0));
    // D4: ST-1 = [4, 20] (ask 4–8, fail 6–12, accept 10–20 overlap); ST-2 = 5 + 10 + 2 + 5 + 1, the card Steve settled not counted
    assert_eq!(sm.time.wait.sum, (16 + 23) * MIN);
    // D5: ST-1 9 min, ST-2 9 + 2 + 8, ST-3 4
    assert_eq!(sm.time.agent.sum, (9 + 19 + 4) * MIN);
    // D2: accepted delivery − first turn: 9, 29, 4 min
    assert_eq!(sm.time.cycle.sum, (9 + 29 + 4) * MIN);
    assert_eq!(sm.time.cycle.p50, Some(9 * MIN));
    // D7, D8
    assert_eq!((sm.time.releases, sm.time.released), (2, 1));
    assert_eq!(sm.time.release_success_rate, Some(0.5));
    assert_eq!(sm.time.accept_to_release.p50, Some(15 * MIN));

    let q = &sm.quality;
    assert_eq!((q.first_pass, q.first_pass_n), (Some(2.0 / 3.0), 3));
    assert_eq!(q.reworks, 1);
    assert_eq!(q.rollbacks, 1);
    assert_eq!(
        q.fails
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        [
            ("error", 1),
            ("other", 0),
            ("release", 1),
            ("restart", 0),
            ("stall", 1)
        ]
    );
    assert_eq!(q.stalled_then_done, Some(0.0));
    assert_eq!(q.turn_error_rate, Some(0.0));

    let h = &sm.human;
    assert_eq!(h.decisions, 10);
    assert_eq!(h.by_kind["accept/item"], 3);
    assert_eq!(h.by_kind["fail/item"], 2);
    assert_eq!(h.by_kind["fail/stall"], 1);
    assert_eq!(h.by_kind["approve/release"], 2);
    assert_eq!(h.by_kind["approve/tool_call"], 1);
    assert_eq!(h.by_kind["ask/question"], 1);
    // 8 resolved by a person (the one Steve settled and the open stall card are not)
    assert_eq!(h.response.n, 8);
    assert_eq!(h.messages, 1);
    assert_eq!(h.approval.permissions, 0);

    let a = &sm.agents;
    assert_eq!((a.turns, a.retries, a.interrupted), (5, 1, 1));
    assert_eq!(
        (a.tools, a.tool_kinds["execute"], a.tool_kinds["tool:Bash"]),
        (1, 1, 1)
    );
    assert_eq!(a.tool_error_rate, Some(1.0));
    assert_eq!(a.retry_success, Some(1.0));

    // the sessions' own dollar figures, with no token counts
    assert!((sm.usage.reported - 3.75).abs() < 1e-9);
    assert_eq!(sm.usage.total, 0);
    assert_eq!(sm.usage.hit_rate, None);
    assert_eq!(sm.since.tokens.as_deref(), Some("2026-11-01"));

    // marks of the cancelled item: active → paused → cancelled
    let st4: Vec<(Option<String>, Option<String>)> = dropped_status_marks(&s).unwrap();
    assert_eq!(
        st4,
        [
            (None, Some("active".into())),
            (Some("active".into()), Some("paused".into())),
            (Some("paused".into()), Some("cancelled".into()))
        ]
    );

    // WIP at the end of the day: only ST-5
    let wip = s.read(|c| wip_by_day(c, &day_query())).unwrap();
    assert_eq!(wip, [("2026-11-01".to_string(), 1)]);

    // the drill-down lists hold exactly the items behind the counts
    let ids = |only: &str| -> Vec<String> {
        let mut v: Vec<String> = s
            .read(|c| views::items(c, &day_query(), "est", Some(only), None, 100, 0))
            .unwrap()
            .rows
            .into_iter()
            .map(|r| r.item_id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(ids("done").len() as i64, th.done);
    assert_eq!(ids("created").len() as i64, th.created);
    assert_eq!(ids("done"), ["ST-1", "ST-2", "ST-3"]);
}

#[test]
fn a_rebuild_changes_nothing() {
    let (_d, s) = store();
    let a = snapshot(&s).unwrap();
    refresh::rebuild(&s, WS).unwrap();
    assert_eq!(a, snapshot(&s).unwrap());
}

#[test]
fn invariants_hold() {
    let (_d, s) = store();
    // token records on top, for the breakdown sums
    let t = steve_store::Tokens {
        input: Some(100),
        output: Some(50),
        cache_read: Some(900),
        cache_write: Some(10),
        reasoning: None,
    };
    for (model, source, bot) in [
        ("claude-opus-5-5", "agent", Some("dev")),
        ("claude-haiku-5-5", "gateway", None),
        ("gpt-9", "agent", Some("dev")),
    ] {
        s.stats()
            .usage_add(&steve_store::NewUsage {
                workspace_id: WS.into(),
                ts: m(400),
                day: "2026-11-01".into(),
                source: source.into(),
                purpose: "turn".into(),
                harness: "claude-code".into(),
                turn_id: None,
                session_id: None,
                item_id: Some("ST-1".into()),
                bot_id: bot.map(Into::into),
                project_id: None,
                node_id: None,
                usage: steve_store::ModelUsage {
                    model: model.into(),
                    tokens: t,
                    ..Default::default()
                },
                duration_ms: None,
                ok: true,
                attempts: 1,
            })
            .unwrap();
    }
    refresh::refresh_all(&s).unwrap();
    let q = day_query();
    let (sm, rows, prices) = s
        .read::<_, steve_store::StoreError>(|c| {
            Ok((summary(c, &q)?.unwrap(), daily(c, &q)?, prices_of(c, WS)?))
        })
        .unwrap();
    for by in [
        By::Model,
        By::Bot,
        By::Project,
        By::Source,
        By::Purpose,
        By::Harness,
        By::Day,
    ] {
        let parts = breakdown(&rows, by, &steve_stats::PriceBook::new(prices.clone()));
        assert_eq!(
            parts.iter().map(|r| r.usage.total).sum::<i64>(),
            sm.usage.total
        );
        assert_eq!(
            parts.iter().map(|r| r.usage.turns).sum::<i64>(),
            sm.usage.turns
        );
        let est: f64 = parts.iter().map(|r| r.usage.est).sum();
        assert!((est - sm.usage.est).abs() < 1e-9);
        let rep: f64 = parts.iter().map(|r| r.usage.reported).sum();
        assert!((rep - sm.usage.reported).abs() < 1e-9);
    }
    assert_eq!(sm.usage.total, 3 * 1060);
    assert_eq!(sm.spend.unpriced_tokens, 1060);
    // the items table says how much of an item's tokens its estimate leaves out
    let page = s
        .read(|c| views::items(c, &q, "est", None, None, 100, 0))
        .unwrap();
    let st1 = page.rows.iter().find(|r| r.item_id == "ST-1").unwrap();
    assert_eq!(st1.unpriced_tokens, 1060);
    assert!(st1.est > 0.0);
    assert!(
        page.rows
            .iter()
            .filter(|r| r.item_id != "ST-1")
            .all(|r| r.unpriced_tokens == 0)
    );
    assert_eq!(sm.usage.hit_rate, Some(2700.0 / 3030.0));
    assert_eq!(sm.since.tokens.as_deref(), Some("2026-11-01"));
    // a filter is a subset that adds up the same way
    let only_dev = Query {
        bot: Some("dev".into()),
        ..day_query()
    };
    let dev = s.read(|c| summary(c, &only_dev)).unwrap().unwrap();
    assert_eq!(dev.usage.total, 2 * 1060);

    // D4 ≤ D1 and D5 ≤ D1 for every finished item; the per-item tokens include the live records
    let rows = item_totals(&s).unwrap();
    for (id, lead, wait, agent, tokens) in rows {
        if let Some(l) = lead {
            assert!(wait.unwrap_or(0) <= l && agent.unwrap_or(0) <= l, "{id}");
        }
        assert_eq!(tokens, if id == "ST-1" { 3 * 1060 } else { 0 }, "{id}");
    }
    assert_eq!(sm.per_item.tokens.sum, 3 * 1060);
}

#[test]
fn idle_refresher_does_not_wait_for_the_store_writer() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (_dir, s) = store();
    refresh::refresh_all(&s).unwrap();
    let mut refresher = steve_stats::Refresher::new();
    assert_eq!(refresher.tick(&s, 200).unwrap(), 0);

    let held = s.clone();
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        held.write(|_| {
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    });
    locked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let refresh = std::thread::spawn(move || {
        done_tx.send(refresher.tick(&s, 200)).unwrap();
    });
    // Always release the writer before asserting, so a regression fails rather than hangs.
    let result = done_rx.recv_timeout(Duration::from_secs(2));
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    refresh.join().unwrap();
    assert_eq!(result.expect("idle tick waited for the writer").unwrap(), 0);
}

#[test]
fn refresher_still_processes_dirty_rows_and_new_decisions() {
    let (_dir, s) = store();
    refresh::refresh_all(&s).unwrap();
    let mut refresher = steve_stats::Refresher::new();
    assert_eq!(refresher.tick(&s, 200).unwrap(), 0);
    s.stats().touch_item("ST-1").unwrap();
    assert!(s.stats().refresh_pending(0).unwrap());
    assert_eq!(refresher.tick(&s, 200).unwrap(), 1);
    assert!(!s.stats().refresh_pending(0).unwrap());
    resolve_question(&s).unwrap();
    assert!(
        s.stats()
            .refresh_pending(steve_store::now() - 1_000)
            .unwrap()
    );
    assert_eq!(refresher.tick(&s, 200).unwrap(), 1);
    assert!(!s.stats().refresh_pending(0).unwrap());
}

#[test]
fn tree_follows_who_started_whom() {
    let (_d, s) = store();
    seed_tree(&s).unwrap();
    refresh::rebuild(&s, WS).unwrap();
    let (ws, tree) = s
        .read::<_, steve_store::StoreError>(|c| Ok(views::tree(c, "ST-1")?))
        .unwrap()
        .unwrap();
    assert_eq!(ws, WS);
    assert!(!tree.truncated);
    let shape: Vec<_> = tree
        .nodes
        .iter()
        .map(|n| (n.item_id.as_str(), n.parent_id.as_deref(), n.depth))
        .collect();
    assert_eq!(
        shape,
        [
            ("ST-1", None, 0),
            ("ST-2", Some("ST-1"), 1),
            ("ST-3", Some("ST-1"), 1),
            ("ST-5", Some("ST-3"), 2)
        ]
    );
    // each row is the item's own figures, as on its drill-down
    for n in &tree.nodes {
        let (_, d) = s
            .read::<_, steve_store::StoreError>(|c| {
                Ok(views::item_detail(c, &n.item_id, m(400), true)?)
            })
            .unwrap()
            .unwrap();
        assert_eq!(n.reported, d.usage.reported, "{}", n.item_id);
        assert_eq!(n.agent_ms, d.facts.agent_ms, "{}", n.item_id);
    }
    assert!(
        s.read::<_, steve_store::StoreError>(|c| Ok(views::tree(c, "ST-404")?))
            .unwrap()
            .is_none()
    );
}

#[test]
fn tree_stops_at_its_depth() {
    let (_d, s) = store();
    seed_tree_chain(&s, views::TREE_DEPTH).unwrap();
    let (_, tree) = s
        .read::<_, steve_store::StoreError>(|c| Ok(views::tree(c, "ST-5")?))
        .unwrap()
        .unwrap();
    assert!(tree.truncated);
    assert_eq!(tree.nodes.last().unwrap().depth, views::TREE_DEPTH);
}

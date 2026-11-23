//! The summary's "工作流" section on one work graph and one plain item, with the expected figures
//! worked out by hand below. All times are minutes after 2026-11-01 00:00 UTC; `stats.tz` is UTC.
//!
//! ST-1 is the graph's item (its coordinator session `sc`); its units are `work` (ST-2), `review`
//! (ST-3) and `integrate` (ST-4). The review failed the first work: work was reset and both ran a
//! second time. ST-5 is an ordinary item.

use steve_stats::metrics::{Query, summary};
use steve_stats::workflow::LONG_TURN_MS;
use steve_store::testkit::stats::seed_workflow;
use steve_store::{NoPublisher, Store, WS};

const T0: i64 = 1_793_491_200_000;
const MIN: i64 = 60_000;

fn m(x: i64) -> i64 {
    T0 + x * MIN
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    seed_workflow(&s, m, LONG_TURN_MS).unwrap();
    (dir, s)
}

fn q(bot: Option<&str>) -> Query {
    Query {
        workspace_id: WS.into(),
        from: "2026-11-01".into(),
        to: "2026-11-01".into(),
        bot: bot.map(Into::into),
        ..Query::default()
    }
}

#[test]
fn the_workflow_section_reads_the_graph_the_marks_and_the_sessions() {
    let (_d, s) = store();
    let sum = s
        .read::<_, steve_store::StoreError>(|c| Ok(summary(c, &q(None))?.unwrap()))
        .unwrap();
    let w = &sum.workflow;
    // five attempts of three units; the second work and review are retries
    assert_eq!((w.graphs, w.units, w.attempts, w.retries), (1, 3, 5, 2));
    assert_eq!(w.retry_rate, Some(0.4));
    assert_eq!(
        (w.attempts_per_unit.sum, w.attempts_per_unit.p50),
        (5, Some(2))
    );
    assert_eq!((w.resets, w.unit_deliveries), (1, 5));
    // first attempts: work passed, review did not, integrate passed
    let ft = |k: &str| (w.first_try[k].delivered, w.first_try[k].passed);
    assert_eq!(
        (ft("work"), ft("review"), ft("integrate")),
        ((1, 1), (1, 0), (1, 1))
    );
    assert_eq!(w.first_try["review"].rate, Some(0.0));
    // ST-1: its own session and its units' three; ST-5: one
    assert_eq!(w.sessions_per_item.n, 2);
    assert_eq!(w.sessions_per_item.sum, 5);
    assert_eq!((w.coordinator_tokens, w.unit_tokens), (400, 600));
    assert_eq!(w.coordinator_share, Some(0.4));
    assert_eq!((w.coordinator_turn_ms.n, w.coordinator_long_turns), (2, 1));
    assert_eq!(
        w.wakes
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        [("failed", 1), ("succeeded", 2)]
    );
    assert_eq!(w.invalidations.get("plan"), Some(&1));
    // accepted at 60, merged at 68
    assert_eq!((w.merged, w.accept_to_merge.p50), (1, Some(8 * MIN)));
    // one refusal against one item delivery and five unit deliveries
    assert_eq!((w.rejected, w.delivered), (1, 6));
    assert_eq!(w.rejects.get("unit_evidence"), Some(&1));
    assert_eq!(w.reject_rate, Some(1.0 / 7.0));
    // the first reply counts, two minutes after the message
    assert_eq!((w.reply_latency.n, w.reply_latency.p50), (1, Some(2 * MIN)));
    // sc 2, sw1 2 (its first tool_call event), sw2 1, s5 1; sr1 made none
    assert_eq!(
        (w.first_tool.n, w.first_tool.sum, w.no_tool),
        (4, 6 * MIN, 1)
    );
    // the two kinds of rework side by side: no acceptance rework here, two unit retries
    assert_eq!((sum.quality.reworks, sum.quality.unit_retries), (0, 2));
}

#[test]
fn the_filters_go_through_the_graphs_item() {
    let (_d, s) = store();
    let w = s
        .read::<_, steve_store::StoreError>(|c| {
            Ok(summary(c, &q(Some("other")))?.unwrap().workflow)
        })
        .unwrap();
    assert_eq!((w.graphs, w.attempts, w.rejected, w.merged), (0, 0, 0, 0));
    // replies follow the workspace only
    assert_eq!(w.reply_latency.n, 1);
}

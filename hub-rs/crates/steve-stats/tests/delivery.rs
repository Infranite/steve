//! General delivery and response statistics on completed items, marks, sessions, and channel replies.
//! All times are minutes after 2026-11-01 00:00 UTC; `stats.tz` is UTC.

use steve_stats::metrics::{Query, summary};
use steve_store::testkit::stats::seed_delivery;
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
    seed_delivery(&s, m).unwrap();
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
fn the_delivery_section_reads_marks_sessions_and_channel_replies() {
    let (_d, s) = store();
    let sum = s
        .read::<_, steve_store::StoreError>(|c| Ok(summary(c, &q(None))?.unwrap()))
        .unwrap();
    let d = &sum.delivery;
    // ST-1 has 2 sessions; ST-5 has 1 session
    assert_eq!(d.sessions_per_item.n, 2);
    assert_eq!(d.sessions_per_item.sum, 3);
    assert_eq!(d.invalidations.get("plan"), Some(&1));
    // accepted at 60, merged at 68
    assert_eq!((d.merged, d.accept_to_merge.p50), (1, Some(8 * MIN)));
    // one refusal against one item delivery
    assert_eq!((d.rejected, d.delivered), (1, 1));
    assert_eq!(d.rejects.get("design_file"), Some(&1));
    assert_eq!(d.reject_rate, Some(0.5));
    // the first reply counts, two minutes after the message
    assert_eq!((d.reply_latency.n, d.reply_latency.p50), (1, Some(2 * MIN)));
    // sc 2m, sw1 2m (first tool_call event), s5 1m; sr1 made none
    assert_eq!(
        (d.first_tool.n, d.first_tool.sum, d.no_tool),
        (3, 5 * MIN, 1)
    );
}

#[test]
fn delivery_filters_scope_items_while_replies_follow_workspace() {
    let (_d, s) = store();
    let d = s
        .read::<_, steve_store::StoreError>(|c| {
            Ok(summary(c, &q(Some("other")))?.unwrap().delivery)
        })
        .unwrap();
    assert_eq!((d.sessions_per_item.n, d.rejected, d.merged), (0, 0, 0));
    // replies follow the workspace only
    assert_eq!(d.reply_latency.n, 1);
}

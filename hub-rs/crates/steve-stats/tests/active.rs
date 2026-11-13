//! The `active` series (the sessions page's activity heatmap): per local day, the distinct pieces of work an agent
//! turned on. An item counts once a day however many turns or sessions it had; a conversation with no item is a
//! piece of its own; a turn just after local midnight belongs to the next day. Asia/Shanghai, UTC+8.

use steve_stats::metrics::{Query, bounds};
use steve_stats::tz;
use steve_stats::views::{self, Bucket, Metric};
use steve_store::{NoPublisher, Store, TurnBegin, WS};

const MIN: i64 = 60_000;

fn begin(s: &Store, session: &str, item: Option<&str>, started: i64) {
    s.stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: session.into(),
            item_id: item.map(Into::into),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            started,
            ..TurnBegin::default()
        })
        .expect("begin");
}

#[test]
fn items_count_once_a_day_and_unlinked_conversations_on_their_own() {
    let dir = tempfile::tempdir().expect("tmp");
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).expect("open");
    s.ensure_workspace().expect("ws");
    s.with_conn(|c| {
        Ok(c.execute_batch("INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'stats.tz', '\"Asia/Shanghai\"', 0);")?)
    })
    .expect("tz");
    let (d1, _) = bounds("2026-11-05", "2026-11-05", tz(Some("Asia/Shanghai"))).expect("day");
    // 11-05: ST-1 in two sessions and three turns, ST-2 once, one unlinked conversation twice
    begin(&s, "a", Some("ST-1"), d1 + 60 * MIN);
    begin(&s, "a", Some("ST-1"), d1 + 120 * MIN);
    begin(&s, "b", Some("ST-1"), d1 + 600 * MIN);
    begin(&s, "c", Some("ST-2"), d1 + 700 * MIN);
    begin(&s, "chat", None, d1 + 800 * MIN);
    begin(&s, "chat", None, d1 + 900 * MIN);
    // 11-06, one minute after local midnight: ST-1 again counts for the new day
    begin(&s, "a", Some("ST-1"), d1 + 24 * 60 * MIN + MIN);
    let q = Query {
        workspace_id: WS.into(),
        from: "2026-11-04".into(),
        to: "2026-11-06".into(),
        ..Query::default()
    };
    let series = s
        .with_conn(|c| views::series(c, &q, Metric::Active, "none", Bucket::Day))
        .expect("series");
    assert_eq!(series.buckets, ["2026-11-04", "2026-11-05", "2026-11-06"]);
    assert_eq!(series.series.len(), 1);
    assert_eq!(series.series[0].values, [Some(0.0), Some(3.0), Some(1.0)]);
    assert!(Metric::parse("active").is_some_and(|m| m.allows("bot") && !m.allows("model")));
}

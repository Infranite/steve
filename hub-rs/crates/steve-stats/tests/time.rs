//! The time series of the daily chart: `turn_ms` (agent working time, each turn whole in the bucket it ended in,
//! adding up to the summary's `usage.turn_ms`) and `wait_ms` (time a person was waited on: per item, the time any
//! of its cards for a person was open, cut at local midnight; a card YOLO answered is not a person's time).
//! Asia/Shanghai, UTC+8.

use steve_stats::metrics::{Query, bounds, summary};
use steve_stats::views::{self, Bucket, Metric, Series};
use steve_stats::{refresh, tz};
use steve_store::testkit::stats::{Card, seed_time};
use steve_store::{NoPublisher, Store, TurnBegin, TurnFinish, WS};

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

fn zone() -> Option<chrono_tz::Tz> {
    tz(Some("Asia/Shanghai"))
}

fn q(from: &str, to: &str) -> Query {
    Query {
        workspace_id: WS.into(),
        from: from.into(),
        to: to.into(),
        ..Query::default()
    }
}

fn turn(s: &Store, started: i64, ended: i64) {
    let id = s
        .stats()
        .turn_begin(&TurnBegin {
            workspace_id: WS.into(),
            session_id: "s1".into(),
            item_id: Some("ST-1".into()),
            bot_id: Some("dev".into()),
            harness: "claude-code".into(),
            started,
            ..TurnBegin::default()
        })
        .expect("begin");
    s.stats()
        .turn_finish(&TurnFinish {
            turn_id: id,
            ended,
            day: steve_stats::day(ended, zone()),
            model: "claude-opus-5-5".into(),
            stop_reason: Some("end_turn".into()),
            ..TurnFinish::default()
        })
        .expect("finish");
}

fn store(cards: &[Card]) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tmp");
    let s = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).expect("open");
    s.ensure_workspace().expect("ws");
    seed_time(&s, cards).expect("seed");
    (dir, s)
}

fn series(s: &Store, q: &Query, m: Metric, bucket: Bucket) -> Series {
    s.read(|c| views::series(c, q, m, "none", bucket))
        .expect("series")
}

#[test]
fn agent_time_per_day_adds_up_to_the_summary() {
    let (d1, _) = bounds("2026-11-05", "2026-11-05", zone()).expect("day");
    let (_d, s) = store(&[]);
    // 11-05: 20 min and 45 min; a turn across midnight (30 min) counts whole on 11-06, the day it ended
    turn(&s, d1 + 9 * HOUR, d1 + 9 * HOUR + 20 * MIN);
    turn(&s, d1 + 14 * HOUR, d1 + 14 * HOUR + 45 * MIN);
    turn(&s, d1 + 24 * HOUR - 10 * MIN, d1 + 24 * HOUR + 20 * MIN);
    refresh::refresh_all(&s).expect("refresh");

    let range = q("2026-11-04", "2026-11-06");
    let t = series(&s, &range, Metric::TurnMs, Bucket::Day);
    assert_eq!(t.buckets, ["2026-11-04", "2026-11-05", "2026-11-06"]);
    assert_eq!(t.series.len(), 1);
    assert_eq!(
        t.series[0].values,
        [Some(0.0), Some((65 * MIN) as f64), Some((30 * MIN) as f64)]
    );
    let sm = s
        .read(|c| summary(c, &range))
        .expect("summary")
        .expect("range");
    assert_eq!(
        t.series[0].values.iter().flatten().sum::<f64>(),
        sm.usage.turn_ms as f64
    );

    // per hour of one day: the bucket each turn ended in
    let h = series(
        &s,
        &q("2026-11-05", "2026-11-05"),
        Metric::TurnMs,
        Bucket::Hour {
            now: d1 + 48 * HOUR,
        },
    );
    assert_eq!(h.series[0].values[9], Some((20 * MIN) as f64));
    assert_eq!(h.series[0].values[14], Some((45 * MIN) as f64));
    assert_eq!(
        h.series[0].values.iter().flatten().sum::<f64>(),
        (65 * MIN) as f64
    );

    let m = Metric::parse("turn_ms").expect("turn_ms");
    assert!(m.allows("none") && m.allows("bot") && m.allows("model") && !m.allows("token_type"));
}

#[test]
fn waiting_on_you_is_cut_at_midnight_and_counted_once_per_item() {
    let (d1, _) = bounds("2026-11-05", "2026-11-05", zone()).expect("day");
    let (_d, s) = store(&[
        // ST-1: an approval open 22:00 → 02:00 overlapping a question 23:00 → 01:00: four hours, two each side
        (
            "a",
            Some("ST-1"),
            "approve",
            d1 + 22 * HOUR,
            Some(d1 + 26 * HOUR),
        ),
        (
            "b",
            Some("ST-1"),
            "ask",
            d1 + 23 * HOUR,
            Some(d1 + 25 * HOUR),
        ),
        // ST-2 waits at the same time: a person is waited on for each item
        (
            "c",
            Some("ST-2"),
            "accept",
            d1 + 23 * HOUR,
            Some(d1 + 24 * HOUR),
        ),
        // a card with no item counts on its own
        (
            "e",
            None,
            "confirm",
            d1 + 8 * HOUR,
            Some(d1 + 8 * HOUR + 30 * MIN),
        ),
        // before the range, still open long after it: the whole of 11-07
        ("f", Some("ST-2"), "ask", d1 - 72 * HOUR, None),
    ]);
    let w = series(
        &s,
        &q("2026-11-05", "2026-11-07"),
        Metric::WaitMs,
        Bucket::Day,
    );
    let day = (24 * HOUR) as f64;
    assert_eq!(
        w.series[0].values,
        [
            // 11-05: ST-1 2 h, ST-2's own card 1 h but its open question already all day, the no-item 30 min
            Some((2 * HOUR + 30 * MIN) as f64 + day),
            // 11-06: ST-1 2 h, ST-2 all day
            Some((2 * HOUR) as f64 + day),
            Some(day),
        ]
    );
    // per hour of 11-05: 23:00 has ST-1 and ST-2 (whose question covers it anyway)
    let h = series(
        &s,
        &q("2026-11-05", "2026-11-05"),
        Metric::WaitMs,
        Bucket::Hour {
            now: d1 + 96 * HOUR,
        },
    );
    assert_eq!(h.series[0].values[23], Some((2 * HOUR) as f64));
    assert_eq!(h.series[0].values[8], Some((HOUR + 30 * MIN) as f64));
    assert_eq!(h.series[0].values[11], Some(HOUR as f64));

    let m = Metric::parse("wait_ms").expect("wait_ms");
    assert!(m.allows("none") && m.allows("bot") && m.allows("project") && !m.allows("model"));
}

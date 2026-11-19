#![cfg(feature = "tz")]
use chrono::NaiveDateTime;
use steve_runtime::tz::{Quiet, Zone};
fn wall(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
}
// Each gap's start/middle/end, then fold start/middle/end; explicit UTC milliseconds.
#[rustfmt::skip]
const DST: &[(&str, &str, i64)] = &[
    ("America/New_York", "2026-03-08 02:00:00", 1772953200000), // 2026-03-08T07:00:00+00:00
    ("America/New_York", "2026-03-08 02:30:00", 1772955000000), // 2026-03-08T07:30:00+00:00
    ("America/New_York", "2026-03-08 03:00:00", 1772953200000), // 2026-03-08T07:00:00+00:00
    ("America/New_York", "2026-11-01 01:00:00", 1793509200000), // 2026-11-01T05:00:00+00:00
    ("America/New_York", "2026-11-01 01:30:00", 1793511000000), // 2026-11-01T05:30:00+00:00
    ("America/New_York", "2026-11-01 02:00:00", 1793516400000), // 2026-11-01T07:00:00+00:00
    ("Australia/Sydney", "2026-10-04 02:00:00", 1791043200000), // 2026-10-03T16:00:00+00:00
    ("Australia/Sydney", "2026-10-04 02:30:00", 1791045000000), // 2026-10-03T16:30:00+00:00
    ("Australia/Sydney", "2026-10-04 03:00:00", 1791043200000), // 2026-10-03T16:00:00+00:00
    ("Australia/Sydney", "2026-04-05 02:00:00", 1775314800000), // 2026-04-04T15:00:00+00:00
    ("Australia/Sydney", "2026-04-05 02:30:00", 1775316600000), // 2026-04-04T15:30:00+00:00
    ("Australia/Sydney", "2026-04-05 03:00:00", 1775322000000), // 2026-04-04T17:00:00+00:00
    ("Europe/London", "2026-03-29 01:00:00", 1774746000000), // 2026-03-29T01:00:00+00:00
    ("Europe/London", "2026-03-29 01:30:00", 1774747800000), // 2026-03-29T01:30:00+00:00
    ("Europe/London", "2026-03-29 02:00:00", 1774746000000), // 2026-03-29T01:00:00+00:00
    ("Europe/London", "2026-10-25 01:00:00", 1792886400000), // 2026-10-25T00:00:00+00:00
    ("Europe/London", "2026-10-25 01:30:00", 1792888200000), // 2026-10-25T00:30:00+00:00
    ("Europe/London", "2026-10-25 02:00:00", 1792893600000), // 2026-10-25T02:00:00+00:00
];
#[test]
fn rfc5545_dst_table_and_both_fold_passes() {
    for &(name, local, utc_ms) in DST {
        let z = Zone::parse(name).unwrap();
        assert_eq!(z.instant(wall(local)), utc_ms, "{name} {local}");
        // For repeated middles both real instants map to the same wall; resolution picks first.
        if [
            "2026-11-01 01:30:00",
            "2026-04-05 02:30:00",
            "2026-10-25 01:30:00",
        ]
        .contains(&local)
        {
            assert_eq!(z.local(utc_ms), wall(local));
            assert_eq!(z.local(utc_ms + 3_600_000), wall(local));
        }
    }
}
#[test]
fn zone_names_days_and_personal_workspace_quiet_semantics() {
    assert_eq!(
        Zone::parse("Eastern Standard Time").unwrap().name(),
        "America/New_York"
    );
    assert!(Zone::parse("Mars/Olympus").is_err());
    assert!(!Zone::parse("UTC").unwrap().is_fallback());
    assert!(Zone::parse(Zone::system().name()).is_ok());
    let z = Zone::parse("America/New_York").unwrap();
    for (day, hours) in [("2026-03-08 12:00:00", 23), ("2026-11-01 12:00:00", 25)] {
        let (a, b) = z.day(z.instant(wall(day)));
        assert_eq!(b - a, hours * 3_600_000);
        assert_eq!(z.local(a).time(), chrono::NaiveTime::MIN);
    }
    let q = Quiet {
        start: "23:00:00".parse().unwrap(),
        end: "08:00:00".parse().unwrap(),
        zone: z,
    };
    for local in ["2026-03-07 23:00:00", "2026-03-08 07:59:00"] {
        assert_eq!(
            q.holds(z.instant(wall(local))),
            Some(z.instant(wall("2026-03-08 08:00:00")))
        );
    }
    assert_eq!(q.holds(z.instant(wall("2026-03-08 08:00:00"))), None);
    let day = Quiet {
        start: "12:00:00".parse().unwrap(),
        end: "13:00:00".parse().unwrap(),
        zone: z,
    };
    assert!(day.holds(z.instant(wall("2026-03-08 12:00:00"))).is_some());
    let disabled = Quiet {
        end: day.start,
        ..day
    };
    assert_eq!(disabled.holds(z.instant(wall("2026-03-08 12:00:00"))), None);
}

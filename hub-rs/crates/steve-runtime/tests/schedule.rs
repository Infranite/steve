#![cfg(feature = "schedule")]
use chrono::{DateTime, Duration, Utc};
use steve_runtime::{
    schedule::{CronError, CronExpr, Schedule, validate},
    tz::Zone,
};
fn utc(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
#[test]
fn copied_flow_syntax_frequency_and_calendar_tests() {
    #[rustfmt::skip]
    let accepted = [ ("30 8 * * *","30 8 * * *"), (" 0 9 * * mon-fri ","0 9 * * 1-5"),
        ("0 9 * * SUN,sat","0 9 * * 0,6"), ("0 10 1 jan,Jul *","0 10 1 1,7 *"),
        ("*/15 * * * *","*/15 * * * *"), ("0 0-12/3 * * *","0 0-12/3 * * *"),
        ("0 18 L * *","0 18 L * *"), ("0 9 * * 7","0 9 * * 7"), ("@hourly","0 * * * *"), ("@DAILY","0 0 * * *"),
        ("@weekly","0 0 * * 0"), ("@monthly","0 0 1 * *") ];
    for (a, b) in accepted {
        assert_eq!(CronExpr::parse(a).unwrap().as_str(), b);
    }
    #[rustfmt::skip]
    let rejected = [
    ("0 0 9 * * *", "0 0 9 * * *"),
    ("0 0 9 * * * 2026", "0 0 9 * * * 2026"),
    ("0 9 * *", "0 9 * *"),
    ("0 9 * * 5#2", "5#2"),
    ("0 9 15W * *", "15W"),
    ("0 9 ? * 1", "?"),
    ("0 9 * * 5L", "5L"),
    ("0 9 * * L", "L"),
    ("@reboot", "@reboot"),
    ("@yearly", "@yearly"),
    ("60 9 * * *", "60"),
    ("0 24 * * *", "24"),
    ("0 9 0 * *", "0"),
    ("0 9 * 13 *", "13"),
    ("0 9 * * 8", "8"),
    ("0 9-5 * * *", "9-5"),
    ("0/10 * * * *", "0/10"),
    ("/10 * * * *", "/10"),
    ("*/0 * * * *", "*/0"),
    ("0 9 * * MONDAY", "MONDAY"),
    ("", ""),
    ];
    for (input, fragment) in rejected {
        assert_eq!(
            CronExpr::parse(input),
            Err(CronError::Unsupported(fragment.into()))
        );
    }
    let z = Zone::parse("UTC").unwrap();
    let now = utc("2026-11-01T00:00:00Z");
    for bad in ["*/2 * * * *", "* * * * *", "0,3 9 * * *", "*/4 * * * *"] {
        assert_eq!(validate(bad, &z, now), Err(CronError::TooFrequent));
    }
    for ok in ["*/5 * * * *", "0,5 9 * * *", "0 * * * *", "0 9 * * 1"] {
        assert!(validate(ok, &z, now).is_ok());
    }
    assert!(matches!(
        validate("0 9 31 2 *", &z, now),
        Err(CronError::Never(_))
    ));
    let e = validate("*/5 * * * *", &z, now).unwrap();
    assert_eq!(
        e.next_after_ms(&z, now.timestamp_millis()),
        Some((now + Duration::minutes(5)).timestamp_millis())
    );
    assert_eq!(
        e.count_between(&z, now, now + Duration::days(30), 1000),
        1000
    );
    assert_eq!(e.count_between(&z, now, now + Duration::minutes(5), 10), 2);
    let e = CronExpr::parse("0 9 15 * 5").unwrap();
    let days: Vec<_> = e
        .upcoming(&z, now, 4)
        .iter()
        .map(|t| chrono::Datelike::day(t))
        .collect();
    assert_eq!(days, [6, 13, 15, 20]);
    let e = CronExpr::parse("0 18 L * *").unwrap();
    let days: Vec<_> = e
        .upcoming(&z, utc("2027-01-01T00:00:00Z"), 3)
        .iter()
        .map(|t| chrono::Datelike::day(t))
        .collect();
    assert_eq!(days, [31, 28, 31]);
    assert_eq!(
        CronExpr::parse("0 9 * * 7").unwrap().next_after(&z, now),
        Some(utc("2026-11-01T09:00:00Z"))
    );
}
#[test]
fn cron_gap_shift_interleaving_and_folds() {
    let z = Zone::parse("America/Los_Angeles").unwrap();
    #[rustfmt::skip]
    let cases = [
        ("30 2 * * *", "2027-03-14T08:00:00Z", "2027-03-14T10:30:00Z"), // old 10:00
        ("30 2,14 * * *", "2027-03-14T08:00:00Z", "2027-03-14T10:30:00Z"), // old 10:00
        ("*/15 * * * *", "2027-03-14T09:40:00Z", "2027-03-14T09:45:00Z"),
        ("*/15 * * * *", "2027-03-14T10:00:00Z", "2027-03-14T10:15:00Z"),
        ("15 2 * * *", "2027-03-14T10:00:00Z", "2027-03-14T10:15:00Z"), // shifted slot still pending
        ("0,45 2,3 * * *", "2027-03-14T10:01:00Z", "2027-03-14T10:45:00Z"),
        ("30 1 * * *", "2026-11-01T07:00:00Z", "2026-11-01T08:30:00Z"),
        ("30 1 * * *", "2026-11-01T09:10:00Z", "2026-11-02T09:30:00Z"),
        ("0 * * * *", "2026-11-01T08:00:00Z", "2026-11-01T10:00:00Z"),
    ];
    for (expr, from, to) in cases {
        assert_eq!(
            CronExpr::parse(expr).unwrap().next_after(&z, utc(from)),
            Some(utc(to)),
            "{expr} {from}"
        );
    }
    let every = Schedule::Every(std::time::Duration::from_millis(10));
    assert_eq!(every.next_after(-1), Some(0));
    assert_eq!(every.missed(-1, 100, 4), 4);
    assert_eq!(every.missed(10, 10, 5), 0);
    assert_eq!(
        Schedule::Every(std::time::Duration::ZERO).next_after(0),
        None
    );
    assert_eq!(Schedule::At(12).missed(11, 12, 5), 1);
    assert_eq!(Schedule::At(12).next_after(12), None);
    let cron = Schedule::Cron(CronExpr::parse("30 1 * * *").unwrap(), z);
    assert_eq!(
        cron.missed(
            utc("2026-11-01T07:00:00Z").timestamp_millis(),
            utc("2026-11-01T11:00:00Z").timestamp_millis(),
            10
        ),
        1
    );
}

#[test]
fn copied_flow_multi_zone_weekdays_month_ends_and_counts() {
    let now = utc("2026-11-01T00:00:00Z");
    #[rustfmt::skip]
    let cases = [
        ("Asia/Taipei", "30 8 * * *", "2026-11-01T00:30:00Z"),
        ("Asia/Taipei", "0 9 * * *", "2026-11-01T01:00:00Z"),
        ("Asia/Tokyo", "0 9 * * *", "2026-11-02T00:00:00Z"),
        ("Asia/Taipei", "0 9 * * 5", "2026-11-06T01:00:00Z"),
        ("Asia/Taipei", "0 9 * * 7", "2026-11-01T01:00:00Z"),
    ];
    for (zone, expr, expected) in cases {
        let z = Zone::parse(zone).unwrap();
        let e = CronExpr::parse(expr).unwrap();
        assert_eq!(e.next_after(&z, now), Some(utc(expected)));
        assert_eq!(
            e.next_after_ms(&z, now.timestamp_millis()),
            Some(utc(expected).timestamp_millis())
        );
    }
    for name in [
        "Asia/Taipei",
        "Asia/Shanghai",
        "Asia/Tokyo",
        "UTC",
        "Europe/London",
        "America/Los_Angeles",
        "America/New_York",
    ] {
        assert!(Zone::parse(name).is_ok());
    }
    let z = Zone::parse("Asia/Taipei").unwrap();
    let e = CronExpr::parse("30 8 * * *").unwrap();
    let from = utc("2026-11-01T00:30:00Z");
    let to = utc("2026-11-04T00:30:00Z");
    assert_eq!(e.next_after(&z, from), Some(utc("2026-11-02T00:30:00Z")));
    assert_eq!(e.count_between(&z, from, to, 1000), 4);
    assert_eq!(
        e.count_between(&z, from, to - Duration::minutes(1), 1000),
        3
    );
    assert_eq!(e.count_between(&z, from, to, 2), 2);
    let last = CronExpr::parse("0 18 L * *").unwrap();
    for (year, days) in [(2027, [31, 28, 31]), (2028, [31, 29, 31])] {
        let start = utc(&format!("{year}-01-01T00:00:00Z"));
        let expected: Vec<_> = days
            .into_iter()
            .enumerate()
            .map(|(m, d)| utc(&format!("{year}-{:02}-{d:02}T10:00:00Z", m + 1)))
            .collect();
        assert_eq!(last.upcoming(&z, start, 3), expected);
    }
    for ms in [i64::MIN, i64::MAX] {
        assert_eq!(e.next_after_ms(&z, ms), None);
    }
}
#[test]
fn copied_flow_dst_upcoming_sequences_use_rfc5545() {
    let z = Zone::parse("America/Los_Angeles").unwrap();
    #[rustfmt::skip]
    let cases: &[(&str,&str,&[&str])] = &[
        ("30 2 * * *", "2027-03-13T20:00:00Z", &["2027-03-14T10:30:00Z", "2027-03-15T09:30:00Z", "2027-03-16T09:30:00Z"]),
        ("30 2,14 * * *", "2027-03-14T08:00:00Z", &["2027-03-14T10:30:00Z", "2027-03-14T21:30:00Z"]),
        ("*/15 * * * *", "2027-03-14T09:40:00Z", &["2027-03-14T09:45:00Z", "2027-03-14T10:00:00Z", "2027-03-14T10:15:00Z"]),
        ("30 1 * * *", "2026-10-31T19:00:00Z", &["2026-11-01T08:30:00Z", "2026-11-02T09:30:00Z"]),
        ("0 * * * *", "2026-11-01T06:30:00Z", &["2026-11-01T07:00:00Z", "2026-11-01T08:00:00Z", "2026-11-01T10:00:00Z"]),
    ];
    for &(expr, start, expected) in cases {
        assert_eq!(
            CronExpr::parse(expr)
                .unwrap()
                .upcoming(&z, utc(start), expected.len()),
            expected.iter().map(|s| utc(s)).collect::<Vec<_>>()
        );
    }
}

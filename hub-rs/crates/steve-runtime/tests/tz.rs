#![cfg(feature = "tz")]
use chrono::NaiveDateTime;
use steve_runtime::tz::{Quiet, TimeError, Zone};
fn wall(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
}
// Gap start/middle/end and independent fold passes; all UTC milliseconds are literal.
#[rustfmt::skip]
const DST: &[(&str, &str, i64, Option<i64>)] = &[
    ("Australia/Lord_Howe", "2026-10-04 02:15:00", 1791042300000, None), // 30-minute gap
    ("Pacific/Apia", "2011-12-30 12:00:00", 1325282400000, None), // skipped day
    ("America/New_York", "2026-03-08 02:00:00", 1772953200000, None),
    ("America/New_York", "2026-03-08 02:30:00", 1772955000000, None),
    ("America/New_York", "2026-03-08 03:00:00", 1772953200000, Some(1772953200000)),
    ("America/New_York", "2026-11-01 01:00:00", 1793509200000, Some(1793509200000)),
    ("America/New_York", "2026-11-01 01:00:00", 1793509200000, Some(1793512800000)), // second pass
    ("America/New_York", "2026-11-01 01:30:00", 1793511000000, Some(1793511000000)),
    ("America/New_York", "2026-11-01 01:30:00", 1793511000000, Some(1793514600000)), // second pass
    ("America/New_York", "2026-11-01 02:00:00", 1793516400000, Some(1793516400000)),
    ("Australia/Sydney", "2026-10-04 02:00:00", 1791043200000, None),
    ("Australia/Sydney", "2026-10-04 02:30:00", 1791045000000, None),
    ("Australia/Sydney", "2026-10-04 03:00:00", 1791043200000, Some(1791043200000)),
    ("Australia/Sydney", "2026-04-05 02:00:00", 1775314800000, Some(1775314800000)),
    ("Australia/Sydney", "2026-04-05 02:00:00", 1775314800000, Some(1775318400000)), // second pass
    ("Australia/Sydney", "2026-04-05 02:30:00", 1775316600000, Some(1775316600000)),
    ("Australia/Sydney", "2026-04-05 02:30:00", 1775316600000, Some(1775320200000)), // second pass
    ("Australia/Sydney", "2026-04-05 03:00:00", 1775322000000, Some(1775322000000)),
    ("Europe/London", "2026-03-29 01:00:00", 1774746000000, None),
    ("Europe/London", "2026-03-29 01:30:00", 1774747800000, None),
    ("Europe/London", "2026-03-29 02:00:00", 1774746000000, Some(1774746000000)),
    ("Europe/London", "2026-10-25 01:00:00", 1792886400000, Some(1792886400000)),
    ("Europe/London", "2026-10-25 01:00:00", 1792886400000, Some(1792890000000)), // second pass
    ("Europe/London", "2026-10-25 01:30:00", 1792888200000, Some(1792888200000)),
    ("Europe/London", "2026-10-25 01:30:00", 1792888200000, Some(1792891800000)), // second pass
    ("Europe/London", "2026-10-25 02:00:00", 1792893600000, Some(1792893600000)),
];
#[test]
fn rfc5545_dst_table_and_both_fold_passes() {
    for &(name, local, resolved_ms, observed_ms) in DST {
        let z = Zone::parse(name).unwrap();
        assert_eq!(z.instant(wall(local)), Ok(resolved_ms), "{name} {local}");
        if let Some(ms) = observed_ms {
            assert_eq!(z.local(ms), Ok(wall(local)), "{name} {ms}");
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
        let (a, b) = z.day(z.instant(wall(day)).unwrap()).unwrap();
        assert_eq!(b - a, hours * 3_600_000);
        assert_eq!(z.local(a).unwrap().time(), chrono::NaiveTime::MIN);
    }
    let q = Quiet {
        start: "23:00:00".parse().unwrap(),
        end: "08:00:00".parse().unwrap(),
        zone: z,
    };
    for local in ["2026-03-07 23:00:00", "2026-03-08 07:59:00"] {
        assert_eq!(
            q.holds(z.instant(wall(local)).unwrap()),
            Some(z.instant(wall("2026-03-08 08:00:00")).unwrap())
        );
    }
    assert_eq!(
        q.holds(z.instant(wall("2026-03-08 08:00:00")).unwrap()),
        None
    );
    let day = Quiet {
        start: "12:00:00".parse().unwrap(),
        end: "13:00:00".parse().unwrap(),
        zone: z,
    };
    assert!(
        day.holds(z.instant(wall("2026-03-08 12:00:00")).unwrap())
            .is_some()
    );
    let disabled = Quiet {
        end: day.start,
        ..day
    };
    assert_eq!(
        disabled.holds(z.instant(wall("2026-03-08 12:00:00")).unwrap()),
        None
    );
}

#[test]
fn all_windows_zone_names_match_the_ics_contract() {
    #[rustfmt::skip]
    let names = [
        ("China Standard Time", "Asia/Shanghai"), ("Taipei Standard Time", "Asia/Taipei"),
        ("Tokyo Standard Time", "Asia/Tokyo"), ("Korea Standard Time", "Asia/Seoul"),
        ("Singapore Standard Time", "Asia/Singapore"), ("India Standard Time", "Asia/Kolkata"),
        ("GMT Standard Time", "Europe/London"), ("W. Europe Standard Time", "Europe/Berlin"),
        ("Romance Standard Time", "Europe/Paris"), ("Eastern Standard Time", "America/New_York"),
        ("Central Standard Time", "America/Chicago"), ("Mountain Standard Time", "America/Denver"),
        ("Pacific Standard Time", "America/Los_Angeles"), ("UTC", "UTC"),
    ];
    for (windows, iana) in names {
        assert_eq!(Zone::parse(windows).unwrap().name(), iana);
        assert_eq!(
            Zone::parse(&format!(" \"/{}\" ", windows.to_lowercase()))
                .unwrap()
                .name(),
            iana
        );
    }
}
#[test]
fn extreme_times_return_errors_instead_of_panicking() {
    let utc = Zone::parse("UTC").unwrap();
    for ms in [i64::MIN, i64::MAX] {
        assert_eq!(utc.local(ms), Err(TimeError));
        assert_eq!(utc.day(ms), Err(TimeError));
    }
    let (min, max) = (NaiveDateTime::MIN, NaiveDateTime::MAX);
    let west = Zone::parse("America/New_York").unwrap();
    let east = Zone::parse("Asia/Tokyo").unwrap();
    assert_eq!(west.instant(max), Err(TimeError));
    assert_eq!(east.instant(min), Err(TimeError));
    assert_eq!(west.local(min.and_utc().timestamp_millis()), Err(TimeError));
    assert_eq!(east.local(max.and_utc().timestamp_millis()), Err(TimeError));
    assert!(utc.instant(min).is_ok() && utc.instant(max).is_ok());
    assert_eq!(utc.day(max.and_utc().timestamp_millis()), Err(TimeError));
    assert!(utc.day(min.and_utc().timestamp_millis()).is_ok());
    for (zone, gap, shifted) in [
        (
            "Australia/Lord_Howe",
            "2026-10-04 02:15:00",
            "2026-10-04 02:45:00",
        ),
        ("Pacific/Apia", "2011-12-30 12:00:00", "2011-12-31 12:00:00"),
    ] {
        let z = Zone::parse(zone).unwrap();
        assert_eq!(z.local(z.instant(wall(gap)).unwrap()), Ok(wall(shifted)));
    }
    assert!(Zone::parse(" ").is_err());
    let q = Quiet {
        start: "23:00:00".parse().unwrap(),
        end: "08:00:00".parse().unwrap(),
        zone: utc,
    };
    assert_eq!(q.holds(i64::MAX), None);
}

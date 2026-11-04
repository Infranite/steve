//! Statistics cut days in the workspace's time zone (setting `stats.tz`, default the Hub's own zone), and a
//! one-day range into its local hours (ST-43: “今天” is charted per hour).

use chrono::{DateTime, Local, TimeZone, Timelike};
use chrono_tz::Tz;

/// The zone named by `stats.tz`; `None` (use the Hub's local zone) when unset or not an IANA name.
pub fn tz(setting: Option<&str>) -> Option<Tz> {
    setting.and_then(|s| s.trim().parse().ok())
}

/// The local date `YYYY-MM-DD` of a Unix-millisecond timestamp in `tz`, or in the Hub's zone for `None`.
pub fn day(ts_ms: i64, tz: Option<Tz>) -> String {
    local(ts_ms, tz, "%Y-%m-%d")
}

/// The local hour bucket `YYYY-MM-DDTHH:00` of a Unix-millisecond timestamp (see [`hours`]).
pub fn hour(ts_ms: i64, tz: Option<Tz>) -> String {
    local(ts_ms, tz, "%Y-%m-%dT%H:00")
}

fn local(ts_ms: i64, tz: Option<Tz>, f: &str) -> String {
    let Some(utc) = DateTime::from_timestamp_millis(ts_ms) else {
        return String::new();
    };
    match tz {
        Some(z) => z.from_utc_datetime(&utc.naive_utc()).format(f).to_string(),
        None => utc.with_timezone(&Local).format(f).to_string(),
    }
}

fn local_hour(ts_ms: i64, tz: Option<Tz>) -> Option<u32> {
    let utc = DateTime::from_timestamp_millis(ts_ms)?;
    Some(match tz {
        Some(z) => z.from_utc_datetime(&utc.naive_utc()).hour(),
        None => utc.with_timezone(&Local).hour(),
    })
}

/// Every UTC offset is a whole number of quarter hours, so local hours start on this grid from midnight.
const QUARTER: i64 = 15 * 60_000;

/// The 24 local hours of `day`: `(bucket, start, end)` in Unix ms, `end` exclusive. An hour runs from the
/// first instant whose local wall-clock hour is at least it to the first instant of the next, so the spans
/// tile the day exactly and a timestamp falls in the span of its [`hour`] bucket. A skipped hour (clocks go
/// forward) is empty, `start == end`; a repeated one (clocks go back) is two real hours long.
pub fn hours(day: &str, tz: Option<Tz>) -> Option<Vec<(String, i64, i64)>> {
    let (lo, hi) = crate::metrics::bounds(day, day, tz)?;
    let mut starts = [hi; 25];
    let mut next = 0;
    let mut t = lo;
    while t < hi && next < 24 {
        let h = local_hour(t, tz)? as usize;
        while next <= h.min(23) {
            starts[next] = t;
            next += 1;
        }
        t += QUARTER;
    }
    Some(
        (0..24)
            .map(|h| (format!("{day}T{h:02}:00"), starts[h], starts[h + 1]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_days_in_the_given_zone() {
        // 2026-11-01 16:30 UTC = 2026-11-02 00:30 in Shanghai
        let ts = 1_793_550_600_000;
        let sh = tz(Some("Asia/Shanghai"));
        assert!(sh.is_some());
        assert_eq!(day(ts, sh), "2026-11-02");
        assert_eq!(day(ts, tz(Some("UTC"))), "2026-11-01");
        assert_eq!(day(ts, tz(Some("America/Los_Angeles"))), "2026-11-01");
    }

    #[test]
    fn hours_tile_the_day_in_the_zone() {
        // Shanghai: 2026-11-05 00:00 local = 2026-11-04 16:00 UTC
        let sh = tz(Some("Asia/Shanghai"));
        let h = hours("2026-11-05", sh).unwrap_or_default();
        assert_eq!(h.len(), 24);
        assert_eq!(h[0].0, "2026-11-05T00:00");
        assert_eq!(h[23].0, "2026-11-05T23:00");
        let (lo, hi) = crate::metrics::bounds("2026-11-05", "2026-11-05", sh).unwrap_or_default();
        assert_eq!((h[0].1, h[23].2), (lo, hi));
        for w in h.windows(2) {
            assert_eq!(w[0].2, w[1].1);
        }
        for (b, s, e) in &h {
            assert_eq!(e - s, 3_600_000);
            assert_eq!(&hour(*s, sh), b);
            assert_eq!(&hour(e - 1, sh), b);
        }
        // the last millisecond of the day is 23:00, the next one is the next day's 00:00
        assert_eq!(hour(hi - 1, sh), "2026-11-05T23:00");
        assert_eq!(hour(hi, sh), "2026-11-06T00:00");
        assert_eq!(hour(lo - 1, sh), "2026-11-04T23:00");
        // a half-hour zone still cuts at local whole hours
        let ist = tz(Some("Asia/Kolkata"));
        let k = hours("2026-11-05", ist).unwrap_or_default();
        assert!(
            k.iter()
                .all(|(b, s, e)| e - s == 3_600_000 && &hour(*s, ist) == b)
        );
    }

    #[test]
    fn hours_on_days_the_clock_changes() {
        // New York goes forward at 02:00 on 2026-03-08: 02:00 does not exist
        let ny = tz(Some("America/New_York"));
        let h = hours("2026-03-08", ny).unwrap_or_default();
        assert_eq!(h[2].1, h[2].2);
        assert_eq!(h[3].2 - h[3].1, 3_600_000);
        // and back at 02:00 on 2026-11-01: 01:00 happens twice
        let h = hours("2026-11-01", ny).unwrap_or_default();
        assert_eq!(h[1].2 - h[1].1, 2 * 3_600_000);
        let (lo, hi) = crate::metrics::bounds("2026-11-01", "2026-11-01", ny).unwrap_or_default();
        assert_eq!(hi - lo, 25 * 3_600_000);
        assert_eq!((h[0].1, h[23].2), (lo, hi));
        for (b, s, e) in &h {
            if e > s {
                assert_eq!(&hour(*s, ny), b);
                assert_eq!(&hour(e - 1, ny), b);
            }
        }
    }

    #[test]
    fn unknown_zone_falls_back_to_local() {
        assert_eq!(tz(Some("Mars/Base")), None);
        assert_eq!(tz(None), None);
        assert_eq!(day(0, None).len(), 10);
    }
}

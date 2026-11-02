//! Statistics cut days in the workspace's time zone (setting `stats.tz`, default the Hub's own zone).

use chrono::{DateTime, Local, TimeZone};
use chrono_tz::Tz;

/// The zone named by `stats.tz`; `None` (use the Hub's local zone) when unset or not an IANA name.
pub fn tz(setting: Option<&str>) -> Option<Tz> {
    setting.and_then(|s| s.trim().parse().ok())
}

/// The local date `YYYY-MM-DD` of a Unix-millisecond timestamp in `tz`, or in the Hub's zone for `None`.
pub fn day(ts_ms: i64, tz: Option<Tz>) -> String {
    let Some(utc) = DateTime::from_timestamp_millis(ts_ms) else {
        return String::new();
    };
    match tz {
        Some(z) => z
            .from_utc_datetime(&utc.naive_utc())
            .format("%Y-%m-%d")
            .to_string(),
        None => utc.with_timezone(&Local).format("%Y-%m-%d").to_string(),
    }
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
    fn unknown_zone_falls_back_to_local() {
        assert_eq!(tz(Some("Mars/Base")), None);
        assert_eq!(tz(None), None);
        assert_eq!(day(0, None).len(), 10);
    }
}

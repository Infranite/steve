//! Replaces modules::zone, flow::cron::resolve_wall and calendar::ics::local_ms
//! with RFC 5545 gap/fold resolution (W2b §1.4). Policy lookup stays in domains.
#[cfg(feature = "schedule")]
use chrono::Duration;
use chrono::{DateTime, LocalResult, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc};
use chrono_tz::{GapInfo, Tz};
pub type TzError = chrono_tz::ParseError;
/// An instant, local time, or complete day is outside chrono's representable range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeError;
impl std::fmt::Display for TimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("time outside representable range")
    }
}
impl std::error::Error for TimeError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    tz: Tz,
    fallback: bool,
}
impl Zone {
    pub fn parse(name: &str) -> Result<Self, TzError> {
        let name = name.trim().trim_matches('"').trim_start_matches('/');
        let name = WINDOWS_ZONES
            .iter()
            .find(|(w, _)| w.eq_ignore_ascii_case(name))
            .map_or(name, |(_, i)| *i);
        Ok(Self {
            tz: name.parse()?,
            fallback: false,
        })
    }
    pub fn system() -> Self {
        Self::detected(iana_time_zone::get_timezone().ok().as_deref())
    }
    fn detected(name: Option<&str>) -> Self {
        name.and_then(|n| Self::parse(n).ok()).unwrap_or(Self {
            tz: chrono_tz::UTC,
            fallback: true,
        })
    }
    pub fn is_fallback(&self) -> bool {
        self.fallback
    }
    pub fn name(&self) -> &'static str {
        self.tz.name()
    }
    /// RFC 5545: resolve gaps with the pre-transition offset, folds with the first pass.
    pub fn instant(&self, wall: NaiveDateTime) -> Result<i64, TimeError> {
        match self.tz.from_local_datetime(&wall) {
            LocalResult::Single(t) => Ok(t.timestamp_millis()),
            LocalResult::Ambiguous(a, b) => Ok(a.min(b).timestamp_millis()),
            LocalResult::None => {
                let (_, offset) = GapInfo::new(&wall, &self.tz)
                    .and_then(|g| g.begin)
                    .ok_or(TimeError)?;
                Ok(wall
                    .checked_sub_offset(offset.fix())
                    .ok_or(TimeError)?
                    .and_utc()
                    .timestamp_millis())
            }
        }
    }
    pub fn local(&self, ms: i64) -> Result<NaiveDateTime, TimeError> {
        let utc = DateTime::<Utc>::from_timestamp_millis(ms)
            .ok_or(TimeError)?
            .naive_utc();
        utc.checked_add_offset(self.tz.offset_from_utc_datetime(&utc).fix())
            .ok_or(TimeError)
    }
    pub fn day(&self, ms: i64) -> Result<(i64, i64), TimeError> {
        let d = self.local(ms)?.date();
        Ok((
            self.instant(d.and_time(NaiveTime::MIN))?,
            self.instant(d.succ_opt().ok_or(TimeError)?.and_time(NaiveTime::MIN))?,
        ))
    }
    #[cfg(feature = "schedule")]
    pub(crate) fn search_start(&self, ms: i64) -> Result<NaiveDateTime, TimeError> {
        // Shifted gap candidates can still be pending after the wall clock has passed the gap.
        let at = DateTime::<Utc>::from_timestamp_millis(ms).ok_or(TimeError)?;
        let before = at.checked_sub_signed(Duration::days(2)).unwrap_or(at);
        let offset = |t: DateTime<Utc>| {
            self.tz
                .offset_from_utc_datetime(&t.naive_utc())
                .fix()
                .local_minus_utc()
        };
        self.local(ms)?
            .checked_sub_signed(Duration::seconds(i64::from(
                (offset(at) - offset(before)).max(0),
            )))
            .ok_or(TimeError)
    }
}
/// Local [start,end), crossing midnight; equal endpoints disable quiet hours.
#[derive(Debug, Clone, Copy)]
pub struct Quiet {
    pub start: NaiveTime,
    pub end: NaiveTime,
    pub zone: Zone,
}
impl Quiet {
    pub fn holds(&self, ms: i64) -> Option<i64> {
        let local = self.zone.local(ms).ok()?;
        let t = local.time();
        let inside = if self.start > self.end {
            t >= self.start || t < self.end
        } else {
            t >= self.start && t < self.end
        };
        if !inside {
            return None;
        }
        let date = if t >= self.end {
            local.date().succ_opt()?
        } else {
            local.date()
        };
        self.zone
            .instant(date.and_time(self.end))
            .ok()
            .filter(|at| *at > ms)
    }
}
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("China Standard Time", "Asia/Shanghai"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("India Standard Time", "Asia/Kolkata"),
    ("GMT Standard Time", "Europe/London"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Eastern Standard Time", "America/New_York"),
    ("Central Standard Time", "America/Chicago"),
    ("Mountain Standard Time", "America/Denver"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("UTC", "UTC"),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_detection_failure_is_observable() {
        for name in [None, Some("Mars/Olympus")] {
            let z = Zone::detected(name);
            assert_eq!(z.name(), "UTC");
            assert!(z.is_fallback());
        }
        assert!(!Zone::detected(Some("UTC")).is_fallback());
        assert_eq!(Zone::detected(Some("Asia/Tokyo")).name(), "Asia/Tokyo");
    }
}

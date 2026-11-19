//! Replaces modules::zone, flow::cron::resolve_wall and calendar::ics::local_ms
//! with RFC 5545 gap/fold resolution (W2b §1.4). Policy lookup stays in domains.
#[cfg(feature = "schedule")]
use chrono::Duration;
use chrono::{LocalResult, NaiveDateTime, NaiveTime, Offset, TimeZone};
use chrono_tz::{GapInfo, Tz};
pub type TzError = chrono_tz::ParseError;
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
    /// Panics outside chrono's representable range; all other local times have one rule.
    pub fn instant(&self, wall: NaiveDateTime) -> i64 {
        match self.tz.from_local_datetime(&wall) {
            LocalResult::Single(t) => t.timestamp_millis(),
            LocalResult::Ambiguous(a, b) => a.min(b).timestamp_millis(),
            LocalResult::None => {
                let (_, offset) = GapInfo::new(&wall, &self.tz)
                    .and_then(|g| g.begin)
                    .expect("time outside zone range");
                wall.and_utc().timestamp_millis() - i64::from(offset.fix().local_minus_utc()) * 1000
            }
        }
    }
    /// Panics when `ms` is outside chrono's representable range.
    pub fn local(&self, ms: i64) -> NaiveDateTime {
        self.tz
            .timestamp_millis_opt(ms)
            .single()
            .expect("time outside chrono range")
            .naive_local()
    }
    pub fn day(&self, ms: i64) -> (i64, i64) {
        let d = self.local(ms).date();
        (
            self.instant(d.and_time(NaiveTime::MIN)),
            self.instant(
                d.succ_opt()
                    .expect("last chrono day")
                    .and_time(NaiveTime::MIN),
            ),
        )
    }
    #[cfg(feature = "schedule")]
    pub(crate) fn search_start(&self, ms: i64) -> NaiveDateTime {
        // Shifted gap candidates can still be pending after the wall clock has passed the gap.
        let offset = |t| {
            self.tz
                .timestamp_millis_opt(t)
                .single()
                .unwrap()
                .offset()
                .fix()
                .local_minus_utc()
        };
        self.local(ms)
            - Duration::seconds(i64::from(
                (offset(ms) - offset(ms.saturating_sub(172_800_000))).max(0),
            ))
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
        let local = self.zone.local(ms);
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
        Some(self.zone.instant(date.and_time(self.end))).filter(|at| *at > ms)
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

//! Whose clock a local date or time is read on: one rule for every module.
//!
//! A person's time zone is the one in their notification policy (`notification_policies.timezone`);
//! someone without a policy gets the workspace's `tz` setting, else the system zone. Todos and their
//! reminders, new schedules, the digest's day and `/汇报` all read it here, so a person never sees one
//! module on one clock and the next on another ([`Zone::from`] says which one it was, for the UI).
//!
//! Local times become instants by one rule ([`Zone::instant`]): a time in a daylight-saving gap moves to the
//! same minute after the gap (RFC 5545), a repeated time is its first pass. Quiet hours ([`Quiet`]) are
//! minutes of the local day, `[start, end)`, crossing midnight when `start > end`.

use chrono::{Duration, NaiveDateTime, TimeZone, Timelike};
use chrono_tz::Tz;
use serde::Serialize;

/// The workspace default (`SETTINGS.tz.def`).
pub const DEFAULT_TZ: &str = "system";

/// Where a [`Zone`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoneFrom {
    /// The person's notification policy.
    Person,
    /// The workspace's `tz` setting (or its default): the person has set none.
    Workspace,
}

/// A time zone and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    pub tz: Tz,
    pub from: ZoneFrom,
}

impl Zone {
    /// A zone given outright (tests, a schedule's own `tz`).
    pub fn new(tz: Tz) -> Self {
        Self {
            tz,
            from: ZoneFrom::Person,
        }
    }

    /// Resolves the user's date rules inside a storage snapshot.
    pub fn for_user(
        r: &mut impl steve_store::Read,
        ws: &str,
        user: Option<&str>,
    ) -> steve_store::Result<Self> {
        let mut settings = steve_store::TimezoneSettings(r);
        if let Some(user) = user
            && let Some(tz) = settings
                .person(ws, user)?
                .and_then(|value| value.parse().ok())
        {
            return Ok(Self {
                tz,
                from: ZoneFrom::Person,
            });
        }
        Ok(Self {
            tz: workspace_tz(r, ws)?,
            from: ZoneFrom::Workspace,
        })
    }

    pub fn for_owner(r: &mut impl steve_store::Read, ws: &str) -> steve_store::Result<Self> {
        let owner = steve_store::Members::new(r).owner(ws)?;
        Self::for_user(r, ws, owner.as_deref())
    }

    pub fn name(&self) -> &'static str {
        self.tz.name()
    }

    /// The local wall-clock time of Unix ms.
    pub fn local(&self, ms: i64) -> Option<NaiveDateTime> {
        steve_runtime::tz::Zone::parse(self.name())
            .ok()?
            .local(ms)
            .ok()
    }

    /// Unix ms of a local time ([`Zone::instant`]).
    pub fn instant(&self, at: NaiveDateTime) -> Option<i64> {
        steve_runtime::tz::Zone::parse(self.name())
            .ok()?
            .instant(at)
            .ok()
    }

    /// RFC 3339 with this zone's offset.
    pub fn show(&self, ms: i64) -> Option<String> {
        self.tz
            .timestamp_millis_opt(ms)
            .single()
            .map(|d| d.to_rfc3339())
    }

    /// `fmt` (strftime) of Unix ms in this zone; empty when out of range.
    pub fn format(&self, ms: i64, fmt: &str) -> String {
        self.tz
            .timestamp_millis_opt(ms)
            .single()
            .map(|d| d.format(fmt).to_string())
            .unwrap_or_default()
    }

    /// The local day `YYYY-MM-DD` of Unix ms.
    pub fn day(&self, ms: i64) -> String {
        self.format(ms, "%Y-%m-%d")
    }

    /// Unix ms of the local midnight that starts the day of `ms`.
    pub fn day_start(&self, ms: i64) -> Option<i64> {
        steve_runtime::tz::Zone::parse(self.name())
            .ok()?
            .day(ms)
            .ok()
            .map(|d| d.0)
    }
}

/// The workspace's `tz` setting (a JSON string), else the system zone.
pub fn workspace_tz(r: &mut impl steve_store::Read, ws: &str) -> steve_store::Result<Tz> {
    let value = steve_store::TimezoneSettings(r).workspace(ws)?;
    Ok(resolve_workspace(value.as_deref(), default_tz))
}

fn resolve_workspace(value: Option<&str>, fallback: impl FnOnce() -> Tz) -> Tz {
    value
        .and_then(|zone| zone.trim().parse().ok())
        .unwrap_or_else(fallback)
}

/// [`DEFAULT_TZ`], parsed.
pub fn default_tz() -> Tz {
    steve_runtime::tz::Zone::system()
        .name()
        .parse()
        .expect("runtime IANA zone")
}

/// Quiet hours: minutes since local midnight, `[start, end)`; `start > end` crosses midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quiet {
    pub start: u16,
    pub end: u16,
}

impl Quiet {
    /// Quiet hours from the two stored columns; `None` when unset or unusable.
    pub fn from_parts(start: Option<u16>, end: Option<u16>) -> Option<Self> {
        match (start, end) {
            (Some(s), Some(e)) if s < 1440 && e < 1440 && s != e => Some(Self { start: s, end: e }),
            _ => None,
        }
    }

    /// Whether `now` falls in the quiet hours, on `tz`'s clock.
    pub fn contains(&self, tz: Tz, now: i64) -> bool {
        let Some(at) = tz.timestamp_millis_opt(now).single() else {
            return false;
        };
        let minute = (at.hour() * 60 + at.minute()) as u16;
        if self.start > self.end {
            minute >= self.start || minute < self.end
        } else {
            minute >= self.start && minute < self.end
        }
    }

    /// When the quiet hours `now` is in end (`None` when `now` is not in them): the next local
    /// `end` minute after `now`, by [`Zone::instant`].
    pub fn ends(&self, tz: Tz, now: i64) -> Option<i64> {
        if !self.contains(tz, now) {
            return None;
        }
        let local = tz.timestamp_millis_opt(now).single()?.naive_local();
        let end =
            chrono::NaiveTime::from_hms_opt(u32::from(self.end / 60), u32::from(self.end % 60), 0)?;
        let today = local.date().and_time(end);
        let at = if today > local {
            today
        } else {
            today + Duration::days(1)
        };
        Zone::new(tz).instant(at).filter(|&t| t > now)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_empty_and_system_settings_use_injected_system() {
        for value in [None, Some(""), Some("system")] {
            assert_eq!(
                resolve_workspace(value, || chrono_tz::Europe::London).name(),
                "Europe/London"
            );
        }
    }

    #[test]
    fn runtime_dst_table() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
        ] {
            let z = steve_runtime::tz::Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let ms = z.instant(wall).unwrap();
            let zone = Zone::new(name.parse().unwrap());
            assert_eq!(zone.instant(wall), Some(ms));
            assert_eq!(zone.local(ms), z.local(ms).ok());
            assert_eq!(zone.day_start(ms), Some(z.day(ms).unwrap().0));
        }
    }

    use super::*;
    use chrono::{NaiveDate, Utc};

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn a_gap_moves_past_it_and_a_repeat_is_the_first_pass() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let gap = NaiveDate::from_ymd_opt(2026, 3, 8)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        // 02:30 does not exist that night: 03:30 EDT = 07:30 UTC
        assert_eq!(Zone::new(ny).instant(gap), Some(utc(2026, 3, 8, 7, 30)));
        let fold = NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();
        // 01:30 happens twice: the first is EDT, 05:30 UTC
        assert_eq!(Zone::new(ny).instant(fold), Some(utc(2026, 11, 1, 5, 30)));
    }

    #[test]
    fn quiet_hours_cross_midnight_and_end_on_the_local_clock() {
        let tz: Tz = "Asia/Shanghai".parse().unwrap();
        let q = Quiet::from_parts(Some(23 * 60), Some(8 * 60)).unwrap();
        // 23:30 local = 15:30 UTC
        let late = utc(2026, 11, 18, 15, 30);
        assert!(q.contains(tz, late));
        assert_eq!(q.ends(tz, late), Some(utc(2026, 11, 19, 0, 0)));
        // 07:59 local
        let early = utc(2026, 11, 18, 23, 59);
        assert!(q.contains(tz, early));
        assert_eq!(q.ends(tz, early), Some(utc(2026, 11, 19, 0, 0)));
        // 08:00 local is no longer quiet
        assert!(!q.contains(tz, utc(2026, 11, 19, 0, 0)));
        assert_eq!(q.ends(tz, utc(2026, 11, 19, 0, 0)), None);
        let day = Quiet::from_parts(Some(12 * 60), Some(13 * 60)).unwrap();
        assert!(day.contains(tz, utc(2026, 11, 18, 4, 30)));
        assert!(!day.contains(tz, utc(2026, 11, 18, 5, 0)));
        assert_eq!(Quiet::from_parts(Some(60), Some(60)), None);
        assert_eq!(Quiet::from_parts(None, Some(60)), None);
    }

    #[test]
    fn a_person_without_a_policy_reads_the_workspace_clock() {
        let e = crate::testkit::TestEnv::new(|_| {});
        use crate::testkit::{ME, OTHER, WS};
        let store = e.store();
        let zone = |user| store.read(|r| Zone::for_user(r, WS, Some(user))).unwrap();
        steve_store::testkit::modules::notification_timezone(store, WS, OTHER, "Europe/London")
            .unwrap();
        assert_eq!(
            (zone(OTHER).name(), zone(OTHER).from),
            ("Europe/London", ZoneFrom::Person)
        );
        assert_eq!(
            (zone(ME).name(), zone(ME).from),
            (default_tz().name(), ZoneFrom::Workspace)
        );
        store
            .settings()
            .set(
                WS,
                &serde_json::json!({"tz":"America/New_York"})
                    .as_object()
                    .unwrap(),
                ME,
            )
            .unwrap();
        assert_eq!(zone(ME).name(), "America/New_York");
        assert_eq!(
            store.read(|r| Zone::for_owner(r, WS)).unwrap().name(),
            "America/New_York"
        );
        let z = zone(OTHER);
        assert_eq!(z.day(utc(2026, 11, 18, 23, 30)), "2026-11-18");
        assert_eq!(
            z.day_start(utc(2026, 11, 18, 23, 30)),
            Some(utc(2026, 11, 18, 0, 0))
        );
    }
}

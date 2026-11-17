//! Whose clock a local date or time is read on: one rule for every module.
//!
//! A person's time zone is the one in their notification policy (`notification_policies.timezone`);
//! someone without a policy gets the workspace's `tz` setting, else [`DEFAULT_TZ`]. Todos and their
//! reminders, new schedules, the digest's day and `/汇报` all read it here, so a person never sees one
//! module on one clock and the next on another ([`Zone::from`] says which one it was, for the UI).
//!
//! Local times become instants by one rule ([`instant`]): a time in a daylight-saving gap moves to the
//! first minute that exists after it, a repeated time is its first pass. Quiet hours ([`Quiet`]) are
//! minutes of the local day, `[start, end)`, crossing midnight when `start > end`.

use chrono::{Duration, NaiveDateTime, TimeZone, Timelike};
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// The workspace default (`SETTINGS.tz.def`).
pub const DEFAULT_TZ: &str = "Asia/Taipei";

/// The longest gap a daylight-saving change opens in a local clock, in minutes.
pub const DST_GAP_MAX_MIN: i64 = 180;

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

    /// `user`'s zone in `ws`: their notification policy's, else the workspace's.
    pub fn of(c: &Connection, ws: &str, user: &str) -> rusqlite::Result<Self> {
        let own: Option<String> = c
            .query_row(
                "SELECT timezone FROM notification_policies WHERE workspace_id = ? AND user_id = ?",
                params![ws, user],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(tz) = own.and_then(|s| s.parse::<Tz>().ok()) {
            return Ok(Self {
                tz,
                from: ZoneFrom::Person,
            });
        }
        Ok(Self {
            tz: workspace_tz(c, ws)?,
            from: ZoneFrom::Workspace,
        })
    }

    /// The zone of the workspace owner (what runs for the workspace as a whole: `/汇报`, the digest).
    pub fn of_owner(c: &Connection, ws: &str) -> rusqlite::Result<Self> {
        let owner: Option<String> = c
            .query_row(
                "SELECT user_id FROM workspace_members WHERE workspace_id = ? AND role = 'owner' ORDER BY created LIMIT 1",
                [ws],
                |r| r.get(0),
            )
            .optional()?;
        match owner {
            Some(u) => Self::of(c, ws, &u),
            None => Ok(Self {
                tz: workspace_tz(c, ws)?,
                from: ZoneFrom::Workspace,
            }),
        }
    }

    pub fn name(&self) -> &'static str {
        self.tz.name()
    }

    /// The local wall-clock time of Unix ms.
    pub fn local(&self, ms: i64) -> Option<NaiveDateTime> {
        self.tz
            .timestamp_millis_opt(ms)
            .single()
            .map(|d| d.naive_local())
    }

    /// Unix ms of a local time ([`instant`]).
    pub fn instant(&self, at: NaiveDateTime) -> Option<i64> {
        instant(self.tz, at)
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
        self.instant(self.local(ms)?.date().and_hms_opt(0, 0, 0)?)
    }
}

/// The workspace's `tz` setting (a JSON string), else [`DEFAULT_TZ`].
pub fn workspace_tz(c: &Connection, ws: &str) -> rusqlite::Result<Tz> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = 'tz'",
            [ws],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|v| serde_json::from_str::<String>(&v).ok())
        .and_then(|t| t.trim().parse::<Tz>().ok())
        .unwrap_or_else(default_tz))
}

/// [`DEFAULT_TZ`], parsed.
pub fn default_tz() -> Tz {
    DEFAULT_TZ.parse().unwrap_or(chrono_tz::UTC)
}

/// Unix ms of local time `at` in `tz`: the first pass of a repeated time; a time in a daylight-saving
/// gap moved to the first minute after it.
pub fn instant(tz: Tz, at: NaiveDateTime) -> Option<i64> {
    (0..=DST_GAP_MAX_MIN).find_map(|m| {
        tz.from_local_datetime(&(at + Duration::minutes(m)))
            .earliest()
            .map(|d| d.timestamp_millis())
    })
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
    /// `end` minute after `now`, by [`instant`].
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
        instant(tz, at).filter(|&t| t > now)
    }
}

#[cfg(test)]
mod tests {
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
        // 02:30 does not exist that night: 03:00 EDT = 07:00 UTC
        assert_eq!(instant(ny, gap), Some(utc(2026, 3, 8, 7, 0)));
        let fold = NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(1, 30, 0)
            .unwrap();
        // 01:30 happens twice: the first is EDT, 05:30 UTC
        assert_eq!(instant(ny, fold), Some(utc(2026, 11, 1, 5, 30)));
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
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notification_policies (workspace_id TEXT, user_id TEXT, timezone TEXT);
             CREATE TABLE settings (workspace_id TEXT, key TEXT, value TEXT);
             CREATE TABLE workspace_members (workspace_id TEXT, user_id TEXT, role TEXT, created INTEGER);
             INSERT INTO notification_policies VALUES ('w', 'me', 'Europe/London');
             INSERT INTO workspace_members VALUES ('w', 'boss', 'owner', 1);",
        )
        .unwrap();
        let me = Zone::of(&c, "w", "me").unwrap();
        assert_eq!((me.name(), me.from), ("Europe/London", ZoneFrom::Person));
        let other = Zone::of(&c, "w", "other").unwrap();
        assert_eq!(
            (other.name(), other.from),
            (DEFAULT_TZ, ZoneFrom::Workspace)
        );
        c.execute(
            "INSERT INTO settings VALUES ('w', 'tz', '\"America/New_York\"')",
            [],
        )
        .unwrap();
        assert_eq!(
            Zone::of(&c, "w", "other").unwrap().name(),
            "America/New_York"
        );
        assert_eq!(Zone::of_owner(&c, "w").unwrap().name(), "America/New_York");
        let z = Zone::of(&c, "w", "me").unwrap();
        assert_eq!(z.day(utc(2026, 11, 18, 23, 30)), "2026-11-18");
        assert_eq!(
            z.day_start(utc(2026, 11, 18, 23, 30)),
            Some(utc(2026, 11, 18, 0, 0))
        );
    }
}

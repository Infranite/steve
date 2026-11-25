//! A model quota that ran out and says when it comes back (a 429 "available again at 00:40 UTC (in
//! 9h27m)"): the turn that failed on it is not continued at once, which only fails again within a
//! second and spends Steve's continues. The item waits until then ([`crate::turn_wait::Work::Until`]) and
//! the agent is continued once the time has come. This schedules a continue; it refuses nothing.
//!
//! Kept in memory: after a restart the stop is an ordinary one again (Steve continues it, a quota
//! still out fails the same way and waits again).

use std::sync::LazyLock;

use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;

use crate::Tx;
use crate::error::Result;
use crate::util::js_head;

/// A wait longer than this is not believed (a misread number): the stop is an ordinary one.
const LONGEST_MS: i64 = 7 * 24 * 3_600_000;

static QUOTA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b429\b|quota|rate[ _-]?limit|usage limit|too many requests").unwrap()
});
static HARD_QUOTA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)quota|usage limit|insufficient_quota|billing|credit balance|credits? exhausted",
    )
    .unwrap()
});

/// True when `error` indicates a model quota or rate-limit failure.
pub(crate) fn is_quota_error(error: &str) -> bool {
    QUOTA.is_match(error)
}

/// True when `error` reports a quota/rate-limit failure with a future reset time, so short
/// transport-level retries (5s/15s/45s) are futile and the turn should hand off to core policy.
pub fn has_quota_reset(error: &str, now: i64) -> bool {
    reset_at(error, now).is_some()
}

/// True when `error` explicitly indicates hard quota/usage exhaustion (as opposed to a bare
/// transient HTTP 429 burst with no reset window).
pub(crate) fn is_hard_quota_error(error: &str) -> bool {
    HARD_QUOTA.is_match(error)
}
static RFC3339: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})")
        .unwrap()
});
static AT_UTC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bat\s+(\d{1,2}):(\d{2})\s*UTC\b").unwrap());
static IN_UNITS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bin\s+((?:\d+\s*[hms]\s*)+)\b").unwrap());
static IN_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bin\s+(\d+)\s*(seconds?|secs?|minutes?|mins?|hours?|hrs?)\b").unwrap()
});
static RETRY_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)retry[- ]after[:\s]+(\d+)").unwrap());
static UNIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s*([hms])").unwrap());

/// When the quota `error` speaks of is back (ms), when it is a quota or rate limit that says so,
/// later than `now` and within a week.
pub(crate) fn reset_at(error: &str, now: i64) -> Option<i64> {
    if !QUOTA.is_match(error) {
        return None;
    }
    let absolute = RFC3339
        .find(error)
        .and_then(|m| DateTime::parse_from_rfc3339(&m.as_str().replacen(' ', "T", 1)).ok())
        .map(|d| d.timestamp_millis())
        .or_else(|| {
            let c = AT_UTC.captures(error)?;
            let (h, m): (u32, u32) = (c[1].parse().ok()?, c[2].parse().ok()?);
            let today = Utc.timestamp_millis_opt(now).single()?.date_naive();
            let at = today.and_hms_opt(h, m, 0)?.and_utc().timestamp_millis();
            Some(if at > now { at } else { at + 24 * 3_600_000 })
        });
    let relative = || {
        if let Some(c) = IN_UNITS.captures(error) {
            let ms: i64 = UNIT
                .captures_iter(&c[1])
                .filter_map(|u| {
                    let n: i64 = u[1].parse().ok()?;
                    Some(match &u[2].to_ascii_lowercase()[..] {
                        "h" => n * 3_600_000,
                        "m" => n * 60_000,
                        _ => n * 1000,
                    })
                })
                .sum();
            return Some(now + ms);
        }
        if let Some(c) = IN_WORDS.captures(error) {
            let n: i64 = c[1].parse().ok()?;
            let unit = c[2].to_ascii_lowercase();
            let ms = if unit.starts_with('h') {
                3_600_000
            } else if unit.starts_with('m') {
                60_000
            } else {
                1000
            };
            return Some(now + n * ms);
        }
        let c = RETRY_AFTER.captures(error)?;
        Some(now + c[1].parse::<i64>().ok()? * 1000)
    };
    absolute
        .or_else(relative)
        .filter(|at| *at > now && *at - now <= LONGEST_MS)
}

/// `at` as you read it: the time, with the date when it is not today.
pub(crate) fn when(at: i64, now: i64, zone: steve_runtime::tz::Zone) -> String {
    let (Some(t), Some(n)) = (zone.local(at).ok(), zone.local(now).ok()) else {
        return String::new();
    };
    if t.date() == n.date() {
        t.format("%H:%M").to_string()
    } else {
        t.format("%m-%d %H:%M").to_string()
    }
}

/// A session waiting for its model's quota.
#[derive(Debug, Clone)]
pub(crate) struct QuotaWait {
    pub session: String,
    pub until: i64,
}

impl Tx<'_> {
    fn quota_zone(&self, item_id: &str) -> steve_runtime::tz::Zone {
        self.db()
            .item(item_id)
            .ok()
            .flatten()
            .and_then(|i| {
                self.db()
                    .store()
                    .read(|r| steve_modules::zone::workspace_tz(r, &i.workspace_id))
                    .ok()
            })
            .and_then(|z| steve_runtime::tz::Zone::parse(z.name()).ok())
            .unwrap_or_else(steve_runtime::tz::Zone::system)
    }

    /// The turn of `sid` failed on a quota that is back at `until`: the item waits for it, no
    /// continue spent, no card.
    pub(crate) fn quota_wait(
        &mut self,
        item_id: &str,
        sid: &str,
        until: i64,
        error: &str,
    ) -> Result<()> {
        self.st.quota_waits.insert(
            item_id.into(),
            QuotaWait {
                session: sid.into(),
                until,
            },
        );
        let line = format!(
            "模型配额用完，{} 后 Steve 让它继续：{}",
            when(until, crate::util::now(), self.quota_zone(item_id)),
            js_head(error, 160)
        );
        self.db()
            .timeline_add(item_id, "steve", "sys", &line, Some(sid))?;
        Ok(())
    }

    pub(crate) fn quota_waiting(&self, item_id: &str) -> bool {
        self.st
            .quota_waits
            .get(item_id)
            .is_some_and(|wait| wait.until > crate::util::now())
    }

    /// A turn of `sid` started (you wrote to it, say): it no longer waits for the quota.
    pub(crate) fn quota_turn_started(&mut self, sid: &str) {
        self.st.quota_waits.retain(|_, w| w.session != sid);
    }

    /// Continues every session whose quota is back by `at`, still idle on an item still at work.
    pub(crate) fn quota_wake(&mut self, at: i64) -> Result<Vec<String>> {
        let due: Vec<(String, QuotaWait)> = self
            .st
            .quota_waits
            .iter()
            .filter(|(_, w)| w.until <= at)
            .map(|(i, w)| (i.clone(), w.clone()))
            .collect();
        let mut out = vec![];
        for (item, w) in due {
            self.st.quota_waits.remove(&item);
            let Some(i) = self.db().item(&item)? else {
                continue;
            };
            let idle = self
                .db()
                .session(&w.session)?
                .is_some_and(|s| s.status != "ended")
                && !self.busy(&w.session)
                && self.db().queued(&w.session)?.is_empty();
            if i.phase != crate::cycle::ACTIVE
                || self.waits_on_user(Some(&i.id))?
                || i.archived
                || !idle
            {
                continue;
            }
            self.db().timeline_add(
                &item,
                "steve",
                "sys",
                "模型配额已恢复，Steve 让它继续",
                Some(&w.session),
            )?;
            self.prompt(
                &w.session,
                "（Steve）上一回合因为模型配额用完中断了，现在配额已恢复。先核对现状再动手：已经完成的推送、合并、打 tag、发布不要重做；然后接着做，做完用 steve_deliver 交付，需要用户决定时用 steve_ask。",
                "steve",
            );
            out.push(item);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_dst_table() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
        ] {
            let z = steve_runtime::tz::Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let ms = z.instant(wall).unwrap();
            assert_eq!(
                when(ms, ms, z),
                z.local(ms).unwrap().format("%H:%M").to_string()
            );
        }
    }

    use super::*;

    #[test]
    fn a_quota_that_says_when_it_is_back() {
        // 2026-11-11 15:13 UTC
        let now = Utc
            .with_ymd_and_hms(2026, 11, 11, 15, 13, 0)
            .unwrap()
            .timestamp_millis();
        let at_0040 = Utc
            .with_ymd_and_hms(2026, 11, 12, 0, 40, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            reset_at(
                "API Error: 429 · upstream account quota exhausted; available again at 00:40 UTC (in 9h27m)",
                now
            ),
            Some(at_0040)
        );
        assert_eq!(
            reset_at("429 Too Many Requests, try again in 9h27m", now),
            Some(now + (9 * 60 + 27) * 60_000)
        );
        assert_eq!(
            reset_at("rate limit reached, retry-after: 30", now),
            Some(now + 30_000)
        );
        assert_eq!(
            reset_at("usage limit reached; resets in 5 minutes", now),
            Some(now + 5 * 60_000)
        );
        assert_eq!(
            reset_at("429 quota exceeded until 2026-11-12T00:40:00Z", now),
            Some(at_0040)
        );
        // no time, not a quota, or not believable: an ordinary stop
        assert_eq!(reset_at("429 Too Many Requests", now), None);
        assert_eq!(reset_at("503 Service Unavailable in 5m", now), None);
        assert_eq!(reset_at("429 quota, back in 400h", now), None);
        assert_eq!(
            reset_at("429 quota exhausted until 2020-01-01T00:00:00Z", now),
            None
        );
    }
}

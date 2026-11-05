//! Missed-run and overlap rules for time triggers, as pure functions.
//!
//! The scheduler (S7) claims a trigger, asks [`plan_claim`] what to do with it, and writes the
//! `flow_runs` row and the new `next_at` in the same transaction.

use chrono::DateTime;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::cron::CronExpr;

/// A claim later than this after its planned time counts as missed.
pub const MISSED_GRACE_MS: i64 = 60_000;

/// Missed runs are counted up to this many.
pub const MISSED_COUNT_CAP: u32 = 1000;

/// `outcome_label` of a run skipped because the previous one is still going.
pub const OVERLAP_SKIP_LABEL: &str = "上一次还没结束，跳过";

/// `schedule.missed` / `flow_triggers.missed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    #[default]
    CatchUpOnce,
    Skip,
}

impl MissedPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            MissedPolicy::CatchUpOnce => "catch_up_once",
            MissedPolicy::Skip => "skip",
        }
    }

    /// Lenient read of the stored column: unknown or missing → default.
    pub fn from_db(s: Option<&str>) -> Self {
        match s {
            Some("skip") => MissedPolicy::Skip,
            _ => MissedPolicy::CatchUpOnce,
        }
    }
}

/// `schedule.overlap`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    #[default]
    Skip,
    Run,
}

impl OverlapPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            OverlapPolicy::Skip => "skip",
            OverlapPolicy::Run => "run",
        }
    }

    /// Lenient read of the stored value: unknown or missing → default.
    pub fn from_db(s: Option<&str>) -> Self {
        match s {
            Some("run") => OverlapPolicy::Run,
            _ => OverlapPolicy::Skip,
        }
    }
}

/// `flow_runs.trigger_kind` for schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Time,
    Manual,
    CatchUp,
}

impl TriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Time => "time",
            TriggerKind::Manual => "manual",
            TriggerKind::CatchUp => "catch_up",
        }
    }
}

/// What a claimed trigger turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimAction {
    /// On time: a normal run.
    Run,
    /// Missed, `catch_up_once`: one run standing in for all `missed` ones.
    CatchUp { late_ms: i64, missed: u32 },
    /// Missed, `skip`: a `skipped` row only.
    Skip { missed: u32 },
}

impl ClaimAction {
    /// The `trigger_kind` of the row to write.
    pub fn trigger_kind(&self) -> TriggerKind {
        match self {
            ClaimAction::Run => TriggerKind::Time,
            ClaimAction::CatchUp { .. } => TriggerKind::CatchUp,
            // A skipped missed run is still the time trigger firing late.
            ClaimAction::Skip { .. } => TriggerKind::Time,
        }
    }

    /// `true` when nothing should execute (row with `status=skipped`).
    pub fn is_skip(&self) -> bool {
        matches!(self, ClaimAction::Skip { .. })
    }

    /// For `Skip`: the whole `outcome_label`. For `CatchUp`: the prefix to put before the
    /// action's own label. `None` for on-time runs.
    pub fn label(&self) -> Option<String> {
        match self {
            ClaimAction::Run => None,
            ClaimAction::CatchUp { missed, .. } => Some(catch_up_label_prefix(*missed)),
            ClaimAction::Skip { missed } => {
                Some(format!("Hub 没在运行，错过 {missed} 次，按设置跳过"))
            }
        }
    }
}

/// `补跑（错过 n 次）`, the prefix of a catch-up run's result label.
pub fn catch_up_label_prefix(missed: u32) -> String {
    format!("补跑（错过 {missed} 次）")
}

/// The decision for one claimed trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Planned time this run stands for (`context.at`): `next_at`, i.e. the earliest missed run.
    pub at_ms: i64,
    /// New `flow_triggers.next_at`: the first run after `now`. `None` = the cron never runs again.
    pub next_at_ms: Option<i64>,
    pub action: ClaimAction,
}

/// Decides what a trigger due at `next_at_ms` does when claimed at `now_ms`.
///
/// Missed = claimed more than [`MISSED_GRACE_MS`] late. Both missed policies move `next_at`
/// past `now`, so a long outage yields exactly one row.
pub fn plan_claim(
    cron: &CronExpr,
    tz: Tz,
    next_at_ms: i64,
    now_ms: i64,
    policy: MissedPolicy,
) -> Claim {
    // Claims may run up to 1 s early; never schedule the same planned time twice.
    let next_at_ms_new = cron.next_after_ms(tz, now_ms.max(next_at_ms));
    let late_ms = now_ms - next_at_ms;
    let action = if late_ms <= MISSED_GRACE_MS {
        ClaimAction::Run
    } else {
        let missed = count_missed(cron, tz, next_at_ms, now_ms);
        match policy {
            MissedPolicy::CatchUpOnce => ClaimAction::CatchUp { late_ms, missed },
            MissedPolicy::Skip => ClaimAction::Skip { missed },
        }
    };
    Claim {
        at_ms: next_at_ms,
        next_at_ms: next_at_ms_new,
        action,
    }
}

/// Planned runs in `[next_at, now]`, at least 1 (the claimed one), at most [`MISSED_COUNT_CAP`].
fn count_missed(cron: &CronExpr, tz: Tz, next_at_ms: i64, now_ms: i64) -> u32 {
    let (Some(from), Some(to)) = (
        DateTime::from_timestamp_millis(next_at_ms),
        DateTime::from_timestamp_millis(now_ms),
    ) else {
        return 1;
    };
    cron.count_between(tz, from, to, MISSED_COUNT_CAP).max(1)
}

/// `next_at` when a trigger is (re-)enabled or its cron/tz changed: counted from now, so time
/// spent disabled is never "missed".
pub fn next_at_from_now(cron: &CronExpr, tz: Tz, now_ms: i64) -> Option<i64> {
    cron.next_after_ms(tz, now_ms)
}

/// Run statuses that count as "still going" for the overlap rule. `item*` runs are `done`
/// once their item exists, so a long item never blocks the next run.
pub fn is_active_status(status: &str) -> bool {
    matches!(status, "running" | "waiting")
}

/// Whether a new run must be written as `skipped` ([`OVERLAP_SKIP_LABEL`]), given the status of
/// the latest earlier run of the same schedule. Applies to time, catch-up and manual runs.
pub fn overlap_blocks(policy: OverlapPolicy, previous_status: Option<&str>) -> bool {
    policy == OverlapPolicy::Skip && previous_status.is_some_and(is_active_status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::parse_tz;
    use chrono::{TimeZone, Utc};

    fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn daily_0830() -> (CronExpr, Tz) {
        // 08:30 Taipei = 00:30 UTC.
        (
            CronExpr::parse("30 8 * * *").unwrap(),
            parse_tz("Asia/Taipei").unwrap(),
        )
    }

    #[test]
    fn on_time_claim_runs_and_moves_next_at() {
        let (cron, tz) = daily_0830();
        let due = ms(2026, 11, 1, 0, 30);
        for now in [due - 1000, due, due + 60_000] {
            let c = plan_claim(&cron, tz, due, now, MissedPolicy::CatchUpOnce);
            assert_eq!(c.action, ClaimAction::Run, "now={now}");
            assert_eq!(c.at_ms, due);
            assert_eq!(c.next_at_ms, Some(ms(2026, 11, 2, 0, 30)));
            assert_eq!(c.action.trigger_kind(), TriggerKind::Time);
            assert_eq!(c.action.label(), None);
        }
    }

    #[test]
    fn three_days_down_catches_up_once() {
        let (cron, tz) = daily_0830();
        let due = ms(2026, 11, 1, 0, 30);
        // Back at 08:00 Taipei on Nov 4: Nov 1, 2, 3 were missed.
        let now = ms(2026, 11, 4, 0, 0);
        let c = plan_claim(&cron, tz, due, now, MissedPolicy::CatchUpOnce);
        assert_eq!(
            c.action,
            ClaimAction::CatchUp {
                late_ms: now - due,
                missed: 3
            }
        );
        assert_eq!(c.at_ms, due);
        assert_eq!(c.next_at_ms, Some(ms(2026, 11, 4, 0, 30)));
        assert_eq!(c.action.trigger_kind(), TriggerKind::CatchUp);
        assert_eq!(c.action.label().as_deref(), Some("补跑（错过 3 次）"));
        assert!(!c.action.is_skip());
    }

    #[test]
    fn three_days_down_with_skip_writes_one_skipped_row() {
        let (cron, tz) = daily_0830();
        let c = plan_claim(
            &cron,
            tz,
            ms(2026, 11, 1, 0, 30),
            ms(2026, 11, 4, 0, 0),
            MissedPolicy::Skip,
        );
        assert_eq!(c.action, ClaimAction::Skip { missed: 3 });
        assert!(c.action.is_skip());
        assert_eq!(
            c.action.label().as_deref(),
            Some("Hub 没在运行，错过 3 次，按设置跳过")
        );
        assert_eq!(c.next_at_ms, Some(ms(2026, 11, 4, 0, 30)));
    }

    #[test]
    fn just_over_the_grace_is_missed_once() {
        let (cron, tz) = daily_0830();
        let due = ms(2026, 11, 1, 0, 30);
        let c = plan_claim(&cron, tz, due, due + 61_000, MissedPolicy::CatchUpOnce);
        assert_eq!(
            c.action,
            ClaimAction::CatchUp {
                late_ms: 61_000,
                missed: 1
            }
        );
    }

    #[test]
    fn missed_count_is_capped() {
        let cron = CronExpr::parse("*/5 * * * *").unwrap();
        let tz = parse_tz("UTC").unwrap();
        let c = plan_claim(
            &cron,
            tz,
            ms(2026, 1, 1, 0, 0),
            ms(2026, 3, 1, 0, 2),
            MissedPolicy::Skip,
        );
        assert_eq!(
            c.action,
            ClaimAction::Skip {
                missed: MISSED_COUNT_CAP
            }
        );
        assert_eq!(c.next_at_ms, Some(ms(2026, 3, 1, 0, 5)));
    }

    #[test]
    fn re_enable_counts_from_now() {
        let (cron, tz) = daily_0830();
        assert_eq!(
            next_at_from_now(&cron, tz, ms(2026, 11, 10, 3, 0)),
            Some(ms(2026, 11, 11, 0, 30))
        );
    }

    #[test]
    fn overlap_rule() {
        for s in ["running", "waiting"] {
            assert!(overlap_blocks(OverlapPolicy::Skip, Some(s)));
            assert!(!overlap_blocks(OverlapPolicy::Run, Some(s)));
        }
        for s in ["done", "failed", "skipped", "cancelled"] {
            assert!(!overlap_blocks(OverlapPolicy::Skip, Some(s)));
        }
        assert!(!overlap_blocks(OverlapPolicy::Skip, None));
        assert_eq!(OVERLAP_SKIP_LABEL, "上一次还没结束，跳过");
    }

    #[test]
    fn policies_round_trip() {
        assert_eq!(
            serde_json::to_string(&MissedPolicy::CatchUpOnce).unwrap(),
            "\"catch_up_once\""
        );
        assert_eq!(
            serde_json::from_str::<MissedPolicy>("\"skip\"").unwrap(),
            MissedPolicy::Skip
        );
        assert_eq!(
            serde_json::to_string(&OverlapPolicy::Run).unwrap(),
            "\"run\""
        );
        assert_eq!(
            serde_json::to_string(&TriggerKind::CatchUp).unwrap(),
            "\"catch_up\""
        );
        assert!(serde_json::from_str::<OverlapPolicy>("\"maybe\"").is_err());
        assert_eq!(MissedPolicy::from_db(None), MissedPolicy::CatchUpOnce);
        assert_eq!(MissedPolicy::from_db(Some("skip")).as_str(), "skip");
        assert_eq!(OverlapPolicy::from_db(Some("run")).as_str(), "run");
        assert_eq!(OverlapPolicy::from_db(Some("x")), OverlapPolicy::Skip);
        assert_eq!(MissedPolicy::default(), MissedPolicy::CatchUpOnce);
        assert_eq!(OverlapPolicy::default(), OverlapPolicy::Skip);
    }
}

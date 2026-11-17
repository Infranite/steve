//! The work policy (#327): every threshold the item redesign names, in one table. The names and
//! defaults live here and nowhere else; the database table `work_policy` holds only the operator's
//! overrides, by the same names. A name this build does not know is ignored and reported, a negative
//! value is refused, so a typo can neither switch something off nor reach code that expects a number.
//!
//! In this release only the derived state ([`crate::derive`]) and the sweep ([`crate::sweep`]) read
//! it, in shadow. The other entries are named now so later steps take their numbers from here
//! instead of a new constant.
//! Steve's continues keep their number in [`crate::config::CoreConfig::takeover_continues`] (the
//! takeover reads it there) until the takeover itself moves to this table.

use serde::Serialize;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

/// One named threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: &'static str,
    pub default: i64,
    /// `ms`, `count` or `switch` (1 on, 0 off).
    pub unit: &'static str,
    pub meaning: &'static str,
}

/// Every threshold, with its default.
pub const TABLE: &[Entry] = &[
    Entry {
        name: "close_grace",
        default: 10 * MINUTE,
        unit: "ms",
        meaning: "how long a close a model judged stays undoable before it takes effect",
    },
    Entry {
        name: "settle_debounce",
        default: 5_000,
        unit: "ms",
        meaning: "how long after a turn ends the Hub looks again before it takes the turn as settled",
    },
    Entry {
        name: "nobody_offline",
        default: 30 * MINUTE,
        unit: "ms",
        meaning: "how long the current session's Node may be away before an item nobody can bring back is yours",
    },
    Entry {
        name: "judge_failures",
        default: 3,
        unit: "count",
        meaning: "judge failures in a row after which the item is yours",
    },
    Entry {
        name: "judge_model",
        default: 1,
        unit: "switch",
        meaning: "1: the judge asks its model about what facts cannot settle; 0: it records only what facts and rules decide",
    },
    Entry {
        name: "judge_recheck",
        default: 2 * MINUTE,
        unit: "ms",
        meaning: "how soon a finished turn that was busy or unknown is looked at again for judging",
    },
    Entry {
        name: "judge_retry",
        default: MINUTE,
        unit: "ms",
        meaning: "the wait after a failed judge call; it doubles with each failure in a row",
    },
    Entry {
        name: "judge_retry_max",
        default: 10 * MINUTE,
        unit: "ms",
        meaning: "the longest wait between judge calls that failed",
    },
    Entry {
        name: "judge_timeout",
        default: 2 * MINUTE,
        unit: "ms",
        meaning: "how long one judge model call may take",
    },
    Entry {
        name: "judge_claim_stale",
        default: 10 * MINUTE,
        unit: "ms",
        meaning: "a judging pass that has not come back after this long is taken over by the next one",
    },
    Entry {
        name: "judge_text_chars",
        default: 6_000,
        unit: "count",
        meaning: "characters of the turn's final text the judge reads (its end)",
    },
    Entry {
        name: "judge_per_pass",
        default: 4,
        unit: "count",
        meaning: "judge model calls one pass makes, every 30 seconds",
    },
    Entry {
        name: "judge_looks_per_pass",
        default: 16,
        unit: "count",
        meaning: "due judge candidates one pass looks at (most only need their gate read again)",
    },
    Entry {
        name: "transient_max",
        default: 30 * MINUTE,
        unit: "ms",
        meaning: "the longest back-off between retries of a transient failure",
    },
    Entry {
        name: "quota_cards",
        default: 3,
        unit: "count",
        meaning: "quota waits after which a card asks you",
    },
    Entry {
        name: "pr_tells_max",
        default: 3,
        unit: "count",
        meaning: "times the agent is woken about its pull request before a card asks you",
    },
    Entry {
        name: "build_remind",
        default: 24 * HOUR,
        unit: "ms",
        meaning: "how long a build may run before a reminder card (it is still followed)",
    },
    Entry {
        name: "deploy_recheck",
        default: 10 * MINUTE,
        unit: "ms",
        meaning: "how often a deployment that could not be checked is looked at again",
    },
    Entry {
        name: "deploy_unknown_max",
        default: 6,
        unit: "count",
        meaning: "failed deployment checks after which a reminder card is raised",
    },
    Entry {
        name: "wait_min",
        default: 2 * MINUTE,
        unit: "ms",
        meaning: "the shortest interval of an open-ended wait",
    },
    Entry {
        name: "wait_max",
        default: HOUR,
        unit: "ms",
        meaning: "the longest interval of an open-ended wait",
    },
    Entry {
        name: "wait_note_after",
        default: 6,
        unit: "count",
        meaning: "looks at an open-ended wait after which its row says how often it waited",
    },
    Entry {
        name: "stall_min",
        default: 20 * MINUTE,
        unit: "ms",
        meaning: "a running turn without new output for this long gets one status question",
    },
    Entry {
        name: "silent_min",
        default: 2 * HOUR,
        unit: "ms",
        meaning: "an item that is not running and records nothing for this long is yours",
    },
    Entry {
        name: "review_rounds",
        default: 2,
        unit: "count",
        meaning: "rework rounds an independent review may ask for",
    },
    Entry {
        name: "session_idle_exit",
        default: 3 * MINUTE,
        unit: "ms",
        meaning: "how long a closed item's session stays before its process exits",
    },
];

/// The policy in force: the defaults with the operator's overrides.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkPolicy {
    values: Vec<(&'static str, i64)>,
    /// Overrides that were not taken (unknown name or a negative value), for the diagnostics.
    pub ignored: Vec<(String, i64)>,
}

impl Default for WorkPolicy {
    fn default() -> Self {
        Self {
            values: TABLE.iter().map(|e| (e.name, e.default)).collect(),
            ignored: vec![],
        }
    }
}

impl WorkPolicy {
    /// The defaults with `overrides` applied.
    pub fn with(overrides: &[(String, i64)]) -> Self {
        let mut p = Self::default();
        for (name, value) in overrides {
            match p.values.iter_mut().find(|(n, _)| n == name) {
                Some(slot) if *value >= 0 => slot.1 = *value,
                _ => p.ignored.push((name.clone(), *value)),
            }
        }
        p
    }

    /// The value of `name`. Every name the code asks for is in [`TABLE`] (a test checks the
    /// accessors below); an unknown one is a bug, answered with 0 in release builds.
    pub fn get(&self, name: &str) -> i64 {
        let v = self
            .values
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v);
        debug_assert!(v.is_some(), "unknown work policy entry {name}");
        v.unwrap_or(0)
    }

    pub fn close_grace(&self) -> i64 {
        self.get("close_grace")
    }
    pub fn nobody_offline(&self) -> i64 {
        self.get("nobody_offline")
    }
    pub fn stall_min(&self) -> i64 {
        self.get("stall_min")
    }
    pub fn silent_min(&self) -> i64 {
        self.get("silent_min")
    }
    pub fn build_remind(&self) -> i64 {
        self.get("build_remind")
    }
    pub fn wait_note_after(&self) -> i64 {
        self.get("wait_note_after")
    }
    pub fn settle_debounce(&self) -> i64 {
        self.get("settle_debounce")
    }
    pub fn judge_failures(&self) -> i64 {
        self.get("judge_failures")
    }
    pub fn judge_model(&self) -> bool {
        self.get("judge_model") != 0
    }
    pub fn judge_recheck(&self) -> i64 {
        self.get("judge_recheck")
    }
    /// The wait before the next try after `failures` failed judge calls in a row.
    pub fn judge_retry_after(&self, failures: i64) -> i64 {
        let base = self.get("judge_retry");
        let doublings = u32::try_from(failures.saturating_sub(1).clamp(0, 30)).unwrap_or(0);
        base.saturating_mul(1_i64 << doublings)
            .min(self.get("judge_retry_max"))
    }
    pub fn judge_timeout(&self) -> i64 {
        self.get("judge_timeout")
    }
    pub fn judge_claim_stale(&self) -> i64 {
        self.get("judge_claim_stale")
    }
    pub fn judge_text_chars(&self) -> usize {
        usize::try_from(self.get("judge_text_chars")).unwrap_or(0)
    }
    pub fn judge_per_pass(&self) -> usize {
        usize::try_from(self.get("judge_per_pass")).unwrap_or(0)
    }
    pub fn judge_looks_per_pass(&self) -> usize {
        usize::try_from(self.get("judge_looks_per_pass")).unwrap_or(0)
    }

    /// Every entry with the value in force, for the debug view.
    pub fn entries(&self) -> Vec<(&'static str, i64)> {
        self.values.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_every_accessor_is_in_the_table() {
        let mut names: Vec<_> = TABLE.iter().map(|e| e.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TABLE.len());
        let p = WorkPolicy::default();
        assert_eq!(p.close_grace(), 10 * MINUTE);
        assert_eq!(p.nobody_offline(), 30 * MINUTE);
        assert_eq!(p.stall_min(), 20 * MINUTE);
        assert_eq!(p.silent_min(), 2 * HOUR);
        assert_eq!(p.build_remind(), 24 * HOUR);
        assert_eq!(p.wait_note_after(), 6);
        assert_eq!(p.settle_debounce(), 5_000);
        assert_eq!(p.judge_failures(), 3);
        assert!(p.judge_model());
        assert_eq!(p.judge_recheck(), 2 * MINUTE);
        assert_eq!(p.judge_timeout(), 2 * MINUTE);
        assert_eq!(p.judge_claim_stale(), 10 * MINUTE);
        assert_eq!(p.judge_text_chars(), 6_000);
        assert_eq!(p.judge_per_pass(), 4);
        assert_eq!(p.judge_looks_per_pass(), 16);
        // the retry waits double up to their cap
        assert_eq!(p.judge_retry_after(1), MINUTE);
        assert_eq!(p.judge_retry_after(2), 2 * MINUTE);
        assert_eq!(p.judge_retry_after(3), 4 * MINUTE);
        assert_eq!(p.judge_retry_after(9), 10 * MINUTE);
    }

    #[test]
    fn overrides_apply_by_name_and_bad_ones_are_reported() {
        let p = WorkPolicy::with(&[
            ("close_grace".into(), 1_000),
            ("no_such".into(), 5),
            ("wait_min".into(), -1),
        ]);
        assert_eq!(p.close_grace(), 1_000);
        assert_eq!(p.get("wait_min"), 2 * MINUTE);
        assert_eq!(
            p.ignored,
            vec![("no_such".to_string(), 5), ("wait_min".to_string(), -1)]
        );
    }
}

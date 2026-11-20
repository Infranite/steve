//! In-memory not-before fences for individual rows, not cached business values.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Caller-owned row deadlines. The domain supplies absolute milliseconds and keeps its own
/// persistence/retry policy. Missing and elapsed fences are ready; nothing is loaded or refreshed.
/// Dropping the ledger (including on restart) removes its fences. This is not a scheduler:
/// it owns neither a clock nor a wake signal. A caller that makes work due earlier outside
/// its current pass must wake its source separately.
#[derive(Default)]
pub struct RowDeadlines {
    rows: Mutex<HashMap<String, i64>>,
}

impl RowDeadlines {
    fn rows(&self) -> std::sync::MutexGuard<'_, HashMap<String, i64>> {
        self.rows.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set this row's not-before time. Replacing a fence is an explicit domain decision.
    pub fn defer(&self, key: impl Into<String>, at: i64) {
        self.rows().insert(key.into(), at);
    }

    pub fn not_before(&self, key: &str) -> Option<i64> {
        self.rows().get(key).copied()
    }

    pub fn ready(&self, key: &str, now: i64) -> bool {
        self.not_before(key).is_none_or(|at| at <= now)
    }

    /// Forget rows no longer present in the source's authoritative live set.
    pub fn retain_live<'a>(&self, keys: impl IntoIterator<Item = &'a str>) {
        let live: HashSet<_> = keys.into_iter().collect();
        self.rows().retain(|key, _| live.contains(key.as_str()));
    }

    /// Snapshot outstanding fences for a batch query. Elapsed fences no longer constrain work
    /// and are discarded; there is no TTL, value cache, loader or implicit retry policy.
    pub fn pending(&self, now: i64) -> HashMap<String, i64> {
        let mut rows = self.rows();
        rows.retain(|_, at| *at > now);
        rows.clone()
    }

    /// Explicit recovery may lift every in-memory fence at once.
    pub fn clear(&self) {
        self.rows().clear();
    }
}

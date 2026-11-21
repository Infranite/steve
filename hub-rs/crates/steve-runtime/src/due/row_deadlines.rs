//! In-memory not-before fences for individual rows, not cached business values.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Caller-owned row deadlines. The domain supplies absolute milliseconds and keeps its own
/// persistence/retry policy. Only future fences delay work; nothing is loaded or refreshed.
/// Dropping the ledger (including on restart) removes its fences. This is not a scheduler:
/// it owns neither a clock nor a wake signal. A caller that makes work due earlier outside
/// its current pass must wake its source separately.
///
/// Callers implementing [`super::DueSource`] must publish the larger of a row's stored due time
/// and its `not_before`/`pending` fence from [`super::DueSource::next_due`], and use that same
/// effective deadline in [`super::DueSource::run_due`]. Filtering only in `run_due` leaves
/// `next_due` reporting an already-due storage value, so `due_loop` repeatedly invokes work
/// before the fence has elapsed instead of sleeping until it.
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

    /// Read the recorded fence without consuming it. It may already be in the past and remains
    /// visible until [`Self::pending`], [`Self::retain_live`] or [`Self::clear`] removes it.
    /// Choose this view or `pending` for a source; do not mix them: `pending` discards elapsed
    /// entries, so a later `not_before` may return `None` for the same key, including after a
    /// wall-clock rewind. This method alone never expires a record.
    pub fn not_before(&self, key: &str) -> Option<i64> {
        self.rows().get(key).copied()
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

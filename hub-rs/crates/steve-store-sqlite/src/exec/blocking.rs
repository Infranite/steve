//! Explicit host contract, not a probe of Tokio's private scheduler state.
use std::{
    cell::Cell,
    collections::HashSet,
    panic::Location,
    sync::{Mutex, OnceLock},
};

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Only recognized blocking adapters enter this scope, after leaving async polling.
/// Nested scopes restore the previous depth on both return and panic.
pub fn blocking_scope<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            DEPTH.with(|v| v.set(self.0));
        }
    }
    let old = DEPTH.with(|v| v.replace(v.get().checked_add(1).expect("blocking scope depth")));
    let _restore = Restore(old);
    run()
}

pub fn in_blocking_scope() -> bool {
    DEPTH.with(|v| v.get() != 0)
}

/// A bare spawn_blocking also violates the contract: use a recognized adapter.
pub fn outside_blocking_scope() -> bool {
    tokio::runtime::Handle::try_current().is_ok() && !in_blocking_scope()
}

pub(super) fn record_violation(site: &'static Location<'static>) {
    metrics::counter!("storage_grant_outside_blocking_scope", "site" => site.to_string())
        .increment(1);
    // Keep the old diagnostic series until its consumers move to the contract metric.
    metrics::counter!("steve_store_tokio_grant_total", "site" => site.to_string()).increment(1);
    static WARNED: OnceLock<Mutex<HashSet<&'static Location<'static>>>> = OnceLock::new();
    let first = WARNED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(site);
    if first {
        tracing::warn!(%site, "storage grant outside an explicit blocking scope");
    }
    // G2-T11 rejection is enabled only after the Node callers migrate in 6b3.
}

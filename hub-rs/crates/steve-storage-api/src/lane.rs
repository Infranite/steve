//! Scheduling context without a runtime dependency.
//! The default source is thread-local and defaults to Interactive. The Hub installs
//! a plain function pointer that reads its Tokio task-local value, falling back to
//! thread_lane(). Never hold the source lock while invoking it. When entering a blocking
//! thread, capture current() and install it there with with_lane(); TLS is not task-local.
use serde::{Deserialize, Serialize};
use std::{cell::Cell, sync::RwLock};

/// Fair-scheduling lane; affects attempts, never request identity.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    #[default]
    Interactive,
    Admin,
    Ingest,
    Background,
}
thread_local! { static DEFAULT: Cell<Lane> = const { Cell::new(Lane::Interactive) }; }
static SOURCE: RwLock<fn() -> Lane> = RwLock::new(thread_lane);

/// Read the fallback value; safe for a runtime source to call without recursion.
pub fn thread_lane() -> Lane {
    DEFAULT.with(Cell::get)
}
/// Read the runtime-selected source.
pub fn current() -> Lane {
    let source = *SOURCE.read().unwrap_or_else(|e| e.into_inner());
    source()
}
/// Install a runtime adapter during startup; returns the previous source for restoration.
pub fn replace_source(source: fn() -> Lane) -> fn() -> Lane {
    std::mem::replace(
        &mut *SOURCE.write().unwrap_or_else(|e| e.into_inner()),
        source,
    )
}
/// Run a synchronous closure with a thread-local lane, restoring it even during unwinding.
/// Do not hold this scope over an await; async callers must use their runtime task-local.
pub fn with_lane<T>(lane: Lane, f: impl FnOnce() -> T) -> T {
    struct Restore(Lane);
    impl Drop for Restore {
        fn drop(&mut self) {
            DEFAULT.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(DEFAULT.with(|v| v.replace(lane)));
    f()
}

//! Runtime lane context. Carries scheduling metadata only, never writer ownership.
pub use super::Lane;
use std::{cell::Cell, future::Future, panic::Location, sync::Once};

tokio::task_local! { static TASK: Lane; }
thread_local! { static THREAD: Cell<Option<Option<Lane>>> = const { Cell::new(None) }; }

fn selected() -> Option<Lane> {
    THREAD.with(|slot| {
        slot.get().unwrap_or_else(|| {
            TASK.try_with(|lane| *lane).ok().or_else(|| {
                // Preserve existing synchronous hosts using the neutral contract's TLS.
                let lane = steve_storage_api::lane::thread_lane();
                (lane != Lane::Interactive).then_some(lane)
            })
        })
    })
}

fn source() -> Lane {
    selected().unwrap_or_else(steve_storage_api::lane::thread_lane)
}

fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        steve_storage_api::lane::replace_source(source);
    });
}

/// Resolve at the storage/carry boundary. Missing context remains visible per caller.
#[track_caller]
pub fn current() -> Lane {
    match selected() {
        Some(lane) => lane,
        None => {
            metrics::counter!("steve_store_unset_lane_total", "site" => Location::caller().to_string()).increment(1);
            steve_storage_api::lane::thread_lane()
        }
    }
}

/// Capture absence as well as a lane: an unclassified caller must not become classified
/// merely because it crossed a blocking boundary.
pub fn capture() -> Option<Lane> {
    selected()
}

/// Restore the worker context on return and unwind. Never keep this scope over await.
pub fn with_context<T>(lane: Option<Lane>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Option<Lane>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            THREAD.with(|slot| slot.set(self.0));
        }
    }
    install();
    let _restore = Restore(THREAD.with(|slot| slot.replace(Some(lane))));
    steve_storage_api::lane::with_lane(lane.unwrap_or_default(), f)
}

pub fn with_lane<T>(lane: Lane, f: impl FnOnce() -> T) -> T {
    with_context(Some(lane), f)
}

/// Runtime scope surrounds the future being polled, including its suspension points.
pub async fn scope<F: Future>(lane: Lane, future: F) -> F::Output {
    install();
    TASK.scope(lane, future).await
}

/// One shared background wrapper for Hub jobs and domain-owned task factories.
pub async fn background<F: Future>(future: F) -> F::Output {
    scope(Lane::Background, async move {
        debug_assert_eq!(current(), Lane::Background);
        future.await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn neutral_contract_reads_the_runtime_scope_and_keeps_thread_fallback() {
        scope(Lane::Admin, async {
            tokio::task::yield_now().await;
            assert_eq!(steve_storage_api::lane::current(), Lane::Admin);
            with_lane(Lane::Ingest, || {
                assert_eq!(steve_storage_api::lane::current(), Lane::Ingest);
            });
            assert_eq!(steve_storage_api::lane::current(), Lane::Admin);
        })
        .await;
        steve_storage_api::lane::with_lane(Lane::Background, || {
            assert_eq!(steve_storage_api::lane::current(), Lane::Background);
            assert_eq!(capture(), Some(Lane::Background));
        });
        assert_eq!(steve_storage_api::lane::current(), Lane::Interactive);
    }
}

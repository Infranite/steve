#[path = "exec_support.rs"]
mod support;
use steve_store_sqlite::exec::{self, Lane};
use support::*;

#[test]
fn blocking_scope_is_nested_and_restored_after_panic() {
    assert!(!exec::in_blocking_scope());
    exec::blocking_scope(|| {
        assert!(exec::in_blocking_scope());
        let panic = std::panic::catch_unwind(|| {
            exec::blocking_scope(|| panic!("nested scope"));
        });
        assert!(panic.is_err());
        assert!(exec::in_blocking_scope());
    });
    assert!(!exec::in_blocking_scope());
}

#[test]
fn explicit_scope_contract_covers_all_runtime_contexts() {
    assert!(!exec::outside_blocking_scope()); // no runtime
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).build().unwrap();
    rt.block_on(async {
        assert!(tokio::spawn(async { exec::outside_blocking_scope() }).await.unwrap());
        assert!(tokio::task::spawn_blocking(exec::outside_blocking_scope).await.unwrap());
        assert!(!tokio::task::spawn_blocking(|| exec::blocking_scope(exec::outside_blocking_scope)).await.unwrap());
        assert!(!tokio::spawn(async {
            tokio::task::block_in_place(|| exec::blocking_scope(exec::outside_blocking_scope))
        }).await.unwrap());
    });
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    rt.block_on(async {
        assert!(tokio::spawn(async { exec::outside_blocking_scope() }).await.unwrap());
    });
}

#[tokio::test]
async fn grants_count_each_violation_by_site_without_rejecting() {
    let (e, _, _) = executor_with_probe(exec::outside_blocking_scope);
    let recorder = Probe::default();
    metrics::with_local_recorder(&recorder, || {
        for _ in 0..3 {
            // Exactly one source site, counted for every call.
            drop(e.grant(Lane::Interactive).unwrap());
        }
        exec::blocking_scope(|| drop(e.grant(Lane::Admin).unwrap()));
    });
    assert_eq!(recorder.total("storage_grant_outside_blocking_scope"), 3.);
}

#[derive(Clone, Default)]
struct Warnings(std::sync::Arc<std::sync::atomic::AtomicU64>);
impl tracing::Subscriber for Warnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool { true }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id { tracing::span::Id::from_u64(1) }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
#[tokio::test]
async fn outside_scope_warns_once_per_site_across_executors_and_elapsed_minutes() {
    let warnings = Warnings::default();
    let count = warnings.0.clone();
    let (a, time, _) = executor_with_probe(exec::outside_blocking_scope);
    let (b, _, _) = executor_with_probe(exec::outside_blocking_scope);
    fn same_site(e: &Exec) { drop(e.grant(Lane::Admin).unwrap()); }
    tracing::subscriber::with_default(warnings, || {
        same_site(&a);
        time.advance(120_000);
        same_site(&a);
        same_site(&b);
    });
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

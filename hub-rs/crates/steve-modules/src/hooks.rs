//! Module observers for committed item creation, completion and cancellation.

use async_trait::async_trait;
use steve_store::Item;

/// One module's reactions. Every method defaults to doing nothing. Hooks run after the item's own write
/// has been committed and must not fail the lifecycle step: they log their errors themselves. Long work
/// (network, model calls) belongs in a spawned task or a queue, not in the hook.
#[async_trait]
pub trait ItemHooks: Send + Sync {
    /// Short id for logs (`"github"`, `"knowledge"`).
    fn name(&self) -> &'static str;

    async fn on_created(&self, item: &Item) {
        let _ = item;
    }

    async fn on_finished(&self, item: &Item) {
        let _ = item;
    }

    async fn on_dropped(&self, item: &Item) {
        let _ = item;
    }
}

/// Runs one observer: a panic is logged and stops only that hook's call, never the others or the
/// lifecycle step it observes.
async fn observe(hook: &str, point: &str, f: impl std::future::Future<Output = ()>) {
    let mut f = std::pin::pin!(f);
    let r = std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.as_mut().poll(cx))) {
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Ok(std::task::Poll::Ready(())) => std::task::Poll::Ready(Ok(())),
            Err(_) => std::task::Poll::Ready(Err(())),
        }
    })
    .await;
    if r.is_err() {
        tracing::error!(hook, point, "item hook panicked");
    }
}

/// The registered hooks, called in registration order.
#[derive(Default)]
pub struct ItemHookList(Vec<Box<dyn ItemHooks>>);

impl ItemHookList {
    pub fn push(&mut self, h: Box<dyn ItemHooks>) {
        assert!(
            self.0.iter().all(|x| x.name() != h.name()),
            "item hooks {} registered twice",
            h.name()
        );
        self.0.push(h);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub async fn created(&self, item: &Item) {
        for h in &self.0 {
            observe(h.name(), "created", h.on_created(item)).await;
        }
    }

    pub async fn finished(&self, item: &Item) {
        for h in &self.0 {
            observe(h.name(), "finished", h.on_finished(item)).await;
        }
    }

    pub async fn dropped(&self, item: &Item) {
        for h in &self.0 {
            observe(h.name(), "dropped", h.on_dropped(item)).await;
        }
    }
}

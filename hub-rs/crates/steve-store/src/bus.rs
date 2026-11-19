//! Typed publications in commit order. The bus assigns revisions to committed batches.
use std::sync::Arc;
use steve_types::BusEvent;
pub trait Publisher: Send + Sync {
    fn publish_batch(&self, events: Vec<BusEvent>);
    fn publish(&self, event: BusEvent) {
        self.publish_batch(vec![event]);
    }
}
impl Publisher for tokio::sync::broadcast::Sender<BusEvent> {
    fn publish_batch(&self, events: Vec<BusEvent>) {
        for event in events {
            let _ = self.send(event);
        }
    }
}
impl Publisher for tokio::sync::broadcast::Sender<Arc<BusEvent>> {
    fn publish_batch(&self, events: Vec<BusEvent>) {
        for event in events {
            let _ = self.send(Arc::new(event));
        }
    }
}
impl<P: Publisher + ?Sized> Publisher for Arc<P> {
    fn publish_batch(&self, events: Vec<BusEvent>) {
        (**self).publish_batch(events);
    }
}
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPublisher;
impl Publisher for NoPublisher {
    fn publish_batch(&self, _: Vec<BusEvent>) {}
}
/// A typed event collector. Batch order is preserved for every callback.
pub struct FnPublisher<F>(pub F);
impl<F: Fn(BusEvent) + Send + Sync> Publisher for FnPublisher<F> {
    fn publish_batch(&self, events: Vec<BusEvent>) {
        for event in events {
            (self.0)(event);
        }
    }
}

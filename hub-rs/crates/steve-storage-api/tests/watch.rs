use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Wake, Waker},
};
use steve_storage_api::{CommitWatch, CommitWatches};
struct Count(AtomicUsize);
impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn pending_waiter_wakes_and_a_commit_before_wait_keeps_its_permit() {
    let bus = CommitWatches::default();
    let watch = bus.subscribe(&["rows".into()]);
    let count = Arc::new(Count(AtomicUsize::new(0)));
    let waker = Waker::from(count.clone());
    let mut cx = Context::from_waker(&waker);
    {
        let mut changed = std::pin::pin!(watch.changed());
        assert!(changed.as_mut().poll(&mut cx).is_pending());
        bus.notify(&["other".into()]);
        assert_eq!(count.0.load(Ordering::SeqCst), 0);
        bus.notify(&["rows".into()]);
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert!(changed.as_mut().poll(&mut cx).is_ready());
    }
    bus.notify(&["rows".into()]);
    bus.notify(&["rows".into()]);
    {
        let mut changed = std::pin::pin!(watch.changed());
        assert!(changed.as_mut().poll(&mut cx).is_ready());
    }
    {
        let mut changed = std::pin::pin!(watch.changed());
        assert!(changed.as_mut().poll(&mut cx).is_pending());
    }
    drop(watch);
    assert!(!bus.has_subscribers());
}
#[test]
fn one_subscription_can_receive_from_multiple_backend_partitions() {
    let a = CommitWatches::default();
    let b = CommitWatches::default();
    let watch = CommitWatch::new();
    a.subscribe_into(&["a".into()], &watch);
    b.subscribe_into(&["b".into()], &watch);
    let waker = Waker::from(Arc::new(Count(AtomicUsize::new(0))));
    let mut cx = Context::from_waker(&waker);
    for (bus, name) in [(&a, "a"), (&b, "b")] {
        bus.notify(&[name.into()]);
        let mut changed = std::pin::pin!(watch.changed());
        assert!(changed.as_mut().poll(&mut cx).is_ready());
    }
}

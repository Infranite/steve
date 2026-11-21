//! Reusable storage conformance suite and reference backends (feature `conformance`).
//! Harnesses provide isolated namespaces and explicitly report unavailable capabilities.
pub mod digest;
pub mod entities;
pub mod loopback;
pub mod mem;
mod scenarios;
mod suite;
pub use suite::*;

use std::{
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
};

/// Small standard-library executor for tests; honors wakeups without polling or a runtime.
pub fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWake(thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}

/// Terminate and reap a harness-owned storage child. On supported Unix installations,
/// Child::kill is SIGKILL; callers must synchronize on an in-flight commit first.
/// Reaping the child is part of the contract check, so no zombie is left behind.
pub fn kill_and_reap(child: &mut std::process::Child) -> std::io::Result<std::process::ExitStatus> {
    child.kill()?;
    child.wait()
}

//! Synchronous, scoped calls into the connection owner. Keeping the scope here lets transaction
//! callers borrow their command/state, without copying an actor or making its logic asynchronous.

use super::{Writer, internal};
use std::sync::mpsc::{self, Sender, SyncSender};
use steve_node_api::ApiResult;

type Call<'a> = Box<dyn FnOnce(&mut Writer) + Send + 'a>;

struct Completion(SyncSender<()>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

pub(super) struct Job {
    // Fields drop in declaration order: release *all* borrowed captures before signalling done,
    // including when an unexecuted job is dropped with a disconnected receiver.
    call: Option<Call<'static>>,
    _done: Completion,
}

impl Job {
    pub fn run(mut self, writer: &mut Writer) {
        self.call.take().expect("one call per job")(writer);
    }
}

pub(super) fn call<R: Send>(
    sender: &Sender<Job>,
    f: impl FnOnce(&mut Writer) -> ApiResult<R> + Send,
) -> ApiResult<ApiResult<R>> {
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let call: Call<'_> = Box::new(move |writer| {
        let _ = result_tx.send(f(writer));
    });
    // SAFETY: this is a synchronous scope. Once sent, we unconditionally wait for Completion
    // before returning (even on a worker panic or channel disconnection). Completion is dropped
    // only AFTER the call and its borrowed captures, whether run or abandoned. No handle to the
    // job escapes; neither cancellation of a Tokio future nor dropping Journal can skip this wait.
    let call: Call<'static> = unsafe { std::mem::transmute(call) };
    let job = Job {
        call: Some(call),
        _done: Completion(done_tx),
    };
    if let Err(abandoned) = sender.send(job) {
        drop(abandoned);
    }
    let _ = done_rx.recv();
    result_rx
        .recv()
        .map_err(|_| internal("writer stopped before replying"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn abandoned_job_releases_borrowed_captures_before_unblocking_the_caller() {
        struct Borrow<'a>(&'a AtomicBool);
        impl Drop for Borrow<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let dropped = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let borrowed = Borrow(&dropped);
            scope.spawn(|| {
                let result = call(&tx, move |_| {
                    let _borrowed = borrowed;
                    Ok(())
                });
                assert!(result.is_err());
                assert!(dropped.load(Ordering::Acquire));
            });
            // A stopped owner drops the queued, unexecuted job. Its callback's destructor may
            // still touch the caller's stack: the completion guard must run strictly after it.
            drop(rx.recv().unwrap());
        });
    }
}

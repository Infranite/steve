use super::*;
use crate::{engine::Engine, open};
use std::sync::mpsc;

struct Noop;
impl<C> BatchRunner<C> for Noop {
    type Job = ();
    type Output = ();
    fn run(&self, _: &C, jobs: Vec<()>) -> Vec<Result<(), CommitError>> {
        jobs.into_iter().map(|_| Ok(())).collect()
    }
}

#[test]
fn closing_releases_storage_before_fifty_immediate_reopens() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..50 {
        let engine = Arc::new(open::open_runtime(dir.path()).unwrap());
        let weak = Arc::downgrade(&engine);
        let exec = Executor::new(
            "reopen",
            engine,
            Arc::new(SystemClock::default()),
            Noop,
            || false,
        )
        .unwrap();
        drop(exec);
        assert!(weak.upgrade().is_none());
    }
}

struct Maintenance {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl BatchRunner<Arc<Engine>> for Maintenance {
    type Job = ();
    type Output = ();
    fn run(&self, _: &Arc<Engine>, _: Vec<()>) -> Vec<Result<(), CommitError>> {
        unreachable!()
    }
    fn maintenance(&self, _: &Arc<Engine>, _: Duration, _: bool) -> Option<Duration> {
        self.entered.send(()).unwrap();
        lock(&self.release).recv().unwrap();
        None
    }
}

#[test]
fn closing_joins_in_flight_maintenance_and_drops_its_connection() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(open::open_runtime(dir.path()).unwrap());
    let weak = Arc::downgrade(&engine);
    let (entered, started) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let exec = Executor::new(
        "maintenance",
        engine,
        Arc::new(SystemClock::default()),
        Maintenance {
            entered,
            release: Mutex::new(resume),
        },
        || false,
    )
    .unwrap();
    started.recv_timeout(Duration::from_secs(10)).unwrap();
    let shared = exec.shared().clone();
    let (closed, done) = mpsc::channel();
    let closer = std::thread::spawn(move || {
        drop(exec);
        closed.send(()).unwrap();
    });
    let mut state = lock(&shared.state);
    while !state.closing {
        state = shared.changed.wait(state).unwrap();
    }
    drop(state);
    assert!(matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
    release.send(()).unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap();
    closer.join().unwrap();
    assert!(
        weak.upgrade().is_none(),
        "worker releases the connection even if Shared is retained"
    );
    drop(open::open_runtime(dir.path()).unwrap());
}

struct Closed(mpsc::Sender<()>);
impl Drop for Closed {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
struct Owned {
    // Field order makes the close acknowledgement follow Engine/storage.lock release.
    _engine: Arc<Engine>,
    _closed: Closed,
}

#[test]
fn last_handle_dropped_by_worker_detaches_without_self_join() {
    let dir = tempfile::tempdir().unwrap();
    let (closed, done) = mpsc::channel();
    let engine = Arc::new(open::open_runtime(dir.path()).unwrap());
    let owned = Owned {
        _engine: engine,
        _closed: Closed(closed),
    };
    let exec = Executor::new(
        "self-drop",
        owned,
        Arc::new(SystemClock::default()),
        Noop,
        || false,
    )
    .unwrap();
    let holder = Arc::new(Mutex::new(None));
    let held = holder.clone();
    let (ready, resume) = mpsc::channel();
    let completion = exec
        .slice(move |_| {
            resume.recv().unwrap();
            drop(lock(&held).take());
            Step::Done
        })
        .unwrap();
    *lock(&holder) = Some(exec);
    ready.send(()).unwrap();
    completion.blocking_recv().unwrap().unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(open::open_runtime(dir.path()).unwrap());
}

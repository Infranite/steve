#![allow(dead_code)]
use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder,
    SharedString, Unit,
};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;
use steve_storage_api::uow::CommitError;
use steve_store_sqlite::exec::{BatchRunner, Clock, Executor, Lane};

#[derive(Default)]
pub struct Time(AtomicU64);
impl Time {
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}
impl Clock for Time {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}
pub struct Job {
    pub id: usize,
    pub fail: bool,
    pub ms: u64,
}
pub fn job(id: usize) -> Job {
    Job {
        id,
        fail: false,
        ms: 1,
    }
}
#[derive(Clone)]
pub struct Runner {
    pub clock: Arc<Time>,
    pub batches: Arc<Mutex<Vec<Vec<usize>>>>,
}
impl BatchRunner<Cell<usize>> for Runner {
    type Job = Job;
    type Output = usize;
    fn run(&self, conn: &Cell<usize>, jobs: Vec<Job>) -> Vec<Result<usize, CommitError>> {
        self.batches
            .lock()
            .unwrap()
            .push(jobs.iter().map(|j| j.id).collect());
        jobs.into_iter()
            .map(|j| {
                conn.set(conn.get() + 1);
                self.clock.advance(j.ms);
                if j.fail {
                    Err(CommitError::Retryable {
                        after_ms: None,
                        detail: "one isolated failure".into(),
                    })
                } else {
                    Ok(j.id)
                }
            })
            .collect()
    }
}
pub type Exec = Executor<Cell<usize>, Runner>;
pub fn executor() -> (Exec, Arc<Time>, Runner) {
    executor_with_probe(|| false)
}
pub fn executor_with_probe(probe: fn() -> bool) -> (Exec, Arc<Time>, Runner) {
    let clock = Arc::new(Time::default());
    let runner = Runner {
        clock: clock.clone(),
        batches: Arc::default(),
    };
    let e = Executor::new(
        "test-writer",
        Cell::new(0),
        clock.clone(),
        runner.clone(),
        probe,
    )
    .unwrap();
    (e, clock, runner)
}

type Readings = (Mutex<Vec<(Key, f64)>>, Condvar);
#[derive(Clone, Default)]
pub struct Probe(Arc<Readings>);
impl Probe {
    pub fn next(&self, name: &str) -> f64 {
        let (mutex, cv) = &*self.0;
        let mut events = mutex.lock().unwrap();
        loop {
            if let Some(i) = events.iter().position(|(k, _)| k.name() == name) {
                return events.remove(i).1;
            }
            events = cv.wait(events).unwrap();
        }
    }
    pub fn total(&self, name: &str) -> f64 {
        self.0
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.name() == name)
            .map(|(_, v)| v)
            .sum()
    }
}
struct Metric(Probe, Key);
impl Metric {
    fn push(&self, v: f64) {
        self.0.0.0.lock().unwrap().push((self.1.clone(), v));
        self.0.0.1.notify_all();
    }
}
impl CounterFn for Metric {
    fn increment(&self, v: u64) {
        self.push(v as f64)
    }
    fn absolute(&self, v: u64) {
        self.push(v as f64)
    }
}
impl GaugeFn for Metric {
    fn increment(&self, v: f64) {
        self.push(v)
    }
    fn decrement(&self, v: f64) {
        self.push(-v)
    }
    fn set(&self, v: f64) {
        self.push(v)
    }
}
impl HistogramFn for Metric {
    fn record(&self, v: f64) {
        self.push(v)
    }
}
impl Recorder for Probe {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn register_counter(&self, k: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(Arc::new(Metric(self.clone(), k.clone())))
    }
    fn register_gauge(&self, k: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::from_arc(Arc::new(Metric(self.clone(), k.clone())))
    }
    fn register_histogram(&self, k: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::from_arc(Arc::new(Metric(self.clone(), k.clone())))
    }
}
/// A local metrics gauge provides a real enqueue acknowledgement, not a timed guess.
pub struct Held {
    release: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Held {
    pub fn finish(mut self) {
        self.release.take().unwrap().send(()).unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(());
        }
    }
}
pub fn queued_grant(e: Exec, lane: Lane) -> (Held, mpsc::Receiver<()>) {
    let probe = Probe::default();
    let child = probe.clone();
    let (ready_tx, ready) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        metrics::with_local_recorder(&child, || {
            let g = e.grant(lane).unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(g);
        })
    });
    probe.next("steve_store_queue_depth");
    (
        Held {
            release: Some(release),
            thread: Some(thread),
        },
        ready,
    )
}
pub fn hold(e: &Exec, lane: Lane) -> Held {
    let (h, r) = queued_grant(e.clone(), lane);
    r.recv().unwrap();
    h
}

#![cfg(feature = "jobs")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use steve_runtime::{FakeClock, FirstRun, every, spawn};

async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn periods_first_run_delay_and_stop_use_the_injected_clock() {
    for (first, immediate) in [(FirstRun::Immediate, 1), (FirstRun::AfterPeriod, 0)] {
        let clock = Arc::new(FakeClock::new(0));
        let runs = Arc::new(AtomicUsize::new(0));
        let n = runs.clone();
        let task = every(
            "period",
            clock.clone(),
            Duration::from_secs(1),
            first,
            move || {
                n.fetch_add(1, Ordering::SeqCst);
                async {}
            },
        );
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), immediate);
        clock.advance(999);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), immediate);
        clock.advance(1);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), immediate + 1);
        clock.advance(10_000);
        settle().await;
        assert_eq!(
            runs.load(Ordering::SeqCst),
            immediate + 2,
            "late ticks do not burst"
        );
        clock.advance(999);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), immediate + 2);
        task.stop();
        task.await;
        clock.advance(10_000);
        settle().await;
        assert_eq!(runs.load(Ordering::SeqCst), immediate + 2);
    }
}

#[tokio::test]
async fn dropping_tasks_cancels_the_current_run_and_future_ticks() {
    let clock = Arc::new(FakeClock::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let n = runs.clone();
    let task = every(
        "drop.period",
        clock.clone(),
        Duration::from_secs(1),
        FirstRun::Immediate,
        move || {
            n.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>()
        },
    );
    settle().await;
    drop(task);
    clock.advance(100_000);
    settle().await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = spawn("drop.spawn", async move {
        let _tx = tx;
        std::future::pending::<()>().await;
    });
    settle().await;
    assert_eq!(task.name(), "drop.spawn");
    drop(task);
    assert!(
        rx.await.is_err(),
        "dropping the task drops its running future"
    );
}

#[derive(Clone, Default)]
struct Recorder {
    panics: Arc<AtomicUsize>,
    runs: Arc<AtomicUsize>,
    durations: Arc<Mutex<Vec<f64>>>,
}
struct Panics(Arc<AtomicUsize>);
impl metrics::CounterFn for Panics {
    fn increment(&self, n: u64) {
        self.0.fetch_add(n as usize, Ordering::SeqCst);
    }
    fn absolute(&self, n: u64) {
        self.0.store(n as usize, Ordering::SeqCst);
    }
}
impl metrics::CounterFn for Recorder {
    fn increment(&self, n: u64) {
        self.runs.fetch_add(n as usize, Ordering::SeqCst);
    }
    fn absolute(&self, n: u64) {
        self.runs.store(n as usize, Ordering::SeqCst);
    }
}
impl metrics::HistogramFn for Recorder {
    fn record(&self, n: f64) {
        self.durations.lock().unwrap().push(n);
    }
}
impl metrics::Recorder for Recorder {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }
    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        if key.name() == "steve_job_panics_total"
            && key
                .labels()
                .any(|l| l.key() == "job" && l.value() == "metric")
        {
            return metrics::Counter::from_arc(Arc::new(Panics(self.panics.clone())));
        }
        if key.name() == "steve_job_runs_total"
            && key
                .labels()
                .any(|l| l.key() == "job" && l.value() == "metric")
        {
            metrics::Counter::from_arc(Arc::new(self.clone()))
        } else {
            metrics::Counter::noop()
        }
    }
    fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }
    fn register_histogram(
        &self,
        key: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        if key.name() == "steve_job_duration_seconds"
            && key
                .labels()
                .any(|l| l.key() == "job" && l.value() == "metric")
        {
            metrics::Histogram::from_arc(Arc::new(self.clone()))
        } else {
            metrics::Histogram::noop()
        }
    }
}

#[tokio::test]
async fn spawn_and_each_period_record_runs_and_duration_including_cancellation() {
    let recorder = Recorder::default();
    metrics::set_global_recorder(recorder.clone()).unwrap();
    spawn("metric", async {}).await;
    assert_eq!(recorder.runs.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.durations.lock().unwrap().len(), 1);
    let clock = Arc::new(FakeClock::new(0));
    let task = every(
        "metric",
        clock.clone(),
        Duration::from_secs(1),
        FirstRun::Immediate,
        || async {},
    );
    settle().await;
    clock.advance(1_000);
    settle().await;
    task.stop();
    task.await;
    assert_eq!(recorder.runs.load(Ordering::SeqCst), 3);
    let task = spawn("metric", std::future::pending());
    settle().await;
    task.stop();
    task.await;
    assert_eq!(recorder.runs.load(Ordering::SeqCst), 4);
    {
        let durations = recorder.durations.lock().unwrap();
        assert_eq!(durations.len(), 4);
        assert!(durations.iter().all(|d| d.is_finite() && *d >= 0.0));
    }
    let panic = spawn("metric", async {
        panic!("named task panic");
    });
    settle().await;
    assert_eq!(
        recorder.panics.load(Ordering::SeqCst),
        1,
        "panic is observable before joining the task"
    );
    panic.await;
    assert_eq!(
        recorder.panics.load(Ordering::SeqCst),
        1,
        "poll does not double count a recorded panic"
    );
    every(
        "metric",
        Arc::new(FakeClock::new(0)),
        Duration::from_secs(1),
        FirstRun::Immediate,
        || async { panic!("periodic panic") },
    )
    .await;
    assert_eq!(recorder.panics.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rewinding_wall_time_does_not_extend_a_periodic_wait() {
    let clock = Arc::new(FakeClock::new(10_000));
    let runs = Arc::new(AtomicUsize::new(0));
    let n = runs.clone();
    let task = every(
        "rewind",
        clock.clone(),
        Duration::from_secs(1),
        FirstRun::Immediate,
        move || {
            n.fetch_add(1, Ordering::SeqCst);
            async {}
        },
    );
    settle().await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    clock.set(-86_400_000);
    clock.advance(999);
    settle().await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    clock.advance(1);
    settle().await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "one elapsed period, despite a one-day wall rewind"
    );
    clock.advance(1_000);
    settle().await;
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    task.stop();
    task.await;
}

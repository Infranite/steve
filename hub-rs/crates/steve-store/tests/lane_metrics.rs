use metrics::{
    Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use std::sync::{Arc, Mutex};
use steve_store::{
    instrumentation::carry,
    lane::{self, Lane},
};

#[derive(Clone, Default)]
struct Counts(Arc<Mutex<Vec<Key>>>);
struct Count(Counts, Key);
impl CounterFn for Count {
    fn increment(&self, count: u64) {
        self.0
            .0
            .lock()
            .unwrap()
            .extend(std::iter::repeat_n(self.1.clone(), count as usize));
    }
    fn absolute(&self, _: u64) {
        panic!("unexpected absolute counter");
    }
}
impl Recorder for Counts {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(Arc::new(Count(self.clone(), key.clone())))
    }
    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }
    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

#[test]
fn unset_is_counted_per_site_but_explicit_interactive_is_not_missing() {
    let counts = Counts::default();
    metrics::with_local_recorder(&counts, || {
        assert_eq!(lane::current(), Lane::Interactive);
        assert_eq!(lane::current(), Lane::Interactive);
        lane::with_lane(Lane::Interactive, || {
            assert_eq!(lane::current(), Lane::Interactive)
        });
        // Crossing threads with no classification must not silently clear the missing bit.
        let work = carry(|| assert_eq!(lane::current(), Lane::Interactive));
        std::thread::scope(|threads| {
            threads
                .spawn(|| metrics::with_local_recorder(&counts, work))
                .join()
                .unwrap();
        });
    });
    let keys = counts.0.lock().unwrap();
    assert_eq!(keys.len(), 3);
    assert!(
        keys.iter()
            .all(|key| key.name() == "steve_store_unset_lane_total")
    );
    let sites: std::collections::BTreeSet<_> = keys
        .iter()
        .flat_map(Key::labels)
        .filter(|label| label.key() == "site")
        .map(|label| label.value())
        .collect();
    assert_eq!(
        sites.len(),
        3,
        "retain the original static caller locations"
    );
}

#[test]
fn carried_lane_restores_the_worker_context() {
    lane::with_lane(Lane::Admin, || {
        let work = carry(|| assert_eq!(lane::current(), Lane::Admin));
        std::thread::spawn(move || {
            lane::with_lane(Lane::Ingest, || {
                work();
                assert_eq!(lane::current(), Lane::Ingest);
            })
        })
        .join()
        .unwrap();
    });
    assert_eq!(lane::capture(), None);
}

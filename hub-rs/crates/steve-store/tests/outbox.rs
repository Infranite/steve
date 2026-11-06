//! The outbox (migration 21): facts only on commit, a receipt per consumer, back-off and dead
//! deliveries (by time, or at once when rejected), one notice per consumer, revive, paused consumers
//! keeping what they owe, registration rules, wake-ups, and delivery after a restart.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use steve_store::outbox::{
    self, Applied, ConsumeError, Consumer, DEAD_AFTER_MS, Dispatcher, Event, Since,
};
use steve_store::{Store, StoreError, Tx};

const TOPIC: &str = "test.happened";

/// Counts deliveries; its business write is a row in `audit`, in the receipt's transaction.
struct Counter {
    id: &'static str,
    topic: &'static str,
    since: Since,
    applied: AtomicUsize,
    /// What `apply` answers (`None` = fine).
    fail: Mutex<Option<ConsumeError>>,
    /// Run inside `effect` once (to interleave another dispatcher).
    during_effect: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Counter {
    fn new(id: &'static str) -> Arc<Self> {
        Self::with(id, TOPIC, Since::Start)
    }
    fn with(id: &'static str, topic: &'static str, since: Since) -> Arc<Self> {
        Arc::new(Self {
            id,
            topic,
            since,
            applied: AtomicUsize::new(0),
            fail: Mutex::new(None),
            during_effect: Mutex::new(None),
        })
    }
    fn n(&self) -> usize {
        self.applied.load(Ordering::SeqCst)
    }
}

impl Consumer for Counter {
    fn id(&self) -> &'static str {
        self.id
    }
    fn topic(&self) -> &'static str {
        self.topic
    }
    fn since(&self) -> Since {
        self.since
    }
    fn effect(&self, _: &Event) -> Result<(), ConsumeError> {
        // taken first: the guard must be gone before `f` (it delivers through this consumer again)
        let f = self.during_effect.lock().unwrap().take();
        if let Some(f) = f {
            f();
        }
        Ok(())
    }
    fn apply(&self, tx: &Tx, ev: &Event) -> Result<Applied, ConsumeError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        tx.execute(
            "INSERT INTO audit (workspace_id, ts, actor, action, target, detail) VALUES (?, 0, ?, 'consumed', ?, '')",
            rusqlite::params![ev.workspace_id, self.id, ev.id],
        )?;
        self.applied.fetch_add(1, Ordering::SeqCst);
        Ok(Applied::noted(format!("ok {}", ev.id)))
    }
}

fn emit(s: &Store, ws: &str, n: i64) -> Event {
    s.transaction(|tx| outbox::emit(tx, ws, TOPIC, 1, &json!({ "n": n })))
        .unwrap()
}

fn one<T: rusqlite::types::FromSql>(s: &Store, sql: &str) -> T {
    s.with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn audits(s: &Store) -> i64 {
    one(s, "SELECT count(*) FROM audit WHERE action = 'consumed'")
}

fn disp(cs: &[Arc<Counter>]) -> Dispatcher {
    Dispatcher::new(cs.iter().map(|c| c.clone() as Arc<dyn Consumer>).collect())
}

#[test]
fn only_committed_facts_exist() {
    let t = common::fresh();
    let r: Result<(), StoreError> = t.store.transaction(|tx| {
        outbox::emit(tx, "default", TOPIC, 1, &json!({}))?;
        Err(StoreError::NotFound("rolled back"))
    });
    assert!(r.is_err());
    assert_eq!(
        one::<i64>(&t.store, "SELECT count(*) FROM outbox_events"),
        0
    );
    emit(&t.store, "default", 1);
    assert_eq!(
        one::<i64>(&t.store, "SELECT count(*) FROM outbox_events"),
        1
    );
}

#[test]
fn each_consumer_gets_each_event_once_with_a_receipt_in_its_workspace() {
    let t = common::fresh();
    let (a, b) = (Counter::new("a"), Counter::new("b"));
    let d = disp(&[a.clone(), b.clone()]);
    d.register(&t.store).unwrap();
    emit(&t.store, "default", 1);
    emit(&t.store, "w2", 2);
    let p = d.pump(&t.store, 1).unwrap();
    assert_eq!(p.done, 4);
    assert_eq!((a.n(), b.n()), (2, 2));
    assert_eq!(
        d.pump(&t.store, 2).unwrap().done,
        0,
        "received: never again"
    );
    // a second dispatcher (another process) finds nothing either
    assert_eq!(
        disp(&[a.clone(), b.clone()])
            .pump(&t.store, 3)
            .unwrap()
            .done,
        0
    );
    assert_eq!(audits(&t.store), 4);
    let scoped: i64 = one(
        &t.store,
        "SELECT count(*) FROM outbox_receipts r JOIN outbox_events e ON e.seq = r.event_seq WHERE r.workspace_id = e.workspace_id AND r.outcome LIKE 'ok %'",
    );
    assert_eq!(scoped, 4);
}

#[test]
fn an_effect_that_delivers_again_meanwhile_still_applies_once() {
    // no fencing: a second pass that overlaps (never in the Hub) is kept to one result by the receipt
    let t = common::fresh();
    let c = Counter::new("c");
    let slow = disp(std::slice::from_ref(&c));
    slow.register(&t.store).unwrap();
    emit(&t.store, "default", 1);
    let (store, c2) = (t.store.clone(), c.clone());
    *c.during_effect.lock().unwrap() = Some(Box::new(move || {
        assert_eq!(disp(&[c2]).pump(&store, 1).unwrap().done, 1);
    }));
    let p = slow.pump(&t.store, 1).unwrap();
    assert_eq!(p.done, 1, "{p:?}");
    assert_eq!(c.n(), 1, "one business result");
    assert_eq!(audits(&t.store), 1);
    assert_eq!(
        one::<i64>(&t.store, "SELECT count(*) FROM outbox_leases"),
        0
    );
}

#[test]
fn a_failing_apply_rolls_back_and_backs_off_until_it_has_failed_for_a_day() {
    let t = common::fresh();
    let c = Counter::new("c");
    let d = disp(std::slice::from_ref(&c));
    d.register(&t.store).unwrap();
    emit(&t.store, "default", 1);
    *c.fail.lock().unwrap() = Some(ConsumeError::Retry("busy".into()));
    let start = 1;
    let mut now = start;
    let mut attempt = 1;
    // many more attempts than the old count of 8, all within the day
    while now - start < DEAD_AFTER_MS {
        let p = d.pump(&t.store, now).unwrap();
        assert_eq!((p.retried, p.dead), (1, 0), "attempt {attempt} at {now}");
        assert_eq!(d.pump(&t.store, now).unwrap().retried, 0, "backing off");
        now += outbox::backoff(attempt) + 1;
        attempt += 1;
    }
    assert!(attempt > 100, "{attempt}");
    let p = d.pump(&t.store, now).unwrap();
    assert_eq!(p.dead, 1, "{p:?}");
    assert_eq!(audits(&t.store), 0, "nothing of the failed attempts stays");
    assert_eq!(
        one::<i64>(&t.store, "SELECT count(*) FROM outbox_receipts"),
        0
    );
    *c.fail.lock().unwrap() = None;
    assert_eq!(
        d.pump(&t.store, now + 10 * DEAD_AFTER_MS).unwrap().done,
        0,
        "dead is not retried"
    );
    let st = t.store.reader(outbox::stats).unwrap();
    assert_eq!((st.pending, st.dead), (1, 1));
    assert_eq!(
        outbox::prune(&t.store, i64::MAX / 2, 0).unwrap(),
        0,
        "dead is never pruned"
    );
}

#[test]
fn a_success_after_failures_starts_the_failure_clock_over() {
    let t = common::fresh();
    let c = Counter::new("c");
    let d = disp(std::slice::from_ref(&c));
    d.register(&t.store).unwrap();
    emit(&t.store, "default", 1);
    *c.fail.lock().unwrap() = Some(ConsumeError::Retry("offline".into()));
    assert_eq!(d.pump(&t.store, 1).unwrap().retried, 1);
    *c.fail.lock().unwrap() = None;
    assert_eq!(d.pump(&t.store, DEAD_AFTER_MS - 1).unwrap().done, 1);
    // a later event failing once is not dead because the first one failed a day ago
    emit(&t.store, "default", 2);
    *c.fail.lock().unwrap() = Some(ConsumeError::Retry("offline".into()));
    let p = d.pump(&t.store, DEAD_AFTER_MS + 10).unwrap();
    assert_eq!((p.retried, p.dead), (1, 0), "{p:?}");
}

#[test]
fn a_rejected_delivery_is_dead_at_once() {
    let t = common::fresh();
    let c = Counter::new("c");
    let d = disp(std::slice::from_ref(&c));
    d.register(&t.store).unwrap();
    emit(&t.store, "default", 1);
    *c.fail.lock().unwrap() = Some(ConsumeError::Reject("另一个工作空间".into()));
    assert_eq!(d.pump(&t.store, 1).unwrap().dead, 1);
    let err: String = one(
        &t.store,
        "SELECT last_error FROM outbox_leases WHERE dead = 1",
    );
    assert_eq!(err, "另一个工作空间");
}

#[test]
fn dead_deliveries_are_noticed_once_per_consumer_and_workspace_until_revived() {
    let t = common::fresh();
    let (a, b) = (Counter::new("a"), Counter::new("b"));
    let d = disp(&[a.clone(), b.clone()]);
    d.register(&t.store).unwrap();
    for n in 0..3 {
        emit(&t.store, "default", n);
    }
    emit(&t.store, "w2", 9);
    *a.fail.lock().unwrap() = Some(ConsumeError::Reject("坏数据".into()));
    let p = d.pump(&t.store, 1).unwrap();
    assert_eq!((p.dead, p.done), (4, 4), "{p:?}");
    let mut groups: Vec<(String, Option<String>)> = p
        .newly_dead
        .iter()
        .map(|g| (g.consumer.clone(), g.workspace_id.clone()))
        .collect();
    groups.sort();
    assert_eq!(
        groups,
        [
            ("a".to_string(), Some("default".to_string())),
            ("a".to_string(), Some("w2".to_string()))
        ],
        "one per (consumer, workspace), not one per event"
    );
    assert_eq!(p.newly_dead[0].topic, TOPIC);
    // another event dying while the group is still dead: no new notice
    emit(&t.store, "default", 4);
    let p = d.pump(&t.store, 2).unwrap();
    assert_eq!((p.dead, p.newly_dead.len()), (1, 0), "{p:?}");
    // the view: one group per consumer over every workspace
    let dead = t.store.reader(outbox::dead).unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(
        (
            dead[0].consumer.as_str(),
            dead[0].topic.as_str(),
            dead[0].count
        ),
        ("a", TOPIC, 5)
    );
    assert_eq!(dead[0].last_error.as_deref(), Some("坏数据"));
    assert_eq!(
        outbox::revive(&t.store, Some("b")).unwrap(),
        0,
        "b has none"
    );
    // revived: delivered again once the cause is gone
    *a.fail.lock().unwrap() = None;
    assert_eq!(outbox::revive(&t.store, Some("a")).unwrap(), 5);
    assert!(t.store.reader(outbox::dead).unwrap().is_empty());
    assert_eq!(d.pump(&t.store, 3).unwrap().done, 5);
    assert_eq!(a.n(), 5);
    // a new failure after the revive is a new episode: noticed again
    emit(&t.store, "default", 5);
    *a.fail.lock().unwrap() = Some(ConsumeError::Reject("又坏了".into()));
    assert_eq!(d.pump(&t.store, 4).unwrap().newly_dead.len(), 1);
}

#[test]
fn a_revived_delivery_gets_a_fresh_day() {
    let t = common::fresh();
    let c = Counter::new("c");
    let d = disp(std::slice::from_ref(&c));
    d.register(&t.store).unwrap();
    emit(&t.store, "default", 1);
    *c.fail.lock().unwrap() = Some(ConsumeError::Retry("down".into()));
    assert_eq!(d.pump(&t.store, 1).unwrap().retried, 1);
    assert_eq!(d.pump(&t.store, 1 + DEAD_AFTER_MS).unwrap().dead, 1);
    assert_eq!(outbox::revive(&t.store, None).unwrap(), 1);
    let at = 2 + DEAD_AFTER_MS;
    let p = d.pump(&t.store, at).unwrap();
    assert_eq!((p.retried, p.dead), (1, 0), "{p:?}");
    let attempts: i64 = one(&t.store, "SELECT attempts FROM outbox_leases");
    assert_eq!(attempts, 1, "attempts start over");
}

#[test]
fn a_paused_consumer_keeps_what_it_owes_through_prune() {
    let t = common::fresh();
    let (a, b) = (Counter::new("a"), Counter::new("b"));
    let d = disp(&[a.clone(), b.clone()]);
    d.register(&t.store).unwrap();
    t.store
        .with_conn(|c| Ok(c.execute("UPDATE outbox_consumers SET enabled = 0 WHERE id = 'b'", [])?))
        .unwrap();
    emit(&t.store, "default", 1);
    assert_eq!(d.pump(&t.store, 1).unwrap().done, 1, "only a");
    assert_eq!(
        outbox::prune(&t.store, i64::MAX / 2, 0).unwrap(),
        0,
        "b still owes it"
    );
    assert_eq!(t.store.reader(outbox::stats).unwrap().pending, 1);
    t.store
        .with_conn(|c| Ok(c.execute("UPDATE outbox_consumers SET enabled = 1 WHERE id = 'b'", [])?))
        .unwrap();
    assert_eq!(d.pump(&t.store, 2).unwrap().done, 1);
    assert_eq!(b.n(), 1);
    assert_eq!(
        outbox::prune(&t.store, i64::MAX / 2, 0).unwrap(),
        1,
        "now fully delivered"
    );
    assert_eq!(
        one::<i64>(&t.store, "SELECT count(*) FROM outbox_receipts"),
        0
    );
}

#[test]
fn prune_keeps_recent_events() {
    let t = common::fresh();
    let c = Counter::new("c");
    let d = disp(std::slice::from_ref(&c));
    d.register(&t.store).unwrap();
    let e = emit(&t.store, "default", 1);
    d.pump(&t.store, 1).unwrap();
    assert_eq!(
        outbox::prune(&t.store, e.created + 10, outbox::KEEP_MS).unwrap(),
        0
    );
    assert_eq!(
        outbox::prune(&t.store, e.created + outbox::KEEP_MS + 1, outbox::KEEP_MS).unwrap(),
        1
    );
}

#[test]
fn registration_rules() {
    let t = common::fresh();
    emit(&t.store, "default", 1);
    let (start, now) = (
        Counter::with("start", TOPIC, Since::Start),
        Counter::with("now", TOPIC, Since::Now),
    );
    let d = disp(&[start.clone(), now.clone()]);
    d.register(&t.store).unwrap();
    d.register(&t.store).unwrap();
    emit(&t.store, "default", 2);
    d.pump(&t.store, 1).unwrap();
    assert_eq!((start.n(), now.n()), (2, 1), "Since::Now skips history");
    // the same id for another topic is refused
    let moved = Counter::with("start", "test.other", Since::Start);
    let err = disp(&[moved]).register(&t.store).unwrap_err();
    assert!(
        err.to_string().contains("registered for test.happened"),
        "{err}"
    );
}

#[tokio::test]
async fn a_commit_that_emitted_wakes_the_dispatcher() {
    let t = common::fresh();
    let wake = t.store.outbox_wake();
    t.store
        .transaction(|tx| -> Result<(), StoreError> {
            tx.execute("UPDATE workspaces SET name = name", [])?;
            Ok(())
        })
        .unwrap();
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(50), wake.notified()).await;
    assert!(quiet.is_err(), "no event, no wake-up");
    emit(&t.store, "default", 1);
    tokio::time::timeout(std::time::Duration::from_secs(1), wake.notified())
        .await
        .expect("woken");
}

#[test]
fn what_was_committed_before_a_crash_is_delivered_after_it() {
    let t = common::fresh();
    let path = t.store.path().to_path_buf();
    let c = Counter::new("c");
    disp(std::slice::from_ref(&c)).register(&t.store).unwrap();
    emit(&t.store, "default", 1);
    // the process stops before any pass: a new store and dispatcher on the same file
    let again = Store::open(&path, steve_store::NoPublisher).unwrap();
    let d = disp(std::slice::from_ref(&c));
    d.register(&again).unwrap();
    assert_eq!(d.pump(&again, 1).unwrap().done, 1);
    assert_eq!(d.pump(&again, 2).unwrap().done, 0);
    assert_eq!(c.n(), 1);
}

#[test]
fn an_idle_pass_takes_no_write_lock() {
    let t = common::fresh();
    let c = Counter::new("c");
    let d = disp(&[c]);
    d.register(&t.store).unwrap();
    // the write connection is held elsewhere: an idle pass must not wait for it
    let store = t.store.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let held = std::thread::spawn(move || {
        store
            .with_conn(|_| {
                tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                Ok(())
            })
            .unwrap()
    });
    rx.recv().unwrap();
    let t0 = std::time::Instant::now();
    assert_eq!(d.pump(&t.store, 1).unwrap().done, 0);
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(200),
        "{:?}",
        t0.elapsed()
    );
    held.join().unwrap();
}

#[test]
fn prune_removes_more_than_one_batch() {
    let t = common::fresh();
    let c = Counter::new("c");
    let d = disp(std::slice::from_ref(&c));
    d.register(&t.store).unwrap();
    // more than one prune batch (500), delivered, and one still owed at the end
    t.store
        .transaction(|tx| {
            for i in 0..1201 {
                outbox::emit(tx, "default", TOPIC, 1, &json!({ "i": i }))?;
            }
            Ok::<_, StoreError>(())
        })
        .unwrap();
    while d.pump(&t.store, 1).unwrap().done > 0 {}
    emit(&t.store, "default", 1);
    assert_eq!(outbox::prune(&t.store, i64::MAX / 2, 0).unwrap(), 1201);
    assert_eq!(
        one::<i64>(&t.store, "SELECT count(*) FROM outbox_events"),
        1
    );
    assert_eq!(
        one::<i64>(&t.store, "SELECT count(*) FROM outbox_receipts"),
        0
    );
}

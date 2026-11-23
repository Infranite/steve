//! Independent review probes of the outbox (written by the review worker on an isolated snapshot).
use steve_store::testkit::SqliteFixtures;
mod common;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use steve_store::outbox::{self, Applied, ConsumeError, Consumer, Dispatcher, Event, Since};
use steve_store::{NoPublisher, Store, W};

struct Probe {
    topic: &'static str,
    fail: AtomicBool,
    since: Since,
}
impl Consumer for Probe {
    fn id(&self) -> &'static str {
        "review.probe"
    }
    fn topic(&self) -> &'static str {
        self.topic
    }
    fn since(&self) -> Since {
        self.since
    }
    fn apply(&self, w: &mut W<'_>, ev: &Event) -> Result<Applied, ConsumeError> {
        steve_store::audit_repo::record(
            w,
            &ev.workspace_id,
            "probe",
            "consumed",
            &ev.id,
            &serde_json::Value::Null,
        )?;
        if self.fail.load(Ordering::SeqCst) {
            return Err(ConsumeError::Retry("injected after write".into()));
        }
        Ok(Applied::noted("applied"))
    }
}
fn db() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open(d.path().join("steve.sqlite3"), NoPublisher).unwrap();
    (d, s)
}
fn probe(since: Since) -> Arc<Probe> {
    Arc::new(Probe {
        topic: "test.done",
        fail: AtomicBool::new(false),
        since,
    })
}
fn emit(s: &Store) -> Event {
    s.write(|tx| outbox::append(tx, "workspace-a", "test.done", 1, &serde_json::json!({})))
        .unwrap()
}
fn count(s: &Store, table: &str) -> i64 {
    s.fixture_read(|c| -> steve_store::Result<i64> {
        Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    })
    .unwrap()
}

#[test]
fn failed_apply_rolls_back_effect_and_receipt_then_retries_once() {
    let (_d, s) = db();
    let p = probe(Since::Start);
    p.fail.store(true, Ordering::SeqCst);
    let d = Dispatcher::new(vec![p.clone()]);
    d.register(&s).unwrap();
    emit(&s);
    assert_eq!(d.pump(&s, 1).unwrap().retried, 1);
    assert_eq!(count(&s, "audit"), 0);
    assert_eq!(count(&s, "outbox_receipts"), 0);
    p.fail.store(false, Ordering::SeqCst);
    assert_eq!(d.pump(&s, 10_000).unwrap().done, 1);
    assert_eq!(d.pump(&s, 10_001).unwrap().done, 0);
    assert_eq!(count(&s, "audit"), 1);
    assert_eq!(count(&s, "outbox_receipts"), 1);
}
#[test]
fn paused_delivery_survives_prune_and_can_resume() {
    let (_d, s) = db();
    let d = Dispatcher::new(vec![probe(Since::Start)]);
    d.register(&s).unwrap();
    let ev = emit(&s);
    s.fixture_write(|c| {
        c.execute("UPDATE outbox_consumers SET enabled=0", [])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(common::prune_outbox(&s, ev.created + 100, 0).unwrap(), 0);
    assert_eq!(d.pump(&s, 10).unwrap().done, 0);
    s.fixture_write(|c| {
        c.execute("UPDATE outbox_consumers SET enabled=1", [])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(d.pump(&s, 10).unwrap().done, 1);
    assert_eq!(common::prune_outbox(&s, ev.created + 100, 0).unwrap(), 1);
}
#[test]
fn consumer_topic_change_is_rejected_without_mutating_registration() {
    let (_d, s) = db();
    let d = Dispatcher::new(vec![probe(Since::Start)]);
    d.register(&s).unwrap();
    let other = Arc::new(Probe {
        topic: "different",
        fail: AtomicBool::new(false),
        since: Since::Start,
    });
    assert!(Dispatcher::new(vec![other]).register(&s).is_err());
    emit(&s);
    assert_eq!(d.pump(&s, 1).unwrap().done, 1);
}
#[test]
fn since_now_skips_old_events_and_does_not_move_on_restart() {
    let (_d, s) = db();
    emit(&s);
    let d = Dispatcher::new(vec![probe(Since::Now)]);
    d.register(&s).unwrap();
    assert_eq!(d.pump(&s, 1).unwrap().done, 0);
    emit(&s);
    let restart = Dispatcher::new(vec![probe(Since::Now)]);
    restart.register(&s).unwrap();
    assert_eq!(restart.pump(&s, 2).unwrap().done, 1);
    assert_eq!(count(&s, "audit"), 1);
}

//! The Hub's coordinator against the fake Node: Hub decisions become commands, Node facts become
//! the Hub's state, once.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_hub::wire::node_runtime::{Binding, NodeDomain, NodeRuntime, Planned, Sink};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_store::{ChangeEvent, FileRef, FnPublisher, Store, StoreError};

#[derive(Default)]
struct Ends(Mutex<Vec<(String, String, Option<String>)>>);

impl NodeDomain for Ends {
    fn turn_ended(&self, session: &str, stop: &str, error: Option<String>) {
        self.0
            .lock()
            .unwrap()
            .push((session.into(), stop.into(), error));
    }
}

struct H {
    _dir: tempfile::TempDir,
    store: Store,
    node: Arc<FakeNode>,
    rt: Arc<NodeRuntime>,
    ends: Arc<Ends>,
    seen: Arc<Tee>,
    published: Arc<Mutex<Vec<ChangeEvent>>>,
}

/// Forwards to the Hub and keeps what went through, to send it again.
struct Tee {
    to: Sink,
    seen: Mutex<Vec<NodeEvent>>,
}

#[async_trait::async_trait]
impl EventSink for Tee {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        self.seen
            .lock()
            .unwrap()
            .extend(batch.events.iter().cloned());
        self.to.commit(batch).await
    }
}

fn turn_of(e: &ExecutionRef) -> TurnId {
    match e {
        ExecutionRef::Turn { turn, .. } => turn.clone(),
        _ => panic!("not a turn"),
    }
}

const S: &str = "s1";

fn hub() -> H {
    let dir = tempfile::tempdir().unwrap();
    let published = Arc::new(Mutex::new(vec![]));
    let p = published.clone();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        FnPublisher(move |e| p.lock().unwrap().push(e)),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                [S],
            )?;
            Ok(())
        })
        .unwrap();
    let node = FakeNode::new("n1");
    let ends = Arc::new(Ends::default());
    let rt = NodeRuntime::new(store.clone(), node.clone(), ends.clone());
    let seen = Arc::new(Tee {
        to: Sink(Arc::downgrade(&rt)),
        seen: Mutex::new(vec![]),
    });
    node.connect(seen.clone());
    rt.start();
    H {
        _dir: dir,
        store,
        node,
        rt,
        ends,
        seen,
        published,
    }
}

impl H {
    fn bind(&self) {
        self.rt
            .bind(&Binding {
                session: S.into(),
                resource: format!("item:{S}"),
                prepare: PrepareKind::Scratch,
                choice: build::choice(),
                policy: build::policy(),
            })
            .unwrap();
    }

    fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }

    fn phase(&self) -> String {
        self.q(&format!(
            "SELECT phase FROM runtime_sessions WHERE session_id='{S}'"
        ))
    }

    fn methods(&self) -> Vec<&'static str> {
        self.node.log().into_iter().map(|(m, _)| m).collect()
    }

    /// The attempts the Node took, in order.
    fn attempts(&self) -> Vec<ExecutionRef> {
        let rows: Vec<String> = self
            .store
            .reader(|c| {
                let mut st =
                    c.prepare("SELECT execution FROM runtime_attempts ORDER BY created, rowid")?;
                let v = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap();
        rows.iter()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }

    fn ends(&self) -> Vec<(String, String, Option<String>)> {
        self.ends.0.lock().unwrap().clone()
    }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn text(exec: &ExecutionRef, t: &str) -> (Subject, EventBody) {
    (
        Subject::Execution(exec.clone()),
        EventBody::TextDelta {
            message: "m1".into(),
            block: "b1".into(),
            utf8_offset: 0,
            text: t.into(),
        },
    )
}

#[tokio::test]
async fn bind_prepares_opens_then_runs_what_is_queued() {
    let h = hub();
    h.bind();
    // queued while opening: it waits for the open, then goes as one turn
    assert_eq!(h.rt.prompt(S, "hello", "user", &[]).unwrap(), Planned::Busy);
    until("start_turn", || h.methods().contains(&"start_turn")).await;
    assert_eq!(
        h.methods(),
        ["prepare_workspace", "open_session", "start_turn"]
    );
    until("active", || h.phase() == "active").await;
    assert!(h.rt.is_busy(S));

    let exec = h.attempts()[0].clone();
    let (s, b) = text(&exec, "hi there");
    h.node.emit(s, b).await.unwrap();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || !h.ends().is_empty()).await;

    assert_eq!(h.ends(), [(S.to_string(), "end_turn".to_string(), None)]);
    assert_eq!(h.phase(), "ready");
    assert!(!h.rt.is_busy(S));
    let texts: i64 =
        h.q("SELECT COUNT(*) FROM events WHERE kind='text_delta' AND format='steve.v1'");
    assert_eq!(texts, 1);
    let receipt: String = h.q("SELECT state FROM node_commands WHERE method='start_turn'");
    assert_eq!(receipt, "finished");
    let dispatched: String = h.q("SELECT state FROM session_dispatch WHERE backend='node'");
    assert_eq!(dispatched, "completed");
    assert_eq!(h.node.unacked(), 0, "every event was acknowledged");
}

#[tokio::test]
async fn a_message_during_a_turn_goes_next_and_the_item_hears_one_end() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("first attempt", || h.attempts().len() == 1).await;
    until("active", || h.phase() == "active").await;
    assert_eq!(
        h.rt.prompt(S, "two", "user", &[]).unwrap(),
        Planned::Busy,
        "waits for the turn"
    );

    let first = h.attempts()[0].clone();
    h.node.finish(&first, build::done()).await.unwrap();
    until("second attempt", || h.attempts().len() == 2).await;
    assert!(h.ends().is_empty(), "the turn goes on with what was queued");
    let second = h.attempts()[1].clone();
    assert_ne!(turn_of(&first), turn_of(&second), "a new logical turn");
    until("second running", || {
        h.methods().iter().filter(|m| **m == "start_turn").count() == 2
    })
    .await;
    h.node.finish(&second, build::done()).await.unwrap();
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.ends().len(), 1);
}

#[tokio::test]
async fn a_passing_failure_with_no_tool_run_is_a_new_attempt_of_the_same_turn() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("attempt", || h.attempts().len() == 1).await;
    until("started", || h.methods().contains(&"start_turn")).await;
    let first = h.attempts()[0].clone();
    let mut c = build::done();
    c.outcome = Outcome::Failed;
    c.stop_reason = "error".into();
    c.transient = true;
    h.node.finish(&first, c).await.unwrap();
    until("retry planned", || h.attempts().len() == 2).await;
    let second = h.attempts()[1].clone();
    assert_eq!(turn_of(&first), turn_of(&second), "the same turn");
    assert_ne!(first.attempt(), second.attempt(), "a new attempt");
    assert!(h.ends().is_empty());
    assert!(h.rt.is_busy(S), "the turn keeps its session while it waits");
    let wait: i64 = h.q("SELECT retry_at - created FROM node_commands WHERE method='start_turn' ORDER BY created DESC, rowid DESC LIMIT 1");
    assert!(wait >= 19_000, "the retry waits ({wait} ms)");
}

#[tokio::test]
async fn a_failure_after_a_tool_ran_ends_the_turn() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let first = h.attempts()[0].clone();
    let mut c = build::done();
    c.outcome = Outcome::Failed;
    c.stop_reason = "error".into();
    c.transient = true;
    c.acted = true;
    h.node.finish(&first, c).await.unwrap();
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.attempts().len(), 1, "no retry once something ran");
}

#[tokio::test]
async fn cancel_targets_the_attempt_running_when_asked() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let exec = h.attempts()[0].clone();
    assert!(h.rt.cancel(S).unwrap());
    until("cancel sent", || h.methods().contains(&"cancel")).await;
    let (_, op) = h
        .node
        .log()
        .into_iter()
        .find(|(m, _)| *m == "start_turn")
        .unwrap();
    assert!(h.node.cancel_requested(&op));
    let mut c = build::done();
    c.outcome = Outcome::Cancelled;
    c.stop_reason = "cancelled".into();
    c.transient = true;
    h.node.finish(&exec, c).await.unwrap();
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.attempts().len(), 1, "a cancelled turn is not retried");
    assert!(!h.rt.cancel(S).unwrap(), "nothing left to cancel");
}

#[tokio::test]
async fn the_same_events_again_are_stored_once() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let exec = h.attempts()[0].clone();
    let (s, b) = text(&exec, "x");
    h.node.emit(s, b).await.unwrap();
    let before: i64 = h.q("SELECT COUNT(*) FROM node_event_inbox");
    // the Node did not hear the ack and sends what it kept again
    let replay = h.seen.seen.lock().unwrap().clone();
    let sink = Sink(Arc::downgrade(&h.rt));
    sink.commit(EventBatch {
        events: replay.clone(),
    })
    .await
    .unwrap();
    sink.commit(EventBatch { events: replay }).await.unwrap();
    let after: i64 = h.q("SELECT COUNT(*) FROM node_event_inbox");
    assert_eq!(before, after);
    until("applied", || {
        h.q::<i64>("SELECT COUNT(*) FROM events WHERE kind='text_delta'") == 1
    })
    .await;
}

#[tokio::test]
async fn a_busy_node_gets_the_very_same_command_again() {
    let h = hub();
    h.node.set_capacity(Capacity {
        sessions: 0,
        jobs: 0,
        execs: 0,
    });
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("refused", || {
        h.q::<i64>(
            "SELECT COALESCE(MAX(lease_generation),0) FROM node_commands WHERE method='start_turn'",
        ) >= 1
    })
    .await;
    let state: String = h.q("SELECT state FROM node_commands WHERE method='start_turn'");
    assert_eq!(
        state, "pending",
        "exhausted is sent again, never a new attempt"
    );
    assert_eq!(h.attempts().len(), 1);
}

#[tokio::test]
async fn steves_lead_goes_first_once_and_attachments_go_by_hash() {
    let h = hub();
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "INSERT INTO files(id,workspace_id,sha256,name,mime,size,created_by,created) VALUES ('f1','default',?,'a.txt','text/plain',3,'u',1)",
                ["a".repeat(64)],
            )?;
            Ok(())
        })
        .unwrap();
    h.bind();
    let f = FileRef {
        id: "f1".into(),
        name: "a.txt".into(),
        mime: "text/plain".into(),
        size: 3,
    };
    h.rt.prompt(S, "look", "user", std::slice::from_ref(&f))
        .unwrap();
    // the open is not done yet: the lead joins the queue in front, once
    until("opened", || {
        h.methods().contains(&"start_turn") || h.phase() == "ready"
    })
    .await;
    let _ = h.rt.flush(S, Some(("lead", "steve")));
    let _ = h.rt.flush(S, Some(("lead", "steve")));
    until("attempt", || h.attempts().len() == 1).await;
    let spec: String = h.q("SELECT spec FROM runtime_attempts");
    let spec: TurnSpec = serde_json::from_str(&spec).unwrap();
    let Part::Text { text } = &spec.run.prompt[0] else {
        panic!("text first")
    };
    assert!(text == "look" || text == "lead\n\nlook", "{text}");
    assert!(
        spec.run
            .prompt
            .iter()
            .any(|p| matches!(p, Part::File { name, .. } if name == "a.txt"))
    );
    let users: i64 = h.q("SELECT COUNT(*) FROM events WHERE kind='user' AND format='steve.v1'");
    assert!(users >= 1);
    let published = h.published.lock().unwrap();
    assert!(
        published
            .iter()
            .any(|e| matches!(e, ChangeEvent::Event { kind, .. } if kind == "user")),
        "the browser hears the turn's messages"
    );
}

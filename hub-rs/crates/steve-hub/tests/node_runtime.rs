//! The Hub's coordinator against the fake Node: Hub decisions become commands, Node facts become
//! the Hub's state, once.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_hub::wire::acp_stats;
use steve_hub::wire::node_runtime::{
    AttemptFacts, Binding, ConfigChange, Configured, NodeDomain, NodeRuntime, Planned, Sink,
};
use steve_node_api::testkit::{FakeNode, Scripted, build};
use steve_node_api::*;
use steve_store::{ChangeEvent, FileRef, FnPublisher, Store, StoreError};

#[derive(Default)]
struct Ends(
    Mutex<Vec<(String, String, Option<String>)>>,
    Mutex<Vec<String>>,
    Mutex<Vec<AttemptFacts>>,
    /// Cards handed to YOLO (`permission_asked`).
    Mutex<Vec<String>>,
    /// The store the attempts' statistics rows go to, as the wired Hub books them.
    std::sync::OnceLock<Store>,
    /// The attempt each turn end named, in the order of `.0`.
    Mutex<Vec<Option<String>>>,
    /// While set, `attempt_finished` holds the apply worker there (mid-pass); `.6` says it does.
    std::sync::atomic::AtomicBool,
    std::sync::atomic::AtomicBool,
);

impl NodeDomain for Ends {
    fn turn_ended(&self, session: &str, attempt: Option<&str>, stop: &str, error: Option<String>) {
        self.0
            .lock()
            .unwrap()
            .push((session.into(), stop.into(), error));
        self.5.lock().unwrap().push(attempt.map(String::from));
    }
    fn attempt_started(
        &self,
        session: &str,
        attempt: &str,
        ordinal: i64,
        user_seq: Option<i64>,
        at: i64,
    ) {
        if let Some(store) = self.4.get() {
            acp_stats::node_attempt_started(store, session, attempt, ordinal, user_seq, at);
        }
        self.1
            .lock()
            .unwrap()
            .push(format!("{attempt}:{}", user_seq.is_some()));
    }
    fn attempt_finished(
        &self,
        session: &str,
        attempt: &str,
        c: &Completion,
        f: &AttemptFacts,
        at: i64,
    ) {
        if let Some(store) = self.4.get() {
            acp_stats::node_attempt_finished(store, session, attempt, c, f, at);
        }
        self.2.lock().unwrap().push(f.clone());
        while self.5.load(std::sync::atomic::Ordering::SeqCst) {
            self.6.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn permission_asked(&self, _: &str, decision: &str, _: &PermissionRequest) {
        self.3.lock().unwrap().push(decision.into());
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
    /// While set, batches are kept here (late delivery), not passed on.
    stash: Mutex<Option<Vec<EventBatch>>>,
}

#[async_trait::async_trait]
impl EventSink for Tee {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        self.seen
            .lock()
            .unwrap()
            .extend(batch.events.iter().cloned());
        if let Some(kept) = self.stash.lock().unwrap().as_mut() {
            kept.push(batch);
            return Ok(EventAck { durable: vec![] });
        }
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
    let _ = ends.4.set(store.clone());
    let rt = NodeRuntime::new(store.clone(), node.clone(), ends.clone());
    let seen = Arc::new(Tee {
        to: Sink(Arc::downgrade(&rt)),
        seen: Mutex::new(vec![]),
        stash: Mutex::new(None),
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

/// A permission request that expires `ttl_ms` from now on the Hub's clock.
fn perm_ttl(pid: &str, exec: ExecutionRef, command: &str, ttl_ms: i64) -> PermissionRequest {
    let mut r = build::permission(pid, exec, command);
    r.expires_at = Timestamp(steve_store::now() + ttl_ms);
    r
}

/// A permission request well within its time.
fn perm(pid: &str, exec: ExecutionRef, command: &str) -> PermissionRequest {
    perm_ttl(pid, exec, command, 600_000)
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
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
    let texts: i64 = h.q("SELECT COUNT(*) FROM events WHERE kind='text' AND format='steve.v1'");
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

/// B2b: the item hears the end of a turn under its last attempt, the gateway retry that ended it,
/// and a flush names the attempt it planned.
#[tokio::test]
async fn the_turn_end_names_the_last_attempt_of_its_retry_chain() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.count("start_turn") == 1).await;
    let first = h.attempts()[0].clone();
    h.transient_failure(&first).await;
    let retry = h.attempts()[1].clone();
    assert_eq!(
        h.rt.active_attempt(S).unwrap().as_deref(),
        Some(retry.attempt().as_str())
    );
    assert!(
        h.rt.same_turn(first.attempt().as_str(), retry.attempt().as_str())
            .unwrap()
    );
    // the retry goes now instead of after its wait
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "UPDATE node_commands SET retry_at=0 WHERE state='pending'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    until("retry sent", || h.count("start_turn") == 2).await;
    h.node.finish(&retry, build::done()).await.unwrap();
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.ends().len(), 1, "one end for the turn");
    assert_eq!(
        *h.ends.5.lock().unwrap(),
        [Some(retry.attempt().to_string())]
    );

    // the next turn, planned by a flush, is a turn of its own
    h.store.queued().add(S, "next", "user", &[]).unwrap();
    let (planned, next) = h.rt.flush_turn(S, None).unwrap();
    assert_eq!(planned, Planned::Created);
    let next = next.expect("the planned attempt");
    assert_eq!(h.rt.active_attempt(S).unwrap(), Some(next.clone()));
    assert!(!h.rt.same_turn(retry.attempt().as_str(), &next).unwrap());
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
async fn cancel_attempt_with_an_old_attempt_queues_nothing() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let exec = h.attempts()[0].clone();
    let a1 = h.rt.active_attempt(S).unwrap().expect("a1 runs");
    h.rt.prompt(S, "next", "user", &[]).unwrap();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("the next turn", || h.attempts().len() == 2).await;
    let a2 = h.rt.active_attempt(S).unwrap().expect("a2 runs");
    assert_ne!(a1, a2);
    until("a2 sent", || {
        h.methods().iter().filter(|m| **m == "start_turn").count() == 2
    })
    .await;
    assert!(!h.rt.cancel_attempt(S, &a1).unwrap(), "a1 is over");
    assert!(!h.rt.cancel_attempt(S, "att_nobody").unwrap());
    let cancels =
        || h.q::<i64>("SELECT COUNT(*) FROM node_commands WHERE method IN ('cancel','abandon')");
    assert_eq!(cancels(), 0, "no command for a stale attempt");
    assert!(h.rt.is_busy(S), "a2 still runs");
    assert!(h.rt.cancel_attempt(S, &a2).unwrap());
    // queued now, or once the Node is known to hold a2's start
    until("a2's cancel", || cancels() == 1).await;
}

/// Issue (B2a review): a stop frozen on an attempt that then failed in passing reaches its gateway
/// retry (the same turn, a new attempt), as the legacy runner's turn token does; it never reaches
/// the next turn.
#[tokio::test]
async fn cancel_attempt_reaches_the_retry_of_its_turn() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("started", || h.count("start_turn") == 1).await;
    let a1 = h.rt.active_attempt(S).unwrap().expect("a1 runs");
    let first = h.attempts()[0].clone();
    h.transient_failure(&first).await;
    let retry = h.rt.active_attempt(S).unwrap().expect("the retry waits");
    assert_ne!(a1, retry);
    let op: String =
        h.q("SELECT operation_id FROM node_commands WHERE method='start_turn' ORDER BY created DESC, rowid DESC LIMIT 1");

    assert!(
        h.rt.cancel_attempt(S, &a1).unwrap(),
        "the turn of a1 still runs"
    );
    let state: String = h.q(&format!(
        "SELECT state FROM node_commands WHERE operation_id='{op}'"
    ));
    assert_eq!(state, "rejected", "the retry is settled here, never sent");
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.ends(), [(S.to_string(), "cancelled".to_string(), None)]);
    assert_eq!(h.count("start_turn"), 1, "no second start_turn");
    assert!(!h.rt.is_busy(S));

    // a stop of that turn, late, leaves the next turn alone
    h.rt.prompt(S, "two", "user", &[]).unwrap();
    until("the next turn", || h.count("start_turn") == 2).await;
    assert!(!h.rt.cancel_attempt(S, &a1).unwrap(), "a1's turn is over");
    assert!(!h.rt.cancel_attempt(S, &retry).unwrap());
    let cancels: i64 =
        h.q("SELECT COUNT(*) FROM node_commands WHERE method IN ('cancel','abandon')");
    assert_eq!(cancels, 0, "nothing queued for the next turn");
    assert!(h.rt.is_busy(S), "the next turn still runs");
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
        h.q::<i64>("SELECT COUNT(*) FROM events WHERE kind='text'") == 1
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

/// Issue: a Node out of room (Exhausted) was asked again every second, forever, and nobody was
/// told. Now the same command waits longer each time and the session hears it once.
#[tokio::test]
async fn an_exhausted_node_backs_off_tells_the_session_once_and_runs_the_same_command_later() {
    let h = hub();
    h.node.refuse("start_turn", Some(ErrorCode::Exhausted));
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    let exhausted = || {
        h.q::<Option<i64>>(
            "SELECT MAX(json_extract(receipt,'$.exhausted')) FROM node_commands WHERE method='start_turn'",
        )
        .unwrap_or(0)
    };
    let mut delays = vec![];
    for n in 1..=3 {
        until("refused again", || exhausted() == n).await;
        // the time between the refusal and the next send
        delays.push(
            h.q::<i64>("SELECT retry_at-updated FROM node_commands WHERE method='start_turn'"),
        );
        // not waiting it out: due now
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "UPDATE node_commands SET retry_at=0 WHERE method='start_turn'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
    }
    for (d, want) in delays.iter().zip([2_000, 4_000, 8_000]) {
        assert!((want..want + 50).contains(d), "{delays:?}");
    }
    let notes: i64 =
        h.q("SELECT COUNT(*) FROM events WHERE kind='system' AND data LIKE '%Node 存储/容量已满%'");
    assert_eq!(notes, 1, "one note, not one per retry");
    assert_eq!(h.attempts().len(), 1, "never a new attempt");
    let state: String = h.q("SELECT state FROM node_commands WHERE method='start_turn'");
    assert_eq!(state, "pending");
    assert!(!h.methods().contains(&"start_turn"));

    // room again: the very same operation runs, once
    h.node.refuse("start_turn", None);
    until("start_turn", || h.methods().contains(&"start_turn")).await;
    until("active", || h.phase() == "active").await;
    let ran: Vec<_> = h
        .node
        .log()
        .into_iter()
        .filter(|(m, _)| *m == "start_turn")
        .collect();
    assert_eq!(ran.len(), 1);
    let op: String = h.q("SELECT operation_id FROM node_commands WHERE method='start_turn'");
    assert_eq!(ran[0].1.to_string(), op);
    let commands: i64 = h.q("SELECT COUNT(*) FROM node_commands WHERE method='start_turn'");
    assert_eq!(commands, 1);
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

#[tokio::test]
async fn a_permission_is_a_card_and_its_answer_goes_to_the_very_request() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let exec = h.attempts()[0].clone();
    let req = perm("p1", exec.clone(), "git push");
    h.node.ask_permission(req.clone()).await.unwrap();
    until("card", || {
        h.q::<i64>(
            "SELECT COUNT(*) FROM decisions WHERE subject_type='tool_call' AND status='open'",
        ) == 1
    })
    .await;
    let d: String = h.q("SELECT id FROM decisions WHERE subject_type='tool_call'");
    let q: String = h.q("SELECT question FROM decisions WHERE subject_type='tool_call'");
    assert_eq!(q, "允许执行：git push");
    let shown: i64 = h.q("SELECT COUNT(*) FROM events WHERE kind='permission'");
    assert_eq!(shown, 1);
    // the same request again (a replay) makes no second card
    let replay = h.seen.seen.lock().unwrap().clone();
    Sink(Arc::downgrade(&h.rt))
        .commit(EventBatch { events: replay })
        .await
        .unwrap();
    assert!(h.rt.answer_permission(&d, Some("allow_once")).unwrap());
    until("answered", || h.methods().contains(&"resolve_permission")).await;
    until("card settled", || {
        h.q::<String>(&format!("SELECT status FROM decisions WHERE id='{d}'")) == "resolved"
    })
    .await;
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM decisions WHERE subject_type='tool_call'"),
        1
    );
    assert!(
        !h.rt.answer_permission(&d, Some("allow_once")).unwrap(),
        "answered once"
    );
}

#[tokio::test]
async fn a_card_expires_with_its_attempt() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let exec = h.attempts()[0].clone();
    h.node
        .ask_permission(perm("p1", exec.clone(), "rm -rf x"))
        .await
        .unwrap();
    until("card", || {
        h.q::<i64>("SELECT COUNT(*) FROM decisions WHERE subject_type='tool_call'") == 1
    })
    .await;
    h.node.finish(&exec, build::done()).await.unwrap();
    until("expired", || {
        h.q::<String>("SELECT status FROM decisions WHERE subject_type='tool_call'") == "expired"
    })
    .await;
    let d: String = h.q("SELECT id FROM decisions WHERE subject_type='tool_call'");
    assert_eq!(h.perm_state(&d), "expired");
    assert!(!h.rt.answer_permission(&d, Some("allow_once")).unwrap());
}

fn usage(id: &str, cost: &str) -> EventBody {
    EventBody::Usage {
        report: UsageReport {
            report_id: id.into(),
            model: Some("sonnet".into()),
            meter_epoch: Epoch::try_from("m1".to_string()).unwrap(),
            scope: UsageScope::Main,
            measurement: Measurement::Cumulative,
            tokens: Tokens::default(),
            context_used: Some(1000),
            context_limit: Some(200_000),
            cost: Some(Money {
                decimal: cost.into(),
                currency: "USD".into(),
            }),
        },
    }
}

#[tokio::test]
async fn usage_and_tools_reach_the_session_and_the_attempts_statistics() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let exec = h.attempts()[0].clone();
    let sub = || Subject::Execution(exec.clone());
    h.node.emit(sub(), usage("u1", "0.5")).await.unwrap();
    h.node.emit(sub(), usage("u2", "0.8")).await.unwrap();
    let tool = ToolView {
        id: CallId::try_from("c1".to_string()).unwrap(),
        parent: None,
        origin: ToolOrigin::Harness,
        name: "Bash".into(),
        category: ToolCategory::Exec,
        title: "ls".into(),
        description: None,
        target: Some("ls".into()),
        phase: ToolPhase::Running,
        action: None,
        result: vec![],
        error: None,
        duration_ms: None,
    };
    h.node
        .emit(sub(), EventBody::ToolChanged { tool: tool.clone() })
        .await
        .unwrap();
    let mut failed = tool;
    failed.phase = ToolPhase::Failed;
    h.node
        .emit(sub(), EventBody::ToolChanged { tool: failed })
        .await
        .unwrap();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || !h.ends().is_empty()).await;
    let cost: f64 = h.q(&format!("SELECT cost FROM sessions WHERE id='{S}'"));
    assert!(
        (cost - 0.8).abs() < 1e-9,
        "the running total, counted once: {cost}"
    );
    let used: i64 = h.q(&format!("SELECT used FROM sessions WHERE id='{S}'"));
    assert_eq!(used, 1000);
    assert_eq!(h.ends.1.lock().unwrap().len(), 1, "started once");
    assert!(
        h.ends.1.lock().unwrap()[0].ends_with(":true"),
        "answers the prompt"
    );
    let f = h.ends.2.lock().unwrap()[0].clone();
    assert_eq!((f.tools, f.tool_errors), (1, 1));
    assert_eq!(f.tool_kinds.get("exec"), Some(&1));
    assert_eq!(f.tool_kinds.get("tool:Bash"), Some(&1));
    assert!((f.cost.unwrap() - 0.8).abs() < 1e-9);
}

impl H {
    /// Runs one turn to its end, leaving the session ready.
    async fn one_turn(&self, text: &str) {
        let (before, n) = (self.ends().len(), self.attempts().len());
        self.rt.prompt(S, text, "user", &[]).unwrap();
        until("attempt", || self.attempts().len() == n + 1).await;
        let exec = self.attempts().last().unwrap().clone();
        until("started", || {
            self.node
                .log()
                .iter()
                .filter(|(m, _)| *m == "start_turn")
                .count()
                == self.attempts().len()
        })
        .await;
        self.node.finish(&exec, build::done()).await.unwrap();
        until("turn end", || self.ends().len() > before).await;
        until("ready", || self.phase() == "ready").await;
    }

    fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| **m == method).count()
    }
}

#[tokio::test]
async fn release_lets_an_idle_session_go_and_only_an_idle_one() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("active", || h.phase() == "active").await;
    assert!(!h.rt.release(S).unwrap(), "busy: not let go");
    let exec = h.attempts()[0].clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("ready", || h.phase() == "ready").await;

    assert!(h.rt.release(S).unwrap());
    assert_eq!(h.phase(), "releasing");
    assert!(!h.rt.release(S).unwrap(), "already being let go");
    until("released", || h.phase() == "released").await;
    assert_eq!(h.count("release_session"), 1);
    let receipt: String = h.q("SELECT state FROM node_commands WHERE method='release_session'");
    assert_eq!(receipt, "finished");
    assert!(!h.rt.release(S).unwrap(), "nothing open to let go");
    assert!(!h.rt.is_busy(S));
}

#[tokio::test]
async fn a_released_session_opens_again_for_the_next_message_with_its_resume_handle() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;

    assert_eq!(
        h.rt.prompt(S, "again", "user", &[]).unwrap(),
        Planned::Busy,
        "waits for the open"
    );
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(
        h.methods(),
        [
            "prepare_workspace",
            "open_session",
            "start_turn",
            "release_session",
            "open_session",
            "start_turn"
        ]
    );
    let resume: Option<String> = h.q(
        "SELECT json_extract(request,'$.body.resume') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1",
    );
    assert!(resume.is_some(), "continues the same conversation");
    let first: Option<String> = h.q(
        "SELECT json_extract(request,'$.body.resume') FROM node_commands WHERE method='open_session' ORDER BY created, rowid LIMIT 1",
    );
    assert!(first.is_none(), "the first open had nothing to resume");
}

#[tokio::test]
async fn a_message_queued_while_releasing_reopens_once_the_release_is_done() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    // both before the release goes out (one thread: nothing is sent until the test waits)
    assert!(h.rt.release(S).unwrap());
    assert_eq!(h.rt.prompt(S, "more", "user", &[]).unwrap(), Planned::Busy);
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("release_session"), 1);
    assert_eq!(h.count("open_session"), 2, "opened again after the release");
    let queued: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM queued WHERE session_id='{S}'"
    ));
    assert_eq!(queued, 0, "the message went with the turn");
    let exec = h.attempts().last().unwrap().clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("second end", || h.ends().len() == 2).await;
    assert_eq!(h.phase(), "ready");
}

#[tokio::test]
async fn an_unknown_attempt_keeps_its_session_and_cancel_abandons_it() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    until("active", || h.phase() == "active").await;
    let exec = h.attempts()[0].clone();
    let (_, start_op) = h
        .node
        .log()
        .into_iter()
        .find(|(m, _)| *m == "start_turn")
        .unwrap();
    // the Node restarted and cannot tell what the attempt did
    h.node.lose_track(&exec).unwrap();
    h.node
        .emit(
            Subject::Execution(exec.clone()),
            EventBody::OperationChanged {
                receipt: Receipt {
                    operation: start_op.clone(),
                    state: Observed::Unknown {
                        reason: "node restarted".into(),
                    },
                },
            },
        )
        .await
        .unwrap();
    until("unknown", || {
        h.q::<String>("SELECT state FROM runtime_attempts") == "unknown"
    })
    .await;
    assert_eq!(
        h.rt.prompt(S, "next", "user", &[]).unwrap(),
        Planned::Busy,
        "an unknown attempt keeps its session"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        h.attempts().len(),
        1,
        "never sent again, never worked around"
    );
    assert!(h.rt.is_busy(S));

    assert!(h.rt.cancel(S).unwrap());
    until("abandon sent", || h.methods().contains(&"abandon")).await;
    assert!(!h.methods().contains(&"cancel"), "abandoned, not cancelled");
    let target: String = h.q(
        "SELECT json_extract(request,'$.body.target') FROM node_commands WHERE method='abandon'",
    );
    assert_eq!(
        target,
        start_op.as_str(),
        "the very attempt that was unknown"
    );

    // the Node proves it stopped: the turn is over, interrupted
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.ends()[0].1, "abandoned");
    let dispatched: String =
        h.q("SELECT state FROM session_dispatch WHERE backend='node' ORDER BY rowid LIMIT 1");
    assert_eq!(dispatched, "interrupted");
    assert_eq!(h.count("abandon"), 1);
    // the harness went with it: what waited opens the session again before its turn
    until("next turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("open_session"), 2);
    let refused: i64 =
        h.q("SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='rejected'");
    assert_eq!(refused, 0, "no turn on a released session");
    assert_eq!(h.attempts().len(), 2);
    assert_ne!(turn_of(&h.attempts()[0]), turn_of(&h.attempts()[1]));
}

#[tokio::test]
async fn after_a_restart_events_taken_in_but_not_applied_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
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
    let q = |sql: &str| -> i64 {
        store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    };
    let node = FakeNode::new("n1");
    let ends = Arc::new(Ends::default());

    // the first Hub runs a turn up to its first words
    let first = NodeRuntime::new(store.clone(), node.clone(), ends.clone());
    node.connect(Arc::new(Sink(Arc::downgrade(&first))));
    first.start();
    first
        .bind(&Binding {
            session: S.into(),
            resource: format!("item:{S}"),
            prepare: PrepareKind::Scratch,
            choice: build::choice(),
            policy: build::policy(),
        })
        .unwrap();
    first.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || {
        node.log().iter().any(|(m, _)| *m == "start_turn")
            && q("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 1
    })
    .await;
    let exec: ExecutionRef = serde_json::from_str(
        &store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT execution FROM runtime_attempts",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap(),
    )
    .unwrap();
    let gone = Arc::downgrade(&first);
    drop(first);
    until("first Hub gone", || gone.upgrade().is_none()).await;

    // the second takes the events in (durable) and stops before applying them
    let second = NodeRuntime::new(store.clone(), node.clone(), ends.clone());
    node.connect(Arc::new(Sink(Arc::downgrade(&second))));
    let (s, b) = text(&exec, "hi");
    node.emit(s, b).await.unwrap();
    assert_eq!(
        q("SELECT COUNT(*) FROM node_streams WHERE durable > applied"),
        1,
        "taken in, not applied"
    );
    assert_eq!(q("SELECT COUNT(*) FROM events WHERE kind='text'"), 0);
    drop(second);

    // the third starts and catches up with no new event to wake it
    let third = NodeRuntime::new(store.clone(), node.clone(), ends.clone());
    node.connect(Arc::new(Sink(Arc::downgrade(&third))));
    third.start();
    until("caught up", || {
        q("SELECT COUNT(*) FROM events WHERE kind='text'") == 1
    })
    .await;
    assert_eq!(
        q("SELECT COUNT(*) FROM node_streams WHERE durable > applied"),
        0
    );
}

/// A Node whose host did not come up: its sessions refuse work instead of queueing it.
#[tokio::test]
async fn a_stopped_runtime_refuses_instead_of_queueing() {
    let h = hub();
    h.bind();
    until("ready", || h.phase() == "ready").await;
    h.rt.stop("its tools did not connect");
    let e = h.rt.prompt(S, "hello", "user", &[]).unwrap_err();
    assert!(e.to_string().contains("not running"), "{e}");
    assert!(h.rt.flush(S, Some(("lead", "steve"))).is_err());
    assert!(h.rt.cancel(S).is_err());
    assert!(h.rt.release(S).is_err());
    let queued: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM queued WHERE session_id='{S}'"
    ));
    assert_eq!(queued, 0, "nothing waits for a sender that never runs");
    assert_eq!(h.count("start_turn"), 0);
}

/// The Node let the session go by itself, then refuses to open it again: the session is not
/// wedged in `opening`; the user hears why, and the next message tries again.
#[tokio::test]
async fn a_refused_reopen_does_not_wedge_the_session() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    // the Node says it is released, but the fake still holds it open: the open is refused
    let mut view = h
        .seen
        .seen
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EventBody::SessionChanged { view } => Some(view.clone()),
            _ => None,
        })
        .unwrap();
    view.phase = SessionPhase::Released;
    h.node
        .emit(
            Subject::Session {
                session: SessionId::new(S).unwrap(),
            },
            EventBody::SessionChanged { view },
        )
        .await
        .unwrap();
    until("released", || h.phase() == "released").await;
    let ends = h.ends().len();
    assert_eq!(h.rt.prompt(S, "again", "user", &[]).unwrap(), Planned::Busy);
    until("refused open reported", || h.ends().len() == ends + 1).await;
    let (session, stop, error) = h.ends().last().unwrap().clone();
    assert_eq!((session.as_str(), stop.as_str()), (S, "error"));
    assert!(error.unwrap().contains("could not open"));
    assert_eq!(h.phase(), "released", "back where it was, not opening");
    let state: String = h.q(
        "SELECT state FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1",
    );
    assert_eq!(state, "rejected");
    let opens: i64 = h.q("SELECT COUNT(*) FROM node_commands WHERE method='open_session'");
    assert_eq!(opens, 2, "never sent again by itself");
    let queued: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM queued WHERE session_id='{S}'"
    ));
    assert_eq!(queued, 1, "the message still waits");

    // once the Node has really let it go, the next message opens it and both go
    h.node
        .release_session(build::cmd("op_test_release", SessionId::new(S).unwrap()))
        .await
        .unwrap();
    h.rt.prompt(S, "third", "user", &[]).unwrap();
    until("reopened turn", || h.count("start_turn") == 2).await;
    let opens: i64 = h.q("SELECT COUNT(*) FROM node_commands WHERE method='open_session'");
    assert_eq!(opens, 3);
    let queued: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM queued WHERE session_id='{S}'"
    ));
    assert_eq!(queued, 0);
}

/// Issue: an applied event pruned from the inbox and delivered again (the Node never heard the ack)
/// was stored again and applied a second time. It is acknowledged and nothing happens.
#[tokio::test]
async fn a_pruned_event_delivered_again_is_acknowledged_and_not_applied_again() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("attempt", || h.attempts().len() == 1).await;
    let exec = h.attempts()[0].clone();
    until("running", || h.phase() == "active").await;
    let (s, b) = text(&exec, "x");
    h.node.emit(s, b).await.unwrap();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || h.ends().len() == 1).await;
    until("applied", || {
        h.q::<i64>("SELECT COUNT(*) FROM node_streams WHERE applied < durable") == 0
    })
    .await;
    h.rt.prune_inbox(steve_store::now() + 8 * 24 * 3600 * 1000);
    let inbox: i64 = h.q("SELECT COUNT(*) FROM node_event_inbox");
    let events: i64 = h.q("SELECT COUNT(*) FROM events");
    let marks: Vec<(String, i64, i64)> = h
        .store
        .reader(|c| {
            let mut st =
                c.prepare("SELECT stream,durable,applied FROM node_streams ORDER BY stream")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        })
        .unwrap();

    let replay = h.seen.seen.lock().unwrap().clone();
    let ack = h
        .seen
        .to
        .commit(EventBatch {
            events: replay.clone(),
        })
        .await
        .unwrap();
    let streams: std::collections::HashSet<_> =
        replay.iter().map(|e| e.cursor.stream.clone()).collect();
    assert_eq!(
        ack.durable.len(),
        streams.len(),
        "every stream acknowledged"
    );
    for w in &ack.durable {
        let top = replay
            .iter()
            .filter(|e| e.cursor.stream == w.stream)
            .map(|e| e.cursor.seq)
            .max()
            .unwrap();
        assert_eq!(w.seq, top);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM node_event_inbox"), inbox);
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM events"), events);
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM node_event_inbox WHERE applied=0"),
        0
    );
    let again: Vec<(String, i64, i64)> = h
        .store
        .reader(|c| {
            let mut st =
                c.prepare("SELECT stream,durable,applied FROM node_streams ORDER BY stream")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        })
        .unwrap();
    assert_eq!(again, marks);
    assert_eq!(h.ends().len(), 1, "the turn ended once");
}

/// Applied events past the retention go, except what is read again: the last resume handle of
/// the session and the last cumulative cost of its harness process.
#[tokio::test]
async fn pruning_the_inbox_keeps_what_is_read_again() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("attempt", || h.attempts().len() == 1).await;
    let exec = h.attempts()[0].clone();
    until("running", || h.phase() == "active").await;
    for (id, cost) in [("u1", "0.1"), ("u2", "0.3")] {
        h.node
            .emit(Subject::Execution(exec.clone()), usage(id, cost))
            .await
            .unwrap();
    }
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || h.ends().len() == 1).await;
    until("ready", || h.phase() == "ready").await;
    let all: i64 = h.q("SELECT COUNT(*) FROM node_event_inbox");
    until("applied", || {
        h.q::<i64>("SELECT COUNT(*) FROM node_streams WHERE applied < durable") == 0
    })
    .await;

    h.rt.prune_inbox(steve_store::now()); // nothing is old yet
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM node_event_inbox"), all);
    h.rt.prune_inbox(steve_store::now() + 8 * 24 * 3600 * 1000);
    let left: i64 = h.q("SELECT COUNT(*) FROM node_event_inbox");
    assert!(left < all, "{left} of {all}");
    let usage: Vec<String> = h
        .store
        .reader(|c| {
            let mut st = c.prepare("SELECT json_extract(payload,'$.body.report.report_id') FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='usage'")?;
            let v = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        })
        .unwrap();
    assert_eq!(usage, ["u2"], "the last cumulative cost stays");
    let views: i64 = h.q(
        "SELECT COUNT(*) FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.resume') IS NOT NULL",
    );
    assert_eq!(views, 1, "the last resume handle stays");
    let opened: i64 = h.q(
        "SELECT COUNT(*) FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.revision')=(SELECT json_extract(receipt,'$.payload.state.result.revision') FROM node_commands WHERE method='open_session')",
    );
    assert_eq!(
        opened, 1,
        "the view of the last open stays (a later release is told from it)"
    );

    // and the session still opens again where it left off
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("second turn", || h.count("start_turn") == 2).await;
    let resume: Option<String> = h.q(
        "SELECT json_extract(request,'$.body.resume') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1",
    );
    assert!(resume.is_some());
}

/// The coordinator's inbox lookups (the same text as in node_runtime.rs) use migration 036's
/// indexes instead of scanning the inbox inside write transactions.
#[tokio::test]
async fn inbox_lookups_use_their_indexes() {
    let h = hub();
    let plan = |sql: &str| -> String {
        h.store
            .reader(|c| {
                let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let n = st.parameter_count();
                let v = st
                    .query_map(
                        rusqlite::params_from_iter(std::iter::repeat_n("x", n)),
                        |r| r.get::<_, String>(3),
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v.join("; "))
            })
            .unwrap()
    };
    for (sql, index) in [
        (
            "SELECT payload FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='permission_requested' AND json_extract(payload,'$.body.request.id')=? AND json_extract(payload,'$.body.request.execution.session')=? LIMIT 1",
            "node_inbox_permission",
        ),
        (
            "SELECT payload FROM node_event_inbox WHERE node_id=? AND json_extract(payload,'$.subject.attempt')=? AND json_extract(payload,'$.subject.session')=? ORDER BY stream, seq",
            "node_inbox_attempt",
        ),
        (
            "SELECT json_extract(payload,'$.body.view.resume') FROM node_event_inbox
           WHERE node_id=? AND json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.id')=?
             AND json_extract(payload,'$.body.view.resume') IS NOT NULL
           ORDER BY received DESC, seq DESC LIMIT 1",
            "node_inbox_session_view",
        ),
        (
            "SELECT json_extract(payload,'$.body.report.cost.decimal') FROM node_event_inbox
           WHERE node_id=? AND stream=? AND epoch=? AND seq<? AND json_extract(payload,'$.body.type')='usage'
             AND json_extract(payload,'$.body.report.meter_epoch')=? AND json_extract(payload,'$.body.report.cost') IS NOT NULL
           ORDER BY seq DESC LIMIT 1",
            "node_inbox_usage",
        ),
    ] {
        let p = plan(sql);
        assert!(p.contains(index), "{index}: {p}");
    }
}

// ── D5: release, refusals, Hub restart and reconciliation ──

impl H {
    /// The Hub process restarts on the same database and the same Node: a new coordinator that
    /// reconciles with the Node before it sends anything.
    async fn restart_hub(&mut self) {
        let rt = NodeRuntime::new(self.store.clone(), self.node.clone(), self.ends.clone());
        let seen = Arc::new(Tee {
            to: Sink(Arc::downgrade(&rt)),
            seen: Mutex::new(vec![]),
            stash: Mutex::new(None),
        });
        self.node.connect(seen.clone());
        let old = Arc::downgrade(&self.rt);
        self.rt = rt;
        self.seen = seen;
        until("old Hub gone", || old.upgrade().is_none()).await;
        // the boot closes the statistics rows the restart cut, before anything runs
        acp_stats::interrupt_open(&self.store, steve_store::now());
        self.rt.start();
        until("reconciled", || self.rt.is_reconciled()).await;
    }

    /// The statistics rows of the attempts: (ended, stop_reason), in the attempts' order.
    fn stat_rows(&self) -> Vec<(Option<i64>, Option<String>)> {
        self.store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT t.ended, t.stop_reason FROM agent_turns t LEFT JOIN runtime_attempts a ON a.attempt_id = t.node_attempt_id ORDER BY a.created, a.rowid",
                )?;
                let v = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap()
    }

    /// The prompt text of the n-th attempt.
    fn prompt_of(&self, n: usize) -> String {
        self.q(&format!(
            "SELECT json_extract(spec,'$.run.prompt[0].text') FROM runtime_attempts ORDER BY created, rowid LIMIT 1 OFFSET {n}"
        ))
    }

    fn systems(&self, like: &str) -> i64 {
        self.q(&format!(
            "SELECT COUNT(*) FROM events WHERE session_id='{S}' AND kind='system' AND data LIKE '%{like}%'"
        ))
    }

    /// The state of the latest command of `method` (open and release ids are random).
    fn last_state(&self, method: &str) -> String {
        self.q(&format!(
            "SELECT state FROM node_commands WHERE method='{method}' ORDER BY created DESC, rowid DESC LIMIT 1"
        ))
    }

    fn state_of(&self, op: &str) -> String {
        self.q(&format!(
            "SELECT state FROM node_commands WHERE operation_id='{op}'"
        ))
    }
}

/// D5 acceptance "断线或重启后不重跑": the Node restarts with the Hub mid-turn. The attempt nobody
/// can tell the end of becomes unknown and keeps its session; it is never sent again. Stop gives
/// it up (abandon), and only what was queued since goes, on a reopened session.
#[tokio::test]
async fn a_restart_mid_turn_is_unknown_never_rerun_and_stop_abandons_it() {
    let mut h = hub();
    h.bind();
    h.rt.prompt(S, "first", "user", &[]).unwrap();
    until("running", || {
        h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 1
    })
    .await;
    let first_op = h
        .node
        .log()
        .into_iter()
        .find(|(m, _)| *m == "start_turn")
        .unwrap()
        .1;

    h.node.restart();
    h.restart_hub().await;
    assert_eq!(
        h.q::<String>("SELECT state FROM runtime_attempts"),
        "unknown"
    );
    assert_eq!(h.systems("等待对账"), 1, "shown as awaiting reconciliation");
    assert!(h.rt.is_busy(S));
    assert_eq!(h.rt.running_turns(), 1);
    assert_eq!(
        h.rt.prompt(S, "second", "user", &[]).unwrap(),
        Planned::Busy,
        "an unknown attempt keeps its session"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.count("start_turn"), 1, "never sent again");

    // a second restart finds nothing new to say
    h.restart_hub().await;
    assert_eq!(h.systems("等待对账"), 1, "told once");
    assert_eq!(h.count("start_turn"), 1);

    assert!(h.rt.cancel(S).unwrap());
    until("abandoned", || h.ends().len() == 1).await;
    assert_eq!(h.ends()[0].1, "abandoned");
    // L4: the restarts did not cut the attempt's statistics row; the abandon ended it
    // (the next turn may have started already: the first row is the abandoned attempt's)
    let rows = h.stat_rows();
    assert!(!rows.is_empty() && rows.len() <= 2, "{rows:?}");
    assert!(
        rows.iter().all(|r| r.1.as_deref() != Some("interrupted")),
        "{rows:?}"
    );
    assert!(rows[0].0.is_some(), "finished");
    assert_eq!(
        rows[0].1.as_deref(),
        Some("abandoned"),
        "never 'interrupted'"
    );
    let target: String = h.q(
        "SELECT json_extract(request,'$.body.target') FROM node_commands WHERE method='abandon'",
    );
    assert_eq!(target, first_op.as_str(), "the frozen target");
    until("reopened and next turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("open_session"), 2);
    let first_turn: i64 = h.q(
        "SELECT COUNT(*) FROM runtime_attempts WHERE turn_id=(SELECT turn_id FROM runtime_attempts ORDER BY created, rowid LIMIT 1)",
    );
    assert_eq!(first_turn, 1, "the first turn was never retried");
    assert!(h.prompt_of(1).contains("second"));
    assert!(!h.prompt_of(1).contains("first"));
    let exec = h.attempts()[1].clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("second end", || h.ends().len() == 2).await;
    until("second row ended", || {
        h.stat_rows().iter().all(|r| r.0.is_some())
    })
    .await;
    assert_eq!(h.stat_rows().len(), 2);
}

/// A restart lets every Node session go silently: an idle one is found released, so the next
/// message opens it again instead of a turn the Node refuses.
#[tokio::test]
async fn a_restart_lets_an_idle_session_go_and_the_next_message_opens_it_again() {
    let mut h = hub();
    h.bind();
    h.one_turn("hello").await;
    h.node.restart();
    h.restart_hub().await;
    assert_eq!(h.phase(), "released");
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("open_session"), 2);
    let refused: i64 =
        h.q("SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='rejected'");
    assert_eq!(refused, 0);
}

/// An open permission card whose request the Node no longer waits for (it restarted) expires.
#[tokio::test]
async fn reconciling_expires_cards_nobody_waits_for() {
    let mut h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("running", || {
        h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 1
    })
    .await;
    let exec = h.attempts()[0].clone();
    h.node
        .ask_permission(perm("p1", exec, "rm -rf build"))
        .await
        .unwrap();
    until("card", || {
        h.q::<i64>("SELECT COUNT(*) FROM decisions WHERE status='open'") == 1
    })
    .await;
    h.node.restart();
    h.restart_hub().await;
    assert_eq!(h.q::<String>("SELECT status FROM decisions"), "expired");
}

/// Issue: a turn the Node refuses because it had let the session go was never accepted: it is not
/// a failed turn. The session opens again and the same turn goes as a new attempt.
#[tokio::test]
async fn a_turn_refused_on_a_released_session_goes_again_after_a_reopen() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    // the Node lets the session go and the Hub does not hear of it
    h.node.restart();
    assert_eq!(h.phase(), "ready");
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("second turn accepted", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("open_session"), 2, "opened again");
    let refused: i64 =
        h.q("SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='rejected'");
    assert_eq!(refused, 1);
    let turns: i64 = h.q("SELECT COUNT(DISTINCT turn_id) FROM runtime_attempts");
    assert_eq!(turns, 2, "the same turn, a new attempt");
    assert!(h.prompt_of(2).contains("again"));
    let users: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM events WHERE session_id='{S}' AND kind='user' AND data LIKE '%again%'"
    ));
    assert_eq!(users, 1, "the message shows once");
    assert_eq!(h.ends().len(), 1, "no turn end for the refusal");
    let exec = h.attempts()[2].clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("end", || h.ends().len() == 2).await;
    let dispatched: String =
        h.q("SELECT state FROM session_dispatch WHERE backend='node' ORDER BY rowid DESC LIMIT 1");
    assert_eq!(dispatched, "completed");
}

/// Issue: a turn refused for good ends, and the business side hears its end.
#[tokio::test]
async fn a_turn_refused_for_good_ends_and_the_item_hears_it() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    h.node
        .refuse_next("start_turn", ApiError::new(ErrorCode::Unsupported, "no"));
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("end", || h.ends().len() == 2).await;
    assert_eq!(h.ends()[1].1, "error");
    assert_eq!(h.phase(), "ready");
    assert!(!h.rt.is_busy(S));
}

/// Issue: a release the Node refuses leaves the session ready (not releasing for good), and what
/// waited meanwhile runs.
#[tokio::test]
async fn a_refused_release_leaves_the_session_ready_and_what_waited_runs() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    h.node
        .refuse_next("release_session", ApiError::new(ErrorCode::Invalid, "no"));
    assert!(h.rt.release(S).unwrap());
    assert_eq!(h.rt.prompt(S, "more", "user", &[]).unwrap(), Planned::Busy);
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("open_session"), 1, "never let go");
    assert_eq!(h.last_state("release_session"), "rejected");
}

/// Issue: an open the Node refuses leaves the session released (not opening for good); the
/// message stays queued, nothing is retried by itself, the next message tries again.
#[tokio::test]
async fn a_refused_reopen_leaves_the_session_released_and_the_next_message_tries_again() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;
    h.node.refuse_next(
        "open_session",
        ApiError::new(ErrorCode::Invalid, "no workspace"),
    );
    h.rt.prompt(S, "more", "user", &[]).unwrap();
    until("refused", || h.last_state("open_session") == "rejected").await;
    assert_eq!(h.phase(), "released");
    assert!(!h.rt.is_busy(S));
    assert_eq!(h.systems("没能打开"), 1);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.count("open_session"), 1, "not retried by itself");
    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("second turn", || h.count("start_turn") == 2).await;
    assert!(h.prompt_of(1).contains("more") && h.prompt_of(1).contains("again"));
}

/// Issue: after an abandon the Node refused, a new stop asks again with a new operation; one that
/// is on its way is not doubled.
#[tokio::test]
async fn a_second_stop_after_a_refused_abandon_asks_again() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("running", || {
        h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 1
    })
    .await;
    let (_, op) = h
        .node
        .log()
        .into_iter()
        .find(|(m, _)| *m == "start_turn")
        .unwrap();
    let exec = h.attempts()[0].clone();
    h.node.lose_track(&exec).unwrap();
    h.node
        .emit(
            Subject::Operation {
                operation: op.clone(),
            },
            EventBody::OperationChanged {
                receipt: h.node.receipt(&op).unwrap(),
            },
        )
        .await
        .unwrap();
    until("unknown", || {
        h.q::<String>("SELECT state FROM runtime_attempts") == "unknown"
    })
    .await;
    let a: String = h.q("SELECT attempt_id FROM runtime_attempts");
    h.node
        .refuse_next("abandon", ApiError::new(ErrorCode::Conflict, "not now"));
    assert!(h.rt.cancel(S).unwrap());
    until("refused", || {
        h.state_of(&format!("op_abandon_{a}")) == "rejected"
    })
    .await;
    assert_eq!(h.systems("放弃这次执行没有成功"), 1, "shown");
    assert!(h.rt.is_busy(S));

    assert!(h.rt.cancel(S).unwrap());
    assert!(h.rt.cancel(S).unwrap(), "the one on its way is not doubled");
    let n: i64 = h.q("SELECT COUNT(*) FROM node_commands WHERE method='abandon'");
    assert_eq!(n, 2);
    until("abandoned", || h.ends().len() == 1).await;
    assert_eq!(h.state_of(&format!("op_abandon_{a}_1")), "finished");
    assert!(h.rt.cancel(S).is_ok_and(|x| !x), "nothing left to stop");
}

/// Issue: Steve's lead on a released, idle session with nothing queued is queued and opens it.
#[tokio::test]
async fn a_lead_on_a_released_idle_session_opens_it() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;
    assert_eq!(
        h.rt.flush(S, Some(("go on", "steve"))).unwrap(),
        Planned::Busy
    );
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("open_session"), 2);
    assert!(h.prompt_of(1).contains("go on"));
}

/// Issue: a `Released` view from before the last open, delivered late, does not let the session
/// open now go.
#[tokio::test]
async fn a_late_release_view_does_not_let_a_reopened_session_go() {
    let h = hub();
    h.bind();
    h.one_turn("hello").await;
    // the stream is late from here on; answers still come
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;
    // a message reopens it, then is withdrawn before its turn: the session ends up idle and ready
    h.rt.prompt(S, "x", "user", &[]).unwrap();
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("DELETE FROM queued", [])?;
            Ok(())
        })
        .unwrap();
    until("ready", || h.phase() == "ready").await;
    let late = h.seen.stash.lock().unwrap().take().unwrap();
    assert!(!late.is_empty());
    for b in late {
        h.seen.to.commit(b).await.unwrap();
    }
    until("applied", || {
        h.q::<i64>("SELECT COUNT(*) FROM node_streams WHERE durable > applied") == 0
    })
    .await;
    assert_eq!(h.phase(), "ready", "the old view says nothing of this open");
    assert_eq!(h.count("open_session"), 2);
}

/// A business-ended session's release asked during its turn is done once the turn is over.
#[tokio::test]
async fn an_ended_session_is_let_go_when_its_turn_is_over() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("active", || h.phase() == "active").await;
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE sessions SET status='ended'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(!h.rt.release(S).unwrap(), "busy");
    let exec = h.attempts()[0].clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("released", || h.phase() == "released").await;
    assert_eq!(h.count("release_session"), 1);
}

/// D5 acceptance "回滚": with the Node not enabled, a Node-bound session's messages are queued
/// (never handed to another backend); once the Node is back they go after reconciliation.
#[tokio::test]
async fn a_parked_session_queues_and_goes_once_the_node_is_back() {
    let mut h = hub();
    h.bind();
    h.one_turn("hello").await;
    let store = h.store.clone();
    assert!(steve_hub::wire::node_runtime::bound(&store, S));
    assert!(!steve_hub::wire::node_runtime::bound(&store, "other"));
    steve_hub::wire::node_runtime::park(&store, S, "while off", "user", &[]).unwrap();
    steve_hub::wire::node_runtime::park(&store, S, "and more", "user", &[]).unwrap();
    assert_eq!(h.systems("STEVE_NODE_MODE"), 1, "told once");
    let queued: i64 = h.q("SELECT COUNT(*) FROM queued");
    assert_eq!(queued, 2);
    assert_eq!(h.count("start_turn"), 1);
    h.node.restart();
    h.restart_hub().await;
    until("parked messages go", || h.count("start_turn") == 2).await;
    assert!(h.prompt_of(1).contains("while off") && h.prompt_of(1).contains("and more"));
}

/// 断线或重启后不重跑: a Hub that restarts and gets the same permission request again makes no
/// second card and hands nothing to YOLO again (the new process has nothing armed for it either),
/// and nothing answers the Node on its own.
#[tokio::test]
async fn a_replayed_permission_after_a_restart_is_neither_a_new_card_nor_answered() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let exec = h.attempts()[0].clone();
    h.node
        .ask_permission(perm("p1", exec.clone(), "kill 4242"))
        .await
        .unwrap();
    until("card", || h.ends.3.lock().unwrap().len() == 1).await;
    let replay = h.seen.seen.lock().unwrap().clone();

    // a second Hub process on the same ledger
    let ends = Arc::new(Ends::default());
    let second = NodeRuntime::new(h.store.clone(), h.node.clone(), ends.clone());
    second.start();
    Sink(Arc::downgrade(&second))
        .commit(EventBatch { events: replay })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        ends.3.lock().unwrap().is_empty(),
        "no card handed to YOLO again"
    );
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM decisions WHERE subject_type='tool_call'"),
        1
    );
    assert_eq!(
        h.q::<String>("SELECT status FROM decisions WHERE subject_type='tool_call'"),
        "open"
    );
    assert!(!h.methods().contains(&"resolve_permission"));
}

// ── D5: permission card expiry, and no late answer or cancel on a newer attempt ──

impl H {
    fn perm_state(&self, decision: &str) -> String {
        self.q(&format!(
            "SELECT state FROM runtime_permissions WHERE decision_id='{decision}'"
        ))
    }

    fn status(&self, decision: &str) -> String {
        self.q(&format!(
            "SELECT status FROM decisions WHERE id='{decision}'"
        ))
    }

    fn commands(&self, method: &str) -> i64 {
        self.q(&format!(
            "SELECT COUNT(*) FROM node_commands WHERE method='{method}'"
        ))
    }

    fn cmd_state(&self, op: &str) -> String {
        self.q(&format!(
            "SELECT state FROM node_commands WHERE operation_id='{op}'"
        ))
    }

    fn cards(&self) -> Vec<String> {
        self.store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT id FROM decisions WHERE subject_type='tool_call' ORDER BY created, rowid",
                )?;
                let v = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap()
    }

    /// A turn running on the Node (its start accepted).
    async fn running(&self, text: &str) -> ExecutionRef {
        let n = self.attempts().len();
        self.rt.prompt(S, text, "user", &[]).unwrap();
        until("attempt", || self.attempts().len() == n + 1).await;
        until("started", || self.count("start_turn") == n + 1).await;
        // the Hub has the Node's answer too (a stop before it waits for it)
        until("accepted", || {
            self.q::<i64>(
                "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='pending'",
            ) == 0
        })
        .await;
        self.attempts().last().unwrap().clone()
    }

    /// The Hub stops and another one starts on the same store and Node after `pause`.
    async fn restart(&mut self, pause: Duration) {
        let gone = Arc::downgrade(&self.rt);
        let next = NodeRuntime::new(self.store.clone(), self.node.clone(), self.ends.clone());
        drop(std::mem::replace(&mut self.rt, next));
        until("old Hub gone", || gone.upgrade().is_none()).await;
        tokio::time::sleep(pause).await;
        self.node.connect(Arc::new(Sink(Arc::downgrade(&self.rt))));
        self.rt.start();
    }
}

#[tokio::test]
async fn a_card_expires_at_its_expires_at() {
    let h = hub();
    h.bind();
    let exec = h.running("hello").await;
    h.node
        .ask_permission(perm_ttl("p1", exec.clone(), "rm x", 300))
        .await
        .unwrap();
    until("card", || h.cards().len() == 1).await;
    let d = h.cards()[0].clone();
    assert_eq!(h.status(&d), "open");
    // only the Hub's clock: the Node says nothing
    until("expired", || h.status(&d) == "expired").await;
    assert_eq!(h.perm_state(&d), "expired");
    let op = format!("op_answer_{d}");
    until("reject sent", || h.cmd_state(&op) == "finished").await;
    assert_eq!(h.commands("resolve_permission"), 1);
    let decided: String = h.q(&format!(
        "SELECT json_extract(request,'$.body.decision') FROM node_commands WHERE operation_id='{op}'"
    ));
    assert_eq!(decided, "reject");
    assert_eq!(h.count("resolve_permission"), 1, "the Node took the reject");
    until("Node's resolution applied", || h.node.unacked() == 0).await;
    assert_eq!(
        h.status(&d),
        "expired",
        "the Node's confirmation leaves the card"
    );
    assert_eq!(h.perm_state(&d), "expired");
    assert!(!h.rt.answer_permission(&d, Some("allow_once")).unwrap());
    assert_eq!(h.commands("resolve_permission"), 1);
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || h.ends().len() == 1).await;
    assert_eq!(h.ends()[0].1, "end_turn");
}

#[tokio::test]
async fn a_request_already_past_expires_at_is_closed_at_once() {
    let h = hub();
    h.bind();
    let exec = h.running("hello").await;
    h.node
        .ask_permission(perm_ttl("p1", exec.clone(), "rm x", -1))
        .await
        .unwrap();
    until("card", || h.cards().len() == 1).await;
    let d = h.cards()[0].clone();
    until("expired", || h.status(&d) == "expired").await;
    assert!(h.ends.3.lock().unwrap().is_empty(), "never offered to YOLO");
    assert_eq!(
        h.commands("resolve_permission"),
        1,
        "the Node is told to reject"
    );
    assert_eq!(h.perm_state(&d), "expired");
}

#[tokio::test]
async fn a_refused_answer_is_not_an_answer() {
    let h = hub();
    h.bind();
    let exec = h.running("hello").await;
    h.node
        .ask_permission(perm_ttl("p1", exec.clone(), "rm x", 60_000))
        .await
        .unwrap();
    until("card", || h.cards().len() == 1).await;
    let d = h.cards()[0].clone();
    assert_eq!(
        h.ends.3.lock().unwrap().as_slice(),
        std::slice::from_ref(&d)
    );
    // the Node's clock is past the request's expiry
    h.node.set_now(steve_store::now() + 120_000);
    assert!(h.rt.answer_permission(&d, Some("allow_once")).unwrap());
    assert_eq!(h.perm_state(&d), "answering");
    // as the orchestrator does after a true
    h.store
        .transaction::<_, StoreError>(|tx| {
            steve_store::decision_resolve_in(
                tx,
                &d,
                &serde_json::json!({"optionId": "allow_once"}),
                "resolved",
            )?;
            Ok(())
        })
        .unwrap();
    until("refused", || {
        h.cmd_state(&format!("op_answer_{d}")) == "rejected"
    })
    .await;
    assert_eq!(h.perm_state(&d), "refused");
    assert_eq!(
        h.status(&d),
        "expired",
        "never claims an answer that was not applied"
    );
    let refused: String = h.q(&format!(
        "SELECT json_extract(result,'$.refused') FROM decisions WHERE id='{d}'"
    ));
    assert_eq!(refused, "expired");
}

#[tokio::test]
async fn an_overdue_card_expires_after_a_restart() {
    let mut h = hub();
    h.bind();
    let exec = h.running("hello").await;
    h.node
        .ask_permission(perm_ttl("p1", exec.clone(), "rm x", 400))
        .await
        .unwrap();
    until("card", || h.cards().len() == 1).await;
    let d = h.cards()[0].clone();
    h.restart(Duration::from_millis(600)).await;
    // the sweep at start closes it, no wait for a tick
    assert_eq!(h.status(&d), "expired");
    assert_eq!(h.commands("resolve_permission"), 1);
}

#[tokio::test]
async fn a_card_closes_when_its_attempt_ends_without_the_nodes_expiry() {
    let h = hub();
    h.bind();
    let exec = h.running("hello").await;
    h.node
        .ask_permission(perm("p1", exec.clone(), "rm x"))
        .await
        .unwrap();
    until("card", || h.cards().len() == 1).await;
    let d = h.cards()[0].clone();
    // a Node that ends the attempt and says nothing of its waits
    h.node
        .emit(
            Subject::Execution(exec.clone()),
            EventBody::AttemptEnded {
                completion: build::done(),
            },
        )
        .await
        .unwrap();
    until("closed", || h.status(&d) == "expired").await;
    assert_eq!(h.perm_state(&d), "expired");
    assert_eq!(h.commands("resolve_permission"), 0);
    assert!(!h.rt.answer_permission(&d, Some("allow_once")).unwrap());
}

#[tokio::test]
async fn a_late_answer_never_reaches_a_newer_attempt() {
    let h = hub();
    h.bind();
    h.node
        .refuse("resolve_permission", Some(ErrorCode::Offline));
    let a = h.running("one").await;
    h.node
        .ask_permission(perm("p1", a.clone(), "rm x"))
        .await
        .unwrap();
    until("card A", || h.cards().len() == 1).await;
    let da = h.cards()[0].clone();
    assert!(h.rt.answer_permission(&da, Some("allow_once")).unwrap());
    assert_eq!(h.perm_state(&da), "answering");
    h.node.finish(&a, build::done()).await.unwrap();
    until("turn A end", || h.ends().len() == 1).await;
    until("card A closed", || h.status(&da) == "expired").await;
    assert_eq!(h.perm_state(&da), "expired");

    // attempt B asks the very same thing under the very same id
    let b = h.running("two").await;
    h.node
        .ask_permission(perm("p1", b.clone(), "rm x"))
        .await
        .unwrap();
    until("card B", || h.cards().len() == 2).await;
    let db = h.cards()[1].clone();
    assert_ne!(da, db);
    assert_eq!(h.status(&db), "open");

    // the old answer gets through now: the Node refuses it, nothing of B moves
    h.node.refuse("resolve_permission", None);
    until("old answer refused", || {
        h.cmd_state(&format!("op_answer_{da}")) == "rejected"
    })
    .await;
    assert_eq!(h.perm_state(&da), "expired");
    assert_eq!(h.status(&db), "open");
    assert_eq!(h.perm_state(&db), "waiting");

    // B's own answer is its own command
    assert!(h.rt.answer_permission(&db, Some("allow_once")).unwrap());
    until("B applied", || h.perm_state(&db) == "applied").await;
    until("B resolved", || h.status(&db) == "resolved").await;
    assert_eq!(h.commands("resolve_permission"), 2);
    let applied = h
        .seen
        .seen
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match &e.body {
            EventBody::PermissionResolved { resolution } => Some(resolution.execution.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(applied, b);
}

#[tokio::test]
async fn an_old_cancel_never_hits_a_newer_attempt() {
    let h = hub();
    h.bind();
    h.node.refuse("cancel", Some(ErrorCode::Offline));
    let a = h.running("one").await;
    assert!(h.rt.cancel(S).unwrap());
    let a_id = a.attempt().to_string();
    let a_op: String = h.q(&format!(
        "SELECT operation_id FROM runtime_attempts WHERE attempt_id='{a_id}'"
    ));
    let (target_attempt, target): (String, String) = h
        .store
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT target_attempt, json_extract(request,'$.body.target') FROM node_commands WHERE method='cancel'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!((target_attempt, target), (a_id, a_op));
    // A ends normally before the cancel gets through
    h.node.finish(&a, build::done()).await.unwrap();
    until("turn A end", || h.ends().len() == 1).await;

    let b = h.running("two").await;
    h.node.refuse("cancel", None);
    until("cancel delivered", || h.count("cancel") == 1).await;
    let b_op: String = h.q(&format!(
        "SELECT operation_id FROM runtime_attempts WHERE attempt_id='{}'",
        b.attempt()
    ));
    assert!(
        !h.node
            .cancel_requested(&OperationId::try_from(b_op).unwrap())
    );
    let flagged: i64 = h.q(&format!(
        "SELECT cancel_requested FROM runtime_attempts WHERE attempt_id='{}'",
        b.attempt()
    ));
    assert_eq!(flagged, 0);
    h.node.finish(&b, build::done()).await.unwrap();
    until("turn B end", || h.ends().len() == 2).await;
    assert_eq!(h.ends()[1].1, "end_turn");
    assert_eq!(h.attempts().len(), 2);
    assert!(!h.rt.cancel(S).unwrap());
}

// ── D5: set_mode / configure_session for Node sessions ──

impl H {
    fn desired(&self) -> Option<String> {
        self.q(&format!(
            "SELECT desired_config FROM runtime_sessions WHERE session_id='{S}'"
        ))
    }

    fn mode(&self, p: PermissionMode) -> Configured {
        self.rt
            .configure(
                S,
                &ConfigChange {
                    permissions: Some(p),
                    ..Default::default()
                },
            )
            .unwrap()
    }

    /// The permission mode the last attempt was started with.
    fn last_mode(&self) -> String {
        self.q("SELECT json_extract(spec,'$.run.policy.permissions') FROM runtime_attempts ORDER BY created DESC, rowid DESC LIMIT 1")
    }

    fn cfg_states(&self) -> Vec<String> {
        self.store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT state FROM node_commands WHERE method='configure_session' ORDER BY created, rowid",
                )?;
                let v = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap()
    }

    fn said(&self, text: &str) -> bool {
        self.q::<i64>(&format!(
            "SELECT COUNT(*) FROM events WHERE kind='system' AND data LIKE '%{text}%'"
        )) > 0
    }

    async fn ready(&self) {
        until("ready", || self.phase() == "ready").await;
    }
}

#[tokio::test]
async fn an_idle_session_is_configured_through_the_node() {
    let h = hub();
    h.bind();
    h.ready().await;
    let known: String = h.q("SELECT json_extract(payload,'$.body.view.revision') FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='session_changed' ORDER BY rowid DESC LIMIT 1");
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    assert_eq!(h.phase(), "configuring");
    assert!(h.rt.is_busy(S));
    until("applied", || h.cfg_states() == ["finished"]).await;
    h.ready().await;
    assert_eq!(h.desired(), None);
    let expected: String =
        h.q("SELECT json_extract(request,'$.body.expected') FROM node_commands WHERE method='configure_session'");
    assert_eq!(expected, known);
    assert!(h.said("已切换"));
    // the next turn carries the new mode and the Node takes it
    h.one_turn("hello").await;
    assert_eq!(h.last_mode(), "full_access");
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Unchanged);
}

#[tokio::test]
async fn a_change_during_a_turn_waits_for_its_end_and_goes_before_the_next() {
    let h = hub();
    h.bind();
    let a = h.running("one").await;
    until("active", || h.phase() == "active").await;
    assert_eq!(h.rt.prompt(S, "two", "user", &[]).unwrap(), Planned::Busy);
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Pending);
    assert!(h.said("将在当前回合结束后生效"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.count("configure_session"),
        0,
        "never under a running attempt"
    );
    h.node.finish(&a, build::done()).await.unwrap();
    until("second start", || h.count("start_turn") == 2).await;
    let order: Vec<_> = h
        .methods()
        .into_iter()
        .filter(|m| matches!(*m, "start_turn" | "configure_session"))
        .collect();
    assert_eq!(order, ["start_turn", "configure_session", "start_turn"]);
    assert_eq!(h.last_mode(), "full_access");
    assert_eq!(h.desired(), None);
}

#[tokio::test]
async fn model_and_effort_changes_go_through_configure() {
    let h = hub();
    h.bind();
    h.ready().await;
    let c =
        h.rt.configure(
            S,
            &ConfigChange {
                model: Some(Some("m2".into())),
                effort: Some(Some("high".into())),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(c, Configured::Sent);
    until("applied", || h.cfg_states() == ["finished"]).await;
    h.ready().await;
    until("model recorded", || {
        h.q::<Option<String>>(&format!("SELECT model FROM sessions WHERE id='{S}'"))
            .as_deref()
            == Some("m2")
    })
    .await;
    let effort: String = h.q(&format!(
        "SELECT reasoning_effort FROM sessions WHERE id='{S}'"
    ));
    assert_eq!(effort, "high");
    h.one_turn("hello").await;
    let model: String = h.q("SELECT json_extract(spec,'$.run.choice.model') FROM runtime_attempts");
    assert_eq!(model, "m2");
    assert_eq!(h.last_mode(), "ask", "the mode stays");
}

#[tokio::test]
async fn a_lost_answer_sends_the_very_same_configure_again() {
    let h = hub();
    h.bind();
    h.ready().await;
    h.node.script_once("configure_session", Scripted::DropReply);
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("applied", || h.cfg_states() == ["finished"]).await;
    let leases: i64 =
        h.q("SELECT lease_generation FROM node_commands WHERE method='configure_session'");
    assert!(leases >= 2, "sent again: {leases}");
    assert_eq!(h.count("configure_session"), 1, "applied once");
    h.ready().await;
    h.one_turn("hello").await;
    assert_eq!(h.last_mode(), "full_access");
}

#[tokio::test]
async fn an_unknown_configure_holds_the_session_and_is_never_resent() {
    let h = hub();
    h.bind();
    h.ready().await;
    h.node.script_once("configure_session", Scripted::Unknown);
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("unknown", || h.cfg_states() == ["unknown"]).await;
    assert_eq!(h.phase(), "configuring");
    assert!(h.rt.is_busy(S));
    assert!(h.rt.running_turns() >= 1, "a drain waits for it");
    assert_eq!(h.rt.prompt(S, "hello", "user", &[]).unwrap(), Planned::Busy);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(h.count("configure_session"), 1);
    assert_eq!(h.cfg_states(), ["unknown"]);
    assert_eq!(h.count("start_turn"), 0);
    assert!(h.said("结果未知"));
}

#[tokio::test]
async fn a_stale_revision_is_refreshed_with_a_new_operation() {
    let h = hub();
    h.bind();
    h.ready().await;
    // the Hub's last view of the session is behind the Node's
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE node_event_inbox SET payload=json_set(payload,'$.body.view.revision','r0') WHERE json_extract(payload,'$.body.type')='session_changed'", [])?;
            tx.execute("UPDATE runtime_sessions SET node_revision='r0'", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("refreshed", || h.cfg_states() == ["rejected", "finished"]).await;
    h.ready().await;
    assert_eq!(h.desired(), None);
    assert_eq!(h.count("configure_session"), 1);
}

/// Issue: the Node's revision of the session outlives the inbox (pruned): a config change still
/// goes with it.
#[tokio::test]
async fn a_configure_needs_no_session_view_left_in_the_inbox() {
    let h = hub();
    h.bind();
    h.ready().await;
    // "ready" comes with the open's receipt; the session view that carries the revision is
    // applied on its own, so wait for it before pruning the inbox under it
    until("revision kept", || {
        h.q::<Option<String>>("SELECT node_revision FROM runtime_sessions")
            .is_some()
    })
    .await;
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("DELETE FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='session_changed'", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("applied", || h.cfg_states() == ["finished"]).await;
    h.ready().await;
    assert_eq!(h.desired(), None);
}

#[tokio::test]
async fn refreshes_are_bounded() {
    let h = hub();
    h.bind();
    h.ready().await;
    for _ in 0..3 {
        h.node.script_once(
            "configure_session",
            Scripted::Refuse(ApiError::new(
                ErrorCode::Conflict,
                "the session changed since",
            )),
        );
    }
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("given up", || h.said("未能切换")).await;
    assert_eq!(h.cfg_states(), ["rejected", "rejected", "rejected"]);
    h.ready().await;
    assert_eq!(h.desired(), None, "a widening falls back");
    h.one_turn("hello").await;
    assert_eq!(h.last_mode(), "ask");
}

#[tokio::test]
async fn a_refused_widening_falls_back_and_a_refused_narrowing_releases() {
    let h = hub();
    h.bind();
    h.ready().await;
    h.node.script_once(
        "configure_session",
        Scripted::Refuse(ApiError::new(ErrorCode::Unsupported, "no")),
    );
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("refused", || h.cfg_states() == ["rejected"]).await;
    h.ready().await;
    assert_eq!(h.desired(), None);
    assert!(h.said("未能切换"));
    h.one_turn("one").await;
    assert_eq!(h.last_mode(), "ask");

    // now wide, then a narrowing the Node refuses: the session is let go and opens narrow
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("wide", || h.cfg_states() == ["rejected", "finished"]).await;
    h.ready().await;
    h.node.script_once(
        "configure_session",
        Scripted::Refuse(ApiError::new(ErrorCode::Unsupported, "no")),
    );
    assert_eq!(h.mode(PermissionMode::Ask), Configured::Sent);
    until("released", || h.phase() == "released").await;
    assert!(h.desired().is_some(), "the narrowing holds");
    assert_eq!(h.count("release_session"), 1);
    let exec = h.running("two").await;
    let opened: String = h.q("SELECT json_extract(request,'$.body.policy.permissions') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1");
    assert_eq!(opened, "ask");
    assert_eq!(h.last_mode(), "ask", "never a turn wider than asked");
    assert_eq!(h.desired(), None);
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || h.ends().len() == 2).await;
}

#[tokio::test]
async fn a_released_session_takes_the_change_when_it_opens_again() {
    let h = hub();
    h.bind();
    h.ready().await;
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::OnReopen);
    h.one_turn("hello").await;
    assert_eq!(h.count("configure_session"), 0);
    let opened: String = h.q("SELECT json_extract(request,'$.body.policy.permissions') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1");
    assert_eq!(opened, "full_access");
    assert_eq!(h.last_mode(), "full_access");
    assert_eq!(h.desired(), None);
}

#[tokio::test]
async fn a_pending_change_skips_the_automatic_retry() {
    let h = hub();
    h.bind();
    let a = h.running("hello").await;
    assert_eq!(h.mode(PermissionMode::Plan), Configured::Pending);
    let mut c = build::done();
    c.outcome = Outcome::Failed;
    c.stop_reason = "error".into();
    c.transient = true;
    h.node.finish(&a, c).await.unwrap();
    until("turn end", || h.ends().len() == 1).await;
    assert!(h.said("不自动重试"));
    until("configure", || h.count("configure_session") == 1).await;
    assert_eq!(h.attempts().len(), 1, "no second attempt with the old mode");
    until("applied", || h.cfg_states() == ["finished"]).await;
    h.ready().await;
    assert_eq!(h.desired(), None);
}

#[tokio::test]
async fn no_configure_goes_out_under_hold() {
    let h = hub();
    h.bind();
    h.ready().await;
    h.rt.hold(true);
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Pending);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.commands("configure_session"), 0);
    h.rt.hold(false);
    h.one_turn("hello").await;
    let order: Vec<_> = h
        .methods()
        .into_iter()
        .filter(|m| matches!(*m, "start_turn" | "configure_session"))
        .collect();
    assert_eq!(order, ["configure_session", "start_turn"]);
    assert_eq!(h.last_mode(), "full_access");
}

#[tokio::test]
async fn a_desired_change_survives_a_hub_restart() {
    let mut h = hub();
    h.bind();
    let a = h.running("hello").await;
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Pending);
    h.restart(Duration::ZERO).await;
    h.node.finish(&a, build::done()).await.unwrap();
    until("configure", || h.count("configure_session") == 1).await;
    until("applied", || h.cfg_states() == ["finished"]).await;
    h.ready().await;
    assert_eq!(h.desired(), None);
}

// ── queued messages of a Node session: hold, send-now, edit and delete ──

impl H {
    /// The text of every `start_turn` the Hub wrote, in order (sent or not).
    fn turn_texts(&self) -> Vec<String> {
        self.store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT json_extract(request,'$.body.run.prompt[0].text') FROM node_commands WHERE method='start_turn' ORDER BY created, rowid",
                )?;
                let v = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap()
    }

    fn queued(&self) -> i64 {
        self.q(&format!(
            "SELECT COUNT(*) FROM queued WHERE session_id='{S}'"
        ))
    }

    fn queued_id(&self, text: &str) -> String {
        self.q(&format!(
            "SELECT id FROM queued WHERE session_id='{S}' AND text='{text}'"
        ))
    }

    async fn transient_failure(&self, exec: &ExecutionRef) {
        let mut c = build::done();
        c.outcome = Outcome::Failed;
        c.stop_reason = "error".into();
        c.transient = true;
        let n = self.attempts().len();
        self.node.finish(exec, c).await.unwrap();
        until("retry planned", || self.attempts().len() == n + 1).await;
    }
}

#[tokio::test]
async fn hold_keeps_the_queue_through_a_turn_end() {
    // D5 acceptance: hold 下队列不被排空
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("active", || h.phase() == "active").await;
    h.rt.hold(true);
    assert_eq!(h.rt.prompt(S, "two", "user", &[]).unwrap(), Planned::Held);
    let first = h.attempts()[0].clone();
    h.node.finish(&first, build::done()).await.unwrap();
    until("ready", || h.phase() == "ready").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.count("start_turn"), 1, "nothing starts under the hold");
    assert_eq!(h.queued(), 1, "the queue is kept");
    let dispatched: i64 = h.q("SELECT COUNT(*) FROM session_dispatch WHERE text='two'");
    assert_eq!(dispatched, 0);
    assert_eq!(h.phase(), "ready");

    h.rt.hold(false);
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Created);
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.turn_texts()[1], "two");
    assert_eq!(h.queued(), 0);
}

#[tokio::test]
async fn hold_keeps_the_queue_through_open_and_release_receipts() {
    let h = hub();
    // a bind's open receipt under the hold claims nothing
    h.rt.hold(true);
    h.bind();
    assert_eq!(h.rt.prompt(S, "one", "user", &[]).unwrap(), Planned::Held);
    until("ready", || h.phase() == "ready").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.count("start_turn"), 0);
    assert_eq!(h.queued(), 1);
    h.rt.hold(false);
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Created);
    until("first turn", || h.count("start_turn") == 1).await;
    let exec = h.attempts()[0].clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("ready again", || {
        h.phase() == "ready" && !h.ends().is_empty()
    })
    .await;

    // a release receipt under the hold neither claims nor reopens
    assert!(h.rt.release(S).unwrap());
    h.rt.hold(true);
    assert_eq!(h.rt.prompt(S, "two", "user", &[]).unwrap(), Planned::Held);
    until("released", || h.phase() == "released").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        h.count("open_session"),
        1,
        "not opened again under the hold"
    );
    assert_eq!(h.count("start_turn"), 1);
    assert_eq!(h.queued(), 1);

    h.rt.hold(false);
    assert_eq!(
        h.rt.flush(S, None).unwrap(),
        Planned::Busy,
        "it opens first"
    );
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.count("open_session"), 2);
    assert_eq!(h.turn_texts()[1], "two");
}

#[tokio::test]
async fn persisted_maintenance_holds_node_turns() {
    let h = hub();
    // set before the restart: nobody calls hold, the store remembers
    h.store.maintenance_set(true).unwrap();
    h.bind();
    assert!(h.rt.is_holding());
    assert_eq!(h.rt.prompt(S, "one", "user", &[]).unwrap(), Planned::Held);
    until("ready", || h.phase() == "ready").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.count("start_turn"), 0);
    assert_eq!(h.queued(), 1);
    h.store.maintenance_set(false).unwrap();
    assert!(!h.rt.is_holding());
    assert_eq!(h.rt.flush(S, None).unwrap(), Planned::Created);
    until("turn", || h.count("start_turn") == 1).await;
}

#[tokio::test]
async fn a_claimed_message_cannot_be_edited_removed_or_moved() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("active", || h.phase() == "active").await;
    h.rt.prompt(S, "two", "user", &[]).unwrap();
    let id = h.queued_id("two");
    let first = h.attempts()[0].clone();
    h.node.finish(&first, build::done()).await.unwrap();
    until("second turn", || h.count("start_turn") == 2).await;

    let queued = h.store.queued();
    assert!(queued.edit(S, &id, "changed").unwrap().is_none());
    assert!(queued.remove(S, &id).unwrap().is_none());
    assert!(queued.to_front(S, &id).unwrap().is_none());
    let dispatched: String =
        h.q("SELECT text FROM session_dispatch WHERE backend='node' ORDER BY rowid DESC LIMIT 1");
    assert_eq!(dispatched, "two");
    assert_eq!(h.turn_texts()[1], "two");

    // a retry sends the frozen prompt, not the queue
    let second = h.attempts()[1].clone();
    h.transient_failure(&second).await;
    assert_eq!(h.turn_texts(), ["one", "two", "two"]);
}

#[tokio::test]
async fn an_edit_before_the_claim_is_what_the_turn_sends() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("active", || h.phase() == "active").await;
    h.rt.prompt(S, "two", "user", &[]).unwrap();
    let id = h.queued_id("two");
    assert!(h.store.queued().edit(S, &id, "two'").unwrap().is_some());
    let first = h.attempts()[0].clone();
    h.node.finish(&first, build::done()).await.unwrap();
    until("second turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.turn_texts(), ["one", "two'"]);
}

#[tokio::test]
async fn an_uncertain_turn_reports_uncertain_and_nothing_abandons_it_implicitly() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("active", || h.phase() == "active").await;
    assert!(!h.rt.is_uncertain(S));
    let exec = h.attempts()[0].clone();
    let (_, op) = h
        .node
        .log()
        .into_iter()
        .find(|(m, _)| *m == "start_turn")
        .unwrap();
    h.node.lose_track(&exec).unwrap();
    h.node
        .emit(
            Subject::Execution(exec.clone()),
            EventBody::OperationChanged {
                receipt: Receipt {
                    operation: op,
                    state: Observed::Unknown {
                        reason: "node restarted".into(),
                    },
                },
            },
        )
        .await
        .unwrap();
    until("uncertain", || h.rt.is_uncertain(S)).await;
    h.rt.prompt(S, "next", "user", &[]).unwrap();
    assert_ne!(h.rt.flush(S, None).unwrap(), Planned::Created);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let abandons: i64 =
        h.q("SELECT COUNT(*) FROM node_commands WHERE method IN ('abandon','cancel')");
    assert_eq!(abandons, 0, "only an explicit cancel gives it up");
    assert_eq!(h.count("start_turn"), 1, "never sent again");
    assert_eq!(h.queued(), 1);
}

#[tokio::test]
async fn cancel_in_retry_wait_never_sends_the_pending_attempt() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("started", || h.count("start_turn") == 1).await;
    let first = h.attempts()[0].clone();
    h.transient_failure(&first).await;
    let retry: String =
        h.q("SELECT operation_id FROM node_commands WHERE method='start_turn' ORDER BY created DESC, rowid DESC LIMIT 1");
    assert_eq!(h.rt.prompt(S, "two", "user", &[]).unwrap(), Planned::Busy);

    assert!(h.rt.cancel(S).unwrap(), "the waiting retry is cancelled");
    let state: String = h.q(&format!(
        "SELECT state FROM node_commands WHERE operation_id='{retry}'"
    ));
    assert_eq!(state, "rejected", "settled here, never sent");
    let cancels: i64 =
        h.q("SELECT COUNT(*) FROM node_commands WHERE method IN ('cancel','abandon')");
    assert_eq!(cancels, 0);
    let dispatched: String =
        h.q("SELECT state FROM session_dispatch WHERE backend='node' AND text='one'");
    assert_eq!(dispatched, "cancelled");
    // what waited goes next, a new turn
    until("next turn", || h.count("start_turn") == 2).await;
    assert_eq!(h.turn_texts(), ["one", "one", "two"]);
    assert!(
        !h.node.log().iter().any(|(_, o)| o.as_str() == retry),
        "the retry never reached the Node"
    );
    assert!(h.ends().is_empty(), "the item goes on with the next turn");
    // the cancelled retry never started: it has no statistics row
    let retry_rows: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM agent_turns t JOIN runtime_attempts a ON a.attempt_id = t.node_attempt_id WHERE a.operation_id='{retry}'"
    ));
    assert_eq!(retry_rows, 0);
    let exec = h.attempts().last().unwrap().clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.ends().len(), 1);
}

#[tokio::test]
async fn cancel_in_retry_wait_with_nothing_queued_tells_the_item_once() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("started", || h.count("start_turn") == 1).await;
    let first = h.attempts()[0].clone();
    h.transient_failure(&first).await;
    assert!(h.rt.cancel(S).unwrap());
    until("turn end", || !h.ends().is_empty()).await;
    assert_eq!(h.ends(), [(S.to_string(), "cancelled".to_string(), None)]);
    assert_eq!(h.phase(), "ready");
    assert!(!h.rt.is_busy(S));
    assert!(!h.rt.cancel(S).unwrap(), "nothing left to cancel");
}

/// B2b review: a turn ended by a local cancel is told before a newer turn whose end the same
/// apply pass reaches. The worker is held mid-pass; meanwhile the waiting retry is cancelled here
/// (its end is left for the worker), a new turn runs and finishes. The item hears the cancelled
/// turn first, then the new one: never an older end after a newer one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_locally_settled_end_is_told_before_a_newer_turns_end() {
    use std::sync::atomic::Ordering::SeqCst;
    let h = hub();
    h.bind();
    h.rt.prompt(S, "one", "user", &[]).unwrap();
    until("started", || h.count("start_turn") == 1).await;
    let first = h.attempts()[0].clone();
    h.ends.5.store(true, SeqCst);
    h.transient_failure(&first).await;
    until("worker held", || h.ends.6.load(SeqCst)).await;
    let retry = h.attempts()[1].clone();

    assert!(h.rt.cancel(S).unwrap(), "the waiting retry is settled here");
    assert_eq!(
        h.rt.prompt(S, "two", "user", &[]).unwrap(),
        Planned::Created
    );
    until("second sent", || h.count("start_turn") == 2).await;
    let second = h.attempts().last().unwrap().clone();
    assert!(
        !h.rt
            .same_turn(retry.attempt().as_str(), second.attempt().as_str())
            .unwrap()
    );
    h.node.finish(&second, build::done()).await.unwrap();
    h.ends.5.store(false, SeqCst);

    until("both ends", || h.ends().len() == 2).await;
    assert_eq!(
        *h.ends.4.lock().unwrap(),
        [
            Some(retry.attempt().to_string()),
            Some(second.attempt().to_string())
        ],
        "in commit order"
    );
    assert_eq!(h.ends()[0].1, "cancelled");
    assert_eq!(h.ends()[1].1, "end_turn");
}

/// Issue: a start_turn the Node refused before accepting (no capacity), stopped while it waits to
/// go again, never ran: it is settled here and never goes out again (no cancel the Node would
/// refuse for a target it does not know).
#[tokio::test]
async fn a_start_turn_refused_before_acceptance_is_settled_by_stop() {
    let h = hub();
    h.node.set_capacity(Capacity {
        sessions: 0,
        jobs: 0,
        execs: 0,
    });
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("leased", || {
        h.q::<i64>(
            "SELECT COALESCE(MAX(lease_generation),0) FROM node_commands WHERE method='start_turn'",
        ) >= 1
    })
    .await;
    assert!(h.rt.cancel(S).unwrap());
    until("cancelled", || !h.ends().is_empty()).await;
    assert_eq!(h.ends()[0].1, "cancelled");
    assert_eq!(
        h.q::<String>("SELECT state FROM node_commands WHERE method='start_turn'"),
        "rejected"
    );
    let cancels: i64 = h.q("SELECT COUNT(*) FROM node_commands WHERE method='cancel'");
    assert_eq!(cancels, 0, "nothing to cancel on the Node");
    h.node.set_capacity(Capacity {
        sessions: 8,
        jobs: 8,
        execs: 8,
    });
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    assert_eq!(h.count("start_turn"), 0, "never sent again");
    assert!(!h.rt.is_busy(S));
    assert!(
        h.stat_rows().is_empty(),
        "it never started: no statistics row"
    );
}

/// Issue: a start_turn whose send was uncertain may be at the Node: the cancel waits until the
/// Node is known to hold it, then goes (never refused as an unknown target and lost).
#[tokio::test]
async fn a_cancel_after_an_uncertain_send_goes_once_the_node_holds_the_turn() {
    let h = hub();
    h.bind();
    h.ready().await;
    h.node.refuse("start_turn", Some(ErrorCode::OutcomeUnknown));
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("uncertain", || {
        h.q::<i64>("SELECT COALESCE(MAX(maybe_sent),0) FROM node_commands WHERE method='start_turn' AND state='pending' AND lease_owner IS NULL") == 1
    })
    .await;
    assert!(h.rt.cancel(S).unwrap());
    assert_eq!(
        h.commands("cancel"),
        0,
        "held back: the Node may not know it yet"
    );
    assert!(h.rt.is_busy(S), "it may run: never settled here");
    h.node.refuse("start_turn", None);
    until("cancel sent", || h.count("cancel") == 1).await;
    let op: String = h.q("SELECT operation_id FROM node_commands WHERE method='start_turn'");
    assert!(h.node.cancel_requested(&OperationId::try_from(op).unwrap()));
}

/// Issue: a start_turn whose first send was uncertain and whose next send was refused before
/// acceptance (busy) was settled by a stop as never sent, though the first send may have reached the
/// Node: the cancel goes to the Node instead, and the attempt is not ended here.
#[tokio::test]
async fn a_refusal_after_an_uncertain_send_still_sends_the_cancel() {
    let h = hub();
    h.bind();
    h.ready().await;
    // the Node's stream is late: nothing says the turn started
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    h.node.script_once("start_turn", Scripted::DropReply);
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    let uncertain = "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND state='pending' AND lease_owner IS NULL AND maybe_sent=1";
    until("uncertain", || h.q::<i64>(uncertain) == 1).await;
    let generation: i64 =
        h.q("SELECT lease_generation FROM node_commands WHERE method='start_turn'");
    assert!(h.rt.cancel(S).unwrap());
    assert_eq!(
        h.commands("cancel"),
        0,
        "held back: the Node may not know it yet"
    );
    h.node.refuse("start_turn", Some(ErrorCode::Busy));
    until("refused", || {
        h.q::<i64>(&format!(
            "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND lease_owner IS NULL AND lease_generation>{generation}"
        )) == 1
    })
    .await;
    assert_eq!(h.q::<i64>(uncertain), 1, "still maybe sent");
    assert!(h.ends().is_empty(), "never settled here");
    assert!(h.rt.is_busy(S));
    h.node.refuse("start_turn", None);
    let late = h.seen.stash.lock().unwrap().take().unwrap();
    for b in late {
        h.seen.to.commit(b).await.unwrap();
    }
    until("cancel sent", || h.count("cancel") == 1).await;
    let op: String = h.q("SELECT operation_id FROM node_commands WHERE method='start_turn'");
    assert!(h.node.cancel_requested(&OperationId::try_from(op).unwrap()));
    assert_eq!(h.count("start_turn"), 1, "it ran once");
}

/// Issue: Stop after a cancel the Node refused asks again with a new operation.
#[tokio::test]
async fn a_second_stop_after_a_refused_cancel_asks_again() {
    let h = hub();
    h.bind();
    let _a = h.running("one").await;
    h.node
        .refuse_next("cancel", ApiError::new(ErrorCode::Invalid, "not now"));
    assert!(h.rt.cancel(S).unwrap());
    until("refused", || {
        h.q::<i64>("SELECT COUNT(*) FROM node_commands WHERE method='cancel' AND state='rejected'")
            == 1
    })
    .await;
    assert!(h.rt.cancel(S).unwrap());
    assert_eq!(h.commands("cancel"), 2);
    until("cancel sent", || h.count("cancel") == 1).await;
}

/// Issue: an attempt reconciled as unknown because the Node holds no record of its start_turn is
/// settled when the Node refuses its abandon for that reason: the session is free again and the
/// attempt is never sent again.
#[tokio::test]
async fn an_attempt_the_node_has_no_record_of_ends_on_stop() {
    let mut h = hub();
    h.bind();
    h.rt.prompt(S, "first", "user", &[]).unwrap();
    until("running", || {
        h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 1
    })
    .await;
    let op: String = h.q("SELECT operation_id FROM node_commands WHERE method='start_turn'");
    h.node.restart();
    h.node.forget(&OperationId::try_from(op).unwrap());
    h.restart_hub().await;
    assert_eq!(
        h.q::<String>("SELECT state FROM runtime_attempts"),
        "unknown"
    );
    assert!(h.rt.is_busy(S));
    assert!(h.rt.cancel(S).unwrap());
    until("settled", || h.ends().len() == 1).await;
    assert_eq!(h.ends()[0].1, "error");
    assert!(!h.rt.is_busy(S));
    assert_eq!(h.rt.running_turns(), 0);
    assert_eq!(h.systems("没有这次执行的记录"), 1);
    // L4: settled by the Hub, the attempt's statistics row ends with that outcome
    until("stats row ended", || {
        h.stat_rows().first().is_some_and(|r| r.0.is_some())
    })
    .await;
    assert_eq!(h.stat_rows(), [(h.stat_rows()[0].0, Some("error".into()))]);
    assert_eq!(h.count("start_turn"), 1, "never sent again");
    assert!(h.rt.cancel(S).is_ok_and(|x| !x), "nothing left to stop");
}

/// N1-3: the Node's journal is made again mid-session (its home deleted). What the Hub sent is no
/// record there, so reconciling makes the running attempt unknown and never sends it again; the
/// new journal's events on the same session stream come in a new epoch with new ids, so the Hub
/// takes them beside the old ones without a conflict and the Node's delivery drains.
#[tokio::test]
async fn a_new_node_journal_mid_session_is_taken_in_without_a_conflict() {
    let mut h = hub();
    h.bind();
    h.one_turn("hello").await;
    h.rt.prompt(S, "second", "user", &[]).unwrap();
    until("running", || {
        h.q::<i64>("SELECT COUNT(*) FROM runtime_attempts WHERE state='running'") == 1
    })
    .await;
    let old_epochs: Vec<String> = h
        .store
        .reader(|c| {
            let mut st = c.prepare("SELECT DISTINCT epoch FROM node_event_inbox")?;
            let v = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        })
        .unwrap();
    assert_eq!(old_epochs, ["e1"]);

    h.node.reset_journal();
    h.restart_hub().await;
    assert_eq!(
        h.q::<String>(
            "SELECT state FROM runtime_attempts ORDER BY created DESC, rowid DESC LIMIT 1"
        ),
        "unknown",
        "the Node has no record of the running attempt"
    );
    assert!(h.rt.is_busy(S));

    // the new journal speaks on the same session stream again, from seq 1
    let failures = h.node.delivery_failures();
    build::ready(&h.node, S).await;
    until("delivered", || h.node.unacked() == 0).await;
    assert_eq!(h.node.delivery_failures(), failures, "no batch was refused");
    let stream = derived_id("session", S);
    let fresh: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM node_event_inbox WHERE stream='{stream}' AND epoch='j1' AND seq=1"
    ));
    assert_eq!(fresh, 1, "the new epoch is taken in");
    let old: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM node_event_inbox WHERE stream='{stream}' AND epoch='e1' AND seq=1"
    ));
    assert_eq!(old, 1, "beside the old one");
    assert_eq!(
        h.count("start_turn"),
        2,
        "the unknown attempt is never sent again"
    );
}

/// Issue: an AttemptStarted applied after reconciliation made the attempt unknown leaves it
/// unknown (Stop still abandons it).
#[tokio::test]
async fn a_late_attempt_started_never_revives_an_unknown_attempt() {
    let h = hub();
    h.bind();
    let a = h.running("one").await;
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE runtime_attempts SET state='unknown'", [])?;
            Ok(())
        })
        .unwrap();
    h.node
        .emit(Subject::Execution(a.clone()), EventBody::AttemptStarted)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        h.q::<String>("SELECT state FROM runtime_attempts"),
        "unknown"
    );
    assert!(h.rt.is_uncertain(S));
}

// ── sessions.status and the harness id ──

impl H {
    fn session_status(&self) -> String {
        self.q(&format!("SELECT status FROM sessions WHERE id='{S}'"))
    }

    fn acp_id(&self) -> Option<String> {
        self.q(&format!(
            "SELECT acp_session_id FROM sessions WHERE id='{S}'"
        ))
    }
}

/// A Node session's row says what the legacy runner's says: running from an attempt's start
/// until its turn is over, then idle; and it keeps the harness's own session id (ST-28).
#[tokio::test]
async fn a_turn_sets_the_session_running_then_idle_and_keeps_the_harness_id() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("running", || h.session_status() == "running").await;
    let view = h
        .node
        .session(SessionId::try_from(S.to_string()).unwrap())
        .await
        .unwrap();
    let resume = view.resume.expect("the fake Node has a resume handle");
    until("harness id", || h.acp_id().is_some()).await;
    assert_eq!(h.acp_id().as_deref(), Some(resume.as_str()));
    let exec = h.attempts()[0].clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || h.ends().len() == 1).await;
    assert_eq!(h.session_status(), "idle");

    // a message during a turn: the session stays running across the turn end that has a next
    h.rt.prompt(S, "two", "user", &[]).unwrap();
    until("second running", || h.attempts().len() == 2).await;
    until("running again", || h.session_status() == "running").await;
    h.rt.prompt(S, "three", "user", &[]).unwrap();
    until("second started", || h.count("start_turn") == 2).await;
    let second = h.attempts()[1].clone();
    h.node.finish(&second, build::done()).await.unwrap();
    until("third planned", || h.attempts().len() == 3).await;
    assert_eq!(h.session_status(), "running", "a turn follows");
    until("third started", || h.count("start_turn") == 3).await;
    let third = h.attempts()[2].clone();
    h.node.finish(&third, build::done()).await.unwrap();
    until("idle", || h.session_status() == "idle").await;
}

#[tokio::test]
async fn a_failed_turn_leaves_the_session_in_error_and_a_retry_stays_running() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || h.methods().contains(&"start_turn")).await;
    let first = h.attempts()[0].clone();
    let mut c = build::done();
    c.outcome = Outcome::Failed;
    c.stop_reason = "error".into();
    c.transient = true;
    h.node.finish(&first, c).await.unwrap();
    until("retry planned", || h.attempts().len() == 2).await;
    assert_eq!(h.session_status(), "running", "a retry is the same turn");
    h.rt.cancel(S).unwrap();
    until("cancelled", || h.ends().len() == 1).await;
    assert_eq!(h.session_status(), "idle");

    h.rt.prompt(S, "again", "user", &[]).unwrap();
    until("second started", || h.count("start_turn") == 2).await;
    let exec = h.attempts().last().unwrap().clone();
    let mut c = build::done();
    c.outcome = Outcome::Failed;
    c.stop_reason = "error".into();
    c.acted = true;
    h.node.finish(&exec, c).await.unwrap();
    until("turn end", || h.ends().len() == 2).await;
    assert_eq!(h.session_status(), "error");
}

#[tokio::test]
async fn an_ended_session_stays_ended_through_its_last_turn() {
    let h = hub();
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("running", || h.session_status() == "running").await;
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE sessions SET status='ended'", [])?;
            Ok(())
        })
        .unwrap();
    let exec = h.attempts()[0].clone();
    h.node.finish(&exec, build::done()).await.unwrap();
    until("released", || h.phase() == "released").await;
    assert_eq!(h.session_status(), "ended");
}

/// An attempt nobody can tell the end of after a restart is still running as far as the session
/// row goes, until Stop gives it up.
#[tokio::test]
async fn an_unknown_attempt_keeps_the_session_running_until_it_is_abandoned() {
    let mut h = hub();
    h.bind();
    h.rt.prompt(S, "first", "user", &[]).unwrap();
    until("running", || h.session_status() == "running").await;
    h.node.restart();
    h.restart_hub().await;
    assert_eq!(
        h.q::<String>("SELECT state FROM runtime_attempts"),
        "unknown"
    );
    assert_eq!(h.session_status(), "running");
    assert!(h.rt.cancel(S).unwrap());
    until("abandoned", || h.ends().len() == 1).await;
    assert_eq!(h.session_status(), "idle");
}

/// Review L2: a turn is under way from the moment it is planned, not from the Node's
/// AttemptStarted. A restart while the Node is still opening or loading the harness leaves the
/// attempt unknown before it ever started; the row must not keep the last turn's `idle`.
#[tokio::test]
async fn an_attempt_unknown_before_it_started_keeps_the_session_running() {
    let mut h = hub();
    h.bind();
    h.one_turn("first").await;
    assert_eq!(h.session_status(), "idle");
    assert!(h.acp_id().is_some());
    // the Node's facts are late from here on: its AttemptStarted never reaches this Hub
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    h.rt.prompt(S, "second", "user", &[]).unwrap();
    until("second planned", || h.attempts().len() == 2).await;
    assert_eq!(
        h.session_status(),
        "running",
        "the harness has a session already"
    );
    until("sent", || h.count("start_turn") == 2).await;
    assert_eq!(
        h.q::<String>(
            "SELECT state FROM runtime_attempts ORDER BY created DESC, rowid DESC LIMIT 1"
        ),
        "planned"
    );
    let late = h.seen.stash.lock().unwrap().take().unwrap();
    h.node.restart();
    h.restart_hub().await;
    assert_eq!(
        h.q::<String>(
            "SELECT state FROM runtime_attempts ORDER BY created DESC, rowid DESC LIMIT 1"
        ),
        "unknown"
    );
    assert!(h.rt.is_uncertain(S));
    assert_eq!(h.session_status(), "running");
    // what the Node said before its restart arrives late (its AttemptStarted among it): it
    // changes nothing, and the stream goes on
    assert!(!late.is_empty());
    for b in late {
        h.seen.to.commit(b).await.unwrap();
    }
    assert_eq!(h.session_status(), "running");
    assert!(h.rt.cancel(S).unwrap());
    until("abandoned", || h.ends().len() == 2).await;
    assert_eq!(h.session_status(), "idle");
}

/// Before the harness has a session of its own, a planned turn shows `starting`, as the legacy
/// runner's; its start makes it `running`.
#[tokio::test]
async fn a_first_turn_is_starting_until_its_attempt_starts() {
    let h = hub();
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE sessions SET status='idle'", [])?;
            Ok(())
        })
        .unwrap();
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    h.bind();
    h.rt.prompt(S, "hello", "user", &[]).unwrap();
    until("planned", || h.attempts().len() == 1).await;
    assert_eq!(h.acp_id(), None);
    assert_eq!(h.session_status(), "starting");
    let late = h.seen.stash.lock().unwrap().take().unwrap();
    for b in late {
        h.seen.to.commit(b).await.unwrap();
    }
    until("running", || h.session_status() == "running").await;
}

/// N1-3 review: a recreated journal hands out revisions the old one also had. A `Released` of the
/// new journal from before the last open, delivered late, says nothing of that open, even when
/// the old journal holds a view with the open's revision: the session open now does not go.
#[tokio::test]
async fn a_late_release_of_a_new_journal_does_not_let_a_reopened_session_go() {
    let mut h = hub();
    h.bind();
    // the old journal: a turn, a release, a reopen (its views stay in the Hub's inbox)
    h.one_turn("hello").await;
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;
    let reopen = |h: &H, text: &str| {
        h.rt.prompt(S, text, "user", &[]).unwrap();
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("DELETE FROM queued", [])?;
                Ok(())
            })
            .unwrap();
    };
    reopen(&h, "x");
    until("ready", || h.phase() == "ready").await;
    let old: Vec<String> = h
        .store
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT json_extract(payload,'$.body.view.revision') FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='session_changed'",
            )?;
            let v = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        })
        .unwrap();

    // the same again on a new journal, its stream late from the release on
    h.node.reset_journal();
    h.restart_hub().await;
    assert_eq!(h.phase(), "released");
    // the new journal has the workspace again (the Hub making it again is not this case)
    h.node
        .prepare_workspace(build::cmd(
            "op_prep_again",
            build::prepare(&format!("item:{S}")),
        ))
        .await
        .unwrap();
    h.one_turn("again").await;
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    assert!(h.rt.release(S).unwrap());
    until("released", || h.phase() == "released").await;
    reopen(&h, "y");
    until("ready", || h.phase() == "ready").await;
    let opened: String = h.q(
        "SELECT json_extract(receipt,'$.payload.state.result.revision') FROM node_commands WHERE method='open_session' ORDER BY created DESC, rowid DESC LIMIT 1",
    );
    assert!(
        !old.contains(&opened),
        "a new journal's revision {opened} is none of the old journal's {old:?}"
    );
    let late = h.seen.stash.lock().unwrap().take().unwrap();
    assert!(!late.is_empty());
    for b in late {
        h.seen.to.commit(b).await.unwrap();
    }
    until("applied", || {
        h.q::<i64>("SELECT COUNT(*) FROM node_streams WHERE durable > applied") == 0
    })
    .await;
    assert_eq!(h.phase(), "ready", "the old view says nothing of this open");
    assert_eq!(h.count("release_session"), 2);
}

// ── what an upgrade drain waits for (L3, D1) ──

/// A turn waiting for a permission answer holds the drain until it is answered and the turn ends;
/// an opening harness counts too, an idle one does not.
#[tokio::test]
async fn working_counts_a_turn_waiting_for_a_permission_until_it_settles() {
    let h = hub();
    *h.seen.stash.lock().unwrap() = Some(vec![]);
    h.bind();
    until("opening", || h.phase() == "opening").await;
    assert_eq!(h.rt.working(), 1, "a harness opening is Node work");
    let late = h.seen.stash.lock().unwrap().take().unwrap();
    for b in late {
        h.seen.to.commit(b).await.unwrap();
    }
    h.ready().await;
    assert_eq!(h.rt.working(), 0, "an idle harness holds nothing");

    let exec = h.running("hello").await;
    assert_eq!(h.rt.working(), 1);
    h.node
        .ask_permission(perm("p1", exec.clone(), "git push"))
        .await
        .unwrap();
    until("card", || h.cards().len() == 1).await;
    assert_eq!(h.rt.working(), 1, "a permission wait holds the drain");
    let d = h.cards()[0].clone();
    assert!(h.rt.answer_permission(&d, Some("allow_once")).unwrap());
    until("answered", || h.status(&d) == "resolved").await;
    assert_eq!(h.rt.working(), 1, "the turn goes on after the answer");
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || h.ends().len() == 1).await;
    h.ready().await;
    assert_eq!(h.rt.working(), 0);
}

/// A card that expires unanswered ends its wait: once the turn is over, nothing is left.
#[tokio::test]
async fn working_drops_to_zero_after_an_expired_card_and_the_turn_end() {
    let h = hub();
    h.bind();
    let exec = h.running("hello").await;
    h.node
        .ask_permission(perm_ttl("p1", exec.clone(), "rm x", 300))
        .await
        .unwrap();
    until("card", || h.cards().len() == 1).await;
    assert_eq!(h.rt.working(), 1);
    let d = h.cards()[0].clone();
    until("expired", || h.status(&d) == "expired").await;
    h.node.finish(&exec, build::done()).await.unwrap();
    until("turn end", || h.ends().len() == 1).await;
    h.ready().await;
    assert_eq!(h.rt.working(), 0);
}

/// An attempt nobody can tell the outcome of holds the drain until Stop settles it.
#[tokio::test]
async fn working_counts_an_unknown_attempt_until_it_is_abandoned() {
    let h = hub();
    h.bind();
    let exec = h.running("hello").await;
    let (_, start_op) = h
        .node
        .log()
        .into_iter()
        .find(|(m, _)| *m == "start_turn")
        .unwrap();
    h.node.lose_track(&exec).unwrap();
    h.node
        .emit(
            Subject::Execution(exec.clone()),
            EventBody::OperationChanged {
                receipt: Receipt {
                    operation: start_op,
                    state: Observed::Unknown {
                        reason: "node restarted".into(),
                    },
                },
            },
        )
        .await
        .unwrap();
    until("unknown", || {
        h.q::<String>("SELECT state FROM runtime_attempts") == "unknown"
    })
    .await;
    assert_eq!(h.rt.working(), 1, "an unknown attempt holds the drain");
    assert!(h.rt.cancel(S).unwrap());
    until("abandoned", || h.ends().len() == 1).await;
    // the harness went with it: nothing waits, so nothing reopens it
    until("settled", || h.rt.working() == 0).await;
}

/// A Node stopped at boot (its tools did not connect) refuses the Stop that would settle its open
/// or unknown turns: it holds no drain, or an upgrade, the one way to fix it, could never seal.
#[tokio::test]
async fn a_stopped_node_holds_no_drain_with_an_open_or_unknown_turn() {
    let h = hub();
    h.bind();
    let exec = h.running("hello").await;
    let (_, start_op) = h
        .node
        .log()
        .into_iter()
        .find(|(m, _)| *m == "start_turn")
        .unwrap();
    h.node.lose_track(&exec).unwrap();
    h.node
        .emit(
            Subject::Execution(exec.clone()),
            EventBody::OperationChanged {
                receipt: Receipt {
                    operation: start_op,
                    state: Observed::Unknown {
                        reason: "node restarted".into(),
                    },
                },
            },
        )
        .await
        .unwrap();
    until("unknown", || {
        h.q::<String>("SELECT state FROM runtime_attempts") == "unknown"
    })
    .await;
    assert_eq!(h.rt.working(), 1);
    h.rt.stop("its tools did not connect");
    assert!(h.rt.cancel(S).is_err(), "a stopped Node cannot abandon it");
    assert_eq!(h.rt.working(), 0, "a stopped Node runs nothing");
    // the turn itself is left as it was, unknown, for a Node that runs again
    assert_eq!(
        h.q::<String>("SELECT state FROM runtime_attempts"),
        "unknown"
    );
}

/// A configure whose outcome is unknown keeps the session configuring: the drain waits for it.
#[tokio::test]
async fn working_counts_a_configuring_harness() {
    let h = hub();
    h.bind();
    h.ready().await;
    h.node.script_once("configure_session", Scripted::Unknown);
    assert_eq!(h.mode(PermissionMode::FullAccess), Configured::Sent);
    until("unknown", || h.cfg_states() == ["unknown"]).await;
    assert_eq!(h.phase(), "configuring");
    assert_eq!(h.rt.working(), 1);
}

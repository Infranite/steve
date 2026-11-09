//! What the link does when it breaks: offline before a request was written, outcome_unknown after,
//! never a second send; lane priority; heartbeats; generation fencing.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::*;
use serde_json::{Value, json};
use steve_node_api::testkit::{FakeNode, build::*};
use steve_node_api::*;
use steve_node_wire::testkit::pair;
use steve_node_wire::*;

fn op(s: &str) -> OperationId {
    OperationId::new(s).unwrap()
}

fn starts(f: &FakeNode) -> usize {
    f.log().iter().filter(|(m, _)| *m == "start_turn").count()
}

async fn ready_remote(n: &RemoteNode) {
    n.prepare_workspace(cmd("op_prep", prepare("item:s_1")))
        .await
        .unwrap();
    n.open_session(cmd("op_open", open("s_1", "n1")))
        .await
        .unwrap();
}

#[tokio::test]
async fn lost_after_it_was_sent_is_uncertain_and_asked_not_redone() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    ready_remote(&w.remote).await;
    // the Node runs it, its answer never arrives, then the link dies
    w.node_faults
        .drop_responses
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let c = cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi"));
    let remote = w.remote.clone();
    let call = tokio::spawn({
        let c = c.clone();
        async move { remote.start_turn(c).await }
    });
    until("the node took it", || starts(&fake) == 1).await;
    w.hub_faults.cut_now();
    let e = call.await.unwrap().unwrap_err();
    assert_eq!(e.code, ErrorCode::OutcomeUnknown);
    assert_eq!(e.retry, RetryClass::AfterReconcile);
    assert_eq!(e.operation, Some(op("op_t1")));
    assert_eq!(starts(&fake), 1, "the transport never resent it");
    // a new connection asks, and learns it runs
    let w2 = wire(fake.clone(), 2);
    let r = w2.remote.operation(op("op_t1")).await.unwrap();
    assert_eq!(r.state, Observed::Running);
    assert_eq!(starts(&fake), 1);
}

#[tokio::test]
async fn never_written_is_offline_and_safe_to_resend() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    ready_remote(&w.remote).await;
    // the Hub's writer hangs on the first frame; the second stays queued
    w.hub_faults.stall(true);
    let (r1, r2) = (w.remote.clone(), w.remote.clone());
    let first = tokio::spawn(async move {
        r1.start_turn(cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi")))
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = tokio::spawn(async move {
        r2.cancel(cmd(
            "op_c",
            Cancel {
                target: op("op_t1"),
                reason: "x".into(),
            },
        ))
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    w.hub_conn.close(None, close::GOODBYE, "test");
    let e1 = first.await.unwrap().unwrap_err();
    let e2 = second.await.unwrap().unwrap_err();
    // the first was being written: maybe it went
    assert_eq!(e1.code, ErrorCode::OutcomeUnknown);
    // the second never left the queue
    assert_eq!(e2.code, ErrorCode::Offline);
    assert_eq!(e2.retry, RetryClass::SameOperation);
    // without a link: offline at once
    w.remote.detach(1);
    assert_eq!(
        w.remote.operation(op("op_t1")).await.unwrap_err().code,
        ErrorCode::Offline
    );
}

#[tokio::test(start_paused = true)]
async fn a_passed_deadline_is_uncertain_for_commands_offline_for_reads() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    ready_remote(&w.remote).await;
    w.node_faults
        .drop_responses
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let e = w
        .remote
        .start_turn(cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::OutcomeUnknown);
    let e = w.remote.operation(op("op_t1")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Offline);
    assert_eq!(starts(&fake), 1);
}

#[tokio::test(start_paused = true)]
async fn silence_past_the_budget_closes_the_link() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    // the Node stops writing anything, heartbeats included
    w.node_faults.stall(true);
    let (code, _) = tokio::time::timeout(Duration::from_secs(40), w.hub_conn.closed())
        .await
        .expect("closed within the budget");
    assert_eq!(code, close::DEAD);
    assert!(w.hub_conn.is_closed());
}

#[tokio::test]
async fn heartbeats_keep_an_idle_link() {
    let fake = FakeNode::new("n1");
    let (h, n) = pair();
    let o = |side| ConnOptions {
        heartbeat: Duration::from_millis(20),
        dead_after: Duration::from_millis(100),
        side,
        ..Default::default()
    };
    let hub = Conn::spawn(h.sink, h.stream, 1, Arc::new(Nothing), o("h"));
    let _node = Conn::spawn(n.sink, n.stream, 1, NodeServer::new(fake), o("n"));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!hub.is_closed());
}

struct Nothing;
#[async_trait]
impl Handler for Nothing {
    async fn handle(&self, m: &str, _: Value) -> ApiResult<Value> {
        Err(steve_node_wire::conn::unsupported(m))
    }
}

/// Records the order requests arrive in.
#[derive(Default)]
struct Order(Mutex<Vec<String>>);
#[async_trait]
impl Handler for Order {
    async fn handle(&self, m: &str, _: Value) -> ApiResult<Value> {
        self.0.lock().unwrap().push(m.to_string());
        Ok(json!({"durable": []}))
    }
}

#[tokio::test]
async fn control_goes_before_bulk() {
    let (h, n) = pair();
    let order = Arc::new(Order::default());
    let _hub = Conn::spawn(h.sink, h.stream, 1, order.clone(), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    n.faults.stall(true);
    // ~10 MB of event batches queued behind a stuck socket, then one control call
    let big = json!({"events": [], "pad": "x".repeat(200 * 1024)});
    let mut bulk = vec![];
    for _ in 0..50 {
        let (c, p) = (node.clone(), big.clone());
        bulk.push(tokio::spawn(async move {
            c.call("events.commit", p, None).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let c = node.clone();
    let ctl = tokio::spawn(async move { c.call("tool.inspect", json!({}), None).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    n.faults.stall(false);
    ctl.await.unwrap().unwrap();
    for b in bulk {
        b.await.unwrap().unwrap();
    }
    let seen = order.0.lock().unwrap().clone();
    let at = seen.iter().position(|m| m == "tool.inspect").unwrap();
    // only the frame already being written goes before it
    assert!(at <= 1, "control at {at} of {}", seen.len());
}

#[tokio::test]
async fn a_request_of_another_generation_is_refused_unexecuted() {
    let fake = FakeNode::new("n1");
    let (h, n) = pair();
    let hub = Conn::spawn(h.sink, h.stream, 1, Arc::new(Nothing), opts("h"));
    // the Node was welcomed at generation 2: a gen-1 request is stale there
    let _node = Conn::spawn(
        n.sink,
        n.stream,
        2,
        NodeServer::new(fake.clone()),
        opts("n"),
    );
    let remote = RemoteNode::new(NodeId::new("n1").unwrap());
    remote.attach(hub.clone(), false);
    let e = remote
        .prepare_workspace(cmd("op_prep", prepare("item:s_1")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    assert!(fake.log().is_empty(), "never executed");
}

#[tokio::test]
async fn reconciling_refuses_new_work_but_lets_inspection_through() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    ready_remote(&w.remote).await;
    w.remote.attach(w.hub_conn.clone(), true);
    let e = w
        .remote
        .start_turn(cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Offline);
    assert_eq!(starts(&fake), 0);
    assert!(w.remote.streams().await.is_ok());
    assert!(w.remote.set_ready(w.hub_conn.generation()));
    w.remote
        .start_turn(cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_lost_tool_call_is_unknown_to_the_agent() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    *w.tools.delay_ms.lock().unwrap() = 200;
    let tools = RemoteTools(w.link.clone());
    let q = ToolCall {
        id: CallId::new("c1").unwrap(),
        execution: exec_turn("s_1", "t_1", "a_1"),
        catalog: Revision::new("c1").unwrap(),
        name: "steve_note".into(),
        args: json!({"x": 1}),
        deadline: Timestamp(0),
    };
    let fut = tokio::spawn({
        let q = q.clone();
        async move { tools.call(q).await }
    });
    until("the hub took it", || {
        w.tools.calls.lock().unwrap().len() == 1
    })
    .await;
    w.node_faults.cut_now();
    let r = fut.await.unwrap().unwrap();
    assert!(matches!(r, ToolResult::Unknown { .. }), "{r:?}");
    assert_eq!(w.tools.calls.lock().unwrap().len(), 1);
    // a short deadline from the call itself: a fresh link answers in time
    let w2 = wire(fake, 1);
    let mut q2 = q.clone();
    q2.deadline = Timestamp(i64::MAX / 2);
    assert!(matches!(
        RemoteTools(w2.link.clone()).call(q2).await.unwrap(),
        ToolResult::Completed { .. }
    ));
}

#[test]
fn an_oversize_batch_is_split_into_contiguous_runs() {
    let ev = |seq| NodeEvent {
        schema: 1,
        id: EventId::new(format!("e{seq}")).unwrap(),
        at: Timestamp(1),
        cursor: Watermark {
            stream: StreamId::new("st").unwrap(),
            epoch: Epoch::new("ep").unwrap(),
            seq,
        },
        subject: Subject::Session {
            session: SessionId::new("s").unwrap(),
        },
        body: EventBody::TextDelta {
            message: "m".into(),
            block: "b".into(),
            utf8_offset: 0,
            text: "x".repeat(60 * 1024),
        },
    };
    let batch = EventBatch {
        events: (1..=10).map(ev).collect(),
    };
    let parts = steve_node_wire::remote_hub::split(batch);
    assert!(parts.len() >= 3, "{}", parts.len());
    let seqs: Vec<u64> = parts
        .iter()
        .flat_map(|b| b.events.iter().map(|e| e.cursor.seq))
        .collect();
    assert_eq!(seqs, (1..=10).collect::<Vec<_>>());
    for p in &parts {
        assert!(serde_json::to_string(p).unwrap().len() <= envelope::BULK_CAP + 1024);
    }
}

#[tokio::test]
async fn a_remote_node_runs_no_jobs_and_nothing_is_sent() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    let before = w.hub_conn.frames_written();
    let e = w
        .remote
        .start_job(cmd("op_j", job("j_1", "a_j", "think")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert_eq!(w.hub_conn.frames_written(), before, "nothing went out");
    assert!(fake.log().iter().all(|(m, _)| *m != "start_job"));
}

/// A Hub-side link of `generation` to nobody in particular.
fn hub_conn(generation: u64) -> Arc<Conn> {
    let (h, n) = pair();
    let c = Conn::spawn(h.sink, h.stream, generation, Arc::new(Nothing), opts("h"));
    std::mem::forget(n);
    c
}

#[tokio::test]
async fn a_late_attach_of_an_older_link_does_not_take_the_slot_back() {
    let remote = RemoteNode::new(NodeId::new("n1").unwrap());
    let (old, new) = (hub_conn(4), hub_conn(5));
    assert!(remote.attach(new.clone(), false));
    // the attach of the fenced link ran last
    assert!(!remote.attach(old.clone(), false));
    assert_eq!(remote.generation(), Some(5));
    // and its detach leaves the newer link alone
    remote.detach(4);
    assert_eq!(remote.generation(), Some(5));
}

#[tokio::test]
async fn reconciling_outlives_a_reconnect_and_only_the_current_link_clears_it() {
    let remote = RemoteNode::new(NodeId::new("n1").unwrap());
    let (a, b) = (hub_conn(1), hub_conn(2));
    assert!(remote.attach(a.clone(), true));
    remote.detach(1);
    // the next link of the same incarnation is not seen as a restart, but the catch-up is owed
    assert!(remote.attach(b.clone(), false));
    assert!(remote.is_reconciling());
    // a catch-up that ran over the old link clears nothing
    assert!(!remote.set_ready(1));
    assert!(remote.is_reconciling());
    assert!(remote.set_ready(2));
    assert!(!remote.is_reconciling());
}

/// Holds every request until released, counting those that reached it.
#[derive(Default)]
struct Gate {
    entered: std::sync::atomic::AtomicUsize,
    open: tokio::sync::Notify,
}
#[async_trait]
impl Handler for Gate {
    async fn handle(&self, _: &str, _: Value) -> ApiResult<Value> {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.open.notified().await;
        Ok(json!({"durable": []}))
    }
}

#[tokio::test]
async fn a_peer_cannot_pile_up_requests_past_the_cap() {
    let (h, n) = pair();
    let gate = Arc::new(Gate::default());
    let _hub = Conn::spawn(h.sink, h.stream, 1, gate.clone(), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    let total = conn::MAX_INBOUND + 20;
    let mut calls = vec![];
    for _ in 0..total {
        let c = node.clone();
        calls.push(tokio::spawn(async move {
            c.call("events.commit", json!({"events": []}), None).await
        }));
    }
    let mut refused = 0;
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if calls.iter().filter(|c| c.is_finished()).count() >= total - conn::MAX_INBOUND {
            break;
        }
    }
    assert_eq!(
        gate.entered.load(std::sync::atomic::Ordering::SeqCst),
        conn::MAX_INBOUND,
        "only the cap reached the handler"
    );
    gate.open.notify_waiters();
    for c in calls {
        match c.await.unwrap() {
            Ok(_) => {}
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Exhausted);
                refused += 1;
            }
        }
    }
    assert_eq!(refused, 20);
}

/// Answers a file chunk big, anything else small.
struct Chunks;
#[async_trait]
impl Handler for Chunks {
    async fn handle(&self, m: &str, _: Value) -> ApiResult<Value> {
        Ok(match m {
            "blob.fetch" | "fs.read" => json!({"pad": "x".repeat(60 * 1024)}),
            _ => json!({"ok": true}),
        })
    }
}

/// A permission answer is not queued behind file chunks answered before it: an answer goes on
/// its request's lane, and control goes first.
#[tokio::test]
async fn an_answer_goes_on_its_requests_lane() {
    let (h, n) = pair();
    let _hub = Conn::spawn(h.sink, h.stream, 1, Arc::new(Chunks), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    // the Hub's answers wait behind a stuck socket: 30 chunks, then one control answer
    h.faults.stall(true);
    let mut calls = vec![];
    for _ in 0..30 {
        let (c, o) = (node.clone(), order.clone());
        calls.push(tokio::spawn(async move {
            c.call("blob.fetch", json!({}), None).await.unwrap();
            o.lock().unwrap().push("chunk");
        }));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (c, o) = (node.clone(), order.clone());
    calls.push(tokio::spawn(async move {
        c.call("tool.inspect", json!({}), None).await.unwrap();
        o.lock().unwrap().push("control");
    }));
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.faults.stall(false);
    for c in calls {
        c.await.unwrap();
    }
    let seen = order.lock().unwrap().clone();
    let at = seen.iter().position(|m| *m == "control").unwrap();
    // only the chunk already being written goes before it
    assert!(at <= 1, "control answer at {at} of {}", seen.len());
}

/// Control goes first, but a lower lane is never starved by it: after a few control frames, a
/// waiting event batch goes, and after a few more of either, a waiting chunk.
#[tokio::test]
async fn no_lane_starves() {
    let (h, n) = pair();
    let order = Arc::new(Order::default());
    let _hub = Conn::spawn(h.sink, h.stream, 1, order.clone(), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    n.faults.stall(true);
    let mut calls = vec![];
    let call = |m: &'static str| {
        let c = node.clone();
        tokio::spawn(async move { c.call(m, json!({}), None).await })
    };
    for _ in 0..25 {
        calls.push(call("tool.inspect"));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    calls.push(call("blob.status"));
    calls.push(call("events.commit"));
    calls.push(call("blob.chunk"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    for _ in 0..25 {
        calls.push(call("tool.inspect"));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    n.faults.stall(false);
    for c in calls {
        c.await.unwrap().unwrap();
    }
    let seen = order.0.lock().unwrap().clone();
    let events = seen.iter().position(|m| m == "events.commit").unwrap();
    let chunk = seen.iter().position(|m| m == "blob.chunk").unwrap();
    let fair = conn::FAIR as usize;
    assert!(events <= fair + 1, "events at {events}: {seen:?}");
    assert!(chunk <= 2 * fair + 2, "chunk at {chunk}: {seen:?}");
    assert!(events < chunk, "events before bulk: {seen:?}");
}

/// Requests in flight are bounded by their bytes too: big ones past the budget are refused
/// `exhausted` before the count cap is reached.
#[tokio::test]
async fn a_peer_cannot_pin_more_than_the_byte_budget() {
    let (h, n) = pair();
    let gate = Arc::new(Gate::default());
    let _hub = Conn::spawn(h.sink, h.stream, 1, gate.clone(), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    let big = json!({"events": [], "pad": "x".repeat(270 * 1024)});
    let per = 271 * 1024;
    let fit = conn::MAX_INBOUND_BYTES / per;
    assert!(fit < conn::MAX_INBOUND, "the bytes bind first");
    let total = conn::MAX_INBOUND;
    let mut calls = vec![];
    for _ in 0..total {
        let (c, p) = (node.clone(), big.clone());
        calls.push(tokio::spawn(async move {
            c.call("events.commit", p, None).await
        }));
    }
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if calls.iter().filter(|c| c.is_finished()).count() >= total - fit {
            break;
        }
    }
    let entered = gate.entered.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        entered <= fit && entered >= fit - 1,
        "{entered} reached the handler, {fit} fit"
    );
    gate.open.notify_waiters();
    let mut refused = 0;
    for c in calls {
        if let Err(e) = c.await.unwrap() {
            assert_eq!(e.code, ErrorCode::Exhausted);
            refused += 1;
        }
    }
    assert_eq!(refused, total - entered);
}

/// L1: a peer that stops reading while it keeps sending heartbeats (its TCP window full) no longer
/// holds the writer forever: the write gives up after the dead-link budget, socket and task with it.
#[tokio::test(start_paused = true)]
async fn a_peer_that_stops_reading_is_given_up() {
    let fake = FakeNode::new("n1");
    let (h, n) = pair();
    let o = |side, dead_after| ConnOptions {
        heartbeat: Duration::from_secs(1),
        dead_after,
        side,
        ..Default::default()
    };
    // what the Hub writes never leaves; the Node never judges the Hub silent, and keeps beating
    h.faults.stall(true);
    let hub = Conn::spawn(
        h.sink,
        h.stream,
        1,
        Arc::new(Nothing),
        o("h", Duration::from_secs(5)),
    );
    let _node = Conn::spawn(
        n.sink,
        n.stream,
        1,
        NodeServer::new(fake),
        o("n", Duration::from_secs(3600)),
    );
    let (code, reason) = tokio::time::timeout(Duration::from_secs(60), hub.closed())
        .await
        .expect("given up within the budget");
    assert_eq!(
        (code, reason.as_str()),
        (close::DEAD, "the peer stopped reading")
    );
}

/// L6: what the peer chose to say is one plain, bounded line before it is logged or shown.
#[test]
fn a_peers_words_are_one_bounded_line() {
    use steve_node_wire::conn::peer_text;
    assert_eq!(
        peer_text("ok\nFAKE 2026 INFO admin logged in\r"),
        "ok FAKE 2026 INFO admin logged in "
    );
    let long = "x".repeat(2000);
    assert_eq!(peer_text(&long).chars().count(), 501);
    assert_eq!(peer_text("short"), "short");
}

//! What the link does when it breaks: offline before a request was written, outcome_unknown after,
//! never a second send; lane priority; heartbeats; generation fencing.

mod common;

use futures_util::{SinkExt, future::join_all};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use steve_node_wire::envelope::{Frame, FrameSink};
use steve_runtime::testkit::eventually_sync;

use async_trait::async_trait;
use common::*;
use serde_json::{Value, json};
use steve_node_api::testkit::{FakeNode, build::*};
use steve_node_api::*;
use steve_node_wire::testkit::pair;
use steve_node_wire::*;

/// Stop an actual writer after it takes a frame, before that frame reaches the peer.
#[derive(Default)]
struct WriterLatch {
    held: AtomicBool,
    writing: AtomicBool,
    release: tokio::sync::Notify,
}
impl WriterLatch {
    fn hold(&self) {
        self.writing.store(false, Ordering::SeqCst);
        self.held.store(true, Ordering::SeqCst);
    }
    async fn writing(&self) {
        eventually_sync("the writer took the held frame", || {
            self.writing.load(Ordering::SeqCst)
        })
        .await;
    }
    fn open(&self) {
        self.held.store(false, Ordering::SeqCst);
        self.release.notify_one();
    }
}
fn latched(sink: FrameSink) -> (FrameSink, Arc<WriterLatch>) {
    let latch = Arc::new(WriterLatch::default());
    let during_write = latch.clone();
    let sink = futures_util::sink::unfold(sink, move |mut sink, frame: Frame| {
        let latch = during_write.clone();
        async move {
            if latch.held.load(Ordering::SeqCst) {
                latch.writing.store(true, Ordering::SeqCst);
                latch.release.notified().await;
            }
            sink.send(frame).await?;
            Ok::<_, std::io::Error>(sink)
        }
    });
    (Box::pin(sink), latch)
}

type PendingCall = Pin<Box<dyn Future<Output = ApiResult<Value>> + Send>>;
/// Poll the call through its immediate queue admission while the writer is held. Large
/// calls may wait on the byte budget; keep their futures alive and drive all to completion.
/// Disable only this initial poll's cooperative budget: Pending must come from the
/// channel/byte budget or response, rather than a scheduler budget yield.
async fn pending_call(conn: Arc<Conn>, method: &'static str, params: Value) -> PendingCall {
    let mut call: PendingCall = Box::pin(async move { conn.call(method, params, None).await });
    assert!(
        futures_util::poll!(tokio::task::unconstrained(call.as_mut())).is_pending(),
        "the held writer cannot answer yet"
    );
    call
}

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

#[tokio::test(start_paused = true)]
async fn uncertain_command_answers_keep_the_original_operation_and_effect() {
    use std::sync::atomic::Ordering::SeqCst;

    for (disconnect, corrupt) in [(true, false), (false, false), (false, true)] {
        let fake = FakeNode::new("n1");
        let w = wire(fake.clone(), 1);
        ready_remote(&w.remote).await;
        w.node_faults.drop_responses.store(!corrupt, SeqCst);
        w.node_faults.corrupt_responses.store(corrupt, SeqCst);
        let c = cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi"));
        let remote = w.remote.clone();
        let call = tokio::spawn({
            let c = c.clone();
            async move { remote.start_turn(c).await }
        });
        if disconnect {
            eventually_sync("the node took it", || starts(&fake) == 1).await;
            w.hub_faults.cut_now();
        }
        let e = call.await.unwrap().unwrap_err();
        assert_eq!(
            (e.code, e.retry),
            (Code::OutcomeUnknown, Retry::Later { after_ms: None })
        );
        assert_eq!(e.params.0["operation"], "op_t1".into());
        assert_eq!(e.params.0["state"], "unknown".into());
        assert_eq!(starts(&fake), 1, "transport made one attempt");

        let w2 = wire(fake.clone(), 2);
        let r = w2.remote.operation(c.id.clone()).await.unwrap();
        assert_eq!(r.state, Observed::Running);
        assert_eq!(w2.remote.start_turn(c).await.unwrap(), r);
        assert_eq!(
            starts(&fake),
            1,
            "same frozen envelope reads the prior receipt"
        );
    }
}

#[tokio::test]
async fn never_written_is_offline_and_safe_to_resend() {
    let fake = FakeNode::new("n1");
    let (h, n) = pair();
    let (sink, latch) = latched(h.sink);
    let hub = Conn::spawn(sink, h.stream, 1, Arc::new(Nothing), opts("h"));
    let _node = Conn::spawn(
        n.sink,
        n.stream,
        1,
        NodeServer::new(fake.clone()),
        opts("n"),
    );
    let remote = RemoteNode::new(fake.id().clone());
    remote.attach(hub.clone(), false);
    ready_remote(&remote).await;
    // the Hub's writer hangs on the first frame; the second stays queued
    latch.hold();
    let (r1, r2) = (remote.clone(), remote.clone());
    let first = tokio::spawn(async move {
        r1.start_turn(cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi")))
            .await
    });
    latch.writing().await;
    let mut second = Box::pin(async move {
        r2.cancel(cmd(
            "op_c",
            Cancel {
                target: op("op_t1"),
                reason: "x".into(),
            },
        ))
        .await
    });
    assert!(futures_util::poll!(tokio::task::unconstrained(second.as_mut())).is_pending());
    hub.close(None, close::GOODBYE, "test");
    let e1 = first.await.unwrap().unwrap_err();
    let e2 = second.await.unwrap_err();
    // the first was being written: maybe it went
    assert_eq!(e1.code, Code::OutcomeUnknown);
    // the second never left the queue
    assert_eq!(e2.code, Code::Unavailable);
    assert_eq!(e2.retry, Retry::Later { after_ms: None });
    // without a link: offline at once
    remote.detach(1);
    assert_eq!(
        remote.operation(op("op_t1")).await.unwrap_err().code,
        Code::Unavailable
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
    assert_eq!(e.code, Code::OutcomeUnknown);
    let e = w.remote.operation(op("op_t1")).await.unwrap_err();
    assert_eq!(e.code, Code::Unavailable);
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

// On the paused clock, as `silence_past_the_budget_closes_the_link`: in real time a runner that stalls
// the runtime past `dead_after` closes the link (#375).
#[tokio::test(start_paused = true)]
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
    let (sink, latch) = latched(n.sink);
    let node = Conn::spawn(sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    latch.hold();
    // ~10 MB of event batches queued behind a stuck socket, then one control call
    let big = json!({"events": [], "pad": "x".repeat(200 * 1024)});
    let mut bulk = vec![];
    bulk.push(pending_call(node.clone(), "events.commit", big.clone()).await);
    latch.writing().await;
    for _ in 1..50 {
        bulk.push(pending_call(node.clone(), "events.commit", big.clone()).await);
    }
    let ctl = pending_call(node.clone(), "tool.inspect", json!({})).await;
    latch.open();
    ctl.await.unwrap();
    for result in join_all(bulk).await {
        result.unwrap();
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
    assert_eq!(e.code, Code::Stale);
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
    assert_eq!(e.code, Code::Unavailable);
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
    eventually_sync("the hub took it", || {
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
    assert_eq!(e.code, Code::Unsupported);
    assert_eq!(w.hub_conn.frames_written(), before, "nothing went out");
    assert!(fake.log().iter().all(|(m, _)| *m != "start_job"));
}

#[tokio::test]
async fn a_colocated_node_without_think_steve_tools_refuses_dispatch_job_before_wire() {
    let fake = FakeNode::new("n1");
    fake.set_features(vec![]);
    let w = wire(fake.clone(), 1);
    w.remote.set_colocated(true);
    // Prime capabilities cache first so we can verify zero frames go out on the refused start_job.
    let caps = w.remote.probe(true).await.unwrap();
    assert!(caps.supports_jobs());
    assert!(!caps.supports_think_steve_tools());

    let mut spec = job("j_disp", "a_disp", "think");
    if let JobWork::Think(ref mut t) = spec.work {
        t.steve_tools = true;
    }
    let before = w.hub_conn.frames_written();
    let e = w
        .remote
        .start_job(cmd("op_j_disp", spec.clone()))
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Unavailable);
    assert!(
        e.reason
            .as_ref()
            .is_some_and(|r| r.is(NodeReason::LocalNodeUpdating)),
        "expected LocalNodeUpdating reason, got {e:?}"
    );
    assert_eq!(w.hub_conn.frames_written(), before, "nothing went out");
    assert!(fake.log().iter().all(|(m, _)| *m != "start_job"));

    // Once the colocated Node advertises think-steve-tools-v1, start_job goes through.
    fake.set_features(vec![FEATURE_THINK_STEVE_TOOLS.into()]);
    let caps = w.remote.probe(true).await.unwrap();
    assert!(caps.supports_think_steve_tools());
    w.remote.start_job(cmd("op_j_disp_ok", spec)).await.unwrap();
    assert!(fake.log().iter().any(|(m, _)| *m == "start_job"));
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
    eventually_sync("requests past the inbound budget have been refused", || {
        calls.iter().filter(|c| c.is_finished()).count() >= total - conn::MAX_INBOUND
    })
    .await;
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
                assert_eq!(e.code, Code::Exhausted);
                refused += 1;
            }
        }
    }
    assert_eq!(refused, 20);
}

/// Answers a file chunk big, anything else small.
#[derive(Default)]
struct Chunks(AtomicUsize);
#[async_trait]
impl Handler for Chunks {
    async fn handle(&self, m: &str, _: Value) -> ApiResult<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
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
    let chunks = Arc::new(Chunks::default());
    let (sink, latch) = latched(h.sink);
    let _hub = Conn::spawn(sink, h.stream, 1, chunks.clone(), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    // the Hub's answers wait behind a stuck socket: 30 chunks, then one control answer
    latch.hold();
    let mut calls = vec![];
    for _ in 0..30 {
        let (c, o) = (node.clone(), order.clone());
        calls.push(tokio::spawn(async move {
            c.call("blob.fetch", json!({}), None).await.unwrap();
            o.lock().unwrap().push("chunk");
        }));
    }
    latch.writing().await;
    eventually_sync("all thirty bulk responses reached the writer queue", || {
        chunks.0.load(Ordering::SeqCst) == 30
    })
    .await;
    let (c, o) = (node.clone(), order.clone());
    calls.push(tokio::spawn(async move {
        c.call("tool.inspect", json!({}), None).await.unwrap();
        o.lock().unwrap().push("control");
    }));
    eventually_sync("the control response is queued", || {
        chunks.0.load(Ordering::SeqCst) == 31
    })
    .await;
    latch.open();
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
    let (sink, latch) = latched(n.sink);
    let node = Conn::spawn(sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    latch.hold();
    let mut calls = vec![];
    calls.push(pending_call(node.clone(), "tool.inspect", json!({})).await);
    latch.writing().await;
    for _ in 1..25 {
        calls.push(pending_call(node.clone(), "tool.inspect", json!({})).await);
    }
    calls.push(pending_call(node.clone(), "blob.status", json!({})).await);
    calls.push(pending_call(node.clone(), "events.commit", json!({})).await);
    calls.push(pending_call(node.clone(), "blob.chunk", json!({})).await);
    for _ in 0..25 {
        calls.push(pending_call(node.clone(), "tool.inspect", json!({})).await);
    }
    latch.open();
    for result in join_all(calls).await {
        result.unwrap();
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
    eventually_sync("requests past the inbound budget have been refused", || {
        calls.iter().filter(|c| c.is_finished()).count() >= total - fit
    })
    .await;
    let entered = gate.entered.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        entered <= fit && entered >= fit - 1,
        "{entered} reached the handler, {fit} fit"
    );
    gate.open.notify_waiters();
    let mut refused = 0;
    for c in calls {
        if let Err(e) = c.await.unwrap() {
            assert_eq!(e.code, Code::Exhausted);
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

/// Answers every request with more than a frame holds.
struct Huge;
#[async_trait]
impl Handler for Huge {
    async fn handle(&self, _: &str, _: Value) -> ApiResult<Value> {
        Ok(json!({"pad": "x".repeat(envelope::MAX_FRAME + 1)}))
    }
}

/// An answer too large for a frame comes back as an error that names the call, and the link stays.
#[tokio::test]
async fn an_oversized_answer_names_its_call() {
    let (h, n) = pair();
    let _hub = Conn::spawn(h.sink, h.stream, 1, Arc::new(Huge), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    let e = node.call("tool.call", json!({}), None).await.unwrap_err();
    assert_eq!(e.code, Code::Exhausted);
    assert_eq!(e.retry, Retry::Never);
    assert!(
        e.safe_detail
            .starts_with("the answer to tool.call request ")
            && e.safe_detail
                .contains(&format!("over the {} limit", envelope::MAX_FRAME)),
        "{}",
        e.safe_detail
    );
    let e = node
        .call("tool.inspect", json!({}), None)
        .await
        .unwrap_err();
    assert!(e.safe_detail.contains("tool.inspect"), "{}", e.safe_detail);
}

/// Server replying with `Retry::Now` on an ordinary wire call is clamped to `Retry::Never`.
struct ImmediateRetry;
#[async_trait]
impl Handler for ImmediateRetry {
    async fn handle(&self, _: &str, _: Value) -> ApiResult<Value> {
        Err(Error::new(Code::Busy, "try right away")
            .retry(Retry::Now)
            .with_param("operation", "op_now"))
    }
}

#[tokio::test]
async fn peer_retry_now_is_clamped_to_never() {
    let (h, n) = pair();
    let _hub = Conn::spawn(h.sink, h.stream, 1, Arc::new(ImmediateRetry), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Nothing), opts("n"));
    let e = node
        .call("tool.inspect", json!({}), None)
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Busy);
    assert_eq!(e.retry, Retry::Never);
    assert_eq!(e.params.0["operation"], "op_now".into());
    assert_eq!(e.safe_detail, "try right away");
}

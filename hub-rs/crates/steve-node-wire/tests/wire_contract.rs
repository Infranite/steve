//! The same contract, directly against the fake Node and through the wire: receipts, errors and
//! events must come out identical.

mod common;

use std::sync::Arc;

use common::*;
use serde_json::json;
use steve_node_api::testkit::{FakeNode, RecordingSink, build::*};
use steve_node_api::*;
use steve_node_wire::RemoteSink;
use steve_node_wire::methods::Lane;

fn op(s: &str) -> OperationId {
    OperationId::new(s).unwrap()
}

fn show<T: serde::Serialize>(r: &ApiResult<T>) -> String {
    match r {
        Ok(v) => format!("ok {}", serde_json::to_string(v).unwrap()),
        Err(e) => format!("err {}", serde_json::to_string(e).unwrap()),
    }
}

/// Runs one scenario through `n` (the fake itself, or a RemoteNode in front of it) and returns
/// everything it answered.
pub async fn contract(n: Arc<dyn Node>, fake: &FakeNode) -> Vec<String> {
    let mut out = vec![];
    out.push(show(
        &n.prepare_workspace(cmd("op_prep", prepare("item:s_1")))
            .await,
    ));
    out.push(show(
        &n.open_session(cmd("op_open", open("s_1", "n1"))).await,
    ));
    let t1 = cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi"));
    out.push(show(&n.start_turn(t1.clone()).await));
    // a resend: the same receipt, nothing new runs
    out.push(show(&n.start_turn(t1.clone()).await));
    // another attempt while one runs: busy
    out.push(show(
        &n.start_turn(cmd("op_t2", turn_on("n1", "s_1", "t_1", "a_2", "hi")))
            .await,
    ));
    // the same attempt under another operation: conflict
    out.push(show(
        &n.start_turn(cmd("op_t3", turn_on("n1", "s_1", "t_1", "a_1", "hi")))
            .await,
    ));
    // past its window: stale
    let mut late = cmd("op_late", turn_on("n1", "s_1", "t_9", "a_9", "x"));
    late.issued_at = Timestamp(10);
    late.accept_before = Timestamp(20);
    out.push(show(&n.start_turn(late).await));
    out.push(show(&n.session(SessionId::new("s_1").unwrap()).await));
    out.push(show(&n.session(SessionId::new("nope").unwrap()).await));
    // a permission the harness raised, answered wrong then right
    let exec = exec_turn("s_1", "t_1", "a_1");
    let req = permission("p_1", exec.clone(), "git push");
    fake.ask_permission(req.clone()).await.unwrap();
    let mut wrong = answer(&req, PermissionDecision::AllowOnce);
    wrong.action_digest = Digest::sha256(b"else");
    out.push(show(&n.resolve_permission(cmd("op_w", wrong)).await));
    out.push(show(
        &n.resolve_permission(cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
            .await,
    ));
    out.push(show(&n.operation(op("op_t1")).await));
    out.push(show(&n.operation(op("op_never")).await));
    out.push(show(
        &n.snapshot(PageRequest {
            cursor: None,
            limit: 10,
        })
        .await,
    ));
    let heads = n.streams().await.unwrap();
    out.push(serde_json::to_string(&heads).unwrap());
    let from = Watermark {
        stream: heads[0].stream.clone(),
        epoch: heads[0].epoch.clone(),
        seq: heads[0].acknowledged,
    };
    let batch = n.replay(from.clone(), 1 << 20).await;
    out.push(show(&batch));
    out.push(show(&n.replay(from.clone(), 10).await));
    let sink = RecordingSink::default();
    let ack = match batch {
        Ok(b) if !b.events.is_empty() => sink.commit(b).await.unwrap(),
        _ => EventAck::default(),
    };
    out.push(show(&n.acknowledge(ack).await));
    let mut zero = from;
    zero.seq = 0;
    out.push(show(&n.replay(zero, 1 << 20).await));
    out.push(show(
        &n.cancel(cmd(
            "op_c",
            Cancel {
                target: op("op_t1"),
                reason: "user".into(),
            },
        ))
        .await,
    ));
    out.push(show(
        &n.history(PageRequest {
            cursor: None,
            limit: 5,
        })
        .await,
    ));
    fake.finish(&exec, done()).await.unwrap();
    out.push(show(&n.operation(op("op_t1")).await));
    out.push(show(
        &n.release_session(cmd("op_rel", SessionId::new("s_1").unwrap()))
            .await,
    ));
    out
}

#[tokio::test]
async fn the_wire_answers_exactly_like_the_node() {
    let direct = FakeNode::new("n1");
    let direct_sink = Arc::new(RecordingSink::default());
    direct.connect(direct_sink.clone());
    let a = contract(direct.clone(), &direct).await;

    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    fake.connect(Arc::new(RemoteSink(w.link.clone())));
    let b = contract(w.remote.clone(), &fake).await;
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x, y, "step {i}");
    }
    // the events went Node → Hub over the same link, identical
    assert_eq!(direct_sink.kinds(), w.sink.kinds());
    assert!(
        w.sink.kinds().contains(&"permission_requested".to_string()),
        "{:?}",
        w.sink.kinds()
    );
    assert_eq!(direct.log(), fake.log());
    // the digest the Node keeps is the one the Hub computed: the payload went verbatim
    let t1 = cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi"));
    let snap = fake
        .snapshot(PageRequest {
            cursor: None,
            limit: 50,
        })
        .await
        .unwrap();
    let e = snap
        .entries
        .iter()
        .find(|e| e.receipt.operation == op("op_t1"))
        .unwrap();
    assert_eq!(e.digest, t1.digest(&e.method).unwrap());
}

#[tokio::test]
async fn unknown_methods_are_unsupported_and_the_link_stays() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    let e = w
        .hub_conn
        .call_with(
            "frobnicate",
            json!({}),
            Lane::Control,
            std::time::Duration::from_secs(5),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    // a Node→Hub method sent to the Node is unknown there too
    let e = w
        .hub_conn
        .call("events.commit", json!({"events": []}), None)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert!(w.remote.probe(false).await.is_ok(), "still up");
}

#[tokio::test]
async fn every_method_is_served() {
    // each table entry reaches its port: nothing answers "unknown method"
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    for s in steve_node_wire::methods::ALL {
        let conn = match s.dir {
            steve_node_wire::methods::Dir::HubToNode => &w.hub_conn,
            steve_node_wire::methods::Dir::NodeToHub => &w.node_conn,
        };
        let r = conn.call(s.name, json!(null), None).await;
        if let Err(e) = r {
            assert!(
                !e.safe_detail.contains("unknown method"),
                "{} is not served: {e}",
                s.name
            );
        }
    }
}

#[tokio::test]
async fn remote_capacity_for_jobs_and_execs_is_zero() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    let c = w.remote.probe(false).await.unwrap();
    assert_eq!((c.capacity.jobs, c.capacity.execs), (0, 0));
}

//! A newer link of a Node fences the older one: the old link's calls resolve by the written rule,
//! and nothing it sends afterwards reaches the Hub.

mod node_harness;

use std::time::Duration;

use node_harness::*;
use serde_json::json;
use steve_hub::nodes::guard::{GuardedSink, GuardedTools, LinkCtx};
use steve_node_api::testkit::{FakeNode, Point, build::*};
use steve_node_api::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_newer_link_wins_and_the_old_ones_calls_are_uncertain() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let fake = FakeNode::new(node.as_str());
    let a = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    let remote = h.fleet.remote(&node);
    until("A attached", 5, || {
        remote.generation() == Some(a.generation)
    })
    .await;
    remote
        .prepare_workspace(cmd("op_prep", prepare("item:s_1")))
        .await
        .unwrap();
    remote
        .open_session(cmd("op_open", open("s_1", node.as_str())))
        .await
        .unwrap();
    // the Node takes its time with this turn; meanwhile a newer link of it arrives
    fake.set_hook(|p| {
        if matches!(p, Point::AfterAccept("start_turn")) {
            std::thread::sleep(Duration::from_millis(400));
        }
    });
    let r = remote.clone();
    let n = node.clone();
    let call = tokio::spawn(async move {
        r.start_turn(cmd("op_t1", turn_on(n.as_str(), "s_1", "t_1", "a_1", "hi")))
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let b = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    assert!(b.generation > a.generation);
    let (code, _) = tokio::time::timeout(Duration::from_secs(2), a.conn.closed())
        .await
        .unwrap();
    assert_eq!(code, steve_node_wire::close::FENCED);
    let e = call.await.unwrap().unwrap_err();
    assert_eq!(e.code, ErrorCode::OutcomeUnknown);
    assert_eq!(e.operation, Some(OperationId::new("op_t1").unwrap()));
    // asked over the new link, not redone
    until("B attached", 5, || {
        remote.generation() == Some(b.generation)
    })
    .await;
    let r = remote
        .operation(OperationId::new("op_t1").unwrap())
        .await
        .unwrap();
    assert_eq!(r.state, Observed::Running);
    assert_eq!(
        fake.log()
            .iter()
            .filter(|(m, _)| *m == "start_turn")
            .count(),
        1
    );
    // A's frames reach nothing now
    let late = a
        .conn
        .call("events.commit", json!({"events": []}), None)
        .await;
    assert_eq!(late.unwrap_err().code, ErrorCode::Offline);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_old_generation_is_stale_at_every_guard() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let fake = FakeNode::new(node.as_str());
    let _a = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    let b = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    until("B current", 5, || {
        h.svc.registry.current(node.as_str()) == Some(b.generation)
    })
    .await;
    // what a frame of A that raced the fencing would meet
    let old = LinkCtx {
        node: node.clone(),
        generation: b.generation - 1,
        store: h.store.clone(),
        registry: h.svc.registry.clone(),
        fleet: h.fleet.clone(),
    };
    let e = GuardedSink(old.clone())
        .commit(EventBatch::default())
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    let q = ToolCall {
        id: CallId::new("c1").unwrap(),
        execution: exec_turn("s_1", "t_1", "a_1"),
        catalog: Revision::new("c1").unwrap(),
        name: "steve_update".into(),
        args: json!({}),
        deadline: Timestamp(i64::MAX / 2),
    };
    assert_eq!(
        GuardedTools(old).call(q.clone()).await.unwrap_err().code,
        ErrorCode::Stale
    );
    // the current link, after a revocation moved the generation in the store: stale as well
    let cur = LinkCtx {
        node: node.clone(),
        generation: b.generation,
        store: h.store.clone(),
        registry: h.svc.registry.clone(),
        fleet: h.fleet.clone(),
    };
    h.store
        .transaction::<_, steve_store::StoreError>(|tx| {
            steve_store::nodes::revoke(tx, node.as_str(), "u1", 1)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        GuardedTools(cur).call(q).await.unwrap_err().code,
        ErrorCode::Stale
    );
}

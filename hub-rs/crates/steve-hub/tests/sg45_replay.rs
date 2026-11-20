//! SG45: bounded replay, durable admission and refusal through catch_up_streams.
use async_trait::async_trait;
use std::sync::Arc;
use steve_hub::wire::node_runtime::{NodeDomain, NodeRuntime, Pull, Sink, catch_up_streams};
use steve_node_api::{
    testkit::{FakeNode, build::*},
    *,
};
use steve_store::{FnPublisher, Store};
struct Domain;
impl NodeDomain for Domain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}
struct Uncommitted;
#[async_trait]
impl EventSink for Uncommitted {
    async fn commit(&self, _: EventBatch) -> ApiResult<EventAck> {
        Err(ApiError::new(ErrorCode::Offline, "retain journal"))
    }
}
#[tokio::test]
async fn sg45_replay_keeps_denied_events_then_expands_the_page_and_durably_acks_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("test.sqlite3"), FnPublisher(|_| {})).unwrap();
    store.ensure_workspace().unwrap();
    let node = FakeNode::new("n1");
    node.connect(Arc::new(Uncommitted));
    node.prepare_workspace(cmd("prepare", prepare("item:s1")))
        .await
        .unwrap();
    node.open_session(cmd("open", open("s1", "n1")))
        .await
        .unwrap();
    let exec = exec_turn("s1", "t1", "a1");
    node.start_turn(cmd("turn", turn_on("n1", "s1", "t1", "a1", "input")))
        .await
        .unwrap();
    node.draft(&exec, "m1", &[&"x".repeat(300 * 1024)])
        .await
        .unwrap();
    let heads = node.streams().await.unwrap();
    let deny = |_: &EventBatch| Err(ApiError::new(ErrorCode::Denied, "not our work"));
    let refused = catch_up_streams(
        &store,
        &*node,
        &heads,
        Some(Pull {
            receive: &deny,
            largest: 1024 * 1024,
        }),
    )
    .await
    .unwrap();
    assert_eq!(refused.replayed, 0);
    assert!(refused.reached.is_empty());
    assert!(node.unacked() > 0);
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Domain));
    let sink = Sink(Arc::downgrade(&rt));
    let receive = |batch: &EventBatch| futures::executor::block_on(sink.commit(batch.clone()));
    let out = catch_up_streams(
        &store,
        &*node,
        &heads,
        Some(Pull {
            receive: &receive,
            largest: 1024 * 1024,
        }),
    )
    .await
    .unwrap();
    assert!(out.replayed >= 2);
    assert_eq!(out.reached.len(), heads.len());
    assert_eq!(out.faulted, 0);
    assert_eq!(node.unacked(), 0);
    let again = catch_up_streams(
        &store,
        &*node,
        &heads,
        Some(Pull {
            receive: &receive,
            largest: 1024 * 1024,
        }),
    )
    .await
    .unwrap();
    assert_eq!(again.replayed, 0);
    assert_eq!(again.faulted, 0);
}

#[tokio::test]
async fn sg45_replay_gap_after_the_snapshot_faults_the_stream_without_admitting_a_batch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("test.sqlite3"), FnPublisher(|_| {})).unwrap();
    store.ensure_workspace().unwrap();
    let node = FakeNode::new("n1");
    node.connect(Arc::new(Uncommitted));
    node.prepare_workspace(cmd("prepare", prepare("item:s1")))
        .await
        .unwrap();
    node.open_session(cmd("open", open("s1", "n1")))
        .await
        .unwrap();
    let heads = node.streams().await.unwrap();
    assert!(heads.iter().any(|h| h.produced > 0));
    node.acknowledge(EventAck {
        durable: heads
            .iter()
            .map(|h| Watermark {
                stream: h.stream.clone(),
                epoch: h.epoch.clone(),
                seq: h.produced,
            })
            .collect(),
    })
    .await
    .unwrap();
    let never = |_: &EventBatch| -> ApiResult<EventAck> { panic!("a gap must not admit a batch") };
    let out = catch_up_streams(
        &store,
        &*node,
        &heads,
        Some(Pull {
            receive: &never,
            largest: 1024 * 1024,
        }),
    )
    .await
    .unwrap();
    assert!(out.faulted > 0);
    assert!(out.reached.is_empty());
    assert_eq!(out.replayed, 0);
    let repeated = catch_up_streams(
        &store,
        &*node,
        &heads,
        Some(Pull {
            receive: &never,
            largest: 1024 * 1024,
        }),
    )
    .await
    .unwrap();
    assert_eq!(repeated.faulted, 0, "the fault is recorded once");
}

//! SG45: link handoff, machine discovery and sampling through public Node interfaces.
mod node_harness;
use node_harness::*;
use serde_json::json;
use std::sync::Arc;
use steve_hub::nodes::vitals;
use steve_node_api::{Node, testkit::FakeNode};
use steve_runtime::testkit::eventually_sync;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg45_link_handoff_fences_the_old_generation_and_keeps_host_and_samples() {
    let h = Hub::start().await;
    let (key, id) = h.paired("u1").await;
    let node = FakeNode::new(id.as_str());
    let first = dial(&h, &key, &id, node.clone(), 1).await.unwrap();
    eventually_sync("host was learned", || {
        h.q::<i64>("SELECT count(*) FROM nodes WHERE host IS NOT NULL") == 1
    })
    .await;
    let second = dial(&h, &key, &id, node, 2).await.unwrap();
    assert!(second.generation > first.generation);
    assert_eq!(first.conn.closed().await.0, steve_node_wire::close::FENCED);
    eventually_sync("replacement is installed", || {
        h.svc
            .registry
            .current(id.as_str())
            .is_some_and(|g| g == second.generation)
    })
    .await;
    let local: Arc<dyn Node> = FakeNode::new("local");
    vitals::sample_once(&h.store, Some(&local), Some(&h.svc)).await;
    let local_rows = h
        .store
        .reader(|c| steve_store::nodes::vitals_since(c, "local", 0))
        .unwrap();
    let remote_rows = h
        .store
        .reader(|c| steve_store::nodes::vitals_since(c, id.as_str(), 0))
        .unwrap();
    assert_eq!(local_rows.len(), 1);
    assert!(local_rows[0].1.is_none());
    assert_eq!(remote_rows.len(), 1);
    assert!(remote_rows[0].1.is_some());
    let day = vitals::day(&remote_rows).unwrap();
    assert_eq!(day["series"][0]["load"], 0.25);
    assert_eq!(day["series"][0]["mem"], 0.75);
    second
        .conn
        .close(None, steve_node_wire::close::GOODBYE, "done");
}

#[tokio::test]
async fn sg45_low_disk_notices_are_once_per_episode_without_timing_dependencies() {
    let h = Hub::start().await;
    let (_, id) = h.paired("u1").await;
    let at = steve_hub::nodes::now();
    for (n, free, expected) in [(0, 2, 1), (1, 1, 1), (2, 50, 1), (3, 1, 2)] {
        let data = json!({"roots":[{"free":free}]}).to_string();
        h.store
            .transaction(|tx| steve_store::nodes::vitals_put(tx, id.as_str(), at + n, None, &data))
            .unwrap();
        vitals::warn_low_disks(&h.store, 10).await;
        assert_eq!(
            h.q::<i64>("SELECT count(*) FROM messages WHERE space='notify'"),
            expected
        );
    }
}

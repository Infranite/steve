//! Node measurements reach the metrics recorder and token usage retains workspace attribution.

mod node_harness;

use std::sync::Arc;
use steve_runtime::testkit::eventually_sync;

use node_harness::*;
use steve_hub::{metrics, nodes::vitals};
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_linked_node_updates_the_metrics_recorder() {
    metrics::install();
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let _l = dial(&h, &id, &node, FakeNode::new(node.as_str()), 1)
        .await
        .unwrap();
    eventually_sync("linked", || h.svc.registry.current(node.as_str()).is_some()).await;
    let local: Arc<dyn Node> = FakeNode::new("local");
    vitals::sample_once(Some(&local), Some(&h.svc)).await;
    let text = metrics::render().unwrap();
    for line in [
        format!("steve_node_cpu_load_ratio{{node=\"{node}\"}} 0.25"),
        format!("steve_node_memory_used_ratio{{node=\"{node}\"}} 0.75"),
        "steve_node_cpu_load_ratio{node=\"local\"} 0.25".into(),
    ] {
        assert!(text.lines().any(|seen| seen == line), "{line} missing");
    }
    assert!(text.contains(&format!("steve_node_rtt_seconds{{node=\"{node}\"}} ")));
}

#[test]
fn session_token_windows_are_attributed_to_the_node_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());
    steve_store::testkit::hub::node_statistics::seed_session_token_windows(&store).unwrap();
    let totals = store
        .read(|c| {
            steve_store::nodes::session_tokens(
                c,
                "default",
                "2026-11-02",
                "2026-11-01",
                1_000,
                61_000,
            )
        })
        .unwrap();
    assert_eq!(
        totals,
        [("local".into(), (35, 15)), ("remote".into(), (55, 55))].into()
    );
}

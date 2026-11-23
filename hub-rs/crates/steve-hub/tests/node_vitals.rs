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
    store.transaction::<_, steve_store::StoreError>(|tx| {
        tx.execute_batch(
            "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES
               ('a','default','fixture','.','ended',0,0), ('b','default','fixture','.','ended',0,0);
             INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES
               ('a','local','{}','a','r','idle',0), ('b','remote','{}','b','r','idle',0);
             INSERT INTO usage_records(workspace_id,ts,day,source,purpose,harness,session_id,input,output,cache_read,cache_write,reasoning) VALUES
               ('default',2000,'2026-11-01','agent','turn','fixture','a',10,6,3,1,4),
               ('default',61000,'2026-11-02','agent','turn','fixture','a',10,5,NULL,NULL,3),
               ('default',61000,'2026-11-02','agent','turn','fixture','b',50,5,NULL,NULL,2),
               ('default',61000,'2026-11-02','gateway','think','fixture',NULL,100,50,NULL,NULL,0),
               ('other',61000,'2026-11-02','agent','turn','fixture','a',500,50,NULL,NULL,0);"
        )?;
        Ok(())
    }).unwrap();
    let totals = store
        .reader(|c| {
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

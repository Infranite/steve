//! How each Node's machine is doing (B3): the Hub asks every linked Node over the wire, keeps what
//! it answers with the round trip, and a Node of an earlier protocol is simply not asked.

mod node_harness;

use std::sync::Arc;
use steve_runtime::testkit::eventually_sync;

use node_harness::*;
use steve_hub::nodes::vitals;
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_linked_node_is_sampled_with_its_round_trip() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let _l = dial(&h, &id, &node, FakeNode::new(node.as_str()), 1)
        .await
        .unwrap();
    eventually_sync("linked", || h.svc.registry.current(node.as_str()).is_some()).await;
    let local: Arc<dyn Node> = FakeNode::new("local");
    vitals::sample_once(&h.store, Some(&local), Some(&h.svc)).await;
    let rows: Vec<(String, Option<i64>, String)> = h
        .store
        .reader(|c| {
            let mut st =
                c.prepare("SELECT node_id, rtt_ms, data FROM node_vitals ORDER BY node_id")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let (local_row, remote_row) = if rows[0].0 == "local" {
        (&rows[0], &rows[1])
    } else {
        (&rows[1], &rows[0])
    };
    assert_eq!(local_row.1, None, "no round trip in process");
    assert_eq!(remote_row.0, node.as_str());
    assert!(remote_row.1.is_some(), "the round trip over the link");
    let v: serde_json::Value = serde_json::from_str(&remote_row.2).unwrap();
    assert_eq!(v["cpus"], 8);
    let day = vitals::day(&[(1, remote_row.1, remote_row.2.clone())]).unwrap();
    assert_eq!(day["series"][0]["load"], 0.25);
    assert_eq!(day["series"][0]["mem"], 0.75);
}

/// F1: a remote Node's project disk short of space is said once, and again only after it had
/// room in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remote_disk_short_of_space_is_said_once_per_episode() {
    let h = Hub::start().await;
    let (_id, node) = h.paired("u1").await;
    let mut sample_at = steve_hub::nodes::now();
    let mut put = |free: u64| {
        sample_at += 1;
        let data = serde_json::json!({"at": 1, "roots": [{"root": "r", "display": "~/code", "free": free, "total": 100u64 << 30}], "sessions": 0}).to_string();
        h.store
            .transaction(|tx| {
                steve_store::nodes::vitals_put(tx, node.as_str(), sample_at, None, &data)
            })
            .unwrap();
    };
    let notices = || h.q::<i64>("SELECT COUNT(*) FROM messages WHERE space='notify'");
    let low = 10u64 << 30;
    put(2 << 30);
    vitals::warn_low_disks(&h.store, low).await;
    vitals::warn_low_disks(&h.store, low).await;
    assert_eq!(notices(), 1, "once");
    put(50 << 30);
    vitals::warn_low_disks(&h.store, low).await;
    put(1 << 30);
    vitals::warn_low_disks(&h.store, low).await;
    assert_eq!(notices(), 2, "again after it had room");
}

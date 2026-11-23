//! Removing a paired Node from its machine (#325), over HTTP and a real link: only its owner asks,
//! the Node is asked over its link and unpaired once it confirmed; an offline Node and one of an
//! earlier release get the command to run on their machine instead.

mod node_harness;

use std::sync::Arc;
use std::time::Duration;
use steve_runtime::testkit::eventually_sync;
use steve_store::testkit::hub::node_checks;

use node_harness::*;
use serde_json::json;
use steve_hub::nodes::removal::{self, MANUAL_COMMAND, Removals};
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;

fn removals(h: &Hub) {
    let _ = h.svc.removals.set(Removals::new(
        h.store.clone(),
        Arc::new(removal::LiveFleet {
            svc: Arc::downgrade(&h.svc),
        }),
        removal::Timing {
            poll: Duration::from_millis(20),
            grace: Duration::from_secs(5),
            settle: Duration::from_secs(2),
        },
    ));
}

async fn list(h: &Hub, user: &str) -> serde_json::Value {
    reqwest::Client::new()
        .get(format!("{}/api/nodes", h.url()))
        .header("x-test-user", user)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_owner_removes_a_node_which_is_asked_over_its_link_then_unpaired() {
    let h = Hub::start().await;
    removals(&h);
    let (id, node) = h.paired("u1").await;
    let fake = FakeNode::new(node.as_str());
    fake.set_features(vec![FEATURE_NODE_REMOVE.into()]);
    let l = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    eventually_sync("linked", || h.svc.registry.current(node.as_str()).is_some()).await;

    let (s, _) = h
        .post("u2", &format!("/api/nodes/{node}/remove"), json!({}))
        .await;
    assert_eq!(s, 403, "not the owner");
    assert!(
        fake.removal_calls().is_empty(),
        "nothing sent for another user"
    );

    let (s, v) = h
        .post("u1", &format!("/api/nodes/{node}/remove"), json!({}))
        .await;
    assert_eq!(s, 202, "{v}");
    let op = v["operation"].as_str().unwrap().to_string();
    eventually_sync("unpaired", || {
        node_checks::node_state_by_node_id(&h.store, node.as_str()).unwrap() == "revoked"
    })
    .await;
    eventually_sync("recorded", || {
        node_checks::node_removals_phase_by_operation(&h.store, &op).unwrap() == "removed"
    })
    .await;
    let opid = OperationId::new(op.clone()).unwrap();
    assert_eq!(
        fake.removal_calls(),
        vec![("begin", opid.clone()), ("commit", opid)]
    );
    let (code, _) = tokio::time::timeout(Duration::from_secs(2), l.conn.closed())
        .await
        .expect("the link goes");
    assert_eq!(code, steve_node_wire::close::DENIED);
    assert_eq!(
        node_checks::node_audit_count_remove_u1(&h.store).unwrap(),
        1
    );
    let v = list(&h, "u1").await;
    let n = &v["nodes"][0];
    assert_eq!(n["state"], "revoked");
    assert_eq!(n["removal"]["latest"]["phase"], "removed");
    assert_eq!(n["removal"]["reason"], "unpaired");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offline_node_or_one_of_an_earlier_release_gets_the_command_to_run_there() {
    let h = Hub::start().await;
    removals(&h);
    // paired, never linked
    let (_, away) = h.paired("u1").await;
    let (s, v) = h
        .post("u1", &format!("/api/nodes/{away}/remove"), json!({}))
        .await;
    assert_eq!(s, 409);
    assert_eq!(v["error"], "offline");
    assert_eq!(v["command"], MANUAL_COMMAND);

    // linked, but it does not say it can remove itself
    let (id, old) = h.paired("u1").await;
    let fake = FakeNode::new(old.as_str());
    let _l = dial(&h, &id, &old, fake.clone(), 1).await.unwrap();
    eventually_sync("linked", || h.svc.registry.current(old.as_str()).is_some()).await;
    let (s, v) = h
        .post("u1", &format!("/api/nodes/{old}/remove"), json!({}))
        .await;
    assert_eq!(s, 409);
    assert_eq!(v["error"], "manual");
    assert_eq!(v["command"], MANUAL_COMMAND);
    assert!(fake.removal_calls().is_empty());
    for n in [&away, &old] {
        assert_eq!(
            node_checks::node_state_by_node_id(&h.store, n.as_str()).unwrap(),
            "active",
            "still paired"
        );
    }
    let v = list(&h, "u1").await;
    let reasons: Vec<_> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["node_id"].as_str().unwrap().to_string(),
                n["removal"]["reason"].clone(),
            )
        })
        .collect();
    assert!(
        reasons.contains(&(away.to_string(), json!("offline"))),
        "{reasons:?}"
    );
    assert!(
        reasons.contains(&(old.to_string(), json!("manual"))),
        "{reasons:?}"
    );
}

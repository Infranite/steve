//! What the devices page shows of a paired Node besides its state (G1, G2, G5): how its link
//! reached the Hub and how long a call over it takes, what machine it is (asked over the system
//! protocol, `node.host`), and an event for its owner whenever any of it changes.

mod node_harness;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use steve_runtime::testkit::eventually_sync;

use node_harness::*;
use serde_json::{Value, json};
use steve_hub::nodes::{self, link::Door, vitals};
use steve_node_api::testkit::FakeNode;
use steve_store::{BusEvent, FnPublisher, Store};

async fn nodes_of(h: &Hub, user: &str) -> Value {
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

async fn row(h: &Hub, node: &str) -> Value {
    nodes_of(h, "u1").await["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node_id"] == node)
        .cloned()
        .unwrap_or(Value::Null)
}

/// A Hub whose store's change events are kept, to read back which reached the stream.
async fn watched() -> (Hub, Arc<Mutex<Vec<BusEvent>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let dir = tempfile::tempdir().unwrap();
    let keep = seen.clone();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        FnPublisher(move |e| keep.lock().unwrap().push(e)),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    (Hub::on(dir, store, None).await, seen)
}

fn node_events(seen: &Mutex<Vec<BusEvent>>, node: &str) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, steve_types::BusEvent::Domain(steve_types::DomainEvent::Devices(steve_types::DevicesEvent::Node { id, user })) if id.as_str() == node && user.as_str() == "u1"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_linked_node_says_how_it_came_what_it_is_and_how_far_it_is() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let before = row(&h, node.as_str()).await;
    assert!(before["link"].is_null(), "never linked: {before}");
    assert!(before["host"].is_null());

    let l = dial(&h, &id, &node, FakeNode::new(node.as_str()), 1)
        .await
        .unwrap();
    eventually_sync("linked", || h.svc.registry.current(node.as_str()).is_some()).await;
    eventually_sync("its machine kept", || {
        h.q::<i64>("SELECT host IS NOT NULL FROM nodes WHERE local=0") == 1
    })
    .await;
    let v = row(&h, node.as_str()).await;
    assert_eq!(v["linked"], true);
    // G1: from loopback on the Hub's own port, straight in
    let link = &v["link"];
    assert_eq!(link["route"], "loopback", "{v}");
    assert_eq!(link["tls"], false);
    assert_eq!(link["via"], "direct");
    assert_eq!(link["peer"], "127.0.0.1");
    assert!(link["since"].as_i64().unwrap() > 0);
    assert!(link["latency_ms"].is_null(), "not measured yet");
    assert!(link["latency_at"].is_null());
    // G2: what the Node said of its machine
    assert_eq!(
        v["host"],
        json!({"hostname": "Studio Mac", "os": "macos", "os_name": "macOS", "os_version": "26.1",
               "arch": "aarch64", "cpus": 8, "mem_total": 17179869184u64})
    );

    // the round trip of the minute's sample is its latency while it is linked
    vitals::sample_once(&h.store, None, Some(&h.svc)).await;
    let v = row(&h, node.as_str()).await;
    assert!(v["link"]["latency_ms"].as_i64().is_some(), "{v}");
    assert!(v["link"]["latency_at"].as_i64().unwrap() > 0);

    // away: how it last came and what it is stay, the latency goes
    l.conn.close(None, steve_node_wire::close::GOODBYE, "bye");
    eventually_sync("unlinked", || {
        h.svc.registry.current(node.as_str()).is_none()
    })
    .await;
    let v = row(&h, node.as_str()).await;
    assert_eq!(v["linked"], false);
    assert_eq!(v["link"]["route"], "loopback");
    assert!(v["link"]["latency_ms"].is_null());
    assert_eq!(v["host"]["hostname"], "Studio Mac");
}

/// A link through the tailcat tunnel's listener says so: its route is not the loopback peer the
/// tunnel's end is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_through_tailcat_says_so() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = nodes::node_router_at(h.svc.clone(), Door::Tailcat);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let _l = dial_url(
        &format!("ws://{addr}/node/ws"),
        &h.svc.hub_id,
        &id,
        &node,
        FakeNode::new(node.as_str()),
        1,
    )
    .await
    .unwrap();
    eventually_sync("linked", || h.svc.registry.current(node.as_str()).is_some()).await;
    let v = row(&h, node.as_str()).await;
    assert_eq!(
        (&v["link"]["via"], &v["link"]["route"], &v["link"]["peer"]),
        (&json!("tailcat"), &json!("unknown"), &Value::Null),
        "{v}"
    );
    server.abort();
}

/// G5: every change of a Node its owner's page shows is an event for its owner: it asked to join,
/// was confirmed, linked, said what it is, had its limit and state changed, and went away.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_change_of_a_node_tells_its_owner() {
    let (h, seen) = watched().await;
    let (id, node) = h.paired("u1").await;
    let n = node.as_str();
    // claimed (pending) and confirmed
    assert_eq!(node_events(&seen, n), 2);
    let l = dial(&h, &id, &node, FakeNode::new(n), 1).await.unwrap();
    // linked, and what its machine is
    eventually_sync("linked and described", || node_events(&seen, n) >= 4).await;
    let steps = [
        ("limit", json!({"max_sessions": 3})),
        ("pause", json!({})),
        ("resume", json!({})),
        ("rename", json!({"name": "Studio Mac"})),
    ];
    for (action, body) in steps {
        let before = node_events(&seen, n);
        let (s, v) = h
            .post("u1", &format!("/api/nodes/{n}/{action}"), body)
            .await;
        assert_eq!(s, 200, "{action}: {v}");
        assert_eq!(node_events(&seen, n), before + 1, "{action}");
    }
    // a refused change tells nobody
    let before = node_events(&seen, n);
    let (s, _) = h
        .post("u2", &format!("/api/nodes/{n}/pause"), json!({}))
        .await;
    assert_eq!(s, 409);
    assert_eq!(node_events(&seen, n), before);
    // gone away
    l.conn.close(None, steve_node_wire::close::GOODBYE, "bye");
    eventually_sync("unlinked", || node_events(&seen, n) > before).await;
    // the same machine again says nothing new about it
    let before = node_events(&seen, n);
    let _l = dial(&h, &id, &node, FakeNode::new(n), 2).await.unwrap();
    eventually_sync("linked again", || node_events(&seen, n) > before).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        node_events(&seen, n),
        before + 1,
        "linked; its machine unchanged"
    );
    // only ever for its owner
    assert!(seen.lock().unwrap().iter().all(|e| match e {
        steve_types::BusEvent::Domain(steve_types::DomainEvent::Devices(
            steve_types::DevicesEvent::Node { user, .. },
        )) => user.as_str() == "u1",
        _ => true,
    }));
}

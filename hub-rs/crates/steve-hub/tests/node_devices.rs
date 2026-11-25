//! What the devices page shows of a paired Node besides its state (G1, G2, G5): how its link
//! reached the Hub and how long a call over it takes, what machine it is (asked over the system
//! protocol, `node.host`), and an event for its owner whenever any of it changes.

mod node_harness;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use steve_runtime::testkit::eventually_sync;
use steve_store::testkit::hub::node_checks;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_incompatible_node_stays_linked_and_devices_explain_the_required_upgrade() {
    use steve_node_api::{Discovery, PROTOCOL};
    for (protocol, older) in [(PROTOCOL - 1, "node"), (PROTOCOL + 1, "hub")] {
        let h = Hub::start().await;
        let (id, node) = h.paired("u1").await;
        let linked = dial_url_as(
            &h.ws_url(),
            &h.svc.hub_id,
            &id,
            &node,
            FakeNode::new(node.as_str()),
            1,
            protocol,
        )
        .await
        .unwrap();
        eventually_sync("upgrade link installed", || {
            h.svc.registry.current(node.as_str()).is_some()
                && h.svc.fleet.protocol(node.as_str()) == Some(protocol)
        })
        .await;
        assert!(linked.conn.is_upgrade());
        let device = row(&h, node.as_str()).await;
        assert_eq!(device["linked"], true);
        assert_eq!(device["protocol"], protocol);
        assert_eq!(device["link_mode"], "upgrade_only");
        assert_eq!(device["protocol_older"], older);
        assert!(
            device["hub_version"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
        );
        assert!(device["link_error"].is_null());
        let again = row(&h, node.as_str()).await;
        assert_eq!(again["link_mode"], device["link_mode"]);
        assert_eq!(again["protocol_older"], device["protocol_older"]);
        assert_eq!(again["link_error"], device["link_error"]);
        assert!(
            device["host"].is_null(),
            "ordinary host discovery is skipped"
        );
        // Vitals must skip upgrade-only links cleanly without failing or recording RTT.
        vitals::sample_once(None, Some(&h.svc)).await;
        let after_vitals = row(&h, node.as_str()).await;
        assert!(after_vitals["link"]["latency_ms"].is_null());

        let remote = h.svc.fleet.remote(&node);
        assert!(remote.is_upgrade_only());
        assert!(remote.probe(false).await.is_err());
        remote.probe_build().await.unwrap();
        assert!(!linked.conn.is_closed());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_common_upgrade_protocol_report_tells_devices_to_upgrade_the_hub() {
    use steve_node_wire::{
        handshake::{self, NodeIntro},
        tls::{self, Trust},
    };
    let h = Hub::start().await;
    let (identity, node) = h.paired("u1").await;
    let (mut sink, mut stream) = tls::connect(&h.ws_url(), &Trust::LoopbackPlaintext)
        .await
        .unwrap();
    let error = handshake::dial_protocols(
        &mut sink,
        &mut stream,
        NodeIntro {
            node: &node,
            identity: &identity,
            incarnation: 1,
            hub_id: Some(&h.svc.hub_id),
            hub_key: None,
        },
        steve_node_api::PROTOCOL,
        &[2],
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "incompatible");
    let device = row(&h, node.as_str()).await;
    assert_eq!(device["linked"], false);
    assert_eq!(device["link_error"]["error"], "版本不一致，需要升级 Hub");
    assert_eq!(device["offline"]["reason"], "protocol_mismatch");
    assert!(h.svc.registry.current(node.as_str()).is_none());
    assert_eq!(
        node_checks::node_generation_by_node_id(&h.store, node.as_str()).unwrap(),
        0
    );
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
async fn only_the_node_owner_can_recheck_file_access_and_devices_read_it_back() {
    use steve_node_api::{FileAccessReport, FileAccessStatus};
    let (h, seen) = watched().await;
    let (identity, node) = h.paired("u1").await;
    let fake = FakeNode::new(node.as_str());
    fake.set_file_access(FileAccessReport::unknown());
    let _link = dial(&h, &identity, &node, fake.clone(), 1).await.unwrap();
    eventually_sync("initial host observation", || {
        node_events(&seen, node.as_str()) >= 4
    })
    .await;
    assert_eq!(
        row(&h, node.as_str()).await["host"]["file_access"]["status"],
        "unknown"
    );
    fake.set_file_access(FileAccessReport {
        status: FileAccessStatus::Granted,
        locations: vec![],
    });
    let path = format!("/api/nodes/{node}/file-access");
    assert_eq!(h.post("u2", &path, json!({})).await.0, 404);
    let (status, report) = h.post("u1", &path, json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(report["status"], "granted");
    assert_eq!(row(&h, node.as_str()).await["host"]["file_access"], report);
    assert_eq!(node_events(&seen, node.as_str()), 5);
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
        node_checks::node_host_is_not_null_with_local_with_value(&h.store).unwrap() == 1
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
    vitals::sample_once(None, Some(&h.svc)).await;
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
    let initial = row(&h, n).await;
    let generation = initial["generation"].as_i64().unwrap();
    let at = initial["last_seen"].as_i64().unwrap() + 100;
    let events = node_events(&seen, n);
    for (generation, at, closed) in [
        (generation, at, false),
        (generation, at - 1, false),
        (generation - 1, at + 1, true),
    ] {
        h.store
            .write(|w| steve_store::node_directory::observe_link(w, n, generation, at, closed))
            .unwrap();
    }
    assert_eq!(row(&h, n).await["last_seen"], at);
    assert_eq!(node_events(&seen, n), events);

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

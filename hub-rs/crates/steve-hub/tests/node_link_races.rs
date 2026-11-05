//! What can land between a Node's handshake and its link being installed: a revoke, a pause, a
//! newer link. And the pairing limiter, which no stranger can drain for everyone.

mod node_harness;

use std::sync::Arc;
use std::time::Duration;

use node_harness::*;
use serde_json::json;
use steve_hub::nodes::listener::install_link;
use steve_node_api::NodeId;
use steve_node_wire::testkit::pair;
use steve_node_wire::{Conn, ConnOptions, Handler, close};
use steve_store::StoreError;

struct Nothing;
#[async_trait::async_trait]
impl Handler for Nothing {
    async fn handle(
        &self,
        m: &str,
        _: serde_json::Value,
    ) -> steve_node_api::ApiResult<serde_json::Value> {
        Err(steve_node_wire::conn::unsupported(m))
    }
}

/// What the handshake committed: generation + 1, as `Admit::admit` does.
fn admitted(h: &Hub, node: &NodeId) -> u64 {
    h.store
        .transaction::<_, StoreError>(|tx| {
            Ok(steve_store::nodes::begin_link(tx, node.as_str(), 1, 1)?
                .unwrap()
                .generation as u64)
        })
        .unwrap()
}

/// The Hub's end of a link at `generation` (the Node's end kept alive by the caller).
fn hub_end(generation: u64) -> (Arc<Conn>, steve_node_wire::testkit::End) {
    let (h, n) = pair();
    let c = Conn::spawn(
        h.sink,
        h.stream,
        generation,
        Arc::new(Nothing),
        ConnOptions {
            side: "h",
            ..Default::default()
        },
    );
    (c, n)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoke_between_the_handshake_and_the_install_keeps_the_link_out() {
    let h = Hub::start().await;
    let (_, node) = h.paired("u1").await;
    let generation = admitted(&h, &node);
    // the owner revokes now: nothing is in the registry to kill yet
    let (s, _) = h
        .post("u1", &format!("/api/nodes/{node}/revoke"), json!({}))
        .await;
    assert_eq!(s, 200);
    let (conn, _end) = hub_end(generation);
    assert_eq!(install_link(&h.svc, &node, conn.clone(), false).await, None);
    let (code, _) = tokio::time::timeout(Duration::from_secs(2), conn.closed())
        .await
        .unwrap();
    assert_eq!(code, close::DENIED);
    assert_eq!(h.svc.registry.current(node.as_str()), None);
    assert_eq!(h.fleet.remote(&node).generation(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pause_between_the_handshake_and_the_install_is_what_counts() {
    let h = Hub::start().await;
    let (_, node) = h.paired("u1").await;
    let generation = admitted(&h, &node); // welcomed as active
    let (s, _) = h
        .post("u1", &format!("/api/nodes/{node}/pause"), json!({}))
        .await;
    assert_eq!(s, 200);
    let (conn, _end) = hub_end(generation);
    assert_eq!(
        install_link(&h.svc, &node, conn.clone(), false).await,
        Some(true),
        "paused, as the store says, not as the welcome said"
    );
    assert_eq!(h.svc.registry.current(node.as_str()), Some(generation));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_link_installed_last_is_fenced_and_the_newer_stays() {
    let h = Hub::start().await;
    let (_, node) = h.paired("u1").await;
    let older = admitted(&h, &node);
    let newer = admitted(&h, &node);
    let (b, _eb) = hub_end(newer);
    let (a, _ea) = hub_end(older);
    assert_eq!(
        install_link(&h.svc, &node, b.clone(), false).await,
        Some(false)
    );
    assert_eq!(install_link(&h.svc, &node, a.clone(), false).await, None);
    let (code, _) = tokio::time::timeout(Duration::from_secs(2), a.closed())
        .await
        .unwrap();
    assert_eq!(code, close::FENCED);
    assert_eq!(h.fleet.remote(&node).generation(), Some(newer));
    assert_eq!(h.svc.registry.current(node.as_str()), Some(newer));
    assert!(!b.is_closed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn garbage_claims_do_not_lock_out_a_real_pairing() {
    let h = Hub::start().await;
    // every claim reaches the Hub from the proxy's loopback address: a stranger's garbage must
    // not drain the bucket the owner's Node needs
    let junk = steve_node_wire::Identity::generate().unwrap();
    for i in 0..20 {
        let (s, _) = h.claim(&format!("stv1-junk{i}"), &junk).await;
        assert_eq!(s, 403);
    }
    let id = steve_node_wire::Identity::generate().unwrap();
    let code = h.code("u1").await;
    let (s, v) = h.claim(&code, &id).await;
    assert_eq!(s, 200, "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_code_hammered_is_slowed() {
    let h = Hub::start().await;
    let junk = steve_node_wire::Identity::generate().unwrap();
    let mut slowed = false;
    for _ in 0..10 {
        let (s, _) = h.claim("stv1-same", &junk).await;
        slowed |= s == 429;
    }
    assert!(slowed);
}

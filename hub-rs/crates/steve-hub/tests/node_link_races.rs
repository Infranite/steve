//! What can land between a Node's handshake and its link being installed: a revoke, a pause, a
//! newer link. And the pairing limiter, which no stranger can drain for everyone.

mod node_harness;

use std::sync::Arc;
use std::time::Duration;
use steve_runtime::testkit::eventually_sync;

use node_harness::*;
use serde_json::json;
use steve_hub::nodes::listener::install_link;
use steve_node_api::{EventSink, NodeId};
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
        .write::<_, StoreError>(|tx| {
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

/// An event batch about work this Node does not hold: refused `denied` once the link is installed.
fn foreign_batch() -> steve_node_api::EventBatch {
    use steve_node_api::*;
    EventBatch {
        events: vec![NodeEvent {
            schema: 1,
            id: EventId::new("early_1").unwrap(),
            at: Timestamp(1),
            cursor: Watermark {
                stream: StreamId::new("st_early").unwrap(),
                epoch: Epoch::new("e1").unwrap(),
                seq: 1,
            },
            subject: Subject::Session {
                session: SessionId::new("s_not_here").unwrap(),
            },
            body: EventBody::TextDelta {
                message: "m".into(),
                block: "b".into(),
                utf8_offset: 0,
                text: "early".into(),
            },
        }],
    }
}

/// A Node may send as soon as its welcome arrives, before the Hub has installed its link: the
/// request waits for the install instead of being refused `stale`. While installs are held it
/// cannot be answered at all, so the check below cannot fail by timing; without the wait it was
/// answered `stale` at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_sent_before_the_install_waits_for_it() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let held = h.svc.hold_installs().await;
    let l = dial(
        &h,
        &id,
        &node,
        steve_node_api::testkit::FakeNode::new(node.as_str()),
        1,
    )
    .await
    .expect("welcomed");
    let link = l.link.clone();
    let mut sent = tokio::spawn(async move {
        steve_node_wire::RemoteSink(link)
            .commit(foreign_batch())
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut sent)
            .await
            .is_err(),
        "answered before the link was installed"
    );
    drop(held);
    let r = sent.await.unwrap();
    assert!(
        matches!(&r, Err(e) if e.code == steve_node_api::Code::Forbidden),
        "judged as the installed link: {r:?}"
    );
    assert_eq!(h.svc.registry.current(node.as_str()), Some(l.generation));
}

/// A link that is never installed (revoked between the handshake and the install) does not leave
/// a request it sent early waiting: it is answered `stale`, or the link closes under it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_sent_before_a_refused_install_does_not_hang() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let held = h.svc.hold_installs().await;
    let l = dial(
        &h,
        &id,
        &node,
        steve_node_api::testkit::FakeNode::new(node.as_str()),
        1,
    )
    .await
    .expect("welcomed");
    let link = l.link.clone();
    let mut sent = tokio::spawn(async move {
        steve_node_wire::RemoteSink(link)
            .commit(foreign_batch())
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut sent)
            .await
            .is_err(),
        "answered before the install was decided"
    );
    // the revoke commits, then waits for the install lock this test holds: release it once the
    // store says revoked, which is what the held install will read
    let url = format!("{}/api/nodes/{node}/revoke", h.url());
    let revoke = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url)
            .header("x-test-user", "u1")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    });
    eventually_sync("revoked in the store", || {
        steve_store::testkit::hub::node_checks::node_state_by_node_id(&h.store, node.as_str())
            .unwrap()
            == "revoked"
    })
    .await;
    drop(held);
    assert_eq!(revoke.await.unwrap(), 200);
    let r = tokio::time::timeout(Duration::from_secs(10), sent)
        .await
        .expect("answered once the install was refused")
        .unwrap();
    assert!(
        matches!(&r, Err(e) if e.code != steve_node_api::Code::Forbidden),
        "{r:?}"
    );
    assert_eq!(h.svc.registry.current(node.as_str()), None);
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

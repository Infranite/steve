//! Pairing, confirmation, pause and revocation of remote Nodes, over HTTP and a real WebSocket.

mod node_harness;

use std::sync::Arc;
use std::time::Duration;

use node_harness::*;
use serde_json::json;
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;
use steve_node_wire::{Identity, RemoteSink, identity};
use steve_store::StoreError;

/// Every text and blob in the Node tables, to look for secrets.
fn everything(h: &Hub) -> Vec<String> {
    h.store
        .reader(|c| {
            let mut out = vec![];
            for t in ["nodes", "node_pairings", "node_audit"] {
                let mut st = c.prepare(&format!("SELECT * FROM {t}"))?;
                let n = st.column_count();
                let rows = st.query_map([], |r| {
                    let mut v = vec![];
                    for i in 0..n {
                        let s = match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into(),
                            rusqlite::types::ValueRef::Blob(b) => hex::encode(b),
                            other => format!("{other:?}"),
                        };
                        v.push(s);
                    }
                    Ok(v)
                })?;
                for r in rows {
                    out.extend(r?);
                }
            }
            Ok::<_, StoreError>(out)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_hashes_are_stored() {
    let h = Hub::start().await;
    let code = h.code("u1").await;
    assert!(code.starts_with("stv1-"));
    let id = Identity::from_seed([42; 32]);
    let (s, _) = h.claim(&code, &id).await;
    assert_eq!(s, 200);
    let stored: String = h.q("SELECT code_hash FROM node_pairings");
    assert_eq!(stored, hex::encode(identity::sha256(code.as_bytes())));
    let all = everything(&h);
    let seed = hex::encode([42u8; 32]);
    for v in &all {
        assert!(!v.contains(&code), "the code is stored in clear: {v}");
        assert!(!v.contains(&seed), "the private key is stored: {v}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_used_and_expired_codes_read_the_same() {
    let h = Hub::start().await;
    let id = Identity::generate().unwrap();
    let used = h.code("u1").await;
    assert_eq!(h.claim(&used, &id).await.0, 200);
    let expired = h.code("u1").await;
    h.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "UPDATE node_pairings SET expires=1 WHERE claimed_at IS NULL",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let mut answers = vec![];
    for code in [used.as_str(), expired.as_str(), "stv1-garbage"] {
        answers.push(h.claim(code, &Identity::generate().unwrap()).await);
    }
    for a in &answers {
        assert_eq!(a, &(403, json!({"error": "denied"})));
    }
    // a signature that does not prove the key is denied the same way
    let fresh = h.code("u1").await;
    let other = Identity::generate().unwrap();
    let pk = id.public();
    let (s, v) = h
        .post(
            "",
            "/node/pair",
            json!({"code": fresh, "pubkey": hex::encode(pk), "name": "x",
                   "sig": hex::encode(other.sign(&identity::pair_message(&fresh, &pk)))}),
        )
        .await;
    assert_eq!((s, v), (403, json!({"error": "denied"})));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_claims_of_one_code_one_wins() {
    let h = Arc::new(Hub::start().await);
    let code = h.code("u1").await;
    let mut tasks = vec![];
    for _ in 0..4 {
        let (h, code) = (h.clone(), code.clone());
        tasks.push(tokio::spawn(async move {
            h.claim(&code, &Identity::generate().unwrap()).await.0
        }));
    }
    let mut won = 0;
    for t in tasks {
        if t.await.unwrap() == 200 {
            won += 1;
        }
    }
    assert_eq!(won, 1);
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM nodes"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_owner_confirms_and_a_pending_node_is_refused() {
    let h = Hub::start().await;
    let id = Identity::generate().unwrap();
    let code = h.code("u1").await;
    let (_, v) = h.claim(&code, &id).await;
    let node = NodeId::new(v["node_id"].as_str().unwrap()).unwrap();
    let fake = FakeNode::new(node.as_str());
    let e = dial(&h, &id, &node, fake.clone(), 1).await.err().unwrap();
    assert_eq!(e.code, "denied", "pending");
    let (s, _) = h
        .post("u2", &format!("/api/nodes/{node}/confirm"), json!({}))
        .await;
    assert_eq!(s, 409, "not the owner");
    let (_, list) = h.post("u2", "/api/nodes", json!({})).await;
    let _ = list;
    let (s, _) = h
        .post("u1", &format!("/api/nodes/{node}/confirm"), json!({}))
        .await;
    assert_eq!(s, 200);
    let l = dial(&h, &id, &node, fake, 1).await.unwrap();
    assert_eq!(l.generation, 1);
    // the confirm page shows the fingerprint the CLI printed
    let r = reqwest::Client::new()
        .get(format!("{}/api/nodes", h.url()))
        .header("x-test-user", "u1")
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["nodes"][0]["fingerprint"], id.fingerprint());
    assert_eq!(v["nodes"][0]["linked"], true);
    assert!(v["nodes"][0].get("pubkey").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_a_live_node_closes_it_and_refuses_it_after() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let fake = FakeNode::new(node.as_str());
    let l = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    until("linked", 5, || {
        h.svc.registry.current(node.as_str()).is_some()
    })
    .await;
    let (s, _) = h
        .post("u1", &format!("/api/nodes/{node}/revoke"), json!({}))
        .await;
    assert_eq!(s, 200);
    let (code, _) = tokio::time::timeout(Duration::from_secs(1), l.conn.closed())
        .await
        .expect("closed within a second");
    assert_eq!(code, steve_node_wire::close::DENIED);
    // late frames go nowhere
    let late = l
        .conn
        .call("events.commit", json!({"events": []}), None)
        .await;
    assert_eq!(late.unwrap_err().code, ErrorCode::Offline);
    let e = dial(&h, &id, &node, fake, 2).await.err().unwrap();
    assert_eq!(e.code, "denied");
    assert_eq!(
        h.q::<String>(&format!("SELECT state FROM nodes WHERE node_id='{node}'")),
        "revoked"
    );
    assert!(h.q::<i64>("SELECT COUNT(*) FROM node_audit WHERE what='revoke'") == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_keeps_the_link_and_holds_new_work() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    h.session("s1");
    let rt = h.bind(&node, "s1");
    let fake = FakeNode::new(node.as_str());
    let l = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    fake.connect(Arc::new(RemoteSink(l.link.clone())));
    let (s, _) = h
        .post("u1", &format!("/api/nodes/{node}/pause"), json!({}))
        .await;
    assert_eq!(s, 200);
    assert!(rt.is_holding());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!l.conn.is_closed());
    // a paused Node still links (welcome says paused)
    let l2 = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    assert_eq!(l2.generation, 2);
    let (s, _) = h
        .post("u1", &format!("/api/nodes/{node}/resume"), json!({}))
        .await;
    assert_eq!(s, 200);
    assert!(!rt.is_holding());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_text_is_only_for_loopback() {
    let home = tempfile::tempdir().unwrap();
    let home = steve_node_cli::config::Home(home.path().to_path_buf());
    let e = steve_node_cli::dial::pair(&home, "http://10.1.2.3:7777", "stv1-x", None, "n")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("127.0.0.1"), "{e}");
    let e = steve_node_cli::dial::pair(&home, "ftp://hub", "stv1-x", None, "n")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("https"), "{e}");
}

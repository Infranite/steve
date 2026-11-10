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
    // the Hub installs the link after its welcome went out: the Node may be through the handshake
    // before the list says linked
    until("linked", 5, || {
        h.svc.registry.current(node.as_str()).is_some()
    })
    .await;
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

/// M5: links that never finish the handshake hold at most MAX_UNPROVEN places; more are turned
/// away before the upgrade, and a place comes back when its link goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn links_in_the_handshake_are_bounded() {
    use steve_node_wire::tls::{Trust, connect};
    let h = Hub::start().await;
    let mut open = vec![];
    for _ in 0..steve_hub::nodes::MAX_UNPROVEN {
        open.push(
            connect(&h.ws_url(), &Trust::LoopbackPlaintext)
                .await
                .expect("a place"),
        );
    }
    assert!(
        connect(&h.ws_url(), &Trust::LoopbackPlaintext)
            .await
            .is_err(),
        "one too many"
    );
    drop(open.pop());
    let mut again = false;
    for _ in 0..100 {
        if connect(&h.ws_url(), &Trust::LoopbackPlaintext)
            .await
            .is_ok()
        {
            again = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(again, "the place came back");
}

fn rotation(h: &Hub, node: &NodeId, old: &Identity, new: &Identity) -> serde_json::Value {
    let msg = identity::rotate_message(&h.svc.hub_id, node.as_str(), &old.public(), &new.public());
    json!({
        "node_id": node.as_str(),
        "new_pubkey": hex::encode(new.public()),
        "sig_old": hex::encode(old.sign(&msg)),
        "sig_new": hex::encode(new.sign(&msg)),
    })
}

/// M4: a Node replaces its key with both keys' signatures; its old link goes, the old key is
/// refused from then on, the new one is welcome, and the change is audited. Neither a replay nor a
/// change signed by one key only does anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_rotates_its_key() {
    let h = Hub::start().await;
    let (old, node) = h.paired("u1").await;
    let l = dial(&h, &old, &node, FakeNode::new(node.as_str()), 1)
        .await
        .unwrap();
    until("linked", 5, || {
        h.svc.registry.current(node.as_str()).is_some()
    })
    .await;
    let new = Identity::generate().unwrap();

    // signed by the old key only: the new key is not proven held
    let mut half = rotation(&h, &node, &old, &new);
    half["sig_new"] = half["sig_old"].clone();
    assert_eq!(h.post("", "/node/rotate", half).await.0, 403);

    let (s, v) = h
        .post("", "/node/rotate", rotation(&h, &node, &old, &new))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["fingerprint"], new.fingerprint());
    until("the old key's link closed", 5, || l.conn.is_closed()).await;
    assert!(
        dial(&h, &old, &node, FakeNode::new(node.as_str()), 2)
            .await
            .is_err()
    );
    assert!(
        dial(&h, &new, &node, FakeNode::new(node.as_str()), 3)
            .await
            .is_ok()
    );
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM node_audit WHERE what='key_rotated'"),
        1
    );
    // the same request again: the old key is no longer this Node's
    assert_eq!(
        h.post("", "/node/rotate", rotation(&h, &node, &old, &new))
            .await
            .0,
        403
    );
}

/// M4: one key held by two parties shows as the Node's live link taken over again and again: the
/// Node is paused (no new work), audited, and its owner told.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_taken_over_again_and_again_pauses_the_node() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let mut links = vec![];
    for i in 0..=steve_hub::nodes::registry::TAKEOVERS as u64 {
        let l = dial(&h, &id, &node, FakeNode::new(node.as_str()), i + 1)
            .await
            .unwrap();
        until("installed", 5, || {
            h.svc.registry.current(node.as_str()) == Some(l.generation)
        })
        .await;
        links.push(l);
    }
    until("paused", 5, || {
        h.q::<String>(&format!("SELECT state FROM nodes WHERE node_id='{node}'")) == "paused"
    })
    .await;
    // one transaction: whoever sees the pause sees its audit line and the owner's notice too
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM node_audit WHERE what='takeover_suspected'"),
        1
    );
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM messages WHERE space='notify'"),
        1
    );
}

/// M4, the Node's side: `steve-node rotate` puts the new key in place only once the Hub took it,
/// keeps the old one aside, and the Hub then knows the Node by the new fingerprint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steve_node_rotate_swaps_the_key_on_disk() {
    // SAFETY: plain loopback for this test's Hub; set before anything reads it
    unsafe { std::env::set_var("STEVE_NODE_LOOPBACK_PLAINTEXT", "1") };
    let h = Hub::start().await;
    h.owns_default("u1");
    let dir = tempfile::tempdir().unwrap();
    let home = steve_node_cli::config::Home(dir.path().to_path_buf());
    let code = h.code("u1").await;
    let p = steve_node_cli::dial::pair(&home, &h.url(), &code, None, "laptop")
        .await
        .unwrap();
    // a pending Node has nothing to rotate yet: confirmed first
    assert!(steve_node_cli::dial::rotate(&home).await.is_err());
    assert!(!home.identity().with_extension("key.next").exists());
    let (s, _) = h
        .post(
            "u1",
            &format!("/api/nodes/{}/confirm", p.node_id),
            json!({}),
        )
        .await;
    assert_eq!(s, 200);
    let before = Identity::load(&home.identity()).unwrap();
    let fp = steve_node_cli::dial::rotate(&home).await.unwrap();
    let after = Identity::load(&home.identity()).unwrap();
    assert_ne!(after.public(), before.public());
    assert_eq!(fp, after.fingerprint());
    let kept = Identity::load(&home.identity().with_extension("key.previous")).unwrap();
    assert_eq!(kept.public(), before.public());
    assert!(!home.identity().with_extension("key.next").exists());
    let stored: String = h.q(&format!(
        "SELECT fingerprint FROM nodes WHERE node_id='{}'",
        p.node_id
    ));
    assert_eq!(stored, fp);
}

/// B5: a session on a device away long enough, with a message waiting, is due to go on elsewhere;
/// one with nothing waiting, one whose device is linked, or one away too short is not. Away counts
/// from when it was last seen, and never from before the Hub started.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn work_waiting_for_a_device_long_away_is_due_to_move() {
    use steve_hub::nodes::handover::due;
    let h = Hub::start().await;
    let (_id, node) = h.paired("u1").await;
    h.session("s_wait");
    h.session("s_idle");
    h.store
        .transaction::<_, StoreError>(|tx| {
            for s in ["s_wait", "s_idle"] {
                tx.execute(
                    "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,?2,'{}',?1,'r','ready',1)",
                    [s, node.as_str()],
                )?;
            }
            tx.execute("INSERT INTO queued(id,session_id,text,via,created) VALUES ('q1','s_wait','hi','u',1)", [])?;
            tx.execute("UPDATE nodes SET last_seen=0 WHERE node_id=?", [node.as_str()])?;
            Ok(())
        })
        .unwrap();
    let min = 60_000;
    let d = due(&h.store, &h.svc, 30, 0, 45 * min);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].0.as_str(), d[0].2), ("s_wait", 45));
    assert!(
        due(&h.store, &h.svc, 30, 0, 20 * min).is_empty(),
        "not long enough"
    );
    assert!(
        due(&h.store, &h.svc, 30, 40 * min, 45 * min).is_empty(),
        "the Hub has been up only 5 minutes"
    );
}

//! The signed hello: the right key on the right challenge gets a generation; everything else gets
//! the same `denied`, and the link closes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use steve_node_api::NodeId;
use steve_node_wire::envelope::{Envelope, Frame};
use steve_node_wire::handshake::{self, Admission, Admitted, Hello, NodeIntro};
use steve_node_wire::testkit::pair;
use steve_node_wire::{Identity, close};

#[derive(Default)]
struct Mem {
    keys: Mutex<HashMap<String, [u8; 32]>>,
    generation: Mutex<u64>,
    /// The store fails.
    broken: Mutex<bool>,
}

#[async_trait]
impl Admission for Mem {
    async fn key(&self, node: &NodeId) -> Result<Option<[u8; 32]>, String> {
        if *self.broken.lock().unwrap() {
            return Err("database is locked".into());
        }
        Ok(self.keys.lock().unwrap().get(node.as_str()).copied())
    }
    async fn admit(&self, _: &Hello) -> Result<Option<Admitted>, String> {
        let mut g = self.generation.lock().unwrap();
        *g += 1;
        Ok(Some(Admitted {
            generation: *g,
            paused: false,
        }))
    }
}

fn nid(s: &str) -> NodeId {
    NodeId::new(s).unwrap()
}

async fn run(
    adm: Arc<Mem>,
    node: &str,
    id: &Identity,
) -> (
    Result<(Hello, Admitted), handshake::Refused>,
    Result<(handshake::Challenge, handshake::Welcome), handshake::Refused>,
    Option<Frame>,
) {
    let (mut h, mut n) = pair();
    let hub = tokio::spawn(async move {
        let r = handshake::accept(&mut h.sink, &mut h.stream, "hub_1", adm.as_ref()).await;
        (r, h)
    });
    let nodeid = nid(node);
    let r = handshake::dial(
        &mut n.sink,
        &mut n.stream,
        NodeIntro {
            node: &nodeid,
            identity: id,
            incarnation: 3,
            heads: vec![],
            caps: None,
            hub_id: Some("hub_1"),
            hub_key: None,
        },
    )
    .await;
    let (hr, _h) = hub.await.unwrap();
    // after a refusal the Hub closed: the last frame the Node sees is the close
    let mut last = None;
    if r.is_err() {
        while let Ok(Some(Ok(f))) =
            tokio::time::timeout(std::time::Duration::from_millis(200), n.stream.next()).await
        {
            last = Some(f);
        }
    }
    (hr, r, last)
}

#[tokio::test]
async fn the_right_key_is_welcomed_with_a_new_generation() {
    let adm = Arc::new(Mem::default());
    let id = Identity::generate().unwrap();
    adm.keys.lock().unwrap().insert("n1".into(), id.public());
    let (h, n, _) = run(adm.clone(), "n1", &id).await;
    let (hello, a) = h.unwrap();
    assert_eq!(hello.incarnation, 3);
    assert_eq!(n.unwrap().1.generation, a.generation);
    let (_, n2, _) = run(adm, "n1", &id).await;
    assert_eq!(n2.unwrap().1.generation, 2, "each link a new generation");
}

#[tokio::test]
async fn every_refusal_reads_the_same() {
    let adm = Arc::new(Mem::default());
    let id = Identity::generate().unwrap();
    let other = Identity::generate().unwrap();
    adm.keys.lock().unwrap().insert("n1".into(), id.public());
    adm.keys.lock().unwrap().insert("n2".into(), other.public());
    // wrong key, unknown node, another node's id signed with one's own key
    for (node, key) in [("n1", &other), ("ghost", &id), ("n2", &id)] {
        let (h, n, last) = run(adm.clone(), node, key).await;
        let e = h.unwrap_err();
        assert_eq!(e.code, "denied");
        assert_eq!(e.detail, "this node may not connect");
        let ne = n.unwrap_err();
        assert_eq!(
            (ne.code.as_str(), ne.detail.as_str()),
            ("denied", "this node may not connect")
        );
        assert_eq!(last, Some(Frame::Close(close::DENIED, "denied".into())));
    }
    assert_eq!(*adm.generation.lock().unwrap(), 0, "nothing was admitted");
}

/// Plays the Node by hand, to send what a real one never would.
async fn manual(
    adm: Arc<Mem>,
    tamper: impl FnOnce(&mut Hello, &handshake::Challenge, &Identity),
) -> handshake::Refused {
    let (mut h, mut n) = pair();
    let id = Identity::generate().unwrap();
    adm.keys.lock().unwrap().insert("n1".into(), id.public());
    let hub = tokio::spawn(async move {
        handshake::accept(&mut h.sink, &mut h.stream, "hub_1", adm.as_ref()).await
    });
    let Some(Ok(Frame::Text(t))) = n.stream.next().await else {
        panic!()
    };
    let ch: handshake::Challenge = serde_json::from_value(Envelope::decode(&t).unwrap().p).unwrap();
    let mut hello = Hello {
        node_id: nid("n1"),
        incarnation: 1,
        proto: steve_node_api::PROTOCOL,
        codecs: vec!["json".into()],
        heads: vec![],
        caps: None,
        sig: String::new(),
    };
    hello.sig = hex::encode(id.sign(&handshake::hello_message(&ch, &hello)));
    tamper(&mut hello, &ch, &id);
    let env = Envelope::ctl("x", "hello", 0, serde_json::to_value(&hello).unwrap());
    n.sink
        .send(Frame::Text(env.encode().unwrap()))
        .await
        .unwrap();
    hub.await.unwrap().unwrap_err()
}

#[tokio::test]
async fn tampering_is_denied() {
    // a field flipped after signing
    let e = manual(Arc::new(Mem::default()), |h, _, _| h.incarnation = 99).await;
    assert_eq!(e.code, "denied");
    // a signature made for another challenge (another nonce and connection)
    let e = manual(Arc::new(Mem::default()), |h, ch, id| {
        let mut old = ch.clone();
        old.nonce = "00".repeat(32);
        old.conn_id = "c_old".into();
        h.sig = String::new();
        h.sig = hex::encode(id.sign(&handshake::hello_message(&old, h)));
    })
    .await;
    assert_eq!(e.code, "denied");
    // garbage signature
    let e = manual(Arc::new(Mem::default()), |h, _, _| h.sig = "zz".into()).await;
    assert_eq!(e.code, "denied");
    // another protocol major
    let e = manual(Arc::new(Mem::default()), |h, ch, id| {
        h.proto = (steve_node_api::PROTOCOL.0 + 1, 0);
        h.sig = String::new();
        h.sig = hex::encode(id.sign(&handshake::hello_message(ch, h)));
    })
    .await;
    assert_eq!(e.code, "unsupported");
}

#[tokio::test]
async fn a_node_refuses_a_hub_it_did_not_pair_with() {
    let adm = Arc::new(Mem::default());
    let id = Identity::generate().unwrap();
    adm.keys.lock().unwrap().insert("n1".into(), id.public());
    let (mut h, mut n) = pair();
    tokio::spawn(async move {
        let _ = handshake::accept(&mut h.sink, &mut h.stream, "hub_evil", adm.as_ref()).await;
    });
    let nodeid = nid("n1");
    let e = handshake::dial(
        &mut n.sink,
        &mut n.stream,
        NodeIntro {
            node: &nodeid,
            identity: &id,
            incarnation: 1,
            heads: vec![],
            caps: None,
            hub_id: Some("hub_1"),
            hub_key: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "denied");
}

#[tokio::test]
async fn a_hub_that_cannot_decide_does_not_refuse() {
    // a busy or failing store is not a verdict on the Node: `internal`, which it dials again on
    let adm = Arc::new(Mem::default());
    let id = Identity::generate().unwrap();
    adm.keys.lock().unwrap().insert("n1".into(), id.public());
    *adm.broken.lock().unwrap() = true;
    let (h, n, last) = run(adm.clone(), "n1", &id).await;
    assert_eq!(h.unwrap_err().code, handshake::INTERNAL);
    let ne = n.unwrap_err();
    assert_eq!(ne.code, handshake::INTERNAL);
    assert!(!ne.detail.contains("locked"), "the store's text stays home");
    assert_eq!(last, Some(Frame::Close(close::INTERNAL, "internal".into())));
    assert_eq!(*adm.generation.lock().unwrap(), 0);
}

#[tokio::test]
async fn a_bare_close_is_not_a_refusal() {
    // a proxy or a restarting Hub closing the socket: the Node hears `offline`, not `denied`
    let (mut h, mut n) = pair();
    h.sink
        .send(Frame::Close(close::GOODBYE, "restart".into()))
        .await
        .unwrap();
    let id = Identity::generate().unwrap();
    let nodeid = nid("n1");
    let e = handshake::dial(
        &mut n.sink,
        &mut n.stream,
        NodeIntro {
            node: &nodeid,
            identity: &id,
            incarnation: 1,
            heads: vec![],
            caps: None,
            hub_id: Some("hub_1"),
            hub_key: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, "offline");
}

/// One handshake between a Hub that signs with `hub` (or not) and a Node that expects `known`.
async fn run_signed(
    hub: Option<Identity>,
    known: Option<[u8; 32]>,
) -> Result<(handshake::Challenge, handshake::Welcome), handshake::Refused> {
    let adm = Arc::new(Mem::default());
    let id = Identity::generate().unwrap();
    adm.keys.lock().unwrap().insert("n1".into(), id.public());
    let (mut h, mut n) = pair();
    tokio::spawn(async move {
        let _ = handshake::accept_as(
            &mut h.sink,
            &mut h.stream,
            "hub_1",
            adm.as_ref(),
            hub.as_ref(),
        )
        .await;
        h
    });
    let nodeid = nid("n1");
    handshake::dial(
        &mut n.sink,
        &mut n.stream,
        NodeIntro {
            node: &nodeid,
            identity: &id,
            incarnation: 1,
            heads: vec![],
            caps: None,
            hub_id: Some("hub_1"),
            hub_key: known,
        },
    )
    .await
}

/// M1: the Hub signs its welcome; a Node that knows the Hub's key takes only a welcome that key
/// signed, whoever ends the TLS on the way.
#[tokio::test]
async fn a_node_that_knows_its_hub_takes_only_its_signed_welcome() {
    let seed = [7u8; 32];
    let key = Identity::from_seed(seed).public();
    // first contact, nothing known: the welcome proves the key it carries
    let (c, w) = run_signed(Some(Identity::from_seed(seed)), None)
        .await
        .unwrap();
    assert_eq!(w.hub_key, hex::encode(key));
    assert!(!w.sig.is_empty());
    let _ = c;
    // known: the same Hub is welcome
    assert!(
        run_signed(Some(Identity::from_seed(seed)), Some(key))
            .await
            .is_ok()
    );
    // another key, or none at all, is not the Hub this Node paired with
    let other = run_signed(Some(Identity::generate().unwrap()), Some(key))
        .await
        .unwrap_err();
    assert_eq!(other.code, handshake::DENIED);
    let unsigned = run_signed(None, Some(key)).await.unwrap_err();
    assert_eq!(unsigned.code, handshake::DENIED);
    // a Node that knows nothing still takes an unsigned welcome (a Hub of an earlier release)
    let (_, w) = run_signed(None, None).await.unwrap();
    assert!(w.hub_key.is_empty() && w.sig.is_empty());
}

/// M1: a signature over anything else than this welcome (a field changed on the way) holds for
/// nothing.
#[test]
fn a_changed_welcome_no_longer_holds() {
    use handshake::{Challenge, ProtoRange, Welcome, welcome_key, welcome_message};
    let hub = Identity::generate().unwrap();
    let c = Challenge {
        nonce: "nn".into(),
        hub_id: "hub_1".into(),
        conn_id: "cc".into(),
        proto: ProtoRange {
            min: (1, 0),
            max: (1, 0),
        },
        hub_time: 0,
    };
    let h = Hello {
        node_id: nid("n1"),
        incarnation: 1,
        proto: (1, 0),
        codecs: vec!["json".into()],
        heads: vec![],
        caps: None,
        sig: "ab".into(),
    };
    let mut w = Welcome {
        generation: 4,
        proto: (1, 0),
        paused: false,
        heartbeat_ms: 10_000,
        hub_key: hex::encode(hub.public()),
        sig: String::new(),
    };
    w.sig = hex::encode(hub.sign(&welcome_message(&c, &h, &w)));
    assert_eq!(welcome_key(&c, &h, &w).unwrap(), Some(hub.public()));
    let mut paused = w.clone();
    paused.paused = true;
    assert!(welcome_key(&c, &h, &paused).is_err());
    let mut other_conn = c.clone();
    other_conn.conn_id = "dd".into();
    assert!(welcome_key(&other_conn, &h, &w).is_err());
}

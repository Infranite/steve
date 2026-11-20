#[path = "sg27/socket.rs"]
mod socket;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use steve_node::LocalNode;
use steve_node_api::{Capacity, NodeId};
use steve_node_cli::{
    config::{Home, Pairing},
    dial,
};
use steve_node_wire::{
    Identity,
    handshake::{self, Admission, Admitted, Hello},
    tls::Trust,
};
use steve_runtime::testkit::eventually_sync;
use tokio::net::TcpListener;

struct Admit {
    key: [u8; 32],
    failure: Option<Option<&'static str>>,
    reports: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Admission for Admit {
    async fn key(&self, _: &NodeId) -> Result<Option<[u8; 32]>, String> {
        match self.failure {
            None => Ok(Some(self.key)),
            Some(None) => Ok(None),
            Some(Some(why)) => Err(why.into()),
        }
    }
    async fn admit(&self, _: &Hello) -> Result<Option<Admitted>, String> {
        Ok(Some(Admitted { generation: 1 }))
    }
    async fn reported(&self, _: &NodeId, error: &str) {
        self.reports.lock().unwrap().push(error.to_string());
    }
}

fn config(home: &Home) -> steve_node::NodeConfig {
    steve_node::NodeConfig {
        node: NodeId::new("sg27-node").unwrap(),
        home: home.local(),
        roots: vec![],
        capacity: Capacity {
            sessions: 0,
            jobs: 0,
            execs: 0,
        },
        storage: Default::default(),
        ceiling: Default::default(),
        acp: steve_acp::AcpConfig {
            adapters: Default::default(),
            declared: Default::default(),
            node: "/usr/bin/false".into(),
            claude_executable: "/usr/bin/false".into(),
            steve_home: home.0.join("steve"),
            codex_home: home.0.join("codex"),
            user_home: home.0.clone(),
            version: "test".into(),
        },
    }
}
fn node(home: &Home) -> LocalNode {
    LocalNode::open(config(home)).unwrap()
}

async fn listener() -> TcpListener {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert_ne!(l.local_addr().unwrap().port(), 8440);
    l
}

fn pairing(url: String, pin: Option<String>) -> Pairing {
    Pairing {
        hub_url: url,
        node_id: "sg27-node".into(),
        hub_id: "sg27-hub".into(),
        pin,
        loopback: true,
        hub_key: None,
        name: "Fixture".into(),
    }
}

#[tokio::test]
async fn signed_welcome_remembers_identity_and_only_updates_pins_for_a_pinned_node() {
    for (pinned, announced) in [
        (false, vec![format!("sha256:{}", "1".repeat(64))]),
        (
            true,
            vec![
                format!("sha256:{}", "1".repeat(64)),
                format!("sha256:{}", "2".repeat(64)),
            ],
        ),
        (true, vec!["invalid-pin".into()]),
        (true, vec![format!("sha256:{}", "0".repeat(64))]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let home = Home(dir.path().into());
        let key = Identity::generate().unwrap();
        let hub_key = Identity::generate().unwrap();
        let expected_key = hex::encode(hub_key.public());
        let l = listener().await;
        let original_pin = pinned.then(|| format!("sha256:{}", "0".repeat(64)));
        let p = pairing(
            format!("http://{}", l.local_addr().unwrap()),
            original_pin.clone(),
        );
        home.save_pairing(&p).unwrap();
        std::fs::write(
            home.0.join(dial::LINK_STATUS),
            r#"{"at":1,"error":"old fault"}"#,
        )
        .unwrap();
        let local = node(&home);
        let shared = local.clone();
        let root = Home(home.0.clone());
        let public = key.public();
        let announced_copy = announced.clone();
        let (release, held) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut s = socket::accept(&l).await;
            let admission = Admit {
                key: public,
                failure: None,
                reports: Default::default(),
            };
            let (hello, _) = handshake::accept_as(
                &mut s.wire.sink,
                &mut s.wire.stream,
                "sg27-hub",
                &admission,
                Some(&hub_key),
                &announced_copy,
            )
            .await
            .unwrap();
            assert_eq!(hello.node_id, "sg27-node");
            held.await.unwrap();
        });
        let serving = tokio::spawn(async move {
            dial::serve_at(shared, Some(root), &p, &key, &Trust::LoopbackPlaintext).await
        });
        assert!(
            eventually_sync("signed welcome persisted", || {
                home.pairing()
                    .is_ok_and(|p| p.hub_key.as_deref() == Some(expected_key.as_str()))
                    && dial::link_status(&home.0).is_none()
            })
            .await
        );
        let saved = home.pairing().unwrap();
        let expected_pin = if pinned && announced[0] != "invalid-pin" {
            Some(announced.join(","))
        } else {
            original_pin
        };
        assert_eq!(saved.pin, expected_pin);
        serving.abort();
        assert!(serving.await.unwrap_err().is_cancelled());
        local.shutdown().await;
        release.send(()).unwrap();
        server.await.unwrap();
        drop(local);
        let mut reopened = None;
        assert!(
            eventually_sync("journal released after connection cancellation", || {
                match LocalNode::open(config(&home)) {
                    Ok(n) => {
                        reopened = Some(n);
                        true
                    }
                    Err(_) => false,
                }
            })
            .await
        );
        reopened.unwrap().shutdown().await;
    }
}

#[tokio::test]
async fn repeated_internal_fault_is_reported_once_before_a_real_refusal_stops_redialling() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home(dir.path().into());
    let key = Identity::generate().unwrap();
    let public = key.public();
    let l = listener().await;
    let p = pairing(format!("http://{}", l.local_addr().unwrap()), None);
    let reports = Arc::new(Mutex::new(vec![]));
    let collected = reports.clone();
    let server = tokio::spawn(async move {
        for (index, failure) in [
            Some(Some("store unavailable")),
            None,
            Some(Some("store unavailable")),
            Some(None),
        ]
        .into_iter()
        .enumerate()
        {
            let mut s = socket::accept(&l).await;
            let a = Admit {
                key: public,
                failure,
                reports: collected.clone(),
            };
            let error = handshake::accept(&mut s.wire.sink, &mut s.wire.stream, "sg27-hub", &a)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                if index == 1 {
                    handshake::REPORTED
                } else if index == 3 {
                    handshake::DENIED
                } else {
                    handshake::INTERNAL
                }
            );
            s.closed().await;
        }
    });
    let local = node(&home);
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        dial::serve_at(
            local.clone(),
            Some(Home(home.0.clone())),
            &p,
            &key,
            &Trust::LoopbackPlaintext,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        error.to_string().starts_with("the hub refused this node:"),
        "{error:#}"
    );
    let (_, stored) = dial::link_status(&home.0).unwrap();
    assert!(
        stored.starts_with("the hub refused this node: denied"),
        "{stored}"
    );
    assert_eq!(reports.lock().unwrap().len(), 1);
    assert_eq!(
        reports.lock().unwrap()[0],
        "internal: the hub could not decide; dial again"
    );
    server.await.unwrap();
    local.shutdown().await;
}

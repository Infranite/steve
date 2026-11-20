//! Manual transport portion of S6. Copy this fixture to two source revisions and run both roles
//! with nextest. It uses each library's real PROTOCOL, never the version-override test seams.
//! It deliberately does not claim service replacement, journal migration or same-op recovery.
use super::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use steve_node_wire::handshake::{self, Admission, Admitted, Hello, NodeIntro};
use steve_node_wire::{Identity, ws};
use tokio::net::TcpListener;

async fn read(dir: &Path, name: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(raw) = std::fs::read(dir.join(name))
                && let Ok(value) = serde_json::from_slice(&raw)
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("cross-build peer did not produce {name}"))
}
fn write(dir: &Path, name: &str, value: Value) {
    std::fs::write(dir.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
}
fn options(side: &'static str, mode: v1::Mode) -> ConnOptions {
    ConnOptions {
        side,
        mode,
        heartbeat: Duration::from_secs(600),
        dead_after: Duration::from_secs(600),
    }
}
struct Admit([u8; 32]);
#[async_trait::async_trait]
impl Admission for Admit {
    async fn key(&self, node: &NodeId) -> Result<Option<[u8; 32]>, String> {
        Ok((node.as_str() == "cross_node").then_some(self.0))
    }
    async fn admit(&self, _: &Hello) -> Result<Option<Admitted>, String> {
        Ok(Some(Admitted { generation: 7 }))
    }
}
#[derive(Default)]
struct Counts {
    normal: AtomicUsize,
    upgrades: Mutex<Vec<String>>,
}
struct Counted {
    inner: Arc<dyn Handler>,
    counts: Arc<Counts>,
}
#[async_trait::async_trait]
impl Handler for Counted {
    async fn handle(&self, method: &str, args: Value) -> ApiResult<Value> {
        self.counts.normal.fetch_add(1, Ordering::SeqCst);
        self.inner.handle(method, args).await
    }
    async fn handle_upgrade(&self, method: &str, args: Value) -> v1::Result<Value> {
        self.counts.upgrades.lock().unwrap().push(method.into());
        self.inner.handle_upgrade(method, args).await
    }
}

async fn hub(dir: &Path) {
    let identity = Identity::generate().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    write(
        dir,
        "hub.json",
        json!({"address":listener.local_addr().unwrap().to_string(),"public_key":hex::encode(identity.public()),"protocol":PROTOCOL}),
    );
    let peer = read(dir, "node.json").await;
    let key: [u8; 32] = hex::decode(peer["public_key"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let (tcp, _) = listener.accept().await.unwrap();
    let socket = tokio_tungstenite::accept_async_with_config(tcp, Some(ws::config()))
        .await
        .unwrap();
    let (mut sink, mut stream) = ws::split(socket);
    let (hello, admitted) = handshake::accept_as(
        &mut sink,
        &mut stream,
        "cross-hub",
        &Admit(key),
        Some(&identity),
        &[],
    )
    .await
    .unwrap();
    assert_ne!(
        hello.proto, PROTOCOL,
        "this requires two different application-protocol builds"
    );
    let mode = if hello.proto == PROTOCOL {
        v1::Mode::Normal
    } else {
        v1::Mode::Upgrade
    };
    let archive = Arc::new(Archive {
        reads: AtomicUsize::new(0),
        full: false,
    });
    let conn = Conn::spawn(
        sink,
        stream,
        admitted.generation,
        Arc::new(ServeHub {
            sink: Arc::new(RecordingSink::default()),
            tools: Arc::new(common::StubTools::default()),
            files: Some(archive.clone()),
            secrets: None,
        }),
        options("cross_hub", mode),
    );
    let remote = RemoteNode::new(NodeId::new("cross_node").unwrap());
    remote.attach(conn.clone(), false);
    assert!(
        remote.probe(false).await.is_err(),
        "ordinary work must be refused"
    );
    let info = remote.probe_build().await.unwrap();
    assert!(info.upgradable);
    let mut upgrade = request("2.0.0");
    upgrade.sha256 = hex::encode(steve_node_wire::identity::sha256(b"archive"));
    assert_eq!(remote.upgrade_stage(upgrade).await.unwrap().phase, "staged");
    let fetched = read(dir, "fetched.json").await;
    assert_eq!(fetched["bytes"], 7);
    assert_eq!(
        remote
            .upgrade_switch(UpgradeSwitch {
                operation: OperationId::new("op_upgrade").unwrap(),
                abandon: false,
                force: false
            })
            .await
            .unwrap()
            .phase,
        "succeeded"
    );
    assert_eq!(
        remote
            .upgrade_status(OperationId::new("op_upgrade").unwrap())
            .await
            .unwrap()
            .phase,
        "succeeded"
    );
    assert_eq!(archive.reads.load(Ordering::SeqCst), 3);
    write(
        dir,
        "hub-result.json",
        json!({"protocol":PROTOCOL,"peer_protocol":hello.proto,"mode":"upgrade","five_methods":true,"archive_reads":3,"ordinary_refused":true,"scope":"wire_only"}),
    );
    read(dir, "node-result.json").await;
    conn.close(None, 1000, "cross-build complete");
}

async fn node(dir: &Path) {
    let identity = Identity::generate().unwrap();
    write(
        dir,
        "node.json",
        json!({"public_key":hex::encode(identity.public()),"protocol":PROTOCOL}),
    );
    let peer = read(dir, "hub.json").await;
    let key: [u8; 32] = hex::decode(peer["public_key"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let address = format!("ws://{}", peer["address"].as_str().unwrap());
    let (socket, _) =
        tokio_tungstenite::connect_async_with_config(address, Some(ws::config()), false)
            .await
            .unwrap();
    let (mut sink, mut stream) = ws::split(socket);
    let id = NodeId::new("cross_node").unwrap();
    let (_, welcome) = handshake::dial(
        &mut sink,
        &mut stream,
        NodeIntro {
            node: &id,
            identity: &identity,
            incarnation: 1,
            hub_id: Some("cross-hub"),
            hub_key: Some(key),
        },
    )
    .await
    .unwrap();
    assert_ne!(welcome.proto, PROTOCOL);
    let mode = if welcome.proto == PROTOCOL {
        v1::Mode::Normal
    } else {
        v1::Mode::Upgrade
    };
    assert_eq!(mode, v1::Mode::Upgrade);
    let fake = FakeNode::new("cross_node");
    fake.set_upgradable(true);
    fake.set_build(Some(build("1.0.0")));
    let counts = Arc::new(Counts::default());
    let conn = Conn::spawn(
        sink,
        stream,
        welcome.generation,
        Arc::new(Counted {
            inner: NodeServer::new(fake.clone()),
            counts: counts.clone(),
        }),
        options("cross_node", mode),
    );
    let link = HubLink::new();
    link.set(Some(conn.clone()));
    let files = RemoteFiles(link);
    tokio::time::timeout(Duration::from_secs(30), async {
        while fake.staged_upgrades().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut bytes = Vec::new();
    loop {
        let chunk = files
            .fetch(
                FileAccess::Operation {
                    operation: OperationId::new("op_upgrade").unwrap(),
                },
                ArtifactId::new("node-upgrade.op_upgrade").unwrap(),
                bytes.len() as u64,
                3,
            )
            .await
            .unwrap();
        bytes.extend(chunk.bytes);
        if chunk.eof {
            break;
        }
    }
    assert_eq!(bytes, b"archive");
    write(dir, "fetched.json", json!({"bytes":bytes.len()}));
    read(dir, "hub-result.json").await;
    assert_eq!(counts.normal.load(Ordering::SeqCst), 0);
    assert_eq!(
        *counts.upgrades.lock().unwrap(),
        [
            v1::PROBE_BUILD,
            v1::UPGRADE_STAGE,
            v1::UPGRADE_SWITCH,
            v1::UPGRADE_STATUS
        ]
    );
    write(
        dir,
        "node-result.json",
        json!({"protocol":PROTOCOL,"peer_protocol":welcome.proto,"mode":"upgrade","ordinary_handler_calls":0,"node_methods":4,"scope":"wire_only"}),
    );
    // The Hub acknowledges receipt of this report by closing the link.
    tokio::time::timeout(Duration::from_secs(30), conn.closed())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual two-build S6 wire check; run hub and node roles with nextest"]
async fn real_build_role() {
    let dir = PathBuf::from(
        std::env::var_os("STEVE_CROSS_BUILD_DIR").expect("isolated result directory"),
    );
    match std::env::var("STEVE_CROSS_BUILD_ROLE").as_deref() {
        Ok("hub") => hub(&dir).await,
        Ok("node") => node(&dir).await,
        _ => panic!("choose the hub or node role"),
    }
}

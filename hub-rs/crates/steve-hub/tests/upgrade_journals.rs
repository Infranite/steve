//! An aborted pre-commit Hub upgrade cannot upgrade either local Node journal.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use steve_runtime::testkit::eventually;

mod handed;

const VERSION: &str = "0.9.0";
const V3: &str = include_str!("../../steve-node/src/journal/fixtures/v3.sql");

fn atomic(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(value).unwrap()).unwrap();
    std::fs::rename(tmp, path).unwrap();
}

fn old_journal(home: &Path, name: &str) -> PathBuf {
    let path = home.join("data").join(name).join("node.sqlite3");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(V3).unwrap();
    db.execute_batch("INSERT INTO meta(key,value) VALUES ('journal_id',123),('incarnation',0);")
        .unwrap();
    db.pragma_update(None, "user_version", 3).unwrap();
    path
}

fn assert_v3(paths: &[PathBuf]) {
    for path in paths {
        let db =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 3, "{}", path.display());
        let ledger: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('ops') WHERE name='receipt')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(ledger, "the old ledger is intact");
    }
}

async fn verifying_failure_keeps_journals(mode: &str, selected: usize) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("steve");
    let journals = [old_journal(&home, "node"), old_journal(&home, "node-jobs")];
    let original: Vec<_> = journals
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let web = dir.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<html></html>").unwrap();
    atomic(&web.join("release.json"), &json!({"version": VERSION}));
    let id = uuid::Uuid::new_v4().to_string();
    let target = home.join("releases/candidate");
    atomic(
        &target.join("release.json"),
        &json!({
            "format_version":1, "version":VERSION, "commit":"a".repeat(40),
            "hub_version":VERSION,"web_version":VERSION,"api_protocol":1,
            "schema":{"min_read":steve_store::db::SCHEMA_VERSION,
                "max_read":steve_store::db::SCHEMA_VERSION,"target":steve_store::db::SCHEMA_VERSION},
            "assets":[]
        }),
    );
    let operation = home.join("updates/operations").join(format!("{id}.json"));
    let op = |phase: &str| {
        json!({"id":id,"idempotency_key":"journal-gate", "version":VERSION,
        "phase":phase,"created_at":1,"updated_at":1,"requested_by":"test",
        "previous":null,"target":target,"error":null})
    };
    atomic(&operation, &op("verifying"));
    let socket = handed::Socket::new();
    let base = format!("http://127.0.0.1:{}", socket.port());
    let mut command = handed::command(env!("CARGO_BIN_EXE_steve-hub"), &[("hub", &socket)]);
    command
        .env("HOME", dir.path())
        .env("STEVE_HOME", &home)
        .env("STEVE_PORT", socket.port().to_string())
        .env("STEVE_HOST", "127.0.0.1")
        .env("STEVE_PUBLIC_URL", &base)
        .env("STEVE_WEB_DIR", &web)
        .env("STEVE_VERSION", VERSION)
        .env("STEVE_UPGRADE_ID", &id)
        .env("STEVE_NODE_MODE", mode)
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_BACKUP", "off")
        .env_remove("STEVE_NO_RESUME")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env_remove("STEVE_KB_STORE")
        .env_remove("STEVE_OIDC_ISSUER")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut hub = command.spawn().unwrap();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .unwrap();
    eventually("candidate is behind the real startup gate", || async {
        let Ok(r) = client.get(format!("{base}/healthz")).send().await else {
            return false;
        };
        r.json::<Value>()
            .await
            .is_ok_and(|v| v["maintenance"] == true && v["upgrade_id"] == id)
    })
    .await;
    assert_v3(&journals);
    assert_eq!(
        client
            .get(format!("{base}/api/state"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        503
    );
    // The supervisor rejected verification before commit. The candidate exits its gate;
    // neither local journal producer has run.
    atomic(&operation, &op("rolling_back"));
    eventually("candidate exits after failed verification", || {
        std::future::ready(hub.try_wait().unwrap().is_some())
    })
    .await;
    assert!(!hub.wait().unwrap().success());
    assert_v3(&journals);
    for (path, bytes) in journals.iter().zip(&original) {
        assert_eq!(
            std::fs::read(path).unwrap(),
            *bytes,
            "the pre-commit gate touched {}",
            path.display()
        );
    }
    // Positive control on the same fresh installation: once committed, the selected producer
    // really opens/migrates its journal. The assertion above is not a disabled-Node shortcut.
    atomic(&operation, &op("committed"));
    let mut committed = command.spawn().unwrap();
    eventually("committed Hub reaches its ordinary listener", || async {
        let Ok(r) = client.get(format!("{base}/healthz")).send().await else {
            return false;
        };
        r.json::<Value>()
            .await
            .is_ok_and(|v| v["ok"] == true && v["maintenance"] != true)
    })
    .await;
    for (index, path) in journals.iter().enumerate() {
        let db =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(
            version,
            if index == selected { 4 } else { 3 },
            "mode={mode}, {}",
            path.display()
        );
    }
    assert_eq!(
        unsafe { libc::kill(committed.id() as i32, libc::SIGTERM) },
        0
    );
    eventually("committed fixture shuts down", || {
        std::future::ready(committed.try_wait().unwrap().is_some())
    })
    .await;
    assert!(committed.wait().unwrap().success());
}

#[tokio::test(flavor = "multi_thread")]
async fn verifying_failure_keeps_the_session_journal_v3_until_commit() {
    verifying_failure_keeps_journals("local", 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verifying_failure_keeps_the_jobs_journal_v3_until_commit() {
    verifying_failure_keeps_journals("off", 1).await;
}

struct KnownNode([u8; 32]);

#[async_trait::async_trait]
impl steve_node_wire::handshake::Admission for KnownNode {
    async fn key(&self, node: &steve_node_api::NodeId) -> Result<Option<[u8; 32]>, String> {
        Ok((node.as_str() == "node-forward").then_some(self.0))
    }
    async fn admit(
        &self,
        _: &steve_node_wire::Hello,
    ) -> Result<Option<steve_node_wire::Admitted>, String> {
        Ok(Some(steve_node_wire::Admitted { generation: 1 }))
    }
}

struct NoWork;

#[async_trait::async_trait]
impl steve_node_wire::Handler for NoWork {
    async fn handle(&self, _: &str, _: Value) -> steve_node_api::ApiResult<Value> {
        Err(steve_node_api::ApiError::new(
            steve_node_api::ErrorCode::Unsupported,
            "no work in recovery fixture",
        ))
    }
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the built repair Node at STEVE_TEST_REPAIR_NODE; verifies its native signature"]
async fn signed_forward_repair_keeps_the_v4_journal_and_reconnects_over_authenticated_wire() {
    use std::sync::{Arc, Mutex};
    use steve_node_wire::{Conn, ConnOptions, Identity, handshake};
    use steve_runtime::testkit::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cli_home = steve_node_cli::config::Home(home.clone());
    let mut cfg = cli_home
        .node_config(steve_node_api::NodeId::new("node-forward").unwrap())
        .unwrap();
    cfg.acp.user_home = dir.path().into();
    cfg.acp.codex_home = dir.path().join("codex");
    cfg.acp.node = "/usr/bin/false".into();
    cfg.acp.claude_executable = "/usr/bin/false".into();
    let journal = cfg.home.join("node.sqlite3");
    let node = steve_node::LocalNode::open(cfg).unwrap();
    node.shutdown().await;
    drop(node);
    let db = rusqlite::Connection::open(&journal).unwrap();
    db.execute(
        "INSERT INTO meta(key,value) VALUES('forward_marker',42)",
        [],
    )
    .unwrap();
    let journal_id: i64 = db
        .query_row("SELECT value FROM meta WHERE key='journal_id'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        4
    );
    drop(db);
    let repair = home.join("releases/repaired");
    let binary = repair.join("bin/steve-node");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    let source =
        PathBuf::from(std::env::var_os("STEVE_TEST_REPAIR_NODE").expect("repair binary fixture"));
    std::fs::copy(source, &binary).unwrap();
    // Verify the candidate's existing native signature. No signing identity or keychain is created.
    let signature = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        signature.status.success(),
        "{}",
        String::from_utf8_lossy(&signature.stderr)
    );
    let build = Command::new(&binary).arg("--build-info").output().unwrap();
    assert!(build.status.success());
    let build: Value = serde_json::from_slice(&build.stdout).unwrap();
    assert!(build["version"].is_string());
    atomic(
        &home.join("harness.json"),
        &json!({"adapters":{"claude-code":"/usr/bin/false","codex":"/usr/bin/false"},"node":"/usr/bin/false","claude_executable":"/usr/bin/false"}),
    );
    // Isolated installation record only: no installed CLI setup/start/stop/update command.
    atomic(&home.join("services/node.json"), &json!({"release":repair}));
    let identity = Identity::generate().unwrap();
    std::fs::create_dir_all(home.join("node")).unwrap();
    identity.save(&home.join("node/identity.key")).unwrap();
    let hub = Arc::new(Identity::generate().unwrap());
    let admission = Arc::new(KnownNode(identity.public()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    atomic(
        &home.join("node/node.json"),
        &json!({"hub_url":url,"hub_id":"hub-forward",
        "node_id":"node-forward","pin":null,"loopback":true,"hub_key":hex::encode(hub.public()),"name":"test"}),
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let router = axum::Router::new().route(
        "/node/ws",
        axum::routing::get(move |ws: axum::extract::ws::WebSocketUpgrade| {
            let (hub, admission, tx) = (hub.clone(), admission.clone(), tx.clone());
            std::future::ready(ws.on_upgrade(move |socket| async move {
                let (mut sink, mut stream) = steve_hub::nodes::listener::split(socket);
                let (_, admitted) = handshake::accept_as(
                    &mut sink,
                    &mut stream,
                    "hub-forward",
                    admission.as_ref(),
                    Some(&hub),
                    &[],
                )
                .await
                .unwrap();
                let connection = Conn::spawn(
                    sink,
                    stream,
                    admitted.generation,
                    Arc::new(NoWork),
                    ConnOptions {
                        side: "h",
                        ..ConnOptions::default()
                    },
                );
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(connection);
                }
            }))
        }),
    );
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut child = Command::new(&binary)
        .arg("run")
        .env("HOME", dir.path())
        .env("STEVE_HOME", &home)
        .env("STEVE_NODE_LOOPBACK_PLAINTEXT", "1")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_BACKUP", "off")
        .env("STEVE_NO_RESUME", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let connection = tokio::time::timeout(std::time::Duration::from_secs(15), rx)
        .await
        .unwrap()
        .unwrap();
    let vitals = connection
        .call("node.vitals", json!({}), None)
        .await
        .unwrap();
    assert!(
        vitals.is_object(),
        "the reconnected Node answers ordinary authenticated RPC"
    );
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    eventually("repaired Node stops cleanly", || {
        std::future::ready(child.try_wait().unwrap().is_some())
    })
    .await;
    assert!(child.wait().unwrap().success());
    let db =
        rusqlite::Connection::open_with_flags(&journal, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        4
    );
    assert_eq!(
        db.query_row("SELECT value FROM meta WHERE key='journal_id'", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        journal_id
    );
    assert_eq!(
        db.query_row(
            "SELECT value FROM meta WHERE key='forward_marker'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        42
    );
}

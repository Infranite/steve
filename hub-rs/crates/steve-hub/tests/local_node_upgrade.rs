//! Upgrade scenario test (REVIEW-606 #1): a new Hub starts while an older colocated service Node
//! (`jobs: 0`, no colocated host ops, no research capability, `NodeServer::new` — matching the
//! 0.13.0 `steve-hub --local-node` binary) is still attached over the wire protocol.
//!
//! Proves that:
//! 1. Capability negotiation detects the capability gap and surfaces `local_node_updating` on
//!    `GET /api/summary`;
//! 2. Colocated host RPCs (`/api/fs/browse`) fail fast with 503 and the localized updating message;
//! 3. Chatting with Steve (`POST /api/thread`) does NOT stall or hang — Steve promptly replies
//!    with the clear "本机 Node 正在更新" notice;
//! 4. Once the upgraded full-capability local Node attaches, `local_node_updating` clears,
//!    `/api/fs/browse` returns 200, and Steve replies normally through the adapter.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use steve_hub::wire::Wired;
use steve_hub::wire::local_service;
use steve_node_api::{Discovery as _, NodeId};

#[path = "support/local_node.rs"]
mod local_node;

const ADAPTER_MODELS: [&str; 2] = ["fixture-smart", "fixture-fast"];

fn fake_adapter(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("adapter.mjs");
    let stub = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract/acp-stub.mjs");
    let body = format!(
        r#"#!/usr/bin/env node
import {{ serve }} from "{stub}"
const answer = {{kind: "answer", confidence: 0.95}}
const reply = {{thought: "", answer: "升级完成，现在可以正常回复。"}}
serve((head, rest) => {{
  const all = head + "\n" + rest
  if (all.includes("You are the router")) {{
    return JSON.stringify(answer)
  }}
  return JSON.stringify(reply)
}}, {models})
"#,
        models = json!(ADAPTER_MODELS)
    );
    std::fs::write(&p, body).expect("write adapter");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    p
}

struct LegacyNode {
    node: Arc<steve_node::LocalNode>,
    hub_conn: Arc<steve_node_wire::Conn>,
    node_conn: Arc<steve_node_wire::Conn>,
}

impl LegacyNode {
    async fn disconnect(self) {
        let weak = Arc::downgrade(&self.node);
        self.hub_conn.close(None, 1000, "upgrade replaced");
        self.node_conn.close(None, 1000, "upgrade replaced");
        let _ = tokio::time::timeout(Duration::from_secs(5), self.hub_conn.closed()).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.node_conn.closed()).await;
        self.node.shutdown().await;
        drop(self.node);
        drop(self.hub_conn);
        drop(self.node_conn);
        // Wait for the closed Conn task to drop its NodeServer's Arc<LocalNode> so HomeLock
        // releases `node.lock` before the upgraded LocalNode opens the same home directory.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while weak.upgrade().is_some() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            weak.upgrade().is_none(),
            "legacy LocalNode still referenced after disconnect"
        );
    }
}

/// Attaches a `jobs: 0` old-generation service Node fixture (no host ops, no research capability,
/// `NodeServer::new`, simulating an older `steve-hub --local-node` surviving a Hub upgrade).
async fn attach_legacy_service_node(
    w: &Wired,
    cfg: &Config,
    vars: &HashMap<String, String>,
    generation: u64,
) -> LegacyNode {
    let svc = w.nodes.as_ref().expect("nodes service");
    let node_id = NodeId::new(local_service::NODE_ID).expect("constant node id");
    let home = local_service::home(cfg);
    let user_home = vars
        .get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.home.clone());
    let mut adapters = HashMap::new();
    for (env_key, harness) in [
        ("STEVE_ADAPTER_CLAUDE", "claude-code"),
        ("STEVE_ADAPTER_CODEX", "codex"),
    ] {
        if let Some(p) = vars.get(env_key).filter(|s| !s.is_empty()) {
            adapters.insert(harness.to_string(), PathBuf::from(p));
        }
    }
    let node_cfg = steve_node::NodeConfig {
        ceiling: steve_node::PolicyCeiling::unlimited(),
        node: node_id.clone(),
        home,
        acp: steve_acp::AcpConfig {
            declared: cfg
                .declared_adapters
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            adapters,
            node: "node".into(),
            claude_executable: PathBuf::from("/usr/bin/false"),
            claude_config_dir: None,
            steve_home: cfg.home.clone(),
            codex_home: user_home.join(".codex"),
            user_home,
            version: "0.13.0".into(),
        },
        roots: vec![],
        capacity: steve_node_api::Capacity {
            sessions: steve_node_api::Capacity::SESSIONS_DEFAULT,
            // Legacy 0.13.0 local service Node did not host model/research jobs.
            jobs: 0,
            execs: 16,
        },
        storage: steve_node::Storage {
            worktree_floor: cfg.disk.floor,
            ..Default::default()
        },
    };
    let local = Arc::new(steve_node::LocalNode::open(node_cfg).expect("open legacy LocalNode"));
    // Deliberately do NOT call enable_host_ops() or enable_research(), and serve with
    // NodeServer::new (not NodeServer::colocated) to reproduce the pre-upgrade local Node.
    let (hub_end, node_end) = steve_node_wire::testkit::pair();
    let hub_server = Arc::new(steve_node_wire::ServeHub {
        sink: svc.fleet.sink_for(&node_id),
        tools: svc.fleet.tools_for(&node_id),
        files: Some(svc.fleet.files_for(&node_id)),
        secrets: svc.secrets.get().map(|s| s.for_node(node_id.clone())),
    });
    let node_server = steve_node_wire::NodeServer::new(local.clone());
    let hub_conn = steve_node_wire::Conn::spawn(
        hub_end.sink,
        hub_end.stream,
        generation,
        hub_server,
        steve_node_wire::ConnOptions {
            side: "h",
            ..Default::default()
        },
    );
    let node_conn = steve_node_wire::Conn::spawn(
        node_end.sink,
        node_end.stream,
        generation,
        node_server,
        steve_node_wire::ConnOptions {
            side: "n",
            ..Default::default()
        },
    );
    let hub_link = steve_node_wire::HubLink::new();
    hub_link.set(Some(node_conn.clone()));
    local.connect(
        Arc::new(steve_node_wire::RemoteSink(hub_link.clone())),
        Some(Arc::new(steve_node_wire::RemoteFiles(hub_link.clone()))),
    );
    local.connect_secrets(Arc::new(steve_node_wire::RemoteSecrets(hub_link.clone())));
    local
        .connect_tools(Arc::new(steve_node_wire::RemoteTools(hub_link)))
        .await
        .expect("connect legacy local tools");

    assert!(
        svc.registry
            .install(local_service::NODE_ID, generation, hub_conn.clone())
    );
    let remote = svc.fleet.remote(&node_id);
    assert!(remote.attach(hub_conn.clone(), false));
    svc.fleet.linked(&node_id, steve_node_api::PROTOCOL);

    // Populate the cached capability view (as listener::learn_capabilities does on attach).
    let caps = remote
        .probe(false)
        .await
        .expect("legacy capabilities probe");
    assert_eq!(caps.capacity.jobs, 0);
    assert!(!caps.supports_jobs());
    assert!(!caps.supports_colocated_host());
    assert!(!caps.supports_research());

    LegacyNode {
        node: local,
        hub_conn,
        node_conn,
    }
}

async fn api_call(
    http: &reqwest::Client,
    base: &str,
    cookie: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
    let mut req = http
        .request(m, format!("{base}{path}"))
        .header("cookie", cookie);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req.send().await.expect("http request");
    let status = r.status().as_u16();
    let text = r.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

async fn wait_thread_len(
    http: &reqwest::Client,
    base: &str,
    cookie: &str,
    min_len: usize,
) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (_, st) = api_call(
            http,
            base,
            cookie,
            "GET",
            "/api/views/page?kind=message",
            None,
        )
        .await;
        let msgs: Vec<Value> = st["views"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .cloned()
            .collect();
        if msgs.len() >= min_len {
            return msgs;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {min_len} messages; got: {msgs:#?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn old_zero_jobs_service_node_surfaces_updating_state_and_recovers_on_replacement() {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    assert_ne!(port, 8440);
    let service_port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("reserve service port")
        .local_addr()
        .expect("service addr")
        .port();
    let adapter = fake_adapter(&h).display().to_string();
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "0".into()),
        ("STEVE_LOCAL_NODE_PORT", service_port.to_string()),
        ("STEVE_BACKUP", "off".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("CODEX_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_MODELS_FAST", ADAPTER_MODELS[1].into()),
        ("STEVE_MODELS_SMART", ADAPTER_MODELS[0].into()),
        ("STEVE_ADAPTER_CLAUDE", adapter.clone()),
        ("STEVE_ADAPTER_CODEX", adapter),
        ("STEVE_ADAPTER_PI", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get)).expect("wire");
    let svc = w.nodes.clone().expect("nodes service");
    let node_rt = w.node.clone().expect("node runtime");

    // 1. Attach the old jobs: 0 local service Node fixture.
    let legacy = attach_legacy_service_node(&w, &cfg, &vars, 1).await;
    let node_home = local_service::home(&cfg);
    let updating = local_service::sync_updating_status(&node_home, &svc, &node_rt, &cfg.version);
    assert!(
        updating.is_some(),
        "old jobs:0 local Node must report updating status"
    );

    let st = AppState::new(cfg.clone(), w.backend.clone(), w.domain.clone(), bus)
        .with_nodes(w.nodes.clone())
        .with_node(w.node.clone())
        .with_modules(w.modules.clone())
        .with_workspaces(Some(w.workspaces.clone()));
    st.runtimes.set_harnesses(w.harnesses);
    steve_hub::app::boot(&st).await.expect("boot");
    local_node::start_jobs(w.jobs, &st.jobs);
    let app = steve_hub::app::app(st.clone(), w.auth, w.mcp, w.module_routes, w.root_routes);
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let r = http
        .get(format!("{base}/auth/dev"))
        .send()
        .await
        .expect("dev login");
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("session cookie");

    // 2. GET /api/summary exposes the live `local_node_updating` banner payload.
    let (status, summary) = api_call(&http, &base, &cookie, "GET", "/api/summary", None).await;
    assert_eq!(status, 200, "{summary}");
    assert!(
        summary["local_node_updating"].is_object(),
        "expected local_node_updating object in /api/summary, got: {summary}"
    );

    // 3. Colocated host FS browsing fails fast with 503 + localized updating message.
    let (status, browse) = api_call(&http, &base, &cookie, "GET", "/api/fs/browse", None).await;
    assert_eq!(
        status, 503,
        "expected 503 during local node update: {browse}"
    );
    assert!(
        browse.to_string().contains("本机 Node 正在更新"),
        "expected localized updating error on /api/fs/browse, got: {browse}"
    );

    // 4. Sending a message to Steve fails fast with a clear visible reply instead of stalling.
    let initial_len = wait_thread_len(&http, &base, &cookie, 0).await.len();
    let (status, posted) = api_call(
        &http,
        &base,
        &cookie,
        "POST",
        "/api/thread",
        Some(json!({ "text": "帮我看看最近的状态" })),
    )
    .await;
    assert_eq!(status, 200, "{posted}");
    let msgs = wait_thread_len(&http, &base, &cookie, initial_len + 2).await;
    let steve_reply = msgs
        .iter()
        .rev()
        .find(|m| m["author"] == "steve")
        .expect("steve must commit a prompt reply instead of stalling");
    assert!(
        steve_reply["text"]
            .as_str()
            .unwrap_or_default()
            .contains("本机 Node 正在更新"),
        "expected Steve's reply to explain the local Node update, got: {steve_reply}"
    );

    // 5. Disconnect the legacy Node and attach an upgraded full-capability local Node.
    legacy.disconnect().await;
    // Use a fresh generation (> 1) when replacing the local link.
    let node_id = NodeId::new(local_service::NODE_ID).expect("constant node id");
    let upgraded = {
        let user_home = PathBuf::from(vars.get("HOME").unwrap());
        let mut adapters = HashMap::new();
        for (env_key, harness) in [
            ("STEVE_ADAPTER_CLAUDE", "claude-code"),
            ("STEVE_ADAPTER_CODEX", "codex"),
        ] {
            if let Some(p) = vars.get(env_key).filter(|s| !s.is_empty()) {
                adapters.insert(harness.to_string(), PathBuf::from(p));
            }
        }
        let node_cfg = steve_node::NodeConfig {
            ceiling: steve_node::PolicyCeiling::unlimited(),
            node: node_id.clone(),
            home: node_home.clone(),
            acp: steve_acp::AcpConfig {
                declared: cfg
                    .declared_adapters
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                adapters,
                node: "node".into(),
                claude_executable: PathBuf::from("/usr/bin/false"),
                claude_config_dir: None,
                steve_home: cfg.home.clone(),
                codex_home: user_home.join(".codex"),
                user_home,
                version: cfg.version.clone(),
            },
            roots: vec![],
            capacity: steve_node_api::Capacity {
                sessions: steve_node_api::Capacity::SESSIONS_DEFAULT,
                jobs: u32::MAX,
                execs: 16,
            },
            storage: steve_node::Storage {
                worktree_floor: cfg.disk.floor,
                ..Default::default()
            },
        };
        let local = Arc::new(steve_node::LocalNode::open(node_cfg).expect("open new LocalNode"));
        local.enable_host_ops();
        local.enable_capability_sandbox();
        local.enable_research(steve_research::ResearchConfig::new(
            cfg.scratch.join("research"),
        ));
        let (hub_end, node_end) = steve_node_wire::testkit::pair();
        let hub_server = Arc::new(steve_node_wire::ServeHub {
            sink: svc.fleet.sink_for(&node_id),
            tools: svc.fleet.tools_for(&node_id),
            files: Some(svc.fleet.files_for(&node_id)),
            secrets: svc.secrets.get().map(|s| s.for_node(node_id.clone())),
        });
        let node_server = steve_node_wire::NodeServer::colocated(local.clone());
        let hub_conn = steve_node_wire::Conn::spawn(
            hub_end.sink,
            hub_end.stream,
            2,
            hub_server,
            steve_node_wire::ConnOptions {
                side: "h",
                ..Default::default()
            },
        );
        let node_conn = steve_node_wire::Conn::spawn(
            node_end.sink,
            node_end.stream,
            2,
            node_server,
            steve_node_wire::ConnOptions {
                side: "n",
                ..Default::default()
            },
        );
        let hub_link = steve_node_wire::HubLink::new();
        hub_link.set(Some(node_conn.clone()));
        local.connect(
            Arc::new(steve_node_wire::RemoteSink(hub_link.clone())),
            Some(Arc::new(steve_node_wire::RemoteFiles(hub_link.clone()))),
        );
        local.connect_secrets(Arc::new(steve_node_wire::RemoteSecrets(hub_link.clone())));
        local
            .connect_tools(Arc::new(steve_node_wire::RemoteTools(hub_link)))
            .await
            .expect("connect upgraded local tools");
        assert!(
            svc.registry
                .install(local_service::NODE_ID, 2, hub_conn.clone())
        );
        let remote = svc.fleet.remote(&node_id);
        assert!(remote.attach(hub_conn.clone(), false));
        svc.fleet.linked(&node_id, steve_node_api::PROTOCOL);
        let caps = remote
            .probe(false)
            .await
            .expect("upgraded capabilities probe");
        assert!(caps.supports_jobs());
        assert!(caps.supports_colocated_host());
        assert!(caps.supports_research());
        assert!(caps.supports_resume_probe());
        (local, hub_conn, node_conn)
    };
    let updating_after =
        local_service::sync_updating_status(&node_home, &svc, &node_rt, &cfg.version);
    assert!(
        updating_after.is_none(),
        "upgraded local Node must clear updating status"
    );
    w.domain
        .harnesses(true)
        .await
        .expect("refresh harnesses after upgrade");

    // 6. Verify /api/summary clears `local_node_updating`, /api/fs/browse returns 200, and Steve replies!
    let (status, summary) = api_call(&http, &base, &cookie, "GET", "/api/summary", None).await;
    assert_eq!(status, 200, "{summary}");
    assert!(
        summary["local_node_updating"].is_null(),
        "expected local_node_updating to clear after upgrade, got: {summary}"
    );
    let (status, browse) = api_call(&http, &base, &cookie, "GET", "/api/fs/browse", None).await;
    assert_eq!(
        status, 200,
        "expected /api/fs/browse to succeed after upgrade: {browse}"
    );
    let (status, posted2) = api_call(
        &http,
        &base,
        &cookie,
        "POST",
        "/api/thread",
        Some(json!({ "text": "谢谢" })),
    )
    .await;
    assert_eq!(status, 200, "{posted2}");
    let msgs = wait_thread_len(&http, &base, &cookie, initial_len + 4).await;
    let last_reply = msgs
        .iter()
        .rev()
        .find(|m| m["author"] == "steve")
        .expect("second steve reply");
    assert!(
        last_reply["text"]
            .as_str()
            .unwrap_or_default()
            .contains("升级完成，现在可以正常回复"),
        "expected normal Steve reply after upgrade, got: {last_reply}"
    );

    server.abort();
    upgraded.1.close(None, 1000, "test done");
    upgraded.2.close(None, 1000, "test done");
    upgraded.0.shutdown().await;
}

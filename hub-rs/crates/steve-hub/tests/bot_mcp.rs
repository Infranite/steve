//! Library MCP configuration and secrets stay in the Hub. Even a remote session receives only
//! Steve's tool endpoint, not one server per plugin or any plugin credentials.

mod node_harness;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use steve_runtime::testkit::{eventually_sync, on_timeout};
use steve_store::testkit::hub::node_checks;

use node_harness::*;
use serde_json::{Value, json};
use steve_acp::AcpConfig;

use steve_hub::wire::node_secrets::NodeSecrets;
use steve_modules::caps::Capabilities;
use steve_modules::caps::mcp::ServerIn;
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_node::{LocalNode, NodeConfig, Root};
use steve_node_api::*;
use steve_node_cli::config::Pairing;
use steve_node_wire::tls::Trust;

const SECRET: &str = "ghp-bot-mcp-SECRET";

fn local_config(root: &Path, node: &NodeId) -> (NodeConfig, PathBuf) {
    let log = root.join("acp.log");
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
    let adapter = root.join("adapter.sh");
    std::fs::write(
        &adapter,
        format!(
            "#!/bin/sh\nexport FAKE_ACP_LOG='{}'\nexec node '{}' \"$@\"\n",
            log.display(),
            fake.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(root.join("work")).unwrap();
    (
        NodeConfig {
            ceiling: steve_node::PolicyCeiling::unlimited(),
            node: node.clone(),
            home: root.join("node"),
            acp: AcpConfig {
                declared: Default::default(),
                adapters: HashMap::from([("claude-code".to_string(), adapter)]),
                node: "node".into(),
                claude_executable: "claude".into(),
                claude_config_dir: None,
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.to_path_buf(),
                version: "test".into(),
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: root.join("work"),
                display: "~/work".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 4,
                jobs: 0,
                execs: 0,
            },
            storage: Default::default(),
        },
        log,
    )
}

fn server(v: Value) -> ServerIn {
    serde_json::from_value(v).unwrap()
}

/// Every file under `dir` (recursively) holds no `needle`.
fn nowhere(dir: &Path, needle: &str) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            nowhere(&p, needle);
        } else if let Ok(bytes) = std::fs::read(&p) {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{} holds the secret",
                p.display()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bots_mcp_servers_and_secrets_never_reach_a_remote_node() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    // the vault and the Bot with its servers, as the page would add them
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        h.store.clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    vault.init("default", "u1").unwrap();
    let svc = Capabilities::new(vault);
    steve_store::testkit::hub::hub_fixtures::seed_bots(&h.store).unwrap();
    let files = svc
        .mcp_create(
            "default",
            "u1",
            server(
                json!({"name": "files", "transport": "stdio", "command": "npx",
                "args": ["-y", "files-mcp"],
                "env": [{"name": "ROOT", "value": "/tmp"},
                        {"name": "TOKEN", "value": SECRET, "secret": true}],
                "bot": "dev"}),
            ),
        )
        .unwrap();
    svc.mcp_create(
        "default",
        "u1",
        server(
            json!({"name": "remote", "transport": "http", "url": "https://mcp.example.com/mcp",
            "headers": [{"name": "X-Team", "value": "core"}], "bot": "dev"}),
        ),
    )
    .unwrap();
    let off = svc
        .mcp_create(
            "default",
            "u1",
            server(json!({"name": "off", "transport": "sse", "url": "https://x.example.com/sse"})),
        )
        .unwrap();
    svc.assign(
        "default",
        "u1",
        "dev",
        "mcp",
        off["id"].as_str().unwrap(),
        false,
    )
    .unwrap();
    // in the library but not dev's: not given either
    svc.mcp_create(
        "default",
        "u1",
        server(json!({"name": "unused", "transport": "http", "url": "https://y.example.com/mcp"})),
    )
    .unwrap();
    let secrets = NodeSecrets::new();
    assert!(h.svc.secrets.set(secrets).is_ok());

    h.session("s1");
    steve_store::testkit::hub::hub_fixtures::set_sessions_bot_id_dev(&h.store).unwrap();
    let _rt = h.bind(&node, "s1");
    // The Hub sends no plugin configuration, not even credential references.
    let open: String = node_checks::command_request_by_method(&h.store, "open_session").unwrap();
    let open: Value = serde_json::from_str(&open).unwrap();
    assert!(open["body"].get("mcp_servers").is_none(), "{open}");

    // A remote Node over the link opens the harness with Steve's endpoint only.
    let dir = tempfile::tempdir().unwrap();
    let (cfg, log) = local_config(dir.path(), &node);
    let local = LocalNode::open(cfg).unwrap();
    let p = Pairing {
        hub_url: h.url(),
        node_id: node.to_string(),
        hub_id: h.svc.hub_id.clone(),
        pin: None,
        loopback: true,
        hub_key: None,
        name: "local".into(),
    };
    let n = local.clone();
    let task = tokio::spawn(async move {
        let _ = steve_node_cli::dial::serve(n, &p, &id, &Trust::LoopbackPlaintext).await;
    });
    eventually_sync("session/new", || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("\"method\":\"session/new\"")
    })
    .await;
    let text = std::fs::read_to_string(&log).unwrap();
    let new = text
        .lines()
        .find(|l| l.contains("\"method\":\"session/new\""))
        .unwrap();
    let v: Value = serde_json::from_str(new).unwrap();
    let got = v["params"]["mcpServers"].as_array().unwrap();
    let by = |n: &str| got.iter().find(|s| s["name"] == n).cloned();
    assert!(
        by("steve").is_some(),
        "the Hub's sole tool endpoint is present: {got:?}"
    );
    for name in ["files", "remote", "off", "unused"] {
        assert_eq!(by(name), None, "{name} must be proxied by the Hub");
    }
    let uses: i64 =
        node_checks::secret_uses_count_by_actor(&h.store, &files["id"].as_str().unwrap()).unwrap();
    assert_eq!(uses, 0, "opening a harness takes no plugin secrets");

    eventually_sync(
        on_timeout(|| async {
            format!(
                "The proxied session s1 finishes opening before secret-leak inspection; {}",
                h.dump()
            )
        }),
        || node_checks::binding_count_s1_ready(&h.store).unwrap() == 1,
    )
    .await;
    task.abort();
    let _ = task.await;
    drop(local);
    steve_store::testkit::hub::backup::checkpoint(&h.store).unwrap();
    nowhere(h.dir.path(), SECRET);
    nowhere(&dir.path().join("node"), SECRET);
}

#[tokio::test]
async fn the_hub_shares_a_stdio_server_and_reopens_it_after_an_edit() {
    use steve_hub::wire::library_mcp::LibraryMcp;
    let h = Hub::start().await;
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        h.store.clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    vault.init("default", "u1").unwrap();
    let caps = Capabilities::new(vault);
    // All protocol traffic is local stdio. A secret is needed only when the Hub opens this one
    // shared process, never by any Node. Python is a test fixture, not a product dependency.
    let script = r#"import json,os,sys
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    method=m['method']
    if method=='initialize': r={'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
    elif method=='tools/list': r={'tools':[{'name':'who','description':'Process and token.','inputSchema':{'type':'object'}}]}
    else: r={'content':[{'type':'text','text':str(os.getpid())+':'+os.environ['TOKEN']}]}
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':r}),flush=True)
"#;
    let made = caps.mcp_create("default", "u1", server(json!({
        "name":"stdio-fixture", "transport":"stdio", "command":"python3", "args":["-u", "-c", script],
        "env":[{"name":"TOKEN", "value":"vault-fixture-token", "secret":true}]
    }))).unwrap();
    let id = made["id"].as_str().unwrap();
    let get = || {
        h.store
            .read(|c| steve_store::caps::mcp::Servers(c).get(id))
            .unwrap()
            .unwrap()
    };
    let library = LibraryMcp::new(
        caps.clone(),
        None,
        Arc::new(steve_runtime::cache::Caches::new(Default::default())),
    );
    let current = get();
    let (a, b) = tokio::join!(
        library.tools_within(&current, "one", Duration::from_secs(5)),
        library.tools_within(&current, "two", Duration::from_secs(5))
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let first = library
        .call(&current, "one", "who", json!({}))
        .await
        .unwrap();
    let second = library
        .call(&current, "two", "who", json!({}))
        .await
        .unwrap();
    assert_eq!(first, second, "one process serves both Bots");
    assert!(first.ends_with(":vault-fixture-token"));
    assert_eq!(
        node_checks::secret_uses_count_by_actor(&h.store, &id).unwrap(),
        1
    );
    caps.mcp_update(
        "default",
        "u1",
        id,
        server(json!({"args":["-u", "-c", script]})),
    )
    .unwrap();
    let changed = get();
    assert!(changed.updated > current.updated);
    library.sweep();
    assert!(
        library.cached(&changed).await.is_none(),
        "the previous revision's list is not reused"
    );
    library
        .tools_within(&changed, "one", Duration::from_secs(5))
        .await
        .unwrap();
    let third = library
        .call(&changed, "one", "who", json!({}))
        .await
        .unwrap();
    assert_ne!(
        first.split(':').next(),
        third.split(':').next(),
        "an edit reopens the server"
    );
    caps.mcp_delete("default", "u1", id).unwrap();
    library.sweep();
    assert!(
        library.cached(&changed).await.is_none(),
        "deletion drops the cache too"
    );
}

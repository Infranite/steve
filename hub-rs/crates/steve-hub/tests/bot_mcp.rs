//! The library's MCP servers a Bot uses reach its session's harness: the Hub puts the servers the
//! Bot has switched on into `open_session` with secrets as references, a remote Node (over the real link) takes each value
//! under `mcp:<session>` as the harness opens, and the scripted ACP adapter gets them in
//! `session/new` next to Steve's own server. Neither the Hub's command ledger nor the Node's
//! journal ever holds a value; a Node the session is not bound to gets nothing.

mod node_harness;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use node_harness::*;
use serde_json::{Value, json};
use steve_acp::AcpConfig;
use steve_hub::wire::bot_mcp::McpSecrets;
use steve_hub::wire::node_secrets::{NodeSecrets, SessionSecrets};
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
async fn a_bots_mcp_servers_reach_session_new_on_a_remote_node() {
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
    h.store
        .transaction::<_, steve_store::StoreError>(|tx| {
            tx.execute_batch(
                "INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1);",
            )?;
            Ok(())
        })
        .unwrap();
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
    let sessions = McpSecrets::new(h.store.clone(), svc.clone());
    secrets.set_sessions(sessions.clone());
    assert!(h.svc.secrets.set(secrets).is_ok());

    h.session("s1");
    h.store
        .transaction::<_, steve_store::StoreError>(|tx| {
            tx.execute("UPDATE sessions SET bot_id = 'dev' WHERE id = 's1'", [])?;
            Ok(())
        })
        .unwrap();
    let _rt = h.bind(&node, "s1");
    // what the Hub sends: references only
    let open: String = h.q("SELECT request FROM node_commands WHERE method = 'open_session'");
    let open: Value = serde_json::from_str(&open).unwrap();
    let servers = &open["body"]["mcp_servers"];
    assert_eq!(servers.as_array().map(Vec::len), Some(2), "{open}");
    let token_ref = format!("{}#env:TOKEN", files["id"].as_str().unwrap());
    assert_eq!(
        servers[0],
        json!({"name": "files", "transport": "stdio", "command": "npx", "args": ["-y", "files-mcp"],
            "env": [{"name": "ROOT", "value": "/tmp"}, {"name": "TOKEN", "secret": token_ref}]})
    );

    // a remote Node over the link opens the harness with them
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
    until("session/new", 25, || {
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
    assert_eq!(
        by("files"),
        Some(
            json!({"name": "files", "command": "npx", "args": ["-y", "files-mcp"],
            "env": [{"name": "ROOT", "value": "/tmp"}, {"name": "TOKEN", "value": SECRET}]})
        ),
        "{got:?}"
    );
    assert_eq!(
        by("remote"),
        Some(
            json!({"type": "http", "name": "remote", "url": "https://mcp.example.com/mcp",
            "headers": [{"name": "X-Team", "value": "core"}]})
        )
    );
    assert_eq!(by("off"), None, "a switched-off server is not given");
    // the use is in the vault's log, once
    let uses: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM secret_uses WHERE actor_id = '{}'",
        files["id"].as_str().unwrap()
    ));
    assert_eq!(uses, 1);
    // another Node, or a session that is not the Bot's, gets nothing
    assert!(sessions.value("other-node", "s1", &token_ref).is_none());
    assert!(sessions.value(node.as_str(), "s9", &token_ref).is_none());
    assert!(sessions.value(node.as_str(), "s1", &token_ref).is_some());

    tokio::time::sleep(Duration::from_millis(200)).await;
    task.abort();
    drop(local);
    h.store
        .with_conn(|c| {
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .unwrap();
    nowhere(h.dir.path(), SECRET);
    nowhere(&dir.path().join("node"), SECRET);
}

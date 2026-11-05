//! A remote Node over a real loopback WebSocket: the coordinator talks to it through RemoteNode
//! exactly as to a local Node. The fake Node answers the contract identically through the wire;
//! a LocalNode with the scripted ACP adapter runs a whole turn (a permission, one of Steve's tools
//! through MCP and the link, the end) and survives a dropped link with its events replayed once.

mod node_harness;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use node_harness::*;
use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root};
use steve_node_api::testkit::{FakeNode, build::*};
use steve_node_api::*;
use steve_node_cli::config::Pairing;
use steve_node_wire::Identity;
use steve_node_wire::tls::Trust;

fn show<T: serde::Serialize>(r: &ApiResult<T>) -> String {
    match r {
        Ok(v) => format!("ok {}", serde_json::to_string(v).unwrap()),
        Err(e) => format!("err {}", serde_json::to_string(e).unwrap()),
    }
}

async fn scenario(n: &dyn Node, node: &str) -> Vec<String> {
    let mut out = vec![];
    out.push(show(
        &n.prepare_workspace(cmd("op_prep", prepare("item:s_1")))
            .await,
    ));
    out.push(show(
        &n.open_session(cmd("op_open", open("s_1", node))).await,
    ));
    let t = cmd("op_t1", turn_on(node, "s_1", "t_1", "a_1", "hi"));
    out.push(show(&n.start_turn(t.clone()).await));
    out.push(show(&n.start_turn(t).await));
    out.push(show(
        &n.start_turn(cmd("op_t2", turn_on(node, "s_1", "t_1", "a_2", "hi")))
            .await,
    ));
    out.push(show(
        &n.start_turn(cmd("op_t3", turn_on(node, "s_1", "t_1", "a_1", "hi")))
            .await,
    ));
    out.push(show(&n.session(SessionId::new("nope").unwrap()).await));
    out.push(show(&n.operation(OperationId::new("op_t1").unwrap()).await));
    out.push(show(&n.streams().await));
    out.push(show(
        &n.cancel(cmd(
            "op_c",
            Cancel {
                target: OperationId::new("op_t1").unwrap(),
                reason: "x".into(),
            },
        ))
        .await,
    ));
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fake_node_answers_the_same_over_a_real_socket() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let direct = FakeNode::new(node.as_str());
    let a = scenario(direct.as_ref(), node.as_str()).await;
    let fake = FakeNode::new(node.as_str());
    let _l = dial(&h, &id, &node, fake.clone(), 1).await.unwrap();
    let remote = h.fleet.remote(&node);
    until("attached", 5, || remote.generation() == Some(1)).await;
    let b = scenario(remote.as_ref(), node.as_str()).await;
    assert_eq!(a, b);
    assert_eq!(direct.log(), fake.log());
}

// ── a LocalNode with the scripted adapter ──

fn local_config(root: &Path, node: &NodeId) -> (NodeConfig, PathBuf) {
    let log = root.join("acp.log");
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
    let adapter = root.join("adapter.sh");
    if !adapter.exists() {
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
    }
    std::fs::create_dir_all(root.join("work")).unwrap();
    (
        NodeConfig {
            node: node.clone(),
            home: root.join("node"),
            acp: AcpConfig {
                adapters: HashMap::from([("claude-code".to_string(), adapter)]),
                node: "node".into(),
                claude_executable: "claude".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.to_path_buf(),
                version: "test".into(),
                idle_release: Duration::from_secs(600),
                retry_delays: vec![],
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

struct Running {
    node: LocalNode,
    log: PathBuf,
    _dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
}

/// The steve-node dial loop around a LocalNode on its own home.
async fn local_node(h: &Hub, id: Identity, node: &NodeId) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let (cfg, log) = local_config(dir.path(), node);
    let local = LocalNode::open(cfg).unwrap();
    let p = Pairing {
        hub_url: h.url(),
        node_id: node.to_string(),
        hub_id: h.svc.hub_id.clone(),
        pin: None,
        loopback: true,
        name: "local".into(),
    };
    let n = local.clone();
    let task = tokio::spawn(async move {
        let r = steve_node_cli::dial::serve(n, &p, &id, &Trust::LoopbackPlaintext).await;
        eprintln!("node stopped: {r:?}");
    });
    Running {
        node: local,
        log,
        _dir: dir,
        task,
    }
}

fn phase(h: &Hub, s: &str) -> String {
    h.q(&format!(
        "SELECT phase FROM runtime_sessions WHERE session_id='{s}'"
    ))
}

fn texts(h: &Hub, node: &NodeId) -> Vec<String> {
    h.store
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT data FROM events WHERE source_node=? AND kind='text' ORDER BY seq",
            )?;
            let v = st
                .query_map([node.as_str()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .unwrap()
}

async fn mcp(url: &str, token: &str, body: serde_json::Value) -> serde_json::Value {
    let r = reqwest::Client::new()
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Protocol-Version", "2025-06-18")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let body = r.text().await.unwrap();
    body.find('{')
        .and_then(|i| body.rfind('}').map(|j| &body[i..=j]))
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or(serde_json::Value::Null)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_local_node_runs_a_whole_turn_through_the_link() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    h.session("s1");
    let rt = h.bind(&node, "s1");
    let n = local_node(&h, id, &node).await;
    for _ in 0..500 {
        if phase(&h, "s1") == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(phase(&h, "s1"), "ready", "{}", h.dump());
    rt.prompt(
        "s1",
        "@say hello @perm Bash @sleep 800 @say done",
        "user",
        &[],
    )
    .unwrap();
    until("permission card", 20, || {
        h.q::<i64>(
            "SELECT COUNT(*) FROM decisions WHERE subject_type='tool_call' AND status='open'",
        ) == 1
    })
    .await;
    let d: String = h.q("SELECT id FROM decisions WHERE subject_type='tool_call'");
    assert!(rt.answer_permission(&d, Some("allow_once")).unwrap());
    // one of Steve's tools, called by the harness on the Node's MCP endpoint
    let log = std::fs::read_to_string(&n.log).unwrap();
    let new = log
        .lines()
        .find(|l| l.contains("\"method\":\"session/new\""))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(new).unwrap();
    let server = &v["params"]["mcpServers"][0];
    let url = server["url"].as_str().unwrap().to_string();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "MCP stays on the Node"
    );
    let token = server["headers"][0]["value"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .to_string();
    let called = mcp(
        &url,
        &token,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "steve_update", "arguments": {"now": "写测试"}}}),
    )
    .await;
    assert_eq!(
        called["result"]["content"][0]["text"], "Steve updated the item.",
        "{called}"
    );
    until("turn end", 20, || !h.ends.turns.lock().unwrap().is_empty()).await;
    let calls = h.service.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "s1");
    let t = texts(&h, &node).join("");
    assert!(t.contains("hello") && t.contains("done"), "{t}");
    assert!(t.contains("perm:selected:once"), "{t}");
    // Steve's own words only: no ACP payload became an event
    let acp: i64 = h.q("SELECT COUNT(*) FROM events WHERE data LIKE '%sessionUpdate%'");
    assert_eq!(acp, 0);
    n.task.abort();
    n.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_link_reconnects_and_replays_once() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    h.session("s1");
    let rt = h.bind(&node, "s1");
    let n = local_node(&h, id, &node).await;
    for _ in 0..500 {
        if phase(&h, "s1") == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(phase(&h, "s1"), "ready", "{}", h.dump());
    rt.prompt("s1", "@say one @sleep 1200 @say two", "user", &[])
        .unwrap();
    until("first words", 20, || {
        texts(&h, &node).join("").contains("one")
    })
    .await;
    let g1 = h.svc.registry.current(node.as_str()).unwrap();
    // the link breaks mid-turn; the Node journals on and dials again
    h.svc.registry.conn(node.as_str()).unwrap().close(
        None,
        steve_node_wire::close::GOODBYE,
        "test",
    );
    until("turn end", 30, || !h.ends.turns.lock().unwrap().is_empty()).await;
    let g2 = h.svc.registry.current(node.as_str()).unwrap();
    assert!(g2 > g1, "a new generation: {g1} → {g2}");
    let t = texts(&h, &node).join("");
    assert_eq!(t.matches("one").count(), 1, "{t}");
    assert_eq!(t.matches("two").count(), 1, "{t}");
    assert_eq!(h.ends.turns.lock().unwrap().len(), 1);
    let starts: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM node_commands WHERE node_id='{node}' AND method='start_turn'"
    ));
    assert_eq!(starts, 1);
    // contiguous, each position once, per stream
    let holes: i64 = h.q(&format!(
        "SELECT COUNT(*) FROM (SELECT COUNT(*) n, MAX(seq) m FROM node_event_inbox WHERE node_id='{node}' GROUP BY stream, epoch) WHERE n != m"
    ));
    assert_eq!(holes, 0);
    n.task.abort();
    n.node.shutdown().await;
}

/// A paired device's LocalNode (the `steve-node` binary) has no discovery switch on: over the
/// link it lists, reads and locates no history, even with transcripts under its user's home, and
/// its probe reports no harness version.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remote_local_node_refuses_history() {
    let h = Hub::start().await;
    let (id, node) = h.paired("u1").await;
    let n = local_node(&h, id, &node).await;
    let home = n.log.parent().unwrap().to_path_buf();
    let sid = "0b7f2c1e-1111-4222-8333-444455556666";
    let t = home.join(format!(".claude/projects/-w/{sid}.jsonl"));
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(
        &t,
        format!(
            r#"{{"type":"user","cwd":"/w","sessionId":"{sid}","message":{{"role":"user","content":"secret"}}}}"#
        ) + "\n",
    )
    .unwrap();
    let remote = h.fleet.remote(&node);
    until("attached", 10, || remote.generation().is_some()).await;
    let unsupported = |r: String| {
        assert!(r.contains("\"unsupported\""), "{r}");
        assert!(!r.contains("secret"), "{r}");
    };
    unsupported(show(
        &remote
            .history(PageRequest {
                cursor: None,
                limit: 50,
            })
            .await,
    ));
    let source = HistoryRef {
        harness: "claude-code".into(),
        key: format!("cc:{sid}"),
    };
    unsupported(show(&remote.locate(source.clone()).await));
    unsupported(show(
        &remote
            .import_history(cmd(
                "op_imp",
                HistoryImport {
                    source,
                    expected: Revision::new("m1-s1").unwrap(),
                },
            ))
            .await,
    ));
    unsupported(show(
        &remote
            .import_page(
                OperationId::new("op_imp").unwrap(),
                PageRequest {
                    cursor: None,
                    limit: 50,
                },
            )
            .await,
    ));
    let caps = remote.probe(true).await.unwrap();
    assert!(
        caps.harnesses
            .iter()
            .all(|h| h.version.is_empty() && h.models.is_empty()),
        "{caps:?}"
    );
    n.task.abort();
    n.node.shutdown().await;
}

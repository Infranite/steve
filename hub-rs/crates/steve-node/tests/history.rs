//! E10: the LocalNode's Discovery face on fixture transcripts. History lists what the harnesses
//! keep (never Steve's own runs); an import reads only a key it listed, at the version listed,
//! and pages its events; a forged key (any other path, a `..` way out, an id that is a path) is
//! refused; the probe asks the configured binaries.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use steve_acp::AcpConfig;
use steve_node::history::{Binaries, DiscoveryConfig, Homes};
use steve_node::{LocalNode, NodeConfig, Storage};
use steve_node_api::*;

const CC_ID: &str = "3b2c458f-84be-4589-a22c-5bfd2579c313";
const CX_ID: &str = "01a1a57d-52a0-7111-9c1d-0f8bf59e7c96";

struct T {
    _dir: tempfile::TempDir,
    root: PathBuf,
    node: LocalNode,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn script(p: &Path, body: &str) {
    write(p, &format!("#!/bin/sh\n{body}\n"));
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A Claude Code transcript of a session that ran in `cwd`.
fn claude(cwd: &str, ask: &str) -> String {
    [
        format!(r#"{{"type":"user","sessionId":"{CC_ID}","cwd":"{cwd}","message":{{"content":"<command-name>x"}}}}"#),
        format!(r#"{{"type":"user","message":{{"content":"{ask}"}}}}"#),
        format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"ok"}},{{"type":"tool_use","id":"t1","name":"Read","input":{{"file_path":"{cwd}/a.rs"}}}}]}}}}"#),
    ]
    .join("\n")
}

fn setup(codex: &str) -> T {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cfg = NodeConfig {
        ceiling: steve_node::PolicyCeiling::unlimited(),
        node: NodeId::new("local").unwrap(),
        home: root.join("node"),
        acp: AcpConfig {
            adapters: HashMap::from([
                ("claude-code".to_string(), PathBuf::from("/usr/bin/false")),
                ("codex".to_string(), PathBuf::from("/usr/bin/false")),
            ]),
            node: "node".into(),
            claude_executable: "claude".into(),
            steve_home: root.join("steve"),
            codex_home: root.join(".codex"),
            user_home: root.clone(),
            version: "test".into(),
            idle_release: Duration::from_secs(600),
            retry_delays: vec![],
        },
        roots: vec![],
        capacity: Capacity {
            sessions: 1,
            jobs: 0,
            execs: 0,
        },
        storage: Storage::default(),
        jobs: Default::default(),
    };
    let node = LocalNode::open(cfg).unwrap();
    script(&root.join("bin/claude"), "echo '2.1.0 (Claude Code)'");
    script(
        &root.join("bin/codex"),
        r#"if [ "$1" = "--version" ]; then echo 'codex-cli 0.80.0'; else echo '{"models":[{"slug":"gpt-5.5","visibility":"list","priority":2},{"slug":"gpt-5.5-codex","visibility":"list","priority":1},{"slug":"hidden","visibility":"hide"}]}'; fi"#,
    );
    node.set_discovery(DiscoveryConfig {
        homes: Homes {
            user_home: root.clone(),
            steve_home: root.join("steve"),
            claude_dir: root.join(".claude"),
            codex_home: root.join(".codex"),
        },
        binaries: Binaries {
            claude: root.join("bin/claude"),
            codex: root.join(codex),
        },
    });
    let cwd = root.join("w/app").display().to_string();
    write(
        &root.join(format!(".claude/projects/-w-app/{CC_ID}.jsonl")),
        &claude(&cwd, "fix the bug"),
    );
    write(
        &root.join(format!(".codex/sessions/2026/11/04/rollout-2026-11-04T15-36-56-{CX_ID}.jsonl")),
        &[
            format!(r#"{{"type":"session_meta","payload":{{"id":"{CX_ID}","cwd":"{cwd}"}}}}"#),
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"hello"}]}}"#.into(),
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell","arguments":"ls"}}"#.into(),
        ]
        .join("\n"),
    );
    // Steve's own run: never listed
    let steve = root.join("steve/worktrees/ST-1").display().to_string();
    write(
        &root.join(".claude/projects/-steve/aaaa.jsonl"),
        &claude(&steve, "steve's own"),
    );
    // a transcript outside the harnesses' stores
    write(&root.join("elsewhere/x.jsonl"), &claude(&cwd, "forged"));
    T {
        _dir: dir,
        root,
        node,
    }
}

fn cmd(op: &str, source: HistoryRef, expected: Revision) -> Command<HistoryImport> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    Command {
        id: OperationId::new(op).unwrap(),
        issued_at: Timestamp(now),
        accept_before: Timestamp(now + 60_000),
        grant: GrantId::new("hub").unwrap(),
        body: HistoryImport { source, expected },
    }
}

fn page(limit: u32, cursor: Option<Cursor>) -> PageRequest {
    PageRequest { cursor, limit }
}

async fn listed(t: &T) -> Vec<HistoryEntry> {
    t.node.history(page(100, None)).await.unwrap().items
}

#[tokio::test]
async fn history_lists_the_harnesses_transcripts_and_never_steves_own() {
    let t = setup("bin/codex");
    let items = listed(&t).await;
    let mut keys: Vec<&str> = items.iter().map(|e| e.source.key.as_str()).collect();
    keys.sort();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(keys[0].starts_with("cc:") && keys[0].ends_with(&format!("{CC_ID}.jsonl")));
    assert!(keys[1].starts_with("cx:") && keys[1].ends_with(&format!("{CX_ID}.jsonl")));
    let cc = items
        .iter()
        .find(|e| e.source.harness == "claude-code")
        .unwrap();
    assert_eq!(cc.title, "fix the bug");
    assert_eq!(
        cc.directory.as_deref(),
        Some(t.root.join("w/app").to_str().unwrap())
    );
    assert!(cc.size.unwrap() > 0);
    // pages: one at a time, then no cursor
    let first = t.node.history(page(1, None)).await.unwrap();
    assert_eq!(first.items.len(), 1);
    let second = t.node.history(page(1, first.next)).await.unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].source, second.items[0].source);
}

#[tokio::test]
async fn an_import_reads_a_listed_key_at_its_version_and_pages_its_events() {
    let t = setup("bin/codex");
    let cc = listed(&t)
        .await
        .into_iter()
        .find(|e| e.source.harness == "claude-code")
        .unwrap();
    let c = cmd("imp_1", cc.source.clone(), cc.version.clone());
    let r = t.node.import_history(c.clone()).await.unwrap();
    let Observed::Finished {
        outcome: Outcome::Succeeded,
        result: Some(OperationResult::Pages { operation }),
        ..
    } = r.state
    else {
        panic!("{r:?}");
    };
    // the same command again (as sent, timestamps and all): the same answer, nothing read twice
    assert_eq!(
        t.node
            .import_history(c.clone())
            .await
            .unwrap()
            .operation
            .as_str(),
        "imp_1"
    );
    let mut events = vec![];
    let mut cursor = None;
    loop {
        let p = t
            .node
            .import_page(operation.clone(), page(2, cursor))
            .await
            .unwrap();
        events.extend(p.events);
        match p.next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    let bodies: Vec<&EventBody> = events.iter().map(|e| &e.body).collect();
    assert!(matches!(
        bodies[0],
        EventBody::MessageStarted {
            role: Role::User,
            ..
        }
    ));
    assert!(matches!(bodies[1], EventBody::TextDelta { text, .. } if text == "fix the bug"));
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, EventBody::TextDelta { text, .. } if text == "ok"))
    );
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, EventBody::ToolChanged { .. }))
    );
    // history never counts as usage
    assert!(!bodies.iter().any(|b| matches!(b, EventBody::Usage { .. })));
    // numbered without gaps under the import's own operation
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e.cursor.seq, i as u64 + 1);
        assert_eq!(
            e.subject,
            Subject::Operation {
                operation: operation.clone()
            }
        );
    }
    // the same id with another content: a conflict
    let e = t
        .node
        .import_history(cmd(
            "imp_1",
            cc.source.clone(),
            Revision::new("m1-s1").unwrap(),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    // changed since it was listed: stale
    let e = t
        .node
        .import_history(cmd(
            "imp_2",
            cc.source.clone(),
            Revision::new("m1-s1").unwrap(),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
}

#[tokio::test]
async fn a_forged_key_is_refused() {
    let t = setup("bin/codex");
    let cc = listed(&t)
        .await
        .into_iter()
        .find(|e| e.source.harness == "claude-code")
        .unwrap();
    let projects = t.root.join(".claude/projects/-w-app");
    let forged = [
        // a transcript the harness does not keep, by its absolute path
        (
            "claude-code",
            format!("cc:{}", t.root.join("elsewhere/x.jsonl").display()),
        ),
        // out of the store with `..`
        (
            "claude-code",
            format!("cc:{}/../../../elsewhere/x.jsonl", projects.display()),
        ),
        // a listed file under the other harness
        ("codex", cc.source.key.replace("cc:", "cx:")),
        ("codex", cc.source.key.clone()),
        // Steve's own run is not listed either
        (
            "claude-code",
            format!(
                "cc:{}",
                t.root.join(".claude/projects/-steve/aaaa.jsonl").display()
            ),
        ),
        ("claude-code", "cc:".into()),
        ("claude-code", "nocolon".into()),
    ];
    for (i, (harness, key)) in forged.iter().enumerate() {
        let source = HistoryRef {
            harness: (*harness).into(),
            key: key.clone(),
        };
        let e = t
            .node
            .import_history(cmd(
                &format!("imp_f{i}"),
                source.clone(),
                cc.version.clone(),
            ))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{key}: {e:?}");
        let e = t.node.locate(source).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{key}");
    }
    // an id that is a path is not an id
    for id in ["../x", "a/b", ""] {
        let e = t
            .node
            .locate(HistoryRef {
                harness: "claude-code".into(),
                key: format!("session:{id}"),
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{id}");
    }
    // an import nobody started has no pages
    let e = t
        .node
        .import_page(OperationId::new("imp_none").unwrap(), page(10, None))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[tokio::test]
async fn locate_finds_a_harness_session_by_its_id() {
    let t = setup("bin/codex");
    let cwd = t.root.join("w/app").display().to_string();
    let l = t
        .node
        .locate(HistoryRef {
            harness: "claude-code".into(),
            key: format!("session:{CC_ID}"),
        })
        .await
        .unwrap();
    assert_eq!(
        l.display,
        format!("~/.claude/projects/-w-app/{CC_ID}.jsonl")
    );
    assert_eq!(l.resume.unwrap().as_str(), CC_ID);
    assert_eq!(l.recorded_id.as_deref(), Some(CC_ID));
    assert_eq!(l.recorded_cwd.as_deref(), Some(cwd.as_str()));
    assert!(l.updated.is_some());
    let cx = t
        .node
        .locate(HistoryRef {
            harness: "codex".into(),
            key: format!("session:{CX_ID}"),
        })
        .await
        .unwrap();
    assert_eq!(cx.recorded_id.as_deref(), Some(CX_ID));
    // a listed key locates too
    let listed = listed(&t).await;
    let e = listed.iter().find(|e| e.source.harness == "codex").unwrap();
    let l = t.node.locate(e.source.clone()).await.unwrap();
    assert_eq!(l.resume.unwrap().as_str(), CX_ID);
    let gone = t
        .node
        .locate(HistoryRef {
            harness: "claude-code".into(),
            key: "session:gone".into(),
        })
        .await;
    assert_eq!(gone.unwrap_err().code, ErrorCode::Invalid);
}

#[tokio::test]
async fn the_probe_asks_the_configured_binaries() {
    let t = setup("bin/codex");
    let caps = t.node.probe(true).await.unwrap();
    let h = |n: &str| caps.harnesses.iter().find(|h| h.name == n).unwrap().clone();
    assert_eq!(h("claude-code").version, "2.1.0 (Claude Code)");
    assert_eq!(h("codex").version, "codex-cli 0.80.0");
    // Codex's own order (priority), listed ones only
    assert_eq!(h("codex").models, vec!["gpt-5.5-codex", "gpt-5.5"]);
    // no codex here: no version, no models
    let none = setup("bin/no-codex");
    let caps = none.node.probe(false).await.unwrap();
    let cx = caps.harnesses.iter().find(|h| h.name == "codex").unwrap();
    assert_eq!(cx.version, "");
    assert!(cx.models.is_empty());
}

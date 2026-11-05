//! The Workspace and Discovery faces (N0e-E0) over the wire: they answer exactly as the Node does,
//! every new kind crosses unchanged, a secret goes Node → Hub once, and an older peer answers
//! `unsupported` for what it does not know without the link dropping.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use steve_node_api::testkit::{FakeNode, FakeSecrets, build::*};
use steve_node_api::*;
use steve_node_wire::conn::{from_value, unsupported};
use steve_node_wire::methods::*;
use steve_node_wire::testkit::pair;
use steve_node_wire::{Conn, Handler, NodeServer, RemoteSecrets};

fn show<T: Serialize>(r: &ApiResult<T>) -> String {
    match r {
        Ok(v) => format!("ok {}", serde_json::to_string(v).unwrap()),
        Err(e) => format!("err {}", serde_json::to_string(e).unwrap()),
    }
}

fn rel(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

fn at(root: &str, p: &str) -> RootPath {
    RootPath {
        root: RootId::new(root).unwrap(),
        rel: rel(p),
    }
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        cursor: None,
        limit,
    }
}

fn history_ref(key: &str) -> HistoryRef {
    HistoryRef {
        harness: "claude-code".into(),
        key: key.into(),
    }
}

fn status_read() -> GitReadFact {
    GitReadFact {
        read: GitRead::Status,
        observed: "abc".into(),
        answer: GitAnswer::Status {
            entries: vec![StatusEntry {
                path: rel("src/中文.rs"),
                code: "??".into(),
            }],
        },
    }
}

fn facts() -> GitFacts {
    GitFacts {
        revision: Revision::new("g1").unwrap(),
        head: "abc".into(),
        branch: Some("steve/st-1".into()),
        dirty: false,
        refs: vec![],
        ranges: vec![],
        reads: vec![status_read()],
    }
}

/// The same fixture on any fake: two roots, files, a history entry, a workspace with git facts.
async fn fixture(n: &FakeNode) {
    n.set_roots(vec![
        RootInfo {
            id: RootId::new("code").unwrap(),
            display: "~/code".into(),
            access: RootAccess::ReadWrite,
        },
        RootInfo {
            id: RootId::new("notes").unwrap(),
            display: "~/notes".into(),
            access: RootAccess::ReadOnly,
        },
    ]);
    n.put_file("notes", "a.md", b"hello");
    n.put_file("notes", "d/b.md", &[0u8, 1, 2, 0xff]);
    n.put_file("notes", "d/big.md", &[b'x'; 64]);
    n.scan_issue("notes", ScanIssue::LinkRefused { path: rel("d/up") });
    let source = history_ref("cc:abc");
    n.put_history(
        HistoryEntry {
            source: source.clone(),
            title: "修 bug".into(),
            directory: Some("~/code/app".into()),
            updated: Timestamp(5),
            version: Revision::new("h1").unwrap(),
        },
        HistoryLocation {
            source,
            display: "~/.claude/projects/app/abc.jsonl".into(),
            resume: Some(ResumeHandle::new("abc").unwrap()),
        },
    );
    n.prepare_workspace(cmd("op_w", prepare("item:ST-1")))
        .await
        .unwrap();
    n.script_git(&workspace("n1", "item:ST-1"), facts());
}

/// Runs the faces through `n` (the fake itself, or a RemoteNode in front of it).
async fn faces(n: Arc<dyn Node>) -> Vec<String> {
    let mut out = vec![];
    out.push(show(&n.list_root(at("notes", ""), page(10)).await));
    out.push(show(&n.list_root(at("etc", ""), page(10)).await));
    out.push(show(
        &n.read_file(FileRead {
            path: FileAddress::Root(at("notes", "d/b.md")),
            expected: None,
            offset: 0,
            max: 100,
        })
        .await,
    ));
    let spec = ScanSpec {
        root: at("notes", ""),
        include: vec![],
        exclude: vec![],
        limits: ScanLimits {
            max_files: 10,
            max_file_bytes: 32,
        },
    };
    out.push(show(&n.scan(cmd("op_scan", spec.clone())).await));
    let scan = OperationId::new("op_scan").unwrap();
    let p1 = n.scan_page(scan.clone(), page(2)).await;
    out.push(show(&p1));
    let p2 = n
        .scan_page(
            scan,
            PageRequest {
                cursor: p1.unwrap().next,
                limit: 2,
            },
        )
        .await;
    out.push(show(&p2));
    let w = workspace("n1", "item:ST-1");
    out.push(show(
        &n.git_facts(GitQuery {
            workspace: w.clone(),
            refs: vec![],
            ranges: vec![],
            fetch: false,
            reads: vec![GitRead::Status],
        })
        .await,
    ));
    let f = facts();
    let push = GitApply {
        workspace: w.clone(),
        expected: f.revision.clone(),
        expected_head: f.head.clone(),
        action: GitAction::Push {
            remote: "origin".into(),
            branch: "steve/st-1".into(),
            expected_remote: None,
            url: "https://github.com/acme/app.git".into(),
            origin_must_be: Some("acme/app".into()),
            expected_local: f.head.clone(),
            auth: Some(SecretRef {
                id: "gh".into(),
                how: SecretDelivery::Header {
                    name: "Authorization".into(),
                },
            }),
        },
    };
    out.push(show(&n.git_apply(cmd("op_push", push.clone())).await));
    // resent: the same receipt, the secret not asked for again
    out.push(show(&n.git_apply(cmd("op_push", push)).await));
    out.push(show(&n.history(page(10)).await));
    out.push(show(&n.locate(history_ref("cc:abc")).await));
    out.push(show(&n.locate(history_ref("cc:/etc/passwd")).await));
    out
}

#[tokio::test]
async fn the_faces_answer_over_the_wire_exactly_like_the_node() {
    let direct = FakeNode::new("n1");
    let direct_secrets = Arc::new(FakeSecrets::default());
    direct_secrets.give("op_push", "gh", b"basic xyz");
    direct.connect_secrets(direct_secrets.clone());
    fixture(&direct).await;
    let a = faces(direct.clone()).await;

    let fake = FakeNode::new("n1");
    let hub_secrets = Arc::new(FakeSecrets::default());
    hub_secrets.give("op_push", "gh", b"basic xyz");
    let w = wire_with(fake.clone(), 1, Some(hub_secrets.clone()));
    // the Node takes its secrets from the Hub over the same link
    fake.connect_secrets(Arc::new(RemoteSecrets(w.link.clone())));
    fixture(&fake).await;
    let b = faces(w.remote.clone()).await;

    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x, y, "step {i}");
    }
    assert!(a[0].starts_with("ok"), "{}", a[0]);
    assert!(a[7].starts_with("ok"), "the push went through: {}", a[7]);
    assert!(a[11].contains("invalid"), "a forged key: {}", a[11]);
    assert_eq!(direct_secrets.taken(), hub_secrets.taken());
    assert_eq!(hub_secrets.taken().len(), 1);
    assert_eq!(direct.log(), fake.log());
}

#[tokio::test]
async fn a_remote_node_never_offers_secret_files_and_runs_no_exec() {
    let fake = FakeNode::new("n1");
    fake.set_features(vec![FEATURE_SECRET_FILE.into(), "other".into()]);
    fake.prepare_workspace(cmd("op_w", prepare("item:ST-1")))
        .await
        .unwrap();
    let w = wire(fake.clone(), 1);
    let c = w.remote.probe(false).await.unwrap();
    assert_eq!(c.features, ["other"]);
    let e = w
        .remote
        .exec(cmd(
            "op_e",
            ExecSpec {
                workspace: workspace("n1", "item:ST-1"),
                command: "true".into(),
                cwd: None,
                policy: policy(),
                limits: limits(),
                env: vec![],
                secrets: vec![],
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert_eq!(fake.log().len(), 1, "nothing was sent");
}

#[tokio::test]
async fn a_hub_without_secrets_answers_unsupported() {
    let fake = FakeNode::new("n1");
    let w = wire(fake, 1);
    let e = RemoteSecrets(w.link.clone())
        .take(OperationId::new("op_1").unwrap(), "gh".into())
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert!(w.remote.probe(false).await.is_ok(), "still up");
}

// ── every new kind, as it crosses ──

/// Answers every call with its own arguments.
struct Echo;

#[async_trait]
impl Handler for Echo {
    async fn handle(&self, _method: &str, p: Value) -> ApiResult<Value> {
        Ok(p)
    }
}

/// Both ends of a link that echoes (keep the second alive).
fn echo_link() -> (Arc<Conn>, Arc<Conn>) {
    let (h, n) = pair();
    let hub = Conn::spawn(h.sink, h.stream, 1, Arc::new(Echo), opts("h"));
    let node = Conn::spawn(n.sink, n.stream, 1, Arc::new(Echo), opts("n"));
    (hub, node)
}

async fn crosses<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(c: &Conn, v: T) {
    let back = c
        .call(GIT_INSPECT, serde_json::to_value(&v).unwrap(), None)
        .await
        .unwrap();
    let back: T = from_value(back).unwrap();
    assert_eq!(back, v);
}

#[tokio::test]
async fn every_new_kind_crosses_the_wire_unchanged() {
    let (c, _node) = echo_link();
    let range = RevRange {
        from: Some("main".into()),
        to: "HEAD".into(),
    };
    for r in [
        GitRead::Show {
            rev: "HEAD".into(),
            path: Some(rel("a")),
            max: 1,
        },
        GitRead::Show {
            rev: "HEAD".into(),
            path: None,
            max: 1,
        },
        GitRead::Log {
            range: range.clone(),
            max: 2,
        },
        GitRead::Diff {
            range: range.clone(),
            max: 3,
        },
        GitRead::IsAncestor {
            ancestor: "a".into(),
            of: "b".into(),
        },
        GitRead::Status,
        GitRead::Fingerprint,
        GitRead::TrackedIgnored { rel: rel("target") },
    ] {
        crosses(&c, r).await;
    }
    let commit = CommitInfo {
        sha: "abc".into(),
        subject: "s".into(),
        author: "a".into(),
        time: Timestamp(9),
        body: None,
    };
    for a in [
        GitAnswer::Blob {
            found: false,
            bytes: vec![0xde, 0xad],
            truncated: true,
        },
        GitAnswer::Commit {
            commit: commit.clone(),
            stat: "x".into(),
            patch: "y".into(),
            truncated: false,
        },
        GitAnswer::Log {
            commits: vec![commit],
            truncated: true,
        },
        GitAnswer::Diff {
            stat: "1".into(),
            patch: "+".into(),
            truncated: false,
        },
        GitAnswer::IsAncestor { yes: false },
        GitAnswer::Status { entries: vec![] },
        GitAnswer::Fingerprint { tree: "t".into() },
        GitAnswer::TrackedIgnored {
            ignored: false,
            tracked: true,
        },
    ] {
        crosses(&c, a).await;
    }
    for i in [
        ScanIssue::Unreadable { path: rel("u") },
        ScanIssue::OverLimit { count: 7 },
        ScanIssue::LinkRefused { path: rel("l") },
        ScanIssue::Denied { path: rel("d") },
        ScanIssue::Interrupted,
    ] {
        crosses(&c, i).await;
    }
    for how in [
        SecretDelivery::Env { name: "A".into() },
        SecretDelivery::Stdin,
        SecretDelivery::File { env: "F".into() },
        SecretDelivery::Header {
            name: "Authorization".into(),
        },
    ] {
        crosses(
            &c,
            SecretRef {
                id: "s".into(),
                how,
            },
        )
        .await;
    }
    crosses(
        &c,
        GitAction::Merge {
            commit: "c".into(),
            message: Some("m".into()),
            back_out_on_conflict: false,
        },
    )
    .await;
    crosses(&c, RootAccess::ReadOnly).await;
    crosses(&c, RootAccess::ReadWrite).await;
    crosses(&c, SecretBytes::new(vec![1, 2, 3])).await;
    crosses(
        &c,
        HistoryLocation {
            source: history_ref("cc:abc"),
            display: "d".into(),
            resume: None,
        },
    )
    .await;
    crosses(
        &c,
        ScanLimits {
            max_files: 1,
            max_file_bytes: 2,
        },
    )
    .await;
}

// ── an older peer ──

/// A Node of the previous minor version: it knows no `history.locate`, and only the git reads
/// and scan issues of its own day.
struct OldPeer(Arc<NodeServer<FakeNode>>);

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct OldQuery {
    workspace: WorkspaceRef,
    #[serde(default)]
    reads: Vec<OldRead>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OldRead {
    Status,
}

#[async_trait]
impl Handler for OldPeer {
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value> {
        match method {
            HISTORY_LOCATE => Err(unsupported(method)),
            GIT_INSPECT => {
                let _: OldQuery = from_value(p.clone())?;
                self.0.handle(method, p).await
            }
            _ => self.0.handle(method, p).await,
        }
    }
}

#[tokio::test]
async fn an_older_peer_answers_unsupported_and_the_link_stays() {
    let fake = FakeNode::new("n1");
    fake.prepare_workspace(cmd("op_w", prepare("item:ST-1")))
        .await
        .unwrap();
    fake.script_git(&workspace("n1", "item:ST-1"), facts());
    let (h, n) = pair();
    let hub = Conn::spawn(h.sink, h.stream, 1, Arc::new(Echo), opts("h"));
    let _node = Conn::spawn(
        n.sink,
        n.stream,
        1,
        Arc::new(OldPeer(NodeServer::new(fake.clone()))),
        opts("n"),
    );
    let remote = steve_node_wire::RemoteNode::new(NodeId::new("n1").unwrap());
    remote.attach(hub.clone(), false);

    let e = remote.locate(history_ref("cc:abc")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    // a read kind it has never heard of: unsupported, not a broken link
    let q = |reads| GitQuery {
        workspace: workspace("n1", "item:ST-1"),
        refs: vec![],
        ranges: vec![],
        fetch: false,
        reads,
    };
    let e = remote
        .git_facts(q(vec![GitRead::Fingerprint]))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported, "{e}");
    // what it knows still works on the same link
    let f = remote.git_facts(q(vec![GitRead::Status])).await.unwrap();
    assert_eq!(f.reads, [status_read()]);
    assert!(remote.probe(false).await.is_ok());
    // a malformed payload is still invalid, not unsupported
    let e = hub
        .call(GIT_INSPECT, json!({"workspace": 1}), None)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

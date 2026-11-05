//! The Workspace and Discovery faces (N0e-E0), checked against the fake Node: readonly roots,
//! scans that say when they are incomplete, typed git reads, execs and one-shot secrets.

use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use steve_node_api::testkit::{FakeNode, FakeSecrets, Scripted, build::*};
use steve_node_api::*;

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
    let s = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&s).unwrap();
    assert_eq!(&back, v, "{s}");
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

fn root(id: &str, access: RootAccess) -> RootInfo {
    RootInfo {
        id: RootId::new(id).unwrap(),
        display: format!("~/{id}"),
        access,
    }
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        cursor: None,
        limit,
    }
}

fn existing(resource: &str, dir: RootPath) -> Prepare {
    Prepare {
        resource: resource.into(),
        kind: PrepareKind::Existing { dir },
    }
}

fn exec_in(w: WorkspaceRef, command: &str) -> ExecSpec {
    ExecSpec {
        workspace: w,
        command: command.into(),
        cwd: None,
        policy: policy(),
        limits: limits(),
        env: vec![("LANG".into(), "C".into())],
        secrets: vec![],
    }
}

fn facts(head: &str) -> GitFacts {
    GitFacts {
        revision: Revision::new("g1").unwrap(),
        head: head.into(),
        branch: Some("main".into()),
        dirty: false,
        refs: vec![],
        ranges: vec![],
        reads: vec![],
    }
}

fn rebase(w: WorkspaceRef, expected: &GitFacts) -> GitApply {
    GitApply {
        workspace: w,
        expected: expected.revision.clone(),
        expected_head: expected.head.clone(),
        action: GitAction::Rebase {
            onto: "origin/main".into(),
        },
    }
}

fn push(w: WorkspaceRef, expected: &GitFacts, auth: Option<SecretRef>) -> GitApply {
    GitApply {
        workspace: w,
        expected: expected.revision.clone(),
        expected_head: expected.head.clone(),
        action: GitAction::Push {
            remote: "origin".into(),
            branch: "steve/st-1".into(),
            expected_remote: None,
            url: "https://github.com/acme/app.git".into(),
            origin_must_be: Some("acme/app".into()),
            expected_local: expected.head.clone(),
            auth,
        },
    }
}

fn result_of(r: Receipt) -> OperationResult {
    match r.state {
        Observed::Finished {
            result: Some(x), ..
        } => x,
        other => panic!("{other:?}"),
    }
}

/// A Node with a read-write `code` root and a readonly `notes` root.
fn two_roots() -> Arc<FakeNode> {
    let n = FakeNode::new("mbp");
    n.set_roots(vec![
        root("code", RootAccess::ReadWrite),
        root("notes", RootAccess::ReadOnly),
    ]);
    n
}

// ── wire shape ──

#[test]
fn the_new_types_survive_the_wire() {
    let w = workspace("mbp", "item:ST-1");
    let reads = vec![
        GitRead::Show {
            rev: "HEAD".into(),
            path: Some(rel("README.md")),
            max: 1024,
        },
        GitRead::Show {
            rev: "abc".into(),
            path: None,
            max: 1024,
        },
        GitRead::Log {
            range: RevRange {
                from: Some("main".into()),
                to: "HEAD".into(),
            },
            max: 50,
        },
        GitRead::Diff {
            range: RevRange {
                from: None,
                to: "HEAD".into(),
            },
            max: 4096,
        },
        GitRead::IsAncestor {
            ancestor: "a".into(),
            of: "b".into(),
        },
        GitRead::Status,
        GitRead::Fingerprint,
        GitRead::TrackedIgnored { rel: rel("target") },
    ];
    roundtrip(&GitQuery {
        workspace: w.clone(),
        refs: vec!["HEAD".into()],
        ranges: vec![],
        fetch: false,
        reads: reads.clone(),
    });
    let commit = CommitInfo {
        sha: "abc".into(),
        subject: "修复".into(),
        author: "Lee".into(),
        time: Timestamp(1),
        body: Some("修复\n\n细节".into()),
    };
    let answers = vec![
        GitAnswer::Blob {
            found: true,
            bytes: vec![0, 1, 0xff],
            truncated: false,
        },
        GitAnswer::Commit {
            commit: commit.clone(),
            stat: "1 file".into(),
            patch: "+x".into(),
            truncated: true,
        },
        GitAnswer::Log {
            commits: vec![commit],
            truncated: false,
        },
        GitAnswer::Diff {
            stat: String::new(),
            patch: String::new(),
            truncated: false,
        },
        GitAnswer::IsAncestor { yes: true },
        GitAnswer::Status {
            entries: vec![StatusEntry {
                path: rel("a.rs"),
                code: " M".into(),
            }],
        },
        GitAnswer::Fingerprint { tree: "t1".into() },
        GitAnswer::TrackedIgnored {
            ignored: true,
            tracked: false,
        },
    ];
    let mut f = facts("abc");
    f.reads = reads
        .into_iter()
        .zip(answers)
        .map(|(read, answer)| GitReadFact {
            read,
            observed: "abc".into(),
            answer,
        })
        .collect();
    roundtrip(&OperationResult::Git(f.clone()));
    let header = SecretRef {
        id: "gh_token".into(),
        how: SecretDelivery::Header {
            name: "Authorization".into(),
        },
    };
    roundtrip(&cmd("op_p", push(w.clone(), &f, Some(header))));
    roundtrip(&cmd(
        "op_m",
        GitApply {
            workspace: w.clone(),
            expected: f.revision.clone(),
            expected_head: f.head.clone(),
            action: GitAction::Merge {
                commit: "def".into(),
                message: Some("合并 ST-1".into()),
                back_out_on_conflict: false,
            },
        },
    ));
    let mut e = exec_in(w, "make check");
    e.secrets = vec![SecretRef {
        id: "v1".into(),
        how: SecretDelivery::File {
            env: "KEY_FILE".into(),
        },
    }];
    roundtrip(&cmd("op_e", e));
    roundtrip(&cmd(
        "op_s",
        ScanSpec {
            root: at("notes", ""),
            include: vec!["**/*.md".into()],
            exclude: vec![],
            limits: ScanLimits {
                max_files: 3,
                max_file_bytes: 10,
            },
        },
    ));
    roundtrip(&ScanPage {
        scan: OperationId::new("op_s").unwrap(),
        manifest: Revision::new("m1").unwrap(),
        entries: vec![FileInfo {
            path: rel("a.md"),
            is_dir: false,
            size: 11,
            version: Revision::new("r1").unwrap(),
            too_big: true,
            mtime: None,
        }],
        next: None,
        complete: false,
        issues: vec![
            ScanIssue::Unreadable {
                path: Some(rel("x")),
            },
            ScanIssue::Unreadable { path: None },
            ScanIssue::OverLimit { count: 2 },
            ScanIssue::Held {
                path: rel("l"),
                why: HeldWhy::Outside,
            },
            ScanIssue::Held {
                path: rel("d"),
                why: HeldWhy::Denied,
            },
            ScanIssue::Held {
                path: rel("f"),
                why: HeldWhy::Unreadable,
            },
            ScanIssue::Interrupted,
        ],
    });
    roundtrip(&root("notes", RootAccess::ReadOnly));
    roundtrip(&HistoryLocation {
        source: HistoryRef {
            harness: "claude-code".into(),
            key: "cc:abc".into(),
        },
        display: "~/.claude/projects/x/abc.jsonl".into(),
        resume: Some(ResumeHandle::new("abc").unwrap()),
    });
    let s = SecretBytes::new(b"hunter2".to_vec());
    let back: SecretBytes = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    assert!(!format!("{s:?}").contains("hunter2"), "{s:?}");
}

#[test]
fn what_an_older_peer_sends_still_decodes() {
    // fields added in this minor version all have defaults
    let r: RootInfo = serde_json::from_str(r#"{"id":"code","display":"~/code"}"#).unwrap();
    assert_eq!(r.access, RootAccess::ReadWrite);
    let f: FileInfo =
        serde_json::from_str(r#"{"path":"a","is_dir":false,"size":1,"version":"r1"}"#).unwrap();
    assert!(!f.too_big);
    let s: ScanSpec =
        serde_json::from_str(r#"{"root":{"root":"notes","rel":""},"include":[],"exclude":[]}"#)
            .unwrap();
    assert_eq!(s.limits, ScanLimits::default());
    let m: GitAction = serde_json::from_str(r#"{"type":"merge","commit":"abc"}"#).unwrap();
    assert_eq!(
        m,
        GitAction::Merge {
            commit: "abc".into(),
            message: None,
            back_out_on_conflict: true,
        }
    );
    let c: Capabilities = serde_json::from_value(serde_json::json!({
        "node": "n1", "os": "x", "version": "0", "harnesses": [], "roots": [],
        "capacity": {"sessions": 1, "jobs": 0, "execs": 0}
    }))
    .unwrap();
    assert!(c.features.is_empty());
    // and an unknown issue kind is refused rather than read as something else
    assert!(serde_json::from_str::<ScanIssue>(r#"{"type":"all_fine"}"#).is_err());
}

// ── readonly roots ──

#[tokio::test]
async fn a_readonly_root_refuses_prepare_exec_and_git_apply() {
    let n = two_roots();
    let denied = |r: ApiResult<Receipt>| r.unwrap_err().code;
    // preparing in it, as a directory or a worktree, is denied
    assert_eq!(
        denied(
            n.prepare_workspace(cmd("op_p1", existing("item:a", at("notes", "x"))))
                .await
        ),
        ErrorCode::Denied
    );
    let wt = Prepare {
        resource: "item:b".into(),
        kind: PrepareKind::Worktree {
            repository: at("notes", "repo"),
            branch: "steve/b".into(),
            base: None,
        },
    };
    assert_eq!(
        denied(n.prepare_workspace(cmd("op_p2", wt)).await),
        ErrorCode::Denied
    );
    // an unknown root is denied too
    assert_eq!(
        denied(
            n.prepare_workspace(cmd("op_p3", existing("item:c", at("elsewhere", ""))))
                .await
        ),
        ErrorCode::Denied
    );
    // a read-write one works; once its root turns readonly, nothing runs or changes there
    let w = match result_of(
        n.prepare_workspace(cmd("op_p4", existing("item:d", at("code", "app"))))
            .await
            .unwrap(),
    ) {
        OperationResult::Workspace(v) => v.workspace,
        other => panic!("{other:?}"),
    };
    n.script_git(&w, facts("abc"));
    n.exec(cmd("op_e1", exec_in(w.clone(), "true")))
        .await
        .unwrap();
    n.set_roots(vec![
        root("code", RootAccess::ReadOnly),
        root("notes", RootAccess::ReadOnly),
    ]);
    assert_eq!(
        denied(n.exec(cmd("op_e2", exec_in(w.clone(), "true"))).await),
        ErrorCode::Denied
    );
    assert_eq!(
        denied(
            n.git_apply(cmd("op_g1", rebase(w.clone(), &facts("abc"))))
                .await
        ),
        ErrorCode::Denied
    );
    // reading is still fine
    assert!(n.git_facts(git_query(w)).await.is_ok());
    let log: Vec<_> = n.log().into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        log,
        ["prepare_workspace", "exec"],
        "nothing refused was recorded"
    );
}

fn git_query(w: WorkspaceRef) -> GitQuery {
    GitQuery {
        workspace: w,
        refs: vec![],
        ranges: vec![],
        fetch: false,
        reads: vec![],
    }
}

#[tokio::test]
async fn a_readonly_root_is_listed_read_and_scanned() {
    let n = two_roots();
    n.put_file("notes", "a.md", b"hello");
    n.put_file("notes", "sub/b.md", b"world");
    let l = n.list_root(at("notes", ""), page(10)).await.unwrap();
    let names: Vec<_> = l
        .items
        .iter()
        .map(|f| (f.path.as_str(), f.is_dir))
        .collect();
    assert_eq!(names, [("a.md", false), ("sub", true)]);
    let l = n.list_root(at("notes", "sub"), page(10)).await.unwrap();
    assert_eq!(l.items[0].path.as_str(), "sub/b.md");
    let a = &n.list_root(at("notes", ""), page(10)).await.unwrap().items[0];
    let read = |expected: Option<Revision>, offset, max| FileRead {
        path: FileAddress::Root(at("notes", "a.md")),
        expected,
        offset,
        max,
    };
    let c = n
        .read_file(read(Some(a.version.clone()), 1, 3))
        .await
        .unwrap();
    assert_eq!((c.bytes.as_slice(), c.eof), (&b"ell"[..], false));
    let c = n.read_file(read(None, 3, 100)).await.unwrap();
    assert_eq!((c.bytes.as_slice(), c.eof), (&b"lo"[..], true));
    // a file that changed since its listing answers conflict
    n.put_file("notes", "a.md", b"hello again");
    assert_eq!(
        n.read_file(read(Some(a.version.clone()), 0, 10))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // a root the Node does not allow is denied
    assert_eq!(
        n.list_root(at("etc", ""), page(10)).await.unwrap_err().code,
        ErrorCode::Denied
    );
    let r = n
        .scan(cmd(
            "op_s",
            ScanSpec {
                root: at("notes", ""),
                include: vec![],
                exclude: vec![],
                limits: ScanLimits::default(),
            },
        ))
        .await
        .unwrap();
    assert!(matches!(result_of(r), OperationResult::Pages { .. }));
}

// ── scans ──

async fn scan_all(n: &FakeNode, op: &str, spec: ScanSpec, limit: u32) -> Vec<ScanPage> {
    let r = n.scan(cmd(op, spec)).await.unwrap();
    let OperationResult::Pages { operation } = result_of(r) else {
        panic!()
    };
    let mut pages = vec![];
    let mut cursor = None;
    loop {
        let p = n
            .scan_page(operation.clone(), PageRequest { cursor, limit })
            .await
            .unwrap();
        assert!(p.is_consistent(), "{p:?}");
        cursor = p.next.clone();
        pages.push(p);
        if cursor.is_none() {
            return pages;
        }
    }
}

fn spec(limits: ScanLimits) -> ScanSpec {
    ScanSpec {
        root: at("notes", ""),
        include: vec![],
        exclude: vec![],
        limits,
    }
}

#[tokio::test]
async fn scan_pages_share_one_manifest_and_only_a_clean_end_is_complete() {
    let n = two_roots();
    for i in 0..5 {
        n.put_file("notes", &format!("d/{i}.md"), b"x");
    }
    n.put_file("code", "other.md", b"not this root");
    let pages = scan_all(&n, "op_s1", spec(ScanLimits::default()), 2).await;
    assert_eq!(pages.len(), 3);
    assert!(pages.iter().all(|p| p.manifest == pages[0].manifest));
    assert_eq!(
        pages.iter().map(|p| p.complete).collect::<Vec<_>>(),
        [false, false, true]
    );
    let all: Vec<_> = pages
        .iter()
        .flat_map(|p| p.entries.iter().map(|e| e.path.as_str().to_string()))
        .collect();
    assert_eq!(all, ["d/0.md", "d/1.md", "d/2.md", "d/3.md", "d/4.md"]);
    // the same tree scanned again: the same manifest
    let again = scan_all(&n, "op_s2", spec(ScanLimits::default()), 10).await;
    assert_eq!(again[0].manifest, pages[0].manifest);

    // over the file limit: cut, the count said, never complete
    let over = scan_all(
        &n,
        "op_s3",
        spec(ScanLimits {
            max_files: 3,
            max_file_bytes: 1024,
        }),
        10,
    )
    .await;
    let last = over.last().unwrap();
    assert_eq!(last.entries.len(), 3);
    assert!(!last.complete);
    assert_eq!(last.issues, [ScanIssue::OverLimit { count: 2 }]);

    // a too-big file is listed (so it is not taken for gone) and marked
    n.put_file("notes", "d/big.md", &[b'a'; 20]);
    let big = scan_all(
        &n,
        "op_s4",
        spec(ScanLimits {
            max_files: 100,
            max_file_bytes: 10,
        }),
        100,
    )
    .await;
    let e = &big[0].entries;
    assert!(e.iter().any(|f| f.path.as_str() == "d/big.md" && f.too_big));
    assert!(big[0].complete, "a too-big file hides nothing");

    // a gap (an issue that may hide a file) makes it incomplete
    for issue in [
        ScanIssue::Unreadable {
            path: Some(rel("d/locked")),
        },
        // an entry whose name could not be read has no path
        ScanIssue::Unreadable { path: None },
        ScanIssue::Interrupted,
    ] {
        assert!(issue.hides_files(), "{issue:?}");
        let m = two_roots();
        m.put_file("notes", "a.md", b"x");
        m.scan_issue("notes", issue.clone());
        let p = scan_all(&m, "op_s", spec(ScanLimits::default()), 10).await;
        assert!(!p.last().unwrap().complete, "{issue:?}");
        assert_eq!(p.last().unwrap().issues, [issue]);
    }
    // a held path is reported and the scan stays complete, as the knowledge walk has it: a link
    // out of the folder or one unreadable file must not stop deletions elsewhere reconciling
    for why in [HeldWhy::Outside, HeldWhy::Denied, HeldWhy::Unreadable] {
        let issue = ScanIssue::Held {
            path: rel("d/held.md"),
            why,
        };
        assert!(!issue.hides_files());
        let m = two_roots();
        m.put_file("notes", "a.md", b"x");
        m.scan_issue("notes", issue.clone());
        let p = scan_all(&m, "op_s", spec(ScanLimits::default()), 10).await;
        assert!(p.last().unwrap().complete, "{why:?}");
        assert_eq!(p.last().unwrap().issues, [issue]);
    }
    // a complete page may carry holds, never a gap
    let page_with = |issues: Vec<ScanIssue>| ScanPage {
        scan: OperationId::new("op_c").unwrap(),
        manifest: Revision::new("m").unwrap(),
        entries: vec![],
        next: None,
        complete: true,
        issues,
    };
    assert!(
        page_with(vec![ScanIssue::Held {
            path: rel("l"),
            why: HeldWhy::Outside,
        }])
        .is_consistent()
    );
    assert!(!page_with(vec![ScanIssue::OverLimit { count: 1 }]).is_consistent());
    // a resend of a scan is the same scan; an unknown one is invalid
    n.scan(cmd("op_s1", spec(ScanLimits::default())))
        .await
        .unwrap();
    assert_eq!(
        n.scan_page(OperationId::new("op_nope").unwrap(), page(1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
    // a cursor of one scan does not page another
    let other = pages[0].next.clone();
    assert_eq!(
        n.scan_page(
            OperationId::new("op_s2").unwrap(),
            PageRequest {
                cursor: other,
                limit: 2
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Invalid
    );
    // include and exclude patterns are the glob's; a bad one is refused
    n.put_file("notes", "d/x.txt", b"x");
    n.put_file("notes", "d/x.png", b"x");
    n.put_file("notes", ".obsidian/w.md", b"x");
    let only = scan_all(
        &n,
        "op_s5",
        ScanSpec {
            include: vec!["**/*.txt".into(), "**/*.png".into()],
            exclude: vec!["**/*.png".into()],
            ..spec(ScanLimits::default())
        },
        10,
    )
    .await;
    let names: Vec<_> = only[0].entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(names, ["d/x.txt"]);
    let dflt = scan_all(&n, "op_s6", spec(ScanLimits::default()), 100).await;
    let names: Vec<_> = dflt[0].entries.iter().map(|e| e.path.as_str()).collect();
    assert!(names.contains(&"d/x.txt") && names.contains(&"d/0.md"));
    assert!(
        !names.contains(&"d/x.png") && !names.contains(&".obsidian/w.md"),
        "the default include, never a dot directory: {names:?}"
    );
    let bad = ScanSpec {
        include: vec!["../*.md".into()],
        ..spec(ScanLimits::default())
    };
    assert_eq!(
        n.scan(cmd("op_s7", bad)).await.unwrap_err().code,
        ErrorCode::Invalid
    );
}

// ── git ──

#[tokio::test]
async fn git_reads_are_typed_and_applies_check_what_they_were_decided_on() {
    let n = FakeNode::new("mbp");
    n.prepare_workspace(cmd("op_w", prepare("item:ST-1")))
        .await
        .unwrap();
    let w = workspace("mbp", "item:ST-1");
    // nothing scripted: the fake does not pretend
    assert_eq!(
        n.git_facts(git_query(w.clone())).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    let mut f = facts("abc");
    f.reads = vec![GitReadFact {
        read: GitRead::IsAncestor {
            ancestor: "abc".into(),
            of: "origin/main".into(),
        },
        observed: "def".into(),
        answer: GitAnswer::IsAncestor { yes: true },
    }];
    n.script_git(&w, f.clone());
    let mut q = git_query(w.clone());
    q.reads = vec![f.reads[0].read.clone()];
    let got = n.git_facts(q.clone()).await.unwrap();
    assert_eq!(got.reads, f.reads);
    q.reads = vec![GitRead::Status];
    assert_eq!(
        n.git_facts(q).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    // decided on an older revision or HEAD: stale, nothing recorded
    let mut old = f.clone();
    old.head = "zzz".into();
    assert_eq!(
        n.git_apply(cmd("op_g0", rebase(w.clone(), &old)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Stale
    );
    let OperationResult::Git(after) = result_of(
        n.git_apply(cmd("op_g1", rebase(w.clone(), &f)))
            .await
            .unwrap(),
    ) else {
        panic!()
    };
    assert_ne!(after.revision, f.revision);
    assert_ne!(after.head, f.head);
    // the old facts no longer hold
    assert_eq!(
        n.git_apply(cmd("op_g2", rebase(w.clone(), &f)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Stale
    );
    // a push credential goes only as a header
    let env = SecretRef {
        id: "t".into(),
        how: SecretDelivery::Env {
            name: "TOKEN".into(),
        },
    };
    assert_eq!(
        n.git_apply(cmd("op_g3", push(w.clone(), &after, Some(env))))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
}

// ── exec and secrets ──

#[tokio::test]
async fn exec_answers_an_exec_result_and_masks_its_secrets() {
    let n = FakeNode::new("mbp");
    let secrets = Arc::new(FakeSecrets::default());
    n.connect_secrets(secrets.clone());
    n.prepare_workspace(cmd("op_w", prepare("item:ST-1")))
        .await
        .unwrap();
    let w = workspace("mbp", "item:ST-1");
    let r = n
        .exec(cmd("op_e1", exec_in(w.clone(), "make check")))
        .await
        .unwrap();
    assert!(matches!(
        result_of(r),
        OperationResult::Exec(ExecResult {
            exit_code: Some(0),
            ..
        })
    ));
    secrets.give("op_e2", "vault:k", b"s3cr3t");
    n.answer_exec(ExecResult {
        exit_code: Some(1),
        stdout: "token=s3cr3t".into(),
        stderr: "s3cr3t!".into(),
        truncated: false,
    });
    let mut spec = exec_in(w.clone(), "deploy");
    spec.secrets = vec![SecretRef {
        id: "vault:k".into(),
        how: SecretDelivery::Env { name: "KEY".into() },
    }];
    let c = cmd("op_e2", spec.clone());
    let first = n.exec(c.clone()).await.unwrap();
    let OperationResult::Exec(x) = result_of(first.clone()) else {
        panic!()
    };
    assert_eq!(
        (x.exit_code, x.stdout.as_str(), x.stderr.as_str()),
        (Some(1), "token=***", "***!")
    );
    // a resend answers the same receipt and takes nothing
    assert_eq!(n.exec(c).await.unwrap(), first);
    assert_eq!(secrets.taken().len(), 1);
    // a new operation with the same secret: it was taken already
    let e = n.exec(cmd("op_e3", spec.clone())).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    // a file needs the Node to offer it
    spec.secrets[0].how = SecretDelivery::File { env: "KEY".into() };
    secrets.give("op_e4", "vault:k", b"s3cr3t");
    assert_eq!(
        n.exec(cmd("op_e4", spec.clone())).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    n.set_features(vec![FEATURE_SECRET_FILE.into()]);
    n.exec(cmd("op_e4", spec)).await.unwrap();
    // a past deadline is refused
    let mut late = exec_in(w, "true");
    late.limits.deadline = Timestamp(1);
    assert_eq!(
        n.exec(cmd("op_e5", late)).await.unwrap_err().code,
        ErrorCode::Expired
    );
}

#[tokio::test]
async fn a_secret_is_taken_once_and_only_by_its_operation() {
    let s = FakeSecrets::default();
    s.give("op_1", "gh", b"tok");
    let op = |x: &str| OperationId::new(x).unwrap();
    assert_eq!(
        s.take(op("op_2"), "gh".into()).await.unwrap_err().code,
        ErrorCode::Denied
    );
    assert_eq!(
        s.take(op("op_1"), "gh".into()).await.unwrap().expose(),
        b"tok"
    );
    assert_eq!(
        s.take(op("op_1"), "gh".into()).await.unwrap_err().code,
        ErrorCode::Stale
    );
    assert_eq!(
        s.take(op("op_1"), "never".into()).await.unwrap_err().code,
        ErrorCode::Denied
    );
}

#[tokio::test]
async fn a_push_takes_its_header_secret_once() {
    let n = FakeNode::new("mbp");
    let secrets = Arc::new(FakeSecrets::default());
    n.connect_secrets(secrets.clone());
    n.prepare_workspace(cmd("op_w", prepare("item:ST-1")))
        .await
        .unwrap();
    let w = workspace("mbp", "item:ST-1");
    let f = facts("abc");
    n.script_git(&w, f.clone());
    let auth = SecretRef {
        id: "gh".into(),
        how: SecretDelivery::Header {
            name: "Authorization".into(),
        },
    };
    secrets.give("op_push", "gh", b"basic xyz");
    let c = cmd("op_push", push(w, &f, Some(auth)));
    let r = n.git_apply(c.clone()).await.unwrap();
    assert_eq!(n.git_apply(c).await.unwrap(), r);
    assert_eq!(secrets.taken().len(), 1);
}

/// A command refused before acceptance never takes its secret: the Hub did not hand it over for
/// work the Node denied, and a resend of the same operation (busy, offline) still finds it.
#[tokio::test]
async fn a_refused_command_leaves_its_secret_with_the_hub() {
    let n = two_roots();
    let secrets = Arc::new(FakeSecrets::default());
    n.connect_secrets(secrets.clone());
    let w = match result_of(
        n.prepare_workspace(cmd("op_w", existing("item:a", at("code", "app"))))
            .await
            .unwrap(),
    ) {
        OperationResult::Workspace(v) => v.workspace,
        other => panic!("{other:?}"),
    };
    let key = SecretRef {
        id: "vault:k".into(),
        how: SecretDelivery::Env { name: "KEY".into() },
    };
    let with_key = |command: &str| {
        let mut e = exec_in(w.clone(), command);
        e.secrets = vec![key.clone()];
        e
    };

    // busy: refused, nothing taken; the same operation sent again runs and takes it then
    secrets.give("op_busy", "vault:k", b"s3cr3t");
    n.refuse("exec", Some(ErrorCode::Busy));
    let c = cmd("op_busy", with_key("deploy"));
    let e = n.exec(c.clone()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    assert_eq!(e.retry, RetryClass::SameOperation);
    assert!(secrets.taken().is_empty());
    n.refuse("exec", None);
    n.refuse_next(
        "exec",
        ApiError::new(ErrorCode::Exhausted, "scripted: no capacity"),
    );
    assert_eq!(
        n.exec(c.clone()).await.unwrap_err().code,
        ErrorCode::Exhausted
    );
    assert!(secrets.taken().is_empty());
    n.exec(c).await.unwrap();
    assert_eq!(secrets.taken().len(), 1);

    // a past deadline, a scripted refusal: refused, nothing taken
    secrets.give("op_late", "vault:k", b"s3cr3t");
    let mut late = with_key("deploy");
    late.limits.deadline = Timestamp(1);
    assert_eq!(
        n.exec(cmd("op_late", late)).await.unwrap_err().code,
        ErrorCode::Expired
    );
    n.script_once(
        "exec",
        Scripted::Refuse(ApiError::new(ErrorCode::Offline, "scripted")),
    );
    assert_eq!(
        n.exec(cmd("op_late", with_key("deploy")))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Offline
    );
    assert_eq!(secrets.taken().len(), 1);

    // a push decided on facts that moved: stale before its credential is taken
    let f = facts("abc");
    n.script_git(&w, f.clone());
    let auth = SecretRef {
        id: "gh".into(),
        how: SecretDelivery::Header {
            name: "Authorization".into(),
        },
    };
    secrets.give("op_push", "gh", b"basic xyz");
    let moved = push(w.clone(), &facts("old"), Some(auth.clone()));
    assert_eq!(
        n.git_apply(cmd("op_push", moved)).await.unwrap_err().code,
        ErrorCode::Stale
    );
    assert_eq!(secrets.taken().len(), 1);

    // the root turned readonly: exec and push denied, nothing taken
    n.set_roots(vec![
        root("code", RootAccess::ReadOnly),
        root("notes", RootAccess::ReadOnly),
    ]);
    assert_eq!(
        n.exec(cmd("op_late", with_key("deploy")))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Denied
    );
    assert_eq!(
        n.git_apply(cmd("op_push", push(w.clone(), &f, Some(auth))))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Denied
    );
    assert_eq!(secrets.taken().len(), 1, "{:?}", secrets.taken());
    let log: Vec<_> = n.log().into_iter().map(|(m, _)| m).collect();
    assert_eq!(log, ["prepare_workspace", "exec"]);
}

// ── discovery ──

#[tokio::test]
async fn only_a_listed_history_is_located() {
    let n = FakeNode::new("mbp");
    let source = HistoryRef {
        harness: "claude-code".into(),
        key: "cc:abc".into(),
    };
    n.put_history(
        HistoryEntry {
            source: source.clone(),
            title: "修 bug".into(),
            directory: Some("~/code/app".into()),
            updated: Timestamp(5),
            version: Revision::new("h1").unwrap(),
        },
        HistoryLocation {
            source: source.clone(),
            display: "~/.claude/projects/app/abc.jsonl".into(),
            resume: Some(ResumeHandle::new("abc").unwrap()),
        },
    );
    assert_eq!(n.history(page(10)).await.unwrap().items.len(), 1);
    let l = n.locate(source).await.unwrap();
    assert_eq!(l.resume.unwrap().as_str(), "abc");
    // a key nobody listed (a path a client made up) is refused
    let forged = HistoryRef {
        harness: "claude-code".into(),
        key: "cc:/etc/passwd".into(),
    };
    assert_eq!(n.locate(forged).await.unwrap_err().code, ErrorCode::Invalid);
}

#[tokio::test]
async fn probe_reports_roots_and_features() {
    let n = two_roots();
    n.set_features(vec![FEATURE_SECRET_FILE.into()]);
    let c = n.probe(false).await.unwrap();
    assert_eq!(c.roots[1].access, RootAccess::ReadOnly);
    assert_eq!(c.features, [FEATURE_SECRET_FILE]);
}

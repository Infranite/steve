//! SG37: characterize real Node scanning, toolchain placement and durable Git pipes.
mod sg37;
use sg37::*;
use std::{os::unix::fs::symlink, path::Path};
use steve_node_api::*;

#[tokio::test]
async fn sg37_scan_entry_classification_and_gap_table() {
    let f = Fixture::new();
    for (p, b) in [
        ("a.md", b"a".as_slice()),
        ("big.md", b"large"),
        ("sub/z.md", b"z"),
        ("skip/a.md", b"x"),
        ("node_modules/a.md", b"x"),
        (".hidden/a.md", b"x"),
        ("bad\\name.md", b"x"),
    ] {
        f.put(p, b);
    }
    let out = f.temp.path().join("out.md");
    std::fs::write(&out, b"outside").unwrap();
    for (to, name) in [
        (f.project().join("a.md"), "link.md"),
        (out.clone(), "outside.md"),
        (out.clone(), "outside.txt"),
        (f.project().join("missing.md"), "dangling.md"),
        (f.project().join("sub"), "linked-dir"),
        (f.project().join("loop.md"), "loop.md"),
    ] {
        symlink(to, f.project().join(name)).unwrap();
    }
    // APFS requires UTF-8 names; exercise unnameable bytes on filesystems that admit them.
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::write(
            f.project().join(std::ffi::OsStr::from_bytes(b"bad\xff.md")),
            b"x",
        )
        .unwrap();
    }
    let _socket = std::os::unix::net::UnixListener::bind(f.project().join("socket.md")).unwrap();
    let n = f.node();
    let pages = scan(
        &n,
        "sg37_classify",
        "proj",
        ScanLimits {
            max_files: 20,
            max_file_bytes: 3,
        },
        vec!["skip/**".into()],
    )
    .await;
    let es = entries(&pages);
    assert_eq!(
        es.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["proj/a.md", "proj/big.md", "proj/link.md", "proj/sub/z.md"]
    );
    assert_eq!(
        es.iter().map(|e| e.too_big).collect::<Vec<_>>(),
        [false, true, false, false]
    );
    assert_eq!(
        es[0].version, es[2].version,
        "an inside link uses its target's real-path revision"
    );
    assert!(es.iter().all(|e| !e.is_dir && e.mtime.is_some()));
    let last = pages.last().unwrap();
    assert!(!last.complete);
    assert_eq!(
        last.issues,
        vec![
            ScanIssue::Unreadable { path: None },
            ScanIssue::Held {
                path: RelPath::new("proj/loop.md").unwrap(),
                why: HeldWhy::Unreadable
            },
            ScanIssue::Held {
                path: RelPath::new("proj/outside.md").unwrap(),
                why: HeldWhy::Outside
            }
        ]
    );
    let again = scan(
        &n,
        "sg37_repeat",
        "proj",
        ScanLimits {
            max_files: 20,
            max_file_bytes: 3,
        },
        vec!["skip/**".into()],
    )
    .await;
    assert_eq!(entries(&again), es);
    assert_eq!(again.last().unwrap().issues, last.issues);
    assert_eq!(again[0].manifest, pages[0].manifest);
    assert_eq!(std::fs::read(out).unwrap(), b"outside");
    n.shutdown().await;
}
#[tokio::test]
async fn sg37_scan_quota_keeps_oversized_files_and_reports_only_wanted_gaps() {
    let f = Fixture::new();
    f.put("a.md", b"a");
    f.put("big.md", b"large");
    f.put("sub/z.md", b"z");
    let n = f.node();
    for (max, want, over) in [
        (0, vec!["proj/big.md"], 2),
        (1, vec!["proj/a.md", "proj/big.md"], 1),
        (2, vec!["proj/a.md", "proj/big.md", "proj/sub/z.md"], 0),
    ] {
        let pages = scan(
            &n,
            &format!("sg37_quota_{max}"),
            "proj",
            ScanLimits {
                max_files: max,
                max_file_bytes: 3,
            },
            vec![],
        )
        .await;
        let es = entries(&pages);
        assert_eq!(es.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), want);
        let last = pages.last().unwrap();
        assert_eq!(last.complete, over == 0);
        assert_eq!(
            last.issues,
            if over == 0 {
                vec![]
            } else {
                vec![ScanIssue::OverLimit { count: over }]
            }
        );
    }
    let sub = scan(&n, "sg37_sub", "proj/sub", ScanLimits::default(), vec![]).await;
    assert_eq!(entries(&sub)[0].path.as_str(), "proj/sub/z.md");
    assert!(sub.last().unwrap().complete);
    f.put("bad\\name.txt", b"not wanted");
    let out = f.temp.path().join("outside");
    std::fs::create_dir(&out).unwrap();
    symlink(&out, f.project().join("directory-link")).unwrap();
    let normal = scan(&n, "sg37_unwanted", "proj", ScanLimits::default(), vec![]).await;
    assert!(normal.last().unwrap().complete);
    assert!(normal.last().unwrap().issues.is_empty());
    f.put("bad\\dir/x.md", b"hidden by an unnameable directory");
    let gap = scan(
        &n,
        "sg37_directory_gap",
        "proj",
        ScanLimits::default(),
        vec![],
    )
    .await;
    assert_eq!(
        gap.last().unwrap().issues,
        [ScanIssue::Unreadable { path: None }]
    );
    assert!(!gap.last().unwrap().complete);
    n.shutdown().await;
}
#[tokio::test]
async fn sg37_git_pipe_reads_stdin_fingerprints_and_reaps_the_journal() {
    let f = Fixture::new();
    f.repo();
    let base = f.git(&["rev-parse", "HEAD"]);
    f.git(&["checkout", "-qb", "feature"]);
    f.put("a.txt", &vec![b'x'; 2048]);
    f.git(&["add", "."]);
    f.git(&["commit", "-qm", "second"]);
    let tip = f.git(&["rev-parse", "HEAD"]);
    f.put("untracked.txt", b"new bytes");
    let index = std::fs::read(f.project().join(".git/index")).unwrap();
    let n = f.node();
    prepare(&n).await;
    let reads = vec![
        GitRead::Show {
            rev: tip.clone(),
            path: Some(RelPath::new("a.txt").unwrap()),
            max: 8,
        },
        GitRead::Show {
            rev: "missing-revision".into(),
            path: None,
            max: 8,
        },
        GitRead::Fingerprint,
        GitRead::IsAncestor {
            ancestor: base.clone(),
            of: tip.clone(),
        },
    ];
    let facts = n
        .git_facts(GitQuery {
            workspace: workspace(),
            refs: vec!["refs/heads/main".into()],
            ranges: vec![],
            fetch: false,
            reads: reads.clone(),
            heads_only: false,
        })
        .await
        .unwrap();
    assert_eq!(facts.head, tip);
    assert_eq!(facts.refs[0].1.as_deref(), Some(base.as_str()));
    assert!(facts.dirty);
    assert_eq!(facts.reads[0].read, reads[0]);
    assert!(
        matches!(&facts.reads[0].answer,GitAnswer::Blob{found:true,bytes,truncated:true} if bytes==b"xxxxxxxx")
    );
    assert!(matches!(
        &facts.reads[1].answer,
        GitAnswer::Blob { found: false, .. }
    ));
    assert!(matches!(
        &facts.reads[3].answer,
        GitAnswer::IsAncestor { yes: true }
    ));
    let GitAnswer::Fingerprint { tree, own } = &facts.reads[2].answer else {
        panic!("missing fingerprint")
    };
    assert_ne!(tree, &f.git(&["rev-parse", "HEAD^{tree}"]));
    assert_eq!(own.as_ref().unwrap().base, base);
    assert_ne!(own.as_ref().unwrap().patch, "-");
    assert_eq!(
        std::fs::read(f.project().join(".git/index")).unwrap(),
        index
    );
    assert_eq!(f.proc_count(), 0);
    n.shutdown().await;
    assert_eq!(f.proc_count(), 0);
}
#[tokio::test]
async fn sg37_git_pipe_worktree_success_and_bad_base_leave_no_process_evidence() {
    let f = Fixture::new();
    f.repo();
    std::fs::create_dir(f.root.join("trees")).unwrap();
    let n = f.node();
    for (op, base, ok) in [
        ("sg37_worktree", "main", true),
        ("sg37_bad_base", "missing-base", false),
    ] {
        n.prepare_workspace(command(
            op,
            Prepare {
                resource: op.into(),
                kind: PrepareKind::Worktree {
                    repository: at("proj"),
                    at: Some(at(&format!("trees/{op}"))),
                    branch: format!("fixture/{op}"),
                    base: Some(base.into()),
                },
            },
        ))
        .await
        .unwrap();
        let s = finished(&n, op).await;
        assert_eq!(succeeded(&s), ok, "{s:?}");
        assert_eq!(f.proc_count(), 0);
        assert_eq!(f.root.join(format!("trees/{op}")).exists(), ok);
    }
    n.shutdown().await;
}

#[test]
fn sg37_toolchain_placement_and_failure_table() {
    // Set startup configuration in a child process instead of changing another test's environment.
    if std::env::var_os("SG37_TOOLCHAIN_WORKER").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sg37_toolchain_placement_and_failure_table",
                "--nocapture",
            ])
            .env("SG37_TOOLCHAIN_WORKER", "1")
            .env("STEVE_TOOLCHAINS", "on")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async{
  for mode in ["cargo","mixed","no-marker","anonymous","cache-link","build-link","partial","keep-ignore"]{
   let f=Fixture::new();if mode!="no-marker"{f.put("Cargo.toml",b"");}if matches!(mode,"mixed"|"partial"){f.put("go.mod",b"");}
   let output=f.temp.path().join("tool-env.json");let adapter=f.temp.path().join("adapter.sh");let fake=Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
   std::fs::write(&adapter,format!("#!/bin/sh\npython3 -c 'import json,os,sys;json.dump({{k:os.environ.get(k) for k in [\"CARGO_TARGET_DIR\",\"GOCACHE\",\"GOMODCACHE\"]}},open(sys.argv[1],\"w\"))' '{}'\nexec node '{}' \"$@\"\n",output.display(),fake.display())).unwrap();use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&adapter,std::fs::Permissions::from_mode(0o755)).unwrap();let mut cfg=f.config();cfg.acp.adapters.insert("claude-code".into(),adapter);let n=steve_node::LocalNode::open(cfg).unwrap();n.connect(f.sink.clone(),None);prepare(&n).await;
   let outside=f.temp.path().join("elsewhere");std::fs::create_dir(&outside).unwrap();std::fs::write(outside.join("untouched"),b"original").unwrap();
   match mode{"cache-link"=>{std::fs::create_dir_all(f.home.join("cache/ws")).unwrap();symlink(&outside,f.home.join("cache/ws/user")).unwrap();},"build-link"=>{f.put(".steve/marker",b"");symlink(&outside,f.project().join(".steve/build")).unwrap();},"partial"=>f.put(".steve/build/cargo/target",b"blocking file"),"keep-ignore"=>f.put(".steve/build/.gitignore",b"keep\n"),_=>{}}
   let mut policy=steve_node_api::testkit::build::policy();policy.permissions=PermissionMode::FullAccess;
   n.open_session(command("sg37_open",OpenSession{session:SessionId::new("sg37_session").unwrap(),workspace:workspace(),choice:steve_node_api::testkit::build::choice(),policy,resume:None,env:Default::default(),state:if mode=="anonymous"{None}else{Some(HarnessState{dir:"ws/user/bot/claude-code".into(),import_owner:false,owner_context:false})},artifact_view:None,mcp_servers:vec![],skills:vec![]})).await.unwrap();let s=finished(&n,"sg37_open").await;assert!(succeeded(&s),"{mode}: {s:?}");let env:serde_json::Value=serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
   let cargo=f.project().join(".steve/build/cargo/target");let enabled=matches!(mode,"cargo"|"mixed"|"keep-ignore");assert_eq!(env["CARGO_TARGET_DIR"].as_str()==cargo.to_str(),enabled,"{mode}");assert_eq!(cargo.is_dir(),enabled,"{mode}");
   if matches!(mode,"mixed"|"partial"){assert_eq!(env["GOCACHE"],f.home.join("cache/ws/user/go/build").display().to_string());assert_eq!(env["GOMODCACHE"],f.home.join("cache/ws/user/go/mod").display().to_string());}
   if matches!(mode,"cargo"|"mixed"|"partial"|"keep-ignore"){assert_eq!(std::fs::read(f.project().join(".steve/build/.gitignore")).unwrap(),if mode=="keep-ignore"{b"keep\n".as_slice()}else{b"*\n".as_slice()});}
   assert_eq!(std::fs::read(outside.join("untouched")).unwrap(),b"original");assert_eq!(std::fs::read_dir(&outside).unwrap().count(),1);n.shutdown().await;
  }
 });
}

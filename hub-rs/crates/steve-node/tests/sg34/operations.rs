use super::fixture;

use fixture::{Env, finished, now_cmd, spec};
use steve_node_api::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg34_exec_preserves_output_caps_exit_code_environment_and_idempotence() {
    let e = Env::new();
    let n = e.node(false, None);
    let w = e.workspace(&n).await;
    let mut s = spec(
        &w,
        "printf '中abcdef'; printf 'error-long' >&2; printf x >> once; exit 7",
    );
    s.limits.max_output_bytes = 4;
    let command = now_cmd("sg34-output", s);
    n.exec(command.clone()).await.unwrap();
    let end = finished(&n, "sg34-output").await;
    match &end {
        Observed::Finished {
            outcome: Outcome::Succeeded,
            result: Some(OperationResult::Exec(r)),
            ..
        } => {
            assert_eq!(r.exit_code, Some(7));
            assert_eq!(r.stdout, "中a");
            assert_eq!(r.stderr, "erro");
            assert!(r.truncated);
        }
        other => panic!("{other:?}"),
    }
    n.exec(command).await.unwrap();
    assert_eq!(finished(&n, "sg34-output").await, end);
    assert_eq!(std::fs::read(e.work.join("once")).unwrap(), b"x");
    let mut s = spec(
        &w,
        "printf '%s|%s|%s' \"$SG34_VALUE\" \"${STEVE_UPGRADE_CONTROL_TOKEN-unset}\" \"${STEVE_SUPERVISOR_PID-unset}\"",
    );
    s.env = vec![
        ("SG34_VALUE".into(), "kept".into()),
        ("STEVE_UPGRADE_CONTROL_TOKEN".into(), "fixture".into()),
        ("STEVE_SUPERVISOR_PID".into(), "fixture".into()),
    ];
    n.exec(now_cmd("sg34-env", s)).await.unwrap();
    match finished(&n, "sg34-env").await {
        Observed::Finished {
            result: Some(OperationResult::Exec(r)),
            ..
        } => assert_eq!(r.stdout, "kept|unset|unset"),
        other => panic!("{other:?}"),
    }
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg34_exec_deadline_takes_blocked_work_and_releases_capacity() {
    let e = Env::new();
    let n = e.node(false, None);
    let w = e.workspace(&n).await;
    let mut s = spec(&w, "mkfifo blocked; cat blocked");
    s.limits.deadline = Timestamp(fixture::wall() + 500);
    n.exec(now_cmd("sg34-deadline", s)).await.unwrap();
    assert!(matches!(
        finished(&n, "sg34-deadline").await,
        Observed::Finished {
            outcome: Outcome::TimedOut,
            ..
        }
    ));
    n.exec(now_cmd("sg34-next", spec(&w, "printf next")))
        .await
        .unwrap();
    match finished(&n, "sg34-next").await {
        Observed::Finished {
            result: Some(OperationResult::Exec(r)),
            ..
        } => assert_eq!(r.stdout, "next"),
        other => panic!("{other:?}"),
    }
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg34_git_read_variants_keep_order_observed_revisions_and_bounds() {
    let e = Env::new();
    fixture::git(&e.work, &["init", "-q", "-b", "main"]);
    std::fs::write(e.work.join("a.txt"), "first\n").unwrap();
    fixture::git(&e.work, &["add", "."]);
    fixture::git(&e.work, &["commit", "-qm", "first"]);
    let base = fixture::git(&e.work, &["rev-parse", "HEAD"]);
    fixture::git(&e.work, &["checkout", "-qb", "topic"]);
    std::fs::write(e.work.join("a.txt"), "first\nsecond\n").unwrap();
    std::fs::write(e.work.join("b.txt"), "new\n").unwrap();
    fixture::git(&e.work, &["add", "."]);
    fixture::git(&e.work, &["commit", "-qm", "second"]);
    let head = fixture::git(&e.work, &["rev-parse", "HEAD"]);
    let bare = e.dir.path().join("bare");
    std::fs::create_dir(&bare).unwrap();
    fixture::git(&bare, &["init", "-q", "--bare"]);
    fixture::git(&e.work, &["remote", "add", "local", bare.to_str().unwrap()]);
    fixture::git(&e.work, &["tag", "-a", "v1", "-m", "tag"]);
    fixture::git(&e.work, &["push", "-q", "local", "topic", "--tags"]);
    fixture::git(
        &e.work,
        &["remote", "add", "origin", "ssh://git@example.test/o/r"],
    );
    std::fs::write(e.work.join("untracked"), "local").unwrap();
    let n = e.node(false, None);
    let w = e.workspace(&n).await;
    let range = RevRange {
        from: Some(base.clone()),
        to: head.clone(),
    };
    let reads = vec![
        GitRead::Show {
            rev: "missing".into(),
            path: None,
            max: 64,
        },
        GitRead::Show {
            rev: head.clone(),
            path: Some(RelPath::new("a.txt").unwrap()),
            max: 5,
        },
        GitRead::Show {
            rev: head.clone(),
            path: None,
            max: 256,
        },
        GitRead::Log {
            range: RevRange {
                from: None,
                to: head.clone(),
            },
            max: 1,
        },
        GitRead::Diff {
            range: range.clone(),
            max: 256,
        },
        GitRead::IsAncestor {
            ancestor: base.clone(),
            of: head.clone(),
        },
        GitRead::Status,
        GitRead::Fingerprint,
        GitRead::ChangedPaths {
            from: base.clone(),
            to: head.clone(),
            max: 1,
        },
        GitRead::Landed {
            commit: base.clone(),
            onto: head.clone(),
        },
        GitRead::RemoteTags {
            remote: "local".into(),
        },
        GitRead::RemoteUrl {
            remote: "origin".into(),
        },
    ];
    let f = n
        .git_facts(GitQuery {
            workspace: w.clone(),
            refs: vec![],
            ranges: vec![],
            fetch: false,
            reads: reads.clone(),
            heads_only: false,
        })
        .await
        .unwrap();
    assert_eq!(
        f.reads.iter().map(|r| r.read.clone()).collect::<Vec<_>>(),
        reads
    );
    assert_eq!(f.reads[0].observed, "");
    assert!(matches!(
        &f.reads[0].answer,
        GitAnswer::Blob { found: false, .. }
    ));
    assert_eq!(f.reads[1].observed, head);
    assert!(
        matches!(&f.reads[1].answer,GitAnswer::Blob {found:true,bytes,truncated:true} if bytes==b"first")
    );
    assert!(
        matches!(&f.reads[2].answer,GitAnswer::Commit {commit,..} if commit.sha==head && commit.subject=="second")
    );
    assert!(
        matches!(&f.reads[3].answer,GitAnswer::Log {commits,truncated:true} if commits.len()==1 && commits[0].sha==head)
    );
    assert!(
        matches!(&f.reads[4].answer,GitAnswer::Diff {stat,patch,..} if stat.contains("a.txt") && patch.contains("+second"))
    );
    assert!(matches!(
        &f.reads[5].answer,
        GitAnswer::IsAncestor { yes: true }
    ));
    assert!(
        matches!(&f.reads[6].answer,GitAnswer::Status {entries} if entries.iter().any(|x|x.path.as_str()=="untracked"))
    );
    assert!(
        matches!(&f.reads[7].answer,GitAnswer::Fingerprint {tree,own:Some(_)} if !tree.is_empty())
    );
    assert!(
        matches!(&f.reads[8].answer,GitAnswer::ChangedPaths {paths,truncated:true} if paths==&["a.txt"])
    );
    assert!(matches!(&f.reads[9].answer,GitAnswer::Landed {commit:Some(sha)} if sha==&base));
    assert!(
        matches!(&f.reads[10].answer,GitAnswer::RemoteTags {tags} if tags==&[("v1".into(),head.clone())])
    );
    assert!(
        matches!(&f.reads[11].answer,GitAnswer::RemoteUrl {url:Some(url)} if url=="ssh://example.test/o/r")
    );
    for read in [
        GitRead::Diff {
            range: RevRange {
                from: None,
                to: head,
            },
            max: 10,
        },
        GitRead::TrackedIgnored {
            rel: RelPath::new("a.txt").unwrap(),
        },
    ] {
        let error = n
            .git_facts(GitQuery {
                workspace: w.clone(),
                refs: vec![],
                ranges: vec![],
                fetch: false,
                reads: vec![read],
                heads_only: true,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
    }
    n.shutdown().await;
}

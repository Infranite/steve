//! What the merge queue asks of GitHub (E1, S6), against the fake: a batch's pull request opened
//! once, the checks on its commit, the merge with its head locked, closing it with its branch, and
//! a pull request handed to the repository's merge queue.

use std::path::Path;
use std::process::Command;

use steve_github::api::GhClient;
use steve_github::fake::{FakeGitHub, defaults};
use steve_github::mq::{self, Bearer, Checks};

const REPO: &str = defaults::REPO;

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// The fake with `main` and a batch branch one merge past it, pushed to its bare remote.
async fn setup() -> (FakeGitHub, tempfile::TempDir, Bearer, String, String) {
    let fake = FakeGitHub::start().await.unwrap();
    let bare = fake.bare_remote(REPO).unwrap();
    let work = tempfile::tempdir().unwrap();
    let w = work.path();
    git(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("a.txt"), "a\n").unwrap();
    git(w, &["add", "-A"]);
    git(w, &["commit", "-qm", "a"]);
    let base = git(w, &["rev-parse", "HEAD"]);
    git(w, &["push", "-q", &bare.display().to_string(), "main:main"]);
    git(w, &["checkout", "-qb", "steve/mq/main/MQ-1"]);
    std::fs::write(w.join("b.txt"), "b\n").unwrap();
    git(w, &["add", "-A"]);
    git(w, &["commit", "-qm", "Merge ST-1 into main"]);
    let tip = git(w, &["rev-parse", "HEAD"]);
    git(
        w,
        &[
            "push",
            "-q",
            &bare.display().to_string(),
            "steve/mq/main/MQ-1",
        ],
    );
    let r = Bearer {
        client: GhClient::new(reqwest::Client::new(), &fake.url()),
        auth: format!("Bearer {}", defaults::PAT),
    };
    let _ = base;
    (fake, work, r, tip, bare.display().to_string())
}

#[tokio::test]
async fn a_batch_pull_request_is_opened_once_checked_and_merged_with_its_head_locked() {
    let (fake, _w, r, tip, bare) = setup().await;
    let head = "steve/mq/main/MQ-1";
    let p = mq::open_pr(&r, REPO, head, "main", "Merge queue MQ-1", "- ST-1")
        .await
        .unwrap();
    let n = p["number"].as_u64().unwrap();
    let again = mq::open_pr(&r, REPO, head, "main", "Merge queue MQ-1", "- ST-1")
        .await
        .unwrap();
    assert_eq!(
        again["number"].as_u64(),
        Some(n),
        "found again, not a second one"
    );
    assert_eq!(fake.pulls_of(REPO, head), 1);

    // the checks on its commit
    assert_eq!(mq::checks(&r, REPO, &tip).await.unwrap(), Checks::None);
    fake.set_check(REPO, &tip, "ci", None);
    assert_eq!(
        mq::checks(&r, REPO, &tip).await.unwrap(),
        Checks::Pending { done: 0 }
    );
    fake.set_check(REPO, &tip, "ci", Some("success"));
    assert_eq!(mq::checks(&r, REPO, &tip).await.unwrap(), Checks::Passed);
    fake.set_check(REPO, &tip, "lint", Some("failure"));
    assert!(
        matches!(mq::checks(&r, REPO, &tip).await.unwrap(), Checks::Failed(f) if f.len() == 1 && f[0].starts_with("lint: failure"))
    );

    // merged only at its head
    let wrong = "0".repeat(40);
    let e = mq::merge(&r, REPO, n, &wrong, "t").await.unwrap_err();
    assert_eq!(e.status(), Some(409));
    mq::merge(&r, REPO, n, &tip, "t").await.unwrap();
    let after = mq::pull(&r, REPO, n).await.unwrap();
    assert_eq!(after["merged"], true);
    assert_eq!(git(Path::new(&bare), &["rev-parse", "main"]), tip);
    // merged again (a lost answer, asked again): refused, and the pull request says merged
    assert!(mq::merge(&r, REPO, n, &tip, "t").await.is_err());

    // closed with its branch
    mq::close(&r, REPO, n, head).await;
    assert!(!fake.branches(REPO).contains_key(head));
}

#[tokio::test]
async fn a_pull_request_goes_into_the_repositorys_merge_queue() {
    let (fake, _w, r, _tip, _) = setup().await;
    let p = mq::open_pr(&r, REPO, "steve/mq/main/MQ-1", "main", "t", "b")
        .await
        .unwrap();
    let id = p["node_id"].as_str().unwrap();
    mq::enqueue(&r, id).await.unwrap();
    assert_eq!(
        fake.with(|w| w.merge_queue.clone()),
        [(REPO.to_string(), p["number"].as_u64().unwrap())]
    );
    assert!(mq::enqueue(&r, "PR_nope").await.is_err());
}

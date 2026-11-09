//! The merge queue through the core (S4), against a Node doing its git for real: accepted items of
//! a project finished when merged go into their target's queue by themselves; the first asks once
//! whether Steve may merge into it; two of them go in one batch; one that conflicts goes back to its
//! agent; one that went back and was only rebased comes in again with no new acceptance.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use steve_store::mq as rows;

use super::mq_exec::{Fault, GitNode};
use crate::git::tests::{branch_with, g, repo};
use crate::repo::{Release, WS};
use crate::testkit::Kit;

struct Setup {
    k: Kit,
    repo: PathBuf,
    project: String,
    node: Arc<GitNode>,
}

/// A project whose completion rule is "merged", its `origin` a bare repository on this machine, and
/// the queue on with a Node staging it.
fn setup() -> Setup {
    let k = Kit::with(|c| c.mq = true);
    let r = repo(k.home());
    let bare = k.home().join("origin.git");
    g(
        k.home(),
        &[
            "clone",
            "-q",
            "--bare",
            &r.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    g(&r, &["remote", "add", "origin", &bare.to_string_lossy()]);
    g(&r, &["fetch", "-q", "origin"]);
    g(
        &r,
        &["branch", "-q", "--set-upstream-to=origin/main", "main"],
    );
    let rel = Release {
        kind: "script".into(),
        cmd: String::new(),
        done_when: Some("merge".into()),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "App",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            Some(&rel),
        )
        .unwrap()
        .id;
    let root = k.home().join("mq-node");
    std::fs::create_dir_all(&root).unwrap();
    let node = Arc::new(GitNode::over(&root, &r, &bare));
    k.core.set_mq_node(node.clone());
    Setup {
        k,
        repo: r,
        project: p,
        node,
    }
}

/// An item of the project with `file` = `text` on its own branch, delivered and accepted by you.
fn accepted(s: &Setup, file: &str, text: &str) -> (String, String, PathBuf) {
    let k = &s.k;
    let id = k.item("run", "交付与验收", Some(&s.project));
    let (branch, wt) = branch_with(&s.repo, &k.core.config().worktrees, &id, file, text);
    k.db()
        .item_patch(
            &id,
            crate::patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    let session = k.session(&id, "idle");
    let d = k.decision(&id, Some(&session), "accept", "item", &["pass", "rework"]);
    k.core
        .tx()
        .lc_request_complete(
            &id,
            "dev",
            crate::cycle::Policy::Human,
            false,
            "交付，等你验收",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    k.core.resolve(&d, "pass", "", "me").unwrap();
    (id, session, wt)
}

fn grant_card(k: &Kit) -> Option<crate::repo::Decision> {
    k.db()
        .decisions_open(WS)
        .unwrap()
        .into_iter()
        .find(|d| d.subject_type == crate::mq::drive::SUBJECT_TARGET)
}

fn entry(k: &Kit, item: &str) -> Option<rows::MqEntry> {
    k.db()
        .store()
        .reader(|c| rows::entry_last_of_item(c, item))
        .unwrap()
}

fn land_status(k: &Kit, item: &str) -> Option<String> {
    let i = k.db().item(item).unwrap().unwrap();
    k.db()
        .store()
        .items()
        .operations(item, i.cycle)
        .unwrap()
        .into_iter()
        .rfind(|o| o.kind == crate::mq::drive::OP_LAND)
        .map(|o| o.status)
}

fn on_main(s: &Setup, head: &str) -> bool {
    let bare = s.k.home().join("origin.git");
    crate::git::git(&bare, &["merge-base", "--is-ancestor", head, "main"]).is_ok()
}

fn head(dir: &Path) -> String {
    g(dir, &["rev-parse", "HEAD"])
}

#[test]
fn two_accepted_items_go_in_one_batch_and_the_one_that_conflicts_goes_back() {
    let s = setup();
    let k = &s.k;
    let (a, _, wa) = accepted(&s, "a.txt", "A\n");
    let (b, sb, wb) = accepted(&s, "a.txt", "B\n");
    // both in the queue, which asks once before it merges into the target
    assert!(
        k.wait(|| entry(k, &a).is_some() && entry(k, &b).is_some()),
        "{:?}",
        k.status(&a)
    );
    assert!(k.wait(|| grant_card(k).is_some()));
    assert_eq!(
        k.db()
            .decisions_open(WS)
            .unwrap()
            .iter()
            .filter(|d| d.subject_type == crate::mq::drive::SUBJECT_TARGET)
            .count(),
        1,
        "one card for the target"
    );
    assert_eq!(entry(k, &a).unwrap().state, "queued");
    k.core
        .resolve(&grant_card(k).unwrap().id, "yes", "", "me")
        .unwrap();
    // one batch: A lands, B conflicted with it and goes back
    assert!(
        k.wait(|| entry(k, &b).is_some_and(|e| e.state == "returned")),
        "{:?}",
        entry(k, &b)
    );
    assert!(
        k.wait(|| entry(k, &a).is_some_and(|e| e.state == "landed")),
        "{:?}",
        entry(k, &a)
    );
    let (ea, eb) = (entry(k, &a).unwrap(), entry(k, &b).unwrap());
    assert_eq!(ea.batch_no, Some(1));
    assert_eq!(eb.batch_no, Some(1));
    assert!(
        eb.reason.as_deref().is_some_and(|r| r.contains("a.txt")),
        "{:?}",
        eb.reason
    );
    assert!(on_main(&s, &head(&wa)));
    assert!(!on_main(&s, &head(&wb)));
    assert_eq!(s.node.pushes.load(std::sync::atomic::Ordering::SeqCst), 1);
    // A is merged: its completion rule holds, it closes
    assert!(
        k.wait(|| k.phase(&a) == "closed"),
        "{:?} {} {:?}",
        k.status(&a),
        k.core.lifecycle_view(&a).unwrap(),
        k.timeline_texts(&a)
    );
    assert_eq!(land_status(k, &a).as_deref(), Some("succeeded"));
    // B's agent heard why; B waits for it (its acceptance stands until its result changes)
    assert!(
        k.rt.prompts_of(&sb)
            .iter()
            .any(|p| p.contains("合并队列退回") && p.contains("a.txt"))
    );
    assert_eq!(land_status(k, &b).as_deref(), Some("failed"));
    let st = k.core.lane_status_of_item(&b).unwrap();
    assert_eq!(
        (st.path, st.state, st.next),
        (Some("queue"), "left", crate::lane::status::Next::Fix)
    );
    assert_eq!(k.phase(&b), "closing");
    // no card per batch, no release card per item
    let open = k.db().decisions_open(WS).unwrap();
    assert!(
        !open
            .iter()
            .any(|d| d.subject_type == "delivery_batch" || d.subject_type == "release"),
        "{open:?}"
    );
}

#[test]
fn a_returned_item_only_rebased_goes_in_again_without_a_new_acceptance() {
    let s = setup();
    let k = &s.k;
    // the target's own check, which gives no result the first time (the Node lost it)
    let t = k
        .db()
        .store()
        .transaction(|tx| {
            let target = crate::lane::target::target_of(&s.repo, None).unwrap();
            let t = rows::target_for(tx, &target.upstream, &target.branch)?;
            rows::target_policy_set(tx, &t.id, r#"{"checks": ["true"]}"#)?;
            rows::target_grant(tx, &t.id, "me")?;
            Ok::<_, steve_store::StoreError>(t)
        })
        .unwrap();
    *s.node.fault.lock().unwrap() = Fault::Killed;
    let (c, sc, wc) = accepted(&s, "c.txt", "C\n");
    let card = || {
        k.db()
            .decisions_open(WS)
            .unwrap()
            .into_iter()
            .find(|d| d.subject_type == crate::mq::drive::SUBJECT_BATCH)
    };
    assert!(
        k.wait(|| card().is_some()),
        "{:?} {:?}",
        entry(k, &c),
        k.status(&c)
    );
    assert!(grant_card(k).is_none(), "allowed already");
    // you send the batch back
    k.core
        .resolve(&card().unwrap().id, "return", "", "me")
        .unwrap();
    assert!(k.wait(|| entry(k, &c).is_some_and(|e| e.state == "returned")));
    let first = entry(k, &c).unwrap();
    assert!(k.wait(|| {
        k.rt.prompts_of(&sc)
            .iter()
            .any(|p| p.contains("合并队列退回"))
    }));
    // meanwhile the target moved; the agent only rebases (the same change) and ends its turn
    s.node.someone_pushes(7);
    *s.node.fault.lock().unwrap() = Fault::None;
    g(&wc, &["fetch", "-q", "origin"]);
    g(&wc, &["rebase", "-q", "origin/main"]);
    let frozen = crate::cycle::closing_of(&k.db().item(&c).unwrap().unwrap())
        .and_then(|c| c.fingerprint)
        .unwrap();
    let now = crate::cycle::fingerprint(Some(&wc.to_string_lossy())).unwrap();
    assert!(
        crate::cycle::same_result(&frozen, &now),
        "the same own change"
    );
    assert_ne!(head(&wc), first.head);
    let accepts = || {
        k.db()
            .decisions_of_item(&c)
            .unwrap()
            .into_iter()
            .filter(|d| d.kind == "accept")
            .count()
    };
    let before = accepts();
    k.rt.end_turn(&k.core, &sc, "end_turn", None);
    k.core.lifecycle_tick().unwrap();
    // in again with its new head, merged, closed: nobody was asked to accept it again
    assert!(
        k.wait(|| entry(k, &c).is_some_and(|e| e.id != first.id && e.state == "landed")),
        "{:?} {:?} {:?}",
        entry(k, &c),
        k.status(&c),
        k.timeline_texts(&c)
    );
    assert_eq!(entry(k, &c).unwrap().head, head(&wc));
    assert!(on_main(&s, &head(&wc)));
    assert!(k.wait(|| k.phase(&c) == "closed"), "{:?}", k.status(&c));
    assert_eq!(accepts(), before);
    assert!(
        k.timeline_texts(&c)
            .iter()
            .any(|l| l.contains("验收仍然有效")),
        "{:?}",
        k.timeline_texts(&c)
    );
    let _ = t;
}

/// The project releases by a tag cut by its own `ops/cut-release.py` (the Node runs it).
fn tagged_setup() -> Setup {
    let s = setup();
    let k = &s.k;
    std::fs::create_dir_all(s.repo.join("ops")).unwrap();
    std::fs::write(s.repo.join("ops/cut-release.py"), "# cut\n").unwrap();
    g(&s.repo, &["add", "-A"]);
    g(&s.repo, &["commit", "-q", "-m", "release script"]);
    g(&s.repo, &["push", "-q", "origin", "main"]);
    let rel = Release {
        kind: "script".into(),
        cmd: String::new(),
        done_when: Some("tag".into()),
    };
    k.db()
        .store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE projects SET release = ? WHERE id = ?",
                rusqlite::params![serde_json::to_string(&rel).unwrap(), s.project],
            )?;
            let target = crate::lane::target::target_of(&s.repo, None).unwrap();
            let t = rows::target_for(tx, &target.upstream, &target.branch)?;
            rows::target_policy_set(tx, &t.id, r#"{"release_max": 2}"#)?;
            rows::target_grant(tx, &t.id, "me")?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
    s
}

fn train_card(k: &Kit) -> Option<crate::repo::Decision> {
    k.db()
        .decisions_open(WS)
        .unwrap()
        .into_iter()
        .find(|d| d.subject_type == crate::mq::drive::SUBJECT_TRAIN)
}

#[test]
fn what_landed_is_released_by_one_train_with_one_card() {
    let s = tagged_setup();
    let k = &s.k;
    let (a, _, wa) = accepted(&s, "a.txt", "A\n");
    assert!(
        k.wait(|| entry(k, &a).is_some_and(|e| e.state == "landed")),
        "{:?} {:?}",
        entry(k, &a),
        k.timeline_texts(&a)
    );
    // merged, not released: it waits for its train (no card yet: one change, a window of 2 h)
    assert_eq!(k.phase(&a), "closing");
    assert!(train_card(k).is_none());
    let (b, _, wb) = accepted(&s, "b.txt", "B\n");
    assert!(k.wait(|| train_card(k).is_some()), "{:?}", entry(k, &b));
    let card = train_card(k).unwrap();
    assert!(
        card.draft.contains(&a) && card.draft.contains(&b),
        "{}",
        card.draft
    );
    assert_eq!(
        s.node.cuts.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing before you answer"
    );
    k.core.resolve(&card.id, "release", "", "me").unwrap();
    // one release for both, and both close by their rule (merged and tagged)
    assert!(
        k.wait(|| k.phase(&a) == "closed" && k.phase(&b) == "closed"),
        "{:?} {:?}",
        k.status(&a),
        k.timeline_texts(&a)
    );
    assert_eq!(s.node.cuts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        k.timeline_texts(&b)
            .iter()
            .any(|l| l.contains("已发布为 v0.1.0"))
    );
    assert_eq!(entry(k, &a).unwrap().state, "released");
    let _ = (wa, wb);
}

#[test]
fn a_release_whose_end_is_lost_is_found_on_the_target_and_not_cut_again() {
    let s = tagged_setup();
    let k = &s.k;
    *s.node.fault.lock().unwrap() = Fault::CutLost;
    let (a, _, _) = accepted(&s, "a.txt", "A\n");
    assert!(k.wait(|| entry(k, &a).is_some_and(|e| e.state == "landed")));
    // release now, without a card
    let t = entry(k, &a).unwrap().target_id;
    k.core.mq_release_now(&t).unwrap();
    assert!(
        k.wait(|| k.phase(&a) == "closed"),
        "{:?} {:?}",
        k.status(&a),
        k.timeline_texts(&a)
    );
    assert_eq!(
        s.node.cuts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "cut once"
    );
    assert!(
        train_card(k).is_none(),
        "no card: the tag says it was released"
    );
    // asked again (a restart's recovery): found released, not cut again
    k.core.mq_recover();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(s.node.cuts.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_refused_release_asks_and_its_changes_go_with_the_next_train_when_given_up() {
    let s = tagged_setup();
    let k = &s.k;
    *s.node.fault.lock().unwrap() = Fault::CutRefused;
    let (a, _, _) = accepted(&s, "a.txt", "A\n");
    assert!(k.wait(|| entry(k, &a).is_some_and(|e| e.state == "landed")));
    let t = entry(k, &a).unwrap().target_id;
    k.core.mq_release_now(&t).unwrap();
    assert!(k.wait(|| train_card(k).is_some_and(|d| d.kind == "fail")));
    let card = train_card(k).unwrap();
    assert!(card.draft.contains("gh run rerun"), "{}", card.draft);
    assert_eq!(card.options, ["retry", "drop"]);
    // retry once CI passes
    *s.node.fault.lock().unwrap() = Fault::None;
    k.core.resolve(&card.id, "retry", "", "me").unwrap();
    assert!(
        k.wait(|| k.phase(&a) == "closed"),
        "{:?}",
        k.timeline_texts(&a)
    );
    assert_eq!(s.node.cuts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// A host for a target that takes changes only through pull requests, over the Node's bare
/// upstream: pull requests are kept here, their checks scripted, a merge is a merge commit pushed
/// to the target.
struct FakeHost {
    node: Arc<GitNode>,
    upstream: PathBuf,
    work: PathBuf,
    prs: std::sync::Mutex<Vec<crate::mq::host::Pr>>,
    heads: std::sync::Mutex<Vec<(u64, String)>>,
    /// What the checks say, one per look (the last one stays).
    checks: std::sync::Mutex<Vec<crate::mq::host::Checks>>,
    merges: std::sync::atomic::AtomicUsize,
    closed: std::sync::Mutex<Vec<String>>,
}

impl FakeHost {
    fn new(s: &Setup) -> Arc<FakeHost> {
        let upstream = s.k.home().join("origin.git");
        let work = s.k.home().join("host-work");
        g(
            s.k.home(),
            &[
                "clone",
                "-q",
                &upstream.to_string_lossy(),
                &work.to_string_lossy(),
            ],
        );
        for (a, b) in [("user.name", "gh"), ("user.email", "gh@example.invalid")] {
            g(&work, &["config", a, b]);
        }
        Arc::new(FakeHost {
            node: s.node.clone(),
            upstream,
            work,
            prs: Default::default(),
            heads: Default::default(),
            checks: std::sync::Mutex::new(vec![crate::mq::host::Checks::None]),
            merges: Default::default(),
            closed: Default::default(),
        })
    }
}

#[async_trait::async_trait]
impl crate::mq::host::MqHost for FakeHost {
    async fn token(&self, _p: &str) -> Option<String> {
        None
    }
    async fn open_pr(
        &self,
        _p: &str,
        head: &str,
        _base: &str,
        _t: &str,
        _b: &str,
    ) -> crate::mq::exec::Step<crate::mq::host::Pr> {
        let mut prs = self.prs.lock().unwrap();
        let mut heads = self.heads.lock().unwrap();
        if let Some((n, _)) = heads.iter().find(|(_, h)| h == head) {
            let p = prs.iter().find(|p| p.number == *n).unwrap().clone();
            return crate::mq::exec::Step::Done(p);
        }
        let sha = g(&self.upstream, &["rev-parse", head]);
        let p = crate::mq::host::Pr {
            number: prs.len() as u64 + 1,
            state: "open".into(),
            merged: false,
            merge_commit: None,
            head_sha: sha,
            node_id: Some(format!("PR_{}", prs.len() + 1)),
        };
        heads.push((p.number, head.into()));
        prs.push(p.clone());
        crate::mq::exec::Step::Done(p)
    }
    async fn pr(&self, _p: &str, n: u64) -> crate::mq::exec::Step<crate::mq::host::Pr> {
        match self.prs.lock().unwrap().iter().find(|p| p.number == n) {
            Some(p) => crate::mq::exec::Step::Done(p.clone()),
            None => crate::mq::exec::Step::Failed("no such pr".into()),
        }
    }
    async fn checks(&self, _p: &str, _sha: &str) -> crate::mq::exec::Step<crate::mq::host::Checks> {
        let mut c = self.checks.lock().unwrap();
        let x = if c.len() > 1 {
            c.remove(0)
        } else {
            c[0].clone()
        };
        crate::mq::exec::Step::Done(x)
    }
    async fn merge(
        &self,
        _p: &str,
        n: u64,
        sha: &str,
        title: &str,
    ) -> crate::mq::exec::Step<String> {
        let mut prs = self.prs.lock().unwrap();
        let Some(p) = prs.iter_mut().find(|p| p.number == n) else {
            return crate::mq::exec::Step::Failed("no such pr".into());
        };
        if p.merged || p.head_sha != sha {
            return crate::mq::exec::Step::Failed("Pull Request is not mergeable".into());
        }
        g(&self.work, &["pull", "-q", "--ff-only", "origin", "main"]);
        g(&self.work, &["fetch", "-q", "origin", sha]);
        g(&self.work, &["merge", "-q", "--no-ff", "-m", title, sha]);
        g(&self.work, &["push", "-q", "origin", "main"]);
        let c = g(&self.work, &["rev-parse", "HEAD"]);
        p.merged = true;
        p.state = "closed".into();
        p.merge_commit = Some(c.clone());
        self.merges
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::mq::exec::Step::Done(c)
    }
    async fn close(&self, _p: &str, n: u64, branch: &str) {
        if let Some(p) = self.prs.lock().unwrap().iter_mut().find(|p| p.number == n)
            && !p.merged
        {
            p.state = "closed".into();
        }
        let _ = crate::git::git(&self.upstream, &["branch", "-D", branch]);
        self.closed.lock().unwrap().push(branch.into());
    }
    async fn enqueue(&self, _p: &str, _pr: &crate::mq::host::Pr) -> crate::mq::exec::Step<()> {
        crate::mq::exec::Step::Done(())
    }
}

/// The target takes changes through pull requests (as its host's rules were read).
fn through_prs(s: &Setup) -> Arc<FakeHost> {
    let h = FakeHost::new(s);
    s.k.core.set_mq_host(h.clone());
    s.k.db()
        .store()
        .transaction(|tx| {
            let target = crate::lane::target::target_of(&s.repo, None).unwrap();
            let t = rows::target_for(tx, &target.upstream, &target.branch)?;
            rows::target_grant(tx, &t.id, "me")?;
            rows::target_policy_set(tx, &t.id, r#"{"host_poll_s": 1}"#)?;
            rows::target_mode_set(
                tx,
                &t.id,
                Some("host_pr"),
                &serde_json::json!({"at": steve_store::now()}).to_string(),
            )
        })
        .unwrap();
    let _ = &h.node;
    h
}

#[test]
fn through_a_pull_request_the_batch_merges_once_the_hosts_checks_pass() {
    let s = setup();
    let k = &s.k;
    let h = through_prs(&s);
    *h.checks.lock().unwrap() = vec![
        crate::mq::host::Checks::Pending { done: 0 },
        crate::mq::host::Checks::Passed,
    ];
    let (a, _, wa) = accepted(&s, "a.txt", "A\n");
    assert!(
        k.wait(|| entry(k, &a).is_some_and(|e| e.state == "landed")),
        "{:?} {:?}",
        entry(k, &a),
        k.timeline_texts(&a)
    );
    // one pull request for the batch, merged with a merge commit: A's commit is in the target
    assert_eq!(h.prs.lock().unwrap().len(), 1);
    assert_eq!(h.merges.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(on_main(&s, &head(&wa)));
    assert_eq!(h.closed.lock().unwrap().as_slice(), ["steve/mq/main/MQ-1"]);
    assert_eq!(
        s.node.pushes.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "never pushed to main directly"
    );
    assert!(
        k.wait(|| k.phase(&a) == "closed"),
        "{:?}",
        k.timeline_texts(&a)
    );
}

#[test]
fn through_a_pull_request_failing_host_checks_send_the_change_back() {
    let s = setup();
    let k = &s.k;
    let h = through_prs(&s);
    *h.checks.lock().unwrap() = vec![crate::mq::host::Checks::Failed(
        "ci: failure https://ci/1".into(),
    )];
    let (a, sa, wa) = accepted(&s, "a.txt", "A\n");
    assert!(
        k.wait(|| entry(k, &a).is_some_and(|e| e.state == "returned")),
        "{:?}",
        entry(k, &a)
    );
    assert!(
        entry(k, &a)
            .unwrap()
            .reason
            .unwrap()
            .contains("ci: failure")
    );
    assert!(!on_main(&s, &head(&wa)));
    assert_eq!(h.merges.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        h.closed.lock().unwrap().len(),
        1,
        "the pull request and its branch go"
    );
    assert!(k.wait(|| {
        k.rt.prompts_of(&sa)
            .iter()
            .any(|p| p.contains("ci: failure"))
    }));
}

#[test]
fn a_host_with_its_own_merge_queue_gets_each_change_and_says_what_merged() {
    let s = setup();
    let k = &s.k;
    let h = through_prs(&s);
    k.db()
        .store()
        .transaction(|tx| {
            let target = crate::lane::target::target_of(&s.repo, None).unwrap();
            let t = rows::target_for(tx, &target.upstream, &target.branch)?;
            rows::target_mode_set(
                tx,
                &t.id,
                Some("host_queue"),
                &serde_json::json!({"at": steve_store::now()}).to_string(),
            )
        })
        .unwrap();
    let (a, _, wa) = accepted(&s, "a.txt", "A\n");
    // handed over: its own pull request, enqueued; Steve batches nothing
    assert!(
        k.wait(|| entry(k, &a).is_some_and(|e| e.pr.is_some())),
        "{:?} {:?}",
        entry(k, &a),
        k.timeline_texts(&a)
    );
    let n = entry(k, &a).unwrap().pr.unwrap() as u64;
    assert_eq!(entry(k, &a).unwrap().state, "batched");
    assert!(
        k.db()
            .store()
            .reader(|c| rows::batches_live(c, &entry(k, &a).unwrap().target_id))
            .unwrap()
            .is_empty()
    );
    // the host's queue merges it; the next look says so and the item closes by its rule
    let sha = h.prs.lock().unwrap()[0].head_sha.clone();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(crate::mq::host::MqHost::merge(
            &*h,
            "p",
            n,
            &sha,
            "merged by the queue",
        ));
    k.core.mq_tick();
    assert!(
        k.wait(|| entry(k, &a).is_some_and(|e| e.state == "landed")),
        "{:?}",
        entry(k, &a)
    );
    assert!(on_main(&s, &head(&wa)));
    assert!(
        k.wait(|| k.phase(&a) == "closed"),
        "{:?}",
        k.timeline_texts(&a)
    );
    assert_eq!(
        s.node.pushes.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing pushed to main by Steve"
    );
}

#[test]
fn the_project_page_shows_the_queue_its_statistics_and_sets_its_policy() {
    let s = setup();
    let k = &s.k;
    let target = crate::lane::target::target_of(&s.repo, None).unwrap();
    let view = |k: &Kit| {
        k.core
            .mq_view(&target.upstream, &target.branch, WS)
            .unwrap()
            .unwrap()
    };
    // before anything went in: the defaults, no target yet
    let v = view(k);
    assert!(v["target"].is_null());
    assert_eq!(v["policy"]["max_batch"], 8);
    // allowed from the page, then a change merges
    let v = k
        .core
        .mq_act(
            &target.upstream,
            &target.branch,
            WS,
            &serde_json::json!({"allow": true}),
            "me",
        )
        .unwrap()
        .unwrap();
    assert_eq!(v["target"]["granted"], true);
    let (a, _, _) = accepted(&s, "a.txt", "A\n");
    assert!(k.wait(|| entry(k, &a).is_some_and(|e| e.state == "landed")));
    let v = view(k);
    assert_eq!(v["batches"][0]["members"][0]["label"], a.as_str());
    assert_eq!(v["batches"][0]["state"], "landed");
    assert_eq!(v["stats"]["batches"], 1);
    assert_eq!(v["stats"]["landed"], 1);
    assert!(
        v["stats"]["avg_wait_to_land_ms"].is_number(),
        "{}",
        v["stats"]
    );
    // every number is set per target; a field it does not have, or a bad value, is refused
    let v = k
        .core
        .mq_act(
            &target.upstream,
            &target.branch,
            WS,
            &serde_json::json!({"policy": {"max_batch": 3, "release_window_s": 600}}),
            "me",
        )
        .unwrap()
        .unwrap();
    assert_eq!(v["policy"]["max_batch"], 3);
    assert_eq!(
        v["set"],
        serde_json::json!({"max_batch": 3, "release_window_s": 600})
    );
    let v = k
        .core
        .mq_act(
            &target.upstream,
            &target.branch,
            WS,
            &serde_json::json!({"policy": {"max_batch": null}}),
            "me",
        )
        .unwrap()
        .unwrap();
    assert_eq!(v["policy"]["max_batch"], 8, "back to the default");
    for bad in [
        serde_json::json!({"policy": {"nope": 1}}),
        serde_json::json!({"policy": {"max_batch": -1}}),
    ] {
        assert!(
            k.core
                .mq_act(&target.upstream, &target.branch, WS, &bad, "me")
                .is_err(),
            "{bad}"
        );
    }
    // another workspace sees the queue but not whose changes are in it
    let other = k
        .core
        .mq_view(&target.upstream, &target.branch, "other")
        .unwrap()
        .unwrap();
    assert_eq!(other["batches"][0]["others"], 1);
    assert!(
        other["batches"][0]["members"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

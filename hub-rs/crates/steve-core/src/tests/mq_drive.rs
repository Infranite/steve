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

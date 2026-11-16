//! An accepted change lands through its branch's pull request (crate::pull): Steve turns on the
//! host's auto-merge, the item closes once it merged, and a conflict, a failed check, a missing pull
//! request or one closed without merging comes back to the agent or to you.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::takeover::{Remote, merge_and_tag, remote_for, told_what_is_missing};
use crate::git::tests::g;
use crate::testkit::Kit;
use crate::upstream;

/// A `gh` that answers `pr list` and `repo view` with what `pr.json` and `repo.json` hold, takes
/// every `pr merge`, and writes each call to `calls`.
struct Gh {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    script: PathBuf,
}

impl Gh {
    fn new() -> Gh {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let script = d.join("gh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nd='{}'\necho \"$@\" >> \"$d/calls\"\ncase \"$1 $2\" in\n  'pr list') cat \"$d/pr.json\" ;;\n  'repo view') cat \"$d/repo.json\" ;;\n  'pr merge') exit 0 ;;\n  *) exit 1 ;;\nesac\n",
                d.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        std::fs::write(d.join("pr.json"), "[]").unwrap();
        std::fs::write(
            d.join("repo.json"),
            json!({"autoMergeAllowed": true, "rebaseMergeAllowed": true}).to_string(),
        )
        .unwrap();
        Gh {
            _dir: dir,
            dir: d,
            script,
        }
    }
    fn prs(&self, v: Value) {
        std::fs::write(self.dir.join("pr.json"), v.to_string()).unwrap();
    }
    fn repo(&self, v: Value) {
        std::fs::write(self.dir.join("repo.json"), v.to_string()).unwrap();
    }
    fn merges(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("pr merge"))
            .map(String::from)
            .collect()
    }
}

/// The open pull request of the item's branch at `head`, with `extra` fields.
fn open_pr(head: &str, extra: Value) -> Value {
    let mut p = json!({"number": 7, "state": "OPEN", "url": "https://github.com/acme/app/pull/7",
        "headRefOid": head, "isDraft": false, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
        "autoMergeRequest": null, "statusCheckRollup": []});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    json!([p])
}

fn head(wt: &Path) -> String {
    g(wt, &["rev-parse", "HEAD"])
}

/// An accepted item in a project done when merged, on GitHub, followed by `gh`.
fn accepted(gh: &Gh) -> (Kit, Remote) {
    let script = gh.script.clone();
    let k = Kit::with(|c| {
        c.takeover_continues = 2;
        c.gh = script.clone();
    });
    let x = remote_for(&k, upstream::DONE_MERGE, true, None);
    told_what_is_missing(&k, &x);
    // the agent ends the turn it was told in
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
    (k, x)
}

/// One look at the pull requests, whatever the interval.
fn look(k: &Kit) {
    k.core.tx().st.pr_checked.clear();
    k.core.check_prs().unwrap();
}

fn prompts_with(k: &Kit, x: &Remote, words: &str) -> usize {
    k.rt.prompts_of(&x.session)
        .iter()
        .filter(|p| p.contains(words))
        .count()
}

#[test]
fn an_accepted_pull_request_gets_auto_merge_and_the_item_closes_once_it_merged() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let p =
        k.rt.prompts_of(&x.session)
            .into_iter()
            .rfind(|p| p.contains("按这个项目的约定"))
            .unwrap();
    assert!(p.contains("自动合并"), "{p}");
    assert!(p.contains("gh pr create"), "{p}");
    assert_eq!(k.phase(&x.item), "closing");
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --rebase --match-head-commit {h} --auto"
        )]
    );
    assert!(
        k.timeline_texts(&x.item)
            .iter()
            .any(|l| l.starts_with("已为 PR #7 打开自动合并"))
    );
    // asked once for this head: auto-merge on, nothing asked again
    gh.prs(open_pr(
        &h,
        json!({"autoMergeRequest": {"mergeMethod": "REBASE"}}),
    ));
    look(&k);
    assert_eq!(gh.merges().len(), 1);
    assert_eq!(k.phase(&x.item), "closing");
    // GitHub merged it (a rebase merge: copies of the branch's commits on main)
    merge_and_tag(&x, None);
    gh.prs(json!([{"number": 7, "state": "MERGED", "url": "https://github.com/acme/app/pull/7", "headRefOid": h}]));
    look(&k);
    assert!(
        k.wait(|| k.phase(&x.item) == "closed"),
        "{:?}",
        k.status(&x.item)
    );
    let t = k.timeline_texts(&x.item);
    assert!(t.iter().any(|l| l.starts_with("PR #7 已合并")), "{t:?}");
    assert!(
        t.iter()
            .any(|l| l.starts_with("Steve 核对：提交已合并到 origin/main")),
        "{t:?}"
    );
    assert!(k.open_fails(&x.item).is_empty());
}

#[test]
fn without_auto_merge_it_merges_once_every_check_passed() {
    let gh = Gh::new();
    gh.repo(json!({"autoMergeAllowed": false, "squashMergeAllowed": true}));
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    let running = json!({"statusCheckRollup": [{"__typename": "CheckRun", "name": "rust", "status": "IN_PROGRESS", "conclusion": ""}]});
    gh.prs(open_pr(&h, running));
    look(&k);
    assert!(gh.merges().is_empty(), "checks still run");
    let passed = json!({"statusCheckRollup": [{"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": "SUCCESS"}]});
    gh.prs(open_pr(&h, passed));
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --squash --match-head-commit {h}"
        )]
    );
    assert!(
        k.timeline_texts(&x.item)
            .iter()
            .any(|l| l.contains("Steve 已合并它"))
    );
}

#[test]
fn a_conflict_or_a_failed_check_goes_back_to_the_agent_once_per_head() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(
        &h,
        json!({"mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}),
    ));
    look(&k);
    look(&k);
    assert_eq!(prompts_with(&k, &x, "和默认分支有冲突"), 1);
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(p.contains("git push --force-with-lease"), "{p}");
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
    // rebased onto main (the same change): a new head, and a failed check on it
    // a new message, not only a new date: an amend within the same second would keep the hash
    g(
        &x.wt,
        &[
            "commit",
            "-q",
            "--amend",
            "-m",
            "the same change, rebased onto main",
        ],
    );
    let h2 = head(&x.wt);
    assert_ne!(h, h2);
    let failed = json!({"statusCheckRollup": [
        {"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": "FAILURE"},
        {"__typename": "CheckRun", "name": "web", "status": "COMPLETED", "conclusion": "SUCCESS"}]});
    gh.prs(open_pr(&h2, failed));
    look(&k);
    look(&k);
    assert_eq!(prompts_with(&k, &x, "的检查没通过：rust"), 1);
    assert_eq!(
        k.phase(&x.item),
        "closing",
        "the same change: still accepted"
    );
    assert!(k.open_fails(&x.item).is_empty());
}

#[test]
fn a_branch_without_a_pull_request_is_asked_to_open_one() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    look(&k);
    look(&k);
    assert_eq!(prompts_with(&k, &x, "还没有 PR"), 1);
    assert!(gh.merges().is_empty());
}

#[test]
fn a_pull_request_closed_without_merging_is_a_card_for_you() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    gh.prs(json!([{"number": 7, "state": "CLOSED", "url": "https://github.com/acme/app/pull/7", "headRefOid": head(&x.wt)}]));
    look(&k);
    look(&k);
    let fails = k.open_fails(&x.item);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(fails[0].contains("PR #7 被关闭了"), "{fails:?}");
    assert!(gh.merges().is_empty());
}

#[test]
fn publishing_in_a_pull_request_project_is_refused_with_how_it_lands() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let s = k.db().session(&x.session).unwrap().unwrap();
    let branch = k.db().item(&x.item).unwrap().unwrap().branch.unwrap();
    let e = k.core.tool_publish(&s, &branch, "说明").unwrap_err();
    assert!(e.to_string().contains("不用 steve_publish"), "{e}");
}

/* ── a branch with nothing of its own (#218) ── */

/// An item in a project done when merged, on GitHub, whose branch has no commit of its own.
fn without_commits(gh: &Gh) -> (Kit, Remote) {
    let script = gh.script.clone();
    let k = Kit::with(|c| {
        c.takeover_continues = 2;
        c.gh = script.clone();
    });
    let x = remote_for(&k, upstream::DONE_MERGE, false, None);
    g(&x.wt, &["reset", "-q", "--hard", "main"]);
    (k, x)
}

#[test]
fn without_a_commit_of_its_own_only_a_merged_pull_request_makes_it_merged() {
    let gh = Gh::new();
    let (k, x) = without_commits(&gh);
    let h = head(&x.wt);
    gh.prs(json!([{"number": 9, "state": "MERGED", "url": "https://github.com/acme/app/pull/9", "headRefOid": h}]));
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert!(k.wait(|| {
        look(&k);
        k.phase(&x.item) == "closed"
    }));
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter()
            .any(|l| l.starts_with("Steve 核对：提交已合并到 origin/main")),
        "{t:?}"
    );
    assert!(gh.merges().is_empty());
}

#[test]
fn without_a_commit_of_its_own_an_open_pull_request_is_not_merged_and_not_auto_merged() {
    let gh = Gh::new();
    let (k, x) = without_commits(&gh);
    gh.prs(open_pr(&head(&x.wt), json!({})));
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert!(k.wait(|| {
        look(&k);
        k.phase(&x.item) == "closed"
    }));
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter().any(|l| l.starts_with("事项分支上没有自己的提交")),
        "{t:?}"
    );
    assert!(!t.iter().any(|l| l.contains("提交已合并到")), "{t:?}");
    assert!(gh.merges().is_empty(), "nothing of its own to merge");
}

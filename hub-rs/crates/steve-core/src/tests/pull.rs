//! An accepted change lands through its branch's pull request (crate::pull): Steve merges it itself
//! at the accepted head once every check passed, only while that result still stands, and only a
//! change an independent review passed (else it waits for your yes); the item closes once it merged,
//! and a conflict, a failed check, a missing pull request, one closed without merging, a merge that
//! keeps failing or a host that can't be asked comes back to the agent or to you.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::takeover::{Remote, merge_and_tag, remote_for, told_what_is_missing};
use crate::cycle::OP_RELEASE;
use crate::git::tests::g;
use crate::pull::{MERGE_FAILS_CARD, PR_AWAITING_MERGE, PR_TELLS_MAX, UNKNOWN_LOOKS_CARD};
use crate::repo::PlanStep;
use crate::testkit::Kit;
use crate::upstream;

/// A `gh` that answers `pr list` with what `pr.json` holds, `pr view` with `view.json` (fails without
/// it, as for a pull request it can't find), `api repos/…/pulls/…` with `pull.json` (by default the
/// host has not worked out whether it can rebase) and `api repos/…` with `repo.json` (by default what
/// the real `gh api` answered for a repository the account may write to), takes every `pr merge`
/// unless `merge.fail` holds why not (a rebase merge also unless `rebase.fail` does), fails `pr list`
/// while `list.fail` exists, and writes each call to `calls`.
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
                "#!/bin/sh\nd='{}'\necho \"$@\" >> \"$d/calls\"\ncase \"$1 $2\" in\n  'pr list') if [ -f \"$d/list.fail\" ]; then echo 'error connecting to api.github.com' >&2; exit 1; fi; cat \"$d/pr.json\" ;;\n  'pr view') if [ -f \"$d/view.json\" ]; then cat \"$d/view.json\"; else echo 'no pull requests found' >&2; exit 1; fi ;;\n  'api repos/'*'/pulls/'*) if [ -f \"$d/pull.json\" ]; then cat \"$d/pull.json\"; else echo '{{\"rebaseable\": null}}'; fi ;;\n  'api repos/'*) cat \"$d/repo.json\" ;;\n  'pr merge') case \"$*\" in *--rebase*) if [ -f \"$d/rebase.fail\" ]; then cat \"$d/rebase.fail\" >&2; exit 1; fi ;; esac; if [ -f \"$d/merge.fail\" ]; then cat \"$d/merge.fail\" >&2; exit 1; fi ;;\n  *) exit 1 ;;\nesac\n",
                d.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        std::fs::write(d.join("pr.json"), "[]").unwrap();
        std::fs::write(
            d.join("repo.json"),
            include_str!("fixtures/gh-api-repo.json"),
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
    /// What `gh pr view` answers for the pull request a result is followed on.
    fn view(&self, v: Value) {
        std::fs::write(self.dir.join("view.json"), v.to_string()).unwrap();
    }
    /// What `gh api repos/…/pulls/…` answers (`rebaseable`).
    fn pull(&self, v: Value) {
        std::fs::write(self.dir.join("pull.json"), v.to_string()).unwrap();
    }
    /// The repository as `gh api repos/…` answers it, with `allow` changed.
    fn repo(&self, allow: Value) {
        let mut r: Value = serde_json::from_str(include_str!("fixtures/gh-api-repo.json")).unwrap();
        for (k, v) in allow.as_object().unwrap() {
            r[k] = v.clone();
        }
        std::fs::write(self.dir.join("repo.json"), r.to_string()).unwrap();
    }
    /// A rebase merge is refused with `why` (every other method is taken).
    fn rebase_fails(&self, why: &str) {
        std::fs::write(self.dir.join("rebase.fail"), why).unwrap();
    }
    fn merge_fails(&self, why: Option<&str>) {
        let f = self.dir.join("merge.fail");
        match why {
            Some(w) => std::fs::write(f, w).unwrap(),
            None => {
                let _ = std::fs::remove_file(f);
            }
        }
    }
    fn offline(&self, off: bool) {
        let f = self.dir.join("list.fail");
        if off {
            std::fs::write(f, "").unwrap();
        } else {
            let _ = std::fs::remove_file(f);
        }
    }
    fn calls(&self, prefix: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with(prefix))
            .map(String::from)
            .collect()
    }
    fn merges(&self) -> Vec<String> {
        self.calls("pr merge")
    }
}

/// The open pull request of the item's branch at `head`, with `extra` fields.
fn open_pr(head: &str, extra: Value) -> Value {
    let mut p = json!({"number": 7, "state": "OPEN", "url": "https://github.com/acme/app/pull/7",
        "headRefOid": head, "isDraft": false, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
        "autoMergeRequest": null, "statusCheckRollup": [
            {"__typename": "CheckRun", "name": "rust", "status": "COMPLETED", "conclusion": "SUCCESS"}]});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    json!([p])
}

fn head(wt: &Path) -> String {
    g(wt, &["rev-parse", "HEAD"])
}

/// An accepted item in a project done when merged, on GitHub, followed by `gh`, its result frozen
/// with the worktree it is in (as a delivery from its worktree freezes it).
fn accepted(gh: &Gh) -> (Kit, Remote) {
    let (k, x) = accepted_unfrozen(gh);
    freeze(&k, &x);
    (k, x)
}

/// The accepted result is what the worktree holds now (its fingerprint frozen with it).
fn freeze(k: &Kit, x: &Remote) {
    let print = crate::cycle::fingerprint(Some(&x.wt.to_string_lossy())).unwrap();
    steve_store::core_fixture::work::closing_fingerprint(k.db().store(), &x.item, Some(&print));
}

/// The result is void (a new message) and the same change is delivered and accepted again.
fn accepted_again(k: &Kit, x: &Remote) {
    k.core
        .tx()
        .lc_invalidate(&x.item, crate::cycle::void::MESSAGE, "收到新的消息")
        .unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_deliver(
            &s,
            "再次交付",
            &[],
            &[crate::Check {
                name: "tests".into(),
                passed: true,
                operated: false,
                scope: None,
            }],
            &[],
        )
        .unwrap();
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    if let Some(a) = k
        .db()
        .decisions_of_item(&x.item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "accept")
    {
        k.core.resolve(&a.id, "pass", "", "me").unwrap();
    }
    // the new result's release asked for what is missing (or followed the pull request on)
    steve_runtime::testkit::eventually_blocking(
        "Acceptance creates the release operation for the current item revision.",
        || {
            let i = k.db().item(&x.item).unwrap().unwrap();
            k.db()
                .store()
                .items()
                .operations(&x.item, i.cycle)
                .unwrap()
                .iter()
                .any(|o| {
                    o.kind == OP_RELEASE
                        && o.revision == i.revision
                        && o.evidence.get("asked").is_some()
                })
        },
    );
    steve_runtime::testkit::eventually_blocking("All completion probes have finished.", || {
        k.core.tx().st.probing.is_empty()
    });
}

/// [`accepted`] with no fingerprint frozen with the result (`remote_for` gives the item its worktree
/// after the delivery).
fn accepted_unfrozen(gh: &Gh) -> (Kit, Remote) {
    let script = gh.script.clone();
    let k = Kit::with(|c| {
        c.takeover_continues = 2;
        c.gh = script.clone();
    });
    let x = remote_for(&k, upstream::DONE_MERGE, true, None);
    told_what_is_missing(&k, &x);
    // the agent ends the turn it was told in
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    steve_runtime::testkit::eventually_blocking("All completion probes have finished.", || {
        k.core.tx().st.probing.is_empty()
    });
    (k, x)
}

/// [`accepted`] for a change an independent review passed: the item's work graph, as accepted, has
/// its required review done.
fn reviewed(gh: &Gh) -> (Kit, Remote) {
    let (k, x) = accepted(gh);
    let i = k.db().item(&x.item).unwrap().unwrap();
    let goal = k
        .db()
        .goals(&x.item)
        .unwrap()
        .iter()
        .map(|g| g.version)
        .max()
        .unwrap_or(0);
    let spec = |id: &str, bot: &str, kind: &str, deps: &[&str]| {
        json!({"id": id, "title": id, "goal": id, "bot_id": bot, "kind": kind,
            "depends_on": deps, "resources": [], "required": true})
        .to_string()
    };
    steve_store::core_fixture::work::reviewed_workflow(
        k.db().store(),
        &x.item,
        i.cycle,
        goal,
        &[
            ("work", spec("work", "dev", "work", &[])),
            ("review", spec("review", "steve", "review", &["work"])),
            (
                "integrate",
                spec("integrate", "dev", "integrate", &["review"]),
            ),
        ],
    );
    assert!(
        k.core
            .tx()
            .pr_reviewed(&k.db().item(&x.item).unwrap().unwrap())
            .unwrap()
    );
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

/// The open "PR awaiting merge" cards of the item.
fn merge_cards(k: &Kit, x: &Remote) -> Vec<String> {
    k.db()
        .decisions_of_item(&x.item)
        .unwrap()
        .into_iter()
        .filter(|d| d.status == "open" && d.subject_type == PR_AWAITING_MERGE)
        .map(|d| d.id)
        .collect()
}

/// `(to_v, detail)` of the item's `pr` marks, oldest first.
fn pr_marks(k: &Kit, x: &Remote) -> Vec<(String, Value)> {
    steve_store::core_fixture::work::item_marks(k.db().store(), &x.item, Some("pr"))
        .into_iter()
        .map(|m| (m.to.unwrap(), m.detail.unwrap()))
        .collect()
}

/// The `merge` marks of the item (the workflow statistics' accepted → merged time): `(to_v, ref)`.
fn merge_marks(k: &Kit, x: &Remote) -> Vec<(String, String)> {
    steve_store::core_fixture::work::item_marks(k.db().store(), &x.item, Some("merge"))
        .into_iter()
        .map(|m| (m.to.unwrap(), m.reference.unwrap()))
        .collect()
}

/// The release operation the pull request is followed on.
fn release_op(k: &Kit, x: &Remote) -> steve_store::ItemOperation {
    let i = k.db().item(&x.item).unwrap().unwrap();
    k.db()
        .store()
        .items()
        .operations(&x.item, i.cycle)
        .unwrap()
        .into_iter()
        .rfind(|o| o.kind == OP_RELEASE && o.revision == i.revision)
        .unwrap()
}

fn step(s: &str, done: bool) -> PlanStep {
    PlanStep {
        step: s.into(),
        done,
        left: false,
    }
}

#[test]
fn a_reviewed_change_is_merged_by_steve_at_the_accepted_head_once_every_check_passed() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    let p =
        k.rt.prompts_of(&x.session)
            .into_iter()
            .rfind(|p| p.contains("按这个项目的约定"))
            .unwrap();
    assert!(p.contains("由 Steve 按验收的版本合并"), "{p}");
    assert!(!p.contains("自动合并"), "{p}");
    assert!(p.contains("steve_github_open_pr"), "{p}");
    assert_eq!(k.phase(&x.item), "closing");
    let h = head(&x.wt);
    let running = json!({"statusCheckRollup": [{"__typename": "CheckRun", "name": "rust", "status": "IN_PROGRESS", "conclusion": ""}]});
    gh.prs(open_pr(&h, running));
    look(&k);
    assert!(gh.merges().is_empty(), "checks still run");
    assert!(
        merge_cards(&k, &x).is_empty(),
        "reviewed: nothing to ask you"
    );
    // GitHub merges it as asked (a rebase merge: copies of the branch's commits on main)
    merge_and_tag(&x, None);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --rebase --match-head-commit {h}"
        )]
    );
    // the methods, then whether the branch can be rebased (the first one allowed)
    assert_eq!(
        gh.calls("api"),
        ["api repos/acme/app", "api repos/acme/app/pulls/7"]
    );
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The reviewed item reaches closed after Steve merges the accepted PR head; {:?}",
            k.status(&x.item)
        )
    });
    steve_runtime::testkit::eventually_blocking(diagnostic, || k.phase(&x.item) == "closed");
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter().any(|l| l.contains("Steve 已按验收的版本")),
        "{t:?}"
    );
    // Steve merged it at the accepted head: that is the evidence, no look at the remote needed
    assert!(
        t.iter().any(|l| l.starts_with("Steve 核对：PR #7 已合并")),
        "{t:?}"
    );
    assert!(k.open_fails(&x.item).is_empty());
    let m = pr_marks(&k, &x);
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(m[0].0, "merged");
    assert_eq!(m[0].1["review"], "graph");
    assert_eq!(m[0].1["by"], "steve");
    assert_eq!(
        merge_marks(&k, &x),
        [("merged".into(), "https://github.com/acme/app/pull/7".into())]
    );
}

#[test]
fn a_head_other_than_the_accepted_one_is_never_merged() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    gh.prs(open_pr(
        "0123456789abcdef0123456789abcdef01234567",
        json!({}),
    ));
    look(&k);
    look(&k);
    assert!(gh.merges().is_empty());
    assert_eq!(prompts_with(&k, &x, "不是验收通过的版本"), 1);
}

#[test]
fn a_head_that_cannot_be_bound_to_the_accepted_result_is_not_merged_and_is_yours() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    steve_store::core_fixture::work::closing_fingerprint(k.db().store(), &x.item, None);
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(&k);
    look(&k);
    assert!(gh.merges().is_empty());
    let fails = k.open_fails(&x.item);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(
        fails[0].contains("看不出它是不是验收通过的版本"),
        "{fails:?}"
    );
}

#[test]
fn an_invalidated_result_is_never_merged() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    k.core
        .tx()
        .lc_invalidate(&x.item, crate::cycle::void::MESSAGE, "收到新的消息")
        .unwrap();
    assert_eq!(k.phase(&x.item), "active");
    look(&k);
    look(&k);
    assert!(gh.merges().is_empty(), "the void result is not merged");
    // nor the release of the void result: once the release is pending again for a new acceptance,
    // only that one counts
    assert!(
        !k.core.tx().pr_may_merge(&release_op(&k, &x).id).unwrap(),
        "nothing to merge for a result that is not closing"
    );
}

#[test]
fn a_plan_reported_after_the_acceptance_is_only_noted_and_the_result_still_merges() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "跟进 PR", &[step("开 PR", true), step("等 CI", false)])
        .unwrap();
    assert_eq!(k.phase(&x.item), "closing", "the accepted result stands");
    assert!(
        k.timeline_texts(&x.item)
            .iter()
            .any(|l| l == "收尾中的计划更新只记录，不作废待收尾的结果"),
    );
    let h = head(&x.wt);
    merge_and_tag(&x, None);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(gh.merges().len(), 1);
    steve_runtime::testkit::eventually_blocking(
        "The item reaches closed after its PR merge despite the later plan update.",
        || k.phase(&x.item) == "closed",
    );
}

#[test]
fn your_delivered_choice_accepts_the_result_but_still_waits_for_merge_approval() {
    let gh = Gh::new();
    let script = gh.script.clone();
    let k = Kit::with(|c| c.gh = script);
    let x = remote_for(&k, upstream::DONE_MERGE, false, None);
    k.core
        .tx()
        .lc_invalidate(&x.item, crate::cycle::void::REDELIVER, "交付回执未成功")
        .unwrap();
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    k.rt.end_turn(&k.core, &x.session, "error", Some("delivery unavailable"));
    let fail = k
        .db()
        .decisions_of_item(&x.item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .unwrap();
    let note = "我已检查 PR 的 CI，确认已交付";
    k.core.resolve(&fail.id, "delivered", note, "me").unwrap();
    let i = k.db().item(&x.item).unwrap().unwrap();
    let closing = crate::cycle::closing_of(&i).unwrap();
    assert!(closing.accepted);
    assert_eq!(closing.policy, "human");
    assert!(closing.decision.is_none());
    assert!(closing.evidence.iter().any(|e| e.contains(note)));
    assert_eq!(i.phase, "closing", "the project's merge rule still applies");
    steve_runtime::testkit::eventually_blocking(
        "The release operation records the delivered pull request repository.",
        || release_op(&k, &x).evidence["pr_slug"] == "acme/app",
    );
    look(&k);
    assert_eq!(merge_cards(&k, &x).len(), 1);
    assert!(gh.merges().is_empty(), "acceptance is not consent to merge");
    assert!(
        !k.db()
            .decisions_of_item(&x.item)
            .unwrap()
            .iter()
            .any(|d| d.kind == "accept" && d.status == "open")
    );
    assert!(
        k.rt.prompts_of(&x.session).is_empty(),
        "an errored session is not restarted"
    );

    let reopened = k.reopened();
    reopened.lifecycle_tick().unwrap();
    let i = reopened.store().item(&x.item).unwrap().unwrap();
    assert_eq!(i.phase, "closing");
    assert!(crate::cycle::closing_of(&i).unwrap().accepted);
    assert_eq!(merge_cards(&k, &x).len(), 1);
    assert!(gh.merges().is_empty());
}

#[test]
fn an_unreviewed_change_waits_for_your_yes() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    look(&k);
    assert!(gh.merges().is_empty(), "no independent review: not merged");
    let cards = merge_cards(&k, &x);
    assert_eq!(cards.len(), 1, "asked once");
    let d = k.db().decision(&cards[0]).unwrap().unwrap();
    assert_eq!(d.kind, "approve");
    assert!(d.question.starts_with("PR #7 待合并"), "{}", d.question);
    k.core.resolve(&d.id, "yes", "", "me").unwrap();
    merge_and_tag(&x, None);
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --rebase --match-head-commit {h}"
        )]
    );
    steve_runtime::testkit::eventually_blocking(
        "The item reaches closed after the approved PR merge.",
        || k.phase(&x.item) == "closed",
    );
    let m = pr_marks(&k, &x);
    assert_eq!(m.last().unwrap().1["review"], "you", "{m:?}");
}

#[test]
fn an_unreviewed_change_you_merged_yourself_completes_the_item() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(merge_cards(&k, &x).len(), 1);
    merge_and_tag(&x, None);
    gh.prs(json!([{"number": 7, "state": "MERGED", "url": "https://github.com/acme/app/pull/7", "headRefOid": h}]));
    look(&k);
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The item reaches closed after the user merges the PR; {:?}",
            k.status(&x.item)
        )
    });
    steve_runtime::testkit::eventually_blocking(diagnostic, || k.phase(&x.item) == "closed");
    assert!(gh.merges().is_empty());
    assert!(
        merge_cards(&k, &x).is_empty(),
        "the card went with the close"
    );
    let m = pr_marks(&k, &x);
    assert_eq!(
        (m[1].0.as_str(), &m[1].1["review"], &m[1].1["by"]),
        ("merged", &json!("none"), &json!("elsewhere"))
    );
    assert!(k.open_fails(&x.item).is_empty());
    // the statistics' accepted → merged time ends at this mark
    assert_eq!(
        merge_marks(&k, &x),
        [("merged".into(), "https://github.com/acme/app/pull/7".into())]
    );
}

#[test]
fn a_no_to_merging_leaves_the_pull_request_to_you() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(&k);
    let card = merge_cards(&k, &x).pop().unwrap();
    k.core.resolve(&card, "no", "先别合", "me").unwrap();
    look(&k);
    look(&k);
    assert!(gh.merges().is_empty());
    assert!(merge_cards(&k, &x).is_empty(), "not asked again");
    assert_eq!(k.phase(&x.item), "closing");
}

#[test]
fn the_merge_card_expires_with_the_result_it_was_for() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(&k);
    let card = merge_cards(&k, &x).pop().unwrap();
    k.core
        .tx()
        .lc_invalidate(&x.item, crate::cycle::void::MESSAGE, "收到新的消息")
        .unwrap();
    assert_eq!(k.db().decision(&card).unwrap().unwrap().status, "expired");
    assert!(k.core.resolve(&card, "yes", "", "me").is_err());
    look(&k);
    assert!(gh.merges().is_empty());
}

#[test]
fn a_failed_merge_is_tried_again_later_and_becomes_a_card_after_a_few() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    gh.merge_fails(Some(
        "GraphQL: Head branch was modified. Review and try the merge again.",
    ));
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(&k);
    assert_eq!(gh.merges().len(), 1);
    let o = release_op(&k, &x);
    assert_eq!(o.evidence["pr_problem"], "merge");
    assert_eq!(o.evidence["pr_merge_failures"], 1);
    // not again before its wait is over
    look(&k);
    assert_eq!(gh.merges().len(), 1, "it waits before trying again");
    assert!(
        k.open_fails(&x.item).is_empty(),
        "one failure is no card yet"
    );
    for n in 2..=MERGE_FAILS_CARD {
        let op = release_op(&k, &x).id;
        k.db()
            .store()
            .write(|tx| {
                steve_store::operation_repo::set(tx, &op, "pending", &json!({"pr_merge_after": 0}))
            })
            .unwrap();
        look(&k);
        assert_eq!(gh.merges().len() as i64, n);
    }
    let fails = k.open_fails(&x.item);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(fails[0].contains("没能合并 PR #7"), "{fails:?}");
    assert_eq!(k.phase(&x.item), "closing");
}

#[test]
fn a_host_that_cannot_be_asked_becomes_a_card_after_a_few_looks() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    gh.offline(true);
    for _ in 1..UNKNOWN_LOOKS_CARD {
        look(&k);
    }
    assert!(k.open_fails(&x.item).is_empty());
    look(&k);
    look(&k);
    let fails = k.open_fails(&x.item);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(fails[0].contains("没能读到分支"), "{fails:?}");
    // the host answers again: counted from nothing
    gh.offline(false);
    gh.prs(open_pr(&head(&x.wt), json!({"isDraft": true})));
    look(&k);
    assert_eq!(release_op(&k, &x).evidence["pr_unknown"], 0);
}

#[test]
fn the_agent_is_woken_about_its_pull_request_only_so_often_a_cycle() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    let op = release_op(&k, &x).id;
    k.db()
        .store()
        .write(|tx| {
            steve_store::operation_repo::set(tx, &op, "pending", &json!({"pr_tells": PR_TELLS_MAX}))
        })
        .unwrap();
    gh.prs(open_pr(
        &head(&x.wt),
        json!({"mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}),
    ));
    look(&k);
    assert_eq!(prompts_with(&k, &x, "和默认分支有冲突"), 0);
    let fails = k.open_fails(&x.item);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(fails[0].contains("叫了 Bot"), "{fails:?}");
}

#[test]
fn a_new_result_of_a_followed_pull_request_does_not_wake_the_agent_again() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(&k);
    let told = prompts_with(&k, &x, "按这个项目的约定");
    assert_eq!(told, 1);
    // the result is void and delivered again (the same change), and accepted again
    accepted_again(&k, &x);
    steve_runtime::testkit::eventually_blocking(
        "The conversation records that Steve is still tracking the pull request.",
        || {
            k.timeline_texts(&x.item)
                .iter()
                .any(|l| l.contains("Steve 继续跟踪 PR #7"))
        },
    );
    assert_eq!(
        prompts_with(&k, &x, "按这个项目的约定"),
        told,
        "the agent is not woken for the pull request it already has"
    );
    // the card for the void result expired; the new result is asked about on its own
    assert!(merge_cards(&k, &x).is_empty());
    look(&k);
    assert_eq!(merge_cards(&k, &x).len(), 1);
}

fn all_merge_cards(k: &Kit, x: &Remote) -> Vec<crate::repo::Decision> {
    k.db()
        .decisions_of_item(&x.item)
        .unwrap()
        .into_iter()
        .filter(|d| d.subject_type == PR_AWAITING_MERGE)
        .collect()
}

fn approve_head(k: &Kit, x: &Remote, gh: &Gh) -> String {
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(k);
    let card = merge_cards(k, x).pop().unwrap();
    k.core.resolve(&card, "yes", "reviewed", "me").unwrap();
    card
}

#[test]
fn merge_consent_survives_report_redelivery_and_restart_at_the_same_exact_target() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    let card = approve_head(&k, &x, &gh);
    let receipt = k.db().decision(&card).unwrap().unwrap();
    assert_eq!(
        receipt.result["merge_target"],
        json!({"repo":"acme/app", "pr":7, "head":h})
    );
    // A plan/report update keeps the closeout; a new accepted report creates another operation.
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "补充报告", &[step("代码已完成", true)])
        .unwrap();
    assert_eq!(k.phase(&x.item), "closing");
    let old = release_op(&k, &x).id;
    accepted_again(&k, &x);
    assert_ne!(release_op(&k, &x).id, old);
    assert_eq!(head(&x.wt), h);
    gh.prs(open_pr(
        &h,
        json!({"statusCheckRollup":[{"name":"CI gate", "status":"IN_PROGRESS"}]}),
    ));
    look(&k);
    assert_eq!(
        all_merge_cards(&k, &x).len(),
        1,
        "no duplicate consent card"
    );
    assert!(gh.merges().is_empty(), "still waits for CI");
    assert_eq!(release_op(&k, &x).evidence["pr_merge_decision"], card);
    // Fresh Core/SQLite connection; no in-memory permission survives this.
    let restarted = k.reopened();
    restarted.check_prs().unwrap();
    assert!(gh.merges().is_empty());
    restarted.tx().st.pr_checked.clear();
    gh.prs(open_pr(&h, json!({})));
    restarted.check_prs().unwrap();
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --rebase --match-head-commit {h}"
        )]
    );
    steve_runtime::testkit::eventually_blocking(
        "The reopened Core records the item as closed after checking the approved merge.",
        || k.phase(&x.item) == "closed",
    );
    assert_eq!(all_merge_cards(&k, &x).len(), 1);
}

#[test]
fn merge_consent_does_not_cross_a_new_sha_pr_or_repository() {
    for change in ["sha", "pr", "repo"] {
        let gh = Gh::new();
        let (k, x) = accepted(&gh);
        approve_head(&k, &x, &gh);
        if change == "sha" {
            // Even an empty commit (same tree/rebase-equivalent result) needs new consent.
            g(&x.wt, &["commit", "--allow-empty", "-m", "new head"]);
        }
        accepted_again(&k, &x);
        if change == "repo" {
            k.core
                .tx()
                .follow_pr(&release_op(&k, &x).id, "acme/other", "item")
                .unwrap();
        }
        gh.prs(open_pr(
            &head(&x.wt),
            if change == "pr" {
                json!({"number":8})
            } else {
                json!({})
            },
        ));
        look(&k);
        look(&k);
        assert!(gh.merges().is_empty(), "{change}");
        assert_eq!(all_merge_cards(&k, &x).len(), 2, "{change}");
        assert_eq!(merge_cards(&k, &x).len(), 1, "{change}");
    }
}

#[test]
fn invalid_or_unverifiable_merge_consent_is_not_reused() {
    for change in [
        "expired",
        "reopened",
        "revoked",
        "missing",
        "legacy",
        "yolo",
        "wrong-target",
        "other-workspace",
        "other-cycle",
        "unknown-host",
        "unknown-result",
    ] {
        let gh = Gh::new();
        let (k, x) = accepted(&gh);
        let card = approve_head(&k, &x, &gh);
        match change {
            "expired" => {
                steve_store::core_fixture::decision(
                    k.db().store(),
                    &card,
                    steve_store::core_fixture::DecisionState::Expired,
                );
            }
            "reopened" => {
                k.db().decision_reopen(&card).unwrap();
            }
            "revoked" => {
                let mut result = k.db().decision(&card).unwrap().unwrap().result;
                result["approved"] = json!(false);
                k.db().decision_set_result(&card, &result).unwrap();
            }
            "missing" => {
                steve_store::core_fixture::work::remove_decision(k.db().store(), &card);
            }
            "legacy" => {
                k.db()
                    .decision_set_result(&card, &json!({"approved":true}))
                    .unwrap();
            }
            "yolo" => {
                let mut result = k.db().decision(&card).unwrap().unwrap().result;
                result["actor"] = json!("yolo");
                k.db().decision_set_result(&card, &result).unwrap();
            }
            "wrong-target" => {
                let mut result = k.db().decision(&card).unwrap().unwrap().result;
                result["merge_target"]["head"] = json!("0123456789abcdef0123456789abcdef01234567");
                k.db().decision_set_result(&card, &result).unwrap();
            }
            "other-workspace" => {
                steve_store::core_fixture::workspace(
                    k.db().store(),
                    "foreign",
                    "Foreign",
                    "ST",
                    0,
                    1,
                );
                steve_store::core_fixture::decision(
                    k.db().store(),
                    &card,
                    steve_store::core_fixture::DecisionState::Workspace("foreign"),
                );
            }
            "other-cycle" => {
                let op = release_op(&k, &x).id;
                steve_store::core_fixture::work::operation_cycle(k.db().store(), &op, 0);
            }
            _ => {}
        }
        accepted_again(&k, &x);
        if change == "unknown-result" {
            steve_store::core_fixture::work::closing_fingerprint(k.db().store(), &x.item, None);
        }
        gh.prs(open_pr(&head(&x.wt), json!({})));
        if change == "unknown-host" {
            gh.offline(true);
        }
        look(&k);
        look(&k);
        assert!(gh.merges().is_empty(), "{change}");
        if change == "revoked" {
            assert!(merge_cards(&k, &x).is_empty(), "a no stays manual");
            assert_eq!(release_op(&k, &x).evidence["pr_merge_declined"], true);
        } else if !change.starts_with("unknown") {
            assert_eq!(merge_cards(&k, &x).len(), 1, "{change}");
        }
    }
}

#[test]
fn a_card_and_an_approved_operation_cannot_authorize_a_different_live_pr() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    let card = approve_head(&k, &x, &gh);
    // Same operation and SHA, but a new PR; the fresh target must be checked before merge.
    gh.prs(open_pr(&h, json!({"number":8})));
    look(&k);
    assert!(gh.merges().is_empty());
    let other = merge_cards(&k, &x).pop().unwrap();
    assert_ne!(other, card);
    gh.prs(open_pr(&h, json!({"number":9})));
    look(&k);
    assert_eq!(k.db().decision(&other).unwrap().unwrap().status, "expired");
    assert!(k.core.resolve(&other, "yes", "", "me").is_err());
    assert!(gh.merges().is_empty());
}

#[test]
fn reused_merge_consent_does_not_skip_failed_ci_or_host_sha_protection() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    approve_head(&k, &x, &gh);
    accepted_again(&k, &x);
    gh.prs(open_pr(&h, json!({"statusCheckRollup":null})));
    look(&k);
    assert!(
        gh.merges().is_empty(),
        "an unread check rollup is not a pass"
    );
    gh.prs(open_pr(&h, json!({"statusCheckRollup":[{"name":"CI gate", "status":"COMPLETED", "conclusion":"FAILURE"}]})));
    look(&k);
    assert!(gh.merges().is_empty());
    assert_eq!(all_merge_cards(&k, &x).len(), 1);
    gh.merge_fails(Some(
        "GraphQL: Head branch was modified. Review and try the merge again.",
    ));
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --rebase --match-head-commit {h}"
        )]
    );
    assert_eq!(release_op(&k, &x).evidence["pr_progress"], "merge_retry");
    assert_eq!(k.phase(&x.item), "closing");
}

#[test]
fn merge_consent_is_rechecked_after_host_methods_and_before_the_merge_call() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let card = approve_head(&k, &x, &gh);
    let script = std::fs::read_to_string(&gh.script).unwrap();
    std::fs::write(&gh.script, script.replace(
        "case \"$1 $2\" in",
        "if [ \"$1 $2\" = 'api repos/acme/app' ]; then touch \"$d/repo.entered\"; while [ -f \"$d/repo.wait\" ]; do sleep 0.01; done; fi\ncase \"$1 $2\" in",
    )).unwrap();
    std::fs::write(gh.dir.join("repo.wait"), "").unwrap();
    let core = k.core.clone();
    let checking = std::thread::spawn(move || core.check_prs());
    steve_runtime::testkit::eventually_blocking(
        "the host methods query is reached after the first consent check; repo.entered exists",
        || gh.dir.join("repo.entered").exists(),
    );
    steve_store::core_fixture::decision(
        k.db().store(),
        &card,
        steve_store::core_fixture::DecisionState::Expired,
    );
    std::fs::remove_file(gh.dir.join("repo.wait")).unwrap();
    checking.join().unwrap().unwrap();
    assert!(
        gh.merges().is_empty(),
        "revoked during the host query: no merge call"
    );
    assert_eq!(merge_cards(&k, &x).len(), 1);
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
    // Local repair and remote authority are separate.
    assert!(p.contains("git merge origin/"), "{p}");
    assert!(
        p.contains("请主人更新 PR 分支") && !p.contains("git push"),
        "{p}"
    );
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    steve_runtime::testkit::eventually_blocking("All completion probes have finished.", || {
        k.core.tx().st.probing.is_empty()
    });
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
    steve_runtime::testkit::eventually_blocking(
        "The no-commit item reaches closed after the host reports its PR merged.",
        || {
            look(&k);
            k.phase(&x.item) == "closed"
        },
    );
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
    steve_runtime::testkit::eventually_blocking(
        "The accepted item with no own commits reaches closed while its PR remains open.",
        || {
            look(&k);
            k.phase(&x.item) == "closed"
        },
    );
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter().any(|l| l.starts_with("事项分支上没有自己的提交")),
        "{t:?}"
    );
    assert!(!t.iter().any(|l| l.contains("提交已合并到")), "{t:?}");
    assert!(gh.merges().is_empty(), "nothing of its own to merge");
}

/* ── what review found (#252): a result not all committed, a pull request of an earlier result, a
 * merge that ran while its result went void ── */

#[test]
fn an_accepted_result_not_all_committed_is_never_merged_in_part() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    // part of the accepted change is an edit not committed: the pushed head does not hold it
    std::fs::write(x.wt.join("feature.txt"), "the accepted change, all of it\n").unwrap();
    g(&x.wt, &["add", "-N", "feature.txt"]);
    freeze(&k, &x);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    look(&k);
    assert!(
        gh.merges().is_empty(),
        "the head holds only part of the result"
    );
    assert_eq!(prompts_with(&k, &x, "还有没提交的改动"), 1);
    assert_eq!(release_op(&k, &x).evidence["pr_problem"], "uncommitted");
    assert!(pr_marks(&k, &x).is_empty());
    // committed and pushed: the head holds all of it now, the same result
    g(&x.wt, &["add", "-A"]);
    g(&x.wt, &["commit", "-q", "-m", "the rest of the change"]);
    let h2 = head(&x.wt);
    gh.prs(open_pr(&h2, json!({})));
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --rebase --match-head-commit {h2}"
        )]
    );
    assert_eq!(k.phase(&x.item), "closing", "still the accepted result");
}

#[test]
fn an_untracked_file_of_the_accepted_result_is_not_merged_either() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    std::fs::write(x.wt.join("new-module.txt"), "never added\n").unwrap();
    freeze(&k, &x);
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(&k);
    assert!(gh.merges().is_empty());
    assert_eq!(prompts_with(&k, &x, "还有没提交的改动"), 1);
}

#[test]
fn a_new_result_after_its_pull_request_closed_is_woken_and_told_to_open_one() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    gh.prs(json!([{"number": 7, "state": "CLOSED", "url": "https://github.com/acme/app/pull/7", "headRefOid": head(&x.wt)}]));
    look(&k);
    assert!(k.open_fails(&x.item)[0].contains("PR #7 被关闭了"));
    let told = prompts_with(&k, &x, "按这个项目的约定");
    accepted_again(&k, &x);
    assert_eq!(
        prompts_with(&k, &x, "按这个项目的约定"),
        told + 1,
        "a closed pull request is not followed on: the agent hears it"
    );
    assert!(
        !k.timeline_texts(&x.item)
            .iter()
            .any(|l| l.contains("Steve 继续跟踪 PR #7"))
    );
    let fails = k.open_fails(&x.item).len();
    look(&k);
    look(&k);
    assert_eq!(prompts_with(&k, &x, "在这次验收之前就关闭了"), 1);
    assert_eq!(
        k.open_fails(&x.item).len(),
        fails,
        "the earlier result's closed pull request is no card for this one"
    );
    // the agent opens a new one: followed as this result's
    gh.prs(json!([
        {"number": 7, "state": "CLOSED", "url": "https://github.com/acme/app/pull/7", "headRefOid": head(&x.wt)},
        open_pr(&head(&x.wt), json!({"number": 8, "url": "https://github.com/acme/app/pull/8"}))[0],
    ]));
    look(&k);
    assert_eq!(release_op(&k, &x).evidence["pr"], 8);
    assert_eq!(merge_cards(&k, &x).len(), 1);
}

#[test]
fn a_pull_request_merged_before_a_new_result_saw_it_is_not_that_result_merged() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    // void, merged on the host meanwhile, and a result accepted again (not on main: the probe is
    // not met)
    gh.prs(json!([{"number": 7, "state": "MERGED", "url": "https://github.com/acme/app/pull/7", "headRefOid": h}]));
    accepted_again(&k, &x);
    look(&k);
    look(&k);
    let o = release_op(&k, &x);
    assert_ne!(o.evidence["pr_merged"], true, "{o:?}");
    assert!(
        pr_marks(&k, &x).iter().all(|(to, _)| to != "merged"),
        "no merge is marked for this result"
    );
    assert_eq!(prompts_with(&k, &x, "在这次验收之前就合并了"), 1);
    assert_eq!(k.phase(&x.item), "closing");
}

#[test]
fn a_merge_that_ran_while_its_result_went_void_is_said_marked_and_yours() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    // the look before the merge was fine; the result goes void while the merge call runs
    let due = k.core.tx().prs_due().unwrap();
    assert_eq!(due.len(), 1);
    k.core
        .tx()
        .lc_invalidate(&x.item, crate::cycle::void::MESSAGE, "收到新的消息")
        .unwrap();
    let crate::pull::Look::Found(pr) = crate::pull::read(&open_pr(&h, json!({}))) else {
        panic!()
    };
    k.core
        .tx()
        .pr_seen(
            &due[0],
            crate::pull::Look::Found(pr),
            crate::pull::Accepted::At(h.clone()),
            Some(crate::pull::Merging::Merged),
        )
        .unwrap();
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter().any(|l| l.contains("它的结果在合并时作废了")),
        "{t:?}"
    );
    let m = pr_marks(&k, &x);
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!((m[0].0.as_str(), &m[0].1["void"]), ("merged", &json!(true)));
    let fails = k.open_fails(&x.item);
    assert!(
        fails.iter().any(|f| f.contains("合并时它的结果刚好作废了")),
        "{fails:?}"
    );
    // the next result is not followed on that pull request: the agent hears it
    let told = prompts_with(&k, &x, "按这个项目的约定");
    gh.prs(json!([{"number": 7, "state": "MERGED", "url": "https://github.com/acme/app/pull/7", "headRefOid": h}]));
    accepted_again(&k, &x);
    assert_eq!(prompts_with(&k, &x, "按这个项目的约定"), told + 1);
}

/* ── a pull request merged on the host with its branch deleted (#347), the merge method (#316) ── */

/// The worktree is cleaned up (as it is once its branch is gone).
fn remove_worktree(x: &Remote) {
    g(
        &x.repo,
        &["worktree", "remove", "--force", &x.wt.to_string_lossy()],
    );
    assert!(!x.wt.exists());
}

/// The pull request #7 as `gh pr view` answers it once it was squash-merged at `head`.
fn squash_merged(head: &str) -> Value {
    json!({"number": 7, "state": "MERGED", "url": "https://github.com/acme/app/pull/7",
        "headRefOid": head, "isDraft": false, "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN",
        "autoMergeRequest": null, "statusCheckRollup": [],
        "mergeCommit": {"oid": "f00dfacef00dfacef00dfacef00dfacef00dface"}})
}

#[test]
fn a_pull_request_merged_with_its_branch_deleted_completes_the_item_without_its_worktree() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(release_op(&k, &x).evidence["pr_state"], "OPEN");
    // squash-merged on GitHub with "delete branch": the branch lists nothing any more, the pull
    // request by its number is merged; then the worktree is cleaned up
    gh.prs(json!([]));
    gh.view(squash_merged(&h));
    remove_worktree(&x);
    // the next closing pass sees the worktree gone: the host is asked first, and the merge decides
    k.core.tx().lc_try_close(&x.item).unwrap();
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The item reaches closed after finding its merged PR despite the deleted worktree; {:?} {:?}",
            k.status(&x.item),
            k.timeline_texts(&x.item)
        )
    });
    steve_runtime::testkit::eventually_blocking(diagnostic, || k.phase(&x.item) == "closed");
    let i = k.db().item(&x.item).unwrap().unwrap();
    assert_eq!(i.outcome.as_deref(), Some("completed"));
    let t = k.timeline_texts(&x.item);
    assert!(!t.iter().any(|l| l.starts_with("收尾作废")), "{t:?}");
    assert!(
        t.iter()
            .any(|l| l.starts_with("Steve 核对：PR #7 已合并（合并提交 f00dfac）")),
        "{t:?}"
    );
    assert_eq!(release_op(&k, &x).status, "succeeded");
    assert!(k.open_fails(&x.item).is_empty());
    assert!(gh.merges().is_empty());
    assert_eq!(
        gh.calls("pr view").first().map(String::as_str),
        Some(
            format!(
                "pr view 7 --repo acme/app --json {}",
                crate::pull::PR_FIELDS
            )
            .as_str()
        )
    );
    // nothing of Steve's was told to the agent to redeliver
    assert_eq!(prompts_with(&k, &x, "你上一次的交付已经作废"), 0);
}

#[test]
fn a_merged_pull_request_seen_before_the_worktree_went_completes_on_any_pass() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    // merged with the branch deleted while a turn still runs: seen merged, the close waits for it
    gh.prs(json!([]));
    gh.view(squash_merged(&h));
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.db()
        .session_patch(&s.id, crate::patch![status: "running"])
        .unwrap();
    look(&k);
    assert_eq!(release_op(&k, &x).evidence["pr_merged"], true);
    assert_eq!(k.phase(&x.item), "closing");
    // the worktree is cleaned up, then the turn ends: completed, not void
    remove_worktree(&x);
    k.db()
        .session_patch(&s.id, crate::patch![status: "idle"])
        .unwrap();
    k.core.lifecycle_tick().unwrap();
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The item reaches closed after its last turn ends using the saved PR merge evidence; {:?}",
            k.timeline_texts(&x.item)
        )
    });
    steve_runtime::testkit::eventually_blocking(diagnostic, || k.phase(&x.item) == "closed");
    assert!(
        !k.timeline_texts(&x.item)
            .iter()
            .any(|l| l.starts_with("收尾作废"))
    );
}

#[test]
fn a_worktree_gone_while_its_pull_request_is_still_open_voids_the_result_as_before() {
    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let h = head(&x.wt);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    remove_worktree(&x);
    // held until the host answered after the move
    k.core.tx().lc_try_close(&x.item).unwrap();
    assert_eq!(k.phase(&x.item), "closing", "the host is asked first");
    look(&k);
    // it answered: still open, so the moved result is void
    steve_runtime::testkit::eventually_blocking(
        "The item reaches its active phase after the worktree removal check.",
        || {
            k.core.tx().lc_try_close(&x.item).unwrap();
            k.phase(&x.item) == "active"
        },
    );
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter()
            .any(|l| l == "收尾作废：冻结之后结果又变了：工作区不见了，需要重新交付"),
        "{t:?}"
    );
}

#[test]
fn a_branch_github_cannot_rebase_is_squashed_without_trying_the_rebase() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    gh.pull(json!({"rebaseable": false}));
    let h = head(&x.wt);
    merge_and_tag(&x, None);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --squash --match-head-commit {h}"
        )]
    );
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The item reaches closed after the allowed squash merge; {:?}",
            k.status(&x.item)
        )
    });
    steve_runtime::testkit::eventually_blocking(diagnostic, || k.phase(&x.item) == "closed");
    assert!(k.open_fails(&x.item).is_empty());
}

#[test]
fn a_rebase_github_refuses_falls_back_to_a_squash_with_one_merge_and_no_card() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    // the host had not worked out whether it can rebase: the rebase is asked for, and refused
    gh.rebase_fails("GraphQL: This branch can't be rebased (mergePullRequest)");
    let h = head(&x.wt);
    merge_and_tag(&x, None);
    gh.prs(open_pr(&h, json!({})));
    look(&k);
    assert_eq!(
        gh.merges(),
        [
            format!("pr merge 7 --repo acme/app --rebase --match-head-commit {h}"),
            format!("pr merge 7 --repo acme/app --squash --match-head-commit {h}"),
        ]
    );
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The item reaches closed after rebase refusal falls back to squash; {:?}",
            k.status(&x.item)
        )
    });
    steve_runtime::testkit::eventually_blocking(diagnostic, || k.phase(&x.item) == "closed");
    assert!(k.open_fails(&x.item).is_empty());
    let o = release_op(&k, &x);
    assert_eq!(o.evidence["pr_merged_by"], "steve");
    assert_eq!(o.evidence["pr_merge_failures"], 0);
    let m = pr_marks(&k, &x);
    assert_eq!(m.len(), 1, "one merge: {m:?}");
}

#[test]
fn when_no_allowed_method_can_merge_the_card_says_why_at_once() {
    let gh = Gh::new();
    let (k, x) = reviewed(&gh);
    gh.repo(json!({"allow_squash_merge": false, "allow_merge_commit": false}));
    gh.pull(json!({"rebaseable": false}));
    gh.prs(open_pr(&head(&x.wt), json!({})));
    look(&k);
    assert!(gh.merges().is_empty(), "nothing it could merge with");
    let fails = k.open_fails(&x.item);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(
        fails[0].contains("合并方式都不行") && fails[0].contains("不能 rebase"),
        "{fails:?}"
    );
    assert_eq!(release_op(&k, &x).evidence["pr_problem"], "merge_method");
    assert_eq!(k.phase(&x.item), "closing");
}

#[test]
fn closeout_publishes_only_current_checks_and_deduplicates_conversation_milestones() {
    use crate::thread::hidden;

    let gh = Gh::new();
    let (k, x) = accepted(&gh);
    let ws = k.db().item(&x.item).unwrap().unwrap().workspace_id;
    let origin = k
        .db()
        .message_add(&ws, "me", "跟进 PR", None, None)
        .unwrap();
    let host = k
        .db()
        .message_add(
            &ws,
            "steve",
            "跟进中",
            Some(&x.item),
            Some(&json!({
                "card": {"item": x.item, "state": "deliver", "delivery": {"text": "验收结果"}}
            })),
        )
        .unwrap();
    k.db()
        .store()
        .thread()
        .item_add(&x.item, &ws, &origin, Some(&host), false)
        .unwrap();
    for _ in 0..crate::thread::flowback::FAR {
        k.db()
            .message_add(&ws, "me", "其他消息", None, None)
            .unwrap();
    }
    let messages = || k.db().messages(&ws).unwrap();
    let visible_card = || {
        let mut cards: Vec<_> = messages()
            .into_iter()
            .filter(|m| !hidden(m) && m.meta["card"]["item"] == x.item)
            .collect();
        assert_eq!(cards.len(), 1);
        cards.pop().unwrap()
    };
    let read = || {
        steve_store::views::closeout_from_evidence(
            &release_op(&k, &x).evidence,
            k.phase(&x.item) == "closed",
            !merge_cards(&k, &x).is_empty(),
        )
        .unwrap()
    };
    let h = head(&x.wt);
    let queued = json!({"statusCheckRollup": [
        {"name": "rust", "status": "QUEUED"},
        {"context": "lint", "state": "SUCCESS"},
        {"name": "optional", "status": "COMPLETED", "conclusion": "SKIPPED"}
    ]});
    gh.prs(open_pr(&h, queued.clone()));
    look(&k);
    assert_eq!(read().status, "awaiting_approval");
    let approvals = merge_cards(&k, &x);
    assert_eq!(approvals.len(), 1);
    assert_ne!(visible_card().id, host, "a decision promotes the card");
    assert_eq!(visible_card().meta["card"]["decision"], approvals[0]);
    look(&k);
    assert_eq!(merge_cards(&k, &x), approvals);
    assert!(gh.merges().is_empty());
    k.core.resolve(&approvals[0], "yes", "", "me").unwrap();
    let quiet_at = visible_card().meta["card"]["at"].clone();
    let lines = k.timeline_texts(&x.item);
    look(&k);
    let p = read();
    assert_eq!((p.status, p.next_owner), ("waiting_checks", "steve"));
    let checks = p.pr.unwrap().checks.unwrap();
    assert_eq!(
        (checks.passed, checks.pending, checks.failed, checks.other),
        (1, 1, 0, 1)
    );
    assert_eq!(visible_card().meta["card"]["at"], quiet_at);
    assert_eq!(
        k.timeline_texts(&x.item),
        lines,
        "routine polls don't add timeline entries"
    );

    gh.prs(open_pr(
        &h,
        json!({"statusCheckRollup": [
            {"name": "rust", "status": "COMPLETED", "conclusion": "FAILURE"}
        ]}),
    ));
    look(&k);
    assert_eq!(
        (read().status, read().next_owner),
        ("checks_failed", "agent")
    );
    let card = visible_card();
    assert_eq!(card.meta["card"]["pr_event"]["kind"], "pr_checks_failed");
    assert_eq!(card.meta["card"]["delivery"]["text"], "验收结果");
    look(&k);
    assert_eq!(visible_card().meta, card.meta);
    assert_eq!(gh.merges().len(), 0);

    let failed = read();
    gh.offline(true);
    look(&k);
    let down = read();
    assert_eq!(down.status, "sync_failed");
    assert_eq!(down.last_success_at, failed.last_success_at);
    assert_eq!(
        down.pr, failed.pr,
        "the old result is labelled stale, not replaced with success"
    );
    assert!(down.sync_error.unwrap().contains("error connecting"));
    assert_eq!(
        visible_card().meta,
        card.meta,
        "an individual failed poll is quiet"
    );
    gh.offline(false);

    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    g(
        &x.wt,
        &["commit", "--amend", "-qm", "same result, new head"],
    );
    let h2 = head(&x.wt);
    assert_ne!(h2, h);
    gh.prs(open_pr(&h2, queued));
    look(&k);
    let fresh = read();
    assert_eq!(fresh.status, "awaiting_approval");
    assert_eq!(fresh.sync_error, None);
    let pr = fresh.pr.unwrap();
    assert_eq!(pr.head, h2);
    assert!(pr.checks.unwrap().failing.is_empty());
    // Same tree on a new SHA still requires a new human answer. Once answered, routine CI
    // observations are quiet again; they must not retain the earlier head's merge permission.
    let changed_head_card = merge_cards(&k, &x).pop().unwrap();
    assert_ne!(changed_head_card, approvals[0]);
    assert_eq!(visible_card().meta["card"]["decision"], changed_head_card);
    k.core.resolve(&changed_head_card, "yes", "", "me").unwrap();
    let approved = visible_card().meta;
    look(&k);
    assert_eq!(read().status, "waiting_checks");
    assert_eq!(visible_card().meta, approved);
    assert!(gh.merges().is_empty());

    gh.prs(open_pr(&h2, json!({})));
    look(&k);
    steve_runtime::testkit::eventually_blocking(
        "The item reaches closed after consent and checks apply to the current PR head.",
        || k.phase(&x.item) == "closed",
    );
    assert_eq!((read().status, read().next_owner), ("merged", "none"));
    assert_eq!(read().pr.unwrap().state, "MERGED");
    assert_eq!(visible_card().meta["card"]["pr_event"]["kind"], "pr_merged");
    assert!(gh.merges()[0].ends_with(&format!("--match-head-commit {h2}")));
    let card = visible_card();
    look(&k);
    assert_eq!(visible_card().meta, card.meta);
}

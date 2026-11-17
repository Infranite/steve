//! An accepted change lands through its branch's pull request (crate::pull): Steve merges it itself
//! at the accepted head once every check passed, only while that result still stands, and only a
//! change an independent review passed (else it waits for your yes); the item closes once it merged,
//! and a conflict, a failed check, a missing pull request, one closed without merging, a merge that
//! keeps failing or a host that can't be asked comes back to the agent or to you.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use steve_modules::outbound::YOLO_ACTOR;

use super::takeover::{Remote, merge_and_tag, remote_for, told_what_is_missing};
use crate::cycle::OP_RELEASE;
use crate::git::tests::g;
use crate::pull::{MERGE_FAILS_CARD, PR_AWAITING_MERGE, PR_TELLS_MAX, UNKNOWN_LOOKS_CARD};
use crate::repo::PlanStep;
use crate::testkit::Kit;
use crate::upstream;

/// A `gh` that answers `pr list` with what `pr.json` holds and `api repos/…` with `repo.json` (by
/// default what the real `gh api` answered for a repository the account may write to), takes every
/// `pr merge` unless `merge.fail` holds why not, fails `pr list` while `list.fail` exists, and writes
/// each call to `calls`.
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
                "#!/bin/sh\nd='{}'\necho \"$@\" >> \"$d/calls\"\ncase \"$1 $2\" in\n  'pr list') if [ -f \"$d/list.fail\" ]; then echo 'error connecting to api.github.com' >&2; exit 1; fi; cat \"$d/pr.json\" ;;\n  'api repos/'*) cat \"$d/repo.json\" ;;\n  'pr merge') if [ -f \"$d/merge.fail\" ]; then cat \"$d/merge.fail\" >&2; exit 1; fi ;;\n  *) exit 1 ;;\nesac\n",
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
    k.db()
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE items SET closing = json_set(closing, '$.fingerprint', ?2) WHERE id = ?1",
                [&x.item, &print],
            )?;
            Ok(())
        })
        .unwrap();
}

/// The result is void (a new message) and the same change is delivered and accepted again.
fn accepted_again(k: &Kit, x: &Remote) {
    k.core.tx().lc_invalidate(&x.item, "收到新的消息").unwrap();
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
    assert!(k.wait(|| {
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
    }));
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
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
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
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
    k.db()
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO workflows (item_id, revision, cycle, goal_version, paused, max_parallel, max_attempts, created, updated)
                 VALUES (?1, 1, ?2, ?3, 0, 1, 3, 0, 0)",
                rusqlite::params![x.item, i.cycle, goal],
            )?;
            for (n, (id, s)) in [
                ("work", spec("work", "dev", "work", &[])),
                ("review", spec("review", "steve", "review", &["work"])),
                ("integrate", spec("integrate", "dev", "integrate", &["review"])),
            ]
            .into_iter()
            .enumerate()
            {
                c.execute(
                    "INSERT INTO workflow_units (workflow_id, id, position, spec, status, attempt)
                     VALUES (?1, ?2, ?3, ?4, 'succeeded', 1)",
                    rusqlite::params![x.item, id, n as i64, s],
                )?;
            }
            c.execute(
                "INSERT INTO workflow_history (workflow_id, revision, action, reason, detail, created)
                 VALUES (?1, 1, 'plan', '评审', '{}', 0)",
                [&x.item],
            )?;
            c.execute(
                "UPDATE items SET closing = json_set(closing, '$.workflow_revision', 1) WHERE id = ?1",
                [&x.item],
            )?;
            Ok(())
        })
        .unwrap();
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
    k.db()
        .store()
        .with_conn(|c| {
            Ok(c.prepare(
                "SELECT to_v, detail FROM item_marks WHERE item_id = ? AND kind = 'pr' ORDER BY id",
            )?
            .query_map([&x.item], |r| {
                let d: String = r.get(1)?;
                Ok((r.get(0)?, serde_json::from_str(&d).unwrap()))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap()
}

/// The release operation the pull request is followed on.
/// The `merge` marks of the item (the workflow statistics' accepted → merged time): `(to_v, ref)`.
fn merge_marks(k: &Kit, x: &Remote) -> Vec<(String, String)> {
    k.db()
        .store()
        .with_conn(|c| {
            Ok(
                c.prepare("SELECT to_v, ref FROM item_marks WHERE item_id = ? AND kind = 'merge'")?
                    .query_map([&x.item], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            )
        })
        .unwrap()
}

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
    assert!(p.contains("gh pr create"), "{p}");
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
    assert_eq!(gh.calls("api"), ["api repos/acme/app"]);
    assert!(
        k.wait(|| k.phase(&x.item) == "closed"),
        "{:?}",
        k.status(&x.item)
    );
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter().any(|l| l.contains("Steve 已按验收的版本")),
        "{t:?}"
    );
    assert!(
        t.iter()
            .any(|l| l.starts_with("Steve 核对：提交已合并到 origin/main")),
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
    let (k, x) = accepted_unfrozen(&gh);
    let op = release_op(&k, &x).id;
    k.db()
        .store()
        .transaction(|tx| {
            steve_store::operation_set_in(tx, &op, "pending", &json!({"pr_merge_ok": true}))
        })
        .unwrap();
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
    k.core.tx().lc_invalidate(&x.item, "收到新的消息").unwrap();
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
    assert!(k.wait(|| k.phase(&x.item) == "closed"));
}

#[test]
fn an_unreviewed_change_waits_for_your_yes_which_yolo_cannot_give() {
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
    // YOLO never answers it: refused by the core, and its judge leaves it to you
    assert!(k.core.resolve(&d.id, "yes", "", YOLO_ACTOR).is_err());
    let f = crate::yolo::judge::facts_of(
        &d,
        crate::yolo::judge::subject_from_row(&d, None, None, None),
    );
    let v = crate::yolo::judge::judge(
        &f,
        &crate::yolo::judge::Policy::default(),
        &crate::yolo::judge::Env {
            steve_home: "/nonexistent".into(),
            home: "/nonexistent".into(),
            tmp: vec![],
        },
    );
    assert!(
        matches!(v, crate::yolo::judge::Verdict::Decline { .. }),
        "{v:?}"
    );
    assert_eq!(pr_marks(&k, &x)[0].0, "awaiting");
    // the card does not hold the close: your yes lets Steve merge it
    k.core.resolve(&d.id, "yes", "", "me").unwrap();
    merge_and_tag(&x, None);
    look(&k);
    assert_eq!(
        gh.merges(),
        [format!(
            "pr merge 7 --repo acme/app --rebase --match-head-commit {h}"
        )]
    );
    assert!(k.wait(|| k.phase(&x.item) == "closed"));
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
    assert!(
        k.wait(|| k.phase(&x.item) == "closed"),
        "{:?}",
        k.status(&x.item)
    );
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
    k.core.tx().lc_invalidate(&x.item, "收到新的消息").unwrap();
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
            .transaction(|tx| {
                steve_store::operation_set_in(tx, &op, "pending", &json!({"pr_merge_after": 0}))
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
        .transaction(|tx| {
            steve_store::operation_set_in(tx, &op, "pending", &json!({"pr_tells": PR_TELLS_MAX}))
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
    assert!(k.wait(|| {
        k.timeline_texts(&x.item)
            .iter()
            .any(|l| l.contains("Steve 继续跟踪 PR #7"))
    }));
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
    // a merge and a plain push of its own branch need no card; a force push after a rebase does
    assert!(p.contains("git merge origin/"), "{p}");
    assert!(
        p.contains("git push --force-with-lease）仍要先用 steve_request_approval"),
        "{p}"
    );
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
    k.core.tx().lc_invalidate(&x.item, "收到新的消息").unwrap();
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

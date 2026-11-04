//! Steve looks first (crate::takeover), with ST-25 as the case: an agent that merged the PR and pushed
//! the tag, then stopped while the Release build ran; a delivery that went void because the branch was
//! rebased, reset onto the merged commit, or got one more commit after it was frozen.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::CreateItem;
use crate::cycle::{self, OP_BUILD, OP_RELEASE};
use crate::git::tests::{branch_with, g, repo};
use crate::repo::{Release, WS};
use crate::runtime::TurnEnd;
use crate::takeover::{LINE, STEVE_TRIED};
use crate::testkit::Kit;
use crate::upstream;

fn kit(continues: u32) -> Kit {
    Kit::with(|c| c.takeover_continues = continues)
}

fn steve_lines(k: &Kit, item: &str) -> Vec<String> {
    k.timeline_texts(item)
        .into_iter()
        .filter(|t| t.starts_with(LINE))
        .collect()
}

fn stop(k: &Kit, sid: &str, words: &str) {
    k.rt.say(sid, words);
    k.rt.end_turn(&k.core, sid, "end_turn", None);
}

fn started(k: &Kit) -> (String, String) {
    let i = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g".into(),
            title: "t".into(),
            ..Default::default()
        })
        .unwrap();
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    (i.id, s.id)
}

#[test]
fn a_stopped_agent_is_continued_twice_before_you_hear_of_it_once() {
    let k = kit(2);
    let (item, sid) = started(&k);
    stop(&k, &sid, "PR 已合并，tag 也推送了，GitHub 正在构建发布包。");
    assert!(
        k.open_fails(&item).is_empty(),
        "Steve took it: {:?}",
        k.open_fails(&item)
    );
    let p = k.rt.prompts_of(&sid).pop().unwrap();
    assert!(p.starts_with("（Steve）你这一回合结束了"), "{p}");
    assert!(
        p.contains("已经完成的推送、合并、打 tag、发布不要重做"),
        "{p}"
    );
    assert_eq!(k.status(&item).2, "Steve 让它继续");
    stop(&k, &sid, "还在等构建。");
    assert_eq!(steve_lines(&k, &item).len(), 2);
    assert!(steve_lines(&k, &item)[1].ends_with("（第 2/2 次）"));
    // out of continues: one card, saying Steve tried
    stop(&k, &sid, "还在等构建。");
    let fails = k.open_fails(&item);
    assert_eq!(fails.len(), 1);
    assert!(
        fails[0].contains(&format!("{STEVE_TRIED} 2 次")),
        "{}",
        fails[0]
    );
    let prompts = k.rt.prompts_of(&sid).len();
    // the same stop reported again: still that one card, nothing sent
    k.core
        .on_turn_end(TurnEnd {
            session_id: sid.clone(),
            stop_reason: "end_turn".into(),
            error: None,
        })
        .unwrap();
    assert_eq!(k.open_fails(&item).len(), 1);
    assert_eq!(k.rt.prompts_of(&sid).len(), prompts);
    // you said go on: the count starts over
    let d = k.db().decisions_of_item(&item).unwrap().pop().unwrap();
    k.core.resolve(&d.id, "continue", "", "me").unwrap();
    stop(&k, &sid, "又停了。");
    assert!(k.open_fails(&item).is_empty());
    assert!(
        steve_lines(&k, &item)
            .last()
            .unwrap()
            .ends_with("（第 1/2 次）")
    );
}

#[test]
fn a_stop_reported_twice_is_continued_once() {
    let k = kit(2);
    let (item, sid) = started(&k);
    stop(&k, &sid, "先停一下。");
    let n = k.rt.prompts_of(&sid).len();
    // a second report of the same stop while the continue already runs
    k.core
        .on_turn_end(TurnEnd {
            session_id: sid.clone(),
            stop_reason: "end_turn".into(),
            error: None,
        })
        .unwrap();
    assert_eq!(k.rt.prompts_of(&sid).len(), n);
    assert_eq!(steve_lines(&k, &item).len(), 1);
    assert!(k.open_fails(&item).is_empty());
}

#[test]
fn a_question_in_plain_words_is_for_you() {
    let k = kit(2);
    let (item, sid) = started(&k);
    stop(&k, &sid, "要先合并 PR 还是先等 CI？");
    assert_eq!(k.open_fails(&item).len(), 1);
    assert!(steve_lines(&k, &item).is_empty());
}

#[test]
fn an_idle_session_is_continued_but_one_you_interrupted_is_not() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    let later = crate::util::now() + crate::stall::STALL_AFTER_MS + 1_000;
    assert!(k.core.tx().check_stalls(later).unwrap().is_empty());
    assert!(k.open_fails(&i).is_empty());
    assert!(
        k.rt.prompts_of(&s)
            .pop()
            .unwrap()
            .starts_with("（Steve）会话停了一会儿")
    );
    // the same with the turn stopped by you: the card, Steve does not undo your stop
    let j = k.item("run", "执行", None);
    let t = k.session(&j, "idle");
    k.db()
        .event_add(&t, "system", json!({"text": "你中断了当前回合"}))
        .unwrap();
    assert_eq!(k.core.tx().check_stalls(later).unwrap(), vec![j.clone()]);
    assert!(k.rt.prompts_of(&t).is_empty());
}

/* ── the frozen result ── */

fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    g(dir, &["add", "."]);
    g(dir, &["commit", "-q", "-m", file]);
}

#[test]
fn a_rebase_or_a_reset_onto_the_merged_copy_is_the_same_result_a_new_file_is_not() {
    let k = Kit::new();
    let r = repo(k.home());
    let (_, wt) = branch_with(&r, &k.home().join("wt"), "ST-9", "x.txt", "x\n");
    let cwd = wt.to_string_lossy().to_string();
    let frozen = cycle::fingerprint(Some(&cwd)).unwrap();
    assert_eq!(frozen.split(':').count(), 4, "{frozen}");
    // main moves on; the branch is rebased onto it: other content, the same own change
    commit(&r, "m.txt", "m\n");
    g(&wt, &["rebase", "-q", "main"]);
    let rebased = cycle::fingerprint(Some(&cwd)).unwrap();
    assert_ne!(
        upstream::parse(&frozen).tree,
        upstream::parse(&rebased).tree
    );
    assert!(cycle::same_result(&frozen, &rebased));
    // merged upstream as a copy (another sha), and the branch reset onto it: the same content
    let tip = g(&wt, &["rev-parse", "HEAD"]);
    g(&r, &["cherry-pick", "-x", &tip]);
    let merged = g(&r, &["rev-parse", "main"]);
    assert_ne!(merged, tip);
    g(&wt, &["reset", "-q", "--hard", &merged]);
    let reset = cycle::fingerprint(Some(&cwd)).unwrap();
    assert_ne!(
        cycle::fingerprint_head(&reset),
        cycle::fingerprint_head(&rebased)
    );
    assert!(cycle::same_result(&rebased, &reset));
    // one more file after the freeze is a new result, and says which file
    commit(&wt, "notes.md", "n\n");
    let more = cycle::fingerprint(Some(&cwd)).unwrap();
    assert!(!cycle::same_result(&reset, &more));
    assert_eq!(
        upstream::describe_change(&wt, &reset, &more),
        "改了 notes.md"
    );
    // fingerprints written before own changes were recorded still compare by content
    let old = |f: &str| f.split(':').take(2).collect::<Vec<_>>().join(":");
    assert!(cycle::same_result(&old(&more), &more));
    assert!(!cycle::same_result(&old(&reset), &more));
}

/* ── the completion rule, against a remote ── */

struct Remote {
    repo: PathBuf,
    item: String,
    session: String,
    wt: PathBuf,
}

/// A project whose completion rule is `rule`, an `origin` it pushes to (a bare repo that answers as
/// `github.com/acme/app`), and an item delivered on its own branch; `accept` = the user accepted it.
fn remote(k: &Kit, rule: &str, accept: bool) -> Remote {
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
    let url = "https://github.com/acme/app.git";
    g(
        &r,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.to_string_lossy()),
            url,
        ],
    );
    g(&r, &["remote", "add", "origin", url]);
    g(&r, &["fetch", "-q", "origin"]);
    let rel = Release {
        kind: "script".into(),
        cmd: k.home().join("release.sh").to_string_lossy().into(),
        done_when: Some(rule.into()),
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
        .unwrap();
    let x = k.delivered(false);
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &x.id, "x.txt", "x\n");
    k.db()
        .item_patch(&x.id, crate::patch![project_id: p.id.as_str(), cwd: wt.to_string_lossy().to_string(), branch: name.as_str()])
        .unwrap();
    if accept {
        k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    }
    Remote {
        repo: r,
        item: x.id,
        session: x.session,
        wt,
    }
}

/// What GitHub does on a rebase merge and a tag push: a copy of the branch's commits on `main`, tagged.
fn merge_and_tag(x: &Remote, tag: Option<&str>) {
    let tip = g(&x.wt, &["rev-parse", "HEAD"]);
    g(&x.repo, &["cherry-pick", "-x", &tip]);
    g(&x.repo, &["push", "-q", "origin", "main"]);
    if let Some(t) = tag {
        g(&x.repo, &["tag", "-a", t, "-m", t, "main"]);
        g(&x.repo, &["push", "-q", "origin", t]);
    }
}

fn op_states(k: &Kit, item: &str) -> Vec<(String, String)> {
    let i = k.db().item(item).unwrap().unwrap();
    k.db()
        .store()
        .items()
        .operations(item, i.cycle)
        .unwrap()
        .into_iter()
        .filter(|o| o.kind == OP_RELEASE || o.kind == OP_BUILD)
        .map(|o| (o.kind, o.status))
        .collect()
}

#[test]
fn accepted_it_waits_for_the_merge_and_the_tag_then_closes_without_the_build() {
    let k = kit(2);
    let x = remote(&k, upstream::DONE_TAG, true);
    // not merged yet: the agent hears once what is missing, nothing for you
    assert!(k.wait(|| {
        k.rt.prompts_of(&x.session)
            .last()
            .is_some_and(|p| p.contains("按这个项目的约定"))
    }));
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(p.contains("PR 还没合并"), "{p}");
    assert!(p.contains("Release 构建不用等"), "{p}");
    assert_eq!(k.phase(&x.item), "closing");
    assert!(k.open_fails(&x.item).is_empty());
    // merged but no tag: still waiting
    merge_and_tag(&x, None);
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
    assert_eq!(k.phase(&x.item), "closing");
    assert_eq!(
        k.rt.prompts_of(&x.session)
            .iter()
            .filter(|p| p.contains("按这个项目的约定"))
            .count(),
        1,
        "told once"
    );
    // the tag is pushed: the close needs nothing else, the build is followed on its own
    g(&x.repo, &["tag", "-a", "v1.0.0", "-m", "v1.0.0", "main"]);
    g(&x.repo, &["push", "-q", "origin", "v1.0.0"]);
    k.core.tx().st.probed.clear();
    k.core.lifecycle_tick().unwrap();
    assert!(
        k.wait(|| k.phase(&x.item) == "closed"),
        "{:?}",
        k.status(&x.item)
    );
    let i = k.db().item(&x.item).unwrap().unwrap();
    assert_eq!(i.outcome.as_deref(), Some("completed"));
    assert_eq!(
        op_states(&k, &x.item),
        vec![
            (OP_RELEASE.to_string(), "succeeded".to_string()),
            (OP_BUILD.to_string(), "running".to_string())
        ]
    );
    let t = k.timeline_texts(&x.item);
    assert!(
        t.iter()
            .any(|l| l.starts_with("Steve 核对：提交已合并到 origin/main")
                && l.contains("tag v1.0.0 已推送")),
        "{t:?}"
    );
    assert!(
        t.iter()
            .any(|l| l.contains("开始单独跟踪 v1.0.0 的 Release 构建"))
    );
}

#[test]
fn stopped_after_merging_and_tagging_it_completes_on_steves_check() {
    let k = kit(2);
    let x = remote(&k, upstream::DONE_TAG, false);
    // delivered once, then the delivery went void (ST-25: the branch changed after the freeze)
    k.core.tx().lc_invalidate(&x.item, "有新的交付").unwrap();
    merge_and_tag(&x, Some("v0.2.21"));
    // ST-25: the agent reports the merge and the tag, the build still running, and stops
    stop(
        &k,
        &x.session,
        "PR #1 已经合并到 main，v0.2.21 的 tag 也已推送，GitHub 正在构建发布包，还没出结果。",
    );
    assert!(
        k.wait(|| k.phase(&x.item) == "closed"),
        "{:?}",
        k.status(&x.item)
    );
    let i = k.db().item(&x.item).unwrap().unwrap();
    assert_eq!(
        (i.outcome.as_deref(), i.acceptance.as_deref()),
        (Some("completed"), Some("automatic"))
    );
    assert!(k.open_fails(&x.item).is_empty(), "nothing for you");
    assert!(
        k.timeline_texts(&x.item)
            .iter()
            .any(|l| l.contains("按约定（PR 合并到默认分支、并推送包含它的 tag）判定完成"))
    );
    assert!(steve_lines(&k, &x.item).is_empty(), "no continue needed");
}

#[test]
fn stopped_before_the_merge_it_is_continued_with_what_steve_found() {
    let k = kit(2);
    let x = remote(&k, upstream::DONE_MERGE, false);
    k.core.tx().lc_invalidate(&x.item, "有新的交付").unwrap();
    stop(&k, &x.session, "分支推上去了。");
    // the line and the prompt are written one after the other: wait for both
    assert!(k.wait(|| {
        !steve_lines(&k, &x.item).is_empty() && !k.rt.prompts_of(&x.session).is_empty()
    }));
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(
        p.contains("Steve 刚核对过远端：提交还不在 origin/main 上（PR 还没合并）"),
        "{p}"
    );
    assert_eq!(k.phase(&x.item), "active");
    assert!(k.open_fails(&x.item).is_empty());
}

#[test]
fn an_accepted_delivery_that_moved_tells_the_agent_what_moved() {
    let k = kit(2);
    let x = remote(&k, upstream::DONE_TAG, false);
    // delivered again on the real worktree (the frozen fingerprint is the branch's)
    k.core.tx().lc_invalidate(&x.item, "有新的交付").unwrap();
    let d = k.decision(
        &x.item,
        Some(&x.session),
        "accept",
        "item",
        &["pass", "rework"],
    );
    k.core
        .tx()
        .lc_request_complete(
            &x.item,
            "dev",
            cycle::Policy::Human,
            false,
            "交付",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    k.core.resolve(&d, "pass", "", "me").unwrap();
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
    // a rebase onto a newer main is not a new result
    commit(&x.repo, "m.txt", "m\n");
    g(&x.wt, &["rebase", "-q", "main"]);
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.phase(&x.item), "closing");
    // release notes committed after the acceptance are: the agent hears which file
    commit(&x.wt, "notes.md", "n\n");
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
    let i = k.db().item(&x.item).unwrap().unwrap();
    assert_eq!(i.phase, "active");
    let t = k.timeline_texts(&x.item);
    assert!(
        t.contains(
            &"收尾作废：冻结之后结果又变了：分支换到了新的基线，改了 notes.md，需要重新交付"
                .to_string()
        ),
        "{t:#?}"
    );
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(
        p.starts_with(
            "（Steve）你上一次的交付已经作废：冻结之后结果又变了：分支换到了新的基线，改了 notes.md"
        ),
        "{p}"
    );
    assert_eq!(steve_lines(&k, &x.item).len(), 1);
}

#[test]
fn a_void_delivery_waiting_on_your_approval_is_told_with_the_answer() {
    let k = kit(2);
    let (x, wt) = super::lifecycle::delivered_in_git(&k, true);
    k.core.tx().lc_invalidate(&x.id, "有新的交付").unwrap();
    let d = k.decision(
        &x.id,
        Some(&x.session),
        "accept",
        "item",
        &["pass", "rework"],
    );
    k.core
        .tx()
        .lc_request_complete(
            &x.id,
            "dev",
            cycle::Policy::Human,
            false,
            "交付",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    k.core.resolve(&d, "pass", "", "me").unwrap();
    commit(&wt, "notes.md", "n\n");
    // the agent asks to push and waits for you
    let a = k.decision(&x.id, Some(&x.session), "approve", "action", &["yes", "no"]);
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.phase(&x.id), "active");
    assert!(
        steve_lines(&k, &x.id).is_empty(),
        "not continued while it waits for you"
    );
    k.core.resolve(&a, "yes", "", "me").unwrap();
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(p.starts_with("（Steve）你上一次的交付已经作废"), "{p}");
}

/* ── the build, on its own ── */

#[test]
fn a_build_is_followed_once_and_only_a_failure_is_told() {
    let gh = tempfile::tempdir().unwrap();
    let script = gh.path().join("gh");
    let answer = gh.path().join("runs.json");
    std::fs::write(&script, format!("#!/bin/sh\ncat '{}'\n", answer.display())).unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let k = Kit::with(|c| {
        c.takeover_continues = 2;
        c.gh = script.clone();
    });
    let x = remote(&k, upstream::DONE_TAG, false);
    k.core.tx().lc_invalidate(&x.item, "有新的交付").unwrap();
    merge_and_tag(&x, Some("v2.0.0"));
    stop(&k, &x.session, "合并了，tag 推了。");
    assert!(k.wait(|| k.phase(&x.item) == "closed"));
    let commit = g(&x.repo, &["rev-parse", "main"]);
    let run = |status: &str, conclusion: &str| {
        json!([{"status": status, "conclusion": conclusion, "headSha": commit, "url": "https://ci/1", "databaseId": 1, "workflowName": "Release"}]).to_string()
    };
    std::fs::write(&answer, run("in_progress", "")).unwrap();
    assert_eq!(k.core.check_builds().unwrap(), 1);
    // looked at a minute ago: not again yet
    assert_eq!(k.core.check_builds().unwrap(), 0);
    assert_eq!(op_states(&k, &x.item)[1].1, "running");
    std::fs::write(&answer, run("completed", "failure")).unwrap();
    k.core.tx().st.build_checked.clear();
    assert_eq!(k.core.check_builds().unwrap(), 1);
    assert_eq!(op_states(&k, &x.item)[1].1, "failed");
    let lines = |k: &Kit| {
        k.timeline_texts(&x.item)
            .into_iter()
            .filter(|l| l.contains("Release 构建失败"))
            .count()
    };
    assert_eq!(lines(&k), 1);
    // a finished build is not looked at again, and the failure is not told twice
    k.core.tx().st.build_checked.clear();
    assert_eq!(k.core.check_builds().unwrap(), 0);
    assert_eq!(lines(&k), 1);
    // the item stays completed: the build never decided it
    assert_eq!(
        k.db().item(&x.item).unwrap().unwrap().outcome.as_deref(),
        Some("completed")
    );
}

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
            attempt: None,
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
            attempt: None,
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

/// ST-49, changed 2026-11-09: the agent stopped to lay out a plan (a heading says so), far above its
/// last words. It is reminded first that it can deliver, or ask with steve_ask, instead of a card.
#[test]
fn a_plan_it_stopped_to_report_is_reminded_first() {
    let k = kit(2);
    let (item, sid) = started(&k);
    let filler = "这一段是背景说明。".repeat(80);
    stop(
        &k,
        &sid,
        &format!("## 方案\n1. 先把分支推到 GitHub\n2. 开 PR 等 CI\n\n{filler}\n以上。"),
    );
    assert!(k.open_fails(&item).is_empty(), "no card yet");
    assert_eq!(steve_lines(&k, &item).len(), 1, "reminded");
    let p = k.rt.prompts_of(&sid).pop().unwrap();
    assert!(p.contains("steve_ask、steve_deliver 仍然可用"), "{p}");
    assert!(p.contains("用 steve_deliver 交付"), "{p}");
    assert!(p.contains("等用户确认的内容"), "{p}");
}

/// ST-54: the agent stopped with a list of things for you to confirm.
#[test]
fn something_it_stopped_for_you_to_confirm_is_for_you() {
    let k = kit(2);
    let (item, sid) = started(&k);
    stop(
        &k,
        &sid,
        "改好了一半。\n\n**待确认**\n- 旧接口是否直接删掉\n- 迁移要不要保留回退",
    );
    assert_eq!(k.open_fails(&item).len(), 1);
    assert!(steve_lines(&k, &item).is_empty());
}

/// You asked the item something from the conversation: its answer ends the turn on purpose.
#[test]
fn an_answer_to_what_you_asked_from_the_conversation_is_not_continued() {
    let k = kit(2);
    let (item, sid) = started(&k);
    // the first turn was cut short by you: the session waits, nothing open
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    k.core
        .forward(&item, "现在进展到哪了？", &[], "m_1")
        .unwrap();
    assert!(
        k.rt.prompts_of(&sid)
            .iter()
            .any(|p| p.contains("现在进展到哪了"))
    );
    stop(&k, &sid, "测试都过了，还差文档。");
    assert!(steve_lines(&k, &item).is_empty(), "not continued");
    assert_eq!(k.open_fails(&item).len(), 1);
}

/// A plan left in plain text is reminded, and so is the silent stop after it: each within the limit.
#[test]
fn a_plan_and_a_later_silent_stop_are_each_reminded() {
    let k = kit(2);
    let (item, sid) = started(&k);
    stop(&k, &sid, "## 方案\n1. 先改接口\n2. 再补测试");
    assert_eq!(steve_lines(&k, &item).len(), 1);
    stop(&k, &sid, "还在改。");
    assert_eq!(steve_lines(&k, &item).len(), 2, "continued");
    assert!(k.open_fails(&item).is_empty());
}

/// An instruction you sent it from the conversation is not a question: its turn is continued.
#[test]
fn an_instruction_forwarded_from_the_conversation_is_still_continued() {
    let k = kit(2);
    let (item, sid) = started(&k);
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    k.core
        .forward(&item, "再把按钮改成蓝色", &[], "m_1")
        .unwrap();
    stop(&k, &sid, "改好了一半。");
    assert_eq!(steve_lines(&k, &item).len(), 1, "continued");
    assert!(k.open_fails(&item).is_empty());
}

/// A project done by its rule (merged): the acceptance has no release to choose, so neither
/// 验收，先不发布 nor 验收并发布 from a card can waive or start what the rule waits for.
#[test]
fn a_done_when_project_ignores_the_release_choice_of_an_acceptance() {
    for text in ["no-release", crate::orchestrator::ACCEPT_AND_RELEASE] {
        let k = kit(2);
        let x = remote(&k, upstream::DONE_MERGE, false);
        k.core.resolve(&x.decision, "pass", text, "me").unwrap();
        assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
        assert_eq!(k.phase(&x.item), "closing", "{text}");
        assert_eq!(
            op_states(&k, &x.item),
            vec![(OP_RELEASE.to_string(), "pending".to_string())],
            "{text}"
        );
        assert!(k.core.tx().st.releasing.is_none(), "{text}");
        assert!(
            !k.db()
                .decisions_of_item(&x.item)
                .unwrap()
                .iter()
                .any(|d| d.subject_type == "release"),
            "{text}"
        );
    }
}

#[test]
fn only_a_heading_counts_as_a_report() {
    use crate::takeover::stopped_to_report;
    assert!(stopped_to_report("## 方案\n1. x"));
    assert!(stopped_to_report("前面\n**待你确认**\n- x"));
    assert!(stopped_to_report("修复方案：\n- x"));
    assert!(!stopped_to_report("方案已经实现了，正在跑测试。"));
    assert!(!stopped_to_report("## 测试结果\n全部通过"));
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

pub(super) struct Remote {
    pub(super) repo: PathBuf,
    pub(super) item: String,
    pub(super) session: String,
    pub(super) decision: String,
    wt: PathBuf,
}

/// A project whose completion rule is `rule`, an `origin` it pushes to (a bare repo that answers as
/// `github.com/acme/app`), and an item delivered on its own branch; `accept` = the user accepted it.
fn remote(k: &Kit, rule: &str, accept: bool) -> Remote {
    remote_for(k, rule, accept, None)
}

/// [`remote`] for an item whose goal also says `goal`.
pub(super) fn remote_for(k: &Kit, rule: &str, accept: bool, goal: Option<&str>) -> Remote {
    remote_at(k, rule, accept, goal, "https://github.com/acme/app.git")
}

/// [`remote_for`] with an `origin` that answers as `url`.
fn remote_at(k: &Kit, rule: &str, accept: bool, goal: Option<&str>, url: &str) -> Remote {
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
    if let Some(goal) = goal {
        k.db().add_goal(&x.id, goal).unwrap();
    }
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
        decision: x.decision,
        wt,
    }
}

/// What GitHub does on a rebase merge and a tag push: a copy of the branch's commits on `main`, tagged.
pub(super) fn merge_and_tag(x: &Remote, tag: Option<&str>) {
    let tip = g(&x.wt, &["rev-parse", "HEAD"]);
    g(&x.repo, &["cherry-pick", "-x", &tip]);
    g(&x.repo, &["push", "-q", "origin", "main"]);
    if let Some(t) = tag {
        g(&x.repo, &["tag", "-a", t, "-m", t, "main"]);
        g(&x.repo, &["push", "-q", "origin", t]);
    }
}

/// Waits for the first look at the remote after the acceptance to come back, before anything was
/// merged: the agent is told what is still missing, and its turn runs. Merging before that look
/// answered would race it: it may read the remote before or after the push.
pub(super) fn told_what_is_missing(k: &Kit, x: &Remote) {
    assert!(
        k.wait(|| {
            k.rt.prompts_of(&x.session)
                .last()
                .is_some_and(|p| p.contains("按这个项目的约定"))
        }),
        "{:?}",
        k.status(&x.item)
    );
}

pub(super) fn op_states(k: &Kit, item: &str) -> Vec<(String, String)> {
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
    k.core.tx().forget_probe(&x.item);
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
fn a_look_begun_before_a_turn_ended_is_not_kept_a_fresh_one_decides() {
    let k = kit(2);
    let x = remote(&k, upstream::DONE_TAG, true);
    told_what_is_missing(&k, &x);
    let seq = {
        let mut t = k.core.tx();
        // a look starts, and can't come back while this section holds the lock
        t.forget_probe(&x.item);
        t.lc_try_close(&x.item).unwrap();
        assert!(t.st.probing.contains_key(&x.item));
        let seq = t.st.probe_seq;
        // meanwhile the agent merged and tagged, and its turn ended: what that look read is old
        merge_and_tag(&x, Some("v1.0.0"));
        t.forget_probe(&x.item);
        seq
    };
    // its answer is not kept: the closing is checked again, with a look of its own (no tick needed)
    assert!(
        k.wait(|| {
            k.core.tx().st.probing.is_empty()
                && op_states(&k, &x.item)
                    .first()
                    .is_some_and(|o| o.1 == "succeeded")
        }),
        "{:?}",
        op_states(&k, &x.item)
    );
    assert_eq!(k.core.tx().st.probe_seq, seq + 1, "one fresh look");
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

/* ── what the item's own goal asks to be shown (ST-57) ── */

/// A `gh` that answers with what the returned file holds.
fn fake_gh() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let gh = tempfile::tempdir().unwrap();
    let script = gh.path().join("gh");
    let answer = gh.path().join("runs.json");
    std::fs::write(&script, format!("#!/bin/sh\ncat '{}'\n", answer.display())).unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    (gh, script, answer)
}

fn runs_json(commit: &str, status: &str, conclusion: &str) -> String {
    json!([{"status": status, "conclusion": conclusion, "headSha": commit, "url": "https://ci/7", "databaseId": 7, "workflowName": "Release"}]).to_string()
}

pub(super) fn blockers(k: &Kit, item: &str) -> Vec<String> {
    k.core.lifecycle_view(item).unwrap()["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["label"].as_str().unwrap().to_string())
        .collect()
}

/// An accepted item whose goal asks for the build to pass, merged and tagged `tag`, its build
/// followed by a `gh` that answers with the returned file.
fn tagged_with_build(tag: &str) -> (Kit, Remote, PathBuf, tempfile::TempDir) {
    let (gh, script, answer) = fake_gh();
    let k = Kit::with(|c| {
        c.takeover_continues = 2;
        c.gh = script.clone();
    });
    // ST-53: "修复 Release 流水线，并确认构建通过"
    let x = remote_for(
        &k,
        upstream::DONE_TAG,
        true,
        Some("修复偶发失败的测试，并确认构建通过"),
    );
    told_what_is_missing(&k, &x);
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(
        p.contains("事项的目标要求构建通过，构建成功后才算完成"),
        "{p}"
    );
    // merged and tagged while the agent's turn still runs: then it ends
    merge_and_tag(&x, Some(tag));
    k.core.tx().forget_probe(&x.item);
    k.core.lifecycle_tick().unwrap();
    assert!(k.wait(|| {
        let _ = k.core.lifecycle_tick();
        op_states(&k, &x.item).len() == 2
    }));
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    k.core.lifecycle_tick().unwrap();
    (k, x, answer, gh)
}

#[test]
fn a_goal_that_asks_for_the_build_to_pass_stays_open_while_it_runs_and_when_it_failed() {
    let (k, x, answer, _gh) = tagged_with_build("v1.1.0");
    // merged and tagged, the build still running: not done, and it says what it waits for
    assert_eq!(k.phase(&x.item), "closing");
    let b = blockers(&k, &x.item);
    assert_eq!(b, ["正在结束：等 Release 构建（v1.1.0）成功"], "{b:?}");
    // in the language of the request
    let en = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(steve_i18n::scope(steve_i18n::EN, async {
            blockers(&k, &x.item)
        }));
    assert_eq!(
        en,
        ["Ending: waiting for the Release build (v1.1.0) to pass"]
    );
    assert!(
        k.timeline_texts(&x.item)
            .iter()
            .any(|l| l.contains("事项目标要求构建通过，构建成功后才算完成"))
    );
    // the build fails: the agent hears it, you hear it, the item stays open
    let commit = g(&x.repo, &["rev-parse", "main"]);
    std::fs::write(&answer, runs_json(&commit, "completed", "failure")).unwrap();
    assert_eq!(k.core.check_builds().unwrap(), 1);
    assert_eq!(k.phase(&x.item), "closing");
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(
        p.contains("Release 构建失败") && p.contains("目标要求构建通过"),
        "{p}"
    );
    assert_eq!(
        k.status(&x.item).2,
        "Release 构建没有通过，事项目标要求构建通过"
    );
    assert!(
        blockers(&k, &x.item)
            .iter()
            .any(|b| b == "Release 构建（v1.1.0）失败")
    );
    // you decide it needs no build after all: marking it done is the evidence
    k.finish(&x.item).unwrap();
    k.rt.end_turn(&k.core, &x.session, "cancelled", None);
    k.core.tx().forget_probe(&x.item);
    k.core.lifecycle_tick().unwrap();
    assert!(
        k.wait(|| {
            let _ = k.core.lifecycle_tick();
            k.phase(&x.item) == "closed"
        }),
        "{:?} {:?}",
        k.status(&x.item),
        blockers(&k, &x.item)
    );
}

#[test]
fn a_goal_that_asks_for_the_build_to_pass_closes_once_it_passed() {
    let (k, x, answer, _gh) = tagged_with_build("v1.2.0");
    assert_eq!(k.phase(&x.item), "closing");
    let commit = g(&x.repo, &["rev-parse", "main"]);
    std::fs::write(&answer, runs_json(&commit, "in_progress", "")).unwrap();
    assert_eq!(k.core.check_builds().unwrap(), 1);
    assert_eq!(k.phase(&x.item), "closing");
    std::fs::write(&answer, runs_json(&commit, "completed", "success")).unwrap();
    k.core.tx().st.build_checked.clear();
    assert_eq!(k.core.check_builds().unwrap(), 1);
    assert_eq!(k.phase(&x.item), "closed");
    assert_eq!(
        op_states(&k, &x.item),
        vec![
            (OP_RELEASE.to_string(), "succeeded".to_string()),
            (OP_BUILD.to_string(), "succeeded".to_string())
        ]
    );
}

#[test]
fn a_required_build_without_a_result_in_time_goes_back_to_the_agent() {
    let (k, x, answer, _gh) = tagged_with_build("v1.4.0");
    // started more than 3 hours ago, still running
    let i = k.db().item(&x.item).unwrap().unwrap();
    let op = k
        .db()
        .store()
        .items()
        .operations(&x.item, i.cycle)
        .unwrap()
        .into_iter()
        .find(|o| o.kind == OP_BUILD)
        .unwrap();
    assert!(op.required);
    let mut ev = op.evidence.clone();
    ev["since"] = json!(crate::util::now() - 4 * 3_600_000);
    k.db()
        .store()
        .transaction(|tx| steve_store::operation_set_in(tx, &op.id, "running", &ev))
        .unwrap();
    let commit = g(&x.repo, &["rev-parse", "main"]);
    std::fs::write(&answer, runs_json(&commit, "in_progress", "")).unwrap();
    assert_eq!(k.core.check_builds().unwrap(), 1);
    assert_eq!(
        op_states(&k, &x.item)[1].1,
        "failed",
        "giving up is no pass"
    );
    assert_eq!(k.phase(&x.item), "closing");
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(
        p.contains("超过 3 小时还没结束") && p.contains("修好"),
        "{p}"
    );
}

#[test]
fn a_goal_that_asks_for_this_instance_waits_until_it_runs_the_tag() {
    for (version, closes) in [("0.2.35", false), ("1.3.0", true)] {
        let k = Kit::with(|c| {
            c.takeover_continues = 2;
            c.steve_repo = c.home.join("repo");
            c.version = version.into();
        });
        // ST-55: the upgrade entry, on the instance the user runs
        let x = remote_for(&k, upstream::DONE_TAG, true, Some("恢复当前实例的升级入口"));
        let i = k.db().item(&x.item).unwrap().unwrap();
        let line = k.core.tx().proof_line(&i).unwrap().unwrap();
        assert!(line.contains("runs on this Steve instance"), "{line}");
        told_what_is_missing(&k, &x);
        merge_and_tag(&x, Some("v1.3.0"));
        k.core.tx().forget_probe(&x.item);
        k.core.lifecycle_tick().unwrap();
        assert!(k.wait(|| {
            let _ = k.core.lifecycle_tick();
            !op_states(&k, &x.item).is_empty() && op_states(&k, &x.item)[0].1 == "succeeded"
        }));
        k.rt.end_turn(&k.core, &x.session, "end_turn", None);
        k.core.lifecycle_tick().unwrap();
        let i = k.db().item(&x.item).unwrap().unwrap();
        let deploy = k
            .db()
            .store()
            .items()
            .operations(&x.item, i.cycle)
            .unwrap()
            .into_iter()
            .find(|o| o.kind == crate::proof::OP_DEPLOY)
            .expect("the deploy requirement is recorded");
        assert_eq!(deploy.label, "本机运行包含 v1.3.0 的版本");
        if closes {
            assert_eq!(
                k.phase(&x.item),
                "closed",
                "{version} {:?} {:?} {:?}",
                blockers(&k, &x.item),
                op_states(&k, &x.item),
                deploy
            );
            assert_eq!(deploy.status, "succeeded");
            assert!(
                k.timeline_texts(&x.item)
                    .iter()
                    .any(|l| l.contains("本机已运行 1.3.0"))
            );
            continue;
        }
        assert_eq!(k.phase(&x.item), "closing", "{version}");
        assert_eq!(deploy.status, "pending");
        assert_eq!(k.status(&x.item).2, "等本机运行包含 v1.3.0 的版本");
        assert_eq!(
            blockers(&k, &x.item),
            ["正在结束：等本机运行包含 v1.3.0 的版本"]
        );
        // looked at again on every tick, said once
        k.core.lifecycle_tick().unwrap();
        let t = k.timeline_texts(&x.item);
        let lines: Vec<_> = t
            .iter()
            .filter(|l| l.starts_with("事项目标要求在本机生效：本机现在运行 0.2.35"))
            .collect();
        assert_eq!(lines.len(), 1, "said once: {t:#?}");
        // you mark it done yourself: that is the evidence
        k.finish(&x.item).unwrap();
        assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
        k.core.lifecycle_tick().unwrap();
        assert!(
            k.wait(|| k.phase(&x.item) == "closed"),
            "{:?}",
            k.status(&x.item)
        );
    }
}

#[test]
fn another_project_is_not_checked_against_this_instance() {
    let k = Kit::with(|c| {
        c.takeover_continues = 2;
        c.version = "0.0.1".into();
    });
    let x = remote_for(&k, upstream::DONE_TAG, true, Some("部署到线上环境"));
    let i = k.db().item(&x.item).unwrap().unwrap();
    let line = k.core.tx().proof_line(&i).unwrap().unwrap();
    assert!(line.contains("check it there"), "{line}");
    told_what_is_missing(&k, &x);
    merge_and_tag(&x, Some("v1.0.0"));
    // the agent's turn ends: the remote is looked at afresh in the background, and a tick after
    // it answered closes the item
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert!(
        k.wait(|| {
            let _ = k.core.lifecycle_tick();
            k.phase(&x.item) == "closed"
        }),
        "{:?}",
        k.status(&x.item)
    );
}

#[test]
fn a_goal_that_asks_for_an_actual_operation_is_noted_when_the_delivery_lacks_one() {
    let k = kit(2);
    k.manual_acceptance();
    let x = remote_for(
        &k,
        upstream::DONE_TAG,
        false,
        Some("验证检查更新、升级备份与重启"),
    );
    let s = k.db().session(&x.session).unwrap().unwrap();
    let check = |name: &str, operated: bool| crate::Check {
        name: name.into(),
        passed: true,
        operated,
    };
    /* 2026-11-09: not refused; the card tells you what the goal asked for and is missing */
    k.core
        .tool_deliver(&s, "ok", &[], &[check("cargo test", false)], &[])
        .unwrap();
    let d = k
        .db()
        .decisions_of_item(&x.item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "accept")
        .unwrap();
    assert!(d.draft.contains("注意：目标要求"), "{}", d.draft);
    assert!(d.draft.contains("没有一条是实际操作"), "{}", d.draft);
    k.core
        .tool_deliver(
            &s,
            "ok",
            &[],
            &[
                check("cargo test", false),
                check("设置·关于点检查更新，显示 v1.0.1", true),
            ],
            &[],
        )
        .unwrap();
    let d = k
        .db()
        .decisions_of_item(&x.item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "accept")
        .unwrap();
    assert!(
        d.draft
            .contains("✓ 实际操作：设置·关于点检查更新，显示 v1.0.1"),
        "{}",
        d.draft
    );
    assert!(!d.draft.contains("注意："), "{}", d.draft);
    // the protocol says what the goal asks for
    let i = k.db().item(&x.item).unwrap().unwrap();
    let line = k.core.tx().proof_line(&i).unwrap().unwrap();
    assert!(line.contains("`operated: true`"), "{line}");
    // stopped after merging and tagging: the remote alone is not that evidence, the agent goes on
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    k.core.tx().lc_invalidate(&x.item, "有新的交付").unwrap();
    merge_and_tag(&x, Some("v1.0.1"));
    stop(&k, &x.session, "合并了，tag 推了。");
    assert!(k.wait(|| !steve_lines(&k, &x.item).is_empty()));
    assert_eq!(k.phase(&x.item), "active");
    // the timeline line comes first, the prompt to the agent a moment later
    assert!(
        k.wait(|| k
            .rt
            .prompts_of(&x.session)
            .last()
            .is_some_and(|p| p.contains("还要求实际操作验证"))),
        "{:?}",
        k.rt.prompts_of(&x.session).last()
    );
}

/// An item whose goal asks for the build to pass, delivered in a project with `rule` on `url`,
/// back to work: the agent stops after merging (and tagging `tag`) without delivering again.
fn stopped_after_merging(rule: &str, url: &str, tag: Option<&str>) -> (Kit, Remote) {
    let k = kit(2);
    // ST-53: "修复偶发失败的测试，并确认构建通过"
    let x = remote_at(
        &k,
        rule,
        false,
        Some("修复偶发失败的测试，并确认构建通过"),
        url,
    );
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    k.core.tx().lc_invalidate(&x.item, "有新的交付").unwrap();
    merge_and_tag(&x, tag);
    let before = k.rt.prompts_of(&x.session).len();
    stop(&k, &x.session, "合并了。");
    // the timeline line comes first, the prompt to the agent a moment later: wait for both
    assert!(k.wait(|| {
        !steve_lines(&k, &x.item).is_empty() && k.rt.prompts_of(&x.session).len() > before
    }));
    (k, x)
}

#[test]
fn a_goal_that_asks_for_the_build_to_pass_is_noted_when_the_delivery_lacks_a_build() {
    let k = kit(2);
    k.manual_acceptance();
    let x = remote_for(&k, upstream::DONE_MERGE, false, Some("确认构建通过"));
    let s = k.db().session(&x.session).unwrap().unwrap();
    let check = |name: &str, passed: bool| crate::Check {
        name: name.into(),
        passed,
        operated: false,
    };
    for checks in [
        vec![],
        vec![check("cargo test", true)],
        vec![check("构建：cargo build", false)],
    ] {
        k.core.tool_deliver(&s, "ok", &[], &checks, &[]).unwrap();
        let d = k
            .db()
            .decisions_of_item(&x.item)
            .unwrap()
            .into_iter()
            .find(|d| d.status == "open" && d.kind == "accept")
            .unwrap();
        assert!(d.draft.contains("目标要求构建通过"), "{}", d.draft);
    }
    k.core
        .tool_deliver(
            &s,
            "ok",
            &[],
            &[
                check("cargo test", true),
                check("构建：cargo build --release", true),
            ],
            &[],
        )
        .unwrap();
    let i = k.db().item(&x.item).unwrap().unwrap();
    let line = k.core.tx().proof_line(&i).unwrap().unwrap();
    assert!(
        line.contains("a passed check for the build you ran"),
        "{line}"
    );
    assert!(!line.contains("Release build"), "{line}");
}

#[test]
fn a_build_goal_in_a_merge_rule_project_is_not_closed_on_the_merge_alone() {
    let (k, x) = stopped_after_merging(
        upstream::DONE_MERGE,
        "https://github.com/acme/app.git",
        None,
    );
    assert_eq!(k.phase(&x.item), "active", "{:?}", k.status(&x.item));
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(
        p.contains("还要求构建通过") && p.contains("合并就算完成，Steve 不跟踪构建"),
        "{p}"
    );
}

#[test]
fn a_build_goal_whose_tag_build_cant_be_followed_rests_on_the_delivery() {
    let url = "https://git.example.com/acme/app.git";
    // stopped after merging and tagging: no Release build to follow, so not closed on the tag alone
    let (k, x) = stopped_after_merging(upstream::DONE_TAG, url, Some("v2.0.0"));
    assert_eq!(k.phase(&x.item), "active", "{:?}", k.status(&x.item));
    let p = k.rt.prompts_of(&x.session).pop().unwrap();
    assert!(
        p.contains("还要求构建通过") && p.contains("跟踪不了 tag 触发的 Release 构建"),
        "{p}"
    );
    assert!(!p.contains("构建成功后才算完成"), "{p}");
    let i = k.db().item(&x.item).unwrap().unwrap();
    let line = k.core.tx().proof_line(&i).unwrap().unwrap();
    assert!(
        line.contains("a passed check for the build you ran") && line.contains("on GitHub"),
        "{line}"
    );
    // an accepted delivery (with its build check) closes on the tag, and says what stood for the build
    let k = kit(2);
    let x = remote_at(&k, upstream::DONE_TAG, true, Some("确认构建通过"), url);
    told_what_is_missing(&k, &x);
    merge_and_tag(&x, Some("v2.1.0"));
    k.core.tx().forget_probe(&x.item);
    k.core.lifecycle_tick().unwrap();
    assert!(k.wait(|| {
        let _ = k.core.lifecycle_tick();
        !op_states(&k, &x.item).is_empty() && op_states(&k, &x.item)[0].1 == "succeeded"
    }));
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert!(
        k.wait(|| {
            let _ = k.core.lifecycle_tick();
            k.phase(&x.item) == "closed"
        }),
        "{:?} {:?}",
        k.status(&x.item),
        blockers(&k, &x.item)
    );
    assert_eq!(
        op_states(&k, &x.item),
        vec![(OP_RELEASE.to_string(), "succeeded".to_string())]
    );
    assert!(k.timeline_texts(&x.item).iter().any(|l| l.contains(
        "跟踪不了 v2.1.0 的 Release 构建；事项目标要求的构建通过以交付里的构建检查为准"
    )));
}

fn lane_request(k: &Kit, item: &str, state: &str) {
    use steve_store::lanes::{self, NewRequest};
    k.db()
        .store()
        .transaction(|tx| {
            let lane = lanes::lane_for(tx, "github.com/x/y", "main")?;
            let r = lanes::request_add(
                tx,
                &NewRequest {
                    lane_id: &lane.id,
                    workspace_id: Some(WS),
                    item_id: Some(item),
                    source: "item",
                    dir: "/nonexistent",
                    branch: "steve/item",
                    head: "abc123",
                    repo: None,
                },
            )?;
            lanes::request_set(tx, &r.id, state, None, None, None)
        })
        .unwrap();
}

#[test]
fn lane_wait_does_not_consume_continues_or_open_stall_cards() {
    for state in ["queued", "collecting", "merging", "merged"] {
        let k = kit(2);
        let (item, sid) = started(&k);
        lane_request(&k, &item, state);
        let before = k.rt.prompts_of(&sid).len();
        stop(&k, &sid, "已提交队列，等待发布。");
        let later = crate::util::now() + crate::stall::STALL_AFTER_MS + 1_000;
        for _ in 0..3 {
            assert!(k.core.tx().check_stalls(later).unwrap().is_empty());
        }
        k.core
            .tx()
            .takeover_invalidated(&item, "计划已更新")
            .unwrap();
        assert_eq!(k.rt.prompts_of(&sid).len(), before, "{state}");
        assert!(steve_lines(&k, &item).is_empty(), "{state}");
        assert!(k.open_fails(&item).is_empty(), "{state}");
    }
}

#[test]
fn a_finished_or_returned_lane_request_does_not_silence_takeover() {
    for state in ["released", "left"] {
        let k = kit(2);
        let (item, sid) = started(&k);
        lane_request(&k, &item, state);
        stop(&k, &sid, "这轮结束。");
        assert_eq!(steve_lines(&k, &item).len(), 1, "{state}");
    }
}

#[test]
fn lane_wait_preserves_questions_and_manual_interrupts() {
    let k = kit(2);
    let (item, sid) = started(&k);
    lane_request(&k, &item, "merged");
    stop(&k, &sid, "这项需要改目标吗？");
    assert_eq!(k.open_fails(&item).len(), 1);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    lane_request(&k, &i, "collecting");
    k.db()
        .event_add(&s, "system", json!({"text": "你中断了当前回合"}))
        .unwrap();
    let later = crate::util::now() + crate::stall::STALL_AFTER_MS + 1_000;
    assert_eq!(k.core.tx().check_stalls(later).unwrap(), vec![i]);
    assert!(k.rt.prompts_of(&s).is_empty());
}

#[test]
fn persisted_lane_wait_survives_a_new_core_and_accepts_user_work() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let sid = k.session(&i, "idle");
    lane_request(&k, &i, "merged");
    let core = crate::Core::new(
        k.db().clone(),
        k.rt.clone(),
        k.gw.clone(),
        k.core.config().clone(),
    );
    let later = crate::util::now() + crate::stall::STALL_AFTER_MS + 1_000;
    assert!(core.tx().check_stalls(later).unwrap().is_empty());
    assert!(k.rt.prompts_of(&sid).is_empty());
    assert_eq!(
        core.session_message(&sid, "继续修正文档", false, &[])
            .unwrap(),
        crate::Prompted::Sent
    );
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "继续修正文档");
}

#[test]
fn a_lane_entered_while_the_remote_probe_runs_owns_the_wait() {
    let k = kit(2);
    let x = remote(&k, upstream::DONE_MERGE, false);
    k.core.tx().lc_invalidate(&x.item, "有新的交付").unwrap();
    k.rt.st().turning.clear();
    let before = k.rt.prompts_of(&x.session).len();
    {
        // Hold the core lock so the probe cannot return until the persisted request exists.
        let mut tx = k.core.tx();
        assert!(
            tx.takeover(
                &x.item,
                &x.session,
                crate::takeover::Stop::Idle {
                    why: "等待核对".into(),
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(!tx.st.probing.is_empty());
        lane_request(&k, &x.item, "collecting");
    }
    assert!(k.wait(|| k.core.tx().st.probing.is_empty()));
    assert_eq!(k.rt.prompts_of(&x.session).len(), before);
    assert!(steve_lines(&k, &x.item).is_empty());
    assert!(k.open_fails(&x.item).is_empty());
}

/// 2026-11-09: a turn the harness failed is continued (within the same limit) before it is yours,
/// told to check before redoing anything; one whose outcome nobody knows is never continued.
#[test]
fn a_failed_turn_is_continued_first_an_unknown_one_is_yours() {
    let k = kit(1);
    let (item, sid) = started(&k);
    k.rt.end_turn(&k.core, &sid, "error", Some("adapter exited"));
    assert!(k.open_fails(&item).is_empty(), "no card yet");
    assert_eq!(steve_lines(&k, &item).len(), 1);
    let p = k.rt.prompts_of(&sid).pop().unwrap();
    assert!(p.contains("上一回合出错中断了：adapter exited"), "{p}");
    assert!(p.contains("不要重做"), "{p}");
    k.rt.end_turn(&k.core, &sid, "error", Some("adapter exited"));
    assert_eq!(k.open_fails(&item).len(), 1, "out of continues: yours");

    let (item, sid) = started(&k);
    k.rt.end_turn(&k.core, &sid, "abandoned", Some("abandoned"));
    assert!(steve_lines(&k, &item).is_empty(), "not continued");
    assert_eq!(k.open_fails(&item), vec!["会话出错：abandoned"]);
}

/// U10: a session that cannot go on (its Node events set aside) hands its item to a new session,
/// which opens with the conversation and the reminder; the old one ends.
#[test]
fn a_session_that_cannot_go_on_continues_in_a_new_one() {
    let k = kit(1);
    let (item, sid) = started(&k);
    let ns = k.core.continue_in_new_session(&sid, "u").unwrap();
    assert_ne!(ns, sid);
    assert_eq!(k.db().session(&sid).unwrap().unwrap().status, "ended");
    let active = k.db().sessions_of_item(&item).unwrap().pop().unwrap();
    assert_eq!(active.id, ns);
    let p = k.rt.prompts_of(&ns).pop().unwrap();
    assert!(p.contains("结果未知") && p.contains("不要重做"), "{p}");
}

/// B6: a turn its Node's restart cut (given up by the Hub, the harness gone) is continued once,
/// told to check first; within the same limit as every other continue.
#[test]
fn a_turn_a_node_restart_cut_is_continued() {
    let k = kit(1);
    let (item, sid) = started(&k);
    k.rt.end_turn(
        &k.core,
        &sid,
        steve_node_api::NODE_RESTARTED,
        Some("abandoned"),
    );
    assert!(k.open_fails(&item).is_empty(), "no card");
    let p = k.rt.prompts_of(&sid).pop().unwrap();
    assert!(p.contains("Node 重启了"), "{p}");
    assert!(p.contains("不要重做") && p.contains("steve_deliver"), "{p}");
    k.rt.end_turn(
        &k.core,
        &sid,
        steve_node_api::NODE_RESTARTED,
        Some("abandoned"),
    );
    assert_eq!(k.open_fails(&item).len(), 1, "out of continues: yours");
}

/// B5: a session whose device has been offline long, with a message waiting, goes on in a new
/// session that carries the conversation and the waiting message; nothing waiting, nothing moves.
#[test]
fn a_session_on_a_long_offline_device_moves_to_a_new_one() {
    let k = kit(1);
    let (item, sid) = started(&k);
    assert_eq!(
        k.core.hand_over_from_offline(&sid, "laptop", 45).unwrap(),
        None
    );
    k.db()
        .queue_add(&sid, "还要加一个测试-W3", "u", &[])
        .unwrap();
    let ns = k
        .core
        .hand_over_from_offline(&sid, "laptop", 45)
        .unwrap()
        .expect("moved");
    assert_eq!(k.db().session(&sid).unwrap().unwrap().status, "ended");
    let active = k.db().sessions_of_item(&item).unwrap().pop().unwrap();
    assert_eq!(active.id, ns);
    let p = k.rt.prompts_of(&ns).pop().unwrap();
    assert!(p.contains("laptop") && p.contains("45 分钟"), "{p}");
    assert!(
        p.contains("还要加一个测试-W3"),
        "the waiting message goes along: {p}"
    );
    assert!(
        p.contains("没有推送出去的改动") && p.contains("不要重做"),
        "{p}"
    );
}

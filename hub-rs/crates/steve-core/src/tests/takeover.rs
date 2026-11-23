//! Steve looks first (crate::takeover), with ST-25 as the case: an agent that merged the PR and pushed
//! the tag, then stopped while the Release build ran; a delivery that went void because the branch was
//! rebased, reset onto the merged commit, or got one more commit after it was frozen.

use std::path::Path;

use crate::git::tests::{branch_with, g, repo};
use crate::repo::WS;
use crate::runtime::TurnEnd;
use crate::takeover::{LINE, STEVE_TRIED};
use crate::testkit::Kit;
use crate::{cycle, upstream};

use crate::{AgentRuntime, CreateItem};

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
fn provider_stream_interruption_never_spends_the_continue_budget_and_deduplicates_the_card() {
    let k = kit(2);
    let (item, sid) = started(&k);
    let error =
        "已自动重试 2 次仍失败：stream disconnected before completion: upstream stream interrupted";
    k.rt.end_turn(
        &k.core,
        &sid,
        crate::runtime::MODEL_STREAM_INTERRUPTED,
        Some(error),
    );
    assert!(steve_lines(&k, &item).is_empty());
    assert_eq!(k.rt.prompts_of(&sid).len(), 1);
    assert_eq!(k.open_fails(&item), vec![format!("模型服务中断：{error}")]);
    for _ in 0..3 {
        k.core
            .on_turn_end(TurnEnd {
                session_id: sid.clone(),
                attempt: None,
                stop_reason: crate::runtime::MODEL_STREAM_INTERRUPTED.into(),
                error: Some(error.into()),
            })
            .unwrap();
        k.core.check_stalls().unwrap();
    }
    assert_eq!(k.open_fails(&item).len(), 1);
    assert!(steve_lines(&k, &item).is_empty());
    // A genuine stop still has both ordinary continues available.
    let d = k.db().decisions_of_item(&item).unwrap().pop().unwrap();
    k.core.resolve(&d.id, "continue", "", "me").unwrap();
    stop(&k, &sid, "继续工作");
    stop(&k, &sid, "继续工作");
    assert_eq!(steve_lines(&k, &item).len(), 2);
    assert!(k.open_fails(&item).is_empty());
}

#[test]
fn provider_capacity_never_spends_continues_and_keeps_one_actionable_card() {
    let k = kit(2);
    let (item, sid) = started(&k);
    let error = "已自动重试 3 次仍失败；预计恢复时间：2026-11-18T08:02:00Z；503 no eligible upstream account";
    k.rt.end_turn(
        &k.core,
        &sid,
        crate::runtime::MODEL_SERVICE_UNAVAILABLE,
        Some(error),
    );
    assert!(steve_lines(&k, &item).is_empty());
    assert_eq!(k.rt.prompts_of(&sid).len(), 1);
    assert_eq!(
        k.open_fails(&item),
        vec![format!("模型服务容量不足：{error}")]
    );
    for _ in 0..3 {
        k.core
            .on_turn_end(TurnEnd {
                session_id: sid.clone(),
                attempt: None,
                stop_reason: crate::runtime::MODEL_SERVICE_UNAVAILABLE.into(),
                error: Some(error.into()),
            })
            .unwrap();
        k.core.check_stalls().unwrap();
    }
    assert_eq!(k.open_fails(&item).len(), 1);
    assert!(steve_lines(&k, &item).is_empty());
    let d = k.db().decisions_of_item(&item).unwrap().pop().unwrap();
    k.core.resolve(&d.id, "continue", "", "me").unwrap();
    stop(&k, &sid, "继续工作");
    stop(&k, &sid, "继续工作");
    assert_eq!(steve_lines(&k, &item).len(), 2);
    assert!(k.open_fails(&item).is_empty());
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
    k.rt.prompt(&t, "做事", "me", &[]).unwrap();
    k.rt.end_turn(&k.core, &t, "cancelled", None);
    let asked = k.rt.prompts_of(&t).len();
    assert_eq!(k.core.tx().check_stalls(later).unwrap(), vec![j.clone()]);
    assert_eq!(k.rt.prompts_of(&t).len(), asked, "not continued");
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

/* ── a branch with no commit of its own is not a merged change (#218) ── */

/// [`remote`] whose branch is back on `main`: nothing of the item's own on it.
fn deliver_refused(k: &Kit, sid: &str, checks: &[crate::Check]) -> String {
    let s = k.db().session(sid).unwrap().unwrap();
    k.core
        .tool_deliver(&s, "做完了", &[], checks, &[])
        .unwrap_err()
        .to_string()
}

/// P0-7: a refused delivery is read back on the next continue, and the same reason twice in a row is
/// yours rather than a third attempt the agent would only have refused again.
#[test]
fn the_same_refusal_twice_hands_the_item_to_you() {
    let k = kit(5);
    let (item, sid) = started(&k);
    k.db().store().items().set_design_gate(&item, true).unwrap();
    let why = deliver_refused(&k, &sid, &[]);
    stop(&k, &sid, "已经交付。");
    assert!(k.open_fails(&item).is_empty());
    let p = k.rt.prompts_of(&sid).pop().unwrap();
    assert!(p.contains(&format!("被拒绝了，原因：{why}")), "{p}");
    assert_eq!(deliver_refused(&k, &sid, &[]), why);
    stop(&k, &sid, "已经交付。");
    let fails = k.open_fails(&item);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(
        fails[0].contains(&format!("交付连续 2 次因同一原因被拒：{why}")),
        "{}",
        fails[0]
    );
    assert_eq!(steve_lines(&k, &item).len(), 1, "no second continue");
}

/// A different reason each time is the agent's to work on: it is continued, told the latest one.
#[test]
fn different_refusals_are_continued_with_the_latest_reason() {
    let k = kit(5);
    let (item, sid) = started(&k);
    let items = k.db().store().items();
    items.set_design_gate(&item, true).unwrap();
    deliver_refused(&k, &sid, &[]);
    stop(&k, &sid, "已经交付。");
    items.set_design_gate(&item, false).unwrap();
    let scope = deliver_refused(
        &k,
        &sid,
        &[crate::Check {
            name: "tests".into(),
            passed: true,
            operated: false,
            scope: Some("everything".into()),
        }],
    );
    stop(&k, &sid, "已经交付。");
    assert!(k.open_fails(&item).is_empty(), "{:?}", k.open_fails(&item));
    assert_eq!(steve_lines(&k, &item).len(), 2);
    let p = k.rt.prompts_of(&sid).pop().unwrap();
    assert!(p.contains(&format!("原因：{scope}")), "{p}");
}

/* ── your Finish when nothing can take Steve's follow-up (#347) ── */

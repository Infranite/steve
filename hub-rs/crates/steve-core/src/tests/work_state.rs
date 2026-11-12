//! One judgement of an item's work (crate::work) for the turn end, the stall watch and Steve's
//! continues, with ST-4 and ST-5 as the cases: a coordinator idle while its work graph runs, queues or
//! is paused is waiting, not stopped; a failed unit is a real failure the coordinator hears of with
//! its reason; a stop card is never built from the session's last system line.

use serde_json::json;
use steve_store::{UnitSpec, WorkUnit};

use crate::stall::{STALL, STALL_AFTER_MS};
use crate::takeover::LINE;
use crate::testkit::Kit;
use crate::util::now;
use crate::{AgentRuntime, patch};

fn later() -> i64 {
    now() + STALL_AFTER_MS + 1_000
}

fn kit(continues: u32) -> Kit {
    let k = Kit::with(|c| c.takeover_continues = continues);
    k.rt.st().workflow_execution_ready = true;
    k
}

fn spec(id: &str) -> UnitSpec {
    UnitSpec {
        id: id.into(),
        title: id.into(),
        goal: format!("完成 {id}"),
        bot_id: "dev".into(),
        kind: "work".into(),
        depends_on: vec![],
        resources: vec![format!("src/{id}")],
        required: true,
    }
}

/// `ids` (independent work units), their required review and integration.
fn graph_of(ids: &[&str]) -> Vec<UnitSpec> {
    let mut units: Vec<_> = ids.iter().map(|id| spec(id)).collect();
    let mut review = spec("review");
    review.kind = "review".into();
    review.bot_id = "steve".into();
    review.resources = vec![];
    review.depends_on = ids.iter().map(|id| (*id).into()).collect();
    let mut integrate = spec("integrate");
    integrate.kind = "integrate".into();
    integrate.resources = vec![];
    integrate.depends_on = vec!["review".into()];
    units.extend([review, integrate]);
    units
}

/// A parent item with the graph of `ids`, planned by you.
fn planned(k: &Kit, ids: &[&str]) -> String {
    let parent = k.item("run", "执行", None);
    let units = graph_of(ids);
    k.core
        .workflow_change(
            &parent,
            json!({"action":"plan","reason":"拆分","expected_revision":0,
                "units":units,"max_parallel":2,"max_attempts":3}),
            "me",
        )
        .unwrap();
    parent
}

fn unit(k: &Kit, parent: &str, id: &str) -> WorkUnit {
    k.db()
        .store()
        .workflows()
        .get(parent)
        .unwrap()
        .unwrap()
        .units
        .into_iter()
        .find(|u| u.spec.id == id)
        .unwrap()
}

fn change(k: &Kit, parent: &str, action: &str, id: Option<&str>) {
    let revision = k
        .db()
        .store()
        .workflows()
        .get(parent)
        .unwrap()
        .unwrap()
        .revision;
    k.core
        .workflow_change(
            parent,
            json!({"action":action,"reason":"调整","unit_id":id,"expected_revision":revision}),
            "me",
        )
        .unwrap();
}

fn work(k: &Kit, item: &str) -> (&'static str, Option<String>) {
    let t = k.core.tx();
    let w = t.work(&t.item(item).unwrap()).unwrap();
    (w.code(), w.text().map(String::from))
}

fn steve_lines(k: &Kit, item: &str) -> Vec<String> {
    k.timeline_texts(item)
        .into_iter()
        .filter(|t| t.starts_with(LINE))
        .collect()
}

fn stall_cards(k: &Kit, item: &str) -> usize {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .iter()
        .filter(|d| d.status == "open" && d.subject_type == STALL)
        .count()
}

/// The coordinator runs one turn and ends it.
fn coordinator_turn(k: &Kit, sid: &str, words: &str) {
    if !k.rt.st().turning.contains(sid) {
        k.rt.prompt(sid, "看看进度", "me", &[]).unwrap();
    }
    k.rt.say(sid, words);
    k.rt.end_turn(&k.core, sid, "end_turn", None);
}

/// ST-5: the outer session sat idle for more than two minutes while its units worked. Neither the
/// stall watch nor the coordinator's own turn end may continue it, card it or spend its budget.
#[test]
fn an_idle_coordinator_while_units_run_for_minutes_is_waiting() {
    let k = kit(2);
    let parent = planned(&k, &["a", "b"]);
    let coord = k.session(&parent, "idle");
    assert_eq!(k.rt.st().turning.len(), 2, "both units run");
    assert_eq!(work(&k, &parent).0, "running");

    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(k.rt.prompts_of(&coord).is_empty(), "not continued");
    assert!(k.open_fails(&parent).is_empty());
    for id in ["a", "b"] {
        let child = unit(&k, &parent, id).item_id.unwrap();
        assert!(
            k.open_fails(&child).is_empty(),
            "{id}: a unit is its graph's"
        );
    }

    coordinator_turn(&k, &coord, "两个单元都在执行，等它们交付。");
    assert_eq!(
        k.rt.prompts_of(&coord).len(),
        1,
        "no continue on the turn end"
    );
    assert!(steve_lines(&k, &parent).is_empty(), "no budget spent");
    assert!(k.open_fails(&parent).is_empty());
    assert_eq!(k.status(&parent).2, "协作执行中：a、b");
    let snap = k.core.diagnostic_snapshot(&parent).unwrap();
    assert_eq!(snap["work"]["state"], "running");
    assert_eq!(snap["work"]["why"], "协作执行中：a、b");
}

#[test]
fn a_stop_card_from_before_recovery_expires_while_the_graph_is_running() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    let old = k.decision(&parent, Some(&coord), "fail", STALL, &["continue", "look"]);
    crate::lifecycle::settle(k.db(), &parent).unwrap();
    assert_eq!(k.status(&parent).0, "wait");

    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.db().decision(&old).unwrap().unwrap().status, "expired");
    assert_eq!(k.status(&parent).0, "run");
    assert_eq!(k.status(&parent).2, "协作执行中：a");

    let old = k.decision(&parent, Some(&coord), "fail", STALL, &["continue", "look"]);
    let real_question = k.decision(&parent, Some(&coord), "ask", "item", &["answer"]);
    crate::lifecycle::settle(k.db(), &parent).unwrap();
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.db().decision(&old).unwrap().unwrap().status, "expired");
    assert_eq!(
        k.db().decision(&real_question).unwrap().unwrap().status,
        "open"
    );
    assert_eq!(k.status(&parent).0, "wait", "the real question still waits");

    let old = k.decision(&parent, Some(&coord), "fail", STALL, &["continue", "look"]);
    let unknown_effect = k.decision(&parent, Some(&coord), "fail", "item", &["release_retry"]);
    let release = k.decision(&parent, Some(&coord), "fail", "item", &["release_live"]);
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.db().decision(&old).unwrap().unwrap().status, "expired");
    assert_eq!(
        k.db().decision(&unknown_effect).unwrap().unwrap().status,
        "open"
    );
    assert_eq!(k.db().decision(&release).unwrap().unwrap().status, "open");
}

/// A paused graph is your stop: nothing continues the coordinator or opens a card for it.
#[test]
fn a_paused_graph_is_not_a_stop() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    change(&k, &parent, "pause", None);
    assert_eq!(work(&k, &parent), ("paused", Some("协作已暂停".into())));
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    coordinator_turn(&k, &coord, "已暂停。");
    assert_eq!(k.rt.prompts_of(&coord).len(), 1);
    assert!(steve_lines(&k, &parent).is_empty());
    assert!(k.open_fails(&parent).is_empty());
    assert_eq!(k.status(&parent).2, "协作已暂停");
}

/// ST-5, ST-10, ST-27: the coordinator paused its own graph and asked nothing. That waits for you:
/// it gets a card (at its turn end, or from the watch for a graph paused earlier), never Steve's
/// continue over its own call; resuming the graph takes the card back.
#[test]
fn a_graph_its_coordinator_paused_without_asking_waits_for_you() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    let revision = k
        .db()
        .store()
        .workflows()
        .get(&parent)
        .unwrap()
        .unwrap()
        .revision;
    k.core
        .workflow_change(
            &parent,
            json!({"action":"pause","reason":"评审单元被上游拒绝","expected_revision":revision}),
            "dev",
        )
        .unwrap();
    assert_eq!(
        work(&k, &parent),
        (
            "paused_by_agent",
            Some("协作已暂停，等你决定：评审单元被上游拒绝".into())
        )
    );
    // a graph paused before its turn ended (or before this Hub): the watch asks you
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        vec![parent.clone()]
    );
    assert!(k.rt.prompts_of(&coord).is_empty(), "not continued");
    assert!(steve_lines(&k, &parent).is_empty(), "no budget spent");
    assert_eq!(stall_cards(&k, &parent), 1);
    assert_eq!(k.status(&parent).0, "wait");
    assert_eq!(work(&k, &parent).0, "waiting_user");
    // you resume it: the card no longer applies
    change(&k, &parent, "resume", None);
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(stall_cards(&k, &parent), 0);
    assert_eq!(k.status(&parent).0, "run");

    // paused in a turn that then ends: the card comes with the turn end
    k.rt.prompt(&coord, "看看进度", "me", &[]).unwrap();
    let revision = k
        .db()
        .store()
        .workflows()
        .get(&parent)
        .unwrap()
        .unwrap()
        .revision;
    k.core
        .workflow_change(
            &parent,
            json!({"action":"pause","reason":"需要你确认范围","expected_revision":revision}),
            "dev",
        )
        .unwrap();
    coordinator_turn(&k, &coord, "已暂停。");
    assert_eq!(k.rt.prompts_of(&coord).len(), 1, "not continued");
    assert!(steve_lines(&k, &parent).is_empty());
    let cards = k.open_fails(&parent);
    assert_eq!(
        cards,
        vec!["协作已暂停，等你决定：需要你确认范围".to_string()]
    );
    assert_eq!(k.status(&parent).0, "wait");
}

/// A pause stops the units' turns; when the graph is resumed before the stopped turn's end comes
/// back, that end is still the pause, not the unit failing: the same session goes on.
#[test]
fn a_turn_the_pause_stopped_is_no_failure_after_a_quick_resume() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let sid = unit(&k, &parent, "a").session_id.unwrap();
    change(&k, &parent, "pause", None);
    change(&k, &parent, "resume", None);
    let before = k.rt.prompts_of(&sid).len();
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    let a = unit(&k, &parent, "a");
    assert_eq!((a.status.as_str(), a.reason), ("running", None));
    assert_eq!(k.rt.prompts_of(&sid).len(), before + 1, "resumed in place");
    assert_eq!(work(&k, &parent).0, "running");
    assert!(k.open_fails(&parent).is_empty());

    // a turn you stop yourself, with no pause, is the unit's failure as before
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    assert_eq!(unit(&k, &parent, "a").status, "failed");
}

/// Units that wait for their turn (here: the Hub holds new turns) are queued work, not a stop.
#[test]
fn queued_units_are_waited_for() {
    let k = kit(2);
    let parent = k.item("run", "执行", None);
    let coord = k.session(&parent, "idle");
    k.rt.prompt(&coord, "拆分", "me", &[]).unwrap();
    k.core.tx().set_hold(crate::occupancy::Hold::Release, true);
    k.core
        .workflow_change(
            &parent,
            json!({"action":"plan","reason":"拆分","expected_revision":0,
                "units":graph_of(&["a", "b"]),"max_parallel":1,"max_attempts":3}),
            "me",
        )
        .unwrap();
    assert_eq!(unit(&k, &parent, "a").status, "pending");
    k.rt.end_turn(&k.core, &coord, "end_turn", None);
    assert_eq!(k.status(&parent).2, "协作排队中：a、b、review 等 4 项");
    assert!(steve_lines(&k, &parent).is_empty());
    k.core.tx().set_hold(crate::occupancy::Hold::Release, false);
    assert_eq!(work(&k, &parent).0, "queued");
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(k.open_fails(&parent).is_empty());
}

/// Units that cannot start here are a real failure, with the reason why, not a quiet wait.
#[test]
fn units_that_cannot_start_say_why() {
    let k = kit(0);
    k.rt.st().workflow_execution_ready = false;
    let parent = k.item("run", "执行", None);
    k.core
        .workflow_change(
            &parent,
            json!({"action":"plan","reason":"拆分","expected_revision":0,
                "units":graph_of(&["a"]),"max_parallel":1,"max_attempts":3}),
            "me",
        )
        .unwrap();
    let (code, why) = work(&k, &parent);
    assert_eq!(code, "failed");
    assert!(
        why.unwrap()
            .starts_with("协作工作无法开始：测试运行器未启用隔离协作执行")
    );
}

/// ST-4: a required unit failed for real. The coordinator's turn end is not silenced by "a graph
/// exists": Steve continues it with the unit's reason within the budget, then the card says so.
#[test]
fn a_failed_unit_reaches_the_coordinator_with_its_reason() {
    let k = kit(1);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    let a = unit(&k, &parent, "a");
    let child = a.item_id.clone().unwrap();
    k.rt.end_turn(
        &k.core,
        a.session_id.as_deref().unwrap(),
        "error",
        Some("检查失败"),
    );
    assert_eq!(unit(&k, &parent, "a").status, "failed");
    // the graph told the coordinator; it ends that turn without acting on it
    assert!(k.rt.st().turning.contains(&coord));
    coordinator_turn(&k, &coord, "收到。");
    assert_eq!(steve_lines(&k, &parent).len(), 1, "one continue");
    let nudge = k.rt.prompts_of(&coord).pop().unwrap();
    assert!(
        nudge.contains("协作工作未通过：a 未通过：检查失败") && nudge.contains("steve_workflow"),
        "{nudge}"
    );
    coordinator_turn(&k, &coord, "我再看看。");
    let cards = k.open_fails(&parent);
    assert_eq!(cards.len(), 1, "out of continues: yours");
    assert!(
        cards[0].contains("协作工作未通过：a 未通过：检查失败")
            && cards[0].contains(crate::takeover::STEVE_TRIED),
        "{cards:?}"
    );
    // the failed unit's own item is not continued or carded by the ordinary watch
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(k.open_fails(&child).is_empty());
}

/// A card for an idle item says what the state is: neither an old release notice in the session nor
/// a unit's failure is replaced by the session's last system line.
#[test]
fn a_stop_card_never_quotes_an_old_system_line() {
    let k = kit(0);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.db()
        .event_add(
            &s,
            "system",
            json!({"text": "已发布 v0.2.99：Release 构建通过"}),
        )
        .unwrap();
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    let q = k.open_fails(&i).pop().unwrap();
    assert_eq!(
        q,
        "它停下了：会话空闲：没有回合在运行、没有排队输入，也没有交付或提问"
    );

    let parent = planned(&k, &["a"]);
    let a = unit(&k, &parent, "a");
    k.rt.end_turn(
        &k.core,
        a.session_id.as_deref().unwrap(),
        "error",
        Some("检查失败"),
    );
    let coord = k.session(&parent, "idle");
    k.db()
        .event_add(&coord, "system", json!({"text": "已发布 v0.2.98"}))
        .unwrap();
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&parent)
    );
    let q = k.open_fails(&parent).pop().unwrap();
    assert_eq!(
        q,
        "它停下了：协作工作未通过：a 未通过：检查失败；受阻：review、integrate"
    );
}

/// A registered prerequisite wait explains an idle item even before its status caught up.
#[test]
fn a_registered_dependency_wait_is_not_a_stall() {
    let k = kit(2);
    let item = k.item("run", "执行", None);
    let dep = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_item_dependencies(
            &s,
            json!({"action":"wait","deps":[dep],"reason":"等前置验收","resume":"automatic"}),
        )
        .unwrap();
    k.db().item_patch(&item, patch![status: "run"]).unwrap();
    assert_eq!(work(&k, &item).0, "dependency_wait");
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(k.rt.prompts_of(&sid).is_empty());
    assert!(k.open_fails(&item).is_empty());
}

/// A unit at work again (retried by the coordinator) expires the parent's old stop card, and a
/// continue asked for meanwhile is looked at again before it is spent.
#[test]
fn a_unit_working_again_clears_the_parents_stop_card() {
    let k = kit(0);
    let parent = planned(&k, &["a"]);
    let a = unit(&k, &parent, "a");
    let old = a.session_id.clone().unwrap();
    k.rt.end_turn(&k.core, &old, "error", Some("检查失败"));
    let coord = k.session(&parent, "idle");
    k.core.tx().check_stalls(later()).unwrap();
    assert_eq!(stall_cards(&k, &parent), 1);
    assert_eq!(k.status(&parent).0, "wait");
    change(&k, &parent, "retry", Some("a"));
    let retried = unit(&k, &parent, "a").session_id.unwrap();
    assert_ne!(retried, old);
    k.core.expire_stall(&retried).unwrap();
    assert_eq!(stall_cards(&k, &parent), 0);
    assert!(k.open_fails(&parent).is_empty());
    assert_eq!(k.status(&parent).0, "run");
    assert!(
        k.timeline_texts(&parent)
            .iter()
            .any(|t| t.contains("协作工作 a 又开始执行了"))
    );
    // a retired attempt's session starting says nothing about the parent
    k.decision(&parent, Some(&coord), "fail", "item", &["continue"]);
    k.core.expire_stall(&old).unwrap();
    assert_eq!(k.open_fails(&parent).len(), 1);
    k.db()
        .decisions_of_item(&parent)
        .unwrap()
        .iter()
        .filter(|d| d.status == "open")
        .for_each(|d| {
            k.db()
                .decision_resolve(&d.id, json!({}), "resolved", None)
                .unwrap();
        });

    // a stop reported late (a probe come back, an old turn end) finds the unit running: no continue
    let before = k.rt.prompts_of(&coord).len();
    k.rt.end_turn(&k.core, &old, "error", Some("迟到的结束"));
    assert_eq!(
        k.rt.prompts_of(&coord).len(),
        before,
        "a retired attempt's late end is no news for the coordinator"
    );
    assert_eq!(unit(&k, &parent, "a").status, "running");
    let r = k
        .core
        .tx()
        .takeover(
            &parent,
            &coord,
            crate::takeover::Stop::Idle {
                why: "旧的停下".into(),
            },
        )
        .unwrap();
    assert!(r.is_none());
    assert_eq!(k.rt.prompts_of(&coord).len(), before);
    assert!(steve_lines(&k, &parent).is_empty());
}

/// An agent that stops waiting for something only you can do is yours: Steve does not continue it
/// (it would only take the step as done), and YOLO meets hard limit 7 on the card.
#[test]
fn an_agent_waiting_for_your_own_action_is_not_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "代码已改好。需要你本人在手机上确认登录，完成后告诉我。");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    assert_eq!(k.open_fails(&i).len(), 1, "the card is yours");
}

/// 2026-11-12: the Node could not link for hours and input waited with only "queued" to show. What
/// waits on it says the Node is not linked and why, and stops saying so once it links.
#[test]
fn input_waiting_on_an_unlinked_node_says_why() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.db().queue_add(&s, "继续", "me", &[]).unwrap();
    assert_eq!(work(&k, &i), ("queued", Some("有排队输入等待发送".into())));
    k.rt.st().node_offline =
        Some("internal: control message hello is 69214 bytes, over the 65536 limit".into());
    let (code, why) = work(&k, &i);
    assert_eq!(code, "queued");
    assert_eq!(
        why.as_deref(),
        Some(
            "等待 Node 连接：internal: control message hello is 69214 bytes, over the 65536 limit"
        )
    );
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.status(&i).2, why.unwrap());
    assert!(k.open_fails(&i).is_empty(), "a wait, not a stop");
    k.rt.st().node_offline = None;
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.status(&i).2, "有排队输入等待发送");
}

/// ST-1/2/3: a 429 that says the quota is back in hours. Continuing at once fails within a second
/// and spends the continues; the item waits until then instead, and the agent goes on after.
#[test]
fn a_quota_that_is_back_later_is_waited_for_not_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做吧", "me", &[]).unwrap();
    k.rt.end_turn(
        &k.core,
        &s,
        "error",
        Some("API Error: 429 · upstream account quota exhausted; try again in 2h"),
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued at once");
    assert!(steve_lines(&k, &i).is_empty(), "no continue spent");
    assert!(k.open_fails(&i).is_empty(), "no card");
    let (code, why) = work(&k, &i);
    assert_eq!(code, "quota_wait");
    assert!(why.unwrap().starts_with("模型配额用完，"));
    assert!(k.status(&i).2.starts_with("模型配额用完，"));
    // the watch leaves it while the quota is out
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.rt.prompts_of(&s).len(), 1);
    // the time has come: continued once, still nothing spent
    let back = now() + 2 * 3_600_000 + 60_000;
    assert!(k.core.tx().check_stalls(back).unwrap().is_empty());
    let prompts = k.rt.prompts_of(&s);
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("配额已恢复"), "{}", prompts[1]);
    assert!(steve_lines(&k, &i).is_empty());
    assert_eq!(work(&k, &i).0, "running");

    // a quota with no time stays an ordinary error: continued within the budget
    k.rt.end_turn(
        &k.core,
        &s,
        "error",
        Some("API Error: 429 Too Many Requests"),
    );
    assert_eq!(steve_lines(&k, &i).len(), 1);
}

/// The look before a continue is spent does not take the item's own stop card for a wait: a voided
/// delivery still reaches an agent whose earlier stop left a card open.
#[test]
fn an_open_stop_card_does_not_hold_back_a_continue() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    old_card(
        &k,
        &i,
        STALL,
        &["continue", "look", "drop"],
        "它停下了：会话空闲",
    );
    assert_eq!(work(&k, &i).0, "waiting_user");
    k.core.tx().takeover_invalidated(&i, "计划已更新").unwrap();
    let p = k.rt.prompts_of(&s);
    assert_eq!(p.len(), 1, "continued");
    assert!(p[0].contains("交付已经作废"), "{p:?}");
    assert_eq!(steve_lines(&k, &i).len(), 1);
}

fn old_card(k: &Kit, item: &str, subject_type: &str, options: &[&str], question: &str) -> String {
    k.db()
        .decision_create(crate::repo::NewDecision {
            workspace_id: crate::repo::WS.into(),
            kind: "fail".into(),
            subject_type: subject_type.into(),
            subject_id: item.into(),
            item_id: Some(item.into()),
            question: question.into(),
            options: Some(options.iter().map(|s| s.to_string()).collect()),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn open_ids(k: &Kit, item: &str) -> Vec<String> {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .into_iter()
        .filter(|d| d.status == "open")
        .map(|d| d.id)
        .collect()
}

fn restarted(k: &Kit) -> crate::Core {
    crate::Core::new(
        k.db().clone(),
        k.rt.clone(),
        k.gw.clone(),
        k.core.config().clone(),
    )
}

/// After an upgrade or a restart, a parent can hold stop cards from before while its units already
/// run: no unit start is going to expire them. The graph's state stands, the next stall pass expires
/// the ordinary stop cards, and nothing continues the coordinator.
#[test]
fn old_stop_cards_of_a_parent_whose_units_run_expire_after_a_restart() {
    let k = kit(2);
    let parent = planned(&k, &["a", "b"]);
    let coord = k.session(&parent, "idle");
    old_card(
        &k,
        &parent,
        STALL,
        &["continue", "look", "drop"],
        "它停下了：已发布 v0.2.98",
    );
    old_card(
        &k,
        &parent,
        "item",
        &["continue", "delivered", "look"],
        "Agent 停下来了，没有交付也没有提问",
    );
    crate::lifecycle::settle(k.db(), &parent).unwrap();
    assert_eq!(k.status(&parent).0, "wait");
    assert_eq!(
        work(&k, &parent),
        ("running", Some("协作执行中：a、b".into()))
    );

    let core = restarted(&k);
    assert!(core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(open_ids(&k, &parent).is_empty(), "both stop cards expired");
    assert_eq!(k.status(&parent).0, "run");
    assert!(k.rt.prompts_of(&coord).is_empty(), "not continued");
    assert!(
        k.timeline_texts(&parent)
            .iter()
            .any(|t| t == "2 件待处理已失效：协作执行中：a、b")
    );
}

/// Only ordinary stop cards go: a question, an effect nobody knows and a release card stay yours, and
/// the parent waits on them.
#[test]
fn real_user_cards_of_a_parent_survive_the_tidy() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    let stop = old_card(
        &k,
        &parent,
        STALL,
        &["continue", "look", "drop"],
        "它停下了：旧卡",
    );
    let ask = k.decision(&parent, Some(&coord), "ask", "item", &["A", "B"]);
    let unknown = old_card(
        &k,
        &parent,
        "item",
        &["release_retry", "look"],
        "发布结果未知",
    );
    let release = old_card(
        &k,
        &parent,
        "item",
        &["release_live", "release_waive", "look"],
        "是否已上线",
    );
    assert_eq!(work(&k, &parent).0, "waiting_user");
    k.core.tx().check_stalls(later()).unwrap();
    let open = open_ids(&k, &parent);
    assert!(!open.contains(&stop));
    for id in [&ask, &unknown, &release] {
        assert!(open.contains(id), "{id} stays");
    }
    assert_eq!(work(&k, &parent).0, "waiting_user");
    assert!(k.rt.prompts_of(&coord).is_empty());
}

/// Queued units outdate an old stop card too, even while the Hub holds new turns.
#[test]
fn old_stop_cards_of_a_parent_with_queued_units_expire_during_a_hold() {
    let k = kit(2);
    k.core.tx().set_hold(crate::occupancy::Hold::Release, true);
    let parent = planned(&k, &["a"]);
    assert_eq!(unit(&k, &parent, "a").status, "pending");
    old_card(
        &k,
        &parent,
        STALL,
        &["continue", "look", "drop"],
        "它停下了：旧卡",
    );
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(open_ids(&k, &parent).is_empty());
    assert_eq!(work(&k, &parent).0, "queued");
}

fn deliver(k: &Kit, parent: &str, id: &str) {
    let sid = unit(k, parent, id).session_id.unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_deliver(
            &s,
            &format!("{id} 已验证"),
            &[crate::Output {
                kind: "文件".into(),
                title: format!("{id} 的成果"),
                location: Some(format!("{id}.txt")),
                role: None,
            }],
            &[crate::Check {
                name: "检查和实际操作".into(),
                passed: true,
                operated: true,
            }],
            &[],
        )
        .unwrap();
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
}

/// ST-11: a review unit reported its failure while its own item still had a question open. The
/// question is moot (the coordinator retries or replans), so the unit's item does not stay waiting
/// for you; a card left on an already failed unit's item from before goes on the next pass.
#[test]
fn a_failed_units_leftover_cards_do_not_keep_its_item_waiting() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    deliver(&k, &parent, "a");
    let review = unit(&k, &parent, "review");
    assert_eq!(review.status, "running");
    let child = review.item_id.clone().unwrap();
    let rs = k
        .db()
        .session(review.session_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    k.decision(&child, Some(&rs.id), "ask", "item", &["A", "B"]);
    crate::lifecycle::settle(k.db(), &child).unwrap();
    assert_eq!(k.status(&child).0, "wait");
    k.core
        .tool_workflow(&rs, json!({"action":"fail","reason":"缺少关键回归"}))
        .unwrap();
    assert_eq!(unit(&k, &parent, "review").status, "failed");
    assert!(open_ids(&k, &child).is_empty(), "the question is moot");
    assert_ne!(k.status(&child).0, "wait");
    k.rt.end_turn(&k.core, &rs.id, "end_turn", None);

    // a stall card the old watch left on the failed unit's item
    old_card(
        &k,
        &child,
        STALL,
        &["continue", "look", "drop"],
        "它停下了：旧卡",
    );
    crate::lifecycle::settle(k.db(), &child).unwrap();
    k.core.tx().check_stalls(later()).unwrap();
    assert!(open_ids(&k, &child).is_empty());
    assert_ne!(k.status(&child).0, "wait");
}

/// ST-25 #2 at the turn end: a request to scan at the start of a long final reply, streamed in many
/// pieces and not ending in a question, is still a wait on you. Steve does not continue it.
#[test]
fn a_long_final_reply_asking_you_to_scan_is_not_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "请扫码授权，完成后告诉我。");
    for _ in 0..120 {
        k.rt.say(&s, "阅读说明。");
    }
    k.rt.say(&s, "我在这里等着");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    assert_eq!(k.open_fails(&i).len(), 1, "the card is yours");
    // and the stall watch later on does not continue it either
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.rt.prompts_of(&s).len(), 1);

    // what it said before its last tool call is not what it waits for now
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "看到文档里写着请扫码授权。");
    k.db()
        .event_add(&s, steve_transcript::TOOL, json!({"tool": {"id": "t1"}}))
        .unwrap();
    k.rt.say(&s, "改好了一半，接着做。");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
}

/// ST-25 #8 at the turn end: an English request to sign in and say when done, with no question mark.
#[test]
fn an_english_request_to_log_in_is_not_continued() {
    let k = kit(2);
    for said in [
        "Please log in to your account, then reply Done.",
        "Please log in to GitHub and tell me when done.",
    ] {
        let i = k.item("run", "执行", None);
        let s = k.session(&i, "idle");
        k.rt.prompt(&s, "do it", "me", &[]).unwrap();
        k.rt.say(&s, said);
        k.rt.end_turn(&k.core, &s, "end_turn", None);
        assert_eq!(k.rt.prompts_of(&s).len(), 1, "{said}: not continued");
        assert_eq!(k.open_fails(&i).len(), 1, "{said}: the card is yours");
    }
}

/// ST-25 #9 at the turn end: words about codes in a report are no wait on you; Steve continues it.
#[test]
fn a_report_about_codes_is_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(
        &s,
        "验证码有效期改成 120 秒了，OTP 位数和二维码组件也换好了，测试通过。",
    );
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
    assert!(k.open_fails(&i).is_empty());
}

/// ST-25 #3: a required unit failed, the coordinator was told and ran, and you stopped that turn
/// (Core::cancel_turn, ended cancelled). Two minutes later the stall watch gives you the card with
/// the failure's reason: your Stop comes first, nothing continues it and no continue is spent.
#[test]
fn your_stop_after_a_failure_notice_is_yours() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    let a = unit(&k, &parent, "a");
    k.rt.end_turn(
        &k.core,
        a.session_id.as_deref().unwrap(),
        "error",
        Some("检查失败"),
    );
    assert_eq!(unit(&k, &parent, "a").status, "failed");
    assert!(k.rt.st().turning.contains(&coord), "the coordinator was told");
    let before = k.rt.prompts_of(&coord).len();
    assert!(k.core.cancel_turn(&coord).unwrap());
    k.rt.end_turn(&k.core, &coord, "cancelled", None);
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&parent)
    );
    assert_eq!(k.rt.prompts_of(&coord).len(), before, "not continued");
    assert!(!k.core.is_busy(&coord), "no turn runs");
    assert!(steve_lines(&k, &parent).is_empty(), "no continue spent");
    let cards = k.open_fails(&parent);
    assert_eq!(cards.len(), 1);
    assert!(
        cards[0].contains("协作工作未通过：a 未通过：检查失败")
            && cards[0].contains("你中断了这一回合"),
        "{cards:?}"
    );
    let snap = k.core.diagnostic_snapshot(&parent).unwrap();
    assert_eq!(snap["work"]["last_stop"]["yours"], true, "{snap}");
    // a voided delivery does not continue it either: the card stays yours
    k.core.tx().takeover_invalidated(&parent, "计划已更新").unwrap();
    assert_eq!(k.rt.prompts_of(&coord).len(), before);
}

/// ST-25 #4: you stop an ordinary item's turn and the Hub restarts before the stall watch opens a
/// card. Your Stop is kept with the session, not in memory: after the restart the card is yours and
/// nothing continues it.
#[test]
fn your_stop_is_yours_after_a_restart() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "在改代码");
    assert!(k.core.cancel_turn(&s).unwrap());
    k.rt.end_turn(&k.core, &s, "cancelled", None);
    assert!(k.open_fails(&i).is_empty(), "no card yet");

    let core = restarted(&k);
    core.recover().unwrap();
    assert_eq!(
        core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    let cards = k.open_fails(&i);
    assert_eq!(cards.len(), 1);
    assert!(cards[0].contains("你中断了这一回合"), "{cards:?}");
}

/// ST-25 #4: the end of an older turn that comes late does not take your Stop from you; a message
/// of yours starts another turn, and once that one ends without delivering Steve may go on again.
#[test]
fn a_late_old_turn_end_does_not_undo_your_stop_but_your_message_does() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    assert!(k.core.cancel_turn(&s).unwrap());
    k.rt.end_turn(&k.core, &s, "cancelled", None);
    // an earlier attempt's end, late
    k.core
        .on_turn_end(crate::TurnEnd {
            session_id: s.clone(),
            attempt: Some("a-old".into()),
            stop_reason: "end_turn".into(),
            error: None,
        })
        .unwrap();
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(crate::work::yours(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    let core = restarted(&k);
    assert_eq!(
        core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    let card = k
        .db()
        .decisions_of_item(&i)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open")
        .unwrap();
    assert!(card.question.contains("你中断了这一回合"));

    // your own message: a new turn; it ends without delivering, and Steve continues it
    core.session_message(&s, "接着做", false, &[]).unwrap();
    assert_eq!(k.rt.prompts_of(&s).len(), 2);
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&core, &s, "end_turn", None);
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(!crate::work::yours(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
    assert_eq!(steve_lines(&k, &i).len(), 1);
}

/// ST-25 #5: a pause stops turn A and the graph is resumed at once; the resumed turn B starts before
/// A's end comes back, and that late end is a stale one. The pause's mark is A's alone: when you
/// stop B yourself, the unit fails as any unit whose turn you stopped, nothing resumes it.
#[test]
fn a_late_end_of_the_paused_turn_does_not_swallow_your_stop_of_the_next() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let sid = unit(&k, &parent, "a").session_id.unwrap();
    let a_turn = k.rt.running(&sid).unwrap();
    change(&k, &parent, "pause", None);
    change(&k, &parent, "resume", None);
    // A is over on the runtime, its end not told yet; the graph resumes the session: B
    {
        let mut st = k.rt.st();
        st.turning.remove(&sid);
        st.attempts.remove(&sid);
    }
    k.db().session_patch(&sid, patch![status: "idle"]).unwrap();
    k.core.tx().workflow_tick().unwrap();
    let b_turn = k.rt.running(&sid).expect("B runs");
    assert_ne!(a_turn, b_turn);
    let prompts = k.rt.prompts_of(&sid).len();
    // A's end, late: stale
    k.core
        .on_turn_end(crate::TurnEnd {
            session_id: sid.clone(),
            attempt: Some(a_turn.attempt.clone()),
            stop_reason: "cancelled".into(),
            error: None,
        })
        .unwrap();
    assert_eq!(unit(&k, &parent, "a").status, "running");
    // you stop B
    assert!(k.core.cancel_turn(&sid).unwrap());
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    let a = unit(&k, &parent, "a");
    assert_eq!(a.status, "failed", "{a:?}");
    assert_eq!(k.rt.prompts_of(&sid).len(), prompts, "not resumed");
    assert_eq!(work(&k, &parent).0, "failed");
}

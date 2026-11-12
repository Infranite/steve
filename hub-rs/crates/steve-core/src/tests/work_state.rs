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
    assert!(
        k.rt.st().turning.contains(&coord),
        "the coordinator was told"
    );
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
    k.core
        .tx()
        .takeover_invalidated(&parent, "计划已更新")
        .unwrap();
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

/// ST-25 #6: a failed, retried while paused (pending, attempt 1 of the old 3), then the same graph is
/// planned again with one attempt and resumed. `a` never starts again: it and everything after it
/// (review, then integrate) are a failure the coordinator hears of, not queued work forever.
#[test]
fn a_unit_out_of_attempts_holds_its_successors_as_failed() {
    for (attempts, want) in [(1, "failed"), (2, "running")] {
        let k = kit(2);
        let parent = planned(&k, &["a"]);
        let a = unit(&k, &parent, "a");
        k.rt.end_turn(
            &k.core,
            a.session_id.as_deref().unwrap(),
            "error",
            Some("检查失败"),
        );
        assert_eq!(unit(&k, &parent, "a").status, "failed");
        change(&k, &parent, "pause", None);
        change(&k, &parent, "retry", Some("a"));
        let a = unit(&k, &parent, "a");
        assert_eq!((a.status.as_str(), a.attempt), ("pending", 1));
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
                json!({"action":"plan","reason":"收紧次数","expected_revision":revision,
                    "units":graph_of(&["a"]),"max_parallel":2,"max_attempts":attempts}),
                "me",
            )
            .unwrap();
        change(&k, &parent, "resume", None);
        let (state, why) = work(&k, &parent);
        assert_eq!(state, want, "max_attempts {attempts}: {why:?}");
        if attempts == 1 {
            assert_eq!(
                why.as_deref(),
                Some("协作工作未通过：a 尝试次数已用完；受阻：review、integrate")
            );
            assert_eq!(unit(&k, &parent, "a").status, "pending");
            // the coordinator hears of it once idle, it is not left waiting
            let coord = k.session(&parent, "idle");
            coordinator_turn(&k, &coord, "看一下");
            let p = k.rt.prompts_of(&coord);
            assert!(p.last().unwrap().contains("a 尝试次数已用完"), "{p:?}");
        }
    }
}

/// ST-25 #7: a unit that reports failure itself (steve_workflow action=fail) and ends its turn: its
/// child item is closed as failed (its result stays on the graph, the coordinator is told), and the
/// ordinary watch has nothing left of it.
#[test]
fn a_unit_failing_itself_closes_its_child_and_tells_the_coordinator() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    let a = unit(&k, &parent, "a");
    let child = a.item_id.clone().unwrap();
    let sid = a.session_id.clone().unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_workflow(&s, json!({"action":"fail","reason":"评审未通过：缺少回归"}))
        .unwrap();
    k.rt.say(&sid, "已报告失败。");
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    let i = k.db().item(&child).unwrap().unwrap();
    assert_eq!(
        (i.phase.as_str(), i.outcome.as_deref()),
        ("closed", Some("failed"))
    );
    let a = unit(&k, &parent, "a");
    assert_eq!(a.status, "failed");
    assert_eq!(a.reason.as_deref(), Some("评审未通过：缺少回归"), "kept");
    assert!(
        k.rt.prompts_of(&coord)
            .iter()
            .any(|p| p.contains("协作工作 a 已更新")),
        "the coordinator was told"
    );
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(k.open_fails(&child).is_empty());
}

/// ST-25 #7: a unit cancelled while its turn runs: once that turn ends, its child item is closed as
/// cancelled; after a restart that lost the turn end, the next pass closes it.
#[test]
fn a_cancelled_units_child_is_closed_after_its_turn_and_after_a_restart() {
    let k = kit(2);
    let parent = planned(&k, &["a", "b"]);
    let a = unit(&k, &parent, "a");
    let child = a.item_id.clone().unwrap();
    let sid = a.session_id.clone().unwrap();
    change(&k, &parent, "cancel", Some("a"));
    assert_eq!(unit(&k, &parent, "a").status, "cancelled");
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    let i = k.db().item(&child).unwrap().unwrap();
    assert_eq!(
        (i.phase.as_str(), i.outcome.as_deref()),
        ("closed", Some("cancelled")),
        "{i:?}"
    );

    // b: cancelled, the Hub restarted before its turn end was handled
    let b = unit(&k, &parent, "b");
    let child = b.item_id.clone().unwrap();
    let sid = b.session_id.clone().unwrap();
    change(&k, &parent, "cancel", Some("b"));
    k.rt.st().turning.remove(&sid);
    k.db()
        .session_patch(&sid, crate::patch![status: "idle"])
        .unwrap();
    let core = restarted(&k);
    core.recover().unwrap();
    core.tx().workflow_tick().unwrap();
    let i = k.db().item(&child).unwrap().unwrap();
    assert_eq!(
        (i.phase.as_str(), i.outcome.as_deref()),
        ("closed", Some("cancelled")),
        "{i:?}"
    );
}

/// ST-25 #7: after a retry the unit runs again in a new child. The old attempt's turn end, coming
/// late, finishes only the old child: the new attempt keeps running and the coordinator is not told
/// of an update that is old.
#[test]
fn a_late_end_of_an_old_attempt_does_not_finish_the_new_one() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let coord = k.session(&parent, "idle");
    let old = unit(&k, &parent, "a");
    let old_child = old.item_id.clone().unwrap();
    let old_sid = old.session_id.clone().unwrap();
    k.rt.end_turn(&k.core, &old_sid, "error", Some("检查失败"));
    assert_eq!(k.phase(&old_child), "closed");
    change(&k, &parent, "retry", Some("a"));
    let new = unit(&k, &parent, "a");
    assert_eq!(new.status, "running");
    let new_child = new.item_id.clone().unwrap();
    assert_ne!(new_child, old_child);
    let told = k.rt.prompts_of(&coord).len();
    // the old attempt's end again, late (a redelivery after a restart)
    let core = restarted(&k);
    core.on_turn_end(crate::TurnEnd {
        session_id: old_sid.clone(),
        attempt: Some("a-old".into()),
        stop_reason: "error".into(),
        error: Some("检查失败".into()),
    })
    .unwrap();
    assert_eq!(k.phase(&new_child), "active");
    assert_eq!(unit(&k, &parent, "a").status, "running");
    assert_eq!(k.rt.prompts_of(&coord).len(), told, "no stale update");
    assert_eq!(work(&k, &parent).0, "running");
}

/// The runtime finished the session's turn but its end is not told yet (held up on the way).
fn over_untold(k: &Kit, sid: &str) {
    {
        let mut st = k.rt.st();
        st.turning.remove(sid);
        st.attempts.remove(sid);
    }
    k.db().session_patch(sid, patch![status: "idle"]).unwrap();
}

fn end_of(core: &crate::Core, sid: &str, attempt: &str, stop: &str) {
    core.on_turn_end(crate::TurnEnd {
        session_id: sid.into(),
        attempt: Some(attempt.into()),
        stop_reason: stop.into(),
        error: None,
    })
    .unwrap();
}

/// ST-37 #1 at the turn end and Steve's continue (takeover): asked to type a code before going on,
/// or to use a passkey, the agent waits on you. Steve spends no continue on it and the stall watch
/// does not either; a report of how the code step was built is continued as usual.
#[test]
fn a_code_to_type_or_a_passkey_is_not_continued() {
    let k = kit(2);
    for said in [
        "输入验证码后继续",
        "Use your passkey",
        "登录页已打开。Use your passkey to sign in, then reply.",
    ] {
        let i = k.item("run", "执行", None);
        let s = k.session(&i, "idle");
        k.rt.prompt(&s, "做事", "me", &[]).unwrap();
        k.rt.say(&s, said);
        k.rt.end_turn(&k.core, &s, "end_turn", None);
        assert_eq!(k.rt.prompts_of(&s).len(), 1, "{said}: not continued");
        assert!(steve_lines(&k, &i).is_empty(), "{said}");
        assert_eq!(k.open_fails(&i).len(), 1, "{said}: the card is yours");
        assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
        assert_eq!(k.rt.prompts_of(&s).len(), 1, "{said}");
    }
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(
        &s,
        "已实现：用户输入验证码后继续登录，passkey 也支持了，测试通过。",
    );
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
    assert!(k.open_fails(&i).is_empty());
}

/// ST-37 #2 at the Core: the same end of the same attempt told again after a restart (the same
/// SQLite file opened afresh) is the same end: your Stop stays, the stall watch gives you the card
/// and Steve continues nothing. Your new message lifts it.
#[test]
fn a_replayed_end_after_a_restart_keeps_your_stop() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    let a = k.rt.running(&s).unwrap().attempt;
    assert!(k.core.cancel_turn(&s).unwrap());
    // your Stop and the turn's completion race: it ends end_turn
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert!(k.db().store().sessions().stop(&s).unwrap().unwrap().yours);

    let core = k.reopened();
    core.recover().unwrap();
    end_of(&core, &s, &a, "end_turn");
    let kept = core.store().store().sessions().stop(&s).unwrap().unwrap();
    assert!(kept.yours, "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    core.tx().check_stalls(later()).unwrap();
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());

    core.session_message(&s, "接着做", false, &[]).unwrap();
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&core, &s, "end_turn", None);
    assert!(!crate::work::held(
        &core.store().store().sessions().stop(&s).unwrap().unwrap()
    ));
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
}

/// ST-37 #3: turn A's end is held up on the way; your message starts B and you stop B, which
/// is already idle when A's end comes, then B's. A's late end does not use up the Stop you asked
/// for B, nor becomes the session's last end; B's own end does, and it is yours.
#[test]
fn old_end_must_not_consume_new_stop_request() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    let a = k.rt.running(&s).unwrap().attempt;
    over_untold(&k, &s);
    k.core.session_message(&s, "换个做法", false, &[]).unwrap();
    let b = k.rt.running(&s).expect("B runs").attempt;
    assert_ne!(a, b);
    assert!(k.core.cancel_turn(&s).unwrap());
    // B is idle by now (busy → idle), its end not told yet either
    over_untold(&k, &s);
    let prompts = k.rt.prompts_of(&s).len();

    end_of(&k.core, &s, &a, "end_turn");
    let mid = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(mid.stop_asked.as_deref(), Some(b.as_str()), "{mid:?}");
    assert_ne!(mid.attempt.as_deref(), Some(a.as_str()), "{mid:?}");
    assert_eq!(
        k.rt.prompts_of(&s).len(),
        prompts,
        "A's end continues nothing"
    );
    assert!(k.open_fails(&i).is_empty(), "nor opens a card");

    end_of(&k.core, &s, &b, "end_turn");
    let after = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(
        (
            after.attempt.as_deref(),
            after.stop_reason.as_deref(),
            after.yours,
            after.stop_asked.as_deref()
        ),
        (Some(b.as_str()), Some("end_turn"), true, None),
        "{after:?}"
    );
    assert_eq!(
        k.rt.prompts_of(&s).len(),
        prompts,
        "your Stop: not continued"
    );
    assert!(steve_lines(&k, &i).is_empty());

    // the order reversed after a restart changes nothing: A's end, told again, is the same end
    let core = k.reopened();
    end_of(&core, &s, &a, "end_turn");
    let again = core.store().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(again.attempt.as_deref(), Some(b.as_str()));
    assert!(again.yours);
}

/// ST-37 #3 contrast: a Stop asked for one turn, then your message starts another before any end
/// came. That newer turn's end is not yours: it uses the old request up and is judged as usual.
#[test]
fn a_newer_turns_end_after_your_message_is_not_your_stop() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    assert!(k.core.cancel_turn(&s).unwrap());
    over_untold(&k, &s);
    k.core.session_message(&s, "接着做", false, &[]).unwrap();
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(!crate::work::held(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
}

/// ST-37 #4: a pause stops unit a's turn A and the graph is resumed at once; the Hub restarts
/// (the same SQLite file opened afresh) before A's cancelled end comes back. That end is still the
/// pause's, not the unit failing: the session's backend recovers it by itself (a Node's) and the
/// same session goes on. After that, a turn you stop yourself fails the unit as before, also across
/// another restart. On a backend the restart cut (the Hub's own), the turn's restart card is the
/// restart's business; the unit is not failed for the pause either.
#[test]
fn pause_resume_restart_then_late_end() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let sid = unit(&k, &parent, "a").session_id.unwrap();
    k.rt.st().recovers_itself.insert(sid.clone());
    change(&k, &parent, "pause", None);
    change(&k, &parent, "resume", None);
    let before = k.rt.prompts_of(&sid).len();

    let core = k.reopened();
    core.recover().unwrap();
    k.rt.end_turn(&core, &sid, "cancelled", None);
    let a = unit(&k, &parent, "a");
    assert_eq!(
        (a.status.as_str(), a.reason),
        ("running", None),
        "the pause's"
    );
    assert_eq!(k.rt.prompts_of(&sid).len(), before + 1, "resumed in place");
    assert!(k.open_fails(&parent).is_empty());

    // B, the resumed turn: you stop it after another restart; the unit fails
    let core = k.reopened();
    core.recover().unwrap();
    assert!(core.cancel_turn(&sid).unwrap());
    k.rt.end_turn(&core, &sid, "cancelled", None);
    let a = unit(&k, &parent, "a");
    assert_eq!(a.status, "failed", "{a:?}");
    assert_eq!(k.rt.prompts_of(&sid).len(), before + 1, "not resumed");

    // the Hub's own runtime: the restart cut the turn
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let sid = unit(&k, &parent, "a").session_id.unwrap();
    change(&k, &parent, "pause", None);
    change(&k, &parent, "resume", None);
    let core = k.reopened();
    core.recover().unwrap();
    k.rt.end_turn(&core, &sid, "cancelled", None);
    let a = unit(&k, &parent, "a");
    assert_eq!(
        (a.status.as_str(), a.reason),
        ("running", None),
        "the pause's"
    );
}

/// ST-37 #4 contrast without a restart: the next turn you stop yourself fails the unit.
#[test]
fn after_a_quick_resume_your_stop_of_the_next_turn_fails_the_unit() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let sid = unit(&k, &parent, "a").session_id.unwrap();
    change(&k, &parent, "pause", None);
    change(&k, &parent, "resume", None);
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    assert_eq!(unit(&k, &parent, "a").status, "running");
    assert!(k.core.cancel_turn(&sid).unwrap());
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    assert_eq!(unit(&k, &parent, "a").status, "failed");
}

/// ST-37 #5: unit a reports its failure itself; its turn is over on the runtime but the end is held
/// up; a is retried (the failed attempt retired) and the old end comes late. The old child closes
/// as failed with the failure's own reason, never as cancelled by the reset; the new attempt keeps
/// running and the coordinator hears nothing old. A snapshot archived as cancelled (rows retired
/// before this fix) reads as failed from the graph's history too.
#[test]
fn late_failed_end_before_retired_tidy_preserves_failed_outcome() {
    for legacy in [false, true] {
        let k = kit(2);
        let parent = planned(&k, &["a"]);
        let coord = k.session(&parent, "idle");
        let old = unit(&k, &parent, "a");
        let old_child = old.item_id.clone().unwrap();
        let old_sid = old.session_id.clone().unwrap();
        let attempt = k.rt.running(&old_sid).unwrap().attempt;
        let s = k.db().session(&old_sid).unwrap().unwrap();
        k.core
            .tool_workflow(&s, json!({"action":"fail","reason":"实测失败证据"}))
            .unwrap();
        over_untold(&k, &old_sid);
        change(&k, &parent, "retry", Some("a"));
        let new = unit(&k, &parent, "a");
        assert_eq!((new.status.as_str(), new.attempt), ("running", 2));
        if legacy {
            k.db()
                .store()
                .with_conn(|c| {
                    c.execute(
                        "UPDATE workflow_attempts SET snapshot=json_set(snapshot,'$.status','cancelled','$.reason','尝试已失效：调整') WHERE item_id=?",
                        [&old_child],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let (_, retired) = k
            .db()
            .store()
            .workflows()
            .by_child(&old_child)
            .unwrap()
            .unwrap();
        assert_eq!(
            (retired.status.as_str(), retired.reason.as_deref()),
            ("failed", Some("实测失败证据")),
            "legacy {legacy}"
        );
        let told = k.rt.prompts_of(&coord).len();

        end_of(&k.core, &old_sid, &attempt, "end_turn");
        let i = k.db().item(&old_child).unwrap().unwrap();
        assert_eq!(
            (i.phase.as_str(), i.outcome.as_deref()),
            ("closed", Some("failed")),
            "legacy {legacy}: {i:?}"
        );
        let new = unit(&k, &parent, "a");
        assert_eq!((new.status.as_str(), new.attempt), ("running", 2));
        assert_eq!(k.phase(new.item_id.as_deref().unwrap()), "active");
        assert_eq!(k.rt.prompts_of(&coord).len(), told, "no stale update");
        assert_eq!(work(&k, &parent).0, "running");
    }
}

/// ST-37 #5 contrast: the old end never comes; after a restart the tidy closes the retired failed
/// attempt as failed with its reason, and leaves the new attempt running.
#[test]
fn a_retired_failed_attempt_is_closed_failed_after_a_restart() {
    let k = kit(2);
    let parent = planned(&k, &["a"]);
    let old = unit(&k, &parent, "a");
    let old_child = old.item_id.clone().unwrap();
    let old_sid = old.session_id.clone().unwrap();
    let s = k.db().session(&old_sid).unwrap().unwrap();
    k.core
        .tool_workflow(&s, json!({"action":"fail","reason":"实测失败证据"}))
        .unwrap();
    over_untold(&k, &old_sid);
    change(&k, &parent, "retry", Some("a"));
    let core = k.reopened();
    core.recover().unwrap();
    core.tx().workflow_tick().unwrap();
    let i = k.db().item(&old_child).unwrap().unwrap();
    assert_eq!(
        (i.phase.as_str(), i.outcome.as_deref()),
        ("closed", Some("failed")),
        "{i:?}"
    );
    assert_eq!(unit(&k, &parent, "a").status, "running");
}

/// ST-37 #6: the agent asked you to scan, then a tool call made that moot (a local substitute, no
/// authorization needed), and the turn ended with nothing said after the call. Its final reply is
/// empty: the obsolete request is not read as a wait on you, Steve continues it. A long request
/// streamed after the last tool call is read whole and stays yours.
#[test]
fn no_final_text_after_tool_does_not_revive_obsolete_request() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "请扫码授权，完成后告诉我。");
    k.db()
        .event_add(
            &s,
            steve_transcript::TOOL,
            json!({"tool": {"id": "t1", "status": "completed", "title": "改用本地替代，无需授权"}}),
        )
        .unwrap();
    assert_eq!(k.db().final_reply(&s).unwrap(), "");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
    assert!(k.open_fails(&i).is_empty());

    // the same with an ACP tool_call row
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "请扫码授权，完成后告诉我。");
    k.db()
        .event_add(
            &s,
            "tool_call",
            json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "status": "completed"}),
        )
        .unwrap();
    assert_eq!(k.db().final_reply(&s).unwrap(), "");

    // after the last tool call: a long request, streamed in pieces, read whole and yours
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.db()
        .event_add(&s, steve_transcript::TOOL, json!({"tool": {"id": "t1"}}))
        .unwrap();
    k.rt.say(&s, "请扫码授权，");
    k.rt.say(&s, "完成后告诉我。");
    for _ in 0..120 {
        k.rt.say(&s, "阅读说明。");
    }
    k.rt.say(&s, "我在这里等着");
    let reply = k.db().final_reply(&s).unwrap();
    assert!(
        reply.starts_with("请扫码授权，完成后告诉我。") && reply.ends_with("我在这里等着"),
        "{}",
        reply.len()
    );
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert_eq!(k.open_fails(&i).len(), 1, "the card is yours");

    // an update of a call begun before the reply, coming after it, does not hide the reply
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.db()
        .event_add(
            &s,
            steve_transcript::TOOL,
            json!({"tool": {"id": "t9", "status": "in_progress"}}),
        )
        .unwrap();
    k.rt.say(&s, "请扫码授权，完成后告诉我。");
    k.db()
        .event_add(
            &s,
            steve_transcript::TOOL,
            json!({"tool": {"id": "t9", "status": "completed"}}),
        )
        .unwrap();
    assert_eq!(
        k.db().final_reply(&s).unwrap(),
        "请扫码授权，完成后告诉我。"
    );
}

/// Your Stop whose outcome the backend could not tell (its answer an error): the Stop stays kept,
/// never weakened into an ordinary stop Steve continues. The idle session is yours at the stall
/// watch, and the turn's end when it comes is yours too. A Stop the backend says reached nothing
/// (the turn was over) is withdrawn: the next end is judged as usual.
#[test]
fn a_stop_with_an_unknown_outcome_stays_yours() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    let a = k.rt.running(&s).unwrap().attempt;
    k.rt.st().user_stop_error = true;
    assert!(k.core.cancel_turn(&s).is_err());
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(kept.stop_asked.as_deref(), Some(a.as_str()), "kept");
    // the turn is gone without an end told: the stall watch gives you the card
    over_untold(&k, &s);
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    let cards = k.open_fails(&i);
    assert!(cards[0].contains("你要求停止这一回合"), "{cards:?}");
    // its end comes after all: yours
    end_of(&k.core, &s, &a, "end_turn");
    assert!(k.db().store().sessions().stop(&s).unwrap().unwrap().yours);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");

    // a Stop that reached nothing is withdrawn
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.st().user_stop_error = false;
    assert!(!k.core.cancel_turn(&s).unwrap());
    assert_eq!(
        k.db()
            .store()
            .sessions()
            .stop(&s)
            .unwrap()
            .unwrap()
            .stop_asked,
        None
    );
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
}

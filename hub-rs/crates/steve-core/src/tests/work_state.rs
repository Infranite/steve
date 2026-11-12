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
    assert_eq!(k.core.tx().check_stalls(later()).unwrap(), std::slice::from_ref(&i));
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
    assert_eq!(k.core.tx().check_stalls(later()).unwrap(), std::slice::from_ref(&parent));
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

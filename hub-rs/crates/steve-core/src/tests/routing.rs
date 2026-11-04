//! Thread routing R3 (modules/thread-routing.md §10): questions are researched in the conversation, work
//! becomes items, unclear requests get a card, corrections work both ways, and the router never fails
//! into an item. The research executor is `FakeResearch`; the router is a fixed fake.

use crate::error::msg;
use crate::repo::{HubEvent, Message, WS};
use crate::testkit::{FakeResearch, Kit, Step};
use crate::thread::RerouteTo;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const A1: &str = "现在steve的知识系统是怎么运作的？给我讲一下";
const A3: &str = "事项页面信息太散了，得优化一下，你看看怎么设计一下";

/// The router answers with `f(message)`; the answer tier answers “直接回答”.
fn router(k: &Kit, f: impl Fn(&str) -> Value + Send + Sync + 'static) {
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router") {
            let m = p
                .lines()
                .rfind(|l| l.starts_with("User message: "))
                .and_then(|l| serde_json::from_str::<String>(&l["User message: ".len()..]).ok())
                .unwrap_or_default();
            Ok(f(&m).to_string())
        } else if p.starts_with("You are Steve") {
            Ok(r#"{"thought":"想","answer":"直接回答","offer":null}"#.into())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
}

/// The script of A1: one knowledge search, three files read, the answer in two parts.
fn a1_research() -> Arc<FakeResearch> {
    FakeResearch::new(vec![
        Step::Tool("mcp__steve__steve_knowledge_search", "知识系统"),
        Step::Text("知识系统分三层：入库、检索、注入对话。"),
        Step::Sleep(80),
        Step::Tool("Read", "hub-rs/crates/steve-knowledge/src/lib.rs"),
        Step::Tool("Read", "hub-rs/crates/steve-knowledge/src/retrieve.rs"),
        Step::Tool("Read", "hub-rs/crates/steve-knowledge/src/context.rs"),
        Step::Text("检索用 FTS5 加向量 [1]。"),
        Step::Sleep(80),
    ])
}

fn items(k: &Kit) -> usize {
    k.db().items(WS).unwrap().len()
}

/// Wait (≤5 s) for `n` messages.
async fn settle(k: &Kit, n: usize) -> Vec<Message> {
    for _ in 0..500 {
        let ms = k.db().messages(WS).unwrap();
        if ms.len() >= n {
            return ms;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("waited for {n}: {:#?}", k.db().messages(WS).unwrap());
}

async fn until(f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never happened");
}

fn route_log(k: &Kit, mid: &str) -> steve_store::RouteLog {
    route_opt(k, mid).expect("a route_log row")
}

fn route_opt(k: &Kit, mid: &str) -> Option<steve_store::RouteLog> {
    k.db().store().thread().route_of(mid).unwrap()
}

#[tokio::test]
async fn a1_a_question_is_researched_in_the_conversation() {
    let k = Kit::new();
    let fr = a1_research();
    assert!(k.core.set_researcher(fr.clone()));
    /* the model of 11-01 made it an item; the guards make it research */
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.9, "signals": ["deliverable"], "title": "讲解知识系统", "goal": "梳理 Steve 知识系统的运作", "bot": "steve", "project": null}),
    );
    let before = items(&k);
    let mid = k.core.say(WS, A1, &[]).unwrap();
    let ms = settle(&k, 2).await;
    let a = &ms[1];
    assert_eq!(items(&k), before, "no item");
    assert_eq!(a.item_id, None);
    assert_eq!(
        a.text,
        "知识系统分三层：入库、检索、注入对话。检索用 FTS5 加向量 [1]。"
    );
    let st = &a.meta["steps"];
    assert_eq!(st["route"]["kind"], "research");
    assert_eq!(st["route"]["modelKind"], "item");
    assert_eq!(st["route"]["guard"], "4");
    let tools: Vec<(&str, &str)> = st["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap(),
                t["args"]["brief"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(tools[0].0, "mcp__steve__steve_knowledge_search");
    assert_eq!(tools.iter().filter(|t| t.0 == "Read").count(), 3);
    assert_eq!(st["research"]["tools"], 4);
    assert_eq!(st["research"]["model"], "fake-research");
    assert_eq!(a.meta["chat"], A1, "the answer can be made an item");
    assert_eq!(a.meta["origin"], json!(mid));
    /* rule 4 keeps what the model wanted as an offer */
    assert_eq!(a.meta["offer"]["title"], "讲解知识系统");
    assert_eq!(a.meta["offer"]["state"], "open");
    /* drafts went out before the reply was stored */
    let ev = k.events.lock().unwrap().clone();
    let stored = ev
        .iter()
        .position(|e| *e == HubEvent::Message { id: a.id.clone() })
        .unwrap();
    let drafts = ev[..stored]
        .iter()
        .filter(|e| matches!(e, HubEvent::SteveDraft { mid: m, .. } if *m == mid))
        .count();
    assert!(drafts >= 2, "{drafts} drafts");
    assert!(
        ev.iter()
            .any(|e| matches!(e, HubEvent::SteveTool { name, .. } if name == "Read"))
    );
    assert!(ev.contains(&HubEvent::SteveThinking {
        on: true,
        ws: Some(WS.into()),
        phase: Some("research".into())
    }));
    /* the request the executor got */
    let req = fr.seen.lock().unwrap()[0].clone();
    assert_eq!(
        (req.question.as_str(), req.mid.as_str()),
        (A1, mid.as_str())
    );
    /* route_log and thread_runs */
    let l = route_log(&k, &mid);
    assert_eq!(
        (l.model_kind.as_str(), l.kind.as_str()),
        ("item", "research")
    );
    assert_eq!(l.guard.as_deref(), Some("4"));
    assert_eq!(l.reply_id.as_deref(), Some(a.id.as_str()));
    let runs = k.db().store().thread().runs_of_message(&mid).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "done");
    assert_eq!(runs[0].tools, 4);
    assert_eq!(runs[0].reply_id.as_deref(), Some(a.id.as_str()));
    assert!(runs[0].first_text_ms.is_some());

    /* taking the offer is not a correction */
    let h = k.core.offer(WS, &a.id, true).unwrap();
    let item = h.item.unwrap();
    assert_eq!(item.title, "讲解知识系统");
    let l = route_log(&k, &mid);
    assert_eq!(l.override_, None);
    assert_eq!(l.outcome.as_deref(), Some("offer_taken"));
    assert_eq!(
        k.db().message(&a.id).unwrap().unwrap().meta["offer"]["state"],
        "taken"
    );
    let ti = k.db().store().thread().item(&item.id).unwrap().unwrap();
    assert_eq!(ti.origin_msg, mid);
    assert!(ti.from_question);
    /* a second click: 409 with the same result */
    let again = k.core.offer(WS, &a.id, true).unwrap();
    assert!(again.already);
    assert_eq!(again.item.unwrap().id, item.id);
}

#[tokio::test]
async fn a4b_making_an_answer_an_item_is_a_correction_with_the_answer_as_context() {
    let k = Kit::new();
    k.core.set_researcher(a1_research());
    router(&k, |m| {
        if m == A1 {
            json!({"kind": "research", "confidence": 0.9, "signals": ["question"]})
        } else {
            json!({"kind": "item", "title": "知识系统文档", "goal": "写一份知识系统说明", "bot": "dev"})
        }
    });
    let mid = k.core.say(WS, A1, &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert!(ms[1].meta.get("offer").is_none());
    let item = k.core.promote(WS, &ms[1].id).await.unwrap();
    let s = k.db().sessions_of_item(&item.id).unwrap().pop().unwrap();
    let opening = &k.rt.prompts_of(&s.id)[0];
    assert!(
        opening.contains("这件事来自你和用户的主线对话。"),
        "{opening}"
    );
    assert!(opening.contains(A1));
    assert!(opening.contains("知识系统分三层：入库、检索、注入对话。检索用 FTS5 加向量 [1]。"));
    assert!(opening.contains(
        "读过的文件：hub-rs/crates/steve-knowledge/src/lib.rs、hub-rs/crates/steve-knowledge/src/retrieve.rs、hub-rs/crates/steve-knowledge/src/context.rs"
    ));
    let l = route_log(&k, &mid);
    assert_eq!(l.override_.as_deref(), Some("to_item"));
    assert_eq!(l.item_id.as_deref(), Some(item.id.as_str()));
    /* reroute {to: item} is the same button */
    let h = k
        .core
        .reroute(WS, &ms[1].id, RerouteTo::Item)
        .await
        .unwrap();
    assert_eq!(h.item.unwrap().id, item.id);
}

#[tokio::test]
async fn a2_a_clear_change_becomes_an_item() {
    let k = Kit::new();
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "改发送按钮文字", "goal": "把发送按钮文字改成“发送给 Steve”", "bot": "steve", "reply": "好，交给它了。 标签：前端。"}),
    );
    let before = items(&k);
    let mid = k
        .core
        .say(WS, "帮我把对话页发送按钮的文字改成‘发送给 Steve’", &[])
        .unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(items(&k), before + 1);
    let id = ms[1].item_id.clone().unwrap();
    assert_eq!(
        ms[1].text, "好，交给它了。",
        "the router's tag note is dropped"
    );
    let ti = k.db().store().thread().item(&id).unwrap().unwrap();
    assert_eq!(ti.origin_msg, mid);
    assert_eq!(ti.reply_msg.as_deref(), Some(ms[1].id.as_str()));
    let l = route_log(&k, &mid);
    assert_eq!(
        (l.kind.as_str(), l.item_id.as_deref()),
        ("item", Some(id.as_str()))
    );
}

/// Your own conversation no longer asks “在这里出方案，还是建成事项？” (ST-33): an unclear request is
/// answered here with a plan, and the work is offered as an item.
#[tokio::test]
async fn a3_an_unclear_request_is_planned_here_and_offered() {
    let k = Kit::new();
    k.core.set_researcher(a1_research());
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.6, "signals": ["change"], "title": "事项页布局优化", "goal": "重新设计事项页面", "bot": "steve"}),
    );
    let before = items(&k);
    let mid = k.core.say(WS, A3, &[]).unwrap();
    let ms = settle(&k, 2).await;
    let a = &ms[1];
    assert_eq!(items(&k), before, "no item");
    assert!(a.meta.get("suggest").is_none(), "no card asking first");
    assert!(a.text.starts_with("知识系统分三层"), "researched in place");
    assert_eq!(a.meta["offer"]["title"], "事项页布局优化");
    assert_eq!(a.meta["offer"]["state"], "open");
    let l = route_log(&k, &mid);
    assert_eq!(
        (l.kind.as_str(), l.guard.as_deref()),
        ("research", Some("plan"))
    );
    /* the offer makes the item in one click */
    let h = k.core.offer(WS, &a.id, true).unwrap();
    assert_eq!(h.item.unwrap().title, "事项页布局优化");
    assert_eq!(items(&k), before + 1);
}

#[tokio::test]
async fn a4a_an_item_goes_back_into_the_conversation() {
    let k = Kit::new();
    k.core.set_researcher(a1_research());
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "改按钮", "bot": "steve"}),
    );
    let mid = k.core.say(WS, "帮我把 X 改成 Y", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let id = ms[1].item_id.clone().unwrap();
    let s = k.db().sessions_of_item(&id).unwrap().pop().unwrap();
    let h = k
        .core
        .reroute(WS, &ms[1].id, RerouteTo::Inline)
        .await
        .unwrap();
    // the item is being cancelled: its running turn is stopped, the close follows the turn's end
    assert_eq!(h.item.as_ref().unwrap().phase, "closing");
    until(|| k.rt.st().cancelled.contains(&s.id)).await;
    k.rt.end_turn(&k.core, &s.id, "cancelled", None);
    assert_eq!(k.db().item(&id).unwrap().unwrap().status, "drop");
    assert!(k.rt.st().released.contains(&s.id) || k.rt.st().cancelled.contains(&s.id));
    assert!(
        k.timeline_texts(&id)
            .contains(&"改在对话里回答".to_string())
    );
    let ms = settle(&k, 3).await;
    let ans = ms.last().unwrap();
    assert_eq!(ans.meta["reply_to"], json!(ms[1].id));
    assert!(ans.text.starts_with("知识系统分三层"));
    let l = route_log(&k, &mid);
    assert_eq!(l.override_.as_deref(), Some("to_inline"));
    assert_eq!(
        k.db().store().thread().runs_of_message(&mid).unwrap().len(),
        1
    );
    /* an accepted item can't go back */
    let done = k.item("done", "完成", None);
    let m = k
        .db()
        .message_add(WS, "steve", "建了", Some(&done), None)
        .unwrap();
    let e = k.core.reroute(WS, &m, RerouteTo::Inline).await.unwrap_err();
    assert_eq!(e.to_string(), "这件事项已经验收，不能改回对话");
    assert!(matches!(e, crate::CoreError::Status(409, _)));
    let e = k
        .core
        .reroute(WS, "msg_nope", RerouteTo::Item)
        .await
        .unwrap_err();
    assert!(matches!(e, crate::CoreError::Status(404, _)));
}

#[tokio::test]
async fn a_failing_router_never_makes_an_item() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Err(msg("Command timed out after 60000 ms"))
        } else {
            Ok(r#"{"thought":"","answer":"先在这里回答"}"#.into())
        }
    });
    let before = items(&k);
    for t in ["帮我把 X 改成 Y", "建个事项吧", A1] {
        let mid = k.core.say(WS, t, &[]).unwrap();
        until(|| route_opt(&k, &mid).is_some_and(|l| l.reply_id.is_some())).await;
        let l = route_log(&k, &mid);
        assert!(l.fallback);
        assert!(l.kind == "answer" || l.kind == "research", "{}", l.kind);
        assert_eq!(l.guard.as_deref(), Some("6"));
    }
    assert_eq!(items(&k), before);
    /* unusable JSON too */
    k.gw.set(|p, _| {
        Ok(if p.starts_with("You are the router") {
            r#"{"kind":"build"}"#.into()
        } else {
            r#"{"answer":"好"}"#.into()
        })
    });
    let mid = k.core.say(WS, "随便", &[]).unwrap();
    until(|| route_opt(&k, &mid).is_some_and(|l| l.reply_id.is_some())).await;
    assert_eq!(items(&k), before);
}

/// What is said while Steve is on a turn waits for it (ST-33): held, visible, editable and withdrawable until
/// the turn ends; a stop ends the turn at once with the partial answer, and the queue goes next, in order,
/// after the reply it waited on — each message once.
#[tokio::test(flavor = "multi_thread")]
async fn while_research_runs_messages_wait_and_can_be_changed_or_withdrawn() {
    let k = Kit::new();
    let fr = FakeResearch::new(vec![Step::Text("部分"), Step::Gate, Step::Text("回答")]);
    k.core.set_researcher(fr.clone());
    router(&k, |m| {
        if m.starts_with("修") {
            json!({"kind": "item", "confidence": 0.9, "signals": ["change"], "title": m})
        } else {
            json!({"kind": "research", "confidence": 0.9, "signals": ["question"]})
        }
    });
    let q1 = k.core.say(WS, "问题一是什么？", &[]).unwrap();
    until(|| k.core.researching(WS)).await;
    assert!(k.core.main_busy(WS).unwrap());
    let before = items(&k);
    let fix = k.core.say(WS, "修登录页", &[]).unwrap();
    let q2 = k.core.say(WS, "问题二？", &[]).unwrap();
    let q3 = k.core.say(WS, "问题三？", &[]).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    for m in [&fix, &q2, &q3] {
        assert_eq!(k.db().message(m).unwrap().unwrap().meta["held"], true);
        assert!(route_opt(&k, m).is_none(), "not routed while the turn runs");
    }
    assert_eq!(items(&k), before, "work waits too");
    /* changed while it waits: the new words are what gets routed */
    assert!(
        k.core
            .thread_queued_edit(WS, &q2, "问题二改过了？")
            .unwrap()
    );
    assert_eq!(k.db().message(&q2).unwrap().unwrap().text, "问题二改过了？");
    /* withdrawn: gone, with its words back */
    let (t, _) = k.core.thread_queued_withdraw(WS, &q3).unwrap().unwrap();
    assert_eq!(t, "问题三？");
    assert!(k.db().message(&q3).unwrap().is_none());
    assert!(k.core.thread_queued_withdraw(WS, &q3).unwrap().is_none());
    /* stop the running one: its partial answer stays, marked; the queue goes next */
    assert!(k.core.stop(WS).unwrap());
    until(|| {
        k.db()
            .store()
            .thread()
            .runs_of_message(&q1)
            .unwrap()
            .first()
            .is_some_and(|r| r.status == "stopped")
    })
    .await;
    let r1 = route_log(&k, &q1).reply_id.unwrap();
    assert_eq!(
        k.db().message(&r1).unwrap().unwrap().text,
        "部分\n\n（已停下）"
    );
    fr.open(10);
    until(|| items(&k) == before + 1).await;
    until(|| route_opt(&k, &q2).is_some_and(|l| l.reply_id.is_some())).await;
    /* gone once it went: no more changes */
    assert!(!k.core.thread_queued_edit(WS, &q2, "太晚了").unwrap());
    assert!(k.core.thread_queued_withdraw(WS, &q2).unwrap().is_none());
    let ms = k.db().messages(WS).unwrap();
    let at = |id: &str| ms.iter().position(|m| m.id == id).unwrap();
    assert!(
        at(&r1) < at(&fix) && at(&fix) < at(&q2),
        "after the reply they waited on, in order"
    );
    for m in [&fix, &q2] {
        assert!(
            k.db()
                .message(m)
                .unwrap()
                .unwrap()
                .meta
                .get("held")
                .is_none()
        );
    }
    let routed: Vec<String> =
        k.gw.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with("You are the router"))
            .filter_map(|p| {
                p.lines()
                    .rfind(|l| l.starts_with("User message: "))
                    .map(String::from)
            })
            .collect();
    assert_eq!(
        routed,
        vec![
            "User message: \"问题一是什么？\"",
            "User message: \"修登录页\"",
            "User message: \"问题二改过了？\"",
        ],
        "each once, as changed, the withdrawn one never"
    );
}

#[tokio::test]
async fn an_answer_with_an_offer_is_taken_by_a_confirmation() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            if p.contains("User message: \"好，改吧\"") {
                assert!(p.contains("（提议：让评论支持 markdown）"), "the router sees the offer");
                Ok(r#"{"kind":"item","confidence":0.9,"signals":["explicit"],"title":"x"}"#.into())
            } else {
                Ok(r#"{"kind":"answer","confidence":0.8}"#.into())
            }
        } else {
            Ok(r#"{"thought":"","answer":"评论现在是纯文本。","offer":{"title":"修评论渲染","goal":"让评论支持 markdown"}}"#.into())
        }
    });
    k.core.say(WS, "评论支持 markdown 吗", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].meta["offer"]["title"], "修评论渲染");
    let mid = k.core.say(WS, "好，改吧", &[]).unwrap();
    let ms = settle(&k, 4).await;
    let item = ms[3].item_id.clone().unwrap();
    assert_eq!(k.db().item(&item).unwrap().unwrap().title, "修评论渲染");
    assert_eq!(
        k.db().message(&ms[1].id).unwrap().unwrap().meta["offer"]["state"],
        "taken"
    );
    let first = route_log(&k, &ms[0].id);
    assert_eq!(first.outcome.as_deref(), Some("offer_taken"));
    assert_eq!(first.override_, None);
    assert_eq!(
        route_log(&k, &mid).reply_id.as_deref(),
        Some(ms[3].id.as_str())
    );
}

#[tokio::test]
async fn status_and_supplement_and_unsure_supplement() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    let item = i.clone();
    router(&k, move |m| {
        if m.contains("进展") {
            json!({"kind": "status", "item": item, "confidence": 0.9})
        } else if m.contains("先别改") {
            json!({"kind": "supplement", "item": item, "confidence": 0.9})
        } else {
            json!({"kind": "supplement", "item": item, "confidence": 0.5})
        }
    });
    k.core.say(WS, &format!("{i} 进展怎样了？"), &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert!(ms[1].text.starts_with(&format!("{i}「")), "{}", ms[1].text);
    assert!(
        k.rt.prompts_of(&s).is_empty(),
        "asking about it does not disturb it"
    );
    k.core.say(WS, "先别改列表页", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(ms[3].text, format!("已作为补充送到 {i}。"));
    assert_eq!(k.rt.prompts_of(&s).len(), 1);
    k.core.say(WS, "标题再短一点", &[]).unwrap();
    let ms = settle(&k, 6).await;
    assert_eq!(ms[5].text, format!("这是给 {i} 的吗？"));
    k.core.suggest(WS, &ms[5].id, "item").unwrap();
    /* the session is busy with the first one: the second waits in its queue */
    let sent = |k: &Kit| k.rt.prompts_of(&s).len() + k.db().queued(&s).unwrap().len();
    assert_eq!(sent(&k), 2);
    /* the menu on a user message: send it to an item */
    let h = k
        .core
        .reroute(WS, &ms[4].id, RerouteTo::Supplement(i.clone()))
        .await
        .unwrap();
    assert_eq!(h.item.unwrap().id, i);
    assert_eq!(
        route_log(&k, &ms[4].id).override_.as_deref(),
        Some("to_supplement")
    );
}

#[tokio::test]
async fn hub_restart_interrupts_research() {
    let k = Kit::new();
    let th = k.db().store().thread();
    let mid = k.db().message_add(WS, "me", "问", None, None).unwrap();
    th.run_add(&steve_store::NewThreadRun {
        workspace_id: WS.into(),
        message_id: mid.clone(),
        ..Default::default()
    })
    .unwrap();
    k.core.thread_recover().unwrap();
    assert_eq!(th.runs_of_message(&mid).unwrap()[0].status, "interrupted");
    let last = k.db().messages(WS).unwrap().pop().unwrap();
    assert_eq!(last.text, "（Hub 重启，服务就绪后会继续回答）");
    assert_eq!(last.meta["resume_pending"], json!(true));
}

/// A2 + R4: an item from the conversation reports back there, at the right place, and the user can talk
/// to it from the conversation.
#[tokio::test]
async fn r4_an_item_from_the_conversation_reports_back() {
    let k = Kit::new();
    router(&k, |m| {
        if m.contains("改成") {
            json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "改发送按钮", "goal": "把发送按钮文字改成“发送给 Steve”", "bot": "steve"})
        } else {
            json!({"kind": "answer", "confidence": 0.9})
        }
    });
    k.core.say(WS, "发送按钮现在叫什么？", &[]).unwrap();
    settle(&k, 2).await;
    let origin = k
        .core
        .say(WS, "帮我把对话页发送按钮的文字改成‘发送给 Steve’", &[])
        .unwrap();
    let ms = settle(&k, 4).await;
    let announce = ms[3].clone();
    let id = announce.item_id.clone().unwrap();
    let ti = k.db().store().thread().item(&id).unwrap().unwrap();
    assert_eq!(ti.origin_msg, origin);
    let s = k.db().sessions_of_item(&id).unwrap().pop().unwrap();
    let opening = &k.rt.prompts_of(&s.id)[0];
    assert!(
        opening.contains("这件事来自你和用户的主线对话。"),
        "{opening}"
    );
    assert!(
        opening.contains("发送按钮现在叫什么？"),
        "the conversation before it"
    );
    assert!(opening.contains("第一段用三句话以内写结论"));

    /* the announcement carries the item's one card */
    let card = |k: &Kit| -> Vec<crate::repo::Message> {
        k.db()
            .messages(WS)
            .unwrap()
            .into_iter()
            .filter(|m| !crate::thread::hidden(m) && m.meta["card"]["item"] == json!(id))
            .collect()
    };
    assert_eq!(card(&k)[0].id, announce.id);
    assert_eq!(card(&k)[0].meta["card"]["state"], "run");

    /* nothing much below the card: it updates in place, no message of its own */
    k.rt.say(&s.id, "要用中文引号吗");
    k.core
        .tool_ask(&s, "按钮文字要带引号吗？", &["要".into(), "不要".into()])
        .unwrap();
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    let a = k.db().message(&announce.id).unwrap().unwrap();
    assert_eq!(a.meta["card"]["state"], "ask");
    assert_eq!(a.meta["card"]["text"], "按钮文字要带引号吗？");
    let dec = a.meta["card"]["decision"].as_str().unwrap().to_string();
    assert_eq!(k.db().messages(WS).unwrap().len(), 4, "no new message");
    /* answered in the conversation: the item gets the answer */
    k.core.resolve(&dec, "不要", "", "me").unwrap();
    assert!(k.rt.prompts_of(&s.id).iter().any(|p| p.contains("不要")));

    /* the card is far up by now: a delivery to accept moves it to the bottom, quoting the origin once */
    router(&k, |_| json!({"kind": "answer", "confidence": 0.9}));
    k.core.say(WS, "顺便问一下天气", &[]).unwrap();
    settle(&k, 6).await;
    k.core.say(WS, "明天呢", &[]).unwrap();
    settle(&k, 8).await;
    k.core
        .tool_deliver(
            &s,
            "按钮改好了，文字是发送给 Steve。\n\n细节：改了 composer.tsx。",
            &[],
            &[],
            &[],
        )
        .unwrap();
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    let last = k.db().messages(WS).unwrap().pop().unwrap();
    assert_eq!(last.item_id.as_deref(), Some(id.as_str()));
    assert_eq!(last.meta["card_only"], true);
    assert_eq!(last.meta["card"]["state"], "deliver");
    assert_eq!(last.meta["reply_to"], json!(origin));
    assert_eq!(
        last.meta["card"]["delivery"]["text"], "按钮改好了，文字是发送给 Steve。",
        "the first paragraph"
    );
    assert!(last.meta["card"]["delivery"]["decision"].is_string());
    assert_eq!(card(&k).len(), 1, "one card");
    let a = k.db().message(&announce.id).unwrap().unwrap();
    assert!(
        a.meta.get("card").is_none() && !crate::thread::hidden(&a),
        "the announcement stays, without the card"
    );

    /* quote-reply to the card: forwarded to the item; the card moves under it and the reply grows there */
    let before = k.rt.prompts_of(&s.id).len();
    let fwd = k
        .core
        .say_to(
            WS,
            None,
            "标题再短一点",
            &[],
            &[],
            crate::thread::SayTo {
                item: None,
                reply_to: Some(last.id.clone()),
            },
        )
        .unwrap();
    let ms = settle(&k, 11).await;
    let ack = ms[10].clone();
    assert_eq!(ack.text, format!("已作为补充送到 {id}。"));
    assert!(
        crate::thread::hidden(&k.db().message(&last.id).unwrap().unwrap()),
        "the card-only message steps aside"
    );
    assert_eq!(card(&k).len(), 1);
    assert_eq!(card(&k)[0].id, ack.id);
    until(|| k.rt.prompts_of(&s.id).len() > before || !k.db().queued(&s.id).unwrap().is_empty())
        .await;
    let s = k.db().sessions_of_item(&id).unwrap().pop().unwrap();
    k.rt.say(&s.id, "好，改成了“发送”。");
    let n0 = k.db().messages(WS).unwrap().len();
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    assert_eq!(
        k.db().messages(WS).unwrap().len(),
        n0,
        "the reply is no message of its own"
    );
    let back = k.db().message(&ack.id).unwrap().unwrap();
    assert_eq!(back.meta["card"]["reply"]["ref"], json!(fwd));
    assert!(
        back.meta["card"]["reply"]["text"]
            .as_str()
            .unwrap()
            .contains("改成了")
    );
    assert_eq!(
        back.meta["card"]["delivery"]["text"], "按钮改好了，文字是发送给 Steve。",
        "the delivery came along"
    );

    /* asking how it goes is answered here, without disturbing the session */
    let n = k.rt.prompts_of(&s.id).len() + k.db().queued(&s.id).unwrap().len();
    k.core
        .say_to(
            WS,
            None,
            "进展怎么样了？",
            &[],
            &[],
            crate::thread::SayTo {
                item: Some(id.clone()),
                reply_to: None,
            },
        )
        .unwrap();
    let ms = k.db().messages(WS).unwrap();
    let n0 = ms.len();
    let ms = settle(&k, n0 + 1).await;
    assert!(ms.last().unwrap().text.starts_with(&format!("{id}「")));
    assert_eq!(
        k.rt.prompts_of(&s.id).len() + k.db().queued(&s.id).unwrap().len(),
        n
    );

    /* a release failure is a notification, not a line in the conversation */
    let n0 = k.db().messages(WS).unwrap().len();
    crate::notify::notify(k.db(), WS, "release", "fail", "发布失败了", Some(&id)).unwrap();
    assert_eq!(k.db().messages(WS).unwrap().len(), n0);
    assert_eq!(k.core.notifications(WS).unwrap()[0].text, "发布失败了");

    /* lifecycle events never add messages: released and done land on the same card */
    crate::thread::flowback::flow(
        k.db(),
        &id,
        crate::thread::flowback::Update {
            kind: "release",
            text: "v9".into(),
            ..Default::default()
        },
    )
    .unwrap();
    crate::thread::flowback::flow(
        k.db(),
        &id,
        crate::thread::flowback::Update {
            kind: "done",
            text: format!("{id} 完成"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(k.db().messages(WS).unwrap().len(), n0);
    let c = &card(&k)[0].meta["card"];
    assert_eq!(
        (c["state"].as_str(), c["released"]["version"].as_str()),
        (Some("done"), Some("v9"))
    );
    assert!(c.get("decision").is_none());
}

/// The one-time cleanup: flow-back written one message per event is hidden (not deleted) and the
/// announcement gets the card it adds up to; a question's answer stays; release notices leave the
/// conversation. Runs once.
#[tokio::test]
async fn old_flow_back_folds_into_the_card() {
    let k = Kit::new();
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "改按钮", "bot": "steve"}),
    );
    let origin = k.core.say(WS, "把按钮改一下", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let id = ms[1].item_id.clone().unwrap();
    let db = k.db();
    /* what the previous build wrote */
    let mut meta = ms[1].meta.clone();
    meta.as_object_mut().unwrap().remove("card");
    meta["updates"] =
        json!([{"kind": "fail", "item": id, "text": format!("{id} 卡住了：会话出错"), "at": 1}]);
    db.message_patch_meta(&ms[1].id, &meta).unwrap();
    let old = |text: &str, up: serde_json::Value| {
        db.message_add(
            WS,
            "steve",
            text,
            Some(&id),
            Some(&json!({"update": up, "reply_to": origin})),
        )
        .unwrap()
    };
    let d1 = old(
        "改好了",
        json!({"kind": "deliver", "item": id, "text": "改好了", "at": 2}),
    );
    let n1 = db
        .message_add(
            WS,
            "steve",
            "批准了发布，但没有发布：冲突",
            Some(&id),
            Some(&json!({"notice": {"kind": "release", "level": "fail"}})),
        )
        .unwrap();
    let d2 = old(
        &format!("{id} 完成：改按钮"),
        json!({"kind": "done", "item": id, "text": "", "at": 3}),
    );
    let ans = old(
        "答案全文",
        json!({"kind": "deliver", "item": id, "text": "答案全文", "full": true, "at": 4}),
    );
    db.message_add_in(
        WS,
        "notify",
        "steve",
        "已发布Hub 和前端新版本 20261102-004055-fd0fd701",
        Some(&id),
        &json!({"notice": {"kind": "release", "level": "success"}}),
    )
    .unwrap();
    let hid = crate::thread::flowback::cleanup(db, WS).unwrap();
    for m in [&d1, &n1, &d2] {
        assert!(hid.contains(m), "{m} hidden");
        assert!(db.message(m).unwrap().is_some(), "not deleted");
    }
    assert!(!hid.contains(&ans));
    let a = db.message(&ans).unwrap().unwrap();
    assert!(
        a.meta.get("update").is_none() && !crate::thread::hidden(&a),
        "a question's answer stays a normal message"
    );
    let shown: Vec<String> = k
        .core
        .thread(WS)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(!shown.contains(&d1) && !shown.contains(&n1) && shown.contains(&ans));
    let card = db.message(&ms[1].id).unwrap().unwrap().meta["card"].clone();
    assert_eq!(card["state"], "release");
    assert_eq!(card["delivery"]["text"], "答案全文");
    assert_eq!(card["released"]["version"], "20261102-004055-fd0fd701");
    assert!(
        crate::thread::flowback::cleanup(db, WS).unwrap().is_empty(),
        "once"
    );
}

/// A non-conversation item gets no flow-back and its prompt has no conversation opening.
#[tokio::test]
async fn r4_other_items_are_untouched() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    let s = k.db().session(&s).unwrap().unwrap();
    let n = k.db().messages(WS).unwrap().len();
    k.core.tool_ask(&s, "问题？", &["a".into()]).unwrap();
    k.core.tool_deliver(&s, "做完了", &[], &[], &[]).unwrap();
    assert_eq!(k.db().messages(WS).unwrap().len(), n);
}

#[tokio::test]
async fn a_follow_up_resumes_the_research_session() {
    let k = Kit::new();
    let fr = a1_research();
    k.core.set_researcher(fr.clone());
    router(
        &k,
        |_| json!({"kind": "research", "confidence": 0.9, "signals": ["question"]}),
    );
    let q1 = k.core.say(WS, A1, &[]).unwrap();
    settle(&k, 2).await;
    let run1 = k.db().store().thread().runs_of_message(&q1).unwrap()[0].clone();
    assert_eq!(run1.claude_session.as_deref(), Some("cs_1"));
    let q2 = k.core.say(WS, "重点讲检索那块", &[]).unwrap();
    settle(&k, 4).await;
    let seen = fr.seen.lock().unwrap().clone();
    assert!(seen[0].resume.is_none());
    let r = seen[1].resume.clone().expect("a follow-up resumes");
    assert_eq!(
        (r.run.as_str(), r.claude_session.as_str(), r.model.as_str()),
        (run1.id.as_str(), "cs_1", "fake-research")
    );
    assert!(r.prior_answer.starts_with("知识系统分三层"));
    let run2 = k.db().store().thread().runs_of_message(&q2).unwrap()[0].clone();
    assert_eq!(run2.prev_run.as_deref(), Some(run1.id.as_str()));
    /* the run had its own MCP token while it ran, cleared at the end */
    let mcp = seen[1].mcp.clone().unwrap();
    assert!(mcp.url.ends_with("/mcp"));
    assert_eq!(run2.token_hash, None);
    assert!(
        k.db()
            .store()
            .thread()
            .run_by_token(&mcp.token)
            .unwrap()
            .is_none()
    );
    /* something else in between: a fresh run */
    k.db()
        .message_add(WS, "steve", "已发布新版本", None, None)
        .unwrap();
    k.core.say(WS, "再问一个", &[]).unwrap();
    settle(&k, 7).await;
    assert!(fr.seen.lock().unwrap()[2].resume.is_none());
}

#[tokio::test]
async fn an_explanation_is_posted_back_once() {
    let k = Kit::new();
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.95, "signals": ["explicit"], "title": "讲解", "bot": "steve"}),
    );
    let origin = k.core.say(WS, "建个事项讲讲知识系统", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let id = ms[1].item_id.clone().unwrap();
    let s = k.db().sessions_of_item(&id).unwrap().pop().unwrap();
    k.core
        .tool_deliver(&s, "## 知识系统\n\n分两种：记忆和文档。", &[], &[], &[])
        .unwrap();
    let ids = [id.as_str()];
    let posted = k.core.postback(&ids).unwrap();
    assert_eq!(posted.len(), 1);
    let m = k.db().message(&posted[0]).unwrap().unwrap();
    assert_eq!(m.text, "## 知识系统\n\n分两种：记忆和文档。");
    assert_eq!(m.meta["reply_to"], json!(origin));
    assert_eq!(m.meta["update"]["full"], true);
    assert!(k.core.postback(&ids).unwrap().is_empty(), "once");
}

/// Screenshots (prod 11-02 12:00, “我看不到你指的那块内容”): research gets the question's files, a
/// follow-up without files gets the screenshot it follows up on, a direct answer gets the image inline,
/// and the router's history says which messages came with files.
#[tokio::test]
async fn screenshots_reach_research_answers_and_follow_ups() {
    let k = Kit::new();
    let fr = a1_research();
    k.core.set_researcher(fr.clone());
    router(&k, |m| {
        if m.contains("看不出来") {
            json!({"kind": "answer", "confidence": 0.9})
        } else {
            json!({"kind": "research", "confidence": 0.9, "signals": ["question"]})
        }
    });
    let f = k.file("截图.png", "image/png", b"\x89PNG fake");
    k.core
        .say(WS, "这是啥机制？看着好乱啊", std::slice::from_ref(&f))
        .unwrap();
    settle(&k, 2).await;
    {
        let seen = fr.seen.lock().unwrap();
        assert_eq!(seen[0].files, vec![f.clone()]);
        assert!(seen[0].earlier_files.is_empty());
    }
    /* a follow-up answered directly: the screenshot it is about goes inline */
    k.core.say(WS, "这样你也看不出来吗？", &[]).unwrap();
    settle(&k, 4).await;
    let calls = k.gw.calls.lock().unwrap().clone();
    let route = calls
        .iter()
        .rfind(|c| c.starts_with("You are the router"))
        .unwrap();
    assert!(
        route.contains("这是啥机制？看着好乱啊\\n（附带 1 个附件：截图.png）"),
        "the router's history names the files: {route}"
    );
    let ans = calls.last().unwrap();
    assert!(ans.contains("用户在前面的消息里附带了 1 个附件"), "{ans}");
    let imgs = k.gw.images.lock().unwrap().last().unwrap().clone();
    assert_eq!(imgs.len(), 1, "the image goes inline");
    assert_eq!(imgs[0]["source"]["media_type"], "image/png");
    /* a research follow-up gets it too */
    k.core.say(WS, "那这块代码在哪", &[]).unwrap();
    settle(&k, 6).await;
    let seen = fr.seen.lock().unwrap().clone();
    assert!(seen[1].files.is_empty());
    assert_eq!(seen[1].earlier_files, vec![f]);
}

/// A message forwarded to an item carries its attachments into the item's session.
#[tokio::test]
async fn forwarded_messages_carry_their_files() {
    let k = Kit::new();
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "改按钮", "bot": "steve"}),
    );
    k.core.say(WS, "把按钮改一下", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let id = ms[1].item_id.clone().unwrap();
    let s = k.db().sessions_of_item(&id).unwrap().pop().unwrap();
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    let f = k.file("新图.png", "image/png", b"\x89PNG fake");
    k.core
        .say_to(
            WS,
            None,
            "照这张图改",
            std::slice::from_ref(&f),
            &[],
            crate::thread::SayTo {
                item: Some(id.clone()),
                reply_to: None,
            },
        )
        .unwrap();
    settle(&k, 4).await;
    until(|| {
        k.rt.st()
            .files
            .iter()
            .any(|(sid, fs)| *sid == s.id && fs.contains(&f))
            || k.db()
                .queued(&s.id)
                .unwrap()
                .iter()
                .any(|q| q.files.contains(&f))
    })
    .await;
    let tl = k.db().timeline(&id).unwrap();
    assert!(tl.iter().any(|t| t.text == "照这张图改"));
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_local_answer_recovers_after_ready_once_across_two_boots() {
    let k = Kit::new();
    router(&k, |_| json!({"kind":"answer"}));
    k.core
        .set_researcher(FakeResearch::new(vec![Step::Text("安全续答")]));
    let th = k.db().store().thread();
    let mid = k
        .db()
        .message_add(WS, "me", "继续解释", None, None)
        .unwrap();
    let run = th
        .run_add(&steve_store::NewThreadRun {
            workspace_id: WS.into(),
            message_id: mid.clone(),
            ..Default::default()
        })
        .unwrap();
    k.core.thread_recover().unwrap();
    k.core.thread_recover().unwrap();
    assert_eq!(th.run(&run.id).unwrap().unwrap().status, "interrupted");
    assert_eq!(th.runs_to_resume(0).unwrap().len(), 1);
    k.core.thread_ready().unwrap();
    k.core.thread_ready().unwrap();
    assert!(k.wait(|| th.run(&run.id).unwrap().unwrap().status == "done"));
    let answers = k
        .db()
        .messages(WS)
        .unwrap()
        .into_iter()
        .filter(|m| m.meta["run_receipt"] == true)
        .collect::<Vec<_>>();
    assert_eq!(answers.len(), 1);
    // Simulate the tiny crash window after answer persistence but before the final run receipt.
    th.run_patch(
        &run.id,
        &steve_store::RunPatch {
            status: Some("running".into()),
            ..Default::default()
        },
    )
    .unwrap();
    k.core.thread_recover().unwrap();
    assert_eq!(th.run(&run.id).unwrap().unwrap().status, "done");
    assert!(th.runs_to_resume(0).unwrap().is_empty());
    assert_eq!(th.runs_of_message(&mid).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_main_answer_that_keeps_taking_the_hub_down_is_re_answered_once() {
    let k = Kit::new();
    router(&k, |_| json!({"kind":"answer"}));
    let th = k.db().store().thread();
    let mid = k
        .db()
        .message_add(WS, "me", "解释一下", None, None)
        .unwrap();
    let run = th
        .run_add(&steve_store::NewThreadRun {
            workspace_id: WS.into(),
            message_id: mid.clone(),
            ..Default::default()
        })
        .unwrap();
    k.core.thread_recover().unwrap();
    // a re-answer already started once and the Hub stopped during it (counted before it starts)
    k.db()
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE thread_resume SET attempts = 1 WHERE run_id = ?",
                [&run.id],
            )?)
        })
        .unwrap();
    k.core.thread_recover().unwrap();
    let r = k.core.thread_ready().unwrap();
    assert_eq!((r.resumed, r.held), (0, 1), "no second automatic re-answer");
    let msgs = k.db().messages(WS).unwrap();
    assert!(
        msgs.iter()
            .any(|m| m.meta["rerun"] == json!(mid) && m.text.contains("不再自动继续")),
        "handed to an explicit rerun"
    );
    assert_eq!(
        msgs.iter()
            .filter(|m| m.text.contains("服务就绪后会继续回答"))
            .count(),
        1,
        "“will continue” said once"
    );
    assert!(th.runs_to_resume(0).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_re_answer_is_counted_before_it_starts() {
    let k = Kit::new();
    router(&k, |_| json!({"kind":"answer"}));
    let th = k.db().store().thread();
    let mid = k
        .db()
        .message_add(WS, "me", "解释一下", None, None)
        .unwrap();
    let run = th
        .run_add(&steve_store::NewThreadRun {
            workspace_id: WS.into(),
            message_id: mid,
            ..Default::default()
        })
        .unwrap();
    k.core.thread_recover().unwrap();
    assert_eq!(th.resume_attempts(&run.id).unwrap(), 0);
    assert_eq!(k.core.thread_ready().unwrap().resumed, 1);
    assert_eq!(th.resume_attempts(&run.id).unwrap(), 1);
}

#[test]
fn a_boot_without_resuming_records_no_main_conversation_intent() {
    let k = Kit::with(|c| c.no_resume = true);
    let th = k.db().store().thread();
    let mid = k
        .db()
        .message_add(WS, "me", "解释一下", None, None)
        .unwrap();
    th.run_add(&steve_store::NewThreadRun {
        workspace_id: WS.into(),
        message_id: mid,
        ..Default::default()
    })
    .unwrap();
    k.core.thread_recover().unwrap();
    assert!(
        th.runs_to_resume(0).unwrap().is_empty(),
        "nothing a later boot could revive"
    );
}

#[test]
fn a_message_lost_before_routing_gets_a_rerun_notice_once() {
    let k = Kit::new();
    let lost = k
        .db()
        .message_add(WS, "me", "这条还没路由", None, None)
        .unwrap();
    let guarded = k
        .core
        .say_guarded(WS, "粘贴里有密钥 ***", "已拦下", "#/vault")
        .unwrap();
    k.core.thread_recover().unwrap();
    k.core.thread_recover().unwrap();
    let msgs = k.db().messages(WS).unwrap();
    let notes: Vec<_> = msgs
        .iter()
        .filter(|m| m.meta["interrupted"] == "route")
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].meta["rerun"], json!(lost));
    assert!(
        !msgs.iter().any(|m| m.meta["rerun"] == json!(guarded)),
        "a guarded paste is not lost"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn maintenance_keeps_incoming_chat_durable_and_resumes_it_once() {
    let k = Kit::new();
    router(
        &k,
        |_| json!({"kind":"answer","confidence":1.0,"why":"direct"}),
    );
    k.db().store().maintenance_set(true).unwrap();
    let mid = k.core.say(WS, "维护中排队", &[]).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(k.db().store().activities().is_empty());
    assert!(k.db().store().thread().route_of(&mid).unwrap().is_none());
    let durable: i64 = k
        .db()
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM settings WHERE key=?",
                [format!("thread.pending.{mid}")],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(durable, 1);
    k.core.thread_ready().unwrap();
    k.db().store().maintenance_set(false).unwrap();
    k.core.thread_ready().unwrap();
    assert!(k.wait(|| {
        k.db()
            .messages(WS)
            .unwrap()
            .iter()
            .any(|m| m.meta["run_receipt"] == true)
    }));
    assert_eq!(
        k.db().store().thread().runs_of_message(&mid).unwrap().len(),
        1
    );
}

#[tokio::test]
async fn only_a_complete_matching_durable_job_suppresses_the_rerun_notice() {
    let k = Kit::new();
    k.db().store().maintenance_set(true).unwrap();
    // Exercise the production serializer: these are real, complete Jobs waiting behind drain.
    let queued = k.core.say(WS, "维护期间排队的消息", &[]).unwrap();
    let invalid_json = k.core.say(WS, "JSON坏了", &[]).unwrap();
    let missing_fields = k.core.say(WS, "缺少结构字段", &[]).unwrap();
    let wrong_mid = k.core.say(WS, "消息不匹配", &[]).unwrap();
    let wrong_conv = k.core.say(WS, "对话不匹配", &[]).unwrap();
    k.db()
        .store()
        .with_conn(|c| {
            for (mid, kind) in [
                (&invalid_json, "json"),
                (&missing_fields, "fields"),
                (&wrong_mid, "mid"),
                (&wrong_conv, "conv"),
            ] {
                let key = format!("thread.pending.{mid}");
                let raw: String = c.query_row(
                    "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                    rusqlite::params![WS, key],
                    |r| r.get(0),
                )?;
                let mut job: Value = serde_json::from_str(&raw).unwrap();
                let damaged = match kind {
                    "json" => "not json".to_string(),
                    "fields" => json!({"mid":mid,"text":"缺少files/mentions/to/conv"}).to_string(),
                    "mid" => {
                        job["mid"] = json!(queued);
                        job.to_string()
                    }
                    "conv" => {
                        job["conv"]["space"] = json!("chan:another");
                        job.to_string()
                    }
                    _ => unreachable!(),
                };
                c.execute(
                    "UPDATE settings SET value=? WHERE workspace_id=? AND key=?",
                    rusqlite::params![damaged, WS, key],
                )?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(
        k.db().store().thread().unrouted_since(0).unwrap().len(),
        5,
        "Store returns candidates; only Core knows the Job schema"
    );
    k.core.thread_recover().unwrap();
    k.core.thread_recover().unwrap();
    let mut notes: Vec<String> = k
        .db()
        .messages(WS)
        .unwrap()
        .into_iter()
        .filter(|m| m.meta["interrupted"] == "route")
        .map(|m| m.meta["rerun"].as_str().unwrap().to_string())
        .collect();
    notes.sort();
    let mut expected = vec![invalid_json, missing_fields, wrong_mid, wrong_conv];
    expected.sort();
    assert_eq!(
        notes, expected,
        "each unusable job gets one rerun card; the real matching Job gets none"
    );
    assert!(!notes.contains(&queued));
}

#[tokio::test]
async fn ready_routes_only_pending_jobs_matching_the_stored_message_identity() {
    // Obtain the full production Job shape without letting the source queue execute.
    let source = Kit::new();
    source.db().store().maintenance_set(true).unwrap();
    let source_mid = source.core.say(WS, "恢复测试", &[]).unwrap();
    let raw: String = source
        .db()
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                rusqlite::params![WS, format!("thread.pending.{source_mid}")],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let template: Value = serde_json::from_str(&raw).unwrap();
    let k = Kit::new();
    router(&k, |_| json!({"kind":"answer"}));
    k.core
        .set_researcher(FakeResearch::new(vec![Step::Text("恢复完成")]));
    let mut rejected = Vec::new();
    let mut accepted = Vec::new();
    for kind in ["mid", "conv", "missing", "workspace", "main", "guest"] {
        let space = if kind == "guest" {
            "chan:valid"
        } else {
            "steve"
        };
        let mid = k
            .db()
            .message_add_in(WS, space, "me", "恢复测试", None, &json!({}))
            .unwrap();
        let mut job = template.clone();
        job["mid"] = json!(mid);
        job["conv"]["space"] = json!(space);
        let ws = if kind == "workspace" {
            "other-workspace"
        } else {
            WS
        };
        let mut key = format!("thread.pending.{mid}");
        match kind {
            "mid" => key = "thread.pending.some-other-message".into(),
            "conv" => job["conv"]["space"] = json!("chan:wrong"),
            "missing" => {
                job["mid"] = json!("missing-message");
                key = "thread.pending.missing-message".into();
            }
            _ => {}
        }
        let payload = job.to_string();
        k.db()
            .store()
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO settings(workspace_id,key,value,updated) VALUES(?,?,?,1)",
                    rusqlite::params![ws, key, payload],
                )?;
                Ok(())
            })
            .unwrap();
        if matches!(kind, "main" | "guest") {
            accepted.push(mid);
        } else {
            rejected.push((ws.to_string(), key, payload, mid));
        }
    }
    assert_eq!(k.core.thread_ready().unwrap().failed, 4);
    until(|| accepted.iter().all(|mid| route_opt(&k, mid).is_some())).await;
    assert_eq!(k.core.thread_ready().unwrap().failed, 4);
    for (ws, key, payload, mid) in rejected {
        assert!(
            route_opt(&k, &mid).is_none(),
            "invalid Job must never be routed"
        );
        let retained: String = k
            .db()
            .store()
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                    rusqlite::params![ws, key],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            retained, payload,
            "retain the original evidence across ready calls"
        );
    }
    assert!(route_opt(&k, "missing-message").is_none());
}

#[tokio::test]
async fn failed_research_tool_keeps_a_safe_diagnostic_in_reply_metadata() {
    let k = Kit::new();
    assert!(k.core.set_researcher(FakeResearch::new(vec![
        Step::ToolFailure("mcp__steve__steve_item_read", "ST-23", "permission_denied"),
        Step::Text("暂时无法查询。"),
    ])));
    router(
        &k,
        |_| json!({"kind":"research","confidence":0.9,"signals":["lookup"],"title":"查询事项"}),
    );
    k.core.say(WS, "查询 ST-23 的进展", &[]).unwrap();
    let messages = settle(&k, 2).await;
    let tool = &messages[1].meta["steps"]["tools"][0];
    assert_eq!(tool["status"], "failed");
    assert_eq!(tool["error"], "permission_denied");
    assert_eq!(tool["args"], json!({"brief":"ST-23"}));
}

#[tokio::test]
async fn research_reads_the_items_the_conversation_names_up_front() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.db()
        .item_patch(&item, crate::patch![now: "正在改对话页样式"])
        .unwrap();
    let fr = FakeResearch::new(vec![Step::Text("在改样式。"), Step::Text("对。")]);
    assert!(k.core.set_researcher(fr.clone()));
    router(
        &k,
        |_| json!({"kind":"research","confidence":0.9,"signals":["lookup"],"title":"查询事项"}),
    );
    k.core
        .say(WS, &format!("查一下 {} 的进展", item.to_lowercase()), &[])
        .unwrap();
    settle(&k, 2).await;
    // a follow-up that names nothing still gets the item from the turn before
    k.core.say(WS, "是的", &[]).unwrap();
    settle(&k, 4).await;
    let seen = fr.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    for r in &seen {
        assert!(
            r.context.contains("对话提到的事项（已读取）"),
            "{}",
            r.context
        );
        assert!(
            r.context.contains(&item) && r.context.contains("正在改对话页样式"),
            "{}",
            r.context
        );
    }
}

/// A failure on the card is over once the item works again: going on from the card, resuming it, the bot
/// reporting progress. The card goes back to running in place; the failure stays in the timeline.
#[tokio::test]
async fn a_failure_on_the_card_clears_when_the_item_goes_on() {
    let k = Kit::new();
    router(
        &k,
        |_| json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "改按钮", "bot": "steve"}),
    );
    k.core.say(WS, "把按钮改一下", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let id = ms[1].item_id.clone().unwrap();
    let s = k.db().sessions_of_item(&id).unwrap().pop().unwrap();
    let card = |k: &Kit| {
        k.db()
            .messages(WS)
            .unwrap()
            .into_iter()
            .rfind(|m| !crate::thread::hidden(m) && m.meta["card"]["item"] == json!(id))
            .unwrap()
            .meta["card"]
            .clone()
    };
    let stop = |k: &Kit| {
        k.rt.say(&s.id, "测试都通过了");
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        let c = card(k);
        assert_eq!(c["state"], "fail");
        assert!(c["text"].as_str().unwrap().contains("Agent 停下来了"));
        c["decision"].as_str().unwrap().to_string()
    };

    /* went on from the card */
    let d = stop(&k);
    k.core.resolve(&d, "continue", "", "me").unwrap();
    let c = card(&k);
    assert_eq!(c["state"], "run");
    assert!(c.get("text").is_none() && c.get("decision").is_none());

    /* looked at, then resumed: the line stays while it is paused */
    let d = stop(&k);
    k.core.resolve(&d, "look", "", "me").unwrap();
    assert_eq!(card(&k)["state"], "fail");
    k.core
        .item_action(&id, crate::ItemAction::Resume, "me")
        .unwrap();
    assert_eq!(card(&k)["state"], "run");

    /* the bot reports progress */
    stop(&k);
    k.core.tool_update(&s, "继续改", &[]).unwrap();
    assert_eq!(card(&k)["state"], "run");

    /* the failures stay in the item's history */
    let tl = k.db().timeline(&id).unwrap();
    assert_eq!(
        tl.iter()
            .filter(|t| t.text.starts_with("Agent 停下来了"))
            .count(),
        3
    );
}

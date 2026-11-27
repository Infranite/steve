//! Thread routing R3: questions are researched in the conversation, work
//! becomes items, unclear requests get a card, corrections work both ways, and the router never fails
//! into an item. The research executor is `FakeResearch`; the router is a fixed fake.

use crate::error::msg;
use crate::repo::{Message, WS};
use crate::testkit::{FakeResearch, Kit, Step};
use crate::thread::RerouteTo;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use steve_runtime::testkit::eventually_sync;

const A1: &str = "现在steve的知识系统是怎么运作的？给我讲一下";
const A3: &str = "事项页面信息太散了，得优化一下，你看看怎么设计一下";

/// The router answers with `f(message)`; the answer tier answers “直接回答”.
fn router(k: &Kit, f: impl Fn(&str) -> Value + Send + Sync + 'static) {
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router") {
            // A batch must receive an ordered array; invalid replies are not replayed one by one.
            if let Some((_, messages)) = p.rsplit_once("The user sent these messages in a row,") {
                let answers = messages
                    .lines()
                    .skip(1)
                    .map_while(|line| {
                        let (index, text) = line.trim_start().split_once(": ")?;
                        index.parse::<usize>().ok()?;
                        serde_json::from_str::<String>(text).ok()
                    })
                    .map(|text| f(&text))
                    .collect();
                return Ok(Value::Array(answers).to_string());
            }
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
        Step::Tool("steve_knowledge_search", "知识系统"),
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

fn route_log(k: &Kit, mid: &str) -> steve_store::ReplyReceipt {
    route_opt(k, mid).expect("a reply receipt row")
}

fn route_opt(k: &Kit, mid: &str) -> Option<steve_store::ReplyReceipt> {
    k.db()
        .store()
        .read(|r| steve_store::thread::receipt_by_origin(r, mid))
        .unwrap()
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
    assert_eq!(tools[0].0, "steve_knowledge_search");
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
        .position(|e| {
            *e == steve_types::BusEvent::from(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Message {
                    space: a.space.clone(),
                    ws: steve_types::WorkspaceId::from(steve_store::WS),
                    id: {
                        let value: String = a.id.clone();
                        steve_types::MessageId::from(value)
                    },
                },
            ))
        })
        .unwrap();
    let drafts = ev[..stored]
        .iter()
        .filter(|e| matches!(e, steve_types::BusEvent::Live(steve_types::LiveEvent::Work(steve_types::WorkLiveEvent::SteveDraft { mid: m, .. })) if m.as_str() == mid.as_str()))
        .count();
    assert!(drafts >= 2, "{drafts} drafts");
    assert!(
        ev.iter()
            .any(|e| matches!(e, steve_types::BusEvent::Live(steve_types::LiveEvent::Work(steve_types::WorkLiveEvent::SteveTool { name, .. })) if name == "Read"))
    );
    assert!(
        ev.contains(&steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
            steve_types::WorkLiveEvent::SteveThinking {
                on: true,
                ws: {
                    let value: Option<String> = Some(WS.into());
                    value
                        .map(Into::into)
                        .unwrap_or_else(|| steve_types::WorkspaceId::from(steve_store::WS))
                },
                phase: Some("research".into())
            }
        )))
    );
    /* the request the executor got */
    let req = fr.seen.lock().unwrap()[0].clone();
    assert_eq!(
        (req.question.as_str(), req.mid.as_str()),
        (A1, mid.as_str())
    );
    /* durable reply receipt and task */
    let l = route_log(&k, &mid);
    assert_eq!(
        (l.model_kind.as_str(), l.route_kind.as_str()),
        ("item", "research")
    );
    assert_eq!(l.route_guard.as_deref(), Some("4"));
    assert_eq!(l.reply_message_id.as_deref(), Some(a.id.as_str()));
    assert_eq!(l.outcome, "answered");
    assert_eq!(l.tools, 4);
    assert!(l.first_text_ms.is_some());
    let tasks = k
        .db()
        .store()
        .read(|r| steve_store::thread::tasks_of_origin(r, &mid))
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, "settled");

    /* taking the offer is not a correction; who took it asked for the item (a member only) */
    steve_store::core_fixture::thread::owner(k.db().store(), WS, "usr_dev", "dev", "Dev");
    let h = k
        .core
        .offer(WS, &a.id, true, None, Some("usr_dev"))
        .unwrap();
    let item = h.item.unwrap();
    let by = steve_store::core_fixture::thread::requested_by(k.db().store(), &item.id);
    assert_eq!(by.as_deref(), Some("usr_dev"));
    assert_eq!(item.title, "讲解知识系统");
    let l = route_log(&k, &mid);
    assert_eq!(l.route_override, None);
    assert_eq!(l.route_outcome.as_deref(), Some("offer_taken"));
    assert_eq!(
        k.db().message(&a.id).unwrap().unwrap().meta["offer"]["state"],
        "taken"
    );
    let ti = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_item(r, &item.id))
        .unwrap()
        .unwrap();
    assert_eq!(ti.origin_message_id, mid);
    assert!(ti.from_question);
    /* a second click: 409 with the same result */
    let again = k.core.offer(WS, &a.id, true, None, None).unwrap();
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
    let item = k.core.promote(WS, &ms[1].id, None).await.unwrap();
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
    assert_eq!(l.route_override.as_deref(), Some("to_item"));
    assert_eq!(l.item_id.as_deref(), Some(item.id.as_str()));
    /* reroute {to: item} is the same button */
    let h = k
        .core
        .reroute(WS, &ms[1].id, RerouteTo::Item, None)
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
        ms[1].text, "好，交给 Steve 开发 Bot 了，做完请你验收。 标签：feat。",
        "the router's tag note is dropped; the code adds the item's own, its category first"
    );
    let ti = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_item(r, &id))
        .unwrap()
        .unwrap();
    assert_eq!(ti.origin_message_id, mid);
    assert_eq!(ti.reply_message_id.as_deref(), Some(ms[1].id.as_str()));
    let l = route_log(&k, &mid);
    assert_eq!(
        (l.route_kind.as_str(), l.item_id.as_deref()),
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
        (l.route_kind.as_str(), l.route_guard.as_deref()),
        ("research", Some("3"))
    );
    /* the offer makes the item in one click */
    let h = k.core.offer(WS, &a.id, true, None, None).unwrap();
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
        .reroute(WS, &ms[1].id, RerouteTo::Inline, None)
        .await
        .unwrap();
    // the item is being cancelled: its running turn is stopped, the close follows the turn's end
    assert_eq!(h.item.as_ref().unwrap().phase.as_str(), "closing");
    eventually_sync(
        "The fake runtime records cancellation of the target session.",
        || k.rt.st().cancelled.contains(&s.id),
    )
    .await;
    k.rt.end_turn(&k.core, &s.id, "cancelled", None);
    assert_eq!(k.item_label(&id), "drop");
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
    assert_eq!(l.route_override.as_deref(), Some("to_inline"));
    assert_eq!(
        k.db()
            .store()
            .read(|r| steve_store::thread::tasks_of_origin(r, &mid))
            .unwrap()
            .len(),
        2
    );
    /* an accepted item can't go back */
    let done = k.item("done", "完成", None);
    let m = k
        .db()
        .message_add(WS, "steve", "建了", Some(&done), None)
        .unwrap();
    let e = k
        .core
        .reroute(WS, &m, RerouteTo::Inline, None)
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "这件事项已经验收，不能改回对话");
    assert!(matches!(e, crate::CoreError::Status(409, _)));
    let e = k
        .core
        .reroute(WS, "msg_nope", RerouteTo::Item, None)
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
    for t in ["帮我把 X 改成 Y", "把登录页改成深色模式", A1] {
        let mid = k.core.say(WS, t, &[]).unwrap();
        eventually_sync("The conversation route is bound to a reply.", || {
            route_opt(&k, &mid).is_some_and(|l| l.reply_message_id.is_some())
        })
        .await;
        let l = route_log(&k, &mid);
        assert!(l.route_fallback);
        assert!(
            l.route_kind == "answer" || l.route_kind == "research",
            "{}",
            l.route_kind
        );
        assert_eq!(l.route_guard.as_deref(), Some("6"));
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
    eventually_sync("The conversation route is bound to a reply.", || {
        route_opt(&k, &mid).is_some_and(|l| l.reply_message_id.is_some())
    })
    .await;
    assert_eq!(items(&k), before);
}

/// What is said while Steve is on a turn waits for it: held in view, editable and withdrawable until
/// the turn ends; a stop ends the turn at once with the partial answer, and the queue goes next,
/// in strict FIFO order — each message once.
#[tokio::test(flavor = "multi_thread")]
async fn while_research_runs_messages_wait_and_can_be_changed_or_withdrawn() {
    use steve_store::Views;
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
    eventually_sync(
        "The research turn starts before another message is submitted.",
        || k.core.researching(WS),
    )
    .await;
    assert!(k.core.main_busy(WS).unwrap());
    let before = items(&k);
    let fix = k.core.say(WS, "修登录页", &[]).unwrap();
    let q2 = k.core.say(WS, "问题二？", &[]).unwrap();
    let q3 = k.core.say(WS, "问题三？", &[]).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let is_held = |id: &str| {
        k.db()
            .store()
            .read(|r| r.message_views().of(WS, &[id.to_string()]))
            .unwrap()
            .first()
            .is_some_and(|v| serde_json::to_value(&v.meta).unwrap()["held"] == true)
    };
    for m in [&fix, &q2, &q3] {
        assert!(is_held(m), "{m} should be held in view");
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
    /* withdrawn: hidden tombstone retained so retries cannot resurrect it, and absent from views */
    let (t, _) = k.core.thread_queued_withdraw(WS, &q3).unwrap().unwrap();
    assert_eq!(t, "问题三？");
    let tomb = k.db().message(&q3).unwrap().expect("tombstone retained");
    assert_eq!(tomb.meta["hidden"], true);
    assert_eq!(tomb.meta["withdrawn"], true);
    assert!(
        k.db()
            .store()
            .read(|r| r.message_views().of(WS, std::slice::from_ref(&q3)))
            .unwrap()
            .is_empty()
    );
    assert!(k.core.thread_queued_withdraw(WS, &q3).unwrap().is_none());
    /* stop the running one: its partial answer stays, marked; the queue goes next */
    assert!(k.core.stop(WS).unwrap());
    eventually_sync(
        "The interrupted conversation turn stores its stopped receipt.",
        || route_opt(&k, &q1).is_some_and(|r| r.outcome == "stopped"),
    )
    .await;
    let r1 = route_log(&k, &q1).reply_message_id.unwrap();
    assert_eq!(
        k.db().message(&r1).unwrap().unwrap().text,
        "部分\n\n（已停下）"
    );
    fr.open(10);
    eventually_sync("Routing creates exactly one new work item.", || {
        items(&k) == before + 1
    })
    .await;
    eventually_sync("The conversation route is bound to a reply.", || {
        route_opt(&k, &q2).is_some_and(|l| l.reply_message_id.is_some())
    })
    .await;
    /* gone once it went: no more changes */
    assert!(!k.core.thread_queued_edit(WS, &q2, "太晚了").unwrap());
    assert!(k.core.thread_queued_withdraw(WS, &q2).unwrap().is_none());
    for m in [&fix, &q2] {
        assert!(!is_held(m), "{m} no longer held");
    }
    let calls: Vec<String> =
        k.gw.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with("You are the router"))
            .cloned()
            .collect();
    let routed: Vec<String> = calls
        .iter()
        .filter_map(|p| {
            p.lines()
                .rfind(|l| l.starts_with("User message: "))
                .map(String::from)
        })
        .collect();
    /* a plain question needs no router (#52, guard q) */
    assert_eq!(route_log(&k, &q1).route_guard.as_deref(), Some("q"));
    /* serial routing: fix routed once (no ahead), edited q2 routed with new words, withdrawn q3 never routed */
    assert_eq!(
        routed,
        vec![
            "User message: \"修登录页\"",
            "User message: \"问题二改过了？\"",
        ],
        "fix once, edited q2 routed once with new text, withdrawn never"
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
    assert_eq!(first.route_outcome.as_deref(), Some("offer_taken"));
    assert_eq!(first.route_override, None);
    assert_eq!(
        route_log(&k, &mid).reply_message_id.as_deref(),
        Some(ms[3].id.as_str())
    );
}

#[tokio::test]
async fn item_references_are_context_for_new_work() {
    for status in ["run", "done", "drop", "fail"] {
        for placement in ["bare", "hash", "middle", "lowercase"] {
            let k = Kit::new();
            let old = k.item(status, "执行", None);
            if status == "fail" {
                steve_store::core_fixture::item(
                    k.db().store(),
                    &old,
                    steve_store::core_fixture::ItemState::Failed,
                );
            }
            let session = k.session(&old, if status == "run" { "idle" } else { "ended" });
            let reference = match placement {
                "bare" => old.clone(),
                "hash" => format!("#{old}"),
                "middle" => format!("根据 {old} 的结果，"),
                _ => old.to_lowercase(),
            };
            let text = format!("{reference} 已开 issues #417–#420，请给每个 issue 分别建一个事项");
            router(&k, |_| {
                json!({"kind":"item", "confidence":0.95, "signals":["explicit"],
                "title":"逐项跟进 issue", "bot":"dev"})
            });
            let before = k.db().timeline(&old).unwrap().len();
            let file = k.file("issues.txt", "text/plain", b"issue context");
            let mid = k.core.say(WS, &text, std::slice::from_ref(&file)).unwrap();
            let ms = settle(&k, 2).await;
            let new = ms[1].item_id.as_deref().expect("new tracked work");
            assert_ne!(new, old, "{status}/{placement}: {}", ms[1].text);
            assert_eq!(
                k.db()
                    .goals(new)
                    .unwrap()
                    .last()
                    .unwrap()
                    .text
                    .lines()
                    .next(),
                Some(text.as_str())
            );
            assert_eq!(route_log(&k, &mid).route_kind, "item");
            assert_eq!(items(&k), 2);
            assert_eq!(k.db().timeline(&old).unwrap().len(), before);
            assert!(k.rt.prompts_of(&session).is_empty());
            assert!(k.db().queued(&session).unwrap().is_empty());
            assert_eq!(
                k.db().message(&mid).unwrap().unwrap().meta["files"],
                json!([file])
            );
        }
    }
}

#[tokio::test]
async fn explicit_new_work_overrides_a_mistaken_supplement() {
    for status in ["run", "done"] {
        let k = Kit::new();
        let old = k.item(status, "执行", None);
        let target = old.clone();
        router(&k, move |_| {
            json!({"kind":"supplement", "confidence":0.95, "item":target,
            "reply":"已送到旧事项", "bot":"dev"})
        });
        let text = format!("{old} 开了 issues #417–#420，请给每个 issue 建一个事项");
        let mid = k.core.say(WS, &text, &[]).unwrap();
        let ms = settle(&k, 2).await;
        assert_ne!(ms[1].item_id.as_deref().expect("new work"), old);
        assert!(!ms[1].text.contains("已送到旧事项"));
        assert_eq!(route_log(&k, &mid).route_guard.as_deref(), Some("2"));
        assert_eq!(items(&k), 2);
    }
}

#[tokio::test]
async fn clear_textual_supplements_still_reach_unfinished_items() {
    for status in ["run", "pause", "wait"] {
        let k = Kit::new();
        let old = k.item(status, "执行", None);
        let session = k.session(&old, "idle");
        let target = old.clone();
        router(
            &k,
            move |_| json!({"kind":"supplement", "confidence":0.95, "item":target}),
        );
        let text = format!("追加到 #{old}：记得覆盖跨日统计");
        k.core.say(WS, &text, &[]).unwrap();
        let ms = settle(&k, 2).await;
        assert_eq!(ms[1].text, format!("已作为补充送到 {old}。"));
        assert_eq!(k.rt.prompts_of(&session), vec![format!("补充信息：{text}")]);
        assert_eq!(items(&k), 1);
    }
}

#[tokio::test]
async fn questions_about_a_finished_item_remain_in_the_conversation() {
    for kind in ["status", "research"] {
        let k = Kit::new();
        let old = k.item("done", "收尾", None);
        let session = k.session(&old, "ended");
        let target = old.clone();
        router(
            &k,
            move |_| json!({"kind":kind, "confidence":0.95, "item":target}),
        );
        assert!(
            k.core
                .set_researcher(FakeResearch::new(vec![Step::Text("旧事项的结论")]))
        );
        k.core
            .say(WS, &format!("#{old} 进展怎么样了？"), &[])
            .unwrap();
        let ms = settle(&k, 2).await;
        assert!(ms[1].meta.get("error").is_none());
        assert_eq!(ms[1].item_id, None);
        assert_eq!(items(&k), 1);
        assert!(k.rt.prompts_of(&session).is_empty());
        assert!(k.db().queued(&session).unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_named_but_unsure_supplement_answers_inline_with_offer() {
    let k = Kit::new();
    let old = k.item("run", "执行", None);
    let session = k.session(&old, "idle");
    let target = old.clone();
    router(&k, move |_| {
        json!({"kind":"supplement", "confidence":0.6, "item":target,
        "title":"补充统计", "goal":"统计按小时展示"})
    });
    let text = format!("#{old} 那个统计也处理一下");
    let mid = k.core.say(WS, &text, &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(route_log(&k, &mid).route_kind, "research");
    assert!(ms[1].meta.get("suggest").is_none());
    assert_eq!(ms[1].meta["offer"]["state"], "open");
    assert!(k.rt.prompts_of(&session).is_empty());
    let first = k.core.offer(WS, &ms[1].id, true, None, None).unwrap();
    let second = k.core.offer(WS, &ms[1].id, true, None, None).unwrap();
    assert_ne!(first.item.unwrap().id, old);
    assert!(second.already);
}

#[tokio::test]
async fn a_closed_supplement_offers_separate_work_without_losing_the_request() {
    for status in ["done", "drop", "fail"] {
        for confidence in [0.6, 0.95] {
            let k = Kit::new();
            let old = k.item(status, "收尾", None);
            if status == "fail" {
                steve_store::core_fixture::item(
                    k.db().store(),
                    &old,
                    steve_store::core_fixture::ItemState::Failed,
                );
            }
            let session = k.session(&old, "ended");
            let target = old.clone();
            router(&k, move |_| {
                json!({"kind":"supplement", "confidence":confidence, "item":target,
                "reply":"已送到旧事项", "bot":"dev"})
            });
            let text = format!("{old} 的统计也需要调整");
            let file = k.file("context.txt", "text/plain", b"keep this");
            let mid = k.core.say(WS, &text, std::slice::from_ref(&file)).unwrap();
            let ms = settle(&k, 2).await;
            let card = &ms[1];
            assert!(card.meta.get("error").is_none());
            assert!(card.meta.get("suggest").is_none());
            assert_eq!(card.meta["offer"]["state"], "open");
            let made = k
                .core
                .offer(WS, &card.id, true, None, None)
                .unwrap()
                .item
                .unwrap();
            assert_ne!(made.id, old);
            assert_eq!(
                k.db()
                    .goals(&made.id)
                    .unwrap()
                    .last()
                    .unwrap()
                    .text
                    .lines()
                    .next(),
                Some(text.as_str())
            );
            assert!(
                k.core
                    .offer(WS, &card.id, true, None, None)
                    .unwrap()
                    .already
            );
            assert_eq!(items(&k), 2);
            assert!(k.rt.prompts_of(&session).is_empty());
            assert!(k.db().queued(&session).unwrap().is_empty());
            assert_eq!(k.db().session(&session).unwrap().unwrap().status, "ended");
            assert!(
                k.rt.st()
                    .files
                    .iter()
                    .any(|(_, files)| files.contains(&file))
            );
            let _ = route_log(&k, &mid);
        }
    }
}

#[tokio::test]
async fn a_closed_supplement_can_be_made_into_work_from_the_message_menu() {
    let k = Kit::new();
    let old = k.item("done", "收尾", None);
    let target = old.clone();
    router(
        &k,
        move |_| json!({"kind":"supplement", "confidence":0.95, "item":target}),
    );
    let text = format!("{old} 的统计也需要调整");
    let mid = k.core.say(WS, &text, &[]).unwrap();
    let ms = settle(&k, 2).await;
    let made = k
        .core
        .reroute(WS, &ms[1].id, RerouteTo::Item, None)
        .await
        .unwrap()
        .item
        .unwrap();
    assert_ne!(made.id, old);
    assert_eq!(k.db().goal(&made.id).unwrap(), text);
    assert_eq!(items(&k), 2);
    assert!(
        k.core
            .reroute(WS, &mid, RerouteTo::Item, None)
            .await
            .unwrap()
            .already
    );
}

#[tokio::test]
async fn stale_focus_and_quotes_of_closed_items_go_through_the_router() {
    for quoted in [false, true] {
        let k = Kit::new();
        let old = k.item("done", "收尾", None);
        let message = k
            .db()
            .message_add(WS, "steve", "旧交付", Some(&old), None)
            .unwrap();
        router(
            &k,
            |_| json!({"kind":"item", "confidence":0.95, "signals":["change"], "bot":"dev"}),
        );
        let to = crate::thread::SayTo {
            item: (!quoted).then_some(old.clone()),
            reply_to: quoted.then_some(message),
        };
        k.core.say_to(WS, None, "修复登录页", &[], &[], to).unwrap();
        let ms = settle(&k, 3).await;
        assert_ne!(ms[2].item_id.as_deref().expect("new work"), old);
        assert!(ms[2].meta.get("error").is_none());
        assert_eq!(items(&k), 2);
    }
}

#[tokio::test]
async fn a_supplement_target_closing_during_routing_is_checked_again() {
    let k = Kit::new();
    let old = k.item("run", "执行", None);
    let target = old.clone();
    let db = k.db().clone();
    router(&k, move |_| {
        steve_store::core_fixture::item(
            db.store(),
            &target,
            steve_store::core_fixture::ItemState::Closed {
                outcome: "completed",
            },
        );
        json!({"kind":"supplement", "confidence":0.95, "item":target})
    });
    k.core
        .say(WS, &format!("追加到 {old}：覆盖跨日统计"), &[])
        .unwrap();
    let ms = settle(&k, 2).await;
    assert!(ms[1].meta.get("suggest").is_none());
    assert_eq!(ms[1].meta["offer"]["state"], "open");
    assert_eq!(ms[1].meta["steps"]["route"]["guard"], "closed");
    assert!(ms[1].meta.get("error").is_none());
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
    assert!(ms[5].meta.get("suggest").is_none());
    assert_eq!(ms[5].meta["offer"]["state"], "open");
    /* the menu on a user message: send it to an item */
    let h = k
        .core
        .reroute(WS, &ms[4].id, RerouteTo::Supplement(i.clone()), None)
        .await
        .unwrap();
    assert_eq!(h.item.unwrap().id, i);
    let sent = |k: &Kit| k.rt.prompts_of(&s).len() + k.db().queued(&s).unwrap().len();
    assert_eq!(sent(&k), 2);
    assert_eq!(
        route_log(&k, &ms[4].id).route_override.as_deref(),
        Some("to_supplement")
    );
}

#[tokio::test]
async fn hub_restart_interrupts_research() {
    let k = Kit::new();
    let mid = k.db().message_add(WS, "me", "问", None, None).unwrap();
    let task_id = k
        .db()
        .store()
        .write(|w| {
            let now = steve_store::now();
            let _ = steve_store::thread::enqueue_revision(
                w,
                &steve_store::NewRevisionTask {
                    ws: WS,
                    space: crate::thread::MAIN,
                    origin_message_id: &mid,
                    user_id: None,
                    project_id: None,
                    conv_json: None,
                    to_item: None,
                    to_reply_to: None,
                    files_json: None,
                    mentions_json: None,
                    reply_to_message_id: None,
                    forced_kind: Some("research"),
                    route_json: None,
                    offer_json: None,
                    prev_receipt_id: None,
                    read_task_intent_key: None,
                },
            )?;
            let lease = steve_store::uid("lease");
            let claimed = steve_store::thread::claim_head(w, WS, crate::thread::MAIN, &lease, now)?
                .expect("claimed task");
            Ok::<_, steve_store::StoreError>(claimed.id)
        })
        .unwrap();
    k.core.thread_recover().unwrap();
    assert_eq!(
        k.db()
            .store()
            .read(|r| steve_store::thread::task_by_id(r, &task_id))
            .unwrap()
            .unwrap()
            .status,
        "settled"
    );
    let receipt = route_log(&k, &mid);
    assert_eq!(receipt.outcome, "unknown");
    let last = k.db().messages(WS).unwrap().pop().unwrap();
    assert_eq!(last.meta["unknown"], json!(true));
    assert!(!last.text.contains("继续回答"));
}

/// A2 + R4: an item from the conversation reports back there, at the right place, and the user can talk
/// to it from the conversation.
#[tokio::test]
async fn r4_an_item_from_the_conversation_reports_back() {
    let k = Kit::new();
    k.manual_acceptance();
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
    let ti = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_item(r, &id))
        .unwrap()
        .unwrap();
    assert_eq!(ti.origin_message_id, origin);
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
    eventually_sync(
        "The item receives its follow-up prompt or a queued message.",
        || k.rt.prompts_of(&s.id).len() > before || !k.db().queued(&s.id).unwrap().is_empty(),
    )
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
    steve_store::notify::notify(
        k.db().store(),
        WS,
        "release",
        "fail",
        "发布失败了",
        Some(&id),
    )
    .unwrap();
    assert_eq!(k.db().messages(WS).unwrap().len(), n0);
    assert_eq!(
        steve_store::notify::list_recent(k.db().store(), WS, 100).unwrap()[0].text,
        "发布失败了"
    );

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

    /* a failed build afterwards is its own line: the version stays a version */
    crate::thread::flowback::flow(
        k.db(),
        &id,
        crate::thread::flowback::Update {
            kind: "build",
            text: format!("{id} v9 的 Release 构建失败（failure）：https://ci/1。"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(k.db().messages(WS).unwrap().len(), n0);
    let c = &card(&k)[0].meta["card"];
    assert_eq!(c["released"]["version"], "v9");
    assert!(
        c["build"]["text"]
            .as_str()
            .unwrap()
            .contains("Release 构建失败")
    );
}

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
    let run1 = route_log(&k, &q1);
    assert_eq!(run1.harness_session.as_deref(), Some("cs_1"));
    assert_eq!(run1.harness.as_deref(), Some("claude-code"));
    let q2 = k.core.say(WS, "重点讲检索那块", &[]).unwrap();
    settle(&k, 4).await;
    let seen = fr.seen.lock().unwrap().clone();
    assert!(seen[0].resume.is_none());
    let r = seen[1].resume.clone().expect("a follow-up resumes");
    assert_eq!(
        (r.run.as_str(), r.session.as_str(), r.model.as_str()),
        (run1.id.as_str(), "cs_1", "fake-research")
    );
    // one that has to start over finds the earlier answer among its recent messages
    assert!(
        seen[1]
            .history
            .iter()
            .any(|h| h.author == "steve" && h.text.starts_with("知识系统分三层"))
    );
    let run2 = route_log(&k, &q2);
    assert_eq!(run2.harness_session.as_deref(), Some("cs_1"));
    /* the run had its own MCP token while it ran, cleared at the end */
    let mcp = seen[1].mcp.clone().unwrap();
    assert!(mcp.url.ends_with("/mcp"));
    let t2 = k
        .db()
        .store()
        .read(|r| steve_store::thread::tasks_of_origin(r, &q2))
        .unwrap();
    assert_eq!(t2[0].token_hash, None);
    assert!(
        k.db()
            .store()
            .read(|r| steve_store::thread::active_task_by_token(r, &mcp.token))
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
    /* not a plain question (a plain one is researched without the router, #52) */
    k.core.say(WS, "这样你也看不出来", &[]).unwrap();
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
    eventually_sync(
        "The forwarded message delivers its original attachment to the item.",
        || {
            k.rt.st()
                .files
                .iter()
                .any(|(sid, fs)| *sid == s.id && fs.contains(&f))
                || k.db()
                    .queued(&s.id)
                    .unwrap()
                    .iter()
                    .any(|q| q.files.contains(&f))
        },
    )
    .await;
    let tl = k.db().timeline(&id).unwrap();
    assert!(tl.iter().any(|t| t.text == "照这张图改"));
}

#[tokio::test]
async fn personal_attention_tracks_only_final_owned_inline_run_receipts() {
    let k = Kit::new();
    steve_store::core_fixture::thread::owner(
        k.db().store(),
        WS,
        "attention_user",
        "attention_user",
        "Reader",
    );
    router(&k, |_| json!({"kind":"answer"}));
    let mid = k
        .core
        .say_as(WS, Some("attention_user"), "解释这个概念", &[], &[])
        .unwrap();
    eventually_sync(
        "The inline conversation run records its final completion receipt.",
        || route_opt(&k, &mid).is_some_and(|r| r.outcome == "answered"),
    )
    .await;
    let receipt = route_log(&k, &mid);
    let reply = receipt
        .reply_message_id
        .as_ref()
        .expect("short answer names its persisted final receipt");
    let (entries, upper) =
        steve_store::core_fixture::thread::attention(k.db().store(), WS, "attention_user").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(&entries[0].id, reply);
    assert_eq!(entries[0].order, Some(upper));
    assert!(k.db().message(reply).unwrap().unwrap().meta["run_receipt"] == true);
    // An unowned workspace answer is not assigned to the signed-in user's attention.
    let unowned = k.core.say(WS, "再解释一下", &[]).unwrap();
    eventually_sync(
        "The inline conversation run records its final completion receipt.",
        || route_opt(&k, &unowned).is_some_and(|r| r.outcome == "answered"),
    )
    .await;
    assert_eq!(
        steve_store::core_fixture::thread::attention(k.db().store(), WS, "attention_user")
            .unwrap()
            .0
            .len(),
        1
    );
}

#[tokio::test]
async fn personal_attention_tracks_actual_stopped_short_answer_receipt() {
    let k = Kit::new();
    steve_store::core_fixture::thread::owner(
        k.db().store(),
        WS,
        "attention_user",
        "attention_user",
        "Reader",
    );
    router(&k, |_| json!({"kind":"answer"}));
    *k.gw.slow.lock().unwrap() = Some(("You are Steve", Duration::from_secs(60)));
    let mid = k
        .core
        .say_as(WS, Some("attention_user"), "解释这个概念", &[], &[])
        .unwrap();
    eventually_sync("The answer prompt reaches the fake model gateway.", || {
        k.gw.calls
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.starts_with("You are Steve"))
    })
    .await;
    assert!(k.core.stop(WS).unwrap());
    eventually_sync(
        "The interrupted conversation turn stores its stopped receipt.",
        || route_opt(&k, &mid).is_some_and(|r| r.outcome == "stopped"),
    )
    .await;
    let receipt = route_log(&k, &mid);
    let id = receipt
        .reply_message_id
        .as_ref()
        .expect("stopped answer names persisted receipt");
    let reply = k.db().message(id).unwrap().unwrap();
    assert_eq!(reply.meta["run_receipt"], true);
    assert_eq!(reply.meta["stopped"], true);
    assert_eq!(
        reply.meta["system"], true,
        "stopped receipt retains the upstream system-note classification"
    );
    assert_eq!(reply.meta["origin"], mid);
    let (entries, _) =
        steve_store::core_fixture::thread::attention(k.db().store(), WS, "attention_user").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(&entries[0].id, id);
    assert_eq!(
        steve_store::core_fixture::thread::attention_target(
            k.db().store(),
            WS,
            "attention_user",
            id
        )
        .unwrap()
        .0,
        *id
    );
}

#[test]
fn a_notice_written_in_a_transaction_stays_or_goes_with_it() {
    let k = Kit::new();
    let told = |text: &str, keep: bool| {
        k.db().store().write(|w| {
            steve_store::notify::notify_in(w, WS, "system", "fail", text, None)?;
            if keep {
                Ok(())
            } else {
                Err(steve_store::StoreError::NotFound(
                    "the rest of the write failed",
                ))
            }
        })
    };
    assert!(told("没写下来", false).is_err());
    assert!(
        steve_store::notify::list_recent(k.db().store(), WS, 100)
            .unwrap()
            .is_empty()
    );
    assert!(k.db().messages(WS).unwrap().is_empty());
    told("写下来了", true).unwrap();
    let n = steve_store::notify::list_recent(k.db().store(), WS, 100).unwrap();
    assert_eq!((n.len(), n[0].text.as_str()), (1, "写下来了"));
    // a failure without an item also leaves its line in the conversation, pointing at the notice
    let ms = k.db().messages(WS).unwrap();
    assert_eq!(
        (ms.len(), &ms[0].meta["notice"]["ref"]),
        (1, &json!(n[0].id))
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
    assert!(route_opt(&k, &mid).is_none());
    assert_eq!(
        steve_store::core_fixture::thread::thread_task_count(
            k.db().store(),
            Some((&mid, "queued"))
        ),
        1
    );
    k.core.thread_ready().unwrap();
    k.db().store().maintenance_set(false).unwrap();
    k.core.thread_ready().unwrap();
    steve_runtime::testkit::eventually_sync(
        "The incoming chat message receives its durable run receipt.",
        || {
            k.db()
                .messages(WS)
                .unwrap()
                .iter()
                .any(|m| m.meta["run_receipt"] == true)
        },
    )
    .await;
    assert_eq!(
        k.db()
            .store()
            .read(|r| steve_store::thread::tasks_of_origin(r, &mid))
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn failed_research_tool_keeps_a_safe_diagnostic_in_reply_metadata() {
    let k = Kit::new();
    assert!(k.core.set_researcher(FakeResearch::new(vec![
        Step::ToolFailure("steve_item_read", "ST-23", "permission_denied"),
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
async fn a_research_guard_stop_keeps_one_neutral_note_in_the_saved_reply() {
    for partial in [
        "已有部分结果。",
        "已有部分结果。\n\n（本次查询在工具检查时停止，尚未完成。）",
    ] {
        let k = Kit::new();
        assert!(k.core.set_researcher(FakeResearch::new(vec![
            Step::Text(partial),
            Step::End(crate::thread::RunEnd::Violated),
        ])));
        router(
            &k,
            |_| json!({"kind":"research","confidence":0.9,"signals":["lookup"],"title":"查询事项"}),
        );
        let mid = k.core.say(WS, "查询 ST-23 的进展", &[]).unwrap();
        eventually_sync("The conversation route is bound to a reply.", || {
            route_opt(&k, &mid).is_some_and(|l| l.reply_message_id.is_some())
        })
        .await;
        let reply = k
            .db()
            .message(&route_log(&k, &mid).reply_message_id.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            reply.text,
            "已有部分结果。\n\n（本次查询在工具检查时停止，尚未完成。）"
        );
        assert_eq!(reply.meta["steps"]["research"]["stopped"], "violation");
        assert_eq!(route_log(&k, &mid).outcome, "failed");
    }
}

#[tokio::test]
async fn research_reads_the_items_the_conversation_names_up_front() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.db()
        .timeline_add(&item, "dev", "progress", "正在改对话页样式", None)
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

/// A tool call is on the live line from the moment it begins: `steve.tool` with `running` and the
/// call's category, then the finished one.
#[tokio::test]
async fn a_tool_call_is_shown_while_it_runs() {
    let k = Kit::new();
    let fr = FakeResearch::new(vec![
        Step::Running("Bash", "exec", "du -sh ~"),
        Step::Sleep(20),
        Step::Tool("Bash", "du -sh ~"),
        Step::Text("主目录 12G。"),
    ]);
    assert!(k.core.set_researcher(fr.clone()));
    router(
        &k,
        |_| json!({"kind": "research", "confidence": 0.9, "signals": ["lookup"], "title": "", "goal": "", "bot": "steve", "project": null}),
    );
    let mid = k.core.say(WS, "主目录占了多少", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let calls: Vec<(String, String, String, bool)> = k
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
                steve_types::WorkLiveEvent::SteveTool {
                    mid: m,
                    name,
                    brief,
                    kind,
                    running,
                    ..
                },
            )) if m.as_str() == mid.as_str() => {
                Some((name.clone(), kind.clone(), brief.clone(), *running))
            }
            _ => None,
        })
        .collect();
    let call = |kind: &str, running| ("Bash".into(), kind.into(), "du -sh ~".into(), running);
    assert_eq!(calls, [call("exec", true), call("", false)]);
    /* only what finished is a step of the answer */
    let tools = ms[1].meta["steps"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{tools:?}");
    assert_eq!(tools[0]["status"], "ok");
}

#[tokio::test]
async fn offer_carries_route_tags_into_preview_and_item() {
    let k = Kit::new();
    let tag = k
        .db()
        .tag_create(WS, "文档", "领域", "文档与资料", "owner")
        .unwrap();
    k.db()
        .store()
        .bots()
        .seed(
            "bot_doc",
            false,
            &steve_store::NewBot {
                name: "文档".into(),
                role: Some("写和整理文档".into()),
                harness: "fixture".into(),
                perm_mode: Some("auto".into()),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    k.core
        .set_researcher(FakeResearch::new(vec![Step::Text("已梳理现状。")]));
    let tid = tag.id.clone();
    router(&k, move |_| {
        json!({
            "kind": "item",
            "confidence": 0.6,
            "signals": ["change"],
            "title": "更新说明",
            "goal": "整理本周变更",
            "tags": [tid],
            "project": null
        })
    });
    k.core
        .say(WS, "本周变更是不是得整理一下，你看看", &[])
        .unwrap();
    let ms = settle(&k, 2).await;
    let offer = &ms[1].meta["offer"];
    assert_eq!(offer["state"], "open", "{offer}");
    assert_eq!(offer["tags"], json!([tag.id]), "{offer}");
    assert_eq!(offer["bot"], "bot_doc", "{offer}");
    assert_eq!(offer["reason"], "description", "{offer}");
    assert_eq!(offer["matched"], json!(["文档"]), "{offer}");

    let handled = k.core.offer(WS, &ms[1].id, true, None, None).unwrap();
    let item = handled.item.unwrap();
    assert_eq!(item.bot_id.as_deref(), Some("bot_doc"));
    assert!(k.db().tags_of_item(&item.id).unwrap().contains(&tag.id));
    let initial = k
        .db()
        .store()
        .read(|r| steve_store::bot_dispatch::initial(r, &item.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        initial.explanation.reason,
        steve_types::PlacementReason::Description
    );
    assert_eq!(initial.explanation.matched, vec!["文档"]);
}

#[tokio::test]
async fn offer_explicit_bot_override_and_failed_override_recovery() {
    let k = Kit::new();
    for (id, enabled) in [("bot_doc", true), ("bot_off", false)] {
        k.db()
            .store()
            .bots()
            .seed(
                id,
                false,
                &steve_store::NewBot {
                    name: id.into(),
                    harness: "fixture".into(),
                    enabled: Some(enabled),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
    }
    for (id, ts) in [("dev", 100), ("bot_doc", 200), ("bot_off", 300)] {
        steve_store::core_fixture::bot_workspace_created(k.db().store(), WS, id, ts);
    }
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(json!({"kind": "answer", "confidence": 0.9}).to_string())
        } else {
            Ok(json!({
                "thought": "想",
                "answer": "直接回答",
                "offer": {"title": "补文档", "goal": "补齐说明"}
            })
            .to_string())
        }
    });
    k.core.say(WS, "看下这部分", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].meta["offer"]["bot"], "dev", "{}", ms[1].meta["offer"]);

    let err = k
        .core
        .offer(WS, &ms[1].id, true, Some("bot_off"), None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            crate::CoreError::Placement(steve_types::PlacementBlocked {
                reason: steve_types::PlacementRejection::Bot,
                ..
            })
        ),
        "{err}"
    );
    assert_eq!(
        k.db().message(&ms[1].id).unwrap().unwrap().meta["offer"]["state"],
        "open"
    );
    assert_eq!(items(&k), 0);

    let ok = k
        .core
        .offer(WS, &ms[1].id, true, Some("bot_doc"), None)
        .unwrap();
    let item = ok.item.unwrap();
    assert_eq!(item.bot_id.as_deref(), Some("bot_doc"));
    let initial = k
        .db()
        .store()
        .read(|r| steve_store::bot_dispatch::initial(r, &item.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        initial.explanation.reason,
        steve_types::PlacementReason::Explicit
    );
    let saved = k.db().message(&ms[1].id).unwrap().unwrap();
    assert_eq!(saved.meta["offer"]["state"], "taken");
    assert_eq!(saved.meta["offer"]["bot"], "bot_doc");
    assert_eq!(saved.meta["offer"]["reason"], "explicit");
    assert!(saved.meta["offer"].get("matched").is_none());
}

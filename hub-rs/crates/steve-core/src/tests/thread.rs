//! The main thread: lanes, /汇报, explicit supplements, routing to chat, config or an item, @bot, stop.

use crate::Tier;
use crate::error::msg;
use crate::repo::WS;
use crate::testkit::Kit;
use serde_json::json;
use std::time::Duration;

/// A router answering `route`; an item gets a work signal and confidence unless the test sets them
/// (without them the guards make it a suggestion card, see `thread::decide`).
fn router(k: &Kit, mut route: serde_json::Value, answer: &'static str) {
    if route["kind"] == "item" {
        route
            .as_object_mut()
            .unwrap()
            .entry("confidence")
            .or_insert(json!(0.9));
        route
            .as_object_mut()
            .unwrap()
            .entry("signals")
            .or_insert(json!(["change"]));
    }
    let r = route.to_string();
    k.gw.set(move |p, tier| {
        if p.starts_with("You are the router of Steve") {
            assert_eq!(tier, Tier::Fast);
            Ok(format!("好的：{r}"))
        } else if p.starts_with("You are Steve, the user's assistant.") {
            Ok(format!("  {answer}  "))
        } else {
            Err(msg("unexpected prompt"))
        }
    });
}

async fn settle(k: &Kit, n: usize) -> Vec<crate::repo::Message> {
    for _ in 0..300 {
        let ms = k.db().messages(WS).unwrap();
        if ms.len() >= n {
            return ms;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no reply: {:?}", k.db().messages(WS).unwrap());
}

#[tokio::test]
async fn digest() {
    let k = Kit::new();
    let a = k.item("run", "执行", None);
    k.decision(&a, None, "ask", "question", &[]);
    k.item("done", "收尾", None);
    k.core.say(WS, " /汇报 ", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].text, "现在：1 件等你，1 件在进行，今天完成 1 件。");
    let ev = k.events.lock().unwrap().clone();
    assert!(ev.iter().any(|e| matches!(
        e,
        steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
            steve_types::WorkLiveEvent::SteveThinking {
                on: false,
                ws,
                phase: None,
                at: Some(_),
                ..
            }
        )) if ws.as_str() == WS
    )));
}

#[tokio::test]
async fn an_explicit_target_supplements_the_item() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    let f = k.file("log.txt", "text/plain; charset=utf-8", b"hello");
    k.core
        .say_to(
            WS,
            None,
            "记得加测试",
            std::slice::from_ref(&f),
            &[],
            crate::thread::SayTo {
                item: Some(i.clone()),
                reply_to: None,
            },
        )
        .unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[0].meta["files"], json!([f]));
    assert_eq!(
        k.rt.st().files.last().unwrap().1,
        vec![f.clone()],
        "the files go with the prompt"
    );
    assert_eq!(
        k.db().timeline(&i).unwrap().last().unwrap().files,
        vec![f.clone()]
    );
    assert_eq!(ms[1].text, format!("已作为补充送到 {i}。"));
    assert_eq!(ms[1].item_id.as_deref(), Some(i.as_str()));
    assert_eq!(k.rt.prompts_of(&s), vec!["补充信息：记得加测试"]);
    assert!(
        k.events.lock().unwrap().iter().any(|e| matches!(
            e,
            steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
                steve_types::WorkLiveEvent::SteveThinking {
                    on: true,
                    ws,
                    phase: Some(phase),
                    started_at: Some(_),
                    at: Some(_),
                }
            )) if ws.as_str() == WS && phase == "supplement"
        )),
        "steve.thinking carries the phase and turn started_at"
    );
    /* unknown references go through the router; with the router down an explicit error is recorded, never an item */
    k.core.say(WS, "XX-99 是什么", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(ms[3].meta["error"], json!(true));
    assert_eq!(ms[3].meta["error_code"], json!("route_failed"));
    assert_eq!(ms[3].item_id, None);
}

#[tokio::test]
async fn chat_is_answered_with_history_and_can_be_promoted() {
    let k = Kit::new();
    router(&k, json!({"kind": "answer", "title": "闲聊"}), "今天挺暖和");
    k.core.say(WS, "聊聊今天的天气", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].text, "今天挺暖和");
    assert_eq!(ms[1].meta["chat"], "聊聊今天的天气");
    let st = &ms[1].meta["steps"];
    assert_eq!(st["route"]["kind"], "answer");
    assert_eq!(
        st["route"]["modelKind"], "answer",
        "an old router's chat reads as answer"
    );
    assert_eq!(st["route"]["model"], "fake-model");
    assert_eq!(st["answer"]["failed"], false);
    assert_eq!(st["tools"], json!([]));
    /* the answer went out as it was written, for the message it answers */
    assert!(
        k.events
            .lock()
            .unwrap()
            .contains(&steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
                steve_types::WorkLiveEvent::SteveDraft {
                    ws: {
                        let value: String = WS.into();
                        steve_types::WorkspaceId::from(value)
                    },
                    mid: {
                        let value: String = ms[0].id.clone();
                        steve_types::MessageId::from(value)
                    },
                    thought: String::new(),
                    text: "今天挺暖和  ".into()
                }
            )))
    );
    k.core.say(WS, "再说一句", &[]).unwrap();
    settle(&k, 4).await;
    let calls = k.gw.calls.lock().unwrap().clone();
    let answer_prompt = calls
        .iter()
        .rfind(|p| p.starts_with("You are Steve"))
        .unwrap();
    assert!(answer_prompt.contains(
        "Recent conversation:\nUser: 聊聊今天的天气\nSteve: 今天挺暖和\n\nUser: 再说一句"
    ));
    let route_prompt = calls
        .iter()
        .rfind(|p| p.starts_with("You are the router"))
        .unwrap();
    assert!(route_prompt.contains(
        "Recent conversation, oldest first:\n  User: \"聊聊今天的天气\"\n  Steve: \"今天挺暖和\""
    ));
    /* promote: the router sees the question and Steve's answer */
    router(
        &k,
        json!({"kind": "answer", "title": "写诗", "goal": "写一首关于秋天的诗", "bot": "dev"}),
        "",
    );
    let item = k.core.promote(WS, &ms[1].id, None).await.unwrap();
    assert_eq!(item.title, "写诗");
    assert_eq!(k.db().goal(&item.id).unwrap(), "写一首关于秋天的诗");
    assert_eq!(item.bot_id.as_deref(), Some("dev"));
    let last = k.gw.calls.lock().unwrap().last().unwrap().clone();
    assert!(last.contains("The user pressed “做成事项”"));
    assert!(last.contains("  Steve: \"今天挺暖和\"\nUser message: \"聊聊今天的天气\""));
}

#[tokio::test]
async fn an_item_with_new_tags_and_a_follow_up_goal() {
    let k = Kit::new();
    router(
        &k,
        json!({"kind": "item", "title": "整理照片", "goal": "把相册按年份整理", "bot": "steve", "newTags": [{"name": "生活", "cat": "领域", "description": "日常"}], "reply": "好，我来安排。"}),
        "",
    );
    k.core.say(WS, "按建议", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(
        ms[1].text,
        "好，交给 Steve 开发 Bot 了，做完请你验收。 标签：feat、生活（新建）。"
    );
    let id = ms[1].item_id.clone().unwrap();
    assert_eq!(
        k.db().goal(&id).unwrap(),
        "把相册按年份整理\n\n（用户原话：按建议）"
    );
    assert_eq!(
        k.db().tag_by_name(WS, "生活").unwrap().unwrap().created_by,
        "Steve"
    );
}

#[tokio::test]
async fn at_bot_picks_the_bot_and_skips_chat() {
    let k = Kit::new();
    router(&k, json!({"kind": "answer", "title": "x"}), "不该回答");
    k.core.say(WS, "@Steve开发 修一下登录", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(
        ms[1].text, "按你指定的，交给 Steve 开发 Bot 了。 标签：bug。",
        "“@Steve开发” (as the composer writes it) names “Steve 开发”"
    );
    let item = k
        .db()
        .item(ms[1].item_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(item.bot_id.as_deref(), Some("dev"));
    k.core.say(WS, "@Steve 修", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(ms[3].text, "按你指定的，交给 Steve Bot 了。 标签：feat。");
    k.db()
        .bot_patch("dev", json!({"name": "开发"}).as_object().unwrap(), "me")
        .unwrap();
    k.core.say(WS, "@开发Bot 修一下登录", &[]).unwrap();
    let ms = settle(&k, 6).await;
    assert_eq!(ms[5].text, "按你指定的，交给 开发 Bot 了。 标签：bug。");
    let item = k
        .db()
        .item(ms[5].item_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(item.bot_id.as_deref(), Some("dev"));
    assert_eq!(k.db().goal(&item.id).unwrap(), "修一下登录");
}

#[tokio::test]
async fn config_requests_are_drafted() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            Ok(r#"{"target":"settings:items"}"#.into())
        } else {
            Ok(
                r#"{"changes":[{"key":"archDays","value":"7"},{"key":"handoff","value":"75"}]}"#
                    .into(),
            )
        }
    });
    k.core.say(WS, "完成 7 天后自动归档", &[]).unwrap();
    let ms = settle(&k, 2).await;
    // a local setting said in the conversation: applied at once, the card offers the undo
    assert_eq!(
        ms[1].text,
        "已经改好了「设置 · 事项与验收」，不对的话在卡片上撤销："
    );
    let p = &ms[1].meta["proposal"];
    assert_eq!(
        p["changes"],
        json!([{"label": "完成后自动归档", "from": "30 天", "to": "7 天"}])
    );
    assert_eq!(p["target"], json!({"kind": "settings", "id": "items"}));
    let id = p["id"].as_str().unwrap();
    assert_eq!(ms[1].meta["cfg_state"], json!({id: "applied"}));
    assert_eq!(k.db().setting(WS, "archDays", "30").unwrap(), "7");
    k.core.undo(id, "me", WS).unwrap();
    assert_eq!(k.db().setting(WS, "archDays", "x").unwrap(), "30");
}

#[tokio::test]
async fn the_default_acceptance_and_retention_still_show_a_preview() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            Ok(r#"{"target":"settings:items"}"#.into())
        } else {
            Ok(r#"{"changes":[{"key":"accDefault","value":"manual"},{"key":"archDays","value":"7"}]}"#.into())
        }
    });
    k.core.say(WS, "以后都要我验收，7 天归档", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(
        ms[1].text,
        "这是配置改动，我起草好了，确认后应用到「设置 · 事项与验收」："
    );
    assert!(ms[1].meta.get("cfg_state").is_none());
    assert_eq!(
        k.db().setting(WS, "accDefault", "manual").unwrap(),
        "manual"
    );
}

#[tokio::test]
async fn one_message_drafts_several_settings_and_their_cards_remember_what_happened() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            assert!(p.contains("settings:notify — 免打扰时段"));
            Ok(r#"{"targets":["settings:notify","bot:dev","bot:dev","nope:x"]}"#.into())
        } else if p.contains("Steve settings · notify") {
            Ok(r#"{"changes":[{"key":"quiet","value":"22:00 – 09:00"}]}"#.into())
        } else {
            Ok(r#"{"changes":[{"key":"reasoning_effort","value":"high"}]}"#.into())
        }
    });
    k.core
        .say(
            WS,
            "免打扰改成 22 点到 9 点，开发 Bot 思考强度调到 high",
            &[],
        )
        .unwrap();
    let ms = settle(&k, 2).await;
    // the quiet hours (local) applied at once, the bot is a card to apply
    assert_eq!(
        ms[1].text,
        "已经改好了「设置 · 通知」，不对的话在卡片上撤销：\n「Steve 开发 Bot 的设置」的改动起草好了，确认后应用："
    );
    let ps = ms[1].meta["proposals"].as_array().unwrap().clone();
    assert_eq!(ps.len(), 2, "a target named twice is drafted once");
    assert_eq!(
        ps[0]["changes"],
        json!([{"label": "免打扰", "from": "23:00 – 08:00", "to": "22:00 – 09:00"}])
    );
    assert_eq!(ps[1]["target"], json!({"kind": "bot", "id": "dev"}));
    let (a, b) = (ps[0]["id"].as_str().unwrap(), ps[1]["id"].as_str().unwrap());
    assert_eq!(ms[1].meta["cfg_state"], json!({a: "applied"}));
    assert_eq!(k.db().setting(WS, "quiet", "x").unwrap(), "22:00 – 09:00");
    k.core.drop_draft(b, WS).unwrap();
    let m = k.db().message(&ms[1].id).unwrap().unwrap();
    assert_eq!(m.meta["cfg_state"], json!({a: "applied", b: "dropped"}));
    assert!(
        k.core.apply(b, "me", WS).is_err(),
        "a dropped draft is gone"
    );
    k.core.undo(a, "me", WS).unwrap();
    let m = k.db().message(&ms[1].id).unwrap().unwrap();
    assert_eq!(m.meta["cfg_state"], json!({b: "dropped"}));
    assert_eq!(k.db().setting(WS, "quiet", "x").unwrap(), "23:00 – 08:00");
}

#[tokio::test]
async fn following_steves_proposal_drafts_new_and_existing_bots_from_it() {
    let k = Kit::new();
    k.db()
        .message_add(
            WS,
            "steve",
            "建议补齐分工：新增「设计」Bot 负责交互方案；开发 Bot 交付要附复现步骤。",
            None,
            None,
        )
        .unwrap();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            assert!(p.contains("bot:new:<名字> — 新建一个 Bot"));
            assert!(p.contains("at most 6"));
            assert!(
                p.contains("新增「设计」Bot 负责交互方案"),
                "the picker reads the proposal"
            );
            Ok(r#"{"targets":["bot:new:设计","bot:dev"]}"#.into())
        } else {
            assert!(p.contains("按这个方案补齐 Bot"));
            assert!(
                p.contains("开发 Bot 交付要附复现步骤"),
                "the drafts read the proposal"
            );
            if p.contains("a new bot \"设计\"") {
                Ok(r#"{"changes":[{"key":"role","value":"交互方案"}]}"#.into())
            } else {
                Ok(r#"{"changes":[{"key":"rules","value":"交付附复现步骤"}]}"#.into())
            }
        }
    });
    k.core.say(WS, "按这个方案补齐 Bot", &[]).unwrap();
    let ms = settle(&k, 3).await;
    let ps = ms[2].meta["proposals"].as_array().unwrap().clone();
    assert_eq!(ps.len(), 2);
    assert_eq!(ps[0]["target"], json!({"kind": "bot", "id": "new:设计"}));
    assert_eq!(ps[0]["title"], "新建 Bot · 设计");
    assert_eq!(ps[1]["target"], json!({"kind": "bot", "id": "dev"}));
    let r = k
        .core
        .apply(ps[0]["id"].as_str().unwrap(), "me", WS)
        .unwrap();
    let made = k.db().bot(r["bot"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(
        (made.name.as_str(), made.role.as_str()),
        ("设计", "交互方案")
    );
}

#[tokio::test]
async fn asking_how_a_setting_is_set_shows_it_without_drafting() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            Ok(r#"{"targets":["settings:notify"],"ask":true}"#.into())
        } else {
            Err(msg("no draft when only asking"))
        }
    });
    k.core.say(WS, "现在免打扰是几点？", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(
        ms[1].text,
        "「设置 · 通知」现在是：\n- 免打扰：23:00 – 08:00\n\n要改的话直接说改成什么，我起草好给你确认。"
    );
    assert!(ms[1].meta.get("proposal").is_none());
}

#[tokio::test]
async fn a_request_that_changes_nothing_says_how_it_is_set() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            Ok(r#"{"target":"settings:look"}"#.into())
        } else {
            Ok(r#"{"changes":[]}"#.into())
        }
    });
    k.core.say(WS, "字体弄好看点", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(
        ms[1].text,
        "我没看出要改「设置 · 外观」的哪一项，换个说法试试？现在是：\n- 字号：标准\n- 减弱动态效果：关"
    );
    assert!(ms[1].meta.get("proposal").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_suppresses_the_reply() {
    let k = Kit::new();
    k.gw.set(|_, _| {
        std::thread::sleep(Duration::from_millis(200));
        Ok(r#"{"kind":"answer"}"#.into())
    });
    k.core.say(WS, "慢慢想", &[]).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(k.core.stop(WS).unwrap());
    tokio::time::sleep(Duration::from_millis(400)).await;
    let ms = k.db().messages(WS).unwrap();
    assert_eq!(
        ms.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        vec!["慢慢想", "好，停下了。要我换个方式就直接说。"]
    );
    assert!(!k.core.stop(WS).unwrap());
}

/// Stopping a direct answer ends it at once (the harness call is dropped, which kills it), not when
/// the whole answer has been written; Steve says so, and a message waiting for the turn goes next.
#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_an_answer_at_once_and_the_queue_goes_next() {
    let k = Kit::new();
    router(&k, json!({"kind": "answer", "confidence": 0.9}), "答好了");
    *k.gw.slow.lock().unwrap() = Some((
        "You are Steve, the user's assistant.",
        Duration::from_secs(30),
    ));
    let asked = k.core.say(WS, "慢慢答", &[]).unwrap();
    /* the answer is being written, not merely queued; only this one is slow */
    steve_runtime::testkit::eventually_sync(
        "The answer prompt reaches the fake model gateway.",
        || {
            let calls = k.gw.calls.lock().unwrap();
            calls.iter().any(|p| p.starts_with("You are Steve"))
        },
    )
    .await;
    *k.gw.slow.lock().unwrap() = None;
    let next = k.core.say(WS, "接着说", &[]).unwrap();
    let view = k
        .db()
        .store()
        .read(|r| {
            use steve_store::Views;
            r.message_views().of(WS, std::slice::from_ref(&next))
        })
        .unwrap();
    assert_eq!(serde_json::to_value(&view[0].meta).unwrap()["held"], true);
    let t0 = std::time::Instant::now();
    assert!(k.core.stop(WS).unwrap());
    let ms = settle(&k, 4).await;
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    let stopped_reply = ms
        .iter()
        .find(|m| m.author == "steve" && m.meta["origin"] == asked)
        .expect("stopped reply for first message");
    assert_eq!(stopped_reply.text, "好，停下了。要我换个方式就直接说。");
    assert_eq!(stopped_reply.meta["stopped"], true);
    let next_reply = ms
        .iter()
        .find(|m| m.author == "steve" && m.meta["origin"] == next)
        .expect("answer for queued message");
    assert_eq!(next_reply.text, "答好了");
    // the answer is written before its turn lets go of the thread
    steve_runtime::testkit::eventually_sync("The main conversation is no longer busy.", || {
        !k.core.main_busy(WS).unwrap()
    })
    .await;
}

/// Stop reaches a queued task its worker has not claimed yet: here new work is held for maintenance.
#[tokio::test(flavor = "multi_thread")]
async fn stop_takes_an_answer_not_started_yet_off_the_queue() {
    let k = Kit::new();
    let store = k.db().store().clone();
    router(&k, json!({"kind": "answer", "confidence": 0.9}), "答好了");
    store.maintenance_set(true).unwrap();
    let asked = k.core.say(WS, "慢慢答", &[]).unwrap();
    let next = k.core.say(WS, "这个要答", &[]).unwrap();
    let view = store
        .read(|r| {
            use steve_store::Views;
            r.message_views().of(WS, std::slice::from_ref(&next))
        })
        .unwrap();
    assert_eq!(serde_json::to_value(&view[0].meta).unwrap()["held"], true);
    let task = store
        .read(|r| steve_store::thread::tasks_of_origin(r, &asked))
        .unwrap()
        .remove(0);
    assert!(k.core.stop_run(WS, &task.id).unwrap());
    let ms = k.db().messages(WS).unwrap();
    assert_eq!(ms.last().unwrap().meta["stopped"], true);
    let stopped_receipt = store
        .read(|r| steve_store::thread::receipt_by_origin(r, &asked))
        .unwrap()
        .expect("stopped receipt");
    assert_eq!(stopped_receipt.outcome, "stopped");
    assert_eq!(
        steve_store::core_fixture::thread::thread_task_count(&store, Some((&asked, "queued"))),
        0
    );
    store.maintenance_set(false).unwrap();
    k.core.thread_ready().unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(ms.iter().filter(|m| m.meta["stopped"] == true).count(), 1);
    assert!(
        ms.iter()
            .any(|m| m.author == "steve" && m.text == "答好了" && m.meta["origin"] == json!(next))
    );
    assert_eq!(
        store
            .read(|r| steve_store::thread::receipt_by_origin(r, &asked))
            .unwrap()
            .unwrap()
            .outcome,
        "stopped"
    );
}

#[tokio::test]
async fn messages_are_handled_in_order() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        assert!(p.starts_with("You are the router"));
        Ok(r#"{"kind":"item","confidence":0.9,"signals":["change"]}"#.into())
    });
    k.core.say(WS, "/汇报", &[]).unwrap();
    k.core.say(WS, "第二件事", &[]).unwrap();
    /* `/汇报` has a lane of its own (#51): it does not wait for a message being routed */
    settle(&k, 4).await;
    k.core.say(WS, "/汇报", &[]).unwrap();
    let ms = settle(&k, 6).await;
    let steve: Vec<&str> = ms
        .iter()
        .filter(|m| m.author == "steve")
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(steve[0], "现在：0 件等你，0 件在进行，今天完成 0 件。");
    assert_eq!(
        steve[1],
        "好，交给 Steve 开发 Bot 了，做完请你验收。 标签：feat。"
    );
    assert_eq!(steve[2], "现在：0 件等你，1 件在进行，今天完成 0 件。");
}

#[tokio::test]
async fn chat_attachments_are_read_by_steve_and_carried_into_promote() {
    let k = Kit::new();
    router(&k, json!({"kind": "answer", "title": "看图"}), "是一张图");
    let f = k.file("a.png", "image/png", b"\x89PNG fake");
    k.core
        .say(WS, "这是什么", std::slice::from_ref(&f))
        .unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].meta["chat"], "这是什么");
    assert_eq!(ms[1].meta["chatFiles"], json!([f]));
    let calls = k.gw.calls.lock().unwrap().clone();
    assert!(
        calls[0].contains("User message: \"这是什么\\n（附带 1 个附件：a.png · image/png）\""),
        "{}",
        calls[0]
    );
    let ans = &calls[1];
    /* the image goes into the message itself: no Read tool, the cache-friendly layout stays */
    assert!(
        ans.contains("The attached images are in this message: look at them before answering.")
    );
    let rel = format!(".steve/attachments/{}-a.png", &f.id[4..]);
    assert!(ans.contains(&format!("User: 这是什么\n\n用户附带了 1 个附件，已保存在工作目录里（绝对路径前缀 {}/），需要时直接读取：\n- {rel}（image/png，1 KB，图片也直接附在这条消息里）", k.core.config().scratch.display())), "{ans}");
    assert_eq!(
        *k.gw.tools.lock().unwrap().last().unwrap(),
        crate::gateway::ThinkTools::None
    );
    let imgs = k.gw.images.lock().unwrap().last().unwrap().clone();
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0]["type"], "image");
    assert_eq!(imgs[0]["source"]["media_type"], "image/png");
    assert!(
        k.gw.images.lock().unwrap()[0].is_empty(),
        "the router gets no image"
    );
    let scratch = &k.core.config().scratch;
    assert!(scratch.join(&rel).exists());
    assert_eq!(
        std::fs::read_to_string(scratch.join(".steve/attachments/.gitignore")).unwrap(),
        "*\n"
    );
    let item = k.core.promote(WS, &ms[1].id, None).await.unwrap();
    let s = k.db().sessions_of_item(&item.id).unwrap().pop().unwrap();
    let sent =
        k.rt.st()
            .files
            .iter()
            .rfind(|(sid, _)| *sid == s.id)
            .unwrap()
            .1
            .clone();
    assert_eq!(sent, vec![f]);
}

/// A chat answer about a text file and an image: Read for the text file only (by id and the path
/// the prompt names), the image inline.
#[tokio::test]
async fn a_chat_answer_reads_the_files_that_are_not_inline_images() {
    use crate::gateway::{Readable, ThinkTools};
    let k = Kit::new();
    router(&k, json!({"kind": "answer", "title": "看日志"}), "看过了");
    let img = k.file("a.png", "image/png", b"\x89PNG fake");
    let txt = k.file("log.txt", "text/plain", b"line");
    k.core
        .say(WS, "这是什么", &[img.clone(), txt.clone()])
        .unwrap();
    let ms = settle(&k, 2).await;
    let rel = format!(".steve/attachments/{}-log.txt", &txt.id[4..]);
    let ans = &k.gw.calls.lock().unwrap()[1];
    assert!(
        ans.contains("read the other attached files with the Read tool"),
        "{ans}"
    );
    assert!(ans.contains(&format!("- {rel}（text/plain")), "{ans}");
    assert_eq!(
        *k.gw.tools.lock().unwrap().last().unwrap(),
        ThinkTools::Read(vec![Readable {
            file_id: txt.id.clone(),
            rel_path: rel.clone(),
        }])
    );
    assert!(k.core.config().scratch.join(&rel).exists());
    assert_eq!(k.gw.images.lock().unwrap().last().unwrap().len(), 1);
    assert_eq!(ms[1].author, "steve");
}

#[tokio::test]
async fn replies_carry_what_steve_did() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            assert!(p.contains("\"why\":\"one short sentence in the user language: why this kind\"}"));
            Ok(r#"{"kind":"item","confidence":0.9,"signals":["change"],"title":"修登录","bot":"dev","why":"要改代码"}"#.into())
        } else {
            Err(msg("x"))
        }
    });
    k.core.say(WS, "修一下登录", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let st = &ms[1].meta["steps"];
    assert_eq!(st["route"]["why"], "要改代码");
    let t = &st["tools"][0];
    assert_eq!(
        (t["name"].as_str(), t["status"].as_str()),
        (Some("create_item"), Some("ok"))
    );
    assert!(t["args"]["bot"].is_null());
    assert_eq!(t["result"]["bot"], "dev");
    assert_eq!(t["result"]["item"], json!(ms[1].item_id));
    /* a router failure records an explicit failure reply instead of silent fallback */
    k.gw.set(|_, _| Err(msg("down")));
    k.core.say(WS, "随便", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(ms[3].meta["error"], json!(true));
    assert_eq!(ms[3].meta["error_code"], json!("route_failed"));
    assert_eq!(ms[3].item_id, None, "a failed router never makes an item");
}

#[tokio::test]
async fn a_json_answer_splits_thought_and_text() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"answer"}"#.into())
        } else {
            assert!(p.contains("Reply with ONLY one JSON object: {\"thought\""));
            assert!(p.ends_with("User: 聊聊瑞利散射"));
            Ok(r#"{"thought":"问的是散射","answer":"因为瑞利散射。"}"#.into())
        }
    });
    k.core.say(WS, "聊聊瑞利散射", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].text, "因为瑞利散射。");
    assert_eq!(ms[1].meta["steps"]["answer"]["thought"], "问的是散射");
}

/// ST-46: a button of an interface in Steve's reply is said as the user's A2UI action, once per surface.
#[tokio::test]
async fn an_a2ui_action_is_said_once_per_surface() {
    use crate::thread::A2uiAction;
    let k = Kit::new();
    router(
        &k,
        json!({"kind": "answer", "title": "选方案"}),
        "选一个：\n```a2ui\n{\"version\":\"v0.9.1\",\"createSurface\":{\"surfaceId\":\"pick\",\"catalogId\":\"steve:catalog/v1\"}}\n```",
    );
    k.core.say(WS, "给我两个方案选", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let reply = ms[1].id.clone();
    let calls = k.gw.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|p| p.starts_with("You are Steve") && p.contains("steve:catalog/v1")),
        "the main conversation's answer knows how to draw an interface"
    );
    let act = |surface: &str| A2uiAction {
        block: "k1".into(),
        surface: surface.into(),
        component: "ok".into(),
        name: "submit".into(),
        label: "就用 A".into(),
        context: json!({"plan": "A"}),
        data: json!({"form": {"plan": ["A"]}}),
        timestamp: "2026-11-05T01:00:00Z".into(),
    };
    let first = k.core.a2ui_action(WS, None, &reply, act("pick")).unwrap();
    assert!(!first.already);
    let said = first.message_id.clone().unwrap();
    let again = k.core.a2ui_action(WS, None, &reply, act("pick")).unwrap();
    assert!(again.already, "a second click gets the record back");
    assert_eq!(again.message_id.as_deref(), Some(said.as_str()));
    let m = k.db().message(&said).unwrap().unwrap();
    assert_eq!(m.author, "me");
    assert!(m.text.starts_with("[A2UI action] 提交「就用 A」\n"));
    let sent: serde_json::Value = serde_json::from_str(m.text.lines().nth(1).unwrap()).unwrap();
    assert_eq!(sent["action"]["surfaceId"], "pick");
    assert_eq!(sent["action"]["context"], json!({"plan": "A"}));
    assert_eq!(m.meta["a2ui_action"]["ref"], json!(reply));
    assert_eq!(m.meta["a2ui_action"]["label"], "就用 A");
    let r = k.db().message(&reply).unwrap().unwrap();
    assert_eq!(
        r.meta["a2ui"]["k1:pick"]["data"],
        json!({"form": {"plan": ["A"]}})
    );
    assert_eq!(r.meta["a2ui"]["k1:pick"]["message"], json!(said));
    /* a surface the reply doesn't draw, or a message that isn't Steve's reply, is refused */
    assert!(k.core.a2ui_action(WS, None, &reply, act("other")).is_err());
    assert!(k.core.a2ui_action(WS, None, &said, act("pick")).is_err());
    assert!(
        k.core
            .a2ui_action("elsewhere", None, &reply, act("pick"))
            .is_err()
    );
    /* the action is a turn like any message: Steve answers it */
    settle(&k, 4).await;
}

/// A message whose routing panics loses that message only: the conversation's lane stays, and the
/// next message is answered (it used to go unanswered until a restart).
#[tokio::test(flavor = "multi_thread")]
async fn a_routing_that_panics_does_not_take_the_conversation_down() {
    let k = Kit::new();
    k.gw.set(|_, _| panic!("router blew up"));
    k.core.say(WS, "第一件", &[]).unwrap();
    steve_runtime::testkit::eventually_sync("The main conversation is no longer busy.", || {
        !k.core.main_busy(WS).unwrap()
    })
    .await;
    k.gw.script.clear_poison();
    k.core.say(WS, "/汇报", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(
        ms.last().unwrap().text,
        "现在：0 件等你，0 件在进行，今天完成 0 件。"
    );
}

/// An answer that panics ends its turn: the message waiting behind it is routed and answered.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_that_panics_ends_its_turn() {
    let k = Kit::new();
    let r = json!({"kind": "answer", "confidence": 0.9}).to_string();
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router of Steve") {
            Ok(r.clone())
        } else {
            panic!("answer blew up")
        }
    });
    k.core.say(WS, "答一下", &[]).unwrap();
    steve_runtime::testkit::eventually_sync("The main conversation is no longer busy.", || {
        !k.core.main_busy(WS).unwrap()
    })
    .await;
    k.gw.script.clear_poison();
    k.core.say(WS, "/汇报", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(
        ms.last().unwrap().text,
        "现在：0 件等你，0 件在进行，今天完成 0 件。"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_contact_field_said_in_the_conversation_is_applied_at_once() {
    let k = Kit::new();
    steve_store::core_fixture::thread::sarah_contact(k.db().store());
    let mut r = steve_modules::Registry::new();
    steve_modules::contacts::changes::register(&mut r, &Default::default());
    let m = steve_modules::Modules::new(k.db().store().clone(), k.core.module_gateway(), r);
    k.core.attach_modules(m.clone());
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            Ok(r#"{"targets":["contact:ct_sarah"]}"#.into())
        } else if p.starts_with("You configure") {
            Ok(r#"{"ops":[{"op":"set","key":"important","value":"是"}]}"#.into())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
    k.core
        .say_as(WS, Some("usr_dev"), "Sarah 很重要", &[], &[])
        .unwrap();
    let ms = settle(&k, 2).await;
    assert!(ms[1].text.starts_with("已经改好了「"), "{:?}", ms[1]);
    let id = ms[1].meta["proposal"]["id"].as_str().unwrap().to_string();
    assert_eq!(ms[1].meta["cfg_state"][&id], "applied");
    let important =
        steve_store::core_fixture::thread::important_contact(k.db().store(), "ct_sarah");
    assert!(important);
}

/// #52: a plain question is researched without asking the router (`route_guard = "q"`); one that
/// names an item or asks for work still goes to the router.
#[tokio::test(flavor = "multi_thread")]
async fn a_plain_question_skips_the_router() {
    let k = Kit::new();
    let routed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r2 = routed.clone();
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router of Steve") {
            r2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(r#"{"kind":"answer","confidence":0.9}"#.into())
        } else {
            Ok("答好了".into())
        }
    });
    let q = k.core.say(WS, "检索是怎么打分的？", &[]).unwrap();
    settle(&k, 2).await;
    assert_eq!(
        routed.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no router"
    );
    let log = k
        .db()
        .store()
        .read(|r| steve_store::thread::receipt_by_origin(r, &q))
        .unwrap()
        .expect("reply receipt");
    assert_eq!(log.route_guard.as_deref(), Some("q"));
    assert_eq!(log.route_model, None);
    steve_runtime::testkit::eventually_sync("The main conversation is no longer busy.", || {
        !k.core.main_busy(WS).unwrap()
    })
    .await;
    // asking for work: the router decides
    k.core.say(WS, "把检索的打分改成按时间加权", &[]).unwrap();
    settle(&k, 4).await;
    assert_eq!(routed.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// In your own conversation a question whose answer reached for a tool it may not use is not
/// made an item on its own (only a trigger word on your account is): the reply says why and points
/// at “做成事项”, and every model of the list is still tried first.
#[tokio::test]
async fn a_question_that_needs_tools_says_to_make_it_an_item() {
    let k = Kit::new();
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router of Steve") {
            Ok(json!({"kind": "answer", "confidence": 0.9, "signals": ["question"], "title": "看磁盘", "goal": "看看这台机器的磁盘用量", "bot": "steve"}).to_string())
        } else if p.starts_with("You are Steve, the user's assistant.") {
            Err(steve_modules::gateway::GatewayError::NeedsTools.into())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
    let before = k.db().items(WS).unwrap().len();
    k.core.say(WS, "帮我看下我现在磁盘情况", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].item_id, None, "{}", ms[1].text);
    assert_eq!(k.db().items(WS).unwrap().len(), before);
    assert_eq!(ms[1].text, crate::thread::handoff::NEEDS_TOOLS_ANSWER);
    assert_eq!(ms[1].meta["error"], true);
}

#[tokio::test]
async fn receipts_record_dispatch_refs_for_create_append_and_plain_replies() {
    let k = Kit::new();

    // 1. Item creation receipt (`create_item`): dispatch_ref_id points to the new create_item ref.
    let create_origin = k.core.say(WS, "@Steve 修一下登录", &[]).unwrap();
    let ms = settle(&k, 2).await;
    let created_item_id = ms[1].item_id.clone().expect("created item id");
    let create_rcpt = k
        .db()
        .store()
        .read(|r| steve_store::thread::receipt_by_origin(r, &create_origin))
        .unwrap()
        .expect("create receipt");
    assert_eq!(create_rcpt.outcome, "handed_off");
    assert_eq!(
        create_rcpt.item_id.as_deref(),
        Some(created_item_id.as_str())
    );
    let create_disp_id = create_rcpt
        .dispatch_ref_id
        .clone()
        .expect("create receipt must carry dispatch_ref_id");
    let create_disp = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_id(r, &create_disp_id))
        .unwrap()
        .expect("create dispatch ref");
    assert_eq!(create_disp.effect_kind, "create_item");
    assert_eq!(create_disp.state, "applied");
    assert_eq!(create_disp.origin_message_id, create_origin);
    assert_eq!(
        create_disp.target_item_id.as_deref(),
        Some(created_item_id.as_str())
    );
    assert_eq!(
        create_disp.reply_message_id.as_deref(),
        create_rcpt.reply_message_id.as_deref()
    );

    // 2a. Explicit SayTo append receipt (`append_item`): links its own append_item ref without
    //     overwriting the original create_item ref's reply_message_id.
    let append_origin = k
        .core
        .say_to(
            WS,
            None,
            "记得补上单测",
            &[],
            &[],
            crate::thread::SayTo {
                item: Some(created_item_id.clone()),
                reply_to: None,
            },
        )
        .unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(ms[3].text, format!("已作为补充送到 {created_item_id}。"));
    let append_rcpt = k
        .db()
        .store()
        .read(|r| steve_store::thread::receipt_by_origin(r, &append_origin))
        .unwrap()
        .expect("append receipt");
    assert_eq!(append_rcpt.outcome, "handed_off");
    assert_eq!(
        append_rcpt.item_id.as_deref(),
        Some(created_item_id.as_str())
    );
    let append_disp_id = append_rcpt
        .dispatch_ref_id
        .clone()
        .expect("append receipt must carry dispatch_ref_id");
    assert_ne!(append_disp_id, create_disp_id);
    let append_disp = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_id(r, &append_disp_id))
        .unwrap()
        .expect("append dispatch ref");
    assert_eq!(append_disp.effect_kind, "append_item");
    assert_eq!(append_disp.state, "applied");
    assert_eq!(append_disp.origin_message_id, append_origin);
    assert_eq!(
        append_disp.target_item_id.as_deref(),
        Some(created_item_id.as_str())
    );
    assert_eq!(
        append_disp.reply_message_id.as_deref(),
        append_rcpt.reply_message_id.as_deref()
    );
    let create_disp_after = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_id(r, &create_disp_id))
        .unwrap()
        .expect("create dispatch ref preserved");
    assert_eq!(
        create_disp_after.reply_message_id, create_disp.reply_message_id,
        "append reply must not overwrite create_item dispatch reply_message_id"
    );

    // 2b. Routed supplement receipt also carries a real append_item dispatch_ref_id.
    router(
        &k,
        json!({
            "kind": "supplement",
            "item": created_item_id,
            "confidence": 0.95,
            "signals": ["followup"],
            "title": "补充"
        }),
        "",
    );
    let routed_supp_origin = k.core.say(WS, "另外把超时日志也打印出来", &[]).unwrap();
    settle(&k, 6).await;
    let routed_supp_rcpt = k
        .db()
        .store()
        .read(|r| steve_store::thread::receipt_by_origin(r, &routed_supp_origin))
        .unwrap()
        .expect("routed supplement receipt");
    assert_eq!(routed_supp_rcpt.outcome, "handed_off");
    let routed_supp_disp_id = routed_supp_rcpt
        .dispatch_ref_id
        .clone()
        .expect("routed supplement receipt must carry dispatch_ref_id");
    let routed_supp_disp = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_id(r, &routed_supp_disp_id))
        .unwrap()
        .expect("routed supplement dispatch ref");
    assert_eq!(routed_supp_disp.effect_kind, "append_item");
    assert_eq!(routed_supp_disp.origin_message_id, routed_supp_origin);
    assert_eq!(
        routed_supp_disp.target_item_id.as_deref(),
        Some(created_item_id.as_str())
    );

    // 3. Plain reply receipt (`NoDispatch`): dispatch_ref_id is NULL and no dispatch ref exists.
    router(&k, json!({"kind": "answer", "title": "闲聊"}), "今天挺暖和");
    let plain_origin = k.core.say(WS, "聊聊今天的天气", &[]).unwrap();
    let ms = settle(&k, 8).await;
    let plain_reply_id = ms[7].id.clone();
    let plain_rcpt = k
        .db()
        .store()
        .read(|r| steve_store::thread::receipt_by_origin(r, &plain_origin))
        .unwrap()
        .expect("plain receipt");
    assert_eq!(plain_rcpt.outcome, "answered");
    assert_eq!(plain_rcpt.item_id, None);
    assert_eq!(plain_rcpt.dispatch_ref_id, None);
    assert!(
        k.db()
            .store()
            .read(|r| steve_store::thread::dispatch_by_origin(r, WS, &plain_origin, "create_item"))
            .unwrap()
            .is_none()
    );
    assert!(
        k.db()
            .store()
            .read(|r| steve_store::thread::dispatch_by_origin(r, WS, &plain_origin, "append_item"))
            .unwrap()
            .is_none()
    );

    // 4. Promoting that plain reply later updates its receipt with the real create_item dispatch_ref_id.
    router(
        &k,
        json!({"kind": "answer", "title": "天气记录", "goal": "记录天气", "bot": "steve"}),
        "",
    );
    let promoted = k.core.promote(WS, &plain_reply_id, None).await.unwrap();
    let promoted_rcpt = k
        .db()
        .store()
        .read(|r| steve_store::thread::receipt_by_origin(r, &plain_origin))
        .unwrap()
        .expect("promoted receipt");
    assert_eq!(promoted_rcpt.route_override.as_deref(), Some("to_item"));
    assert_eq!(promoted_rcpt.item_id.as_deref(), Some(promoted.id.as_str()));
    let promoted_disp_id = promoted_rcpt
        .dispatch_ref_id
        .expect("promoted receipt must backfill dispatch_ref_id");
    let promoted_disp = k
        .db()
        .store()
        .read(|r| steve_store::thread::dispatch_by_id(r, &promoted_disp_id))
        .unwrap()
        .expect("promoted dispatch ref");
    assert_eq!(promoted_disp.effect_kind, "create_item");
    assert_eq!(promoted_disp.origin_message_id, plain_origin);
    assert_eq!(
        promoted_disp.target_item_id.as_deref(),
        Some(promoted.id.as_str())
    );
}

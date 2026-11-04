//! The main thread: lanes, /汇报, #REF supplements, routing to chat, config or an item, @bot, stop.

use crate::Tier;
use crate::error::msg;
use crate::repo::{HubEvent, WS};
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
    assert!(ev.contains(&HubEvent::SteveThinking {
        on: false,
        ws: Some(WS.into()),
        phase: None
    }));
}

#[tokio::test]
async fn a_reference_supplements_the_item() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    let f = k.file("log.txt", "text/plain; charset=utf-8", b"hello");
    k.core
        .say(WS, &format!("#{i} 记得加测试"), std::slice::from_ref(&f))
        .unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[0].meta, json!({"files": [f]}));
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
        k.events.lock().unwrap().contains(&HubEvent::SteveThinking {
            on: true,
            ws: Some(WS.into()),
            phase: Some("supplement".into())
        }),
        "steve.thinking carries the phase"
    );
    /* unknown references go through the router; with the router down that is an answer, never an item */
    k.core.say(WS, "XX-99 是什么", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert!(ms[3].text.starts_with("（回答失败"), "{}", ms[3].text);
    assert_eq!(ms[3].item_id, None);
    assert_eq!(ms[3].meta["steps"]["route"]["fallback"], true);
}

#[tokio::test]
async fn chat_is_answered_with_history_and_can_be_promoted() {
    let k = Kit::new();
    router(&k, json!({"kind": "chat", "title": "问候"}), "你好呀");
    k.core.say(WS, "你好", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].text, "你好呀");
    assert_eq!(ms[1].meta["chat"], "你好");
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
    assert!(k.events.lock().unwrap().contains(&HubEvent::SteveDraft {
        ws: WS.into(),
        mid: ms[0].id.clone(),
        thought: String::new(),
        text: "你好呀  ".into(),
    }));
    k.core.say(WS, "再说一句", &[]).unwrap();
    settle(&k, 4).await;
    let calls = k.gw.calls.lock().unwrap().clone();
    let answer_prompt = calls
        .iter()
        .rfind(|p| p.starts_with("You are Steve"))
        .unwrap();
    assert!(
        answer_prompt.contains("Recent conversation:\nUser: 你好\nSteve: 你好呀\n\nUser: 再说一句")
    );
    let route_prompt = calls
        .iter()
        .rfind(|p| p.starts_with("You are the router"))
        .unwrap();
    assert!(
        route_prompt
            .contains("Recent conversation, oldest first:\n  User: \"你好\"\n  Steve: \"你好呀\"")
    );
    /* promote: the router sees the question and Steve's answer */
    router(
        &k,
        json!({"kind": "chat", "title": "写诗", "goal": "写一首关于秋天的诗", "bot": "dev"}),
        "",
    );
    let item = k.core.promote(WS, &ms[1].id).await.unwrap();
    assert_eq!(item.title, "写诗");
    assert_eq!(k.db().goal(&item.id).unwrap(), "写一首关于秋天的诗");
    assert_eq!(item.bot_id.as_deref(), Some("dev"));
    let last = k.gw.calls.lock().unwrap().last().unwrap().clone();
    assert!(last.contains("The user pressed “做成事项”"));
    assert!(last.contains("  Steve: \"你好呀\"\nUser message: \"你好\""));
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
    assert_eq!(ms[1].text, "好，我来安排。 标签：生活（新建）。");
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
    router(&k, json!({"kind": "chat", "title": "x"}), "不该回答");
    k.core.say(WS, "@Steve开发 修一下登录", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(
        ms[1].text, "按你指定的，交给Steve 开发 Bot 了。",
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
    assert_eq!(ms[3].text, "按你指定的，交给Steve Bot 了。");
    k.db()
        .bot_patch("dev", json!({"name": "开发"}).as_object().unwrap(), "me")
        .unwrap();
    k.core.say(WS, "@开发Bot 修一下登录", &[]).unwrap();
    let ms = settle(&k, 6).await;
    assert_eq!(ms[5].text, "按你指定的，交给开发 Bot 了。");
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
    assert_eq!(
        ms[1].text,
        "这是配置改动，我起草好了，确认后应用到「设置 · 事项与验收」："
    );
    let p = &ms[1].meta["proposal"];
    assert_eq!(
        p["changes"],
        json!([{"label": "完成后自动归档", "from": "30 天", "to": "7 天"}])
    );
    assert_eq!(p["target"], json!({"kind": "settings", "id": "items"}));
    k.core.apply(p["id"].as_str().unwrap(), "me", WS).unwrap();
    assert_eq!(k.db().setting(WS, "archDays", "30").unwrap(), "7");
}

#[tokio::test]
async fn one_message_drafts_several_settings_and_their_cards_remember_what_happened() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            assert!(p.contains("settings:notify — 免打扰时段、每天汇报时间"));
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
    assert_eq!(
        ms[1].text,
        "这是配置改动，我起草好了 2 处，确认后分别应用："
    );
    let ps = ms[1].meta["proposals"].as_array().unwrap().clone();
    assert_eq!(ps.len(), 2, "a target named twice is drafted once");
    assert_eq!(
        ps[0]["changes"],
        json!([{"label": "免打扰", "from": "23:00 – 08:00", "to": "22:00 – 09:00"}])
    );
    assert_eq!(ps[1]["target"], json!({"kind": "bot", "id": "dev"}));
    let (a, b) = (ps[0]["id"].as_str().unwrap(), ps[1]["id"].as_str().unwrap());
    k.core.apply(a, "me", WS).unwrap();
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
        "「设置 · 通知」现在是：\n- 免打扰：23:00 – 08:00\n- 每天汇报：08:30\n\n要改的话直接说改成什么，我起草好给你确认。"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_settings_are_drafted_from_the_conversation() {
    let k = Kit::new();
    k.db()
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO users (id, workspace_id, subject, name, role, created) VALUES ('usr_dev', 'default', 'dev', 'Dev', 'owner', 1);
                 INSERT OR IGNORE INTO workspace_members (workspace_id, user_id, role, created) VALUES ('default', 'usr_dev', 'owner', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let mut r = steve_modules::Registry::new();
    crate::yolo::draft::register(&mut r, None);
    let m = steve_modules::Modules::new(k.db().store().clone(), k.core.module_gateway(), r);
    k.core.attach_modules(m.clone());
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            assert!(p.contains("yolo:default — YOLO 模式"));
            Ok(r#"{"targets":["yolo:default"]}"#.into())
        } else if p.starts_with("You configure YOLO mode") {
            Ok(r#"{"ops":[{"op":"yolo.set","on":true}]}"#.into())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
    k.core
        .say_as(WS, Some("usr_dev"), "开 YOLO", &[], &[])
        .unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(
        ms[1].text, "这是配置改动，我起草好了，确认后应用到「YOLO 模式」：",
        "{:?}",
        ms[1]
    );
    let p = &ms[1].meta["proposal"];
    assert_eq!(p["target"], json!({"kind": "yolo", "id": "default"}));
    assert_eq!(p["changes"][0]["to"], "开");
    let caller = steve_modules::Caller {
        ws: WS.into(),
        me: "usr_dev".into(),
    };
    let id = p["id"].as_str().unwrap();
    steve_modules::changes::apply(&m, &caller, id)
        .unwrap()
        .unwrap();
    k.core.mark_proposal(WS, id, Some("applied"));
    let msg = k.db().message(&ms[1].id).unwrap().unwrap();
    assert_eq!(msg.meta["cfg_state"][id], "applied");
    /* without a known user the modules are not offered */
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"config","title":"x"}"#.into())
        } else if p.starts_with("Which Steve configuration") {
            assert!(!p.contains("yolo:"));
            Ok(r#"{"targets":["yolo:default"]}"#.into())
        } else {
            Ok(r#"{"changes":[]}"#.into())
        }
    });
    k.core.say(WS, "关 YOLO", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert!(
        ms[3].text.starts_with("我没看出要改「设置 · 通用」"),
        "{}",
        ms[3].text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_suppresses_the_reply() {
    let k = Kit::new();
    k.gw.set(|_, _| {
        std::thread::sleep(Duration::from_millis(200));
        Ok(r#"{"kind":"chat"}"#.into())
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

#[tokio::test]
async fn messages_are_handled_in_order() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        assert!(p.starts_with("You are the router"));
        Ok(r#"{"kind":"item","confidence":0.9,"signals":["change"]}"#.into())
    });
    k.core.say(WS, "/汇报", &[]).unwrap();
    k.core.say(WS, "第二件事", &[]).unwrap();
    k.core.say(WS, "/汇报", &[]).unwrap();
    let ms = settle(&k, 6).await;
    let steve: Vec<&str> = ms
        .iter()
        .filter(|m| m.author == "steve")
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(steve[0], "现在：0 件等你，0 件在进行，今天完成 0 件。");
    assert_eq!(steve[1], "好，交给它了，做完请你验收。");
    assert_eq!(steve[2], "现在：0 件等你，1 件在进行，今天完成 0 件。");
}

#[tokio::test]
async fn chat_attachments_are_read_by_steve_and_carried_into_promote() {
    let k = Kit::new();
    router(&k, json!({"kind": "chat", "title": "看图"}), "是一张图");
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
    assert!(k.gw.extras.lock().unwrap().last().unwrap().is_empty());
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
    let item = k.core.promote(WS, &ms[1].id).await.unwrap();
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
    assert_eq!(st["route"]["fallback"], false);
    let t = &st["tools"][0];
    assert_eq!(
        (t["name"].as_str(), t["status"].as_str()),
        (Some("create_item"), Some("ok"))
    );
    assert_eq!(t["args"]["bot"], "dev");
    assert_eq!(t["result"]["item"], json!(ms[1].item_id));
    /* a router failure is recorded as the fallback */
    k.gw.set(|_, _| Err(msg("down")));
    k.core.say(WS, "随便", &[]).unwrap();
    let ms = settle(&k, 4).await;
    assert_eq!(
        ms[3].meta["steps"]["route"]["why"],
        "没能判断，先在这里回答"
    );
    assert_eq!(ms[3].meta["steps"]["route"]["fallback"], true);
    assert_eq!(ms[3].meta["steps"]["route"]["guard"], "6");
    assert_eq!(ms[3].item_id, None, "a failed router never makes an item");
}

#[tokio::test]
async fn a_json_answer_splits_thought_and_text() {
    let k = Kit::new();
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(r#"{"kind":"chat"}"#.into())
        } else {
            assert!(p.contains("Reply with ONLY one JSON object: {\"thought\""));
            assert!(p.ends_with("User: 天为什么是蓝的"));
            Ok(r#"{"thought":"问的是散射","answer":"因为瑞利散射。"}"#.into())
        }
    });
    k.core.say(WS, "天为什么是蓝的", &[]).unwrap();
    let ms = settle(&k, 2).await;
    assert_eq!(ms[1].text, "因为瑞利散射。");
    assert_eq!(ms[1].meta["steps"]["answer"]["thought"], "问的是散射");
}

#[test]
fn notifications_and_the_read_cursor() {
    let k = Kit::new();
    /* a legacy release message in the conversation */
    k.db()
        .message_add(WS, "steve", "已发布新版本 v0", None, None)
        .unwrap();
    k.db().message_add(WS, "me", "你好", None, None).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3));
    crate::notify::notify(
        k.db(),
        WS,
        "archive",
        "info",
        "自动归档了 1 件",
        Some("ST-1"),
    )
    .unwrap();
    let ns = k.core.notifications(WS).unwrap();
    assert_eq!(
        ns.iter()
            .map(|n| (n.kind.as_str(), n.level.as_str()))
            .collect::<Vec<_>>(),
        vec![("archive", "info"), ("release", "success")]
    );
    assert_eq!(
        k.core
            .thread(WS)
            .unwrap()
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        vec!["你好"]
    );
    let first = k.core.notify_seen(WS).unwrap();
    assert_eq!(k.core.notify_seen(WS).unwrap(), first, "set once");
    assert_eq!(k.core.mark_notifications_seen(WS, Some(5)).unwrap(), 5);
    assert_eq!(k.core.notify_seen(WS).unwrap(), 5);
    assert!(k.core.mark_notifications_seen(WS, Some(i64::MAX)).unwrap() <= crate::util::now());
}

/// Real model calls through the Claude CLI (claude-haiku-5-5; the kiro route has no upstream); `cargo test -- --ignored`.
fn real_gateway(home: &std::path::Path) -> crate::ClaudeCli {
    crate::ClaudeCli {
        codex: None,
        executable: std::env::var("CLAUDE_CODE_EXECUTABLE")
            .unwrap_or_else(|_| "/opt/homebrew/bin/claude".into())
            .into(),
        fast: vec!["claude-haiku-5-5".into()],
        smart: vec!["claude-haiku-5-5".into()],
        cwd: home.to_path_buf(),
        models: None,
    }
}

#[tokio::test]
#[ignore = "calls a real model"]
async fn real_model_print_and_route() {
    use crate::ModelGateway;
    let dir = tempfile::tempdir().unwrap();
    let gw = real_gateway(dir.path());
    let p = gw
        .call(
            "Reply with ONLY this JSON: {\"ok\":true}",
            Tier::Fast,
            std::time::Duration::from_secs(90),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(p.model, "claude-haiku-5-5");
    assert_eq!(
        crate::gateway::first_json(&p.text).unwrap(),
        Some(json!({"ok": true}))
    );
    let k = Kit::new();
    let c = k.core.route_ctx(WS).unwrap();
    let r = crate::gateway::route(
        &gw,
        &c,
        "帮我把 ~/Downloads 里的照片按年份整理到文件夹",
        &[],
        false,
    )
    .await;
    assert!(!r.fallback, "the router answered: {r:?}");
    assert_eq!(r.kind, "item");
    assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
    let main = k.db().main_bot().unwrap();
    let a = crate::gateway::answer(
        &gw,
        &main,
        "1+1 等于几？只回答数字",
        &[],
        &Default::default(),
        "",
        &|_, _| {},
    )
    .await;
    assert!(!a.failed && a.text.contains('2'), "{a:?}");
}

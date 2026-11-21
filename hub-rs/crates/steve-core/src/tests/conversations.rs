//! Guest conversations: an IM space's turns are routed like web chat in a conversation
//! of their own (`chan:<space>`), answered in the host's voice, and never configure Steve or talk to
//! items; follow-ups continue the same conversation; items made there flow back there.

use crate::error::msg;
use crate::repo::{Message, WS};
use crate::testkit::Kit;
use crate::thread::{Conv, ConvHost};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SPACE: &str = "chan:spc_ops";

#[derive(Default)]
struct Host {
    answered: Mutex<Vec<String>>,
    said: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl ConvHost for Host {
    async fn answer(&self, _ws: &str, _space: &str, origin: &str) -> Result<String, String> {
        self.answered.lock().unwrap().push(origin.into());
        Ok("以值班小运的身份回答".into())
    }
    fn research_context(&self, _ws: &str, _space: &str, _origin: &str) -> String {
        "这次的问题来自渠道「运维群」".into()
    }
    fn said(&self, _ws: &str, space: &str, id: &str) {
        self.said.lock().unwrap().push((space.into(), id.into()));
    }
}

/// The router answers with `f(message)` and records the prompts it saw.
fn router(
    k: &Kit,
    seen: Arc<Mutex<Vec<String>>>,
    f: impl Fn(&str) -> Value + Send + Sync + 'static,
) {
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router") {
            seen.lock().unwrap().push(p.to_string());
            let m = p
                .lines()
                .rfind(|l| l.starts_with("User message: "))
                .and_then(|l| serde_json::from_str::<String>(&l["User message: ".len()..]).ok())
                .unwrap_or_default();
            Ok(f(&m).to_string())
        } else if p.starts_with("You are Steve") {
            Ok(r#"{"thought":"想","answer":"主线的回答","offer":null}"#.into())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
}

async fn settle(k: &Kit, space: &str, n: usize) -> Vec<Message> {
    let mut ms = Vec::new();
    steve_runtime::testkit::eventually_sync(
        steve_runtime::testkit::on_timeout(|| async {
            format!(
                "waited for {n}: {:#?}",
                k.db().messages_in(WS, space, 200).unwrap()
            )
        }),
        || {
            ms = k.db().messages_in(WS, space, 200).unwrap();
            ms.len() >= n
        },
    )
    .await;
    ms
}

/// Someone else speaking in a group (not the owner: what they ask for waits for the owner's yes).
fn guest(research: bool, items: bool) -> Conv {
    Conv {
        owner_only: false,
        space: SPACE.into(),
        research,
        items,
        start: false,
        hands_on: false,
    }
}

fn turn(k: &Kit, text: &str, c: Conv) -> String {
    k.core
        .say_in(WS, c, text, &json!({"chan": {"message": "cm_x", "space_name": "运维群", "sender": "李雷", "provider": "飞书"}}))
        .unwrap()
}

#[tokio::test]
async fn main_channel_turns_report_outcomes_without_using_the_hosts_voice() {
    let k = Kit::new();
    let host = Arc::new(Host::default());
    k.core.set_conv_host(host.clone());
    router(
        &k,
        Arc::new(Mutex::new(vec![])),
        |_| json!({"kind": "answer", "confidence": 1}),
    );
    let origin = k
        .core
        .say_in(
            WS,
            Conv::main(),
            "看下磁盘",
            &json!({"chan": {"message": "cm_web", "web": true}}),
        )
        .unwrap();
    let ms = settle(&k, crate::thread::MAIN, 2).await;
    assert_eq!(ms[1].text, "主线的回答");
    assert_eq!(ms[1].meta["origin"], origin);
    steve_runtime::testkit::eventually_sync(
        "The host receives the main conversation reply.",
        || !host.said.lock().unwrap().is_empty(),
    )
    .await;
    let mut expected = vec![(crate::thread::MAIN.to_string(), ms[1].id.clone())];
    assert_eq!(*host.said.lock().unwrap(), expected);
    assert!(host.answered.lock().unwrap().is_empty());

    // The same hook handles failure and stop receipts, including system messages.
    for outcome in ["error", "stopped"] {
        let mut meta = json!({"origin": origin, "system": true});
        meta[outcome] = json!(true);
        let id = k
            .core
            .post(WS, crate::thread::MAIN, outcome, None, Some(&meta))
            .unwrap();
        expected.push((crate::thread::MAIN.into(), id));
    }
    assert_eq!(*host.said.lock().unwrap(), expected);

    // Ordinary main-conversation turns and an unscoped note never reach the host.
    let ordinary = k
        .db()
        .message_add_in(WS, crate::thread::MAIN, "me", "你好", None, &json!({}))
        .unwrap();
    for meta in [
        json!({}),
        json!({"origin": ordinary}),
        json!({"origin": "missing"}),
    ] {
        k.core
            .post(WS, crate::thread::MAIN, "你好", None, Some(&meta))
            .unwrap();
    }
    assert_eq!(*host.said.lock().unwrap(), expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_main_channel_turn_during_routing_or_queueing_reports_its_origin() {
    for stop_kind in ["routing", "queued", "queued_run"] {
        let queued = stop_kind != "routing";
        let k = Kit::new();
        let host = Arc::new(Host::default());
        k.core.set_conv_host(host.clone());
        let store = k.db().store().clone();
        router(&k, Arc::new(Mutex::new(vec![])), move |_| {
            if queued {
                store.maintenance_set(true).unwrap();
            }
            json!({"kind": "answer", "confidence": 1})
        });
        if !queued {
            *k.gw.slow.lock().unwrap() = Some(("You are the router", Duration::from_secs(30)));
        }
        let origin = k
            .core
            .say_in(
                WS,
                Conv::main(),
                "慢慢答",
                &json!({"chan": {"message": "cm_web", "web": true}}),
            )
            .unwrap();
        let description = if queued {
            "The router has finished and the inline answer is in progress."
        } else {
            "The gateway has received the routing request."
        };
        steve_runtime::testkit::eventually_sync(description, || {
            if queued {
                k.core.inline_busy(WS) && !k.core.routing(WS)
            } else {
                k.gw.calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| p.starts_with("You are the router"))
            }
        })
        .await;
        if stop_kind == "queued_run" {
            let run = k.db().store().thread().runs_of_message(&origin).unwrap()[0]
                .id
                .clone();
            assert!(k.core.stop_run(WS, &run).unwrap());
            assert!(!k.core.stop_run(WS, &run).unwrap());
        } else {
            assert!(k.core.stop(WS).unwrap());
        }
        let ms = settle(&k, crate::thread::MAIN, 2).await;
        assert_eq!(ms[1].meta["origin"], origin);
        assert_eq!(ms[1].meta["stopped"], true);
        assert_eq!(
            *host.said.lock().unwrap(),
            [(crate::thread::MAIN.into(), ms[1].id.clone())]
        );
        k.db().store().maintenance_set(false).unwrap();
        steve_runtime::testkit::eventually_sync("The main conversation is no longer busy.", || {
            !k.core.main_busy(WS).unwrap()
        })
        .await;
        assert_eq!(k.db().messages(WS).unwrap().len(), 2, "one stop receipt");
    }
}

#[tokio::test]
async fn guest_turns_are_routed_and_answered_in_their_own_conversation() {
    let k = Kit::new();
    let host = Arc::new(Host::default());
    assert!(k.core.set_conv_host(host.clone()));
    let seen = Arc::new(Mutex::new(vec![]));
    router(&k, seen.clone(), |m| {
        if m.contains("以后") {
            json!({"kind": "config", "confidence": 0.9, "signals": [], "title": "", "goal": "", "bot": "steve"})
        } else {
            json!({"kind": "answer", "confidence": 0.9, "signals": ["question"], "title": "", "goal": "", "bot": "steve"})
        }
    });
    let a = turn(&k, "部署挂了怎么办", guest(true, true));
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(ms[0].author, "me");
    assert_eq!(ms[1].author, "steve");
    assert_eq!(
        ms[1].text, "以值班小运的身份回答",
        "the host's voice, not the main bot's"
    );
    assert_eq!(ms[1].meta["origin"], json!(a));
    assert_eq!(*host.answered.lock().unwrap(), std::slice::from_ref(&a));
    assert_eq!(
        *host.said.lock().unwrap(),
        [(SPACE.to_string(), ms[1].id.clone())]
    );
    assert!(
        k.db().messages(WS).unwrap().is_empty(),
        "the web conversation is untouched"
    );

    // a follow-up continues the same conversation: the router sees the first exchange
    let b = turn(&k, "以后都直接回滚吧", guest(true, true));
    let ms = settle(&k, SPACE, 4).await;
    let p = seen.lock().unwrap().last().cloned().unwrap();
    assert!(
        p.contains("部署挂了怎么办") && p.contains("以值班小运的身份回答"),
        "{p}"
    );
    // configuration never comes from a chat
    assert_eq!(ms[3].meta["steps"]["route"]["kind"], "answer");
    assert_eq!(ms[3].meta["steps"]["route"]["guard"], "guest");
    assert!(ms[3].meta.get("proposal").is_none());
    assert_eq!(ms[3].meta["origin"], json!(b));

    // “#ST-n …” and /汇报 are ordinary text there
    turn(&k, "/汇报", guest(true, true));
    let ms = settle(&k, SPACE, 6).await;
    assert_eq!(ms[5].text, "以值班小运的身份回答");
}

#[tokio::test]
async fn work_from_a_chat_becomes_an_item_only_when_the_host_allows_it() {
    let k = Kit::new();
    let host = Arc::new(Host::default());
    k.core.set_conv_host(host.clone());
    let seen = Arc::new(Mutex::new(vec![]));
    router(
        &k,
        seen,
        |_| json!({"kind": "item", "confidence": 0.95, "signals": ["explicit", "change"], "title": "修告警脚本", "goal": "把告警脚本的阈值修好", "bot": "steve", "reply": "好，交给它了。"}),
    );
    let before = k.db().items(WS).unwrap().len();
    // a space with outside members: answered in place, the work offered to you in the conversation
    turn(&k, "把告警脚本修一下", guest(false, false));
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(k.db().items(WS).unwrap().len(), before);
    assert_eq!(ms[1].item_id, None);
    assert_eq!(ms[1].meta["offer"]["title"], "修告警脚本");
    assert_eq!(ms[1].meta["steps"]["route"]["guard"], "guest");

    // an internal space: the item is made, its origin and announcement are in this conversation
    let mid = turn(&k, "把告警脚本修一下", guest(true, true));
    let ms = settle(&k, SPACE, 4).await;
    let id = ms[3].item_id.clone().expect("an item");
    let ti = k.db().store().thread().item(&id).unwrap().unwrap();
    assert_eq!(ti.origin_msg, mid);
    // someone else asked: a proposal that waits for you, from a source YOLO doesn't take for yours
    let it = k.db().item(&id).unwrap().unwrap();
    assert_eq!(it.source.1, "chat_proposal");
    assert!(
        k.db().store().sessions().of_item(&id).unwrap().is_empty(),
        "nothing runs"
    );
    let open: Vec<_> = k
        .db()
        .store()
        .decisions()
        .of_item(&id)
        .unwrap()
        .into_iter()
        .filter(|d| d.kind == "propose" && d.status == "open")
        .collect();
    assert_eq!(open.len(), 1, "a card asks you first");
    // your yes, however much was said since, starts it with what it would have started with: the
    // conversation it came from
    for n in 0..210 {
        k.db()
            .message_add_in(WS, SPACE, "me", &format!("闲聊 {n}"), None, &json!({}))
            .unwrap();
    }
    k.core.resolve(&open[0].id, "yes", "", "me").unwrap();
    let ses = k.db().store().sessions().of_item(&id).unwrap();
    assert_eq!(ses.len(), 1, "it runs now");
    let first =
        k.rt.prompts_of(&ses[0].id)
            .first()
            .cloned()
            .unwrap_or_default();
    assert!(
        first.contains("把告警脚本修一下") && first.contains("运维群"),
        "{first}"
    );
    // you asking in the same group: the work starts
    let mine = turn(
        &k,
        "把告警脚本也改一下",
        Conv {
            start: true,
            ..guest(true, true)
        },
    );
    let mut own = None;
    steve_runtime::testkit::eventually_sync(
        "The owner's conversation is bound to its new item.",
        || {
            own = k
                .db()
                .store()
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT item_id FROM thread_items WHERE origin_msg = ?",
                        [&mine],
                        |r| r.get::<_, String>(0),
                    )
                    .ok())
                })
                .unwrap();
            own.is_some()
        },
    )
    .await;
    let own = own.expect("your item");
    assert_ne!(own, id);
    assert_eq!(k.db().item(&own).unwrap().unwrap().source.1, "chat");
    assert!(
        k.db()
            .store()
            .decisions()
            .of_item(&own)
            .unwrap()
            .iter()
            .all(|d| d.kind != "propose")
    );
    assert_eq!(
        k.db()
            .store()
            .thread()
            .item(&own)
            .unwrap()
            .unwrap()
            .origin_msg,
        mine
    );
    assert_eq!(ti.reply_msg.as_deref(), Some(ms[3].id.as_str()));
    assert!(
        host.said
            .lock()
            .unwrap()
            .iter()
            .any(|(s, m)| s == SPACE && *m == ms[3].id)
    );
    // flow-back goes to the chat's conversation, not the web's
    crate::thread::flowback::flow(
        k.db(),
        &id,
        crate::thread::flowback::Update {
            kind: "done",
            text: "ST 完成".into(),
            decision: None,
            reference: None,
            full: false,
        },
    )
    .unwrap();
    assert!(k.db().messages(WS).unwrap().is_empty());
    let ms = k.db().messages_in(WS, SPACE, 200).unwrap();
    assert!(ms.iter().any(|m| m.meta["card"]["item"] == json!(id)));
}

#[tokio::test]
async fn the_owners_private_chat_answers_with_tools() {
    use crate::testkit::{FakeResearch, Step};
    let k = Kit::new();
    let host = Arc::new(Host::default());
    assert!(k.core.set_conv_host(host.clone()));
    let fr = FakeResearch::new(vec![Step::Text("在跑 2 个会话。")]);
    assert!(k.core.set_researcher(fr.clone()));
    router(
        &k,
        Arc::new(Mutex::new(vec![])),
        |_| json!({"kind": "answer", "confidence": 0.9, "signals": ["question"], "title": "", "goal": "", "bot": "steve"}),
    );
    let item = k.item("run", "执行", None);
    let mine = Conv {
        owner_only: true,
        ..guest(true, true)
    };
    k.core
        .say_in(
            WS,
            mine,
            &format!("{item} 现在怎么样"),
            &json!({"chan": {"message": "cm_me", "owner_only": true}}),
        )
        .unwrap();
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(ms[1].meta["steps"]["route"]["kind"], "research");
    assert_eq!(ms[1].meta["steps"]["route"]["guard"], "owner");
    assert!(
        host.answered.lock().unwrap().is_empty(),
        "no tool-less answer"
    );
    let seen = fr.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "a run with tools");
    assert!(
        seen[0].context.contains("对话提到的事项（已读取）"),
        "{}",
        seen[0].context
    );
}

#[tokio::test]
async fn a_shared_chat_gets_no_items_read_up_front() {
    use crate::testkit::{FakeResearch, Step};
    let k = Kit::new();
    let host = Arc::new(Host::default());
    assert!(k.core.set_conv_host(host.clone()));
    let fr = FakeResearch::new(vec![Step::Text("查不到。")]);
    assert!(k.core.set_researcher(fr.clone()));
    router(
        &k,
        Arc::new(Mutex::new(vec![])),
        |_| json!({"kind": "research", "confidence": 0.9, "signals": ["lookup"], "title": "", "goal": "", "bot": "steve"}),
    );
    let item = k.item("run", "执行", None);
    turn(&k, &format!("{item} 现在怎么样"), guest(true, true));
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(ms[1].meta["steps"]["route"]["kind"], "research");
    let seen = fr.seen.lock().unwrap().clone();
    assert!(
        !seen[0].context.contains("对话提到的事项"),
        "{}",
        seen[0].context
    );
}

/// A run has the owner's own access only where the instance's owner asks and they alone read the
/// answer; the router hears about the Hub's machine in Steve's own conversation only.
#[tokio::test]
async fn only_the_owner_alone_gets_a_run_with_their_own_access() {
    use crate::testkit::{FakeResearch, Step};
    use crate::thread::MAIN;
    const MACHINE: &str = "acts on the machine the Hub runs on";
    let k = Kit::new();
    let host = Arc::new(Host::default());
    assert!(k.core.set_conv_host(host.clone()));
    let fr = FakeResearch::new(vec![Step::Text("看过了。")]);
    assert!(k.core.set_researcher(fr.clone()));
    let prompts = Arc::new(Mutex::new(vec![]));
    router(
        &k,
        prompts.clone(),
        |_| json!({"kind": "research", "confidence": 0.9, "signals": ["lookup"], "title": "", "goal": "", "bot": "steve"}),
    );
    let full = |n: usize| fr.seen.lock().unwrap()[n].full;
    /* what the router was told when it routed `text` (a plain question is not routed at all) */
    let told = |text: &str| {
        let ps = prompts.lock().unwrap();
        ps.iter()
            .rfind(|p| p.contains(text))
            .map(|p| p.contains(MACHINE))
    };
    let mine = || Conv {
        owner_only: true,
        ..guest(true, true)
    };
    let private = json!({"chan": {"message": "cm_me", "owner_only": true}});

    /* an instance with one user: whoever writes in Steve's own conversation is the owner */
    assert!(k.db().store().users().count().unwrap() <= 1);
    k.core
        .say(WS, "把下载目录里的截图归到一个文件夹", &[])
        .unwrap();
    settle(&k, MAIN, 2).await;
    assert!(full(0));
    assert_eq!(told("归到一个文件夹"), Some(true));
    k.core
        .say_in(WS, mine(), "把桌面上的截图也归进去", &private)
        .unwrap();
    settle(&k, SPACE, 2).await;
    assert!(full(1), "the owner's private chat");
    assert_ne!(
        told("也归进去"),
        Some(true),
        "a chat is not Steve's own conversation"
    );
    turn(&k, "把服务器上的日志清一下", guest(true, true));
    settle(&k, SPACE, 4).await;
    assert!(!full(2), "a shared chat stays read-only");
    assert_eq!(told("日志清一下"), Some(false));

    /* several users: the owner, by role */
    let users = k.db().store().users();
    let owner = users.create("local:lee", "Lee", None, "owner").unwrap();
    let member = users.create("local:sam", "Sam", None, "member").unwrap();
    k.core
        .say_as(WS, Some(&owner), "装一下 ripgrep", &[], &[])
        .unwrap();
    settle(&k, MAIN, 4).await;
    assert!(full(3));
    k.core
        .say_as(WS, Some(&member), "装一下 fd", &[], &[])
        .unwrap();
    settle(&k, MAIN, 6).await;
    assert!(!full(4), "a member reads, as before");
    assert_eq!(
        told("装一下 fd"),
        Some(true),
        "the conversation is the same; the run is not"
    );
    k.core.say(WS, "装一下 jq", &[]).unwrap();
    settle(&k, MAIN, 8).await;
    assert!(!full(5), "nobody in particular is not the owner");
    k.core.say_in(WS, mine(), "装一下 bat", &private).unwrap();
    settle(&k, SPACE, 6).await;
    assert!(!full(6));
    assert_eq!(fr.seen.lock().unwrap().len(), 7);
}

/// A host whose model reaches for a tool its answer may not use: a request that needs the machine
/// (“看下我现在磁盘情况”). The channel's composer reports it in its own words around the gateway's.
#[derive(Default)]
struct NeedsTools {
    answered: Mutex<usize>,
    said: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl ConvHost for NeedsTools {
    async fn answer(&self, _ws: &str, _space: &str, _origin: &str) -> Result<String, String> {
        *self.answered.lock().unwrap() += 1;
        Err(format!(
            "回复没有生成：{}",
            steve_modules::gateway::NEEDS_TOOLS
        ))
    }
    fn research_context(&self, _ws: &str, _space: &str, _origin: &str) -> String {
        String::new()
    }
    fn said(&self, _ws: &str, space: &str, id: &str) {
        self.said.lock().unwrap().push((space.into(), id.into()));
    }
}

fn needs_tools_router(k: &Kit) {
    router(
        k,
        Arc::new(Mutex::new(vec![])),
        |_| json!({"kind": "answer", "confidence": 0.9, "signals": ["question"], "title": "看磁盘", "goal": "看看这台机器的磁盘用量", "bot": "steve"}),
    );
}

/// Your own trigger word asked for something only the machine can tell; the answer has no tools,
/// its model reached for one and the call refused it. Before, that failure ended in the log only.
/// Now the question becomes an item a Bot runs, once, said in the conversation (and so in the chat).
#[tokio::test]
async fn an_answer_that_needs_tools_is_handed_to_an_item_once() {
    let k = Kit::new();
    let host = Arc::new(NeedsTools::default());
    assert!(k.core.set_conv_host(host.clone()));
    needs_tools_router(&k);
    let before = k.db().items(WS).unwrap().len();
    // your own trigger word
    let mine = Conv {
        start: true,
        hands_on: true,
        ..guest(true, true)
    };
    let mid = turn(&k, "看下我现在磁盘情况", mine);
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(*host.answered.lock().unwrap(), 1);
    let id = ms[1].item_id.clone().expect("handed to an item");
    assert_eq!(k.db().items(WS).unwrap().len(), before + 1);
    assert!(
        ms[1].text.contains(&id) && ms[1].text.contains("跑命令"),
        "says why: {}",
        ms[1].text
    );
    assert_eq!(ms[1].meta["origin"], json!(mid));
    assert_eq!(ms[1].meta["handoff"]["why"], "needs_tools");
    let it = k.db().item(&id).unwrap().unwrap();
    assert_eq!(it.source.1, "chat", "yours: the work starts");
    assert_eq!(
        k.db()
            .store()
            .thread()
            .item(&id)
            .unwrap()
            .unwrap()
            .origin_msg,
        mid
    );
    let log = k.db().store().thread().route_of(&mid).unwrap().unwrap();
    assert_eq!(log.outcome.as_deref(), Some("needs_tools"));
    assert_eq!(log.item_id.as_deref(), Some(id.as_str()));
    // the announcement reaches the chat through the host, like any item made there
    steve_runtime::testkit::eventually_sync("The host receives the handoff announcement.", || {
        host.said
            .lock()
            .unwrap()
            .iter()
            .any(|(_, m)| m == &ms[1].id)
    })
    .await;
    assert!(
        host.said
            .lock()
            .unwrap()
            .iter()
            .any(|(_, m)| m == &ms[1].id)
    );

    // once: the same question failing again does not make a second item
    let job = crate::thread::research::InlineJob {
        kind: crate::thread::research::Inline::Answer,
        origin: mid.clone(),
        question: "看下我现在磁盘情况".into(),
        files: vec![],
        earlier: vec![],
        user: None,
        project: None,
        route: None,
        offer: None,
        reply_to: None,
        run: None,
        stop: crate::thread::Stop::default(),
        t0: crate::util::now(),
        resume: None,
        space: SPACE.into(),
        to_item: Some(true),
    };
    assert_eq!(k.core.hand_off(WS, &job).await.unwrap(), None);
    assert_eq!(k.db().items(WS).unwrap().len(), before + 1);
}

/// Where a turn may not become an item (a chat with outside members), a request that needs tools
/// is not dropped either: the conversation says why in plain words and what to do, marked as a
/// failure so the chat hears of it.
#[tokio::test]
async fn an_answer_that_needs_tools_where_no_item_may_be_made_says_why() {
    let k = Kit::new();
    let host = Arc::new(NeedsTools::default());
    assert!(k.core.set_conv_host(host.clone()));
    needs_tools_router(&k);
    let before = k.db().items(WS).unwrap().len();
    let mine = Conv {
        start: true,
        hands_on: true,
        ..guest(true, false)
    };
    turn(&k, "看下我现在磁盘情况", mine);
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(k.db().items(WS).unwrap().len(), before);
    assert_eq!(ms[1].item_id, None);
    assert_eq!(ms[1].text, crate::thread::handoff::NEEDS_TOOLS_HERE);
    assert_eq!(ms[1].meta["error"], true);
    assert!(
        host.said
            .lock()
            .unwrap()
            .iter()
            .any(|(_, m)| m == &ms[1].id)
    );
}

/// A turn that is not a trigger word on the owner's account (a Bot's chat, where items are
/// allowed) is not handed to an item on its own when its answer needs tools: it stays a failed
/// answer that says why, as before; “做成事项” is the way to make one.
#[tokio::test]
async fn an_answer_that_needs_tools_in_a_bots_chat_is_not_made_an_item() {
    let k = Kit::new();
    let host = Arc::new(NeedsTools::default());
    assert!(k.core.set_conv_host(host.clone()));
    needs_tools_router(&k);
    let before = k.db().items(WS).unwrap().len();
    let mid = turn(
        &k,
        "看下我现在磁盘情况",
        Conv {
            start: true,
            ..guest(true, true)
        },
    );
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(*host.answered.lock().unwrap(), 1);
    assert_eq!(
        k.db().items(WS).unwrap().len(),
        before,
        "no item on its own"
    );
    assert_eq!(ms[1].item_id, None);
    assert_eq!(ms[1].text, crate::thread::handoff::NEEDS_TOOLS_HERE);
    assert_eq!(ms[1].meta["error"], true);
    let log = k.db().store().thread().route_of(&mid).unwrap().unwrap();
    assert_eq!(log.item_id, None);
}

/// The router's clarifying question (with options the chat would get spelled out) is never put to a
/// guest conversation: the host answers in its own words instead.
#[tokio::test]
async fn a_guest_never_gets_the_routers_question() {
    let k = Kit::new();
    let host = Arc::new(Host::default());
    assert!(k.core.set_conv_host(host.clone()));
    router(&k, Arc::new(Mutex::new(vec![])), |_| {
        json!({"kind": "ask", "confidence": 0.9, "signals": ["question"], "title": "", "goal": "",
               "bot": "steve", "reply": "你说的“很稳定”具体是指什么？", "options": ["产品", "服务"]})
    });
    let a = turn(&k, "但是真的很稳定", guest(true, true));
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(ms[1].text, "以值班小运的身份回答");
    assert_eq!(ms[1].meta["origin"], json!(a));
    assert!(ms[1].meta.get("ask").is_none(), "{:?}", ms[1].meta);
    assert_eq!(*host.answered.lock().unwrap(), std::slice::from_ref(&a));
}

/// A host whose answers wait until released.
#[derive(Default)]
struct SlowHost {
    go: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl ConvHost for SlowHost {
    async fn answer(&self, _ws: &str, _space: &str, _origin: &str) -> Result<String, String> {
        self.go.notified().await;
        Ok("答".into())
    }
    fn research_context(&self, _ws: &str, _space: &str, _origin: &str) -> String {
        String::new()
    }
    fn said(&self, _ws: &str, _space: &str, _id: &str) {}
}

/// The queue's “前面还有 3 个问题在排队…” is the thread's own state: marked as a system text, which a
/// guest conversation's host never says in the chat.
#[tokio::test]
async fn a_full_queue_is_marked_as_a_system_text() {
    let k = Kit::new();
    let host = Arc::new(SlowHost::default());
    assert!(k.core.set_conv_host(host.clone()));
    router(
        &k,
        Arc::new(Mutex::new(vec![])),
        |_| json!({"kind": "answer", "confidence": 0.9, "signals": ["question"], "title": "", "goal": "", "bot": "steve"}),
    );
    // one answering, MAX_WAITING waiting: the next turn of the conversation finds the queue full
    for i in 0..=crate::thread::research::MAX_WAITING + 1 {
        turn(&k, &format!("在吗 {i}"), guest(true, true));
    }
    let mut full = None;
    steve_runtime::testkit::eventually_sync(
        "A full conversation queue records its system reply.",
        || {
            full = k
                .db()
                .messages_in(WS, SPACE, 200)
                .unwrap()
                .into_iter()
                .find(|m| m.author == "steve");
            full.is_some()
        },
    )
    .await;
    let m = full.expect("the queue filled up");
    assert_eq!(m.text, crate::thread::research::QUEUE_FULL);
    assert_eq!(m.meta[crate::thread::research::SYSTEM], json!(true));
    host.go.notify_waiters();
}

struct PreparingResearch(tokio::sync::Semaphore);

#[async_trait::async_trait]
impl ConvHost for PreparingResearch {
    async fn answer(&self, _: &str, _: &str, _: &str) -> Result<String, String> {
        panic!("research never uses the tool-less answer")
    }
    fn research_context(&self, _: &str, _: &str, _: &str) -> String {
        String::new()
    }
    async fn research_answer(&self, _: &str, _: &str, _: &str, _: &str) -> Result<String, String> {
        self.0.add_permits(1);
        std::future::pending().await
    }
    fn said(&self, _: &str, _: &str, _: &str) {}
}

#[tokio::test]
async fn stopping_during_research_preparation_keeps_the_result_without_sending_authority() {
    use crate::testkit::{FakeResearch, Step};
    let k = Kit::new();
    let host = Arc::new(PreparingResearch(tokio::sync::Semaphore::new(0)));
    k.core.set_conv_host(host.clone());
    k.core
        .set_researcher(FakeResearch::new(vec![Step::Text("已读到的部分摘要")]));
    router(
        &k,
        Arc::new(Mutex::new(vec![])),
        |_| json!({"kind":"research","confidence":0.99,"signals":["lookup"],"title":"群聊摘要","bot":"steve"}),
    );
    let origin = turn(&k, "总结最近两天", guest(true, true));
    tokio::time::timeout(Duration::from_secs(5), host.0.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let run = k
        .db()
        .store()
        .thread()
        .runs_of_message(&origin)
        .unwrap()
        .remove(0);
    assert!(k.core.stop_run(WS, &run.id).unwrap());
    let ms = settle(&k, SPACE, 2).await;
    assert_eq!(ms[1].meta["research_result"], "已读到的部分摘要");
    assert_eq!(ms[1].meta["host_reply"]["status"], "ineligible");
    assert!(
        ms[1].meta["host_reply"]["reason"]
            .as_str()
            .unwrap()
            .contains("停止")
    );
    assert_eq!(ms[1].meta["steps"]["research"]["stopped"], "user");
    assert!(ms[1].text.contains("已读到的部分摘要") && ms[1].text.contains("已停下"));
    steve_runtime::testkit::eventually_sync("The stopped research run settles durably.", || {
        k.db()
            .store()
            .thread()
            .run(&run.id)
            .unwrap()
            .unwrap()
            .status
            == "stopped"
    })
    .await;
}

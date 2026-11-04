//! Guest conversations (channels §2.12): an IM space's turns are routed like web chat in a conversation
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
    fn research_context(&self, _ws: &str, _space: &str) -> String {
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
    for _ in 0..500 {
        let ms = k.db().messages_in(WS, space, 200).unwrap();
        if ms.len() >= n {
            return ms;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "waited for {n}: {:#?}",
        k.db().messages_in(WS, space, 200).unwrap()
    );
}

/// Someone else speaking in a group (not the owner: what they ask for waits for the owner's yes).
fn guest(research: bool, items: bool) -> Conv {
    Conv {
        owner_only: false,
        space: SPACE.into(),
        research,
        items,
        start: false,
    }
}

fn turn(k: &Kit, text: &str, c: Conv) -> String {
    k.core
        .say_in(WS, c, text, &json!({"chan": {"message": "cm_x", "space_name": "运维群", "sender": "李雷", "provider": "飞书"}}))
        .unwrap()
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
    for _ in 0..300 {
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
        if own.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

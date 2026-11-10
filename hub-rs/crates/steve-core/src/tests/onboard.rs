//! The first meeting: the welcome, the turns that skip the router and carry the brief, and its end.

use crate::repo::WS;
use crate::testkit::{FakeResearch, Kit, Step};
use crate::thread::MAIN;
use crate::thread::onboard::{BRIEF, DONE, PENDING, STATE, TURNS};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A router that always says research; returns how many times it was asked.
fn router(k: &Kit) -> Arc<AtomicUsize> {
    let asked = Arc::new(AtomicUsize::new(0));
    let n = asked.clone();
    k.gw.set(move |p, _| {
        assert!(p.starts_with("You are the router of Steve"), "{p}");
        n.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"kind": "research", "confidence": 0.9, "signals": ["lookup"], "title": "", "goal": "", "bot": "steve"}).to_string())
    });
    asked
}

async fn settle(k: &Kit, n: usize) -> Vec<crate::repo::Message> {
    for _ in 0..500 {
        let ms = k.db().messages(WS).unwrap();
        if ms.len() >= n {
            return ms;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no reply: {:?}", k.db().messages(WS).unwrap());
}

#[tokio::test]
async fn the_first_meeting_is_a_conversation_with_steve_itself() {
    let k = Kit::new();
    let fr = FakeResearch::new(vec![Step::Text("好的。")]);
    assert!(k.core.set_researcher(fr.clone()));
    let asked = router(&k);
    let brief = |n: usize| fr.seen.lock().unwrap()[n].brief.clone();
    let state = || k.db().setting(WS, STATE, "").unwrap();

    /* before it: a request is routed, and the run hears nothing of a meeting */
    assert!(!k.core.onboard_pending(WS).unwrap());
    let before = k.core.say(WS, "把下载目录整理一下", &[]).unwrap();
    settle(&k, 2).await;
    assert_eq!((asked.load(Ordering::SeqCst), brief(0)), (1, String::new()));
    assert!(!k.core.onboard_allows(WS, &before, None).unwrap());

    /* Steve says hello; a second start (another tab) says nothing more */
    let hello = k.core.onboard_start(WS, "me").unwrap();
    assert_eq!(k.core.onboard_start(WS, "me").unwrap(), hello);
    let ms = settle(&k, 3).await;
    assert_eq!(ms.len(), 3);
    assert_eq!(
        (ms[2].id.as_str(), ms[2].author.as_str()),
        (hello.as_str(), "steve")
    );
    assert_eq!(ms[2].meta, json!({"onboard": true}));
    assert!(
        ms[2].text.starts_with("你好，我是 Steve。"),
        "{}",
        ms[2].text
    );
    assert_eq!(state(), PENDING);

    /* the same request now goes straight to a run that carries the brief */
    let mid = k
        .core
        .say(WS, "叫我小李，帮我把下载目录整理一下", &[])
        .unwrap();
    let ms = settle(&k, 5).await;
    assert_eq!(asked.load(Ordering::SeqCst), 1, "no router");
    assert_eq!(brief(1), BRIEF.trim());
    let route = &ms[4].meta["steps"]["route"];
    assert_eq!(
        (&route["kind"], &route["guard"]),
        (&json!("research"), &json!("meet"))
    );
    assert!(fr.seen.lock().unwrap()[1].full);
    assert!(k.core.onboard_allows(WS, &mid, None).unwrap());

    /* a message that names an item is still the router's; the run it picks is part of the meeting */
    k.core.say(WS, "ST-99 是做什么的？", &[]).unwrap();
    settle(&k, 7).await;
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert_eq!(brief(2), BRIEF.trim());

    /* another member's message is not the meeting, and neither is a chat */
    let users = k.db().store().users();
    let owner = users.create("local:owner", "Owner", None, "owner").unwrap();
    let member = users
        .create("local:member", "Member", None, "member")
        .unwrap();
    let theirs = k
        .core
        .say_as(WS, Some(&member), "帮我看看磁盘还剩多少", &[], &[])
        .unwrap();
    settle(&k, 9).await;
    assert_eq!((asked.load(Ordering::SeqCst), brief(3)), (3, String::new()));
    assert!(!k.core.onboard_allows(WS, &theirs, Some(&member)).unwrap());
    assert!(k.core.onboard_allows(WS, &mid, Some(&owner)).unwrap());
    let private = json!({"chan": {"message": "cm_1", "owner_only": true}});
    let there = k
        .db()
        .message_add_in(WS, "sp_chat", "me", "你好", None, &private)
        .unwrap();
    assert!(!k.core.onboard_allows(WS, &there, Some(&owner)).unwrap());

    /* done (or skipped): the router is back, the brief is gone, the tools are closed */
    k.core.onboard_end(WS, "me").unwrap();
    assert_eq!(state(), DONE);
    k.core
        .say_as(WS, Some(&owner), "帮我把桌面也整理一下", &[], &[])
        .unwrap();
    settle(&k, 11).await;
    let n = fr.seen.lock().unwrap().len();
    assert_eq!(
        (asked.load(Ordering::SeqCst), brief(n - 1)),
        (4, String::new())
    );
    assert!(!k.core.onboard_allows(WS, &mid, Some(&owner)).unwrap());

    /* meeting again (the web's “replay”) starts a new one */
    let again = k.core.onboard_start(WS, "me").unwrap();
    assert_ne!(again, hello);
    assert_eq!(state(), PENDING);
}

#[tokio::test]
async fn a_meeting_nobody_finished_runs_out() {
    let k = Kit::new();
    let fr = FakeResearch::new(vec![Step::Text("好的。")]);
    assert!(k.core.set_researcher(fr.clone()));
    let asked = router(&k);
    k.core.onboard_start(WS, "me").unwrap();
    for i in 0..TURNS {
        k.core.say(WS, &format!("帮我做第 {i} 件事"), &[]).unwrap();
        settle(&k, 1 + 2 * (i + 1)).await;
    }
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(fr.seen.lock().unwrap().iter().all(|r| !r.brief.is_empty()));
    assert!(k.core.onboard_pending(WS).unwrap());
    k.core.say(WS, "帮我再做一件事", &[]).unwrap();
    settle(&k, 3 + 2 * TURNS).await;
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the router is back");
    assert!(fr.seen.lock().unwrap()[TURNS].brief.is_empty());
    assert_eq!(k.db().setting(WS, STATE, "").unwrap(), DONE);
}

#[tokio::test]
async fn the_welcome_is_in_the_language_of_the_request() {
    let k = Kit::new();
    let id = steve_i18n::scope(steve_i18n::EN, async {
        k.core.onboard_start(WS, "me").unwrap()
    })
    .await;
    let m = k.db().message(&id).unwrap().unwrap();
    assert!(m.text.starts_with("Hi, I'm Steve."), "{}", m.text);
    assert_eq!(m.space, MAIN);
}

//! hub/test/inbox.test.ts: a chat answer becomes at most one item, and every reminder in 待处理 can be cleared.

use crate::ItemAction;
use crate::patch;
use crate::repo::WS;
use crate::testkit::Kit;
use serde_json::json;
use std::time::Duration;

fn chat_answer(k: &Kit) -> String {
    k.db()
        .message_add(
            WS,
            "steve",
            "建议这样做……",
            None,
            Some(&json!({"chat": "按建议"})),
        )
        .unwrap()
}
fn count(k: &Kit) -> usize {
    k.db().items(WS).unwrap().len()
}

#[tokio::test]
async fn promoting_the_same_answer_twice_at_once_makes_one_item() {
    let k = Kit::new();
    let m = chat_answer(&k);
    let before = count(&k);
    let (a, b) = tokio::join!(k.core.promote(WS, &m), k.core.promote(WS, &m));
    assert_eq!(a.unwrap().id, b.unwrap().id);
    assert_eq!(count(&k), before + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn promoting_with_a_slow_router_waits_for_the_same_item() {
    let k = Kit::new();
    let m = chat_answer(&k);
    let before = count(&k);
    let gw = k.gw.clone();
    gw.set(|_, _| {
        std::thread::sleep(Duration::from_millis(50));
        Err(crate::error::msg("slow and broken"))
    });
    let c1 = k.core.clone();
    let c2 = k.core.clone();
    let (m1, m2) = (m.clone(), m.clone());
    let a = tokio::spawn(async move { c1.promote(WS, &m1).await.map(|i| i.id) });
    let b = tokio::spawn(async move { c2.promote(WS, &m2).await.map(|i| i.id) });
    let (a, b) = (a.await.unwrap().unwrap(), b.await.unwrap().unwrap());
    assert_eq!(a, b);
    assert_eq!(count(&k), before + 1);
}

#[tokio::test]
async fn promoting_an_answer_that_was_already_promoted_returns_the_same_item() {
    let k = Kit::new();
    let m = chat_answer(&k);
    let first = k.core.promote(WS, &m).await.unwrap();
    let before = count(&k);
    let again = k.core.promote(WS, &m).await.unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(count(&k), before);
    let meta = k.db().message(&m).unwrap().unwrap().meta;
    assert!(meta.get("chat").is_none());
    assert_eq!(meta["promoted"], json!(first.id));
    let last = k.db().messages(WS).unwrap().pop().unwrap();
    assert_eq!(last.text, "好，转成事项交给它了，对话内容作为上下文带入。");
    /* the session opening carries Steve's answer */
    let s = k.db().sessions_of_item(&first.id).unwrap().pop().unwrap();
    let first = &k.rt.prompts_of(&s.id)[0];
    assert!(first.contains("这件事来自你和用户的主线对话。"), "{first}");
    assert!(first.contains("：建议这样做……"), "{first}");
}

#[tokio::test]
async fn promoting_a_message_from_another_workspace_is_refused() {
    let k = Kit::new();
    let m = chat_answer(&k);
    assert_eq!(
        k.core
            .promote("elsewhere", &m)
            .await
            .unwrap_err()
            .to_string(),
        "这条消息不能转成事项"
    );
    let plain = k.db().message_add(WS, "steve", "x", None, None).unwrap();
    assert!(k.core.promote(WS, &plain).await.is_err());
}

fn waiting(k: &Kit, kind: &str) -> (String, String) {
    let i = k.item("wait", "执行", None);
    let s = k.session(&i, "idle");
    let d = k.decision(&i, Some(&s), kind, "item", &[]);
    (i, d)
}

#[test]
fn dismissing_a_reminder_clears_it_and_undo_brings_it_back() {
    let k = Kit::new();
    let (i, d) = waiting(&k, "ask");
    k.core.dismiss_decision(&d, "me").unwrap();
    assert!(k.open_kinds(&i).is_empty());
    /* nothing else open and the session is idle: parked, so 继续 picks it up */
    assert_eq!(k.status(&i).0, "pause");
    k.core.undo_decision(&d, "me").unwrap();
    assert_eq!(k.open_kinds(&i).len(), 1);
    assert_eq!(k.status(&i).0, "wait");
}

#[test]
fn dismissing_twice_is_refused() {
    let k = Kit::new();
    let (_, d) = waiting(&k, "fail");
    k.core.dismiss_decision(&d, "me").unwrap();
    assert_eq!(
        k.core.dismiss_decision(&d, "me").unwrap_err().to_string(),
        "这个决定已经处理过了"
    );
}

#[test]
fn dismissing_a_permission_denies_it_and_cannot_be_undone() {
    let k = Kit::new();
    let (i, _) = waiting(&k, "ask");
    let s = k.db().sessions_of_item(&i).unwrap().pop().unwrap();
    let p = k.decision(
        &i,
        Some(&s.id),
        "approve",
        "tool_call",
        &["a|允许|allow_once"],
    );
    k.rt.st().permissions.insert(p.clone());
    k.core.dismiss_decision(&p, "me").unwrap();
    assert_eq!(k.rt.st().answered.last().unwrap(), &(p.clone(), None));
    assert!(k.core.undo_decision(&p, "me").is_err());
    /* the question is still open, so the item still waits */
    assert_eq!(k.status(&i).0, "wait");
}

#[test]
fn dropping_an_item_clears_all_its_reminders() {
    let k = Kit::new();
    let (i, _) = waiting(&k, "fail");
    k.decision(&i, None, "ask", "question", &[]);
    k.core.item_action(&i, ItemAction::Drop, "me").unwrap();
    assert!(k.open_kinds(&i).is_empty());
    assert_eq!(k.status(&i).0, "drop");
}

#[test]
fn finishing_an_item_clears_reminders_still_open_on_it() {
    let k = Kit::new();
    let (i, _) = waiting(&k, "ask");
    let acc = k.decision(&i, None, "accept", "item", &["pass", "rework"]);
    k.core
        .tx()
        .lc_request_complete(
            &i,
            "dev",
            crate::cycle::Policy::Human,
            false,
            "交付",
            Some(acc.clone()),
            vec![],
        )
        .unwrap();
    k.core.resolve(&acc, "pass", "", "me").unwrap();
    assert_eq!(k.status(&i).0, "done");
    assert!(k.open_kinds(&i).is_empty());
}

#[test]
fn on_boot_reminders_left_on_finished_or_dropped_items_are_cleared() {
    let k = Kit::new();
    let (i, _) = waiting(&k, "fail");
    // dropped before the lifecycle existed (as migration 027 leaves such an item), its card still open
    k.db()
        .exec(
            "UPDATE items SET status = 'drop', phase = 'closed', outcome = 'cancelled' WHERE id = ?",
            &[&i as &dyn rusqlite::ToSql],
        )
        .unwrap();
    k.core.recover().unwrap();
    assert!(k.open_kinds(&i).is_empty());
    let d = k.db().decisions_of_item(&i).unwrap().pop().unwrap();
    assert_eq!(d.result, json!({"dropped": true}));
}

#[test]
fn on_boot_a_question_older_than_a_delivery_expires() {
    let k = Kit::new();
    let (i, _) = waiting(&k, "ask");
    std::thread::sleep(Duration::from_millis(5));
    let acc = k.decision(&i, None, "accept", "item", &["pass", "rework"]);
    k.core.recover().unwrap();
    let open: Vec<String> = k
        .db()
        .decisions_of_item(&i)
        .unwrap()
        .into_iter()
        .filter(|d| d.status == "open")
        .map(|d| d.id)
        .collect();
    assert_eq!(open, vec![acc]);
}

#[test]
fn a_turn_cut_off_by_a_restart_that_nothing_continues_shows_one_card() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    assert_eq!(k.db().session(&s).unwrap().unwrap().status, "idle");
    assert_eq!(k.open_kinds(&i), vec!["fail"]);
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty(), "never resumed");
    k.db().session_patch(&s, patch![status: "running"]).unwrap();
    k.core.recover().unwrap();
    assert_eq!(k.open_kinds(&i), vec!["fail"], "the same card");
}

/// The Hub calls `recover` on a blocking thread of its tokio runtime; `ready` must flush inside that
/// runtime (a backend may start the turn with `tokio::spawn`, which panics on a bare thread).
#[tokio::test(flavor = "multi_thread")]
async fn the_ready_flush_runs_inside_the_tokio_runtime() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.db().queue_add(&s, "排着的", "me", &[]).unwrap();
    let core = k.core.clone();
    tokio::task::spawn_blocking(move || core.recover())
        .await
        .unwrap()
        .unwrap();
    assert!(
        k.rt.prompts_of(&s).is_empty(),
        "recover does not start a turn before listener ready"
    );
    let core = k.core.clone();
    tokio::task::spawn_blocking(move || core.ready())
        .await
        .unwrap();
    assert_eq!(k.rt.prompts_of(&s), vec!["排着的"]);
    assert_eq!(k.rt.st().flush_in_runtime, vec![true]);
}

#[test]
fn a_paused_item_is_not_resumed() {
    let k = Kit::new();
    let i = k.item("pause", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    k.core.ready();
    assert!(k.user_texts(&s).is_empty());
    let last = k.db().events_after(&s, 0).unwrap().pop().unwrap();
    assert_eq!(
        last.data["text"],
        "Hub 重启，这一回合被中断。事项已暂停，继续时接着做。"
    );
}

#[test]
fn boot_expires_open_tool_permissions() {
    let k = Kit::new();
    let i = k.item("wait", "执行", None);
    let s = k.session(&i, "idle");
    let p = k.decision(&i, Some(&s), "approve", "tool_call", &["a|允许|allow_once"]);
    k.core.recover().unwrap();
    assert_eq!(k.db().decision(&p).unwrap().unwrap().status, "expired");
    assert_eq!(k.status(&i).0, "run");
    assert!(
        k.db()
            .events_after(&s, 0)
            .unwrap()
            .iter()
            .any(|e| e.data["approval_expired"] == p)
    );
}

#[test]
fn no_resume_at_boot_when_switched_off() {
    let k = Kit::with(|c| c.no_resume = true);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.db().queue_add(&s, "排着的", "me", &[]).unwrap();
    k.core.recover().unwrap();
    k.core.ready();
    assert!(k.user_texts(&s).is_empty());
    assert!(k.rt.prompts_of(&s).is_empty());
    assert_eq!(k.db().session(&s).unwrap().unwrap().status, "idle");
    assert_eq!(
        k.db().events_after(&s, 0).unwrap().pop().unwrap().data["text"],
        "Hub 重启，这一回合被中断。没有自动继续，需要时让它继续。"
    );
}

#[test]
fn no_resume_where_the_working_directory_is_gone() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.db()
        .exec(
            "UPDATE sessions SET cwd = '/nonexistent/steve-test' WHERE id = ?",
            &[&s],
        )
        .unwrap();
    k.core.recover().unwrap();
    k.core.ready();
    assert!(k.rt.prompts_of(&s).is_empty());
    assert_eq!(
        k.open_fails(&i),
        vec!["Hub 重启时这个会话正在运行，这一回合被中断了"]
    );
}

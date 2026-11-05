//! Queued messages of a session: edit, withdraw and send now, including the races with a turn that
//! takes the queue.

use crate::repo::WS;
use crate::testkit::Kit;
use crate::{CoreError, CreateItem, SendNow};

fn started(k: &Kit) -> (String, String) {
    let i = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g".into(),
            title: "标题".into(),
            ..Default::default()
        })
        .unwrap();
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    (i.id, s.id)
}

fn queue(k: &Kit, sid: &str, texts: &[&str]) -> Vec<String> {
    for t in texts {
        assert_eq!(
            k.core.session_message(sid, t, false, &[]).unwrap(),
            crate::Prompted::Queued
        );
    }
    k.db()
        .queued(sid)
        .unwrap()
        .into_iter()
        .map(|q| q.id)
        .collect()
}

fn kinds(k: &Kit, sid: &str) -> Vec<String> {
    k.db()
        .events_after(sid, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

fn is_conflict(e: CoreError) -> bool {
    matches!(&e, CoreError::Module(m) if m.status() == 409 && e.to_string() == "这条消息已经发出，不能再改")
}

#[test]
fn edit_changes_what_the_next_turn_sends() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二"]);
    let after = k.core.queued_edit(&sid, &q[1], "二（改）").unwrap();
    assert_eq!(after.text, "二（改）");
    let ev = k.db().events_after(&sid, 0).unwrap();
    let e = ev.iter().find(|e| e.kind == "queued_edited").unwrap();
    assert_eq!(
        (
            e.data["id"].as_str(),
            e.data["text"].as_str(),
            e.data["was"].as_str()
        ),
        (Some(q[1].as_str()), Some("二（改）"), Some("二"))
    );
    assert!(
        k.timeline_texts(&item)
            .contains(&"修改了排队的消息：二（改）".to_string())
    );
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "一\n\n二（改）");
}

#[test]
fn remove_takes_it_out_of_the_queue() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    let f = k.file("a.txt", "text/plain", b"x");
    k.core
        .session_message(&sid, "一", false, std::slice::from_ref(&f))
        .unwrap();
    let q = queue(&k, &sid, &["二"]);
    let gone = k.core.queued_remove(&sid, &q[0]).unwrap();
    assert_eq!(
        (gone.text.as_str(), gone.files.as_slice()),
        ("一", &[f][..]),
        "the row comes back with its files (for the composer)"
    );
    assert!(kinds(&k, &sid).contains(&"queued_removed".to_string()));
    assert!(
        k.timeline_texts(&item)
            .contains(&"撤回了排队的消息：一".to_string())
    );
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "二");
}

#[test]
fn a_row_a_turn_already_took_is_a_409_and_nothing_changes() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一"]);
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "一");
    let before = kinds(&k, &sid).len();
    assert!(is_conflict(
        k.core.queued_edit(&sid, &q[0], "晚了").unwrap_err()
    ));
    assert!(is_conflict(k.core.queued_remove(&sid, &q[0]).unwrap_err()));
    assert!(is_conflict(
        k.core.queued_send_now(&sid, &q[0]).unwrap_err()
    ));
    assert_eq!(kinds(&k, &sid).len(), before, "no event for a miss");
    assert_eq!(k.rt.prompts_of(&sid).len(), 2, "not sent again");
    // another session's id is a miss too
    let q = queue(&k, &sid, &["二"]);
    let (_, other) = started(&k);
    assert!(is_conflict(
        k.core.queued_edit(&other, &q[0], "x").unwrap_err()
    ));
}

#[test]
fn send_now_interrupts_and_the_queue_runs_next_with_it_first() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二", "三"]);
    assert_eq!(
        k.core.queued_send_now(&sid, &q[2]).unwrap(),
        SendNow::Interrupted
    );
    assert_eq!(k.rt.st().cancelled, std::slice::from_ref(&sid));
    assert!(k.rt.st().stopped.is_empty(), "send-now is no user stop");
    assert!(kinds(&k, &sid).contains(&"queued_sent_now".to_string()));
    // the adapter ends the cancelled turn: the queue is the next turn, in order
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "三\n\n一\n\n二");
    assert!(k.db().queued(&sid).unwrap().is_empty());
}

/// B2b review: the turn ends and takes the queue (the moved-up message with it) between reading
/// the running turn and stopping it. The stop reaches nothing and the flush finds the follow-up
/// busy; the row is gone, so the message went: Sent, said in the session, and the item goes on.
#[test]
fn send_now_after_the_turn_took_the_queue_as_its_follow_up_is_sent() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二"]);
    k.rt.st().follow_up_before_stop = true;
    assert_eq!(k.core.queued_send_now(&sid, &q[1]).unwrap(), SendNow::Sent);
    assert_eq!(k.rt.st().stops.last().unwrap().1, crate::Stop::NotRunning);
    assert!(k.rt.st().cancelled.is_empty());
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "二\n\n一");
    assert!(k.db().queued(&sid).unwrap().is_empty());
    assert!(kinds(&k, &sid).contains(&"queued_sent_now".to_string()));
}

#[test]
fn send_now_on_an_idle_session_starts_the_turn() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二"]);
    // left idle with a queue (a restart, an item waiting on the user)
    k.rt.st().turning.clear();
    assert_eq!(k.core.queued_send_now(&sid, &q[1]).unwrap(), SendNow::Sent);
    assert!(k.rt.st().cancelled.is_empty());
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "二\n\n一");
}

#[test]
fn send_now_while_holding_for_a_release_waits_and_says_why() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二"]);
    k.core.tx().set_hold(crate::occupancy::Hold::Release, true);
    assert_eq!(
        k.core.queued_send_now(&sid, &q[1]).unwrap(),
        SendNow::Waiting("Hub 正在准备发布，发布后发送".into())
    );
    assert!(
        k.rt.st().cancelled.is_empty(),
        "the running turn is not interrupted"
    );
    let texts: Vec<_> = k
        .db()
        .queued(&sid)
        .unwrap()
        .into_iter()
        .map(|q| q.text)
        .collect();
    assert_eq!(texts, ["二", "一"], "it goes first once the hold ends");
}

#[test]
fn resumed_rows_are_conflicts_without_edit_removal_reordering_or_interrupt() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    queue(&k, &sid, &["ordinary first"]);
    k.db()
        .store()
        .queued()
        .add(&sid, "already delivered", "resumed:me", &[])
        .unwrap();
    let before = k.db().queued(&sid).unwrap();
    let resumed = before
        .iter()
        .find(|q| q.via.starts_with("resumed:"))
        .unwrap();
    let events = kinds(&k, &sid);
    let prompts = k.rt.prompts_of(&sid);
    assert!(is_conflict(
        k.core
            .queued_edit(&sid, &resumed.id, "replacement action")
            .unwrap_err()
    ));
    assert!(is_conflict(
        k.core.queued_remove(&sid, &resumed.id).unwrap_err()
    ));
    assert!(is_conflict(
        k.core.queued_send_now(&sid, &resumed.id).unwrap_err()
    ));
    let after = k.db().queued(&sid).unwrap();
    let before = before
        .into_iter()
        .map(|q| (q.id, q.text, q.via, q.created))
        .collect::<Vec<_>>();
    let after = after
        .into_iter()
        .map(|q| (q.id, q.text, q.via, q.created))
        .collect::<Vec<_>>();
    assert_eq!(after, before);
    assert_eq!(kinds(&k, &sid), events);
    assert_eq!(k.rt.prompts_of(&sid), prompts);
    assert!(
        k.rt.st().cancelled.is_empty(),
        "send-now must not interrupt an in-flight turn"
    );
    // Stopping recovery is an explicit task action, not a false claim that delivery was undone.
    let paused = k
        .core
        .item_action(&item, crate::ItemAction::Pause, "me")
        .unwrap();
    assert_eq!(paused.status, "pause");
    assert_eq!(k.rt.st().cancelled, vec![sid.clone()]);
    assert!(k.rt.st().stopped.is_empty(), "a pause is no user stop");
    assert!(
        k.db()
            .queued(&sid)
            .unwrap()
            .iter()
            .any(|q| q.via == "resumed:me"),
        "pause keeps delivery evidence for a later deliberate resume"
    );
}

fn texts(k: &Kit, sid: &str) -> Vec<String> {
    k.db()
        .queued(sid)
        .unwrap()
        .into_iter()
        .map(|q| q.text)
        .collect()
}

#[test]
fn send_now_on_an_uncertain_session_waits_and_does_not_give_the_turn_up() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二"]);
    k.rt.st().uncertain.insert(sid.clone());
    assert_eq!(
        k.core.queued_send_now(&sid, &q[1]).unwrap(),
        SendNow::Waiting("上一轮的结果还不确定，确认后再发送".into())
    );
    assert!(k.rt.st().cancelled.is_empty(), "no cancel, no abandon");
    assert_eq!(texts(&k, &sid), ["二", "一"], "it goes first once settled");
    assert!(!kinds(&k, &sid).contains(&"queued_sent_now".to_string()));
}

#[test]
fn send_now_during_persisted_maintenance_waits() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二"]);
    k.db().store().maintenance_set(true).unwrap();
    assert_eq!(
        k.core.queued_send_now(&sid, &q[1]).unwrap(),
        SendNow::Waiting("Hub 正在维护，维护结束后发送".into())
    );
    assert!(k.rt.st().cancelled.is_empty());
    assert_eq!(texts(&k, &sid), ["二", "一"]);
    assert!(!kinds(&k, &sid).contains(&"queued_sent_now".to_string()));
}

#[test]
fn send_now_waits_when_the_flush_leaves_the_message_queued() {
    let k = Kit::new();
    let (_, sid) = started(&k);
    let q = queue(&k, &sid, &["一", "二"]);
    k.rt.st().turning.clear();
    k.rt.st().flush_stuck = true;
    assert_eq!(
        k.core.queued_send_now(&sid, &q[1]).unwrap(),
        SendNow::Waiting("暂时发不出去，稍后自动发送".into())
    );
    assert_eq!(texts(&k, &sid), ["二", "一"]);
    assert!(
        !kinds(&k, &sid).contains(&"queued_sent_now".to_string()),
        "nothing was sent: no event says so"
    );
    // nothing took it and nothing went: the flush gets its turn later
    k.rt.st().flush_stuck = false;
    assert_eq!(k.core.queued_send_now(&sid, &q[1]).unwrap(), SendNow::Sent);
    assert_eq!(k.rt.prompts_of(&sid).last().unwrap(), "二\n\n一");
    assert!(kinds(&k, &sid).contains(&"queued_sent_now".to_string()));
}

/// Only the user's Stop is a stop (a backend may retire what holds the session then); the
/// lifecycle's cancels are cancels only.
#[test]
fn the_users_stop_is_a_stop_and_a_pause_only_a_cancel() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    assert!(k.core.cancel_turn(&sid).unwrap());
    assert_eq!(k.rt.st().stopped, std::slice::from_ref(&sid));
    k.core
        .item_action(&item, crate::ItemAction::Pause, "me")
        .unwrap();
    assert_eq!(k.rt.st().cancelled, vec![sid.clone(), sid.clone()]);
    assert_eq!(k.rt.st().stopped, std::slice::from_ref(&sid));
}

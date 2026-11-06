//! An idle session its runtime can no longer take is ended with why (B5: a legacy session that
//! cannot move to the Node), once, and only at an idle boundary.

use crate::CreateItem;
use crate::repo::WS;
use crate::testkit::Kit;

fn started(k: &Kit) -> String {
    let i = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g".into(),
            title: "标题".into(),
            ..Default::default()
        })
        .unwrap();
    k.db().sessions_of_item(&i.id).unwrap().pop().unwrap().id
}

fn retired(k: &Kit, sid: &str) -> Vec<String> {
    k.db()
        .events_after(sid, 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "system" && e.data["retired"] == true)
        .filter_map(|e| e.data["text"].as_str().map(String::from))
        .collect()
}

#[test]
fn an_idle_session_is_retired_once_with_why() {
    let k = Kit::new();
    let sid = started(&k);
    // its first turn runs: not now
    assert!(!k.core.retire_session(&sid, "不能再运行").unwrap());
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    // a message waits: not now either
    k.db()
        .store()
        .queued()
        .add_unless_waiting(&sid, "等着", "me", &[])
        .unwrap();
    assert!(!k.core.retire_session(&sid, "不能再运行").unwrap());
    k.db().store().queued().claim(&sid).unwrap();
    assert!(retired(&k, &sid).is_empty());

    assert!(k.core.retire_session(&sid, "不能再运行").unwrap());
    let s = k.db().session(&sid).unwrap().unwrap();
    assert_eq!(s.status, "ended");
    assert!(k.rt.st().released.contains(&sid), "its harness let go");
    assert_eq!(retired(&k, &sid), ["不能再运行"]);
    // ended: never again
    assert!(!k.core.retire_session(&sid, "不能再运行").unwrap());
    assert_eq!(retired(&k, &sid).len(), 1);
    assert!(!k.core.retire_session("no-such", "x").unwrap());
}

fn kinds(k: &Kit, sid: &str, kind: &str) -> usize {
    k.db()
        .events_after(sid, 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .count()
}

#[test]
fn a_retired_session_is_never_woken_again() {
    let k = Kit::new();
    let sid = started(&k);
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    assert!(k.core.retire_session(&sid, "不能再运行").unwrap());
    let before = k.rt.prompts_of(&sid).len();

    // a message straight to it is refused, the session left as it is
    let e = k.core.session_message(&sid, "还在吗", false, &[]);
    assert!(e.is_err(), "a retired session is not prompted again");
    assert_eq!(
        k.rt.prompts_of(&sid).len(),
        before,
        "its runtime never prompted"
    );
    assert_eq!(k.db().session(&sid).unwrap().unwrap().status, "ended");
    // and never retired a second time
    assert!(!k.core.retire_session(&sid, "不能再运行").unwrap());
    assert_eq!(retired(&k, &sid), ["不能再运行"]);
}

#[test]
fn queued_messages_of_a_session_whose_directory_is_gone_go_with_it() {
    let k = Kit::new();
    let sid = started(&k);
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    k.db()
        .store()
        .queued()
        .add_unless_waiting(&sid, "等着", "me", &[])
        .unwrap();
    // its directory is there: the flush will send it, not now
    assert!(!k.core.retire_session(&sid, "不能再运行").unwrap());

    // its directory is gone: nothing ever flushes the queue, it goes with the session
    k.db()
        .exec(
            "UPDATE sessions SET cwd = '/nonexistent/steve-retire' WHERE id = ?",
            &[&sid],
        )
        .unwrap();
    assert!(k.core.retire_session(&sid, "不能再运行").unwrap());
    assert!(k.db().queued(&sid).unwrap().is_empty());
    assert_eq!(
        kinds(&k, &sid, "queued_removed"),
        1,
        "the dropped message is said"
    );
    let notes = retired(&k, &sid);
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].starts_with("不能再运行") && notes[0].contains('1'),
        "{notes:?}"
    );
    assert_eq!(k.db().session(&sid).unwrap().unwrap().status, "ended");
}

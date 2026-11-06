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

//! B7: a session moved off the legacy runner to a Node at boot ([`crate::Core::moved_to_node`]).
//! What the old runner left of it is made right for the Node: its open tool permissions are gone,
//! messages it had delivered come back to be checked, and a turn the restart cut continues there
//! once, under the rules the old boot resume had.

use crate::orchestrator::{RESUME, RESUME_MAX_AGE_MS};
use crate::testkit::Kit;
use crate::util::now;
use serde_json::json;

fn intent(k: &Kit, sid: &str, age: i64) {
    k.db()
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO session_resume VALUES (?1, ?2)",
                rusqlite::params![sid, now() - age],
            )?;
            Ok(())
        })
        .unwrap();
}

fn queued(k: &Kit, sid: &str) -> Vec<(String, String)> {
    k.db()
        .queued(sid)
        .unwrap()
        .into_iter()
        .map(|q| (q.text, q.via))
        .collect()
}

fn system(k: &Kit, sid: &str) -> Vec<serde_json::Value> {
    k.db()
        .events_after(sid, 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "system")
        .map(|e| e.data)
        .collect()
}

#[test]
fn a_cut_turn_continues_on_the_node_once_with_what_it_had_delivered_to_check() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    // what the old runner's boot handed back: a message it had delivered, then a new one
    k.db().queue_add(&s, "发布一下", "resumed:me", &[]).unwrap();
    k.db().queue_add(&s, "补充：用中文", "me", &[]).unwrap();
    intent(&k, &s, 0);
    let card = k.decision(&i, Some(&s), "approve", "tool_call", &["a|允许|allow_once"]);

    assert!(k.core.moved_to_node(&s).unwrap());
    assert_eq!(
        queued(&k, &s),
        vec![
            (RESUME.to_string(), "steve".to_string()),
            (
                format!("{}\n\n发布一下", crate::RESUMED_NOTE),
                "me".to_string()
            ),
            ("补充：用中文".to_string(), "me".to_string()),
        ],
        "RESUME first; the delivered one to be checked, as an ordinary message"
    );
    assert_eq!(
        k.db().decision(&card).unwrap().unwrap().status,
        "expired",
        "the old harness's permission died with it"
    );
    let sys = system(&k, &s);
    assert!(sys.iter().any(|e| e["approval_expired"] == card));
    assert!(
        sys.iter().any(|e| e["resumed"] == true),
        "marked resumed: {sys:?}"
    );
    assert_eq!(k.db().session(&s).unwrap().unwrap().status, "idle");
    assert!(k.db().store().queued().resume_ids().unwrap().is_empty());
    assert!(
        k.timeline_texts(&i)
            .contains(&"Hub 重启打断了会话，已自动继续".to_string())
    );
    assert!(k.open_fails(&i).is_empty());
    assert!(
        k.rt.prompts_of(&s).is_empty(),
        "the Node sends it, not the move"
    );
    // asked again (a second boot): nothing more
    assert!(!k.core.moved_to_node(&s).unwrap());
    assert_eq!(queued(&k, &s).len(), 3);
}

#[test]
fn an_idle_session_moves_as_it_is() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.db().queue_add(&s, "排着的", "me", &[]).unwrap();
    assert!(!k.core.moved_to_node(&s).unwrap());
    assert_eq!(queued(&k, &s), vec![("排着的".into(), "me".into())]);
    assert!(system(&k, &s).is_empty());
}

#[test]
fn a_cut_turn_is_not_continued_where_the_old_resume_would_not_have_been() {
    let k = Kit::new();
    // paused: said, continued when the pause ends
    let p = k.item("pause", "执行", None);
    let ps = k.session(&p, "running");
    // finished: said
    let d = k.item("done", "收尾", None);
    let ds = k.session(&d, "running");
    // waiting on you: said, not poked
    let w = k.item("run", "执行", None);
    let ws = k.session(&w, "running");
    k.decision(&w, Some(&ws), "ask", "question", &["a", "b"]);
    // cut a day ago: a failure card
    let o = k.item("run", "执行", None);
    let os = k.session(&o, "idle");
    intent(&k, &os, RESUME_MAX_AGE_MS + 1);
    // continued like this a moment ago: a failure card
    let a = k.item("run", "执行", None);
    let again = k.session(&a, "running");
    k.db()
        .event_add(
            &again,
            "system",
            json!({"text": "自动继续", "resumed": true}),
        )
        .unwrap();
    for sid in [&ps, &ds, &ws, &os, &again] {
        assert!(!k.core.moved_to_node(sid).unwrap(), "{sid}");
        assert!(queued(&k, sid).is_empty(), "{sid}: nothing queued");
        assert_eq!(k.db().session(sid).unwrap().unwrap().status, "idle");
    }
    let said = |sid: &str, needle: &str| {
        system(&k, sid)
            .iter()
            .any(|e| e["text"].as_str().is_some_and(|t| t.contains(needle)))
    };
    assert!(said(&ps, "事项已暂停"));
    assert!(said(&ds, "事项已结束"));
    assert!(said(&ws, "事项在等你处理"));
    assert!(said(&os, "超过一天"));
    assert!(said(&again, "半小时内已经自动继续过一次"));
    assert!(k.open_fails(&p).is_empty() && k.open_fails(&d).is_empty());
    assert!(k.open_fails(&w).is_empty());
    assert_eq!(k.open_fails(&o).len(), 1);
    assert!(k.open_fails(&a)[0].contains("已经自动继续过一次"));
    assert!(k.db().store().queued().resume_ids().unwrap().is_empty());
}

/// A deploy cut a turn that only waited for a permission answer: it continues even though another
/// card waits on the item and it was continued a moment ago (a deploy is not a crash loop).
#[test]
fn a_turn_a_deploy_cut_continues_past_other_cards_and_the_half_hour_limit() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.decision(&i, Some(&s), "ask", "question", &["a", "b"]);
    k.db()
        .event_add(&s, "system", json!({"text": "自动继续", "resumed": true}))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    k.db()
        .event_add(
            &s,
            "system",
            json!({"text": "为了升级暂停了这一回合", "drain_cut": true}),
        )
        .unwrap();
    assert!(k.core.moved_to_node(&s).unwrap());
    assert_eq!(queued(&k, &s), vec![(RESUME.into(), "steve".into())]);
}

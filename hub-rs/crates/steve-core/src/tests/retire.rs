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
    k.db().store().queued().take(&sid).unwrap();
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

/// The first turn ends; the card its plain stop left is answered, so the item waits on nobody.
fn idle(k: &Kit, sid: &str) {
    k.rt.end_turn(&k.core, sid, "end_turn", None);
    let item = k.db().session(sid).unwrap().unwrap().item_id.unwrap();
    for d in k.db().decisions_of_item(&item).unwrap() {
        if d.status == "open" {
            k.db()
                .decision_resolve(&d.id, serde_json::json!({}), "answered", None)
                .unwrap();
        }
    }
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

#[test]
fn a_session_nothing_will_run_again_ends_with_its_queue() {
    let k = Kit::new();
    let sid = started(&k);
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    k.db()
        .store()
        .queued()
        .add_unless_waiting(&sid, "等着", "me", &[])
        .unwrap();
    // its directory is there, but nothing would ever flush the queue
    assert!(k.core.end_unmovable(&sid, "不能再运行").unwrap());
    assert!(k.db().queued(&sid).unwrap().is_empty());
    assert_eq!(kinds(&k, &sid, "queued_removed"), 1, "said");
    let last = retired(&k, &sid).pop().unwrap();
    assert!(
        last.starts_with("不能再运行") && last.contains('1'),
        "{last}"
    );
    assert!(!k.core.end_unmovable(&sid, "不能再运行").unwrap(), "once");
}

/// B7: a new session no backend takes is ended at once, said in it and on its item, and the start
/// fails with why; it never waits for a runner that is not there.
#[test]
fn a_new_session_no_backend_takes_is_refused_visibly() {
    let k = Kit::new();
    k.rt.st().refuse = Some("本机 Node 已关闭".into());
    let e = k.core.create_item(CreateItem {
        ws: WS.into(),
        goal: "g".into(),
        title: "标题".into(),
        ..Default::default()
    });
    let i = match e {
        Err(e) => {
            assert!(e.to_string().contains("本机 Node 已关闭"), "{e}");
            k.db()
                .items(WS)
                .unwrap()
                .into_iter()
                .find(|i| i.title == "标题")
                .expect("the item is there")
        }
        Ok(i) => i,
    };
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    assert_eq!(s.status, "ended");
    assert!(k.rt.prompts_of(&s.id).is_empty(), "never prompted");
    assert!(
        k.db()
            .events_after(&s.id, 0)
            .unwrap()
            .iter()
            .any(|e| e.kind == "system" && e.data["refused"] == true)
    );
    let fails = k.open_fails(&i.id);
    assert_eq!(fails.len(), 1, "one card: {fails:?}");
    assert!(fails[0].contains("本机 Node 已关闭"), "{fails:?}");
}

/// B7: a session that can never run again loses all of its queue, the messages the old runner
/// had delivered (`resumed:`, returned to the queue at boot) included, and the note counts them.
#[test]
fn a_session_nothing_will_run_again_drops_what_the_old_runner_had_delivered_too() {
    let k = Kit::new();
    let sid = started(&k);
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    k.db()
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "INSERT INTO queued(id,session_id,text,via,created) VALUES ('q-r',?1,'已送出','resumed:me',1)",
                [&sid],
            )?)
        })
        .unwrap();
    k.db()
        .store()
        .queued()
        .add_unless_waiting(&sid, "等着", "me", &[])
        .unwrap();
    assert_eq!(k.db().queued(&sid).unwrap().len(), 2);
    assert!(k.core.end_unmovable(&sid, "不能再运行").unwrap());
    assert!(
        k.db().queued(&sid).unwrap().is_empty(),
        "nothing left behind"
    );
    assert_eq!(kinds(&k, &sid, "queued_removed"), 2, "each one said");
    let last = retired(&k, &sid).pop().unwrap();
    assert!(
        last.starts_with("不能再运行") && last.contains('2'),
        "{last}"
    );
}

/// B7: a turn the restart cut in a session that can never run again is not left looking like
/// work in progress: it is said, and its item gets a failure card with why.
#[test]
fn a_cut_turn_nothing_will_run_again_gets_a_card() {
    for (status, intent) in [("running", false), ("starting", false), ("idle", true)] {
        let k = Kit::new();
        let sid = started(&k);
        idle(&k, &sid);
        let item = k.db().session(&sid).unwrap().unwrap().item_id.unwrap();
        // as the old Hub left it when its stop cut the turn
        k.db()
            .store()
            .with_conn(|c| {
                c.execute("UPDATE sessions SET status=?2 WHERE id=?1", [&sid, status])?;
                if intent {
                    c.execute(
                        "INSERT INTO session_resume(session_id,created) VALUES (?1,1)",
                        [&sid],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        assert!(
            k.core.end_unmovable(&sid, "不能再运行").unwrap(),
            "{status}"
        );
        assert_eq!(k.db().session(&sid).unwrap().unwrap().status, "ended");
        let cards: Vec<_> = k
            .db()
            .decisions_of_item(&item)
            .unwrap()
            .into_iter()
            .filter(|d| d.status == "open" && d.kind == "fail")
            .collect();
        assert_eq!(cards.len(), 1, "{status}: {cards:?}");
        assert!(
            cards[0].question.starts_with("Hub 重启时这个会话正在运行")
                && cards[0].question.contains("不能再运行"),
            "{}",
            cards[0].question
        );
        assert!(
            k.db()
                .store()
                .queued()
                .resume_created(&sid)
                .unwrap()
                .is_none(),
            "the intent is dropped"
        );
        assert_eq!(kinds(&k, &sid, "queued_removed"), 0);
    }
}

/// B7: a session moved to a Node after boot (the later pass) sends what it has queued at once,
/// the old runner's returned messages included; behind an upgrade's hold it waits for the lift.
#[test]
fn a_session_moved_after_boot_sends_its_queue() {
    let k = Kit::new();
    let sid = started(&k);
    idle(&k, &sid);
    k.db()
        .store()
        .queued()
        .add_unless_waiting(&sid, "等着", "me", &[])
        .unwrap();
    k.db().store().maintenance_set(true).unwrap();
    k.core.flush_moved(&sid).unwrap();
    assert_eq!(k.db().queued(&sid).unwrap().len(), 1, "held");
    assert!(!k.core.is_busy(&sid));
    k.db().store().maintenance_set(false).unwrap();
    k.core.flush_moved(&sid).unwrap();
    assert!(k.db().queued(&sid).unwrap().is_empty(), "sent");
    assert!(k.core.is_busy(&sid), "a turn runs it");
    // nothing queued: nothing happens
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    k.core.flush_moved(&sid).unwrap();
    assert!(!k.core.is_busy(&sid));
}

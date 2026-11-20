use crate::mcp_recovery::ATTACH_GRACE_MS;
use crate::repo::WS;
use crate::runtime::{Released, TurnEnd};
use crate::testkit::Kit;
use crate::{CreateItem, ItemAction};
use serde_json::json;

fn started(k: &Kit) -> (String, String) {
    let i = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "MCP recovery".into(),
            goal: "Finish the existing work".into(),
            ..Default::default()
        })
        .unwrap();
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    expect(k, &s.id);
    (i.id, s.id)
}

fn expect(k: &Kit, sid: &str) {
    k.db()
        .store()
        .sessions()
        .expect_mcp(sid, &format!("open-{sid}"))
        .unwrap();
    k.db()
        .event_add(
            sid,
            "user",
            json!({"text":"Keep the existing PR and finish it."}),
        )
        .unwrap();
}

fn latest(k: &Kit, item: &str) -> String {
    k.db().sessions_of_item(item).unwrap().pop().unwrap().id
}

#[test]
fn stopped_without_mcp_opens_a_new_session_instead_of_nagging_the_old_one() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    k.rt.say(
        &sid,
        "tools.mcp__steve__steve_deliver is undefined; PR already exists.",
    );
    let before = k.rt.prompts_of(&sid).len();
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    let ns = latest(&k, &item);
    assert_ne!(sid, ns);
    assert_eq!(k.rt.prompts_of(&sid).len(), before);
    assert_eq!(k.db().session(&sid).unwrap().unwrap().status, "ended");
    assert!(k.rt.prompts_of(&ns)[0].contains("PR already exists"));
    assert!(k.rt.prompts_of(&ns)[0].contains("先用 steve_update"));
    assert!(k.open_fails(&item).is_empty());
    k.core
        .on_turn_end(TurnEnd {
            session_id: sid,
            attempt: None,
            stop_reason: "end_turn".into(),
            error: None,
        })
        .unwrap();
    assert_eq!(
        latest(&k, &item),
        ns,
        "late turn end must not replace its successor"
    );
}

#[test]
fn watchdog_preserves_user_queue_and_files_and_waits_for_the_old_release() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    let f = k.file("evidence.txt", "text/plain", b"keep me");
    k.db()
        .queue_add(
            &sid,
        "You are working for Steve, the user's assistant, on item ST-1: Include this pending detail.",
            "me",
            std::slice::from_ref(&f),
        )
        .unwrap();
    k.rt.st()
        .release_answers
        .insert(sid.clone(), Released::Pending("release-old".into()));
    k.rt.st().starts_require_commit = true;
    k.core
        .tx()
        .check_stalls(crate::util::now() + ATTACH_GRACE_MS + 1)
        .unwrap();
    let ns = latest(&k, &item);
    assert_ne!(sid, ns);
    assert!(k.db().queued(&sid).unwrap().is_empty());
    let state = k.rt.st();
    assert!(state.cancelled.contains(&sid));
    assert!(
        state
            .created
            .contains(&(ns.clone(), Some("release-old".into())))
    );
    assert!(
        state
            .files
            .iter()
            .any(|(id, files)| id == &ns && files.iter().any(|x| x.id == f.id))
    );
    drop(state);
    assert!(k.rt.prompts_of(&ns)[0].contains("Include this pending detail"));
}

#[test]
fn preparing_a_workspace_and_a_healthy_or_slow_attachment_are_not_restarted() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    let sessions = k.db().store().sessions();
    sessions.expect_mcp(&sid, "still-preparing").unwrap();
    assert!(
        !k.core
            .tx()
            .mcp_overdue(&sid, crate::util::now() + 10 * ATTACH_GRACE_MS)
            .unwrap()
    );
    k.db()
        .event_add(&sid, "user", json!({"text":"first turn"}))
        .unwrap();
    assert!(
        !k.core
            .tx()
            .mcp_overdue(&sid, crate::util::now() + ATTACH_GRACE_MS - 100)
            .unwrap()
    );
    sessions.confirm_mcp(&sid, "still-preparing").unwrap();
    k.core
        .tx()
        .check_stalls(crate::util::now() + 10 * ATTACH_GRACE_MS)
        .unwrap();
    assert_eq!(latest(&k, &item), sid);
}

#[test]
fn consecutive_missing_attachments_stop_once_survive_restart_and_allow_manual_new_session() {
    let k = Kit::new();
    let (item, mut sid) = started(&k);
    for _ in 0..2 {
        k.rt.end_turn(&k.core, &sid, "end_turn", None);
        let next = latest(&k, &item);
        assert_ne!(sid, next);
        sid = next;
        expect(&k, &sid);
    }
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    assert_eq!(latest(&k, &item), sid);
    let card = k
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.subject_type == steve_store::SESSION_MCP_FAILURE)
        .unwrap();
    assert_eq!(k.status(&item).0, "wait");
    let reopened = k.reopened();
    for _ in 0..3 {
        reopened.check_stalls().unwrap();
    }
    assert_eq!(
        k.db()
            .decisions_of_item(&item)
            .unwrap()
            .iter()
            .filter(|d| d.status == "open")
            .count(),
        1
    );
    assert!(
        reopened
            .resolve(&card.id, "retry", "", steve_modules::outbound::YOLO_ACTOR)
            .is_err()
    );
    assert!(reopened.dismiss_decision(&card.id, "me").is_err());
    let result = reopened.resolve(&card.id, "retry", "", "me").unwrap();
    assert_eq!(result.status, "resolved");
    assert_ne!(latest(&k, &item), sid);
}

#[test]
fn a_confirmed_attachment_resets_the_consecutive_recreation_budget() {
    let k = Kit::new();
    let (item, sid) = started(&k);
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    let ns = latest(&k, &item);
    expect(&k, &ns);
    k.db()
        .store()
        .sessions()
        .confirm_mcp_call(&ns, &format!("open-{ns}"))
        .unwrap();
    // The adapter may attach before the inherited counter is recorded by the creator.
    k.db()
        .event_add(&ns, "system", json!({"mcp_recovery_attempt":2}))
        .unwrap();
    expect(&k, &ns); // a later open in the same conversation loses attachment
    k.rt.end_turn(&k.core, &ns, "end_turn", None);
    let next = latest(&k, &item);
    assert_ne!(next, ns);
    assert!(k.timeline_texts(&item).last().unwrap().contains("开始会话"));
    let attempts = k.db().events_after(&next, 0).unwrap();
    assert!(attempts.iter().any(|e| e.data["mcp_recovery_attempt"] == 1));
}

#[test]
fn pause_stop_and_an_uncertain_or_offline_node_keep_the_session() {
    for mode in ["pause", "stop", "unknown", "offline"] {
        let k = Kit::new();
        let (item, sid) = started(&k);
        match mode {
            "pause" => {
                k.core.item_action(&item, ItemAction::Pause, "me").unwrap();
            }
            "stop" => {
                k.rt.end_turn(&k.core, &sid, "cancelled", None);
            }
            "unknown" => {
                k.rt.st().uncertain.insert(sid.clone());
            }
            _ => {
                k.rt.st().node_offline = Some("offline".into());
            }
        }
        k.core
            .tx()
            .check_stalls(crate::util::now() + ATTACH_GRACE_MS + 1)
            .unwrap();
        assert_eq!(latest(&k, &item), sid, "{mode}");
    }
}

#[test]
fn attachment_proof_is_per_open_and_cannot_leak_across_sessions_or_arrive_late() {
    let k = Kit::new();
    let (_, a) = started(&k);
    let (_, b) = started(&k);
    let sessions = k.db().store().sessions();
    sessions.confirm_mcp(&b, &format!("open-{a}")).unwrap();
    assert!(!sessions.mcp_attachment(&b).unwrap().unwrap().ready);
    sessions.expect_mcp(&a, "new-open").unwrap();
    sessions.confirm_mcp(&a, &format!("open-{a}")).unwrap();
    assert!(!sessions.mcp_attachment(&a).unwrap().unwrap().ready);
    sessions.confirm_mcp(&a, "new-open").unwrap();
    let proof = sessions.mcp_attachment(&a).unwrap().unwrap();
    sessions.confirm_mcp(&a, "new-open").unwrap();
    assert_eq!(sessions.mcp_attachment(&a).unwrap().unwrap().seq, proof.seq);
    sessions.expect_mcp(&a, "configure-refused").unwrap();
    sessions.mcp_open_refused(&a, "old-configure").unwrap();
    assert!(!sessions.mcp_attachment(&a).unwrap().unwrap().ready);
    sessions.mcp_open_refused(&a, "configure-refused").unwrap();
    let restored = sessions.mcp_attachment(&a).unwrap().unwrap();
    assert!(restored.ready);
    assert_eq!(restored.open, "new-open");
}

#[test]
fn a_catalog_without_a_usable_attempt_does_not_reset_recovery_budget() {
    let k = Kit::new();
    let (item, mut sid) = started(&k);
    for attempt in 0..3 {
        let sessions = k.db().store().sessions();
        sessions.confirm_mcp(&sid, &format!("open-{sid}")).unwrap();
        sessions.mcp_attempt_missing(&sid).unwrap();
        sessions.confirm_mcp(&sid, &format!("open-{sid}")).unwrap();
        assert!(
            !sessions.mcp_attachment(&sid).unwrap().unwrap().ready,
            "listing again cannot clear an execution fault"
        );
        k.rt.end_turn(&k.core, &sid, "end_turn", None);
        let ns = latest(&k, &item);
        if attempt < 2 {
            assert_ne!(ns, sid);
            sid = ns;
            expect(&k, &sid);
        } else {
            assert_eq!(ns, sid);
        }
    }
    assert_eq!(k.open_fails(&item).len(), 1);
}

#[test]
fn a_next_open_expectation_does_not_replace_the_turn_that_just_ended() {
    let k = Kit::with(|cfg| cfg.takeover_continues = 0);
    let (item, sid) = started(&k);
    k.db()
        .store()
        .sessions()
        .confirm_mcp(&sid, &format!("open-{sid}"))
        .unwrap();
    // A mode switch at the idle boundary queues configure before Core sees the old turn end.
    k.db()
        .store()
        .sessions()
        .expect_mcp(&sid, "next-configure")
        .unwrap();
    k.rt.end_turn(&k.core, &sid, "end_turn", None);
    assert_eq!(latest(&k, &item), sid);
    assert_ne!(k.db().session(&sid).unwrap().unwrap().status, "ended");
}

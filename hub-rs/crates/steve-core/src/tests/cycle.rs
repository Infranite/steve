//! The closing gate of cycle.rs on its own: the frozen result is the worktree's content, every effect of
//! the cycle in flight or of unknown result holds any closing, the operations an accepted result needs
//! are in place before anything can close over it (also after a crash), and the command API.

use super::lifecycle::{delivered_in_git, release_decisions};
use crate::cycle::{self, OP_RELEASE};
use crate::error::CoreError;
use crate::git::tests::{g, repo};
use crate::testkit::Kit;
use serde_json::json;

fn ops(k: &Kit, item: &str) -> Vec<(String, bool, i64, String)> {
    let i = k.db().item(item).unwrap().unwrap();
    k.db()
        .store()
        .items()
        .operations(item, i.cycle)
        .unwrap()
        .into_iter()
        .map(|o| (o.kind, o.required, o.revision, o.status))
        .collect()
}

fn conflict(e: CoreError) -> String {
    match e {
        CoreError::Status(409, m) => m,
        e => panic!("expected a 409, got {e}"),
    }
}

fn done_events(k: &Kit, item: &str) -> usize {
    k.db()
        .store()
        .reader(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM outbox_events WHERE topic = 'work.item.done' AND payload LIKE ?",
                [format!("%{item}%")],
                |r| r.get::<_, i64>(0),
            )
            .map_err(steve_store::StoreError::from)
        })
        .unwrap() as usize
}

#[test]
fn the_fingerprint_follows_file_content_not_just_git_status() {
    let k = Kit::new();
    let r = repo(k.home());
    let cwd = r.to_string_lossy().to_string();
    std::fs::write(r.join("a.txt"), "one\n").unwrap();
    std::fs::write(r.join("new.txt"), "x\n").unwrap();
    let status = g(&r, &["status", "--porcelain"]);
    let before = cycle::fingerprint(Some(&cwd)).unwrap();
    // same status lines (` M a.txt`, `?? new.txt`), other bytes
    std::fs::write(r.join("a.txt"), "two\n").unwrap();
    assert_eq!(g(&r, &["status", "--porcelain"]), status);
    let tracked = cycle::fingerprint(Some(&cwd)).unwrap();
    assert_ne!(before, tracked, "a tracked file changed under the same `M`");
    std::fs::write(r.join("new.txt"), "y\n").unwrap();
    assert_eq!(g(&r, &["status", "--porcelain"]), status);
    let untracked = cycle::fingerprint(Some(&cwd)).unwrap();
    assert_ne!(
        tracked, untracked,
        "an untracked file changed under the same `??`"
    );
    // the same content again is the same fingerprint, and the item's own index is untouched
    assert_eq!(cycle::fingerprint(Some(&cwd)).unwrap(), untracked);
    assert_eq!(g(&r, &["status", "--porcelain"]), status);
    assert_eq!(cycle::fingerprint_head(&before).len(), 40);
}

#[test]
fn an_edit_that_keeps_the_status_still_voids_the_frozen_result() {
    let k = Kit::new();
    let (x, wt) = delivered_in_git(&k, true);
    std::fs::write(wt.join("x.txt"), "first\n").unwrap();
    // delivered again on the dirty worktree (as `steve_deliver` does it): this is the frozen result
    k.core.tx().lc_invalidate(&x.id, "有新的交付").unwrap();
    let d = k.decision(
        &x.id,
        Some(&x.session),
        "accept",
        "item",
        &["pass", "rework"],
    );
    k.core
        .tx()
        .lc_request_complete(
            &x.id,
            "dev",
            cycle::Policy::Human,
            false,
            "交付",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    let rev = k.db().item(&x.id).unwrap().unwrap().revision;
    assert!(
        cycle::closing_of(&k.db().item(&x.id).unwrap().unwrap())
            .unwrap()
            .fingerprint
            .is_some()
    );
    std::fs::write(wt.join("x.txt"), "second\n").unwrap();
    k.core.resolve(&d, "pass", "", "me").unwrap();
    // the agent was asked to publish: its turn holds the gate, its end finds the change
    assert_eq!(k.phase(&x.id), "closing");
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(
        i.phase, "active",
        "the accepted result is not what is in the worktree"
    );
    assert_eq!(i.revision, rev);
    assert!(i.now.starts_with("收尾作废"), "{}", i.now);
    assert_eq!(done_events(&k, &x.id), 0);
}

#[test]
fn an_unknown_effect_of_an_older_revision_holds_the_close_until_you_check_it() {
    let k = Kit::new();
    let x = k.delivered(false);
    let i = k.db().item(&x.id).unwrap().unwrap();
    // a release attempt of an earlier delivery whose script died without a result
    let mut old = i.clone();
    old.revision -= 1;
    k.core
        .tx()
        .lc_op_add(
            &old,
            OP_RELEASE,
            Some("attempt:test"),
            false,
            "unknown",
            "发布",
            json!({}),
        )
        .unwrap();
    k.core
        .tx()
        .release_unknown(&x.id, "发布脚本没有结果")
        .unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(
        k.phase(&x.id),
        "closing",
        "an unknown effect is never closed over"
    );
    let v = k.core.lifecycle_view(&x.id).unwrap();
    assert_eq!(v["closing"]["waiting"], json!(["operation"]));
    assert_eq!(k.lifecycle_tick_closed(), 0);
    // you checked: it is live
    let card = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .unwrap();
    k.core.resolve(&card.id, "release_live", "", "me").unwrap();
    assert_eq!(k.phase(&x.id), "closed");
    assert!(
        ops(&k, &x.id).iter().all(|o| o.3 == "succeeded"),
        "{:?}",
        ops(&k, &x.id)
    );
}

#[test]
fn a_cancel_waits_for_an_unknown_effect_and_keeps_the_card_that_asks() {
    let k = Kit::new();
    let x = k.delivered(false);
    let i = k.db().item(&x.id).unwrap().unwrap();
    k.core
        .tx()
        .lc_op_add(
            &i,
            OP_RELEASE,
            Some("attempt:t"),
            false,
            "unknown",
            "发布",
            json!({}),
        )
        .unwrap();
    k.core
        .tx()
        .release_unknown(&x.id, "发布脚本没有结果")
        .unwrap();
    k.core
        .item_action(&x.id, crate::ItemAction::Drop, "me")
        .unwrap();
    assert_eq!(k.phase(&x.id), "closing");
    let open: Vec<_> = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .filter(|d| d.status == "open")
        .map(|d| d.kind)
        .collect();
    assert_eq!(open, ["fail"], "the acceptance goes, the check stays");
    let card = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open")
        .unwrap();
    // not live after all: settled as failed, the cancel commits, nobody is asked to publish
    k.core.resolve(&card.id, "release_retry", "", "me").unwrap();
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(
        (i.phase.as_str(), i.outcome.as_deref()),
        ("closed", Some("cancelled"))
    );
    assert_eq!(ops(&k, &x.id)[0].3, "failed");
    assert!(
        k.rt.prompts_of(&x.session)
            .iter()
            .all(|p| !p.contains("steve_publish"))
    );
}

#[test]
fn a_running_operation_of_an_older_revision_holds_a_failure_ruling() {
    let k = Kit::new();
    let x = k.delivered(false);
    let i = k.db().item(&x.id).unwrap().unwrap();
    let mut old = i.clone();
    old.revision -= 1;
    let (op, _) = k
        .core
        .tx()
        .lc_op_add(
            &old,
            "hooks.accepted",
            Some("h:old"),
            true,
            "running",
            "收尾模块确认",
            json!({}),
        )
        .unwrap();
    k.core
        .lifecycle_command(
            &x.id,
            "fail",
            i.cycle,
            i.revision,
            Some("做不成"),
            None,
            "me",
        )
        .unwrap();
    assert_eq!(k.phase(&x.id), "closing");
    k.core.module_done(&x.id, "模块处理完了").unwrap();
    assert_eq!(
        k.phase(&x.id),
        "closing",
        "module_done settles only the current revision's op"
    );
    k.core
        .tx()
        .lc_op_set(&op.id, "succeeded", json!({}))
        .unwrap();
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(
        (i.phase.as_str(), i.outcome.as_deref(), i.status.as_str()),
        ("closed", Some("failed"), "fail")
    );
}

/// A crash right after the accepted closing was written and before its required release was recorded.
#[test]
fn after_a_crash_before_the_required_release_was_recorded_the_gate_records_it_first() {
    let k = Kit::new();
    let (x, _) = delivered_in_git(&k, true);
    let i = k.db().item(&x.id).unwrap().unwrap();
    let mut c = cycle::closing_of(&i).unwrap();
    c.accepted = true;
    k.db()
        .exec(
            "UPDATE items SET closing = ? WHERE id = ?",
            &[
                &serde_json::to_string(&c).unwrap() as &dyn rusqlite::ToSql,
                &x.id,
            ],
        )
        .unwrap();
    assert!(ops(&k, &x.id).is_empty(), "the crash left no operation");
    k.core.recover().unwrap();
    assert_eq!(k.core.lifecycle_tick().unwrap(), 0);
    assert_eq!(k.phase(&x.id), "closing");
    assert_eq!(
        ops(&k, &x.id),
        [(
            "release".to_string(),
            true,
            i.revision,
            "pending".to_string()
        )]
    );
    assert_eq!(k.status(&x.id).2, "等待发布");
    // ticking again asks nobody twice
    k.core.lifecycle_tick().unwrap();
    let asks =
        k.rt.prompts_of(&x.session)
            .iter()
            .filter(|p| p.starts_with("验收通过。"))
            .count();
    assert_eq!(asks, 1);
    assert_eq!(release_decisions(&k, &x.id), 0);
    assert_eq!(done_events(&k, &x.id), 0);
}

#[test]
fn commands_check_the_version_and_the_phase() {
    let k = Kit::new();
    let x = k.delivered(false);
    let i = k.db().item(&x.id).unwrap().unwrap();
    // a stale revision is a conflict and changes nothing
    let e = k
        .core
        .lifecycle_command(&x.id, "accept", i.cycle, i.revision - 1, None, None, "me")
        .unwrap_err();
    conflict(e);
    assert_eq!(k.phase(&x.id), "closing");
    // fail needs a reason
    assert!(
        k.core
            .lifecycle_command(&x.id, "fail", i.cycle, i.revision, Some(" "), None, "me")
            .is_err()
    );
    assert_eq!(k.phase(&x.id), "closing");
    // new_cycle needs a closed item
    conflict(
        k.core
            .lifecycle_command(&x.id, "new_cycle", i.cycle, i.revision, None, None, "me")
            .unwrap_err(),
    );
    let done = k
        .core
        .lifecycle_command(&x.id, "accept", i.cycle, i.revision, None, None, "me")
        .unwrap();
    assert_eq!(
        (done.phase.as_str(), done.acceptance.as_deref()),
        ("closed", Some("human"))
    );
    assert_eq!(
        k.db().decision(&x.decision).unwrap().unwrap().status,
        "resolved",
        "accepting through the command answers the card"
    );
    // closed: accept / cancel / fail are conflicts, sends are refused
    for c in ["accept", "cancel"] {
        conflict(
            k.core
                .lifecycle_command(&x.id, c, done.cycle, done.revision, None, None, "me")
                .unwrap_err(),
        );
    }
    conflict(
        k.core
            .lifecycle_command(
                &x.id,
                "fail",
                done.cycle,
                done.revision,
                Some("晚了"),
                None,
                "me",
            )
            .unwrap_err(),
    );
    let s = k.db().session(&x.session).unwrap().unwrap();
    assert_eq!(
        conflict(
            k.core
                .session_message(&s.id, "还有一件", false, &[])
                .unwrap_err()
        ),
        cycle::closed_msg()
    );
    // the new cycle starts at revision 0; the old cycle stays as it closed
    let n = k
        .core
        .lifecycle_command(
            &x.id,
            "new_cycle",
            done.cycle,
            done.revision,
            None,
            None,
            "me",
        )
        .unwrap();
    assert_eq!((n.phase.as_str(), n.cycle, n.revision), ("ready", 2, 0));
    let cycles = k.db().store().items().cycles(&x.id).unwrap();
    assert_eq!(cycles.len(), 1);
    assert_eq!(
        (cycles[0].cycle, cycles[0].outcome.as_str()),
        (1, "completed")
    );
    assert_eq!(done_events(&k, &x.id), 1);
}

#[test]
fn a_busy_turn_holds_the_close_and_its_end_commits_it_once() {
    let k = Kit::new();
    let x = k.delivered(false);
    // the agent is still at work (e.g. waiting for a permission) when you accept
    k.core
        .session_message(&x.session, "顺便看看日志", false, &[])
        .unwrap();
    assert_eq!(
        k.phase(&x.id),
        "active",
        "new input voids the frozen result"
    );
    let i = k.db().item(&x.id).unwrap().unwrap();
    let d = k.decision(
        &x.id,
        Some(&x.session),
        "accept",
        "item",
        &["pass", "rework"],
    );
    k.core
        .tx()
        .lc_request_complete(
            &x.id,
            "dev",
            cycle::Policy::Human,
            false,
            "交付",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    k.core.resolve(&d, "pass", "", "me").unwrap();
    let v = k.core.lifecycle_view(&x.id).unwrap();
    assert_eq!(v["phase"], "closing");
    assert_eq!(v["closing"]["waiting"], json!(["turn"]));
    assert_eq!(v["revision"], i.revision + 1);
    assert_eq!(k.core.lifecycle_tick().unwrap(), 0);
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.phase(&x.id), "closed");
    k.core.lifecycle_tick().unwrap();
    assert_eq!(done_events(&k, &x.id), 1);
}

impl Kit {
    fn lifecycle_tick_closed(&self) -> usize {
        self.core.lifecycle_tick().unwrap()
    }
}

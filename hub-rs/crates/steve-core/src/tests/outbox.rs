//! Closing on the durable outbox: the state change, its timeline note, the expired decisions and
//! `work.item.done` commit together or not at all; a cycle closes once (a repeated finish is refused, a
//! new cycle closes again with its own fact); `work.tidy` runs after the commit; acknowledged events
//! are not replayed. A crash before acknowledgement may repeat the idempotent external effect.

use serde_json::Value;

use crate::repo::WS;
use crate::testkit::Kit;

fn events(k: &Kit, item: &str) -> Vec<(String, Value)> {
    k.db()
        .store()
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT id, payload FROM outbox_events WHERE topic = 'work.item.done' ORDER BY seq",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .map(|(id, p)| (id, serde_json::from_str::<Value>(&p).unwrap()))
                .filter(|(_, p)| p["item_id"] == item)
                .collect())
        })
        .unwrap()
}

fn status(k: &Kit, item: &str) -> String {
    k.db().item(item).unwrap().unwrap().status
}

#[test]
fn finishing_emits_one_fact_with_the_item_as_finished() {
    let k = Kit::new();
    let id = k.item("run", "执行", None);
    k.finish(&id).unwrap();
    assert_eq!(status(&k, &id), "done");
    let evs = events(&k, &id);
    assert_eq!(evs.len(), 1);
    let p = &evs[0].1;
    assert_eq!(
        (p["status"].as_str(), p["title"].as_str()),
        (Some("done"), Some("t"))
    );
    let ws: String = k
        .db()
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT workspace_id FROM outbox_events WHERE id = ?",
                [&evs[0].0],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(ws, WS);
}

#[test]
fn a_cycle_closes_once_and_a_new_cycle_emits_its_own_fact() {
    let k = Kit::new();
    let id = k.item("run", "执行", None);
    k.finish(&id).unwrap();
    // e.g. the caller retries after a failure that came after the commit: refused, nothing emitted
    assert!(k.finish(&id).is_err());
    assert_eq!(
        events(&k, &id).len(),
        1,
        "no second fact for the same cycle"
    );
    // a legacy status write cannot reopen it any more
    assert!(
        k.db()
            .item_patch(&id, crate::patch![status: "run"])
            .is_err()
    );
    // only an explicit new cycle does; its close is a new fact, keyed by its cycle
    k.core
        .lifecycle_command(&id, "new_cycle", 1, 1, None, None, "me")
        .unwrap();
    k.finish(&id).unwrap();
    let evs = events(&k, &id);
    assert_eq!(evs.len(), 2, "a new cycle's close is a new fact");
    assert_eq!(
        (evs[0].1["cycle"].as_i64(), evs[1].1["cycle"].as_i64()),
        (Some(1), Some(2))
    );
}

#[test]
fn a_failing_finish_leaves_nothing_behind() {
    let k = Kit::new();
    let id = k.item("run", "执行", None);
    // a step left open (so finishing writes a timeline note) and an open decision (expired by it)
    k.db()
        .item_patch(
            &id,
            crate::patch![plan: serde_json::json!([{"step": "写测试", "done": false}])],
        )
        .unwrap();
    let d = k.decision(&id, None, "ask", "item", &["yes", "no"]);
    let before = k.timeline_texts(&id).len();
    // fault injection: the outbox insert fails, after the patch, note and expiry ran in the same transaction
    k.db()
        .store()
        .with_conn(|c| {
            Ok(c.execute_batch("ALTER TABLE outbox_events RENAME TO outbox_events_gone")?)
        })
        .unwrap();
    assert!(k.finish(&id).is_err());
    assert_eq!(status(&k, &id), "run", "the status change rolled back");
    assert_eq!(k.phase(&id), "active", "the lifecycle rolled back");
    // your own “标记完成” line stays (it happened); nothing of the close was written
    assert_eq!(k.timeline_texts(&id).len(), before + 1);
    assert!(
        !k.timeline_texts(&id)
            .iter()
            .any(|t| t.contains("还有 1 步没做")),
        "the note rolled back"
    );
    let open: String = k
        .db()
        .store()
        .with_conn(|c| {
            Ok(
                c.query_row("SELECT status FROM decisions WHERE id = ?", [&d], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(open, "open", "the expiry rolled back");
    // repaired: the finish goes through with everything
    k.db()
        .store()
        .with_conn(|c| {
            Ok(c.execute_batch("ALTER TABLE outbox_events_gone RENAME TO outbox_events")?)
        })
        .unwrap();
    k.finish(&id).unwrap();
    assert_eq!(status(&k, &id), "done");
    assert!(
        k.timeline_texts(&id)
            .iter()
            .any(|t| t.contains("还有 1 步没做"))
    );
    assert_eq!(events(&k, &id).len(), 1);
}

#[test]
fn work_tidy_takes_each_event_once() {
    let k = Kit::new();
    let id = k.item("run", "执行", None);
    k.finish(&id).unwrap();
    let p = k.pump_outbox();
    assert_eq!((p.done, p.retried, p.dead), (1, 0, 0));
    assert_eq!(k.pump_outbox().done, 0, "received: never again");
    let outcome: Option<String> = k
        .db()
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT outcome FROM outbox_receipts WHERE consumer_id = 'work.tidy'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(outcome.is_some(), "the receipt records the delivery");
}

#[test]
fn work_tidy_retries_while_a_session_is_busy() {
    use crate::git::tests::{branch_with, g, repo};
    let k = Kit::new();
    let r = repo(k.home());
    let gp = k
        .db()
        .project_create(
            WS,
            "G",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            None,
        )
        .unwrap();
    let id = k.item("run", "执行", None);
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &id, "x.txt", "x\n");
    g(&r, &["merge", "-q", "--ff-only", &name]);
    k.db()
        .item_patch(&id, crate::patch![project_id: gp.id.as_str(), cwd: wt.to_string_lossy().to_string(), branch: name.as_str()])
        .unwrap();
    let s = k.session(&id, "idle");
    k.rt.st().turning.insert(s.clone());
    k.finish(&id).unwrap();
    // the close itself waits for the turn: no fact, nothing to tidy yet
    assert_eq!(k.phase(&id), "closing");
    assert_eq!(k.pump_outbox().done, 0);
    assert!(wt.exists(), "nothing touched while busy");
    k.rt.st().turning.remove(&s);
    k.core.lifecycle_tick().unwrap();
    assert_eq!(k.phase(&id), "closed", "closed once the turn ended");
    let p = k.pump_outbox();
    assert_eq!(p.done, 1);
    assert!(!wt.exists(), "given back once the session is idle");
}

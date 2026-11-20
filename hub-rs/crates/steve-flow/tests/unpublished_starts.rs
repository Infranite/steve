//! The unpublished R4c/d/e shapes are deleted, never converted or loaded as current state.
use rusqlite::params;
use serde_json::{Value, json};
use steve_flow::{runs, testkit::FlowEnv};
use steve_modules::{ApiResult, testkit::WS};

const NOW: i64 = 1_795_180_000_000;
fn old(e: &FlowEnv, context: &Value) -> String {
    e.env
        .store()
        .transaction(|tx| {
            runs::insert(
                tx,
                &runs::NewRun {
                    ws: WS,
                    flow_id: "flow_cleanup",
                    flow_version: 1,
                    trigger_kind: "manual",
                    status: "failed",
                    outcome_label: "bad graph",
                    error: Some("bad graph"),
                    cursor: None,
                    context,
                    now: NOW,
                },
            )
        })
        .unwrap()
}
fn context() -> Value {
    json!({"at":NOW,"source":"digest.bot","trigger":"manual"})
}
fn clean(e: &FlowEnv) -> usize {
    e.env
        .store()
        .transaction(runs::cleanup_unpublished_digest_starts)
        .unwrap()
}
fn get(e: &FlowEnv, id: &str) -> Option<runs::RunRow> {
    e.env.store().reader(|c| runs::get(c, id)).unwrap()
}

#[test]
fn three_unpublished_shapes_are_removed_in_bounded_idempotent_batches() {
    let e = FlowEnv::new();
    let mut ids = Vec::new();
    for n in 0..runs::UNPUBLISHED_START_BATCH + 3 {
        let mut ctx = context();
        if n % 3 > 0 {
            ctx["start_failures"] = json!(2);
        }
        if n % 3 > 1 {
            ctx["first_at"] = json!(NOW - 60_000);
        }
        ids.push(old(&e, &ctx));
    }
    assert_eq!(clean(&e), runs::UNPUBLISHED_START_BATCH);
    assert_eq!(ids.iter().filter(|id| get(&e, id).is_some()).count(), 3);
    assert_eq!(clean(&e), 3);
    assert_eq!(clean(&e), 0);
    assert!(ids.iter().all(|id| get(&e, id).is_none()));
}

#[test]
fn ordinary_or_unprovable_records_are_never_deleted() {
    let e = FlowEnv::new();
    let mut ids = Vec::new();
    for (key, value) in [
        ("source", json!("another.source")),
        ("trigger", json!("time")),
        ("at", json!(NOW + 1)),
        ("at", json!(NOW.to_string())),
        ("start_failures", json!(0)),
        ("start_failures", json!(-1)),
        ("start_failures", json!(1.5)),
        ("start_failures", json!("2")),
        ("start_failures", Value::Null),
        ("first_at", json!(NOW)),
        ("unknown", json!(true)),
        ("startup", json!({"format":2})),
    ] {
        let mut ctx = context();
        ctx[key] = value;
        ids.push(old(&e, &ctx));
    }
    let mut ctx = context();
    ctx["start_failures"] = json!(2);
    ctx["first_at"] = json!("yesterday");
    ids.push(old(&e, &ctx));
    let mut ctx = context();
    ctx.as_object_mut().unwrap().remove("at");
    ids.push(old(&e, &ctx));
    for change in [
        "trigger_kind='time'",
        "status='done'",
        "error=NULL",
        "outcome_label='different'",
        "cursor='node'",
        "event_id='event'",
        "dry_run=1",
        "parent_run='parent'",
        "depth=1",
        "attempts=1",
        "cost=0.25",
        "retry_at=1",
        "outcome='failed'",
        "ended=NULL",
        "ended=started+1",
        "work_item_id='item'",
        "context='{'",
        "context='[]'",
        r#"context='{"at":1795180000000,"source":"digest.bot","trigger":"manual","trigger":"manual"}'"#,
    ] {
        let id = old(&e, &context());
        e.env
            .store()
            .with_conn(|c| {
                c.execute(&format!("UPDATE flow_runs SET {change} WHERE id=?"), [&id])?;
                Ok(())
            })
            .unwrap();
        ids.push(id);
    }
    let before: Vec<_> = ids.iter().map(|id| get(&e, id).unwrap()).collect();
    assert_eq!(clean(&e), 0);
    assert_eq!(
        ids.iter()
            .map(|id| get(&e, id).unwrap())
            .collect::<Vec<_>>(),
        before
    );
}

fn dependents(e: &FlowEnv, id: &str) -> String {
    let child = old(e, &json!({"at":NOW})); // an independent execution history row
    e.env.store().transaction(|tx| -> ApiResult<()> {
        tx.execute("INSERT INTO flow_run_steps (run_id,seq,workspace_id,node_id,status,started) VALUES (?,1,?,'trigger','failed',?)",params![id,WS,NOW])?;
        tx.execute("INSERT INTO flow_waits (run_id,node_id,workspace_id,mode,created) VALUES (?,'wait',?,'delay',?)",params![id,WS,NOW])?;
        tx.execute("UPDATE flow_runs SET parent_run=?1 WHERE id=?2",params![id,child])?;
        tx.execute("INSERT INTO items (id,workspace_id,no,title,status,stage,created,updated) VALUES ('ST-cleanup',?,9901,'keep this item','wait','',?,?)",params![WS,NOW,NOW])?;
        for (subject_type,subject_id,object_type,object_id) in [
            ("item","ST-cleanup","flow_run",id), ("flow_run",id,"item","ST-cleanup"),
            ("item","ST-cleanup","item","ST-cleanup"),
        ] {
            tx.execute("INSERT INTO links (workspace_id,subject_type,subject_id,predicate,object_type,object_id,source,created) VALUES (?,?,?,'from',?,?,'fixture',?)",params![WS,subject_type,subject_id,object_type,object_id,NOW])?;
        }
        Ok(())
    }).unwrap();
    child
}
fn counts(e: &FlowEnv, id: &str) -> (i64, i64, i64, i64) {
    e.env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>((
                c.query_row(
                    "SELECT count(*) FROM flow_run_steps WHERE run_id=?",
                    [id],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT count(*) FROM flow_waits WHERE run_id=?",
                    [id],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT count(*) FROM links WHERE source='fixture'",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT count(*) FROM items WHERE id='ST-cleanup' AND title='keep this item'",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .unwrap()
}

#[test]
fn dependent_rows_and_relations_go_but_independent_items_and_child_runs_stay() {
    let e = FlowEnv::new();
    let id = old(&e, &context());
    let child = dependents(&e, &id);
    let mut expected = get(&e, &child).unwrap();
    expected.parent_run = None;
    assert_eq!(counts(&e, &id), (1, 1, 3, 1));
    assert_eq!(clean(&e), 1);
    assert!(get(&e, &id).is_none());
    assert_eq!(counts(&e, &id), (0, 0, 1, 1));
    assert_eq!(get(&e, &child).unwrap(), expected);
}

#[test]
fn a_failed_delete_rolls_back_dependents_links_and_parent_refs_then_can_retry() {
    let e = FlowEnv::new();
    let id = old(&e, &context());
    let child = dependents(&e, &id);
    let before = get(&e, &child).unwrap();
    e.env.store().with_conn(|c| { c.execute_batch("CREATE TRIGGER reject_old_start_delete BEFORE DELETE ON flow_runs BEGIN SELECT RAISE(ABORT,'cleanup unavailable'); END")?; Ok(()) }).unwrap();
    let error = e
        .env
        .store()
        .transaction(runs::cleanup_unpublished_digest_starts)
        .unwrap_err();
    assert!(error.to_string().contains("cleanup unavailable"));
    assert!(get(&e, &id).is_some());
    assert_eq!(get(&e, &child).unwrap(), before);
    assert_eq!(counts(&e, &id), (1, 1, 3, 1));
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER reject_old_start_delete")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(clean(&e), 1);
    assert_eq!(clean(&e), 0);
}

#[test]
fn current_dead_budget_survives_old_marker_cleanup_and_reopening() {
    let e = FlowEnv::new();
    old(&e, &context());
    let revision = json!({"schedule":1});
    for n in 0..3 {
        e.env
            .store()
            .transaction(|tx| {
                runs::record_start_failure(
                    tx,
                    &runs::FailedStart {
                        ws: WS,
                        flow_id: "flow_cleanup",
                        flow_version: 1,
                        source: "digest.bot",
                        error: "bad graph",
                        now: NOW + n * 60_000,
                        revision: &revision,
                        limit: 3,
                    },
                )
            })
            .unwrap();
    }
    let read = |c: &rusqlite::Connection| {
        runs::startup_state(c, WS, "flow_cleanup", "digest.bot", &revision)
    };
    let before = e.env.store().reader(read).unwrap().unwrap();
    assert_eq!(before.state, runs::StartupStatus::Dead);
    assert_eq!(clean(&e), 1);
    // Keep the directory, but release every old owner before acquiring a new writer.
    let FlowEnv { env, core, flow } = e;
    let steve_modules::testkit::TestEnv {
        dir,
        modules,
        gateway,
        clock,
        events,
    } = env;
    drop((flow, core, modules, gateway, clock, events));
    let reopened =
        steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
            .unwrap();
    assert_eq!(reopened.reader(read).unwrap().unwrap(), before);
}

#[test]
fn r4g_old_localized_startup_rows_are_deleted_not_converted() {
    let e = FlowEnv::new();
    let mut ctx = context();
    ctx["start_failures"] = json!(2);
    ctx["first_at"] = json!(NOW - 60_000);
    ctx["error_fingerprint"] = json!("a".repeat(64));
    ctx["startup"] =
        json!({"format":1,"revision":{"flow":1},"state":"retrying","attempts":2,"limit":3});
    let id = old(&e, &ctx);
    let child = dependents(&e, &id);
    let mut without_fingerprint = ctx.clone();
    without_fingerprint
        .as_object_mut()
        .unwrap()
        .remove("error_fingerprint");
    let old_without_fingerprint = old(&e, &without_fingerprint);
    e.env.store().with_conn(|c| { c.execute_batch("CREATE TRIGGER reject_text_cleanup BEFORE DELETE ON flow_runs BEGIN SELECT RAISE(ABORT,'cleanup refused'); END")?;Ok(()) }).unwrap();
    assert!(
        e.env
            .store()
            .transaction(runs::cleanup_unpublished_digest_starts)
            .is_err()
    );
    assert!(get(&e, &id).is_some() && get(&e, &old_without_fingerprint).is_some());
    assert_eq!(counts(&e, &id), (1, 1, 3, 1));
    e.env
        .store()
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER reject_text_cleanup")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(clean(&e), 2);
    assert!(get(&e, &id).is_none());
    assert!(get(&e, &old_without_fingerprint).is_none());
    assert!(get(&e, &child).is_some());
    assert_eq!(counts(&e, &id), (0, 0, 1, 1));
    assert_eq!(clean(&e), 0);
}

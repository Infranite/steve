//! Whole-state query boundaries and legacy ordering, independent of Hub's display DTOs.
mod common;

use rusqlite::{Connection, params};
use serde_json::json;
use steve_store::{Store, WS};

fn item(c: &Connection, id: &str, no: i64, kind: &str) {
    c.execute("INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES (?1, ?2, ?3, ?4, 'fixture', 'run', '定义', 1, 1)", params![id, WS, no, kind]).unwrap();
}

fn session(c: &Connection, id: &str, item: Option<&str>, no: i64, updated: i64, status: &str) {
    c.execute("INSERT INTO sessions (id, workspace_id, item_id, no, harness, cwd, status, created, updated) VALUES (?1, ?2, ?3, ?4, 'fixture', '/tmp', ?5, 1, ?6)", params![id, WS, item, no, status, updated]).unwrap();
}

fn queue(c: &Connection, id: &str, sid: &str, created: i64, files: Option<&str>) {
    c.execute("INSERT INTO queued (id, session_id, text, via, created, files) VALUES (?1, ?2, 'queued', 'user', ?3, ?4)", params![id, sid, created, files]).unwrap();
}

#[test]
fn item_extras_keep_task_scope_goal_order_and_last_live_session_queue() {
    let t = common::fresh();
    t.store.with_conn(|c| {
        item(c, "task", 1, "task");
        item(c, "note", 2, "note");
        for (id, version, text) in [("task", 2, "new"), ("task", 1, "old"), ("note", 1, "ignored")] {
            c.execute("INSERT INTO item_goals (item_id, version, text, created) VALUES (?1, ?2, ?3, 1)", params![id, version, text])?;
        }
        session(c, "live-first", Some("task"), 1, 1, "idle");
        session(c, "live-last", Some("task"), 2, 2, "idle");
        session(c, "ended", Some("task"), 3, 3, "ended");
        for (id, sid) in [("q1", "live-first"), ("q2", "live-last"), ("q3", "live-last"), ("q4", "ended")] {
            queue(c, id, sid, 1, None);
        }
        Ok(())
    }).unwrap();
    let extras = t.store.items().state_extras(WS).unwrap();
    assert_eq!(extras.len(), 1);
    let x = &extras["task"];
    assert_eq!((&*x.goal, x.goals, x.queued), ("new", 2, 2));
    assert_eq!(
        x.sessions
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["live-first", "live-last", "ended"]
    );
    assert!(t.store.items().state_extras("absent").unwrap().is_empty());
}

#[test]
fn queued_state_preserves_array_shape_fallbacks_and_created_rowid_order() {
    let t = common::fresh();
    t.store
        .with_conn(|c| {
            session(c, "s", None, 1, 1, "idle");
            for (id, created, files) in [
                ("array", 2, Some(r#"[{"unknown":true},7,null]"#)),
                ("bad", 1, Some("broken")),
                ("object", 1, Some("{}")),
                ("null", 3, None),
            ] {
                queue(c, id, "s", created, files);
            }
            Ok(())
        })
        .unwrap();
    let rows = t.store.sessions().queued_by_session(WS).unwrap();
    let rows = &rows["s"];
    assert_eq!(
        rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["bad", "object", "array", "null"]
    );
    assert_eq!(rows[2].files, json!([{"unknown":true}, 7, null]));
    for i in [0, 1, 3] {
        assert_eq!(rows[i].files, json!([]));
    }
    assert_eq!(
        serde_json::to_value(&rows[2]).unwrap(),
        json!({"id":"array", "session_id":"s", "text":"queued", "via":"user", "created":2, "files":[{"unknown":true},7,null]})
    );
    assert!(
        t.store
            .sessions()
            .queued_by_session("absent")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn queued_state_limits_sessions_to_the_300_most_recently_updated() {
    let t = common::fresh();
    t.store
        .with_conn(|c| {
            for i in 0..301 {
                let id = format!("s{i}");
                session(c, &id, None, 1, i, "idle");
                queue(c, &format!("q{i}"), &id, 1, None);
            }
            Ok(())
        })
        .unwrap();
    let rows = t.store.sessions().queued_by_session(WS).unwrap();
    assert_eq!(rows.len(), 300);
    assert!(!rows.contains_key("s0"));
    assert!(rows.contains_key("s300"));
}

#[test]
fn bot_extras_keep_global_memberships_and_mcp_but_scope_memory_counts() {
    let t = common::fresh();
    t.store.with_conn(|c| {
        c.execute("INSERT INTO workspaces (id, name, created) VALUES ('other', 'other', 1)", [])?;
        for id in ["b1", "b2"] {
            c.execute("INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES (?1, 'other', ?1, 'fixture', 1)", [id])?;
        }
        for (bot, ws) in [("b1", WS), ("b1", "other"), ("b2", "other")] {
            c.execute("INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?1, ?2, 1)", params![bot, ws])?;
        }
        for (id, name, enabled, kind) in [("z", "Zeta", 0, "mcp"), ("a", "alpha", 2, "mcp"), ("ignored", "hidden", 1, "skill")] {
            c.execute("INSERT INTO mcp_servers (id, workspace_id, name, transport, created, updated) VALUES (?1, 'other', ?2, 'stdio', 1, 1)", params![id, name])?;
            c.execute("INSERT INTO bot_capabilities (bot_id, kind, entry_id, enabled, created) VALUES ('b2', ?1, ?2, ?3, 1)", params![kind, id, enabled])?;
        }
        for (id, ws) in [("m1", WS), ("m2", WS), ("m3", "other")] {
            c.execute("INSERT INTO bot_memories (id, bot_id, workspace_id, text, source, created) VALUES (?1, 'b2', ?2, 'memory', 'fixture', 1)", params![id, ws])?;
        }
        Ok(())
    }).unwrap();
    let x = t.store.bots().state_extras(WS).unwrap();
    assert_eq!(x.workspaces["b1"], [WS, "other"]);
    assert_eq!(x.workspaces["b2"], ["other"]);
    assert_eq!(x.memories["b2"], 2);
    let mcp = t.store.bots().mcp_summaries("b2").unwrap();
    assert_eq!(x.mcp["b2"], mcp);
    assert_eq!(
        serde_json::to_value(mcp).unwrap(),
        json!([
            {"id":"a", "name":"alpha", "transport":"stdio", "enabled":true},
            {"id":"z", "name":"Zeta", "transport":"stdio", "enabled":false}
        ])
    );
    assert!(t.store.bots().mcp_summaries("absent").unwrap().is_empty());
}

fn check_empty_reads(store: &Store) -> steve_store::Result<()> {
    assert!(store.items().state_extras(WS)?.is_empty());
    assert!(store.items().closeout_data("absent")?.is_none());
    assert!(store.sessions().queued_by_session(WS)?.is_empty());
    assert!(store.bots().state_extras(WS)?.mcp.is_empty());
    assert!(store.bots().mcp_summaries("absent")?.is_empty());
    Ok(())
}

#[test]
fn all_five_reads_reuse_an_active_transaction_and_observe_its_writes() {
    let t = common::fresh();
    check_empty_reads(&t.store).unwrap();
    let result: steve_store::Result<()> = t.store.atomic(|| {
        check_empty_reads(&t.store)?;
        t.store.with_conn(|c| {
            session(c, "uncommitted", None, 1, 1, "idle");
            queue(c, "q", "uncommitted", 1, None);
            Ok(())
        })?;
        assert!(
            t.store
                .sessions()
                .queued_by_session(WS)?
                .contains_key("uncommitted")
        );
        Err(steve_store::StoreError::Conflict(
            "roll back fixture".into(),
        ))
    });
    assert!(result.is_err());
    check_empty_reads(&t.store).unwrap();
}

#[test]
fn five_batches_each_use_one_readonly_snapshot_from_the_engine_pool() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let dir = tempfile::tempdir().unwrap();
    let engine = steve_store_sqlite::open::open_runtime(dir.path()).unwrap();
    let pool = engine.readers().clone();
    let store = Store::from_engine(
        engine,
        steve_store::NoPublisher,
        steve_store::FacadeMode::Runtime,
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    let begins = Arc::new(AtomicUsize::new(0));
    let readers = (0..pool.max_size())
        .map(|_| pool.get().unwrap())
        .collect::<Vec<_>>();
    for reader in &readers {
        let begins = begins.clone();
        reader
            .authorizer(Some(move |context: AuthContext<'_>| {
                match context.action {
                    AuthAction::Transaction {
                        operation: TransactionOperation::Begin,
                    } => {
                        begins.fetch_add(1, Ordering::SeqCst);
                    }
                    AuthAction::Insert { .. }
                    | AuthAction::Update { .. }
                    | AuthAction::Delete { .. } => return Authorization::Deny,
                    _ => {}
                }
                Authorization::Allow
            }))
            .unwrap();
    }
    drop(readers);
    check_empty_reads(&store).unwrap();
    assert_eq!(
        begins.load(Ordering::SeqCst),
        5,
        "one snapshot for every public batch call on the engine's pool"
    );
    let readers = (0..pool.max_size())
        .map(|_| pool.get().unwrap())
        .collect::<Vec<_>>();
    assert!(
        readers.iter().all(|c| c.is_autocommit()),
        "all snapshots were closed before returning the connections"
    );
}

#[test]
fn item_batch_does_not_mix_a_newer_commit_into_its_later_selects() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let dir = tempfile::tempdir().unwrap();
    let engine = steve_store_sqlite::open::open_runtime(dir.path()).unwrap();
    let pool = engine.readers().clone();
    let store = Store::from_engine(
        engine,
        steve_store::NoPublisher,
        steve_store::FacadeMode::Runtime,
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store.with_conn(|c| {
        item(c, "task", 1, "task");
        c.execute("INSERT INTO item_goals (item_id, version, text, created) VALUES ('task', 1, 'before', 1)", [])?;
        Ok(())
    }).unwrap();
    let committed = Arc::new(AtomicBool::new(false));
    let readers = (0..pool.max_size())
        .map(|_| pool.get().unwrap())
        .collect::<Vec<_>>();
    for reader in &readers {
        let writer = store.clone();
        let committed = committed.clone();
        reader
            .authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "sessions",
                        ..
                    }
                ) && !committed.swap(true, Ordering::SeqCst)
                {
                    writer
                        .with_conn(|c| {
                            c.execute(
                                "UPDATE item_goals SET text='after' WHERE item_id='task'",
                                [],
                            )?;
                            session(c, "new-session", Some("task"), 1, 1, "idle");
                            Ok(())
                        })
                        .unwrap();
                }
                Authorization::Allow
            }))
            .unwrap();
    }
    drop(readers);
    let before = store.items().state_extras(WS).unwrap();
    assert!(committed.load(Ordering::SeqCst));
    assert_eq!(before["task"].goal, "before");
    assert!(before["task"].sessions.is_empty());
    let after = store.items().state_extras(WS).unwrap();
    assert_eq!(after["task"].goal, "after");
    assert_eq!(
        after["task"].sessions,
        [("new-session".into(), "idle".into())]
    );
    // Authorizers hold the writer clone: remove them before releasing this fixture.
    let readers = (0..pool.max_size())
        .map(|_| pool.get().unwrap())
        .collect::<Vec<_>>();
    for reader in &readers {
        reader
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .unwrap();
    }
}

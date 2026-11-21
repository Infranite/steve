use super::*;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::atomic::AtomicUsize;

const V3: &str = include_str!("../fixtures/v3.sql");
const GONE_PGID: i64 = i32::MAX as i64;

fn variants() -> Vec<Observed> {
    vec![
        Observed::Accepted,
        Observed::Running,
        Observed::Waiting { on: vec![] },
        Observed::Finished {
            outcome: Outcome::Succeeded,
            result: None,
            error: None,
        },
        Observed::Unknown {
            reason: "existing uncertainty".into(),
        },
    ]
}

fn receipt(n: usize, state: Observed) -> Receipt {
    Receipt {
        operation: OperationId::new(format!("op_{n}")).unwrap(),
        state,
    }
}

fn v3(path: &Path, started: bool, disk: bool, version: i64) -> Connection {
    let c = Connection::open(path).unwrap();
    let schema = if started {
        V3.to_owned()
    } else {
        V3.replace(
            "reports INTEGER NOT NULL DEFAULT 0,\n  started INTEGER",
            "reports INTEGER NOT NULL DEFAULT 0",
        )
    };
    c.execute_batch(&schema).unwrap();
    c.pragma_update(None, "user_version", version).unwrap();
    c.execute("INSERT INTO meta (key,value) VALUES ('incarnation',7)", [])
        .unwrap();
    if version >= 3 {
        c.execute("INSERT INTO meta (key,value) VALUES ('journal_id',123)", [])
            .unwrap();
    }
    assert!(crate::procs::state(GONE_PGID, None).gone());
    c.execute(
        "INSERT INTO procs (session,pgid,generation,incarnation,meter) VALUES ('s',?1,1,7,'m')",
        [GONE_PGID],
    )
    .unwrap();
    if started {
        c.execute("UPDATE procs SET started=99", []).unwrap();
    }
    if disk {
        c.execute_batch("CREATE TABLE disk_observations (bytes INTEGER);")
            .unwrap();
    }
    for (n, state) in variants().into_iter().enumerate() {
        let r = receipt(n, state);
        c.execute("INSERT INTO ops (rowid,id,method,digest,receipt,session,cancel,accepted) VALUES (?4,?1,'test',?2,?3,'s',1,123)",
            params![r.operation.as_str(), Digest::sha256(b"test").as_str(), json(&r), ((n + 1) * 10) as i64]).unwrap();
    }
    c.execute("INSERT INTO ops (id,method,digest,receipt,accepted) VALUES ('legacy_opaque','test',?1,'legacy receipt bytes',456)", [Digest::sha256(b"legacy_opaque").as_str()]).unwrap();
    c
}

// Exercise the one-time ledger deletion before the same transaction's restart recovery.
fn migrate(c: &mut Connection) {
    let heads = BTreeMap::new();
    let counters = BTreeMap::new();
    let t = Tx {
        sql: c.transaction().unwrap(),
        heads: &heads,
        head_changes: Default::default(),
        identity: "",
        counters: &counters,
        accept_floor: 0,
        counter_changes: Default::default(),
        used: 0,
        limit: u64::MAX,
        appended: Cell::new(0),
    };
    t.initialize(3).unwrap();
    t.sql.commit().unwrap();
}

fn columns(c: &Connection, table: &str) -> Vec<String> {
    c.prepare("SELECT name FROM pragma_table_info(?1)")
        .unwrap()
        .query_map([table], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn every_observed_variant_matches_the_partial_index_and_checked_column() {
    let d = tempfile::tempdir().unwrap();
    let (mut w, _) = Writer::open(&d.path().join("node.db")).unwrap();
    w.write(|t| {
        for (n, observed) in variants().into_iter().enumerate() {
            // Exhaustive on purpose: adding a wire variant must update this test as well.
            let unfinished = match &observed {
                Observed::Accepted | Observed::Running | Observed::Waiting { .. } | Observed::Unknown { .. } => true,
                Observed::Finished { .. } => false,
            };
            let r = receipt(n, observed.clone());
            let op = Op { method: "test".into(), digest: Digest::sha256(b"test"), receipt: r.clone(), execution: None, workspace: None, cancel: false };
            t.accept(&r.operation, &op, None, 0)?;
            let indexed: bool = t.c().query_row(&format!("SELECT EXISTS(SELECT 1 FROM ops INDEXED BY ops_open WHERE {OPEN_OPS} AND id=?1)"), [r.operation.as_str()], |r| r.get(0)).map_err(internal)?;
            assert_eq!(indexed, unfinished, "{observed:?}");
            assert_eq!(indexed, !observed.is_finished());
            let stored: String = t.c().query_row("SELECT state FROM ops WHERE id=?1", [r.operation.as_str()], |r| r.get(0)).map_err(internal)?;
            assert_eq!(stored, observed.state().as_str());
            assert!(t.c().execute("UPDATE ops SET state='waiting' WHERE id=?1", [r.operation.as_str()]).is_err());
            assert!(t.c().execute("UPDATE ops SET state=NULL WHERE id=?1", [r.operation.as_str()]).is_err());
        }
        Ok(())
    }).unwrap();
}

#[test]
fn upgrade_refuses_live_or_unproven_process_groups_before_discarding_any_records() {
    let live_group = unsafe { libc::getpgrp() } as i64;
    for started in [false, true] {
        for pgid in [live_group, 1, i64::MAX] {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("node.db");
            let c = v3(&p, started, true, 3);
            c.execute("UPDATE procs SET pgid=?1", [pgid]).unwrap();
            c.execute(
                "INSERT INTO reservations (key,op) VALUES ('held','missing_op')",
                [],
            )
            .unwrap();
            let e = Writer::open(&p)
                .err()
                .expect("unproven group must block migration");
            assert_eq!(e.code, ErrorCode::Unsupported, "{started} / {pgid}");
            assert!(e.safe_detail.contains("stop the old Node's processes"));
            assert_eq!(
                c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                3
            );
            assert_eq!(
                c.query_row("SELECT value FROM meta WHERE key='incarnation'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
                7
            );
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM meta WHERE key='accept_floor'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(
                c.query_row("SELECT pgid FROM procs", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                pgid
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM ops", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                variants().len() as i64 + 1
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM reservations", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert!(!columns(&c, "ops").contains(&"state".into()));
        }
    }
}

#[test]
fn v3_discards_the_entire_ledger_and_retires_compatibility_schema() {
    let d = tempfile::tempdir().unwrap();
    let mut c = v3(&d.path().join("node.db"), false, true, 3);
    c.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::AlterTable { .. } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    let before = crate::now();
    migrate(&mut c);
    let after = crate::now();
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM ops", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let floor: i64 = c
        .query_row("SELECT value FROM meta WHERE key='accept_floor'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!((before..=after).contains(&floor));
    assert!(columns(&c, "procs").contains(&"started".into()));
    assert_eq!(
        c.query_row("SELECT count(*) FROM procs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='disk_observations'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let definition: String = c
        .query_row("SELECT sql FROM sqlite_schema WHERE name='ops'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(definition.contains("CHECK"));
    let state_default: Option<String> = c
        .query_row(
            "SELECT dflt_value FROM pragma_table_info('ops') WHERE name='state'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state_default, None);
}


#[test]
fn a_mid_migration_failure_rolls_back_every_ddl_and_row() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("node.db");
    let c = v3(&p, false, true, 3);
    c.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::CreateTable {
            table_name: "procs",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    assert!(Writer::from_connection(&p, c).is_err());
    let c = Connection::open(&p).unwrap();
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert!(!columns(&c, "ops").contains(&"state".into()));
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM meta WHERE key='accept_floor'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(!columns(&c, "procs").contains(&"started".into()));
    assert_eq!(
        c.query_row("SELECT pgid FROM procs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        GONE_PGID
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='disk_observations'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT receipt FROM ops WHERE id='legacy_opaque'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "legacy receipt bytes"
    );
    assert_eq!(
        c.query_row("SELECT value FROM meta WHERE key='incarnation'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        7
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM ops", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        variants().len() as i64 + 1,
        "all legacy command rows survive the failed migration"
    );
}


#[test]
fn restart_uses_one_indexed_update_and_never_touches_finished_or_unknown_receipts() {
    let d = tempfile::tempdir().unwrap();
    let (mut w, _) = Writer::open(&d.path().join("node.db")).unwrap();
    w.write(|t| {
        for (n, observed) in variants().into_iter().enumerate() {
            let r = receipt(n, observed);
            t.accept(
                &r.operation,
                &Op {
                    method: "test".into(),
                    digest: Digest::sha256(b"test"),
                    receipt: r.clone(),
                    execution: None,
                    workspace: None,
                    cancel: false,
                },
                None,
                0,
            )?;
        }
        // If restart ever scans/parses this completed receipt, startup fails.
        t.c()
            .execute(
                "UPDATE ops SET receipt='not-json' WHERE state='finished'",
                [],
            )
            .map_err(internal)?;
        Ok(())
    })
    .unwrap();
    let updates = Arc::new(AtomicUsize::new(0));
    let observed = updates.clone();
    w.db.authorizer(Some(move |context: AuthContext<'_>| {
        if matches!(
            context.action,
            AuthAction::Update {
                table_name: "ops",
                column_name: "receipt"
            }
        ) {
            observed.fetch_add(1, Ordering::SeqCst);
        }
        Authorization::Allow
    }))
    .unwrap();
    w.write(|t| t.mark_unknown()).unwrap();
    assert_eq!(updates.load(Ordering::SeqCst), 1);
    let unknown = Observed::Unknown {
        reason: steve_node_api::RESTARTED.into(),
    };
    let plan = w
        .read(|t| {
            t.rows(
                &format!("EXPLAIN QUERY PLAN {}", *RESTART_OPS_SQL),
                params![unknown.state().as_str(), json(&unknown)],
                |r| r.get::<_, String>(3),
            )
        })
        .unwrap();
    assert!(
        plan.iter().any(|line| line.contains("ops_open")),
        "{plan:?}"
    );
    w.read(|t| {
        for n in 0..3 {
            assert_eq!(
                t.op(&OperationId::new(format!("op_{n}")).unwrap())?
                    .unwrap()
                    .receipt
                    .state,
                Observed::Unknown {
                    reason: steve_node_api::RESTARTED.into()
                }
            );
        }
        assert_eq!(
            t.c()
                .query_row("SELECT receipt FROM ops WHERE id='op_3'", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "not-json"
        );
        assert_eq!(
            t.op(&OperationId::new("op_4").unwrap())?.unwrap().receipt,
            receipt(4, variants().pop().unwrap())
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn fresh_and_v4_startup_execute_no_ddl_or_column_probes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("node.db");
    let c = Connection::open(&p).unwrap();
    c.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::AlterTable { .. } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    let (w, inc) = Writer::from_connection(&p, c).unwrap();
    assert_eq!(inc, 1);
    assert_eq!(
        w.db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    drop(w);
    let c = Connection::open(&p).unwrap();
    c.execute(
        "INSERT INTO procs (session,pgid,generation,incarnation,meter) VALUES ('held',1,1,1,'m')",
        [],
    )
    .unwrap();
    let denied = Arc::new(AtomicUsize::new(0));
    let observed = denied.clone();
    c.authorizer(Some(move |context: AuthContext<'_>| {
        let schema_work = match context.action {
            AuthAction::AlterTable { .. }
            | AuthAction::CreateTable { .. }
            | AuthAction::DropTable { .. }
            | AuthAction::CreateIndex { .. } => true,
            AuthAction::Pragma { pragma_name, .. } => pragma_name == "table_info",
            AuthAction::Read { table_name, .. } => table_name == "pragma_table_info",
            _ => false,
        };
        if schema_work {
            observed.fetch_add(1, Ordering::SeqCst);
            Authorization::Deny
        } else {
            Authorization::Allow
        }
    }))
    .unwrap();
    let (mut w, inc) = Writer::from_connection(&p, c).unwrap();
    assert_eq!(inc, 2);
    assert_eq!(
        w.read(|t| t.procs()).unwrap().len(),
        1,
        "normal v4 restart retains an unproven group"
    );
    assert_eq!(denied.load(Ordering::SeqCst), 0);
}


#[test]
fn acceptance_and_transitions_keep_receipt_and_column_in_sync() {
    let d = tempfile::tempdir().unwrap();
    let (mut w, _) = Writer::open(&d.path().join("node.db")).unwrap();
    let id = OperationId::new("op_0").unwrap();
    w.write(|t| {
        let r = receipt(0, Observed::Accepted);
        t.accept(
            &id,
            &Op {
                method: "test".into(),
                digest: Digest::sha256(b"test"),
                receipt: r,
                execution: None,
                workspace: None,
                cancel: false,
            },
            None,
            0,
        )?;
        for observed in variants() {
            assert!(t.set_state(&id, &observed)?);
            let state: String = t
                .c()
                .query_row("SELECT state FROM ops WHERE id=?1", [id.as_str()], |r| {
                    r.get(0)
                })
                .map_err(internal)?;
            assert_eq!(state, observed.state().as_str());
            assert_eq!(t.op(&id)?.unwrap().receipt.state, observed);
            if observed.is_finished() {
                break;
            }
        }
        assert!(!t.set_state(&id, &Observed::Running)?);
        Ok(())
    })
    .unwrap();
}

#[test]
fn transitions_target_the_requested_row_even_when_a_stored_receipt_has_a_wrong_id() {
    let d = tempfile::tempdir().unwrap();
    let (mut w, _) = Writer::open(&d.path().join("node.db")).unwrap();
    w.write(|t| {
        for n in 0..2 {
            let r = receipt(n, Observed::Accepted);
            t.accept(
                &r.operation,
                &Op {
                    method: "test".into(),
                    digest: Digest::sha256(b"test"),
                    receipt: r.clone(),
                    execution: None,
                    workspace: None,
                    cancel: false,
                },
                None,
                0,
            )?;
        }
        let wrong = receipt(1, Observed::Accepted);
        t.c()
            .execute("UPDATE ops SET receipt=?1 WHERE id='op_0'", [json(&wrong)])
            .map_err(internal)?;
        t.set_state(
            &OperationId::new("op_0").unwrap(),
            &Observed::Waiting { on: vec![] },
        )?;
        let states = t.rows("SELECT state FROM ops ORDER BY id", [], |r| {
            r.get::<_, String>(0)
        })?;
        assert_eq!(states, vec!["running", "accepted"]);
        Ok(())
    })
    .unwrap();
}

// All command records are discarded, including compatible ones. Pending payloads and session
// stream heads are a separate durable ledger and must survive this one-time schema upgrade.



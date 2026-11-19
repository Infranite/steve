use super::*;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::atomic::AtomicUsize;

const V3: &str = include_str!("../fixtures/v3.sql");

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
    c.execute(
        "INSERT INTO procs (session,pgid,generation,incarnation,meter) VALUES ('s',42,1,7,'m')",
        [],
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
    c.execute("INSERT INTO ops (id,method,digest,receipt,accepted) VALUES ('bad','test',?1,'not-json',456)", [Digest::sha256(b"bad").as_str()]).unwrap();
    c
}

// Exercise initialize alone to inspect the backfill before the same transaction's recovery.
fn migrate(c: &mut Connection) {
    let heads = BTreeMap::new();
    let counters = BTreeMap::new();
    let t = Tx {
        sql: c.transaction().unwrap(),
        heads: &heads,
        head_changes: Default::default(),
        identity: "",
        counters: &counters,
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
fn migration_and_recovery_commit_together_and_v4_keeps_the_original_floor() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("node.db");
    drop(v3(&p, true, false, 3));
    let (mut w, inc) = Writer::open(&p).unwrap();
    assert_eq!(inc, 8);
    assert!(w.read(|t| t.procs()).unwrap().is_empty());
    let floor = w
        .read(|t| {
            assert!(
                t.rows("SELECT id FROM ops", [], |r| r.get::<_, String>(0))?
                    .is_empty()
            );
            Ok(t.accept_floor())
        })
        .unwrap();
    assert!(floor > 0);
    let r = receipt(
        0,
        Observed::Unknown {
            reason: "v4 uncertainty".into(),
        },
    );
    w.write(|t| {
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
            floor,
        )
    })
    .unwrap();
    drop(w);
    let (mut w, inc) = Writer::open(&p).unwrap();
    assert_eq!(inc, 9);
    w.read(|t| {
        assert_eq!(
            t.accept_floor(),
            floor,
            "ordinary restart never advances the upgrade boundary"
        );
        assert_eq!(t.op(&r.operation)?.unwrap().receipt, r);
        Ok(())
    })
    .unwrap();
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
        42
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
        c.query_row("SELECT receipt FROM ops WHERE id='bad'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "not-json"
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
            "SELECT count(*) FROM sqlite_schema WHERE name='ops_v3'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn failed_recovery_rolls_back_the_completed_migration_and_incarnation() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("node.db");
    let c = v3(&p, false, true, 3);
    c.execute_batch("CREATE TRIGGER fail_recovery BEFORE UPDATE OF value ON meta WHEN NEW.key='incarnation' BEGIN SELECT RAISE(ABORT,'recovery fixture'); END;").unwrap();
    assert!(Writer::open(&p).is_err());
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
        42
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
            "SELECT count(*) FROM sqlite_schema WHERE name='disk_observations'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    c.execute_batch("DROP TRIGGER fail_recovery;").unwrap();
    drop(c);
    assert_eq!(Writer::open(&p).unwrap().1, 8);
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
    let plan = w.read(|t| t.rows(&format!("EXPLAIN QUERY PLAN UPDATE ops SET state='unknown',receipt=json_set(receipt,'$.state',json(?1)) WHERE {OPEN_OPS} AND state<>'unknown'"), [json(&Observed::Unknown { reason: "restart".into() })], |r| r.get::<_, String>(3))).unwrap();
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
    let (_w, inc) = Writer::from_connection(&p, c).unwrap();
    assert_eq!(inc, 2);
    assert_eq!(denied.load(Ordering::SeqCst), 0);
}

#[test]
fn unversioned_journal_discards_legacy_process_records_without_backfilling_identity() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("node.db");
    drop(v3(&p, false, false, 0));
    let (mut w, inc) = Writer::open(&p).unwrap();
    assert_eq!(inc, 8);
    assert!(w.read(|t| t.procs()).unwrap().is_empty());
    drop(w);
    assert_eq!(Writer::open(&p).unwrap().1, 9);
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
fn dependent_v3(c: &Connection) {
    let job = |name: &str, attempt: &str| ExecutionRef::Job {
        job: JobId::new(name).unwrap(),
        attempt: AttemptId::new(attempt).unwrap(),
    };
    let turn = ExecutionRef::Turn {
        session: SessionId::new("shared_session").unwrap(),
        turn: TurnId::new("turn").unwrap(),
        attempt: AttemptId::new("turn_attempt").unwrap(),
    };
    for (op, execution) in [
        ("bad", Some(job("private", "private_attempt"))),
        ("bad_shared", Some(job("shared", "bad_attempt"))),
        ("bad_turn", Some(turn)),
        ("bad_pair_1", Some(job("bad_pair", "pair_1"))),
        ("bad_pair_2", Some(job("bad_pair", "pair_2"))),
        ("bad_plain", None),
        ("op_0", Some(job("shared", "good_attempt"))),
    ] {
        let encoded = execution.as_ref().map(json);
        if op != "op_0" {
            c.execute("INSERT OR IGNORE INTO ops (id,method,digest,receipt,accepted) VALUES (?1,'test','digest','not-json',456)", [op]).unwrap();
        }
        c.execute(
            "UPDATE ops SET execution=?2 WHERE id=?1",
            params![op, encoded],
        )
        .unwrap();
        c.execute(
            "INSERT INTO attempts (attempt,execution,op) VALUES (?1,?2,?1)",
            params![op, encoded.as_deref().unwrap_or("null")],
        )
        .unwrap();
        c.execute(
            "INSERT INTO artifact_grants (attempt,workspace,body) VALUES (?1,'workspace','grant')",
            [op],
        )
        .unwrap();
        c.execute(
            "INSERT INTO command_specs (op,body) VALUES (?1,'body')",
            [op],
        )
        .unwrap();
        c.execute("INSERT INTO reservations (key,op) VALUES (?1,?1)", [op])
            .unwrap();
        c.execute(
            "INSERT INTO answers (op,permission,resolution) VALUES (?1,'permission','resolution')",
            [op],
        )
        .unwrap();
        c.execute(
            "INSERT INTO waits (id,op,session,request) VALUES (?1,?1,'session','request')",
            [op],
        )
        .unwrap();
        c.execute(
            "INSERT INTO harness_setups (harness,op) VALUES (?1,?1)",
            [op],
        )
        .unwrap();
        for stream in Tx::op_streams(op, encoded.as_deref()).unwrap() {
            c.execute("INSERT OR IGNORE INTO streams (stream,epoch,produced,acknowledged) VALUES (?1,'epoch',1,0)", [&stream]).unwrap();
            c.execute(
                "INSERT OR IGNORE INTO events (stream,seq,event) VALUES (?1,1,'pending')",
                [&stream],
            )
            .unwrap();
        }
    }
}

#[test]
fn upgrade_deletes_all_command_dependencies_but_preserves_pending_events_and_session_heads() {
    let d = tempfile::tempdir().unwrap();
    let mut c = v3(&d.path().join("node.db"), false, true, 3);
    dependent_v3(&c);
    for stream in [
        "op:acknowledged",
        "job:acknowledged",
        "session:acknowledged",
        "op:pending_not_counted",
    ] {
        c.execute(
            "INSERT INTO streams (stream,epoch,produced,acknowledged) VALUES (?1,'epoch',0,0)",
            [stream],
        )
        .unwrap();
    }
    // Protect an unacknowledged event even if its head's counters are inconsistent.
    c.execute(
        "INSERT INTO events (stream,seq,event) VALUES ('op:pending_not_counted',1,'pending')",
        [],
    )
    .unwrap();
    let event_rows = |c: &Connection| {
        c.prepare("SELECT stream,seq,event FROM events ORDER BY stream,seq")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let stream_rows = |c: &Connection| {
        c.prepare("SELECT stream,epoch,produced,acknowledged FROM streams ORDER BY stream")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let events = event_rows(&c);
    let heads: Vec<_> = stream_rows(&c)
        .into_iter()
        .filter(|(stream, _, _, _)| {
            !matches!(stream.as_str(), "op:acknowledged" | "job:acknowledged")
        })
        .collect();
    migrate(&mut c);
    for table in [
        "ops",
        "attempts",
        "command_specs",
        "reservations",
        "answers",
        "waits",
        "harness_setups",
        "artifact_grants",
    ] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(
        event_rows(&c),
        events,
        "pending payloads are byte-for-byte unchanged"
    );
    assert_eq!(
        stream_rows(&c),
        heads,
        "only fully acknowledged operation/job heads are removed"
    );
}

#[test]
fn failed_ledger_deletion_rolls_back_everything() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("node.db");
    let c = v3(&p, false, true, 3);
    dependent_v3(&c);
    let counts = |c: &Connection| {
        [
            "ops",
            "attempts",
            "command_specs",
            "reservations",
            "answers",
            "waits",
            "harness_setups",
            "artifact_grants",
            "streams",
            "events",
        ]
        .map(|table| {
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        })
    };
    let before = counts(&c);
    c.execute_batch("CREATE TRIGGER fail_discard BEFORE DELETE ON reservations WHEN OLD.op='bad' BEGIN SELECT RAISE(ABORT,'discard fixture'); END;").unwrap();
    drop(c);
    assert!(Writer::open(&p).is_err());
    let c = Connection::open(&p).unwrap();
    assert_eq!(counts(&c), before);
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
    assert_eq!(
        c.query_row("SELECT receipt FROM ops WHERE id='bad'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "not-json"
    );
    c.execute_batch("DROP TRIGGER fail_discard;").unwrap();
    drop(c);
    assert_eq!(Writer::open(&p).unwrap().1, 8);
}

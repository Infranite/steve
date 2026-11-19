#[path = "uow_support.rs"]
mod support;
use rusqlite::Connection;
use std::sync::{Arc, mpsc};
use steve_storage_api::{query::*, uow::*, *};
use steve_store_sqlite::{
    domains::{Registry, values},
    exec::{BatchRunner, Executor, SystemClock},
    publish::Attached,
    rev::{BLOCK, RevAllocator},
    uow::{SqliteBatchRunner, apply_in},
};
use support::*;

#[test]
fn revision_reservation_survives_rollback_other_commit_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rev.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(steve_store_sqlite::schema::CONTRACT)
        .unwrap();
    let a = RevAllocator::open(&c).unwrap();
    let issued = a.issue(&c, 1).unwrap()[0];
    assert_eq!(issued, Rev(1));
    c.execute_batch("BEGIN IMMEDIATE; INSERT INTO meta VALUES('core','discard','x'); ROLLBACK;")
        .unwrap();
    let second = a.issue(&c, 1).unwrap()[0];
    c.execute(
        "INSERT INTO meta VALUES('core','other',?)",
        [second.0.to_string()],
    )
    .unwrap();
    assert!(second > issued);
    drop(a);
    drop(c);
    let c = Connection::open(path).unwrap();
    let a = RevAllocator::open(&c).unwrap();
    assert_eq!(a.issue(&c, 1).unwrap()[0], Rev(BLOCK + 1));
    c.execute_batch("BEGIN").unwrap();
    assert!(a.issue(&c, 1).is_err());
    c.execute_batch("ROLLBACK").unwrap();
    let many = a.issue(&c, 4097).unwrap();
    assert_eq!(many.len(), 4097);
    assert!(many.windows(2).all(|w| w[0] < w[1]));
}
#[test]
fn bridge_stages_events_and_receipts_until_outer_commit_and_drops_them_on_rollback() {
    let (c, r, _, events) = fixture();
    assert!(
        r.apply_in(
            &c,
            Attached::new(unit("no-tx", vec![]), vec![]),
            &r.reserve_root(&c).unwrap()
        )
        .is_err()
    );
    let root = r.reserve_root(&c).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    let u = unit("bridge", vec![insert("a", 1)]);
    let applied = apply_in(
        &r,
        &c,
        Attached::new(u.clone(), vec!["bridge-event".into()]),
        &root,
    )
    .unwrap();
    assert_eq!(value(&c, "a"), Some(1));
    assert!(events.lock().unwrap().is_empty());
    c.execute_batch("ROLLBACK").unwrap();
    drop(applied);
    assert_eq!(value(&c, "a"), None);
    assert_eq!(count(&c, "uow_applied"), 0);
    let root = r.reserve_root(&c).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    let applied = r
        .apply_in(&c, Attached::new(u, vec!["committed".into()]), &root)
        .unwrap();
    let mut duplicate = unit("bad", vec![insert("a", 2)]);
    duplicate.preconditions.push(Precondition::Absent {
        entity: "verify_core".into(),
        key: vec![Cell::Text("a".into())],
    });
    sign(&mut duplicate);
    assert!(
        r.apply_in(&c, Attached::new(duplicate, vec![]), &root)
            .is_err()
    );
    assert!(!c.is_autocommit());
    c.execute_batch("COMMIT").unwrap();
    let log = events.clone();
    applied
        .pending
        .unwrap()
        .publish(&move |rev, ev| log.lock().unwrap().push((rev, ev)));
    assert_eq!(events.lock().unwrap()[0].1, vec!["committed"]);
}
#[test]
fn registered_operation_panics_and_sql_errors_rollback_only_their_unit() {
    let c = Connection::open_in_memory().unwrap();
    let mut registry = Registry::verification().unwrap();
    registry
        .register_op("test.panic", Domain::Core, |c, _| {
            c.execute(
                "INSERT INTO verify_core(id,value,rev) VALUES('panic',1,1)",
                [],
            )
            .unwrap();
            panic!("operation failure")
        })
        .unwrap();
    registry
        .register_op("test.sql-error", Domain::Core, |c, _| {
            c.execute("INSERT INTO missing_table VALUES(1)", [])
                .map_err(|_| {
                    CommitError::Fatal(steve_storage_api::error(
                        steve_types::Code::Internal,
                        "expected SQL failure",
                    ))
                })?;
            Ok(())
        })
        .unwrap();
    schema(&c, &registry);
    let r = SqliteBatchRunner::new(
        &c,
        Arc::new(registry),
        Arc::new(Time::default()),
        Arc::new(|_, _: Vec<String>| {}),
    )
    .unwrap();
    for name in ["test.panic", "test.sql-error"] {
        let bad = unit(
            name,
            vec![
                insert("before-panic", 1),
                Mutation::Op(AnyOp {
                    name: name.into(),
                    domain: Domain::Core,
                    body: encode(&Row::default()).unwrap(),
                }),
            ],
        );
        let out = r.run(
            &c,
            vec![
                Attached::new(bad, vec![]),
                Attached::new(unit(&format!("after-{name}"), vec![]), vec![]),
            ],
        );
        assert!(out[0].is_err());
        assert!(out[1].is_ok());
        assert!(c.is_autocommit());
        assert_eq!(value(&c, "before-panic"), None);
        assert_eq!(value(&c, "panic"), None);
    }
    commit(&c, &r, unit("next", vec![insert("healthy", 7)])).unwrap();
    assert_eq!(value(&c, "healthy"), Some(7));
}
#[test]
fn events_are_handed_off_before_replies_even_when_later_caller_resumes_first() {
    let (c, r, _, events) = fixture();
    let e = Executor::new("events", c, Arc::new(SystemClock::default()), r, || false).unwrap();
    let (tx, rx) = mpsc::channel();
    let (permit, wait) = mpsc::channel();
    let first = e.clone();
    let a = std::thread::spawn(move || {
        let reply = first
            .submit(
                Lane::Interactive,
                Attached::new(unit("first", vec![insert("a", 1)]), vec!["a".into()]),
            )
            .unwrap();
        tx.send(()).unwrap();
        wait.recv().unwrap();
        reply.blocking_recv().unwrap().unwrap()
    });
    rx.recv().unwrap();
    let b = e
        .submit(
            Lane::Interactive,
            Attached::new(unit("second", vec![insert("b", 2)]), vec!["b".into()]),
        )
        .unwrap()
        .blocking_recv()
        .unwrap()
        .unwrap();
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, v)| v.clone())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    permit.send(()).unwrap();
    let a = a.join().unwrap();
    assert!(a.rev < b.rev);
}
#[test]
fn codec_validation_range_deletes_upserts_and_shape_cache_use_real_sql() {
    let (c, r, _, _) = fixture();
    for (id, n) in [("a", 1), ("b", 2), ("c", 3)] {
        commit(&c, &r, unit(id, vec![insert(id, n)])).unwrap();
    }
    let mut original = row("a", 4);
    set(
        &mut original,
        "data",
        Cell::Json(RawJson::from_str("{ \"z\":2, \"a\":1 }").unwrap()),
    );
    set(&mut original, "bytes", Cell::Bytes(vec![0, 255, 1]));
    set(&mut original, "real", Cell::Real(1.25));
    set(&mut original, "flag", Cell::Bool(true));
    commit(
        &c,
        &r,
        unit(
            "upsert",
            vec![Mutation::Upsert {
                entity: "verify_core".into(),
                row: original.clone(),
            }],
        ),
    )
    .unwrap();
    let e = r.registry.entity("verify_core", Domain::Core).unwrap();
    let actual = e.get(&c, &[Cell::Text("a".into())]).unwrap().unwrap().0;
    for (name, v) in &original.0 {
        assert_eq!(steve_store_sqlite::domains::value(&actual, name), Some(v));
    }
    assert!(e.shape_count() > 0);
    let u = unit(
        "delete",
        vec![Mutation::DeleteWhere {
            entity: "verify_core".into(),
            index: "value".into(),
            range: KeyRange {
                start: KeyBound::Included(vec![Cell::Int(2)]),
                end: KeyBound::Excluded(vec![Cell::Int(4)]),
            },
            filter: vec![Pred::Ge {
                field: "value".into(),
                value: Cell::Int(2),
            }],
            limit: 1,
        }],
    );
    commit(&c, &r, u).unwrap();
    assert_eq!(value(&c, "b"), None);
    assert_eq!(value(&c, "c"), Some(3));
    let mut bad = original;
    set(&mut bad, "real", Cell::Real(f64::NAN));
    let mut u = unit("bad-finite", vec![]);
    u.mutations.push(Mutation::Upsert {
        entity: "verify_core".into(),
        row: bad,
    });
    assert!(commit(&c, &r, u).is_err());
    assert_eq!(
        values(&actual, &["id".into()]),
        vec![Cell::Text("a".into())]
    );
}

#[test]
fn failed_outer_commit_discards_all_success_results_and_events_then_connection_recovers() {
    let c = Connection::open_in_memory().unwrap();
    let mut registry = Registry::verification().unwrap();
    schema(&c, &registry);
    c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(id INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);").unwrap();
    registry
        .register_op("deferred-failure", Domain::Core, |c, _| {
            c.execute("INSERT INTO child VALUES(99)", []).unwrap();
            Ok(())
        })
        .unwrap();
    let events: Events = Arc::default();
    let log = events.clone();
    let r = SqliteBatchRunner::new(
        &c,
        Arc::new(registry),
        Arc::new(Time::default()),
        Arc::new(move |rev, ev| log.lock().unwrap().push((rev, ev))),
    )
    .unwrap();
    let bad = unit(
        "deferred",
        vec![Mutation::Op(AnyOp {
            name: "deferred-failure".into(),
            domain: Domain::Core,
            body: encode(&Row::default()).unwrap(),
        })],
    );
    let out = r.run(
        &c,
        vec![
            Attached::new(
                unit("sibling", vec![insert("discard", 1)]),
                vec!["event".into()],
            ),
            Attached::new(bad, vec![]),
        ],
    );
    assert!(out.iter().all(Result::is_err));
    assert!(events.lock().unwrap().is_empty());
    assert!(c.is_autocommit());
    assert_eq!(count(&c, "uow_applied"), 0);
    assert_eq!(value(&c, "discard"), None);
    assert_eq!(count(&c, "child"), 0);
    commit(&c, &r, unit("healthy", vec![insert("after", 1)])).unwrap();
    assert_eq!(value(&c, "after"), Some(1));
}

#[path = "uow_support.rs"]
mod support;
use std::sync::{Arc, Mutex};
use steve_storage_api::{fact::FactRecord, query::*, uow::*, *};
use steve_store_sqlite::{
    domains::Registry,
    rev::{BLOCK, RevAllocator},
    store::{NoReadServices, SqliteStore},
};
use support::*;
fn get(entity: &str, id: &str, domain: Domain) -> AnyQuery {
    AnyQuery {
        name: "get".into(),
        domain,
        body: Payload::Json(
            encode(&Get::<Row> {
                key: Key {
                    entity: entity.into(),
                    values: vec![Cell::Text(id.into())],
                    marker: std::marker::PhantomData,
                },
            })
            .unwrap(),
        ),
    }
}
#[test]
fn import_advances_live_and_persistent_revision_ranges_and_never_publishes() {
    for split in [false, true] {
        let source_dir = tempfile::tempdir().unwrap();
        prepare_layout(source_dir.path(), split);
        let target_dir = tempfile::tempdir().unwrap();
        prepare_layout(target_dir.path(), split);
        let time = Arc::new(Time::default());
        let reg = Arc::new(Registry::verification().unwrap());
        let source = open_store(source_dir.path(), reg.clone(), time.clone());
        source.install_registered().unwrap();
        let target = open_store(target_dir.path(), reg.clone(), time.clone());
        target.install_registered().unwrap();
        for domain in [Domain::Core, Domain::Stream, Domain::Comms] {
            source
                .with_root(domain, Lane::Admin, |c, r, _| {
                    r.revisions.issue(c, BLOCK as usize + 5)?;
                    Ok(())
                })
                .unwrap();
            let mut u = unit("source", vec![]);
            u.domain = domain;
            u.mutations = if domain == Domain::Stream {
                vec![Mutation::Append {
                    stream: append::StreamName("verify_stream".into()),
                    records: vec![Row(vec![
                        ("id".into(), Cell::Text("source".into())),
                        ("value".into(), Cell::Int(1)),
                    ])],
                }]
            } else {
                vec![Mutation::Insert {
                    entity: if domain == Domain::Core {
                        "verify_core"
                    } else {
                        "verify_comms"
                    }
                    .into(),
                    row: row("source", 1),
                }]
            };
            u.facts = vec![FactRecord {
                fact_id: format!("fact-{domain:?}"),
                topic: format!(
                    "verify.{}.fact",
                    steve_store_sqlite::domains::domain_name(domain)
                ),
                key: "k".into(),
                payload: RawJson::from_str("{ \"original\": true }").unwrap(),
            }];
            sign(&mut u);
            let committed = block_on(source.commit(u)).unwrap();
            assert!(committed.rev.0 > BLOCK);
            let session = block_on(source.begin_export(domain)).unwrap();
            let mut next = ExportCursor::default();
            loop {
                let batch = block_on(source.export(&session, next)).unwrap();
                assert_eq!(
                    steve_store_sqlite::transfer::batch_digest(&batch).unwrap(),
                    batch.digest
                );
                let id = batch.id.clone();
                let report = block_on(target.import(batch.clone())).unwrap();
                assert_eq!(report.batch_id, id);
                assert!(!report.replayed);
                assert!(block_on(target.import(batch.clone())).unwrap().replayed);
                match batch.next {
                    Some(cursor) => next = cursor,
                    None => break,
                }
            }
            let mut after = unit("after-import", vec![]);
            after.domain = domain;
            sign(&mut after);
            let actual = block_on(target.commit(after)).unwrap();
            assert!(
                actual.rev > session.rev,
                "rev must exceed imported rows and receipts"
            );
        }
        drop(source);
        drop(target);
        let reopened = open_store(target_dir.path(), reg, time);
        let rev = block_on(reopened.commit(unit("after-reopen", vec![])))
            .unwrap()
            .rev;
        assert!(rev.0 > BLOCK);
    }
    // A publisher attached to the target is never called by import.
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let events: Events = Arc::default();
    let log = events.clone();
    let time = Arc::new(Time::default());
    let _runtime = runtime().enter();
    let target = SqliteStore::new(
        vec![steve_store_sqlite::open::open_admin(dir.path()).unwrap()],
        Arc::new(Registry::verification().unwrap()),
        time,
        Arc::new(move |rev, items: Vec<String>| log.lock().unwrap().push((rev, items))),
        || false,
    )
    .unwrap();
    target.install_registered().unwrap();
    let srcdir = tempfile::tempdir().unwrap();
    prepare_layout(srcdir.path(), false);
    let source = open_store(
        srcdir.path(),
        Arc::new(Registry::verification().unwrap()),
        Arc::new(Time::default()),
    );
    source.install_registered().unwrap();
    block_on(source.commit(unit("one", vec![insert("one", 1)]))).unwrap();
    let session = block_on(source.begin_export(Domain::Core)).unwrap();
    let batch = block_on(source.export(&session, ExportCursor::default())).unwrap();
    block_on(target.import(batch)).unwrap();
    assert!(events.lock().unwrap().is_empty());
}
#[test]
fn imported_floor_discards_the_lower_part_of_an_unused_memory_block() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(steve_store_sqlite::schema::CONTRACT)
        .unwrap();
    let allocator = RevAllocator::open(&c).unwrap();
    assert_eq!(allocator.issue(&c, 1).unwrap()[0], Rev(1));
    allocator.advance_past(&c, Rev(20)).unwrap();
    assert_eq!(allocator.issue(&c, 1).unwrap()[0], Rev(21));
    allocator.advance_past(&c, Rev(BLOCK * 3)).unwrap();
    assert!(allocator.issue(&c, 1).unwrap()[0] > Rev(BLOCK * 3));
    let reopened = RevAllocator::open(&c).unwrap();
    assert!(reopened.issue(&c, 1).unwrap()[0] > Rev(BLOCK * 3));
    c.execute_batch("BEGIN").unwrap();
    assert!(allocator.advance_past(&c, Rev(30)).is_err());
    c.execute_batch("ROLLBACK").unwrap();
}
#[test]
fn registry_aliases_and_read_capabilities_use_the_real_services() {
    let mut registry = Registry::default();
    let mut desc = steve_store_sqlite::domains::fixtures::CORE.to_wire();
    desc.name = "logical".into();
    registry.register(desc, "physical").unwrap();
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let time = Arc::new(Time::default());
    let partial = open_store(dir.path(), Arc::new(registry), time.clone());
    partial.install_registered().unwrap();
    assert!(
        partial
            .hello()
            .domains
            .iter()
            .all(|o| o.features.contains("snapshot_read") && !o.features.contains("export"))
    );
    let mut u = unit(
        "alias",
        vec![Mutation::Insert {
            entity: "logical".into(),
            row: row("id", 5),
        }],
    );
    sign(&mut u);
    block_on(partial.commit(u)).unwrap();
    let value: Option<(Row, Rev)> = block_on(partial.query(get("logical", "id", Domain::Core)))
        .unwrap()
        .body
        .decode()
        .unwrap();
    assert!(value.is_some());
    let partial = partial.with_reads(Arc::new(NoReadServices));
    assert!(
        partial
            .hello()
            .domains
            .iter()
            .all(|o| !o.features.contains("snapshot_read") && !o.features.contains("export"))
    );
    assert!(block_on(partial.query(get("logical", "id", Domain::Core))).is_err());
    drop(partial);
    let mut registry = Registry::default();
    let mut desc = steve_store_sqlite::domains::fixtures::CORE.to_wire();
    desc.name = "logical".into();
    registry.register(desc, "physical").unwrap();
    registry.allow_export(Domain::Core).unwrap();
    let source = open_store(dir.path(), Arc::new(registry), time);
    let session = block_on(source.begin_export(Domain::Core)).unwrap();
    let batch = block_on(source.export(&session, ExportCursor::default())).unwrap();
    assert_eq!(batch.entity.name, "logical");
    block_on(source.end_export(&session)).unwrap();
    assert_eq!(
        block_on(source.export(&session, ExportCursor::default()))
            .unwrap_err()
            .code,
        steve_types::Code::Stale
    );
    let dir2 = tempfile::tempdir().unwrap();
    prepare_layout(dir2.path(), false);
    let mut registry = Registry::default();
    let mut desc = batch.entity.clone();
    desc.name = "logical".into();
    registry.register(desc, "different_physical").unwrap();
    registry.allow_export(Domain::Core).unwrap();
    let target = open_store(dir2.path(), Arc::new(registry), Arc::new(Time::default()));
    target.install_registered().unwrap();
    block_on(target.import(batch.clone())).unwrap();
    assert!(block_on(target.import(batch)).unwrap().replayed);
    assert!(
        block_on(target.query(get("logical", "id", Domain::Core)))
            .unwrap()
            .body
            .decode::<Option<(Row, Rev)>>()
            .unwrap()
            .is_some()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn a_blocked_named_query_does_not_block_the_async_executor() {
    let runtime_id = tokio::runtime::Handle::current().id();
    let (ready, started) = tokio::sync::oneshot::channel();
    let ready = Mutex::new(Some(ready));
    let (release, resume) = std::sync::mpsc::channel();
    let resume = Mutex::new(resume);
    let mut registry = Registry::verification().unwrap();
    registry
        .register_query("test.block", Domain::Core, move |_, _| {
            ready.lock().unwrap().take().unwrap().send(()).unwrap();
            assert_eq!(tokio::runtime::Handle::current().id(), runtime_id);
            resume.lock().unwrap().recv().unwrap();
            encode(&7)
        })
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let store = Arc::new(open_store(
        dir.path(),
        Arc::new(registry),
        Arc::new(Time::default()),
    ));
    store.install_registered().unwrap();
    let reader = store.clone();
    let task = tokio::spawn(async move {
        reader
            .query(AnyQuery {
                name: "test.block".into(),
                domain: Domain::Core,
                body: Payload::Json(encode(&()).unwrap()),
            })
            .await
    });
    started.await.unwrap();
    release.send(()).unwrap();
    assert_eq!(
        task.await.unwrap().unwrap().body.decode::<i32>().unwrap(),
        7
    );
}

#[test]
fn a_panicking_import_adapter_rolls_back_without_poisoning_the_writer() {
    let source_dir = tempfile::tempdir().unwrap();
    prepare_layout(source_dir.path(), false);
    let source = open_store(
        source_dir.path(),
        Arc::new(Registry::verification().unwrap()),
        Arc::new(Time::default()),
    );
    source.install_registered().unwrap();
    let mut u = unit("fact", vec![insert("row", 1)]);
    u.facts.push(FactRecord {
        fact_id: "fact".into(),
        topic: "verify.core.fact".into(),
        key: "k".into(),
        payload: RawJson::from_str("{}").unwrap(),
    });
    sign(&mut u);
    block_on(source.commit(u)).unwrap();
    let session = block_on(source.begin_export(Domain::Core)).unwrap();
    let batch = block_on(source.export(&session, ExportCursor::default())).unwrap();
    let mut registry = Registry::default();
    registry
        .register(batch.entity.clone(), batch.entity.name.clone())
        .unwrap();
    registry
        .register_transfer_topic(
            "verify.core.fact",
            Domain::Core,
            |_| panic!("adapter fault"),
            |raw| decode(raw).map_err(CommitError::Fatal),
        )
        .unwrap();
    registry.allow_export(Domain::Core).unwrap();
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let target = open_store(dir.path(), Arc::new(registry), Arc::new(Time::default()));
    target.install_registered().unwrap();
    assert_eq!(
        block_on(target.import(batch)).unwrap_err().code,
        steve_types::Code::Internal
    );
    assert!(
        block_on(target.query(get("verify_core", "row", Domain::Core)))
            .unwrap()
            .body
            .decode::<Option<(Row, Rev)>>()
            .unwrap()
            .is_none()
    );
    block_on(target.commit(unit("after-panic", vec![insert("usable", 2)]))).unwrap();
}

#[test]
fn named_query_panic_rolls_back_before_returning_the_reader() {
    let mut registry = Registry::verification().unwrap();
    registry
        .register_query("test.panic", Domain::Core, |c, _| {
            c.execute_batch("SAVEPOINT named_query").unwrap();
            panic!("query fault")
        })
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let store = open_store(dir.path(), Arc::new(registry), Arc::new(Time::default()));
    store.install_registered().unwrap();
    assert_eq!(
        block_on(store.query(AnyQuery {
            name: "test.panic".into(),
            domain: Domain::Core,
            body: Payload::Json(encode(&()).unwrap())
        }))
        .unwrap_err()
        .code,
        steve_types::Code::Internal
    );
    let mut readers = Vec::new();
    let count = store.engine(Domain::Core).readers().state().connections;
    for _ in 0..count {
        let c = store.engine(Domain::Core).read_conn().unwrap();
        assert!(c.is_autocommit());
        readers.push(c);
    }
}

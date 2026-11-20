#![cfg(feature = "conformance")]
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use steve_storage_api::{
    entity::*,
    query::*,
    registry::*,
    retention::*,
    uow::*,
    verify::{entities::*, mem::*, *},
    *,
};

fn unit(now: Millis, id: &str, mutations: Vec<Mutation>) -> UnitOfWork {
    let mut unit = UnitOfWork {
        domain: Domain::Core,
        id: IdempotencyKey {
            issued_at: now,
            key: id.into(),
        },
        digest: RequestDigest([0; 32]),
        lane: Lane::Interactive,
        idempotency: Idempotency::Recorded,
        preconditions: vec![],
        mutations,
        facts: vec![],
    };
    unit.digest = unit.request_digest().unwrap();
    unit
}
fn query(id: &str) -> AnyQuery {
    Get::<CoreRecord> {
        key: Key::new(vec![Cell::Text(id.into())]),
    }
    .to_wire()
    .unwrap()
}
#[test]
fn every_registered_name_requires_exactly_one_semantic_check() {
    let entry = steve_storage_api::ops::comms::digest::SYNC_PENDING_MEMBERSHIP;
    assert!(
        check_registry(ENTRIES, &[])
            .unwrap_err()
            .contains(entry.name)
    );
    let checks = digest::named_checks();
    assert!(check_registry(ENTRIES, &checks).is_ok());
    let mut future = entry;
    future.name = "verify.future_operation";
    let mut entries = ENTRIES.to_vec();
    entries.push(future);
    assert!(
        check_registry(&entries, &checks)
            .unwrap_err()
            .contains(future.name)
    );
    let mut wrong_domain = digest::named_checks();
    wrong_domain[0].domain = Domain::Core;
    assert!(check_registry(ENTRIES, &wrong_domain).is_err());
    let mut wrong_kind = digest::named_checks();
    wrong_kind[0].kind = EntryKind::Query;
    assert!(check_registry(ENTRIES, &wrong_kind).is_err());
    let mut duplicate = digest::named_checks();
    duplicate.extend(digest::named_checks());
    assert!(check_registry(ENTRIES, &duplicate).is_err());
}
#[test]
fn registry_matches_reference_backend_named_features() {
    let store = MemStore::new();
    for domain in [Domain::Core, Domain::Stream, Domain::Comms] {
        let expected: std::collections::BTreeSet<_> = ENTRIES
            .iter()
            .filter(|entry| entry.domain == domain)
            .map(|entry| {
                format!(
                    "{}:{}",
                    match entry.kind {
                        EntryKind::Operation => "op",
                        EntryKind::Query => "query",
                    },
                    entry.name
                )
            })
            .collect();
        let offer = store
            .hello()
            .domains
            .iter()
            .find(|offer| offer.domain == domain)
            .unwrap();
        let actual: std::collections::BTreeSet<_> = offer
            .features
            .iter()
            .filter(|feature| feature.starts_with("op:") || feature.starts_with("query:"))
            .cloned()
            .collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn digest_semantics_on_memory_and_loopback() {
    for loopback in [false, true] {
        let harness = MemHarness::new(loopback);
        digest::check(&harness, harness.open().unwrap());
    }
}
#[test]
fn forged_lease_cannot_evict_another_valid_lease() {
    let store = MemStore::new();
    let other = MemStore::new();
    let real = block_on(store.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    let elsewhere = block_on(other.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    assert_ne!(real.id, elsewhere.id);
    let mut forged = real.clone();
    forged.expires_at = Millis(0);
    assert!(block_on(store.lease_query(forged, query("absent"))).is_err());
    block_on(store.lease_query(real.clone(), query("absent"))).unwrap();
    assert!(block_on(other.lease_query(real.clone(), query("absent"))).is_err());
    block_on(store.lease_end(real)).unwrap();
}
#[test]
fn a_late_invalid_import_never_publishes_earlier_rows() {
    let source = MemStore::new();
    block_on(source.commit(unit(
        Millis(1_000_000),
        "export",
        vec![
            Mutation::Insert {
                entity: CORE.name.into(),
                row: row("a", 1),
            },
            Mutation::Insert {
                entity: CORE.name.into(),
                row: row("b", 2),
            },
        ],
    )))
    .unwrap();
    let session = block_on(source.begin_export(Domain::Core)).unwrap();
    let batch = block_on(source.export(
        &session,
        ExportCursor {
            entity: CORE.name.into(),
            after: None,
        },
    ))
    .unwrap();
    assert_eq!(batch.rows.len(), 2);
    let target = MemStore::new();
    let mut invalid = batch.clone();
    set(&mut invalid.rows[1].0, "value", Cell::Text("bad".into()));
    invalid.digest = batch_digest(&invalid).unwrap();
    assert!(block_on(target.import(invalid)).is_err());
    let result: Option<(CoreRecord, Rev)> = block_on(target.query(query("a")))
        .unwrap()
        .body
        .decode()
        .unwrap();
    assert!(result.is_none());
    assert!(!block_on(target.import(batch.clone())).unwrap().replayed);
    assert!(block_on(target.import(batch)).unwrap().replayed);
}

// Probe rejects any Local request and returns Local responses. Thus successful calls
// through Loopback demonstrate serialization in both directions, rather than just serde derives.
struct WireProbe {
    inner: MemStore,
    batches: AtomicUsize,
}
fn local_output(mut output: AnyOutput) -> AnyOutput {
    output.body = Payload::Local(Box::new(output.body.to_json().unwrap()));
    output
}
#[async_trait]
impl DomainStore for WireProbe {
    fn hello(&self) -> &Hello {
        self.inner.hello()
    }
    async fn query(&self, q: AnyQuery) -> Result<AnyOutput, Error> {
        assert!(matches!(q.body, Payload::Json(_)));
        self.inner.query(q).await.map(local_output)
    }
    async fn snapshot(&self, d: Domain, qs: Vec<AnyQuery>) -> Result<Vec<AnyOutput>, Error> {
        assert!(qs.iter().all(|q| matches!(q.body, Payload::Json(_))));
        self.inner
            .snapshot(d, qs)
            .await
            .map(|out| out.into_iter().map(local_output).collect())
    }
    async fn begin_snapshot(&self, d: Domain, l: Lane) -> Result<LeaseId, Error> {
        self.inner.begin_snapshot(d, l).await
    }
    async fn lease_query(&self, l: LeaseId, q: AnyQuery) -> Result<AnyOutput, Error> {
        assert!(matches!(q.body, Payload::Json(_)));
        self.inner.lease_query(l, q).await.map(local_output)
    }
    async fn lease_end(&self, l: LeaseId) -> Result<(), Error> {
        self.inner.lease_end(l).await
    }
    async fn commit(&self, u: UnitOfWork) -> Result<Committed, CommitError> {
        self.inner.commit(u).await
    }
    async fn commit_many(&self, us: Vec<UnitOfWork>) -> Vec<Result<Committed, CommitError>> {
        self.batches.fetch_add(1, Ordering::SeqCst);
        self.inner.commit_many(us).await
    }
    async fn begin_export(&self, d: Domain) -> Result<ExportSession, Error> {
        self.inner.begin_export(d).await
    }
    async fn export(&self, s: &ExportSession, c: ExportCursor) -> Result<ExportBatch, Error> {
        self.inner.export(s, c).await
    }
    async fn import(&self, b: ExportBatch) -> Result<ImportReport, Error> {
        self.inner.import(b).await
    }
    async fn retention(&self, e: String, b: Budget) -> Result<RetentionReport, Error> {
        self.inner.retention(e, b).await
    }
}
fn local_query() -> AnyQuery {
    AnyQuery {
        name: "get".into(),
        domain: Domain::Core,
        body: Payload::Local(Box::new(Get::<CoreRecord> {
            key: Key::new(vec![Cell::Text("absent".into())]),
        })),
    }
}
#[test]
fn loopback_serializes_local_both_ways_and_keeps_batch_errors() {
    let probe = Arc::new(WireProbe {
        inner: MemStore::new(),
        batches: AtomicUsize::new(0),
    });
    let wire = loopback::Loopback::new(probe.clone()).unwrap();
    let out = block_on(wire.query(local_query())).unwrap();
    assert!(matches!(out.body, Payload::Json(_)));
    let outputs = block_on(wire.snapshot(Domain::Core, vec![local_query()])).unwrap();
    assert!(matches!(outputs[0].body, Payload::Json(_)));
    let lease = block_on(wire.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    let out = block_on(wire.lease_query(lease.clone(), local_query())).unwrap();
    assert!(matches!(out.body, Payload::Json(_)));
    block_on(wire.lease_end(lease)).unwrap();
    let now = Millis(1_000_000);
    let a = unit(
        now,
        "a",
        vec![Mutation::Insert {
            entity: CORE.name.into(),
            row: row("a", 1),
        }],
    );
    let b = unit(
        now,
        "b",
        vec![Mutation::Insert {
            entity: CORE.name.into(),
            row: row("a", 2),
        }],
    );
    let c = unit(
        now,
        "c",
        vec![Mutation::Insert {
            entity: CORE.name.into(),
            row: row("c", 3),
        }],
    );
    let results = block_on(wire.commit_many(vec![a, b, c]));
    assert_eq!(probe.batches.load(Ordering::SeqCst), 1);
    assert!(results[0].is_ok());
    assert!(matches!(results[1], Err(CommitError::Unique { .. })));
    assert!(results[2].is_ok());
    let mut invalid = unit(
        now,
        "bad",
        vec![Mutation::Insert {
            entity: CORE.name.into(),
            row: row("bad", 1),
        }],
    );
    if let Mutation::Insert { row, .. } = &mut invalid.mutations[0] {
        set(row, "real", Cell::Real(f64::NAN));
    }
    let results = block_on(wire.commit_many(vec![
        unit(now, "before", vec![]),
        invalid,
        unit(now, "after", vec![]),
    ]));
    assert!(results[0].is_ok());
    assert!(matches!(
        results[1],
        Err(CommitError::Fatal(Error {
            code: steve_types::Code::Invalid,
            ..
        }))
    ));
    assert!(results[2].is_ok());
    assert_eq!(probe.batches.load(Ordering::SeqCst), 3);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Typed {
    id: String,
    value: i64,
}
steve_storage_api::entity_desc!(pub TYPED={name:"verify_typed",domain:Domain::Core,key:&["id"],fields:&[Field{name:"id",ty:StaticFieldType::Text,nullable:false},Field{name:"value",ty:StaticFieldType::Int,nullable:false}],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
impl ToRow for Typed {
    fn to_row(&self) -> Result<Row, Error> {
        Ok(Row(vec![
            ("id".into(), Cell::Text(self.id.clone())),
            ("value".into(), Cell::Int(self.value)),
        ]))
    }
}
impl FromRow for Typed {
    fn from_row(row: Row) -> Result<Self, Error> {
        match (cell(&row, "id"), cell(&row, "value")) {
            (Some(Cell::Text(id)), Some(Cell::Int(value))) => Ok(Self {
                id: id.clone(),
                value: *value,
            }),
            _ => Err(error(steve_types::Code::Invalid, "typed row")),
        }
    }
}
impl StoredEntity for Typed {
    const ENTITY: &'static Entity = &TYPED;
}
#[test]
fn registered_domain_codec_returns_domain_owned_rows() {
    let clock = Arc::new(ManualClock::new(Millis(1_000_000)));
    let mut store = MemStore::with_entities(vec![], clock).unwrap();
    store.register::<Typed>().unwrap();
    let wire = loopback::Loopback::new(Arc::new(store)).unwrap();
    let expected = Typed {
        id: "typed".into(),
        value: 42,
    };
    block_on(wire.commit(unit(
        Millis(1_000_000),
        "typed",
        vec![Mutation::Insert {
            entity: TYPED.name.into(),
            row: expected.to_row().unwrap(),
        }],
    )))
    .unwrap();
    let query = Get::<Typed> {
        key: Key::new(vec![Cell::Text("typed".into())]),
    };
    let result = block_on(wire.query(query.to_wire().unwrap()))
        .unwrap()
        .decode::<Get<Typed>>()
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(result, expected);
}

#[test]
fn retention_variants_preserve_live_rows_and_report_real_changes() {
    for selection in [
        Select::MaxRows {
            group: vec![],
            keep: 2,
            order: vec![("value".into(), Order::Desc)],
        },
        Select::Terminal {
            state: "flag".into(),
            done: vec![Cell::Bool(true)],
            since: "at".into(),
            keep_ms: 1000,
        },
        Select::Bytes {
            group: vec![],
            max: 0,
        },
    ] {
        let mut entity = LOG.to_wire();
        entity.retention = Some(RetentionPolicy {
            entity: LOG.name.into(),
            select: selection.clone(),
            keep_from: KeepFrom::Setting {
                key: "keepVerify".into(),
                per: SettingScope::Workspace,
                default_ms: 1000,
            },
            before: None,
            protect: None,
            action: Action::Delete,
            budget: Budget::default(),
        });
        let clock = Arc::new(ManualClock::new(Millis(1_000_000)));
        let store = MemStore::with_entities(vec![entity], clock.clone()).unwrap();
        let rows = (0..4)
            .map(|n| {
                let mut row = row(format!("row-{n}"), n);
                set(&mut row, "at", Cell::Millis(0));
                set(&mut row, "flag", Cell::Bool(n % 2 == 0));
                Mutation::Insert {
                    entity: LOG.name.into(),
                    row,
                }
            })
            .collect();
        block_on(store.commit(unit(clock.now(), "retention", rows))).unwrap();
        let report = block_on(store.retention(LOG.name.into(), Budget::default())).unwrap();
        assert_eq!(
            report.affected,
            if matches!(selection, Select::Bytes { .. }) {
                4
            } else {
                2
            }
        );
        assert!(!report.more);
    }
    let mut entity = LOG.to_wire();
    entity.retention = Some(RetentionPolicy {
        entity: LOG.name.into(),
        select: Select::MaxAge {
            field: "at".into(),
            keep_ms: 1,
        },
        keep_from: KeepFrom::Fixed { millis: 1 },
        before: None,
        protect: None,
        action: Action::Purge {
            fields: vec!["data".into()],
        },
        budget: Budget::default(),
    });
    let store =
        MemStore::with_entities(vec![entity], Arc::new(ManualClock::new(Millis(1_000_000))))
            .unwrap();
    let mut record = row("purge", 1);
    set(
        &mut record,
        "data",
        Cell::Json(RawJson::from_str("{\"value\":1}").unwrap()),
    );
    block_on(store.commit(unit(
        Millis(1_000_000),
        "purge",
        vec![Mutation::Insert {
            entity: LOG.name.into(),
            row: record,
        }],
    )))
    .unwrap();
    assert_eq!(
        block_on(store.retention(LOG.name.into(), Budget::default()))
            .unwrap()
            .affected,
        1
    );
    assert_eq!(
        block_on(store.retention(LOG.name.into(), Budget::default()))
            .unwrap()
            .affected,
        0
    );
}
#[test]
fn executor_honors_pending_wakeups_without_a_runtime() {
    let mut pending = true;
    let mut worker = None;
    let value = block_on(std::future::poll_fn(|context| {
        if pending {
            pending = false;
            let waker = context.waker().clone();
            worker = Some(std::thread::spawn(move || waker.wake()));
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(42)
        }
    }));
    assert_eq!(value, 42);
    worker.unwrap().join().unwrap();
}

#[test]
fn malformed_keys_ranges_and_filters_fail_instead_of_looking_empty() {
    let store = MemStore::new();
    let query = Get::<CoreRecord> {
        key: Key::new(vec![Cell::Int(42)]),
    };
    assert!(matches!(
        block_on(store.query(query.to_wire().unwrap())),
        Err(Error {
            code: steve_types::Code::Invalid,
            ..
        })
    ));
    let query = Scan::<CoreRecord> {
        index: IndexName {
            entity: CORE.name.into(),
            name: "value".into(),
        },
        range: KeyRange {
            start: KeyBound::Included(vec![Cell::Text("wrong".into())]),
            end: KeyBound::Unbounded,
        },
        filter: vec![],
        order: Order::Asc,
        limit: 1,
        budget: ScanBudget(1),
        resume: None,
        marker: std::marker::PhantomData,
    };
    assert!(matches!(
        block_on(store.query(query.to_wire().unwrap())),
        Err(Error {
            code: steve_types::Code::Invalid,
            ..
        })
    ));
    let query = Scan::<CoreRecord> {
        range: KeyRange::default(),
        filter: vec![Pred::Eq {
            field: "value".into(),
            value: Cell::Text("wrong".into()),
        }],
        ..query
    };
    assert!(matches!(
        block_on(store.query(query.to_wire().unwrap())),
        Err(Error {
            code: steve_types::Code::Invalid,
            ..
        })
    ));
}

#[test]
fn facts_roll_back_with_mutations_when_a_late_fact_is_duplicate() {
    let store = MemStore::new();
    let fact = steve_storage_api::fact::FactRecord {
        fact_id: "same-fact".into(),
        topic: "verify.core.fact".into(),
        key: "key".into(),
        payload: RawJson::from_str("{}").unwrap(),
    };
    let mut seed = unit(Millis(1_000_000), "fact-seed", vec![]);
    seed.facts.push(fact.clone());
    seed.digest = seed.request_digest().unwrap();
    block_on(store.commit(seed)).unwrap();
    let mut failing = unit(
        Millis(1_000_000),
        "fact-rollback",
        vec![Mutation::Insert {
            entity: CORE.name.into(),
            row: row("rolled-back", 42),
        }],
    );
    failing.facts.push(fact);
    failing.digest = failing.request_digest().unwrap();
    assert!(matches!(
        block_on(store.commit(failing)),
        Err(CommitError::Unique { .. })
    ));
    let row: Option<(CoreRecord, Rev)> = block_on(store.query(query("rolled-back")))
        .unwrap()
        .body
        .decode()
        .unwrap();
    assert!(row.is_none());
}

#[test]
fn reference_backend_rejects_the_reserved_revision_column() {
    let mut desc = CORE.to_wire();
    desc.fields.push(FieldDesc {
        name: "rev".into(),
        ty: FieldType::Int,
        nullable: false,
    });
    let result = MemStore::with_entities(vec![desc], Arc::new(ManualClock::new(Millis(1_000_000))));
    assert!(matches!(
        result,
        Err(Error {
            code: steve_types::Code::Invalid,
            ..
        })
    ));
}

#[test]
fn cursor_commit_does_not_prune_or_replace_old_recorded_receipts() {
    #[derive(Deserialize)]
    struct Ledger {
        receipts: Vec<serde::de::IgnoredAny>,
    }
    fn count(store: &MemStore) -> usize {
        let session = block_on(store.begin_export(Domain::Core)).unwrap();
        let batch = block_on(store.export(&session, ExportCursor::default())).unwrap();
        decode::<Ledger>(&batch.state).unwrap().receipts.len()
    }
    let clock = Arc::new(ManualClock::new(Millis(1_000_000)));
    let store = MemStore::with_entities(descriptors(), clock.clone()).unwrap();
    block_on(store.commit(unit(clock.now(), "old", vec![]))).unwrap();
    clock.advance(32 * 86_400_000);
    assert_eq!(count(&store), 1);
    let mut u = unit(
        clock.now(),
        "cursor",
        vec![Mutation::AdvanceCursor {
            name: steve_storage_api::cursor::CursorName("consumer".into()),
            to: steve_storage_api::cursor::Position(steve_storage_api::cursor::CursorKey::Int(1)),
        }],
    );
    u.idempotency = Idempotency::Cursor;
    u.preconditions.push(Precondition::Cursor {
        name: steve_storage_api::cursor::CursorName("consumer".into()),
        at: None,
    });
    u.digest = u.request_digest().unwrap();
    block_on(store.commit(u)).unwrap();
    assert_eq!(
        count(&store),
        1,
        "cursor mode must leave the entire recorded ledger untouched"
    );
}

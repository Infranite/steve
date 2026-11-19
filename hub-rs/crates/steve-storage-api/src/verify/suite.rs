//! Backend-independent scenarios. Every open must create fresh fixture state.
use super::{
    block_on,
    entities::*,
    mem::{Clock, ManualClock, MemStore},
};
use crate::{
    append::StreamName, cursor::*, entity::Row, query::*, registry::*, retention::Budget, uow::*, *,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeSet,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Barrier, atomic::Ordering},
    thread,
    time::Instant,
};
use steve_types::Code;

pub const SUITE_VERSION: u32 = 1;
pub const SEED: u64 = 0x57e7_e202_6111_9002;
#[derive(Clone, Copy, Debug)]
pub enum Capability {
    Reopen,
    Crash,
    AdvanceClock,
    Interrupt,
}
/// Evidence returned by a backend-specific subprocess harness. The harness must use an
/// actual forced process termination while a submitted commit is in flight, not graceful
/// shutdown. Storage handles in `store` refer to the recovered namespace.
pub struct CrashOutcome {
    pub store: Arc<dyn DomainStore>,
    pub killed_during_commit: bool,
}
/// Backend lifecycle seam. A fresh open installs entities::descriptors() and fixture topics.
/// Backends may choose their own files/processes without exposing them to the suite.
pub trait Harness: Send + Sync {
    fn open(&self) -> Result<Arc<dyn DomainStore>, String>;
    fn now(&self) -> Millis;
    fn unavailable(&self, capability: Capability) -> Option<String> {
        Some(format!("harness does not provide {capability:?}"))
    }
    fn reopen(&self, _store: Arc<dyn DomainStore>) -> Result<Arc<dyn DomainStore>, String> {
        Err("reopen unavailable".into())
    }
    fn crash_commit(
        &self,
        _store: Arc<dyn DomainStore>,
        _uow: UnitOfWork,
    ) -> Result<CrashOutcome, String> {
        Err("crash unavailable".into())
    }
    fn advance_clock(&self, _millis: i64) -> Result<(), String> {
        Err("clock unavailable".into())
    }
    /// Arrange a blocked lease query, advance past expiry while it is executing, and
    /// return only after checking stale plus successful resource reuse.
    fn interrupt_expired_query(&self, _store: Arc<dyn DomainStore>) -> Result<(), String> {
        Err("interruption unavailable".into())
    }
}
/// Reference harness, optionally adding the serialization boundary.
pub struct MemHarness {
    pub loopback: bool,
    clock: Arc<ManualClock>,
}
impl MemHarness {
    pub fn new(loopback: bool) -> Self {
        Self {
            loopback,
            clock: Arc::new(ManualClock::new(Millis(1_000_000))),
        }
    }
}
impl Harness for MemHarness {
    fn open(&self) -> Result<Arc<dyn DomainStore>, String> {
        let store = Arc::new(
            MemStore::with_entities(descriptors(), self.clock.clone())
                .map_err(|e| e.to_string())?,
        );
        if self.loopback {
            Ok(Arc::new(
                super::loopback::Loopback::new(store).map_err(|e| e.to_string())?,
            ))
        } else {
            Ok(store)
        }
    }
    fn now(&self) -> Millis {
        self.clock.now()
    }
    fn unavailable(&self, cap: Capability) -> Option<String> {
        match cap {
            Capability::AdvanceClock => None,
            Capability::Reopen => Some("memory has no persistent medium".into()),
            Capability::Crash => Some("memory has no crashable storage process".into()),
            Capability::Interrupt => Some("memory has no interruptible in-flight statement".into()),
        }
    }
    fn advance_clock(&self, millis: i64) -> Result<(), String> {
        self.clock.advance(millis);
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemReport {
    pub name: String,
    pub status: Status,
    pub elapsed_us: u64,
    pub detail: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub name: String,
    pub samples: u64,
    pub p50_us: u64,
    pub p99_us: u64,
    pub warning: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub suite_version: u32,
    pub seed: u64,
    pub implementation: String,
    pub artifact_sha256: Option<String>,
    pub items: Vec<ItemReport>,
    pub measurements: Vec<Measurement>,
}
impl Report {
    pub fn failed(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == Status::Failed)
            .count()
    }
    pub fn skipped(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == Status::Skipped)
            .count()
    }
    pub fn passed(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == Status::Passed)
            .count()
    }
    /// Required capability skips do not count as successful conformance.
    pub fn all_passed(&self) -> bool {
        self.items.iter().all(|i| i.status == Status::Passed)
    }
}
type Check = fn(&dyn Harness, Arc<dyn DomainStore>);
/// Named coverage is explicit: adding an implemented catalogue entry requires a semantic
/// function, not merely a declaration that the name exists.
pub struct NamedCheck {
    pub name: &'static str,
    pub domain: Domain,
    pub kind: EntryKind,
    pub check: Check,
}
pub fn check_registry(entries: &[RegistryEntry], checks: &[NamedCheck]) -> Result<(), String> {
    for e in entries.iter().filter(|e| e.implemented) {
        if checks
            .iter()
            .filter(|c| c.name == e.name && c.domain == e.domain && c.kind == e.kind)
            .count()
            != 1
        {
            return Err(format!("missing or duplicate semantic check: {}", e.name));
        }
    }
    Ok(())
}
fn request<T: Serialize>(name: &str, domain: Domain, body: &T) -> AnyQuery {
    AnyQuery {
        name: name.into(),
        domain,
        body: Payload::Json(encode(body).unwrap()),
    }
}
fn result<T: DeserializeOwned>(store: &dyn DomainStore, q: AnyQuery) -> T {
    block_on(store.query(q)).unwrap().body.decode().unwrap()
}
fn lookup(domain: Domain, entity: &str, id: &str) -> AnyQuery {
    request(
        "get",
        domain,
        &Get {
            key: Key::<Row> {
                entity: entity.into(),
                values: vec![Cell::Text(id.into())],
                marker: PhantomData,
            },
        },
    )
}
fn get(store: &dyn DomainStore, id: &str) -> Option<(Row, Rev)> {
    result(store, lookup(Domain::Core, CORE.name, id))
}
// This fixture builder mirrors the independent fields of the public Scan DTO.
#[allow(clippy::too_many_arguments)]
fn scan(
    domain: Domain,
    entity: &str,
    index: &str,
    limit: u32,
    budget: u32,
    resume: Option<ResumeToken>,
    filter: Vec<Pred>,
    order: Order,
) -> AnyQuery {
    request(
        "scan",
        domain,
        &Scan::<Row> {
            index: IndexName {
                entity: entity.into(),
                name: index.into(),
            },
            range: KeyRange::default(),
            filter,
            order,
            limit,
            budget: ScanBudget(budget),
            resume,
            marker: PhantomData,
        },
    )
}
fn page(store: &dyn DomainStore, domain: Domain, entity: &str) -> Page<Row> {
    result(
        store,
        scan(domain, entity, "", 500, 5000, None, vec![], Order::Asc),
    )
}
fn value(row: &Row) -> i64 {
    match cell(row, "value").unwrap() {
        Cell::Int(v) => *v,
        _ => panic!("expected Int"),
    }
}
fn insert(entity: &str, id: &str, n: i64) -> Mutation {
    Mutation::Insert {
        entity: entity.into(),
        row: row(id, n),
    }
}
fn update(id: &str, n: i64) -> Mutation {
    Mutation::Update {
        entity: CORE.name.into(),
        key: vec![Cell::Text(id.into())],
        set: Row(vec![("value".into(), Cell::Int(n))]),
    }
}
pub(super) fn uow(
    h: &dyn Harness,
    domain: Domain,
    id: impl Into<String>,
    mutations: Vec<Mutation>,
) -> UnitOfWork {
    let mut unit = UnitOfWork {
        domain,
        id: IdempotencyKey {
            issued_at: h.now(),
            key: id.into(),
        },
        digest: RequestDigest([0; 32]),
        lane: Lane::Interactive,
        idempotency: Idempotency::Recorded,
        preconditions: vec![],
        mutations,
        facts: vec![],
    };
    sign(&mut unit);
    unit
}
fn sign(unit: &mut UnitOfWork) {
    unit.digest = unit.request_digest().unwrap();
}
fn commit(store: &dyn DomainStore, unit: UnitOfWork) -> Committed {
    block_on(store.commit(unit)).unwrap()
}
fn require(id: &str, rev: Rev) -> Precondition {
    Precondition::Rev {
        entity: CORE.name.into(),
        key: vec![Cell::Text(id.into())],
        rev,
    }
}
fn cursor(store: &dyn DomainStore, name: &str) -> Option<Position> {
    result(
        store,
        request(
            "cursor_get",
            Domain::Core,
            &CursorGet {
                domain: Domain::Core,
                name: CursorName(name.into()),
            },
        ),
    )
}
fn append(entity: &str, id: &str) -> Mutation {
    Mutation::Append {
        stream: StreamName(entity.into()),
        records: vec![Row(vec![
            ("id".into(), Cell::Text(id.into())),
            ("value".into(), Cell::Int(1)),
        ])],
    }
}
fn fact(id: &str) -> crate::fact::FactRecord {
    crate::fact::FactRecord {
        fact_id: id.into(),
        topic: "verify.core.fact".into(),
        key: "key".into(),
        payload: RawJson::from_str("{ \"b\":2, \"a\":1 }").unwrap(),
    }
}

/// Run every scenario in stable order. Panics become failures; unavailable lifecycle
/// capabilities alone become skips. The result is intentionally not an activation decision.
pub fn run(h: &dyn Harness) -> Report {
    let named: Vec<_> = super::digest::named_checks()
        .into_iter()
        .filter(|check| {
            ENTRIES.iter().any(|e| {
                e.implemented
                    && e.name == check.name
                    && e.domain == check.domain
                    && e.kind == check.kind
            })
        })
        .collect();
    run_with_named(h, &named)
}
pub fn run_with_named(h: &dyn Harness, named: &[NamedCheck]) -> Report {
    let cases: &[(&str, Option<Capability>, Check)] = &[
        ("atomicity", None, atomicity),
        ("lost_update", None, lost_update),
        ("write_skew", None, write_skew),
        ("transfer_snapshot", None, transfer_snapshot),
        ("durability", Some(Capability::Reopen), durability),
        ("crash_atomicity", Some(Capability::Crash), crash_atomicity),
        ("idempotency_recorded", None, idempotency_recorded),
        ("idempotency_mismatch", None, idempotency_mismatch),
        (
            "idempotency_expiry",
            Some(Capability::AdvanceClock),
            idempotency_expiry,
        ),
        ("idempotency_natural", None, idempotency_natural),
        (
            "idempotency_after_crash",
            Some(Capability::Crash),
            idempotency_after_crash,
        ),
        ("append_order", None, append_order),
        ("snapshot_isolation", None, snapshot_isolation),
        ("unique_and_c_order", None, unique_and_c_order),
        ("encoding_precision", None, encoding_precision),
        ("performance_baseline", None, performance_baseline),
        ("lease_expiry", Some(Capability::AdvanceClock), lease_expiry),
        (
            "lease_interruption",
            Some(Capability::Interrupt),
            lease_interruption,
        ),
        ("snapshot_domain", None, snapshot_domain),
        ("uow_domain", None, uow_domain),
        ("cursor_monotonic", None, cursor_monotonic),
        ("scan_window", None, scan_window),
        ("transfer_roundtrip", None, transfer_roundtrip),
        (
            "retention_report",
            Some(Capability::AdvanceClock),
            retention_report,
        ),
    ];
    let mut report = Report {
        suite_version: SUITE_VERSION,
        seed: SEED,
        implementation: String::new(),
        artifact_sha256: None,
        items: vec![],
        measurements: vec![],
    };
    for (name, cap, check) in cases {
        if let Some(reason) = cap.and_then(|c| h.unavailable(c)) {
            report.items.push(ItemReport {
                name: (*name).into(),
                status: Status::Skipped,
                elapsed_us: 0,
                detail: Some(reason),
            });
            continue;
        }
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let store = h.open().expect("open fixture store");
            if report.implementation.is_empty() {
                report.implementation = store.hello().implementation.clone();
            }
            check(h, store);
        }));
        report.items.push(item(name, started, outcome));
    }
    let started = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| check_registry(ENTRIES, named).unwrap()));
    report.items.push(item("named_registry", started, outcome));
    for check in named {
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| (check.check)(h, h.open().unwrap())));
        report.items.push(item(check.name, started, outcome));
    }
    // Advisory measurements live outside correctness statuses and never gate activation.
    if let Ok(store) = h.open() {
        report.measurements =
            catch_unwind(AssertUnwindSafe(|| measure(h, store.as_ref()))).unwrap_or_default();
    }
    report
}
fn item(
    name: &str,
    started: Instant,
    outcome: Result<(), Box<dyn std::any::Any + Send>>,
) -> ItemReport {
    let (status, detail) = match outcome {
        Ok(()) => (Status::Passed, None),
        Err(panic) => (
            Status::Failed,
            Some(
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).into()))
                    .unwrap_or_else(|| "scenario panicked".into()),
            ),
        ),
    };
    ItemReport {
        name: name.into(),
        status,
        elapsed_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        detail,
    }
}

fn atomicity(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    for secondary in [false, true] {
        let isolated = h.open().unwrap();
        let mut existing = row("existing", 0);
        set(&mut existing, "label", Cell::Text("taken".into()));
        commit(
            isolated.as_ref(),
            uow(
                h,
                Domain::Core,
                "seed",
                vec![Mutation::Insert {
                    entity: CORE.name.into(),
                    row: existing,
                }],
            ),
        );
        let mut duplicate = row(if secondary { "other" } else { "existing" }, 2);
        set(&mut duplicate, "label", Cell::Text("taken".into()));
        let mut unit = uow(
            h,
            Domain::Core,
            "atomic",
            vec![
                insert(CORE.name, "sentinel", 1),
                append(CORE_STREAM.name, "atomic"),
                Mutation::AdvanceCursor {
                    name: CursorName("atomic".into()),
                    to: Position(CursorKey::Int(1)),
                },
                Mutation::Insert {
                    entity: CORE.name.into(),
                    row: duplicate,
                },
            ],
        );
        unit.facts.push(fact("atomic"));
        sign(&mut unit);
        assert!(matches!(
            block_on(isolated.commit(unit.clone())),
            Err(CommitError::Unique { .. })
        ));
        assert!(get(isolated.as_ref(), "sentinel").is_none());
        assert!(cursor(isolated.as_ref(), "atomic").is_none());
        assert!(
            page(isolated.as_ref(), Domain::Core, CORE_STREAM.name)
                .rows
                .is_empty()
        );
        unit.mutations.pop();
        sign(&mut unit);
        let result = commit(isolated.as_ref(), unit);
        assert!(!result.replayed);
    }
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "seed",
            vec![insert(CORE.name, "existing", 0)],
        ),
    );
    let units = vec![
        uow(h, Domain::Core, "batch-a", vec![insert(CORE.name, "a", 1)]),
        uow(
            h,
            Domain::Core,
            "batch-b",
            vec![insert(CORE.name, "b", 1), insert(CORE.name, "existing", 1)],
        ),
        uow(h, Domain::Core, "batch-c", vec![insert(CORE.name, "c", 1)]),
    ];
    let results = block_on(store.commit_many(units));
    assert_eq!(results.len(), 3);
    assert!(results[0].is_ok());
    assert!(matches!(results[1], Err(CommitError::Unique { .. })));
    assert!(results[2].is_ok());
    assert!(get(store.as_ref(), "b").is_none());
}
fn lost_update(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let first = commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "counter-seed",
            vec![insert(CORE.name, "counter", 0)],
        ),
    );
    let mut stale = uow(h, Domain::Core, "forced-stale", vec![update("counter", 0)]);
    stale
        .preconditions
        .push(require("counter", Rev(first.rev.0 + 1)));
    sign(&mut stale);
    assert!(matches!(
        block_on(store.commit(stale)),
        Err(CommitError::Conflict(_))
    ));
    let barrier = Barrier::new(16);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|worker| {
                let store = store.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let mut last = Rev(0);
                    for n in 0..1000 {
                        let mut attempts = 0;
                        loop {
                            attempts += 1;
                            assert!(attempts < 100_000, "CAS retry bound");
                            let (row, rev) = get(store.as_ref(), "counter").unwrap();
                            let mut unit = uow(
                                h,
                                Domain::Core,
                                format!("cas-{worker}-{n}"),
                                vec![update("counter", value(&row) + 1)],
                            );
                            unit.preconditions.push(require("counter", rev));
                            sign(&mut unit);
                            match block_on(store.commit(unit)) {
                                Ok(done) => {
                                    assert!(done.rev > rev && done.rev > last);
                                    last = done.rev;
                                    break;
                                }
                                Err(CommitError::Conflict(failed)) => {
                                    assert!(!failed.is_empty());
                                    thread::yield_now();
                                }
                                Err(e) => panic!("unexpected CAS error: {e}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    });
    assert_eq!(value(&get(store.as_ref(), "counter").unwrap().0), 16000);
}
fn write_skew(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    for round in 0..16 {
        let barrier = Barrier::new(2);
        let names = [format!("a-{round}"), format!("b-{round}")];
        let outcomes = thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|worker| {
                    let names = &names;
                    let barrier = &barrier;
                    let store = store.clone();
                    scope.spawn(move || {
                        let prepared = catch_unwind(AssertUnwindSafe(|| {
                            assert!(get(store.as_ref(), &names[0]).is_none());
                            assert!(get(store.as_ref(), &names[1]).is_none());
                            let mut unit = uow(
                                h,
                                Domain::Core,
                                format!("skew-{round}-{worker}"),
                                vec![insert(CORE.name, &names[worker], 1)],
                            );
                            unit.preconditions = names
                                .iter()
                                .map(|id| Precondition::Absent {
                                    entity: CORE.name.into(),
                                    key: vec![Cell::Text(id.clone())],
                                })
                                .collect();
                            sign(&mut unit);
                            unit
                        }));
                        // Every client reaches the rendezvous even if its initial read
                        // fails; a bad backend must produce a report, not hang its peer.
                        barrier.wait();
                        let unit =
                            prepared.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                        block_on(store.commit(unit))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|r| matches!(r, Err(CommitError::Conflict(_))))
                .count(),
            1
        );
    }
}
struct Working<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for Working<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
fn transfer_snapshot(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "accounts",
            (0..4)
                .map(|n| insert(CORE.name, &n.to_string(), 1000))
                .collect(),
        ),
    );
    let remaining = std::sync::atomic::AtomicUsize::new(4);
    let barrier = Barrier::new(5);
    let mut samples = 0;
    thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|worker| {
                let store = store.clone();
                let remaining = &remaining;
                let barrier = &barrier;
                scope.spawn(move || {
                    let _working = Working(remaining);
                    barrier.wait();
                    let mut seed = SEED + worker as u64;
                    for n in 0..200 {
                        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                        let a = (seed % 4).to_string();
                        let b = ((seed / 4 % 3 + (seed % 4) + 1) % 4).to_string();
                        for attempt in 0..100_000 {
                            let outputs = block_on(store.snapshot(
                                Domain::Core,
                                vec![
                                    lookup(Domain::Core, CORE.name, &a),
                                    lookup(Domain::Core, CORE.name, &b),
                                ],
                            ))
                            .unwrap();
                            let left: Option<(Row, Rev)> = outputs[0].body.decode().unwrap();
                            let right: Option<(Row, Rev)> = outputs[1].body.decode().unwrap();
                            let (left, lr) = left.unwrap();
                            let (right, rr) = right.unwrap();
                            if value(&left) == 0 {
                                break;
                            }
                            let mut unit = uow(
                                h,
                                Domain::Core,
                                format!("transfer-{worker}-{n}"),
                                vec![update(&a, value(&left) - 1), update(&b, value(&right) + 1)],
                            );
                            unit.preconditions = vec![require(&a, lr), require(&b, rr)];
                            sign(&mut unit);
                            match block_on(store.commit(unit)) {
                                Ok(_) => break,
                                Err(CommitError::Conflict(_)) => {
                                    assert!(attempt < 99_999);
                                    thread::yield_now();
                                }
                                Err(e) => panic!("transfer: {e}"),
                            }
                        }
                    }
                })
            })
            .collect();
        barrier.wait();
        loop {
            let outputs = block_on(
                store.snapshot(
                    Domain::Core,
                    (0..4)
                        .map(|n| lookup(Domain::Core, CORE.name, &n.to_string()))
                        .collect(),
                ),
            )
            .unwrap();
            let balances: Vec<_> = outputs
                .iter()
                .map(|out| {
                    let row: Option<(Row, Rev)> = out.body.decode().unwrap();
                    value(&row.unwrap().0)
                })
                .collect();
            assert_eq!(balances.iter().sum::<i64>(), 4000);
            assert!(balances.iter().all(|v| *v >= 0));
            samples += 1;
            if remaining.load(Ordering::SeqCst) == 0 {
                break;
            }
            thread::yield_now();
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });
    assert!(samples > 0);
}
fn durable_unit(h: &dyn Harness) -> UnitOfWork {
    let mut unit = uow(
        h,
        Domain::Core,
        "durable",
        vec![
            insert(CORE.name, "durable", 42),
            append(CORE_STREAM.name, "durable"),
            Mutation::AdvanceCursor {
                name: CursorName("durable".into()),
                to: Position(CursorKey::Int(42)),
            },
        ],
    );
    unit.facts.push(fact("durable"));
    sign(&mut unit);
    unit
}
fn durability(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let unit = durable_unit(h);
    let original = commit(store.as_ref(), unit.clone());
    let reopened = h.reopen(store).unwrap();
    assert_eq!(value(&get(reopened.as_ref(), "durable").unwrap().0), 42);
    assert_eq!(
        cursor(reopened.as_ref(), "durable"),
        Some(Position(CursorKey::Int(42)))
    );
    assert_eq!(
        page(reopened.as_ref(), Domain::Core, CORE_STREAM.name)
            .rows
            .len(),
        1
    );
    let mut replay = commit(reopened.as_ref(), unit);
    assert!(replay.replayed);
    replay.replayed = false;
    assert_eq!(replay, original);
    probe_fact(h, reopened.as_ref(), "durable", true);
}
fn crash_unit(h: &dyn Harness) -> UnitOfWork {
    let mut unit = uow(
        h,
        Domain::Core,
        "crash",
        (0..128)
            .map(|n| insert(CORE.name, &format!("crash-{n:03}"), n))
            .chain([
                append(CORE_STREAM.name, "crash"),
                Mutation::AdvanceCursor {
                    name: CursorName("crash".into()),
                    to: Position(CursorKey::Int(1)),
                },
            ])
            .collect(),
    );
    unit.facts.push(fact("crash"));
    sign(&mut unit);
    unit
}
fn check_crash_state(store: &dyn DomainStore) -> usize {
    let count = page(store, Domain::Core, CORE.name).rows.len();
    assert!(count == 0 || count == 128);
    assert_eq!(
        page(store, Domain::Core, CORE_STREAM.name).rows.len(),
        usize::from(count != 0)
    );
    assert_eq!(cursor(store, "crash").is_some(), count != 0);
    count
}
fn probe_fact(h: &dyn Harness, store: &dyn DomainStore, fact_id: &str, exists: bool) {
    let mut probe = uow(h, Domain::Core, format!("probe-{fact_id}"), vec![]);
    probe.facts.push(fact(fact_id));
    sign(&mut probe);
    match block_on(store.commit(probe)) {
        Err(CommitError::Unique { .. }) if exists => {}
        Ok(_) if !exists => {}
        result => panic!("fact presence disagrees with transaction outcome: {result:?}"),
    }
}
fn crash_atomicity(h: &dyn Harness, _store: Arc<dyn DomainStore>) {
    for _ in 0..50 {
        let store = h.open().unwrap();
        let outcome = h.crash_commit(store, crash_unit(h)).unwrap();
        assert!(outcome.killed_during_commit, "kill was not during commit");
        let count = check_crash_state(outcome.store.as_ref());
        probe_fact(h, outcome.store.as_ref(), "crash", count != 0);
    }
}
fn idempotency_recorded(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let unit = durable_unit(h);
    let original = commit(store.as_ref(), unit.clone());
    for _ in 0..3 {
        let mut replay = commit(store.as_ref(), unit.clone());
        assert!(replay.replayed);
        replay.replayed = false;
        assert_eq!(replay, original);
    }
    assert_eq!(
        page(store.as_ref(), Domain::Core, CORE_STREAM.name)
            .rows
            .len(),
        1
    );
}
fn idempotency_mismatch(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let unit = uow(
        h,
        Domain::Core,
        "same-key",
        vec![insert(CORE.name, "same", 1)],
    );
    commit(store.as_ref(), unit.clone());
    let mut changed = unit.clone();
    changed.mutations = vec![update("same", 2)];
    sign(&mut changed);
    assert!(matches!(
        block_on(store.commit(changed)),
        Err(CommitError::IdempotencyMismatch)
    ));
    let mut forged = unit;
    forged.digest.0[0] ^= 1;
    assert!(matches!(
        block_on(store.commit(forged)),
        Err(CommitError::Fatal(Error {
            code: Code::Invalid,
            ..
        }))
    ));
    assert_eq!(value(&get(store.as_ref(), "same").unwrap().0), 1);
}
fn idempotency_expiry(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let unit = uow(h, Domain::Core, "age", vec![insert(CORE.name, "age", 1)]);
    commit(store.as_ref(), unit.clone());
    let mut future = uow(h, Domain::Core, "future", vec![]);
    future.id.issued_at.0 += 300_001;
    assert!(matches!(
        block_on(store.commit(future.clone())),
        Err(CommitError::Fatal(Error {
            code: Code::Invalid,
            ..
        }))
    ));
    future.id.issued_at.0 -= 1;
    commit(store.as_ref(), future);
    h.advance_clock(30 * 24 * 60 * 60 * 1000).unwrap();
    assert!(commit(store.as_ref(), unit.clone()).replayed);
    h.advance_clock(1).unwrap();
    assert_eq!(
        block_on(store.commit(unit.clone())),
        Err(CommitError::Expired)
    );
    let mut absent = unit;
    absent.id.key = "never-seen".into();
    assert_eq!(block_on(store.commit(absent)), Err(CommitError::Expired));
}
fn idempotency_natural(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let mut record = row("natural", 7);
    set(&mut record, "label", Cell::Text("natural-key".into()));
    let mut unit = uow(
        h,
        Domain::Comms,
        "natural-1",
        vec![Mutation::Insert {
            entity: COMMS.name.into(),
            row: record,
        }],
    );
    unit.idempotency = Idempotency::Natural {
        entity: COMMS.name.into(),
        index: "label".into(),
    };
    sign(&mut unit);
    let original = commit(store.as_ref(), unit.clone());
    for n in 0..3 {
        unit.id.key = format!("natural-{n}");
        let replay = commit(store.as_ref(), unit.clone());
        assert!(replay.replayed);
        assert_eq!(replay.rev, original.rev);
    }
    let mut changed = unit.clone();
    if let Mutation::Insert { row, .. } = &mut changed.mutations[0] {
        set(row, "value", Cell::Int(8));
    }
    sign(&mut changed);
    assert_eq!(
        block_on(store.commit(changed)),
        Err(CommitError::IdempotencyMismatch)
    );
    unit.facts.push(crate::fact::FactRecord {
        topic: "verify.comms.fact".into(),
        ..fact("unsafe-natural")
    });
    sign(&mut unit);
    assert!(matches!(
        block_on(store.commit(unit)),
        Err(CommitError::Fatal(Error {
            code: Code::Invalid,
            ..
        }))
    ));
    assert_eq!(
        page(store.as_ref(), Domain::Comms, COMMS.name).rows.len(),
        1
    );
}
fn idempotency_after_crash(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let known = durable_unit(h);
    let original = commit(store.as_ref(), known.clone());
    let outcome = h
        .crash_commit(store, uow(h, Domain::Core, "crash-empty", vec![]))
        .unwrap();
    assert!(outcome.killed_during_commit);
    let replay = commit(outcome.store.as_ref(), known);
    assert!(replay.replayed);
    assert_eq!(replay.rev, original.rev);
    let unit = crash_unit(h);
    let outcome = h.crash_commit(h.open().unwrap(), unit.clone()).unwrap();
    assert!(outcome.killed_during_commit);
    check_crash_state(outcome.store.as_ref());
    commit(outcome.store.as_ref(), unit);
    assert_eq!(
        page(outcome.store.as_ref(), Domain::Core, CORE.name)
            .rows
            .len(),
        128
    );
}
fn append_order(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let remaining = std::sync::atomic::AtomicUsize::new(4);
    let barrier = Barrier::new(5);
    let mut seen = BTreeSet::new();
    let mut last = 0i64;
    thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|worker| {
                let store = store.clone();
                let remaining = &remaining;
                let barrier = &barrier;
                scope.spawn(move || {
                    let _working = Working(remaining);
                    barrier.wait();
                    let mut assigned = Vec::new();
                    for n in 0..100 {
                        let done = commit(
                            store.as_ref(),
                            uow(
                                h,
                                Domain::Stream,
                                format!("append-{worker}-{n}"),
                                vec![append(STREAM.name, &format!("{worker}-{n}"))],
                            ),
                        );
                        assert_eq!(done.seqs.len(), 1);
                        assigned.push(done.seqs[0].1[0].0);
                    }
                    assigned
                })
            })
            .collect();
        barrier.wait();
        loop {
            let active = remaining.load(Ordering::SeqCst) > 0;
            let mut query = Scan::<Row> {
                index: IndexName {
                    entity: STREAM.name.into(),
                    name: String::new(),
                },
                range: KeyRange {
                    start: KeyBound::Excluded(vec![Cell::Int(last)]),
                    end: KeyBound::Unbounded,
                },
                filter: vec![],
                order: Order::Asc,
                limit: 7,
                budget: ScanBudget(11),
                resume: None,
                marker: PhantomData,
            };
            loop {
                let out: Page<Row> =
                    result(store.as_ref(), request("scan", Domain::Stream, &query));
                for (row, _) in &out.rows {
                    let Cell::Int(seq) = cell(row, "seq").unwrap() else {
                        panic!("seq type")
                    };
                    assert!(*seq > last);
                    last = *seq;
                    let Cell::Text(id) = cell(row, "id").unwrap() else {
                        panic!("id type")
                    };
                    assert!(seen.insert(id.clone()));
                }
                if let Some(resume) = out.resume {
                    query.resume = Some(resume);
                } else {
                    break;
                }
            }
            if !active {
                break;
            }
            thread::yield_now();
        }
        let mut assigned = BTreeSet::new();
        for handle in handles {
            for seq in handle.join().unwrap() {
                assert!(assigned.insert(seq));
            }
        }
        assert_eq!(assigned.len(), 400);
        assert_eq!(assigned.iter().next_back().copied(), Some(last));
    });
    assert_eq!(seen.len(), 400);
}
fn snapshot_isolation(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let original = commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "snapshot-seed",
            vec![insert(CORE.name, "snapshot", 1)],
        ),
    );
    let lease = block_on(store.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    let before: Option<(Row, Rev)> =
        block_on(store.lease_query(lease.clone(), lookup(Domain::Core, CORE.name, "snapshot")))
            .unwrap()
            .body
            .decode()
            .unwrap();
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "snapshot-change",
            vec![update("snapshot", 2)],
        ),
    );
    let after: Option<(Row, Rev)> =
        block_on(store.lease_query(lease.clone(), lookup(Domain::Core, CORE.name, "snapshot")))
            .unwrap()
            .body
            .decode()
            .unwrap();
    assert_eq!(before, after);
    assert_eq!(before.unwrap().1, original.rev);
    assert_eq!(value(&get(store.as_ref(), "snapshot").unwrap().0), 2);
    block_on(store.lease_end(lease.clone())).unwrap();
    assert!(matches!(
        block_on(store.lease_query(lease, lookup(Domain::Core, CORE.name, "snapshot"))),
        Err(Error {
            code: Code::Stale,
            ..
        })
    ));
}
fn unique_and_c_order(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let names = ["é", "a", "Z", "😀", "中", "A", "z"];
    for (n, name) in names.iter().enumerate() {
        let mut record = row(*name, n as i64);
        set(&mut record, "label", Cell::Text((*name).into()));
        commit(
            store.as_ref(),
            uow(
                h,
                Domain::Core,
                format!("order-{n}"),
                vec![Mutation::Insert {
                    entity: CORE.name.into(),
                    row: record,
                }],
            ),
        );
    }
    assert!(matches!(
        block_on(store.commit(uow(
            h,
            Domain::Core,
            "pk-duplicate",
            vec![insert(CORE.name, "a", 0)]
        ))),
        Err(CommitError::Unique { .. })
    ));
    let mut duplicate = row("different", 0);
    set(&mut duplicate, "label", Cell::Text("a".into()));
    assert!(matches!(
        block_on(store.commit(uow(
            h,
            Domain::Core,
            "index-duplicate",
            vec![Mutation::Insert {
                entity: CORE.name.into(),
                row: duplicate
            }]
        ))),
        Err(CommitError::Unique { .. })
    ));
    let mut expected = names
        .iter()
        .map(|name| Cell::Text((*name).into()))
        .collect::<Vec<_>>();
    expected.sort_by(|a, b| match (a, b) {
        (Cell::Text(a), Cell::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        _ => unreachable!(),
    });
    for order in [Order::Asc, Order::Desc] {
        let out: Page<Row> = result(
            store.as_ref(),
            scan(
                Domain::Core,
                CORE.name,
                "label",
                500,
                500,
                None,
                vec![],
                order,
            ),
        );
        let mut expected = expected.clone();
        if order == Order::Desc {
            expected.reverse();
        }
        assert_eq!(
            out.rows
                .iter()
                .map(|(row, _)| cell(row, "label").unwrap().clone())
                .collect::<Vec<_>>(),
            expected
        );
    }
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "numeric",
            vec![
                insert(CORE.name, "num-10", 10),
                insert(CORE.name, "num-2", 2),
            ],
        ),
    );
    let out: Page<Row> = result(
        store.as_ref(),
        scan(
            Domain::Core,
            CORE.name,
            "value",
            500,
            500,
            None,
            vec![Pred::In {
                field: "id".into(),
                values: vec![Cell::Text("num-2".into()), Cell::Text("num-10".into())],
            }],
            Order::Asc,
        ),
    );
    assert_eq!(
        out.rows.iter().map(|(r, _)| value(r)).collect::<Vec<_>>(),
        vec![2, 10]
    );
}
fn encoding_precision(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let mut record = row("编码😀\u{1}\n", i64::MIN);
    set(
        &mut record,
        "label",
        Cell::Text("汉字😀\u{1}\t".repeat(131_072)),
    );
    set(&mut record, "at", Cell::Millis(1_789_123_456_789));
    set(
        &mut record,
        "data",
        Cell::Json(RawJson::from_str("{ \"z\": [1, 2], \"a\": \"中😀\" }").unwrap()),
    );
    set(&mut record, "bytes", Cell::Bytes((0..=255).collect()));
    set(&mut record, "real", Cell::Real(-1.23456789012345));
    set(&mut record, "flag", Cell::Bool(true));
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "encoding",
            vec![Mutation::Insert {
                entity: CORE.name.into(),
                row: record.clone(),
            }],
        ),
    );
    let returned = get(store.as_ref(), "编码😀\u{1}\n").unwrap().0;
    assert_eq!(returned, record);
    assert_eq!(
        serde_json::to_vec(&returned).unwrap(),
        serde_json::to_vec(&record).unwrap()
    );
    for (n, v) in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY]
        .into_iter()
        .enumerate()
    {
        let mut unit = uow(
            h,
            Domain::Core,
            format!("nonfinite-{n}"),
            vec![insert(CORE.name, "invalid", 1)],
        );
        if let Mutation::Insert { row, .. } = &mut unit.mutations[0] {
            set(row, "real", Cell::Real(v));
        }
        assert!(matches!(
            block_on(store.commit(unit)),
            Err(CommitError::Fatal(Error {
                code: Code::Invalid,
                ..
            }))
        ));
    }
    assert!(get(store.as_ref(), "invalid").is_none());
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "maxint",
            vec![insert(CORE.name, "maxint", i64::MAX)],
        ),
    );
    assert_eq!(value(&get(store.as_ref(), "maxint").unwrap().0), i64::MAX);
}
fn measure(h: &dyn Harness, store: &dyn DomainStore) -> Vec<Measurement> {
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for n in 0..16 {
        let unit = uow(
            h,
            Domain::Core,
            format!("baseline-{n}"),
            (0..128)
                .map(|i| insert(CORE.name, &format!("baseline-{n}-{i}"), i))
                .collect(),
        );
        let started = Instant::now();
        let result = block_on(store.commit(unit));
        if result.is_err() {
            return vec![];
        }
        writes.push(started.elapsed().as_micros() as u64);
        let started = Instant::now();
        if block_on(store.query(lookup(Domain::Core, CORE.name, &format!("baseline-{n}-0"))))
            .is_err()
        {
            return vec![];
        }
        reads.push(started.elapsed().as_micros() as u64);
    }
    [("point_query", reads), ("commit_128", writes)]
        .into_iter()
        .map(|(name, mut samples)| {
            samples.sort_unstable();
            Measurement {
                name: name.into(),
                samples: samples.len() as u64,
                p50_us: samples[samples.len() / 2],
                p99_us: samples[samples.len() - 1],
                warning: None,
            }
        })
        .collect()
}
fn performance_baseline(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let measurements = measure(h, store.as_ref());
    assert_eq!(measurements.len(), 2);
    assert!(
        measurements
            .iter()
            .all(|m| m.samples > 0 && m.p99_us >= m.p50_us)
    );
}
fn lease_expiry(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let lease = block_on(store.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    h.advance_clock(1999).unwrap();
    block_on(store.lease_query(lease.clone(), lookup(Domain::Core, CORE.name, "absent"))).unwrap();
    h.advance_clock(1).unwrap();
    assert!(matches!(
        block_on(store.lease_query(lease.clone(), lookup(Domain::Core, CORE.name, "absent"))),
        Err(Error {
            code: Code::Stale,
            ..
        })
    ));
    let fresh = block_on(store.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    assert_ne!(lease.id, fresh.id);
    block_on(store.lease_end(fresh)).unwrap();
}
fn lease_interruption(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    h.interrupt_expired_query(store).unwrap();
}
fn snapshot_domain(_h: &dyn Harness, store: Arc<dyn DomainStore>) {
    assert!(
        block_on(store.snapshot(Domain::Core, vec![]))
            .unwrap()
            .is_empty()
    );
    for q in [
        lookup(Domain::Comms, COMMS.name, "absent"),
        lookup(Domain::Core, COMMS.name, "absent"),
    ] {
        assert!(matches!(
            block_on(store.snapshot(Domain::Core, vec![q])),
            Err(Error {
                code: Code::Invalid,
                ..
            })
        ));
    }
    let lease = block_on(store.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    assert!(matches!(
        block_on(store.lease_query(lease.clone(), lookup(Domain::Comms, COMMS.name, "absent"))),
        Err(Error {
            code: Code::Invalid,
            ..
        })
    ));
    block_on(store.lease_end(lease)).unwrap();
}
fn uow_domain(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let foreign = COMMS.name;
    let foreign_mutations = vec![
        insert(foreign, "foreign", 1),
        Mutation::Upsert {
            entity: foreign.into(),
            row: row("foreign", 1),
        },
        Mutation::Update {
            entity: foreign.into(),
            key: vec![Cell::Text("foreign".into())],
            set: Row(vec![("value".into(), Cell::Int(2))]),
        },
        Mutation::Delete {
            entity: foreign.into(),
            key: vec![Cell::Text("foreign".into())],
        },
        Mutation::DeleteWhere {
            entity: foreign.into(),
            index: String::new(),
            range: KeyRange::default(),
            filter: vec![],
            limit: 1,
        },
        append(STREAM.name, "foreign"),
        Mutation::Op(AnyOp {
            name: "unknown".into(),
            domain: Domain::Comms,
            body: RawJson::from_str("{}").unwrap(),
        }),
    ];
    for (n, mutation) in foreign_mutations.into_iter().enumerate() {
        let unit = uow(
            h,
            Domain::Core,
            format!("mixed-{n}"),
            vec![insert(CORE.name, "sentinel", 1), mutation],
        );
        assert!(matches!(
            block_on(store.commit(unit)),
            Err(CommitError::Fatal(Error {
                code: Code::Invalid,
                ..
            }))
        ));
        assert!(get(store.as_ref(), "sentinel").is_none());
    }
    for (n, condition) in [
        Precondition::Absent {
            entity: foreign.into(),
            key: vec![Cell::Text("foreign".into())],
        },
        Precondition::Present {
            entity: foreign.into(),
            key: vec![Cell::Text("foreign".into())],
        },
        Precondition::Rev {
            entity: foreign.into(),
            key: vec![Cell::Text("foreign".into())],
            rev: Rev(1),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut unit = uow(
            h,
            Domain::Core,
            format!("mixed-pre-{n}"),
            vec![insert(CORE.name, "sentinel", 1)],
        );
        unit.preconditions.push(condition);
        sign(&mut unit);
        assert!(matches!(
            block_on(store.commit(unit)),
            Err(CommitError::Fatal(Error {
                code: Code::Invalid,
                ..
            }))
        ));
    }
    let mut unit = uow(
        h,
        Domain::Core,
        "mixed-fact",
        vec![insert(CORE.name, "sentinel", 1)],
    );
    unit.facts.push(crate::fact::FactRecord {
        topic: "verify.comms.fact".into(),
        ..fact("mixed")
    });
    sign(&mut unit);
    assert!(matches!(
        block_on(store.commit(unit)),
        Err(CommitError::Fatal(Error {
            code: Code::Invalid,
            ..
        }))
    ));
    assert!(get(store.as_ref(), "sentinel").is_none());
    for (domain, other) in [(Domain::Comms, CORE.name), (Domain::Stream, CORE.name)] {
        assert!(matches!(
            block_on(store.commit(uow(
                h,
                domain,
                "mixed-other",
                vec![insert(other, "wrong", 1)]
            ))),
            Err(CommitError::Fatal(Error {
                code: Code::Invalid,
                ..
            }))
        ));
    }
}
fn cursor_monotonic(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    assert_eq!(cursor(store.as_ref(), "consumer"), None);
    let mut unit = uow(
        h,
        Domain::Core,
        "cursor-first",
        vec![
            insert(CORE.name, "side-effect", 1),
            Mutation::AdvanceCursor {
                name: CursorName("consumer".into()),
                to: Position(CursorKey::Composite(vec![
                    CursorKey::Int(2),
                    CursorKey::Text("a".into()),
                ])),
            },
        ],
    );
    unit.preconditions.push(Precondition::Cursor {
        name: CursorName("consumer".into()),
        at: None,
    });
    sign(&mut unit);
    commit(store.as_ref(), unit.clone());
    unit.id.key = "cursor-conflict".into();
    assert!(matches!(
        block_on(store.commit(unit)),
        Err(CommitError::Conflict(_))
    ));
    let before = cursor(store.as_ref(), "consumer");
    let failed = uow(
        h,
        Domain::Core,
        "cursor-backwards",
        vec![
            update("side-effect", 2),
            Mutation::AdvanceCursor {
                name: CursorName("consumer".into()),
                to: Position(CursorKey::Composite(vec![
                    CursorKey::Int(1),
                    CursorKey::Text("z".into()),
                ])),
            },
        ],
    );
    assert!(matches!(
        block_on(store.commit(failed)),
        Err(CommitError::Fatal(Error {
            code: Code::Invalid,
            ..
        }))
    ));
    assert_eq!(cursor(store.as_ref(), "consumer"), before);
    assert_eq!(value(&get(store.as_ref(), "side-effect").unwrap().0), 1);
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "cursor-forward",
            vec![Mutation::AdvanceCursor {
                name: CursorName("consumer".into()),
                to: Position(CursorKey::Composite(vec![
                    CursorKey::Int(10),
                    CursorKey::Text("A".into()),
                ])),
            }],
        ),
    );
}
fn scan_window(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "scan-data",
            (0..31)
                .map(|n| insert(CORE.name, &format!("scan-{n:03}"), n / 3))
                .collect(),
        ),
    );
    for order in [Order::Asc, Order::Desc] {
        let mut resume = None;
        let mut seen = Vec::new();
        let mut empty = 0;
        for iteration in 0..32 {
            let out: Page<Row> = result(
                store.as_ref(),
                scan(
                    Domain::Core,
                    CORE.name,
                    "value",
                    2,
                    3,
                    resume,
                    vec![Pred::Eq {
                        field: "value".into(),
                        value: Cell::Int(8),
                    }],
                    order,
                ),
            );
            if out.rows.is_empty() && out.resume.is_some() {
                empty += 1;
                assert!(out.exhausted_budget);
            }
            seen.extend(
                out.rows
                    .into_iter()
                    .map(|(row, _)| cell(&row, "id").unwrap().clone()),
            );
            resume = out.resume;
            if resume.is_none() {
                break;
            }
            assert!(iteration < 31, "scan continuation did not terminate");
        }
        assert_eq!(seen.len(), 3);
        assert!(empty > 0);
        assert_eq!(
            seen.iter()
                .map(|id| format!("{id:?}"))
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }
    let count = Count::<Row> {
        index: IndexName {
            entity: CORE.name.into(),
            name: String::new(),
        },
        range: KeyRange::default(),
        filter: vec![],
        budget: ScanBudget(3),
        marker: PhantomData,
    };
    let out: CountResult = result(store.as_ref(), request("count", Domain::Core, &count));
    assert_eq!(out, CountResult::AtLeast(3));
    let count = Count {
        budget: ScanBudget(31),
        ..count
    };
    let out: CountResult = result(store.as_ref(), request("count", Domain::Core, &count));
    assert_eq!(out, CountResult::Exact(31));
    assert!(matches!(
        block_on(store.query(scan(
            Domain::Core,
            CORE.name,
            "",
            0,
            1,
            None,
            vec![],
            Order::Asc
        ))),
        Err(Error {
            code: Code::Invalid,
            ..
        })
    ));
}
fn transfer_roundtrip(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let mut original = uow(
        h,
        Domain::Core,
        "export-original",
        vec![
            insert(CORE.name, "export-original", 7),
            append(CORE_STREAM.name, "export-original"),
            Mutation::AdvanceCursor {
                name: CursorName("export-cursor".into()),
                to: Position(CursorKey::Int(7)),
            },
        ],
    );
    original.facts.push(fact("export-original"));
    sign(&mut original);
    let original_result = commit(store.as_ref(), original.clone());
    for domain in [Domain::Core, Domain::Comms, Domain::Stream] {
        let entity = if domain == Domain::Core {
            CORE.name
        } else {
            COMMS.name
        };
        if domain != Domain::Stream {
            commit(
                store.as_ref(),
                uow(
                    h,
                    domain,
                    "export-rows",
                    (0..260)
                        .map(|n| insert(entity, &format!("export-{n:03}"), n))
                        .collect(),
                ),
            );
        } else {
            commit(
                store.as_ref(),
                uow(
                    h,
                    domain,
                    "export-stream",
                    vec![append(STREAM.name, "export-stream")],
                ),
            );
        }
    }
    let destination = h.open().unwrap();
    for domain in [Domain::Core, Domain::Stream, Domain::Comms] {
        let session = block_on(store.begin_export(domain)).unwrap();
        if domain == Domain::Core {
            commit(
                store.as_ref(),
                uow(
                    h,
                    domain,
                    "after-export",
                    vec![insert(CORE.name, "late", 1)],
                ),
            );
        }
        let mut next = ExportCursor::default();
        for iteration in 0..32 {
            let batch = block_on(store.export(&session, next.clone())).unwrap();
            let retry = block_on(store.export(&session, next)).unwrap();
            assert_eq!(batch, retry);
            assert_eq!(super::mem::batch_digest(&batch).unwrap(), batch.digest);
            let mut corrupted = batch.clone();
            corrupted.digest.0[0] ^= 1;
            assert!(block_on(destination.import(corrupted)).is_err());
            let imported = block_on(destination.import(batch.clone())).unwrap();
            assert!(!imported.replayed);
            assert_eq!(imported.imported, batch.rows.len() as u64);
            assert!(
                block_on(destination.import(batch.clone()))
                    .unwrap()
                    .replayed
            );
            if let Some(cursor) = batch.next {
                next = cursor;
            } else {
                break;
            }
            assert!(iteration < 31, "export did not terminate");
        }
    }
    assert!(get(destination.as_ref(), "late").is_none());
    assert_eq!(
        page(destination.as_ref(), Domain::Core, CORE.name)
            .rows
            .len(),
        261
    );
    assert_eq!(
        page(destination.as_ref(), Domain::Comms, COMMS.name)
            .rows
            .len(),
        260
    );
    assert_eq!(
        cursor(destination.as_ref(), "export-cursor"),
        Some(Position(CursorKey::Int(7)))
    );
    let replay = commit(destination.as_ref(), original);
    assert!(replay.replayed);
    assert_eq!(replay.rev, original_result.rev);
    probe_fact(h, destination.as_ref(), "export-original", true);
    let seq = commit(
        destination.as_ref(),
        uow(
            h,
            Domain::Core,
            "after-import",
            vec![append(CORE_STREAM.name, "after-import")],
        ),
    );
    assert_eq!(seq.seqs[0].1[0].0, 2);
}
fn retention_report(h: &dyn Harness, store: Arc<dyn DomainStore>) {
    let mut mutations = Vec::new();
    for n in 0..9 {
        let mut record = row(format!("log-{n}"), n);
        set(&mut record, "at", Cell::Millis(h.now().0));
        mutations.push(Mutation::Insert {
            entity: LOG.name.into(),
            row: record,
        });
    }
    commit(store.as_ref(), uow(h, Domain::Core, "logs", mutations));
    h.advance_clock(1001).unwrap();
    let mut live = row("live", 99);
    set(&mut live, "at", Cell::Millis(h.now().0));
    commit(
        store.as_ref(),
        uow(
            h,
            Domain::Core,
            "live",
            vec![Mutation::Insert {
                entity: LOG.name.into(),
                row: live,
            }],
        ),
    );
    let mut affected = 0;
    let mut slices = 0;
    loop {
        let report = block_on(store.retention(
            LOG.name.into(),
            Budget {
                rows: 3,
                target_ms: 20,
            },
        ))
        .unwrap();
        assert_eq!(report.entity, LOG.name);
        assert_eq!(report.slices, 1);
        assert!(report.affected <= 3);
        assert_eq!(report.overruns, u32::from(report.max_slice_ms > 20));
        affected += report.affected;
        slices += report.slices;
        if !report.more {
            break;
        }
        assert!(slices < 10);
    }
    assert_eq!(affected, 9);
    assert_eq!(page(store.as_ref(), Domain::Core, LOG.name).rows.len(), 1);
    let again = block_on(store.retention(LOG.name.into(), Budget::default())).unwrap();
    assert_eq!(again.affected, 0);
    assert!(!again.more);
}

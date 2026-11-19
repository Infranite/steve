//! Single-lock reference implementation. Failed work units never publish candidate state.
//! No filesystem, runtime, or backend dependency is required. Register typed codecs when
//! using domain-owned StoredEntity mappings; descriptor-only entities use Row on the wire.
use super::entities::{cell, set};
use crate::{
    append::{Seq, StreamName},
    cursor::*,
    entity::*,
    query::*,
    retention::*,
    uow::*,
    *,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering as AtomicOrdering},
    },
    time::Instant,
};
use steve_types::Code;

/// Injectable wall clock; elapsed report durations use Instant independently.
pub trait Clock: Send + Sync {
    fn now(&self) -> Millis;
}
/// Deterministic test clock, shared between a harness and its stores.
pub struct ManualClock(AtomicI64);
impl ManualClock {
    pub fn new(now: Millis) -> Self {
        Self(AtomicI64::new(now.0))
    }
    pub fn advance(&self, millis: i64) {
        self.0.fetch_add(millis, AtomicOrdering::SeqCst);
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Millis {
        Millis(self.0.load(AtomicOrdering::SeqCst))
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Data {
    pub(super) rows: BTreeMap<String, Vec<(Row, Rev)>>,
    cursors: BTreeMap<(Domain, String), Position>,
    pub(super) seqs: BTreeMap<(Domain, String), u64>,
    facts: BTreeMap<(Domain, String), crate::fact::FactRecord>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    digest: RequestDigest,
    result: Committed,
}
type ReceiptKey = (Domain, i64, String);
type NaturalKey = (Domain, String, String, Vec<u8>);
#[derive(Clone, Serialize, Deserialize)]
struct TransferState {
    receipts: Vec<(ReceiptKey, Receipt)>,
    natural: Vec<(NaturalKey, Receipt)>,
    cursors: Vec<((Domain, String), Position)>,
    seqs: Vec<((Domain, String), u64)>,
    facts: Vec<((Domain, String), crate::fact::FactRecord)>,
}
struct Export {
    session: ExportSession,
    data: Data,
    metadata: TransferState,
}
#[derive(Default)]
struct State {
    data: Data,
    rev: u64,
    next_handle: u64,
    last_cleanup: i64,
    receipts: BTreeMap<ReceiptKey, Receipt>,
    natural: BTreeMap<NaturalKey, Receipt>,
    leases: BTreeMap<String, (LeaseId, Domain, Data)>,
    exports: BTreeMap<String, Export>,
    imported: BTreeMap<String, RequestDigest>,
    retention_after: BTreeMap<String, Vec<Cell>>,
}
type Codec = fn(Row) -> Result<RawJson, Error>;
type IndexedRow<'a> = (&'a (Row, Rev), Vec<Cell>);
static INSTANCE: AtomicU64 = AtomicU64::new(1);

/// DomainStore fake with real CAS, receipts, immutable snapshots and bounded scans.
pub struct MemStore {
    entities: BTreeMap<String, EntityDesc>,
    codecs: BTreeMap<String, Codec>,
    topics: BTreeMap<String, Domain>,
    hello: Hello,
    clock: Arc<dyn Clock>,
    instance: u64,
    state: Mutex<State>,
}
fn invalid(detail: &str) -> Error {
    error(Code::Invalid, detail)
}
fn unsupported(detail: &str) -> Error {
    error(Code::Unsupported, detail)
}
fn stale() -> Error {
    error(Code::Stale, "unknown or expired snapshot")
}
fn fatal(error: Error) -> CommitError {
    CommitError::Fatal(error)
}
fn cell_rank(cell: &Cell) -> u8 {
    match cell {
        Cell::Null => 0,
        Cell::Int(_) => 1,
        Cell::Real(_) => 2,
        Cell::Text(_) => 3,
        Cell::Bytes(_) => 4,
        Cell::Bool(_) => 5,
        Cell::Millis(_) => 6,
        Cell::Json(_) => 7,
    }
}
fn cmp(a: &Cell, b: &Cell) -> Ordering {
    match (a, b) {
        (Cell::Null, Cell::Null) => Ordering::Equal,
        (Cell::Null, _) => Ordering::Less,
        (_, Cell::Null) => Ordering::Greater,
        (Cell::Int(a), Cell::Int(b)) | (Cell::Millis(a), Cell::Millis(b)) => a.cmp(b),
        (Cell::Real(a), Cell::Real(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Cell::Text(a), Cell::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Cell::Bytes(a), Cell::Bytes(b)) => a.cmp(b),
        (Cell::Bool(a), Cell::Bool(b)) => a.cmp(b),
        (Cell::Json(a), Cell::Json(b)) => serde_json::to_vec(a)
            .expect("valid JSON")
            .cmp(&serde_json::to_vec(b).expect("valid JSON")),
        _ => cell_rank(a).cmp(&cell_rank(b)),
    }
}
fn key_cmp(a: &[Cell], b: &[Cell]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(a, b)| cmp(a, b))
        .find(|o| *o != Ordering::Equal)
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}
fn values(row: &Row, fields: &[String]) -> Vec<Cell> {
    fields
        .iter()
        .map(|f| cell(row, f).cloned().unwrap_or(Cell::Null))
        .collect()
}
fn same_shape(a: &CursorKey, b: &CursorKey) -> bool {
    match (a, b) {
        (CursorKey::Int(_), CursorKey::Int(_)) | (CursorKey::Text(_), CursorKey::Text(_)) => true,
        (CursorKey::Composite(a), CursorKey::Composite(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_shape(a, b))
        }
        _ => false,
    }
}
fn in_range(key: &[Cell], range: &KeyRange) -> bool {
    let bound_cmp = |bound: &[Cell]| key_cmp(&key[..key.len().min(bound.len())], bound);
    let start = match &range.start {
        KeyBound::Unbounded => true,
        KeyBound::Included(v) => bound_cmp(v) != Ordering::Less,
        KeyBound::Excluded(v) => bound_cmp(v) == Ordering::Greater,
    };
    let end = match &range.end {
        KeyBound::Unbounded => true,
        KeyBound::Included(v) => bound_cmp(v) != Ordering::Greater,
        KeyBound::Excluded(v) => bound_cmp(v) == Ordering::Less,
    };
    start && end
}
fn matches(row: &Row, predicates: &[Pred]) -> bool {
    predicates.iter().all(|pred| {
        let compare = |field: &str, value: &Cell, accept: fn(Ordering) -> bool| {
            cell(row, field).is_some_and(|v| {
                !matches!(v, Cell::Null) && !matches!(value, Cell::Null) && accept(cmp(v, value))
            })
        };
        match pred {
            Pred::Eq { field, value } => compare(field, value, |o| o == Ordering::Equal),
            Pred::Ne { field, value } => compare(field, value, |o| o != Ordering::Equal),
            Pred::Lt { field, value } => compare(field, value, |o| o == Ordering::Less),
            Pred::Le { field, value } => compare(field, value, |o| o != Ordering::Greater),
            Pred::Gt { field, value } => compare(field, value, |o| o == Ordering::Greater),
            Pred::Ge { field, value } => compare(field, value, |o| o != Ordering::Less),
            Pred::In { field, values } => values
                .iter()
                .any(|v| compare(field, v, |o| o == Ordering::Equal)),
            Pred::IsNull { field, is_null } => {
                cell(row, field).is_some_and(|v| matches!(v, Cell::Null) == *is_null)
            }
        }
    })
}
impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}
impl MemStore {
    /// Fixture-ready store; isolated from every other instance.
    pub fn new() -> Self {
        Self::with_entities(
            super::entities::descriptors(),
            Arc::new(ManualClock::new(Millis(1_000_000))),
        )
        .expect("valid fixture descriptors")
    }
    pub fn with_entities(entities: Vec<EntityDesc>, clock: Arc<dyn Clock>) -> Result<Self, Error> {
        let mut descriptions = BTreeMap::new();
        for entity in entities {
            Self::validate_descriptor(&entity)?;
            if descriptions.insert(entity.name.clone(), entity).is_some() {
                return Err(invalid("duplicate entity"));
            }
        }
        let domains = Self::offers(&descriptions);
        Ok(Self {
            entities: descriptions,
            codecs: BTreeMap::new(),
            topics: [
                ("verify.core.fact".into(), Domain::Core),
                ("verify.stream.fact".into(), Domain::Stream),
                ("verify.comms.fact".into(), Domain::Comms),
            ]
            .into_iter()
            .collect(),
            hello: Hello {
                implementation: "conformance-memory".into(),
                domains,
            },
            clock,
            instance: INSTANCE.fetch_add(1, AtomicOrdering::Relaxed),
            state: Mutex::new(State::default()),
        })
    }
    fn offers(descriptions: &BTreeMap<String, EntityDesc>) -> Vec<DomainOffer> {
        [Domain::Core, Domain::Stream, Domain::Comms]
            .into_iter()
            .map(|domain| {
                let mut features: BTreeSet<String> = ["cas", "append_order", "snapshot_read"]
                    .into_iter()
                    .map(String::from)
                    .collect();
                // For this fake, the supplied descriptors ARE the complete namespace.
                if descriptions.values().any(|entity| entity.domain == domain) {
                    features.insert("export".into());
                }
                if domain == Domain::Comms
                    && crate::ops::comms::digest::SYNC_PENDING_MEMBERSHIP.implemented
                    && super::digest::descriptors()
                        .iter()
                        .all(|entity| descriptions.get(&entity.name) == Some(entity))
                {
                    features.insert(format!(
                        "op:{}",
                        crate::ops::comms::digest::SYNC_PENDING_MEMBERSHIP.name
                    ));
                }
                DomainOffer {
                    domain,
                    versions: (ContractVersion(1), ContractVersion(1)),
                    isolation: Isolation::Serializable,
                    features,
                }
            })
            .collect()
    }
    /// Register a typed row codec before sharing the store. Existing descriptors must match.
    pub fn register<E: StoredEntity>(&mut self) -> Result<(), Error> {
        let desc = E::ENTITY.to_wire();
        Self::validate_descriptor(&desc)?;
        if self
            .entities
            .get(&desc.name)
            .is_some_and(|existing| existing != &desc)
        {
            return Err(invalid("entity descriptor mismatch"));
        }
        fn encode_row<E: StoredEntity>(row: Row) -> Result<RawJson, Error> {
            encode(&E::from_row(row)?)
        }
        self.codecs.insert(desc.name.clone(), encode_row::<E>);
        self.entities.insert(desc.name.clone(), desc);
        self.hello.domains = Self::offers(&self.entities);
        Ok(())
    }
    pub fn register_topic(&mut self, name: String, domain: Domain) -> Result<(), Error> {
        if self.topics.get(&name).is_some_and(|d| *d != domain) {
            return Err(invalid("topic domain mismatch"));
        }
        self.topics.insert(name, domain);
        Ok(())
    }
    fn validate_descriptor(e: &EntityDesc) -> Result<(), Error> {
        let names: BTreeSet<_> = e.fields.iter().map(|f| &f.name).collect();
        if e.key.is_empty()
            || names.len() != e.fields.len()
            || e.key.iter().any(|k| !names.contains(k))
            || e.indexes
                .iter()
                .any(|i| i.fields.is_empty() || i.fields.iter().any(|f| !names.contains(f)))
        {
            return Err(invalid("invalid entity descriptor"));
        }
        if e.retention.as_ref().is_some_and(|p| p.entity != e.name) {
            return Err(invalid("retention entity mismatch"));
        }
        Ok(())
    }
    fn entity(&self, name: &str, domain: Domain) -> Result<&EntityDesc, Error> {
        let entity = self
            .entities
            .get(name)
            .ok_or_else(|| unsupported("unknown entity"))?;
        if entity.domain != domain {
            return Err(invalid("mixed-domain request"));
        }
        Ok(entity)
    }
    fn check_row(&self, e: &EntityDesc, row: &Row) -> Result<(), Error> {
        let names: BTreeSet<_> = row.0.iter().map(|(name, _)| name).collect();
        if names.len() != row.0.len() || row.0.len() != e.fields.len() {
            return Err(invalid("row fields do not match descriptor"));
        }
        for field in &e.fields {
            let value = cell(row, &field.name).ok_or_else(|| invalid("missing field"))?;
            if matches!(value, Cell::Null) {
                if !field.nullable || e.key.contains(&field.name) {
                    return Err(invalid("null non-nullable field"));
                }
                continue;
            }
            Self::check_cell(field, value)?;
        }
        Ok(())
    }
    fn check_cell(field: &FieldDesc, value: &Cell) -> Result<(), Error> {
        value.validate()?;
        let valid = match (&field.ty, value) {
            (_, Cell::Null) => field.nullable,
            (FieldType::Int, Cell::Int(_))
            | (FieldType::Real, Cell::Real(_))
            | (FieldType::Text, Cell::Text(_))
            | (FieldType::Bytes, Cell::Bytes(_))
            | (FieldType::Bool, Cell::Bool(_))
            | (FieldType::Millis, Cell::Millis(_))
            | (FieldType::Json, Cell::Json(_)) => true,
            (FieldType::Enum(codes), Cell::Text(value)) => {
                if !codes.contains(value) {
                    return Err(error(Code::Incompatible, "unknown enum code"));
                }
                true
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(invalid("invalid field type"))
        }
    }
    fn check_key(
        &self,
        entity: &EntityDesc,
        fields: &[String],
        key: &[Cell],
        prefix: bool,
    ) -> Result<(), Error> {
        if key.len() > fields.len() || (!prefix && key.len() != fields.len()) || key.is_empty() {
            return Err(invalid("invalid index key shape"));
        }
        for (field, value) in fields.iter().zip(key) {
            let field = entity
                .fields
                .iter()
                .find(|f| f.name == *field)
                .ok_or_else(|| invalid("invalid index field"))?;
            Self::check_cell(field, value)?;
            if entity.key.contains(&field.name) && *value == Cell::Null {
                return Err(invalid("null primary key"));
            }
        }
        Ok(())
    }
    fn index_fields(&self, e: &EntityDesc, index: &str) -> Result<Vec<String>, Error> {
        let mut fields = if index.is_empty() {
            e.key.clone()
        } else {
            e.indexes
                .iter()
                .find(|i| i.name == index)
                .ok_or_else(|| unsupported("unknown index"))?
                .fields
                .clone()
        };
        for key in &e.key {
            if !fields.contains(key) {
                fields.push(key.clone());
            }
        }
        Ok(fields)
    }
    fn check_filter(&self, e: &EntityDesc, filter: &[Pred]) -> Result<(), Error> {
        for pred in filter {
            let field = match pred {
                Pred::Eq { field, .. }
                | Pred::Ne { field, .. }
                | Pred::Lt { field, .. }
                | Pred::Le { field, .. }
                | Pred::Gt { field, .. }
                | Pred::Ge { field, .. }
                | Pred::In { field, .. }
                | Pred::IsNull { field, .. } => field,
            };
            let field = e
                .fields
                .iter()
                .find(|f| f.name == *field)
                .ok_or_else(|| unsupported("unknown filter field"))?;
            let values: Vec<&Cell> = match pred {
                Pred::Eq { value, .. }
                | Pred::Ne { value, .. }
                | Pred::Lt { value, .. }
                | Pred::Le { value, .. }
                | Pred::Gt { value, .. }
                | Pred::Ge { value, .. } => vec![value],
                Pred::In { values, .. } => values.iter().collect(),
                Pred::IsNull { .. } => vec![],
            };
            for value in values {
                Self::check_cell(field, value)?;
                if *value == Cell::Null {
                    return Err(invalid("null predicates require IsNull"));
                }
            }
        }
        Ok(())
    }
    fn ordered<'a>(
        &self,
        data: &'a Data,
        e: &EntityDesc,
        index: &str,
        range: &KeyRange,
        order: Order,
    ) -> Result<Vec<IndexedRow<'a>>, Error> {
        let fields = self.index_fields(e, index)?;
        for bound in [&range.start, &range.end] {
            if let KeyBound::Included(key) | KeyBound::Excluded(key) = bound {
                self.check_key(e, &fields, key, true)?;
            }
        }
        let mut rows: Vec<_> = data
            .rows
            .get(&e.name)
            .into_iter()
            .flatten()
            .map(|r| (r, values(&r.0, &fields)))
            .filter(|(_, key)| in_range(key, range))
            .collect();
        rows.sort_by(|(_, a), (_, b)| key_cmp(a, b));
        if order == Order::Desc {
            rows.reverse();
        }
        Ok(rows)
    }
    fn locate(&self, data: &Data, e: &EntityDesc, key: &[Cell]) -> Result<Option<usize>, Error> {
        self.check_key(e, &e.key, key, false)?;
        Ok(data
            .rows
            .get(&e.name)
            .and_then(|rows| rows.iter().position(|(row, _)| values(row, &e.key) == key)))
    }
    fn unique(
        &self,
        data: &Data,
        e: &EntityDesc,
        row: &Row,
        except: Option<usize>,
    ) -> Result<(), CommitError> {
        let indexes = std::iter::once(("", &e.key)).chain(
            e.indexes
                .iter()
                .filter(|i| i.unique)
                .map(|i| (i.name.as_str(), &i.fields)),
        );
        for (name, fields) in indexes {
            let key = values(row, fields);
            if key.contains(&Cell::Null) {
                continue;
            }
            if data
                .rows
                .get(&e.name)
                .into_iter()
                .flatten()
                .enumerate()
                .any(|(n, (other, _))| Some(n) != except && values(other, fields) == key)
            {
                return Err(CommitError::Unique {
                    entity: e.name.clone(),
                    index: name.into(),
                });
            }
        }
        Ok(())
    }
    fn query_data(&self, data: &Data, q: AnyQuery) -> Result<AnyOutput, Error> {
        let body = match q.name.as_str() {
            "cursor_get" => {
                let request: CursorGet = q.body.decode()?;
                if request.domain != q.domain {
                    return Err(invalid("mixed-domain cursor"));
                }
                encode(&data.cursors.get(&(q.domain, request.name.0)))?
            }
            "get" | "exists" => {
                #[derive(Deserialize)]
                struct Lookup {
                    key: Key<Row>,
                }
                let request: Lookup = q.body.decode()?;
                let e = self.entity(&request.key.entity, q.domain)?;
                let found = self
                    .locate(data, e, &request.key.values)?
                    .map(|n| &data.rows[&e.name][n]);
                if q.name == "exists" {
                    encode(&found.is_some())?
                } else {
                    let result = found
                        .map(|(row, rev)| {
                            Ok::<_, Error>((self.encode_row(&e.name, row.clone())?, *rev))
                        })
                        .transpose()?;
                    encode(&result)?
                }
            }
            "scan" | "count" => {
                #[derive(Deserialize)]
                struct Window {
                    index: IndexName,
                    range: KeyRange,
                    filter: Vec<Pred>,
                    budget: ScanBudget,
                    #[serde(default)]
                    order: Option<Order>,
                    #[serde(default)]
                    limit: Option<u32>,
                    #[serde(default)]
                    resume: Option<ResumeToken>,
                }
                let request: Window = q.body.decode()?;
                let e = self.entity(&request.index.entity, q.domain)?;
                self.check_filter(e, &request.filter)?;
                let limit = request.limit.unwrap_or(500);
                if request.budget.0 == 0 || !(1..=500).contains(&limit) {
                    return Err(invalid("invalid scan limit or budget"));
                }
                let order = request.order.unwrap_or(Order::Asc);
                let fields = self.index_fields(e, &request.index.name)?;
                if request
                    .resume
                    .as_ref()
                    .is_some_and(|r| r.key.len() != fields.len())
                {
                    return Err(invalid("invalid resume key"));
                }
                if let Some(resume) = &request.resume {
                    self.check_key(e, &fields, &resume.key, false)?;
                }
                let rows: Vec<_> = self
                    .ordered(data, e, &request.index.name, &request.range, order)?
                    .into_iter()
                    .filter(|(_, key)| {
                        request.resume.as_ref().is_none_or(|r| {
                            if order == Order::Asc {
                                key_cmp(key, &r.key) == Ordering::Greater
                            } else {
                                key_cmp(key, &r.key) == Ordering::Less
                            }
                        })
                    })
                    .collect();
                let mut result = Vec::new();
                let mut examined = 0;
                let mut last = None;
                let mut count = 0;
                for ((row, rev), key) in &rows {
                    if examined == request.budget.0 as usize
                        || (q.name == "scan" && result.len() == limit as usize)
                    {
                        break;
                    }
                    examined += 1;
                    last = Some(ResumeToken { key: key.clone() });
                    if matches(row, &request.filter) {
                        count += 1;
                        if q.name == "scan" {
                            result.push((self.encode_row(&e.name, row.clone())?, *rev));
                        }
                    }
                }
                let more = examined < rows.len();
                let exhausted_budget = more && examined == request.budget.0 as usize;
                if q.name == "count" {
                    encode(&if more {
                        CountResult::AtLeast(count)
                    } else {
                        CountResult::Exact(count)
                    })?
                } else {
                    encode(&Page {
                        rows: result,
                        resume: if more { last } else { None },
                        exhausted_budget,
                    })?
                }
            }
            _ => return Err(unsupported("unknown query")),
        };
        Ok(AnyOutput {
            name: q.name,
            domain: q.domain,
            body: Payload::Json(body),
        })
    }
    fn encode_row(&self, name: &str, row: Row) -> Result<RawJson, Error> {
        match self.codecs.get(name) {
            Some(codec) => codec(row),
            None => encode(&row),
        }
    }
    fn apply(
        &self,
        data: &mut Data,
        uow: &UnitOfWork,
        rev: Rev,
    ) -> Result<Vec<(StreamName, Vec<Seq>)>, CommitError> {
        let mut failures = Vec::new();
        for condition in &uow.preconditions {
            let (ok, actual_rev, actual_position) = match condition {
                Precondition::Cursor { name, at } => {
                    let actual = data.cursors.get(&(uow.domain, name.0.clone())).cloned();
                    (actual == *at, None, actual)
                }
                Precondition::Rev { entity, key, .. }
                | Precondition::Absent { entity, key }
                | Precondition::Present { entity, key } => {
                    let e = self.entity(entity, uow.domain).map_err(fatal)?;
                    let found = self
                        .locate(data, e, key)
                        .map_err(fatal)?
                        .map(|n| data.rows[entity][n].1);
                    let ok = match condition {
                        Precondition::Rev { rev, .. } => found == Some(*rev),
                        Precondition::Absent { .. } => found.is_none(),
                        _ => found.is_some(),
                    };
                    (ok, found, None)
                }
            };
            if !ok {
                failures.push(FailedPrecondition {
                    condition: condition.clone(),
                    actual_rev,
                    actual_position,
                });
            }
        }
        if !failures.is_empty() {
            return Err(CommitError::Conflict(failures));
        }
        let mut sequences = Vec::new();
        for mutation in &uow.mutations {
            match mutation {
                Mutation::Insert { entity, row } | Mutation::Upsert { entity, row } => {
                    let e = self.entity(entity, uow.domain).map_err(fatal)?;
                    if matches!(e.kind, EntityKind::Append { .. }) {
                        return Err(fatal(invalid("append entities require Append")));
                    }
                    self.check_row(e, row).map_err(fatal)?;
                    let found = self.locate(data, e, &values(row, &e.key)).map_err(fatal)?;
                    if matches!(mutation, Mutation::Insert { .. }) && found.is_some() {
                        return Err(CommitError::Unique {
                            entity: entity.clone(),
                            index: String::new(),
                        });
                    }
                    self.unique(data, e, row, found)?;
                    let rows = data.rows.entry(entity.clone()).or_default();
                    if let Some(n) = found {
                        rows[n] = (row.clone(), rev);
                    } else {
                        rows.push((row.clone(), rev));
                    }
                }
                Mutation::Update {
                    entity,
                    key,
                    set: patch,
                } => {
                    let e = self.entity(entity, uow.domain).map_err(fatal)?;
                    if matches!(e.kind, EntityKind::Append { .. }) {
                        return Err(fatal(invalid("append entities are immutable")));
                    }
                    let found = self.locate(data, e, key).map_err(fatal)?;
                    let names: BTreeSet<_> = patch.0.iter().map(|(name, _)| name).collect();
                    if names.len() != patch.0.len()
                        || patch
                            .0
                            .iter()
                            .any(|(name, _)| !e.fields.iter().any(|f| f.name == *name))
                    {
                        return Err(fatal(invalid("invalid update fields")));
                    }
                    for (name, value) in &patch.0 {
                        let field = e
                            .fields
                            .iter()
                            .find(|f| f.name == *name)
                            .expect("validated update field");
                        Self::check_cell(field, value).map_err(fatal)?;
                    }
                    if let Some(n) = found {
                        let mut row = data.rows[entity][n].0.clone();
                        for (field, value) in &patch.0 {
                            set(&mut row, field, value.clone());
                        }
                        self.check_row(e, &row).map_err(fatal)?;
                        self.unique(data, e, &row, Some(n))?;
                        data.rows.get_mut(entity).expect("located row")[n] = (row, rev);
                    }
                }
                Mutation::Delete { entity, key } => {
                    let e = self.entity(entity, uow.domain).map_err(fatal)?;
                    if let Some(n) = self.locate(data, e, key).map_err(fatal)? {
                        data.rows.get_mut(entity).expect("located row").remove(n);
                    }
                }
                Mutation::DeleteWhere {
                    entity,
                    index,
                    range,
                    filter,
                    limit,
                } => {
                    if !(1..=500).contains(limit) {
                        return Err(fatal(invalid("invalid delete limit")));
                    }
                    let e = self.entity(entity, uow.domain).map_err(fatal)?;
                    self.check_filter(e, filter).map_err(fatal)?;
                    let keys: Vec<_> = self
                        .ordered(data, e, index, range, Order::Asc)
                        .map_err(fatal)?
                        .into_iter()
                        .filter(|((row, _), _)| matches(row, filter))
                        .take(*limit as usize)
                        .map(|((row, _), _)| values(row, &e.key))
                        .collect();
                    if let Some(rows) = data.rows.get_mut(entity) {
                        rows.retain(|(row, _)| !keys.contains(&values(row, &e.key)));
                    }
                }
                Mutation::Append { stream, records } => {
                    let e = self.entity(&stream.0, uow.domain).map_err(fatal)?;
                    let EntityKind::Append { seq, .. } = &e.kind else {
                        return Err(fatal(invalid("not an append stream")));
                    };
                    let mut allocated = Vec::new();
                    for record in records {
                        if cell(record, seq).is_some() {
                            return Err(fatal(invalid("sequence is backend allocated")));
                        }
                        let next = data
                            .seqs
                            .get(&(uow.domain, stream.0.clone()))
                            .copied()
                            .unwrap_or(0)
                            .checked_add(1)
                            .ok_or_else(|| fatal(invalid("sequence exhausted")))?;
                        let value = i64::try_from(next)
                            .map_err(|_| fatal(invalid("sequence exhausted")))?;
                        let mut row = record.clone();
                        set(&mut row, seq, Cell::Int(value));
                        self.check_row(e, &row).map_err(fatal)?;
                        self.unique(data, e, &row, None)?;
                        data.rows
                            .entry(e.name.clone())
                            .or_default()
                            .push((row, rev));
                        data.seqs.insert((uow.domain, stream.0.clone()), next);
                        allocated.push(Seq(value));
                    }
                    sequences.push((stream.clone(), allocated));
                }
                Mutation::AdvanceCursor { name, to } => {
                    let key = (uow.domain, name.0.clone());
                    if data
                        .cursors
                        .get(&key)
                        .is_some_and(|old| !same_shape(&old.0, &to.0) || to < old)
                    {
                        return Err(fatal(invalid(
                            "cursor must not move backwards or change shape",
                        )));
                    }
                    data.cursors.insert(key, to.clone());
                }
                Mutation::Op(op) => {
                    if op.domain != uow.domain {
                        return Err(fatal(invalid("mixed-domain operation")));
                    }
                    super::digest::apply(data, &self.entities, op, rev).map_err(fatal)?;
                }
            }
        }
        for fact in &uow.facts {
            let domain = self
                .topics
                .get(&fact.topic)
                .ok_or_else(|| fatal(unsupported("unknown fact topic")))?;
            if *domain != uow.domain {
                return Err(fatal(invalid("mixed-domain fact")));
            }
            if data
                .facts
                .insert((uow.domain, fact.fact_id.clone()), fact.clone())
                .is_some()
            {
                return Err(CommitError::Unique {
                    entity: "facts".into(),
                    index: "fact_id".into(),
                });
            }
        }
        Ok(sequences)
    }
    fn validate_references(&self, unit: &UnitOfWork) -> Result<(), Error> {
        for condition in &unit.preconditions {
            match condition {
                Precondition::Rev { entity, .. }
                | Precondition::Present { entity, .. }
                | Precondition::Absent { entity, .. } => {
                    self.entity(entity, unit.domain)?;
                }
                _ => {}
            }
        }
        for mutation in &unit.mutations {
            match mutation {
                Mutation::Insert { entity, .. }
                | Mutation::Upsert { entity, .. }
                | Mutation::Update { entity, .. }
                | Mutation::Delete { entity, .. }
                | Mutation::DeleteWhere { entity, .. } => {
                    self.entity(entity, unit.domain)?;
                }
                Mutation::Append { stream, .. } => {
                    self.entity(&stream.0, unit.domain)?;
                }
                Mutation::Op(op) if op.domain != unit.domain => {
                    return Err(invalid("mixed-domain operation"));
                }
                _ => {}
            }
        }
        for fact in &unit.facts {
            match self.topics.get(&fact.topic) {
                Some(domain) if *domain != unit.domain => return Err(invalid("mixed-domain fact")),
                None => return Err(unsupported("unknown fact topic")),
                _ => {}
            }
        }
        Ok(())
    }
    fn natural_key(&self, uow: &UnitOfWork) -> Result<Option<NaturalKey>, CommitError> {
        let Idempotency::Natural { entity, index } = &uow.idempotency else {
            return Ok(None);
        };
        let e = self.entity(entity, uow.domain).map_err(fatal)?;
        // A single immutable insert with no additional effects is provably natural.
        // More complex units require a domain-specific proof and are rejected closed.
        let [
            Mutation::Insert {
                entity: target,
                row,
            },
        ] = uow.mutations.as_slice()
        else {
            return Err(fatal(invalid("unproven natural work unit")));
        };
        if target != entity || !uow.facts.is_empty() || !uow.preconditions.is_empty() {
            return Err(fatal(invalid("natural work unit has side effects")));
        }
        let fields = if index.is_empty() {
            &e.key
        } else {
            &e.indexes
                .iter()
                .find(|i| i.name == *index && i.unique)
                .ok_or_else(|| fatal(invalid("natural index must be unique")))?
                .fields
        };
        let key = values(row, fields);
        if key.contains(&Cell::Null) {
            return Err(fatal(invalid("natural index must not contain null")));
        }
        Ok(Some((
            uow.domain,
            entity.clone(),
            index.clone(),
            serde_json::to_vec(&key).map_err(|_| fatal(invalid("invalid natural key")))?,
        )))
    }
}

/// SHA-256 of canonical batch fields excluding digest itself.
pub fn batch_digest(batch: &ExportBatch) -> Result<RequestDigest, Error> {
    #[derive(Serialize)]
    struct Contents<'a> {
        session: &'a ExportSession,
        id: &'a str,
        entity: &'a EntityDesc,
        rows: &'a [(Row, Rev)],
        state: &'a RawJson,
        next: &'a Option<ExportCursor>,
    }
    let content = Contents {
        session: &batch.session,
        id: &batch.id,
        entity: &batch.entity,
        rows: &batch.rows,
        state: &batch.state,
        next: &batch.next,
    };
    Ok(RequestDigest(
        Sha256::digest(crate::uow::canonical_json(&encode(&content)?)?).into(),
    ))
}
#[async_trait]
impl DomainStore for MemStore {
    fn hello(&self) -> &Hello {
        &self.hello
    }
    async fn query(&self, q: AnyQuery) -> Result<AnyOutput, Error> {
        let state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        self.query_data(&state.data, q)
    }
    async fn snapshot(&self, domain: Domain, qs: Vec<AnyQuery>) -> Result<Vec<AnyOutput>, Error> {
        if qs.iter().any(|q| q.domain != domain) {
            return Err(invalid("mixed-domain snapshot"));
        }
        let state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        qs.into_iter()
            .map(|q| self.query_data(&state.data, q))
            .collect()
    }
    async fn begin_snapshot(&self, domain: Domain, _lane: Lane) -> Result<LeaseId, Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        let now = self.clock.now();
        state
            .leases
            .retain(|_, (lease, _, _)| lease.expires_at.0 > now.0);
        state.next_handle += 1;
        let lease = LeaseId {
            id: format!("lease-{}-{}", self.instance, state.next_handle),
            expires_at: Millis(
                now.0
                    .checked_add(2000)
                    .ok_or_else(|| invalid("clock overflow"))?,
            ),
        };
        let snapshot = state.data.clone();
        state
            .leases
            .insert(lease.id.clone(), (lease.clone(), domain, snapshot));
        Ok(lease)
    }
    async fn lease_query(&self, lease: LeaseId, q: AnyQuery) -> Result<AnyOutput, Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        let (actual, _, _) = state.leases.get(&lease.id).ok_or_else(stale)?;
        if actual != &lease {
            return Err(stale());
        }
        if self.clock.now().0 >= actual.expires_at.0 {
            state.leases.remove(&lease.id);
            return Err(stale());
        }
        let (_, domain, data) = state.leases.get(&lease.id).ok_or_else(stale)?;
        if *domain != q.domain {
            return Err(invalid("mixed-domain lease"));
        }
        self.query_data(data, q)
    }
    async fn lease_end(&self, lease: LeaseId) -> Result<(), Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        if state
            .leases
            .get(&lease.id)
            .is_none_or(|(actual, _, _)| actual != &lease)
        {
            return Err(stale());
        }
        state.leases.remove(&lease.id);
        if self.clock.now().0 >= lease.expires_at.0 {
            return Err(stale());
        }
        Ok(())
    }
    async fn commit(&self, uow: UnitOfWork) -> Result<Committed, CommitError> {
        uow.validate_age(self.clock.now())?;
        uow.verify_digest().map_err(fatal)?;
        self.validate_references(&uow).map_err(fatal)?;
        let natural = self.natural_key(&uow)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| fatal(error(Code::Internal, "memory lock poisoned")))?;
        let key = (uow.domain, uow.id.issued_at.0, uow.id.key.clone());
        let receipt = if let Some(natural) = &natural {
            state.natural.get(natural)
        } else {
            state.receipts.get(&key)
        };
        if let Some(receipt) = receipt {
            if receipt.digest != uow.digest {
                return Err(CommitError::IdempotencyMismatch);
            }
            let mut result = receipt.result.clone();
            result.replayed = true;
            return Ok(result);
        }
        let rev = Rev(state
            .rev
            .checked_add(1)
            .ok_or_else(|| fatal(invalid("revision exhausted")))?);
        let mut candidate = state.data.clone();
        let seqs = self.apply(&mut candidate, &uow, rev)?;
        let result = Committed {
            rev,
            seqs,
            replayed: false,
        };
        let receipt = Receipt {
            digest: uow.digest,
            result: result.clone(),
        };
        state.data = candidate;
        state.rev = rev.0;
        let now = self.clock.now().0;
        if i128::from(now) - i128::from(state.last_cleanup) >= 86_400_000 {
            state
                .receipts
                .retain(|key, _| i128::from(now) - i128::from(key.1) <= 31 * 86_400_000);
            state.last_cleanup = now;
        }
        if let Some(natural) = natural {
            state.natural.insert(natural, receipt);
        } else {
            state.receipts.insert(key, receipt);
        }
        Ok(result)
    }
    async fn commit_many(&self, uows: Vec<UnitOfWork>) -> Vec<Result<Committed, CommitError>> {
        let mut results = Vec::with_capacity(uows.len());
        for uow in uows {
            results.push(self.commit(uow).await);
        }
        results
    }
    async fn begin_export(&self, domain: Domain) -> Result<ExportSession, Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        if !self.entities.values().any(|e| e.domain == domain) {
            return Err(unsupported("no declared entities in domain"));
        }
        state.next_handle += 1;
        let session = ExportSession {
            id: format!("export-{}-{}", self.instance, state.next_handle),
            domain,
            version: ContractVersion(1),
            layout: 1,
            schema: 1,
            rev: Rev(state.rev),
        };
        let metadata = TransferState {
            receipts: state
                .receipts
                .iter()
                .filter(|(k, _)| k.0 == domain)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            natural: state
                .natural
                .iter()
                .filter(|(k, _)| k.0 == domain)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            cursors: state
                .data
                .cursors
                .iter()
                .filter(|(k, _)| k.0 == domain)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            seqs: state
                .data
                .seqs
                .iter()
                .filter(|(k, _)| k.0 == domain)
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            facts: state
                .data
                .facts
                .iter()
                .filter(|(k, _)| k.0 == domain)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        let data = state.data.clone();
        state.exports.insert(
            session.id.clone(),
            Export {
                session: session.clone(),
                data,
                metadata,
            },
        );
        Ok(session)
    }
    async fn export(
        &self,
        session: &ExportSession,
        cursor: ExportCursor,
    ) -> Result<ExportBatch, Error> {
        let state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        let export = state
            .exports
            .get(&session.id)
            .filter(|e| e.session == *session)
            .ok_or_else(stale)?;
        let entities: Vec<_> = self
            .entities
            .values()
            .filter(|e| e.domain == session.domain)
            .collect();
        let entity = if cursor.entity.is_empty() {
            if cursor.after.is_some() {
                return Err(invalid("initial export cursor has a key"));
            }
            entities[0]
        } else {
            self.entity(&cursor.entity, session.domain)?
        };
        if cursor
            .after
            .as_ref()
            .is_some_and(|key| key.len() != entity.key.len())
        {
            return Err(invalid("invalid export key"));
        }
        let ordered = self.ordered(&export.data, entity, "", &KeyRange::default(), Order::Asc)?;
        let remaining: Vec<_> = ordered
            .into_iter()
            .filter(|(_, key)| {
                cursor
                    .after
                    .as_ref()
                    .is_none_or(|after| key_cmp(key, after) == Ordering::Greater)
            })
            .collect();
        let rows: Vec<_> = remaining
            .iter()
            .take(128)
            .map(|(row, _)| (*row).clone())
            .collect();
        let next = if remaining.len() > rows.len() {
            Some(ExportCursor {
                entity: entity.name.clone(),
                after: rows.last().map(|(row, _)| values(row, &entity.key)),
            })
        } else {
            entities
                .iter()
                .position(|e| e.name == entity.name)
                .and_then(|n| entities.get(n + 1))
                .map(|e| ExportCursor {
                    entity: e.name.clone(),
                    after: None,
                })
        };
        let id = format!(
            "{}:{}:{}",
            session.id,
            entity.name,
            serde_json::to_string(&cursor.after).map_err(|_| invalid("invalid cursor"))?
        );
        let mut batch = ExportBatch {
            session: session.clone(),
            id,
            entity: entity.clone(),
            rows,
            state: encode(&export.metadata)?,
            next,
            digest: RequestDigest([0; 32]),
        };
        batch.digest = batch_digest(&batch)?;
        Ok(batch)
    }
    async fn import(&self, batch: ExportBatch) -> Result<ImportReport, Error> {
        if batch_digest(&batch)? != batch.digest {
            return Err(invalid("import checksum mismatch"));
        }
        if batch.session.version != ContractVersion(1)
            || batch.entity.domain != batch.session.domain
        {
            return Err(invalid("incompatible import domain/version"));
        }
        let e = self.entity(&batch.entity.name, batch.session.domain)?;
        if e != &batch.entity {
            return Err(invalid("import descriptor mismatch"));
        }
        let metadata: TransferState = decode(&batch.state)?;
        let domain = batch.session.domain;
        if metadata.receipts.iter().any(|(k, _)| k.0 != domain)
            || metadata.natural.iter().any(|(k, _)| k.0 != domain)
            || metadata.cursors.iter().any(|(k, _)| k.0 != domain)
            || metadata.seqs.iter().any(|(k, _)| k.0 != domain)
            || metadata.facts.iter().any(|(k, _)| k.0 != domain)
        {
            return Err(invalid("mixed-domain import metadata"));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        if let Some(digest) = state.imported.get(&batch.id) {
            if *digest != batch.digest {
                return Err(invalid("import batch identity mismatch"));
            }
            return Ok(ImportReport {
                batch_id: batch.id,
                imported: 0,
                replayed: true,
            });
        }
        let mut candidate = state.data.clone();
        let mut previous = None;
        for (row, rev) in &batch.rows {
            self.check_row(e, row)?;
            if rev.0 > batch.session.rev.0 {
                return Err(invalid("import revision exceeds snapshot"));
            }
            let key = values(row, &e.key);
            if previous
                .as_ref()
                .is_some_and(|last: &Vec<Cell>| key_cmp(last, &key) != Ordering::Less)
            {
                return Err(invalid("import rows not in primary key order"));
            }
            previous = Some(key.clone());
            let existing = self.locate(&candidate, e, &key)?;
            if existing.is_some() {
                return Err(invalid("import key already exists"));
            }
            self.unique(&candidate, e, row, None).map_err(Error::from)?;
            candidate
                .rows
                .entry(e.name.clone())
                .or_default()
                .push((row.clone(), *rev));
        }
        for ((_, topic_id), fact) in &metadata.facts {
            if *topic_id != fact.fact_id || self.topics.get(&fact.topic) != Some(&domain) {
                return Err(invalid("invalid imported fact"));
            }
        }
        candidate.cursors.extend(metadata.cursors);
        candidate.seqs.extend(metadata.seqs);
        candidate.facts.extend(metadata.facts);
        state.data = candidate;
        state.receipts.extend(metadata.receipts);
        state.natural.extend(metadata.natural);
        state.rev = state.rev.max(batch.session.rev.0);
        state.imported.insert(batch.id.clone(), batch.digest);
        Ok(ImportReport {
            batch_id: batch.id,
            imported: batch.rows.len() as u64,
            replayed: false,
        })
    }
    async fn retention(&self, entity: String, budget: Budget) -> Result<RetentionReport, Error> {
        let started = Instant::now();
        if budget.rows == 0 || budget.target_ms == 0 {
            return Err(invalid("invalid retention budget"));
        }
        let e = self
            .entities
            .get(&entity)
            .ok_or_else(|| unsupported("unknown retention entity"))?;
        let policy = e
            .retention
            .as_ref()
            .ok_or_else(|| unsupported("entity has no retention policy"))?;
        if policy.before.is_some()
            || policy.protect.is_some()
            || matches!(policy.action, Action::Op(_))
        {
            return Err(unsupported("unregistered retention hook"));
        }
        let keep = match policy.keep_from {
            KeepFrom::Fixed { millis } => millis,
            KeepFrom::Setting { default_ms, .. } => default_ms,
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(Code::Internal, "memory lock poisoned"))?;
        let ordered = self.ordered(&state.data, e, "", &KeyRange::default(), Order::Asc)?;
        let after = state.retention_after.get(&entity);
        let remaining: Vec<_> = ordered
            .iter()
            .filter(|(_, key)| after.is_none_or(|after| key_cmp(key, after) == Ordering::Greater))
            .collect();
        let inspected: Vec<_> = remaining
            .iter()
            .take(budget.rows as usize)
            .copied()
            .collect();
        let mut eligible = Vec::new();
        let now = self.clock.now().0;
        for ((row, _), key) in &inspected {
            let selected = match &policy.select {
                Select::MaxAge { field, .. } => {
                    matches!(cell(row,field),Some(Cell::Millis(at)) if i128::from(now)-i128::from(*at)>i128::from(keep))
                }
                Select::Terminal {
                    state: field,
                    done,
                    since,
                    ..
                } => {
                    cell(row, field).is_some_and(|v| done.contains(v))
                        && matches!(cell(row,since),Some(Cell::Millis(at)) if i128::from(now)-i128::from(*at)>i128::from(keep))
                }
                Select::MaxRows { group, keep, order } => {
                    let group_key = values(row, group);
                    let mut peers: Vec<_> = ordered
                        .iter()
                        .filter(|((r, _), _)| values(r, group) == group_key)
                        .collect();
                    peers.sort_by(|((a, _), ak), ((b, _), bk)| {
                        order
                            .iter()
                            .map(|(field, dir)| {
                                let c = cmp(
                                    cell(a, field).unwrap_or(&Cell::Null),
                                    cell(b, field).unwrap_or(&Cell::Null),
                                );
                                if *dir == Order::Desc { c.reverse() } else { c }
                            })
                            .find(|o| *o != Ordering::Equal)
                            .unwrap_or_else(|| key_cmp(ak, bk))
                    });
                    peers
                        .iter()
                        .position(|(_, k)| k == key)
                        .is_some_and(|n| n as u64 >= *keep)
                }
                Select::Bytes { group, max } => {
                    // The reference fake measures encoded row bytes and keeps the newest
                    // primary keys, consistently with its deterministic export ordering.
                    let group_key = values(row, group);
                    let mut bytes = 0u64;
                    let mut selected = false;
                    for ((peer, _), peer_key) in ordered
                        .iter()
                        .rev()
                        .filter(|((peer, _), _)| values(peer, group) == group_key)
                    {
                        bytes = bytes.saturating_add(
                            serde_json::to_vec(peer)
                                .map_err(|_| invalid("invalid retention row"))?
                                .len() as u64,
                        );
                        if *peer_key == *key {
                            selected = bytes > *max;
                            break;
                        }
                    }
                    selected
                }
            };
            if selected
                && match &policy.action {
                    Action::Purge { fields } => fields
                        .iter()
                        .any(|field| cell(row, field).is_some_and(|value| *value != Cell::Null)),
                    _ => true,
                }
            {
                eligible.push((*key).clone());
            }
        }
        let more = remaining.len() > inspected.len();
        let last = inspected.last().map(|(_, key)| (*key).clone());
        let mut candidate = state.data.clone();
        if let Some(rows) = candidate.rows.get_mut(&entity) {
            match &policy.action {
                Action::Delete => rows.retain(|(row, _)| !eligible.contains(&values(row, &e.key))),
                Action::Purge { fields } => {
                    let rev = Rev(state
                        .rev
                        .checked_add(1)
                        .ok_or_else(|| invalid("revision exhausted"))?);
                    for (row, row_rev) in rows {
                        if eligible.contains(&values(row, &e.key)) {
                            for field in fields {
                                set(row, field, Cell::Null);
                            }
                            self.check_row(e, row)?;
                            *row_rev = rev;
                        }
                    }
                }
                Action::Op(_) => unreachable!("hooks rejected before lock"),
            }
        }
        if !eligible.is_empty() {
            state.rev += 1;
        }
        state.data = candidate;
        if more {
            state
                .retention_after
                .insert(entity.clone(), last.expect("positive budget"));
        } else {
            state.retention_after.remove(&entity);
        }
        let elapsed = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        Ok(RetentionReport {
            entity,
            affected: eligible.len() as u64,
            slices: 1,
            max_slice_ms: elapsed,
            overruns: u32::from(elapsed > budget.target_ms),
            more,
        })
    }
}

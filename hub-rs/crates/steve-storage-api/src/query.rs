//! Bounded generic queries and named typed queries. A scan inspects an index window:
//! starting strictly after resume, take at most budget index entries, then filter only
//! inside that window. resume is the last examined index key (including PK tie breakers),
//! even if no row matched. Stop the window early when limit matches have been collected;
//! never inspect further entries then truncate matches, as that would skip rows on resume.
//! exhausted_budget means the inspection budget stopped traversal before the range ended.
use crate::{
    Domain, Error, RawJson, Rev,
    entity::{Cell, StoredEntity},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{any::Any, fmt, marker::PhantomData};

/// Statically typed named query. Generic Hub wrappers encode it with to_wire().
pub trait Query: Serialize + DeserializeOwned + Send + 'static {
    const NAME: &'static str;
    const DOMAIN: Domain;
    type Output: Serialize + DeserializeOwned + Send + 'static;
    fn to_wire(&self) -> Result<AnyQuery, Error> {
        Ok(AnyQuery {
            name: Self::NAME.into(),
            domain: Self::DOMAIN,
            body: Payload::Json(encode(self)?),
        })
    }
}

/// An optional local fast path; the runtime can downcast or force serialization.
pub trait LocalPayload: Any + Send {
    fn as_any(&self) -> &dyn Any;
    fn to_json(&self) -> Result<RawJson, Error>;
}
impl<T: Any + Send + Serialize> LocalPayload for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn to_json(&self) -> Result<RawJson, Error> {
        encode(self)
    }
}
/// Local values serialize exactly like Json; deserialization always produces Json.
pub enum Payload {
    Json(RawJson),
    Local(Box<dyn LocalPayload>),
}
impl Payload {
    pub fn to_json(&self) -> Result<RawJson, Error> {
        match self {
            Self::Json(v) => Ok(v.clone()),
            Self::Local(v) => v.to_json(),
        }
    }
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, Error> {
        decode(&self.to_json()?)
    }
}
impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(v) => f.debug_tuple("Json").field(v).finish(),
            Self::Local(_) => f.write_str("Local(..)"),
        }
    }
}
impl Serialize for Payload {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json()
            .map_err(serde::ser::Error::custom)?
            .serialize(s)
    }
}
impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawJson::deserialize(d).map(Self::Json)
    }
}
/// Owned query envelope. The name identifies the body type within the negotiated version.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnyQuery {
    pub name: String,
    pub domain: Domain,
    pub body: Payload,
}
/// Owned output envelope, carrying the originating query name and domain.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnyOutput {
    pub name: String,
    pub domain: Domain,
    pub body: Payload,
}
impl AnyQuery {
    /// Reject mismatched names/domains instead of guessing a payload's type.
    pub fn decode<Q: Query>(&self) -> Result<Q, Error> {
        check_name(&self.name, self.domain, Q::NAME, Q::DOMAIN)?;
        self.body.decode()
    }
}
impl AnyOutput {
    pub fn decode<Q: Query>(&self) -> Result<Q::Output, Error> {
        check_name(&self.name, self.domain, Q::NAME, Q::DOMAIN)?;
        self.body.decode()
    }
}
fn check_name(
    name: &str,
    domain: Domain,
    expected: &str,
    expected_domain: Domain,
) -> Result<(), Error> {
    if name != expected || domain != expected_domain {
        return Err(crate::error(
            steve_types::Code::Unsupported,
            "unknown query type",
        ));
    }
    Ok(())
}
/// Encode typed data only through the opaque RawJson boundary.
pub fn encode<T: Serialize + ?Sized>(v: &T) -> Result<RawJson, Error> {
    let text = serde_json::to_string(v)
        .map_err(|_| crate::error(steve_types::Code::Invalid, "invalid storage payload"))?;
    RawJson::from_str(&text)
        .map_err(|_| crate::error(steve_types::Code::Invalid, "invalid JSON payload"))
}
/// Decode an opaque payload into an explicitly selected contract type.
pub fn decode<T: DeserializeOwned>(v: &RawJson) -> Result<T, Error> {
    let text = serde_json::to_string(v)
        .map_err(|_| crate::error(steve_types::Code::Invalid, "invalid JSON payload"))?;
    serde_json::from_str(&text)
        .map_err(|_| crate::error(steve_types::Code::Invalid, "invalid storage payload"))
}

/// Full primary key, plus its stable entity name. The marker never travels on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Key<E> {
    pub entity: String,
    pub values: Vec<Cell>,
    #[serde(skip)]
    pub marker: PhantomData<E>,
}
impl<E: StoredEntity> Key<E> {
    pub fn new(values: Vec<Cell>) -> Self {
        Self {
            entity: E::ENTITY.name.into(),
            values,
            marker: PhantomData,
        }
    }
}
/// Named index of one entity; an empty name designates the primary key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexName {
    pub entity: String,
    pub name: String,
}
/// Endpoint of a lexicographic index-key range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyBound {
    Unbounded,
    Included(Vec<Cell>),
    Excluded(Vec<Cell>),
}
/// Index range in ascending key order, independent of scan direction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyRange {
    pub start: KeyBound,
    pub end: KeyBound,
}
impl Default for KeyRange {
    fn default() -> Self {
        Self {
            start: KeyBound::Unbounded,
            end: KeyBound::Unbounded,
        }
    }
}
/// Conjunctive predicates applied only after forming the index window. Null uses IsNull.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pred {
    Eq { field: String, value: Cell },
    Ne { field: String, value: Cell },
    Lt { field: String, value: Cell },
    Le { field: String, value: Cell },
    Gt { field: String, value: Cell },
    Ge { field: String, value: Cell },
    In { field: String, values: Vec<Cell> },
    IsNull { field: String, is_null: bool },
}
/// Direction of index traversal; continuation always means strictly after in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    Asc,
    Desc,
}
/// Maximum index entries inspected, regardless of predicate selectivity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanBudget(pub u32);
impl Default for ScanBudget {
    fn default() -> Self {
        Self(5_000)
    }
}
/// Last examined total index key. Includes primary-key tie breakers; never a matched-row offset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResumeToken {
    pub key: Vec<Cell>,
}
/// A bounded window result, which may be empty and still have a continuation token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page<E> {
    pub rows: Vec<(E, Rev)>,
    pub resume: Option<ResumeToken>,
    pub exhausted_budget: bool,
}
/// Count is exact only when the range ends before the inspection budget is exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountResult {
    Exact(u64),
    AtLeast(u64),
}

/// Primary-key lookup, returning the typed row and its committed revision if present.
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Get<E> {
    pub key: Key<E>,
}
/// Bounded index-window scan; limit is 1..=500, budget must be positive.
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Scan<E> {
    pub index: IndexName,
    pub range: KeyRange,
    pub filter: Vec<Pred>,
    pub order: Order,
    pub limit: u32,
    pub budget: ScanBudget,
    pub resume: Option<ResumeToken>,
    #[serde(skip)]
    pub marker: PhantomData<E>,
}
/// Bounded count; a lower bound replaces an unbounded full-range count.
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Count<E> {
    pub index: IndexName,
    pub range: KeyRange,
    pub filter: Vec<Pred>,
    pub budget: ScanBudget,
    #[serde(skip)]
    pub marker: PhantomData<E>,
}
/// Primary-key existence test.
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Exists<E> {
    pub key: Key<E>,
}
impl<E: StoredEntity> Query for Get<E> {
    const NAME: &'static str = "get";
    const DOMAIN: Domain = E::ENTITY.domain;
    type Output = Option<(E, Rev)>;
}
impl<E: StoredEntity> Query for Scan<E> {
    const NAME: &'static str = "scan";
    const DOMAIN: Domain = E::ENTITY.domain;
    type Output = Page<E>;
}
impl<E: StoredEntity> Query for Count<E> {
    const NAME: &'static str = "count";
    const DOMAIN: Domain = E::ENTITY.domain;
    type Output = CountResult;
}
impl<E: StoredEntity> Query for Exists<E> {
    const NAME: &'static str = "exists";
    const DOMAIN: Domain = E::ENTITY.domain;
    type Output = bool;
}
/// Consumer cursor lookup is domain-local and participates in snapshot reads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CursorGet {
    pub domain: Domain,
    pub name: crate::cursor::CursorName,
}

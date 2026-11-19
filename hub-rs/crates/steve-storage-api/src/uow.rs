//! Atomic single-domain work units. Intent IDs survive retries; lane is attempt metadata.
use crate::{
    Domain, Error, Lane, Millis, RawJson, Rev,
    append::{Seq, StreamName},
    cursor::{CursorName, Position},
    entity::{Cell, Row},
    fact::FactRecord,
    query::{KeyRange, Pred},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Stable intent key, signed by the caller's wall clock (not regenerated for each attempt).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdempotencyKey {
    pub issued_at: Millis,
    pub key: String,
}
/// Recorded stores the complete result for 31 days and accepts retries for 30 days.
/// Natural relies on the named unique index; the backend must validate that the entire
/// unit is naturally idempotent, including facts and side effects, or reject it as invalid.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    #[default]
    Recorded,
    Natural {
        entity: String,
        index: String,
    },
}

/// SHA-256 of canonical request content; never a backend-specific hasher or random salt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestDigest(pub [u8; 32]);

/// Owned command. Events are deliberately absent. Backends recompute digest and validate
/// every referenced entity/topic belongs to domain, even when domains share a file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnitOfWork {
    pub domain: Domain,
    pub id: IdempotencyKey,
    pub digest: RequestDigest,
    pub lane: Lane,
    #[serde(default)]
    pub idempotency: Idempotency,
    pub preconditions: Vec<Precondition>,
    pub mutations: Vec<Mutation>,
    pub facts: Vec<FactRecord>,
}
/// Conditions are checked atomically; Cursor(None) means no existing row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precondition {
    Rev {
        entity: String,
        key: Vec<Cell>,
        rev: Rev,
    },
    Absent {
        entity: String,
        key: Vec<Cell>,
    },
    Present {
        entity: String,
        key: Vec<Cell>,
    },
    Cursor {
        name: CursorName,
        at: Option<Position>,
    },
}
/// Portable named operation. name fixes the parameter type within the negotiated version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnyOp {
    pub name: String,
    pub domain: Domain,
    pub body: RawJson,
}
/// Typed named command declaration; registry entries govern capability advertisement.
pub trait Op: Serialize + serde::de::DeserializeOwned + Send + 'static {
    const NAME: &'static str;
    const DOMAIN: Domain;
    fn to_wire(&self) -> Result<AnyOp, Error> {
        Ok(AnyOp {
            name: Self::NAME.into(),
            domain: Self::DOMAIN,
            body: crate::query::encode(self)?,
        })
    }
}
impl AnyOp {
    pub fn decode<O: Op>(&self) -> Result<O, Error> {
        if self.name != O::NAME || self.domain != O::DOMAIN {
            return Err(crate::error(
                steve_types::Code::Unsupported,
                "unknown operation type",
            ));
        }
        crate::query::decode(&self.body)
    }
}
/// Ordered effects; DeleteWhere is bounded, Append allocates commit-ordered sequence
/// numbers, and AdvanceCursor rejects backward movement. All effects share one transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mutation {
    Insert {
        entity: String,
        row: Row,
    },
    Upsert {
        entity: String,
        row: Row,
    },
    Update {
        entity: String,
        key: Vec<Cell>,
        set: Row,
    },
    Delete {
        entity: String,
        key: Vec<Cell>,
    },
    DeleteWhere {
        entity: String,
        index: String,
        range: KeyRange,
        filter: Vec<Pred>,
        limit: u32,
    },
    Append {
        stream: StreamName,
        records: Vec<Row>,
    },
    AdvanceCursor {
        name: CursorName,
        to: Position,
    },
    Op(AnyOp),
}
/// First commit's full result, replayed without new sequences, facts or process events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Committed {
    pub rev: Rev,
    pub seqs: Vec<(StreamName, Vec<Seq>)>,
    pub replayed: bool,
}
/// Failed condition plus the observed revision/position, when meaningful.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FailedPrecondition {
    pub condition: Precondition,
    pub actual_rev: Option<Rev>,
    pub actual_position: Option<Position>,
}
/// Stable commit failures and retry classifications. A unit inside a legacy root grant
/// fails immediately rather than deadlocking its own writer (UowInsideGrant = invalid/never).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum CommitError {
    #[error("precondition conflict")]
    Conflict(Vec<FailedPrecondition>),
    #[error("idempotency key reused with different content")]
    IdempotencyMismatch,
    #[error("idempotency key expired")]
    Expired,
    #[error("unique constraint: {entity}/{index}")]
    Unique { entity: String, index: String },
    #[error("retryable storage failure: {detail}")]
    Retryable {
        after_ms: Option<u64>,
        detail: String,
    },
    #[error("unit of work inside legacy root grant")]
    UowInsideGrant,
    #[error("{0}")]
    Fatal(Error),
}
impl From<CommitError> for Error {
    fn from(e: CommitError) -> Self {
        use steve_types::{Code, Retry};
        match e {
            CommitError::Fatal(e) => e,
            CommitError::Retryable { after_ms, detail } => Error {
                retry: Retry::Later { after_ms },
                ..crate::error(Code::Busy, detail)
            },
            CommitError::Expired => crate::error(Code::Stale, e.to_string()),
            CommitError::UowInsideGrant => crate::error(Code::Invalid, e.to_string()),
            _ => crate::error(Code::Conflict, e.to_string()),
        }
    }
}
impl UnitOfWork {
    /// Canonical encoding includes domain, idempotency mode, conditions, effects and facts;
    /// excludes id, issued_at, digest and lane. Lists retain execution order. Row fields
    /// sort by UTF-8 name; duplicate names are invalid. JSON object keys sort recursively.
    pub fn canonical_request(&self) -> Result<Vec<u8>, Error> {
        #[derive(Serialize)]
        struct Content<'a> {
            domain: Domain,
            idempotency: &'a Idempotency,
            preconditions: &'a [Precondition],
            mutations: Vec<Mutation>,
            facts: &'a [FactRecord],
        }
        let mut mutations = self.mutations.clone();
        for m in &mut mutations {
            match m {
                Mutation::Insert { row, .. } | Mutation::Upsert { row, .. } => canonical_row(row)?,
                Mutation::Update { set, .. } => canonical_row(set)?,
                Mutation::Append { records, .. } => {
                    for row in records {
                        canonical_row(row)?;
                    }
                }
                _ => {}
            }
        }
        canonical_json(&crate::query::encode(&Content {
            domain: self.domain,
            idempotency: &self.idempotency,
            preconditions: &self.preconditions,
            mutations,
            facts: &self.facts,
        })?)
    }
    pub fn request_digest(&self) -> Result<RequestDigest, Error> {
        Ok(RequestDigest(
            Sha256::digest(self.canonical_request()?).into(),
        ))
    }
    /// Backends call before dedup lookup; mismatch is a malformed request, never a retry.
    pub fn verify_digest(&self) -> Result<(), Error> {
        if self.request_digest()? != self.digest {
            return Err(crate::error(
                steve_types::Code::Invalid,
                "request digest mismatch",
            ));
        }
        Ok(())
    }
    /// Age validation precedes dedup lookup so pruned intent keys can never execute again.
    pub fn validate_age(&self, now: Millis) -> Result<(), CommitError> {
        let age = i128::from(now.0) - i128::from(self.id.issued_at.0);
        if age > 30 * 24 * 60 * 60 * 1_000 {
            return Err(CommitError::Expired);
        }
        if age < -5 * 60 * 1_000 {
            return Err(CommitError::Fatal(crate::error(
                steve_types::Code::Invalid,
                "intent timestamp is in the future",
            )));
        }
        Ok(())
    }
}
fn canonical_row(row: &mut Row) -> Result<(), Error> {
    row.0.sort_by(|a, b| a.0.cmp(&b.0));
    if row.0.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(crate::error(
            steve_types::Code::Invalid,
            "duplicate row field",
        ));
    }
    Ok(())
}

/// Deterministic JSON encoding for digests, not for ordinary DTO roundtrips. Recursively
/// sorts object keys, rejects duplicates, preserves array order, normalizes escaped strings
/// and numeric spelling using Serde's i64/u64/f64 decoding (integers retain all 64 bits).
/// The visitor stores encoded fragments only, never exposes a general JSON tree.
pub fn canonical_json(raw: &RawJson) -> Result<Vec<u8>, Error> {
    let text = serde_json::to_string(raw)
        .map_err(|_| crate::error(steve_types::Code::Invalid, "invalid JSON"))?;
    serde_json::from_str::<Canonical>(&text)
        .map(|c| c.0)
        .map_err(|_| crate::error(steve_types::Code::Invalid, "invalid canonical JSON"))
}
struct Canonical(Vec<u8>);
impl<'de> Deserialize<'de> for Canonical {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Canonical;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("canonical JSON")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Canonical, E> {
                Ok(Canonical(v.to_string().into_bytes()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Canonical, E> {
                Ok(Canonical(b"null".to_vec()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Canonical, E> {
                Ok(Canonical(v.to_string().into_bytes()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Canonical, E> {
                Ok(Canonical(v.to_string().into_bytes()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Canonical, E> {
                if !v.is_finite() {
                    return Err(E::custom("non-finite number"));
                }
                serde_json::to_vec(&if v == 0.0 { 0.0 } else { v })
                    .map(Canonical)
                    .map_err(E::custom)
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Canonical, E> {
                serde_json::to_vec(v).map(Canonical).map_err(E::custom)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Canonical, A::Error> {
                let mut out = vec![b'['];
                while let Some(Canonical(v)) = seq.next_element()? {
                    if out.len() > 1 {
                        out.push(b',');
                    }
                    out.extend(v);
                }
                out.push(b']');
                Ok(Canonical(out))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Canonical, A::Error> {
                let mut fields = std::collections::BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Canonical>()? {
                    if fields.insert(key, value.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                let mut out = vec![b'{'];
                for (key, value) in fields {
                    if out.len() > 1 {
                        out.push(b',');
                    }
                    out.extend(serde_json::to_vec(&key).map_err(serde::de::Error::custom)?);
                    out.push(b':');
                    out.extend(value);
                }
                out.push(b'}');
                Ok(Canonical(out))
            }
        }
        d.deserialize_any(Visitor)
    }
}

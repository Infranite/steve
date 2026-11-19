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
    /// Check semantic parameter combinations before a backend starts effects.
    fn validate(&self) -> Result<(), Error> {
        Ok(())
    }
    fn to_wire(&self) -> Result<AnyOp, Error> {
        self.validate()?;
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
        let op: O = crate::query::decode(&self.body)?;
        op.validate()?;
        Ok(op)
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

struct Encoder<'a> {
    text: &'a str,
    pos: usize,
}
impl Encoder<'_> {
    fn invalid() -> Error {
        crate::error(steve_types::Code::Invalid, "invalid canonical JSON")
    }
    fn ws(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }
    fn take(&mut self, byte: u8) -> bool {
        self.ws();
        if self.text.as_bytes().get(self.pos) == Some(&byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Result<String, Error> {
        self.ws();
        let start = self.pos;
        if !self.take(b'"') {
            return Err(Self::invalid());
        }
        while let Some(&byte) = self.text.as_bytes().get(self.pos) {
            self.pos += 1;
            match byte {
                b'\\' => self.pos += 1,
                b'"' => {
                    return serde_json::from_str(&self.text[start..self.pos])
                        .map_err(|_| Self::invalid());
                }
                _ => {}
            }
        }
        Err(Self::invalid())
    }
    fn value(&mut self, depth: u32) -> Result<Vec<u8>, Error> {
        if depth > 128 {
            return Err(Self::invalid());
        }
        self.ws();
        match self.text.as_bytes().get(self.pos) {
            Some(b'{') => {
                self.pos += 1;
                let mut fields = std::collections::BTreeMap::new();
                if !self.take(b'}') {
                    loop {
                        let key = self.string()?;
                        if !self.take(b':') {
                            return Err(Self::invalid());
                        }
                        let value = self.value(depth + 1)?;
                        if fields.insert(key, value).is_some() {
                            return Err(Self::invalid());
                        }
                        if self.take(b'}') {
                            break;
                        }
                        if !self.take(b',') {
                            return Err(Self::invalid());
                        }
                    }
                }
                let mut out = vec![b'{'];
                for (key, value) in fields {
                    if out.len() > 1 {
                        out.push(b',');
                    }
                    out.extend(serde_json::to_vec(&key).map_err(|_| Self::invalid())?);
                    out.push(b':');
                    out.extend(value);
                }
                out.push(b'}');
                Ok(out)
            }
            Some(b'[') => {
                self.pos += 1;
                let mut out = vec![b'['];
                if !self.take(b']') {
                    loop {
                        if out.len() > 1 {
                            out.push(b',');
                        }
                        out.extend(self.value(depth + 1)?);
                        if self.take(b']') {
                            break;
                        }
                        if !self.take(b',') {
                            return Err(Self::invalid());
                        }
                    }
                }
                out.push(b']');
                Ok(out)
            }
            Some(b'"') => serde_json::to_vec(&self.string()?).map_err(|_| Self::invalid()),
            Some(_) => {
                let start = self.pos;
                // RawJson has already validated every primitive, including arbitrary-size
                // number lexemes. Copy them exactly instead of reparsing with loss of precision.
                while self
                    .text
                    .as_bytes()
                    .get(self.pos)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b",]}".contains(b))
                {
                    self.pos += 1;
                }
                if start == self.pos {
                    return Err(Self::invalid());
                }
                Ok(self.text.as_bytes()[start..self.pos].to_vec())
            }
            None => Err(Self::invalid()),
        }
    }
}

/// Deterministic JSON encoding for request digests, not ordinary DTO roundtrips.
/// Object keys sort recursively; duplicate keys are invalid. Array order and numeric
/// lexemes remain significant. Strings normalize escape spelling; whitespace is removed.
/// Keeping number lexemes avoids collisions from converting large opaque JSON integers
/// to f64. Only encoded fragments are retained; no general-purpose JSON tree is exposed.
pub fn canonical_json(raw: &RawJson) -> Result<Vec<u8>, Error> {
    let text = serde_json::to_string(raw).map_err(|_| Encoder::invalid())?;
    let mut encoder = Encoder {
        text: &text,
        pos: 0,
    };
    let out = encoder.value(0)?;
    encoder.ws();
    if encoder.pos != text.len() {
        return Err(Encoder::invalid());
    }
    Ok(out)
}

//! Backend-neutral storage contracts. Only owned DTOs cross the DomainStore boundary.
//! Events remain process-local attachments; storage commits contain durable facts only.
//! Named contracts require an implementation and semantic conformance test in the same task.

pub mod append;
pub mod cursor;
pub mod entity;
pub mod fact;
pub mod lane;
pub mod ops;
pub mod query;
pub mod registry;
pub mod retention;
pub mod snapshot;
pub mod transfer;
pub mod uow;
#[cfg(feature = "conformance")]
pub mod verify;

use async_trait::async_trait;
pub use entity::{Cell, Entity, EntityDesc, Row};
pub use lane::Lane;
pub use query::{AnyOutput, AnyQuery, LocalPayload, Payload, Query};
pub use retention::{Budget, RetentionReport};
use serde::{Deserialize, Serialize};
pub use snapshot::LeaseId;
use std::collections::BTreeSet;
pub use steve_types::{Error, Millis, RawJson, Rev};
pub use transfer::{ExportBatch, ExportCursor, ExportSession, ImportReport};
pub use uow::{CommitError, Committed, UnitOfWork};

/// Logical atomicity boundary, independent of the current physical file layout.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Core,
    Stream,
    Comms,
}

/// Independently negotiated integer version of one domain's storage contract.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Serialize, Deserialize)]
pub struct ContractVersion(pub u32);

/// Backend capabilities; physical schema compatibility must be checked separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub implementation: String,
    pub domains: Vec<DomainOffer>,
}

/// Inclusive version range and feature names (cas, append_order, snapshot_read, export,
/// op:<name>, query:<name>). Export requires every table in the domain to be declared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainOffer {
    pub domain: Domain,
    pub versions: (ContractVersion, ContractVersion),
    pub features: BTreeSet<String>,
    pub isolation: Isolation,
}

/// Isolation guaranteed for work units and snapshot reads.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    Serializable,
    SnapshotWithLocks,
}

/// Domain-level backend port. Mixed-domain requests fail with invalid even if colocated.
/// Unknown entity, query, operation or type names fail with unsupported. Implementations
/// serialize lease queries, interrupt expired leases and release their connection after rollback.
#[async_trait]
pub trait DomainStore: Send + Sync {
    fn hello(&self) -> &Hello;
    async fn query(&self, q: AnyQuery) -> Result<AnyOutput, Error>;
    async fn snapshot(&self, domain: Domain, qs: Vec<AnyQuery>) -> Result<Vec<AnyOutput>, Error>;
    async fn begin_snapshot(&self, domain: Domain, lane: Lane) -> Result<LeaseId, Error>;
    async fn lease_query(&self, lease: LeaseId, q: AnyQuery) -> Result<AnyOutput, Error>;
    async fn lease_end(&self, lease: LeaseId) -> Result<(), Error>;
    async fn commit(&self, uow: UnitOfWork) -> Result<Committed, CommitError>;
    /// Results retain input order. Each unit is independently atomic, including in a merged commit.
    async fn commit_many(&self, uows: Vec<UnitOfWork>) -> Vec<Result<Committed, CommitError>>;
    async fn begin_export(&self, domain: Domain) -> Result<ExportSession, Error>;
    async fn export(
        &self,
        session: &ExportSession,
        cursor: ExportCursor,
    ) -> Result<ExportBatch, Error>;
    async fn import(&self, batch: ExportBatch) -> Result<ImportReport, Error>;
    async fn retention(&self, entity: String, budget: Budget) -> Result<RetentionReport, Error>;
}

/// Construct a transport-neutral error without exposing backend details or private paths.
pub fn error(code: steve_types::Code, detail: impl Into<String>) -> Error {
    Error::new(code, detail).retry(code.default_retry())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        append::{Seq, StreamName},
        cursor::{CursorKey, CursorName, Position},
        entity::*,
        fact::FactRecord,
        ops::comms::digest::*,
        query::*,
        registry::*,
        retention::*,
        transfer::*,
        uow::*,
    };
    use serde::de::DeserializeOwned;
    use std::{collections::BTreeSet, marker::PhantomData};

    fn raw(text: &str) -> RawJson {
        RawJson::from_str(text).unwrap()
    }
    fn wire<T: Serialize + DeserializeOwned>(value: &T) -> T {
        let text = serde_json::to_string(value).unwrap();
        let decoded = serde_json::from_str(&text).unwrap();
        assert_eq!(text, serde_json::to_string(&decoded).unwrap());
        decoded
    }
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestRow {
        id: i64,
        text: String,
    }
    impl ToRow for TestRow {
        fn to_row(&self) -> Result<Row, Error> {
            Ok(Row(vec![
                ("id".into(), Cell::Int(self.id)),
                ("text".into(), Cell::Text(self.text.clone())),
            ]))
        }
    }
    impl FromRow for TestRow {
        fn from_row(row: Row) -> Result<Self, Error> {
            let mut id = None;
            let mut text = None;
            for (key, value) in row.0 {
                match (key.as_str(), value) {
                    ("id", Cell::Int(v)) => id = Some(v),
                    ("text", Cell::Text(v)) => text = Some(v),
                    _ => return Err(error(steve_types::Code::Invalid, "unexpected test field")),
                }
            }
            Ok(Self {
                id: id.ok_or_else(|| error(steve_types::Code::Invalid, "missing id"))?,
                text: text.ok_or_else(|| error(steve_types::Code::Invalid, "missing text"))?,
            })
        }
    }
    entity_desc!(TEST_ENTITY = {
        name: "verify_wire", domain: Domain::Core, key: &["id"],
        fields: &[Field { name: "id", ty: StaticFieldType::Int, nullable: false },
            Field { name: "text", ty: StaticFieldType::Text, nullable: false },
            Field { name: "code", ty: StaticFieldType::Enum(&["ready", "done"]), nullable: true }],
        indexes: &[Index { name: "text", fields: &["text", "id"], unique: true }],
        kind: StaticEntityKind::Mutable, retention: None
    });
    impl StoredEntity for TestRow {
        const ENTITY: &'static Entity = &TEST_ENTITY;
    }
    fn sample_row() -> Row {
        Row(vec![
            ("z".into(), Cell::Int(i64::MIN)),
            (
                "a".into(),
                Cell::Json(raw("{ \"z\": [1, true], \"a\": \"中文😀\" }")),
            ),
        ])
    }
    fn unit() -> UnitOfWork {
        UnitOfWork {
            domain: Domain::Core,
            id: IdempotencyKey {
                issued_at: Millis(0),
                key: "intent😀".into(),
            },
            digest: RequestDigest([0; 32]),
            lane: Lane::Interactive,
            idempotency: Idempotency::Recorded,
            preconditions: vec![Precondition::Absent {
                entity: "verify_wire".into(),
                key: vec![Cell::Int(1)],
            }],
            mutations: vec![Mutation::Insert {
                entity: "verify_wire".into(),
                row: sample_row(),
            }],
            facts: vec![FactRecord {
                fact_id: "fact".into(),
                topic: "core.test".into(),
                key: "中文".into(),
                payload: raw("{\"b\":2,\"a\":1}"),
            }],
        }
    }
    fn scan() -> Scan<TestRow> {
        Scan {
            index: IndexName {
                entity: "verify_wire".into(),
                name: "text".into(),
            },
            range: KeyRange::default(),
            filter: vec![],
            order: Order::Asc,
            limit: 500,
            budget: ScanBudget::default(),
            resume: None,
            marker: PhantomData,
        }
    }

    #[test]
    fn scalar_encoding_is_lossless_and_rejects_non_finite_reals() {
        let cells = vec![
            Cell::Null,
            Cell::Int(i64::MIN),
            Cell::Int(i64::MAX),
            Cell::Int(-2),
            Cell::Millis(i64::MIN),
            Cell::Millis(i64::MAX),
            Cell::Real(f64::MIN),
            Cell::Real(f64::MAX),
            Cell::Real(f64::MIN_POSITIVE),
            Cell::Real(-0.0),
            Cell::Real(f64::from_bits(1)),
            Cell::Text("中文😀".repeat(128 * 1024)),
            Cell::Bytes((0..=255).collect()),
            Cell::Bool(false),
            Cell::Bool(true),
            Cell::Json(raw("{ \"b\": 1, \"a\": [\"😀\", -9] }")),
        ];
        for cell in &cells {
            assert_eq!(*cell, wire(cell));
        }
        let megabyte = Cell::Text("x".repeat(1024 * 1024));
        assert_eq!(megabyte, wire(&megabyte));
        assert_eq!(
            serde_json::to_string(&Cell::Bytes(vec![0, 255])).unwrap(),
            r#"{"bytes":"AP8="}"#
        );
        assert!(serde_json::from_str::<Cell>(r#"{"bytes":"?bad"}"#).is_err());
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(serde_json::to_string(&Cell::Real(v)).is_err());
            assert_eq!(
                encode(&Cell::Real(v)).unwrap_err().code,
                steve_types::Code::Invalid
            );
        }
        for text in [r#"{"real":1e999}"#, r#"{"real":null}"#, r#"{"real":"NaN"}"#] {
            assert!(serde_json::from_str::<Cell>(text).is_err());
        }
        // Ordinary JSON preserves the exact opaque bytes, not only semantic JSON equality.
        let json = "{ \"z\": 1, \"a\": [2, 3] }";
        assert_eq!(serde_json::to_string(&wire(&raw(json))).unwrap(), json);
        assert_eq!(
            wire(&Row(cells
                .iter()
                .enumerate()
                .map(|(i, c)| (i.to_string(), c.clone()))
                .collect()))
            .0
            .len(),
            cells.len()
        );
    }

    #[test]
    fn discovery_and_entity_descriptions_roundtrip() {
        for domain in [Domain::Core, Domain::Stream, Domain::Comms] {
            for isolation in [Isolation::Serializable, Isolation::SnapshotWithLocks] {
                let offer = DomainOffer {
                    domain,
                    versions: (ContractVersion(1), ContractVersion(u32::MAX)),
                    features: [
                        "cas",
                        "append_order",
                        "snapshot_read",
                        "export",
                        "op:中文",
                        "query:😀",
                    ]
                    .map(str::to_owned)
                    .into(),
                    isolation,
                };
                wire(&offer);
                wire(&Hello {
                    implementation: "backend😀".into(),
                    domains: vec![offer],
                });
            }
        }
        let mut desc = TEST_ENTITY.to_wire();
        assert_eq!(
            desc.fields[2].ty,
            FieldType::Enum(vec!["ready".into(), "done".into()])
        );
        wire(&desc);
        for ty in [
            StaticFieldType::Int,
            StaticFieldType::Real,
            StaticFieldType::Text,
            StaticFieldType::Bytes,
            StaticFieldType::Bool,
            StaticFieldType::Millis,
            StaticFieldType::Json,
            StaticFieldType::Enum(&["中文", "😀"]),
        ] {
            desc.fields[0].ty = ty.to_wire();
            wire(&desc.fields[0]);
        }
        for kind in [
            StaticEntityKind::Mutable,
            StaticEntityKind::Log,
            StaticEntityKind::Append {
                stream_key: &["session"],
                seq: "seq",
            },
        ] {
            let entity = Entity {
                kind,
                ..TEST_ENTITY
            };
            wire(&entity.to_wire());
        }
        let typed = TestRow {
            id: i64::MIN,
            text: "中文😀".into(),
        };
        assert_eq!(typed, TestRow::from_row(typed.to_row().unwrap()).unwrap());
    }

    #[test]
    fn query_dtos_and_local_payload_roundtrip() {
        let get = Get::<TestRow> {
            key: Key::new(vec![Cell::Int(i64::MIN)]),
        };
        wire(&get);
        wire(&Exists::<TestRow> {
            key: Key::new(vec![Cell::Int(-1)]),
        });
        let mut query = scan();
        query.range = KeyRange {
            start: KeyBound::Included(vec![Cell::Text("中".into())]),
            end: KeyBound::Excluded(vec![Cell::Text("😀".into())]),
        };
        query.resume = Some(ResumeToken {
            key: vec![Cell::Text("中文".into()), Cell::Int(i64::MAX)],
        });
        let field = "id".to_owned();
        let value = Cell::Int(-9);
        query.filter = vec![
            Pred::Eq {
                field: field.clone(),
                value: value.clone(),
            },
            Pred::Ne {
                field: field.clone(),
                value: value.clone(),
            },
            Pred::Lt {
                field: field.clone(),
                value: value.clone(),
            },
            Pred::Le {
                field: field.clone(),
                value: value.clone(),
            },
            Pred::Gt {
                field: field.clone(),
                value: value.clone(),
            },
            Pred::Ge {
                field: field.clone(),
                value: value.clone(),
            },
            Pred::In {
                field: field.clone(),
                values: vec![value],
            },
            Pred::IsNull {
                field,
                is_null: true,
            },
        ];
        wire(&query);
        query.order = Order::Desc;
        wire(&query);
        wire(&Count::<TestRow> {
            index: query.index.clone(),
            range: query.range.clone(),
            filter: query.filter.clone(),
            budget: ScanBudget(u32::MAX),
            marker: PhantomData,
        });
        wire(&CountResult::Exact(u64::MAX));
        wire(&CountResult::AtLeast(0));
        wire(&Page {
            rows: vec![(
                TestRow {
                    id: -1,
                    text: "😀".into(),
                },
                Rev(u64::MAX),
            )],
            resume: query.resume,
            exhausted_budget: true,
        });
        wire(&Page::<TestRow> {
            rows: vec![],
            resume: Some(ResumeToken {
                key: vec![Cell::Int(5_000)],
            }),
            exhausted_budget: true,
        });
        wire(&CursorGet {
            domain: Domain::Stream,
            name: CursorName("consumer😀".into()),
        });
        let envelope = get.into_local().unwrap();
        match &envelope.body {
            Payload::Local(v) => assert!(v.as_any().is::<Get<TestRow>>()),
            _ => panic!("local fast path lost its concrete type"),
        }
        let decoded: AnyQuery = wire(&envelope);
        assert!(matches!(decoded.body, Payload::Json(_)));
        assert_eq!(
            decoded.decode::<Get<TestRow>>().unwrap().key.values,
            vec![Cell::Int(i64::MIN)]
        );
        let output = AnyOutput {
            name: "get".into(),
            domain: Domain::Core,
            body: Payload::Local(Box::new(Some((
                TestRow {
                    id: 1,
                    text: "中😀".into(),
                },
                Rev(u64::MAX),
            )))),
        };
        assert_eq!(
            wire(&output).decode::<Get<TestRow>>().unwrap().unwrap().1,
            Rev(u64::MAX)
        );
        // Envelopes must preserve opaque payload bytes even when body precedes the name.
        let input = r#"{"body":{ "z": 1, "a": "😀" },"name":"future","domain":"core"}"#;
        let decoded: AnyQuery = serde_json::from_str(input).unwrap();
        assert_eq!(
            serde_json::to_string(&decoded.body).unwrap(),
            r#"{ "z": 1, "a": "😀" }"#
        );
        assert_eq!(
            decoded.decode::<Get<TestRow>>().unwrap_err().code,
            steve_types::Code::Unsupported
        );
        let mut query = scan();
        query.limit = 501;
        assert_eq!(
            query.to_wire().unwrap_err().code,
            steve_types::Code::Invalid
        );
        query.limit = 0;
        assert!(query.to_wire().is_err());
        query.limit = 500;
        query.budget = ScanBudget(0);
        assert!(query.to_wire().is_err());
        query.budget = ScanBudget(5_000);
        query.index.name = "unknown".into();
        assert_eq!(
            query.to_wire().unwrap_err().code,
            steve_types::Code::Unsupported
        );
        assert!(
            Get::<TestRow> {
                key: Key::new(vec![])
            }
            .to_wire()
            .is_err()
        );
        let mut local = scan();
        local.filter = vec![Pred::Eq {
            field: "id".into(),
            value: Cell::Real(f64::NAN),
        }];
        assert_eq!(
            local.into_local().unwrap_err().code,
            steve_types::Code::Invalid
        );
        let invalid_key = Get::<TestRow> {
            key: Key::new(vec![Cell::Real(f64::INFINITY)]),
        };
        assert_eq!(
            invalid_key.into_local().unwrap_err().code,
            steve_types::Code::Invalid
        );
    }

    #[test]
    fn work_unit_variants_and_errors_roundtrip() {
        let mut u = unit();
        assert_eq!(u, wire(&u));
        u.idempotency = Idempotency::Natural {
            entity: "verify_wire".into(),
            index: "text".into(),
        };
        u.preconditions.extend([
            Precondition::Rev {
                entity: "verify_wire".into(),
                key: vec![Cell::Int(-1)],
                rev: Rev(u64::MAX),
            },
            Precondition::Present {
                entity: "verify_wire".into(),
                key: vec![Cell::Int(-1)],
            },
            Precondition::Cursor {
                name: CursorName("水位😀".into()),
                at: None,
            },
            Precondition::Cursor {
                name: CursorName("水位😀".into()),
                at: Some(Position(CursorKey::Int(i64::MIN))),
            },
        ]);
        let op = AnyOp {
            name: "core.future".into(),
            domain: Domain::Core,
            body: raw("{ \"x\": [1], \"y\": 2 }"),
        };
        u.mutations.extend([
            Mutation::Upsert {
                entity: "verify_wire".into(),
                row: sample_row(),
            },
            Mutation::Update {
                entity: "verify_wire".into(),
                key: vec![Cell::Int(1)],
                set: sample_row(),
            },
            Mutation::Delete {
                entity: "verify_wire".into(),
                key: vec![Cell::Int(2)],
            },
            Mutation::DeleteWhere {
                entity: "verify_wire".into(),
                index: "text".into(),
                range: KeyRange::default(),
                filter: vec![],
                limit: 500,
            },
            Mutation::Append {
                stream: StreamName("流😀".into()),
                records: vec![sample_row()],
            },
            Mutation::AdvanceCursor {
                name: CursorName("水位".into()),
                to: Position(CursorKey::Composite(vec![
                    CursorKey::Text("epoch".into()),
                    CursorKey::Int(i64::MAX),
                ])),
            },
            Mutation::Op(op),
        ]);
        assert_eq!(u, wire(&u));
        assert!(!serde_json::to_string(&u).unwrap().contains("events"));
        wire(&Committed {
            rev: Rev(u64::MAX),
            seqs: vec![(
                StreamName("stream".into()),
                vec![Seq(i64::MIN), Seq(i64::MAX)],
            )],
            replayed: true,
        });
        let failures = vec![FailedPrecondition {
            condition: u.preconditions[0].clone(),
            actual_rev: Some(Rev(9)),
            actual_position: None,
        }];
        for (failure, code, retry) in [
            (
                CommitError::Conflict(failures),
                steve_types::Code::Conflict,
                steve_types::Retry::Never,
            ),
            (
                CommitError::IdempotencyMismatch,
                steve_types::Code::Conflict,
                steve_types::Retry::Never,
            ),
            (
                CommitError::Expired,
                steve_types::Code::Stale,
                steve_types::Retry::Never,
            ),
            (
                CommitError::Unique {
                    entity: "entity".into(),
                    index: "index".into(),
                },
                steve_types::Code::Conflict,
                steve_types::Retry::Never,
            ),
            (
                CommitError::Retryable {
                    after_ms: Some(u64::MAX),
                    detail: "busy".into(),
                },
                steve_types::Code::Busy,
                steve_types::Retry::Later {
                    after_ms: Some(u64::MAX),
                },
            ),
            (
                CommitError::UowInsideGrant,
                steve_types::Code::Invalid,
                steve_types::Retry::Never,
            ),
            (
                CommitError::Fatal(error(steve_types::Code::Incompatible, "incompatible")),
                steve_types::Code::Incompatible,
                steve_types::Retry::Never,
            ),
        ] {
            assert_eq!(failure, wire(&failure));
            let e: Error = failure.into();
            assert_eq!(e.code, code);
            assert_eq!(e.retry, retry);
        }
    }

    #[test]
    fn request_digest_canonicalizes_fields_and_ignores_attempt_metadata() {
        let a = unit();
        let mut b = a.clone();
        if let Mutation::Insert { row, .. } = &mut b.mutations[0] {
            row.0.reverse();
            row.0[0].1 = Cell::Json(raw("{\"a\":\"\\u4e2d文😀\",\"z\":[1,true]}"));
        }
        b.facts[0].payload = raw("{ \"a\":1, \"b\":2 }");
        for lane in [
            Lane::Interactive,
            Lane::Admin,
            Lane::Ingest,
            Lane::Background,
        ] {
            let mut attempt = a.clone();
            attempt.lane = lane;
            assert_eq!(
                a.request_digest().unwrap(),
                attempt.request_digest().unwrap()
            );
        }
        b.lane = Lane::Background;
        b.id.key = "retry".into();
        b.id.issued_at = Millis(i64::MAX);
        b.digest = RequestDigest([255; 32]);
        assert_eq!(
            a.canonical_request().unwrap(),
            b.canonical_request().unwrap()
        );
        assert_eq!(a.request_digest().unwrap(), b.request_digest().unwrap());
        b.facts[0].key.push('!');
        assert_ne!(a.request_digest().unwrap(), b.request_digest().unwrap());
        b = a.clone();
        b.domain = Domain::Comms;
        assert_ne!(a.request_digest().unwrap(), b.request_digest().unwrap());
        b = a.clone();
        b.digest = b.request_digest().unwrap();
        assert!(b.verify_digest().is_ok());
        b.mutations.clear();
        assert_eq!(
            b.verify_digest().unwrap_err().code,
            steve_types::Code::Invalid
        );
        b = a.clone();
        if let Mutation::Insert { row, .. } = &mut b.mutations[0] {
            row.0.push(row.0[0].clone());
        }
        assert_eq!(
            b.request_digest().unwrap_err().code,
            steve_types::Code::Invalid
        );
        assert!(canonical_json(&raw(r#"{"x":1,"x":2}"#)).is_err());
        // Opaque integers beyond u64 must never collide through floating-point conversion.
        assert_ne!(
            canonical_json(&raw("18446744073709551616")).unwrap(),
            canonical_json(&raw("18446744073709551617")).unwrap()
        );
        assert_eq!(
            canonical_json(&raw(r#" {"b":[null,false,-1e300],"a":"a\\\"b"} "#)).unwrap(),
            br#"{"a":"a\\\"b","b":[null,false,-1e300]}"#
        );
        use sha2::{Digest, Sha256};
        assert_eq!(
            a.request_digest().unwrap().0.as_slice(),
            Sha256::digest(a.canonical_request().unwrap()).as_slice()
        );
        assert!(a.validate_age(Millis(30 * 24 * 60 * 60 * 1_000)).is_ok());
        assert_eq!(
            a.validate_age(Millis(30 * 24 * 60 * 60 * 1_000 + 1)),
            Err(CommitError::Expired)
        );
        assert!(a.validate_age(Millis(-300_000)).is_ok());
        assert!(matches!(
            a.validate_age(Millis(-300_001)),
            Err(CommitError::Fatal(_))
        ));
        assert_eq!(a.validate_age(Millis(i64::MAX)), Err(CommitError::Expired));
        assert!(matches!(
            a.validate_age(Millis(i64::MIN)),
            Err(CommitError::Fatal(_))
        ));
    }

    #[test]
    fn cursor_keys_follow_sql_typed_ordering() {
        assert!(CursorKey::Int(2) < CursorKey::Int(10));
        assert!(CursorKey::Int(i64::MIN) < CursorKey::Int(i64::MAX));
        let mut text = ["😀", "中", "a", "Z", "é", "a\0", ""].map(|s| CursorKey::Text(s.into()));
        text.sort();
        let mut expected = ["😀", "中", "a", "Z", "é", "a\0", ""];
        expected.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(
            text.to_vec(),
            expected.map(|s| CursorKey::Text(s.into())).to_vec()
        );
        assert!(
            CursorKey::Composite(vec![CursorKey::Int(2), CursorKey::Text("z".into())])
                < CursorKey::Composite(vec![CursorKey::Int(10), CursorKey::Text("a".into())])
        );
        assert!(
            CursorKey::Composite(vec![CursorKey::Int(2)])
                < CursorKey::Composite(vec![CursorKey::Int(2), CursorKey::Int(0)])
        );
        for key in [
            CursorKey::Int(i64::MIN),
            CursorKey::Text("中文😀".into()),
            CursorKey::Composite(text.to_vec()),
        ] {
            wire(&Position(key));
        }
        wire(&LeaseId {
            id: "lease😀".into(),
            expires_at: Millis(i64::MAX),
        });
    }

    #[test]
    fn retention_and_transfer_dtos_roundtrip() {
        let mut policy = RetentionPolicy {
            entity: "verify_wire".into(),
            select: Select::MaxAge {
                field: "created".into(),
                keep_ms: u64::MAX,
            },
            keep_from: KeepFrom::Fixed { millis: u64::MAX },
            before: Some(OpName("comms.unlink".into())),
            action: Action::Delete,
            protect: Some(OpName("comms.in_use".into())),
            budget: Budget::default(),
        };
        wire(&policy);
        policy.keep_from = KeepFrom::Setting {
            key: "keepRaw".into(),
            per: SettingScope::Workspace,
            default_ms: 30,
        };
        for select in [
            Select::Terminal {
                state: "state".into(),
                done: vec![Cell::Text("done".into())],
                since: "updated".into(),
                keep_ms: 10,
            },
            Select::MaxRows {
                group: vec!["workspace".into()],
                keep: u64::MAX,
                order: vec![("created".into(), Order::Desc)],
            },
            Select::Bytes {
                group: vec!["workspace".into()],
                max: u64::MAX,
            },
        ] {
            policy.select = select;
            wire(&policy);
        }
        for action in [
            Action::Purge {
                fields: vec!["body".into()],
            },
            Action::Op(AnyOp {
                name: "comms.purge".into(),
                domain: Domain::Comms,
                body: raw("{ \"emoji\": \"😀\" }"),
            }),
        ] {
            policy.action = action;
            wire(&policy);
        }
        wire(&RetentionReport {
            entity: "verify_wire".into(),
            affected: u64::MAX,
            slices: u32::MAX,
            max_slice_ms: u32::MAX,
            overruns: u32::MAX,
            more: true,
        });
        let mut entity = TEST_ENTITY.to_wire();
        entity.retention = Some(policy);
        let session = ExportSession {
            id: "export😀".into(),
            domain: Domain::Core,
            version: ContractVersion(1),
            layout: u32::MAX,
            schema: u32::MAX,
            rev: Rev(u64::MAX),
        };
        wire(&session);
        wire(&ExportCursor::default());
        let batch = ExportBatch {
            session,
            id: "batch".into(),
            entity,
            rows: vec![(sample_row(), Rev(u64::MAX))],
            state: raw("{ \"cursors\": [] }"),
            next: Some(ExportCursor {
                entity: "verify_wire".into(),
                after: Some(vec![Cell::Int(i64::MAX)]),
            }),
            digest: RequestDigest([255; 32]),
        };
        assert_eq!(batch, wire(&batch));
        wire(&ImportReport {
            batch_id: "batch😀".into(),
            imported: u64::MAX,
            replayed: true,
        });
    }

    #[test]
    fn registry_matches_implemented_operations_within_domain_limits() {
        let mut names = BTreeSet::new();
        for entry in ENTRIES {
            assert!(names.insert(entry.name));
            assert!(!entry.semantics.is_empty());
            assert!(!entry.parameter_type.is_empty());
            let prefix = match entry.domain {
                Domain::Core => "core.",
                Domain::Stream => "stream.",
                Domain::Comms => "comms.",
            };
            assert!(entry.name.starts_with(prefix));
            assert_eq!(
                lookup(entry.domain, entry.kind, entry.name).unwrap().name,
                entry.name
            );
        }
        let expected: BTreeSet<_> = [<SyncPendingMembership as Op>::NAME].into();
        assert_eq!(names, expected);
        let entry = lookup(
            <SyncPendingMembership as Op>::DOMAIN,
            EntryKind::Operation,
            <SyncPendingMembership as Op>::NAME,
        )
        .unwrap();
        assert_eq!(entry.parameter_type, "SyncPendingMembership");
        for domain in [Domain::Core, Domain::Stream, Domain::Comms] {
            assert!(ENTRIES.iter().filter(|e| e.domain == domain).count() <= DOMAIN_LIMIT);
        }
        assert_eq!(
            lookup(Domain::Core, EntryKind::Query, "unknown")
                .unwrap_err()
                .code,
            steve_types::Code::Unsupported
        );
        for source in [Source::ChanMessage, Source::MailMessage] {
            let enqueue = SyncPendingMembership::enqueue_digestible(
                source,
                "message😀".into(),
                "workspace".into(),
            )
            .unwrap();
            assert_eq!(enqueue, wire(&enqueue));
            assert_eq!(
                enqueue,
                enqueue
                    .to_wire()
                    .unwrap()
                    .decode::<SyncPendingMembership>()
                    .unwrap()
            );
            wire(&SyncPendingMembership::sync_pending_conversation(
                source,
                "id".into(),
                "workspace".into(),
            ));
        }
        let receipt =
            SyncPendingMembership::dequeue_on_receipt("space:中文", "msg😀", "workspace".into())
                .unwrap();
        wire(&receipt);
        assert_eq!(
            receipt.receipt_identity().unwrap(),
            ("space:中文".into(), "msg😀".into())
        );
        assert!(receipt.to_wire().is_ok());
        let mut malformed = receipt.clone();
        malformed.id = "ambiguous/id".into();
        assert_eq!(
            malformed.to_wire().unwrap_err().code,
            steve_types::Code::Invalid
        );
        assert!(
            SyncPendingMembership::enqueue_digestible(
                Source::Receipt,
                "id".into(),
                "workspace".into()
            )
            .is_err()
        );
        let op = AnyOp {
            name: "future".into(),
            domain: Domain::Comms,
            body: raw("{}"),
        };
        assert_eq!(
            op.decode::<SyncPendingMembership>().unwrap_err().code,
            steve_types::Code::Unsupported
        );
    }

    #[test]
    fn lane_context_restores_on_unwind_and_does_not_leak_between_threads() {
        assert_eq!(lane::thread_lane(), Lane::Interactive);
        lane::with_lane(Lane::Admin, || {
            assert_eq!(lane::thread_lane(), Lane::Admin);
            let _ = std::panic::catch_unwind(|| {
                lane::with_lane(Lane::Background, || panic!("unwind test"))
            });
            assert_eq!(lane::thread_lane(), Lane::Admin);
            assert_eq!(
                std::thread::spawn(lane::thread_lane).join().unwrap(),
                Lane::Interactive
            );
        });
        assert_eq!(lane::thread_lane(), Lane::Interactive);
        fn source() -> Lane {
            lane::thread_lane()
        }
        let previous = lane::replace_source(source);
        lane::with_lane(Lane::Ingest, || assert_eq!(lane::current(), Lane::Ingest));
        lane::replace_source(previous);
    }

    fn violations(source: &str) -> Vec<&'static str> {
        let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        let mut found = vec![];
        for forbidden in ["serde_json::Value", "serde_json::Map", "json!", "RawValue"] {
            if compact.contains(forbidden) {
                found.push(forbidden);
            }
        }
        for (i, _) in compact.match_indices("serde_json::{") {
            let tail = &compact[i + 13..];
            let mut depth = 1;
            let end = tail
                .char_indices()
                .find_map(|(i, c)| {
                    match c {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    (depth == 0).then_some(i)
                })
                .unwrap_or(tail.len());
            if tail[..end]
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|t| t.starts_with("Value") || t.starts_with("Map"))
            {
                found.push("braced JSON tree import");
            }
        }
        found
    }
    #[test]
    fn contract_sources_keep_json_opaque() {
        fn scan(dir: &std::path::Path) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    // Tests are inline because this card owns src only; scan all production
                    // code, and leave the guard's own forbidden fixtures out of its input.
                    let production = source.split("#[cfg(test)]").next().unwrap();
                    assert!(
                        violations(production).is_empty(),
                        "{}: {:?}",
                        path.display(),
                        violations(production)
                    );
                }
            }
        }
        scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        for source in [
            "serde_json::Value",
            "serde_json :: Map",
            "serde_json::json!({})",
            "json ! ({})",
            "use serde_json::{Deserialize, Value as Tree};",
            "use serde_json::{value::{RawValue}, Map as Object};",
        ] {
            assert!(!violations(source).is_empty());
        }
        assert!(violations("use serde_json::{from_str, to_string};").is_empty());
    }
}

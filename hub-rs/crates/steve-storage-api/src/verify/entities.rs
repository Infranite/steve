//! Portable fixture catalogue. Backends register these descriptors in an isolated namespace.
use crate::{entity::*, retention::*, *};
use serde::{Deserialize, Serialize};

const FIELDS: &[Field] = &[
    Field {
        name: "id",
        ty: StaticFieldType::Text,
        nullable: false,
    },
    Field {
        name: "value",
        ty: StaticFieldType::Int,
        nullable: false,
    },
    Field {
        name: "label",
        ty: StaticFieldType::Text,
        nullable: true,
    },
    Field {
        name: "at",
        ty: StaticFieldType::Millis,
        nullable: true,
    },
    Field {
        name: "data",
        ty: StaticFieldType::Json,
        nullable: true,
    },
    Field {
        name: "bytes",
        ty: StaticFieldType::Bytes,
        nullable: true,
    },
    Field {
        name: "real",
        ty: StaticFieldType::Real,
        nullable: true,
    },
    Field {
        name: "flag",
        ty: StaticFieldType::Bool,
        nullable: true,
    },
];
const INDEXES: &[Index] = &[
    Index {
        name: "label",
        fields: &["label"],
        unique: true,
    },
    Index {
        name: "value",
        fields: &["value"],
        unique: false,
    },
];
crate::entity_desc!(pub CORE = { name: "verify_core", domain: Domain::Core, key: &["id"], fields: FIELDS, indexes: INDEXES, kind: StaticEntityKind::Mutable, retention: None });
crate::entity_desc!(pub COMMS = { name: "verify_comms", domain: Domain::Comms, key: &["id"], fields: FIELDS, indexes: INDEXES, kind: StaticEntityKind::Mutable, retention: None });
crate::entity_desc!(pub STREAM = { name: "verify_stream", domain: Domain::Stream, key: &["seq"],
    fields: &[Field { name: "seq", ty: StaticFieldType::Int, nullable: false }, Field { name: "id", ty: StaticFieldType::Text, nullable: false }, Field { name: "value", ty: StaticFieldType::Int, nullable: false }],
    indexes: &[], kind: StaticEntityKind::Append { stream_key: &[], seq: "seq" }, retention: None });
crate::entity_desc!(pub CORE_STREAM = { name: "verify_core_stream", domain: Domain::Core, key: &["seq"], fields: STREAM.fields, indexes: &[], kind: StaticEntityKind::Append { stream_key: &[], seq: "seq" }, retention: None });
crate::entity_desc!(pub LOG = { name: "verify_log", domain: Domain::Core, key: &["id"], fields: FIELDS, indexes: &[], kind: StaticEntityKind::Log, retention: None });

macro_rules! record {
    ($name:ident, $entity:ident) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Row);
        impl ToRow for $name {
            fn to_row(&self) -> Result<Row, Error> {
                Ok(self.0.clone())
            }
        }
        impl FromRow for $name {
            fn from_row(row: Row) -> Result<Self, Error> {
                Ok(Self(row))
            }
        }
        impl StoredEntity for $name {
            const ENTITY: &'static Entity = &$entity;
        }
    };
}
record!(CoreRecord, CORE);
record!(CommsRecord, COMMS);
record!(StreamRecord, STREAM);
record!(LogRecord, LOG);

/// All fixture descriptions, including a deterministic age policy for the retained log.
pub fn descriptors() -> Vec<EntityDesc> {
    let mut log = LOG.to_wire();
    log.retention = Some(RetentionPolicy {
        entity: LOG.name.into(),
        select: Select::MaxAge {
            field: "at".into(),
            keep_ms: 1000,
        },
        keep_from: KeepFrom::Fixed { millis: 1000 },
        before: None,
        action: Action::Delete,
        protect: None,
        budget: Budget::default(),
    });
    vec![
        CORE.to_wire(),
        COMMS.to_wire(),
        STREAM.to_wire(),
        CORE_STREAM.to_wire(),
        log,
    ]
}
/// Fully populated mutable fixture row; null secondary keys may repeat.
pub fn row(id: impl Into<String>, value: i64) -> Row {
    Row(vec![
        ("id".into(), Cell::Text(id.into())),
        ("value".into(), Cell::Int(value)),
        ("label".into(), Cell::Null),
        ("at".into(), Cell::Millis(0)),
        ("data".into(), Cell::Null),
        ("bytes".into(), Cell::Null),
        ("real".into(), Cell::Null),
        ("flag".into(), Cell::Null),
    ])
}
/// Field access without coupling a backend to the fixture's Rust type.
pub fn cell<'a>(row: &'a Row, field: &str) -> Option<&'a Cell> {
    row.0
        .iter()
        .find(|(name, _)| name == field)
        .map(|(_, value)| value)
}
/// Replace an existing field or append it, retaining deterministic order.
pub fn set(row: &mut Row, field: &str, value: Cell) {
    if let Some((_, old)) = row.0.iter_mut().find(|(name, _)| name == field) {
        *old = value;
    } else {
        row.0.push((field.into(), value));
    }
}

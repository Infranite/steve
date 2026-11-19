//! Contract verification entities; isolated from the legacy production tables.
use steve_storage_api::{entity::*, retention::*, *};
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
steve_storage_api::entity_desc!(pub CORE = { name: "verify_core", domain: Domain::Core, key: &["id"], fields: FIELDS, indexes: INDEXES, kind: StaticEntityKind::Mutable, retention: None });
steve_storage_api::entity_desc!(pub COMMS = { name: "verify_comms", domain: Domain::Comms, key: &["id"], fields: FIELDS, indexes: INDEXES, kind: StaticEntityKind::Mutable, retention: None });
steve_storage_api::entity_desc!(pub STREAM = { name: "verify_stream", domain: Domain::Stream, key: &["seq"],
    fields: &[Field { name: "seq", ty: StaticFieldType::Int, nullable: false }, Field { name: "id", ty: StaticFieldType::Text, nullable: false }, Field { name: "value", ty: StaticFieldType::Int, nullable: false }],
    indexes: &[], kind: StaticEntityKind::Append { stream_key: &[], seq: "seq" }, retention: None });
steve_storage_api::entity_desc!(pub CORE_STREAM = { name: "verify_core_stream", domain: Domain::Core, key: &["seq"], fields: STREAM.fields, indexes: &[], kind: StaticEntityKind::Append { stream_key: &[], seq: "seq" }, retention: None });
steve_storage_api::entity_desc!(pub LOG = { name: "verify_log", domain: Domain::Core, key: &["id"], fields: FIELDS, indexes: &[], kind: StaticEntityKind::Log, retention: None });

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

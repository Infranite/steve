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
    let mut entities = vec![
        CORE.to_wire(),
        COMMS.to_wire(),
        STREAM.to_wire(),
        CORE_STREAM.to_wire(),
        log,
    ];
    entities.extend(digest_descriptors());
    entities
}
const TEXT: StaticFieldType = StaticFieldType::Text;
const INT: StaticFieldType = StaticFieldType::Int;
macro_rules! fields {($($name:literal : $ty:expr),* $(,)?)=>{&[$(Field{name:$name,ty:$ty,nullable:false}),*]};}
steve_storage_api::entity_desc!(pub SOURCE={name:"verify_digest_source",domain:Domain::Comms,key:&["source","id"],fields:fields!["source":TEXT,"id":TEXT,"workspace_id":TEXT,"conv":TEXT,"status":TEXT,"at":INT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
steve_storage_api::entity_desc!(pub RECEIPT={name:"verify_digest_receipt",domain:Domain::Comms,key:&["workspace_id","conv","message_id"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"message_id":TEXT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
steve_storage_api::entity_desc!(pub PENDING={name:"verify_digest_pending",domain:Domain::Comms,key:&["workspace_id","conv","message_id"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"message_id":TEXT,"at":INT,"position":INT],indexes:&[],kind:StaticEntityKind::Log,retention:None});
steve_storage_api::entity_desc!(pub CONVS={name:"verify_digest_convs",domain:Domain::Comms,key:&["workspace_id","conv"],fields:fields!["workspace_id":TEXT,"conv":TEXT,"messages":INT,"oldest":INT,"newest":INT],indexes:&[],kind:StaticEntityKind::Mutable,retention:None});
pub fn digest_descriptors() -> Vec<EntityDesc> {
    vec![
        SOURCE.to_wire(),
        RECEIPT.to_wire(),
        PENDING.to_wire(),
        CONVS.to_wire(),
    ]
}

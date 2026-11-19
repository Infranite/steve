//! Static entity metadata and owned wire descriptions; no backend-specific SQL types.
use crate::{Domain, Error, RawJson, retention::RetentionPolicy};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde::{Deserialize, Serialize};

/// Owned field type. Enum codes are stored as text; unknown codes are incompatible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Int,
    Real,
    Text,
    Bytes,
    Bool,
    Millis,
    Json,
    Enum(Vec<String>),
}

/// Const-friendly counterpart of FieldType, including enum code names.
#[derive(Clone, Copy, Debug)]
pub enum StaticFieldType {
    Int,
    Real,
    Text,
    Bytes,
    Bool,
    Millis,
    Json,
    Enum(&'static [&'static str]),
}
impl StaticFieldType {
    /// Allocate an owned field type for transport.
    pub fn to_wire(self) -> FieldType {
        match self {
            Self::Int => FieldType::Int,
            Self::Real => FieldType::Real,
            Self::Text => FieldType::Text,
            Self::Bytes => FieldType::Bytes,
            Self::Bool => FieldType::Bool,
            Self::Millis => FieldType::Millis,
            Self::Json => FieldType::Json,
            Self::Enum(codes) => FieldType::Enum(codes.iter().map(|s| (*s).into()).collect()),
        }
    }
}

/// One static field, including nullability; its name is stable across backends.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub name: &'static str,
    pub ty: StaticFieldType,
    pub nullable: bool,
}
/// Owned field metadata used in schema discovery and export.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDesc {
    pub name: String,
    pub ty: FieldType,
    pub nullable: bool,
}

/// Static index definition. Indexed text always uses bytewise C collation.
#[derive(Clone, Copy, Debug)]
pub struct Index {
    pub name: &'static str,
    pub fields: &'static [&'static str],
    pub unique: bool,
}
/// Owned index metadata; continuation keys append primary-key fields to break ties.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDesc {
    pub name: String,
    pub fields: Vec<String>,
    pub unique: bool,
}

/// Const-friendly entity storage shape; append sequence allocation belongs to the backend.
#[derive(Clone, Copy, Debug)]
pub enum StaticEntityKind {
    Mutable,
    Append {
        stream_key: &'static [&'static str],
        seq: &'static str,
    },
    Log,
}
/// Owned entity shape. Mutable rows carry a commit Rev, not a user-editable column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Mutable,
    Append {
        stream_key: Vec<String>,
        seq: String,
    },
    Log,
}

/// Process-local description; domain code defines these alongside its row mappings.
#[derive(Debug)]
pub struct Entity {
    pub name: &'static str,
    pub domain: Domain,
    pub key: &'static [&'static str],
    pub fields: &'static [Field],
    pub indexes: &'static [Index],
    pub kind: StaticEntityKind,
    pub retention: Option<&'static RetentionPolicy>,
}
/// Owned equivalent of Entity; safe to send across a process boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityDesc {
    pub name: String,
    pub domain: Domain,
    pub key: Vec<String>,
    pub fields: Vec<FieldDesc>,
    pub indexes: Vec<IndexDesc>,
    pub kind: EntityKind,
    pub retention: Option<RetentionPolicy>,
}
impl Entity {
    /// Convert every static reference, including nested index/enum names, to owned data.
    pub fn to_wire(&self) -> EntityDesc {
        EntityDesc {
            name: self.name.into(),
            domain: self.domain,
            key: self.key.iter().map(|s| (*s).into()).collect(),
            fields: self
                .fields
                .iter()
                .map(|f| FieldDesc {
                    name: f.name.into(),
                    ty: f.ty.to_wire(),
                    nullable: f.nullable,
                })
                .collect(),
            indexes: self
                .indexes
                .iter()
                .map(|i| IndexDesc {
                    name: i.name.into(),
                    fields: i.fields.iter().map(|s| (*s).into()).collect(),
                    unique: i.unique,
                })
                .collect(),
            kind: match self.kind {
                StaticEntityKind::Mutable => EntityKind::Mutable,
                StaticEntityKind::Log => EntityKind::Log,
                StaticEntityKind::Append { stream_key, seq } => EntityKind::Append {
                    stream_key: stream_key.iter().map(|s| (*s).into()).collect(),
                    seq: seq.into(),
                },
            },
            retention: self.retention.cloned(),
        }
    }
}

/// Declare a static entity with ordinary Rust expressions, without a procedural macro crate.
/// ```
/// use steve_storage_api::{entity_desc, Domain, entity::StaticEntityKind};
/// entity_desc!(pub TEST = { name: "test", domain: Domain::Core, key: &["id"],
///     fields: &[], indexes: &[], kind: StaticEntityKind::Mutable, retention: None });
/// assert_eq!(TEST.to_wire().name, "test");
/// ```
#[macro_export]
macro_rules! entity_desc {
    ($vis:vis $id:ident = { $($field:ident : $value:expr),* $(,)? }) => {
        $vis static $id: $crate::entity::Entity = $crate::entity::Entity { $($field: $value),* };
    };
}

/// Scalar cell. Real rejects non-finite values in both directions; Bytes uses standard
/// padded base64; Json preserves its original bytes, including object order and whitespace.
/// External enum tagging avoids Serde's buffered-content path, which loses opaque JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cell {
    Null,
    Int(i64),
    Real(#[serde(with = "finite")] f64),
    Text(String),
    Bytes(#[serde(with = "bytes")] Vec<u8>),
    Bool(bool),
    Millis(i64),
    Json(RawJson),
}
mod finite {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if !v.is_finite() {
            return Err(serde::ser::Error::custom("Real must be finite (invalid)"));
        }
        s.serialize_f64(*v)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        let v = f64::deserialize(d)?;
        if !v.is_finite() {
            return Err(serde::de::Error::custom("Real must be finite (invalid)"));
        }
        Ok(v)
    }
}
mod bytes {
    use super::*;
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64_STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        BASE64_STANDARD
            .decode(String::deserialize(d)?)
            .map_err(serde::de::Error::custom)
    }
}

/// Ordered fields. Duplicate names are invalid. Canonical request encoding sorts names,
/// but ordinary serialization deliberately retains row order for bytewise roundtrips.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row(pub Vec<(String, Cell)>);

/// Domain-owned mapping from a typed row to contract cells.
pub trait ToRow {
    fn to_row(&self) -> Result<Row, Error>;
}
/// Domain-owned mapping, rejecting unknown enum codes with incompatible.
pub trait FromRow: Sized {
    fn from_row(row: Row) -> Result<Self, Error>;
}
/// Associates a typed row with its static descriptor for the generic queries.
pub trait StoredEntity:
    ToRow + FromRow + Serialize + serde::de::DeserializeOwned + Send + 'static
{
    const ENTITY: &'static Entity;
}

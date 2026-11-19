//! Typed consumer positions; compare only keys of the same SQL index shape.
use serde::{Deserialize, Serialize};

/// SQL ordering: signed numeric Int, UTF-8 bytewise (C collation) Text, then lexicographic
/// Composite. Derived Ord supplies exactly these rules within each variant. Cross-variant
/// ordering is only deterministic Rust ordering; backends must reject mixed index shapes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorKey {
    Int(i64),
    Text(String),
    Composite(Vec<CursorKey>),
}

/// Monotone position in one consumer's side-effect domain.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position(pub CursorKey);

/// Domain-local consumer name. Advancement and side effects share a unit of work.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CursorName(pub String);

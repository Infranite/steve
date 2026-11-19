//! Durable facts are written atomically with the unit's mutations, never process events.
use serde::{Deserialize, Serialize};
use steve_types::RawJson;

/// Caller supplies a stable fact_id; consumer receipts live as long as the corresponding fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactRecord {
    pub fact_id: String,
    pub topic: String,
    pub key: String,
    pub payload: RawJson,
}

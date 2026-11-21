//! Single-domain read snapshots. Leases have a hard two-second deadline.
use serde::{Deserialize, Serialize};
use steve_types::Millis;

/// Opaque remote-safe lease identity and absolute expiry; stale IDs must never be reused.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LeaseId {
    pub id: String,
    pub expires_at: Millis,
}

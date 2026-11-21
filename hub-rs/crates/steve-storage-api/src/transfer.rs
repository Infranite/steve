//! Deterministic backend transfer, separate from native backups. Export holds a fixed
//! snapshot for the session; import batches are idempotent and publish only after validation.
use crate::{ContractVersion, Domain, EntityDesc, RawJson, Rev, entity::Row, uow::RequestDigest};
use serde::{Deserialize, Serialize};

/// Fixed snapshot identity and compatibility metadata; exports use the Background lane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSession {
    pub id: String,
    pub domain: Domain,
    pub version: ContractVersion,
    pub layout: u32,
    pub schema: u32,
    pub rev: Rev,
}
/// Deterministic entity/key continuation; None starts that entity at its first key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportCursor {
    pub entity: String,
    pub after: Option<Vec<crate::entity::Cell>>,
}
/// One dependency-ordered, primary-key-ordered batch. digest authenticates canonical
/// batch contents (excluding digest itself); id identifies retries. state carries opaque,
/// versioned backend-neutral metadata (sequences/cursors/receipts) needed for full fidelity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportBatch {
    pub session: ExportSession,
    pub id: String,
    pub entity: EntityDesc,
    pub rows: Vec<(Row, Rev)>,
    pub state: RawJson,
    pub next: Option<ExportCursor>,
    pub digest: RequestDigest,
}
/// Import progress; replayed batches must not reapply effects or emit events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub batch_id: String,
    pub imported: u64,
    pub replayed: bool,
}

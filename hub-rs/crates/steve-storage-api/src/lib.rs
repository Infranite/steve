//! Backend-neutral storage contracts. Only owned DTOs cross the DomainStore boundary.
//! Events remain process-local attachments; storage commits contain durable facts only.

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
    Error {
        code,
        retry: code.default_retry(),
        reason: None,
        params: Default::default(),
        safe_detail: detail.into(),
    }
}

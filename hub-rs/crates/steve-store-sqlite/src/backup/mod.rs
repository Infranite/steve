//! The single backup implementation: a pinned SQLite snapshot and a durable plan manifest.
pub mod manifest;
pub mod offline;
pub mod plan;
pub use manifest::{Artifact, Files, Manifest, Usage};
pub use offline::{CancelFlag, Progress};
pub use plan::{
    BackupPlan, FileSet, Keep, PlanError, Pruned, RULES, Reason, RetentionStatus, Rule,
    TOTAL_BYTES, backup, list, prune, prune_at, retain_at, retention_status, usage,
};

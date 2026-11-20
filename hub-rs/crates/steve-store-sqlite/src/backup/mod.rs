//! The single backup implementation: a pinned SQLite snapshot and a durable plan manifest.
pub mod manifest;
pub mod offline;
pub mod plan;
pub use manifest::{Artifact, Files, Manifest, SkippedDir, Usage};
pub use offline::{CancelFlag, Progress};
pub use plan::{
    BackupPlan, DEFAULT_DAILY_KEEP_DAYS, FileArchive, FileSet, Limits, MAX_DAILY_KEEP_DAYS,
    PlanError, Pruned, RULES, Reason, RetentionStatus, Rule, TOTAL_BYTES, backup, list, retain_at,
    retention_status, usage,
};

//! Portable manifest data; artifact names are relative to the published backup directory.
use super::plan::Reason;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub quick_check: Option<String>,
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SkippedDir {
    pub path: PathBuf,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Files {
    pub consistency: String,
    pub complete: bool,
    pub artifacts: Vec<Artifact>,
    pub error: Option<String>,
    #[serde(default)]
    pub skipped_dirs: Vec<SkippedDir>,
    #[serde(default)]
    pub skipped_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub id: String,
    pub reason: Reason,
    pub created_at: String,
    pub created_ms: u64,
    /// Monotonic publication order; wall-clock steps cannot retire a just-published copy.
    #[serde(default)]
    pub published_order: u64,
    pub source: PathBuf,
    pub directory: PathBuf,
    pub schema_version: Option<i64>,
    pub database: Artifact,
    pub files: Files,
    /// A verified database and durable manifest were published. Files may be incomplete.
    pub completed: bool,
}
impl Manifest {
    pub fn database_path(&self) -> PathBuf {
        self.directory.join(&self.database.name)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub count: u64,
    pub bytes: u64,
    pub over_limit: bool,
    pub latest_over_limit: bool,
}

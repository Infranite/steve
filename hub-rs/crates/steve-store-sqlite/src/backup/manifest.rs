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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Files {
    /// Logical bytes in the daily backup's independently readable `files/` tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_bytes: Option<u64>,
    pub consistency: String,
    pub complete: bool,
    pub artifacts: Vec<Artifact>,
    pub error: Option<String>,
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
    pub fn files_path(&self) -> Option<PathBuf> {
        if self.files.tree_bytes.is_some() {
            Some(self.directory.join(super::plan::FILE_TREE))
        } else {
            self.files
                .artifacts
                .first()
                .map(|a| self.directory.join(&a.name))
        }
    }

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

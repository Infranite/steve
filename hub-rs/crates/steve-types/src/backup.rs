//! Backup receipts shared by the Hub and its offline consumers.
use serde::{Deserialize, Serialize};
use std::fmt;

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotId(String);

impl TryFrom<String> for SnapshotId {
    type Error = &'static str;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if id.len() == 64
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(id))
        } else {
            Err("invalid full backup snapshot ID")
        }
    }
}

impl From<SnapshotId> for String {
    fn from(id: SnapshotId) -> Self {
        id.0
    }
}

impl SnapshotId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackupPurpose {
    Daily,
    PreUpgrade,
    PreDestructive,
    Manual,
    Diagnostic,
}

impl BackupPurpose {
    pub const ALL: [Self; 5] = [
        Self::Daily,
        Self::PreUpgrade,
        Self::PreDestructive,
        Self::Manual,
        Self::Diagnostic,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::PreUpgrade => "pre-upgrade",
            Self::PreDestructive => "pre-destructive",
            Self::Manual => "manual",
            Self::Diagnostic => "diagnostic",
        }
    }
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupReceipt {
    pub snapshot_id: SnapshotId,
    pub purpose: BackupPurpose,
    pub created_ms: u64,
    pub database: VerifiedDatabase,
    pub written_bytes: u64,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Serialize, Deserialize)]
pub struct VerifiedDatabase {
    pub sha256: String,
    pub bytes: u64,
}

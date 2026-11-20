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
    /// UTF-8 paths remain strings; other Unix paths use a disjoint unix_bytes object.
    #[serde(with = "skipped_path")]
    pub path: PathBuf,
    pub bytes: u64,
}

mod skipped_path {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::path::{Path, PathBuf};

    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Text(String),
        Unix { unix_bytes: Vec<u8> },
    }

    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(text) = path.to_str() {
            return serializer.serialize_str(text);
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Repr::Unix {
                unix_bytes: path.as_os_str().as_bytes().to_vec(),
            }
            .serialize(serializer)
        }
        #[cfg(not(unix))]
        Err(serde::ser::Error::custom(
            "unsupported non-UTF-8 path encoding",
        ))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        match Repr::deserialize(deserializer)? {
            Repr::Text(text) => Ok(text.into()),
            Repr::Unix { unix_bytes } => {
                #[cfg(unix)]
                {
                    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
                    Ok(OsString::from_vec(unix_bytes).into())
                }
                #[cfg(not(unix))]
                {
                    let _ = unix_bytes;
                    Err(serde::de::Error::custom(
                        "Unix path bytes require a Unix host",
                    ))
                }
            }
        }
    }
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
    pub storage: crate::compat::CurrentStorage,
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

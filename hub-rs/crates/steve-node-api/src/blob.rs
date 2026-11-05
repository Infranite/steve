//! Moving bytes: attachments to a Node, outputs worth keeping to the Hub. A transfer resumes on its
//! own; it is not the operation it serves. Secrets never go this way.

use serde::{Deserialize, Serialize};

use crate::common::ExecutionRef;
use crate::ids::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobManifest {
    pub sha256: Digest,
    pub size: u64,
    pub mime: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    pub id: TransferId,
    pub next_offset: u64,
}

/// Starts (or, with the same key, finds again) an upload: a lost answer never leaves an orphan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeginUpload {
    pub key: OperationId,
    pub access: FileAccess,
    pub manifest: BlobManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TransferState {
    Open {
        next_offset: u64,
    },
    /// Already kept: finishing again answers the same artifact.
    Done {
        artifact: crate::run::ArtifactRef,
    },
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobChunk {
    pub transfer: TransferId,
    pub offset: u64,
    #[serde(with = "hexbytes")]
    pub bytes: Vec<u8>,
    /// Hash of `bytes`, checked on arrival.
    pub digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadChunk {
    pub offset: u64,
    #[serde(with = "hexbytes")]
    pub bytes: Vec<u8>,
    pub eof: bool,
}

/// On whose behalf bytes are read: an execution that was given the artifact, or an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "for", rename_all = "snake_case")]
pub enum FileAccess {
    Execution(ExecutionRef),
    Operation { operation: OperationId },
}

/// Bytes as hex in JSON, twice their size: a chunk is cut so that its encoded message stays within
/// the frame limit. A binary codec will need its own bytes representation.
pub(crate) mod hexbytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s).map_err(serde::de::Error::custom)
    }
}

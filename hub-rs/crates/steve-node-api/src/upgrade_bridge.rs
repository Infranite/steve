//! Adapters between the frozen upgrade schema and the ordinary, evolving Node contract.
//! Nothing in upgrade_v1 imports this module.

use crate::upgrade_v1 as v1;
use crate::*;

/// Local archive naming, outside the permanent wire contract.
pub const ARTIFACT_PREFIX: &str = "node-upgrade.";

impl From<ApiError> for v1::Error {
    fn from(error: ApiError) -> Self {
        let code = match error.code {
            ErrorCode::Busy => v1::Code::Busy,
            ErrorCode::Unsupported => v1::Code::Unsupported,
            ErrorCode::Denied => v1::Code::Denied,
            ErrorCode::Conflict => v1::Code::Conflict,
            ErrorCode::Exhausted => v1::Code::Exhausted,
            ErrorCode::Invalid | ErrorCode::Expired => v1::Code::Invalid,
            ErrorCode::Downgrade => v1::Code::Downgrade,
            ErrorCode::Stale | ErrorCode::Offline => v1::Code::Incompatible,
            // Never turn uncertainty into permission to resend a possibly executed command.
            ErrorCode::OutcomeUnknown | ErrorCode::Gap | ErrorCode::Internal => v1::Code::Internal,
        };
        Self {
            code,
            detail: error.safe_detail,
        }
    }
}

impl From<v1::Error> for ApiError {
    fn from(error: v1::Error) -> Self {
        let code = match error.code {
            v1::Code::Incompatible => ErrorCode::Offline,
            v1::Code::Unsupported => ErrorCode::Unsupported,
            v1::Code::Invalid => ErrorCode::Invalid,
            v1::Code::Downgrade => ErrorCode::Downgrade,
            v1::Code::Busy => ErrorCode::Busy,
            v1::Code::Denied => ErrorCode::Denied,
            v1::Code::Conflict => ErrorCode::Conflict,
            v1::Code::Exhausted => ErrorCode::Exhausted,
            v1::Code::Internal => ErrorCode::Internal,
        };
        let retry = if code == ErrorCode::Offline || error.code.retryable() {
            RetryClass::SameOperation
        } else {
            RetryClass::Never
        };
        Self {
            code,
            retry,
            operation: None,
            state: None,
            safe_detail: error.detail,
        }
    }
}

impl From<BuildIdentity> for v1::BuildIdentity {
    fn from(build: BuildIdentity) -> Self {
        Self {
            version: build.version,
            commit: build.commit,
            target: build.target,
        }
    }
}
impl From<v1::BuildIdentity> for BuildIdentity {
    fn from(build: v1::BuildIdentity) -> Self {
        Self {
            version: build.version,
            commit: build.commit,
            target: build.target,
        }
    }
}
impl From<NodeUpgrade> for v1::StageRequest {
    fn from(u: NodeUpgrade) -> Self {
        Self {
            operation: u.operation.to_string(),
            build: u.build.into(),
            size: u.size,
            sha256: u.sha256,
            health_secs: u.health_secs,
        }
    }
}
impl TryFrom<v1::StageRequest> for NodeUpgrade {
    type Error = v1::Error;
    fn try_from(u: v1::StageRequest) -> v1::Result<Self> {
        Ok(Self {
            artifact: ArtifactId::new(format!("{}{}", ARTIFACT_PREFIX, u.operation))
                .map_err(|e| invalid(e.to_string()))?,
            operation: operation(u.operation)?,
            build: u.build.into(),
            size: u.size,
            sha256: u.sha256,
            health_secs: u.health_secs,
        })
    }
}
impl From<UpgradeSwitch> for v1::SwitchRequest {
    fn from(s: UpgradeSwitch) -> Self {
        Self {
            operation: s.operation.to_string(),
            action: if s.abandon {
                v1::SwitchAction::Abandon
            } else if s.force {
                v1::SwitchAction::Force
            } else {
                v1::SwitchAction::Switch
            },
        }
    }
}
impl TryFrom<v1::SwitchRequest> for UpgradeSwitch {
    type Error = v1::Error;
    fn try_from(s: v1::SwitchRequest) -> v1::Result<Self> {
        Ok(Self {
            operation: operation(s.operation)?,
            abandon: s.action == v1::SwitchAction::Abandon,
            force: s.action == v1::SwitchAction::Force,
        })
    }
}
impl TryFrom<NodeUpgradeState> for v1::UpgradeState {
    type Error = v1::Error;
    fn try_from(s: NodeUpgradeState) -> v1::Result<Self> {
        let phase = serde_json::from_value(serde_json::Value::String(s.phase))
            .map_err(|_| invalid("an upgrade phase not represented by v1"))?;
        Ok(Self {
            operation: s.operation.to_string(),
            phase,
            version: s.version,
            previous: s.previous,
            error: s.error,
            updated: s.updated.0,
        })
    }
}
impl TryFrom<v1::UpgradeState> for NodeUpgradeState {
    type Error = v1::Error;
    fn try_from(s: v1::UpgradeState) -> v1::Result<Self> {
        let phase = serde_json::to_value(s.phase)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        Ok(Self {
            operation: operation(s.operation)?,
            phase,
            version: s.version,
            previous: s.previous,
            error: s.error,
            updated: Timestamp(s.updated),
        })
    }
}
impl From<ReadChunk> for v1::FetchResponse {
    fn from(chunk: ReadChunk) -> Self {
        Self {
            offset: chunk.offset,
            bytes: chunk.bytes,
            eof: chunk.eof,
        }
    }
}
impl From<v1::FetchResponse> for ReadChunk {
    fn from(chunk: v1::FetchResponse) -> Self {
        Self {
            offset: chunk.offset,
            bytes: chunk.bytes,
            eof: chunk.eof,
        }
    }
}

pub fn operation(id: String) -> v1::Result<OperationId> {
    OperationId::new(id).map_err(|e| invalid(e.to_string()))
}

pub fn fetch_request(q: v1::FetchRequest) -> v1::Result<(FileAccess, ArtifactId, u64, u32)> {
    if q.max == 0 {
        return Err(invalid("nonzero read size required"));
    }
    let op = operation(q.operation)?;
    let id =
        ArtifactId::new(format!("{}{op}", ARTIFACT_PREFIX)).map_err(|e| invalid(e.to_string()))?;
    Ok((FileAccess::Operation { operation: op }, id, q.offset, q.max))
}

pub fn invalid(detail: impl Into<String>) -> v1::Error {
    v1::Error::new(v1::Code::Invalid, detail)
}

/// Parse the release installer's three decimal components once for validation and comparison.
pub fn release_version(v: &str) -> v1::Result<[u64; 3]> {
    let parts: Vec<_> = v.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid(
            "upgrade versions must be three decimal release components",
        ));
    }
    Ok([
        parts[0].parse().map_err(|_| invalid("version overflow"))?,
        parts[1].parse().map_err(|_| invalid("version overflow"))?,
        parts[2].parse().map_err(|_| invalid("version overflow"))?,
    ])
}

/// Release versions are exactly three decimal components, as required by the release installer.
/// Compare numerically (1.10.0 > 1.9.0), never lexically. Unknown versions fail closed.
pub fn refuse_downgrade(current: &str, target: &str) -> v1::Result<()> {
    if release_version(target)? < release_version(current)? {
        return Err(v1::Error::new(
            v1::Code::Downgrade,
            format!(
                "refusing downgrade from {current} to {target}: an older Node may not open this journal"
            ),
        ));
    }
    Ok(())
}

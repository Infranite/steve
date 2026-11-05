//! Opaque ids. Each is its own type so a session id can never be passed where an attempt id belongs.
//! They are checked when built or decoded (non-empty, at most 128 bytes, `[A-Za-z0-9_.:-]`), which
//! keeps them printable and bounded. They are not file names (`..` is a valid id): a Node that names
//! a directory after one encodes it first. Whoever receives an id still checks what it may do.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Why a string is not a valid id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadId(pub String);

impl fmt::Display for BadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad id: {}", self.0)
    }
}

impl std::error::Error for BadId {}

pub(crate) fn check_id(s: &str) -> Result<(), BadId> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(BadId(s.chars().take(40).collect()))
    }
}

macro_rules! ids {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Result<Self, BadId> {
                let s = s.into();
                check_id(&s)?;
                Ok(Self(s))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = BadId;
            fn try_from(s: String) -> Result<Self, BadId> {
                Self::new(s)
            }
        }
        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    )*};
}

ids! {
    /// A paired machine (`devices.id`); the Hub's own machine is `hub`.
    NodeId,
    /// A Steve session (`sessions.id`), the Hub's.
    SessionId,
    /// A logical turn of a session, the Hub's; it may take several attempts.
    TurnId,
    /// A one-shot job (a Steve model call, a research run), the Hub's.
    JobId,
    /// One try of a turn or a job, chosen by the Hub. Resending the same attempt is not a retry.
    AttemptId,
    /// The idempotency key of a command: retries of the same command keep it.
    OperationId,
    /// A permission request a harness raised.
    PermissionId,
    /// A tool invocation inside an execution.
    CallId,
    /// What an execution or operation may do, issued by the Hub.
    GrantId,
    /// A directory root a Node allows (a logical id, never a path).
    RootId,
    /// A Node-side working directory (not a Hub workspace: that one is a collaboration boundary).
    WorkspaceId,
    /// A blob the Hub keeps.
    ArtifactId,
    /// An ordered event stream on a Node (one per session or job).
    StreamId,
    /// A generation: a stream epoch, a meter epoch, a Hub incarnation.
    Epoch,
    TransferId,
    /// The version of something that can change (a workspace, a policy, a decision).
    Revision,
    /// Where a harness session continues; only the Node that issued it understands it.
    ResumeHandle,
    /// A page position.
    Cursor,
    EventId,
}

/// A content hash, `sha256:` and 64 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    /// `sha256:<hex>` of `bytes`.
    pub fn sha256(bytes: &[u8]) -> Self {
        use sha2::Digest as _;
        Self(format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(bytes))
        ))
    }
    pub fn new(s: impl Into<String>) -> Result<Self, BadId> {
        let s = s.into();
        let ok = s.strip_prefix("sha256:").is_some_and(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if ok {
            Ok(Self(s))
        } else {
            Err(BadId(s.chars().take(40).collect()))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Digest {
    type Error = BadId;
    fn try_from(s: String) -> Result<Self, BadId> {
        Self::new(s)
    }
}
impl From<Digest> for String {
    fn from(v: Digest) -> String {
        v.0
    }
}
impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An id derived from other text (`session:<id>`, a resource key): `<prefix>:<text>` when that is a
/// valid id and `text` does not begin with `:`, otherwise `<prefix>::` plus a hash. The two forms
/// never meet (only the hashed one has `::` after the prefix), so deriving never fails and never
/// collides short of a hash collision.
pub fn derived_id(prefix: &str, text: &str) -> String {
    let plain = format!("{prefix}:{text}");
    if !text.starts_with(':') && check_id(&plain).is_ok() {
        return plain;
    }
    use sha2::Digest as _;
    format!(
        "{prefix}::{}",
        &hex::encode(sha2::Sha256::digest(text.as_bytes()))[..32]
    )
}

/// A path relative to a root or workspace: `/`-separated, no `..`, not absolute. The Node resolves it
/// (realpath, symlinks) and checks again on every open; this type only rules out the obvious escapes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    pub fn new(s: impl Into<String>) -> Result<Self, BadId> {
        let s = s.into();
        let bad = s.len() > 4096
            || s.starts_with('/')
            || s.contains('\\')
            || s.contains('\0')
            || s.split('/').any(|c| c == "..")
            || (!s.is_empty() && s.split('/').any(str::is_empty))
            || s.as_bytes().get(1) == Some(&b':');
        if bad {
            Err(BadId(s.chars().take(40).collect()))
        } else {
            Ok(Self(s))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for RelPath {
    type Error = BadId;
    fn try_from(s: String) -> Result<Self, BadId> {
        Self::new(s)
    }
}
impl From<RelPath> for String {
    fn from(v: RelPath) -> String {
        v.0
    }
}

/// Unix milliseconds. Durations on a Node are measured with a monotonic clock; this is for deadlines
/// and records that cross machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_checked() {
        assert!(SessionId::new("s_1a2b").is_ok());
        assert!(SessionId::new("").is_err());
        assert!(SessionId::new("a b").is_err());
        assert!(SessionId::new("x".repeat(129)).is_err());
        assert!(serde_json::from_str::<TurnId>("\"t/1\"").is_err());
        let t: TurnId = serde_json::from_str("\"t_1\"").unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"t_1\"");
    }

    #[test]
    fn digests_and_derived_ids() {
        assert!(Digest::new("x").is_err());
        assert!(Digest::new(Digest::sha256(b"a").to_string()).is_ok());
        assert_eq!(derived_id("session", "s_1"), "session:s_1");
        let long = derived_id("session", &"x".repeat(128));
        assert!(check_id(&long).is_ok() && long.starts_with("session::"));
        // a valid id that looks like the hashed form of another still differs from it
        let forged = long.trim_start_matches("session:").to_string();
        assert_ne!(derived_id("session", &forged), long);
        assert!(check_id(&derived_id("item", "ST 12/中文")).is_ok());
    }

    #[test]
    fn rel_paths_stay_inside() {
        assert!(RelPath::new("src/main.rs").is_ok());
        assert!(RelPath::new("").is_ok());
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../../b",
            "a\\b",
            "a//b",
            "a/",
            "C:foo",
        ] {
            assert!(RelPath::new(bad).is_err(), "{bad}");
        }
    }
}

//! Shared one-shot model call types carried across the Hub/Core/Node boundary without pulling
//! in `steve-acp` or any agent transport.

use std::fmt;

use crate::TimeoutPhase;

/// Why a one-shot model call failed. User-facing text belongs to the caller/UI layer.
#[derive(Debug)]
pub enum OneShotError {
    /// The accepted run's output/ingress budget was exhausted.
    OutputLimit,
    /// Still running after the timeout; the adapter is stopped.
    Timeout(TimeoutPhase),
    /// The adapter could not be started or opened.
    Failed(String),
    /// The model reached for a tool the call's policy does not allow.
    ToolRefused,
    /// The adapter answered the prompt with an error (the provider or model).
    /// `transient`: it said the service was unavailable or overloaded, so another try may succeed.
    Harness {
        text: String,
        code: Option<i64>,
        transient: bool,
    },
    /// The adapter lacks what the call needs (`unsupported: images`).
    Unsupported(&'static str),
    /// The colocated local service Node is running an older build lacking job capability and is
    /// waiting for active turns to finish before replacing itself.
    LocalNodeUpdating,
    Io(std::io::Error),
}

impl OneShotError {
    /// Whether another try of the same call may succeed.
    pub fn transient(&self) -> bool {
        matches!(
            self,
            OneShotError::Harness {
                transient: true,
                ..
            }
        )
    }
}

impl fmt::Display for OneShotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputLimit => f.write_str("output limit"),
            Self::Timeout(phase) => write!(f, "timed out in {phase}"),
            Self::Failed(msg) => f.write_str(msg),
            Self::ToolRefused => f.write_str("read-only policy refused a tool"),
            Self::Harness { text, .. } => f.write_str(text),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::LocalNodeUpdating => f.write_str("local node is updating"),
            Self::Io(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for OneShotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for OneShotError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// An image carried in a one-shot prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `image/png`, `image/jpeg`, …
    pub mime: String,
    pub bytes: Vec<u8>,
}

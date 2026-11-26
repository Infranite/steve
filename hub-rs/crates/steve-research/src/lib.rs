//! Read-only research for the main thread: the policy of a research run (its limits, the attempts
//! it makes and what the thread shows of its end, [`plan`], [`drive`], [`present`]), its prompt,
//! its attachments and its system sandbox. Each attempt runs on a Node as a one-shot ACP call of
//! the chosen harness adapter. Read-only is enforced by:
//!
//! 1. **The adapter's read-only mode**: Steve's Plan mode as the adapter spells it, with
//!    `session/request_permission` unconditionally refused in one-shot ACP calls.
//! 2. **System sandbox** ([`steve_confine`]): on macOS the adapter runs under `sandbox-exec` with a
//!    profile that denies file writes except the run directory and the harness's own state.
//!
//! A run has no time, turn or tool limit unless setting `thread.research` names one; it ends when it is
//! done, when the user stops it, or when the harness says nothing at all for [`DEFAULT_IDLE`] (the same
//! for every harness), keeping what it wrote so far. One run per workspace.

pub mod answer;
pub mod attach;
pub mod guard;
pub mod prompt;
mod run;
#[cfg(test)]
mod sandbox;

pub use answer::{Citation, Offer};
pub use attach::Attachment;
pub use guard::{Scope, Violation};
pub use prompt::{DirInfo, PromptInput};
pub use run::{
    AttemptPlan, AttemptRunner, Attempted, Budget, BudgetHit, DEFAULT_IDLE, EXIT_WAIT, McpAccess,
    ResearchConfig, ResearchError, ResearchOutcome, ResearchRequest, Resume, RunStatus, drive,
    plan, present,
};
// The public ResearchConfig policy type, shared with confinement.
pub use steve_confine::SandboxMode;

//! Read-only research for the main thread: the policy of a research run (its limits, the attempts
//! it makes and what the thread shows of its end, [`plan`], [`drive`], [`present`]), its prompt, the
//! guard every tool call it makes is checked by, its attachments and its system sandbox. Each
//! attempt runs on a Node as a one-shot ACP call of the chosen harness adapter. Read-only is enforced
//! three times, each enough on its own to stop a write:
//!
//! 1. **The adapter's read-only mode**: Steve's Plan mode as the adapter spells it.
//! 2. **The guard** ([`guard`]): every tool call the adapter reports is checked by what it is (its
//!    ACP kind and locations); the first one that isn't a read inside the run's directories ends the
//!    run (`violated`).
//! 3. **System sandbox** ([`sandbox`]): on macOS the adapter runs under `sandbox-exec` with a profile
//!    that denies file writes except the run directory and the harness's own state.
//!
//! A run has no time, turn or tool limit unless setting `thread.research` names one; it ends when it is
//! done, when the user stops it, or when the harness says nothing at all for [`DEFAULT_IDLE`] (the same
//! for every harness), keeping what it wrote so far. One run per workspace.

pub mod answer;
pub mod attach;
pub mod guard;
pub mod prompt;
mod run;
pub mod sandbox;

pub use answer::{Citation, Offer};
pub use attach::Attachment;
pub use guard::{Scope, Violation};
pub use prompt::{DirInfo, PromptInput};
pub use run::{
    AttemptPlan, AttemptRunner, Attempted, Budget, BudgetHit, DEFAULT_IDLE, EXIT_WAIT, McpAccess,
    ResearchConfig, ResearchError, ResearchOutcome, ResearchRequest, Resume, RunStatus, drive,
    plan, present,
};
pub use sandbox::SandboxMode;

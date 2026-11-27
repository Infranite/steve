//! Read-only research for the Node's one-shot research jobs: budgets, attempt plans, the
//! kind-based guard, attachments, and OS confinement. Hub-side prompt rendering and `OFFER:`
//! parsing live in `steve-hub` / `steve-core`.

pub mod attach;
pub mod guard;
mod run;
#[cfg(test)]
mod sandbox;

pub use attach::Attachment;
pub use guard::{Scope, Violation};
pub use run::{
    AttemptPlan, AttemptRunner, Attempted, Budget, BudgetHit, DEFAULT_IDLE, EXIT_WAIT, McpAccess,
    ResearchConfig, ResearchError, ResearchOutcome, ResearchRequest, Resume, RunStatus, drive,
    plan,
};
// The public ResearchConfig policy type, shared with confinement.
pub use steve_confine::SandboxMode;

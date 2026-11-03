//! Inline read-only research for the main thread (thread-routing.md §4, slice R2).
//!
//! A research run is one `claude -p --output-format stream-json` process that may only read the
//! workspace's project directories, the message's attachments and (through Steve's MCP) the knowledge base
//! and items. Read-only is enforced three times, each enough on its own to stop a write:
//!
//! 1. **Harness allowlist** ([`command`]): only Read/Grep/Glob exist, writing/shell/web tools are denied,
//!    secrets are denied in `permissions.deny`, project settings and hooks are not loaded, anything not
//!    pre-approved is refused (`dontAsk`).
//! 2. **Hub guard** ([`guard::research_guard`]): every tool call the harness reports is checked; the first
//!    one that isn't a read inside the run's directories kills the process (`violated`).
//! 3. **System sandbox** ([`sandbox`]): on macOS the process runs under `sandbox-exec` with a profile that
//!    denies file writes except the run directory and `~/.claude`.
//!
//! Budgets (240 s, 24 turns, 40 tools; one run per workspace) end a run with what it wrote so far.
//!
//! ```no_run
//! # async fn demo() -> Result<(), steve_research::ResearchError> {
//! use steve_research::*;
//! let r = Researcher::new(ResearchConfig::new("/opt/homebrew/bin/claude", "/tmp/scratch/research"));
//! let mut req = ResearchRequest::new("ws_1", "检索怎么打分？", "…prompt…");
//! req.cwd = Some("/path/to/project".into());
//! let mut run = r.start(req)?;
//! while let Some(ev) = run.next_event().await {
//!     if let ResearchEvent::Text { text } = ev { /* steve.draft */ let _ = text; }
//! }
//! let out = run.finish().await;
//! // follow-up: ResearchRequest { resume: Some(Resume { claude_session: out.claude_session.unwrap(), … }), … }
//! # Ok(()) }
//! ```

pub mod answer;
pub mod attach;
pub mod command;
pub mod guard;
mod processes;
pub mod prompt;
mod run;
pub mod sandbox;
pub mod stream;

pub use answer::{Citation, Offer};
pub use attach::Attachment;
pub use guard::{Scope, Violation, research_guard};
pub use prompt::{DirInfo, PromptInput};
pub use run::{
    Budget, BudgetHit, DEFAULT_IDLE, DEFAULT_MODELS, McpAccess, ResearchConfig, ResearchError,
    ResearchEvent, ResearchOutcome, ResearchRequest, ResearchRun, Researcher, Resume, RunStatus,
    StopHandle, ToolSeen, models_from_env,
};
pub use sandbox::SandboxMode;

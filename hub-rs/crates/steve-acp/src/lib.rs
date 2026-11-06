//! ACP client for the Steve Hub: runs harness adapters (Claude Code via claude-agent-acp, Codex via
//! codex-acp) as child processes and talks JSON-RPC over their stdio. Port of `hub/src/acp.ts` and
//! `hub/src/codex.ts`, plus the adapter resolution of `config.ts`.
//!
//! Independent of the database: the Hub implements [`Host`] to persist events, session status,
//! queued prompts and permission decisions, and to receive turn ends.
//!
//! Hand-written rather than built on the `agent-client-protocol` crate: its schema has no
//! `session/set_model`, and Steve stores raw update JSON and reads non-standard `_meta` fields.
//! One intended difference from the TS: adapters run in their own process group and the whole group
//! is signalled, so the `claude` child of claude-agent-acp does not outlive it.

mod attachments;
mod codex;
mod config;
pub mod normalize;
pub mod permission;
mod rollout;
pub mod rpc;
mod runner;
pub mod session;
mod util;

pub use attachments::{FileRef, ForAgent, INLINE_IMAGES, StoredFile, for_agent, kb};
pub use codex::{codex_args, codex_mode, sanitize_catalog};
pub use config::{
    AcpConfig, AdapterProperties, default_claude_adapter, default_codex_adapter, npm_root,
};
pub use rollout::find as codex_rollout;
pub use rollout::turn_usage as codex_turn_usage;
pub use rpc::RpcError;
pub use runner::{
    Acp, AcpError, AutonomousUsage, Flushed, Host, McpEndpoint, Message, PermissionRequest,
    Prompted, RESUME_AFTER_TOOLS, SessionInfo, TurnBegin, TurnEnd, TurnFinish, TurnUsage, Usage,
    auto_allow, is_dead_agent, is_transient, permission_request, steve_tool,
};

//! The ACP driver Steve's Node runs harness adapters with (Claude Code via claude-agent-acp, Codex
//! via codex-acp): child processes spoken to in JSON-RPC over their stdio ([`rpc`]), a session's
//! handshake ([`session`]), its updates read into Steve's events ([`normalize`]) and permission
//! requests ([`permission`]), plus the adapter resolution of `config.ts`.
//!
//! Independent of the database and of the Hub: the Node (`steve-node`) holds the sessions and
//! reports what they do. The Hub's own runner of these adapters is gone (B7).
//!
//! Hand-written rather than built on the `agent-client-protocol` crate: its schema has no
//! `session/set_model`, and Steve stores raw update JSON and reads non-standard `_meta` fields.
//! One intended difference from the TS: adapters run in their own process group and the whole group
//! is signalled, so the `claude` child of claude-agent-acp does not outlive it.

mod codex;
mod config;
mod errors;
pub mod normalize;
pub mod permission;
mod rollout;
pub mod rpc;
pub mod session;
mod util;

pub use codex::{codex_args, codex_mode, sanitize_catalog};
pub use config::{
    AcpConfig, AdapterProperties, default_claude_adapter, default_codex_adapter, npm_root,
};
pub use errors::{is_dead_agent, is_transient};
pub use rollout::find as codex_rollout;
pub use rollout::turn_usage as codex_turn_usage;
pub use rpc::RpcError;

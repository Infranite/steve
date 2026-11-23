//! The contract between the Hub and a Node.
//!
//! The Hub is the brain: queue, decisions, model choice and retries, permission rulings, Steve's tools.
//! A Node hosts harnesses: it runs attempts, owns the project directories on its machine and reports
//! facts. This crate is the system protocol between the two, in Steve's own words. Agent protocols
//! (ACP, MCP, a CLI's stream-json) end inside a Node's drivers and never appear here: no ACP payload,
//! no MCP URL, no CLI argument, no local path.
//!
//! Everything is owned data that serializes (the same types later go over the wire); the ports are
//! async and object safe. A dropped future only stops waiting, it never cancels an operation: cancel
//! is its own command.

pub mod blob;
pub mod common;
pub mod discovery;
pub mod env;
pub mod events;
pub mod glob;
pub mod harness_updates;
pub mod ids;
pub mod intent;
pub mod ports;
pub mod run;
pub mod stop;
pub mod tools;
pub mod upgrade_bridge;
pub mod upgrade_v1;
pub mod workspace;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use blob::*;
pub use common::*;
pub use discovery::*;
pub use events::*;
pub use harness_updates::*;
pub use ids::*;
pub use intent::{Intent, IntentKey};
pub use ports::*;
pub use run::*;
pub use steve_types::gateway::{GatewayFailure, TimeoutPhase};
pub use stop::*;
pub use tools::*;
pub use workspace::*;

/// Exact ordinary wire contract. Every wire-format change increments this in the same commit.
/// A different value permits only the independent, permanent upgrade protocol.
pub const PROTOCOL: u32 = 13;

/// Why an execution a Node held is `unknown` after the Node restarted (its process, the harness
/// with it, is gone): what the Hub gives up and continues on its own (B6), unlike other unknowns.
pub const RESTARTED: &str = "the node restarted while it ran";
/// The `stop_reason` of a turn given up because its Node restarted, its harness provably gone:
/// the agent is continued once, told to check first.
pub const NODE_RESTARTED: &str = "node_restarted";
/// The code of the [`EventBody::Diagnostic`] for an attempt whose session's harness was given
/// Steve's tools and never listed them: its agent ran without them.
pub const STEVE_TOOLS_MISSING: &str = "steve_tools_missing";
/// Version of [`events::NodeEvent`] bodies, separate from the transport.
pub const EVENT_SCHEMA: u16 = 1;

pub mod usage;
pub use usage::{UsageMeter, accounting_usage, accumulate_usage, merged_usage};

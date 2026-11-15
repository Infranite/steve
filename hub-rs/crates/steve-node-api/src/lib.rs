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
pub mod ids;
pub mod ports;
pub mod run;
pub mod stop;
pub mod tools;
pub mod workspace;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use blob::*;
pub use common::*;
pub use discovery::*;
pub use events::*;
pub use ids::*;
pub use ports::*;
pub use run::*;
pub use stop::*;
pub use tools::*;
pub use workspace::*;

/// Major.minor of this contract. A major change refuses the connection; a minor one only adds, and
/// the Hub sends what a minor added only to a Node whose hello says that minor (see [`AUTO_MODE`]).
pub const PROTOCOL: (u16, u16) = (0, 4);
/// The first protocol with [`PermissionMode::Auto`]: an older Node cannot decode it, so the Hub
/// never opens or configures a session in `auto` on one.
pub const AUTO_MODE: (u16, u16) = (0, 2);
/// The first protocol with [`Discovery::vitals`] (B3): an older Node is not asked.
pub const VITALS: (u16, u16) = (0, 3);
/// The first protocol with live per-device turn capacity ([`Discovery::set_session_capacity`]).
pub const SESSION_CAPACITY: (u16, u16) = (0, 4);
/// Why an execution a Node held is `unknown` after the Node restarted (its process, the harness
/// with it, is gone): what the Hub gives up and continues on its own (B6), unlike other unknowns.
pub const RESTARTED: &str = "the node restarted while it ran";
/// The `stop_reason` of a turn given up because its Node restarted, its harness provably gone:
/// the agent is continued once, told to check first.
pub const NODE_RESTARTED: &str = "node_restarted";
/// Version of [`events::NodeEvent`] bodies, separate from the transport.
pub const EVENT_SCHEMA: u16 = 1;

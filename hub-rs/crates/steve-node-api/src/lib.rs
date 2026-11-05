//! The contract between the Hub and a Node (docs/hub-node-client.md, docs/node-protocol.md).
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
pub mod events;
pub mod ids;
pub mod ports;
pub mod run;
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
pub use tools::*;
pub use workspace::*;

/// Major.minor of this contract. A major change refuses the connection; a minor one only adds.
pub const PROTOCOL: (u16, u16) = (0, 1);
/// Version of [`events::NodeEvent`] bodies, separate from the transport.
pub const EVENT_SCHEMA: u16 = 1;

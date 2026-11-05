//! The Hub ↔ Node transport (N2): one WebSocket per Node, dialled by the Node, carrying the
//! `steve-node-api` ports as RPC in both directions.
//!
//! The port traits are the protocol: every port method is one wire method whose argument and
//! result are the existing serde types, unchanged. [`RemoteNode`] implements [`steve_node_api::Node`]
//! on the Hub side, so the coordinator talks to a remote Node exactly as to a local one;
//! [`NodeServer`] dispatches the same calls to any `Node` on the Node side. ACP and MCP never cross
//! this link: only Steve's commands, receipts, events, tool calls and blob chunks do.
//!
//! The transport never retries. A call that was never written fails `offline` (the same command may
//! be sent again, it is deduplicated by its operation id); a call that was written and then lost its
//! link or its deadline fails `outcome_unknown` for anything carrying an operation, `offline` for a
//! pure read. Reconciling is the coordinator's job (operation, snapshot, call_status).

pub mod conn;
pub mod envelope;
pub mod handshake;
pub mod identity;
pub mod methods;
pub mod node_server;
pub mod remote_hub;
pub mod remote_node;
pub mod serve_hub;
pub mod testkit;
pub mod tls;
pub mod ws;

pub use conn::{Conn, ConnOptions, Handler};
pub use envelope::{Envelope, Frame, FrameSink, FrameStream, Kind};
pub use handshake::{Admission, Admitted, Hello, Welcome};
pub use identity::Identity;
pub use node_server::NodeServer;
pub use remote_hub::{HubLink, RemoteFiles, RemoteSink, RemoteTools};
pub use remote_node::RemoteNode;
pub use serve_hub::ServeHub;

/// Version of the envelope itself; [`steve_node_api::PROTOCOL`] is the version of what it carries.
pub const WIRE: (u16, u16) = (1, 0);

/// Close codes (application range).
pub mod close {
    /// A malformed envelope or an oversize control frame.
    pub const MALFORMED: u16 = 4400;
    /// Refused: unknown, pending, revoked Node or a bad signature (one text for all).
    pub const DENIED: u16 = 4403;
    /// Silent past the heartbeat budget.
    pub const DEAD: u16 = 4408;
    /// A newer connection of the same Node took over.
    pub const FENCED: u16 = 4409;
    /// No common protocol major.
    pub const UNSUPPORTED: u16 = 4426;
    /// The Hub could not decide (store error, timeout); dial again.
    pub const INTERNAL: u16 = 1011;
    pub const GOODBYE: u16 = 1000;
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
pub mod sock;
pub mod testkit;
pub mod tls;
pub mod ws;

pub use conn::{Conn, ConnOptions, Handler};
pub use envelope::{Envelope, Frame, FrameSink, FrameStream, Kind};
pub use handshake::{Admission, Admitted, Hello, Welcome};
pub use identity::Identity;
pub use node_server::NodeServer;
pub use remote_hub::{HubLink, RemoteFiles, RemoteSecrets, RemoteSink, RemoteTools};
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

/// A wait drawn at random between half of `d` and all of it ("equal jitter"): Nodes that lost
/// their link at the same moment (the Hub restarted) dial again spread out, not all at once.
pub fn jitter(d: std::time::Duration) -> std::time::Duration {
    let mut b = [0u8; 8];
    let r = match getrandom::fill(&mut b) {
        Ok(()) => u64::from_le_bytes(b),
        // no randomness: the clock's nanoseconds spread them as well
        Err(_) => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.subsec_nanos() as u64)
            .unwrap_or(0),
    };
    let half = d / 2;
    half + std::time::Duration::from_nanos(r % (d - half).as_nanos().max(1) as u64)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn a_jittered_wait_stays_within_half_and_all_of_it() {
        let d = Duration::from_millis(800);
        let waits: Vec<Duration> = (0..200).map(|_| super::jitter(d)).collect();
        assert!(waits.iter().all(|w| *w >= d / 2 && *w <= d), "{waits:?}");
        let distinct: std::collections::HashSet<_> = waits.iter().collect();
        assert!(distinct.len() > 100, "spread out: {}", distinct.len());
    }
}

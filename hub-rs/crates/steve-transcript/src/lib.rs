//! A session's transcript in Steve's own terms: what clients, the core and statistics read.
//!
//! Every row of `events` is one of two formats:
//!
//! - `steve.v1`: written by the Hub. Its kinds are the transcript's own ([`Entry`]); a reader takes
//!   them as stored.
//! - `legacy.acp`: rows the ACP runner wrote before (and, until the switch, still writes): an ACP
//!   `session/update` object under its own kind. [`LegacyReader`] reads them into the same entries,
//!   with the shared normalizer a Node's driver uses, and only on the read path: nothing new is
//!   written in that format by the Node protocol, and nothing above this crate sees ACP.
//!
//! Kinds the Hub writes itself (`user`, `queued…`, `system`, `permission`, `turn_end`) are the same
//! in both formats. The transcript's own kinds never collide with ACP's.

use serde_json::{Value, json};
use steve_node_api::{EventBody, Role};

pub const V1: &str = "steve.v1";
pub const LEGACY: &str = "legacy.acp";

/// Text of one message: `{role, message, text}`; consecutive texts of a message are appended.
pub const TEXT: &str = "text";
/// A message begins: `{message, role}` (kept so a later text knows its role).
pub const MESSAGE: &str = "message";
/// The whole current view of one tool invocation: `{tool: ToolView}`, replacing the previous one.
pub const TOOL: &str = "tool";
/// The agent's plan: `{entries: [{content, status}]}`.
pub const PLAN: &str = "plan";

/// One transcript entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub kind: String,
    pub data: Value,
}

impl Entry {
    fn new(kind: &str, data: Value) -> Self {
        Entry {
            kind: kind.into(),
            data,
        }
    }
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Thought => "thought",
    }
}

/// The entry of one Node event, if it belongs in the transcript. `role`: the role of the message a
/// text delta belongs to (from its `message_started`); the rest of a Node's events are the Hub's
/// business (attempts, permissions, usage), not the transcript's.
pub fn of_node(body: &EventBody, role: Option<Role>) -> Option<Entry> {
    match body {
        EventBody::MessageStarted { message, role, .. } => Some(Entry::new(
            MESSAGE,
            json!({ "message": message, "role": role_name(*role) }),
        )),
        EventBody::TextDelta { message, text, .. } => Some(Entry::new(
            TEXT,
            json!({
                "role": role_name(role.unwrap_or(Role::Assistant)),
                "message": message,
                "text": text,
            }),
        )),
        EventBody::ToolChanged { tool } => Some(Entry::new(TOOL, json!({ "tool": tool }))),
        EventBody::Plan { entries } => Some(Entry::new(PLAN, json!({ "entries": entries }))),
        _ => None,
    }
}

/// The assistant's text in a row of either format, if it is one.
pub fn assistant_text<'a>(kind: &str, data: &'a Value) -> Option<&'a str> {
    match kind {
        TEXT if data.get("role").and_then(Value::as_str) == Some("assistant") => {
            data.get("text").and_then(Value::as_str)
        }
        "agent_message_chunk" => data.pointer("/content/text").and_then(Value::as_str),
        _ => None,
    }
}

/// Whether a row is one of the ACP runner's: an ACP update under its own kind.
pub fn is_legacy(kind: &str, data: &Value) -> bool {
    data.get("sessionUpdate").and_then(Value::as_str) == Some(kind)
}

#[cfg(feature = "legacy")]
mod legacy;
#[cfg(feature = "legacy")]
pub use legacy::LegacyReader;

#[cfg(test)]
mod tests;

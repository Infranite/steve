//! Reading the old ACP rows (`legacy.acp`), with the normalizer a Node's driver uses.

use serde_json::Value;
use steve_acp::normalize::{Fact, Normalizer};
use steve_node_api::{Epoch, EventBody, Role};

use crate::{Entry, is_legacy, of_node};

/// Reads a session's rows, in order, as transcript entries: the old ACP rows are read, every other
/// row (the Hub's own kinds, `steve.v1` entries) passes as it is. Tool updates carry only what
/// changed: the reader keeps what it saw before, and [`Self::missing_tool`] names a tool whose
/// beginning it has not seen (a page or a live row read alone), so the caller can [`Self::prime`]
/// it with that tool's earlier rows first.
pub struct LegacyReader {
    norm: Normalizer,
    seen: std::collections::HashSet<String>,
}

impl LegacyReader {
    /// `cwd`: the session's working directory, to show paths inside it relative.
    pub fn new(cwd: &str) -> Self {
        LegacyReader {
            norm: Normalizer::new(
                cwd,
                "legacy",
                Epoch::try_from("legacy".to_string()).expect("a valid epoch"),
            ),
            seen: Default::default(),
        }
    }

    /// The raw tool id of a tool update whose earlier rows this reader has not seen.
    pub fn missing_tool(&self, kind: &str, data: &Value) -> Option<String> {
        if kind != "tool_call_update" || !is_legacy(kind, data) {
            return None;
        }
        let id = data.get("toolCallId")?.as_str()?;
        (!self.seen.contains(id)).then(|| id.to_string())
    }

    /// Takes earlier rows in without producing entries.
    pub fn prime(&mut self, kind: &str, data: &Value) {
        if matches!(kind, "tool_call" | "tool_call_update")
            && is_legacy(kind, data)
            && let Some(id) = data.get("toolCallId").and_then(Value::as_str)
        {
            self.seen.insert(id.to_string());
            self.norm.merge_tool(data);
        }
    }

    /// The entries of one row. Rows that are not ACP's (the Hub's own kinds) pass as they are.
    pub fn read(&mut self, kind: &str, data: &Value) -> Vec<Entry> {
        if !is_legacy(kind, data) {
            return vec![Entry {
                kind: kind.into(),
                data: data.clone(),
            }];
        }
        if let Some(id) = data.get("toolCallId").and_then(Value::as_str) {
            self.seen.insert(id.to_string());
        }
        let mut role = None;
        let mut out = vec![];
        for f in self.norm.update(data) {
            let Fact::Event(body) = f else { continue };
            match &body {
                // the message boundaries of old rows are the reader's own guess: not kept
                EventBody::MessageStarted { role: r, .. } => role = Some(*r),
                EventBody::MessageCompleted { .. } => {}
                EventBody::TextDelta { .. } => {
                    let r = role.or(Some(match kind {
                        "agent_thought_chunk" => Role::Thought,
                        _ => Role::Assistant,
                    }));
                    out.extend(of_node(&body, r));
                }
                _ => out.extend(of_node(&body, None)),
            }
        }
        out
    }
}

//! Group 6 owns events and reasons for items, sessions, decisions, main conversation, transcript and threads.
//! Associated ID labels (ItemId, SessionId, DecisionId, MessageId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

pub const REASONS: &[ReasonSet] = &[];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum WorkEvent {
    #[serde(rename = "item")]
    Item { id: ItemId, ws: WorkspaceId },
    #[serde(rename = "decision")]
    Decision {
        id: DecisionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional = nullable, as = "Option<ItemId>"))]
        item_id: Option<Option<ItemId>>,
        ws: WorkspaceId,
    },
    #[serde(rename = "session")]
    Session {
        id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional = nullable, as = "Option<ItemId>"))]
        item_id: Option<Option<ItemId>>,
        ws: WorkspaceId,
    },
    #[serde(rename = "message")]
    Message {
        id: MessageId,
        ws: WorkspaceId,
        space: String,
    },
}

impl Scoped for WorkEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Item { ws, .. } => Scope::Ws(ws.clone()),
            Self::Decision { ws, .. } => Scope::Ws(ws.clone()),
            Self::Session { ws, .. } => Scope::Ws(ws.clone()),
            Self::Message { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum WorkLiveEvent {
    #[serde(rename = "event")]
    Event {
        session_id: SessionId,
        seq: Seq,
        kind: String,
        data: RawJson,
        ws: WorkspaceId,
    },
    #[serde(rename = "steve.thinking")]
    SteveThinking {
        on: bool,
        ws: WorkspaceId,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        phase: Option<String>,
    },
    #[serde(rename = "steve.draft")]
    SteveDraft {
        ws: WorkspaceId,
        mid: MessageId,
        thought: String,
        text: String,
    },
    #[serde(rename = "steve.tool")]
    SteveTool {
        ws: WorkspaceId,
        mid: MessageId,
        run: String,
        name: String,
        brief: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        kind: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        running: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        call: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        started_at: Option<Millis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        at: Option<Millis>,
    },
}

impl Scoped for WorkLiveEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Event { ws, .. } => Scope::Ws(ws.clone()),
            Self::SteveThinking { ws, .. } => Scope::Ws(ws.clone()),
            Self::SteveDraft { ws, .. } => Scope::Ws(ws.clone()),
            Self::SteveTool { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

impl WorkEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Item { .. } => "item",
            Self::Decision { .. } => "decision",
            Self::Session { .. } => "session",
            Self::Message { .. } => "message",
        }
    }
}

impl WorkLiveEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Event { .. } => "event",
            Self::SteveThinking { .. } => "steve.thinking",
            Self::SteveDraft { .. } => "steve.draft",
            Self::SteveTool { .. } => "steve.tool",
        }
    }
}

impl WorkEvent {
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::Item { id, .. } => Some(id.as_str()),
            Self::Decision { id, .. } => Some(id.as_str()),
            Self::Session { id, .. } => Some(id.as_str()),
            Self::Message { id, .. } => Some(id.as_str()),
        }
    }
}

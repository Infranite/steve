//! Group 6 owns events and reasons for items, sessions, decisions, main conversation, transcript and threads.
//! Associated ID labels (ItemId, SessionId, DecisionId, MessageId) are all defined centrally in `id.rs`.

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ThreadPlanStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ThreadPlanEntry {
    pub content: String,
    pub status: ThreadPlanStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ThreadResearchStage {
    Triage,
    Preparing,
    Submitting,
    Submitted,
    Responding,
    Thinking,
    Reading,
    Searching,
    Executing,
    Editing,
    Fetching,
    CallingTool,
    Writing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ThreadSubtaskOrigin {
    Subagent,
    BackgroundTask,
    NativeWorkflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ThreadSubtaskStatus {
    Queued,
    Running,
    NeedsYou,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ThreadSubtask {
    pub id: String,
    pub origin: ThreadSubtaskOrigin,
    pub status: ThreadSubtaskStatus,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub progress: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub started_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional, type = "number"))]
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ThreadProgressKind {
    Stage {
        stage: ThreadResearchStage,
    },
    Thought {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        done: bool,
    },
    Plan {
        entries: Vec<ThreadPlanEntry>,
    },
    Subtask {
        subtask: ThreadSubtask,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ThreadLiveTool {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub call: Option<u32>,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brief: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub started_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional, type = "number"))]
    pub ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ThreadLiveThought {
    pub id: String,
    pub text: String,
    pub started_at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub at: Option<Millis>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
}

/// Lightweight in-flight snapshot returned by `GET /api/summary` so page reloads restore total elapsed time, step progress, plan, thoughts, and tools immediately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ThreadLiveSnapshot {
    pub run: String,
    pub started_at: Millis,
    pub last_activity_at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub stage: Option<ThreadResearchStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<ThreadPlanEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thoughts: Vec<ThreadLiveThought>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ThreadLiveTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<ThreadSubtask>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft_text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft_thought: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mid: Option<MessageId>,
}

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
    #[serde(rename = "workflow.run")]
    WorkflowRun {
        id: String,
        item_id: ItemId,
        ws: WorkspaceId,
    },
}

impl Scoped for WorkEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Item { ws, .. } => Scope::Ws(ws.clone()),
            Self::Decision { ws, .. } => Scope::Ws(ws.clone()),
            Self::Session { ws, .. } => Scope::Ws(ws.clone()),
            Self::Message { ws, .. } => Scope::Ws(ws.clone()),
            Self::WorkflowRun { ws, .. } => Scope::Ws(ws.clone()),
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        started_at: Option<Millis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        at: Option<Millis>,
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
        #[serde(default, skip_serializing_if = "String::is_empty")]
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        step_started_at: Option<Millis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional, type = "number"))]
        ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        excerpt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        error: Option<String>,
    },
    #[serde(rename = "steve.progress")]
    SteveProgress {
        ws: WorkspaceId,
        mid: MessageId,
        run: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        started_at: Option<Millis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        at: Option<Millis>,
        body: ThreadProgressKind,
    },
}

impl Scoped for WorkLiveEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Event { ws, .. } => Scope::Ws(ws.clone()),
            Self::SteveThinking { ws, .. } => Scope::Ws(ws.clone()),
            Self::SteveDraft { ws, .. } => Scope::Ws(ws.clone()),
            Self::SteveTool { ws, .. } => Scope::Ws(ws.clone()),
            Self::SteveProgress { ws, .. } => Scope::Ws(ws.clone()),
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
            Self::WorkflowRun { .. } => "workflow.run",
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
            Self::SteveProgress { .. } => "steve.progress",
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
            Self::WorkflowRun { id, .. } => Some(id.as_str()),
        }
    }
}

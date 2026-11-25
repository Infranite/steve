//! Read views: what clients (and, later, plugins) read of the core's data. A view holds one entity's
//! own fields and the ids of the entities it refers to, never another entity's data; values are
//! code values, and text is as it was written. The Hub keeps the views of the hot entities in
//! memory and sends what changed as [`Patch`] frames; [`Snapshot`], [`ById`] and [`PageOf`] are
//! the reads around them. Every one carries a `rev` (the bus revision it is as of): a client keeps,
//! per entity, the copy with the larger `rev`.

use crate::*;
use serde::{Deserialize, Serialize};

fn default_session_source(value: &str) -> bool {
    value == "Steve"
}

fn zero(n: &u32) -> bool {
    *n == 0
}

/// The kinds of view. A kind is listed here once the Hub keeps its views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ViewKind {
    Decision,
    Item,
    Session,
    Message,
}

impl ViewKind {
    pub const ALL: [ViewKind; 4] = [Self::Decision, Self::Item, Self::Session, Self::Message];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Item => "item",
            Self::Session => "session",
            Self::Message => "message",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ItemView {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub placement: Option<PlacementExplanation>,
    pub id: ItemId,
    pub ws: WorkspaceId,
    pub no: i64,
    pub title: String,
    pub state: ItemState,
    pub cycle: i64,
    pub revision: i64,
    pub delivery_revision: i64,
    pub delivery: Vec<DeliveryRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub bot_id: Option<BotId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub project_id: Option<ProjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub parent: Option<ItemParent>,
    pub priority: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(test, ts(as = "Option<bool>", optional))]
    pub star: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(test, ts(as = "Option<bool>", optional))]
    pub archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub snooze: Option<ItemSnooze>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub accept_mode: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(test, ts(as = "Option<bool>", optional))]
    pub design_gate: bool,
    pub source: String,
    pub created: Millis,
    pub updated: Millis,
    #[serde(skip_serializing_if = "zero")]
    #[cfg_attr(test, ts(as = "Option<u32>", optional))]
    pub plan_done: u32,
    #[serde(skip_serializing_if = "zero")]
    #[cfg_attr(test, ts(as = "Option<u32>", optional))]
    pub plan_total: u32,
    pub goals: u32,
    pub tags: Vec<TagId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(test, ts(as = "Option<Vec<ItemId>>", optional))]
    pub deps: Vec<ItemId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(test, ts(as = "Option<Vec<ItemWait>>", optional))]
    pub waits: Vec<ItemWait>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub prerequisite_wait: Option<WaitContract>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(test, ts(as = "Option<Vec<OperationBlocker>>", optional))]
    pub op_blockers: Vec<OperationBlocker>,
    // Changes when detail-only facts need a fresh read, without embedding them in the list.
    #[cfg_attr(test, ts(type = "number"))]
    pub detail_stamp: u64,
    #[serde(skip_serializing_if = "zero")]
    #[cfg_attr(test, ts(as = "Option<u32>", optional))]
    pub active_operations: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(test, ts(as = "Option<bool>", optional))]
    pub has_branch: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ItemParent {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub item: Option<ItemId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub session: Option<SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub at: Option<Millis>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ItemSnooze {
    pub label: String,
    pub until: Millis,
}

/// The discriminator stored for an item cycle. The close request and outcome are its payload.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    strum::AsRefStr,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ItemPhase {
    #[default]
    Ready,
    Active,
    Paused,
    Closing,
    Closed,
}

impl ItemPhase {
    pub fn as_str(&self) -> &str {
        self.as_ref()
    }

    pub fn can_run(self) -> bool {
        match self {
            Self::Ready | Self::Active => true,
            Self::Paused | Self::Closing | Self::Closed => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ItemState {
    Ready,
    Active,
    Paused,
    Closing {
        request: CloseKind,
        waits: Vec<ItemWait>,
    },
    Closed {
        outcome: ItemOutcome,
        by: ClosedBy,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum CloseKind {
    #[default]
    Complete,
    Cancel,
    Fail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ItemOutcome {
    Finished,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ClosedBy {
    Owner,
    Acceptance,
    Rule,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ItemWait {
    Prerequisite,
    Delivery,
    Operation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct OperationBlocker {
    pub kind: String,
    pub op_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub wait: Option<WaitContract>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SessionView {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub placement: Option<PlacementExplanation>,
    pub id: SessionId,
    pub ws: WorkspaceId,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub item_id: Option<ItemId>,
    pub no: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub bot_id: Option<BotId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "ReasoningEffort::is_default")]
    #[cfg_attr(test, ts(as = "Option<ReasoningEffort>", optional))]
    pub reasoning_effort: ReasoningEffort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub effort_mapping: Option<EffortMapping>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub device_id: Option<DeviceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub node_id: Option<NodeId>,
    pub title: String,
    #[serde(skip_serializing_if = "default_session_source")]
    #[cfg_attr(test, ts(as = "Option<String>", optional))]
    pub source: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(test, ts(as = "Option<bool>", optional))]
    pub readonly: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub harness_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub used: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    #[cfg_attr(test, ts(as = "Option<f64>"))]
    pub cost: Option<serde_json::Number>,
    pub created: Millis,
    pub updated: Millis,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(test, ts(as = "Option<bool>", optional))]
    pub busy: bool,
    #[serde(skip_serializing_if = "zero")]
    #[cfg_attr(test, ts(as = "Option<u32>", optional))]
    pub queued: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub waiting: Option<SessionWaiting>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SessionWaiting {
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub detail: Option<String>,
}

/// A decision (待处理 card) as clients see it. Hot while `status` is `open`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DecisionView {
    pub id: DecisionId,
    pub ws: WorkspaceId,
    /// `accept` | `ask` | `approve` | `fail` | `propose` | `confirm`
    pub kind: String,
    pub subject_type: String,
    pub subject_id: String,
    pub item_id: Option<ItemId>,
    pub session_id: Option<SessionId>,
    pub question: Said,
    pub draft: String,
    pub options: Vec<String>,
    pub status: DecisionStatus,
    /// What it was settled with, as recorded.
    pub result: Option<RawJson>,
    pub requested_by: String,
    /// Actor who settled the decision (a user id, `"yolo"`, or `None` when expired/system-settled).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub resolved_by: Option<String>,
    pub created: Millis,
    pub resolved: Option<Millis>,
    /// Until when the answer can be taken back; `None` when it cannot.
    pub undo_until: Option<Millis>,
    /// The conversation (chat space) the decision is about, when it is about a chat message.
    pub space_id: Option<SpaceId>,
    /// Where it goes in 待处理 (the Hub's rules, in a fixed order).
    pub tier: DecisionTier,
    /// Why it is folded away (`tier` = `folded`).
    pub folded: Option<FoldReason>,
    #[cfg_attr(test, ts(type = "number"))]
    pub revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub wait: Option<WaitContract>,
}

/// The text shown on a decision card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Said {
    Raw { text: String },
    Key { template: DecisionPrompt },
}

/// Machine-authored prompts carry their arguments separately from the UI text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DecisionPrompt {
    TurnRepeatedFailure,
    TurnNoProgress,
    MergePullRequest {
        #[cfg_attr(test, ts(type = "number"))]
        number: i64,
        repo: String,
    },
    NoExecutor {
        items: Vec<NoExecutorItem>,
        rejected: Vec<PlacementBlocker>,
        reason: PlacementRejection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        team: Option<ProposalTeam>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        proposal: Option<BotProposal>,
    },
    WaitTimeout {
        wait_kind: WaitKind,
        subject: String,
    },
}

/// The current state of a decision. An answer receipt survives reopening.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, strum::EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DecisionStatus {
    Open,
    Resolved,
    Dismissed,
    Expired,
    Superseded,
}

impl DecisionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Dismissed => "dismissed",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }
}

impl std::fmt::Display for DecisionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The tiers of 待处理, decided by the Hub in a fixed order of rules: an approved send that failed
/// and a starred contact's message always ask you; a conversation you answered yourself folds; a
/// reply drafted for you is a draft; everything else asks you.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DecisionTier {
    /// Asks you: counts in the badge and reminds you.
    Decide,
    /// A reply drafted for you: one row per conversation, no reminder, no badge.
    Draft,
    /// Folded away, with the reason.
    Folded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum FoldReason {
    /// You spoke in the conversation yourself after the message it is about.
    Replied,
    /// The decision has expired.
    Expired,
    /// The draft has lost its sending route.
    Unsendable,
}

/// One stored turn in the main conversation. Metadata keeps the thread's structured cards.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct MessageView {
    pub id: MessageId,
    pub ws: WorkspaceId,
    pub author: String,
    pub text: String,
    pub item_id: Option<ItemId>,
    pub created: Millis,
    pub meta: RawJson,
}

/// One view of any kind; the kind travels next to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum View {
    Decision(DecisionView),
    Item(ItemView),
    Session(SessionView),
    Message(MessageView),
}

impl View {
    pub fn id(&self) -> &str {
        match self {
            Self::Decision(v) => v.id.as_str(),
            Self::Item(v) => v.id.as_str(),
            Self::Session(v) => v.id.as_str(),
            Self::Message(v) => v.id.as_str(),
        }
    }

    pub fn workspace(&self) -> &str {
        match self {
            Self::Decision(v) => v.ws.as_str(),
            Self::Item(v) => v.ws.as_str(),
            Self::Session(v) => v.ws.as_str(),
            Self::Message(v) => v.ws.as_str(),
        }
    }

    pub fn kind(&self) -> ViewKind {
        match self {
            Self::Decision(_) => ViewKind::Decision,
            Self::Item(_) => ViewKind::Item,
            Self::Session(_) => ViewKind::Session,
            Self::Message(_) => ViewKind::Message,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Op {
    Upsert,
    Remove,
}

/// One change: `upsert` carries the view, `remove` (the entity is gone) does not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PatchOp {
    #[serde(rename = "type")]
    pub kind: ViewKind,
    pub op: Op,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub view: Option<View>,
}

/// What changed in one workspace's views, as of the `rev` of the envelope it travels in (an SSE
/// frame `{"type":"patch","ws":…,"ops":[…],"rev":…}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Patch {
    #[serde(rename = "type")]
    pub tag: PatchTag,
    pub ws: WorkspaceId,
    pub ops: Vec<PatchOp>,
}

impl Patch {
    pub fn new(ws: WorkspaceId, ops: Vec<PatchOp>) -> Self {
        Self {
            tag: PatchTag::Patch,
            ws,
            ops,
        }
    }
}

/// The frame type of a [`Patch`]: always `patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum PatchTag {
    Patch,
}

impl Scoped for Patch {
    fn scope(&self) -> Scope {
        Scope::Ws(self.ws.clone())
    }
}

/// `GET /api/snapshot`: the hot views of the kinds asked for. `cursor` is where the event stream
/// continues from (`/api/stream?after=`) so that no patch after this snapshot is missed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Snapshot {
    pub epoch: Epoch,
    pub rev: Rev,
    pub cursor: String,
    pub views: SnapshotViews,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SnapshotViews {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub decision: Option<Vec<DecisionView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub item: Option<Vec<ItemView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub session: Option<Vec<SessionView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub message: Option<PageOf<MessageView>>,
}

/// `GET /api/views?kind=&ids=`: the views asked for that exist, and the ids that do not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ById {
    pub rev: Rev,
    pub views: Vec<View>,
    pub missing: Vec<String>,
}

/// `GET /api/views/page?kind=…`: one page of views outside the hot set; `next` continues it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PageOf<V> {
    pub rev: Rev,
    pub views: Vec<V>,
    pub next: Option<String>,
}

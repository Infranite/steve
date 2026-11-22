//! Read views: what clients (and, later, plugins) read of the core's data. A view holds one entity's
//! own fields and the ids of the entities it refers to, never another entity's data; values are
//! code values, and text is as it was written. The Hub keeps the views of the hot entities in
//! memory and sends what changed as [`Patch`] frames; [`Snapshot`], [`ById`] and [`PageOf`] are
//! the reads around them. Every one carries a `rev` (the bus revision it is as of): a client keeps,
//! per entity, the copy with the larger `rev`.

use crate::*;
use serde::Serialize;

/// The kinds of view. A kind is listed here once the Hub keeps its views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ViewKind {
    Decision,
}

impl ViewKind {
    pub const ALL: [ViewKind; 1] = [ViewKind::Decision];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
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
    pub question: String,
    pub draft: String,
    pub options: Vec<String>,
    /// `open` | `resolved` | `expired`
    pub status: String,
    /// What it was settled with, as recorded.
    pub result: Option<RawJson>,
    pub requested_by: String,
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

/// One view of any kind; the kind travels next to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum View {
    Decision(DecisionView),
}

impl View {
    pub fn kind(&self) -> ViewKind {
        match self {
            Self::Decision(_) => ViewKind::Decision,
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

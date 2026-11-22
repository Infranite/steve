//! Decision views. Hot: `status = 'open'`.
//!
//! A view's `tier` follows [`tier`]; the facts its rules need about a conversation ([`ChatFacts`]:
//! whether the person is a starred contact, whether you spoke there since) are the channels'
//! (steve-channels `tier`), so the views read here carry the tier without them and the Hub sets it
//! with them.

use sea_query::{Expr, ExprTrait, IntoColumnRef, JoinType, Order, Query, SelectStatement};
use steve_types::{DecisionTier, DecisionView, FoldReason, Millis, RawJson};

use super::CHUNK;
use crate::contract::{Json, Row};
use crate::{Read, Result};

mod schema {
    use sea_query::Iden;

    #[derive(Iden, Clone, Copy)]
    pub enum Decisions {
        Table,
        Id,
        WorkspaceId,
        Kind,
        SubjectType,
        SubjectId,
        ItemId,
        SessionId,
        Question,
        Draft,
        Options,
        Status,
        Result,
        RequestedBy,
        Created,
        Resolved,
        Undo,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum ChanMessages {
        Table,
        Id,
        SpaceId,
    }

    #[derive(Iden, Clone, Copy)]
    pub enum Spaces {
        Table,
        Id,
        WorkspaceId,
        ExternalId,
    }
}
use schema::ChanMessages as M;
use schema::Decisions as D;
use schema::Spaces as S;

/// The `subject_type` of a decision about a chat message (steve-channels `respond::SUBJECT_MESSAGE`).
pub const ABOUT_CHAT_MESSAGE: &str = "chan_message";

/// What a decision's tier depends on beyond the decision itself: facts about the conversation of
/// the message it is about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChatFacts {
    /// The message is from a starred contact.
    pub starred: bool,
    /// You spoke in the conversation yourself after the message.
    pub replied: bool,
}

/// The tier of an open decision, by the first rule that holds:
/// 1. an approved send that failed asks you;
/// 2. a starred contact's message asks you (in any space, even one that only records);
/// 3. a conversation you answered yourself is folded (`replied`);
/// 4. a reply drafted for you is a draft;
/// 5. anything else asks you (Steve's questions, broken connections, every other decision).
pub fn tier(kind: &str, subject_type: &str, f: ChatFacts) -> (DecisionTier, Option<FoldReason>) {
    let chat = subject_type == ABOUT_CHAT_MESSAGE;
    if chat && kind == "fail" {
        (DecisionTier::Decide, None)
    } else if chat && f.starred {
        (DecisionTier::Decide, None)
    } else if chat && f.replied {
        (DecisionTier::Folded, Some(FoldReason::Replied))
    } else if chat && kind == "approve" {
        (DecisionTier::Draft, None)
    } else {
        (DecisionTier::Decide, None)
    }
}

fn d(c: D) -> Expr {
    Expr::col((D::Table, c).into_column_ref())
}

/// An answer can be taken back for a day (the orchestrator's `undo_decision` checks the same).
const UNDO_FOR: i64 = 24 * 3_600_000;

pub struct DecisionViews<'a, C>(pub(super) &'a mut C);

/// A position in the settled decisions, newest first: `<resolved>:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledCursor {
    pub resolved: i64,
    pub id: String,
}

impl SettledCursor {
    pub fn parse(s: &str) -> Option<Self> {
        let (at, id) = s.split_once(':')?;
        Some(Self {
            resolved: at.parse().ok()?,
            id: id.to_owned(),
        })
    }
}

impl std::fmt::Display for SettledCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.resolved, self.id)
    }
}

/// One page of a workspace's settled decisions (resolved or expired), newest first.
#[derive(Debug, Clone)]
pub struct DecisionPage {
    /// Only those settled after this (ms).
    pub since: i64,
    pub before: Option<SettledCursor>,
    pub limit: u32,
}

fn select() -> SelectStatement {
    Query::select()
        .columns(
            [
                D::Id,
                D::WorkspaceId,
                D::Kind,
                D::SubjectType,
                D::SubjectId,
                D::ItemId,
                D::SessionId,
                D::Question,
                D::Draft,
                D::Options,
                D::Status,
                D::Result,
                D::RequestedBy,
                D::Created,
                D::Resolved,
                D::Undo,
            ]
            .map(|c| (D::Table, c)),
        )
        .column((M::Table, M::SpaceId))
        .from(D::Table)
        .join(
            JoinType::LeftJoin,
            M::Table,
            Expr::col((M::Table, M::Id))
                .equals((D::Table, D::SubjectId))
                .and(d(D::SubjectType).eq(ABOUT_CHAT_MESSAGE)),
        )
        .to_owned()
}

fn view(r: &Row<'_>) -> Result<DecisionView> {
    let status: String = r.get(10)?;
    let resolved: Option<i64> = r.get(14)?;
    // JSON written by the store; a value that is not JSON reads as nothing rather than failing
    // every view of the workspace
    let result: Option<String> = r.get(11)?;
    let undo: Option<String> = r.get(15)?;
    let undoable = status == "resolved"
        && undo
            .as_deref()
            .and_then(|u| serde_json::from_str(u).ok())
            .is_some_and(|u| crate::types::truthy(&u));
    let options: Option<Json<Vec<String>>> = r.get(9).ok();
    let (kind, subject_type): (String, String) = (r.get(2)?, r.get(3)?);
    let (tier, folded) = tier(&kind, &subject_type, ChatFacts::default());
    Ok(DecisionView {
        id: r.get::<String>(0)?.into(),
        ws: r.get::<String>(1)?.into(),
        kind,
        subject_type,
        subject_id: r.get(4)?,
        item_id: r.get::<Option<String>>(5)?.map(Into::into),
        session_id: r.get::<Option<String>>(6)?.map(Into::into),
        question: r.get(7)?,
        draft: r.get(8)?,
        options: options.map(|o| o.0).unwrap_or_default(),
        result: result.and_then(|s| RawJson::from_str(&s).ok()),
        requested_by: r.get(12)?,
        created: Millis(r.get(13)?),
        resolved: resolved.map(Millis),
        undo_until: resolved
            .filter(|r| undoable && *r != 0)
            .map(|r| Millis(r + UNDO_FOR)),
        status,
        space_id: r.get::<Option<String>>(16)?.map(Into::into),
        tier,
        folded,
    })
}

impl<C: Read> DecisionViews<'_, C> {
    /// Every workspace's open decisions, oldest first (loading and rebuilding the projection).
    pub fn open(&mut self) -> Result<Vec<DecisionView>> {
        self.0.all(
            select()
                .and_where(d(D::Status).eq("open"))
                .order_by((D::Table, D::Created), Order::Asc),
            view,
        )
    }

    /// The decisions `ids` of workspace `ws` that exist, in any status.
    pub fn of(&mut self, ws: &str, ids: &[String]) -> Result<Vec<DecisionView>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            out.extend(
                self.0.all(
                    select()
                        .and_where(d(D::WorkspaceId).eq(ws))
                        .and_where(d(D::Id).is_in(chunk.iter().map(String::as_str))),
                    view,
                )?,
            );
        }
        Ok(out)
    }

    /// An item's decisions, oldest first.
    pub fn of_item(&mut self, ws: &str, item: &str) -> Result<Vec<DecisionView>> {
        self.0.all(
            select()
                .and_where(d(D::WorkspaceId).eq(ws))
                .and_where(d(D::ItemId).eq(item))
                .order_by((D::Table, D::Created), Order::Asc),
            view,
        )
    }

    /// The open decisions about a message in the conversation of space `space` (every space of
    /// the same chat: your own account and a bot see one chat as two spaces).
    pub fn open_about_chat(&mut self, ws: &str, space: &str) -> Result<Vec<String>> {
        let chat = Query::select()
            .column((S::Table, S::Id))
            .from(S::Table)
            .and_where(Expr::col((S::Table, S::WorkspaceId)).eq(ws))
            .and_where(
                Expr::col((S::Table, S::Id))
                    .eq(space)
                    .or(Expr::col((S::Table, S::ExternalId)).in_subquery(
                        Query::select()
                            .column(S::ExternalId)
                            .from(S::Table)
                            .and_where(Expr::col(S::Id).eq(space))
                            .to_owned(),
                    )),
            )
            .to_owned();
        self.0.all(
            Query::select()
                .column((D::Table, D::Id))
                .from(D::Table)
                .inner_join(
                    M::Table,
                    Expr::col((M::Table, M::Id)).equals((D::Table, D::SubjectId)),
                )
                .and_where(d(D::WorkspaceId).eq(ws))
                .and_where(d(D::Status).eq("open"))
                .and_where(d(D::SubjectType).eq(ABOUT_CHAT_MESSAGE))
                .and_where(Expr::col((M::Table, M::SpaceId)).in_subquery(chat)),
            |r| r.get(0),
        )
    }

    /// Every open decision about a chat message in `ws`.
    pub fn open_about_chats(&mut self, ws: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column(D::Id)
                .from(D::Table)
                .and_where(Expr::col(D::WorkspaceId).eq(ws))
                .and_where(Expr::col(D::Status).eq("open"))
                .and_where(Expr::col(D::SubjectType).eq(ABOUT_CHAT_MESSAGE)),
            |r| r.get(0),
        )
    }

    /// One page of the settled decisions, and where the next one starts.
    pub fn settled(
        &mut self,
        ws: &str,
        p: &DecisionPage,
    ) -> Result<(Vec<DecisionView>, Option<SettledCursor>)> {
        let mut q = select()
            .and_where(d(D::WorkspaceId).eq(ws))
            .and_where(d(D::Status).ne("open"))
            .and_where(d(D::Resolved).gt(p.since))
            .order_by((D::Table, D::Resolved), Order::Desc)
            .order_by((D::Table, D::Id), Order::Desc)
            .limit(u64::from(p.limit) + 1)
            .to_owned();
        if let Some(b) = &p.before {
            q.and_where(
                d(D::Resolved).lt(b.resolved).or(d(D::Resolved)
                    .eq(b.resolved)
                    .and(d(D::Id).lt(b.id.as_str()))),
            );
        }
        let mut views = self.0.all(&q, view)?;
        let next = (views.len() > p.limit as usize).then(|| {
            views.truncate(p.limit as usize);
            let last = views.last().expect("a full page");
            SettledCursor {
                resolved: last.resolved.map_or(0, |m| m.0),
                id: last.id.to_string(),
            }
        });
        Ok((views, next))
    }
}

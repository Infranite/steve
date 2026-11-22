//! Decision views. Hot: `status = 'open'`.

use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use steve_types::{DecisionView, Millis, RawJson};

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
}
use schema::Decisions as D;

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
        .columns([
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
        ])
        .from(D::Table)
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
    Ok(DecisionView {
        id: r.get::<String>(0)?.into(),
        ws: r.get::<String>(1)?.into(),
        kind: r.get(2)?,
        subject_type: r.get(3)?,
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
    })
}

impl<C: Read> DecisionViews<'_, C> {
    /// Every workspace's open decisions, oldest first (loading and rebuilding the projection).
    pub fn open(&mut self) -> Result<Vec<DecisionView>> {
        self.0.all(
            select()
                .and_where(Expr::col(D::Status).eq("open"))
                .order_by(D::Created, Order::Asc),
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
                        .and_where(Expr::col(D::WorkspaceId).eq(ws))
                        .and_where(Expr::col(D::Id).is_in(chunk.iter().map(String::as_str))),
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
                .and_where(Expr::col(D::WorkspaceId).eq(ws))
                .and_where(Expr::col(D::ItemId).eq(item))
                .order_by(D::Created, Order::Asc),
            view,
        )
    }

    /// One page of the settled decisions, and where the next one starts.
    pub fn settled(
        &mut self,
        ws: &str,
        p: &DecisionPage,
    ) -> Result<(Vec<DecisionView>, Option<SettledCursor>)> {
        let mut q = select()
            .and_where(Expr::col(D::WorkspaceId).eq(ws))
            .and_where(Expr::col(D::Status).ne("open"))
            .and_where(Expr::col(D::Resolved).gt(p.since))
            .order_by(D::Resolved, Order::Desc)
            .order_by(D::Id, Order::Desc)
            .limit(u64::from(p.limit) + 1)
            .to_owned();
        if let Some(b) = &p.before {
            q.and_where(
                Expr::col(D::Resolved)
                    .lt(b.resolved)
                    .or(Expr::col(D::Resolved)
                        .eq(b.resolved)
                        .and(Expr::col(D::Id).lt(b.id.as_str()))),
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

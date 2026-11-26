//! Decision views. Hot: `status = 'open'`.
use sea_query::{Alias, Expr, ExprTrait, IntoColumnRef, JoinType, Order, Query, SelectStatement};
use steve_types::{
    DecisionPrompt, DecisionStatus, DecisionTier, DecisionView, FoldReason, Millis, RawJson, Said,
};

use super::{CHUNK, decision_chat};
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
        Revision,
        QuestionTemplate,
        WaitOwner,
        WaitStartedAt,
        WaitDeadline,
        WaitOnTimeout,
        ResolvedBy,
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
    }
}
use schema::ChanMessages as M;
use schema::Decisions as D;
use schema::Spaces as S;

/// The `subject_type` of a decision about a chat message (steve-channels `respond::SUBJECT_MESSAGE`).
pub const ABOUT_CHAT_MESSAGE: &str = "chan_message";

/// High-stakes or irreversible decision kinds that justify interrupting the owner in "待我决定".
/// Low-risk / informational kinds (`signal`, `party_merge`, `steward`, `attr_value`, `attr_def`,
/// `mem`, etc.) fall outside this whitelist and fold into passive review (`DecisionTier::Folded`).
fn is_decide_kind(d: &DecisionView) -> bool {
    if d.kind == "fail" {
        return true;
    }
    match d.subject_type.as_str() {
        // Permission approvals
        "tool_call" | "secret_use" | "connection" => true,
        // Outbound approvals & channel handoff asks
        ABOUT_CHAT_MESSAGE => d.kind == "ask",
        "mail_draft" | "gh_pr_open" | "gh_pr_merge" | "gh_comment" | "release" => true,
        // Delivery acceptance, design gate, blocking item questions & effectful actions
        "design_implementation" | "question" | "action" => true,
        "item" => matches!(d.kind.as_str(), "accept" | "propose" | "ask" | "approve"),
        // Bot creation / placement decisions
        "no_executor" | "handoff_unavailable" | "agents_proposal" => true,
        // Failures & stalls
        "stall" | "turn_stall" | "session_start" => true,
        // High-risk flow approvals & run budget/recovery gates
        "flow_step" | "flow_run" => true,
        _ => false,
    }
}

fn tier(
    d: &DecisionView,
    starred: bool,
    replied: bool,
    unsendable: bool,
    recovery: bool,
) -> (DecisionTier, Option<FoldReason>) {
    let chat = d.subject_type == ABOUT_CHAT_MESSAGE;
    let draft = chat && d.kind == "approve";
    if chat && (d.kind == "fail" || starred || recovery) {
        (DecisionTier::Decide, None)
    } else if matches!(
        d.status,
        DecisionStatus::Expired | DecisionStatus::Superseded
    ) {
        (DecisionTier::Folded, Some(FoldReason::Expired))
    } else if replied {
        (DecisionTier::Folded, Some(FoldReason::Replied))
    } else if draft && unsendable {
        (DecisionTier::Folded, Some(FoldReason::Unsendable))
    } else if draft && d.status == DecisionStatus::Open {
        (DecisionTier::Draft, None)
    } else if is_decide_kind(d) {
        (DecisionTier::Decide, None)
    } else {
        (DecisionTier::Folded, Some(FoldReason::LowPriority))
    }
}

fn d(c: D) -> Expr {
    Expr::col((D::Table, c).into_column_ref())
}

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
    let mut q = Query::select();
    q.columns(
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
    .expr(decision_chat::conversation())
    .expr(decision_chat::starred())
    .expr(Expr::cust(
        "c.id IS NULL OR c.status = 'revoked' OR s.status = 'left'
         OR m.direction <> 'out' OR m.status <> 'draft' OR m.decision_id IS NULL
         OR m.decision_id <> decisions.id",
    ))
    .column((D::Table, D::Revision))
    .column((D::Table, D::QuestionTemplate))
    .column((D::Table, D::WaitOwner))
    .column((D::Table, D::WaitStartedAt))
    .column((D::Table, D::WaitDeadline))
    .column((D::Table, D::WaitOnTimeout))
    .column((D::Table, D::ResolvedBy))
    .from(D::Table)
    .join_as(
        JoinType::LeftJoin,
        M::Table,
        Alias::new("m"),
        Expr::cust("m.id = decisions.subject_id AND m.workspace_id = decisions.workspace_id")
            .and(d(D::SubjectType).eq(ABOUT_CHAT_MESSAGE)),
    )
    .join_as(
        JoinType::LeftJoin,
        S::Table,
        Alias::new("s"),
        Expr::cust("s.id = m.space_id AND s.workspace_id = decisions.workspace_id"),
    )
    .join_as(
        JoinType::LeftJoin,
        Alias::new("connections"),
        Alias::new("c"),
        Expr::cust("c.id = s.connection_id"),
    );
    decision_chat::join_source(&mut q);
    q
}

struct Candidate {
    view: DecisionView,
    recovery: bool,
    starred: bool,
    unsendable: bool,
}

fn candidate(r: &Row<'_>) -> Result<Candidate> {
    let status = crate::decision_repo::decode_status(r, 10)?;
    let resolved: Option<i64> = r.get(14)?;
    // JSON written by the store; a value that is not JSON reads as nothing rather than failing
    // every view of the workspace
    let result: Option<String> = r.get(11)?;
    let undo = r
        .get::<Option<Json<crate::DecisionUndo>>>(15)?
        .map(|undo| undo.0);
    let undo_until = crate::decision_repo::undo_until(status, resolved, undo.as_ref());
    let options: Option<Json<Vec<String>>> = r.get(9).ok();
    let (kind, subject_type): (String, String) = (r.get(2)?, r.get(3)?);
    let recovery = subject_type == ABOUT_CHAT_MESSAGE
        && kind == "approve"
        && result
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|r| {
                r.get("recovery_review")
                    .and_then(serde_json::Value::as_bool)
            })
            == Some(true);
    let wait =
        crate::wait_repo::decode_contract(r.get(21)?, r.get(22)?, r.get(23)?, r.get(24)?).ok();
    let view = DecisionView {
        id: r.get::<String>(0)?.into(),
        revision: r.get(19)?,
        ws: r.get::<String>(1)?.into(),
        kind,
        subject_type,
        subject_id: r.get(4)?,
        item_id: r.get::<Option<String>>(5)?.map(Into::into),
        session_id: r.get::<Option<String>>(6)?.map(Into::into),
        question: match r.get::<Option<Json<DecisionPrompt>>>(20)? {
            Some(Json(template)) => Said::Key { template },
            None => Said::Raw { text: r.get(7)? },
        },
        draft: r.get(8)?,
        options: options.map(|o| o.0).unwrap_or_default(),
        result: result.and_then(|s| RawJson::from_str(&s).ok()),
        requested_by: r.get(12)?,
        resolved_by: r.get(25)?,
        created: Millis(r.get(13)?),
        resolved: resolved.map(Millis),
        undo_until: undo_until.map(Millis),
        status,
        space_id: r.get::<Option<String>>(16)?.map(Into::into),
        tier: DecisionTier::Decide,
        folded: None,
        wait,
    };
    Ok(Candidate {
        view,
        recovery,
        starred: r.get(17)?,
        unsendable: r.get::<Option<bool>>(18)?.unwrap_or(false),
    })
}

impl<C: Read> DecisionViews<'_, C> {
    fn read(&mut self, q: &SelectStatement) -> Result<Vec<DecisionView>> {
        let rows = self.0.all(q, candidate)?;
        let ids = rows
            .iter()
            .filter(|r| {
                let d = &r.view;
                d.status == DecisionStatus::Open
                    && d.subject_type == ABOUT_CHAT_MESSAGE
                    && d.kind != "fail"
                    && !r.starred
                    && !r.recovery
            })
            .map(|r| r.view.id.as_str())
            .collect::<Vec<_>>();
        let replied = decision_chat::replied(self.0, &ids)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let mut view = r.view;
                (view.tier, view.folded) = tier(
                    &view,
                    r.starred,
                    replied.contains(view.id.as_str()),
                    r.unsendable,
                    r.recovery,
                );
                view
            })
            .collect())
    }

    /// Every workspace's open decisions, oldest first (loading and rebuilding the projection).
    pub fn open(&mut self) -> Result<Vec<DecisionView>> {
        self.read(
            select()
                .and_where(d(D::Status).eq("open"))
                .order_by((D::Table, D::Created), Order::Asc)
                .order_by((D::Table, D::Id), Order::Asc),
        )
    }

    /// The decisions `ids` of workspace `ws` that exist, in any status.
    pub fn of(&mut self, ws: &str, ids: &[String]) -> Result<Vec<DecisionView>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            out.extend(
                self.read(
                    select()
                        .and_where(d(D::WorkspaceId).eq(ws))
                        .and_where(d(D::Id).is_in(chunk.iter().map(String::as_str))),
                )?,
            );
        }
        Ok(out)
    }

    /// An item's decisions, oldest first.
    pub fn of_item(&mut self, ws: &str, item: &str) -> Result<Vec<DecisionView>> {
        self.read(
            select()
                .and_where(d(D::WorkspaceId).eq(ws))
                .and_where(d(D::ItemId).eq(item))
                .order_by((D::Table, D::Created), Order::Asc)
                .order_by((D::Table, D::Id), Order::Asc),
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
                Expr::col((S::Table, S::Id)).eq(space).or(
                    Expr::cust_with_expr(
                        "EXISTS (SELECT 1 FROM spaces changed WHERE changed.id = ? AND
                          changed.workspace_id = spaces.workspace_id AND changed.system_id = spaces.system_id AND
                          changed.external_id = spaces.external_id)",
                        Expr::val(space),
                    ),
                ),
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
        // Apply the cursor and limit before reading channel facts for the selected decisions.
        let mut ids = Query::select();
        ids.column(D::Id)
            .from(D::Table)
            .and_where(d(D::WorkspaceId).eq(ws))
            .and_where(d(D::Status).ne("open"))
            .and_where(d(D::Resolved).gt(p.since))
            .order_by((D::Table, D::Resolved), Order::Desc)
            .order_by((D::Table, D::Id), Order::Desc)
            .limit(u64::from(p.limit) + 1);
        if let Some(b) = &p.before {
            ids.and_where(
                d(D::Resolved).lt(b.resolved).or(d(D::Resolved)
                    .eq(b.resolved)
                    .and(d(D::Id).lt(b.id.as_str()))),
            );
        }
        let mut query = select();
        query
            .and_where(d(D::Id).in_subquery(ids))
            .order_by((D::Table, D::Resolved), Order::Desc)
            .order_by((D::Table, D::Id), Order::Desc);
        let mut views = self.read(&query)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: &str, subject_type: &str) -> DecisionView {
        DecisionView {
            id: "dec_1".into(),
            revision: 0,
            ws: "default".into(),
            kind: kind.into(),
            subject_type: subject_type.into(),
            subject_id: "sub_1".into(),
            item_id: None,
            session_id: None,
            question: Said::Raw { text: "q".into() },
            draft: String::new(),
            options: vec![],
            result: None,
            requested_by: "steve".into(),
            resolved_by: None,
            created: Millis(1),
            resolved: None,
            undo_until: None,
            status: DecisionStatus::Open,
            space_id: None,
            tier: DecisionTier::Decide,
            folded: None,
            wait: None,
        }
    }

    #[test]
    fn high_stakes_whitelist_decides_and_low_risk_kinds_fold() {
        for (kind, subject) in [
            ("approve", "tool_call"),
            ("approve", "secret_use"),
            ("approve", "connection"),
            ("approve", "mail_draft"),
            ("approve", "gh_pr_open"),
            ("approve", "gh_pr_merge"),
            ("approve", "gh_comment"),
            ("approve", "release"),
            ("accept", "item"),
            ("ask", "design_implementation"),
            ("ask", "question"),
            ("approve", "action"),
            ("propose", "item"),
            ("ask", "no_executor"),
            ("ask", "handoff_unavailable"),
            ("propose", "agents_proposal"),
            ("ask", "stall"),
            ("ask", "turn_stall"),
            ("fail", "session_start"),
            ("fail", "anything"),
            ("approve", "flow_step"),
            ("ask", "flow_run"),
            ("ask", ABOUT_CHAT_MESSAGE),
        ] {
            let view = sample(kind, subject);
            assert_eq!(
                tier(&view, false, false, false, false),
                (DecisionTier::Decide, None),
                "{kind}/{subject} should be Decide"
            );
        }

        for (kind, subject) in [
            ("ask", "signal"),
            ("propose", "party_merge"),
            ("confirm", "steward"),
            ("confirm", "attr_value"),
            ("confirm", "attr_def"),
            ("confirm", "mem"),
            ("ask", "space"),
        ] {
            let view = sample(kind, subject);
            assert_eq!(
                tier(&view, false, false, false, false),
                (DecisionTier::Folded, Some(FoldReason::LowPriority)),
                "{kind}/{subject} should fold as LowPriority"
            );
        }
    }
}

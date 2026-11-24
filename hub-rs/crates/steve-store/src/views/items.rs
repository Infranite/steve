//! Item views. The list reads item facts in one statement, with no session or decision data.
use std::hash::{DefaultHasher, Hash, Hasher};

use sea_query::{Alias, Expr, ExprTrait, Query, SelectStatement};
use serde::{Deserialize, Serialize};
use steve_types::{
    BotPlacement, CloseKind, ClosedBy, ItemOutcome, ItemParent, ItemPhase, ItemSnooze, ItemState,
    ItemView, ItemWait, ItemWorkflow, Millis, OperationBlocker,
};

use super::{Page, page, read_ids};
use crate::contract::{Json, Row};
use crate::{Goal, Parent, PlanStep, Read, Result, Snooze};

fn select() -> SelectStatement {
    let mut q = Query::select();
    q.from(Alias::new("items"))
        .columns([
            "id", "workspace_id", "no", "title", "cas_revision", "outcome", "phase", "cycle", "revision",
            "bot_id", "project_id", "parent", "priority", "star", "archived",
            "due", "snooze", "accept_mode", "design_gate", "source", "created", "updated",
        ].map(Alias::new))
        .expr(Expr::cust("branch IS NOT NULL"))
        .expr(Expr::cust("CASE WHEN json_valid(plan) THEN json_array_length(plan) END"))
        .expr(Expr::cust("(SELECT count(*) FROM json_each(CASE WHEN json_valid(items.plan) THEN items.plan END) WHERE json_extract(value, '$.done') = 1)"))
        .expr(Expr::cust("CASE WHEN phase='closing' THEN json_extract(closing, '$.kind') END"))
        .expr(Expr::cust("(SELECT count(*) FROM item_goals WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT json_group_array(tag_id) FROM (SELECT tag_id FROM item_tags WHERE item_id=items.id ORDER BY rowid))"))
        .expr(Expr::cust("(SELECT json_group_array(dep_id) FROM (SELECT dep_id FROM item_deps WHERE item_id=items.id ORDER BY created,dep_id))"))
        .expr(Expr::cust("(SELECT paused FROM workflows WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT cycle FROM workflows WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT json_group_array(item_id) FROM (SELECT u.item_id FROM workflow_units u JOIN workflows w ON w.item_id=u.workflow_id WHERE u.workflow_id=items.id AND w.cycle=items.cycle AND items.phase<>'closed' AND u.status='failed' AND u.item_id IS NOT NULL ORDER BY u.position))"))
        .expr(Expr::cust("(SELECT attempt FROM workflow_units WHERE item_id=items.id LIMIT 1)"))
        .expr(Expr::cust("(SELECT max(attempt) FROM workflow_attempts WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT json_group_array(json_object('kind',kind,'status',status,'required',required,'revision',revision,'tag',json_extract(evidence,'$.tag'))) FROM (SELECT * FROM item_operations WHERE item_id=items.id AND cycle=items.cycle AND status NOT IN ('succeeded','waived') ORDER BY created,id))"))
        // The detail observer refetches when its own facts change, even if the list looks the same.
        .expr(Expr::cust("json_array(
            (SELECT max(id) FROM timeline WHERE item_id=items.id),
            (SELECT json_array(count(*), max(rowid)) FROM artifacts WHERE item_id=items.id),
            (SELECT json_array(count(*), max(updated)) FROM annotations WHERE item_id=items.id),
            (SELECT json_array(revision, updated) FROM workflows WHERE item_id=items.id),
            (SELECT follow FROM thread_items WHERE item_id=items.id)
        )"))
        .expr(Expr::cust("(SELECT fact FROM bot_placements WHERE item_id=items.id AND fact IS NOT NULL ORDER BY seq DESC LIMIT 1)"))
        .expr(Expr::cust("(SELECT json_group_array(json_object('repository',repository,'number',number,'target',target,'head',head,'base',base,'required',json(CASE required WHEN 1 THEN 'true' ELSE 'false' END))) FROM (SELECT repository,number,target,head,base,required FROM item_delivery_refs WHERE item_id=items.id AND cycle=items.cycle AND scope=COALESCE((SELECT MAX(version) FROM item_goals WHERE item_id=items.id),0) ORDER BY repository,number))"))
        .column(Alias::new("acceptance"))
        .expr(Expr::cust("EXISTS (SELECT 1 FROM item_dependency_waits w WHERE w.item_id=items.id AND w.cycle=items.cycle AND w.state='waiting') OR EXISTS (SELECT 1 FROM item_deps d LEFT JOIN items p ON p.id=d.dep_id WHERE d.item_id=items.id AND (p.phase IS NOT 'closed' OR p.outcome IS NOT 'completed'))"))
        .and_where(Expr::col(Alias::new("kind")).eq("task"));
    q
}

#[derive(Deserialize)]
struct Operation {
    kind: String,
    status: String,
    required: i64,
    revision: i64,
    tag: Option<String>,
}

fn apply_operations(v: &mut ItemView, revision: i64, ops: Vec<Operation>) {
    let closing = match v.state {
        ItemState::Closing { request, .. } => Some(request),
        ItemState::Closed { .. } | ItemState::Ready | ItemState::Active | ItemState::Paused => None,
    };
    let mut waits = false;
    for op in ops {
        let required = op.required != 0 && op.revision == revision;
        waits |= matches!(op.status.as_str(), "running" | "unknown")
            || (required && closing == Some(CloseKind::Complete));
        if op.status == "running" {
            v.active_operations += 1;
        }
        let kind = match op.status.as_str() {
            "unknown" => "unknown_effect",
            "failed" if required && closing.is_some() => "failure",
            "pending" | "running" if required && closing.is_some() => "operation",
            _ => continue,
        };
        v.op_blockers.push(OperationBlocker {
            kind: kind.into(),
            op_kind: op.kind,
            tag: op.tag,
        });
    }
    if let ItemState::Closing { waits: reasons, .. } = &mut v.state
        && waits
    {
        reasons.push(ItemWait::Operation);
    }
}

fn row(r: &Row<'_>) -> Result<ItemView> {
    let phase: ItemPhase = r
        .get::<String>(6)?
        .parse::<ItemPhase>()
        .map_err(|error| crate::StoreError::Rejected(error.to_string()))?;
    let parent = r
        .get::<Option<Json<Parent>>>(11)
        .ok()
        .flatten()
        .map(|Json(p)| ItemParent {
            item: p.item.flatten().map(Into::into),
            session: p.session.flatten().map(Into::into),
            at: p.at.flatten().map(Millis),
        });
    let snooze = r
        .get::<Option<Json<Snooze>>>(16)
        .ok()
        .flatten()
        .map(|Json(s)| ItemSnooze {
            label: s.label,
            until: Millis(s.until),
        });
    let paused: Option<bool> = r.get(29)?;
    let unit: Option<i64> = r.get(32)?;
    let attempt = unit.or(r.get(33)?);
    let workflow = if paused.is_some() || attempt.is_some() {
        Some(ItemWorkflow {
            paused,
            cycle: r.get(30)?,
            failed: r.get::<Json<Vec<_>>>(31)?.0,
            attempt,
            superseded: unit.is_none() && attempt.is_some(),
        })
    } else {
        None
    };
    let revision: i64 = r.get(4)?;
    let delivery_revision: i64 = r.get(8)?;
    let mut stamp = DefaultHasher::new();
    revision.hash(&mut stamp);
    r.get::<String>(35)?.hash(&mut stamp);
    let delivery: Vec<steve_types::DeliveryRef> = r.get::<Json<Vec<_>>>(37)?.0;
    let outcome: Option<String> = r.get(5)?;
    let acceptance: Option<String> = r.get(38)?;
    let state = match phase {
        ItemPhase::Closed => ItemState::Closed {
            outcome: match outcome.as_deref() {
                Some("completed") => ItemOutcome::Finished,
                Some("failed") => ItemOutcome::Failed,
                _ => ItemOutcome::Cancelled,
            },
            by: match acceptance.as_deref() {
                Some("owner") => ClosedBy::Owner,
                Some("human" | "waived") => ClosedBy::Acceptance,
                Some("automatic") => ClosedBy::Rule,
                _ => ClosedBy::Unknown,
            },
        },
        ItemPhase::Paused => ItemState::Paused,
        ItemPhase::Closing => ItemState::Closing {
            request: match r.get::<Option<String>>(25)?.as_deref() {
                Some("cancel") => CloseKind::Cancel,
                Some("fail") => CloseKind::Fail,
                _ => CloseKind::Complete,
            },
            waits: delivery
                .iter()
                .any(|reference| reference.required)
                .then_some(ItemWait::Delivery)
                .into_iter()
                .collect(),
        },
        ItemPhase::Ready => ItemState::Ready,
        ItemPhase::Active => ItemState::Active,
    };
    let waits = if !matches!(state, ItemState::Closed { .. }) && r.get::<bool>(39)? {
        vec![ItemWait::Prerequisite]
    } else {
        Vec::new()
    };
    let mut v = ItemView {
        placement: r
            .get::<Option<Json<BotPlacement>>>(36)?
            .map(|Json(f)| f.explanation),
        id: r.get::<String>(0)?.into(),
        ws: r.get::<String>(1)?.into(),
        no: r.get(2)?,
        title: r.get(3)?,
        state,
        cycle: r.get(7)?,
        revision,
        delivery_revision,
        delivery,
        bot_id: r.get::<Option<String>>(9)?.map(Into::into),
        project_id: r.get::<Option<String>>(10)?.map(Into::into),
        parent,
        priority: r.get(12)?,
        star: r.get(13)?,
        archived: r.get(14)?,
        due: r.get(15)?,
        snooze,
        accept_mode: r.get(17)?,
        design_gate: r.get(18)?,
        source: r
            .get::<Json<(String, String)>>(19)
            .map(|s| s.0.1)
            .unwrap_or_else(|_| crate::types::default_source().1),
        created: Millis(r.get(20)?),
        updated: Millis(r.get(21)?),
        plan_done: r.get::<i64>(24)? as u32,
        plan_total: r.get::<Option<i64>>(23)?.unwrap_or_default() as u32,
        goals: r.get::<i64>(26)? as u32,
        tags: r.get::<Json<Vec<_>>>(27)?.0,
        deps: r.get::<Json<Vec<_>>>(28)?.0,
        workflow,
        waits,
        op_blockers: Vec::new(),
        active_operations: 0,
        detail_stamp: stamp.finish() & ((1 << 53) - 1),
        has_branch: r.get(22)?,
    };
    apply_operations(&mut v, delivery_revision, r.get::<Json<Vec<_>>>(34)?.0);
    Ok(v)
}

/// Full text read only when opening an item's detail.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemText {
    pub plan: Vec<PlanStep>,
    pub goals_list: Vec<Goal>,
}

pub struct ItemViews<'a, C>(pub(super) &'a mut C);

impl<C: Read> ItemViews<'_, C> {
    pub fn text(&mut self, ws: &str, id: &str) -> Result<Option<ItemText>> {
        let Some(plan) = self.0.one(
            Query::select()
                .column(Alias::new("plan"))
                .from(Alias::new("items"))
                .and_where(super::column("workspace_id").eq(ws))
                .and_where(super::column("id").eq(id)),
            |r| {
                Ok(r.get::<Json<Vec<PlanStep>>>(0)
                    .map(|p| p.0)
                    .unwrap_or_default())
            },
        )?
        else {
            return Ok(None);
        };
        let goals_list = self.0.all(
            Query::select()
                .columns(["version", "text", "created"].map(Alias::new))
                .from(Alias::new("item_goals"))
                .and_where(super::column("item_id").eq(id))
                .order_by(Alias::new("version"), sea_query::Order::Asc),
            |r| {
                Ok(Goal {
                    version: r.get(0)?,
                    text: r.get(1)?,
                    created: r.get(2)?,
                })
            },
        )?;
        Ok(Some(ItemText { plan, goals_list }))
    }

    pub fn hot(&mut self) -> Result<Vec<ItemView>> {
        self.0.all(
            select().and_where(Expr::col(Alias::new("archived")).eq(false)),
            row,
        )
    }

    pub fn of(&mut self, ws: &str, ids: &[String]) -> Result<Vec<ItemView>> {
        read_ids(self.0, select(), ws, ids, row)
    }

    pub fn archived(
        &mut self,
        ws: &str,
        options: &Page,
    ) -> Result<(Vec<ItemView>, Option<String>)> {
        page(
            self.0,
            select().and_where(Expr::col(Alias::new("archived")).eq(true)),
            ws,
            options,
            "updated",
            row,
            |v| (v.updated.0, v.id.as_str()),
        )
    }
}

//! Item views. The list reads item facts in one statement, with no session or decision data.
use sea_query::{Alias, Expr, ExprTrait, Query, SelectStatement};
use serde::Deserialize;
use steve_types::{
    Closeout, ItemClosing, ItemParent, ItemSnooze, ItemView, ItemWorkflow, Millis, OperationBlocker,
};

use super::{Page, page, read_ids};
use crate::contract::{Json, Row};
use crate::{Parent, PlanStep, Read, Result, Snooze};

fn select() -> SelectStatement {
    let mut q = Query::select();
    q.from(Alias::new("items"))
        .columns([
            "id", "workspace_id", "no", "title", "status", "stage", "phase", "cycle", "revision",
            "outcome", "acceptance", "bot_id", "project_id", "parent", "priority", "star", "archived",
            "due", "snooze", "accept_mode", "design_gate", "source", "created", "updated", "closed_at",
            "closed_reason", "now", "plan", "closing", "cwd", "branch",
        ].map(Alias::new))
        .expr(Expr::cust("(SELECT text FROM item_goals WHERE item_id=items.id ORDER BY version DESC LIMIT 1)"))
        .expr(Expr::cust("(SELECT count(*) FROM item_goals WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT json_group_array(tag_id) FROM (SELECT tag_id FROM item_tags WHERE item_id=items.id ORDER BY rowid))"))
        .expr(Expr::cust("(SELECT json_group_array(dep_id) FROM (SELECT dep_id FROM item_deps WHERE item_id=items.id ORDER BY created,dep_id))"))
        .expr(Expr::cust("(SELECT paused FROM workflows WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT cycle FROM workflows WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT json_group_array(item_id) FROM (SELECT u.item_id FROM workflow_units u JOIN workflows w ON w.item_id=u.workflow_id WHERE u.workflow_id=items.id AND w.cycle=items.cycle AND items.phase<>'closed' AND u.status='failed' AND u.item_id IS NOT NULL ORDER BY u.position))"))
        .expr(Expr::cust("(SELECT attempt FROM workflow_units WHERE item_id=items.id LIMIT 1)"))
        .expr(Expr::cust("(SELECT max(attempt) FROM workflow_attempts WHERE item_id=items.id)"))
        .expr(Expr::cust("(SELECT evidence FROM item_operations WHERE item_id=items.id AND cycle=items.cycle AND revision=items.revision AND kind='release' AND json_extract(evidence,'$.pr_slug') IS NOT NULL AND status IN ('pending','succeeded') AND (items.phase='closing' OR (items.phase='closed' AND items.status='done')) ORDER BY created DESC,id DESC LIMIT 1)"))
        .expr(Expr::cust("(SELECT json_group_array(json_object('kind',kind,'status',status,'required',required,'revision',revision,'label',label,'tag',json_extract(evidence,'$.tag'))) FROM (SELECT * FROM item_operations WHERE item_id=items.id AND cycle=items.cycle AND status NOT IN ('succeeded','waived') ORDER BY created,id))"))
        // The detail observer refetches when its own facts change, even if the list looks the same.
        .expr(Expr::cust("json_array(
            (SELECT max(id) FROM timeline WHERE item_id=items.id),
            (SELECT json_array(count(*), max(rowid)) FROM artifacts WHERE item_id=items.id),
            (SELECT json_array(count(*), max(updated)) FROM annotations WHERE item_id=items.id),
            (SELECT json_array(revision, updated) FROM workflows WHERE item_id=items.id),
            (SELECT follow FROM thread_items WHERE item_id=items.id)
        )"))
        .and_where(Expr::col(Alias::new("kind")).eq("task"));
    q
}

#[derive(Deserialize)]
struct Closing {
    kind: String,
    reason: String,
    by: String,
    at: i64,
    frozen_revision: i64,
    accepted: bool,
    #[serde(default)]
    policy: String,
}

#[derive(Deserialize)]
struct Operation {
    kind: String,
    status: String,
    required: i64,
    revision: i64,
    label: String,
    tag: Option<String>,
}

fn apply_operations(v: &mut ItemView, ops: Vec<Operation>) {
    if v.phase == "closed" {
        return;
    }
    let mut waits = false;
    for op in ops {
        let build = op.kind == "release.build";
        let required = op.required != 0 && op.revision == v.revision;
        waits |= (!build && matches!(op.status.as_str(), "running" | "unknown"))
            || (required && v.closing.as_ref().is_some_and(|c| c.kind == "complete"));
        if !build && op.status == "running" {
            v.active_operations += 1;
        }
        let (kind, label) = match op.status.as_str() {
            "unknown" => ("unknown_effect", Some(op.label)),
            "failed" if required && v.closing.is_some() => ("failure", Some(op.label)),
            "pending" | "running"
                if required && v.closing.is_some() && (build || op.kind == "deploy") =>
            {
                ("operation", None)
            }
            _ => continue,
        };
        v.op_blockers.push(OperationBlocker {
            kind: kind.into(),
            op_kind: op.kind,
            label,
            tag: op.tag,
        });
    }
    if let Some(c) = &mut v.closing
        && waits
    {
        c.waiting.push("operation".into());
    }
}

fn row(r: &Row<'_>) -> Result<ItemView> {
    let phase: String = r.get(6)?;
    let parent = r
        .get::<Option<Json<Parent>>>(13)
        .ok()
        .flatten()
        .map(|Json(p)| ItemParent {
            item: p.item.flatten().map(Into::into),
            session: p.session.flatten().map(Into::into),
            at: p.at.flatten().map(Millis),
        });
    let snooze = r
        .get::<Option<Json<Snooze>>>(18)
        .ok()
        .flatten()
        .map(|Json(s)| ItemSnooze {
            label: s.label,
            until: Millis(s.until),
        });
    let plan = r
        .get::<Json<Vec<PlanStep>>>(27)
        .map(|p| p.0)
        .unwrap_or_default();
    let intent = r
        .get::<Option<Json<Closing>>>(28)
        .ok()
        .flatten()
        .map(|p| p.0)
        .filter(|_| phase == "closing");
    let paused: Option<bool> = r.get(35)?;
    let unit: Option<i64> = r.get(38)?;
    let attempt = unit.or(r.get(39)?);
    let workflow = if paused.is_some() || attempt.is_some() {
        Some(ItemWorkflow {
            paused,
            cycle: r.get(36)?,
            failed: r.get::<Json<Vec<_>>>(37)?.0,
            attempt,
            superseded: unit.is_none() && attempt.is_some(),
        })
    } else {
        None
    };
    let closeout = r
        .get::<Option<Json<serde_json::Value>>>(40)
        .ok()
        .flatten()
        .and_then(|Json(ev)| Closeout::from_evidence(&ev, phase == "closed", false));
    let acceptance = intent
        .as_ref()
        .filter(|c| !c.policy.is_empty())
        .map(|c| c.policy.clone())
        .or(r.get(10)?);
    let closed_reason = intent.as_ref().map(|c| c.reason.clone()).or(r.get(25)?);
    let mut v = ItemView {
        id: r.get::<String>(0)?.into(),
        ws: r.get::<String>(1)?.into(),
        no: r.get(2)?,
        title: r.get(3)?,
        status: r.get(4)?,
        stage: r.get(5)?,
        phase,
        cycle: r.get(7)?,
        revision: r.get(8)?,
        outcome: r.get(9)?,
        acceptance,
        bot_id: r.get::<Option<String>>(11)?.map(Into::into),
        project_id: r.get::<Option<String>>(12)?.map(Into::into),
        parent,
        priority: r.get(14)?,
        star: r.get(15)?,
        archived: r.get(16)?,
        due: r.get(17)?,
        snooze,
        accept_mode: r.get(19)?,
        design_gate: r.get(20)?,
        source: r
            .get::<Json<_>>(21)
            .map(|s| s.0)
            .unwrap_or_else(|_| crate::types::default_source()),
        created: Millis(r.get(22)?),
        updated: Millis(r.get(23)?),
        closed_at: r.get::<Option<i64>>(24)?.map(Millis),
        closed_reason,
        now: r.get(26)?,
        plan_done: plan.iter().filter(|p| p.done).count() as u32,
        plan_total: plan.len() as u32,
        goal: r.get::<Option<String>>(31)?.unwrap_or_default(),
        goals: r.get::<i64>(32)? as u32,
        tags: r.get::<Json<Vec<_>>>(33)?.0,
        deps: r.get::<Json<Vec<_>>>(34)?.0,
        workflow,
        closeout,
        closing: intent.map(|c| ItemClosing {
            kind: c.kind,
            reason: c.reason,
            requested_by: c.by,
            requested_at: Millis(c.at),
            frozen_revision: c.frozen_revision,
            accepted: c.accepted,
            waiting: Vec::new(),
        }),
        op_blockers: Vec::new(),
        active_operations: 0,
        detail_stamp: r.get(42)?,
        cwd: r.get(29)?,
        branch: r.get(30)?,
    };
    apply_operations(&mut v, r.get::<Json<Vec<_>>>(41)?.0);
    Ok(v)
}

pub struct ItemViews<'a, C>(pub(super) &'a mut C);

impl<C: Read> ItemViews<'_, C> {
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
            row,
            |v| (v.updated.0, v.id.as_str()),
        )
    }
}

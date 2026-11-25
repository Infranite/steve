//! Operations anchored to an item cycle and its acceptance revision.

use sea_query::{Condition, Expr, ExprTrait, Order, Query, SelectStatement};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::contract::{Q, Row, items};
use crate::repo::to_json;
use crate::{Read, Result, StoreError, W, now, uid};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemOperation {
    pub id: String,
    pub item_id: String,
    pub cycle: i64,
    pub revision: i64,
    pub kind: String,
    pub effect_id: Option<String>,
    pub required: bool,
    pub status: String,
    pub label: String,
    pub evidence: Value,
    pub created: i64,
    pub updated: i64,
    pub wait: steve_types::WaitContract,
}

const COLUMNS: &[&str] = &[
    "id",
    "item_id",
    "cycle",
    "revision",
    "kind",
    "effect_id",
    "required",
    "status",
    "label",
    "evidence",
    "created",
    "updated",
    "wait_owner",
    "wait_started_at",
    "wait_deadline",
    "wait_on_timeout",
];

fn query() -> SelectStatement {
    Query::select()
        .columns(COLUMNS.iter().copied())
        .from("item_operations")
        .take()
}

fn row(r: &Row<'_>) -> Result<ItemOperation> {
    let wait = crate::wait_repo::decode_contract(r.get(12)?, r.get(13)?, r.get(14)?, r.get(15)?)?;
    Ok(ItemOperation {
        id: r.get(0)?,
        item_id: r.get(1)?,
        cycle: r.get(2)?,
        revision: r.get(3)?,
        kind: r.get(4)?,
        effect_id: r.get(5)?,
        required: r.get::<i64>(6)? != 0,
        status: r.get(7)?,
        label: r.get(8)?,
        evidence: crate::types::json_or(r.get(9)?, Value::Object(Map::new())),
        created: r.get(10)?,
        updated: r.get(11)?,
        wait,
    })
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<ItemOperation>> {
    r.one(query().and_where(Expr::col("id").eq(id)), row)
}

pub fn for_cycle(r: &mut impl Read, item: &str, cycle: i64) -> Result<Vec<ItemOperation>> {
    r.all(
        query()
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("cycle").eq(cycle))
            .order_by("created", Order::Asc)
            .order_by("id", Order::Asc),
        row,
    )
}

pub fn with_status(r: &mut impl Read, kind: &str, status: &str) -> Result<Vec<ItemOperation>> {
    r.all(
        query()
            .and_where(Expr::col("kind").eq(kind))
            .and_where(Expr::col("status").eq(status))
            .order_by("created", Order::Asc)
            .order_by("id", Order::Asc),
        row,
    )
}

/// A repeated report of the same effect returns its existing operation.
#[allow(clippy::too_many_arguments)]
pub fn add(
    w: &mut W<'_>,
    item_id: &str,
    cycle: i64,
    revision: i64,
    kind: &str,
    effect_id: Option<&str>,
    required: bool,
    status: &str,
    label: &str,
    evidence: &Value,
) -> Result<(ItemOperation, bool)> {
    if let Some(effect) = effect_id
        && let Some(op) = w.one(
            query().cond_where(
                Condition::all()
                    .add(Expr::col("item_id").eq(item_id))
                    .add(Expr::col("kind").eq(kind))
                    .add(Expr::col("effect_id").eq(effect)),
            ),
            row,
        )?
    {
        return Ok((op, false));
    }
    let id = uid("op");
    let at = now();
    let ws = items::item_workspace(w, item_id)?.unwrap_or_else(|| crate::WS.to_string());
    let owner_name = if kind.trim().is_empty() {
        "operations"
    } else {
        kind
    };
    let wait = crate::wait_repo::contract_for(
        w,
        &ws,
        steve_types::WaitKind::ItemOperation,
        steve_types::WaitOwner::system(owner_name),
        at,
        steve_types::WaitTimeoutAction::EscalateToOwner,
    )?;
    let wait_owner = wait.owner.to_code();
    w.exec(
        Query::insert()
            .into_table("item_operations")
            .columns(COLUMNS.iter().copied())
            .values_panic([
                id.as_str().into(),
                item_id.into(),
                cycle.into(),
                revision.into(),
                kind.into(),
                effect_id.into(),
                i64::from(required).into(),
                status.into(),
                label.into(),
                to_json(evidence)?.into(),
                at.into(),
                at.into(),
                wait_owner.as_str().into(),
                wait.started_at.0.into(),
                wait.deadline.0.into(),
                wait.on_timeout.as_str().into(),
            ]),
    )?;
    emit_item(w, item_id)?;
    let row = get(w, &id)?.ok_or(StoreError::NotFound("没有这个操作"))?;
    Ok((row, true))
}

/// Sets status and merges evidence. Terminal operations keep their status and evidence.
pub fn set(w: &mut W<'_>, id: &str, status: &str, evidence: &Value) -> Result<bool> {
    let Some(op) = get(w, id)? else {
        return Ok(false);
    };
    if matches!(op.status.as_str(), "succeeded" | "failed" | "waived") {
        return Ok(false);
    }
    let mut merged = op.evidence;
    if let (Some(merged), Some(evidence)) = (merged.as_object_mut(), evidence.as_object()) {
        merged.extend(evidence.clone());
    }
    w.exec(
        Query::update()
            .table("item_operations")
            .values([
                ("status", status.into()),
                ("evidence", to_json(&merged)?.into()),
                ("updated", now().into()),
            ])
            .and_where(Expr::col("id").eq(id)),
    )?;
    emit_item(w, &op.item_id)?;
    Ok(true)
}

fn emit_item(w: &mut W<'_>, item: &str) -> Result<()> {
    if let Some(ws) = items::item_workspace(w, item)? {
        w.emit(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Item {
                ws: ws.into(),
                id: item.into(),
            },
        ));
    }
    Ok(())
}

//! Unified durable wait-contract storage, named settings lookup, and due timeout transitions.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query};
use serde_json::{Value, json};
use steve_types::{
    DecisionPrompt, DecisionStatus, DomainEvent, ItemPhase, Millis, WaitContract, WaitKind,
    WaitLimits, WaitOwner, WaitTimeoutAction, WorkEvent, wait,
};

use super::decisions::DecisionChange;
use super::{Q, Row};
use crate::{DecisionRows, DecisionSource, NewDecision, Read, Result, StoreError, W};

/// Maximum wait rows processed per category per due sweep.
pub const WAIT_SWEEP_BATCH: u64 = 64;

/// A due open decision ready for expiration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueDecision {
    pub id: String,
    pub workspace_id: String,
    pub subject_type: String,
    pub kind: String,
}

/// Read the workspace's configured wait upper bound (in milliseconds) for `kind`, clamped to its
/// documented `[min, max]` range and falling back to the named default when unset or invalid.
pub fn limit_ms(r: &mut impl Read, ws: &str, kind: WaitKind) -> Result<i64> {
    let raw = r
        .one(
            Query::select()
                .column("value")
                .from("settings")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("key").eq(kind.setting_key())),
            |row| row.get::<Option<String>>(0),
        )?
        .flatten();
    let parsed = raw.as_deref().and_then(parse_setting_ms);
    Ok(match parsed {
        Some(ms) => kind.clamp_limit_ms(ms),
        None => kind.default_limit_ms(),
    })
}

fn parse_setting_ms(raw: &str) -> Option<i64> {
    let v: Value = serde_json::from_str(raw).ok()?;
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Resolve all named wait upper bounds for a workspace.
pub fn limits(r: &mut impl Read, ws: &str) -> Result<WaitLimits> {
    Ok(WaitLimits {
        dependency_ms: limit_ms(r, ws, WaitKind::Dependency)?,
        decision_ms: limit_ms(r, ws, WaitKind::Decision)?,
        channel_draft_ms: limit_ms(r, ws, WaitKind::ChannelDraft)?,
        item_operation_ms: limit_ms(r, ws, WaitKind::ItemOperation)?,
        external_effect_ms: limit_ms(r, ws, WaitKind::ExternalEffect)?,
        mail_draft_ms: limit_ms(r, ws, WaitKind::MailDraft)?,
        flow_wait_ms: limit_ms(r, ws, WaitKind::FlowWait)?,
    })
}

pub fn validate_contract(contract: &WaitContract) -> Result<()> {
    contract
        .validate()
        .map_err(|err| StoreError::Rejected(err.into()))
}

pub fn contract_for(
    r: &mut impl Read,
    ws: &str,
    kind: WaitKind,
    owner: WaitOwner,
    started_at: i64,
    on_timeout: WaitTimeoutAction,
) -> Result<WaitContract> {
    let limit = limit_ms(r, ws, kind)?;
    WaitContract::bounded(owner, started_at, limit, on_timeout)
        .map_err(|err| StoreError::Rejected(err.into()))
}

pub(crate) fn decode_contract(
    owner: String,
    started_at: i64,
    deadline: i64,
    on_timeout: String,
) -> Result<WaitContract> {
    let owner = WaitOwner::parse(&owner)
        .map_err(|err| StoreError::Storage(format!("invalid wait_owner '{owner}': {err}")))?;
    let on_timeout = on_timeout
        .parse::<WaitTimeoutAction>()
        .map_err(|_| StoreError::Storage(format!("invalid wait_on_timeout '{on_timeout}'")))?;
    WaitContract::new(owner, Millis(started_at), Millis(deadline), on_timeout)
        .map_err(|err| StoreError::Storage(format!("invalid wait contract: {err}")))
}

pub(crate) fn decode_contract_rusqlite(r: &rusqlite::Row<'_>) -> rusqlite::Result<WaitContract> {
    let owner: String = r.get("wait_owner")?;
    let started_at: i64 = r.get("wait_started_at")?;
    let deadline: i64 = r.get("wait_deadline")?;
    let on_timeout: String = r.get("wait_on_timeout")?;
    let owner = WaitOwner::parse(&owner).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            r.as_ref().column_index("wait_owner").unwrap_or(0),
            rusqlite::types::Type::Text,
            err.into(),
        )
    })?;
    let on_timeout = on_timeout.parse::<WaitTimeoutAction>().map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            r.as_ref().column_index("wait_on_timeout").unwrap_or(0),
            rusqlite::types::Type::Text,
            Box::new(err),
        )
    })?;
    WaitContract::new(owner, Millis(started_at), Millis(deadline), on_timeout).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            r.as_ref().column_index("wait_deadline").unwrap_or(0),
            rusqlite::types::Type::Integer,
            err.into(),
        )
    })
}

/// Resolve the enforced [`WaitContract`] for a new decision row.
pub fn default_decision_contract(
    r: &mut impl Read,
    d: &NewDecision,
    created: i64,
) -> Result<WaitContract> {
    if let Some(explicit) = &d.wait {
        validate_contract(explicit)?;
        return Ok(explicit.clone());
    }
    let limit = match (d.subject_type.as_str(), d.kind.as_str()) {
        ("mem", _) => wait::WAIT_LIMIT_MEMORY_CANDIDATE_MS,
        ("secret_use", _) => wait::WAIT_LIMIT_SECRET_USE_MS,
        ("chan_message", "approve") => limit_ms(r, &d.workspace_id, WaitKind::ChannelDraft)?,
        ("mail_draft", _) => limit_ms(r, &d.workspace_id, WaitKind::MailDraft)?,
        ("flow_step", _) => limit_ms(r, &d.workspace_id, WaitKind::FlowWait)?,
        _ => limit_ms(r, &d.workspace_id, WaitKind::Decision)?,
    };
    WaitContract::bounded(WaitOwner::Owner, created, limit, WaitTimeoutAction::Close)
        .map_err(|err| StoreError::Rejected(err.into()))
}

/// Resolve the enforced [`WaitContract`] for a mail draft row.
pub fn default_mail_draft_contract(
    r: &mut impl Read,
    ws: &str,
    status: &str,
    started_at: i64,
) -> Result<WaitContract> {
    let (owner, action) = if status == "unknown" {
        (
            WaitOwner::system("mail"),
            WaitTimeoutAction::EscalateToOwner,
        )
    } else {
        (WaitOwner::Owner, WaitTimeoutAction::Close)
    };
    contract_for(r, ws, WaitKind::MailDraft, owner, started_at, action)
}

/// Resolve the enforced [`WaitContract`] for a `flow_waits` row (guaranteeing a finite deadline).
pub fn default_flow_wait_contract(
    r: &mut impl Read,
    ws: &str,
    mode: &str,
    created: i64,
    explicit_until: Option<i64>,
) -> Result<WaitContract> {
    let owner = if mode == "decision" {
        WaitOwner::Owner
    } else {
        WaitOwner::system("flow")
    };
    let deadline = match explicit_until {
        Some(at) => at,
        None => {
            let limit = limit_ms(r, ws, WaitKind::FlowWait)?;
            created
                .checked_add(limit)
                .ok_or_else(|| StoreError::Rejected("flow wait deadline overflow".into()))?
        }
    };
    let started_at = std::cmp::min(created, deadline);
    WaitContract::new(
        owner,
        Millis(started_at),
        Millis(deadline),
        WaitTimeoutAction::Close,
    )
    .map_err(|err| StoreError::Rejected(err.into()))
}

/// Earliest due deadline across the unified wait tables served by the Hub `decisions.expire` loop.
/// (`flow_waits` has its own dedicated `DueSource` in `steve-flow`.)
pub fn next_due(r: &mut impl Read) -> Result<Option<i64>> {
    let decisions: Option<i64> = r
        .one(
            Query::select()
                .expr(Func::min(Expr::col("wait_deadline")))
                .from("decisions")
                .and_where(Expr::col("status").eq("open")),
            |row| row.get(0),
        )?
        .flatten();
    let deps: Option<i64> = r
        .one(
            Query::select()
                .expr(Func::min(Expr::col(("w", "wait_deadline"))))
                .from_as("item_dependency_waits", "w")
                .join_as(
                    JoinType::InnerJoin,
                    "items",
                    "i",
                    Expr::col(("i", "id"))
                        .equals(("w", "item_id"))
                        .and(Expr::col(("i", "cycle")).equals(("w", "cycle"))),
                )
                .and_where(Expr::col(("w", "state")).eq("waiting"))
                .and_where(Expr::col(("i", "archived")).eq(0_i64))
                .and_where(Expr::col(("i", "phase")).is_in(["ready", "active"])),
            |row| row.get(0),
        )?
        .flatten();
    let ops: Option<i64> = r
        .one(
            Query::select()
                .expr(Func::min(Expr::col("wait_deadline")))
                .from("item_operations")
                .and_where(Expr::col("status").is_in(["pending", "running", "unknown"]))
                .and_where(Expr::col("wait_escalated_at").is_null()),
            |row| row.get(0),
        )?
        .flatten();
    let gh: Option<i64> = r
        .one(
            Query::select()
                .expr(Func::min(Expr::col("wait_deadline")))
                .from("gh_effects")
                .and_where(Expr::col("state").is_in(["pending", "running", "unknown"]))
                .and_where(Expr::col("wait_escalated_at").is_null()),
            |row| row.get(0),
        )?
        .flatten();
    let mail: Option<i64> = r
        .one(
            Query::select()
                .expr(Func::min(Expr::col("wait_deadline")))
                .from("mail_drafts")
                .and_where(Expr::col("status").is_in(["draft", "pending", "unknown"]))
                .and_where(Expr::col("wait_escalated_at").is_null()),
            |row| row.get(0),
        )?
        .flatten();
    Ok([decisions, deps, ops, gh, mail].into_iter().flatten().min())
}

/// Open decisions whose wait deadline has been reached, oldest deadline first.
pub fn due_decisions(r: &mut impl Read, now: i64, limit: u64) -> Result<Vec<DueDecision>> {
    r.all(
        Query::select()
            .columns(["id", "workspace_id", "subject_type", "kind"])
            .from("decisions")
            .and_where(Expr::col("status").eq("open"))
            .and_where(Expr::col("wait_deadline").lte(now))
            .order_by("wait_deadline", Order::Asc)
            .order_by("id", Order::Asc)
            .limit(limit),
        |row| {
            Ok(DueDecision {
                id: row.get(0)?,
                workspace_id: row.get(1)?,
                subject_type: row.get(2)?,
                kind: row.get(3)?,
            })
        },
    )
}

/// Expire a single non-module decision whose `WaitContract` reached its deadline.
/// If an item was in `closing` waiting on this decision, return the item to `active` so it is
/// never stranded in `closing`.
pub fn expire_core_decision(w: &mut W<'_>, id: &str, now: i64) -> Result<bool> {
    let Some(d) = DecisionRows(w).get(id)? else {
        return Ok(false);
    };
    if d.status != DecisionStatus::Open || !d.wait.is_due(now) {
        return Ok(false);
    }
    let changed = DecisionRows(w).apply(
        &d,
        DecisionChange::Expire,
        &json!({ "expired": true, "reason": "wait_timeout" }),
        None,
    )?;
    if !changed {
        return Ok(false);
    }
    if let Some(item_id) = d.item_id.as_deref()
        && let Some((phase, closing)) = w.one(
            Query::select()
                .columns(["phase", "closing"])
                .from("items")
                .and_where(Expr::col("id").eq(item_id)),
            |row| Ok((row.get::<String>(0)?, row.get::<Option<String>>(1)?)),
        )?
        && phase == ItemPhase::Closing.as_str()
        && closing
            .as_deref()
            .and_then(|raw| serde_json::from_str::<steve_types::CloseRequest>(raw).ok())
            .is_some_and(|req| req.decision.as_deref() == Some(d.id.as_str()))
    {
        w.exec(
            Query::update()
                .table("items")
                .value("phase", ItemPhase::Active.as_str())
                .value("closing", Option::<String>::None)
                .value("updated", now)
                .and_where(Expr::col("id").eq(item_id))
                .and_where(Expr::col("phase").eq(ItemPhase::Closing.as_str())),
        )?;
        w.emit(DomainEvent::from(WorkEvent::Item {
            ws: d.workspace_id.clone().into(),
            id: item_id.into(),
        }));
    }
    w.audit(
        &d.workspace_id,
        "steve",
        "wait.timeout",
        &d.id,
        &json!({
            "kind": WaitKind::Decision.as_str(),
            "subject_type": d.subject_type,
            "action": d.wait.on_timeout.as_str(),
        }),
    )?;
    Ok(true)
}

/// Process overdue prerequisite waits (`item_dependency_waits`): transition `waiting -> timed_out`
/// and escalate to the workspace owner (when configured) with a typed `DecisionPrompt::WaitTimeout`.
pub fn expire_dependency_waits(w: &mut W<'_>, now: i64, limit: u64) -> Result<usize> {
    let rows = w.all(
        Query::select()
            .columns([
                ("w", "item_id"),
                ("i", "workspace_id"),
                ("w", "cycle"),
                ("w", "session_id"),
                ("w", "wait_on_timeout"),
                ("w", "wait_deadline"),
            ])
            .from_as("item_dependency_waits", "w")
            .join_as(
                JoinType::InnerJoin,
                "items",
                "i",
                Expr::col(("i", "id"))
                    .equals(("w", "item_id"))
                    .and(Expr::col(("i", "cycle")).equals(("w", "cycle"))),
            )
            .and_where(Expr::col(("w", "state")).eq("waiting"))
            .and_where(Expr::col(("w", "wait_deadline")).lte(now))
            .and_where(Expr::col(("i", "archived")).eq(0_i64))
            .and_where(Expr::col(("i", "phase")).is_in(["ready", "active"]))
            .order_by(("w", "wait_deadline"), Order::Asc)
            .order_by(("w", "item_id"), Order::Asc)
            .limit(limit),
        |row: &Row<'_>| {
            Ok((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<i64>(2)?,
                row.get::<Option<String>>(3)?,
                row.get::<String>(4)?,
                row.get::<i64>(5)?,
            ))
        },
    )?;
    let mut count = 0;
    for (item_id, ws, cycle, session_id, on_timeout, deadline) in rows {
        let updated = w.exec(
            Query::update()
                .table("item_dependency_waits")
                .value("state", "timed_out")
                .value("updated", now)
                .and_where(Expr::col("item_id").eq(item_id.as_str()))
                .and_where(Expr::col("cycle").eq(cycle))
                .and_where(Expr::col("state").eq("waiting")),
        )?;
        if updated == 0 {
            continue;
        }
        let action = on_timeout
            .parse::<WaitTimeoutAction>()
            .unwrap_or(WaitTimeoutAction::EscalateToOwner);
        if action == WaitTimeoutAction::EscalateToOwner {
            DecisionRows(w).open(&NewDecision {
                workspace_id: ws.clone(),
                kind: "fail".into(),
                subject_type: "item".into(),
                subject_id: item_id.clone(),
                item_id: Some(item_id.clone()),
                session_id,
                question: String::new(),
                question_template: Some(DecisionPrompt::WaitTimeout {
                    wait_kind: WaitKind::Dependency,
                    subject: item_id.clone(),
                }),
                draft: None,
                options: Some(vec!["continue".into(), "drop".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: format!("wait_timeout:dep:{cycle}:{deadline}"),
                }),
                wait: None,
            })?;
        }
        w.audit(
            &ws,
            "steve",
            "wait.timeout",
            &item_id,
            &json!({
                "kind": WaitKind::Dependency.as_str(),
                "cycle": cycle,
                "action": action.as_str(),
            }),
        )?;
        w.emit(DomainEvent::from(WorkEvent::Item {
            ws: ws.into(),
            id: item_id.into(),
        }));
        count += 1;
    }
    Ok(count)
}

/// Process overdue external item operations (`item_operations`): waive historical pending rows on
/// closed items, transition open overdue rows out of silent waiting (`pending -> failed`,
/// `running -> unknown`), and escalate to the owner.
pub fn expire_item_operations(w: &mut W<'_>, now: i64, limit: u64) -> Result<usize> {
    let rows = w.all(
        Query::select()
            .columns([
                ("o", "id"),
                ("o", "item_id"),
                ("i", "workspace_id"),
                ("i", "phase"),
                ("o", "kind"),
                ("o", "status"),
                ("o", "wait_on_timeout"),
                ("o", "wait_deadline"),
            ])
            .from_as("item_operations", "o")
            .join_as(
                JoinType::InnerJoin,
                "items",
                "i",
                Expr::col(("i", "id")).equals(("o", "item_id")),
            )
            .and_where(Expr::col(("o", "status")).is_in(["pending", "running", "unknown"]))
            .and_where(Expr::col(("o", "wait_escalated_at")).is_null())
            .and_where(Expr::col(("o", "wait_deadline")).lte(now))
            .order_by(("o", "wait_deadline"), Order::Asc)
            .order_by(("o", "id"), Order::Asc)
            .limit(limit),
        |row: &Row<'_>| {
            Ok((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
                row.get::<String>(6)?,
                row.get::<i64>(7)?,
            ))
        },
    )?;
    let mut count = 0;
    for (op_id, item_id, ws, phase, kind, status, on_timeout, deadline) in rows {
        let closed = phase == ItemPhase::Closed.as_str();
        let next_status = if closed && status == "pending" {
            "waived"
        } else if status == "pending" {
            "failed"
        } else {
            "unknown"
        };
        let mut update = Query::update();
        update
            .table("item_operations")
            .value("status", next_status)
            .value("wait_escalated_at", now)
            .value("updated", now)
            .and_where(Expr::col("id").eq(op_id.as_str()))
            .and_where(Expr::col("status").eq(status.as_str()))
            .and_where(Expr::col("wait_escalated_at").is_null());
        if closed && status == "pending" {
            update.value("required", 0_i64);
        }
        if w.exec(&update)? == 0 {
            continue;
        }
        let action = on_timeout
            .parse::<WaitTimeoutAction>()
            .unwrap_or(WaitTimeoutAction::EscalateToOwner);
        if !closed && action == WaitTimeoutAction::EscalateToOwner {
            DecisionRows(w).open(&NewDecision {
                workspace_id: ws.clone(),
                kind: "fail".into(),
                subject_type: "item".into(),
                subject_id: item_id.clone(),
                item_id: Some(item_id.clone()),
                session_id: None,
                question: String::new(),
                question_template: Some(DecisionPrompt::WaitTimeout {
                    wait_kind: WaitKind::ItemOperation,
                    subject: kind.clone(),
                }),
                draft: None,
                options: Some(vec!["continue".into(), "drop".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: format!("wait_timeout:op:{op_id}:{deadline}"),
                }),
                wait: None,
            })?;
        }
        w.audit(
            &ws,
            "steve",
            "wait.timeout",
            &op_id,
            &json!({
                "kind": WaitKind::ItemOperation.as_str(),
                "item_id": item_id,
                "op_kind": kind,
                "next_status": next_status,
                "action": if closed { WaitTimeoutAction::Close.as_str() } else { action.as_str() },
            }),
        )?;
        w.emit(DomainEvent::from(WorkEvent::Item {
            ws: ws.into(),
            id: item_id.into(),
        }));
        count += 1;
    }
    Ok(count)
}

/// Process overdue external GitHub write effects (`gh_effects`): stop silent retries (`next_at = NULL`),
/// mark escalated, and surface an owner escalation card.
pub fn expire_gh_effects(w: &mut W<'_>, now: i64, limit: u64) -> Result<usize> {
    let rows = w.all(
        Query::select()
            .columns([
                "id",
                "workspace_id",
                "decision_id",
                "kind",
                "state",
                "wait_on_timeout",
                "wait_deadline",
            ])
            .from("gh_effects")
            .and_where(Expr::col("state").is_in(["pending", "running", "unknown"]))
            .and_where(Expr::col("wait_escalated_at").is_null())
            .and_where(Expr::col("wait_deadline").lte(now))
            .order_by("wait_deadline", Order::Asc)
            .order_by("id", Order::Asc)
            .limit(limit),
        |row: &Row<'_>| {
            Ok((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
                row.get::<i64>(6)?,
            ))
        },
    )?;
    let mut count = 0;
    for (id, ws, decision_id, kind, state, on_timeout, deadline) in rows {
        let next_state = if state == "pending" {
            "failed"
        } else {
            "unknown"
        };
        let updated = w.exec(
            Query::update()
                .table("gh_effects")
                .value("state", next_state)
                .value("next_at", Option::<i64>::None)
                .value("wait_escalated_at", now)
                .value("updated", now)
                .and_where(Expr::col("id").eq(id.as_str()))
                .and_where(Expr::col("state").eq(state.as_str()))
                .and_where(Expr::col("wait_escalated_at").is_null()),
        )?;
        if updated == 0 {
            continue;
        }
        let action = on_timeout
            .parse::<WaitTimeoutAction>()
            .unwrap_or(WaitTimeoutAction::EscalateToOwner);
        let item_id = super::decisions::get(w, &decision_id)?.and_then(|d| d.item_id);
        if action == WaitTimeoutAction::EscalateToOwner {
            DecisionRows(w).open(&NewDecision {
                workspace_id: ws.clone(),
                kind: "fail".into(),
                subject_type: "gh_effect".into(),
                subject_id: id.clone(),
                item_id,
                session_id: None,
                question: String::new(),
                question_template: Some(DecisionPrompt::WaitTimeout {
                    wait_kind: WaitKind::ExternalEffect,
                    subject: format!("{kind}:{decision_id}"),
                }),
                draft: None,
                options: Some(vec!["dismiss".into()]),
                requested_by: "steve".into(),
                source: Some(DecisionSource::Revision {
                    revision: format!("wait_timeout:gh:{id}:{deadline}"),
                }),
                wait: None,
            })?;
        }
        w.audit(
            &ws,
            "steve",
            "wait.timeout",
            &id,
            &json!({
                "kind": WaitKind::ExternalEffect.as_str(),
                "effect_kind": kind,
                "next_state": next_state,
                "action": action.as_str(),
            }),
        )?;
        count += 1;
    }
    Ok(count)
}

/// Process overdue mail drafts (`mail_drafts`): discard expired `draft`/`pending` rows cleanly and
/// ensure `unknown` sends escalate to an owner reconciliation card.
pub fn expire_mail_drafts(w: &mut W<'_>, now: i64, limit: u64) -> Result<usize> {
    let rows = w.all(
        Query::select()
            .columns([
                "id",
                "workspace_id",
                "status",
                "subject",
                "item_id",
                "session_id",
                "wait_on_timeout",
                "wait_deadline",
            ])
            .from("mail_drafts")
            .and_where(Expr::col("status").is_in(["draft", "pending", "unknown"]))
            .and_where(Expr::col("wait_escalated_at").is_null())
            .and_where(Expr::col("wait_deadline").lte(now))
            .order_by("wait_deadline", Order::Asc)
            .order_by("id", Order::Asc)
            .limit(limit),
        |row: &Row<'_>| {
            Ok((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
                row.get::<String>(3)?,
                row.get::<Option<String>>(4)?,
                row.get::<Option<String>>(5)?,
                row.get::<String>(6)?,
                row.get::<i64>(7)?,
            ))
        },
    )?;
    let mut count = 0;
    for (id, ws, status, subject, item_id, session_id, on_timeout, deadline) in rows {
        let action = on_timeout
            .parse::<WaitTimeoutAction>()
            .unwrap_or(WaitTimeoutAction::Close);
        if status == "unknown" || action == WaitTimeoutAction::EscalateToOwner {
            let updated = w.exec(
                Query::update()
                    .table("mail_drafts")
                    .value("wait_escalated_at", now)
                    .value("updated", now)
                    .and_where(Expr::col("id").eq(id.as_str()))
                    .and_where(Expr::col("status").eq(status.as_str()))
                    .and_where(Expr::col("wait_escalated_at").is_null()),
            )?;
            if updated == 0 {
                continue;
            }
            if super::decisions::open_draft_decisions(w, &ws, &id)?.is_empty() {
                let d = DecisionRows(w).open(&NewDecision {
                    workspace_id: ws.clone(),
                    kind: "fail".into(),
                    subject_type: "mail_draft".into(),
                    subject_id: id.clone(),
                    item_id,
                    session_id,
                    question: String::new(),
                    question_template: Some(DecisionPrompt::WaitTimeout {
                        wait_kind: WaitKind::MailDraft,
                        subject,
                    }),
                    draft: None,
                    options: Some(vec!["confirmed_not_sent".into(), "discard".into()]),
                    requested_by: "steve".into(),
                    source: Some(DecisionSource::Revision {
                        revision: format!("wait_timeout:mail:{id}:{deadline}"),
                    }),
                    wait: None,
                })?;
                w.exec(
                    Query::update()
                        .table("mail_drafts")
                        .value("decision_id", d.id.as_str())
                        .and_where(Expr::col("id").eq(id.as_str())),
                )?;
            }
            w.audit(
                &ws,
                "steve",
                "wait.timeout",
                &id,
                &json!({
                    "kind": WaitKind::MailDraft.as_str(),
                    "status": status,
                    "action": WaitTimeoutAction::EscalateToOwner.as_str(),
                }),
            )?;
        } else {
            let updated = w.exec(
                Query::update()
                    .table("mail_drafts")
                    .value("status", "discarded")
                    .value("wait_escalated_at", now)
                    .value("updated", now)
                    .and_where(Expr::col("id").eq(id.as_str()))
                    .and_where(Expr::col("status").is_in(["draft", "pending"]))
                    .and_where(Expr::col("wait_escalated_at").is_null()),
            )?;
            if updated == 0 {
                continue;
            }
            for dec_id in super::decisions::open_draft_decisions(w, &ws, &id)? {
                DecisionRows(w).settle(
                    &dec_id,
                    &json!({ "expired": true, "reason": "wait_timeout" }),
                    DecisionChange::Expire,
                    None,
                )?;
            }
            w.audit(
                &ws,
                "steve",
                "wait.timeout",
                &id,
                &json!({
                    "kind": WaitKind::MailDraft.as_str(),
                    "status": status,
                    "action": WaitTimeoutAction::Close.as_str(),
                }),
            )?;
        }
        count += 1;
    }
    Ok(count)
}

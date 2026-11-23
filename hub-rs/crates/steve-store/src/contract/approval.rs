//! Approval policy is a workspace-scoped CAS, independent of Bot and item settings.

use sea_query::{Expr, ExprTrait, OnConflict, Query};
use steve_types::{ApprovalMode, ApprovalPolicy, AutonomyEvent};

use crate::{Members, Read, Result, StoreError, W};

pub fn policy(r: &mut impl Read, ws: &str) -> Result<ApprovalPolicy> {
    Ok(r.one(
        Query::select()
            .columns(["mode", "revision", "actor"])
            .from("approval_policies")
            .and_where(Expr::col("workspace_id").eq(ws)),
        |r| {
            Ok(ApprovalPolicy {
                mode: match r.get::<String>(0)?.as_str() {
                    "ask_owner" => ApprovalMode::AskOwner,
                    "harness_auto" => ApprovalMode::HarnessAuto,
                    _ => return Err(StoreError::Conflict("invalid stored approval mode".into())),
                },
                revision: r.get(1)?,
                actor: Some(r.get::<String>(2)?.into()),
            })
        },
    )?
    .unwrap_or_default())
}

pub fn owner(r: &mut impl Read, ws: &str, actor: &str) -> Result<bool> {
    Ok(Members::new(r).role(ws, actor)?.as_deref() == Some("owner"))
}

pub fn set(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    expected: i64,
    mode: ApprovalMode,
) -> Result<ApprovalPolicy> {
    if !owner(w, ws, actor)? {
        return Err(StoreError::Rejected(
            "only the workspace owner may change approval policy".into(),
        ));
    }
    let before = policy(w, ws)?;
    if before.revision != expected {
        return Err(StoreError::Conflict(
            "approval policy changed; reload before updating".into(),
        ));
    }
    if before.mode == mode {
        return Ok(before);
    }
    let next = ApprovalPolicy {
        mode,
        revision: expected + 1,
        actor: Some(actor.into()),
    };
    let key = match mode {
        ApprovalMode::AskOwner => "ask_owner",
        ApprovalMode::HarnessAuto => "harness_auto",
    };
    w.exec(
        Query::insert()
            .into_table("approval_policies")
            .columns(["workspace_id", "mode", "revision", "actor"])
            .values_panic([ws.into(), key.into(), next.revision.into(), actor.into()])
            .on_conflict(
                OnConflict::column("workspace_id")
                    .update_columns(["mode", "revision", "actor"])
                    .to_owned(),
            ),
    )?;
    w.emit(AutonomyEvent::ApprovalPolicy {
        ws: ws.into(),
        policy: next.clone(),
    });
    w.audit(
        ws,
        actor,
        "approval_policy.set",
        ws,
        &serde_json::json!({"before": before, "after": next}),
    )?;
    Ok(next)
}

/// The same logical turn may have several attempts; count it once.
pub fn auto_turn(w: &mut W<'_>, session: &str, turn: &str, at: i64) -> Result<bool> {
    Ok(w.exec(
        Query::insert()
            .into_table("approval_turns")
            .columns(["session_id", "turn_id", "created"])
            .values_panic([session.into(), turn.into(), at.into()])
            .on_conflict(
                OnConflict::columns(["session_id", "turn_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )? == 1)
}

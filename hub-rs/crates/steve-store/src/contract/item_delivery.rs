use sea_query::{Expr, ExprTrait, OnConflict, Order, Query};
use steve_types::{
    DeliveryBaseline, DeliveryProof, DeliveryRef, DeliveryVersion, ItemId, WorkEvent,
};

use crate::{Read, Result, StoreError, W};

pub fn scope(c: &mut impl Read, item: &str) -> Result<i64> {
    Ok(c.one(
        Query::select()
            .column("version")
            .from("item_goals")
            .and_where(Expr::col("item_id").eq(item))
            .order_by("version", Order::Desc)
            .limit(1),
        |r| r.get(0),
    )?
    .unwrap_or_default())
}

pub fn open_version(c: &mut impl Read, ws: &str, item: &str) -> Result<Option<DeliveryVersion>> {
    let Some((cycle, revision)) = c.one(
        Query::select()
            .columns(["cycle", "revision"])
            .from("items")
            .and_where(Expr::col("id").eq(item))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("phase").ne("closed")),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    else {
        return Ok(None);
    };
    Ok(Some(DeliveryVersion {
        cycle,
        revision,
        scope: scope(c, item)?,
    }))
}

pub fn refs(c: &mut impl Read, item: &ItemId, cycle: i64, scope: i64) -> Result<Vec<DeliveryRef>> {
    c.all(
        Query::select()
            .columns(["repository", "number", "target", "head", "base", "required"])
            .from("item_delivery_refs")
            .and_where(Expr::col("item_id").eq(item.as_str()))
            .and_where(Expr::col("cycle").eq(cycle))
            .and_where(Expr::col("scope").eq(scope))
            .order_by("repository", Order::Asc)
            .order_by("number", Order::Asc),
        |r| {
            Ok(DeliveryRef {
                repository: r.get(0)?,
                number: r.get(1)?,
                target: r.get(2)?,
                head: r.get(3)?,
                base: r.get(4)?,
                required: r.get::<i64>(5)? != 0,
            })
        },
    )
}

/// The owner-selected PR ranges and the item state read before resolving them.
pub struct DeliveryDeclaration<'a> {
    pub item: &'a str,
    pub cycle: i64,
    pub expected_revision: i64,
    pub scope: i64,
    pub refs: &'a [DeliveryRef],
}

pub fn declare(w: &mut W<'_>, ws: &str, by: &str, pinned: &DeliveryDeclaration<'_>) -> Result<()> {
    let item = pinned.item;
    if scope(w, item)? != pinned.scope
        || w.exec(
            Query::update()
                .table("items")
                .value("revision", Expr::col("revision").add(1_i64))
                .value("updated", crate::now())
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(item))
                .and_where(Expr::col("cycle").eq(pinned.cycle))
                .and_where(Expr::col("cas_revision").eq(pinned.expected_revision))
                .and_where(Expr::col("phase").is_in(["ready", "active", "paused"])),
        )? != 1
    {
        return Err(StoreError::Conflict("item delivery scope changed".into()));
    }
    w.exec(
        Query::delete()
            .from_table("item_delivery_refs")
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("cycle").eq(pinned.cycle))
            .and_where(Expr::col("scope").eq(pinned.scope)),
    )?;
    for pr in pinned.refs {
        w.exec(
            Query::insert()
                .into_table("item_delivery_refs")
                .columns([
                    "item_id",
                    "cycle",
                    "scope",
                    "repository",
                    "number",
                    "target",
                    "head",
                    "base",
                    "required",
                ])
                .values_panic([
                    item.into(),
                    pinned.cycle.into(),
                    pinned.scope.into(),
                    pr.repository.as_str().into(),
                    pr.number.into(),
                    pr.target.as_str().into(),
                    pr.head.as_str().into(),
                    pr.base.as_deref().into(),
                    i64::from(pr.required).into(),
                ]),
        )?;
    }
    w.audit(
        ws,
        by,
        "item.declare_delivery",
        item,
        &serde_json::json!({
            "cycle": pinned.cycle, "scope": pinned.scope, "pull_requests": pinned.refs.len(),
        }),
    )?;
    w.emit(WorkEvent::Item {
        ws: ws.to_owned().into(),
        id: pinned.item.into(),
    });
    Ok(())
}

pub fn pending(c: &mut impl Read) -> Result<Vec<(String, DeliveryBaseline)>> {
    let items = c.all(
        Query::select().columns(["workspace_id", "id", "cycle", "revision"])
            .from("items")
            .and_where(Expr::col("phase").eq("closing"))
            .and_where(Expr::cust("json_extract(closing, '$.kind')='complete' AND json_extract(closing, '$.accepted')=1"))
            .and_where(Expr::cust("EXISTS (SELECT 1 FROM item_delivery_refs d WHERE d.item_id=items.id AND d.cycle=items.cycle AND d.required=1 AND d.scope=COALESCE((SELECT MAX(version) FROM item_goals WHERE item_id=items.id),0))"))
            .order_by("updated", Order::Asc),
        |r| Ok((r.get::<String>(0)?, r.get::<String>(1)?, r.get::<i64>(2)?, r.get::<i64>(3)?)),
    )?;
    items
        .into_iter()
        .map(|(ws, item, cycle, revision)| {
            let scope = scope(c, &item)?;
            let item = ItemId::from(item);
            let refs = refs(c, &item, cycle, scope)?
                .into_iter()
                .filter(|pr| pr.required)
                .collect();
            Ok((
                ws,
                DeliveryBaseline {
                    item,
                    cycle,
                    revision,
                    scope,
                    refs,
                },
            ))
        })
        .collect()
}

/// Persist only evidence for a complete, current set of required ranges. The event and the
/// evidence share the caller's transaction.
pub fn record_proof(w: &mut W<'_>, ws: &str, proof: &DeliveryProof) -> Result<bool> {
    let baseline = &proof.baseline;
    if scope(w, baseline.item.as_str())? != baseline.scope {
        return Ok(false);
    }
    let required = refs(w, &baseline.item, baseline.cycle, baseline.scope)?
        .into_iter()
        .filter(|pr| pr.required);
    if !required.eq(baseline.refs.iter().cloned())
        || baseline.refs.len() != proof.pulls.len()
        || baseline.refs.is_empty()
        || baseline
            .refs
            .iter()
            .zip(&proof.pulls)
            .any(|(pr, check)| pr.repository != check.repository || pr.number != check.number)
    {
        return Ok(false);
    }
    let mut changed = false;
    for pull in &proof.pulls {
        changed |= w.exec(
            Query::insert()
                .into_table("item_delivery_proofs")
                .columns([
                    "item_id",
                    "cycle",
                    "scope",
                    "revision",
                    "repository",
                    "number",
                    "target_head",
                ])
                .values_panic([
                    baseline.item.as_str().into(),
                    baseline.cycle.into(),
                    baseline.scope.into(),
                    baseline.revision.into(),
                    pull.repository.as_str().into(),
                    pull.number.into(),
                    pull.target_head.as_str().into(),
                ])
                .on_conflict(
                    OnConflict::columns([
                        "item_id",
                        "cycle",
                        "scope",
                        "revision",
                        "repository",
                        "number",
                        "target_head",
                    ])
                    .do_nothing()
                    .to_owned(),
                ),
        )? == 1;
    }
    if changed {
        w.emit(WorkEvent::Item {
            ws: ws.to_owned().into(),
            id: baseline.item.clone(),
        });
    }
    Ok(true)
}

pub fn unsettled_github_writes(c: &mut impl Read, item: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column(("gh_effects", "id"))
            .from("gh_effects")
            .inner_join(
                "decisions",
                Expr::col(("gh_effects", "decision_id")).equals(("decisions", "id")),
            )
            .and_where(Expr::col(("decisions", "item_id")).eq(item))
            .and_where(Expr::col(("gh_effects", "state")).is_in(["pending", "running", "unknown"])),
        |_| Ok(()),
    )?
    .is_some())
}

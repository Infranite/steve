//! Prerequisite facts and obsolescence of decisions after a conversation advances.

use crate::error::Result;
use crate::repo::Db as Store;
use serde_json::json;

/// Prerequisites that are not done yet.
pub fn deps_open(db: &Store, item: &str) -> Result<Vec<String>> {
    let mut out = vec![];
    for d in db.deps_of(item)? {
        if db.item(&d)?.is_none_or(|i| {
            i.status != "done" || i.phase != "closed" || i.outcome.as_deref() != Some("completed")
        }) {
            out.push(d);
        }
    }
    Ok(out)
}

/// Questions and failure cards only matter until the conversation goes on without them: the user writes to
/// the session anyway, or the bot keeps working, asks again or delivers.
pub fn supersede(db: &Store, item: &str, kinds: &[&str], why: &str) -> Result<usize> {
    supersede_where(
        db,
        item,
        |d| {
            d.subject_type != steve_store::SESSION_START_FAILURE && kinds.contains(&d.kind.as_str())
        },
        why,
    )
}

/// A card saying the agent stopped (its turn ended without a delivery, it failed, it sat idle): it
/// gives way when there is news. A card about an effect nobody knows, or about a release, waits on you.
pub fn stop_card(d: &steve_store::Decision) -> bool {
    d.kind == "fail"
        && matches!(d.subject_type.as_str(), "item" | crate::stall::STALL)
        && !crate::cycle::unknown_effect_card(d)
        && !d.options.iter().any(|o| o.starts_with("release_"))
}

/// [`supersede`] for the open cards of `item` that `pick` picks.
pub fn supersede_where(
    db: &Store,
    item: &str,
    pick: impl Fn(&steve_store::Decision) -> bool,
    why: &str,
) -> Result<usize> {
    let xs: Vec<_> = db
        .decisions_of_item(item)?
        .into_iter()
        .filter(|d| d.status == "open" && pick(d))
        .collect();
    for d in &xs {
        db.decision_resolve(&d.id, json!({"superseded": why}), "expired", None)?;
    }
    if !xs.is_empty() {
        db.timeline_add(
            item,
            "steve",
            "sys",
            &format!("{} 件待处理已失效：{why}", xs.len()),
            None,
        )?;
    }
    Ok(xs.len())
}

/// Cards left from before these rules: a question or failure older than a delivery or approval request has been moved past.
pub fn tidy(db: &Store, item: &str) -> Result<usize> {
    let open: Vec<_> = db
        .decisions_of_item(item)?
        .into_iter()
        .filter(|d| d.status == "open")
        .collect();
    let later = open
        .iter()
        .filter(|d| d.kind == "accept" || d.kind == "approve")
        .map(|d| d.created)
        .max()
        .unwrap_or(0)
        .max(0);
    let stale: Vec<_> = open
        .iter()
        .filter(|d| {
            d.subject_type != steve_store::SESSION_START_FAILURE
                && (d.kind == "ask" || d.kind == "fail")
                && d.created < later
        })
        .collect();
    for d in &stale {
        db.decision_resolve(
            &d.id,
            json!({"superseded": "Bot 后来已经交付或请求批准"}),
            "expired",
            None,
        )?;
    }
    if !stale.is_empty() {
        db.timeline_add(
            item,
            "steve",
            "sys",
            &format!("{} 件待处理已失效：Bot 后来已经交付或请求批准", stale.len()),
            None,
        )?;
    }
    Ok(stale.len())
}

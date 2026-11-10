//! lifecycle.ts, first half: item status from its open decisions; decisions the conversation has moved past
//! expire instead of waiting forever.

use crate::error::Result;
use crate::patch;
use crate::repo::{Db as Store, ItemExt};
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

/// Recompute the user-facing status from open decisions and prerequisites.
pub fn settle(db: &Store, item: &str) -> Result<()> {
    let Some(i) = db.item(item)? else {
        return Ok(());
    };
    if i.is_settled() {
        return Ok(());
    }
    if db
        .store()
        .dependency_waits()
        .get(item)?
        .is_some_and(|w| w.state == "waiting")
    {
        return db.item_patch(item, patch![status: "block"]);
    }
    if db
        .decisions_of_item(item)?
        .iter()
        .any(|d| d.status == "open")
    {
        return db.item_patch(item, patch![status: "wait"]);
    }
    if i.status == "block" && !deps_open(db, item)?.is_empty() {
        return Ok(());
    }
    db.item_patch(item, patch![status: "run"])
}

/// Questions and failure cards only matter until the conversation goes on without them: the user writes to
/// the session anyway, or the bot keeps working, asks again or delivers.
pub fn supersede(db: &Store, item: &str, kinds: &[&str], why: &str) -> Result<usize> {
    supersede_where(db, item, |d| kinds.contains(&d.kind.as_str()), why)
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
        settle(db, item)?;
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
        .filter(|d| (d.kind == "ask" || d.kind == "fail") && d.created < later)
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
        settle(db, item)?;
    }
    Ok(stale.len())
}

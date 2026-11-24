//! Prerequisite facts and obsolescence of decisions after a conversation advances.

use crate::error::Result;
use crate::repo::Db as Store;
use serde_json::json;

/// Prerequisites that are not done yet.
pub fn deps_open(db: &Store, item: &str) -> Result<Vec<String>> {
    let mut out = vec![];
    for d in db.deps_of(item)? {
        if db.item(&d)?.is_none_or(|i| !i.is_completed()) {
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
    db.store().write(|w| {
        let mut count = 0;
        for decision in steve_store::decision_repo::open_item_decisions(w, item)? {
            if pick(&decision)
                && steve_store::DecisionRows(w).apply(
                    &decision,
                    steve_store::decision_repo::DecisionChange::Expire,
                    &json!({"superseded": why}),
                    &serde_json::Value::Null,
                )?
            {
                count += 1;
            }
        }
        if count > 0 {
            db.timeline_add(
                item,
                "steve",
                "sys",
                &format!("{count} 件待处理已失效：{why}"),
                None,
            )?;
        }
        Ok(count)
    })
}

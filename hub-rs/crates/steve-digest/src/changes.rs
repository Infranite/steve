//! 整理报告: every write a digest run makes is a `digest_changes` row of the run (the item or session) that
//! made it, with what undo needs, and can be undone from the page.
//!
//! Ops: `summary` (a conversation summary version), `segment` (a segment and its matter assignment; a matter it
//! opened is dropped again when nothing else is in it), `todo` (a todo written), `todo_state` (a todo's state
//! changed), `mem_link` (a memory indexed to a conversation, matter, people and message range), `mem_new` (a
//! candidate memory proposed, also one that would replace another; undone, it goes the way an unanswered
//! candidate goes).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::base::mems::{self, MemPatch};
use steve_store::{StoreError, Tx, uid};

use crate::conv::ConvRef;
use crate::{matter, memindex, summary, todo};

type Result<T> = std::result::Result<T, StoreError>;

#[allow(clippy::too_many_arguments)]
pub fn record(
    tx: &Tx,
    ws: &str,
    run: &str,
    writer: &str,
    op: &str,
    target: &str,
    packet: Option<&str>,
    before: &Value,
    after: &Value,
    now: i64,
) -> Result<String> {
    let id = uid("dch");
    tx.execute(
        "INSERT INTO digest_changes (id, workspace_id, run_id, writer, op, target, packet_id, before, after, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![id, ws, run, writer, op, target, packet, before.to_string(), after.to_string(), now, now],
    )?;
    crate::event(tx, ws, &id);
    Ok(id)
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let j = |s: String| serde_json::from_str::<Value>(&s).unwrap_or(json!({}));
    Ok(json!({
        "id": r.get::<_, String>(0)?, "run": r.get::<_, String>(1)?, "writer": r.get::<_, String>(2)?,
        "op": r.get::<_, String>(3)?, "target": r.get::<_, String>(4)?, "packet": r.get::<_, Option<String>>(5)?,
        "before": j(r.get(6)?), "after": j(r.get(7)?), "state": r.get::<_, String>(8)?, "created": r.get::<_, i64>(9)?,
    }))
}

const COLS: &str = "id, run_id, writer, op, target, packet_id, before, after, state, created";

/// The newest changes of `ws`, of one run or packet when given.
pub fn list(
    c: &Connection,
    ws: &str,
    run: Option<&str>,
    packet: Option<&str>,
    limit: usize,
) -> Result<Vec<Value>> {
    Ok(c
        .prepare_cached(&format!(
            "SELECT {COLS} FROM digest_changes WHERE workspace_id = ?1 AND (?2 IS NULL OR run_id = ?2) AND (?3 IS NULL OR packet_id = ?3)
              ORDER BY created DESC, id DESC LIMIT {}",
            limit.clamp(1, 500)
        ))?
        .query_map(params![ws, run, packet], row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn get(c: &Connection, ws: &str, id: &str) -> Result<Option<Value>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM digest_changes WHERE workspace_id = ? AND id = ?"
    ))?
    .query_row(params![ws, id], row)
    .optional()?)
}

/// Undoes change `id` of `ws`. `ApiError::Rule` when it cannot be undone any more (a newer summary, a todo
/// changed since), `NotFound` when there is no such change.
pub fn undo(m: &Modules, ws: &str, actor: &str, id: &str) -> ApiResult<Value> {
    let now = m.clock.now();
    let s = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    m.store.transaction(|tx| {
        let ch = get(tx, ws, id)?
            .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.change")))?;
        if ch["state"] != "applied" {
            return Err(ApiError::rule(steve_i18n::t!(
                "digest.change.undone_already"
            )));
        }
        let after = ch["after"].clone();
        match ch["op"].as_str().unwrap_or_default() {
            "summary" => {
                let conv =
                    ConvRef::parse(&s(&after, "conv")).ok_or_else(|| ApiError::invalid("conv"))?;
                let v = after["version"].as_i64().unwrap_or(0);
                if !summary::remove_latest(tx, ws, &conv, v)? {
                    return Err(ApiError::rule(steve_i18n::t!(
                        "digest.change.summary_newer"
                    )));
                }
            }
            "segment" => {
                let seg = s(&after, "segment");
                let mat: Option<Option<String>> = tx
                    .query_row(
                        "SELECT matter_id FROM digest_segments WHERE workspace_id = ? AND id = ?",
                        params![ws, seg],
                        |r| r.get(0),
                    )
                    .optional()?;
                let mat = mat
                    .ok_or_else(|| ApiError::rule(steve_i18n::t!("digest.change.target_moved")))?;
                if after.get("matter").is_none() || mat.as_deref() != after["matter"].as_str() {
                    return Err(ApiError::rule(steve_i18n::t!("digest.change.target_moved")));
                }
                tx.execute(
                    "DELETE FROM digest_segments WHERE workspace_id = ? AND id = ?",
                    params![ws, seg],
                )?;
                if let Some(mt) = mat {
                    matter::recount(tx, &mt, now)?;
                    matter::reindex(tx, &mt)?;
                    let left: i64 = tx.query_row(
                        "SELECT count(*) FROM digest_segments WHERE matter_id = ?",
                        [&mt],
                        |r| r.get(0),
                    )?;
                    if after["opened"] == true && left == 0 {
                        tx.execute(
                            "UPDATE matters SET state = 'dropped', updated = ? WHERE id = ?",
                            params![now, mt],
                        )?;
                    }
                    crate::event(tx, ws, &mt);
                }
            }
            "todo" => {
                let t = s(&after, "todo");
                let cur = todo::get(tx, ws, &t)?
                    .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
                if cur["state"] != "open"
                    || after.get("updated").is_some_and(|u| u != &cur["updated"])
                {
                    return Err(ApiError::rule(steve_i18n::t!("digest.change.todo_moved")));
                }
                todo::delete(tx, ws, &t)?;
            }
            "todo_state" => {
                let t = s(&after, "todo");
                let cur = todo::current(tx, ws, &t)?
                    .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
                if cur.0 != s(&after, "state") || cur.1.as_deref() != after["item"].as_str() {
                    return Err(ApiError::rule(steve_i18n::t!("digest.change.todo_moved")));
                }
                let before = &ch["before"];
                let item = before.get("item").and_then(Value::as_str);
                todo::set_state(tx, ws, &t, &s(before, "state"), item, now)?;
            }
            "mem_link" => {
                let a: memindex::Added = serde_json::from_value(after.clone())
                    .map_err(|_| ApiError::invalid("after"))?;
                memindex::unlink(tx, ws, &a)?;
            }
            "mem_new" => {
                let id = s(&after, "mem");
                if let Some(mm) = mems::get(tx, &id)?
                    && mm.workspace_id == ws
                    && mm.status == "candidate"
                {
                    match mm.decision_id.clone() {
                        Some(d) => {
                            steve_modules::decisions::expire(m, &d)?;
                        }
                        None => {
                            mems::patch(
                                tx,
                                &id,
                                &MemPatch {
                                    status: Some("stale".into()),
                                    ..Default::default()
                                },
                            )?;
                        }
                    }
                } else {
                    return Err(ApiError::rule(steve_i18n::t!("digest.change.mem_answered")));
                }
            }
            _ => return Err(ApiError::invalid("op")),
        }
        tx.execute(
            "UPDATE digest_changes SET state = 'undone', updated = ? WHERE id = ?",
            params![now, id],
        )?;
        steve_store::base::audit(tx, ws, actor, "digest.undo", id, &json!({"op": ch["op"]}))?;
        crate::event(tx, ws, id);
        Ok::<_, ApiError>(())
    })?;
    Ok(json!({"ok": true, "id": id}))
}

//! 整理报告: every write a digest run makes is a `digest_changes` row of the run (the item or session) that
//! made it, with what undo needs, and can be undone from the page.
//!
//! Ops: `summary` (a conversation summary version), `segment` (a segment and its matter assignment; a matter it
//! opened is dropped again when nothing else is in it), `todo` (a todo written), `todo_state` (a todo's state
//! changed, with the next occurrence a recurring one made), `todo_edit` (a todo's fields changed: `before` the
//! old fields, `after` the new ones), `todo_link` (an item linked to or unlinked from a goal), `mem_link` (a memory indexed to a conversation, matter, people and message range), `mem_new` (a
//! candidate memory proposed, also one that would replace another; undone, it goes the way an unanswered
//! candidate goes).

use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::W;

use crate::conv::ConvRef;
use crate::{matter, todo};

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn undo_summary(w: &mut W<'_>, ws: &str, after: &Value) -> ApiResult<()> {
    let conv = ConvRef::parse(&text(after, "conv")).ok_or_else(|| ApiError::invalid("conv"))?;
    let v = after["version"].as_i64().unwrap_or(0);
    if !w.summaries().remove_latest(ws, &conv.key(), v)? {
        return Err(ApiError::rule(steve_i18n::t!(
            "digest.change.summary_newer"
        )));
    }
    Ok(())
}

fn undo_segment(w: &mut W<'_>, ws: &str, after: &Value, now: i64) -> ApiResult<()> {
    let seg = text(after, "segment");
    let mat = w.digest_segments().assignment(ws, &seg)?;
    let mat = mat.ok_or_else(|| ApiError::rule(steve_i18n::t!("digest.change.target_moved")))?;
    if after.get("matter").is_none() || mat.as_deref() != after["matter"].as_str() {
        return Err(ApiError::rule(steve_i18n::t!("digest.change.target_moved")));
    }
    w.digest_segments().remove(ws, &seg)?;
    if let Some(mt) = mat {
        matter::recount(w, &mt, now)?;
        matter::reindex(w, &mt)?;
        if after["opened"] == true && w.digest_segments().count(&mt)? == 0 {
            steve_store::digest::Matters(w).set_state(&mt, "dropped", now)?;
        }
        crate::emit(w, ws, &mt);
    }
    Ok(())
}

fn undo_created_todo(tx: &mut W<'_>, ws: &str, after: &Value) -> ApiResult<()> {
    let t = text(after, "todo");
    let cur = todo::get(tx, ws, &t)?
        .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
    if cur["state"] != "open"
        || after.get("updated").is_some_and(|u| u != &cur["updated"])
        || todo::parent_of_any(tx, &t)?
    {
        return Err(ApiError::rule(steve_i18n::t!("digest.change.todo_moved")));
    }
    todo::delete(tx, ws, &t)?;
    Ok(())
}

fn undo_todo_state(
    tx: &mut W<'_>,
    ws: &str,
    before: &Value,
    after: &Value,
    now: i64,
) -> ApiResult<()> {
    let t = text(after, "todo");
    let cur = todo::current(tx, ws, &t)?
        .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
    if cur.0 != text(after, "state") || cur.1.as_deref() != after["item"].as_str() {
        return Err(ApiError::rule(steve_i18n::t!("digest.change.todo_moved")));
    }
    // the next occurrence it made goes with it, while nobody touched that one
    if let Some(next) = after.get("next").and_then(Value::as_str)
        && let Some(n) = todo::get(tx, ws, next)?
    {
        if n["state"] != "open" || n["updated"] != after["next_updated"] {
            return Err(ApiError::rule(steve_i18n::t!("digest.change.todo_moved")));
        }
        todo::delete(tx, ws, next)?;
    }
    let item = before.get("item").and_then(Value::as_str);
    todo::set_state(tx, ws, &t, &text(before, "state"), item, now)?;
    Ok(())
}

fn undo_todo_edit(
    tx: &mut W<'_>,
    ws: &str,
    before: &Value,
    after: &Value,
    now: i64,
) -> ApiResult<()> {
    let t = text(after, "todo");
    let (_, cur, _) = todo::fields(tx, ws, &t)?
        .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
    let was: todo::Fields =
        serde_json::from_value(before.clone()).map_err(|_| ApiError::invalid("before"))?;
    let now_fields: todo::Fields =
        serde_json::from_value(after["fields"].clone()).map_err(|_| ApiError::invalid("after"))?;
    // a later state change does not stand in the way; a later edit of its fields does
    if cur != now_fields {
        return Err(ApiError::rule(steve_i18n::t!("digest.change.todo_moved")));
    }
    todo::write(tx, ws, &t, &was, now).map_err(|e| match todo::message(&e) {
        Some(_) => ApiError::rule(steve_i18n::t!("digest.change.todo_moved")),
        None => e.into(),
    })?;
    Ok(())
}

fn undo_todo_link(tx: &mut W<'_>, ws: &str, after: &Value) -> ApiResult<()> {
    let (t, item) = (text(after, "todo"), text(after, "item"));
    let on = after["linked"] == true;
    if todo::item_linked(tx, ws, &t, &item)? != on {
        return Err(ApiError::rule(steve_i18n::t!("digest.change.todo_moved")));
    }
    todo::link_item(tx, ws, &t, &item, !on).map_err(|e| match todo::message(&e) {
        Some(m) => ApiError::rule(m),
        None => e.into(),
    })?;
    Ok(())
}

fn undo_memory_link(w: &mut W<'_>, ws: &str, after: &Value) -> ApiResult<()> {
    let a: steve_store::AddedMemoryIndex =
        serde_json::from_value(after.clone()).map_err(|_| ApiError::invalid("after"))?;
    let mut index = w.memory_index();
    if !index.can_unlink(ws, &a)? {
        return Err(ApiError::Conflict(steve_i18n::t!(
            "digest.change.target_moved"
        )));
    }
    index.unlink(ws, &a)?;
    Ok(())
}

fn undo_new_memory(m: &Modules, w: &mut W<'_>, ws: &str, after: &Value) -> ApiResult<()> {
    let id = text(after, "mem");
    match w.memory_index().candidate_decision(ws, &id)? {
        Some(Some(decision)) => {
            steve_modules::decisions::expire(m, &decision)?;
        }
        Some(None) => w.memory_index().expire_candidate(ws, &id)?,
        None => return Err(ApiError::rule(steve_i18n::t!("digest.change.mem_answered"))),
    }
    Ok(())
}

/// Undoes change `id` of `ws`. `ApiError::Rule` when it cannot be undone any more (a newer summary, a todo
/// changed since), `NotFound` when there is no such change.
pub fn undo(m: &Modules, ws: &str, actor: &str, id: &str) -> ApiResult<Value> {
    let now = m.clock.now();
    m.store.write(|w| {
        let ch = w
            .digest_changes()
            .get(ws, id)?
            .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.change")))?;
        if ch.state != "applied" {
            return Err(ApiError::rule(steve_i18n::t!(
                "digest.change.undone_already"
            )));
        }
        let after = ch.after;
        match ch.op.as_str() {
            "summary" => undo_summary(w, ws, &after)?,
            "segment" => undo_segment(w, ws, &after, now)?,
            "todo" => undo_created_todo(w, ws, &after)?,
            "todo_state" => undo_todo_state(w, ws, &ch.before, &after, now)?,
            "todo_edit" => undo_todo_edit(w, ws, &ch.before, &after, now)?,
            "todo_link" => undo_todo_link(w, ws, &after)?,
            "mem_link" => undo_memory_link(w, ws, &after)?,
            "mem_new" => undo_new_memory(m, w, ws, &after)?,
            _ => return Err(ApiError::invalid("op")),
        }
        w.digest_changes().mark_undone(ws, id, now)?;
        w.audit(ws, actor, "digest.undo", id, &json!({"op": ch.op}))?;
        Ok::<_, ApiError>(())
    })?;
    Ok(json!({"ok": true, "id": id}))
}

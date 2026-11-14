//! 我的待办: what you yourself are to do — distinct from items, the work handed to Bots.
//!
//! A todo has its text, the messages it came from (`todo_refs`: conversation + Steve message id), its matter,
//! the people involved (`todo_parties`), a due time only when a message states one (`due_quote`: the words
//! that say it), and a state: `open`, `done`, `dropped`, or `handed` (given to a Bot as an item, `item_id`).
//! A digest writes todos straight in (`source = steve`, marked “Steve 记的”), without asking; every change can
//! be undone ([`crate::changes`]).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_store::{StoreError, Tx, uid};

use crate::conv::ConvRef;

type Result<T> = std::result::Result<T, StoreError>;

pub const STATES: [&str; 4] = ["open", "done", "dropped", "handed"];
pub const TEXT_MAX: usize = 500;

pub struct NewTodo<'a> {
    pub ws: &'a str,
    pub owner: &'a str,
    pub text: &'a str,
    pub matter: Option<&'a str>,
    pub due: Option<i64>,
    pub due_quote: Option<&'a str>,
    /// `steve|you`
    pub source: &'a str,
    pub writer: &'a str,
    pub packet: Option<&'a str>,
    pub refs: Vec<(ConvRef, String)>,
    pub parties: Vec<String>,
}

pub fn create(tx: &Tx, t: &NewTodo, now: i64) -> Result<String> {
    if t.text.trim().is_empty()
        || t.text.chars().count() > TEXT_MAX
        || !["steve", "you"].contains(&t.source)
    {
        return Err(StoreError::Hook("invalid todo text or source".into()));
    }
    let owner: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
        params![t.ws, t.owner],
        |r| r.get(0),
    )?;
    if !owner {
        return Err(StoreError::Hook("unknown todo owner".into()));
    }
    let matter = match t.matter {
        Some(m) => Some(
            crate::matter::resolve(tx, t.ws, m)?
                .ok_or_else(|| StoreError::Hook("unknown todo matter".into()))?
                .0,
        ),
        None => None,
    };
    for p in &t.parties {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM parties WHERE workspace_id = ? AND id = ?)",
            params![t.ws, p],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::Hook("unknown todo party".into()));
        }
    }
    for (cv, id) in &t.refs {
        if !crate::conv::owner_sees(tx, t.ws, cv)?
            || crate::conv::by_ids(tx, t.ws, cv, &[id.clone()])?.len() != 1
        {
            return Err(StoreError::Hook("unknown todo message".into()));
        }
    }
    if let Some(packet) = t.packet {
        if crate::queue::get(tx, t.ws, packet)?.is_none() {
            return Err(StoreError::Hook("unknown todo packet".into()));
        }
    }
    if t.source == "steve" && t.due.is_some() {
        let quote = t
            .due_quote
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| StoreError::Hook("todo deadline needs its source words".into()))?;
        let mut found = false;
        for (cv, id) in &t.refs {
            found |= crate::conv::by_ids(tx, t.ws, cv, &[id.clone()])?
                .iter()
                .any(|m| m.text.contains(quote));
        }
        if !found {
            return Err(StoreError::Hook(
                "todo deadline quote not in source messages".into(),
            ));
        }
    }
    let id = uid("td");
    tx.execute(
        "INSERT INTO todos (id, workspace_id, owner, text, matter_id, due, due_quote, source, writer, packet_id, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![id, t.ws, t.owner, t.text.trim(), matter, t.due, t.due_quote, t.source, t.writer, t.packet, now, now],
    )?;
    for (conv, msg) in &t.refs {
        tx.execute(
            "INSERT OR IGNORE INTO todo_refs (todo_id, conv, message_id) VALUES (?, ?, ?)",
            params![id, conv.key(), msg],
        )?;
    }
    for p in &t.parties {
        tx.execute(
            "INSERT OR IGNORE INTO todo_parties (todo_id, party_id) VALUES (?, ?)",
            params![id, p],
        )?;
    }
    crate::event(tx, t.ws, &id);
    Ok(id)
}

/// `(state, item_id, text, due)` of a todo of `ws`.
pub fn current(
    c: &Connection,
    ws: &str,
    id: &str,
) -> Result<Option<(String, Option<String>, String, Option<i64>)>> {
    Ok(c.prepare_cached(
        "SELECT state, item_id, text, due FROM todos WHERE workspace_id = ? AND id = ?",
    )?
    .query_row(params![ws, id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .optional()?)
}

/// Sets the state (and the item when `handed`).
pub fn set_state(
    tx: &Tx,
    ws: &str,
    id: &str,
    state: &str,
    item: Option<&str>,
    now: i64,
) -> Result<bool> {
    if !STATES.contains(&state) || (state == "handed") != item.is_some() {
        return Err(StoreError::Hook("invalid todo state or handoff".into()));
    }
    if let Some(id) = item {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE workspace_id = ? AND id = ?)",
            params![ws, id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::Hook("unknown todo handoff item".into()));
        }
    }
    let closed = (state != "open").then_some(now);
    let n = tx.execute(
        "UPDATE todos SET state = ?, item_id = ?, closed = ?, updated = ? WHERE workspace_id = ? AND id = ?",
        params![state, item, closed, now, ws, id],
    )?;
    crate::event(tx, ws, id);
    Ok(n > 0)
}

pub fn set_text(
    tx: &Tx,
    ws: &str,
    id: &str,
    text: Option<&str>,
    due: Option<Option<i64>>,
    now: i64,
) -> Result<bool> {
    if current(tx, ws, id)?.is_none() {
        return Ok(false);
    }
    if let Some(t) = text {
        if t.trim().is_empty() || t.chars().count() > TEXT_MAX {
            return Err(StoreError::Hook("invalid todo text".into()));
        }
        tx.execute(
            "UPDATE todos SET text = ?, updated = ? WHERE workspace_id = ? AND id = ?",
            params![t.trim(), now, ws, id],
        )?;
    }
    if let Some(d) = due {
        // a due set by hand has no quote
        tx.execute(
            "UPDATE todos SET due = ?, due_quote = NULL, updated = ? WHERE workspace_id = ? AND id = ?",
            params![d, now, ws, id],
        )?;
    }
    crate::event(tx, ws, id);
    Ok(true)
}

/// Deletes a todo (undo of its creation).
pub fn delete(tx: &Tx, ws: &str, id: &str) -> Result<bool> {
    let n = tx.execute(
        "DELETE FROM todos WHERE workspace_id = ? AND id = ?",
        params![ws, id],
    )?;
    crate::event(tx, ws, id);
    Ok(n > 0)
}

fn conv_name(c: &Connection, conv: &ConvRef) -> Option<String> {
    let sql = match conv.kind.as_str() {
        "space" => "SELECT name FROM spaces WHERE id = ?",
        "mail_thread" => "SELECT subject FROM mail_threads WHERE id = ?",
        _ => return None,
    };
    c.query_row(sql, [&conv.id], |r| r.get(0)).ok()
}

fn json_of(c: &Connection, r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: String = r.get(0)?;
    let matter: Option<String> = r.get(4)?;
    let refs: Vec<(String, String)> = c
        .prepare_cached("SELECT conv, message_id FROM todo_refs WHERE todo_id = ? ORDER BY rowid")?
        .query_map([&id], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut sources: Vec<Value> = Vec::new();
    for (k, m) in &refs {
        let cv = ConvRef::parse(k);
        let excerpt: Option<String> = match cv.as_ref().map(|x| x.kind.as_str()) {
            Some("space") => c
                .query_row("SELECT text FROM chan_messages WHERE id = ?", [m], |x| {
                    x.get(0)
                })
                .optional()?
                .flatten(),
            Some("mail_thread") => c
                .query_row("SELECT snippet FROM mail_messages WHERE id = ?", [m], |x| {
                    x.get(0)
                })
                .optional()?,
            _ => None,
        };
        sources.push(json!({
            "conv": k, "message": m,
            "name": cv.as_ref().and_then(|x| conv_name(c, x)),
            "href": cv.as_ref().map(|x| x.href()),
            "excerpt": excerpt.map(|e| crate::cut(&e, 80)),
        }));
    }
    let people: Vec<Value> = c
        .prepare_cached("SELECT tp.party_id, p.name FROM todo_parties tp LEFT JOIN parties p ON p.id = tp.party_id WHERE tp.todo_id = ?")?
        .query_map([&id], |x| Ok(json!({"id": x.get::<_, String>(0)?, "name": x.get::<_, Option<String>>(1)?})))?
        .collect::<rusqlite::Result<_>>()?;
    let matter_title: Option<String> = match &matter {
        Some(m) => c
            .query_row("SELECT title FROM matters WHERE id = ?", [m], |x| x.get(0))
            .optional()?,
        None => None,
    };
    Ok(json!({
        "id": id, "text": r.get::<_, String>(1)?, "state": r.get::<_, String>(2)?, "item_id": r.get::<_, Option<String>>(3)?,
        "matter": matter.map(|m| json!({"id": m, "title": matter_title})),
        "due": r.get::<_, Option<i64>>(5)?, "due_quote": r.get::<_, Option<String>>(6)?,
        "source": r.get::<_, String>(7)?, "writer": r.get::<_, String>(8)?,
        "created": r.get::<_, i64>(9)?, "updated": r.get::<_, i64>(10)?, "closed": r.get::<_, Option<i64>>(11)?,
        "sources": sources, "people": people,
    }))
}

const COLS: &str =
    "id, text, state, item_id, matter_id, due, due_quote, source, writer, created, updated, closed";

/// The owner's todos in `states` (all when empty): open ones with a due date first, then the newest.
pub fn list(
    c: &Connection,
    ws: &str,
    owner: &str,
    states: &[String],
    limit: usize,
) -> Result<Vec<Value>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM todos WHERE workspace_id = ?1 AND owner = ?2
            AND (?3 = '[]' OR state IN (SELECT value FROM json_each(?3)))
          ORDER BY state <> 'open', due IS NULL, due, created DESC LIMIT {}",
        limit.clamp(1, 500)
    ))?;
    let states = serde_json::to_string(states).unwrap_or_else(|_| "[]".into());
    let rows = st.query_map(params![ws, owner, states], |r| json_of(c, r))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, ws: &str, id: &str) -> Result<Option<Value>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM todos WHERE workspace_id = ? AND id = ?"
    ))?
    .query_row(params![ws, id], |r| json_of(c, r))
    .optional()?)
}

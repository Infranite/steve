//! 事 (matters): one thing that happens across conversations — a launch, a contract, an incident.
//!
//! A matter has a title, a state (`open|done|dropped|merged`), its segments (`digest_segments` with its id:
//! a conversation and a message range, each with the digest Bot's one-line summary), its participants
//! (`matter_parties`, from the senders and @ parties of its segments, counted by code) and its links to items
//! and projects (`links`, subject `matter`, predicate `about`). Every change is a `matter_events` row: create,
//! assign, unassign, merge, split, not_same, link, unlink, state, rename. A split remembers its two halves as
//! 不是同一件 (`matter_not_same`), and a merge of two matters said to be different is refused unless you do it.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_store::base::ObjRef;
use steve_store::base::links::{self, LinkKey, NewLink};
use steve_store::{StoreError, Tx, uid};

use crate::conv::ConvRef;

type Result<T> = std::result::Result<T, StoreError>;

pub const STATES: [&str; 3] = ["open", "done", "dropped"];
/// The link predicate from a matter to an item or project.
pub const ABOUT: &str = "about";
pub const TITLE_MAX: usize = 200;

/// Why a matter change was refused.
#[derive(Debug)]
pub enum Refused {
    NotFound(String),
    NotSame(String, String),
    Invalid(&'static str),
    Store(StoreError),
}

impl From<StoreError> for Refused {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<rusqlite::Error> for Refused {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}

fn record(
    tx: &Tx,
    ws: &str,
    matter: &str,
    op: &str,
    other: Option<&str>,
    detail: Value,
    actor: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO matter_events (workspace_id, matter_id, op, other, detail, actor, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![ws, matter, op, other, detail.to_string(), actor, now],
    )?;
    crate::event(tx, ws, matter);
    Ok(())
}

/// The matter `id` of `ws` as `(id, title, state)`, following merges to the one it went into.
pub fn resolve(c: &Connection, ws: &str, id: &str) -> Result<Option<(String, String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT title, state, merged_into FROM matters WHERE workspace_id = ? AND id = ?",
    )?;
    let mut cur = id.to_string();
    for _ in 0..8 {
        let r: Option<(String, String, Option<String>)> = st
            .query_row(params![ws, cur], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        match r {
            Some((_, s, Some(into))) if s == "merged" => cur = into,
            Some((t, s, _)) => return Ok(Some((cur, t, s))),
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// Rebuilds the matter's full-text row: title, summary and its newest segment summaries.
pub fn reindex(tx: &Tx, matter: &str) -> Result<()> {
    let (title, summary): (String, String) = tx.query_row(
        "SELECT title, summary FROM matters WHERE id = ?",
        [matter],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let segs: Vec<String> = tx
        .prepare_cached("SELECT summary FROM digest_segments WHERE matter_id = ? ORDER BY last_at DESC LIMIT 30")?
        .query_map([matter], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let text = format!("{title}\n{summary}\n{}", segs.join("\n"));
    let row: Option<i64> = tx
        .prepare_cached("SELECT row FROM matters_fts_rows WHERE matter_id = ?")?
        .query_row([matter], |r| r.get(0))
        .optional()?;
    let row = match row {
        Some(r) => {
            tx.execute("DELETE FROM matters_fts WHERE rowid = ?", [r])?;
            r
        }
        None => {
            tx.execute(
                "INSERT INTO matters_fts_rows (matter_id) VALUES (?)",
                [matter],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.execute(
        "INSERT INTO matters_fts (rowid, seg) VALUES (?, ?)",
        params![row, steve_knowledge::seg::seg(&text)],
    )?;
    Ok(())
}

/// A new open matter. Returns its id.
pub fn create(
    tx: &Tx,
    ws: &str,
    title: &str,
    actor: &str,
    now: i64,
) -> std::result::Result<String, Refused> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_MAX {
        return Err(Refused::Invalid("title"));
    }
    let id = uid("mtr");
    tx.execute(
        "INSERT INTO matters (id, workspace_id, title, created_by, created, updated) VALUES (?, ?, ?, ?, ?, ?)",
        params![id, ws, title, actor, now, now],
    )?;
    record(
        tx,
        ws,
        &id,
        "create",
        None,
        json!({"title": title}),
        actor,
        now,
    )?;
    reindex(tx, &id)?;
    Ok(id)
}

/// Recomputes a matter's participants and time span from its segments.
pub fn recount(tx: &Tx, matter: &str, now: i64) -> Result<()> {
    let segs: Vec<(String, i64, i64)> = tx
        .prepare_cached(
            "SELECT parties, first_at, last_at FROM digest_segments WHERE matter_id = ?",
        )?
        .query_map([matter], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut parties: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let (mut first, mut last) = (i64::MAX, 0i64);
    for (ps, a, z) in &segs {
        first = first.min(*a);
        last = last.max(*z);
        for p in serde_json::from_str::<Vec<String>>(ps).unwrap_or_default() {
            let e = parties.entry(p).or_insert((0, 0));
            e.0 += 1;
            e.1 = e.1.max(*z);
        }
    }
    tx.execute("DELETE FROM matter_parties WHERE matter_id = ?", [matter])?;
    for (p, (n, at)) in parties {
        tx.execute(
            "INSERT INTO matter_parties (matter_id, party_id, messages, last_at) VALUES (?, ?, ?, ?)",
            params![matter, p, n, at],
        )?;
    }
    tx.execute(
        "UPDATE matters SET first_at = ?, last_at = ?, updated = ? WHERE id = ?",
        params![
            (!segs.is_empty()).then_some(first),
            (!segs.is_empty()).then_some(last),
            now,
            matter
        ],
    )?;
    Ok(())
}

/// A semantic segment names packet positions, never invented message or participant ids.
/// Phase D can call this storage contract without adding its Bot implementation here.
pub struct NewSegment<'a> {
    pub packet: &'a str,
    pub chunk: i64,
    pub first: i64,
    pub last: i64,
    pub summary: &'a str,
    pub tags: &'a [String],
    pub matter: Option<&'a str>,
}

pub fn segment(
    tx: &Tx,
    ws: &str,
    new: &NewSegment<'_>,
    writer: &str,
    now: i64,
) -> std::result::Result<String, Refused> {
    let p = crate::queue::get(tx, ws, new.packet)?
        .ok_or_else(|| Refused::NotFound(new.packet.into()))?;
    let conv = ConvRef::parse(&p.conv).ok_or(Refused::Invalid("conv"))?;
    if !crate::conv::owner_sees(tx, ws, &conv)? {
        return Err(Refused::NotFound(p.conv));
    }
    let chunks = crate::queue::chunks(tx, new.packet)?;
    let chunk = chunks
        .iter()
        .find(|c| c.n == new.chunk)
        .ok_or(Refused::Invalid("chunk"))?;
    if new.first < chunk.first_pos
        || new.last > chunk.last_pos
        || new.last < new.first
        || new.summary.trim().is_empty()
        || new.summary.chars().count() > 2000
    {
        return Err(Refused::Invalid("range_or_summary"));
    }
    let messages = p.facts["messages"]
        .as_array()
        .ok_or(Refused::Invalid("facts"))?;
    let selected: Vec<&Value> = messages
        .iter()
        .filter(|m| {
            m["pos"]
                .as_i64()
                .is_some_and(|n| n >= new.first && n <= new.last)
        })
        .collect();
    if selected.len() as i64 != new.last - new.first + 1 {
        return Err(Refused::Invalid("positions"));
    }
    let endpoints = [selected[0], selected[selected.len() - 1]];
    let mut ids = Vec::new();
    let mut times = Vec::new();
    for m in endpoints {
        ids.push(
            m["id"]
                .as_str()
                .ok_or(Refused::Invalid("message"))?
                .to_string(),
        );
        times.push(m["at"].as_i64().ok_or(Refused::Invalid("time"))?);
    }
    let mut people = std::collections::BTreeSet::new();
    let mut chains = std::collections::BTreeSet::new();
    for m in &selected {
        if let Some(id) = m["sender"].as_str() {
            people.insert(id.to_string());
        }
        if let Some(ids) = m["mentions"].as_array() {
            people.extend(ids.iter().filter_map(Value::as_str).map(str::to_string));
        }
        if let Some(id) = m["chain"].as_str() {
            chains.insert(id.to_string());
        }
    }
    let id = uid("dsg");
    tx.execute("INSERT INTO digest_segments (id, workspace_id, packet_id, conv, chunk, first_pos, last_pos,
        first_id, last_id, first_at, last_at, summary, tags, chains, parties, writer, created)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![id, ws, p.id, p.conv, new.chunk, new.first, new.last, ids[0], ids[1], times[0], times[1],
            new.summary.trim(), json!(new.tags).to_string(), json!(chains).to_string(), json!(people).to_string(), writer, now])?;
    assign(tx, ws, &id, new.matter, writer, now)?;
    let matter: Option<String> = tx.query_row(
        "SELECT matter_id FROM digest_segments WHERE id = ?",
        [&id],
        |r| r.get(0),
    )?;
    crate::changes::record(
        tx,
        ws,
        &p.id,
        writer,
        "segment",
        &id,
        Some(&p.id),
        &json!({}),
        &json!({"segment": id, "matter": matter, "opened": false}),
        now,
    )?;
    crate::event(tx, ws, &p.conv);
    Ok(id)
}

/// Puts segment `seg` into `matter` (or out of any, `None`).
pub fn assign(
    tx: &Tx,
    ws: &str,
    seg: &str,
    matter: Option<&str>,
    actor: &str,
    now: i64,
) -> std::result::Result<Option<String>, Refused> {
    let old: Option<Option<String>> = tx
        .prepare_cached("SELECT matter_id FROM digest_segments WHERE workspace_id = ? AND id = ?")?
        .query_row(params![ws, seg], |r| r.get(0))
        .optional()?;
    let Some(old) = old else {
        return Err(Refused::NotFound(seg.into()));
    };
    let target = match matter {
        Some(m) => Some(
            resolve(tx, ws, m)?
                .ok_or_else(|| Refused::NotFound(m.into()))?
                .0,
        ),
        None => None,
    };
    tx.execute(
        "UPDATE digest_segments SET matter_id = ? WHERE id = ?",
        params![target, seg],
    )?;
    if let Some(o) = &old
        && Some(o) != target.as_ref()
    {
        recount(tx, o, now)?;
        reindex(tx, o)?;
        record(tx, ws, o, "unassign", Some(seg), json!({}), actor, now)?;
    }
    if let Some(t) = &target {
        recount(tx, t, now)?;
        reindex(tx, t)?;
        record(tx, ws, t, "assign", Some(seg), json!({}), actor, now)?;
    }
    Ok(old)
}

fn pair(a: &str, b: &str) -> (String, String) {
    if a < b {
        (a.into(), b.into())
    } else {
        (b.into(), a.into())
    }
}

pub fn said_not_same(c: &Connection, ws: &str, a: &str, b: &str) -> Result<bool> {
    let (a, b) = pair(a, b);
    Ok(c.prepare_cached(
        "SELECT 1 FROM matter_not_same WHERE workspace_id = ? AND a = ? AND b = ?",
    )?
    .query_row(params![ws, a, b], |_| Ok(()))
    .optional()?
    .is_some())
}

/// Remembers that `a` and `b` are not the same.
pub fn not_same(
    tx: &Tx,
    ws: &str,
    a: &str,
    b: &str,
    actor: &str,
    now: i64,
) -> std::result::Result<(), Refused> {
    let a = resolve(tx, ws, a)?
        .ok_or_else(|| Refused::NotFound(a.into()))?
        .0;
    let b = resolve(tx, ws, b)?
        .ok_or_else(|| Refused::NotFound(b.into()))?
        .0;
    if a == b {
        return Err(Refused::Invalid("other"));
    }
    let (x, y) = pair(&a, &b);
    tx.execute(
        "INSERT OR IGNORE INTO matter_not_same (workspace_id, a, b, actor, created) VALUES (?, ?, ?, ?, ?)",
        params![ws, x, y, actor, now],
    )?;
    record(tx, ws, &a, "not_same", Some(&b), json!({}), actor, now)?;
    record(tx, ws, &b, "not_same", Some(&a), json!({}), actor, now)?;
    Ok(())
}

/// Merges `from` into `into`: its segments, links and participants move, it becomes `merged`. Refused when
/// the two were said not to be the same, unless `force` (you).
pub fn merge(
    tx: &Tx,
    ws: &str,
    from: &str,
    into: &str,
    actor: &str,
    force: bool,
    now: i64,
) -> std::result::Result<(), Refused> {
    let from = resolve(tx, ws, from)?
        .ok_or_else(|| Refused::NotFound(from.into()))?
        .0;
    let into = resolve(tx, ws, into)?
        .ok_or_else(|| Refused::NotFound(into.into()))?
        .0;
    if from == into {
        return Err(Refused::Invalid("other"));
    }
    if !force && said_not_same(tx, ws, &from, &into)? {
        return Err(Refused::NotSame(from, into));
    }
    let segs: Vec<String> = tx
        .prepare_cached("SELECT id FROM digest_segments WHERE matter_id = ?")?
        .query_map([&from], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute(
        "UPDATE digest_segments SET matter_id = ? WHERE matter_id = ?",
        params![into, from],
    )?;
    links::retarget(
        tx,
        ws,
        &ObjRef::new("matter", &from),
        &ObjRef::new("matter", &into),
    )?;
    tx.execute(
        "UPDATE todos SET matter_id = ? WHERE matter_id = ?",
        params![into, from],
    )?;
    tx.execute("INSERT OR IGNORE INTO mem_about (mem_id, workspace_id, object_type, object_id)
        SELECT mem_id, workspace_id, 'matter', ? FROM mem_about WHERE workspace_id = ? AND object_type = 'matter' AND object_id = ?", params![into, ws, from])?;
    tx.execute(
        "DELETE FROM mem_about WHERE workspace_id = ? AND object_type = 'matter' AND object_id = ?",
        params![ws, from],
    )?;
    // A merge must not forget an earlier explicit separation from a third matter.
    let separated: Vec<String> = tx.prepare_cached("SELECT CASE WHEN a = ?2 THEN b ELSE a END FROM matter_not_same WHERE workspace_id = ?1 AND (a = ?2 OR b = ?2)")?
        .query_map(params![ws, from], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for other in separated {
        if other != into {
            let (a, b) = pair(&into, &other);
            tx.execute("INSERT OR IGNORE INTO matter_not_same (workspace_id, a, b, actor, created) VALUES (?, ?, ?, ?, ?)", params![ws, a, b, actor, now])?;
        }
    }
    tx.execute(
        "UPDATE matters SET state = 'merged', merged_into = ?, updated = ? WHERE id = ?",
        params![into, now, from],
    )?;
    if force {
        let (x, y) = pair(&from, &into);
        tx.execute(
            "DELETE FROM matter_not_same WHERE workspace_id = ? AND a = ? AND b = ?",
            params![ws, x, y],
        )?;
    }
    recount(tx, &from, now)?;
    recount(tx, &into, now)?;
    reindex(tx, &into)?;
    if let Some(r) = tx
        .prepare_cached("SELECT row FROM matters_fts_rows WHERE matter_id = ?")?
        .query_row([&from], |r| r.get::<_, i64>(0))
        .optional()?
    {
        tx.execute("DELETE FROM matters_fts WHERE rowid = ?", [r])?;
        tx.execute("DELETE FROM matters_fts_rows WHERE row = ?", [r])?;
    }
    record(
        tx,
        ws,
        &into,
        "merge",
        Some(&from),
        json!({"segments": segs}),
        actor,
        now,
    )?;
    record(
        tx,
        ws,
        &from,
        "merge",
        Some(&into),
        json!({"into": into}),
        actor,
        now,
    )?;
    Ok(())
}

/// Moves `segments` of `matter` into a new matter titled `title`, and remembers the two as not the same.
pub fn split(
    tx: &Tx,
    ws: &str,
    matter: &str,
    segments: &[String],
    title: &str,
    actor: &str,
    now: i64,
) -> std::result::Result<String, Refused> {
    let matter = resolve(tx, ws, matter)?
        .ok_or_else(|| Refused::NotFound(matter.into()))?
        .0;
    if segments.is_empty() {
        return Err(Refused::Invalid("segments"));
    }
    for s in segments {
        let m: Option<Option<String>> = tx
            .query_row(
                "SELECT matter_id FROM digest_segments WHERE workspace_id = ? AND id = ?",
                params![ws, s],
                |r| r.get(0),
            )
            .optional()?;
        if m.flatten().as_deref() != Some(matter.as_str()) {
            return Err(Refused::NotFound(s.clone()));
        }
    }
    let new = create(tx, ws, title, actor, now)?;
    for s in segments {
        tx.execute(
            "UPDATE digest_segments SET matter_id = ? WHERE id = ?",
            params![new, s],
        )?;
    }
    recount(tx, &matter, now)?;
    recount(tx, &new, now)?;
    reindex(tx, &matter)?;
    reindex(tx, &new)?;
    record(
        tx,
        ws,
        &matter,
        "split",
        Some(&new),
        json!({"segments": segments}),
        actor,
        now,
    )?;
    not_same(tx, ws, &matter, &new, actor, now)?;
    Ok(new)
}

/// Links a matter to an item or project (`object.kind` `item|project`), or unlinks it.
pub fn link(
    tx: &Tx,
    ws: &str,
    matter: &str,
    object: &ObjRef,
    on: bool,
    actor: &str,
    now: i64,
) -> std::result::Result<bool, Refused> {
    let matter = resolve(tx, ws, matter)?
        .ok_or_else(|| Refused::NotFound(matter.into()))?
        .0;
    let table = match object.kind.as_str() {
        "item" => "items",
        "project" => "projects",
        _ => return Err(Refused::Invalid("object")),
    };
    let exists: bool = tx.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE workspace_id = ? AND id = ?)"),
        params![ws, object.id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(Refused::NotFound(object.id.clone()));
    }
    let subject = ObjRef::new("matter", &matter);
    let changed = if on {
        links::add(tx, ws, &NewLink::new(subject, ABOUT, object.clone(), actor))?
    } else {
        links::remove(
            tx,
            ws,
            &LinkKey {
                subject,
                predicate: ABOUT.into(),
                object: object.clone(),
            },
        )?
    };
    if changed {
        record(
            tx,
            ws,
            &matter,
            if on { "link" } else { "unlink" },
            Some(&format!("{}:{}", object.kind, object.id)),
            json!({}),
            actor,
            now,
        )?;
    }
    Ok(changed)
}

/// Sets the state (`open|done|dropped`) or the title.
pub fn edit(
    tx: &Tx,
    ws: &str,
    matter: &str,
    state: Option<&str>,
    title: Option<&str>,
    summary: Option<&str>,
    actor: &str,
    now: i64,
) -> std::result::Result<(), Refused> {
    let (id, old_title, old_state) =
        resolve(tx, ws, matter)?.ok_or_else(|| Refused::NotFound(matter.into()))?;
    if let Some(s) = state {
        if !STATES.contains(&s) {
            return Err(Refused::Invalid("state"));
        }
        if s != old_state {
            tx.execute(
                "UPDATE matters SET state = ?, updated = ? WHERE id = ?",
                params![s, now, id],
            )?;
            record(
                tx,
                ws,
                &id,
                "state",
                None,
                json!({"from": old_state, "to": s}),
                actor,
                now,
            )?;
        }
    }
    if let Some(t) = title {
        let t = t.trim();
        if t.is_empty() || t.chars().count() > TITLE_MAX {
            return Err(Refused::Invalid("title"));
        }
        if t != old_title {
            tx.execute(
                "UPDATE matters SET title = ?, updated = ? WHERE id = ?",
                params![t, now, id],
            )?;
            record(
                tx,
                ws,
                &id,
                "rename",
                None,
                json!({"from": old_title, "to": t}),
                actor,
                now,
            )?;
        }
    }
    if let Some(s) = summary {
        tx.execute(
            "UPDATE matters SET summary = ?, updated = ? WHERE id = ?",
            params![s.trim(), now, id],
        )?;
    }
    reindex(tx, &id)?;
    Ok(())
}

fn name_of(c: &Connection, kind: &str, id: &str) -> Option<String> {
    let sql = match kind {
        "party" => "SELECT name FROM parties WHERE id = ?",
        "item" => "SELECT title FROM items WHERE id = ?",
        "project" => "SELECT name FROM projects WHERE id = ?",
        "space" => "SELECT name FROM spaces WHERE id = ?",
        "mail_thread" => "SELECT subject FROM mail_threads WHERE id = ?",
        _ => return None,
    };
    c.query_row(sql, [id], |r| r.get(0)).ok()
}

/// A matter in brief.
fn brief(c: &Connection, id: &str) -> Result<Value> {
    let (title, state, summary, first, last, updated): (
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
        i64,
    ) = c.query_row(
        "SELECT title, state, summary, first_at, last_at, updated FROM matters WHERE id = ?",
        [id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let convs: Vec<String> = c
        .prepare_cached("SELECT DISTINCT conv FROM digest_segments WHERE matter_id = ?")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let segments: i64 = c.query_row(
        "SELECT count(*) FROM digest_segments WHERE matter_id = ?",
        [id],
        |r| r.get(0),
    )?;
    let people: Vec<Value> = c
        .prepare_cached(
            "SELECT mp.party_id, p.name FROM matter_parties mp LEFT JOIN parties p ON p.id = mp.party_id
              WHERE mp.matter_id = ? ORDER BY mp.messages DESC, mp.last_at DESC LIMIT 8",
        )?
        .query_map([id], |r| Ok(json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, Option<String>>(1)?})))?
        .collect::<rusqlite::Result<_>>()?;
    let convs: Vec<Value> = convs
        .iter()
        .filter_map(|k| ConvRef::parse(k))
        .map(
            |cv| json!({"conv": cv.key(), "name": name_of(c, &cv.kind, &cv.id), "href": cv.href()}),
        )
        .collect();
    Ok(json!({
        "id": id, "title": title, "state": state, "summary": summary, "first_at": first, "last_at": last,
        "updated": updated, "segments": segments, "conversations": convs, "people": people,
    }))
}

/// What a list of matters is narrowed to.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    /// `party|project|item|space|mail_thread`, and its id.
    pub subject: Option<ObjRef>,
    pub states: Vec<String>,
    pub query: Option<String>,
    pub limit: usize,
}

/// Matters of `ws`, the most recently active first.
pub fn list(c: &Connection, ws: &str, f: &Filter) -> Result<Vec<Value>> {
    let limit = if f.limit == 0 { 50 } else { f.limit.min(200) };
    let mut ids: Vec<String> = match &f.subject {
        Some(s) if s.kind == "party" => c
            .prepare_cached("SELECT matter_id FROM matter_parties WHERE party_id = ?")?
            .query_map([&s.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?,
        Some(s) if s.kind == "item" || s.kind == "project" => {
            let mut v: Vec<String> = links::to(c, ws, s, Some(ABOUT))?
                .into_iter()
                .filter(|l| l.subject_type == "matter")
                .map(|l| l.subject_id)
                .collect();
            if s.kind == "project" {
                // and the matters of the project's items
                let items: Vec<String> = c
                    .prepare_cached("SELECT id FROM items WHERE workspace_id = ? AND project_id = ?")?
                    .query_map(params![ws, s.id], |r| r.get(0))?
                    .filter_map(|r| r.ok())
                    .collect();
                for it in items {
                    v.extend(
                        links::to(c, ws, &ObjRef::new("item", &it), Some(ABOUT))?
                            .into_iter()
                            .filter(|l| l.subject_type == "matter")
                            .map(|l| l.subject_id),
                    );
                }
            }
            v
        }
        Some(s) => c
            .prepare_cached("SELECT DISTINCT matter_id FROM digest_segments WHERE workspace_id = ? AND conv = ? AND matter_id IS NOT NULL")?
            .query_map(params![ws, format!("{}:{}", s.kind, s.id)], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?,
        None => c
            .prepare_cached("SELECT id FROM matters WHERE workspace_id = ? AND state <> 'merged'")?
            .query_map([ws], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?,
    };
    if let Some(q) = f.query.as_deref().filter(|q| !q.trim().is_empty())
        && let Some(expr) = steve_knowledge::seg::fts_query(q)
    {
        let hits: Vec<String> = c
            .prepare_cached(
                "SELECT r.matter_id FROM matters_fts JOIN matters_fts_rows r ON r.row = matters_fts.rowid
                  WHERE matters_fts MATCH ? ORDER BY bm25(matters_fts) LIMIT 200",
            )?
            .query_map([expr], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.retain(|i| hits.contains(i));
    }
    ids.sort();
    ids.dedup();
    let mut rows: Vec<(i64, String)> = Vec::new();
    for id in ids {
        let r: Option<(String, i64)> = c
            .query_row(
                "SELECT state, coalesce(last_at, updated) FROM matters WHERE workspace_id = ? AND id = ?",
                params![ws, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((state, at)) = r
            && state != "merged"
            && (f.states.is_empty() || f.states.contains(&state))
        {
            rows.push((at, id));
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    rows.truncate(limit);
    rows.into_iter().map(|(_, id)| brief(c, &id)).collect()
}

/// One matter with its timeline across conversations, people, links and history; `None` when it does not exist.
pub fn get(c: &Connection, ws: &str, id: &str) -> Result<Option<Value>> {
    let Some(viewer) = crate::conv::owner_viewer(c, ws)? else {
        return Ok(None);
    };
    get_for(c, &viewer, id)
}

/// Matter detail for the actual caller; owner status does not bypass memory visibility.
pub fn get_for(
    c: &Connection,
    viewer: &steve_store::base::Viewer,
    id: &str,
) -> Result<Option<Value>> {
    let ws = viewer.ws.as_str();
    let Some((id, _, _)) = resolve(c, ws, id)? else {
        return Ok(None);
    };
    let mut out = brief(c, &id)?;
    let mut names: HashMap<String, Option<String>> = HashMap::new();
    let segs: Vec<Value> = c
        .prepare_cached(
            "SELECT id, conv, packet_id, first_id, last_id, first_at, last_at, summary, tags, writer
               FROM digest_segments WHERE matter_id = ? ORDER BY first_at, id",
        )?
        .query_map([&id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "conv": r.get::<_, String>(1)?, "packet": r.get::<_, String>(2)?,
                "first_id": r.get::<_, String>(3)?, "last_id": r.get::<_, String>(4)?,
                "first_at": r.get::<_, i64>(5)?, "last_at": r.get::<_, i64>(6)?, "summary": r.get::<_, String>(7)?,
                "tags": serde_json::from_str::<Value>(&r.get::<_, String>(8)?).unwrap_or(json!([])),
                "writer": r.get::<_, String>(9)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let segs: Vec<Value> = segs
        .into_iter()
        .map(|mut s| {
            let k = s["conv"].as_str().unwrap_or_default().to_string();
            let name = names
                .entry(k.clone())
                .or_insert_with(|| ConvRef::parse(&k).and_then(|cv| name_of(c, &cv.kind, &cv.id)))
                .clone();
            s["conv_name"] = json!(name);
            s["href"] = json!(ConvRef::parse(&k).map(|cv| cv.href()));
            s
        })
        .collect();
    let all_people: Vec<Value> = c
        .prepare_cached(
            "SELECT mp.party_id, p.name, mp.messages, mp.last_at FROM matter_parties mp LEFT JOIN parties p ON p.id = mp.party_id
              WHERE mp.matter_id = ? ORDER BY mp.messages DESC, mp.last_at DESC",
        )?
        .query_map([&id], |r| {
            Ok(json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, Option<String>>(1)?, "segments": r.get::<_, i64>(2)?, "last_at": r.get::<_, i64>(3)?}))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let linked: Vec<Value> = links::from(c, ws, &ObjRef::new("matter", &id), Some(ABOUT))?
        .into_iter()
        .map(|l| json!({"type": l.object_type, "id": l.object_id, "name": name_of(c, &l.object_type, &l.object_id)}))
        .collect();
    let events: Vec<Value> = c
        .prepare_cached("SELECT op, other, detail, actor, created FROM matter_events WHERE matter_id = ? ORDER BY created DESC, id DESC LIMIT 50")?
        .query_map([&id], |r| {
            Ok(json!({"op": r.get::<_, String>(0)?, "other": r.get::<_, Option<String>>(1)?,
                      "detail": serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(json!({})),
                      "actor": r.get::<_, String>(3)?, "at": r.get::<_, i64>(4)?}))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let not_same: Vec<String> = c
        .prepare_cached("SELECT CASE WHEN a = ?2 THEN b ELSE a END FROM matter_not_same WHERE workspace_id = ?1 AND (a = ?2 OR b = ?2)")?
        .query_map(params![ws, id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let todos: Vec<Value> = c
        .prepare_cached("SELECT id, text, state FROM todos WHERE matter_id = ? ORDER BY created DESC LIMIT 20")?
        .query_map([&id], |r| Ok(json!({"id": r.get::<_, String>(0)?, "text": r.get::<_, String>(1)?, "state": r.get::<_, String>(2)?})))?
        .collect::<rusqlite::Result<_>>()?;
    let scope = viewer.sql("m");
    let mut args: Vec<rusqlite::types::Value> = vec![ws.to_string().into(), id.clone().into()];
    args.extend(scope.params);
    let mems: Vec<Value> = c
        .prepare(&format!(
            "SELECT m.id, m.text, m.status FROM mem_about a JOIN mems m ON m.id = a.mem_id
              WHERE a.workspace_id = ? AND a.object_type = 'matter' AND a.object_id = ?
                AND m.workspace_id = a.workspace_id AND m.status IN ('confirmed', 'candidate') AND {}
              ORDER BY m.updated DESC LIMIT 20", scope.sql,
        ))?
        .query_map(rusqlite::params_from_iter(args), |r| Ok(json!({"id": r.get::<_, String>(0)?, "text": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?})))?
        .collect::<rusqlite::Result<_>>()?;
    out["timeline"] = json!(segs);
    out["people"] = json!(all_people);
    out["links"] = json!(linked);
    out["events"] = json!(events);
    out["not_same"] = json!(not_same);
    out["todos"] = json!(todos);
    out["memories"] = json!(mems);
    Ok(Some(out))
}

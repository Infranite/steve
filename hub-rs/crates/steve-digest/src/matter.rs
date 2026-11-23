//! 事 (matters): one thing that happens across conversations — a launch, a contract, an incident.
//!
//! A matter has a title, a state (`open|done|dropped|merged`), its segments (`digest_segments` with its id:
//! a conversation and a message range, each with the digest Bot's one-line summary), its participants
//! (`matter_parties`, from the senders and @ parties of its segments, counted by code) and its links to items
//! and projects (`links`, subject `matter`, predicate `about`). Every change is a `matter_events` row: create,
//! assign, unassign, merge, split, not_same, link, unlink, state, rename. A split remembers its two halves as
//! 不是同一件 (`matter_not_same`), and a merge of two matters said to be different is refused unless you do it.

use std::collections::HashMap;

use serde_json::{Value, json};
use steve_store::base::ObjRef;
use steve_store::base::links::{LinkKey, Links, NewLink};
use steve_store::digest::{Matter, MatterEvent, Matters, NewChange, SegmentInput, Segments, Todos};
use steve_store::{
    ChannelSources, Contacts, ItemReads, MailSources, MemoryIndex, Read, StoreError, W,
};

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

fn record(
    tx: &mut W<'_>,
    ws: &str,
    matter: &str,
    op: &str,
    other: Option<&str>,
    detail: Value,
    actor: &str,
    now: i64,
) -> Result<()> {
    Matters(tx).record(
        ws,
        matter,
        MatterEvent {
            op,
            other,
            detail,
            actor,
            now,
        },
    )
}

/// The matter of this workspace, following merges.
pub fn resolve(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, String, String)>> {
    Ok(Matters(c)
        .resolve(ws, id)?
        .map(|m| (m.id, m.title, m.state)))
}

pub fn reindex(tx: &mut W<'_>, matter: &str) -> Result<()> {
    let mut matters = Matters(tx);
    let text = matters.search_document(matter)?;
    matters.put_search_document(matter, &steve_knowledge::seg::seg(&text))
}

/// A new open matter. Returns its id.
pub fn create(
    tx: &mut W<'_>,
    ws: &str,
    title: &str,
    actor: &str,
    now: i64,
) -> std::result::Result<String, Refused> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_MAX {
        return Err(Refused::Invalid("title"));
    }
    let id = Matters(tx).create(ws, title, actor, now)?;
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
pub fn recount(tx: &mut W<'_>, matter: &str, now: i64) -> Result<()> {
    Matters(tx).recount(matter, now)
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
    tx: &mut W<'_>,
    ws: &str,
    new: &NewSegment<'_>,
    writer: &str,
    now: i64,
) -> std::result::Result<String, Refused> {
    let p = tx
        .digest_packets()
        .get(ws, new.packet)?
        .ok_or_else(|| Refused::NotFound(new.packet.into()))?;
    let conv = ConvRef::parse(&p.conv).ok_or(Refused::Invalid("conv"))?;
    if !crate::conv::owner_sees(tx, ws, &conv)? {
        return Err(Refused::NotFound(p.conv));
    }
    let chunks = tx.digest_packets().chunks(new.packet)?;
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
    let id = tx.digest_segments().insert(SegmentInput {
        ws,
        packet: &p.id,
        conv: &p.conv,
        chunk: new.chunk,
        first: new.first,
        last: new.last,
        first_id: &ids[0],
        last_id: &ids[1],
        first_at: times[0],
        last_at: times[1],
        summary: new.summary.trim(),
        tags: new.tags,
        chains: &chains,
        parties: &people,
        writer,
        now,
    })?;
    assign(tx, ws, &id, new.matter, writer, now)?;
    let matter = tx.digest_segments().assignment(ws, &id)?.flatten();
    tx.digest_changes().record(NewChange {
        ws,
        run: &p.id,
        writer,
        op: "segment",
        target: &id,
        packet: Some(&p.id),
        before: &json!({}),
        after: &json!({"segment": id, "matter": matter, "opened": false}),
        now,
    })?;
    crate::emit(tx, ws, &p.conv);
    Ok(id)
}

/// Puts segment `seg` into `matter` (or out of any, `None`).
pub fn assign(
    tx: &mut W<'_>,
    ws: &str,
    seg: &str,
    matter: Option<&str>,
    actor: &str,
    now: i64,
) -> std::result::Result<Option<String>, Refused> {
    let old = tx.digest_segments().assignment(ws, seg)?;
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
    tx.digest_segments().assign(ws, seg, target.as_deref())?;
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

pub fn said_not_same(c: &mut impl Read, ws: &str, a: &str, b: &str) -> Result<bool> {
    Matters(c).not_same(ws, a, b)
}

/// Remembers that `a` and `b` are not the same.
pub fn not_same(
    tx: &mut W<'_>,
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
    Matters(tx).separate(ws, &a, &b, actor, now)?;
    record(tx, ws, &a, "not_same", Some(&b), json!({}), actor, now)?;
    record(tx, ws, &b, "not_same", Some(&a), json!({}), actor, now)?;
    Ok(())
}

/// Merges `from` into `into`: its segments, links and participants move, it becomes `merged`. Refused when
/// the two were said not to be the same, unless `force` (you).
pub fn merge(
    tx: &mut W<'_>,
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
    let segs = tx.digest_segments().move_all(ws, &from, &into)?;
    Links(tx).retarget(
        ws,
        &ObjRef::new("matter", &from),
        &ObjRef::new("matter", &into),
    )?;
    Todos(tx).move_matter(ws, &from, &into)?;
    tx.memory_index().retarget(ws, "matter", &from, &into)?;
    let mut matters = Matters(tx);
    for other in matters.separated(ws, &from)? {
        if other != into {
            matters.separate(ws, &into, &other, actor, now)?;
        }
    }
    matters.mark_merged(&from, &into, now)?;
    if force {
        matters.clear_separation(ws, &from, &into)?;
    }
    matters.recount(&from, now)?;
    matters.recount(&into, now)?;
    matters.remove_search_document(&from)?;
    reindex(tx, &into)?;
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
    tx: &mut W<'_>,
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
        let m = tx.digest_segments().assignment(ws, s)?;
        if m.flatten().as_deref() != Some(matter.as_str()) {
            return Err(Refused::NotFound(s.clone()));
        }
    }
    let new = create(tx, ws, title, actor, now)?;
    for s in segments {
        tx.digest_segments().assign(ws, s, Some(&new))?;
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
    tx: &mut W<'_>,
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
    let exists = match object.kind.as_str() {
        "item" => ItemReads(tx).title(ws, &object.id)?.is_some(),
        "project" => ItemReads(tx).project_name(ws, &object.id)?.is_some(),
        _ => return Err(Refused::Invalid("object")),
    };
    if !exists {
        return Err(Refused::NotFound(object.id.clone()));
    }
    let subject = ObjRef::new("matter", &matter);
    let changed = if on {
        Links(tx).add(ws, &NewLink::new(subject, ABOUT, object.clone(), actor))?
    } else {
        Links(tx).remove(
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
    tx: &mut W<'_>,
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
            Matters(tx).set_state(&id, s, now)?;
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
            Matters(tx).set_title(&id, t, now)?;
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
        Matters(tx).set_summary(&id, s.trim(), now)?;
    }
    reindex(tx, &id)?;
    Ok(())
}

pub(crate) fn name_of(c: &mut impl Read, ws: &str, kind: &str, id: &str) -> Result<Option<String>> {
    Ok(match kind {
        "party" => Contacts::new(c).party_names([id])?.remove(id),
        "item" => ItemReads(c).title(ws, id)?,
        "project" => ItemReads(c).project_name(ws, id)?,
        "space" => ChannelSources(c).space(ws, id)?.map(|space| space.name),
        "mail_thread" => MailSources(c).thread(ws, id)?.map(|thread| thread.subject),
        _ => None,
    })
}

fn people(c: &mut impl Read, id: &str, limit: Option<u64>) -> Result<Vec<Value>> {
    let people = Matters(c).people(id, limit)?;
    let names = Contacts::new(c).party_names(people.iter().map(|p| p.party.as_str()))?;
    Ok(people.into_iter().map(|p| match limit {
        Some(_) => json!({"id": p.party, "name": names.get(&p.party)}),
        None => json!({"id": p.party, "name": names.get(&p.party), "segments": p.segments, "last_at": p.last_at}),
    }).collect())
}

fn brief(c: &mut impl Read, ws: &str, matter: &Matter) -> Result<Value> {
    let mut out = json!(matter);
    let mut segments = Segments::new(c);
    let conversations = segments.conversations(&matter.id)?;
    out["segments"] = json!(segments.count(&matter.id)?);
    let mut convs = Vec::new();
    for cv in conversations.iter().filter_map(|key| ConvRef::parse(key)) {
        let name = name_of(c, ws, &cv.kind, &cv.id)?;
        convs.push(json!({"conv": cv.key(), "name": name, "href": cv.href()}));
    }
    out["conversations"] = json!(convs);
    out["people"] = json!(people(c, &matter.id, Some(8))?);
    Ok(out)
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
pub fn list(c: &mut impl Read, ws: &str, f: &Filter) -> Result<Vec<Value>> {
    let limit = if f.limit == 0 { 50 } else { f.limit.min(200) };
    let mut ids = match &f.subject {
        Some(s) if s.kind == "party" => Some(Matters(c).related_to_person(ws, &s.id)?),
        Some(s) if s.kind == "item" || s.kind == "project" => {
            let mut items = vec![s.clone()];
            if s.kind == "project" {
                items.extend(
                    ItemReads(c)
                        .in_project(ws, &s.id)?
                        .into_iter()
                        .map(|id| ObjRef::new("item", &id)),
                );
            }
            let mut matches = Vec::new();
            for obj in items {
                matches.extend(
                    Links(c)
                        .to(ws, &obj, Some(ABOUT))?
                        .into_iter()
                        .filter(|row| row.subject_type == "matter")
                        .map(|row| row.subject_id),
                );
            }
            Some(matches)
        }
        Some(s) => {
            Some(Segments::new(c).conversation_matters(ws, &format!("{}:{}", s.kind, s.id))?)
        }
        None => None,
    };
    if let Some(expr) = f
        .query
        .as_deref()
        .filter(|q| !q.trim().is_empty())
        .and_then(steve_knowledge::seg::fts_query)
    {
        let mut hits: Vec<_> = Matters(c)
            .search(ws, &expr, &STATES, 200)?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if let Some(ids) = ids {
            hits.retain(|id| ids.contains(id));
        }
        ids = Some(hits);
    }
    if let Some(ids) = &mut ids {
        ids.sort();
        ids.dedup();
    }
    Matters(c)
        .list(ws, ids.as_deref(), &f.states, limit)?
        .iter()
        .map(|row| brief(c, ws, row))
        .collect()
}

/// One matter with its timeline across conversations, people, links and history; `None` when it does not exist.
pub fn get(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<Value>> {
    let Some(viewer) = crate::conv::owner_viewer(c, ws)? else {
        return Ok(None);
    };
    get_for(c, &viewer, id)
}

/// Matter detail for the actual caller within their workspace.
pub fn get_for(
    c: &mut impl Read,
    viewer: &steve_store::base::Viewer,
    id: &str,
) -> Result<Option<Value>> {
    let ws = viewer.ws.as_str();
    let Some(matter) = Matters(c).resolve(ws, id)? else {
        return Ok(None);
    };
    let id = matter.id.as_str();
    let mut out = brief(c, ws, &matter)?;
    let mut names = HashMap::new();
    let mut timeline = Vec::new();
    for segment in Segments::new(c).timeline(id)? {
        let conv = ConvRef::parse(&segment.conv);
        let name = match names.entry(segment.conv.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let name = match &conv {
                    Some(cv) => name_of(c, ws, &cv.kind, &cv.id)?,
                    None => None,
                };
                entry.insert(name)
            }
        };
        let mut row = json!(segment);
        row["conv_name"] = json!(name);
        row["href"] = json!(conv.map(|cv| cv.href()));
        timeline.push(row);
    }
    let object = ObjRef::new("matter", id);
    let mut linked = Vec::new();
    for link in Links(c).from(ws, &object, Some(ABOUT))? {
        let name = name_of(c, ws, &link.object_type, &link.object_id)?;
        linked.push(json!({"type": link.object_type, "id": link.object_id, "name": name}));
    }
    out["timeline"] = json!(timeline);
    out["people"] = json!(people(c, id, None)?);
    out["links"] = json!(linked);
    out["events"] = json!(Matters(c).events(id)?);
    out["not_same"] = json!(Matters(c).separated(ws, id)?);
    out["todos"] = json!(Todos(c).for_matter(ws, id, &viewer.user)?);
    out["memories"] = json!(MemoryIndex::new(c).about(viewer, &object, 20)?);
    Ok(Some(out))
}

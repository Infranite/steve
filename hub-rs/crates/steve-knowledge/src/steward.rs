//! Keeping the memories in order: the agent tools `steve_memory_list` and `steve_memory_tidy`, and
//! the change log `mem_changes` that lists, per run, everything an agent changed, with undo.
//!
//! Any agent can use the tools; the knowledge steward is an ordinary Bot on an ordinary schedule
//! that does (the knowledge page sets both up from the Bot template). What a tidy does:
//!
//! - `merge`: several memories say the same thing. A candidate with the merged wording that replaces
//!   all of them once confirmed — the usual 待确认 flow; until then nothing changes.
//! - `resolve`: memories contradict each other and one is the newer explicit correction. The others
//!   become stale at once (kept, no longer used).
//! - `flag`: they contradict and it is not clear which holds. Nothing changes; the page asks which
//!   one to keep.
//! - `stale`: no longer true or no longer useful; kept, no longer used.
//! - `about`: says who or what it is about (a person, organization, project or bot).
//! - `source`: where it came from (an item), for a memory that has no source yet.
//!
//! Every change is a `mem_changes` row of the run (the item, or the session) that made it, and can
//! be undone from the page. Nothing is redacted or filtered: the tools see the memories as written.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::mems::{self, Mem, MemFilter, MemPatch};
use steve_store::base::{self, Level, ObjRef, Viewer};
use steve_store::{ChangeEvent, Tx, uid};

use crate::candidates::{self, NewCandidate, Origin, OriginKind};
use crate::extract::Names;
use crate::provenance;
use crate::service::{mem_json_for, prepare};
use crate::sync;

pub const LIST: &str = "steve_memory_list";
pub const TIDY: &str = "steve_memory_tidy";
pub const OPS: [&str; 6] = ["merge", "resolve", "flag", "stale", "about", "source"];
/// The most memories one merge, resolve or stale may name.
pub const MAX_IDS: usize = 10;
const NOUN: &str = "记忆";
/// The runs the page lists.
const RUNS_SHOWN: usize = 10;

pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": LIST,
            "description": "List the user's memories (long-term memories and knowledge entries) with their ids, kind, status, what they are about, where they came from, when they were written and how often they were used. Use it before steve_memory_tidy.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["confirmed", "candidate", "stale", "all"], "description": "default: confirmed and candidate" }
                }
            }
        }),
        json!({
            "name": TIDY,
            "description": "Change the user's memories to keep them in order; every change is listed for the user with undo. op: merge (ids say the same thing: text is the merged wording; it becomes a candidate the user confirms, and replaces them once confirmed), resolve (ids contradict keep, which is the newer explicit correction: they become stale), flag (ids contradict and it is unclear which holds: the user decides), stale (ids are outdated: kept, no longer used), about (ids[0] is about this person, organization, project or bot), source (ids[0] came from this item; only for a memory without a source). reason: one sentence the user reads.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": OPS },
                    "ids": { "type": "array", "items": { "type": "string" }, "description": "memory ids (mm_…)" },
                    "text": { "type": "string", "description": "merge: the merged wording" },
                    "keep": { "type": "string", "description": "resolve: the memory that holds" },
                    "about": { "type": "string", "description": "about: a person, organization, project or bot, by exact name or id" },
                    "item": { "type": "string", "description": "source: the item id (ST-12)" },
                    "reason": { "type": "string" }
                },
                "required": ["op", "ids", "reason"]
            }
        }),
    ]
}

pub fn handles(name: &str) -> bool {
    name == LIST || name == TIDY
}

fn who_of(cx: &McpCx) -> Caller {
    Caller {
        ws: cx.ws.clone(),
        me: cx.user.clone(),
    }
}

/// Runs one tool (synchronous: call it off the runtime). `Ok` is the result, `Err` the `isError` text.
pub fn call(m: &Modules, cx: &McpCx, name: &str, args: &Value) -> Result<String, String> {
    let who = who_of(cx);
    prepare(m, &who).map_err(|e| e.to_string())?;
    let viewer = m.viewer(&who).map_err(|e| e.to_string())?;
    match name {
        LIST => list_tool(m, &viewer, args).map_err(|e| e.to_string()),
        TIDY => tidy(m, cx, &viewer, args).map_err(|e| e.to_string()),
        _ => Err(format!("MCP error -32602: Tool {name} not found")),
    }
}

fn list_tool(m: &Modules, viewer: &Viewer, args: &Value) -> ApiResult<String> {
    let statuses = match args.get("status").and_then(Value::as_str) {
        Some("all") => Vec::new(),
        Some(s @ ("confirmed" | "candidate" | "stale")) => vec![s.to_string()],
        None => vec!["confirmed".into(), "candidate".into()],
        Some(_) => return Err(ApiError::invalid("status")),
    };
    let f = MemFilter {
        statuses,
        include_never: true,
        limit: 0,
    };
    // Workspace service callers can list shared memories, but have no personal profile.
    let member: bool = m.store.reader(|c| {
        Ok::<_, steve_store::StoreError>(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
            params![viewer.ws, viewer.user],
            |r| r.get(0),
        )?)
    })?;
    let profile = if member {
        Some(steve_modules::contacts::service::me_contact(
            m,
            &Caller {
                ws: viewer.ws.clone(),
                me: viewer.user.clone(),
            },
        )?)
    } else {
        None
    };
    let out = m.store.reader(|c| {
        let used = provenance::use_counts(c, &viewer.ws, "mem")?;
        let list = mems::all(c, viewer, &f)?;
        let mut out = Vec::with_capacity(list.len());
        for x in &list {
            let about: Vec<String> = x
                .about
                .iter()
                .map(|a| {
                    crate::repo::ref_name_for(c, viewer, a)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| a.id.clone())
                })
                .collect();
            let ranges: Vec<Value> = c.prepare_cached("SELECT conv, first_id, last_id, first_at, last_at, packet_id FROM mem_ranges WHERE workspace_id = ? AND mem_id = ? ORDER BY first_at, conv, first_id")?
                .query_map(params![viewer.ws, x.id], |r| Ok(json!({"conv": r.get::<_, String>(0)?, "first_id": r.get::<_, String>(1)?, "last_id": r.get::<_, String>(2)?,
                    "first_at": r.get::<_, i64>(3)?, "last_at": r.get::<_, i64>(4)?, "packet_id": r.get::<_, Option<String>>(5)?})))?
                .collect::<rusqlite::Result<_>>()?;
            let o = provenance::origin(c, x)?;
            let (n, last) = used.get(&x.id).copied().unwrap_or((0, 0));
            out.push(json!({
                "id": x.id, "kind": x.kind, "status": x.status, "text": x.text, "about": about, "index": {"about": x.about, "ranges": ranges},
                "source": if o["kind"] == "you" { json!("written by the user") } else { o["label"].clone() },
                "source_kind": o["kind"], "written_by": o["writer_name"],
                "created": iso(x.created), "updated": iso(x.updated),
                "used": n, "last_used": (n > 0).then(|| iso(last)),
            }));
        }
        Ok::<_, steve_store::StoreError>(out)
    })?;
    Ok(serde_json::to_string(&json!({ "memories": out, "profile": profile,
        "profile_guidance": "Preserve original notes verbatim. Extract soft facts about role, projects or habits only when supported by notes or memories. Use steve_contacts_remember with this profile id; it creates a candidate, never a confirmed fact. Do not re-propose existing or rejected facts. Use separate call (addressing the user) and call_external (introducing them); never infer either from the other." })).unwrap_or_default())
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn strs(args: &Value, k: &str) -> Vec<String> {
    args.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The memory `id`, visible to the viewer.
fn own(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<Mem> {
    mems::visible(c, viewer, id)?
        .ok_or_else(|| ApiError::rule(format!("No memory {id}. Use ids from steve_memory_list.")))
}

/// The run a change belongs to: the agent's item, else its session.
fn run_of(cx: &McpCx) -> String {
    cx.item_id
        .clone()
        .or_else(|| cx.session_id.clone())
        .unwrap_or_else(|| "agent".into())
}

#[allow(clippy::too_many_arguments)]
fn record(
    tx: &Tx,
    ws: &str,
    run: &str,
    writer: &str,
    op: &str,
    mem_id: &str,
    targets: &[String],
    before: &Value,
    after: &Value,
    reason: &str,
    state: &str,
) -> ApiResult<String> {
    let id = uid("mch");
    let now = steve_store::now();
    tx.prepare_cached(
        "INSERT INTO mem_changes (id, workspace_id, run_id, writer, op, mem_id, targets, before, after, reason, state, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        ws,
        run,
        writer,
        op,
        mem_id,
        serde_json::to_string(targets).unwrap_or_default(),
        before.to_string(),
        after.to_string(),
        reason,
        state,
        now,
        now
    ])?;
    base::audit(
        tx,
        ws,
        writer,
        &format!("mm.{op}"),
        mem_id,
        &json!({"change": id, "targets": targets}),
    )?;
    Ok(id)
}

fn set_status(tx: &Tx, id: &str, status: &str) -> ApiResult<()> {
    mems::patch(
        tx,
        id,
        &MemPatch {
            status: Some(status.into()),
            ..Default::default()
        },
    )?;
    sync::sync_mem(tx, id, steve_store::now())?;
    Ok(())
}

fn tidy(m: &Modules, cx: &McpCx, viewer: &Viewer, args: &Value) -> ApiResult<String> {
    let op = args
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !OPS.contains(&op.as_str()) {
        return Err(ApiError::rule(format!(
            "op must be one of {}.",
            OPS.join(", ")
        )));
    }
    let ids = strs(args, "ids");
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if ids.is_empty() || ids.len() > MAX_IDS {
        return Err(ApiError::rule(format!("ids: 1 to {MAX_IDS} memory ids.")));
    }
    if reason.is_empty() || reason.chars().count() > 500 {
        return Err(ApiError::rule(
            "reason: one sentence for the user (at most 500 characters).",
        ));
    }
    let run = run_of(cx);
    let ws = cx.ws.clone();
    let writer = cx.bot_id.clone();
    let out = m.store.transaction(|tx| -> ApiResult<String> {
        let list: Vec<Mem> = ids.iter().map(|i| own(tx, viewer, i)).collect::<ApiResult<_>>()?;
        match op.as_str() {
            "merge" => {
                if list.len() < 2 {
                    return Err(ApiError::rule("merge: at least two ids."));
                }
                if let Some(x) = list.iter().find(|x| x.status != "confirmed") {
                    return Err(ApiError::rule(format!("merge: {} is {}, only confirmed memories merge.", x.id, x.status)));
                }
                let text = args.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_string();
                if text.is_empty() || text.chars().count() > 2000 {
                    return Err(ApiError::rule("merge: text is the merged wording (1 to 2000 characters)."));
                }
                let mut about: Vec<ObjRef> = Vec::new();
                for a in list.iter().flat_map(|x| x.about.iter()) {
                    if !about.contains(a) {
                        about.push(a.clone());
                    }
                }
                let origin = Origin {
                    ws: ws.clone(),
                    kind: OriginKind::Tool,
                    id: run.clone(),
                    label: match &cx.item_id {
                        Some(i) => format!("{i} 的会话"),
                        None => "Agent 会话".into(),
                    },
                    user: cx.user.clone(),
                    writer: writer.clone(),
                    default_level: Level::Conclusion,
                    item_id: cx.item_id.clone(),
                    session_id: cx.session_id.clone(),
                };
                let level = list.iter().map(|x| x.level).max_by_key(|l| level_rank(*l)).unwrap_or(Level::Conclusion);
                let new = candidates::propose(
                    tx,
                    &origin,
                    &NewCandidate {
                        kind: list[0].kind.clone(),
                        text: text.clone(),
                        about,
                        level,
                        evidence: list.iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join(" / "),
                        supersedes: Some(list[0].id.clone()),
                        why: reason.clone(),
                        scope: String::new(),
                        owner_said: false,
                    },
                )?;
                for x in &list {
                    tx.prepare_cached("INSERT OR IGNORE INTO mem_supersedes (mem_id, old_id) VALUES (?, ?)")?
                        .execute(params![new.id, x.id])?;
                }
                let id = record(tx, &ws, &run, &writer, "merge", &new.id, &ids, &json!({}), &json!({"text": text}), &reason, "applied")?;
                Ok(format!("Merged into candidate {} (change {id}); the user confirms it, and then it replaces {}.", new.id, ids.join(", ")))
            }
            "resolve" => {
                let keep = args.get("keep").and_then(Value::as_str).unwrap_or_default();
                let k = own(tx, viewer, keep)
                    .map_err(|_| ApiError::rule("resolve: keep is the id of the memory that holds."))?;
                let retire: Vec<&Mem> = list.iter().filter(|x| x.id != k.id).collect();
                if retire.is_empty() {
                    return Err(ApiError::rule("resolve: ids are the memories that no longer hold."));
                }
                let before: serde_json::Map<String, Value> = retire.iter().map(|x| (x.id.clone(), json!(x.status))).collect();
                for x in &retire {
                    set_status(tx, &x.id, "stale")?;
                }
                let targets: Vec<String> = retire.iter().map(|x| x.id.clone()).collect();
                let id = record(tx, &ws, &run, &writer, "resolve", &k.id, &targets, &Value::Object(before), &json!({}), &reason, "applied")?;
                Ok(format!("Kept {}; {} no longer used (change {id}, the user can undo it).", k.id, targets.join(", ")))
            }
            "flag" => {
                if list.len() < 2 {
                    return Err(ApiError::rule("flag: the ids of at least two memories that contradict each other."));
                }
                let id = record(tx, &ws, &run, &writer, "flag", &ids[0], &ids[1..], &json!({}), &json!({}), &reason, "open")?;
                Ok(format!("Flagged for the user (change {id}); nothing was changed."))
            }
            "stale" => {
                let mut done = Vec::new();
                for x in &list {
                    if x.status == "stale" {
                        continue;
                    }
                    set_status(tx, &x.id, "stale")?;
                    done.push(record(tx, &ws, &run, &writer, "stale", &x.id, &[], &Value::Object(std::iter::once((x.id.clone(), json!(x.status))).collect()), &json!({"status": "stale"}), &reason, "applied")?);
                }
                Ok(format!("{} marked stale (changes {}).", done.len(), done.join(", ")))
            }
            "about" => {
                let x = &list[0];
                let name = args.get("about").and_then(Value::as_str).unwrap_or_default().trim();
                let names = Names::load(tx, viewer, None)?;
                let r = names.resolve(name).ok_or_else(|| ApiError::rule(format!("No person, organization, project or bot is called \"{name}\". Pass the exact name or id.")))?;
                if x.about.contains(&r) {
                    return Ok(format!("{} is already about {name}.", x.id));
                }
                tx.prepare_cached("INSERT OR IGNORE INTO mem_about (mem_id, workspace_id, object_type, object_id) VALUES (?, ?, ?, ?)")?
                    .execute(params![x.id, ws, r.kind, r.id])?;
                touch(tx, &ws, &x.id)?;
                let id = record(tx, &ws, &run, &writer, "about", &x.id, &[], &json!({}), &json!({"about": r}), &reason, "applied")?;
                Ok(format!("{} is now about {name} (change {id}).", x.id))
            }
            _ => {
                // source
                let x = &list[0];
                if x.source_type == "me" || x.source_id.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(ApiError::rule(format!("{} already says where it came from.", x.id)));
                }
                let item = args.get("item").and_then(Value::as_str).unwrap_or_default().trim().to_string();
                let found: Option<String> = tx
                    .prepare_cached("SELECT title FROM items WHERE id = ? AND workspace_id = ?")?
                    .query_row(params![item, ws], |r| r.get(0))
                    .optional()?;
                if item.is_empty() || found.is_none() {
                    return Err(ApiError::rule("source: item is the id of the item it came from (ST-12)."));
                }
                let label = format!("{item} 交付时");
                tx.execute(
                    "UPDATE mems SET source_type = 'item', source_id = ?, source_label = ?, updated = ? WHERE id = ?",
                    params![item, label, steve_store::now(), x.id],
                )?;
                touch(tx, &ws, &x.id)?;
                let before = json!({"type": x.source_type, "id": x.source_id, "label": x.source_label});
                let id = record(tx, &ws, &run, &writer, "source", &x.id, &[], &before, &json!({"type": "item", "id": item, "label": label}), &reason, "applied")?;
                Ok(format!("{} now says it came from {item} (change {id}).", x.id))
            }
        }
    })?;
    Ok(out)
}

fn level_rank(l: Level) -> u8 {
    // the most careful level of the merged ones holds for the merge
    match l {
        Level::Public => 0,
        Level::Conclusion => 1,
        Level::Reasoning => 2,
        Level::Never => 3,
    }
}

/// Re-indexes a memory whose `about` or source changed, and tells the pages.
fn touch(tx: &Tx, ws: &str, id: &str) -> ApiResult<()> {
    sync::sync_mem(tx, id, steve_store::now())?;
    tx.publish(ChangeEvent::Mm {
        id: id.into(),
        ws: ws.into(),
    });
    Ok(())
}

// ---------------------------------------------------------------- the change log on the page

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/kb/changes", get(changes_route))
        .route("/kb/changes/{id}/undo", post(undo_route))
        .route("/kb/changes/{id}/keep", post(keep_route))
        .route("/kb/steward", get(steward_get).put(steward_put))
}

/// Which schedule is the knowledge steward (`kb_marks`): the page sets it up from the Bot and schedule
/// templates and finds it here again. Only a pointer; the schedule itself is an ordinary one.
const STEWARD_MARK: &str = "steward:schedule";

#[derive(Debug, Deserialize)]
pub struct StewardBody {
    #[serde(default)]
    pub schedule_id: Option<String>,
}

async fn steward_get(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let id = m
            .store
            .reader(|c| crate::repo::mark(c, &who.ws, STEWARD_MARK))?;
        Ok(Json(json!({ "schedule_id": id.filter(|x| !x.is_empty()) })))
    })
    .await
}

async fn steward_put(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<StewardBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let id = b.schedule_id.unwrap_or_default();
        if id.chars().count() > 100 {
            return Err(ApiError::invalid("schedule_id"));
        }
        let now = m.clock.now();
        m.store
            .transaction(|tx| crate::repo::set_mark(tx, &who.ws, STEWARD_MARK, &id, now))?;
        Ok(Json(
            json!({ "schedule_id": (!id.is_empty()).then_some(id) }),
        ))
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
pub struct ChangesParams {
    pub run: Option<String>,
}

async fn changes_route(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<ChangesParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| changes(m, &who, p.run.as_deref()).map(Json))
        .await
}

async fn undo_route(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| undo(m, &who, &id).map(Json)).await
}

#[derive(Debug, Deserialize)]
pub struct KeepBody {
    /// The memory that holds; `null` keeps them all (the flag was wrong).
    #[serde(default)]
    pub keep: Option<String>,
}

async fn keep_route(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<KeepBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| keep(m, &who, &id, b.keep.as_deref()).map(Json))
        .await
}

struct Row {
    id: String,
    op: String,
    mem_id: String,
    targets: Vec<String>,
    before: Value,
    after: Value,
    reason: String,
    state: String,
    created: i64,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    let js = |s: String| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null);
    Ok(Row {
        id: r.get("id")?,
        op: r.get("op")?,
        mem_id: r.get("mem_id")?,
        targets: serde_json::from_str(&r.get::<_, String>("targets")?).unwrap_or_default(),
        before: js(r.get("before")?),
        after: js(r.get("after")?),
        reason: r.get("reason")?,
        state: r.get("state")?,
        created: r.get("created")?,
    })
}

fn row(c: &Connection, ws: &str, id: &str) -> ApiResult<Row> {
    c.prepare_cached("SELECT * FROM mem_changes WHERE id = ? AND workspace_id = ?")?
        .query_row([id, ws], from_row)
        .optional()?
        .ok_or_else(|| ApiError::not_found("改动"))
}

/// A merge's state follows its candidate: `pending` until answered, `confirmed`, or `dismissed` once
/// the candidate is gone; `undone` stays.
fn live_state(c: &Connection, r: &Row) -> ApiResult<String> {
    if r.op != "merge" || r.state == "undone" {
        return Ok(r.state.clone());
    }
    Ok(match mems::get(c, &r.mem_id)?.map(|m| m.status) {
        Some(s) if s == "candidate" => "pending".into(),
        Some(s) if s == "confirmed" => "confirmed".into(),
        Some(_) => "stale".into(),
        None => "dismissed".into(),
    })
}

fn mem_or_gone(c: &Connection, viewer: &Viewer, id: &str) -> ApiResult<Option<Value>> {
    Ok(match mems::get(c, id)? {
        Some(x) if x.workspace_id == viewer.ws && viewer.sees(&x.visibility, &x.created_by) => {
            Some(mem_json_for(c, viewer, &x))
        }
        Some(_) => None,
        None => Some(json!({"id": id, "gone": true})),
    })
}

fn run_json(c: &Connection, ws: &str, run: &str, writer: &str, at: i64) -> ApiResult<Value> {
    let item: Option<(String, String)> = c
        .prepare_cached("SELECT id, title FROM items WHERE id = ? AND workspace_id = ?")?
        .query_row(params![run, ws], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let href = match (&item, run.starts_with("ses_")) {
        (Some((i, _)), _) => format!("#/items/{i}"),
        (None, true) => format!("#/sessions/{run}"),
        _ => String::new(),
    };
    Ok(json!({
        "run_id": run, "href": href, "title": item.map(|x| x.1), "writer": writer,
        "writer_name": provenance::writer_name(c, writer)?, "at": at,
    }))
}

/// `GET /kb/changes[?run=]`: the latest runs that changed memories, newest first, each with its
/// changes: `[{run_id, href, title, writer, writer_name, at, changes: [{id, op, state, reason, mem,
/// targets, after, created}]}]`. Changes to memories the caller cannot see are left out.
pub fn changes(m: &Modules, who: &Caller, run: Option<&str>) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    m.store.reader(|c| {
        let runs: Vec<(String, String, i64)> = match run {
            Some(r) => c
                .prepare_cached("SELECT run_id, writer, MAX(created) FROM mem_changes WHERE workspace_id = ? AND run_id = ? GROUP BY run_id, writer")?
                .query_map([&who.ws, r], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?,
            None => c
                .prepare_cached("SELECT run_id, writer, MAX(created) AS at FROM mem_changes WHERE workspace_id = ? GROUP BY run_id, writer ORDER BY at DESC LIMIT ?")?
                .query_map(params![who.ws, RUNS_SHOWN as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?,
        };
        let mut out = Vec::new();
        for (run_id, writer, at) in runs {
            let rows: Vec<Row> = c
                .prepare_cached("SELECT * FROM mem_changes WHERE workspace_id = ? AND run_id = ? AND writer = ? ORDER BY created, rowid")?
                .query_map(params![who.ws, run_id, writer], from_row)?
                .collect::<rusqlite::Result<_>>()?;
            let mut list = Vec::new();
            for r in &rows {
                let Some(mem) = mem_or_gone(c, &viewer, &r.mem_id)? else { continue };
                let targets: Vec<Value> = r
                    .targets
                    .iter()
                    .filter_map(|t| mem_or_gone(c, &viewer, t).transpose())
                    .collect::<ApiResult<_>>()?;
                list.push(json!({
                    "id": r.id, "op": r.op, "state": live_state(c, r)?, "reason": r.reason, "mem": mem,
                    "targets": targets, "after": r.after, "created": r.created,
                }));
            }
            if list.is_empty() {
                continue;
            }
            let mut v = run_json(c, &who.ws, &run_id, &writer, at)?;
            v["changes"] = Value::Array(list);
            out.push(v);
        }
        Ok(Value::Array(out))
    })
}

fn set_state(tx: &Tx, id: &str, state: &str, before: Option<&Value>) -> ApiResult<()> {
    match before {
        Some(b) => tx.execute(
            "UPDATE mem_changes SET state = ?, before = ?, updated = ? WHERE id = ?",
            params![state, b.to_string(), steve_store::now(), id],
        )?,
        None => tx.execute(
            "UPDATE mem_changes SET state = ?, updated = ? WHERE id = ?",
            params![state, steve_store::now(), id],
        )?,
    };
    Ok(())
}

/// Puts back what `before` says each memory's status was (only those still `stale`).
fn restore_statuses(tx: &Tx, before: &Value) -> ApiResult<()> {
    if let Some(o) = before.as_object() {
        for (id, s) in o {
            if let Some(s) = s.as_str()
                && mems::get(tx, id)?.is_some_and(|x| x.status == "stale")
            {
                set_status(tx, id, s)?;
            }
        }
    }
    Ok(())
}

/// `POST /kb/changes/{id}/undo`: puts back what the change did. A merge still waiting is ignored
/// (its candidate goes); a confirmed merge gives the merged memories back and removes the merge.
pub fn undo(m: &Modules, who: &Caller, id: &str) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    let (r, state) = m.store.reader(|c| {
        let r = row(c, &who.ws, id)?;
        let s = live_state(c, &r)?;
        Ok::<_, ApiError>((r, s))
    })?;
    // a merge still waiting: ignoring its candidate goes through the 待确认 answer, decision and all
    if r.op == "merge" && state == "pending" {
        candidates::resolve_many(
            m,
            who,
            &candidates::ResolveBody {
                ids: vec![r.mem_id.clone()],
                choice: "no".into(),
            },
        )?;
    }
    m.store.transaction(|tx| {
        let state = live_state(tx, &r)?;
        if r.op == "flag" && state == "dismissed" {
            set_state(tx, &r.id, "open", None)?;
            return Ok(json!({"ok": true}));
        }
        if r.op == "merge" && state == "dismissed" {
            set_state(tx, &r.id, "undone", None)?;
            return Ok(json!({"ok": true}));
        }
        if matches!(state.as_str(), "undone" | "dismissed" | "open") {
            return Err(ApiError::conflict(steve_i18n::t!("knowledge.change.not_undoable")));
        }
        own(tx, &viewer, &r.mem_id).map_err(|_| ApiError::not_found(NOUN))?;
        match r.op.as_str() {
            "stale" | "resolve" => restore_statuses(tx, &r.before)?,
            "flag" => {
                // kept one: the others come back, and the question is open again
                restore_statuses(tx, &r.before)?;
                set_state(tx, &r.id, "open", Some(&json!({})))?;
                base::audit(tx, &who.ws, &who.me, "mm.change_undo", &r.mem_id, &json!({"change": r.id}))?;
                return Ok(json!({"ok": true}));
            }
            "about" => {
                if let Ok(a) = serde_json::from_value::<ObjRef>(r.after["about"].clone()) {
                    tx.execute(
                        "DELETE FROM mem_about WHERE mem_id = ? AND object_type = ? AND object_id = ?",
                        params![r.mem_id, a.kind, a.id],
                    )?;
                    touch(tx, &who.ws, &r.mem_id)?;
                }
            }
            "source" => {
                let b = &r.before;
                tx.execute(
                    "UPDATE mems SET source_type = ?, source_id = ?, source_label = ?, updated = ? WHERE id = ?",
                    params![
                        b["type"].as_str().unwrap_or("steve"),
                        b["id"].as_str(),
                        b["label"].as_str().unwrap_or_default(),
                        steve_store::now(),
                        r.mem_id
                    ],
                )?;
                touch(tx, &who.ws, &r.mem_id)?;
            }
            "merge" => {
                let olds: Vec<String> = tx
                    .prepare_cached("SELECT old_id FROM mem_supersedes WHERE mem_id = ?")?
                    .query_map([&r.mem_id], |x| x.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for o in &olds {
                    if mems::get(tx, o)?.is_some_and(|x| x.status == "stale") {
                        set_status(tx, o, "confirmed")?;
                    }
                }
                tx.execute("DELETE FROM mem_supersedes WHERE mem_id = ?", [&r.mem_id])?;
                tx.execute("DELETE FROM mem_candidates WHERE mem_id = ?", [&r.mem_id])?;
                mems::delete(tx, &r.mem_id)?;
                sync::sync_mem(tx, &r.mem_id, steve_store::now())?;
            }
            _ => {}
        }
        set_state(tx, &r.id, "undone", None)?;
        base::audit(tx, &who.ws, &who.me, "mm.change_undo", &r.mem_id, &json!({"change": r.id}))?;
        Ok(json!({"ok": true}))
    })
}

/// `POST /kb/changes/{id}/keep {keep}`: answers a flag — `keep` holds and the others become stale
/// (undoable), or `null`: they all hold.
pub fn keep(m: &Modules, who: &Caller, id: &str, keep: Option<&str>) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    m.store.transaction(|tx| {
        let r = row(tx, &who.ws, id)?;
        if r.op != "flag" || r.state != "open" {
            return Err(ApiError::conflict(steve_i18n::t!(
                "knowledge.change.answered"
            )));
        }
        let all: Vec<String> = std::iter::once(r.mem_id.clone())
            .chain(r.targets.iter().cloned())
            .collect();
        match keep {
            None => set_state(tx, &r.id, "dismissed", None)?,
            Some(k) if all.iter().any(|x| x == k) => {
                let mut before = serde_json::Map::new();
                for x in all.iter().filter(|x| x.as_str() != k) {
                    if let Ok(mm) = own(tx, &viewer, x)
                        && mm.status != "stale"
                    {
                        before.insert(mm.id.clone(), json!(mm.status));
                        set_status(tx, &mm.id, "stale")?;
                    }
                }
                set_state(tx, &r.id, "kept", Some(&Value::Object(before)))?;
            }
            Some(_) => return Err(ApiError::invalid("keep")),
        }
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "mm.flag_answer",
            &r.mem_id,
            &json!({"change": r.id, "keep": keep}),
        )?;
        Ok(json!({"ok": true}))
    })
}

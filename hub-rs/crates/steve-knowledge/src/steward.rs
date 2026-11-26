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
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::mems::{Mem, MemFilter, MemPatch, NewMem};
use steve_store::base::{Level, ObjRef, Viewer, Visibility};
use steve_store::knowledge::{
    CandidateRows, Documents, Memories, MemoryChange, MemoryChanges, NewMemoryChange,
};
use steve_store::{ItemReads, Members, MemoryIndex, Read, References, W, WorkspaceSettings};

use crate::candidates;
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
    let member = m.store.read(|c| {
        Members::new(c)
            .role(&viewer.ws, &viewer.user)
            .map(|role| role.is_some())
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
    let out = m.store.read(|c| {
        let used = provenance::use_counts(c, &viewer.ws, "mem")?;
        let list = Memories(c).all(viewer, &f)?;
        let mut out = Vec::with_capacity(list.len());
        for x in &list {
            let about: Vec<String> = x
                .about
                .iter()
                .map(|a| {
                    References(c).in_workspace(viewer,a)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| a.id.clone())
                })
                .collect();
            let ranges = MemoryIndex::new(c).get(&viewer.ws,&x.id)?.ranges;
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
fn own(c: &mut impl Read, viewer: &Viewer, id: &str) -> ApiResult<Mem> {
    Memories(c)
        .visible(viewer, id)?
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
    tx: &mut W<'_>,
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
    let id = MemoryChanges(tx).record(
        ws,
        NewMemoryChange {
            run,
            writer,
            op,
            mem: mem_id,
            targets,
            before,
            after,
            reason,
            state,
        },
    )?;
    tx.audit(
        ws,
        writer,
        &format!("mm.{op}"),
        mem_id,
        &json!({"change": id, "targets": targets}),
    )?;
    Ok(id)
}

fn set_status(tx: &mut W<'_>, id: &str, status: &str) -> ApiResult<()> {
    Memories(tx).patch(
        id,
        &MemPatch {
            status: Some(status.into()),
            ..Default::default()
        },
    )?;
    sync::sync_mem(tx, id, steve_store::now())?;
    Ok(())
}

struct TidyRequest {
    op: String,
    ids: Vec<String>,
    reason: String,
    run: String,
    ws: String,
    writer: String,
}

struct Tidying<'a, 'tx> {
    tx: &'a mut W<'tx>,
    cx: &'a McpCx,
    viewer: &'a Viewer,
    request: &'a TidyRequest,
    list: &'a [Mem],
    args: &'a Value,
}

fn merge_text(list: &[Mem], args: &Value) -> ApiResult<String> {
    if list.len() < 2 {
        return Err(ApiError::rule("merge: at least two ids."));
    }
    if let Some(x) = list.iter().find(|x| x.status != "confirmed") {
        return Err(ApiError::rule(format!(
            "merge: {} is {}, only confirmed memories merge.",
            x.id, x.status
        )));
    }
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() || text.chars().count() > 2000 {
        return Err(ApiError::rule(
            "merge: text is the merged wording (1 to 2000 characters).",
        ));
    }
    Ok(text)
}

impl Tidying<'_, '_> {
    fn merge(&mut self) -> ApiResult<String> {
        let tx = &mut *self.tx;
        let cx = self.cx;
        let args = self.args;
        let list = self.list;
        let ids = &self.request.ids;
        let ws = &self.request.ws;
        let run = &self.request.run;
        let writer = &self.request.writer;
        let reason = &self.request.reason;
        let text = merge_text(list, args)?;
        let mut about: Vec<ObjRef> = Vec::new();
        for a in list.iter().flat_map(|x| x.about.iter()) {
            if !about.contains(a) {
                about.push(a.clone());
            }
        }
        let level = list
            .iter()
            .map(|x| x.level)
            .max_by_key(|l| level_rank(*l))
            .unwrap_or(Level::Reasoning);
        let source_label = match &cx.item_id {
            Some(i) => format!("{i} 的会话"),
            None => "Agent 会话".into(),
        };
        let now = steve_store::now();
        let new = Memories(tx).create(
            ws,
            &NewMem {
                kind: list[0].kind.clone(),
                text: text.clone(),
                source_type: if cx.item_id.is_some() {
                    "item".into()
                } else {
                    "steve".into()
                },
                source_id: cx.item_id.clone().or_else(|| cx.session_id.clone()),
                source_label,
                visibility: Visibility::Private,
                level,
                status: "confirmed".into(),
                writer: writer.clone(),
                created_by: cx.user.clone(),
                about,
                ..Default::default()
            },
        )?;
        sync::sync_mem(tx, &new.id, now)?;
        for x in list {
            CandidateRows(tx).supersede(&new.id, &x.id)?;
            set_status(tx, &x.id, "stale")?;
        }
        let id = record(
            tx,
            ws,
            run,
            writer,
            "merge",
            &new.id,
            ids,
            &json!({}),
            &json!({"text": text}),
            reason,
            "applied",
        )?;
        Ok(format!(
            "Merged silently into {} (change {id}); replaced {} (undoable).",
            new.id,
            ids.join(", ")
        ))
    }

    fn resolve(&mut self) -> ApiResult<String> {
        let tx = &mut *self.tx;
        let viewer = self.viewer;
        let args = self.args;
        let list = self.list;
        let ws = &self.request.ws;
        let run = &self.request.run;
        let writer = &self.request.writer;
        let reason = &self.request.reason;
        let keep = args.get("keep").and_then(Value::as_str).unwrap_or_default();
        let k = own(tx, viewer, keep)
            .map_err(|_| ApiError::rule("resolve: keep is the id of the memory that holds."))?;
        let retire: Vec<&Mem> = list.iter().filter(|x| x.id != k.id).collect();
        if retire.is_empty() {
            return Err(ApiError::rule(
                "resolve: ids are the memories that no longer hold.",
            ));
        }
        let before: serde_json::Map<String, Value> = retire
            .iter()
            .map(|x| (x.id.clone(), json!(x.status)))
            .collect();
        for x in &retire {
            set_status(tx, &x.id, "stale")?;
        }
        let targets: Vec<String> = retire.iter().map(|x| x.id.clone()).collect();
        let id = record(
            tx,
            ws,
            run,
            writer,
            "resolve",
            &k.id,
            &targets,
            &Value::Object(before),
            &json!({}),
            reason,
            "applied",
        )?;
        Ok(format!(
            "Kept {}; {} no longer used (change {id}, the user can undo it).",
            k.id,
            targets.join(", ")
        ))
    }

    fn flag(&mut self) -> ApiResult<String> {
        let tx = &mut *self.tx;
        let list = self.list;
        let ids = &self.request.ids;
        let ws = &self.request.ws;
        let run = &self.request.run;
        let writer = &self.request.writer;
        let reason = &self.request.reason;
        if list.len() < 2 {
            return Err(ApiError::rule(
                "flag: the ids of at least two memories that contradict each other.",
            ));
        }
        let id = record(
            tx,
            ws,
            run,
            writer,
            "flag",
            &ids[0],
            &ids[1..],
            &json!({}),
            &json!({}),
            reason,
            "open",
        )?;
        Ok(format!(
            "Flagged for the user (change {id}); nothing was changed."
        ))
    }

    fn stale(&mut self) -> ApiResult<String> {
        let tx = &mut *self.tx;
        let list = self.list;
        let ws = &self.request.ws;
        let run = &self.request.run;
        let writer = &self.request.writer;
        let reason = &self.request.reason;
        let mut done = Vec::new();
        for x in list {
            if x.status == "stale" {
                continue;
            }
            set_status(tx, &x.id, "stale")?;
            done.push(record(
                tx,
                ws,
                run,
                writer,
                "stale",
                &x.id,
                &[],
                &Value::Object(std::iter::once((x.id.clone(), json!(x.status))).collect()),
                &json!({"status": "stale"}),
                reason,
                "applied",
            )?);
        }
        Ok(format!(
            "{} marked stale (changes {}).",
            done.len(),
            done.join(", ")
        ))
    }

    fn about(&mut self) -> ApiResult<String> {
        let tx = &mut *self.tx;
        let viewer = self.viewer;
        let args = self.args;
        let list = self.list;
        let ws = &self.request.ws;
        let run = &self.request.run;
        let writer = &self.request.writer;
        let reason = &self.request.reason;
        let x = &list[0];
        let name = args
            .get("about")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let names = Names::load(tx, viewer, None)?;
        let r = names.resolve(name).ok_or_else(|| ApiError::rule(format!("No person, organization, project or bot is called \"{name}\". Pass the exact name or id.")))?;
        if x.about.contains(&r) {
            return Ok(format!("{} is already about {name}.", x.id));
        }
        Memories(tx).add_subject(ws, &x.id, &r)?;
        touch(tx, ws, &x.id)?;
        let id = record(
            tx,
            ws,
            run,
            writer,
            "about",
            &x.id,
            &[],
            &json!({}),
            &json!({"about": r}),
            reason,
            "applied",
        )?;
        Ok(format!("{} is now about {name} (change {id}).", x.id))
    }

    fn source(&mut self) -> ApiResult<String> {
        let tx = &mut *self.tx;
        let args = self.args;
        let list = self.list;
        let ws = &self.request.ws;
        let run = &self.request.run;
        let writer = &self.request.writer;
        let reason = &self.request.reason;
        // source
        let x = &list[0];
        if x.source_type == "me" || x.source_id.as_deref().is_some_and(|s| !s.is_empty()) {
            return Err(ApiError::rule(format!(
                "{} already says where it came from.",
                x.id
            )));
        }
        let item = args
            .get("item")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let found = ItemReads(tx).title(ws, &item)?;
        if item.is_empty() || found.is_none() {
            return Err(ApiError::rule(
                "source: item is the id of the item it came from (ST-12).",
            ));
        }
        let label = format!("{item} 交付时");
        Memories(tx).set_source(&x.id, "item", Some(&item), &label)?;
        touch(tx, ws, &x.id)?;
        let before = json!({"type": x.source_type, "id": x.source_id, "label": x.source_label});
        let id = record(
            tx,
            ws,
            run,
            writer,
            "source",
            &x.id,
            &[],
            &before,
            &json!({"type": "item", "id": item, "label": label}),
            reason,
            "applied",
        )?;
        Ok(format!(
            "{} now says it came from {item} (change {id}).",
            x.id
        ))
    }
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
    let request = TidyRequest {
        op,
        ids,
        reason,
        run,
        ws,
        writer,
    };
    let out = m.store.write(|tx| -> ApiResult<String> {
        let list: Vec<Mem> = request
            .ids
            .iter()
            .map(|i| own(tx, viewer, i))
            .collect::<ApiResult<_>>()?;
        let mut tidy = Tidying {
            tx,
            cx,
            viewer,
            request: &request,
            list: &list,
            args,
        };
        match request.op.as_str() {
            "merge" => tidy.merge(),
            "resolve" => tidy.resolve(),
            "flag" => tidy.flag(),
            "stale" => tidy.stale(),
            "about" => tidy.about(),
            _ => tidy.source(),
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
fn touch(tx: &mut W<'_>, ws: &str, id: &str) -> ApiResult<()> {
    sync::sync_mem(tx, id, steve_store::now())?;
    tx.emit(steve_types::DomainEvent::from(
        steve_types::ConfigEvent::Mm {
            id: {
                let value: String = id.into();
                steve_types::MemoryId::from(value)
            },
            ws: {
                let value: String = ws.into();
                steve_types::WorkspaceId::from(value)
            },
        },
    ));
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
pub const STEWARD_MARK: &str = "steward:schedule";
/// Watermark of the newest steward run (`kb_marks`).
pub const LAST_RUN_MARK: &str = "steward:last_run_at";
/// Workspace setting key for the deterministic steward trigger threshold.
pub const THRESHOLD_SETTING: &str = "knowledge.steward.threshold";
/// Default minimum number of new candidates + conflicts before a scheduled tidy creates an item.
pub const DEFAULT_THRESHOLD: usize = 3;
pub const MIN_THRESHOLD: usize = 1;
pub const MAX_THRESHOLD: usize = 100;

/// Signals counted since the last steward run to decide whether a scheduled tidy should fire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StewardSignals {
    pub new_candidates: usize,
    pub conflicts: usize,
}

impl StewardSignals {
    pub fn total(self) -> usize {
        self.new_candidates.saturating_add(self.conflicts)
    }
}

/// Deterministic decision of [`check_schedule_trigger`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StewardGate {
    /// The flow is not the workspace's knowledge steward schedule.
    NotSteward,
    /// Proceed with the run (`steward:last_run_at` was advanced to `now`).
    Run { reason: &'static str },
    /// Skip this scheduled run (nothing to tidy or below threshold).
    Skip {
        reason: &'static str,
        label: String,
        signals: StewardSignals,
        threshold: usize,
    },
}

/// Configured threshold for `ws`, falling back to [`DEFAULT_THRESHOLD`].
pub fn threshold(r: &mut impl Read, ws: &str) -> ApiResult<usize> {
    let raw = WorkspaceSettings(r).get(ws, THRESHOLD_SETTING)?;
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| (MIN_THRESHOLD..=MAX_THRESHOLD).contains(n));
    Ok(parsed.unwrap_or(DEFAULT_THRESHOLD))
}

/// Persists a validated trigger threshold for `ws`.
pub fn set_threshold(w: &mut W<'_>, ws: &str, value: usize, now: i64) -> ApiResult<usize> {
    if !(MIN_THRESHOLD..=MAX_THRESHOLD).contains(&value) {
        return Err(ApiError::invalid("threshold"));
    }
    WorkspaceSettings(w).set(ws, THRESHOLD_SETTING, &value.to_string(), now)?;
    Ok(value)
}

fn last_run_at(r: &mut impl Read, ws: &str, flow_id: Option<&str>) -> ApiResult<i64> {
    if let Some(mark) = Documents(r).mark(ws, LAST_RUN_MARK)?
        && let Ok(ts) = mark.trim().parse::<i64>()
        && ts > 0
    {
        return Ok(ts);
    }
    if let Some(fid) = flow_id.filter(|s| !s.is_empty())
        && let Some(ts) = MemoryChanges(r).last_steward_run_at(ws, fid)?
    {
        return Ok(ts);
    }
    Ok(0)
}

/// Current pending tidy signals since the last steward run, plus that watermark timestamp.
pub fn signals(
    r: &mut impl Read,
    ws: &str,
    flow_id: Option<&str>,
) -> ApiResult<(StewardSignals, i64)> {
    let since = last_run_at(r, ws, flow_id)?;
    let new_candidates = Memories(r).new_since(ws, since)?;
    let conflicts = Memories(r)
        .conflicting_candidates_since(ws, since)?
        .saturating_add(MemoryChanges(r).open_flags_since(ws, since)?);
    Ok((
        StewardSignals {
            new_candidates,
            conflicts,
        },
        since,
    ))
}

/// Deterministic trigger check invoked by the flow scheduler before starting a schedule run.
/// - Non-steward schedules return [`StewardGate::NotSteward`].
/// - Manual runs always proceed and advance `steward:last_run_at`.
/// - Scheduled runs proceed only when `new_candidates + conflicts >= threshold`; otherwise they
///   are skipped and metered (`steve_knowledge_steward_runs_total`).
pub fn check_schedule_trigger(
    w: &mut W<'_>,
    ws: &str,
    flow_id: &str,
    manual: bool,
    now: i64,
) -> ApiResult<StewardGate> {
    let Some(steward_id) = Documents(w)
        .mark(ws, STEWARD_MARK)?
        .filter(|s| !s.is_empty())
    else {
        return Ok(StewardGate::NotSteward);
    };
    if steward_id != flow_id {
        return Ok(StewardGate::NotSteward);
    }
    if manual {
        Documents(w).set_mark(ws, LAST_RUN_MARK, &now.to_string(), now)?;
        metrics::counter!(
            "steve_knowledge_steward_runs_total",
            "outcome" => "triggered",
            "reason" => "manual"
        )
        .increment(1);
        return Ok(StewardGate::Run { reason: "manual" });
    }
    let th = threshold(w, ws)?;
    let (sig, _) = signals(w, ws, Some(flow_id))?;
    let total = sig.total();
    if total == 0 {
        metrics::counter!(
            "steve_knowledge_steward_runs_total",
            "outcome" => "skipped",
            "reason" => "nothing_to_tidy"
        )
        .increment(1);
        return Ok(StewardGate::Skip {
            reason: "nothing_to_tidy",
            label: steve_i18n::t!("knowledge.steward.skip_nothing"),
            signals: sig,
            threshold: th,
        });
    }
    if total < th {
        metrics::counter!(
            "steve_knowledge_steward_runs_total",
            "outcome" => "skipped",
            "reason" => "below_threshold"
        )
        .increment(1);
        return Ok(StewardGate::Skip {
            reason: "below_threshold",
            label: steve_i18n::t!(
                "knowledge.steward.skip_below",
                count = total,
                threshold = th
            ),
            signals: sig,
            threshold: th,
        });
    }
    Documents(w).set_mark(ws, LAST_RUN_MARK, &now.to_string(), now)?;
    metrics::counter!(
        "steve_knowledge_steward_runs_total",
        "outcome" => "triggered",
        "reason" => "threshold"
    )
    .increment(1);
    Ok(StewardGate::Run {
        reason: "threshold",
    })
}

fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

#[derive(Debug, Deserialize)]
pub struct StewardBody {
    #[serde(default, deserialize_with = "nullable")]
    pub schedule_id: Option<Option<String>>,
    #[serde(default)]
    pub threshold: Option<usize>,
}

fn steward_status(r: &mut impl Read, ws: &str) -> ApiResult<Value> {
    let schedule_id = Documents(r)
        .mark(ws, STEWARD_MARK)?
        .filter(|x| !x.is_empty());
    let th = threshold(r, ws)?;
    let (sig, last_run) = signals(r, ws, schedule_id.as_deref())?;
    let skipped_runs = match schedule_id.as_deref() {
        Some(id) => MemoryChanges(r).skipped_steward_runs(ws, id)?,
        None => 0,
    };
    Ok(json!({
        "schedule_id": schedule_id,
        "threshold": th,
        "signals": {
            "new_candidates": sig.new_candidates,
            "conflicts": sig.conflicts,
            "total": sig.total(),
        },
        "last_run_at": (last_run > 0).then_some(last_run),
        "skipped_runs": skipped_runs,
    }))
}

async fn steward_get(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| crate::read_api(&m.store, |c| steward_status(c, &who.ws)).map(Json))
        .await
}

async fn steward_put(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<StewardBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        if let Some(Some(ref id)) = b.schedule_id
            && id.chars().count() > 100
        {
            return Err(ApiError::invalid("schedule_id"));
        }
        let now = m.clock.now();
        m.store.write(|tx| {
            if let Some(sched) = &b.schedule_id {
                let val = sched.as_deref().unwrap_or("");
                Documents(tx).set_mark(&who.ws, STEWARD_MARK, val, now)?;
            }
            if let Some(th) = b.threshold {
                set_threshold(tx, &who.ws, th, now)?;
            }
            Ok::<_, ApiError>(())
        })?;
        crate::read_api(&m.store, |c| steward_status(c, &who.ws)).map(Json)
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

fn row(c: &mut impl Read, ws: &str, id: &str) -> ApiResult<MemoryChange> {
    MemoryChanges(c)
        .get(ws, id)?
        .ok_or_else(|| ApiError::not_found("改动"))
}

/// A merge's state follows its candidate: `pending` until answered, `confirmed`, or `dismissed` once
/// the candidate is gone; `undone` stays.
fn live_state(c: &mut impl Read, r: &MemoryChange) -> ApiResult<String> {
    if r.op != "merge" || r.state == "undone" {
        return Ok(r.state.clone());
    }
    Ok(match Memories(c).get(&r.mem_id)?.map(|m| m.status) {
        Some(s) if s == "candidate" => "pending".into(),
        Some(s) if s == "confirmed" => "confirmed".into(),
        Some(_) => "stale".into(),
        None => "dismissed".into(),
    })
}

fn mem_or_gone(c: &mut impl Read, viewer: &Viewer, id: &str) -> ApiResult<Option<Value>> {
    Ok(match Memories(c).get(id)? {
        Some(x) if x.workspace_id == viewer.ws && viewer.sees(&x.visibility, &x.created_by) => {
            Some(mem_json_for(c, viewer, &x))
        }
        Some(_) => None,
        None => Some(json!({"id": id, "gone": true})),
    })
}

fn run_json(c: &mut impl Read, ws: &str, run: &str, writer: &str, at: i64) -> ApiResult<Value> {
    let item = ItemReads(c).title(ws, run)?.map(|title| (run, title));
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
    crate::read_api(&m.store, |c| {
        let runs = MemoryChanges(c).runs(&who.ws, run, RUNS_SHOWN)?;
        let mut out = Vec::new();
        for (run_id, writer, at) in runs {
            let rows = MemoryChanges(c).of_run(&who.ws, &run_id, &writer)?;
            let mut list = Vec::new();
            for r in &rows {
                let Some(mem) = mem_or_gone(c, &viewer, &r.mem_id)? else {
                    continue;
                };
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

fn set_state(tx: &mut W<'_>, id: &str, state: &str, before: Option<&Value>) -> ApiResult<()> {
    Ok(MemoryChanges(tx).set_state(id, state, before)?)
}

/// Puts back what `before` says each memory's status was (only those still `stale`).
fn restore_statuses(tx: &mut W<'_>, before: &Value) -> ApiResult<()> {
    if let Some(o) = before.as_object() {
        for (id, s) in o {
            if let Some(s) = s.as_str()
                && Memories(tx).get(id)?.is_some_and(|x| x.status == "stale")
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
    let (r, state) = crate::read_api(&m.store, |c| {
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
    m.store.write(|tx| {
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
            return Err(ApiError::conflict(steve_i18n::t!(
                "knowledge.change.not_undoable"
            )));
        }
        own(tx, &viewer, &r.mem_id).map_err(|_| ApiError::not_found(NOUN))?;
        match r.op.as_str() {
            "stale" | "resolve" => restore_statuses(tx, &r.before)?,
            "flag" => {
                // kept one: the others come back, and the question is open again
                restore_statuses(tx, &r.before)?;
                set_state(tx, &r.id, "open", Some(&json!({})))?;
                tx.audit(
                    &who.ws,
                    &who.me,
                    "mm.change_undo",
                    &r.mem_id,
                    &json!({"change": r.id}),
                )?;
                return Ok(json!({"ok": true}));
            }
            "about" => {
                if let Ok(a) = serde_json::from_value::<ObjRef>(r.after["about"].clone()) {
                    Memories(tx).remove_subject(&r.mem_id, &a)?;
                    touch(tx, &who.ws, &r.mem_id)?;
                }
            }
            "source" => {
                let b = &r.before;
                Memories(tx).set_source(
                    &r.mem_id,
                    b["type"].as_str().unwrap_or("steve"),
                    b["id"].as_str(),
                    b["label"].as_str().unwrap_or_default(),
                )?;
                touch(tx, &who.ws, &r.mem_id)?;
            }
            "merge" => {
                let olds = CandidateRows(tx).supersedes(&r.mem_id)?;
                for o in &olds {
                    if Memories(tx).get(o)?.is_some_and(|x| x.status == "stale") {
                        set_status(tx, o, "confirmed")?;
                    }
                }
                CandidateRows(tx).remove(&r.mem_id)?;
                Memories(tx).delete(&r.mem_id)?;
                sync::sync_mem(tx, &r.mem_id, steve_store::now())?;
            }
            _ => {}
        }
        set_state(tx, &r.id, "undone", None)?;
        tx.audit(
            &who.ws,
            &who.me,
            "mm.change_undo",
            &r.mem_id,
            &json!({"change": r.id}),
        )?;
        Ok(json!({"ok": true}))
    })
}

/// `POST /kb/changes/{id}/keep {keep}`: answers a flag — `keep` holds and the others become stale
/// (undoable), or `null`: they all hold.
pub fn keep(m: &Modules, who: &Caller, id: &str, keep: Option<&str>) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    m.store.write(|tx| {
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
        tx.audit(
            &who.ws,
            &who.me,
            "mm.flag_answer",
            &r.mem_id,
            &json!({"change": r.id, "keep": keep}),
        )?;
        Ok(json!({"ok": true}))
    })
}

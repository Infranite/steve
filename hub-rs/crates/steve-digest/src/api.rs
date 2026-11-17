//! Owner-facing base APIs, relative to `/api`. Raw conversation data is not a workspace-wide feed.
//! The ordinary digest Bot consumes the same store through `tools`; schedule binding is Hub wiring.
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::{ObjRef, mems};

use crate::{ConvRef, changes, conv, matter, memindex, profile, queue, summary, todo, window};

pub fn routes() -> Router<Modules> {
    Router::new()
        .route(
            "/digest/conversations/{conv}",
            get(conversation).patch(patch_profile),
        )
        .route("/digest/conversations/{conv}/cut", post(cut))
        .route("/digest/packets", get(packets))
        .route("/digest/packets/{id}", get(packet))
        .route("/digest/packets/{id}/retry", post(retry_packet))
        .route("/digest/matters", get(matters).post(new_matter))
        .route("/digest/matters/{id}", get(read_matter).patch(edit_matter))
        .route("/digest/matters/{id}/relate", post(relate_matter))
        .route("/digest/todos", get(todos).post(new_todo))
        .route(
            "/digest/todos/{id}",
            get(read_todo).post(todo_state).patch(edit_todo),
        )
        .route("/digest/todos/{id}/items", post(link_todo_item))
        .route(
            "/digest/memories/{id}/index",
            get(memory_index).post(link_memory),
        )
        .route("/digest/changes", get(change_list))
        .route("/digest/changes/{id}/undo", post(undo))
}

fn owner(m: &Modules, who: &Caller) -> ApiResult<()> {
    let ok: bool = m.store.reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ? AND role = 'owner')",
        params![who.ws, who.me], |r| r.get(0))?))?;
    if !ok {
        return Err(ApiError::Forbidden(steve_i18n::t!("digest.owner_only")));
    }
    Ok(())
}

/// Todos are every member's own: any member of the workspace, for their own todos only.
fn member(m: &Modules, who: &Caller) -> ApiResult<()> {
    let ok: bool =
        m.store.reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
        params![who.ws, who.me], |r| r.get(0))?)
        })?;
    if !ok {
        return Err(ApiError::Forbidden(steve_i18n::t!("digest.todo.not_yours")));
    }
    Ok(())
}

/// A todo value refused with its reason; other storage errors as [`input_error`].
fn todo_error(e: steve_store::StoreError) -> ApiError {
    match todo::message(&e) {
        Some(m) => ApiError::rule(m),
        None => input_error(e),
    }
}

/// The caller's own todo `id`, else 404 (another member's todo is not there for them).
fn own_todo(c: &rusqlite::Connection, who: &Caller, id: &str) -> ApiResult<()> {
    if todo::owner_of(c, &who.ws, id)?.as_deref() != Some(who.me.as_str()) {
        return Err(ApiError::not_found(steve_i18n::t!("digest.noun.todo")));
    }
    Ok(())
}

fn cv(key: &str) -> ApiResult<ConvRef> {
    ConvRef::parse(key).ok_or_else(|| ApiError::invalid("conv"))
}

fn input_error(e: steve_store::StoreError) -> ApiError {
    match e {
        steve_store::StoreError::Hook(_) => ApiError::invalid("reference_or_value"),
        e => e.into(),
    }
}

fn refused(e: matter::Refused) -> ApiError {
    match e {
        matter::Refused::NotFound(_) => ApiError::not_found("matter"),
        matter::Refused::Invalid(f) => ApiError::invalid(f),
        matter::Refused::NotSame(_, _) => ApiError::rule(steve_i18n::t!("digest.not_same")),
        matter::Refused::Store(e) => e.into(),
    }
}

async fn conversation(
    State(m): State<Modules>,
    who: Caller,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let cv = cv(&key)?;
        m.store.reader(|c| {
            let info = conv::info(c, &who.ws, &cv)?.ok_or_else(|| ApiError::not_found("conversation"))?;
            Ok(Json(json!({"conversation": info, "profile": profile::json(&profile::profile(c, &who.ws, &info)?),
                "cursor": window::cursor(c, &who.ws, &cv)?, "summary": summary::latest(c, &who.ws, &cv)?,
                "queue": queue::stats(c, &who.ws, Some(&key))?})))
        })
    }).await
}

async fn patch_profile(
    State(m): State<Modules>,
    who: Caller,
    Path(key): Path<String>,
    Body(p): Body<profile::Patch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let cv = cv(&key)?;
        m.store.transaction(|tx| {
            if conv::info(tx, &who.ws, &cv)?.is_none() {
                return Err(ApiError::not_found("conversation"));
            }
            let rev = profile::apply(tx, &who.ws, &cv, &p, m.clock.now())
                .map_err(|e| match e {
                    Ok(profile::Bad(f)) => ApiError::invalid(f),
                    Err(e) => e.into(),
                })?
                .ok_or_else(|| ApiError::Conflict(steve_i18n::t!("digest.profile_moved")))?;
            Ok(Json(json!({"revision": rev})))
        })
    })
    .await
}

async fn cut(
    State(m): State<Modules>,
    who: Caller,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let cv = cv(&key)?;
        if m.store.reader(|c| conv::info(c, &who.ws, &cv))?.is_none() {
            return Err(ApiError::not_found("conversation"));
        }
        Ok(Json(json!(window::cut_now(
            &m.store,
            &who.ws,
            &cv,
            m.clock.now()
        )?)))
    })
    .await
}

#[derive(Default, Deserialize)]
struct List {
    state: Option<String>,
    conv: Option<String>,
    query: Option<String>,
    run: Option<String>,
    packet: Option<String>,
    limit: Option<usize>,
    kind: Option<String>,
    parent: Option<String>,
}
impl List {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(50).clamp(1, 500)
    }
}

async fn packets(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<List>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let packets = m.store.reader(|c| {
            let mut out = Vec::new();
            for packet in queue::list(c, &who.ws, p.state.as_deref(), p.conv.as_deref(), p.limit())?
            {
                if let Some(cv) = ConvRef::parse(&packet.conv) {
                    if conv::owner_sees(c, &who.ws, &cv)? {
                        out.push(packet);
                    }
                }
            }
            Ok::<_, steve_store::StoreError>(out)
        })?;
        Ok(Json(json!({"packets": packets})))
    })
    .await
}

async fn packet(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        m.store.reader(|c| {
            let p = queue::get(c, &who.ws, &id)?.ok_or_else(|| ApiError::not_found("packet"))?;
            if !conv::owner_sees(c, &who.ws, &cv(&p.conv)?)? {
                return Err(ApiError::not_found("packet"));
            }
            Ok(Json(json!({"packet": p, "chunks": queue::chunks(c, &id)?})))
        })
    })
    .await
}

async fn retry_packet(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        m.store.transaction(|tx| {
            if queue::get(tx, &who.ws, &id)?.is_none() {
                return Err(ApiError::not_found("packet"));
            }
            let changed = queue::retry(tx, &who.ws, &id, m.clock.now())?;
            Ok(Json(json!({"changed": changed})))
        })
    })
    .await
}

async fn matters(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<List>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let f = matter::Filter {
            states: p.state.clone().into_iter().collect(),
            query: p.query.clone(),
            limit: p.limit(),
            subject: p.conv.as_deref().map(cv).transpose()?.map(|cv| cv.obj()),
        };
        Ok(Json(
            json!({"matters": m.store.reader(|c| matter::list(c, &who.ws, &f))?}),
        ))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatterBody {
    title: Option<String>,
    state: Option<String>,
    summary: Option<String>,
}
async fn new_matter(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<MatterBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let title = b.title.ok_or_else(|| ApiError::invalid("title"))?;
        let id = m
            .store
            .transaction(|tx| {
                let now = m.clock.now();
                let id = matter::create(tx, &who.ws, &title, &who.me, now)?;
                matter::edit(
                    tx,
                    &who.ws,
                    &id,
                    b.state.as_deref(),
                    None,
                    b.summary.as_deref(),
                    &who.me,
                    now,
                )?;
                Ok::<_, matter::Refused>(id)
            })
            .map_err(refused)?;
        Ok(Json(json!({"id": id})))
    })
    .await
}
async fn read_matter(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let viewer = m.viewer(&who)?;
        Ok(Json(
            m.store
                .reader(|c| matter::get_for(c, &viewer, &id))?
                .ok_or_else(|| ApiError::not_found("matter"))?,
        ))
    })
    .await
}
async fn edit_matter(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<MatterBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        m.store
            .transaction(|tx| {
                matter::edit(
                    tx,
                    &who.ws,
                    &id,
                    b.state.as_deref(),
                    b.title.as_deref(),
                    b.summary.as_deref(),
                    &who.me,
                    m.clock.now(),
                )
            })
            .map_err(refused)?;
        Ok(Json(json!({"ok": true})))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Relate {
    op: String,
    other: Option<String>,
    title: Option<String>,
    #[serde(default)]
    segments: Vec<String>,
    object: Option<ObjRef>,
    #[serde(default)]
    force: bool,
}
async fn relate_matter(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<Relate>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let now = m.clock.now();
        let out = m.store.transaction(|tx| {
            match b.op.as_str() {
                "merge" | "not_same" => {
                    let other = b.other.as_deref().ok_or(matter::Refused::Invalid("other"))?;
                    if b.op == "merge" { matter::merge(tx, &who.ws, &id, other, &who.me, b.force, now)?; }
                    else { matter::not_same(tx, &who.ws, &id, other, &who.me, now)?; }
                    Ok(json!({"ok": true}))
                }
                "split" => Ok(json!({"id": matter::split(tx, &who.ws, &id, &b.segments, b.title.as_deref().unwrap_or(""), &who.me, now)?})),
                "link" | "unlink" => Ok(json!({"changed": matter::link(tx, &who.ws, &id, b.object.as_ref().ok_or(matter::Refused::Invalid("object"))?, b.op == "link", &who.me, now)?})),
                _ => Err(matter::Refused::Invalid("op")),
            }
        }).map_err(refused)?;
        Ok(Json(out))
    }).await
}

async fn todos(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<List>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        member(m, &who)?;
        let f = todo::Filter {
            states: p.state.clone().into_iter().collect(),
            kind: p.kind.clone(),
            parent: p.parent.clone(),
            limit: p.limit(),
        };
        Ok(Json(
            json!({"todos": m.store.reader(|c| todo::list_by(c, &who.ws, &who.me, &f))?}),
        ))
    })
    .await
}
async fn read_todo(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        member(m, &who)?;
        m.store.reader(|c| {
            own_todo(c, &who, &id)?;
            let mut t = todo::get(c, &who.ws, &id)?
                .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
            if t["kind"] == "goal" {
                let f = todo::Filter {
                    parent: Some(id.clone()),
                    limit: 500,
                    ..Default::default()
                };
                t["children"] = json!(todo::list_by(c, &who.ws, &who.me, &f)?);
            }
            Ok(Json(t))
        })
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoBody {
    text: String,
    matter: Option<String>,
    due: Option<i64>,
    due_date: Option<String>,
    kind: Option<String>,
    parent: Option<String>,
    target_date: Option<String>,
    remind_at: Option<i64>,
    #[serde(default)]
    pri: i64,
    #[serde(default)]
    star: bool,
    recur: Option<String>,
    #[serde(default)]
    refs: Vec<(ConvRef, String)>,
    #[serde(default)]
    parties: Vec<String>,
}
async fn new_todo(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<TodoBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        member(m, &who)?;
        let now = m.clock.now();
        let (id, change) = m.store.transaction(|tx| {
            let id = todo::create(
                tx,
                &todo::NewTodo {
                    ws: &who.ws,
                    owner: &who.me,
                    text: &b.text,
                    matter: b.matter.as_deref(),
                    due: b.due,
                    due_date: b.due_date.as_deref(),
                    kind: b.kind.as_deref().unwrap_or("todo"),
                    parent: b.parent.as_deref(),
                    target_date: b.target_date.as_deref(),
                    remind_at: b.remind_at,
                    pri: b.pri,
                    star: b.star,
                    recur: b.recur.as_deref(),
                    source: "you",
                    writer: &who.me,
                    refs: b.refs,
                    parties: b.parties,
                    ..Default::default()
                },
                now,
            )
            .map_err(todo_error)?;
            let change = changes::record(
                tx,
                &who.ws,
                &who.me,
                &who.me,
                "todo",
                &id,
                None,
                &json!({}),
                &json!({"todo": id, "updated": now}),
                now,
            )?;
            Ok::<_, ApiError>((id, change))
        })?;
        Ok(Json(json!({"id": id, "change": change})))
    })
    .await
}
async fn edit_todo(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<todo::Patch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        member(m, &who)?;
        let now = m.clock.now();
        let change = m.store.transaction(|tx| {
            own_todo(tx, &who, &id)?;
            let (before, after) = todo::edit(tx, &who.ws, &id, &p, now)
                .map_err(todo_error)?
                .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
            Ok::<_, ApiError>(changes::record(
                tx,
                &who.ws,
                &who.me,
                &who.me,
                "todo_edit",
                &id,
                None,
                &json!(before),
                &json!({"todo": id, "fields": after, "updated": now}),
                now,
            )?)
        })?;
        Ok(Json(json!({"ok": true, "change": change})))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoState {
    state: String,
    item: Option<String>,
}
async fn todo_state(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<TodoState>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        member(m, &who)?;
        if !todo::STATES.contains(&b.state.as_str()) || (b.state == "handed") != b.item.is_some() {
            return Err(ApiError::invalid("state"));
        }
        let now = m.clock.now();
        let (change, next) = m.store.transaction(|tx| {
            own_todo(tx, &who, &id)?;
            set_todo_state(
                tx,
                &who.ws,
                &who.me,
                &who.me,
                &id,
                &b.state,
                b.item.as_deref(),
                now,
            )
        })?;
        Ok(Json(json!({"ok": true, "change": change, "next": next})))
    })
    .await
}

/// Sets a todo's state (a recurring one done makes the next) and records the undoable change:
/// `(change, next)`. The caller has checked whose todo it is.
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_todo_state(
    tx: &steve_store::Tx,
    ws: &str,
    run: &str,
    writer: &str,
    id: &str,
    state: &str,
    item: Option<&str>,
    now: i64,
) -> ApiResult<(String, Option<String>)> {
    let t = todo::get(tx, ws, id)?
        .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
    let next = todo::complete(tx, ws, id, state, item, now).map_err(todo_error)?;
    let next_updated = match &next {
        Some(n) => todo::fields(tx, ws, n)?.map(|x| x.2),
        None => None,
    };
    let change = changes::record(
        tx,
        ws,
        run,
        writer,
        "todo_state",
        id,
        None,
        &json!({"state": t["state"], "item": t["item_id"]}),
        &json!({"todo": id, "state": state, "item": item, "next": next, "next_updated": next_updated}),
        now,
    )?;
    Ok((change, next))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkItem {
    item: String,
    #[serde(default = "yes")]
    linked: bool,
}
fn yes() -> bool {
    true
}
async fn link_todo_item(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<LinkItem>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        member(m, &who)?;
        let now = m.clock.now();
        let (changed, change) = m.store.transaction(|tx| {
            own_todo(tx, &who, &id)?;
            let changed =
                todo::link_item(tx, &who.ws, &id, &b.item, b.linked).map_err(todo_error)?;
            let change = if changed {
                Some(changes::record(
                    tx,
                    &who.ws,
                    &who.me,
                    &who.me,
                    "todo_link",
                    &id,
                    None,
                    &json!({}),
                    &json!({"todo": id, "item": b.item, "linked": b.linked}),
                    now,
                )?)
            } else {
                None
            };
            Ok::<_, ApiError>((changed, change))
        })?;
        Ok(Json(json!({"changed": changed, "change": change})))
    })
    .await
}

async fn memory_index(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let v = m.viewer(&who)?;
        m.store.reader(|c| {
            let mem = mems::get(c, &id)?
                .filter(|x| x.workspace_id == who.ws && v.sees(&x.visibility, &x.created_by))
                .ok_or_else(|| ApiError::not_found("memory"))?;
            Ok(Json(memindex::index(c, &who.ws, &mem.id)?))
        })
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemLink {
    conv: String,
    matter: Option<String>,
    #[serde(default)]
    parties: Vec<String>,
    range: Option<memindex::Range>,
}
async fn link_memory(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<MemLink>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let cv = cv(&b.conv)?;
        let v = m.viewer(&who)?;
        m.store.transaction(|tx| {
            mems::get(tx, &id)?
                .filter(|x| x.workspace_id == who.ws && v.sees(&x.visibility, &x.created_by))
                .ok_or_else(|| ApiError::not_found("memory"))?;
            let a = memindex::link(
                tx,
                &who.ws,
                &id,
                &cv,
                b.matter.as_deref(),
                &b.parties,
                b.range.as_ref(),
                None,
            )
            .map_err(input_error)?;
            let change = changes::record(
                tx,
                &who.ws,
                &who.me,
                &who.me,
                "mem_link",
                &id,
                None,
                &json!({}),
                &json!(a),
                m.clock.now(),
            )?;
            Ok(Json(json!({"change": change, "added": a})))
        })
    })
    .await
}

async fn change_list(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<List>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        let rows = m.store.reader(|c| {
            let mut out = vec![];
            for ch in changes::list(c, &who.ws, p.run.as_deref(), p.packet.as_deref(), p.limit())? {
                // another member's todo changes are theirs alone
                if changes::is_todo(&ch)
                    && todo::owner_of(c, &who.ws, ch["target"].as_str().unwrap_or_default())?
                        .is_some_and(|o| o != who.me)
                {
                    continue;
                }
                out.push(ch);
            }
            Ok::<_, steve_store::StoreError>(out)
        })?;
        Ok(Json(json!({"changes": rows})))
    })
    .await
}
async fn undo(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        // a todo change is undone by the todo's owner, any member; every other change by the workspace owner
        let todo_of = m.store.reader(|c| {
            Ok::<_, steve_store::StoreError>(match changes::get(c, &who.ws, &id)? {
                Some(ch) if changes::is_todo(&ch) => Some(todo::owner_of(
                    c,
                    &who.ws,
                    ch["target"].as_str().unwrap_or_default(),
                )?),
                _ => None,
            })
        })?;
        match todo_of {
            Some(Some(o)) if o != who.me => {
                return Err(ApiError::not_found(steve_i18n::t!("digest.noun.change")));
            }
            Some(_) => member(m, &who)?,
            None => owner(m, &who)?,
        }
        changes::undo(m, &who.ws, &who.me, &id).map(Json)
    })
    .await
}

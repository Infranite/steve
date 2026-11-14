//! Owner-facing base APIs, relative to `/api`. Raw conversation data is not a workspace-wide feed.
//! The digest Bot and its MCP tool surface are supplied separately in phase D.
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
        .route("/digest/todos/{id}", post(todo_state))
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
        Ok(Json(
            m.store
                .reader(|c| matter::get(c, &who.ws, &id))?
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
        owner(m, &who)?;
        Ok(Json(json!({"todos": m.store.reader(|c| todo::list(c, &who.ws, &who.me, &p.state.clone().into_iter().collect::<Vec<_>>(), p.limit()))?})))
    }).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoBody {
    text: String,
    matter: Option<String>,
    due: Option<i64>,
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
        owner(m, &who)?;
        let now = m.clock.now();
        let id = m.store.transaction(|tx| {
            let id = todo::create(
                tx,
                &todo::NewTodo {
                    ws: &who.ws,
                    owner: &who.me,
                    text: &b.text,
                    matter: b.matter.as_deref(),
                    due: b.due,
                    due_quote: None,
                    source: "you",
                    writer: &who.me,
                    packet: None,
                    refs: b.refs,
                    parties: b.parties,
                },
                now,
            )
            .map_err(input_error)?;
            changes::record(
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
            Ok::<_, ApiError>(id)
        })?;
        Ok(Json(json!({"id": id})))
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
        owner(m, &who)?;
        if !todo::STATES.contains(&b.state.as_str()) || (b.state == "handed") != b.item.is_some() {
            return Err(ApiError::invalid("state"));
        }
        m.store.transaction(|tx| {
            let t = todo::get(tx, &who.ws, &id)?.ok_or_else(|| ApiError::not_found("todo"))?;
            let mine: bool = tx.query_row(
                "SELECT owner = ? FROM todos WHERE id = ?",
                params![who.me, id],
                |r| r.get(0),
            )?;
            if !mine {
                return Err(ApiError::not_found("todo"));
            }
            todo::set_state(tx, &who.ws, &id, &b.state, b.item.as_deref(), m.clock.now())
                .map_err(input_error)?;
            changes::record(
                tx,
                &who.ws,
                &who.me,
                &who.me,
                "todo_state",
                &id,
                None,
                &json!({"state": t["state"], "item": t["item_id"]}),
                &json!({"todo": id, "state": b.state, "item": b.item}),
                m.clock.now(),
            )?;
            Ok::<_, ApiError>(())
        })?;
        Ok(Json(json!({"ok": true})))
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
    m.blocking(move |m| { owner(m, &who)?; Ok(Json(json!({"changes": m.store.reader(|c| changes::list(c, &who.ws, p.run.as_deref(), p.packet.as_deref(), p.limit()))?}))) }).await
}
async fn undo(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        owner(m, &who)?;
        changes::undo(m, &who.ws, &who.me, &id).map(Json)
    })
    .await
}

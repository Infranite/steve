//! Owner-facing base APIs, relative to `/api`. Raw conversation data is not a workspace-wide feed.
//! The ordinary digest Bot consumes the same store through `tools`; schedule binding is Hub wiring.
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::ObjRef;
use steve_store::digest::NewChange;

use crate::{ConvRef, changes, conv, matter, memindex, profile, todo, window};

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
        .route("/digest/briefing/today", get(briefing_today))
        .route(
            "/digest/memories/{id}/index",
            get(memory_index).post(link_memory),
        )
        .route("/digest/changes", get(change_list))
        .route("/digest/changes/{id}/undo", post(undo))
}

fn owner(m: &Modules, who: &Caller) -> ApiResult<()> {
    let role = m.store.read(|r| r.members().role(&who.ws, &who.me))?;
    if role.as_deref() != Some("owner") {
        return Err(ApiError::Forbidden(steve_i18n::t!("digest.owner_only")));
    }
    Ok(())
}

/// Todos are every member's own: any member of the workspace, for their own todos only.
fn member(m: &Modules, who: &Caller) -> ApiResult<()> {
    if m.store
        .read(|r| r.members().role(&who.ws, &who.me))?
        .is_none()
    {
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
fn own_todo(c: &mut impl steve_store::Read, who: &Caller, id: &str) -> ApiResult<()> {
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
        let result = m.store.read::<_, steve_store::StoreError>(|r| {
            let Some(info) = conv::info(r, &who.ws, &cv)? else {
                return Ok(None);
            };
            let stored = r.digest_profiles().get(&who.ws, &key)?;
            let profile = profile::profile(&info, stored, || conv::guess_kind(r, &who.ws, &info))?;
            Ok(Some(Json(json!({
                "conversation": info, "profile": profile::json(&profile),
                "cursor": r.digest_windows().cursor(&who.ws, &key)?,
                "summary": r.summaries().latest(&who.ws, &key)?,
                "queue": r.digest_packets().stats(&who.ws, Some(&key))?,
            }))))
        })?;
        result.ok_or_else(|| ApiError::not_found("conversation"))
    })
    .await
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
        m.store.write(|w| {
            if conv::info(w, &who.ws, &cv)?.is_none() {
                return Err(ApiError::not_found("conversation"));
            }
            let rev = profile::apply(w, &who.ws, &cv, &p, m.clock.now())
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
        if m.store.read(|c| conv::info(c, &who.ws, &cv))?.is_none() {
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
        let packets = m.store.read::<_, steve_store::StoreError>(|r| {
            let found = r.digest_packets().list(
                &who.ws,
                p.state.as_deref(),
                p.conv.as_deref(),
                p.limit(),
            )?;
            let mut out = Vec::new();
            for packet in found {
                if let Some(cv) = ConvRef::parse(&packet.conv)
                    && conv::owner_sees(r, &who.ws, &cv)?
                {
                    out.push(packet);
                }
            }
            Ok(out)
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
        let found = m.store.read::<_, steve_store::StoreError>(|r| {
            let Some(p) = r.digest_packets().get(&who.ws, &id)? else {
                return Ok(None);
            };
            let Some(conv) = ConvRef::parse(&p.conv) else {
                return Ok(None);
            };
            if !conv::owner_sees(r, &who.ws, &conv)? {
                return Ok(None);
            }
            Ok(Some(Json(
                json!({"packet": p, "chunks": r.digest_packets().chunks(&id)?}),
            )))
        })?;
        found.ok_or_else(|| ApiError::not_found("packet"))
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
        let changed = m
            .store
            .write(|w| {
                let mut packets = w.digest_packets();
                if packets.get(&who.ws, &id)?.is_none() {
                    return Ok(None);
                }
                packets.retry(&who.ws, &id, m.clock.now()).map(Some)
            })?
            .ok_or_else(|| ApiError::not_found("packet"))?;
        Ok(Json(json!({"changed": changed})))
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
            json!({"matters": m.store.read(|c| matter::list(c, &who.ws, &f))?}),
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
            .write(|tx| {
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
                .read(|c| matter::get_for(c, &viewer, &id))?
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
            .write(|tx| {
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
        let out = m.store.write(|tx| {
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
            json!({"todos": m.store.read(|c| todo::list_by(c, &who.ws, &who.me, &f))?}),
        ))
    })
    .await
}
/// The caller's brief, made again now ([`crate::brief::today`]).
async fn briefing_today(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        member(m, &who)?;
        let now = m.clock.now();
        Ok(Json(
            m.store
                .read(|c| crate::brief::today(c, &who.ws, &who.me, now))?,
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
        m.store
            .read::<_, steve_store::StoreError>(|c| {
                let Some(mut t) = todo::get_own(c, &who.ws, &who.me, &id)? else {
                    return Ok(None);
                };
                if t["kind"] == "goal" {
                    let f = todo::Filter {
                        parent: Some(id.clone()),
                        limit: 500,
                        ..Default::default()
                    };
                    t["children"] = json!(todo::list_by(c, &who.ws, &who.me, &f)?);
                    // items belong to the workspace and every member reads them: the ids are enough
                    t["items"] = json!(todo::linked_items(c, &who.ws, &id)?);
                }
                Ok(Some(Json(t)))
            })?
            .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))
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
        let (id, change) = m.store.write(|tx| {
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
            let change = tx.digest_changes().record(NewChange {
                ws: &who.ws,
                run: &who.me,
                writer: &who.me,
                op: "todo",
                target: &id,
                packet: None,
                before: &json!({}),
                after: &json!({"todo": id, "updated": now}),
                now,
            })?;
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
        let change = m.store.write(|tx| {
            own_todo(tx, &who, &id)?;
            let (before, after) = todo::edit(tx, &who.ws, &id, &p, now)
                .map_err(todo_error)?
                .ok_or_else(|| ApiError::not_found(steve_i18n::t!("digest.noun.todo")))?;
            Ok::<_, ApiError>(tx.digest_changes().record(NewChange {
                ws: &who.ws,
                run: &who.me,
                writer: &who.me,
                op: "todo_edit",
                target: &id,
                packet: None,
                before: &json!(before),
                after: &json!({"todo": id, "fields": after, "updated": now}),
                now,
            })?)
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
        let (change, next) = m.store.write(|tx| {
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
    tx: &mut steve_store::W<'_>,
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
    let change = tx.digest_changes().record(NewChange { ws, run, writer, op: "todo_state", target: id, packet: None, before: &json!({"state": t["state"], "item": t["item_id"]}), after: &json!({"todo": id, "state": state, "item": item, "next": next, "next_updated": next_updated}), now })?;
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
        let (changed, change) = m.store.write(|tx| {
            own_todo(tx, &who, &id)?;
            let changed =
                todo::link_item(tx, &who.ws, &id, &b.item, b.linked).map_err(todo_error)?;
            let change = if changed {
                Some(tx.digest_changes().record(NewChange {
                    ws: &who.ws,
                    run: &who.me,
                    writer: &who.me,
                    op: "todo_link",
                    target: &id,
                    packet: None,
                    before: &json!({}),
                    after: &json!({"todo": id, "item": b.item, "linked": b.linked}),
                    now,
                })?)
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
        let index = m
            .store
            .read(|r| {
                let mut index = r.memory_index();
                if !index.visible(&v, &id)? {
                    return Ok(None);
                }
                index.get(&who.ws, &id).map(Some)
            })?
            .ok_or_else(|| ApiError::not_found("memory"))?;
        Ok(Json(serde_json::to_value(index)?))
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
        m.store.write(|w| {
            if !w.memory_index().visible(&v, &id)? {
                return Err(ApiError::not_found("memory"));
            }
            let a = memindex::link(
                w,
                &who.ws,
                &id,
                &cv,
                b.matter.as_deref(),
                &b.parties,
                b.range.as_ref(),
                None,
            )
            .map_err(input_error)?;
            let change = w.digest_changes().record(NewChange {
                ws: &who.ws,
                run: &who.me,
                writer: &who.me,
                op: "mem_link",
                target: &id,
                packet: None,
                before: &json!({}),
                after: &json!(a),
                now: m.clock.now(),
            })?;
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
        let rows = m.store.read(|c| {
            let found = c.digest_changes().list(
                &who.ws,
                p.run.as_deref(),
                p.packet.as_deref(),
                p.limit(),
                None,
            )?;
            let mut out = vec![];
            for ch in found {
                // another member's todo changes are theirs alone
                if ch.is_todo()
                    && todo::owner_of(c, &who.ws, &ch.target)?.is_some_and(|o| o != who.me)
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
        let todo_of = m.store.read::<_, steve_store::StoreError>(|r| {
            match r.digest_changes().get(&who.ws, &id)? {
                Some(ch) if ch.is_todo() => Ok(Some(todo::owner_of(r, &who.ws, &ch.target)?)),
                _ => Ok(None),
            }
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

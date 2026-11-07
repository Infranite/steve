//! Session/bootstrap, workspaces, conversation, decisions, tags, settings, config drafts, devices.

use std::collections::HashMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use serde_json::{Map, Value, json};

use super::{
    ApiResult, AppState, Ctx, Noun, body, objs, ok, ok_true, opt_obj, own, set_cookie, str_of,
};
use crate::backend::{Obj, Tier};
use crate::error::ApiError;
use crate::validate::{Check, Num, Str};

const WS_PREFIX: Str = Str::pattern(r"^[A-Z]{1,6}$");

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) use crate::js::num_value as js_num;

pub async fn me(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    let version = st.cfg.version.clone();
    let web_version = crate::web::web_version(&st.cfg);
    st.read(move |r| {
        let ws = r.workspaces_of_user(&c.me)?;
        ok(json!({ "user": c.user, "version": version, "web_version": web_version, "workspaces": objs(ws), "workspace": c.ws }))
    })
    .await
}

/// How long a computed `/api/state` may be served again when nothing was published in between (bounds the
/// staleness of what changes without an event: the 7-day `recent` window, adapter busy flags, runtimes).
const STATE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// One cached `/api/state` body per (user, workspace, language): the body carries text in the request's language.
pub struct StateCached {
    generation: u64,
    /// the change-log head read before the body was computed (`x-steve-cursor`)
    cursor: String,
    at: std::time::Instant,
    body: axum::body::Bytes,
}

pub type StateCache = std::sync::Mutex<
    std::collections::HashMap<
        (String, String, &'static str),
        std::sync::Arc<tokio::sync::Mutex<Option<StateCached>>>,
    >,
>;

/// `/api/state` is the hottest endpoint and a pure function of the database (plus a few in-memory flags):
/// computed once per (user, workspace, language) until the bus publishes anything; concurrent requests wait for the one
/// computation instead of repeating it.
///
/// The response carries the bus change-log head as `x-steve-cursor: <epoch>:<n>`, a watermark a client can
/// pass to `/api/stream?after=` to get only what changed since this snapshot. It is read before
/// `compute_state` and is sound because store writes publish after their commit and the head only moves once
/// the dispatcher has sent a frame: every frame with an id up to the watermark describes a write committed
/// before any read below, so the body already reflects it, and every later change gets a larger id. A frame
/// published before the read but sent after it is delivered again on resume; that is harmless, clients
/// refetch by id. A cached body keeps its older watermark: the generation check means only frames that
/// change no state (`steve.tool`, `stats`) can lie between it and the head, and replaying those is safe.
pub async fn state(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    let slot = {
        let mut m = st.state_cache.lock().unwrap_or_else(|e| e.into_inner());
        m.entry((c.me.clone(), c.ws.clone(), steve_i18n::current()))
            .or_default()
            .clone()
    };
    let mut cached = slot.lock().await;
    if let Some(x) = cached.as_ref()
        && x.generation == st.bus.generation()
        && x.at.elapsed() < STATE_TTL
    {
        return Ok(state_response(x.body.clone(), &x.cursor));
    }
    let generation = st.bus.generation();
    let cursor = st.bus.cursor().to_string();
    let at = std::time::Instant::now();
    let v = compute_state(&st, c).await?;
    let body = axum::body::Bytes::from(
        serde_json::to_vec(&v).map_err(|e| ApiError::internal(e.to_string()))?,
    );
    *cached = Some(StateCached {
        generation,
        cursor: cursor.clone(),
        at,
        body: body.clone(),
    });
    Ok(state_response(body, &cursor))
}

/// The header carrying `/api/state`'s change-log watermark.
pub const CURSOR_HEADER: &str = "x-steve-cursor";

fn state_response(body: axum::body::Bytes, cursor: &str) -> axum::response::Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::HeaderName::from_static(CURSOR_HEADER), cursor),
        ],
        body,
    )
        .into_response()
}

async fn compute_state(st: &AppState, c: Ctx) -> Result<Value, ApiError> {
    let runtimes = st.runtimes.get(false).await;
    let version = st.cfg.version.clone();
    let web_version = crate::web::web_version(&st.cfg);
    let update = crate::upgrades::available(&st.cfg);
    let domain = st.domain.clone();
    let hold = domain.hold_status().await?;
    let lifecycles = domain.lifecycle_views(&c.ws).await?;
    let thinking = domain.thread_busy(&c.ws).await?;
    // the read cursor is written on the first look, so it goes through the writer first
    let ws = c.ws.clone();
    let seen = st.write(move |r| Ok(crate::notify::seen(r, &ws)?)).await?;
    st.read(move |r| {
        let w = c.ws.as_str();
        let mut out = Map::new();
        out.insert("version".into(), json!(version));
        out.insert("web_version".into(), json!(web_version));
        // a newer release the last version check found: the page's update notice
        if let Some(version) = update {
            let owner = c.user.get("role").and_then(Value::as_str) == Some("owner");
            out.insert(
                "update".into(),
                json!({ "version": version, "owner": owner }),
            );
        }
        // `workspace: undefined` drops the key
        if let Some(x) = r.workspace_get(w)? {
            out.insert("workspace".into(), Value::Object(x));
        }
        out.insert("workspaces".into(), objs(r.workspaces_of_user(&c.me)?));
        let t0 = std::time::Instant::now();
        let mut items = super::summaries(r, w)?;
        for i in &mut items {
            if let Some(l) = i
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| lifecycles.get(id))
            {
                i.insert("lifecycle".into(), l.clone());
            }
        }
        out.insert("items".into(), objs(items));
        let t_items = t0.elapsed();
        out.insert(
            "decisions".into(),
            objs(agent_busy(r, r.decisions_open(w)?)?),
        );
        out.insert(
            "recent".into(),
            objs(r.decisions_recent(w, now_ms() - 7 * 24 * 3_600_000)?),
        );
        let (bots, all) = super::bots_out(r, w)?;
        out.insert("bots".into(), objs(bots));
        out.insert("allBots".into(), Value::Array(all));
        out.insert("projects".into(), objs(r.projects_list(w)?));
        out.insert("tags".into(), objs(r.tags_list(w)?));
        out.insert("tagUses".into(), Value::Object(r.tag_uses(w)?));
        out.insert("thread".into(), objs(crate::notify::thread(r, w)?));
        out.insert("thinking".into(), json!(thinking));
        out.insert(
            "notifications".into(),
            Value::Array(crate::notify::list(r, w, 100)?),
        );
        out.insert("notifySeen".into(), seen);
        let mut settings = crate::settings::defaults();
        settings.extend(r.settings_all(w)?);
        out.insert("settings".into(), Value::Object(settings));
        out.insert(
            "sessions".into(),
            objs(super::sessions_out(r, domain.as_ref(), w)?),
        );
        let t_sessions = t0.elapsed();
        out.insert("hold".into(), hold);
        out.insert("devices".into(), objs(r.devices_list()?));
        out.insert("runtimes".into(), runtimes);
        tracing::debug!(
            "state: items {:?}, through sessions {:?}, all {:?}",
            t_items,
            t_sessions,
            t0.elapsed()
        );
        Ok(Value::Object(out))
    })
    .await
}

/// Open cards an agent opened from inside its turn (a question, an approval, a delivery) get
/// `agent_busy: true` while that turn is still going: the answer reaches the agent once it stops.
/// Tool permissions are not marked (the agent waits on those). Turn end publishes the session, so the
/// flag clears through SSE.
fn agent_busy(r: &dyn crate::backend::Repo, ds: Vec<Obj>) -> Result<Vec<Obj>, ApiError> {
    let mut busy: HashMap<String, bool> = HashMap::new();
    ds.into_iter()
        .map(|mut d| {
            let sid = str_of(&d, "session_id");
            if sid.is_empty()
                || str_of(&d, "subject_type") == "tool_call"
                || !matches!(
                    str_of(&d, "kind").as_str(),
                    "ask" | "approve" | "accept" | "confirm"
                )
            {
                return Ok(d);
            }
            let b = match busy.get(&sid) {
                Some(b) => *b,
                None => {
                    let b = r.session_get(&sid)?.is_some_and(|s| {
                        matches!(str_of(&s, "status").as_str(), "running" | "starting")
                    });
                    busy.insert(sid, b);
                    b
                }
            };
            if b {
                d.insert("agent_busy".into(), Value::Bool(true));
            }
            Ok(d)
        })
        .collect()
}

// ── workspaces ──

/// `{name, description?, item_prefix?}`; `name` required unless `partial`.
fn workspace_in(v: &Value, partial: bool) -> Result<Obj, ApiError> {
    let mut c = Check::object(v);
    let mut p = Obj::new();
    let name = if partial {
        c.opt_str("name", Str::len(1, 40))
    } else {
        Some(c.str("name", Str::len(1, 40)))
    };
    if let Some(n) = name {
        p.insert("name".into(), json!(n));
    }
    if let Some(d) = c.opt_str("description", Str::max(400)) {
        p.insert("description".into(), json!(d));
    }
    if let Some(x) = c.opt_str("item_prefix", WS_PREFIX) {
        p.insert("item_prefix".into(), json!(x));
    }
    c.finish()?;
    Ok(p)
}

pub async fn workspace_create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let p = workspace_in(&body(&b)?, false)?;
    st.write(move |r| {
        let w = r.workspace_create(
            &str_of(&p, "name"),
            p.get("description").and_then(Value::as_str),
            p.get("item_prefix").and_then(Value::as_str),
            &c.me,
        )?;
        let id = str_of(&w, "id");
        let mut steve = r.bot_workspaces("steve")?;
        steve.push(id.clone());
        r.bot_set_workspaces("steve", &steve)?;
        r.audit(&c.me, "workspace.create", &id, &Value::Object(p), &id)?;
        ok(Value::Object(w))
    })
    .await
}

pub async fn workspace_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let (me, i) = (c.me.clone(), id.clone());
    if !st
        .read(move |r| Ok(r.workspace_is_member(&i, &me)?))
        .await?
    {
        return Err(ApiError::not_found(steve_i18n::t!(
            "hub.not_found.workspace"
        )));
    }
    let p = workspace_in(&body(&b)?, true)?;
    st.write(move |r| ok(Value::Object(r.workspace_patch(&id, &p)?)))
        .await
}

pub async fn workspace_switch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let i = id.clone();
    if !st
        .read(move |r| Ok(r.workspace_is_member(&i, &c.me)?))
        .await?
    {
        return Err(ApiError::not_found(steve_i18n::t!(
            "hub.not_found.workspace"
        )));
    }
    let cookie = set_cookie(
        "steve_ws",
        &id,
        "/",
        365 * 24 * 3600,
        st.cfg.secure_cookies(),
    );
    let mut res = ok_true()?;
    if let Ok(v) = cookie.parse() {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
    Ok(res.into_response())
}

// ── conversation ──

pub async fn thread_say(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let text = k.str("text", Str::len(1, 20000));
    let files = super::files::file_ids(&mut k);
    let mentions = k.raw("mentions").cloned();
    let item = k.opt_str("item", Str::len(1, 40));
    let reply_to = k.opt_str("reply_to", Str::len(1, 80));
    let client_id = k.opt_str("client_id", Str::len(1, 80));
    k.finish()?;
    // `@` mentions (contacts P3): checked like the module does before anything happens
    let mut checked = Vec::new();
    if let Some(ms) = mentions.filter(|v| !v.is_null()) {
        let ms: Vec<steve_modules::contacts::item_people::Mention> =
            match serde_json::from_value(ms) {
                Ok(ms) => ms,
                Err(_) => return Err(ApiError::Invalid(vec!["mentions".into()])),
            };
        if let Err(e) = steve_modules::contacts::item_people::check_mentions(&ms) {
            return Ok(e.into_response());
        }
        checked = ms;
    }
    // a retry of the same draft (`client_id`) answers like the first send and sends nothing; the
    // send runs on even if this request goes away, so it needs its own state
    let (st2, c2) = (st.clone(), c.clone());
    let v = st
        .dedupe
        .once(&c.ws, &c.me, "thread", client_id, move || async move {
            let (st, c) = (st2, c2);
            // V4 paste guard: a password or token is replaced before the message is stored or
            // routed; the rest of it goes on as usual, with Steve's note on where to keep the secret
            let guarded = steve_vault::guard::intercept(&text);
            let refs = super::files::refs(&st, &c.ws, files).await?;
            st.domain
                .say_to(&c.ws, &c.me, &guarded.text, refs, checked, item, reply_to)
                .await?;
            if guarded.blocked {
                st.domain.say_guard_note(&c.ws).await?;
            }
            Ok::<_, ApiError>(json!({ "ok": true }))
        })
        .await?;
    ok(v)
}

pub async fn thread_stop(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    ok(json!({ "ok": st.domain.stop(&c.ws).await? }))
}

#[derive(serde::Deserialize)]
pub struct HistoryQuery {
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<i64>,
}

/// `GET /api/thread/history?before=<ms>[&after=<ms>][&limit=n]`: the conversation before what `/api/state` carries,
/// oldest first. With `after` it fills the gap down to an older message (a search hit); `has_more` says paging
/// back can go on.
pub async fn thread_history(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult {
    let before = q.before.unwrap_or(i64::MAX);
    let after = q.after.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(100).clamp(1, 2000);
    st.read(move |r| {
        let raw = r.messages_before(&c.ws, "steve", before, after, limit)?;
        let more = raw.len() as i64 == limit
            || (after > 0 && !r.messages_before(&c.ws, "steve", after, 0, 1)?.is_empty());
        let oldest = raw.first().and_then(|m| m.get("created")).cloned();
        let list = crate::notify::thread_before(r, &c.ws, before, after, limit)?;
        ok(json!({ "messages": objs(list), "has_more": more, "oldest": oldest }))
    })
    .await
}

#[derive(serde::Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    limit: Option<i64>,
}

/// `GET /api/thread/search?q=…`: messages of the conversation that mention `q` or belong to item `q`, newest first.
pub async fn thread_search(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<SearchQuery>,
) -> ApiResult {
    let text = q.q.unwrap_or_default().trim().to_string();
    if text.is_empty() || text.chars().count() > 200 {
        return Err(ApiError::Invalid(vec!["q".into()]));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    st.read(move |r| {
        let list = crate::notify::thread_search(r, &c.ws, &text, limit)?;
        ok(json!({ "messages": objs(list) }))
    })
    .await
}

/// `PATCH /api/thread/queued/:id {text}`: rewrite a message still waiting for the turn before it.
pub async fn thread_queued_edit(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let text = k.str("text", Str::len(1, 20000));
    k.finish()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(ApiError::Invalid(vec!["text".into()]));
    }
    // V4 paste guard: what is sent is guarded, so is what it is changed to (the secret replaced)
    let guarded = steve_vault::guard::intercept(&text);
    if !st
        .domain
        .thread_queued_edit(&c.ws, &id, &guarded.text)
        .await?
    {
        return Err(ApiError::Json(409, steve_i18n::t!("hub.queued.gone")));
    }
    ok_true()
}

/// `DELETE /api/thread/queued/:id` → `{text, files}`: withdraw a message still waiting for the turn
/// before it (the composer takes it back).
pub async fn thread_queued_withdraw(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    match st.domain.thread_queued_withdraw(&c.ws, &id).await? {
        Some(m) => ok(Value::Object(m)),
        None => Err(ApiError::Json(409, steve_i18n::t!("hub.queued.gone"))),
    }
}

/// Notifications: everything up to `at` (default now) counts as read.
pub async fn notifications_read(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let at = k.opt_num(
        "at",
        Num {
            int: true,
            min: None,
            max: None,
            gt: Some(0.0),
        },
    );
    k.finish()?;
    let now = now_ms();
    let at = at.map(|a| (a as i64).min(now)).unwrap_or(now);
    let bus = st.bus.clone();
    st.write(move |r| ok(json!({ "seen": crate::notify::mark_seen(r, &bus, &c.ws, at)? })))
        .await
}

/// Notifications: the user closes one (its id, or its line in the conversation).
pub async fn notifications_dismiss(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let bus = st.bus.clone();
    st.write(move |r| {
        crate::notify::dismiss(r, &bus, &c.ws, &id)?;
        ok_true()
    })
    .await
}

pub async fn thread_promote(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    ok(Value::Object(st.domain.promote(&c.ws, &id).await?))
}

/// A thread action's answer: the result, or `409 已经处理过了` with the result when it was handled before.
fn thread_result((o, already): (Obj, bool)) -> ApiResult {
    if already {
        let mut v = o;
        v.insert(
            "error".into(),
            json!(steve_i18n::t!("hub.thread.already_handled")),
        );
        return Ok(crate::error::json_response(409, &Value::Object(v)));
    }
    ok(Value::Object(o))
}

/// `POST /api/thread/:id/reroute {to: "inline" | "item" | "item:<ST-n>"}`.
pub async fn thread_reroute(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let to = k.str("to", Str::len(1, 40));
    k.finish()?;
    if !(to == "inline" || to == "item" || to.strip_prefix("item:").is_some_and(|i| !i.is_empty()))
    {
        return Err(ApiError::Invalid(vec!["to".into()]));
    }
    thread_result(st.domain.reroute(&c.ws, &id, &to).await?)
}

/// `POST /api/thread/:id/offer {take: bool}`.
pub async fn thread_offer(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let take = k.opt_bool("take");
    k.finish()?;
    let Some(take) = take else {
        return Err(ApiError::Invalid(vec!["take".into()]));
    };
    thread_result(st.domain.offer(&c.ws, &id, take).await?)
}

/// `POST /api/thread/:id/suggest {choice: "here" | "item" | "new"}`.
pub async fn thread_suggest(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let choice = k.enum_("choice", &["here", "item", "new"]);
    k.finish()?;
    thread_result(st.domain.suggest(&c.ws, &id, &choice).await?)
}

/// `POST /api/thread/:id/a2ui` (ST-46): a button of an A2UI interface in Steve's reply. Answers the record
/// with `already: true` when that surface had been submitted, so a second click is harmless.
pub async fn thread_a2ui(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let block = k.str("block", Str::len(1, 16));
    let surface = k.str("surfaceId", Str::len(1, 120));
    let component = k.str("sourceComponentId", Str::len(1, 120));
    let name = k.str("name", Str::len(1, 64));
    let label = k.opt_str("label", Str::ANY);
    let timestamp = k.opt_str("timestamp", Str::len(0, 40));
    let context = k.raw("context").cloned();
    let data = k.raw("data").cloned();
    k.finish()?;
    let action = steve_core::thread::A2uiAction {
        block,
        surface,
        component,
        name,
        label: label.unwrap_or_default(),
        context: context.unwrap_or_else(|| json!({})),
        data: data.unwrap_or(Value::Null),
        timestamp: timestamp.unwrap_or_default(),
    };
    ok(Value::Object(
        st.domain.a2ui_action(&c.ws, &c.me, &id, action).await?,
    ))
}

/// `POST /api/thread/runs/:id/stop` and `DELETE /api/thread/runs/:id`: stop a research run or take
/// a queued one off the queue.
pub async fn thread_run_stop(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    ok(json!({ "ok": st.domain.stop_run(&c.ws, &id).await? }))
}

/// `PATCH /api/items/:id/thread {follow: bool}`.
pub async fn item_thread_follow(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let follow = k.opt_bool("follow");
    k.finish()?;
    let Some(follow) = follow else {
        return Err(ApiError::Invalid(vec!["follow".into()]));
    };
    if !st.domain.thread_follow(&c.ws, &id, follow).await? {
        return Err(ApiError::not_found(Noun::Item.missing()));
    }
    ok_true()
}

// ── decisions ──

pub async fn decision_resolve(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let (i, ws) = (id.clone(), c.ws.clone());
    st.read(move |r| Ok(own(r.decision_get(&i)?, &ws, Noun::Decision)?))
        .await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let choice = k.str("choice", Str::ANY);
    let text = k.opt_str("text", Str::ANY);
    k.finish()?;
    // decisions a module opened go to that module's handler
    if let Some(m) = st.modules.clone() {
        let (caller, id2, ch, tx) = (
            steve_modules::Caller {
                ws: c.ws.clone(),
                me: c.me.clone(),
            },
            id.clone(),
            choice.clone(),
            text.clone().unwrap_or_default(),
        );
        let r = tokio::task::spawn_blocking(steve_i18n::carry(move || {
            steve_modules::decisions::dispatch(&m, &caller, &id2, &ch, &tx)
        }))
        .await
        .ok()
        .flatten();
        if let Some(r) = r {
            return Ok(match r {
                Ok(d) => super::ok(
                    serde_json::to_value(d).map_err(|e| ApiError::internal(e.to_string()))?,
                )?,
                Err(e) => e.into_response(),
            });
        }
    }
    ok(Value::Object(
        st.domain
            .resolve(&id, &choice, text.as_deref().unwrap_or(""), &c.me)
            .await?,
    ))
}

pub async fn decision_dismiss(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let (i, ws) = (id.clone(), c.ws.clone());
    let d = st
        .read(move |r| Ok(own(r.decision_get(&i)?, &ws, Noun::Decision)?))
        .await?;
    // a reply waiting for your approval that no longer needs handling is not sent: its draft is
    // discarded like 不发出, so the conversation it belongs to stops waiting (and undo puts it back)
    let field = |k: &str| d.get(k).and_then(Value::as_str);
    if field("subject_type") == Some(steve_channels::respond::SUBJECT_MESSAGE)
        && field("kind") == Some("approve")
        && let Some(m) = st.modules.clone()
    {
        let (caller, id2) = (
            steve_modules::Caller {
                ws: c.ws.clone(),
                me: c.me.clone(),
            },
            id.clone(),
        );
        let r = tokio::task::spawn_blocking(steve_i18n::carry(move || {
            steve_modules::decisions::dispatch(&m, &caller, &id2, "no", "")
        }))
        .await
        .ok()
        .flatten();
        if let Some(r) = r {
            return Ok(match r {
                Ok(d) => super::ok(
                    serde_json::to_value(d).map_err(|e| ApiError::internal(e.to_string()))?,
                )?,
                Err(e) => e.into_response(),
            });
        }
    }
    ok(Value::Object(st.domain.dismiss_decision(&id, &c.me).await?))
}

pub async fn decision_undo(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let (i, ws) = (id.clone(), c.ws.clone());
    st.read(move |r| Ok(own(r.decision_get(&i)?, &ws, Noun::Decision)?))
        .await?;
    ok(Value::Object(st.domain.undo_decision(&id, &c.me).await?))
}

// ── tags ──

const TAG_CATS: &[&str] = &["项目", "类型", "领域", "客户", "主题"];

/// `TagIn` (or `TagIn.partial()`), as the parsed object with only the present keys.
fn tag_in(v: &Value, partial: bool) -> Result<Obj, ApiError> {
    let mut c = Check::object(v);
    let mut p = Obj::new();
    let name = if partial {
        c.opt_str("name", Str::len(1, 20))
    } else {
        Some(c.str("name", Str::len(1, 20)))
    };
    let cat = if partial {
        c.opt_enum("cat", TAG_CATS)
    } else {
        Some(c.enum_("cat", TAG_CATS))
    };
    let desc = if partial {
        c.opt_str("description", Str::len(1, 400))
    } else {
        Some(c.str("description", Str::len(1, 400)))
    };
    c.finish()?;
    for (k, x) in [("name", name), ("cat", cat), ("description", desc)] {
        if let Some(x) = x {
            p.insert(k.into(), json!(x));
        }
    }
    Ok(p)
}

pub async fn tag_create(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let p = tag_in(&body(&b)?, false)?;
    st.write(move |r| {
        let name = str_of(&p, "name");
        if r.tag_by_name(&c.ws, &name)?.is_some() {
            return Err(ApiError::bad_request(steve_i18n::t!("hub.tag.duplicate")));
        }
        ok(Value::Object(r.tag_create(
            &c.ws,
            &name,
            &str_of(&p, "cat"),
            &str_of(&p, "description"),
            "你",
        )?))
    })
    .await
}

pub async fn tag_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let i = id.clone();
    st.read(move |r| Ok(own(r.tag_get(&i)?, &c.ws, Noun::Tag)?))
        .await?;
    let p = tag_in(&body(&b)?, true)?;
    st.write(move |r| ok(opt_obj(r.tag_patch(&id, &p)?))).await
}

pub async fn tag_remove(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    st.write(move |r| {
        own(r.tag_get(&id)?, &c.ws, Noun::Tag)?;
        r.tag_remove(&id)?;
        ok_true()
    })
    .await
}

pub async fn tag_merge(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let (i, ws) = (id.clone(), c.ws.clone());
    st.read(move |r| Ok(own(r.tag_get(&i)?, &ws, Noun::Tag)?))
        .await?;
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let into = k.str("into", Str::ANY);
    k.finish()?;
    st.write(move |r| {
        own(r.tag_get(&into)?, &c.ws, Noun::Tag)?;
        r.tag_merge(&id, &into)?;
        ok_true()
    })
    .await
}

// ── settings and conversational configuration ──

pub async fn settings_schema() -> ApiResult {
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        crate::settings::SETTINGS_JSON,
    )
        .into_response())
}

pub async fn settings_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let k = Check::object(&v);
    let p = k.map().cloned();
    k.finish()?;
    let p = p.unwrap_or_default();
    let specs = crate::settings::specs();
    let mut values = Obj::new();
    for (key, val) in &p {
        let s = crate::js::string(val);
        match specs.iter().find(|x| &x.key == key) {
            Some(spec) if spec.values.contains(&s) => {
                values.insert(key.clone(), json!(s));
            }
            _ => {
                return Err(ApiError::bad_request(steve_i18n::t!(
                    "hub.settings.bad_key",
                    key = key
                )));
            }
        }
    }
    st.write(move |r| {
        r.settings_set(&c.ws, &values, &c.me)?;
        ok_true()
    })
    .await
}

/// 设置 · 模型: which models each of Steve's own decisions uses, and the agent defaults.
pub async fn models_get(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    ok(st.domain.models_view(&c.ws).await?)
}

/// `{"purposes": {"route": {"models": [...], "effort": "high"} | null}, "agents": {"codex": "id" | null}}`;
/// answers the new view. Takes effect on the next call.
pub async fn models_patch(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let values = steve_core::models::validate(&v).map_err(ApiError::bad_request)?;
    st.domain.models_write(&c.ws, &values, &c.me).await?;
    ok(st.domain.models_view(&c.ws).await?)
}

pub async fn config_draft(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let mut target = Obj::new();
    // the TS kinds plus every module target (`schedule`, …)
    let mut kinds = vec!["bot", "project", "settings", "models", "tags", "global"];
    if let Some(m) = &st.modules {
        kinds.extend(m.registry.target_kinds());
    }
    if let Some(mut t) = k.sub("target", false) {
        let kind = t.enum_("kind", &kinds);
        let id = t.opt_str("id", Str::ANY);
        k.absorb(t);
        target.insert("kind".into(), json!(kind));
        if let Some(id) = id {
            target.insert("id".into(), json!(id));
        }
    }
    let text = k.str("text", Str::len(1, 4000));
    k.finish()?;
    // V4 paste guard: no draft (and no model call) from a text with a password or token in it
    if steve_vault::guard::intercept(&text).blocked {
        return Ok(crate::error::json_response(
            400,
            &json!({ "error": steve_vault::guard::REPLY, "href": steve_vault::guard::REPLY_HREF }),
        ));
    }
    let kind = target
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(m) = st
        .modules
        .as_ref()
        .filter(|m| m.registry.target_kinds().contains(&kind.as_str()))
    {
        let id = target.get("id").and_then(Value::as_str).map(str::to_string);
        let caller = steve_modules::Caller {
            ws: c.ws.clone(),
            me: c.me.clone(),
        };
        return Ok(
            match steve_modules::changes::draft(
                m,
                &caller,
                &kind,
                id.as_deref(),
                &text,
                Default::default(),
            )
            .await
            {
                Ok(p) => super::ok(
                    serde_json::to_value(p).map_err(|e| ApiError::internal(e.to_string()))?,
                )?,
                Err(e) => e.into_response(),
            },
        );
    }
    ok(Value::Object(
        st.domain.config_draft(&c.ws, &target, &text).await?,
    ))
}

/// What to do with a stored module changeset.
#[derive(Clone, Copy)]
enum Act {
    Apply,
    Undo,
    Drop,
}

/// A stored module changeset first (apply, undo or drop); `None` = not one, use the in-memory drafts.
/// A change that went through updates its card in the conversation.
async fn module_change(
    st: &AppState,
    c: &Ctx,
    id: &str,
    act: Act,
) -> Option<axum::response::Response> {
    let m = st.modules.clone()?;
    let (caller, cid) = (
        steve_modules::Caller {
            ws: c.ws.clone(),
            me: c.me.clone(),
        },
        id.to_string(),
    );
    let r = tokio::task::spawn_blocking(steve_i18n::carry(move || match act {
        Act::Apply => steve_modules::changes::apply(&m, &caller, &cid),
        Act::Undo => steve_modules::changes::undo(&m, &caller, &cid),
        Act::Drop => steve_modules::changes::drop_draft(&m, &caller, &cid),
    }))
    .await
    .ok()??;
    Some(match r {
        Ok(v) => {
            let state = match act {
                Act::Apply => Some("applied"),
                Act::Undo => None,
                Act::Drop => Some("dropped"),
            };
            if let Err(e) = st.domain.config_mark(&c.ws, id, state).await {
                tracing::warn!("config card {id}: {e}");
            }
            crate::error::json_response(200, &v)
        }
        Err(e) => e.into_response(),
    })
}

pub async fn config_apply(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    if let Some(r) = module_change(&st, &c, &id, Act::Apply).await {
        return Ok(r);
    }
    ok(Value::Object(
        st.domain.config_apply(&id, &c.me, &c.ws).await?,
    ))
}

pub async fn config_undo(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    if let Some(r) = module_change(&st, &c, &id, Act::Undo).await {
        return Ok(r);
    }
    ok(Value::Object(
        st.domain.config_undo(&id, &c.me, &c.ws).await?,
    ))
}

pub async fn config_drop(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    if let Some(r) = module_change(&st, &c, &id, Act::Drop).await {
        return Ok(r);
    }
    ok(Value::Object(st.domain.config_drop(&id, &c.ws).await?))
}

// ── models a harness offers ──

#[derive(serde::Deserialize)]
pub struct ModelsQuery {
    harness: Option<String>,
    fresh: Option<String>,
}

/// `GET /api/system/models?harness=claude-code|codex[&fresh=1]`: the models the harness offers
/// (Claude Code's model picker, Codex's catalogue), grouped by family, with what each ran on this
/// week. Never reads keys.
pub async fn system_models(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Query(q): Query<ModelsQuery>,
) -> ApiResult {
    let h = q.harness.unwrap_or_else(|| "claude-code".into());
    let list = st
        .catalog
        .get(&h, q.fresh.as_deref() == Some("1"))
        .await
        .ok_or_else(|| {
            ApiError::bad_request(steve_i18n::t!("hub.models.no_harness", harness = h))
        })?;
    let used = st
        .domain
        .models_used(&c.ws, &h)
        .await
        .unwrap_or_else(|_| json!({}));
    ok(crate::catalog::with_usage(list, &used))
}

// ── task workspaces (ST-45) ──

/// Paths and sizes on this machine are the instance manager's.
fn instance_owner(c: &Ctx) -> Result<(), ApiError> {
    owner_or(c, || steve_i18n::t!("hub.workspaces.owner_only"))
}

fn owner_or(c: &Ctx, why: impl FnOnce() -> String) -> Result<(), ApiError> {
    if c.user.get("role").and_then(Value::as_str) == Some("owner") {
        Ok(())
    } else {
        Err(ApiError::Json(403, why()))
    }
}

/// `GET /api/system/workspaces`: every item's worktree with its size and caches, the disk's free space and
/// thresholds, folders no item points at. Walks the worktrees: seconds for a hundred GB.
pub async fn system_workspaces(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
) -> ApiResult {
    instance_owner(&c)?;
    ok(st.domain.disk_report().await?)
}

/// `POST /api/system/workspaces/reclaim`: one reclaim pass now (what the ten-minute watcher does).
pub async fn system_workspaces_reclaim(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
) -> ApiResult {
    instance_owner(&c)?;
    ok(st.domain.reclaim_workspaces().await?)
}

/// The outbox's store: only a Hub with its modules wired has one.
fn outbox_store(st: &AppState) -> Result<steve_store::Store, ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::not_found(steve_i18n::t!("hub.outbox.none")))
}

/// `GET /api/system/outbox`: what the outbox still owes, and the dead deliveries grouped by consumer.
pub async fn system_outbox(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.outbox.owner_only"))?;
    let store = outbox_store(&st)?;
    let (stats, dead) = tokio::task::spawn_blocking(move || {
        Ok::<_, steve_store::StoreError>((
            store.reader(steve_store::outbox::stats)?,
            store.reader(steve_store::outbox::dead)?,
        ))
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(
        json!({ "pending": stats.pending, "dead": stats.dead, "oldest": stats.oldest, "groups": dead }),
    )
}

/// `POST /api/system/outbox/revive` `{consumer?}`: the dead deliveries (of one consumer, or all) are
/// tried again, with a fresh day before they can be given up on again.
pub async fn system_outbox_revive(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    owner_or(&c, || steve_i18n::t!("hub.outbox.owner_only"))?;
    let store = outbox_store(&st)?;
    let v = if b.is_empty() { json!({}) } else { body(&b)? };
    let consumer = match v.get("consumer") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(_) => return Err(ApiError::Invalid(vec!["consumer".into()])),
    };
    let n = tokio::task::spawn_blocking(move || {
        steve_store::outbox::revive(&store, consumer.as_deref())
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(|e| ApiError::internal(e.to_string()))?;
    ok(json!({ "revived": n }))
}

// ── devices ──

#[derive(serde::Deserialize)]
pub struct Fresh {
    fresh: Option<String>,
}

pub async fn devices(State(st): State<AppState>, Query(q): Query<Fresh>) -> ApiResult {
    let runtimes = st.runtimes.get(q.fresh.as_deref() == Some("1")).await;
    st.read(move |r| ok(json!({ "devices": objs(r.devices_list()?), "runtimes": runtimes })))
        .await
}

pub async fn device_patch(
    State(st): State<AppState>,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let mut p = Obj::new();
    if let Some(n) = k.opt_str("name", Str::len(1, 60)) {
        p.insert("name".into(), json!(n));
    }
    if let Some(s) = k.opt_enum("status", &["正常", "暂停接活"]) {
        p.insert("status".into(), json!(s));
    }
    if let Some(m) = k.opt_num(
        "max_sessions",
        Num {
            int: true,
            min: Some(1.0),
            max: Some(16.0),
            gt: None,
        },
    ) {
        p.insert("max_sessions".into(), js_num(m));
    }
    k.finish()?;
    st.write(move |r| {
        r.device_patch(&id, &p)?;
        ok_true()
    })
    .await
}

/// gateway tiers by name, for the route modules.
pub(crate) const SMART: Tier = Tier::Smart;
pub(crate) const FAST: Tier = Tier::Fast;

// ── item categories (ST-34) ──

pub async fn kinds_backfill(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = if b.is_empty() { json!({}) } else { body(&b)? };
    let mut k = Check::object(&v);
    let dry = k.opt_bool("dry").unwrap_or(false);
    k.finish()?;
    ok(st.domain.kinds_backfill(&c.ws, dry).await?)
}

pub async fn kinds_review(State(st): State<AppState>, Extension(c): Extension<Ctx>) -> ApiResult {
    ok(st.domain.kinds_review(&c.ws).await?)
}

pub async fn kinds_reviewed(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    b: Bytes,
) -> ApiResult {
    let v = body(&b)?;
    let mut k = Check::object(&v);
    let ids = k.opt_str_array("ids", None, None).unwrap_or_default();
    k.finish()?;
    ok(st.domain.kinds_reviewed(&c.ws, ids).await?)
}

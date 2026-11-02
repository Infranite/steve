//! REST + SSE for the web app (api.ts). Thin: validation here, behaviour in steve-core.
//! Every request runs in one workspace (header `x-steve-ws` or cookie `steve_ws`, checked against membership).

mod bots;
mod files;
mod items;
mod misc;
mod projects;
mod sessions;
pub mod slim;
pub mod stats;
pub mod stream;
mod updates;
pub mod yolo;

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use serde_json::{Value, json};

use crate::backend::{BResult, Backend, BackendError, Domain, ItemExtras, Obj, Repo};
use crate::bus::Bus;
use crate::config::Config;
use crate::error::{ApiError, json_response, text_response};
use crate::system::Runtimes;

/// Shared state of every handler.
#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

pub struct Inner {
    pub cfg: Arc<Config>,
    pub backend: Arc<dyn Backend>,
    pub domain: Arc<dyn Domain>,
    pub bus: Bus,
    pub runtimes: Runtimes,
    /// The models each harness offers (`/system/models`).
    pub catalog: crate::catalog::Catalog,
    pub state_cache: misc::StateCache,
    /// The new modules (steve-modules), for config drafts and decisions they own.
    pub modules: Option<steve_modules::Modules>,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(
        cfg: Arc<Config>,
        backend: Arc<dyn Backend>,
        domain: Arc<dyn Domain>,
        bus: Bus,
    ) -> Self {
        let runtimes = Runtimes::new(cfg.clone());
        let catalog = crate::catalog::Catalog::of(&cfg);
        AppState(Arc::new(Inner {
            catalog,
            cfg,
            backend,
            domain,
            bus,
            runtimes,
            state_cache: Default::default(),
            modules: None,
        }))
    }

    /// The same state with the new modules attached.
    pub fn with_modules(self, modules: Option<steve_modules::Modules>) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.modules = modules;
                AppState(Arc::new(inner))
            }
            Err(shared) => AppState(shared),
        }
    }

    /// Runs `f` on a read-pool connection off the async runtime.
    pub async fn read<R: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn Repo) -> Result<R, ApiError> + Send + 'static,
    ) -> Result<R, ApiError> {
        let b = self.backend.clone();
        blocking(move || f(b.reader()?.as_ref())).await
    }

    /// Runs `f` under the core write lock off the async runtime; the lock is never held across an await.
    pub async fn write<R: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn Repo) -> Result<R, ApiError> + Send + 'static,
    ) -> Result<R, ApiError> {
        let b = self.backend.clone();
        blocking(move || f(b.writer()?.as_ref())).await
    }
}

pub(crate) async fn blocking<R: Send + 'static>(
    f: impl FnOnce() -> Result<R, ApiError> + Send + 'static,
) -> Result<R, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(format!("task failed: {e}")))?
}

/// The signed-in user and the workspace this request runs in.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub user: Obj,
    pub me: String,
    pub ws: String,
}

/// Value of the first cookie called `name` (Hono `getCookie`).
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(v);
            percent_decode(v)
        })
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// The signed-in user of a request and their workspace: `ws` = the wanted workspace if the user is a
/// member, else their first workspace, else `"default"` (清单 §1.3, copied as is).
async fn lookup_user(st: &AppState, headers: &HeaderMap) -> Result<Option<Ctx>, ApiError> {
    let token = cookie(headers, "steve_session");
    let want = headers
        .get("x-steve-ws")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| cookie(headers, "steve_ws"));
    st.read(move |r| {
        let Some(t) = token else { return Ok(None) };
        let Some(user) = r.user_by_session(&t)? else {
            return Ok(None);
        };
        let me = str_of(&user, "id");
        let mine = r.workspaces_of_user(&me)?;
        let ws = mine
            .iter()
            .find(|w| {
                want.as_deref()
                    .is_some_and(|x| w.get("id").and_then(Value::as_str) == Some(x))
            })
            .or(mine.first())
            .map(|w| str_of(w, "id"))
            .unwrap_or_else(|| "default".into());
        Ok(Some(Ctx { user, me, ws }))
    })
    .await
}

fn insert_caller(req: &mut Request, ctx: Ctx) {
    // the module routers read the caller from here (steve-modules B0)
    req.extensions_mut().insert(steve_modules::Caller {
        ws: ctx.ws.clone(),
        me: ctx.me.clone(),
    });
    req.extensions_mut().insert(ctx);
}

/// `/api` GETs that are top-level browser navigations (the GitHub App manifest flow): the session is
/// read when there is one, and the handler redirects with an error when there isn't.
fn session_optional(req: &Request) -> bool {
    use steve_github::manifest::{CALLBACK_PATH, INSTALLED_PATH};
    req.method() == axum::http::Method::GET
        && matches!(req.uri().path(), CALLBACK_PATH | INSTALLED_PATH)
}

/// Inserts the caller when signed in, and lets the request through either way (routes that answer a
/// missing login themselves, like `/auth/connect/callback`).
pub async fn optional_user(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    match lookup_user(&st, req.headers()).await {
        Ok(Some(ctx)) => insert_caller(&mut req, ctx),
        Ok(None) => {}
        Err(e) => tracing::warn!("optional_user: {e}"),
    }
    next.run(req).await
}

/// `requireUser` + the workspace middleware: 401 without a live session (except [`session_optional`]).
async fn require_user(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    match lookup_user(&st, req.headers()).await {
        Ok(Some(ctx)) => {
            insert_caller(&mut req, ctx);
            next.run(req).await
        }
        Ok(None) if session_optional(&req) => next.run(req).await,
        Ok(None) => {
            let login = if st.cfg.oidc.issuer.is_some() {
                json!("/auth/login")
            } else if st.cfg.dev_login {
                json!("/auth/dev")
            } else {
                Value::Null
            };
            json_response(401, &json!({ "error": "unauthenticated", "login": login }))
        }
        // a throw in requireUser reaches Hono's default handler, not api.onError
        Err(e) => {
            tracing::error!("require_user: {e}");
            text_response(500, "Internal Server Error")
        }
    }
}

/// Unknown `/api` paths: GET falls through to the SPA index (after auth), anything else is Hono's 404.
async fn api_fallback(State(st): State<AppState>, req: Request) -> Response {
    crate::web::serve(&st.cfg, req).await
}

/// The `/api` router with every route of api.ts, in declaration order.
pub fn router(st: AppState, modules: Router) -> Router {
    Router::new()
        .route("/updates", get(updates::status).post(updates::request))
        .route("/updates/check", post(updates::check))
        .route("/updates/{id}", get(updates::get))
        .route("/updates/{id}/approve", post(updates::approve))
        .route("/updates/{id}/reject", post(updates::reject))
        .route("/me", get(misc::me))
        .route("/state", get(misc::state))
        // workspaces
        .route("/workspaces", post(misc::workspace_create))
        .route("/workspaces/{id}", patch(misc::workspace_patch))
        .route("/workspaces/{id}/switch", post(misc::workspace_switch))
        // attachments
        .route("/files", post(files::upload))
        .route("/files/{id}", get(files::download))
        // conversation
        .route("/thread", post(misc::thread_say))
        .route("/thread/stop", post(misc::thread_stop))
        .route("/notifications/read", post(misc::notifications_read))
        .route(
            "/notifications/{id}/dismiss",
            post(misc::notifications_dismiss),
        )
        .route("/thread/{id}/promote", post(misc::thread_promote))
        .route("/thread/{id}/reroute", post(misc::thread_reroute))
        .route("/thread/{id}/offer", post(misc::thread_offer))
        .route("/thread/{id}/suggest", post(misc::thread_suggest))
        .route("/thread/runs/{id}/stop", post(misc::thread_run_stop))
        .route("/thread/runs/{id}", delete(misc::thread_run_stop))
        .route("/items/{id}/thread", patch(misc::item_thread_follow))
        // items (`/items/bulk` before `/items/{id}` matters only for POST, which axum resolves statically)
        .route("/items", post(items::create))
        .route("/items/bulk", post(items::bulk))
        .route(
            "/items/{id}",
            get(items::get_one)
                .patch(items::patch_one)
                .delete(items::delete_one),
        )
        .route("/items/{id}/actions", post(items::actions))
        .route("/items/{id}/goal", post(items::goal))
        .route("/items/{id}/supplement", post(items::supplement))
        .route("/items/{id}/deliver", post(items::deliver))
        .route("/items/{id}/reassign", post(items::reassign))
        .route("/items/{id}/deps", post(items::add_dep))
        .route("/items/{id}/deps/{dep}", delete(items::rm_dep))
        .route("/items/{id}/snooze", post(items::snooze))
        // item sidebar panels of the modules (B1)
        .route("/items/{id}/panels", get(items::panels))
        .route(
            "/items/{id}/panels/{panel}/actions/{action}",
            post(items::panel_action),
        )
        // sessions
        .route("/sessions", get(sessions::list))
        .route("/sessions/search", get(sessions::search))
        .route(
            "/sessions/{id}",
            get(sessions::get_one)
                .patch(sessions::patch_one)
                .delete(sessions::delete_one),
        )
        .route("/sessions/{id}/events", get(sessions::events))
        .route("/sessions/{id}/messages", post(sessions::message))
        .route("/sessions/{id}/cancel", post(sessions::cancel))
        .route("/sessions/{id}/end", post(sessions::end))
        .route("/sessions/{id}/fork", post(sessions::fork))
        .route("/sessions/{id}/link", post(sessions::link))
        .route("/sessions/{id}/queued", get(sessions::queued_list))
        .route(
            "/sessions/{id}/queued/{qid}",
            patch(sessions::queued_edit).delete(sessions::queued_remove),
        )
        .route(
            "/sessions/{id}/queued/{qid}/send-now",
            post(sessions::queued_send_now),
        )
        .route("/queued/{id}", delete(sessions::drop_queued))
        .route("/local-sessions", get(sessions::local))
        .route("/local-sessions/import", post(sessions::import))
        // decisions
        .route("/decisions/{id}", post(misc::decision_resolve))
        .route("/decisions/{id}/dismiss", post(misc::decision_dismiss))
        .route("/decisions/{id}/undo", post(misc::decision_undo))
        // bots
        .route("/bots", post(bots::create))
        .route("/bots/draft", post(bots::draft))
        .route(
            "/bots/{id}",
            patch(bots::patch_one).delete(bots::delete_one),
        )
        .route("/bots/{id}/prompt", get(bots::prompt))
        .route("/bots/{id}/workspaces", put(bots::set_workspaces))
        .route("/bots/{id}/duplicate", post(bots::duplicate))
        .route("/bots/{id}/restore", post(bots::restore))
        .route(
            "/bots/{id}/memories",
            get(bots::memories).post(bots::memory_add),
        )
        .route("/memories", delete(bots::memories_clear))
        .route(
            "/memories/{id}",
            patch(bots::memory_edit).delete(bots::memory_remove),
        )
        // projects and filesystem
        .route("/projects", post(projects::create))
        .route("/projects/describe", post(projects::describe))
        .route("/projects/{id}", patch(projects::patch_one))
        .route("/fs/dirs", get(projects::dirs))
        // tags
        .route("/tags", post(misc::tag_create))
        .route(
            "/tags/{id}",
            patch(misc::tag_patch).delete(misc::tag_remove),
        )
        .route("/tags/{id}/merge", post(misc::tag_merge))
        // settings and conversational configuration
        .route("/settings/schema", get(misc::settings_schema))
        .route(
            "/settings/models",
            get(misc::models_get).patch(misc::models_patch),
        )
        .route("/settings", patch(misc::settings_patch))
        .route("/config/draft", post(misc::config_draft))
        .route("/config/{id}/apply", post(misc::config_apply))
        .route("/config/{id}/undo", post(misc::config_undo))
        // devices
        .route("/devices", get(misc::devices))
        .route("/devices/{id}", patch(misc::device_patch))
        .route("/system/models", get(misc::system_models))
        // event stream
        .route("/stream", get(stream::stream))
        .fallback(api_fallback)
        .method_not_allowed_fallback(api_fallback)
        .with_state(st.clone())
        // the new modules (their paths do not overlap the TS routes), behind the same login
        .merge(modules)
        // Hono reads bodies without a limit; /files enforces its own
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(middleware::from_fn_with_state(st, require_user))
}

// ── helpers shared by the route modules ──

pub(crate) type ApiResult = Result<Response, ApiError>;

pub(crate) fn ok(v: Value) -> ApiResult {
    Ok(json_response(200, &v))
}

pub(crate) fn ok_true() -> ApiResult {
    ok(json!({ "ok": true }))
}

/// Parses a request body like Hono's `c.req.json()` (invalid JSON → 500 SyntaxError message).
pub(crate) fn body(b: &Bytes) -> Result<Value, ApiError> {
    crate::js::parse_json(&String::from_utf8_lossy(b))
}

pub(crate) fn str_of(o: &Obj, k: &str) -> String {
    o.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `own(x, what)`: the object must exist and belong to the current workspace, else `404 没有这个<what>`.
pub(crate) fn own(x: Option<Obj>, ws: &str, what: &str) -> BResult<Obj> {
    match x {
        Some(o) if o.get("workspace_id").and_then(Value::as_str) == Some(ws) => Ok(o),
        _ => Err(BackendError::not_found(format!("没有这个{what}"))),
    }
}

/// `ownBot(id)`: the bot must be a member of the current workspace (deleted bots too).
pub(crate) fn own_bot(r: &dyn Repo, id: &str, ws: &str) -> BResult<()> {
    if r.bot_in_workspace(id, ws)? {
        Ok(())
    } else {
        Err(BackendError::not_found("没有这个 Bot"))
    }
}

/// The `summary()` extras of one item, the per-row way.
pub(crate) fn extras_one(r: &dyn Repo, id: &str) -> BResult<ItemExtras> {
    let status = |s: &Obj| {
        s.get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let sessions: Vec<(String, String)> = r
        .sessions_of_item(id)?
        .iter()
        .map(|s| (str_of(s, "id"), status(s)))
        .collect();
    let open = r
        .decisions_of_item(id)?
        .into_iter()
        .filter(|d| d.get("status").and_then(Value::as_str) == Some("open"))
        .map(|d| str_of(&d, "id"))
        .collect();
    let queued = match sessions.iter().rev().find(|(_, st)| st != "ended") {
        Some((sid, _)) => r.queued_list(sid)?.len(),
        None => 0,
    };
    Ok(ItemExtras {
        goal: r.item_goal(id)?,
        goals: r.item_goals(id)?.len(),
        tags: r.tags_of_item(id)?,
        deps: r.deps_of(id)?,
        sessions,
        open,
        queued,
    })
}

/// `{...i, goal, goals, tags, deps, sessionIds, open, queued, live}`.
pub(crate) fn summary_with(mut i: Obj, x: ItemExtras) -> Obj {
    let live = x
        .sessions
        .iter()
        .any(|(_, st)| st == "running" || st == "starting");
    i.insert("goal".into(), json!(x.goal));
    i.insert("goals".into(), json!(x.goals));
    i.insert("tags".into(), json!(x.tags));
    i.insert("deps".into(), json!(x.deps));
    i.insert(
        "sessionIds".into(),
        Value::Array(
            x.sessions
                .into_iter()
                .map(|(id, _)| Value::String(id))
                .collect(),
        ),
    );
    i.insert("open".into(), json!(x.open));
    i.insert("queued".into(), json!(x.queued));
    i.insert("live".into(), json!(live));
    i
}

pub(crate) fn summary(r: &dyn Repo, i: Obj) -> BResult<Obj> {
    let x = extras_one(r, &str_of(&i, "id"))?;
    Ok(summary_with(i, x))
}

/// `items.all(ws).map(summary)`, batched when the backend can.
pub(crate) fn summaries(r: &dyn Repo, ws: &str) -> BResult<Vec<Obj>> {
    let items = r.items_all(ws)?;
    match r.item_extras(ws)? {
        Some(mut m) => Ok(items
            .into_iter()
            .map(|i| {
                let x = m.remove(&str_of(&i, "id")).unwrap_or_default();
                summary_with(i, x)
            })
            .collect()),
        None => items.into_iter().map(|i| summary(r, i)).collect(),
    }
}

/// `sessOut`: the raw row without `token_hash`/`import_key`, `readonly` as a boolean, plus `busy` and `queued`.
pub(crate) fn sess_out(r: &dyn Repo, d: &dyn Domain, s: Obj) -> BResult<Obj> {
    let q = r.queued_list(&str_of(&s, "id"))?;
    Ok(sess_out_with(d, s, q))
}

pub(crate) fn sess_out_with(d: &dyn Domain, mut s: Obj, queued: Vec<Obj>) -> Obj {
    let id = str_of(&s, "id");
    // shift_remove keeps the column order of the remaining keys
    s.shift_remove("token_hash");
    s.shift_remove("import_key");
    let ro = crate::js::truthy(s.get("readonly"));
    s.insert("readonly".into(), json!(ro));
    s.insert("busy".into(), json!(d.is_busy(&id)));
    s.insert("queued".into(), objs(queued));
    s
}

/// `sessions.list(ws).map(sessOut)`, batched when the backend can.
pub(crate) fn sessions_out(r: &dyn Repo, d: &dyn Domain, ws: &str) -> BResult<Vec<Obj>> {
    let list = r.sessions_list(ws)?;
    match r.queued_by_session(ws)? {
        Some(mut m) => Ok(list
            .into_iter()
            .map(|s| {
                let q = m.remove(&str_of(&s, "id")).unwrap_or_default();
                sess_out_with(d, s, q)
            })
            .collect()),
        None => list.into_iter().map(|s| sess_out(r, d, s)).collect(),
    }
}

/// `botOut`: the bot plus its workspace ids and its memory count in `ws`.
pub(crate) fn bot_out(r: &dyn Repo, mut b: Obj, ws: &str) -> BResult<Obj> {
    let id = str_of(&b, "id");
    b.insert("workspaces".into(), json!(r.bot_workspaces(&id)?));
    b.insert("memories".into(), json!(r.memories_count(&id, ws)?));
    Ok(b)
}

/// The workspace ids of a bot, from the batch when there is one.
fn bot_ws(r: &dyn Repo, x: Option<&crate::backend::BotExtras>, id: &str) -> BResult<Vec<String>> {
    match x {
        Some(x) => Ok(x.workspaces.get(id).cloned().unwrap_or_default()),
        None => r.bot_workspaces(id),
    }
}

/// `bots.list(ws).map(botOut)` and `allBots`, batched when the backend can.
pub(crate) fn bots_out(r: &dyn Repo, ws: &str) -> BResult<(Vec<Obj>, Vec<Value>)> {
    let x = r.bot_extras(ws)?;
    let mut bots = vec![];
    for mut b in r.bots_list(Some(ws))? {
        let id = str_of(&b, "id");
        b.insert("workspaces".into(), json!(bot_ws(r, x.as_ref(), &id)?));
        let n = match &x {
            Some(x) => x.memories.get(&id).copied().unwrap_or(0),
            None => r.memories_count(&id, ws)?,
        };
        b.insert("memories".into(), json!(n));
        bots.push(b);
    }
    let mut all = vec![];
    for b in r.bots_list(None)? {
        let id = str_of(&b, "id");
        all.push(json!({ "id": id, "name": b.get("name").cloned().unwrap_or(Value::Null), "workspaces": bot_ws(r, x.as_ref(), &id)? }));
    }
    Ok((bots, all))
}

pub(crate) fn objs(v: Vec<Obj>) -> Value {
    Value::Array(v.into_iter().map(Value::Object).collect())
}

pub(crate) fn opt_obj(v: Option<Obj>) -> Value {
    v.map(Value::Object).unwrap_or(Value::Null)
}

/// `Set-Cookie` value as Hono's `setCookie` writes it.
pub(crate) fn set_cookie(
    name: &str,
    value: &str,
    path: &str,
    max_age: u64,
    secure: bool,
) -> String {
    let mut c = format!("{name}={value}; Max-Age={max_age}; Path={path}; HttpOnly");
    if secure {
        c.push_str("; Secure");
    }
    c.push_str("; SameSite=Lax");
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn cookies() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; steve_ws=ws_x; steve_session=t%2Bk; steve_ws=second"),
        );
        assert_eq!(cookie(&h, "steve_ws").as_deref(), Some("ws_x"));
        assert_eq!(cookie(&h, "steve_session").as_deref(), Some("t+k"));
        assert_eq!(cookie(&h, "nope"), None);
    }

    #[test]
    fn cookie_attributes_like_hono() {
        assert_eq!(
            set_cookie("steve_ws", "ws_1", "/", 31_536_000, false),
            "steve_ws=ws_1; Max-Age=31536000; Path=/; HttpOnly; SameSite=Lax"
        );
        assert_eq!(
            set_cookie("steve_ws", "ws_1", "/", 1, true),
            "steve_ws=ws_1; Max-Age=1; Path=/; HttpOnly; Secure; SameSite=Lax"
        );
    }

    #[test]
    fn own_checks_workspace() {
        let o: Obj =
            serde_json::from_value(json!({"id": "ST-1", "workspace_id": "default"})).expect("obj");
        assert!(own(Some(o.clone()), "default", "事项").is_ok());
        assert_eq!(
            own(Some(o), "ws_b", "事项").unwrap_err(),
            BackendError::not_found("没有这个事项")
        );
        assert_eq!(
            own(None, "default", "会话").unwrap_err().message,
            "没有这个会话"
        );
    }
}

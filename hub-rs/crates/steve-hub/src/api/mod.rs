//! REST + SSE for the web app (api.ts). Thin: validation here, behaviour in steve-core.
//! Every request runs in one workspace (header `x-steve-ws` or cookie `steve_ws`, checked against membership).

mod annotations;
mod bots;
pub mod dedupe;
mod diagnostics;
mod files;
mod items;
mod misc;
mod node_env;
mod outputs;
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
    /// `client_id` of recent sends (`POST /thread`, `POST /items`), so a retry does not send twice.
    pub dedupe: dedupe::Dedupe,
    /// The new modules (steve-modules), for config drafts and decisions they own.
    pub modules: Option<steve_modules::Modules>,
    /// Remote Nodes (enabled by default): pairing, the `/node/ws` listener, their coordinators.
    pub nodes: Option<Arc<crate::nodes::NodeService>>,
    /// The local Node's coordinator (`STEVE_NODE_MODE=local`), for `/healthz`.
    pub node: Option<Arc<crate::wire::node_runtime::NodeRuntime>>,
    /// Where projects' and items' files are (which Node); `None` reads the Hub's own disk.
    pub workspaces: Option<Arc<crate::wire::workspaces::Workspaces>>,
    /// Single sign-on as configured now (A4b): set up or changed in the settings without a restart.
    pub oidc: Option<Arc<dyn crate::access::OidcSwitch>>,
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
            dedupe: Default::default(),
            modules: None,
            nodes: None,
            node: None,
            workspaces: None,
            oidc: None,
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

    /// The same state with single sign-on's switch attached (as wired).
    pub fn with_oidc(self, oidc: Option<Arc<dyn crate::access::OidcSwitch>>) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.oidc = oidc;
                AppState(Arc::new(inner))
            }
            Err(shared) => AppState(shared),
        }
    }

    /// Single sign-on is configured now.
    pub fn oidc_enabled(&self) -> bool {
        match &self.oidc {
            Some(o) => o.enabled(),
            None => self.cfg.oidc.issuer.is_some(),
        }
    }

    /// The same state with the remote Nodes' service attached (as wired).
    pub fn with_nodes(self, nodes: Option<Arc<crate::nodes::NodeService>>) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.nodes = nodes;
                AppState(Arc::new(inner))
            }
            Err(shared) => AppState(shared),
        }
    }

    /// The same state with the local Node's coordinator attached (as wired).
    pub fn with_node(self, node: Option<Arc<crate::wire::node_runtime::NodeRuntime>>) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                // the local Node runs the harness binaries for the harness list and Codex's models
                if let Some(n) = &node {
                    inner.runtimes.set_node(n.node());
                    inner.catalog.set_node(n.node());
                }
                inner.node = node;
                AppState(Arc::new(inner))
            }
            Err(shared) => AppState(shared),
        }
    }

    /// The same state with the workspace router attached (as wired).
    pub fn with_workspaces(
        self,
        workspaces: Option<Arc<crate::wire::workspaces::Workspaces>>,
    ) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.workspaces = workspaces;
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
    // the request's language goes along: errors and labels built off the runtime are in it too
    tokio::task::spawn_blocking(steve_i18n::carry(f))
        .await
        .map_err(|e| ApiError::internal(format!("task failed: {e}")))?
}

/// The signed-in user and the workspace this request runs in.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub user: Obj,
    pub me: String,
    pub ws: String,
    /// The client signed in with an access token, and the route of this request; `None` for a
    /// browser session (OIDC, development login).
    pub client: Option<crate::access::ClientRef>,
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

/// Who a request is from, and in which workspace.
enum Who {
    Anonymous,
    User(Box<Ctx>),
    /// Signed in, but the workspace it is for can't be told or isn't theirs: refused with this status
    /// and message, never quietly run in another workspace.
    Refused(u16, Refusal),
}

/// Why a signed-in request is refused its workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The page named a workspace the user is not (or no longer) a member of.
    Gone,
    /// A write from a browser page that does not say its workspace (a page loaded before the Hub required it).
    Reload,
    /// The user belongs to no workspace.
    NoWorkspace,
}

impl Refusal {
    /// The `{"error"}` text, in the request's language.
    pub fn message(self) -> String {
        match self {
            Refusal::Gone => steve_i18n::t!("hub.workspace.gone"),
            Refusal::Reload => steve_i18n::t!("hub.workspace.reload"),
            Refusal::NoWorkspace => steve_i18n::t!("hub.workspace.none"),
        }
    }
}

/// The value of query parameter `name` (percent-decoded).
fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == name).then(|| percent_decode(v))
    })
}

/// The signed-in user of a request and the workspace it runs in. The page says which (`x-steve-ws`, or
/// `?ws=` where a header can't be set): a
/// workspace the user is not a member of is refused. Without the header the `steve_ws` cookie decides, else
/// the user's first workspace; a write from a browser (Fetch Metadata `sec-fetch-site`) without the header
/// is refused, so a page loaded earlier never writes into the workspace another tab switched to. A user
/// with no workspace is refused, never put into `default`.
async fn lookup_user(
    st: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
    write: bool,
    peer: Option<std::net::IpAddr>,
) -> Result<Who, ApiError> {
    let token = cookie(headers, "steve_session");
    // a client signed in with an access token says who it is with every request (A1)
    let signed = match (crate::access::credential_of(headers), st.modules.as_ref()) {
        (Some(cred), Some(m)) => {
            let store = m.store.clone();
            let found = blocking(move || {
                store
                    .access()
                    .client_user(&cred)
                    .map_err(|e| ApiError::Json(500, e.to_string()))
            })
            .await?;
            let Some((u, c)) = found else {
                return Ok(Who::Anonymous);
            };
            let (route, ip) = crate::access::route_of(peer, headers);
            crate::access::seen(&m.store, &c, &route, ip);
            Some((
                serde_json::to_value(&u)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default(),
                crate::access::ClientRef::of(&c, &route),
            ))
        }
        _ => None,
    };
    // what can't carry a header (an EventSource, an <img>, a download link) names it as `?ws=`
    let named = headers
        .get("x-steve-ws")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| query_param(query, "ws"))
        .filter(|s| !s.is_empty());
    let browser = headers.contains_key("sec-fetch-site");
    let remembered = cookie(headers, "steve_ws");
    st.read(move |r| {
        let (user, client) = match signed {
            Some((u, c)) => (u, Some(c)),
            None => {
                let Some(t) = token else {
                    return Ok(Who::Anonymous);
                };
                let Some(user) = r.user_by_session(&t)? else {
                    return Ok(Who::Anonymous);
                };
                (user, None)
            }
        };
        let me = str_of(&user, "id");
        let mine = r.workspaces_of_user(&me)?;
        let is_mine = |id: &str| {
            mine.iter()
                .any(|w| w.get("id").and_then(Value::as_str) == Some(id))
        };
        let ws = match named {
            Some(n) if is_mine(&n) => n,
            Some(_) => return Ok(Who::Refused(409, Refusal::Gone)),
            None if write && browser => return Ok(Who::Refused(409, Refusal::Reload)),
            None => match remembered.filter(|c| is_mine(c)) {
                Some(c) => c,
                None => match mine.first() {
                    Some(w) => str_of(w, "id"),
                    None => return Ok(Who::Refused(403, Refusal::NoWorkspace)),
                },
            },
        };
        Ok(Who::User(Box::new(Ctx {
            user,
            me,
            ws,
            client,
        })))
    })
    .await
}

fn writes(req: &Request) -> bool {
    !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    )
}

/// The address the request came from (the server is run with connect info).
fn peer(req: &Request) -> Option<std::net::IpAddr> {
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
}

fn insert_caller(req: &mut Request, ctx: Ctx) {
    if let Some(c) = &ctx.client {
        req.extensions_mut().insert(c.clone());
    }
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
    match lookup_user(
        &st,
        req.headers(),
        req.uri().query(),
        writes(&req),
        peer(&req),
    )
    .await
    {
        Ok(Who::User(ctx)) => insert_caller(&mut req, *ctx),
        Ok(Who::Anonymous | Who::Refused(..)) => {}
        Err(e) => tracing::warn!("optional_user: {e}"),
    }
    next.run(req).await
}

/// `requireUser` + the workspace middleware: 401 without a live session (except [`session_optional`]).
async fn require_user(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    match lookup_user(
        &st,
        req.headers(),
        req.uri().query(),
        writes(&req),
        peer(&req),
    )
    .await
    {
        Ok(Who::User(ctx)) => {
            insert_caller(&mut req, *ctx);
            next.run(req).await
        }
        Ok(Who::Refused(status, why)) => json_response(status, &json!({ "error": why.message() })),
        Ok(Who::Anonymous) if session_optional(&req) => next.run(req).await,
        Ok(Who::Anonymous) => {
            let login = if st.oidc_enabled() {
                json!("/auth/login")
            } else if st.cfg.dev_login {
                json!("/auth/dev")
            } else {
                Value::Null
            };
            // `token`: a client may sign in with a user's name and an access token (`/auth/token`)
            let token = st.modules.is_some();
            json_response(
                401,
                &json!({ "error": "unauthenticated", "login": login, "token": token }),
            )
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

/// The largest request body the API reads (uploads to `/files` have their own limit).
const BODY_MAX: usize = 16 * 1024 * 1024;

/// The `/api` router with every route of api.ts, in declaration order.
pub fn router(st: AppState, modules: Router) -> Router {
    Router::new()
        .route("/updates", get(updates::status).post(updates::request))
        .route("/updates/check", post(updates::check))
        .route("/updates/{id}", get(updates::get))
        .route("/updates/{id}/approve", post(updates::approve))
        .route("/updates/{id}/reject", post(updates::reject))
        .route("/me", get(misc::me))
        .route("/node-env/{node}", get(node_env::list))
        .route(
            "/node-env/{node}/{key}",
            put(node_env::put).delete(node_env::delete),
        )
        .route("/state", get(misc::state))
        // workspaces
        .route("/workspaces", post(misc::workspace_create))
        .route("/workspaces/{id}", patch(misc::workspace_patch))
        .route("/workspaces/{id}/switch", post(misc::workspace_switch))
        // attachments
        // reads the body itself up to its own limit, and answers a larger one with a clean 413
        .route(
            "/files",
            post(files::upload).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/files/{id}", get(files::download))
        // conversation
        .route("/thread", post(misc::thread_say))
        .route("/thread/stop", post(misc::thread_stop))
        .route("/thread/history", get(misc::thread_history))
        .route("/thread/search", get(misc::thread_search))
        .route(
            "/thread/queued/{id}",
            patch(misc::thread_queued_edit).delete(misc::thread_queued_withdraw),
        )
        .route("/notifications/read", post(misc::notifications_read))
        .route(
            "/notifications/{id}/dismiss",
            post(misc::notifications_dismiss),
        )
        .route("/thread/{id}/promote", post(misc::thread_promote))
        .route("/thread/{id}/reroute", post(misc::thread_reroute))
        .route("/thread/{id}/offer", post(misc::thread_offer))
        .route("/thread/{id}/suggest", post(misc::thread_suggest))
        .route("/thread/{id}/a2ui", post(misc::thread_a2ui))
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
        .route(
            "/items/{id}/debug",
            get(diagnostics::explain).post(diagnostics::capture),
        )
        .route("/items/{id}/debug/events", get(diagnostics::events))
        .route(
            "/items/{id}/workflow",
            get(items::workflow_get).post(items::workflow_change),
        )
        .route("/items/{id}/lifecycle", post(items::lifecycle))
        .route(
            "/items/{id}/dependency-wait",
            get(items::dependency_get).post(items::dependency_wait),
        )
        .route("/items/{id}/goal", post(items::goal))
        .route("/items/{id}/supplement", post(items::supplement))
        .route("/items/{id}/deliver", post(items::deliver))
        .route("/items/{id}/reassign", post(items::reassign))
        .route("/items/{id}/deps", post(items::add_dep))
        .route("/items/{id}/deps/{dep}", delete(items::rm_dep))
        .route("/items/{id}/snooze", post(items::snooze))
        .route("/items/{id}/artifacts/{art}/preview", get(outputs::preview))
        .route("/items/{id}/artifacts/{art}/raw", get(outputs::raw))
        .route("/items/{id}/annotations", post(annotations::add))
        .route("/items/{id}/annotations/send", post(annotations::send))
        .route(
            "/items/{id}/annotations/{ann}",
            patch(annotations::edit).delete(annotations::remove),
        )
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
        .route("/sessions/{id}/continue", post(sessions::continue_new))
        .route("/sessions/{id}/fork", post(sessions::fork))
        .route("/sessions/{id}/link", post(sessions::link))
        .route("/sessions/{id}/queued", get(sessions::queued_list))
        .route("/sessions/{id}/resume", get(sessions::resume))
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
        .route(
            "/projects/{id}/lane",
            get(projects::lane).patch(projects::lane_policy),
        )
        .route("/projects/{id}/lane/now", post(projects::lane_now))
        .route("/projects/{id}/queue", post(projects::queue_act))
        .route("/fs/dirs", get(projects::dirs))
        // tags
        .route("/tags", post(misc::tag_create))
        .route(
            "/tags/{id}",
            patch(misc::tag_patch).delete(misc::tag_remove),
        )
        .route("/tags/{id}/merge", post(misc::tag_merge))
        .route("/tags/kinds/backfill", post(misc::kinds_backfill))
        .route(
            "/tags/kinds/review",
            get(misc::kinds_review).post(misc::kinds_reviewed),
        )
        // settings · access and clients (A4)
        .route("/access", get(crate::access::overview))
        .route(
            "/access/oidc",
            get(crate::access::oidc_get)
                .put(crate::access::oidc_put)
                .delete(crate::access::oidc_delete),
        )
        .route("/access/tokens", post(crate::access::token_create))
        .route("/access/tokens/{id}", delete(crate::access::token_revoke))
        .route("/access/clients/{id}", delete(crate::access::client_revoke))
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
        .route("/config/{id}/drop", post(misc::config_drop))
        // devices
        .route("/devices", get(misc::devices))
        .route("/vitals", get(crate::nodes::vitals::get))
        .route("/devices/{id}", patch(misc::device_patch))
        .route("/system/models", get(misc::system_models))
        .route("/system/workspaces", get(misc::system_workspaces))
        .route(
            "/system/workspaces/reclaim",
            post(misc::system_workspaces_reclaim),
        )
        .route("/system/outbox", get(misc::system_outbox))
        .route("/system/outbox/revive", post(misc::system_outbox_revive))
        // event stream
        .route("/stream", get(stream::stream))
        .fallback(api_fallback)
        .method_not_allowed_fallback(api_fallback)
        .with_state(st.clone())
        // the new modules (their paths do not overlap the TS routes), behind the same login
        .merge(modules)
        // Hono reads bodies without a limit; every JSON body here (the modules' too) is far below this
        .layer(axum::extract::DefaultBodyLimit::max(BODY_MAX))
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

/// What `own()` looked for, for its `404 没有这个<noun>` in the request's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Noun {
    Item,
    Session,
    Decision,
    Queued,
    Tag,
    Memory,
    File,
    Project,
    Bot,
}

impl Noun {
    /// "没有这个事项" / "No such item".
    pub(crate) fn missing(self) -> String {
        match self {
            Noun::Item => steve_i18n::t!("hub.not_found.item"),
            Noun::Session => steve_i18n::t!("hub.not_found.session"),
            Noun::Decision => steve_i18n::t!("hub.not_found.decision"),
            Noun::Queued => steve_i18n::t!("hub.not_found.queued"),
            Noun::Tag => steve_i18n::t!("hub.not_found.tag"),
            Noun::Memory => steve_i18n::t!("hub.not_found.memory"),
            Noun::File => steve_i18n::t!("hub.not_found.file"),
            Noun::Project => steve_i18n::t!("hub.not_found.project"),
            Noun::Bot => steve_i18n::t!("hub.not_found.bot"),
        }
    }
}

/// `own(x, what)`: the object must exist and belong to the current workspace, else `404 没有这个<what>`.
pub(crate) fn own(x: Option<Obj>, ws: &str, what: Noun) -> BResult<Obj> {
    match x {
        Some(o) if o.get("workspace_id").and_then(Value::as_str) == Some(ws) => Ok(o),
        _ => Err(BackendError::not_found(what.missing())),
    }
}

/// `ownBot(id)`: the bot must be a member of the current workspace (deleted bots too).
pub(crate) fn own_bot(r: &dyn Repo, id: &str, ws: &str) -> BResult<()> {
    if r.bot_in_workspace(id, ws)? {
        Ok(())
    } else {
        Err(BackendError::not_found(Noun::Bot.missing()))
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

/// `sessOut`: the raw row without `token_hash`/`import_key`, `readonly` as a boolean, plus `busy` and `queued`,
/// and the harness's own session id (`harness_session_id`, also for an imported session).
pub(crate) fn sess_out(r: &dyn Repo, d: &dyn Domain, s: Obj) -> BResult<Obj> {
    let q = r.queued_list(&str_of(&s, "id"))?;
    Ok(sess_out_with(d, s, q))
}

pub(crate) fn sess_out_with(d: &dyn Domain, mut s: Obj, queued: Vec<Obj>) -> Obj {
    let id = str_of(&s, "id");
    let native = crate::resume::native_id(&s);
    // shift_remove keeps the column order of the remaining keys
    s.shift_remove("token_hash");
    s.shift_remove("import_key");
    let ro = crate::js::truthy(s.get("readonly"));
    s.insert("readonly".into(), json!(ro));
    s.insert("busy".into(), json!(d.is_busy(&id)));
    s.insert("queued".into(), objs(queued));
    s.insert("harness_session_id".into(), json!(native));
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
        assert!(own(Some(o.clone()), "default", Noun::Item).is_ok());
        assert_eq!(
            own(Some(o), "ws_b", Noun::Item).unwrap_err(),
            BackendError::not_found("没有这个事项")
        );
        assert_eq!(
            own(None, "default", Noun::Session).unwrap_err().message,
            "没有这个会话"
        );
    }
}

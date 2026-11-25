//! REST + SSE for the web app (api.ts). Thin: validation here, behaviour in steve-core.
//! Every request runs in one workspace (header `x-steve-ws` or cookie `steve_ws`, checked against membership).

mod annotations;
pub(crate) mod approval;
pub(crate) mod attention;
mod avatar;
mod bot_dispatch;
mod bots;
pub mod dedupe;
mod deliveries;
mod diagnostics;
pub(crate) mod feed;
mod files;
pub mod harness_updates;
mod hub_metadata;
mod items;
pub(crate) mod misc;
mod node_context;
mod node_env;
mod node_harnesses;
mod node_tags;
pub(crate) mod outputs;
mod project_setup;
mod projects;
mod sessions;
pub mod slim;
pub mod stats;
pub mod stream;
mod updates;
mod views;

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
    pub caches: Arc<steve_runtime::cache::Caches>,
    pub origins: steve_runtime::cache::Cached<String, bool>,
    pub deliveries: Option<crate::deliveries::Service>,
    pub backend: Arc<dyn Backend>,
    pub domain: Arc<dyn Domain>,
    pub bus: Bus,
    pub runtimes: Runtimes,
    /// Device observations stay separate from the merged model/dispatch catalogue.
    pub node_harnesses: node_harnesses::Inventory,
    /// The models each harness offers (`/system/models`).
    pub catalog: crate::catalog::Catalog,
    /// `client_id` of recent sends (`POST /thread`, `POST /items`), so a retry does not send twice.
    pub dedupe: dedupe::Dedupe,
    /// The new modules (steve-modules), for config drafts and decisions they own.
    pub modules: Option<steve_modules::Modules>,
    pub optional_modules: steve_types::integrations::OptionalModules,
    /// Remote Nodes (enabled by default): pairing, the `/node/ws` listener, their coordinators.
    pub nodes: Option<Arc<crate::nodes::NodeService>>,
    /// The local Node's coordinator (`STEVE_NODE_MODE=local`), for `/healthz`.
    pub node: Option<Arc<crate::wire::node_runtime::NodeRuntime>>,
    /// Local history discovery, independent of whether agent sessions run here.
    pub history: Option<Arc<dyn steve_node_api::Discovery>>,
    /// Where projects' and items' files are (which Node); `None` reads the Hub's own disk.
    pub workspaces: Option<Arc<crate::wire::workspaces::Workspaces>>,
    /// Single sign-on as configured now: set up or changed in the settings without a restart.
    pub oidc: Option<Arc<dyn crate::access::OidcSwitch>>,
    /// Which signed-in clients hold the live stream open now.
    pub presence: Arc<crate::presence::Presence>,
    pub jobs: Arc<crate::jobs::Tasks>,
    /// The read projection (views, snapshots, patches), on the modules' store.
    pub projection: Option<Arc<crate::projection::Projection>>,
}

fn runtime_routes(inner: &Inner) -> crate::projection::RuntimeRoutes {
    crate::projection::RuntimeRoutes {
        local: inner.node.as_ref().map(|n| n.node_id().to_string()),
        remote: inner.nodes.is_some(),
    }
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
        let caches = bus.caches().clone();
        let origins = caches.space(&crate::access::ORIGIN_SPACE);
        let runtimes = Runtimes::from_reports(cfg.clone(), bus.reports());
        let node_harnesses = node_harnesses::Inventory::new(caches.clone());
        let catalog = crate::catalog::Catalog::of(&cfg);
        AppState(Arc::new(Inner {
            catalog,
            deliveries: None,
            cfg,
            backend,
            jobs: domain.background_tasks().unwrap_or_default(),
            domain,
            bus,
            runtimes,
            node_harnesses,
            caches,
            origins,
            dedupe: Default::default(),
            modules: None,
            optional_modules: Default::default(),
            nodes: None,
            node: None,
            history: None,
            workspaces: None,
            oidc: None,
            presence: Default::default(),
            projection: None,
        }))
    }

    /// Stop domain resources first, then every Hub-owned task (including domains without a bag).
    pub async fn shutdown(&self) {
        self.domain.shutdown().await;
        self.jobs.stop().await;
    }

    pub(crate) fn runtime_routes(&self) -> crate::projection::RuntimeRoutes {
        runtime_routes(&self.0)
    }

    pub(crate) fn with_optional_modules(
        mut self,
        modules: steve_types::integrations::OptionalModules,
    ) -> Self {
        Arc::get_mut(&mut self.0)
            .expect("Hub state is still being assembled")
            .optional_modules = modules;
        self
    }

    /// The same state with the new modules attached.
    pub fn with_modules(self, modules: Option<steve_modules::Modules>) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.projection = modules.as_ref().map(|m| {
                    crate::projection::Projection::new(
                        m.store.clone(),
                        inner.bus.clone(),
                        runtime_routes(&inner),
                    )
                });
                inner.deliveries = modules.as_ref().map(|m| {
                    crate::deliveries::Service::new(
                        m.store.clone(),
                        inner.cfg.home.clone(),
                        inner.cfg.public_url.clone(),
                    )
                    .with_ancestors(
                        inner
                            .cfg
                            .public_urls
                            .iter()
                            .chain(&inner.cfg.client_origins)
                            .map(String::as_str),
                    )
                });
                inner.modules = modules;
                AppState(Arc::new(inner))
            }
            Err(shared) => AppState(shared),
        }
    }

    pub fn with_deliveries(self, service: crate::deliveries::Service) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.deliveries = Some(service);
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
                // the local Node runs the harness adapters: the runtimes list asks it
                if let Some(n) = &node {
                    inner.runtimes.set_node(n.node());
                }
                inner.node = node;
                AppState(Arc::new(inner))
            }
            Err(shared) => AppState(shared),
        }
    }

    pub fn with_history(self, history: Option<Arc<dyn steve_node_api::Discovery>>) -> Self {
        match Arc::try_unwrap(self.0) {
            Ok(mut inner) => {
                inner.history = history;
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
    tokio::task::spawn_blocking(crate::metrics::carry(steve_i18n::carry(f)))
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
    // Relative HTML/CSS/image navigations cannot inherit a query string or set a workspace
    // header. Derive only their workspace selector from the immutable delivery, then run the
    // ordinary user membership check below. An explicit selector still wins (and can fail).
    if matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && !req.headers().contains_key("x-steve-ws")
        && query_param(req.uri().query(), "ws").is_none()
        && let Some(id) = req
            .uri()
            .path()
            .strip_prefix("/file-deliveries/")
            .and_then(|p| p.split('/').next())
        && let Some(svc) = st.deliveries.clone()
    {
        let id = id.to_string();
        if let Ok(Some(d)) = blocking(move || {
            svc.get(&id, None)
                .map_err(|e| ApiError::internal(e.to_string()))
        })
        .await
            && let Ok(ws) = axum::http::HeaderValue::from_str(&d.workspace_id)
        {
            req.headers_mut().insert("x-steve-ws", ws);
        }
    }
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

/// API requests only match registered resources.
async fn api_fallback() -> Response {
    use axum::response::IntoResponse;
    ApiError::no_route().into_response()
}

/// The largest request body the API reads (uploads to `/files` have their own limit).
const BODY_MAX: usize = 16 * 1024 * 1024;

/// The `/api` router with every route of api.ts, in declaration order.
pub fn router(st: AppState, modules: Router) -> Router {
    let areas: &[fn() -> Router<AppState>] = &[
        deliveries::routes,
        updates::routes,
        misc::routes,
        node_env::routes,
        files::routes,
        avatar::routes,
        items::routes,
        diagnostics::routes,
        outputs::routes,
        annotations::routes,
        sessions::routes,
        bot_dispatch::routes,
        bots::routes,
        projects::routes,
        project_setup::routes,
        hub_metadata::routes,
        node_context::routes,
        node_tags::routes,
        harness_updates::routes,
        node_harnesses::routes,
        stream::routes,
        views::routes,
    ];
    let routes = areas
        .iter()
        .fold(Router::new(), |routes, area| routes.merge(area()));
    routes
        .fallback(api_fallback)
        .method_not_allowed_fallback(api_fallback)
        .with_state(st.clone())
        // the new modules (their paths do not overlap the TS routes), behind the same login
        .merge(modules)
        // Hono reads bodies without a limit; every JSON body here (the modules' too) is far below this
        .layer(axum::extract::DefaultBodyLimit::max(BODY_MAX))
        .layer(middleware::from_fn_with_state(st.clone(), require_user))
        .merge(deliveries::public_router(st))
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

pub(crate) fn sess_out(r: &dyn Repo, d: &dyn Domain, s: Obj) -> BResult<Obj> {
    let id = str_of(&s, "id");
    let q = r.queued_list(&id)?;
    let room = r.waiting_for_room()?.remove(&id);
    Ok(sess_out_with(d, s, q, room))
}

pub(crate) fn sess_out_with(
    d: &dyn Domain,
    mut s: Obj,
    queued: Vec<Obj>,
    room: Option<Value>,
) -> Obj {
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
    s.insert("room".into(), room.unwrap_or(Value::Null));
    s
}

/// Workspaces as the pages show them: a seeded name in the request's language ([`steve_store::seeded`]).
pub(crate) fn workspaces_out(mut ws: Vec<Obj>) -> Vec<Obj> {
    for w in &mut ws {
        steve_store::seeded::show(w, steve_store::seeded::WORKSPACE);
    }
    ws
}

/// `botOut`: the bot plus its workspace ids and its memory count in `ws`.
pub(crate) fn bot_out(r: &dyn Repo, mut b: Obj, ws: &str) -> BResult<Obj> {
    let id = str_of(&b, "id");
    steve_store::seeded::show(&mut b, steve_store::seeded::BOT);
    b.insert("workspaces".into(), json!(r.bot_workspaces(&id)?));
    b.insert("memories".into(), json!(r.memories_count(&id, ws)?));
    b.insert("mcp_servers".into(), json!(r.bot_mcp_servers(&id)?));
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
    let x = r.bot_extras()?;
    let mut bots = vec![];
    for mut b in r.bots_list(Some(ws))? {
        let id = str_of(&b, "id");
        steve_store::seeded::show(&mut b, steve_store::seeded::BOT);
        b.insert("workspaces".into(), json!(bot_ws(r, x.as_ref(), &id)?));
        let mcp = match &x {
            Some(x) => x.mcp.get(&id).cloned().unwrap_or_default(),
            None => r.bot_mcp_servers(&id)?,
        };
        b.insert("mcp_servers".into(), json!(mcp));
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

pub(crate) use node_harnesses::CACHE_SPEC as NODE_CACHE_SPEC;

#[cfg(test)]
pub(crate) use node_harnesses::register_cache_spaces as register_node_cache_spaces;

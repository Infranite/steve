//! GitHub: connections, repositories, pull requests and panels
//!
//! Slices so far:
//! - G0: [`fake`] (feature `fake`), [`signature`];
//! - G1: the API client ([`api`]: headers, ETag, rate limits, 401 → re-mint once → `reauth`), App
//!   authentication ([`auth`]: JWT, installation tokens), the two connection adapters
//!   ([`adapters::GitHubApp`] `github_app`, [`adapters::GitHubPat`] `github_pat`), and the App
//!   manifest flow ([`manifest`]: create → install → connection);
//! - G2: linked repositories and their REST ([`repos`]), the webhook entries ([`hooks`]), state
//!   sync and signals ([`sync`]), the polling fallback ([`poll`]), the delivery log
//!   (`gh_deliveries`, [`repo`]), and the seams to S1 / SSE ([`events`]).
//!
//! Integration (integrator):
//! 1. register the adapters on the connections builder:
//!    `let env = GitHubEnv::from_env(|k| std::env::var(k).ok());`
//!    `for p in steve_github::providers_for(&env) { builder = builder.provider(p) }`;
//! 2. after `build()`: `let gh = GitHub::new(conns.clone(), env);`
//! 3. `.merge(steve_github::manifest::routes(gh.clone()))` and
//!    `.merge(steve_github::repos::routes(gh.clone()))` under `/api` (the `Caller` layer; the two
//!    manifest GETs are browser navigations, the session cookie is enough);
//! 4. at the root, without session: `.merge(steve_github::hooks::routes(gh.clone()))` serves
//!    `POST /hooks/github` (the App's webhook URL `<STEVE_PUBLIC_URL>/hooks/github`). The signals
//!    route `POST /hooks/:connection_id` hands connections whose adapter passes
//!    [`hooks::handles`] (`github_pat`) to [`hooks::connection_hook`] (or
//!    [`GitHub::receive_connection`] with the raw body ≤ [`hooks::BODY_LIMIT`]);
//! 5. signals: `gh.set_event_sink(Arc<impl EventSink>)` forwarding to
//!    `steve_flow::EventSink::ingest(ws, Some(connection_id), RawEvent { source: "github", .. })`
//!    (field mapping on [`EventSink`]); register `github` as an event source with the types
//!    `github.check_failed|check_passed|review_requested|review|pr_comment|pr_merged|pr_closed|issue_opened`;
//! 6. SSE: add `ChangeEvent::Gh { id, ws }` (`{type:"gh", id, ws}`) and `"gh"` to
//!    `steve_modules::sse::MODULE_TYPES`, then `gh.set_notifier(..)` publishing it;
//! 7. `gh.spawn_jobs()` once the runtime is up (poll every 30 s what is due, prune deliveries).
//!
//! G3 (items, opening and merging PRs: [`link`], [`accept`], [`push`], [`flow`]):
//! 8. before `Modules` exists: `let gh_slot = steve_github::accept::register(&mut registry);`
//!    (decision handlers `gh_pr_open`, `gh_pr_merge`; item hooks `on_accepted`); after
//!    `GitHub::new`: `gh_slot.bind(&gh)`;
//! 9. `gh.set_core(Arc<impl ItemCore>)` over the orchestrator ([`core::ItemCore`]: `progress` =
//!    `item_patch(stage, now)` with the item kept open, `finish`, `fail`);
//! 10. flows: on the `FlowSetup`, `.item_finder("gh_pr", Arc::new(steve_github::flow::GhPrFinder))`
//!     and `.action(Arc::new(steve_github::flow::GhItemLink))` (S2's `item.link` plus `by:"gh_pr"`;
//!     every other `by` goes to S2's action); after `build`: `gh.set_flow(flow.clone())` (installs
//!     the “PR 检查失败” template when the first repo is linked). Point 5 can now be
//!     `gh.set_event_sink(Arc::new(steve_github::flow::FlowSink(Arc::new(flow.clone()))))`;
//! 11. `POST /github/prs/{id}/link` is part of [`repos::routes`]; nothing else to mount;
//! 12. web: 待处理 cards for `gh_pr_open` (title and body editable, sent as `text`, first line =
//!     title) and `gh_pr_merge` (the draft is the check list).
//!
//! G4 (agents' tools, the item panel, comments: [`mcp`], [`panel`], [`comment`]):
//! 13. MCP: list [`mcp::tools`] after the Steve tools; route names with [`mcp::handles`] to
//!     [`mcp::call`]`(&gh, &McpCx, name, args)`; `awaiting_approval` → mark the session
//!     pending=approval and set the item's `now` as for `steve_request_approval`. Only
//!     [`mcp::READ_ONLY`] (`steve_github_status`, `steve_github_ci_log`) may join steve-acp's
//!     `OWN_TOOLS` auto-allow list; the comment and open-PR tools do nothing outward without your
//!     answer or grant, but leave them to the harness prompt like other side-effect tools;
//! 14. `gh.set_session_sink(..)` with steve-core's “send to the item's session” (the vault's
//!     [`SessionSink`] trait): answers to agents' `gh_pr_open` / `gh_comment` requests;
//! 15. `.merge(steve_github::comment::routes(gh.clone()))` under `/api` (`POST /github/comments`);
//! 16. panels and the `gh_comment` handler are registered by `accept::register` (point 8);
//!     steve-hub serves `GET /api/items/:id/panels` and `POST …/panels/:panel/actions/:action`
//!     through `steve_modules::panels`;
//! 17. web: 待处理 card for `gh_comment` (draft editable → `text`; options `yes`/`no`/`grant_8h`
//!     “允许 8 小时”); vault grants list may show `capability=github.comment` rows (resource = repo).
//!
//! G5 (conversational config, issues: [`config`], [`issues`]):
//! 18. `accept::register` (point 8) also registers the draft target `gh_repo` and the operations
//!     `gh.repo.patch`, `gh.grant.add`: route `target.kind = "gh_repo"` to
//!     `steve_modules::changes::draft`; its `global` candidates are `gh_repo:<ghr> — 仓库 <full_name>`;
//! 19. `.merge(steve_github::issues::routes(gh.clone()))` under `/api`
//!     (`POST /github/issues/:id/to-item {bot?, mode?}` → `{item_id}`); the item is made through
//!     the flow's `Core::create_item` (point 10's `gh.set_flow`);
//! 20. `GET /github/repos/:id` is part of [`repos::routes`];
//! 21. V6 (vault `grant.add` drafts): resolve `capability = "github.comment"` through
//!     [`config::CommentGrants`] ([`config::GrantCapability`]: Chinese name “在 GitHub 评论”, the
//!     repo as resource, `plan_grant` → a `gh.grant.add` operation); the grants list shows the
//!     resource with `resource_label`;
//! 22. web: `#/github/:repo` reads `GET /github/repos/:id`; Issue rows' “转成事项” posts to-item.

steve_i18n::init!();

pub mod accept;
pub mod adapters;
pub mod after;
pub mod api;
pub mod auth;
pub mod comment;
pub mod config;
pub mod core;
pub mod effects;
pub mod events;
pub mod flow;
pub mod hooks;
pub mod issues;
pub mod link;
pub mod manifest;
pub mod mcp;
pub mod mq;
pub mod panel;
pub mod poll;
pub mod push;
pub mod repo;
pub mod repos;
pub mod signature;
pub mod sync;

#[cfg(feature = "fake")]
pub mod fake;

use std::sync::{Arc, RwLock};

use axum::http::HeaderMap;
use steve_modules::accounts::connections::{ConnectionProvider, Connections, PendingStates};
use steve_modules::vault::{SessionSink, VaultResult};
use steve_store::Store;

pub use crate::core::{ItemCore, NoCore, RecordingCore};
pub use adapters::{GitHubApp, GitHubPat};
pub use api::{EtagCache, GhClient, GhConn, GhError, GhResponse};
pub use events::{Actor, EventSink, GhNotifier, NoSink, RecordingSink, SignalEvent};
pub use manifest::ManifestFlow;

/// The adapters, for `ConnectionsBuilder::provider`, on the process environment's
/// [`GitHubEnv::from_env`]. Prefer [`providers_for`] with the same env as [`GitHub::new`].
pub fn providers() -> Vec<Arc<dyn ConnectionProvider>> {
    providers_for(&GitHubEnv::from_env(|k| std::env::var(k).ok()))
}

/// The adapters on `env`: a `github_pat` connection without `config.api_base` uses
/// `env.api_base` (an App connection has its base in its config from the manifest flow).
pub fn providers_for(env: &GitHubEnv) -> Vec<Arc<dyn ConnectionProvider>> {
    vec![Arc::new(GitHubApp), Arc::new(GitHubPat::new(&env.api_base))]
}

/// Where GitHub is. Real github.com unless the test guard allows otherwise.
#[derive(Debug, Clone)]
pub struct GitHubEnv {
    /// REST base (`https://api.github.com`), written into new App connections' `config.api_base`.
    pub api_base: String,
    /// Web base for the manifest form and App pages (`https://github.com`).
    pub web_base: String,
    /// Where branches are pushed: `<git_base>/<owner>/<name>.git` (`https://github.com`; a test's
    /// bare remotes).
    pub git_base: String,
}

impl Default for GitHubEnv {
    fn default() -> Self {
        Self {
            api_base: api::DEFAULT_API_BASE.into(),
            web_base: "https://github.com".into(),
            git_base: "https://github.com".into(),
        }
    }
}

impl GitHubEnv {
    /// `STEVE_GITHUB_API_BASE` / `STEVE_GITHUB_WEB_BASE` / `STEVE_GITHUB_GIT_BASE` (e.g. a
    /// `steve-fake-github` and its bare remotes) count only
    /// when `STEVE_HOME` is set and is not `~/.steve` — the same guard as the connections'
    /// `api_base`, so a real Hub always talks to github.com.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let home = var("STEVE_HOME");
        let real = var("HOME").map(|h| format!("{}/.steve", h.trim_end_matches('/')));
        let allow = matches!((&home, &real), (Some(h), Some(r)) if h.trim_end_matches('/') != r);
        let mut env = Self::default();
        if allow {
            let pick = |k: &str| {
                var(k)
                    .map(|v| v.trim().trim_end_matches('/').to_string())
                    .filter(|v| !v.is_empty())
            };
            if let Some(v) = pick("STEVE_GITHUB_API_BASE") {
                env.api_base = v;
            }
            if let Some(v) = pick("STEVE_GITHUB_WEB_BASE") {
                env.web_base = v;
            }
            if let Some(v) = pick("STEVE_GITHUB_GIT_BASE") {
                env.git_base = v;
            }
        }
        env
    }
}

/// The GitHub module's service. Cheap to clone.
#[derive(Clone)]
pub struct GitHub(Arc<Inner>);

struct Inner {
    conns: Connections,
    env: GitHubEnv,
    flows: PendingStates<ManifestFlow>,
    etags: EtagCache,
    sink: RwLock<Arc<dyn EventSink>>,
    notifier: RwLock<Arc<dyn GhNotifier>>,
    queue: hooks::Queue,
    limiter: hooks::Limiter,
    secrets: hooks::Secrets,
    available: repos::AvailableCache,
    core: RwLock<Arc<dyn ItemCore>>,
    flow: RwLock<Option<Arc<steve_flow::FlowCx>>>,
    /// Wakes the effect runner ([`effects`]) after an answer queued a write.
    effect_wake: Arc<tokio::sync::Notify>,
    sessions: RwLock<Option<Arc<dyn SessionSink>>>,
    /// Items' git on their Nodes ([`push::GitNodes`]); none: the Hub's own git.
    git_nodes: RwLock<Option<Arc<dyn push::GitNodes>>>,
}

impl GitHub {
    pub fn new(conns: Connections, env: GitHubEnv) -> Self {
        Self(Arc::new(Inner {
            conns,
            env,
            flows: PendingStates::default(),
            etags: EtagCache::default(),
            sink: RwLock::new(Arc::new(NoSink)),
            notifier: RwLock::new(Arc::new(NoSink)),
            queue: Default::default(),
            limiter: Default::default(),
            secrets: Default::default(),
            available: Default::default(),
            core: RwLock::new(Arc::new(NoCore)),
            flow: RwLock::new(None),
            effect_wake: Default::default(),
            sessions: RwLock::new(None),
            git_nodes: RwLock::new(None),
        }))
    }

    /// steve-core for the wrap-up (finish, fail, progress). Default: [`NoCore`].
    pub fn set_core(&self, c: Arc<dyn ItemCore>) {
        *self.0.core.write().unwrap_or_else(|e| e.into_inner()) = c;
    }

    fn core(&self) -> Arc<dyn ItemCore> {
        self.0
            .core
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// steve-core's “send to the item's session” for the answers to agents' requests (a PR opened
    /// or not, a comment posted or not). Default: logged and dropped.
    pub fn set_session_sink(&self, s: Arc<dyn SessionSink>) {
        *self.0.sessions.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }

    /// Reads and pushes items' branches on the Nodes that hold them (a Hub with its local Node
    /// on). Default: the Hub's own git ([`push`]).
    pub fn set_git_nodes(&self, n: Arc<dyn push::GitNodes>) {
        *self.0.git_nodes.write().unwrap_or_else(|e| e.into_inner()) = Some(n);
    }

    pub(crate) fn git_nodes(&self) -> Option<Arc<dyn push::GitNodes>> {
        self.0
            .git_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Tells the agent's session how its request ended.
    pub(crate) async fn tell_session(
        &self,
        ws: &str,
        item: Option<&str>,
        session: Option<&str>,
        text: &str,
    ) {
        let s = self
            .0
            .sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match s {
            Some(s) => s.send(ws, item, session, text).await,
            None => tracing::warn!(item = ?item, "github: no session sink, message dropped"),
        }
    }

    /// Where signal events go (S1's `ingest`, wired by the integrator). Default: dropped.
    pub fn set_event_sink(&self, sink: Arc<dyn EventSink>) {
        *self.0.sink.write().unwrap_or_else(|e| e.into_inner()) = sink;
    }

    /// Where `{type:"gh", id, ws}` notices go. Default: dropped.
    pub fn set_notifier(&self, n: Arc<dyn GhNotifier>) {
        *self.0.notifier.write().unwrap_or_else(|e| e.into_inner()) = n;
    }

    pub(crate) fn sink(&self) -> Arc<dyn EventSink> {
        self.0
            .sink
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn notifier(&self) -> Arc<dyn GhNotifier> {
        self.0
            .notifier
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn connections(&self) -> &Connections {
        &self.0.conns
    }

    pub fn env(&self) -> &GitHubEnv {
        &self.0.env
    }

    pub fn store(&self) -> &Store {
        &self.0.conns.modules().store
    }

    fn now(&self) -> i64 {
        self.0.conns.modules().clock.now()
    }

    /// The primary public URL (`STEVE_PUBLIC_URL`): where GitHub sends webhooks.
    pub fn public_url(&self) -> String {
        self.0.conns.origin(&HeaderMap::new())
    }

    /// A client without a token on the module's API base (App JWT calls, manifest conversion).
    pub fn client(&self) -> GhClient {
        GhClient::new(self.0.conns.http().clone(), &self.0.env.api_base)
    }

    /// The API client of a GitHub connection in `ws` (404 when it isn't one).
    pub fn conn(&self, ws: &str, connection_id: &str) -> VaultResult<GhConn> {
        self.gh_conn(self.0.conns.cx(ws, connection_id)?)
    }

    /// The client for a write (push, PR, merge, comment, rerun), checked when it happens: refused
    /// while the connection is paused, to be authorized again or revoked, or `repo.write` is off.
    pub fn conn_for_write(&self, ws: &str, connection_id: &str) -> VaultResult<GhConn> {
        self.gh_conn(
            self.0
                .conns
                .cx_for(ws, connection_id, Some(adapters::WRITE_CAP))?,
        )
    }

    fn gh_conn(&self, cx: steve_modules::accounts::connections::ConnCx) -> VaultResult<GhConn> {
        if cx.adapter() != adapters::github_app::ID && cx.adapter() != adapters::github_pat::ID {
            return Err(steve_modules::ApiError::not_found("连接").into());
        }
        let base = api::api_base_or(&cx, &self.0.env.api_base).to_string();
        Ok(GhConn::on(cx, self.0.etags.clone(), &base))
    }

    /// Single-use manifest states in progress (for tests and diagnostics).
    pub fn pending_flows(&self) -> usize {
        self.0.flows.len()
    }
}

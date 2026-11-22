//! The public extension entry: an [`Assembly`] of [`Extension`]s, each handing the Hub its
//! connection and channel-side adapters, routes and background jobs. The `steve-hub` binary runs
//! [`Assembly::builtin`]; another binary can depend on these crates and run its own assembly:
//!
//! ```no_run
//! # async fn f() -> anyhow::Result<()> {
//! let asm = steve_hub::Assembly::builtin().without("feishu.self")?;
//! steve_hub::run(asm).await
//! # }
//! ```
//!
//! Extensions are linked at compile time. Every conflict (an extension or adapter id twice, an
//! [`Extension::api`] other than [`EXTENSION_API`], overlapping routes, a connection hook for an
//! adapter the extension did not register) stops the Hub at startup with the extension's id. These are messages for whoever assembles the Hub, in the language of
//! the Hub's other startup errors.

mod builtin;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use serde_json::Value;
use steve_channels::channel::ActAsUser;
use steve_channels::ext::{ChannelDecl, ChannelSide, ConnectionHook, MediaFetch, Queries};
use steve_channels::{ChannelAdapter, MessageSink, ProfileAdapter};
use steve_modules::Modules;
use steve_modules::accounts::connections::{ConnectionProvider, Connections};
use steve_modules::contacts::PartyHooks;

use crate::bus::Bus;
use crate::config::Config;
pub use crate::wire::Vars;
pub use crate::wire::modules::Job;

/// The extension API this Hub is built with; raised only on an incompatible change.
pub const EXTENSION_API: u32 = 2;

/// Why an assembly was refused, with the extension it is about once the Hub knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionError {
    extension: Option<String>,
    msg: String,
}

impl ExtensionError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            extension: None,
            msg: msg.into(),
        }
    }

    /// The extension it is about.
    pub fn extension(&self) -> Option<&str> {
        self.extension.as_deref()
    }

    pub fn message(&self) -> &str {
        &self.msg
    }

    fn of(mut self, ext: &str) -> Self {
        self.extension.get_or_insert_with(|| ext.to_string());
        self
    }
}

impl std::fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.extension {
            Some(e) => write!(f, "extension {e}: {}", self.msg),
            None => f.write_str(&self.msg),
        }
    }
}

impl std::error::Error for ExtensionError {}

/// A set of adapters registered with the Hub.
pub trait Extension: Send + Sync + 'static {
    /// Stable and unique: `feishu.self`, `example.self`.
    fn id(&self) -> &str;
    /// The [`EXTENSION_API`] it was compiled against; the Hub refuses to start on a mismatch.
    fn api(&self) -> u32 {
        EXTENSION_API
    }
    /// Step one, before `Modules`, `Connections` and `Channels` are built: hand over the adapters.
    /// No network access.
    fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError>;
    /// Step two, once `Channels` is built: routes and background jobs.
    fn start(&self, _s: &Started) -> Result<Mounted, ExtensionError> {
        Ok(Mounted::default())
    }
}

struct Entry {
    ext: Arc<dyn Extension>,
    /// Built in: its routes keep their paths. Any other extension's are under
    /// `/channels/ext/{id}`.
    builtin: bool,
}

/// The ordered extensions a Hub runs with.
pub struct Assembly {
    list: Vec<Entry>,
}

impl Assembly {
    /// What the `steve-hub` binary registers: `feishu.bot`, `feishu.self`, `ext_ws`, `mail.imap`,
    /// `github`, `calendar`.
    pub fn builtin() -> Self {
        let list: Vec<Arc<dyn Extension>> = vec![
            Arc::new(builtin::FeishuBot),
            Arc::new(builtin::FeishuSelf::default()),
            Arc::new(builtin::ExternalChannel::default()),
            Arc::new(builtin::MailImap),
            Arc::new(builtin::GitHub),
            Arc::new(builtin::Calendars),
        ];
        Self {
            list: list
                .into_iter()
                .map(|ext| Entry { ext, builtin: true })
                .collect(),
        }
    }

    pub fn empty() -> Self {
        Self { list: Vec::new() }
    }

    pub fn with(mut self, e: impl Extension) -> Self {
        self.list.push(Entry {
            ext: Arc::new(e),
            builtin: false,
        });
        self
    }

    /// Without the extension `id`; an id not in the assembly is an error.
    pub fn without(mut self, id: &str) -> Result<Self, ExtensionError> {
        let before = self.list.len();
        self.list.retain(|e| e.ext.id() != id);
        if self.list.len() == before {
            return Err(ExtensionError::new("not in the assembly").of(id));
        }
        Ok(self)
    }

    pub fn ids(&self) -> Vec<&str> {
        self.list.iter().map(|e| e.ext.id()).collect()
    }

    /// Unique ids and the API version, before anything is built.
    pub(crate) fn check(&self) -> Result<(), ExtensionError> {
        let mut seen = BTreeSet::new();
        for e in &self.list {
            let id = e.ext.id();
            if id.is_empty() {
                return Err(ExtensionError::new("an extension without an id"));
            }
            // it is part of the path of its routes
            if !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
            {
                return Err(ExtensionError::new(
                    "an id is lowercase letters, digits, '.', '_' and '-'",
                )
                .of(id));
            }
            if !seen.insert(id) {
                return Err(ExtensionError::new("registered twice").of(id));
            }
            if e.ext.api() != EXTENSION_API {
                return Err(ExtensionError::new(format!(
                    "built for extension API {}, this Hub has {EXTENSION_API}",
                    e.ext.api()
                ))
                .of(id));
            }
        }
        Ok(())
    }

    /// Step one for every extension, in order.
    pub(crate) fn declare<'a>(
        &self,
        vars: &'a Vars,
        party_hooks: Vec<Arc<dyn PartyHooks>>,
    ) -> Result<Declare<'a>, ExtensionError> {
        let mut d = Declare {
            vars,
            party_hooks,
            ext: String::new(),
            owners: BTreeMap::new(),
            decls: BTreeMap::new(),
            providers: Vec::new(),
            channel_adapters: Vec::new(),
            profiles: Vec::new(),
            act_as_user: Vec::new(),
            media: Vec::new(),
            history: Vec::new(),
            connectors: Vec::new(),
            mail: Vec::new(),
            calendars: Vec::new(),
            event_sources: Vec::new(),
            systems: Vec::new(),
        };
        for e in &self.list {
            let id = e.ext.id();
            d.ext = id.to_string();
            e.ext.declare(&mut d).map_err(|err| err.of(id))?;
        }
        d.check_pairings()?;
        d.check_systems()?;
        Ok(d)
    }

    /// Step two for every extension: their routes merged into `routes` (an overlap names the
    /// extension), their jobs and their connection hooks (each for an adapter the extension
    /// registered, at most one per adapter).
    pub(crate) fn start(
        &self,
        s: &Started,
        d: &Declare<'_>,
        mut routes: Router<Modules>,
    ) -> Result<StartedAll, ExtensionError> {
        let mut jobs = Vec::new();
        let mut hooks: Vec<Lifecycle> = Vec::new();
        for e in &self.list {
            let id = e.ext.id();
            let m = e.ext.start(s).map_err(|err| err.of(id))?;
            jobs.extend(m.jobs);
            for (adapter, hook) in m.lifecycle {
                if d.owners.get(&adapter).map(String::as_str) != Some(id) {
                    return Err(ExtensionError::new(format!(
                        "a connection hook for adapter {adapter}, which it did not register"
                    ))
                    .of(id));
                }
                if hooks.iter().any(|h| h.adapter == adapter) {
                    return Err(ExtensionError::new(format!(
                        "a second connection hook for adapter {adapter}"
                    ))
                    .of(id));
                }
                hooks.push(Lifecycle {
                    decl: d.decls.get(&adapter).cloned(),
                    adapter,
                    hook,
                });
            }
            let mine = if e.builtin {
                m.api_routes
            } else if m.api_routes.has_routes() {
                Router::new().nest(&format!("/channels/ext/{id}"), m.api_routes)
            } else {
                continue;
            };
            routes = merge_routes(routes, mine).map_err(|e| e.of(id))?;
        }
        Ok(StartedAll {
            routes,
            jobs,
            hooks,
        })
    }
}

/// What step two collected from every extension.
pub(crate) struct StartedAll {
    pub(crate) routes: Router<Modules>,
    pub(crate) jobs: Vec<Job>,
    pub(crate) hooks: Vec<Lifecycle>,
}

/// One connection hook, with the declaration of its adapter when it is a channel side.
pub(crate) struct Lifecycle {
    pub(crate) adapter: String,
    pub(crate) decl: Option<ChannelDecl>,
    pub(crate) hook: Arc<dyn ConnectionHook>,
}

/// `routes` with `mine` merged in. axum refuses an overlapping route by panicking; this turns it
/// into an error.
fn merge_routes(
    routes: Router<Modules>,
    mine: Router<Modules>,
) -> Result<Router<Modules>, ExtensionError> {
    std::panic::catch_unwind(AssertUnwindSafe(move || routes.merge(mine))).map_err(|p| {
        let why = p
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| p.downcast_ref::<&str>().copied())
            .unwrap_or("overlapping routes");
        ExtensionError::new(format!("routes: {why}"))
    })
}

/// What step one collects. Each adapter id may be registered once across the whole assembly.
pub struct Declare<'a> {
    /// Environment variables (a map in tests).
    pub vars: &'a Vars,
    /// The hooks to run when contacts are merged.
    pub party_hooks: Vec<Arc<dyn PartyHooks>>,
    ext: String,
    /// Adapter id → the extension that registered it.
    owners: BTreeMap<String, String>,
    /// Adapter id → its declaration, for the channel sides.
    decls: BTreeMap<String, ChannelDecl>,
    pub(crate) providers: Vec<Arc<dyn ConnectionProvider>>,
    pub(crate) channel_adapters: Vec<Arc<dyn ChannelAdapter>>,
    pub(crate) profiles: Vec<Arc<dyn ProfileAdapter>>,
    pub(crate) act_as_user: Vec<Arc<dyn ActAsUser>>,
    pub(crate) media: Vec<Arc<dyn MediaFetch>>,
    pub(crate) history: Vec<Arc<dyn steve_channels::ext::HistoryRead>>,
    pub(crate) connectors: Vec<Arc<dyn steve_channels::ext::ConnectorTools>>,
    pub(crate) mail: Vec<Arc<dyn steve_mail::MailAdapter>>,
    pub(crate) calendars: Vec<Arc<dyn steve_calendar::CalendarSource>>,
    pub(crate) event_sources: Vec<Arc<dyn steve_flow::EventAdapter>>,
    /// Handle systems declared by the extensions: `(id, label, extension)`.
    pub(crate) systems: Vec<(String, String, String)>,
}

impl Declare<'_> {
    fn claim(&mut self, adapter: &str) -> Result<(), ExtensionError> {
        if adapter.is_empty() {
            return Err(ExtensionError::new("an adapter without an id"));
        }
        if let Some(owner) = self.owners.get(adapter) {
            return Err(ExtensionError::new(format!(
                "adapter {adapter} is already registered by {owner}"
            )));
        }
        self.owners.insert(adapter.to_string(), self.ext.clone());
        Ok(())
    }

    /// A channel-side adapter: its provider goes to `Connections`, its other views to
    /// `Channels` (channel adapter, profiles, media) and the outbox (acting as you).
    pub fn channel(&mut self, side: ChannelSide) -> Result<(), ExtensionError> {
        side.check().map_err(ExtensionError::new)?;
        let id = side.decl.adapter.clone();
        self.claim(&id)?;
        self.decls.insert(id.clone(), side.decl);
        self.providers.push(side.provider);
        self.channel_adapters.extend(side.adapter);
        self.profiles.extend(side.profiles);
        self.act_as_user.extend(side.act_as_user);
        self.media.extend(side.media);
        self.history.extend(side.history);
        self.connectors.extend(side.connector);
        Ok(())
    }

    /// The declarations of the channel sides registered so far.
    pub(crate) fn decls(&self) -> impl Iterator<Item = &ChannelDecl> {
        self.decls.values()
    }

    /// A provider whose connections need a pair of an adapter nobody registered would never run.
    fn check_pairings(&self) -> Result<(), ExtensionError> {
        for p in &self.providers {
            let Some(pairing) = p.pairing() else { continue };
            if pairing.required && !pairing.with.iter().any(|a| self.owners.contains_key(a)) {
                let owner = self.owners.get(p.id()).cloned().unwrap_or_default();
                return Err(ExtensionError::new(format!(
                    "adapter {} needs one of {} in the assembly",
                    p.id(),
                    pairing.with.join(", ")
                ))
                .of(&owner));
            }
        }
        Ok(())
    }

    /// A handle system that is not one of the Hub's own (`systems.id`, for example an adapter's
    /// internal user ids), named `label`: a [`steve_channels::channel::Sender`]'s handle or aliases
    /// may use it, and [`ChannelDecl::handle_system`] may name it. Upserted into `systems` at every
    /// boot, owned by this extension. The id is lowercase letters, digits and `_`; one the Hub
    /// seeds, or one another extension declared, is an error.
    pub fn system(&mut self, id: &str, label: &str) -> Result<(), ExtensionError> {
        let id = id.trim();
        let label = label.trim();
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(ExtensionError::new(format!(
                "handle system {id:?}: an id is lowercase letters, digits and '_'"
            )));
        }
        if label.is_empty() {
            return Err(ExtensionError::new(format!(
                "handle system {id} without a label"
            )));
        }
        if steve_modules::accounts::repo::BUILTIN_SYSTEMS
            .iter()
            .any(|b| b.0 == id)
        {
            return Err(ExtensionError::new(format!(
                "handle system {id} is one of the Hub's own"
            )));
        }
        if let Some((_, _, owner)) = self.systems.iter().find(|(x, _, _)| x == id) {
            return Err(ExtensionError::new(format!(
                "handle system {id} is already declared by {owner}"
            )));
        }
        self.systems
            .push((id.to_string(), label.to_string(), self.ext.clone()));
        Ok(())
    }

    /// Every channel side names its people in a system the Hub seeds or an extension declared
    /// (in any order).
    fn check_systems(&self) -> Result<(), ExtensionError> {
        for d in self.decls.values() {
            let s = d.handle_system.as_str();
            let known = steve_modules::accounts::repo::BUILTIN_SYSTEMS
                .iter()
                .any(|b| b.0 == s)
                || self.systems.iter().any(|(x, _, _)| x == s);
            if !known {
                let owner = self.owners.get(&d.adapter).cloned().unwrap_or_default();
                return Err(ExtensionError::new(format!(
                    "adapter {} names its people in handle system {s}, which nobody declares",
                    d.adapter
                ))
                .of(&owner));
            }
        }
        Ok(())
    }

    /// An adapter with a connection and no channel side.
    pub fn provider(&mut self, p: Arc<dyn ConnectionProvider>) -> Result<(), ExtensionError> {
        self.claim(p.id())?;
        self.providers.push(p);
        Ok(())
    }

    /// A mailbox adapter: a provider for `Connections` and an adapter for the mail module.
    pub fn mail(&mut self, a: Arc<dyn steve_mail::MailAdapter>) -> Result<(), ExtensionError> {
        self.claim(a.id())?;
        self.providers.push(a.clone());
        self.mail.push(a);
        Ok(())
    }

    /// A calendar source with its own connections (a provider for `Connections`, a source for the
    /// calendar service). A connector plugin's calendars need nothing here: an ext_ws connection
    /// that declares `calendar.v1` is a source already.
    pub fn calendar<T>(&mut self, a: Arc<T>) -> Result<(), ExtensionError>
    where
        T: ConnectionProvider + steve_calendar::CalendarSource + 'static,
    {
        self.claim(ConnectionProvider::id(a.as_ref()))?;
        self.providers.push(a.clone());
        self.calendars.push(a);
        Ok(())
    }

    /// An event source for flows, for an adapter already registered by this or an earlier
    /// extension.
    pub fn event_source(
        &mut self,
        a: Arc<dyn steve_flow::EventAdapter>,
    ) -> Result<(), ExtensionError> {
        let id = a.id();
        if !self.owners.contains_key(id) {
            return Err(ExtensionError::new(format!(
                "event source {id} without a registered adapter"
            )));
        }
        if self.event_sources.iter().any(|e| e.id() == id) {
            return Err(ExtensionError::new(format!(
                "event source {id} is registered twice"
            )));
        }
        self.event_sources.push(a);
        Ok(())
    }
}

/// Subscribing to the Hub's bus; publishing is not offered.
#[derive(Clone)]
pub struct BusTap(Bus);

/// Plugin boundary view over the original bounded typed tap. Encoding is lazy, so it adds
/// no queue, reorder point or overflow policy of its own.
pub struct PluginEvents {
    inner: tokio::sync::mpsc::Receiver<steve_types::Notification>,
}
impl PluginEvents {
    pub async fn recv(&mut self) -> Option<Value> {
        self.inner.recv().await.map(|event| {
            serde_json::from_slice(&crate::api::stream::encode_event(&event))
                .expect("plugin event frame")
        })
    }
    pub fn try_recv(&mut self) -> Result<Value, tokio::sync::mpsc::error::TryRecvError> {
        self.inner.try_recv().map(|event| {
            serde_json::from_slice(&crate::api::stream::encode_event(&event))
                .expect("plugin event frame")
        })
    }
}
impl BusTap {
    /// The plugin's declared wire tags, encoded only when the plugin reads each frame.
    pub fn tap(&self, types: &[&str]) -> PluginEvents {
        let types: Vec<String> = types.iter().map(|ty| (*ty).to_owned()).collect();
        let inner = self
            .0
            .tap(move |event| types.iter().any(|ty| ty == event.wire_type()));
        PluginEvents { inner }
    }
}

/// What step two gets: the built services. `Channels` itself and the store's write transactions
/// are not part of it; inbound messages go through `inbound` (or a connection hook's own sink).
#[non_exhaustive]
pub struct Started {
    pub modules: Modules,
    pub connections: Connections,
    /// The channels' inbound entry point.
    pub inbound: Arc<dyn MessageSink>,
    /// Read views, bound to one connection with [`Queries::for_cx`].
    pub queries: Queries,
    pub bus: BusTap,
    /// Environment variables (a map in tests).
    pub vars: Vars,
    pub(crate) channels: steve_channels::Channels,
}

impl Started {
    pub(crate) fn new(channels: &steve_channels::Channels, bus: &Bus, vars: &Vars) -> Self {
        Self {
            modules: channels.modules().clone(),
            connections: channels.connections().clone(),
            inbound: Arc::new(channels.clone()),
            queries: Queries::new(channels.clone()),
            bus: BusTap(bus.clone()),
            vars: vars.clone(),
            channels: channels.clone(),
        }
    }
}

/// What step two hands back. Build it from [`Mounted::default`] with the builders.
#[derive(Default)]
#[non_exhaustive]
pub struct Mounted {
    /// Merged under `/api` behind `require_user` and the `Caller` layer (an extension that is not
    /// built in: under `/api/channels/ext/{id}`).
    pub api_routes: Router<Modules>,
    pub jobs: Vec<Job>,
    /// What runs for each connection of an adapter (`connections.adapter`, hook).
    pub lifecycle: Vec<(String, Arc<dyn ConnectionHook>)>,
}

impl Mounted {
    /// Routes merged with the ones given before.
    pub fn routes(mut self, r: Router<Modules>) -> Self {
        self.api_routes = self.api_routes.merge(r);
        self
    }

    pub fn job(mut self, j: Job) -> Self {
        self.jobs.push(j);
        self
    }

    /// The hook run for each connection of `adapter`, which this extension must have registered;
    /// one per adapter. The Hub starts, restarts and stops its runs as the connections change.
    pub fn lifecycle(mut self, adapter: &str, h: Arc<dyn ConnectionHook>) -> Self {
        self.lifecycle.push((adapter.to_string(), h));
        self
    }
}

/// Runs the Hub with `asm`: the whole of the `steve-hub` binary.
pub async fn run(asm: Assembly) -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--build-info") {
        println!(
            "{}",
            serde_json::json!({"version":option_env!("STEVE_BUILD_VERSION"),"commit":option_env!("STEVE_BUILD_COMMIT"),"target":env!("STEVE_BUILD_TARGET"),"schema":steve_store::db::SCHEMA_VERSION,"check_config":true,
                "upgrade_runner":{"journal":1,"kinds":["release_bundle"]}})
        );
        return Ok(());
    }
    // the startup checks on this environment, without the database or the Hub's port (check.rs)
    if std::env::args().any(|arg| arg == "--check-config") {
        /* its warnings in the language of whoever runs it (`STEVE_LANG`, which `steve setup` passes on), else en */
        let lang = std::env::var("STEVE_LANG")
            .ok()
            .as_deref()
            .and_then(steve_i18n::match_tag)
            .unwrap_or(steve_i18n::EN);
        let report = steve_i18n::with(lang, || crate::check::check(|k| std::env::var(k).ok()));
        println!("{}", serde_json::to_string(&report)?);
        std::process::exit(if report.ok { 0 } else { 2 });
    }
    // the web client alone (A3): `--serve-web --listen <address:port>` serves STEVE_WEB_DIR and nothing else
    if std::env::args().nth(1).as_deref() == Some("--serve-web") {
        let args: Vec<String> = std::env::args().collect();
        let listen = args
            .iter()
            .position(|a| a == "--listen")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| "127.0.0.1:8441".into());
        let cfg = Arc::new(Config::from_env()?);
        let listener = tokio::net::TcpListener::bind(&listen).await?;
        println!("steve web (only) on http://{listen}");
        axum::serve(
            listener,
            crate::web::web_only(cfg).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
        return Ok(());
    }
    // tokens and clients on this machine (access.rs); needs the configuration, not the running Hub
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("--access" | "--admin")
    ) {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(crate::access::main(&args));
    }
    // the ACP adapters in this Hub's home (steve_acp::managed); `steve setup` and `steve adapters` run it
    if std::env::args().nth(1).as_deref() == Some("--adapters") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        let home = std::env::var_os("STEVE_HOME")
            .filter(|h| !h.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| crate::config::home_dir().join(".steve"));
        let node = std::env::var_os("STEVE_NODE").map(std::path::PathBuf::from);
        std::process::exit(steve_acp::managed::cli(&home, node.as_deref(), &args));
    }
    // the one-shot launchd job of a one-click local upgrade (upgrades/runner.rs); needs no Config
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--upgrade-runner") {
        let (Some(home), Some(id)) = (args.get(2), args.get(3)) else {
            anyhow::bail!("usage: steve-hub --upgrade-runner <home> <operation id>")
        };
        std::process::exit(crate::upgrades::runner::main(
            std::path::Path::new(home),
            id,
        ));
    }
    // Every other mode was dispatched above; what is left starts a Hub in STEVE_HOME (or ~/.steve).
    // An argument this binary does not know (a test filter, a typo) must not do that.
    match args.get(1..).unwrap_or_default() {
        [] => {}
        [one] if one == "--supervise" || one == "--local-node" => {}
        other => {
            eprintln!(
                "steve-hub: unknown arguments {other:?}; run it without arguments to start the Hub"
            );
            std::process::exit(2);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("STEVE_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    crate::wire::tune_sqlite();
    // the Hub's own Node in a process of its own (STEVE_NODE_MODE=service, B1): started by the
    // Hub, it outlives Hub restarts and never creates a missing Hub home.
    if args.get(1).is_some_and(|arg| arg == "--local-node") {
        let cfg = Config::from_vars(|k| std::env::var(k).ok())?;
        return crate::wire::local_service::run(&cfg).await;
    }
    let mut cfg = Config::from_env()?;
    if args.get(1).is_some_and(|arg| arg == "--supervise") {
        return crate::upgrades::supervise(cfg).await;
    }
    // listening sockets it was started with (socket activation), before it starts any process
    cfg.listen
        .adopt(crate::config::Handed::take()?)
        .context("the sockets the Hub was started with")?;
    let _instance_lock = crate::upgrades::instance_lock(&cfg)?;
    let ready = crate::admin::ReadyFile::new(&cfg.home)?;
    // Bind before wiring cookies, origins and the local Node.
    let listeners = cfg.listen.bind().await?;
    cfg.set_http_port(listeners[0].local_addr()?.port());
    let web_listeners = match cfg.web_port {
        Some(port) => cfg.listen.bind_port(port).await?,
        None => Vec::new(),
    };
    let cfg = Arc::new(cfg);
    // collection is on from here; exporting it is optional (`metrics::start`)
    crate::metrics::install();
    let supervised_state: Arc<std::sync::Mutex<Option<crate::api::AppState>>> =
        Arc::new(std::sync::Mutex::new(None));
    if std::env::var("STEVE_MANAGED").as_deref() == Ok("1") {
        let expected: i32 = std::env::var("STEVE_SUPERVISOR_PID")
            .context("managed child needs supervisor PID")?
            .parse()?;
        anyhow::ensure!(
            expected > 1 && unsafe { libc::getppid() } == expected,
            "managed child lost its supervisor before boot"
        );
        let state = supervised_state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if unsafe { libc::getppid() } != expected {
                    let state = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(state) = state {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(15),
                            state.shutdown(),
                        )
                        .await;
                    }
                    crate::upgrades::exit_orphaned_managed_hub();
                }
            }
        });
    }
    crate::upgrades::startup_gate(&cfg).await?;
    let caches = Arc::new(steve_runtime::cache::Caches::new(cfg.cache));
    let bus = Bus::new(caches);
    let w =
        crate::wire::wire_assembly(&cfg, &bus, Arc::new(|k: &str| std::env::var(k).ok()), &asm)?;
    let st = crate::api::AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_oidc(w.oidc.clone())
        .with_nodes(w.nodes)
        .with_node(w.node)
        .with_history(w.history)
        .with_workspaces(Some(w.workspaces));
    *supervised_state.lock().unwrap_or_else(|e| e.into_inner()) = Some(st.clone());
    st.runtimes.set_harnesses(w.harnesses);
    crate::app::boot(&st).await?;

    for job in w.jobs {
        job.start(&st.jobs);
    }
    Job::new(
        "upgrades.check",
        crate::upgrades::run_auto_check((*cfg).clone()),
    )
    .start(&st.jobs);
    Job::new(
        "signing.migrate",
        crate::upgrades::signing::migrate_background(cfg.home.clone()),
    )
    .start(&st.jobs);
    let app = crate::app::own_cookies(
        crate::app::app(st.clone(), w.auth, w.mcp, w.module_routes, w.root_routes),
        cfg.listen.port,
    );
    let metrics = crate::metrics::start(st.clone()).await;
    if cfg.listen.any() {
        tracing::warn!(
            "STEVE_HOST includes a wildcard address: the Hub is reachable from every network \
             this machine is on; list the addresses you need instead"
        );
    }
    // recovery trouble is reported (`/healthz`.recovery), never a reason not to serve
    if let Err(e) = st.domain.ready().await {
        tracing::error!("recovery after start: {e}");
        crate::recovery::failed(&e.to_string());
    }
    let addresses = |listeners: &[tokio::net::TcpListener]| {
        listeners
            .iter()
            .map(tokio::net::TcpListener::local_addr)
            .collect::<std::io::Result<Vec<_>>>()
    };
    let hub_addresses = addresses(&listeners)?;
    let web_addresses = addresses(&web_listeners)?;
    #[derive(serde::Serialize)]
    struct Ready {
        pid: u32,
        hub: Vec<SocketAddr>,
        web: Vec<SocketAddr>,
        local_node: Option<SocketAddr>,
        management: std::path::PathBuf,
        metrics: Option<String>,
    }

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let stop = async {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    };
    // a Node's link is one of these connections: little unsent data in the kernel lets a
    // permission answer overtake file chunks (steve_node_wire::sock)
    use axum::serve::ListenerExt as _;
    let mut servers = tokio::task::JoinSet::new();
    for listener in listeners {
        let app = app.clone();
        let listener = listener
            .tap_io(|tcp: &mut tokio::net::TcpStream| steve_node_wire::sock::low_water(tcp));
        servers.spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
    }
    // the web client on its own port (A3): its pages only; it calls the Hub's port like any client
    if let Some(port) = cfg.web_port {
        let web = crate::web::web_only(cfg.clone());
        for listener in web_listeners {
            let web = web.clone();
            servers.spawn(async move {
                axum::serve(
                    listener,
                    web.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
            });
        }
        println!("steve web on port {port}");
    }
    ready.publish(&Ready {
        pid: std::process::id(),
        hub: hub_addresses,
        web: web_addresses,
        local_node: st
            .nodes
            .as_ref()
            .and_then(|svc| svc.local_listener.get().copied()),
        management: crate::admin::socket_path(&cfg.home)?,
        metrics,
    })?;
    println!("steve hub {} on {}", cfg.version, cfg.listen.urls());

    tokio::select! {
        _ = stop => {
            drop(ready);
            crate::nodes::tailcat::shutdown();
            st.shutdown().await;
        }
        result = async {
            while let Some(result) = servers.join_next().await {
                result??;
            }
            Ok::<_, anyhow::Error>(())
        } => result?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plugin_boundary_encodes_one_bounded_tap_in_domain_live_order() {
        let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let mut events = BusTap(bus.clone()).tap(&["project", "steve.thinking"]);
        assert_eq!(events.inner.max_capacity(), Bus::TAP_CAPACITY);
        bus.publish_batch(vec![
            steve_types::ConfigEvent::Project {
                id: "p".into(),
                ws: "a".into(),
            }
            .into(),
            steve_types::WorkLiveEvent::SteveThinking {
                on: true,
                ws: "a".into(),
                phase: None,
            }
            .into(),
        ]);
        for tag in ["project", "steve.thinking"] {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event["type"], tag);
            assert_eq!(event["ws"], "a");
            assert_eq!(event["rev"], 1);
        }
    }

    #[tokio::test]
    async fn plugin_transcript_reads_match_sse_slimming_and_keep_internal_taps_full() {
        use steve_types::{RawJson, Seq, WorkLiveEvent};

        let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let mut plugin = BusTap(bus.clone()).tap(&["event"]);
        let mut typed = bus.tap(|event| event.wire_type() == "event");
        let mut sse = bus.subscribe();
        let long = "😀".repeat(2500);
        let diff = "line\n".repeat(5000);
        let data = serde_json::json!({
            "rawOutput": long,
            "_meta": {"claudeCode": {"toolName": "Read", "toolResponse": "private"}},
            "rawInput": {"command": "cat file", "timeout": 5},
            "content": [
                {"type": "image", "data": "base64", "mimeType": "image/png"},
                {"type": "diff", "oldText": diff, "newText": diff}
            ]
        });
        for seq in 1..=2 {
            bus.live(
                WorkLiveEvent::Event {
                    session_id: "s".into(),
                    seq: Seq(seq),
                    kind: "tool".into(),
                    data: RawJson::from_str(&data.to_string()).unwrap(),
                    ws: "a".into(),
                }
                .into(),
            );
            let delivery = tokio::time::timeout(std::time::Duration::from_secs(5), sse.recv())
                .await
                .unwrap()
                .unwrap();
            let frame = crate::api::stream::Frame::from(delivery.as_ref());
            let sse_json = &frame.bytes[6..frame.bytes.len() - 2];
            // Bus dispatch delivers typed taps before broadcasting, so try_recv needs no polling.
            let event = if seq == 1 {
                plugin.recv().await.unwrap()
            } else {
                plugin.try_recv().unwrap()
            };
            assert_eq!(serde_json::to_vec(&event).unwrap(), sse_json);
            assert_eq!(event["seq"], seq);
            assert_eq!(event["ws"], "a");
            assert_eq!(event["rev"], 0);
            assert!(frame.id.is_none());
            assert!(event["data"].get("_meta").is_none());
            assert_eq!(event["data"]["tool"], "Read");
            assert_eq!(
                event["data"]["rawInput"],
                serde_json::json!({"command": "cat file"})
            );
            assert_eq!(
                event["data"]["content"][0],
                serde_json::json!({"type": "image"})
            );
            assert_eq!(
                event["data"]["rawOutput"],
                format!("{}…", "😀".repeat(2000))
            );
            assert_eq!(event["data"]["content"][1]["oldText"], diff);
            assert_eq!(event["data"]["content"][1]["newText"], diff);
            assert_eq!(
                serde_json::to_value(&*typed.recv().await.unwrap()).unwrap()["data"],
                data
            );
        }
        assert!(matches!(
            plugin.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn overlapping_routes_are_an_error_not_a_panic() {
        let one = || Router::<Modules>::new().route("/a", axum::routing::get(|| async { "a" }));
        let merged = merge_routes(
            one(),
            Router::new().route("/b", axum::routing::get(|| async { "b" })),
        );
        assert!(merged.is_ok());
        let e = merge_routes(one(), one()).expect_err("overlap");
        assert!(e.message().starts_with("routes: "), "{e}");
        assert!(e.message().contains("/a"), "{e}");
    }
}

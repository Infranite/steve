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
use steve_channels::channel::{ActAsUser, InboundMessage, SpaceEvent};
use steve_channels::ext::{ChannelDecl, ChannelSide, ConnectionHook, MediaFetch, Queries};
use steve_channels::{ChannelAdapter, Inbound, MessageSink, ProfileAdapter, SpaceChange};
use steve_modules::accounts::connections::{ConnCx, ConnectionProvider, Connections};
use steve_modules::contacts::PartyHooks;
use steve_types::{Scope, Scoped};

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

    pub(crate) fn optional_modules(&self) -> steve_types::integrations::OptionalModules {
        steve_types::integrations::OptionalModules {
            feishu_registration: self
                .list
                .iter()
                .any(|e| e.builtin && e.ext.id() == builtin::FEISHU_BOT_ID),
        }
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
            capabilities: BTreeMap::new(),
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

    /// Step two for every extension: each extension receives its own scoped [`Started`] handle
    /// limited to its registered adapters, declared capabilities and authorized workspaces.
    /// Stateless routes are merged into `routes` (an overlap names the extension), alongside jobs
    /// and connection hooks (each for an adapter the extension registered, at most one per adapter).
    pub(crate) fn start(
        &self,
        channels: &steve_channels::Channels,
        bus: &Bus,
        vars: &Vars,
        d: &Declare<'_>,
        mut routes: Router,
    ) -> Result<StartedAll, ExtensionError> {
        let mut jobs = Vec::new();
        let mut hooks: Vec<Lifecycle> = Vec::new();
        for e in &self.list {
            let id = e.ext.id();
            let owned_adapters: BTreeSet<String> = d
                .owners
                .iter()
                .filter_map(|(adapter, owner)| (owner == id).then(|| adapter.clone()))
                .collect();
            let caps = d.capabilities.get(id).cloned().unwrap_or_default();
            let started =
                Started::for_extension(id, channels, bus, vars, owned_adapters, caps, e.builtin);
            let m = e.ext.start(&started).map_err(|err| err.of(id))?;
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
    pub(crate) routes: Router,
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
fn merge_routes(routes: Router, mine: Router) -> Result<Router, ExtensionError> {
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
    /// Extension id → granted event/read capabilities.
    capabilities: BTreeMap<String, BTreeSet<EventCapability>>,
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

    fn grant_capability(&mut self, cap: EventCapability) {
        self.capabilities
            .entry(self.ext.clone())
            .or_default()
            .insert(cap);
    }

    /// Declares explicit domain event/read capabilities for this extension.
    /// Workspace authorization is still enforced dynamically from active connections of adapters
    /// registered by this extension; vault events are never exposed.
    pub fn event_capabilities(&mut self, caps: impl IntoIterator<Item = EventCapability>) {
        self.capabilities
            .entry(self.ext.clone())
            .or_default()
            .extend(caps);
    }

    /// A channel-side adapter: its provider goes to `Connections`, its other views to
    /// `Channels` (channel adapter, profiles, media) and the outbox (acting as you).
    pub fn channel(&mut self, side: ChannelSide) -> Result<(), ExtensionError> {
        side.check().map_err(ExtensionError::new)?;
        let id = side.decl.adapter.clone();
        self.claim(&id)?;
        self.grant_capability(EventCapability::Channels);
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
        if steve_store::is_builtin_system(id) {
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
            let known =
                steve_store::is_builtin_system(s) || self.systems.iter().any(|(x, _, _)| x == s);
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
        self.grant_capability(EventCapability::Mail);
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
        self.grant_capability(EventCapability::Calendar);
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

/// Domain event and read capabilities granted to a compile-time extension.
/// Vault events (`"vault"`) are intentionally excluded and always forbidden at the plugin boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventCapability {
    Channels,
    Work,
    Config,
    People,
    Automation,
    Knowledge,
    Calendar,
    Mail,
    Github,
    Devices,
    Autonomy,
    Stats,
}

impl EventCapability {
    /// Maps a `steve_types::WIRE_TYPES` tag to its required capability.
    /// Returns `Err(EventTypeRefusal::VaultForbidden)` for `"vault"` and
    /// `Err(EventTypeRefusal::Unknown)` for any unrecognized or internal-only tag.
    fn for_wire_type(ty: &str) -> Result<Self, EventTypeRefusal> {
        match ty {
            "vault" => Err(EventTypeRefusal::VaultForbidden),
            "space" | "chan_message" | "channel" | "persona" | "connection" => Ok(Self::Channels),
            "item" | "decision" | "session" | "event" | "message" | "steve.thinking"
            | "steve.draft" | "steve.tool" | "workflow.run" => Ok(Self::Work),
            "bot" | "project" | "tag" | "workspace" | "settings" | "mm" | "changeset" => {
                Ok(Self::Config)
            }
            "party" | "account" => Ok(Self::People),
            "signal" | "flow" | "run" | "digest" => Ok(Self::Automation),
            "kb" => Ok(Self::Knowledge),
            "calendar" => Ok(Self::Calendar),
            "mail" => Ok(Self::Mail),
            "gh" => Ok(Self::Github),
            "device" | "node" | "client" | "release.hold" => Ok(Self::Devices),
            "approval_policy" => Ok(Self::Autonomy),
            "stats" => Ok(Self::Stats),
            _ => Err(EventTypeRefusal::Unknown),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventTypeRefusal {
    VaultForbidden,
    Unknown,
}

/// Strongly typed refusal at the compile-time extension boundary ([`BusTap`], [`DomainViews`],
/// [`DomainCommands`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionBoundaryError {
    #[error("extension {extension}: vault events are forbidden across the extension boundary")]
    VaultEventForbidden { extension: String },
    #[error("extension {extension}: unknown or unexposed event type `{event_type}`")]
    UnknownEventType {
        extension: String,
        event_type: String,
    },
    #[error("extension {extension}: missing capability {capability:?} for event/view `{target}`")]
    CapabilityDenied {
        extension: String,
        capability: EventCapability,
        target: String,
    },
    #[error("extension {extension}: not authorized for workspace `{workspace}`")]
    UnauthorizedWorkspace {
        extension: String,
        workspace: String,
    },
    #[error("extension {extension}: does not own adapter `{adapter}`")]
    UnauthorizedAdapter { extension: String, adapter: String },
    #[error("extension {extension}: {resource} not found")]
    NotFound {
        extension: String,
        resource: &'static str,
    },
    #[error("extension {extension}: domain command failed: {detail}")]
    CommandFailed { extension: String, detail: String },
    #[error("extension {extension}: store read failed: {detail}")]
    StoreReadFailed { extension: String, detail: String },
}

fn record_extension_refusal(boundary: &'static str, reason: &'static str) {
    metrics::counter!(
        "steve_security_refusals_total",
        "boundary" => boundary,
        "reason" => reason
    )
    .increment(1);
}

#[derive(Clone)]
enum WorkspaceAuth {
    /// Production: authorized workspaces are exactly those with an active (non-revoked) connection
    /// for one of the adapters registered by this extension.
    ConnectedAdapters {
        store: steve_store::Store,
        adapters: Arc<Vec<String>>,
    },
    #[cfg(test)]
    Static(Arc<BTreeSet<String>>),
}

impl WorkspaceAuth {
    fn allows(&self, ws: &str) -> bool {
        if ws.is_empty() {
            return false;
        }
        match self {
            Self::ConnectedAdapters { store, adapters } => {
                if adapters.is_empty() {
                    return false;
                }
                let refs: Vec<&str> = adapters.iter().map(String::as_str).collect();
                store
                    .read(|r| {
                        steve_store::connections::Connections::new(r).has_active_adapter(ws, &refs)
                    })
                    .unwrap_or(false)
            }
            #[cfg(test)]
            Self::Static(allowed) => allowed.contains(ws),
        }
    }

    fn workspaces(&self) -> Result<BTreeSet<String>, steve_store::StoreError> {
        match self {
            Self::ConnectedAdapters { store, adapters } => {
                if adapters.is_empty() {
                    return Ok(BTreeSet::new());
                }
                let refs: Vec<&str> = adapters.iter().map(String::as_str).collect();
                let list = store.read(|r| {
                    steve_store::connections::Connections::new(r)
                        .active_workspaces_for_adapters(&refs)
                })?;
                Ok(list.into_iter().collect())
            }
            #[cfg(test)]
            Self::Static(allowed) => Ok((**allowed).clone()),
        }
    }
}

/// Read-only view of a connection owned by an extension's registered adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionView {
    pub id: String,
    pub workspace_id: String,
    pub account_id: String,
    pub adapter: String,
    pub status: String,
    pub caps: Vec<String>,
}

/// Read-only summary of a Bot in an authorized workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotView {
    pub id: String,
    pub name: String,
    pub role: String,
    pub enabled: bool,
}

/// Read-only domain view facade handed to extensions in [`Started`].
/// Never exposes a writable [`steve_store::Store`] or [`steve_modules::Modules`].
#[derive(Clone)]
pub struct DomainViews {
    extension_id: Arc<str>,
    store: steve_store::Store,
    adapters: Arc<BTreeSet<String>>,
    capabilities: Arc<BTreeSet<EventCapability>>,
    auth: WorkspaceAuth,
}

impl DomainViews {
    /// Workspaces where this extension currently has an active (non-revoked) adapter connection.
    pub fn authorized_workspaces(&self) -> Result<BTreeSet<String>, ExtensionBoundaryError> {
        self.auth
            .workspaces()
            .map_err(|e| ExtensionBoundaryError::StoreReadFailed {
                extension: self.extension_id.to_string(),
                detail: e.to_string(),
            })
    }

    /// Read-only list of this extension's active connections in `ws`.
    pub fn connections_in_workspace(
        &self,
        ws: &str,
    ) -> Result<Vec<ConnectionView>, ExtensionBoundaryError> {
        if !self.auth.allows(ws) {
            record_extension_refusal("extension_views", "workspace_denied");
            return Err(ExtensionBoundaryError::UnauthorizedWorkspace {
                extension: self.extension_id.to_string(),
                workspace: ws.to_owned(),
            });
        }
        let adapter_list: Vec<&str> = self.adapters.iter().map(String::as_str).collect();
        let rows = self
            .store
            .read(|r| {
                steve_store::connections::Connections::new(r)
                    .sources_in_workspace_for_adapters(ws, &adapter_list)
            })
            .map_err(|e| ExtensionBoundaryError::StoreReadFailed {
                extension: self.extension_id.to_string(),
                detail: e.to_string(),
            })?;
        Ok(rows
            .into_iter()
            .map(|c| ConnectionView {
                id: c.id,
                workspace_id: c.workspace_id,
                account_id: c.account_id,
                adapter: c.adapter,
                status: c.status,
                caps: c.caps,
            })
            .collect())
    }

    /// Read-only view of one connection in `ws`, only when `ws` is authorized for this extension
    /// and the connection's adapter belongs to this extension.
    pub fn connection(
        &self,
        ws: &str,
        connection_id: &str,
    ) -> Result<ConnectionView, ExtensionBoundaryError> {
        if !self.auth.allows(ws) {
            record_extension_refusal("extension_views", "workspace_denied");
            return Err(ExtensionBoundaryError::UnauthorizedWorkspace {
                extension: self.extension_id.to_string(),
                workspace: ws.to_owned(),
            });
        }
        let Some(row) = self
            .store
            .read(|r| steve_store::connections::Connections::new(r).source(ws, connection_id))
            .map_err(|e| ExtensionBoundaryError::StoreReadFailed {
                extension: self.extension_id.to_string(),
                detail: e.to_string(),
            })?
        else {
            return Err(ExtensionBoundaryError::NotFound {
                extension: self.extension_id.to_string(),
                resource: "connection",
            });
        };
        if !self.adapters.contains(&row.adapter) {
            record_extension_refusal("extension_views", "adapter_denied");
            return Err(ExtensionBoundaryError::UnauthorizedAdapter {
                extension: self.extension_id.to_string(),
                adapter: row.adapter,
            });
        }
        Ok(ConnectionView {
            id: row.id,
            workspace_id: row.workspace_id,
            account_id: row.account_id,
            adapter: row.adapter,
            status: row.status,
            caps: row.caps,
        })
    }

    /// Read-only list of Bots in `ws`. Requires [`EventCapability::Config`] and workspace
    /// authorization.
    pub fn workspace_bots(&self, ws: &str) -> Result<Vec<BotView>, ExtensionBoundaryError> {
        if !self.capabilities.contains(&EventCapability::Config) {
            record_extension_refusal("extension_views", "capability_denied");
            return Err(ExtensionBoundaryError::CapabilityDenied {
                extension: self.extension_id.to_string(),
                capability: EventCapability::Config,
                target: "workspace_bots".into(),
            });
        }
        if !self.auth.allows(ws) {
            record_extension_refusal("extension_views", "workspace_denied");
            return Err(ExtensionBoundaryError::UnauthorizedWorkspace {
                extension: self.extension_id.to_string(),
                workspace: ws.to_owned(),
            });
        }
        let bots = self.store.bots().list(Some(ws)).map_err(|e| {
            ExtensionBoundaryError::StoreReadFailed {
                extension: self.extension_id.to_string(),
                detail: e.to_string(),
            }
        })?;
        Ok(bots
            .into_iter()
            .map(|b| BotView {
                id: b.id,
                name: b.name,
                role: b.role,
                enabled: b.enabled,
            })
            .collect())
    }
}

/// Authorized domain command facade handed to extensions in [`Started`].
/// Every mutation passes through core domain rules and verifies adapter ownership.
#[derive(Clone)]
pub struct DomainCommands {
    extension_id: Arc<str>,
    adapters: Arc<BTreeSet<String>>,
    inbound: Arc<dyn MessageSink>,
    queries: Queries,
}

impl DomainCommands {
    fn check_adapter(&self, cx: &ConnCx) -> Result<(), ExtensionBoundaryError> {
        if !self.adapters.contains(cx.adapter()) {
            record_extension_refusal("extension_commands", "adapter_denied");
            return Err(ExtensionBoundaryError::UnauthorizedAdapter {
                extension: self.extension_id.to_string(),
                adapter: cx.adapter().to_owned(),
            });
        }
        Ok(())
    }

    /// Delivers an inbound channel message for a connection owned by this extension.
    pub async fn ingest_message(
        &self,
        cx: &ConnCx,
        msg: InboundMessage,
    ) -> Result<Inbound, ExtensionBoundaryError> {
        self.check_adapter(cx)?;
        self.inbound
            .inbound(cx.ws(), cx.id(), msg)
            .await
            .map_err(|e| ExtensionBoundaryError::CommandFailed {
                extension: self.extension_id.to_string(),
                detail: e.to_string(),
            })
    }

    /// Delivers a channel space lifecycle event for a connection owned by this extension.
    pub async fn ingest_space_event(
        &self,
        cx: &ConnCx,
        ev: SpaceEvent,
    ) -> Result<Option<SpaceChange>, ExtensionBoundaryError> {
        self.check_adapter(cx)?;
        self.inbound
            .space_event(cx.ws(), cx.id(), ev)
            .await
            .map_err(|e| ExtensionBoundaryError::CommandFailed {
                extension: self.extension_id.to_string(),
                detail: e.to_string(),
            })
    }

    /// Persists an adapter cursor scoped to a connection owned by this extension.
    pub fn save_cursor(
        &self,
        cx: &ConnCx,
        container: &str,
        cursor: &str,
    ) -> Result<(), ExtensionBoundaryError> {
        self.check_adapter(cx)?;
        self.queries
            .for_cx(cx)
            .save_cursor(container, cursor)
            .map_err(|e| ExtensionBoundaryError::CommandFailed {
                extension: self.extension_id.to_string(),
                detail: e.to_string(),
            })
    }
}

fn allow_tap_event(
    allowed_types: &[String],
    auth: &WorkspaceAuth,
    allow_hub_scope: bool,
    event: &steve_types::BusEvent,
) -> bool {
    if !allowed_types.iter().any(|ty| ty == event.wire_type()) {
        return false;
    }
    match event.scope() {
        Scope::Ws(ws) => {
            if auth.allows(ws.as_str()) {
                true
            } else {
                record_extension_refusal("extension_bus", "workspace_denied");
                false
            }
        }
        Scope::Hub => {
            if allow_hub_scope {
                true
            } else {
                record_extension_refusal("extension_bus", "hub_scope_denied");
                false
            }
        }
    }
}

/// Subscribing to the Hub's bus, filtered by the extension's granted capabilities and authorized
/// workspaces; publishing is never offered.
#[derive(Clone)]
pub struct BusTap {
    bus: Bus,
    extension_id: Arc<str>,
    capabilities: Arc<BTreeSet<EventCapability>>,
    auth: WorkspaceAuth,
    allow_hub_scope: bool,
}

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
    #[cfg(test)]
    fn scoped_for_test(
        bus: Bus,
        extension_id: &str,
        workspaces: impl IntoIterator<Item = &'static str>,
        capabilities: impl IntoIterator<Item = EventCapability>,
        allow_hub_scope: bool,
    ) -> Self {
        Self {
            bus,
            extension_id: Arc::from(extension_id),
            capabilities: Arc::new(capabilities.into_iter().collect()),
            auth: WorkspaceAuth::Static(Arc::new(
                workspaces.into_iter().map(str::to_owned).collect(),
            )),
            allow_hub_scope,
        }
    }

    /// Subscribes to `types` on the Hub bus.
    /// Refuses vault events, unknown tags, and any tag whose [`EventCapability`] was not granted
    /// to this extension. At delivery time, workspace-scoped events are delivered only for
    /// workspaces authorized for this extension, and Hub-scoped events require explicit Hub scope.
    pub fn tap(&self, types: &[&str]) -> Result<PluginEvents, ExtensionBoundaryError> {
        let mut allowed_types = Vec::with_capacity(types.len());
        for &ty in types {
            let cap = match EventCapability::for_wire_type(ty) {
                Ok(cap) => cap,
                Err(EventTypeRefusal::VaultForbidden) => {
                    record_extension_refusal("extension_bus", "vault_forbidden");
                    return Err(ExtensionBoundaryError::VaultEventForbidden {
                        extension: self.extension_id.to_string(),
                    });
                }
                Err(EventTypeRefusal::Unknown) => {
                    record_extension_refusal("extension_bus", "unknown_event_type");
                    return Err(ExtensionBoundaryError::UnknownEventType {
                        extension: self.extension_id.to_string(),
                        event_type: ty.to_owned(),
                    });
                }
            };
            if !self.capabilities.contains(&cap) {
                record_extension_refusal("extension_bus", "capability_denied");
                return Err(ExtensionBoundaryError::CapabilityDenied {
                    extension: self.extension_id.to_string(),
                    capability: cap,
                    target: ty.to_owned(),
                });
            }
            allowed_types.push(ty.to_owned());
        }
        let auth = self.auth.clone();
        let allow_hub_scope =
            self.allow_hub_scope && self.capabilities.contains(&EventCapability::Devices);
        let inner = self
            .bus
            .tap(move |event| allow_tap_event(&allowed_types, &auth, allow_hub_scope, event));
        Ok(PluginEvents { inner })
    }
}

/// What step two gets: read-only domain views, authorized domain commands, connection-scoped
/// queries, and a capability/workspace-filtered bus tap. `Channels`, `Modules`, `Connections` and
/// writable `Store` handles are never exposed across the public extension boundary.
#[non_exhaustive]
pub struct Started {
    /// Read-only domain views scoped to this extension's adapters and authorized workspaces.
    pub views: DomainViews,
    /// Authorized domain commands scoped to this extension's adapters.
    pub commands: DomainCommands,
    /// Connection-scoped read views, bound to one connection with [`Queries::for_cx`].
    pub queries: Queries,
    /// Capability- and workspace-filtered bus subscription handle.
    pub bus: BusTap,
    /// Environment variables (a map in tests).
    pub vars: Vars,
    pub(crate) connections: Connections,
    pub(crate) channels: steve_channels::Channels,
}

impl Started {
    pub(crate) fn for_extension(
        extension_id: &str,
        channels: &steve_channels::Channels,
        bus: &Bus,
        vars: &Vars,
        owned_adapters: BTreeSet<String>,
        capabilities: BTreeSet<EventCapability>,
        allow_hub_scope: bool,
    ) -> Self {
        let ext: Arc<str> = Arc::from(extension_id);
        let adapters = Arc::new(owned_adapters);
        let adapter_vec = Arc::new(adapters.iter().cloned().collect::<Vec<_>>());
        let caps = Arc::new(capabilities);
        let store = channels.store().clone();
        let auth = WorkspaceAuth::ConnectedAdapters {
            store: store.clone(),
            adapters: adapter_vec,
        };
        let queries = Queries::new(channels.clone());
        let inbound: Arc<dyn MessageSink> = Arc::new(channels.clone());
        Self {
            views: DomainViews {
                extension_id: ext.clone(),
                store,
                adapters: adapters.clone(),
                capabilities: caps.clone(),
                auth: auth.clone(),
            },
            commands: DomainCommands {
                extension_id: ext.clone(),
                adapters,
                inbound,
                queries: queries.clone(),
            },
            queries,
            bus: BusTap {
                bus: bus.clone(),
                extension_id: ext,
                capabilities: caps,
                auth,
                allow_hub_scope,
            },
            vars: vars.clone(),
            connections: channels.connections().clone(),
            channels: channels.clone(),
        }
    }
}

/// What step two hands back. Build it from [`Mounted::default`] with the builders.
#[derive(Default)]
#[non_exhaustive]
pub struct Mounted {
    /// Stateless routes merged under `/api` behind `require_user` and the `Caller` layer (an
    /// extension that is not built in: under `/api/channels/ext/{id}`).
    pub api_routes: Router,
    pub jobs: Vec<Job>,
    /// What runs for each connection of an adapter (`connections.adapter`, hook).
    pub lifecycle: Vec<(String, Arc<dyn ConnectionHook>)>,
}

impl Mounted {
    /// Routes merged with the ones given before.
    pub fn routes(mut self, r: Router) -> Self {
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

    steve_store::db::tune_sqlite();
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
    crate::metrics::install_with_backup(cfg.home.clone(), cfg.db_path());
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
    crate::upgrades::startup_gate(&cfg, &listeners).await?;
    // After commit (post-startup_gate), migrate any legacy Linux systemd unit that still points
    // to ops/run-managed.sh to direct `steve-hub --supervise`.
    #[cfg(target_os = "linux")]
    if std::env::var("STEVE_MANAGED").as_deref() == Ok("1")
        && let Ok(current) = std::fs::canonicalize(cfg.home.join("current"))
        && let Err(e) = steve_install::service::sync_hub_systemd_unit(&cfg.home, &current)
    {
        tracing::warn!("sync systemd hub unit after upgrade: {e:#}");
    }
    let caches = Arc::new(steve_runtime::cache::Caches::new(cfg.cache));
    let bus = Bus::new(caches);
    let w =
        crate::wire::wire_assembly(&cfg, &bus, Arc::new(|k: &str| std::env::var(k).ok()), &asm)?;
    let st = crate::api::AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_oidc(w.oidc.clone())
        .with_nodes(w.nodes)
        .with_node(w.node)
        .with_modules(w.modules)
        .with_optional_modules(asm.optional_modules())
        .with_history(w.history)
        .with_workspaces(Some(w.workspaces));
    *supervised_state.lock().unwrap_or_else(|e| e.into_inner()) = Some(st.clone());
    st.runtimes.set_harnesses(w.harnesses);
    crate::app::boot(&st).await?;

    for job in w.jobs {
        job.start(&st.jobs);
    }
    let upgrade_cfg = (*cfg).clone();
    Job::task("upgrades.check", move || {
        crate::upgrades::auto_check_task(upgrade_cfg)
    })
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
        let mut events = BusTap::scoped_for_test(
            bus.clone(),
            "test.ext",
            ["a"],
            [EventCapability::Config, EventCapability::Work],
            false,
        )
        .tap(&["project", "steve.thinking"])
        .unwrap();
        assert_eq!(events.inner.max_capacity(), Bus::TAP_CAPACITY);
        bus.publish_batch(vec![
            steve_types::ConfigEvent::Project {
                id: "p_other".into(),
                ws: "b".into(),
            }
            .into(),
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
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn plugin_transcript_reads_match_sse_slimming_and_keep_internal_taps_full() {
        use steve_types::{RawJson, Seq, WorkLiveEvent};

        let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let mut plugin = BusTap::scoped_for_test(
            bus.clone(),
            "test.ext",
            ["a"],
            [EventCapability::Work],
            false,
        )
        .tap(&["event"])
        .unwrap();
        let mut typed = bus.tap(|event| event.wire_type() == "event");
        let mut sse = bus.subscribe();
        let long = "😀".repeat(2500);
        let diff = "line\n".repeat(5000);
        let data = serde_json::json!({
            "tool": {
                "id": "t1",
                "origin": "harness",
                "name": "Read",
                "category": "read",
                "title": "Read file",
                "phase": "succeeded",
                "action": {
                    "type": "tool",
                    "name": "steve_update",
                    "args": {
                        "now": long,
                        "image": {"type": "image", "data": "base64", "mimeType": "image/png"}
                    }
                },
                "result": [
                    {"type": "text", "text": long},
                    {"type": "diff", "path": "a.rs", "old": diff, "new": diff, "added": 5000, "removed": 5000}
                ]
            }
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
            assert_eq!(event["data"]["tool"]["name"], "Read");
            assert_eq!(
                event["data"]["tool"]["action"]["args"]["now"],
                format!("{}…", "😀".repeat(2000))
            );
            assert_eq!(
                event["data"]["tool"]["action"]["args"]["image"],
                serde_json::json!({"type": "image"})
            );
            assert_eq!(
                event["data"]["tool"]["result"][0]["text"],
                format!("{}…", "😀".repeat(2000))
            );
            assert_eq!(event["data"]["tool"]["result"][1]["old"], diff);
            assert_eq!(event["data"]["tool"]["result"][1]["new"], diff);
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
    fn registration_capability_follows_builtin_extension() {
        assert!(Assembly::builtin().optional_modules().feishu_registration);
        assert!(
            !Assembly::builtin()
                .without(builtin::FEISHU_BOT_ID)
                .unwrap()
                .optional_modules()
                .feishu_registration
        );
        assert!(!Assembly::empty().optional_modules().feishu_registration);
    }

    #[test]
    fn every_wire_type_maps_to_a_capability_or_explicit_vault_refusal() {
        for ty in steve_types::WIRE_TYPES {
            let mapped = EventCapability::for_wire_type(ty);
            if ty == "vault" {
                assert_eq!(mapped, Err(EventTypeRefusal::VaultForbidden));
            } else {
                assert!(mapped.is_ok(), "unmapped wire type: {ty}");
            }
        }
        assert_eq!(
            EventCapability::for_wire_type("patch"),
            Err(EventTypeRefusal::Unknown)
        );
        assert_eq!(
            EventCapability::for_wire_type("not_a_real_event"),
            Err(EventTypeRefusal::Unknown)
        );
    }

    #[tokio::test]
    async fn bus_tap_refusals_return_typed_errors_and_increment_metrics() {
        let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let tap = BusTap::scoped_for_test(
            bus.clone(),
            "ext.demo",
            ["ws_allowed"],
            [EventCapability::Config],
            false,
        );
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let mut sub = metrics::with_local_recorder(&recorder, || {
            assert_eq!(
                tap.tap(&["vault"]).err(),
                Some(ExtensionBoundaryError::VaultEventForbidden {
                    extension: "ext.demo".into()
                })
            );
            assert_eq!(
                tap.tap(&["bogus"]).err(),
                Some(ExtensionBoundaryError::UnknownEventType {
                    extension: "ext.demo".into(),
                    event_type: "bogus".into()
                })
            );
            assert_eq!(
                tap.tap(&["item"]).err(),
                Some(ExtensionBoundaryError::CapabilityDenied {
                    extension: "ext.demo".into(),
                    capability: EventCapability::Work,
                    target: "item".into()
                })
            );
            tap.tap(&["project"]).unwrap()
        });
        // Authorized workspace passes; unauthorized workspace is dropped and counted.
        metrics::with_local_recorder(&recorder, || {
            bus.publish_batch(vec![
                steve_types::ConfigEvent::Project {
                    id: "p_denied".into(),
                    ws: "ws_other".into(),
                }
                .into(),
                steve_types::ConfigEvent::Project {
                    id: "p_ok".into(),
                    ws: "ws_allowed".into(),
                }
                .into(),
            ]);
        });
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got["id"], "p_ok");
        assert_eq!(got["ws"], "ws_allowed");
        assert!(matches!(
            sub.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        // Verify workspace and Hub-scope refusal metrics synchronously through the predicate.
        metrics::with_local_recorder(&recorder, || {
            let denied_ws: steve_types::BusEvent = steve_types::ConfigEvent::Project {
                id: "p_denied".into(),
                ws: "ws_other".into(),
            }
            .into();
            let denied_hub: steve_types::BusEvent = steve_types::DevicesEvent::Node {
                id: "node_1".into(),
                user: "usr_dev".into(),
            }
            .into();
            assert!(!allow_tap_event(
                &["project".into()],
                &tap.auth,
                false,
                &denied_ws
            ));
            assert!(!allow_tap_event(
                &["node".into()],
                &tap.auth,
                false,
                &denied_hub
            ));
        });
        let text = handle.render();
        for reason in [
            "vault_forbidden",
            "unknown_event_type",
            "capability_denied",
            "workspace_denied",
            "hub_scope_denied",
        ] {
            assert!(
                text.lines()
                    .any(|line| line.starts_with("steve_security_refusals_total{")
                        && line.contains("boundary=\"extension_bus\"")
                        && line.contains(&format!("reason=\"{reason}\""))
                        && line.ends_with(" 1")),
                "missing {reason} in:\n{text}"
            );
        }
    }

    #[test]
    fn overlapping_routes_are_an_error_not_a_panic() {
        let one = || Router::new().route("/a", axum::routing::get(|| async { "a" }));
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

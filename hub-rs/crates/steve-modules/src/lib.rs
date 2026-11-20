//! The shared base every new Hub module builds on (slice B0):
//!
//! - [`ApiError`] and the extractors ([`Caller`], [`Body`], [`Check`]): the error-code conventions;
//! - [`Modules`]: the state module routers run on (store, model gateway, registries);
//! - [`changes`]: persisted conversational-config drafts as operation lists, apply/undo in one
//!   transaction, `409` when the target moved, 24-hour undo;
//! - [`decisions`]: dispatch of `POST /api/decisions/:id` to the module that opened the decision, and the
//!   `confirm` kind;
//! - [`own`]: the `own_<x>(ws, me, id)` check;
//! - [`sse`]: which subscribers get a module event;
//! - B1, the seams between modules and steve-core: [`gateway`] (model calls),
//!   [`hooks`] (item lifecycle), [`context`] (extra prompt context), [`panels`] (item sidebar panels),
//!   [`outbound`] (disclosure check), [`clock`]; decision handlers are in [`decisions`];
//! - [`zone`]: whose clock a local date or time is read on (a person's, else the workspace's);
//! - [`net`]: the `STEVE_FORBID_REAL_HOSTS=1` guard every outbound client calls before connecting.
//!
//! The SQL lives in `steve_store::base` (tables of migration 4). Integration (steve-hub / steve-core,
//! done by the integrator):
//! 1. build a [`Registry`], let every module register its targets, operations and decision handlers,
//!    then `Modules::new(store, gateway, registry)`;
//! 2. on `/api`, after `require_user`: insert `Caller { ws, me }` into the request extensions and
//!    `.merge(module_routes)`;
//! 3. `POST /api/config/draft`: kinds in `registry.target_kinds()` go to [`changes::draft`];
//!    `POST /api/config/:id/apply|undo` try [`changes::apply`] / [`changes::undo`] first and fall back to
//!    the in-memory drafts on `None`; `pickTarget` appends [`changes::candidates`];
//! 4. `POST /api/decisions/:id`: try [`decisions::dispatch`] first; on `None` run the old logic;
//! 5. `/api/stream`: filter typed envelopes using `Scoped::scope()`;
//! 6. `undoDecision`: call [`Registry::before_undo`] in its transaction first;
//! 7. the B1 call points: `registry.hooks()` in createItem / accept / finish / drop,
//!    `registry.contexts()` in `startSession` and `answer`, [`panels::panels`] / [`panels::action`]
//!    behind `GET /api/items/:id/panels` and `POST /api/items/:id/panels/:panel/actions/:action`.

steve_i18n::init!();

pub mod accounts;
pub mod caps;
pub mod changes;
pub mod clock;
pub mod contacts;
pub mod context;
pub mod decisions;
mod error;
mod extract;
pub mod gateway;
pub mod hooks;
pub mod net;
pub mod outbound;
pub mod panels;
pub mod shown;
pub mod testkit;
pub mod vault;
pub mod zone;

use std::collections::BTreeMap;
use std::sync::Arc;

use steve_store::Store;
use steve_store::base::Viewer;
use steve_store::base::Visibility;
use steve_store::base::mems::Mem;

pub use clock::{Clock, FakeClock, SystemClock};
pub use error::{ApiError, ApiResult};
pub use extract::{Body, Caller, Check, parse};
pub use gateway::{Gateway, GatewayOut, Tier};

/// Everything registered by the modules at startup. Immutable once [`Modules`] is built, so lookups
/// need no lock. Registering a key twice is a programming error and panics at startup.
#[derive(Default)]
pub struct Registry {
    targets: BTreeMap<String, Box<dyn changes::DraftTarget>>,
    ops: BTreeMap<String, Box<dyn changes::ChangeOp>>,
    handlers: BTreeMap<String, Box<dyn decisions::DecisionHandler>>,
    hooks: hooks::ItemHookList,
    contexts: context::PromptContexts,
    panels: Vec<Box<dyn panels::ItemPanel>>,
    outbound: Option<Box<dyn outbound::OutboundCheck>>,
    digests: Vec<Box<dyn DigestNotes>>,
    bot_deleted: Vec<Box<dyn BotDeleted>>,
}

/// A module that keeps references to Bots (channel personas, flow `bot` nodes): steve-core calls
/// it after `deleteBot`, once per workspace the Bot served, with the heir it used there, all
/// modules in one transaction.
pub trait BotDeleted: Send + Sync {
    fn on_bot_deleted(
        &self,
        tx: &steve_store::Tx,
        ws: &str,
        bot: &str,
        heir: &str,
        now: i64,
    ) -> ApiResult<()>;
}

/// Notes a module collected for the next report (`/汇报`; steve-flow's `digest` action). `take`
/// marks them as reported.
pub trait DigestNotes: Send + Sync {
    fn take(&self, ws: &str) -> Vec<String>;
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `changes::register(kind, Box<dyn DraftTarget>)`: a `target.kind` of `POST /api/config/draft`.
    pub fn target(&mut self, kind: &str, t: Box<dyn changes::DraftTarget>) -> &mut Self {
        assert!(
            self.targets.insert(kind.into(), t).is_none(),
            "draft target {kind} registered twice"
        );
        self
    }

    /// `changes::register_op(op, Box<dyn ChangeOp>)`: an operation name like `party.patch`.
    pub fn op(&mut self, name: &str, op: Box<dyn changes::ChangeOp>) -> &mut Self {
        assert!(
            self.ops.insert(name.into(), op).is_none(),
            "change op {name} registered twice"
        );
        self
    }

    /// `decisions::register(subject_type, Box<dyn DecisionHandler>)`.
    pub fn decision(
        &mut self,
        subject_type: &str,
        h: Box<dyn decisions::DecisionHandler>,
    ) -> &mut Self {
        assert!(
            self.handlers.insert(subject_type.into(), h).is_none(),
            "decision handler {subject_type} registered twice"
        );
        self
    }

    /// The draft target kinds the modules handle (the rest stay with steve-core's changes.rs).
    pub fn target_kinds(&self) -> Vec<&str> {
        self.targets.keys().map(String::as_str).collect()
    }

    pub fn handles_decision(&self, subject_type: &str) -> bool {
        self.handlers.contains_key(subject_type)
    }

    pub fn retain_on_completed_item_recovery(&self, d: &steve_store::Decision) -> bool {
        self.handlers
            .get(&d.subject_type)
            .is_some_and(|h| h.retain_on_completed_item_recovery(d))
    }

    /// `undoDecision` calls this in its transaction before reopening `d`: the handler of its
    /// `subject_type` may undo its side or refuse (`Err` → the undo is rolled back). No handler = Ok.
    pub fn before_undo(&self, tx: &steve_store::Tx, d: &steve_store::Decision) -> ApiResult<()> {
        match self.handlers.get(&d.subject_type) {
            Some(h) => h.before_undo(tx, d),
            None => Ok(()),
        }
    }

    /// Item lifecycle hooks, called in registration order.
    pub fn item_hooks(&mut self, h: Box<dyn hooks::ItemHooks>) -> &mut Self {
        self.hooks.push(h);
        self
    }

    /// A provider of extra prompt context.
    pub fn prompt_context(&mut self, p: Box<dyn context::PromptContext>) -> &mut Self {
        self.contexts.push(p);
        self
    }

    /// A provider of item panels; its id must be unique.
    pub fn item_panel(&mut self, p: Box<dyn panels::ItemPanel>) -> &mut Self {
        assert!(
            self.panels.iter().all(|x| x.id() != p.id()),
            "item panel {} registered twice",
            p.id()
        );
        self.panels.push(p);
        self
    }

    /// The disclosure check (one, from steve-channels).
    pub fn outbound_check(&mut self, c: Box<dyn outbound::OutboundCheck>) -> &mut Self {
        assert!(self.outbound.is_none(), "outbound check registered twice");
        self.outbound = Some(c);
        self
    }

    /// A module's reaction to a deleted Bot.
    pub fn bot_deleted(&mut self, h: Box<dyn BotDeleted>) -> &mut Self {
        self.bot_deleted.push(h);
        self
    }

    /// Runs every [`BotDeleted`] for one workspace in `tx`.
    pub fn on_bot_deleted(
        &self,
        tx: &steve_store::Tx,
        ws: &str,
        bot: &str,
        heir: &str,
        now: i64,
    ) -> ApiResult<()> {
        for h in &self.bot_deleted {
            h.on_bot_deleted(tx, ws, bot, heir, now)?;
        }
        Ok(())
    }

    /// A source of report notes.
    pub fn digest_notes(&mut self, d: Box<dyn DigestNotes>) -> &mut Self {
        self.digests.push(d);
        self
    }

    /// Takes every module's notes for `ws`, in registration order.
    pub fn take_digest(&self, ws: &str) -> Vec<String> {
        self.digests.iter().flat_map(|d| d.take(ws)).collect()
    }

    pub fn hooks(&self) -> &hooks::ItemHookList {
        &self.hooks
    }

    pub fn contexts(&self) -> &context::PromptContexts {
        &self.contexts
    }
}

/// Shared state of the module routers. Cheap to clone.
#[derive(Clone)]
pub struct Modules(Arc<Inner>);

pub struct Inner {
    pub store: Store,
    pub gateway: Arc<dyn Gateway>,
    pub registry: Registry,
    pub clock: Arc<dyn Clock>,
}

impl std::ops::Deref for Modules {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Modules {
    pub fn new(store: Store, gateway: Arc<dyn Gateway>, registry: Registry) -> Self {
        Self::with_clock(store, gateway, registry, Arc::new(SystemClock))
    }

    /// Like [`Modules::new`] with a given clock (tests: [`FakeClock`]).
    pub fn with_clock(
        store: Store,
        gateway: Arc<dyn Gateway>,
        registry: Registry,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self(Arc::new(Inner {
            store,
            gateway,
            registry,
            clock,
        }))
    }

    /// Runs synchronous store work off the async runtime (the store is synchronous by design).
    pub async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Modules) -> ApiResult<R> + Send + 'static,
    ) -> ApiResult<R> {
        let m = self.clone();
        steve_store::blocking(move || f(&m)).await
    }

    /// Single-user read visibility needs no team/space providers or database reads.
    /// The caller's identity and workspace still govern identity-specific operations.
    pub fn viewer(&self, c: &Caller) -> ApiResult<Viewer> {
        Ok(Viewer::bare(&c.ws, &c.me))
    }
}

/// An object the ownership check applies to.
pub trait Owned {
    fn workspace_id(&self) -> &str;
    /// `None` for objects without a visibility (visible to the whole workspace).
    fn visibility(&self) -> Option<&Visibility>;
    fn created_by(&self) -> &str;
}

impl Owned for Mem {
    fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    fn visibility(&self) -> Option<&Visibility> {
        Some(&self.visibility)
    }
    fn created_by(&self) -> &str {
        &self.created_by
    }
}

/// `own_<x>(ws, me, id)`: exists, in the viewer's workspace, visible to them; otherwise
/// `404 没有这个<noun>`. Also for objects referenced from a request body (with their own noun).
pub fn own<T: Owned>(x: Option<T>, viewer: &Viewer, noun: &str) -> ApiResult<T> {
    match x {
        Some(x)
            if x.workspace_id() == viewer.ws
                && x.visibility()
                    .is_none_or(|v| viewer.sees(v, x.created_by())) =>
        {
            Ok(x)
        }
        _ => Err(ApiError::not_found(noun)),
    }
}

//! [`Channels`]: what the channel module runs on (the modules state, the connections service, the
//! S1 entry point, the registered channel adapters), and [`MessageSink`], the inbound entry point as
//! a trait object for other modules.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use steve_flow::{EventSink, FlowCx};
use steve_modules::Modules;
use steve_modules::accounts::connections::Connections;
use steve_store::Store;

use crate::ChannelResult;
use crate::channel::{ChannelAdapter, InboundMessage, ProfileAdapter, SpaceEvent};
use crate::conversations::Thread;
use crate::ext::ChannelDecl;
use crate::inbound::Inbound;
use crate::media::MediaFetch;
use crate::membership::SpaceChange;
use crate::names::Names;
use crate::reactions::{AdapterReactor, Locks, Reactor};
use crate::respond::{Fallback, Responder};
use steve_modules::contacts::PartyHooks;

/// Shared state of the channel module. Cheap to clone.
#[derive(Clone)]
pub struct Channels(Arc<Inner>);

/// A [`Channels`] that does not keep the module alive (for what the module itself holds, like the
/// flow actions inside the flow module).
#[derive(Clone)]
pub struct WeakChannels(Weak<Inner>);

impl WeakChannels {
    pub fn upgrade(&self) -> Option<Channels> {
        self.0.upgrade().map(Channels)
    }
}

struct Inner {
    modules: Modules,
    conns: Connections,
    sink: Arc<dyn EventSink>,
    adapters: BTreeMap<String, Arc<dyn ChannelAdapter>>,
    others: Vec<Arc<dyn OtherEvents>>,
    responder: Arc<dyn Responder>,
    flow: Option<Arc<FlowCx>>,
    reactor: Arc<dyn Reactor>,
    thread: Option<Arc<dyn Thread>>,
    locks: Locks,
    gens: crate::conversations::Generations,
    profiles: BTreeMap<String, Arc<dyn ProfileAdapter>>,
    media: BTreeMap<String, Arc<dyn MediaFetch>>,
    history: BTreeMap<String, Arc<dyn crate::history::HistoryRead>>,
    connectors: BTreeMap<String, Arc<dyn crate::connector::ConnectorTools>>,
    decls: BTreeMap<String, ChannelDecl>,
    party_hooks: Vec<Arc<dyn PartyHooks>>,
    yolo: Option<Arc<dyn YoloSwitch>>,
    names: Names,
    /// Where replies' knowledge is searched (the Hub sets the workspace's store; SQLite until then).
    knowledge: std::sync::OnceLock<Arc<dyn steve_knowledge::port::KnowledgeStore>>,
}

/// Events of a connection that are not messages (Feishu address book changes → the directory
/// sync, F5). The long connection offers every such envelope to each hook in turn until one takes it.
pub trait OtherEvents: Send + Sync {
    /// `true` when it handled the envelope.
    fn on_event(&self, ws: &str, connection_id: &str, envelope: &serde_json::Value) -> bool;
}

/// Whether YOLO decides a workspace's cards now (steve-core's switch; a channel card has no item).
pub trait YoloSwitch: Send + Sync {
    fn on(&self, ws: &str) -> bool;
}

/// Builds [`Channels`]: `Channels::builder(modules, conns, sink).adapter(feishu).build()`.
pub struct ChannelsBuilder {
    modules: Modules,
    conns: Connections,
    sink: Arc<dyn EventSink>,
    adapters: BTreeMap<String, Arc<dyn ChannelAdapter>>,
    others: Vec<Arc<dyn OtherEvents>>,
    responder: Arc<dyn Responder>,
    flow: Option<Arc<FlowCx>>,
    reactor: Arc<dyn Reactor>,
    thread: Option<Arc<dyn Thread>>,
    profiles: BTreeMap<String, Arc<dyn ProfileAdapter>>,
    media: BTreeMap<String, Arc<dyn MediaFetch>>,
    history: BTreeMap<String, Arc<dyn crate::history::HistoryRead>>,
    connectors: BTreeMap<String, Arc<dyn crate::connector::ConnectorTools>>,
    decls: BTreeMap<String, ChannelDecl>,
    party_hooks: Vec<Arc<dyn PartyHooks>>,
    yolo: Option<Arc<dyn YoloSwitch>>,
}

impl ChannelsBuilder {
    pub fn connector(mut self, c: Arc<dyn crate::connector::ConnectorTools>) -> Self {
        self.connectors.insert(c.adapter_id().to_string(), c);
        self
    }

    /// Registers an identity that can look people up, under the
    /// `connections.adapter` it serves: Feishu's bot and you.
    pub fn profiles(mut self, a: Arc<dyn ProfileAdapter>) -> Self {
        self.profiles.insert(a.adapter_id().to_string(), a);
        self
    }

    /// Registers the history view of a channel side under the `connections.adapter` it serves:
    /// how an agent's `steve_im_read` reads its conversations ([`crate::history`]).
    pub fn history(mut self, h: Arc<dyn crate::history::HistoryRead>) -> Self {
        self.history.insert(h.adapter_id().to_string(), h);
        self
    }

    /// Registers the media view of a channel side under the `connections.adapter` it serves: how
    /// the resources of messages its connections received are fetched.
    pub fn media(mut self, m: Arc<dyn MediaFetch>) -> Self {
        self.media.insert(m.adapter_id().to_string(), m);
        self
    }

    /// Registers how a channel side behaves ([`ChannelDecl`]), under its adapter id: whether its
    /// messages carry provider ids, how precise their times are, the system of its senders.
    pub fn decl(mut self, d: ChannelDecl) -> Self {
        self.decls.insert(d.adapter.clone(), d);
        self
    }

    /// The contacts ports' party hooks, for the merges that names settle (accounts move along).
    /// Default: the accounts module's alone.
    pub fn party_hooks(mut self, hooks: Vec<Arc<dyn PartyHooks>>) -> Self {
        self.party_hooks = hooks;
        self
    }

    /// YOLO's switch: while it decides a workspace's cards, a flagged reply that waits for approval is
    /// still rewritten once first (YOLO never approves a flagged text). Default: off.
    pub fn yolo(mut self, y: Arc<dyn YoloSwitch>) -> Self {
        self.yolo = Some(y);
        self
    }

    /// What puts the “收到 → 完成” reactions on (the outbox, which can react as you too). Default
    /// [`AdapterReactor`]: bot accounts through their channel adapter.
    pub fn reactor(mut self, r: Arc<dyn Reactor>) -> Self {
        self.reactor = r;
        self
    }

    /// Steve's conversation (steve-core's thread): with it, a `reply` / `draft` goes through the
    /// same routing as web chat in a conversation of its own per space ([`crate::conversations`]).
    /// Without it the C4 responder composes the reply directly.
    pub fn thread(mut self, t: Arc<dyn Thread>) -> Self {
        self.thread = Some(t);
        self
    }

    /// Adds a hook for the non-message events of the long connections.
    pub fn other_events(mut self, h: Arc<dyn OtherEvents>) -> Self {
        self.others.push(h);
        self
    }
    /// What carries out `reply`, `draft` and `create_item` and trigger words (C4/C6). Default
    /// [`Fallback`]: never sends — replies become drafts, items become handoffs.
    pub fn responder(mut self, r: Arc<dyn Responder>) -> Self {
        self.responder = r;
        self
    }

    /// The flow module, so “转成流程” validates the compiled graph against the registered node
    /// kinds (without it the graph is saved as compiled).
    pub fn flow(mut self, f: Arc<FlowCx>) -> Self {
        self.flow = Some(f);
        self
    }

    /// Registers a channel adapter under its `id()` (the `connections.adapter` it serves). The
    /// same object must be registered with `Connections` as a provider.
    pub fn adapter(mut self, a: Arc<dyn ChannelAdapter>) -> Self {
        let id = a.id().to_string();
        assert!(
            self.adapters.insert(id.clone(), a).is_none(),
            "channel adapter {id} registered twice"
        );
        self
    }

    pub fn build(self) -> Channels {
        for m in self.media.values() {
            crate::media::publish(m);
        }
        // who can look people up, for the contact pages' “can be looked up” (by declared system)
        for id in self.profiles.keys() {
            if let Some(d) = self.decls.get(id) {
                steve_modules::contacts::profile::register_lookup(id, &d.handle_system);
            }
        }
        Channels(Arc::new(Inner {
            modules: self.modules,
            conns: self.conns,
            sink: self.sink,
            adapters: self.adapters,
            others: self.others,
            responder: self.responder,
            flow: self.flow,
            reactor: self.reactor,
            thread: self.thread,
            locks: Locks::default(),
            gens: Default::default(),
            profiles: self.profiles,
            media: self.media,
            history: self.history,
            connectors: self.connectors,
            decls: self.decls,
            party_hooks: self.party_hooks,
            yolo: self.yolo,
            names: Names::default(),
            knowledge: std::sync::OnceLock::new(),
        }))
    }
}

impl Channels {
    /// `sink` is S1's entry point (`Arc::new(flow.clone())` of `Arc<FlowCx>`).
    pub fn builder(
        modules: Modules,
        conns: Connections,
        sink: Arc<dyn EventSink>,
    ) -> ChannelsBuilder {
        ChannelsBuilder {
            modules,
            conns,
            sink,
            adapters: BTreeMap::new(),
            others: Vec::new(),
            responder: Arc::new(Fallback),
            flow: None,
            reactor: Arc::new(AdapterReactor),
            thread: None,
            profiles: BTreeMap::new(),
            media: BTreeMap::new(),
            history: BTreeMap::new(),
            connectors: BTreeMap::new(),
            decls: BTreeMap::new(),
            party_hooks: vec![Arc::new(steve_modules::accounts::AccountHooks)],
            yolo: None,
        }
    }

    pub fn downgrade(&self) -> WeakChannels {
        WeakChannels(Arc::downgrade(&self.0))
    }

    pub fn modules(&self) -> &Modules {
        &self.0.modules
    }

    pub fn store(&self) -> &Store {
        &self.0.modules.store
    }

    pub fn now(&self) -> i64 {
        self.0.modules.clock.now()
    }

    pub fn connections(&self) -> &Connections {
        &self.0.conns
    }

    pub(crate) fn sink(&self) -> &dyn EventSink {
        self.0.sink.as_ref()
    }

    /// The channel adapter serving `connections.adapter = id`.
    pub fn adapter(&self, id: &str) -> Option<Arc<dyn ChannelAdapter>> {
        self.0.adapters.get(id).cloned()
    }

    /// Offers a non-message event to the [`OtherEvents`] hooks; `true` when one took it.
    pub fn other_event(&self, ws: &str, connection_id: &str, envelope: &serde_json::Value) -> bool {
        self.0
            .others
            .iter()
            .any(|h| h.on_event(ws, connection_id, envelope))
    }

    pub(crate) fn responder(&self) -> &dyn Responder {
        self.0.responder.as_ref()
    }

    pub(crate) fn flow(&self) -> Option<&Arc<FlowCx>> {
        self.0.flow.as_ref()
    }

    /// YOLO decides `ws`'s cards now ([`ChannelsBuilder::yolo`]).
    pub(crate) fn yolo_on(&self, ws: &str) -> bool {
        self.0.yolo.as_ref().is_some_and(|y| y.on(ws))
    }

    /// Sets the knowledge store replies search (the same one as the knowledge module's).
    pub fn set_knowledge_store(&self, store: Arc<dyn steve_knowledge::port::KnowledgeStore>) {
        if self.0.knowledge.set(store).is_err() {
            tracing::warn!("channels: knowledge store already set");
        }
    }

    pub fn knowledge_store(&self) -> &dyn steve_knowledge::port::KnowledgeStore {
        self.0
            .knowledge
            .get_or_init(|| Arc::new(steve_knowledge::port::SqliteStore))
            .as_ref()
    }

    pub(crate) fn reactor(&self) -> &dyn Reactor {
        self.0.reactor.as_ref()
    }

    pub(crate) fn reaction_locks(&self) -> &Locks {
        &self.0.locks
    }

    pub(crate) fn thread(&self) -> Option<&Arc<dyn Thread>> {
        self.0.thread.as_ref()
    }

    pub(crate) fn generations(&self) -> &crate::conversations::Generations {
        &self.0.gens
    }

    pub(crate) fn names_state(&self) -> &Names {
        &self.0.names
    }

    /// The identity that looks people up for `connections.adapter = id`.
    pub fn profile_adapter(&self, id: &str) -> Option<Arc<dyn ProfileAdapter>> {
        self.0.profiles.get(id).cloned()
    }

    /// The adapters that can look people up.
    pub fn profile_adapter_ids(&self) -> Vec<String> {
        self.0.profiles.keys().cloned().collect()
    }

    /// The systems whose people a registered profile view looks up (its side's declared
    /// `handle_system`).
    pub fn profile_systems(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in self.0.profiles.keys() {
            if let Some(d) = self.0.decls.get(id)
                && !out.contains(&d.handle_system)
            {
                out.push(d.handle_system.clone());
            }
        }
        out
    }

    pub(crate) fn connector_views(
        &self,
    ) -> &BTreeMap<String, Arc<dyn crate::connector::ConnectorTools>> {
        &self.0.connectors
    }

    /// The history views, by `connections.adapter`.
    pub(crate) fn history_views(&self) -> &BTreeMap<String, Arc<dyn crate::history::HistoryRead>> {
        &self.0.history
    }

    /// The media view for `connections.adapter = id`.
    pub fn media(&self, id: &str) -> Option<Arc<dyn MediaFetch>> {
        self.0.media.get(id).cloned()
    }

    /// The adapters whose messages' resources can be fetched.
    pub fn media_adapter_ids(&self) -> Vec<String> {
        self.0.media.keys().cloned().collect()
    }

    /// The declaration of the channel side serving `connections.adapter = id`, when one was
    /// registered.
    pub fn decl(&self, id: &str) -> Option<&ChannelDecl> {
        self.0.decls.get(id)
    }

    /// The declared channel sides, by adapter id.
    pub fn decls(&self) -> impl Iterator<Item = &ChannelDecl> {
        self.0.decls.values()
    }

    pub(crate) fn party_hooks(&self) -> &[Arc<dyn PartyHooks>] {
        &self.0.party_hooks
    }

    /// Registered channel adapters, by id.
    pub fn adapters(&self) -> impl Iterator<Item = &Arc<dyn ChannelAdapter>> {
        self.0.adapters.values()
    }
}

/// The inbound entry point as a trait object: what the Feishu long connection, the self poller
/// (F8) and other message sources (mail threads recorded as exchanges) call. `Channels` runs the
/// whole pipeline (steps 1–7: rules included).
#[async_trait]
pub trait MessageSink: Send + Sync {
    /// [`Channels::on_inbound`].
    async fn inbound(&self, ws: &str, conn: &str, msg: InboundMessage) -> ChannelResult<Inbound>;

    /// [`Channels::on_space_event`]. Sinks that only take messages ignore it.
    async fn space_event(
        &self,
        ws: &str,
        conn: &str,
        ev: SpaceEvent,
    ) -> ChannelResult<Option<SpaceChange>> {
        let _ = (ws, conn, ev);
        Ok(None)
    }
}

#[async_trait]
impl MessageSink for Channels {
    /// The whole pipeline, steps 1–7 ([`Channels::on_message`]).
    async fn inbound(&self, ws: &str, conn: &str, msg: InboundMessage) -> ChannelResult<Inbound> {
        self.on_message(ws, conn, msg).await
    }

    async fn space_event(
        &self,
        ws: &str,
        conn: &str,
        ev: SpaceEvent,
    ) -> ChannelResult<Option<SpaceChange>> {
        self.on_space_event(ws, conn, ev).await
    }
}

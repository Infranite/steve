//! Email: accounts, sync, drafts and sending.
//!
//! Slice E1 — IMAP connection, sync and the local mail library:
//! - [`adapter`]: the [`MailAdapter`] / [`MailSink`] traits and what they exchange;
//! - [`adapters::ImapAdapter`]: IMAP + SMTP with an app password, presets for the dialog;
//! - [`store`]: [`store::StoreSink`] — parse, dedupe, raw file, attachments, identification, threads;
//! - [`parse`]: MIME → text, `body_new`, snippet, attachments, signature organization;
//! - [`events`]: the [`MailEvents`] seam to steve-flow (`ingest` / fixed outcomes) and SSE [`MailNotify`];
//! - [`Mail`] (`sync.rs`): sync now, background runners (IDLE + 2-minute polling, backoff 5 s → 5 min);
//! - [`retention`]: `keepMail`; [`timeline::MailTimeline`]: “往来” of a contact;
//!   [`usage::MailUsage`]: cleanup on disconnect; [`settings`]: `mail_account_settings`.
//!
//! Integration (integrator):
//! 1. `let imap = Arc::new(ImapAdapter::new(MailConfig::from_env(|k| std::env::var(k).ok())?));`
//!    register it with `Connections::builder(..).provider(imap.clone()).usage(Arc::new(MailUsage::new(cfg.clone())))`;
//! 2. `let mail = Mail::builder(modules, conns, cfg).adapter(imap).events(<steve-flow impl of
//!    MailEvents>).notify(<publisher of {type:"mail", id, ws}>).build();` and `mail.spawn_jobs()`;
//! 3. contacts `Ports.timelines.push(Arc::new(MailTimeline))`;
//! 4. `HubEvent` gets `{type:"mail", id, ws}`; SETTINGS gets `keepMail`.
//!
//! Slice E4 — triage: [`nodes`] (`mail.draft_reply`, `mail.archive`, `mail.mark_read`, the mail
//! links of items, the `thread_key → item` lookup, `archive_on_done` via [`nodes::MailHooks`]),
//! [`template`] (“邮件分诊”, [`template::install`]) and [`flow_events::FlowEvents`] (mail events
//! into `steve_flow::ingest`).
//!
//! Slice E6 — knowledge and conversational configuration: [`knowledge`] (“加进知识库”,
//! candidates when a thread is done, [`knowledge::ThreadLoader`]), [`config::targets`] (target
//! `mail_account`, op `mail.account.patch`) and the thread routes `archive` / `read` / `to-knowledge`.
//! Integrator: `Mail::builder(..).knowledge(kb)`; `Knowledge::with_loaders(guard, notify,
//! vec![Arc::new(ThreadLoader), Arc::new(FilesLoader::new(data_dir))])`; `config::targets::register(r)`.

steve_i18n::init!();

pub mod adapter;
pub mod adapters;
pub mod api;
pub mod attach;
pub mod compose;
pub mod config;
pub mod drafts;
pub mod error;
pub mod events;
pub mod flow_events;
pub mod items;
pub mod knowledge;
pub mod mcp;
pub mod nodes;
pub mod outbox;
pub mod parse;
pub mod repo;
pub mod retention;
pub mod send;
pub mod settings;
pub mod store;
mod sync;
pub mod template;
pub mod threads;
pub mod timeline;
pub mod usage;

#[cfg(feature = "fake")]
pub mod fake;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use steve_modules::Modules;
use steve_modules::accounts::connections::Connections;

pub use adapter::{
    FetchedMessage, FlagChange, FolderInfo, FolderState, MailAdapter, MailSink, ScopeMode,
    SendMeta, SentRef, SyncReport, SyncSession, SyncState,
};
pub use adapters::ImapAdapter;
pub use config::MailConfig;
pub use error::{AdapterError, MailError, MailResult};
pub use events::{Delivery, MailEvent, MailEvents, MailNotify, RecordingEvents};
pub use send::{DraftHooks, DraftLink, SecretScan, SendOutcome};
pub use sync::{BACKOFF_MAX, BACKOFF_MIN, POLL};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Builds [`Mail`].
pub struct MailBuilder {
    m: Modules,
    conns: Connections,
    cfg: MailConfig,
    adapters: BTreeMap<String, Arc<dyn MailAdapter>>,
    events: Arc<dyn MailEvents>,
    notify: Arc<dyn MailNotify>,
    link: Option<DraftLink>,
    secrets: Arc<dyn SecretScan>,
    hooks: Arc<dyn DraftHooks>,
    guard: Arc<dyn steve_knowledge::Guard>,
    files: Arc<dyn attach::FileStore>,
    items: Arc<dyn items::MailItems>,
    kb: Option<steve_knowledge::Knowledge>,
}

impl MailBuilder {
    /// The knowledge module (E6): `{type:"kb"}` events for threads added to the knowledge base and
    /// candidate memories when a thread is done. Without it, threads are still indexed (same store)
    /// but no candidates are extracted.
    pub fn knowledge(mut self, kb: steve_knowledge::Knowledge) -> Self {
        self.kb = Some(kb);
        self
    }
    /// The link whose [`DraftLink::handler`] was registered for `mail_draft` decisions.
    pub fn drafts(mut self, link: DraftLink) -> Self {
        self.link = Some(link);
        self
    }
    /// The vault redactor (V4); without it nothing is recognised as a secret.
    pub fn secrets(mut self, s: Arc<dyn SecretScan>) -> Self {
        self.secrets = s;
        self
    }
    /// steve-core's side of drafts (agent feedback, finishing items).
    pub fn hooks(mut self, h: Arc<dyn DraftHooks>) -> Self {
        self.hooks = h;
        self
    }
    /// `guard::scan` for mail text going into prompts (default: the knowledge module's token guard).
    pub fn guard(mut self, g: Arc<dyn steve_knowledge::Guard>) -> Self {
        self.guard = g;
        self
    }
    /// steve-hub's attachment store (landing attachments, files an agent attaches; E5).
    pub fn files(mut self, f: Arc<dyn attach::FileStore>) -> Self {
        self.files = f;
        self
    }
    /// steve-core's `createItem` for `to-item` (E5).
    pub fn items(mut self, i: Arc<dyn items::MailItems>) -> Self {
        self.items = i;
        self
    }
    /// Registers an adapter (the same one registered as a connection provider).
    pub fn adapter(mut self, a: Arc<dyn MailAdapter>) -> Self {
        let id = a.id().to_string();
        assert!(
            self.adapters.insert(id.clone(), a).is_none(),
            "mail adapter {id} registered twice"
        );
        self
    }
    pub fn events(mut self, e: Arc<dyn MailEvents>) -> Self {
        self.events = e;
        self
    }
    pub fn notify(mut self, n: Arc<dyn MailNotify>) -> Self {
        self.notify = n;
        self
    }
    pub fn build(self) -> Mail {
        let link = self.link.unwrap_or_default();
        link.bind(self.m.clock.clone(), self.hooks);
        Mail(Arc::new(Inner {
            link,
            send_recovered: Mutex::new(false),
            secrets: self.secrets,
            guard: self.guard,
            files: self.files,
            items: self.items,
            kb: self.kb,
            m: self.m,
            conns: self.conns,
            cfg: self.cfg,
            adapters: self.adapters,
            events: self.events,
            notify: self.notify,
            locks: Mutex::new(HashMap::new()),
            flag_checks: Mutex::new(HashMap::new()),
            runners: Mutex::new(HashMap::new()),
        }))
    }
}

struct Inner {
    m: Modules,
    conns: Connections,
    cfg: MailConfig,
    adapters: BTreeMap<String, Arc<dyn MailAdapter>>,
    events: Arc<dyn MailEvents>,
    notify: Arc<dyn MailNotify>,
    /// One sync at a time per connection.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Last flag re-check per `(connection, folder)` (servers without CONDSTORE).
    flag_checks: Mutex<HashMap<(String, String), i64>>,
    /// Background runners by connection.
    runners: Mutex<HashMap<String, sync::Runner>>,
    link: DraftLink,
    /// One startup recovery gate for the single sender; shared by all Mail clones.
    send_recovered: Mutex<bool>,
    secrets: Arc<dyn SecretScan>,
    guard: Arc<dyn steve_knowledge::Guard>,
    files: Arc<dyn attach::FileStore>,
    items: Arc<dyn items::MailItems>,
    kb: Option<steve_knowledge::Knowledge>,
}

/// The mail module. Cheap to clone.
#[derive(Clone)]
pub struct Mail(Arc<Inner>);

impl Mail {
    /// Events default to [`RecordingEvents`]-like dropping ([`events::NoEvents`]) and SSE to nothing
    /// until the integrator sets them.
    pub fn builder(m: Modules, conns: Connections, cfg: MailConfig) -> MailBuilder {
        MailBuilder {
            m,
            conns,
            cfg,
            adapters: BTreeMap::new(),
            events: Arc::new(events::NoEvents),
            notify: Arc::new(events::NoNotify),
            link: None,
            secrets: Arc::new(send::NoSecrets),
            hooks: Arc::new(send::NoHooks),
            guard: Arc::new(steve_knowledge::TokenGuard),
            files: Arc::new(attach::NoFiles),
            items: Arc::new(items::NoItems),
            kb: None,
        }
    }

    pub fn modules(&self) -> &Modules {
        &self.0.m
    }
    pub fn connections(&self) -> &Connections {
        &self.0.conns
    }
    pub fn config(&self) -> &MailConfig {
        &self.0.cfg
    }
    /// The adapter of an adapter id.
    pub fn adapter(&self, id: &str) -> Option<Arc<dyn MailAdapter>> {
        self.0.adapters.get(id).cloned()
    }
    fn now(&self) -> i64 {
        self.0.m.clock.now()
    }
}

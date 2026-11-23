//! Test helpers: a [`FakeCore`] that writes real `items` rows and records every call,
//! [`FakeEventAdapter`], a scripted event source, and [`FlowEnv`], the module on a temp store
//! (steve-modules' `TestEnv`) with bots and a fake clock.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};
use steve_modules::testkit::TestEnv;
use steve_store::testkit::flow as flow_store;

use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, Connections, ConnectionsConfig,
    ProviderError,
};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;

use crate::adapter::{
    AdapterError, Batch, EventAdapter, EventTypeDef, IngestMode, PushRequest, RawEvent, ScopeDef,
};
use crate::nodes::{Grant, NodeOutcome, RunCx};
use crate::ports::{Core, ItemRequest};
use crate::{Action, FlowCx, FlowSetup, GraphEngine, NodeKind};

/// One call to [`FakeCore`].
#[derive(Debug, Clone, PartialEq)]
pub enum CoreCall {
    Create(ItemRequest),
    Pause(String),
    Resume(String),
    Post {
        ws: String,
        text: String,
        meta: Value,
    },
    Supplement {
        item: String,
        text: String,
    },
}

/// steve-core stand-in: `create_item` inserts an `items` row `ST-<n>` (with `accept_mode`, `source`,
/// `bot_id`, `project_id`, tags); pause/resume set `status`.
#[derive(Default)]
pub struct FakeCore {
    pub calls: Mutex<Vec<CoreCall>>,
    pub store: Mutex<Option<steve_store::Store>>,
    /// Make `post_steve_message` fail.
    pub fail_post: Mutex<bool>,
    /// [`Core::can_run`] refuses every Bot with this reason (#222).
    pub blocked: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FakeCore {
    pub fn calls(&self) -> Vec<CoreCall> {
        lock(&self.calls).clone()
    }

    pub fn created(&self) -> Vec<ItemRequest> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                CoreCall::Create(r) => Some(r),
                _ => None,
            })
            .collect()
    }

    pub fn posts(&self) -> Vec<(String, Value)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                CoreCall::Post { text, meta, .. } => Some((text, meta)),
                _ => None,
            })
            .collect()
    }

    fn with_store<T>(
        &self,
        f: impl FnOnce(&steve_store::Store) -> Result<T, String>,
    ) -> Result<T, String> {
        let s = lock(&self.store).clone().ok_or("no store")?;
        f(&s)
    }
}

#[async_trait]
impl Core for FakeCore {
    fn can_run(
        &self,
        _: &steve_store::Store,
        _: &str,
        _: Option<&str>,
        _: &str,
    ) -> Result<(), String> {
        match lock(&self.blocked).clone() {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }
    async fn create_item(&self, req: ItemRequest) -> Result<String, String> {
        lock(&self.calls).push(CoreCall::Create(req.clone()));
        self.with_store(|s| {
            let bot = match req.bot.as_deref() {
                Some(bot) => bot.to_owned(),
                None => {
                    s.bots()
                        .list(Some(&req.ws))
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .find(|bot| bot.enabled)
                        .ok_or("fixture has no enabled Bot")?
                        .id
                }
            };
            flow_store::create_item(
                s,
                flow_store::FixtureItem {
                    ws: &req.ws,
                    title: &req.title,
                    bot: &bot,
                    project: req.project.as_deref(),
                    source: &json!([req.source.0, req.source.1]),
                    requester: req.requester.as_deref(),
                    tag_ids: &req.tag_ids,
                    propose: req.propose,
                    accept_auto: req.accept_auto,
                },
            )
            .map_err(|e| e.to_string())
        })
    }

    async fn pause_item(&self, _: &str, item_id: &str) -> Result<(), String> {
        lock(&self.calls).push(CoreCall::Pause(item_id.into()));
        self.with_store(|s| {
            flow_store::set_item_status(s, item_id, "paused").map_err(|e| e.to_string())
        })
    }

    async fn resume_item(&self, _: &str, item_id: &str) -> Result<(), String> {
        lock(&self.calls).push(CoreCall::Resume(item_id.into()));
        self.with_store(|s| {
            flow_store::set_item_status(s, item_id, "run").map_err(|e| e.to_string())
        })
    }

    async fn post_steve_message(&self, ws: &str, text: &str, meta: Value) -> Result<(), String> {
        if *lock(&self.fail_post) {
            return Err("post failed".into());
        }
        lock(&self.calls).push(CoreCall::Post {
            ws: ws.into(),
            text: text.into(),
            meta,
        });
        Ok(())
    }

    async fn digest_text(&self, _: &str) -> Result<String, String> {
        Ok("现在：0 件等你，1 件在进行，今天完成 2 件。".into())
    }

    async fn supplement(&self, _: &str, item_id: &str, text: &str, _: &str) -> Result<(), String> {
        lock(&self.calls).push(CoreCall::Supplement {
            item: item_id.into(),
            text: text.into(),
        });
        Ok(())
    }
}

/// The flow module on a fresh store: bots `steve` (main), `ops`, `off` (disabled) in `default`,
/// `w2bot` only in `w2`; a project `prj_a` and a tag `tag_a` in `default`.
pub struct FlowEnv {
    pub env: TestEnv,
    pub core: Arc<FakeCore>,
    pub flow: Arc<FlowCx>,
}

impl FlowEnv {
    pub fn new() -> Self {
        Self::build(Vec::new())
    }

    /// With event adapters registered and a real `Connections` service (vault keys in the temp dir)
    /// for their [`ConnCx`]. Make connections with [`FlowEnv::connection`].
    pub fn with_events(adapters: Vec<Arc<dyn EventAdapter>>) -> Self {
        Self::build(adapters)
    }

    fn build(adapters: Vec<Arc<dyn EventAdapter>>) -> Self {
        Self::with(adapters, Vec::new(), Vec::new())
    }

    /// With event adapters, extra node kinds (stand-ins for `bot`, `script`, `http`) and actions.
    pub fn with(
        adapters: Vec<Arc<dyn EventAdapter>>,
        nodes: Vec<Arc<dyn NodeKind>>,
        actions: Vec<Arc<dyn Action>>,
    ) -> Self {
        Self::with_late_nodes(adapters, actions, move |_| nodes)
    }

    /// Like [`FlowEnv::with`], with node kinds made once the seeded store exists (a vault-backed
    /// `http` node needs the store).
    pub fn with_late_nodes(
        adapters: Vec<Arc<dyn EventAdapter>>,
        actions: Vec<Arc<dyn Action>>,
        nodes: impl FnOnce(&TestEnv) -> Vec<Arc<dyn NodeKind>>,
    ) -> Self {
        let core = Arc::new(FakeCore::default());
        let mut setup = FlowSetup::new(core.clone());
        for a in actions {
            setup = setup.action(a);
        }
        let env = TestEnv::new(|r| setup.register(r));
        *lock(&core.store) = Some(env.store().clone());
        flow_store::seed_workspace(env.store()).expect("seed bots");
        if !adapters.is_empty() {
            let keys = FileKeyStore::open(env.dir.path().join("keys")).expect("keys");
            let vault = Vault::new(env.store().clone(), Arc::new(keys));
            let mut b = Connections::builder(
                env.modules.clone(),
                vault,
                ConnectionsConfig::new(Vec::new(), true, |_| None),
            );
            for a in &adapters {
                let p: Arc<dyn ConnectionProvider> = a.clone();
                b = b.provider(p);
            }
            setup = setup.connections(b.build());
            for a in adapters {
                setup = setup.event_adapter(a);
            }
        }
        for n in nodes(&env) {
            setup = setup.node(n);
        }
        let flow = setup.build(env.modules.clone(), Arc::new(GraphEngine));
        Self { env, core, flow }
    }

    /// A connection of `adapter` in `ws` with the `events` capability. `owner = Some(user)`: an
    /// account of that user's “我” (a personal connection, private events); `None`: of the own
    /// organization (shared). Created by `created_by`.
    pub fn connection(
        &self,
        ws: &str,
        adapter: &str,
        owner: Option<&str>,
        created_by: &str,
    ) -> String {
        flow_store::event_connection(self.env.store(), ws, adapter, owner, created_by)
            .expect("connection")
    }

    pub fn run_row(&self, id: &str) -> steve_store::RunRow {
        self.env
            .store()
            .read(|r| steve_store::RunRows(r).get(id))
            .unwrap()
            .unwrap()
    }

    pub fn flow_row(&self, id: &str) -> steve_store::FlowRow {
        self.env
            .store()
            .read(|r| steve_store::FlowRows(r).get(id))
            .unwrap()
            .unwrap()
    }

    pub fn event_runs(&self, id: &str) -> Vec<steve_store::RunRow> {
        self.env
            .store()
            .read(|r| steve_store::RunRows(r).by_event(steve_modules::testkit::WS, id))
            .unwrap()
    }

    pub fn signal_row(&self, id: &str) -> steve_store::SignalRow {
        use steve_modules::testkit::{ME, WS, caller};
        let viewer = self.env.modules.viewer(&caller(WS, ME)).unwrap();
        self.env
            .store()
            .read(|r| steve_store::SignalRows(r).get(&viewer, id))
            .unwrap()
            .unwrap()
    }

    pub fn router(&self) -> Router {
        crate::routes(self.flow.clone()).with_state(self.env.modules.clone())
    }

    pub fn now(&self) -> i64 {
        self.flow.now()
    }
}

impl Default for FlowEnv {
    fn default() -> Self {
        Self::new()
    }
}

/// The types of [`FakeEventAdapter`]: `mail.new`, `mail.reply`, and `mail.bulk` (off by default).
pub const FAKE_TYPES: &[EventTypeDef] = &[
    EventTypeDef::new("mail.new", "收到新邮件", "每封进入收件箱的邮件"),
    EventTypeDef::new("mail.reply", "收到回复", "对你发出的邮件的回复"),
    EventTypeDef::new("mail.bulk", "群发邮件", "通知、订阅").off(),
];

/// A scripted event source (`FakeEventAdapter` of the spec): adapter id `fake_mail`, source `mail`.
/// [`FakeEventAdapter::emit`] appends to its feed; `poll(cursor)` returns the feed after `cursor`
/// (an index) and the new end. Push bodies are a JSON array of `{type, id, summary, scope?}` and must
/// carry `x-fake-signature: ok`.
pub struct FakeEventAdapter {
    pub mode: IngestMode,
    pub feed: Mutex<Vec<RawEvent>>,
    pub scopes: Mutex<Vec<ScopeDef>>,
    /// The next poll fails with this.
    pub fail_next: Mutex<Option<ProviderError>>,
}

impl FakeEventAdapter {
    pub const ID: &'static str = "fake_mail";

    pub fn poll_every(every: std::time::Duration) -> Self {
        Self {
            mode: IngestMode::Poll { every },
            feed: Mutex::default(),
            scopes: Mutex::new(vec![
                ScopeDef::new("INBOX", "收件箱"),
                ScopeDef::new("IMPORTANT", "重要"),
            ]),
            fail_next: Mutex::default(),
        }
    }

    pub fn push() -> Self {
        Self {
            mode: IngestMode::Push,
            ..Self::poll_every(std::time::Duration::from_secs(120))
        }
    }

    /// A `mail` event of `type_key` in `scope` with a payload `{text}`.
    pub fn event(type_key: &str, external_id: &str, scope: &str, summary: &str) -> RawEvent {
        let mut e = RawEvent::new("mail", type_key, external_id, steve_store::now(), summary);
        e.scope_ext_id = Some(scope.into());
        e.payload = json!({ "text": format!("原文：{summary}") });
        e
    }

    pub fn emit(&self, e: RawEvent) {
        lock(&self.feed).push(e);
    }
}

#[async_trait]
impl ConnectionProvider for FakeEventAdapter {
    fn id(&self) -> &str {
        Self::ID
    }
    fn system(&self) -> &str {
        "gmail"
    }
    fn name(&self) -> &str {
        "邮件"
    }
    fn caps(&self) -> &[&'static str] {
        &["mail.read", "events"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::None
    }
    async fn verify(&self, _: &ConnCx) -> Result<AccountInfo, ProviderError> {
        Ok(AccountInfo::new("fake@example.com", "Fake"))
    }
}

#[async_trait]
impl EventAdapter for FakeEventAdapter {
    fn source(&self) -> &'static str {
        "mail"
    }
    fn event_types(&self) -> &'static [EventTypeDef] {
        FAKE_TYPES
    }
    async fn scopes(&self, _: &ConnCx) -> Result<Vec<ScopeDef>, AdapterError> {
        Ok(lock(&self.scopes).clone())
    }
    fn mode(&self) -> IngestMode {
        self.mode
    }
    async fn poll(&self, _: &ConnCx, cursor: Option<&str>) -> Result<Batch, AdapterError> {
        if let Some(e) = lock(&self.fail_next).take() {
            return Err(e.into());
        }
        let feed = lock(&self.feed);
        // no cursor yet: start at the end (no backlog)
        let from = match cursor {
            Some(c) => c.parse::<usize>().unwrap_or(0).min(feed.len()),
            None => feed.len(),
        };
        Ok(Batch {
            events: feed[from..].to_vec(),
            cursor: Some(feed.len().to_string()),
        })
    }
    async fn parse_push(
        &self,
        _: &ConnCx,
        req: &PushRequest,
    ) -> Result<Vec<RawEvent>, AdapterError> {
        if req
            .headers
            .get("x-fake-signature")
            .and_then(|v| v.to_str().ok())
            != Some("ok")
        {
            return Err(AdapterError::BadSignature);
        }
        let body: Vec<Value> = serde_json::from_slice(&req.body)
            .map_err(|e| AdapterError::Malformed(e.to_string()))?;
        Ok(body
            .iter()
            .map(|b| {
                Self::event(
                    b["type"].as_str().unwrap_or("mail.new"),
                    b["id"].as_str().unwrap_or(""),
                    b["scope"].as_str().unwrap_or("INBOX"),
                    b["summary"].as_str().unwrap_or(""),
                )
            })
            .collect())
    }
}

/// A stand-in node kind (for `bot`, `script`, `http` until S3/S4): outputs `output`, or the value at
/// `output_from` when set, on `branch`; `effectful` with an optional grant. Counts its runs.
pub struct StubNode {
    pub kind: &'static str,
    pub effectful: bool,
    pub grant: Option<Grant>,
    pub output: Value,
    pub output_from: Option<&'static str>,
    pub branch: Mutex<Option<String>>,
    pub runs: Mutex<usize>,
}

impl StubNode {
    pub fn new(kind: &'static str, output: Value) -> Self {
        Self {
            kind,
            effectful: false,
            grant: None,
            output,
            output_from: None,
            branch: Mutex::default(),
            runs: Mutex::default(),
        }
    }

    pub fn effect(mut self, grant: Option<Grant>) -> Self {
        self.effectful = true;
        self.grant = grant;
        self
    }

    pub fn from_path(mut self, path: &'static str) -> Self {
        self.output_from = Some(path);
        self
    }

    pub fn set_branch(&self, b: Option<&str>) {
        *lock(&self.branch) = b.map(String::from);
    }

    pub fn runs(&self) -> usize {
        *lock(&self.runs)
    }
}

#[async_trait]
impl NodeKind for StubNode {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn effectful(&self, _: &Value) -> bool {
        self.effectful
    }
    fn grant(&self, _: &Value) -> Option<Grant> {
        self.grant.clone()
    }
    fn validate(&self, _: &Value) -> Result<(), String> {
        Ok(())
    }
    async fn run(&self, cx: &mut RunCx<'_>, _: &Value, input: &Value) -> NodeOutcome {
        *lock(&self.runs) += 1;
        let output = match self.output_from {
            Some(p) => cx.scope(input).get(p),
            None => self.output.clone(),
        };
        NodeOutcome::Next {
            output,
            branch: lock(&self.branch).clone(),
        }
    }
}

/// A stand-in outward action (`reply_as`-like): effectful, counts what it would have sent.
pub struct FakeSend {
    pub name: &'static str,
    pub sent: Mutex<Vec<Value>>,
}

impl FakeSend {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            sent: Mutex::default(),
        }
    }

    pub fn sent(&self) -> usize {
        lock(&self.sent).len()
    }
}

#[async_trait]
impl Action for FakeSend {
    fn name(&self) -> &'static str {
        self.name
    }
    fn effectful(&self, _: &Value) -> bool {
        true
    }
    fn validate(&self, _: &Value) -> Result<(), String> {
        Ok(())
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        lock(&self.sent).push(cx.render_value(cfg, input));
        cx.effects.outcome = Some("need".into());
        cx.effects.label = Some("回复已发出".into());
        NodeOutcome::next(json!({ "sent": true }))
    }
}

/// Explicit deterministic claims for tests. Production startup only opens recovery gates; the
/// three independent sources own timing and reuse these same source-specific claim functions.
pub fn claim_due(cx: &FlowCx) -> crate::FlowResult<Vec<String>> {
    let now = cx.now();
    let mut ids = Vec::new();
    for claims in [
        crate::scheduler::claim_triggers(cx, now),
        crate::engine::claim_waits_at(cx, now),
        crate::scheduler::claim_retries(cx, now),
    ] {
        let claims = claims?;
        if let Some(error) = claims.error {
            return Err(error);
        }
        ids.extend(claims.ids);
    }
    Ok(ids)
}

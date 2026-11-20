#[path = "../support/digest_env.rs"]
pub mod digest_env;
use async_trait::async_trait;
use digest_env::TestEnv;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use steve_channels::channel::{ProfileAdapter, ProfileAnswer, ProfileQuery};
use steve_channels::deliver::{Delivered, OutRequest, Outbound};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::flow_actions::plan_of;
use steve_channels::history::HistoryRead;
use steve_channels::respond::Plan;
use steve_channels::{Channels, InboundMessage, SpaceKind};
use steve_flow::{AdapterError, FlowSetup, LinearExecutor, testkit::FakeCore};
use steve_modules::accounts::connections::{self, ConnCx, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
pub const AT: i64 = 1_800_000_000_000;

pub struct History {
    pub id: String,
    pub messages: Mutex<Vec<InboundMessage>>,
    pub fail: AtomicBool,
    pub requests: Mutex<Vec<(String, i64, i64, usize)>>,
}
impl Default for History {
    fn default() -> Self {
        Self {
            id: FakeChannel::ID.into(),
            messages: Mutex::new(vec![]),
            fail: AtomicBool::new(false),
            requests: Mutex::new(vec![]),
        }
    }
}
#[async_trait]
impl HistoryRead for History {
    fn adapter_id(&self) -> &str {
        &self.id
    }
    async fn read(
        &self,
        _: &ConnCx,
        space: &str,
        from: i64,
        to: i64,
        limit: usize,
    ) -> Result<Vec<InboundMessage>, AdapterError> {
        self.requests
            .lock()
            .unwrap()
            .push((space.into(), from, to, limit));
        if self.fail.load(Ordering::SeqCst) {
            return Err(AdapterError::Malformed("history refused".into()));
        }
        Ok(self.messages.lock().unwrap().clone())
    }
}
#[derive(Default)]
pub struct Profiles {
    pub answer: Mutex<ProfileAnswer>,
    pub fail: AtomicBool,
    pub calls: AtomicUsize,
}
#[async_trait]
impl ProfileAdapter for Profiles {
    fn adapter_id(&self) -> &str {
        FakeChannel::ID
    }
    async fn resolve_profiles(
        &self,
        _: &ConnCx,
        _: &ProfileQuery,
    ) -> Result<ProfileAnswer, AdapterError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(AdapterError::Malformed("profiles refused".into()));
        }
        Ok(self.answer.lock().unwrap().clone())
    }
}
#[derive(Default)]
pub struct Spy {
    pub calls: Mutex<Vec<OutRequest>>,
    pub fail: AtomicBool,
}
#[async_trait]
impl Outbound for Spy {
    async fn deliver(
        &self,
        _: &Channels,
        req: &OutRequest,
    ) -> steve_channels::ChannelResult<Delivered> {
        self.calls.lock().unwrap().push(req.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(steve_modules::ApiError::rule("spy refused").into());
        }
        Ok(Delivered::Sent {
            message_id: "sg35-out".into(),
        })
    }
}
pub struct H {
    pub env: TestEnv,
    pub ch: Channels,
    pub conn: String,
    pub fake: Arc<FakeChannel>,
    pub core: Arc<FakeCore>,
    pub history: Arc<History>,
    pub profiles: Arc<Profiles>,
    pub _keys: tempfile::TempDir,
}
impl H {
    pub fn new() -> Self {
        let core = Arc::new(FakeCore::default());
        let flow = FlowSetup::new(core.clone());
        let env = TestEnv::new(|r| {
            flow.register(r);
            connections::register(r);
            steve_channels::space_decisions::register(r);
        });
        env.clock.set(AT);
        *core.store.lock().unwrap() = Some(env.store().clone());
        env.store()
            .bots()
            .seed(
                "steve",
                true,
                &steve_store::NewBot {
                    name: "Worker".into(),
                    harness: "sg35".into(),
                    model: Some("model-sg35".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        let keys = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        vault.init(WS, ME).unwrap();
        let fake = Arc::new(FakeChannel::new());
        let history = Arc::new(History::default());
        let profiles = Arc::new(Profiles::default());
        let conns = Connections::builder(
            env.modules.clone(),
            vault,
            ConnectionsConfig::new(vec![], true, |_| None),
        )
        .provider(fake.clone())
        .build();
        let flow = flow
            .connections(conns.clone())
            .build(env.modules.clone(), Arc::new(LinearExecutor));
        let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
            .flow(flow)
            .adapter(fake.clone())
            .history(history.clone())
            .profiles(profiles.clone())
            .build();
        let (conn, _) = FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).unwrap();
        env.store()
            .with_conn(|c| {
                c.execute("UPDATE connections SET status='ok' WHERE id=?", [&conn])?;
                Ok(())
            })
            .unwrap();
        Self {
            env,
            ch,
            conn,
            fake,
            core,
            history,
            profiles,
            _keys: keys,
        }
    }
    pub async fn plan(&self, space: &str, id: &str, text: &str) -> Plan {
        self.fake.space(space, "Room", SpaceKind::Group, false, &[]);
        self.env.clock.advance(1_000);
        let mut m = FakeChannel::message(space, id, "uguest", text);
        m.created = self.ch.now();
        let received = self.ch.on_message(WS, &self.conn, m).await.unwrap();
        self.env
            .store()
            .reader(|c| plan_of(c, WS, &received.message_id))
            .unwrap()
    }
    pub fn n(&self, sql: &str, args: &[&str]) -> i64 {
        self.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    sql,
                    rusqlite::params_from_iter(args),
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }
}

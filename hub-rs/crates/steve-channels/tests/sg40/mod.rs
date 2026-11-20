#[path = "../support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::Arc;
use steve_channels::fake::channel::FakeChannel;
use steve_channels::{Channels, Inbound, InboundMessage, SpaceKind, messages, spaces};
use steve_flow::{FlowSetup, LinearExecutor, testkit::FakeCore};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};

pub struct H {
    pub env: TestEnv,
    pub fake: Arc<FakeChannel>,
    pub ch: Channels,
    pub conn: String,
    pub own: String,
    _keys: tempfile::TempDir,
}
impl H {
    pub fn new() -> Self {
        let flow = FlowSetup::new(Arc::new(FakeCore::default()));
        let env = TestEnv::new(|r| {
            flow.register(r);
            connections::register(r);
        });
        let keys = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        let fake = Arc::new(FakeChannel::new());
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
        let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
            .adapter(fake.clone())
            .build();
        let (conn, _) = FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).unwrap();
        let (own, _) =
            FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("uowner")).unwrap();
        for (id, kind) in [("group", SpaceKind::Group), ("dm", SpaceKind::Dm)] {
            fake.space(
                id,
                "provider title",
                kind,
                false,
                &[("ualice", "Alice"), (FakeChannel::BOT, "Bot")],
            );
        }
        Self {
            env,
            fake,
            ch,
            conn,
            own,
            _keys: keys,
        }
    }
    pub fn msg(&self, space: &str, id: &str) -> InboundMessage {
        let mut m = FakeChannel::message(space, id, "ualice", "contract input");
        m.created = self.ch.now();
        m
    }
    pub async fn inbound(&self, m: InboundMessage) -> Inbound {
        self.ch.on_inbound(WS, &self.conn, m).await.unwrap()
    }
    pub fn row(&self, id: &str) -> messages::ChanMessage {
        self.env
            .store()
            .reader(|c| messages::get(c, id))
            .unwrap()
            .unwrap()
    }
    pub fn page(&self, space: &str) -> Vec<messages::ChanMessage> {
        self.env
            .store()
            .reader(|c| messages::page(c, space, ME, None, 100))
            .unwrap()
    }
    pub fn attention(&self, space: &str, level: spaces::Attention) {
        self.env
            .store()
            .transaction(|tx| spaces::write_attention(tx, WS, space, level, self.ch.now()))
            .unwrap();
    }
    pub fn n(&self, sql: &str) -> i64 {
        self.env
            .store()
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
}

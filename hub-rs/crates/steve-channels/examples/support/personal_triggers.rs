//! Shared isolated channel fixture. Only generated traffic, in-process adapters and model stubs.
use serde_json::{Value, json};
use std::sync::Arc;
use steve_channels::compose::H_SAFETY;
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::outbound::{OutLink, Outbox};
use steve_channels::respond::SUBJECT_MESSAGE;
use steve_channels::{Channels, Inbound, InboundMessage, Replies, SpaceKind, conv_record, spaces};
use steve_flow::{FlowSetup, LinearExecutor, testkit::FakeCore};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, caller};
use steve_modules::vault::{Vault, keystore::FileKeyStore};
use steve_modules::{ApiResult, Clock};
use steve_store::base::Viewer;

pub struct Fixture {
    pub env: TestEnv,
    _keys: tempfile::TempDir,
    pub ch: Channels,
    pub fake: Arc<FakeChannel>,
    #[allow(dead_code)] // The integration tests also exercise approval/queue handling directly.
    pub out: Arc<Outbox>,
    pub me: String,
    pub second: String,
    pub other: String,
    pub bot: String,
}

impl Fixture {
    pub async fn new() -> Self {
        let core = Arc::new(FakeCore::default());
        let flow_setup = FlowSetup::new(core.clone());
        let disclosure = Disclosure::with_models(Vec::new());
        let link = OutLink::new();
        let env = TestEnv::new(|r| {
            flow_setup.register(r);
            connections::register(r);
            r.outbound_check(Box::new(disclosure.clone()));
            r.decision(SUBJECT_MESSAGE, link.handler());
        });
        *core.store.lock().unwrap() = Some(env.store().clone());
        disclosure.bind(env.store().clone(), env.gateway.clone());
        link.bind_clock(env.clock.clone());
        let keys = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            env.store().clone(),
            Arc::new(FileKeyStore::open(keys.path()).unwrap()),
        );
        let fake = Arc::new(FakeChannel::new());
        let conns = Connections::builder(
            env.modules.clone(),
            vault,
            ConnectionsConfig::new(Vec::new(), true, |_| None),
        )
        .provider(fake.clone())
        .build();
        let out = Arc::new(Outbox::new(link));
        let replies = Arc::new(Replies::new().outbound(out.clone()));
        let flow = flow_setup
            .connections(conns.clone())
            .build(env.modules.clone(), Arc::new(LinearExecutor));
        let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
            .adapter(fake.clone())
            .responder(replies)
            .flow(flow)
            .build();
        let (me, _) =
            FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("owner-one")).unwrap();
        let (second, _) =
            FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("owner-two")).unwrap();
        let (other, _) =
            FakeChannel::seed_connection(env.store(), &caller(WS, OTHER), Some("other-member"))
                .unwrap();
        let (bot, _) = FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).unwrap();
        fake.space(
            "fixture-chat",
            "Fixture chat",
            SpaceKind::Group,
            false,
            &[("human", "Human")],
        );
        for conn in [&me, &second, &other, &bot] {
            env.store().transaction(|tx| -> ApiResult<()> {
                let (_, mode) = tx.write(|w| steve_channels::bindings::connection_account(w, WS, conn))?;
                tx.write(|w| spaces::ensure_channel_settings(w, WS, conn, mode, env.clock.now()))?;
                tx.execute("UPDATE channel_settings SET rules='[{\"when\":{\"kind\":\"all\"},\"act\":\"record\"}]' WHERE connection_id=?", [conn])?;
                Ok(())
            }).unwrap();
        }
        env.gateway
            .script(CHECK_HEAD, r#"{"verdict":"pass","spans":[]}"#);
        env.gateway.script(
            "你是一道隐私关卡",
            r#"{"verdict":"safe","why":"public fixture question"}"#,
        );
        let f = Self {
            env,
            _keys: keys,
            ch,
            fake,
            out,
            me,
            second,
            other,
            bot,
        };
        f.reply(false);
        f.configure(
            &f.me,
            json!({"on":true,"words":["/bot"],"where":["*"],"who":["me"],"mark":true}),
        );
        f
    }

    pub fn reply(&self, approval: bool) {
        self.env.gateway.unscript(H_SAFETY);
        self.env.gateway.script(
            H_SAFETY,
            &json!({"text":"Synthetic fixture answer.","needs_approval":approval,"why":"fixture"})
                .to_string(),
        );
    }

    pub fn configure(&self, conn: &str, value: Value) {
        self.env
            .store()
            .transaction(|tx| -> ApiResult<()> {
                tx.execute(
                    "UPDATE channel_settings SET trigger=?,updated=updated+1 WHERE connection_id=?",
                    rusqlite::params![value.to_string(), conn],
                )?;
                Ok(())
            })
            .unwrap();
    }

    pub fn message(&self, id: &str, from: &str, text: &str) -> InboundMessage {
        let mut m = FakeChannel::message("fixture-chat", id, from, text);
        m.created = self.ch.now();
        m
    }

    pub async fn receive(&self, conn: &str, m: InboundMessage) -> Inbound {
        self.ch.on_message(WS, conn, m).await.unwrap()
    }

    pub fn record(&self, space: &str) -> Value {
        conv_record::record(
            self.env.store(),
            WS,
            &Viewer::bare(WS, ME),
            &steve_channels::conversations::conv_space(space),
            &Default::default(),
        )
        .unwrap()
    }

    pub fn sends(&self) -> usize {
        self.fake.sent.lock().unwrap().len()
    }
}

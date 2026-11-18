//! Shared isolated channel fixture for presence regressions and manual HTTP acceptance.

pub(crate) use std::sync::atomic::{AtomicI64, Ordering};
pub(crate) use std::sync::{Arc, Mutex};

pub(crate) use async_trait::async_trait;
pub(crate) use axum::Router;
pub(crate) use serde_json::{Value, json};
pub(crate) use steve_channels::compose::{self, Composer};
pub(crate) use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
pub(crate) use steve_channels::fake::channel::FakeChannel;
pub(crate) use steve_channels::inbound::{LABEL_REPLY_OFF, LABEL_REPLY_RECORD};
pub(crate) use steve_channels::outbound::{OutLink, Outbox};
pub(crate) use steve_channels::presence::{self, BY_LATER, BY_YOU, LABEL_LIVE, LABEL_REPLIED};
pub(crate) use steve_channels::respond::SUBJECT_MESSAGE;
pub(crate) use steve_channels::{
    Channels, Inbound, InboundMessage, Mention, Replies, SpaceKind, api, spaces,
};
pub(crate) use steve_flow::nodes::action::Action;
pub(crate) use steve_flow::testkit::FakeCore;
pub(crate) use steve_flow::{FlowSetup, LinearExecutor, NodeOutcome, RunCx};
pub(crate) use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
pub(crate) use steve_modules::outbound::YOLO_ACTOR;
pub(crate) use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
pub(crate) use steve_modules::vault::Vault;
pub(crate) use steve_modules::vault::keystore::FileKeyStore;
pub(crate) use steve_modules::{ApiResult, FakeClock, GatewayOut};
pub(crate) use steve_store::base::Viewer;
pub(crate) use tempfile::TempDir;

pub(crate) const PASS: &str = r#"{"verdict": "pass", "spans": []}"#;
pub(crate) const REPLY: &str =
    r#"{"text": "好 我晚点看", "needs_approval": false, "why": "对方在等"}"#;

/// What happens elsewhere while the composer “thinks”.
pub(crate) type During = Box<dyn FnOnce() + Send>;

/// The composer's model: answers [`REPLY`], keeps the prompts, lets `advance` ms pass while it
/// “thinks” (drafting takes minutes in real life) and runs `during` (once) meanwhile.
pub(crate) struct Slow {
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) advance: AtomicI64,
    pub(crate) prompts: Mutex<Vec<String>>,
    pub(crate) during: Mutex<Option<During>>,
}

#[async_trait]
impl compose::PinnedModel for Slow {
    async fn run(
        &self,
        model: &str,
        prompt: &str,
        t: std::time::Duration,
    ) -> Result<GatewayOut, String> {
        self.run_with_images(model, prompt, &[], t).await
    }
    async fn run_with_images(
        &self,
        _: &str,
        prompt: &str,
        _: &[Value],
        _: std::time::Duration,
    ) -> Result<GatewayOut, String> {
        self.prompts.lock().unwrap().push(prompt.into());
        self.clock.advance(self.advance.load(Ordering::SeqCst));
        let during = self.during.lock().unwrap().take();
        if let Some(f) = during {
            f();
        }
        Ok(GatewayOut {
            text: REPLY.into(),
            ..Default::default()
        })
    }
}

/// C4's `reply_as`, so a space's rules turned into a flow validate (it never sends here).
pub(crate) struct StubReplyAs;

#[async_trait]
impl Action for StubReplyAs {
    fn name(&self) -> &'static str {
        "reply_as"
    }
    fn effectful(&self, _: &Value) -> bool {
        true
    }
    fn validate(&self, _: &Value) -> Result<(), String> {
        Ok(())
    }
    async fn run(&self, _: &mut RunCx<'_>, _: &Value, _: &Value) -> NodeOutcome {
        NodeOutcome::Fail("stub".into())
    }
}

pub(crate) struct H {
    pub(crate) env: TestEnv,
    pub(crate) _keys: TempDir,
    pub(crate) ch: Channels,
    pub(crate) fake: Arc<FakeChannel>,
    pub(crate) outbox: Arc<Outbox>,
    pub(crate) model: Arc<Slow>,
    pub(crate) api: Router,
    pub(crate) me: String,
    pub(crate) my_account: String,
    pub(crate) oncall: String,
}

pub(crate) async fn setup() -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let disclosure = Disclosure::with_models(Vec::new());
    let link = OutLink::new();
    let (reg_disclosure, reg_link) = (disclosure.clone(), link.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
        r.outbound_check(Box::new(reg_disclosure.clone()));
        r.decision(SUBJECT_MESSAGE, reg_link.handler());
    });
    disclosure.bind(env.store().clone(), env.gateway.clone());
    link.bind_clock(env.clock.clone());
    *core.store.lock().unwrap() = Some(env.store().clone());
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, role, rules, model, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '', '', '', 'fixture-model', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1);",
            )?;
            Ok(())
        })
        .expect("bots");
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    let fake = Arc::new(FakeChannel::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(fake.clone())
    .build();
    let outbox = Arc::new(Outbox::new(link));
    let model = Arc::new(Slow {
        clock: env.clock.clone(),
        advance: AtomicI64::new(0),
        prompts: Mutex::new(Vec::new()),
        during: Mutex::new(None),
    });
    let replies = Arc::new(
        Replies::new()
            .outbound(outbox.clone())
            .composer(Composer::new().pinned(model.clone())),
    );
    let flow = flow_setup
        .connections(conns.clone())
        .action(Arc::new(StubReplyAs))
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
        .adapter(fake.clone())
        .responder(replies)
        .flow(flow)
        .build();
    let api = api::routes(ch.clone()).with_state(env.modules.clone());
    let (me, my_account) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).expect("you");
    let (oncall, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("bot");
    let people = [("ulilei", "李雷"), ("uhan", "韩梅梅")];
    fake.space("C_DEV", "开发群", SpaceKind::Group, false, &people);
    fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
    fake.space("C_BOT", "值班群", SpaceKind::Group, false, &people);
    let h = H {
        env,
        _keys: keys,
        ch,
        fake,
        outbox,
        model,
        api,
        me,
        my_account,
        oncall,
    };
    h.env.gateway.script(CHECK_HEAD, PASS);
    let now = h.ch.now();
    let v = Viewer::bare(WS, ME);
    h.env
        .store()
        .transaction(|tx| {
            spaces::set_account_rules(
                tx,
                &v,
                &h.me,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
                now,
            )?;
            spaces::set_account_rules(
                tx,
                &v,
                &h.oncall,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}]),
                now,
            )
        })
        .expect("rules");
    h
}

/// A message `secs` from now (the fake clock).
pub(crate) fn msg(
    h: &H,
    space: &str,
    id: &str,
    from: &str,
    text: &str,
    secs: i64,
) -> InboundMessage {
    let mut m = FakeChannel::message(space, id, from, text);
    m.created = h.ch.now() + secs * 1000;
    m
}

pub(crate) fn at_you(mut m: InboundMessage) -> InboundMessage {
    m.mentions.push(Mention {
        handle: None,
        name: "你".into(),
        is_connection: true,
    });
    m
}

pub(crate) async fn push(h: &H, conn: &str, m: InboundMessage) -> Inbound {
    h.ch.on_message(WS, conn, m).await.expect("message")
}

pub(crate) fn row(h: &H, sql: &str, p: &[&str]) -> Vec<Value> {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let v = st.query_row(rusqlite::params_from_iter(p.iter()), |r| {
                (0..n)
                    .map(|i| {
                        Ok(match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(x) => json!(x),
                            rusqlite::types::ValueRef::Real(x) => json!(x),
                            rusqlite::types::ValueRef::Text(t) => {
                                json!(String::from_utf8_lossy(t))
                            }
                            rusqlite::types::ValueRef::Blob(_) => Value::Null,
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .expect("row")
}

pub(crate) fn count(h: &H, sql: &str, p: &[&str]) -> i64 {
    row(h, sql, p)[0].as_i64().expect("count")
}

pub(crate) fn drafts(h: &H) -> i64 {
    count(
        h,
        "SELECT count(*) FROM chan_messages WHERE direction = 'out' AND status = 'draft'",
        &[],
    )
}

pub(crate) fn decision(h: &H, id: &str) -> (String, Value) {
    let r = row(
        h,
        "SELECT status, result FROM decisions WHERE id = ?",
        &[id],
    );
    (
        r[0].as_str().unwrap().to_string(),
        serde_json::from_str(r[1].as_str().unwrap_or("null")).unwrap(),
    )
}

pub(crate) fn status(h: &H, id: &str) -> String {
    row(h, "SELECT status FROM chan_messages WHERE id = ?", &[id])[0]
        .as_str()
        .unwrap()
        .to_string()
}

/// Your own message stored by another way in (another device's connection), so nothing reacts to it
/// when it lands: only the later gates can see it.
pub(crate) fn you_said_elsewhere(h: &H, space_ext: &str, secs: i64) {
    your_line_in(h, space_ext)(h.ch.now() + secs * 1000);
}

/// [`you_said_elsewhere`] as a closure that stores your line with the time it is given (to run while
/// the composer “thinks”).
pub(crate) fn your_line_in(h: &H, space_ext: &str) -> impl Fn(i64) + Send + 'static {
    let space: String = row(
        h,
        "SELECT id FROM spaces WHERE external_id = ?",
        &[space_ext],
    )[0]
    .as_str()
    .unwrap()
    .into();
    let party: String = row(
        h,
        "SELECT party_id FROM accounts WHERE id = ?",
        &[&h.my_account],
    )[0]
    .as_str()
    .unwrap()
    .into();
    let (store, account) = (h.env.store().clone(), h.my_account.clone());
    move |now| {
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id,
                        sender_party_id, text, status, created)
                     VALUES (?, ?, ?, ?, 'in', ?, ?, '我来吧', 'received', ?)",
                    rusqlite::params![
                        steve_store::uid("cm"),
                        WS,
                        space,
                        format!("elsewhere_{now}"),
                        account,
                        party,
                        now
                    ],
                )?;
                Ok(())
            })
            .expect("your message");
    }
}

pub(crate) fn yolo(h: &H, d: &str, choice: &str) -> ApiResult<steve_store::Decision> {
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, YOLO_ACTOR), d, choice, "")
        .expect("ours")
}

pub(crate) fn you_decide(h: &H, d: &str, choice: &str) -> ApiResult<steve_store::Decision> {
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, ME), d, choice, "")
        .expect("ours")
}

/// A draft in your name for a private chat message, and its card, after its disclosure check (which
/// runs once the draft is written).
pub(crate) async fn drafted(h: &H, id: &str) -> (String, String) {
    let r = push(
        h,
        &h.me.clone(),
        msg(h, "D_LI", id, "ulilei", "今晚能看下吗", 0),
    )
    .await
    .response
    .expect("response");
    checked(h).await;
    (
        r.out_message_id.expect("draft"),
        r.decision_id.expect("card"),
    )
}

/// Runs the queue until no draft's disclosure check is still running.
pub(crate) async fn checked(h: &H) {
    for _ in 0..500 {
        let waiting: i64 = h
            .env
            .store()
            .reader(|c| {
                c.query_row(
                    "SELECT count(*) FROM chan_messages m JOIN reply_basis b ON b.id = m.basis_id
                      WHERE m.status = 'draft' AND b.verdict = 'checking'",
                    [],
                    |r| r.get(0),
                )
                .map_err(steve_store::StoreError::from)
            })
            .unwrap();
        if waiting == 0 {
            return;
        }
        h.outbox.send_due(&h.ch).await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("a disclosure check did not finish");
}

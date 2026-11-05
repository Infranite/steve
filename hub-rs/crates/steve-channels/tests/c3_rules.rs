//! C3 acceptance: the prototype's four accounts (you, 值班小运, 需求官,
//! 小助理) and six spaces evaluated rule by rule on the [`FakeChannel`] with a scripted gateway —
//! including a handoff that still reaches the inbox while muted and an `unconfirmed` contact that
//! does not count as a `member`; converting to a flow hands messages to the flow, converting back
//! restores the rules. Nothing is ever sent. Plus the cross-connection duplicate (F8 gap), rule
//! validation and the REST routes.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::inbound::{LABEL_ELSEWHERE, LABEL_OWN};
use steve_channels::respond::SUBJECT_MESSAGE;
use steve_channels::rules::{self, Act, INTENT_START, SELF_NO_REPLY};
use steve_channels::{
    By, Channels, Inbound, InboundMessage, Mention, Next, Plan, Responder, SpaceKind, api,
    bindings, spaces,
};
use steve_flow::nodes::action::Action;
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowCx, FlowSetup, LinearExecutor, NodeOutcome, RunCx};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::accounts::repo::{self as arepo, NewAccount};
use steve_modules::accounts::service::ensure_seeds;
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::base::Viewer;
use steve_store::uid;
use tempfile::TempDir;

/// Records handoff acknowledgements; takes nothing else (C3's fallback applies).
#[derive(Default)]
struct Rec {
    acks: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Responder for Rec {
    async fn acknowledge(
        &self,
        _: &Channels,
        plan: &Plan,
        text: &str,
    ) -> steve_channels::ChannelResult<bool> {
        self.acks
            .lock()
            .unwrap()
            .push((plan.space_id.clone(), text.into()));
        Ok(false)
    }
}

/// C4's `reply_as`, so compiled graphs validate (never run here: nothing is sent).
struct StubReplyAs;

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

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    rec: Arc<Rec>,
    api: Router,
    flow: Arc<FlowCx>,
    /// Connections: you, 值班小运, 需求官, 小助理.
    me: String,
    oncall: String,
    pm: String,
    help: String,
}

/// Another bot account of the own organization with its connection.
fn bot_connection(h: &TestEnv, handle: &str, name: &str) -> String {
    let c = caller(WS, ME);
    h.store()
        .transaction(|tx| -> steve_modules::ApiResult<String> {
            ensure_seeds(tx, WS)?;
            steve_modules::contacts::service::ensure_base(tx, &c)?;
            let org = steve_modules::contacts::repo::self_org(tx, WS)?.expect("self org");
            let a = arepo::insert(
                tx,
                WS,
                &NewAccount {
                    system_id: FakeChannel::SYSTEM.into(),
                    family: "slack_user".into(),
                    party_id: org.id,
                    kind: "app_bot".into(),
                    handle: handle.into(),
                    handle_display: handle.into(),
                    display_name: name.into(),
                    verified: true,
                    confidence: 1.0,
                    source: "connect".into(),
                    source_label: String::new(),
                },
            )?;
            let con = uid("con");
            tx.execute(
                "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
                 VALUES (?, ?, ?, ?, 'none', '[\"msg.receive\",\"msg.send\",\"msg.send_as_user\"]', ?, 1, 1)",
                rusqlite::params![con, WS, a.id, FakeChannel::ID, ME],
            )?;
            Ok(con)
        })
        .expect("bot connection")
}

fn viewer() -> Viewer {
    Viewer::bare(WS, ME)
}

fn set_rules(h: &H, conn: &str, rules: Value) {
    let now = h.ch.now();
    h.env
        .store()
        .transaction(|tx| spaces::set_account_rules(tx, &viewer(), conn, &rules, now))
        .expect("rules");
}

async fn new_persona(h: &H, name: &str, bot: &str) -> String {
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "POST",
        "/personas",
        Some(&json!({"name": name, "bot_id": bot})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    v["id"].as_str().expect("id").to_string()
}

async fn setup() -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '', 1), ('ops', 'default', '运维', 0, 'claude', '', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1), ('ops', 'default', 2);",
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
    let flow = flow_setup
        .connections(conns.clone())
        .action(Arc::new(StubReplyAs))
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let rec = Arc::new(Rec::default());
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
        .adapter(fake.clone())
        .responder(rec.clone())
        .flow(flow.clone())
        .build();
    let api = api::routes(ch.clone()).with_state(env.modules.clone());

    let (me, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).expect("you");
    let oncall = bot_connection(&env, "uoncall", "值班小运");
    let pm = bot_connection(&env, "upm", "需求官");
    let help = bot_connection(&env, "uhelp", "小助理");

    // the six spaces of the prototype
    let people = [("ulilei", "李雷"), ("uhan", "韩梅梅"), ("uwang", "王芳")];
    fake.space("C_DEV", "Steve 开发群", SpaceKind::Group, false, &people);
    fake.space("C_MEDM", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
    fake.space("C_OPS", "运维群", SpaceKind::Group, false, &people);
    fake.space("C_REQ", "产品需求群", SpaceKind::Group, false, &people);
    fake.space(
        "C_ACME",
        "Acme 客户群",
        SpaceKind::Group,
        true,
        &[("uwang", "王芳"), ("usarah", "Sarah")],
    );
    fake.space("C_HELP", "", SpaceKind::Dm, false, &[("uwang", "王芳")]);

    let h = H {
        env,
        _keys: keys,
        ch,
        rec,
        api,
        flow,
        me,
        oncall,
        pm,
        help,
    };
    // personas (C2) for the bots
    for (conn, name, bot) in [
        (&h.oncall, "值班小运", "ops"),
        (&h.pm, "需求官", "steve"),
        (&h.help, "小助理", "steve"),
    ] {
        let p = new_persona(&h, name, bot).await;
        let now = h.ch.now();
        h.env
            .store()
            .transaction(|tx| bindings::set_account_default(tx, &viewer(), conn, &p, now))
            .expect("bind");
    }
    // the accounts' rules (channel-data.ts)
    set_rules(
        &h,
        &h.me.clone(),
        json!([
            {"when": {"kind": "mention_or_dm"}, "act": "draft"},
            {"when": {"kind": "other_group"}, "act": "record"}
        ]),
    );
    set_rules(
        &h,
        &h.oncall.clone(),
        json!([
            {"when": {"kind": "mention_or_dm"}, "act": "reply"},
            {"when": {"kind": "keyword", "words": ["告警", "部署"]}, "act": "reply"},
            {"when": {"kind": "intent", "text": "需要动手操作"}, "act": "create_item", "mode": "start"}
        ]),
    );
    set_rules(
        &h,
        &h.pm.clone(),
        json!([
            {"when": {"kind": "mention"}, "act": "reply"},
            {"when": {"kind": "intent", "text": "是需求"}, "act": "create_item", "mode": "propose"}
        ]),
    );
    set_rules(
        &h,
        &h.help.clone(),
        json!([{"when": {"kind": "dm"}, "act": "reply"}]),
    );
    // the gateway: a fixed judgement for “是需求”; “需要动手操作” is answered per test
    h.env.gateway.script(
        format!("{INTENT_START}\n条件：是需求"),
        "{\"match\": false}",
    );
    h
}

fn rows(h: &H, sql: &str, p: &[&str]) -> Vec<Vec<Value>> {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let out = st
                .query_map(rusqlite::params_from_iter(p.iter()), |r| {
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
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(out)
        })
        .expect("rows")
}

fn count(h: &H, sql: &str, p: &[&str]) -> i64 {
    rows(h, sql, p)[0][0].as_i64().expect("count")
}

/// A message from `from` in `space`; `at` = @-mentions the receiving account.
fn msg(space: &str, id: &str, from: &str, text: &str, at: bool) -> InboundMessage {
    let mut m = FakeChannel::message(space, id, from, text);
    if at {
        m.mentions.push(Mention {
            handle: None,
            name: "你".into(),
            is_connection: true,
        });
    }
    m
}

async fn push(h: &H, conn: &str, m: InboundMessage) -> Inbound {
    h.ch.on_message(WS, conn, m).await.expect("message")
}

/// The decision of the response, `(kind, subject_type, question, options)`.
fn decision(h: &H, id: &str) -> Vec<Value> {
    rows(
        h,
        "SELECT kind, subject_type, question, options, draft FROM decisions WHERE id = ?",
        &[id],
    )
    .remove(0)
}

/// Asserts the rule and action of a response; returns it.
fn expect(i: &Inbound, rule: Option<usize>, act: Act) -> steve_channels::Response {
    let r = i.response.clone().expect("response");
    assert_eq!((r.rule, r.act), (rule, act), "{r:?}");
    r
}

/// A message not addressed to the account (no @, not a private chat): only recorded by the gate, before
/// any flow or rule, and no model is asked.
fn unaddressed(h: &H, i: &Inbound, calls_before: usize) {
    let r = expect(i, None, Act::Record);
    assert_eq!(r.label, steve_channels::rules::LABEL_UNADDRESSED, "{r:?}");
    assert!(
        r.decision_id.is_none() && r.out_message_id.is_none(),
        "{r:?}"
    );
    assert_eq!(
        h.env.gateway.calls.lock().unwrap().len(),
        calls_before,
        "no model call"
    );
}

fn calls(h: &H) -> usize {
    h.env.gateway.calls.lock().unwrap().len()
}

fn nothing_sent(h: &H) {
    assert_eq!(
        count(
            h,
            "SELECT count(*) FROM chan_messages WHERE direction = 'out' AND status = 'sent'",
            &[]
        ),
        0,
        "rules never send"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn your_account_drafts_records_and_takes_trigger_words() {
    let h = setup().await;
    let me = h.me.clone();

    // Steve 开发群: @你 → draft, as you
    let i = push(
        &h,
        &me,
        msg(
            "C_DEV",
            "m1",
            "ulilei",
            "@Alice 会话搜索什么时候能用？",
            true,
        ),
    )
    .await;
    assert_eq!(i.next, Next::Handle);
    let r = expect(&i, Some(0), Act::Draft);
    assert_eq!(r.by, By::Rules);
    let out = r.out_message_id.clone().expect("draft");
    let d = decision(&h, r.decision_id.as_deref().expect("decision"));
    assert_eq!(d[0], json!("approve"));
    assert_eq!(d[1], json!(SUBJECT_MESSAGE));
    assert_eq!(d[2], json!("以你本人发到「Steve 开发群」"));
    let m = rows(
        &h,
        "SELECT direction, status, on_behalf, persona_id, reply_to, decision_id FROM chan_messages WHERE id = ?",
        &[&out],
    )
    .remove(0);
    assert_eq!(
        m,
        vec![
            json!("out"),
            json!("draft"),
            json!("none"),
            Value::Null,
            json!("m1"),
            json!(r.decision_id)
        ]
    );
    // the signal says so
    let e = rows(
        &h,
        "SELECT e.outcome, e.outcome_label, e.decision_id, e.reason FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?",
        &[&i.message_id],
    )
    .remove(0);
    assert_eq!(
        e,
        vec![
            json!("need"),
            json!("等你确认回复"),
            json!(r.decision_id),
            json!("响应规则第 1 条")
        ]
    );

    // other group messages → only recorded, by the gate
    let n = calls(&h);
    let i = push(&h, &me, msg("C_DEV", "m2", "uhan", "我先下线了", false)).await;
    unaddressed(&h, &i, n);

    // a private chat → draft
    let i = push(&h, &me, msg("C_MEDM", "m3", "ulilei", "在吗", false)).await;
    expect(&i, Some(0), Act::Draft);

    // your own message: not processed …
    let i = push(&h, &me, msg("C_DEV", "m4", "upx", "我看一下", false)).await;
    assert_eq!(i.next, Next::Own);
    let r = expect(&i, None, Act::Record);
    assert_eq!((r.by, r.label.as_str()), (By::Skipped, LABEL_OWN));

    // … unless it starts with a trigger word where they work (handed on; nothing sent here)
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
                rusqlite::params![
                    json!({"on": true, "words": ["/bot", "/steve"], "where": [i.space_id, "dm:*"], "mark": true}).to_string(),
                    me
                ],
            )?;
            Ok(())
        })
        .expect("trigger");
    let i = push(&h, &me, msg("C_DEV", "m5", "upx", "/bot 回复他进度", false)).await;
    let r = i.response.clone().expect("response");
    assert_eq!(
        r.by,
        By::Trigger {
            instruction: "回复他进度".into()
        }
    );
    assert_eq!(r.label, "触发语未接入，只记录");
    // someone else's trigger word is just a message
    let n = calls(&h);
    let i = push(&h, &me, msg("C_DEV", "m6", "uhan", "/bot 帮我发", false)).await;
    unaddressed(&h, &i, n);

    // your own account never replies by itself
    let err = h
        .env
        .store()
        .transaction(|tx| {
            spaces::set_account_rules(
                tx,
                &viewer(),
                &me,
                &json!([{"when": {"kind": "dm"}, "act": "reply"}]),
                1,
            )
        })
        .expect_err("self reply");
    assert_eq!(err.to_string(), SELF_NO_REPLY);
    nothing_sent(&h);
}

#[tokio::test(flavor = "multi_thread")]
async fn oncall_replies_on_mention_and_records_the_rest() {
    let h = setup().await;
    let c = h.oncall.clone();

    // @值班小运 → reply (until C6: a draft as the persona)
    let i = push(
        &h,
        &c,
        msg(
            "C_OPS",
            "o1",
            "uhan",
            "@值班小运 昨晚备份是不是又挂了？",
            true,
        ),
    )
    .await;
    let r = i.response.clone().expect("response");
    assert_eq!(r.rule, Some(0));
    assert_eq!(r.act, Act::Draft, "reply without a sender drafts: {r:?}");
    let d = decision(&h, r.decision_id.as_deref().expect("decision"));
    assert_eq!(d[2], json!("以值班小运发到「运维群」"));
    let p = rows(
        &h,
        "SELECT m.on_behalf, p.name FROM chan_messages m JOIN personas p ON p.id = m.persona_id WHERE m.id = ?",
        &[r.out_message_id.as_deref().expect("out")],
    )
    .remove(0);
    assert_eq!(p, vec![json!("persona"), json!("值班小运")]);

    // keyword and intent rules never see a message that does not address the account: recorded by the
    // gate, and the intent model is not asked
    for (id, text) in [
        ("o2", "今晚部署吗"),
        ("o3", "db-2 磁盘满了，帮忙清一下"),
        ("o4", "今天天气不错"),
    ] {
        let n = calls(&h);
        let i = push(&h, &c, msg("C_OPS", id, "uwang", text, false)).await;
        unaddressed(&h, &i, n);
    }
    nothing_sent(&h);
}

#[tokio::test(flavor = "multi_thread")]
async fn pm_handoff_reaches_the_inbox_while_muted() {
    let h = setup().await;
    let c = h.pm.clone();

    // 产品需求群: @需求官 → reply; a plain idea does not address it → recorded, no intent judgement
    let i = push(
        &h,
        &c,
        msg(
            "C_REQ",
            "p1",
            "uwang",
            "@需求官 手机端能不能语音派活？",
            true,
        ),
    )
    .await;
    assert_eq!(i.response.clone().expect("r").rule, Some(0));
    let n = calls(&h);
    let i = push(&h, &c, msg("C_REQ", "p2", "uwang", "哈哈今天好热", false)).await;
    unaddressed(&h, &i, n);

    // Acme 客户群: muted, its own rules: 被 @ → 转给你, 全部消息 → 只记录 (the prototype lists the
    // two the other way round, where first-match would never reach the handoff)
    let i = push(&h, &c, msg("C_ACME", "p3", "usarah", "hello", false)).await;
    let space = i.space_id.clone();
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"muted": true, "rules": [
            {"when": {"kind": "mention"}, "act": "handoff"},
            {"when": {"kind": "all"}, "act": "record"}
        ]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["rules"][0]["act"], json!("handoff"));
    assert_eq!(v["effective_rules"], v["rules"]);

    let i = push(
        &h,
        &c,
        msg(
            "C_ACME",
            "p4",
            "usarah",
            "@需求官 Can we get the report today?",
            true,
        ),
    )
    .await;
    let r = expect(&i, Some(0), Act::Handoff);
    assert!(!r.muted, "a handoff is not muted");
    let d = decision(&h, r.decision_id.as_deref().expect("decision"));
    assert_eq!(d[0], json!("ask"));
    assert_eq!(
        d[2],
        json!("「Acme 客户群」里 Sarah 找你：@需求官 Can we get the report today?")
    );
    assert_eq!(
        rows(
            &h,
            "SELECT decision_id FROM chan_messages WHERE id = ?",
            &[&i.message_id]
        )[0][0],
        json!(r.decision_id)
    );
    assert!(h.rec.acks.lock().unwrap().is_empty(), "muted: says nothing");

    // plain messages there: recorded
    let n = calls(&h);
    let i = push(&h, &c, msg("C_ACME", "p5", "usarah", "thanks", false)).await;
    unaddressed(&h, &i, n);

    // muted turns a reply into record
    let (s, _) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"rules": [{"when": {"kind": "mention"}, "act": "reply"}]})),
    )
    .await;
    assert_eq!(s, 200);
    let i = push(&h, &c, msg("C_ACME", "p6", "usarah", "@需求官 ping", true)).await;
    let r = expect(&i, Some(0), Act::Record);
    assert!(r.muted);

    // not muted: the handoff asks the responder to say “我请 … 来看。”
    let (s, _) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"muted": false, "rules": [{"when": {"kind": "mention"}, "act": "handoff"}]})),
    )
    .await;
    assert_eq!(s, 200);
    let i = push(&h, &c, msg("C_ACME", "p7", "usarah", "@需求官 again", true)).await;
    expect(&i, Some(0), Act::Handoff);
    let acks = h.rec.acks.lock().unwrap().clone();
    assert_eq!(acks.len(), 1);
    assert!(
        acks[0].1.starts_with("我请 ") && acks[0].1.ends_with(" 来看。"),
        "{acks:?}"
    );

    // back to following the account
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"rules": null})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v["rules"], Value::Null);
    assert_eq!(v["effective_rules"][1]["mode"], json!("propose"));
    nothing_sent(&h);
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_replies_in_private_chats_only() {
    let h = setup().await;
    let c = h.help.clone();
    let i = push(
        &h,
        &c,
        msg("C_HELP", "h1", "uwang", "报销流程在哪？", false),
    )
    .await;
    let r = expect(&i, Some(0), Act::Draft);
    assert_eq!(
        decision(&h, r.decision_id.as_deref().expect("d"))[2],
        json!("以小助理发到「王芳」")
    );
    // the same bot in a group (pulled into one): no rule → record
    let i = push(&h, &c, msg("C_REQ", "h2", "uwang", "@小助理 在吗", true)).await;
    expect(&i, None, Act::Record);
    nothing_sent(&h);
}

#[tokio::test(flavor = "multi_thread")]
async fn unconfirmed_contacts_are_not_members() {
    let h = setup().await;
    let c = h.oncall.clone();
    let first = push(&h, &c, msg("C_OPS", "u1", "uhan", "大家好", false)).await;
    let space = first.space_id.clone();
    let party = first.sender_party_id.clone().expect("party");
    assert_eq!(
        rows(
            &h,
            "SELECT contact_kind FROM parties WHERE id = ?",
            &[&party]
        )[0][0],
        json!("unconfirmed")
    );
    let team = uid("tm");
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES (?, ?, 'team', '运维组', ?, 1, 1)",
                rusqlite::params![team, WS, ME],
            )?;
            Ok(())
        })
        .expect("team");
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"rules": [
            {"when": {"kind": "member", "parties": [party]}, "act": "handoff"},
            {"when": {"kind": "member", "teams": [team]}, "act": "draft"},
            {"when": {"kind": "all"}, "act": "record"}
        ]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");

    // conditions refine messages addressed to the account; 韩梅梅 is still unconfirmed: not a member
    let i = push(
        &h,
        &c,
        msg("C_OPS", "u2", "uhan", "@值班小运 看下这个", true),
    )
    .await;
    expect(&i, Some(2), Act::Record);

    // confirmed: the first rule
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE parties SET contact_kind = 'real' WHERE id = ?",
                [&party],
            )?;
            Ok(())
        })
        .expect("confirm");
    let i = push(
        &h,
        &c,
        msg("C_OPS", "u3", "uhan", "@值班小运 看下这个", true),
    )
    .await;
    expect(&i, Some(0), Act::Handoff);
    // without the @ the same member is only recorded
    let n = calls(&h);
    let i = push(&h, &c, msg("C_OPS", "u3b", "uhan", "看下这个", false)).await;
    unaddressed(&h, &i, n);

    // a confirmed team member
    let other = push(&h, &c, msg("C_OPS", "u4", "uwang", "你好", false)).await;
    let wang = other.sender_party_id.clone().expect("wang");
    h.env
        .store()
        .with_conn(|c| {
            c.execute_batch(&format!(
                "UPDATE parties SET contact_kind = 'real' WHERE id = '{wang}';
                 INSERT INTO team_members (workspace_id, team_id, contact_id, joined) VALUES ('{WS}', '{team}', '{wang}', 1);"
            ))?;
            Ok(())
        })
        .expect("member");
    let i = push(
        &h,
        &c,
        msg("C_OPS", "u5", "uwang", "@值班小运 部署好了", true),
    )
    .await;
    expect(&i, Some(1), Act::Draft);

    // references are checked
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"rules": [{"when": {"kind": "member", "parties": ["ct_nope"]}, "act": "draft"}]})),
    )
    .await;
    assert_eq!((s, v), (404, json!({"error": "没有这个联系人"})));
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_to_the_bot_and_rule_validation_over_rest() {
    let h = setup().await;
    let c = h.oncall.clone();
    let first = push(&h, &c, msg("C_OPS", "r1", "uhan", "早", false)).await;
    let space = first.space_id.clone();
    // a message the bot sent earlier
    h.env
        .store()
        .with_conn(|cn| {
            cn.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, status, created, sent)
                 VALUES ('cm_sent1', ?, ?, 'fm_1', 'out', '备份已恢复', 'sent', 1, 1)",
                rusqlite::params![WS, space],
            )?;
            Ok(())
        })
        .expect("sent");
    let (s, _) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"rules": [{"when": {"kind": "reply_to_me"}, "act": "draft"}]})),
    )
    .await;
    assert_eq!(s, 200);
    // a reply to the bot's message that does not @ it is not addressed to it: recorded
    let mut m = msg("C_OPS", "r2", "uhan", "谢谢", false);
    m.reply_to = Some("fm_1".into());
    let n = calls(&h);
    let i = push(&h, &c, m).await;
    assert!(i.facts.as_ref().expect("facts").reply_to_me);
    unaddressed(&h, &i, n);
    // with the @ the reply_to_me rule decides
    let mut m = msg("C_OPS", "r2b", "uhan", "@值班小运 谢谢", true);
    m.reply_to = Some("fm_1".into());
    let i = push(&h, &c, m).await;
    expect(&i, Some(0), Act::Draft);
    let i = push(&h, &c, msg("C_OPS", "r3", "uhan", "谢谢", false)).await;
    expect(&i, None, Act::Record);

    for (body, want) in [
        (
            json!({"rules": [{"when": {"kind": "sometimes"}, "act": "reply"}]}),
            "参数不对：rules.0.when.kind",
        ),
        (
            json!({"rules": [{"when": {"kind": "all"}, "act": "create_item", "mode": "later"}]}),
            "参数不对：rules.0.mode",
        ),
        (json!({"rules": "all"}), "参数不对：rules"),
    ] {
        let (s, v) = call(
            &h.api,
            Some(&caller(WS, ME)),
            "PATCH",
            &format!("/spaces/{space}"),
            Some(&body),
        )
        .await;
        assert_eq!((s, v["error"].as_str()), (400, Some(want)), "{body}");
    }
    // a space of your own account cannot reply either
    let mine = push(&h, &h.me.clone(), msg("C_DEV", "r4", "uhan", "hi", false)).await;
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{}", mine.space_id),
        Some(&json!({"rules": [{"when": {"kind": "all"}, "act": "reply"}]})),
    )
    .await;
    assert_eq!((s, v), (400, json!({"error": SELF_NO_REPLY})));
}

#[tokio::test(flavor = "multi_thread")]
async fn to_flow_hands_messages_to_the_flow_and_back_restores_the_rules() {
    let h = setup().await;
    let c = h.oncall.clone();
    let first = push(&h, &c, msg("C_OPS", "f1", "uhan", "早", false)).await;
    let space = first.space_id.clone();
    let me = caller(WS, ME);

    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &format!("/spaces/{space}/rules/to-flow"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let flow_id = v["flow_id"].as_str().expect("flow_id").to_string();
    let f = rows(
        &h,
        "SELECT kind, enabled, name, origin_label, graph FROM flows WHERE id = ?",
        &[&flow_id],
    )
    .remove(0);
    assert_eq!(f[0], json!(rules::FLOW_KIND));
    assert_eq!(f[1], json!(1));
    assert_eq!(f[2], json!("「运维群」的响应规则"));
    assert_eq!(f[3], json!(rules::FLOW_ORIGIN_LABEL));
    let graph: Value = serde_json::from_str(f[4].as_str().expect("graph")).expect("json");
    // valid against the registered node kinds
    steve_flow::graph::validate(&graph, &h.flow.nodes).expect("valid graph");
    let kinds: Vec<&str> = graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|n| n["kind"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        kinds,
        ["trigger", "filter", "action", "action", "bot", "action"]
    );
    let t = rows(
        &h,
        "SELECT kind, spaces, enabled FROM flow_triggers WHERE flow_id = ?",
        &[&flow_id],
    );
    assert_eq!(
        t,
        vec![vec![
            json!("message"),
            json!(json!([space]).to_string()),
            json!(1)
        ]]
    );
    // the rule form now shows the flow; converting twice and editing rules are refused
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &format!("/spaces/{space}/rules/to-flow"),
        None,
    )
    .await;
    assert_eq!(s, 409, "{v}");
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"rules": [{"when": {"kind": "all"}, "act": "record"}]})),
    )
    .await;
    assert_eq!(
        (s, v),
        (
            409,
            json!({"error": "这个空间已经转成流程「「运维群」的响应规则」，先改回规则"})
        )
    );

    // a mention: the flow takes it (its trigger opened a run), the rules do not run
    let drafts = |h: &H| {
        count(
            h,
            "SELECT count(*) FROM chan_messages WHERE space_id = ? AND direction = 'out'",
            &[&space],
        )
    };
    let before = drafts(&h);
    let i = push(&h, &c, msg("C_OPS", "f2", "uhan", "@值班小运 在吗", true)).await;
    let r = i.response.clone().expect("r");
    assert_eq!(
        r.by,
        By::Flow {
            flow_id: flow_id.clone()
        }
    );
    assert_eq!(drafts(&h), before, "no draft from the rules");
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM flow_runs r JOIN chan_messages m ON m.event_id = r.event_id WHERE r.flow_id = ? AND m.id = ?",
            &[&flow_id, &i.message_id]
        ),
        1,
        "the flow got the message"
    );
    let ev = rows(
        &h,
        "SELECT e.payload FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?",
        &[&i.message_id],
    );
    let payload: Value = serde_json::from_str(ev[0][0].as_str().expect("p")).expect("payload");
    assert_eq!(payload["facts"]["mentioned"], json!(true));
    // a message that does not address the account starts no run of the space's flow
    let n = calls(&h);
    let i = push(&h, &c, msg("C_OPS", "f2b", "uhan", "大家早", false)).await;
    unaddressed(&h, &i, n);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM flow_runs r JOIN chan_messages m ON m.event_id = r.event_id WHERE m.id = ?",
            &[&i.message_id]
        ),
        0,
        "no flow run for an unaddressed message"
    );
    assert_eq!(
        rows(
            &h,
            "SELECT e.outcome, e.outcome_label FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?",
            &[&i.message_id]
        )[0],
        vec![
            json!("recorded"),
            json!(steve_channels::rules::LABEL_UNADDRESSED)
        ]
    );

    // back to the rules: the flow is disabled (kept), the rules decide again
    let (s, v) = call(
        &h.api,
        Some(&me),
        "DELETE",
        &format!("/spaces/{space}/flow"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["flow_id"], Value::Null);
    assert_eq!(
        rows(
            &h,
            "SELECT enabled, deleted FROM flows WHERE id = ?",
            &[&flow_id]
        )[0],
        vec![json!(0), json!(0)]
    );
    let i = push(&h, &c, msg("C_OPS", "f3", "uhan", "@值班小运 在吗", true)).await;
    let r = expect(&i, Some(0), Act::Draft);
    assert_eq!(r.by, By::Rules);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM flow_runs r JOIN chan_messages m ON m.event_id = r.event_id WHERE m.id = ?",
            &[&i.message_id]
        ),
        0
    );
    // revisions of both changes
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM audit WHERE target = ? AND action IN ('space.to_flow', 'space.flow_off')",
            &[&space]
        ),
        2
    );
    // a second conversion makes a new flow
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &format!("/spaces/{space}/rules/to-flow"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_ne!(v["flow_id"], json!(flow_id));
    nothing_sent(&h);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_seen_through_both_connections_is_handled_once() {
    let h = setup().await;
    let (me, bot) = (h.me.clone(), h.oncall.clone());

    // your poller reads it first (you are @-mentioned): handled by your rules
    let mut m = msg("C_OPS", "x1", "uhan", "@Alice @值班小运 看下告警", true);
    let first = push(&h, &me, m.clone()).await;
    expect(&first, Some(0), Act::Draft);
    // then the bot's long connection: a different space of another connection, same chat and id
    m.thread_key = Some("th_x1".into());
    let again = push(&h, &bot, m.clone()).await;
    assert!(again.duplicate, "{again:?}");
    assert_eq!(again.next, Next::Record(LABEL_ELSEWHERE));
    assert!(again.response.is_none(), "no rules");
    assert_eq!(again.message_id, first.message_id);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_messages WHERE external_id = 'x1'",
            &[]
        ),
        1
    );
    // only fills in what the first copy lacked
    assert_eq!(
        rows(
            &h,
            "SELECT thread_key FROM chan_messages WHERE id = ?",
            &[&first.message_id]
        )[0][0],
        json!("th_x1")
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM ext_events WHERE external_id = 'x1'",
            &[]
        ),
        1,
        "one signal"
    );

    // the other way round: the bot first, then your poller
    let m = msg("C_OPS", "x2", "uhan", "@值班小运 部署呢", true);
    let first = push(&h, &bot, m.clone()).await;
    expect(&first, Some(0), Act::Draft);
    let again = push(&h, &me, m).await;
    assert!(again.duplicate && again.response.is_none());

    // two bots in one group each answer for themselves
    let m = msg("C_OPS", "x3", "uhan", "@大家 部署好了吗", true);
    let a = push(&h, &bot, m.clone()).await;
    let b = push(&h, &h.pm.clone(), m).await;
    assert!(!a.duplicate && !b.duplicate);
    assert!(a.response.is_some() && b.response.is_some());
    nothing_sent(&h);
}

#[tokio::test(flavor = "multi_thread")]
async fn default_rules_of_a_new_own_account() {
    let h = setup().await;
    // a second personal account without settings yet: the defaults
    let (con, _) = FakeChannel::seed_connection(h.env.store(), &caller(WS, ME), Some("upx2"))
        .expect("you again");
    let i = push(&h, &con, msg("C_MEDM", "d1", "ulilei", "在吗", false)).await;
    expect(&i, Some(0), Act::Draft);
    let i = push(&h, &con, msg("C_DEV", "d2", "ulilei", "@你 看下", true)).await;
    expect(&i, Some(1), Act::Draft);
    let n = calls(&h);
    let i = push(&h, &con, msg("C_DEV", "d3", "ulilei", "闲聊", false)).await;
    unaddressed(&h, &i, n);
    let s = h
        .env
        .store()
        .reader(|c| spaces::channel_settings(c, WS, &con))
        .expect("settings")
        .expect("row");
    assert_eq!(
        rules::load(&s.rules, s.mode)
            .iter()
            .map(|r| r.act)
            .collect::<Vec<_>>(),
        [Act::Draft, Act::Draft, Act::Record]
    );
}

/// Without the grant to send as you (some tenants don't let members request
/// `im:message.send_as_user`), nothing is drafted or sent on your account: the rules that would
/// draft only record, and the space says why.
#[tokio::test]
async fn without_send_as_you_your_account_only_records() {
    let h = setup().await;
    let (con, _) = FakeChannel::seed_connection(h.env.store(), &caller(WS, ME), Some("upx3"))
        .expect("you without the send grant");
    h.env
        .store()
        .transaction(|tx| -> Result<(), steve_store::StoreError> {
            tx.execute(
                "UPDATE connections SET caps = '[\"msg.receive\"]' WHERE id = ?",
                [&con],
            )?;
            Ok(())
        })
        .expect("drop the send grant");
    let i = push(&h, &con, msg("C_MEDM", "n1", "ulilei", "在吗", false)).await;
    let r = expect(&i, Some(0), Act::Record);
    assert_eq!(r.label, steve_channels::respond::NO_SEND_AS_YOU);
    assert!(
        r.decision_id.is_none() && r.out_message_id.is_none(),
        "no draft: {r:?}"
    );
    let i = push(&h, &con, msg("C_DEV", "n2", "ulilei", "@你 看下", true)).await;
    let r = expect(&i, Some(1), Act::Record);
    assert!(r.decision_id.is_none(), "no draft for a mention either");
    let drafts = rows(
        &h,
        "SELECT count(*) FROM chan_messages WHERE direction = 'out' AND reply_to IN ('n1', 'n2')",
        &[],
    );
    assert_eq!(drafts[0][0], json!(0));
    assert!(
        h.env
            .store()
            .reader(|c| spaces::can_send_as_you(c, &con))
            .is_ok_and(|ok| !ok)
    );
}

/// An intent condition still refines a message addressed to the account: the fast tier judges it with
/// the space's earlier messages as context; a "no" falls through to the next rule.
#[tokio::test(flavor = "multi_thread")]
async fn intent_refines_an_addressed_message() {
    let h = setup().await;
    let c = h.oncall.clone();
    let first = push(
        &h,
        &c,
        msg(
            "C_OPS",
            "i0",
            "uhan",
            "@值班小运 昨晚备份是不是又挂了？",
            true,
        ),
    )
    .await;
    let space = first.space_id.clone();
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"rules": [
            {"when": {"kind": "intent", "text": "需要动手操作"}, "act": "handoff"},
            {"when": {"kind": "mention_or_dm"}, "act": "draft"}
        ]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    // a later message of the space, not addressed: still context for the judgement
    push(&h, &c, msg("C_OPS", "i0b", "uwang", "今晚部署吗", false)).await;
    h.env.gateway.reply("{\"match\": true}");
    let n = calls(&h);
    let i = push(
        &h,
        &c,
        msg(
            "C_OPS",
            "i1",
            "uhan",
            "@值班小运 db-2 磁盘满了，帮忙清一下",
            true,
        ),
    )
    .await;
    expect(&i, Some(0), Act::Handoff);
    let (k, (tier, prompt)) = {
        let prompts = h.env.gateway.calls.lock().unwrap();
        (prompts.len(), prompts.last().expect("prompt").clone())
    };
    assert_eq!(k, n + 1, "one judgement");
    assert_eq!(tier, steve_modules::Tier::Fast);
    assert!(
        prompt.contains("条件：需要动手操作") && prompt.contains("db-2 磁盘满了"),
        "{prompt}"
    );
    // the context: earlier messages of the space, oldest first
    let a = prompt.find("昨晚备份是不是又挂了").expect("context 1");
    let b = prompt.find("今晚部署吗").expect("context 2");
    assert!(a < b, "{prompt}");
    // the model says no, or fails: the next rule
    h.env.gateway.reply("{\"match\": false}");
    let i = push(
        &h,
        &c,
        msg("C_OPS", "i2", "uhan", "@值班小运 今天天气不错", true),
    )
    .await;
    expect(&i, Some(1), Act::Draft);
    h.env.gateway.fail("timeout");
    let i = push(
        &h,
        &c,
        msg("C_OPS", "i3", "uhan", "@值班小运 重启一下 web-1", true),
    )
    .await;
    expect(&i, Some(1), Act::Draft);
}

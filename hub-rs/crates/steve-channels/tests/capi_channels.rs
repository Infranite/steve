//! The channel REST gaps the web pages found (W11/W12), on the fake Feishu: `GET/POST/PATCH
//! /channels…` (the ChannelAccount with its persona, rules, trigger and permission check), the
//! “新群” decision (open / mute / undo, expired when the bot leaves), `GET /spaces/:id/members`,
//! and the connection runtime with the Feishu hooks starting the long connection, `sync_bot_chats`
//! and the self poller as connections change.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::{BotHook, FeishuApp, FeishuUser, SelfPollHook};
use steve_channels::ext::{ChannelDecl, Mode, Receive};
use steve_channels::fake::feishu::{FakeFeishu, WsClientConfig, ids};
use steve_channels::lifecycle::ConnectionRuntime;
use steve_channels::{Channels, SpaceEvent, channels_api, space_decisions};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{Caller, decisions};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tower::ServiceExt;

const T: Duration = Duration::from_secs(10);

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: FakeFeishu,
    ch: Channels,
    user: Arc<FeishuUser>,
    api: Router,
    cb: Router,
}

async fn setup() -> H {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    fake.ws_set_client_config(WsClientConfig {
        reconnect_count: -1,
        reconnect_interval: 1,
        reconnect_nonce: 0,
        ping_interval: 1,
    });
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
        space_decisions::register(r);
    });
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '沉稳、简洁。', 1),
                    ('ops', 'default', '运维', 0, 'claude', '', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES
                    ('steve', 'default', 1), ('ops', 'default', 2);",
            )?;
            Ok(())
        })
        .expect("bots");
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    vault.init(WS, OTHER).ok();
    let app = Arc::new(FeishuApp::new());
    let user = Arc::new(FeishuUser::with_base(fake.base_url()));
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec!["http://hub.test:8440".into()], true, |_| None),
    )
    .provider(app.clone())
    .provider(user.clone())
    .usage(Arc::new(steve_channels::usage::ChannelUsage))
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    // the built-in sides as the Hub declares them
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .decl(steve_channels::adapters::feishu::app_decl(&app))
        .decl(steve_channels::adapters::feishu::user_decl())
        .adapter(app)
        .build();
    let api = connections::routes(conns.clone())
        .merge(steve_channels::api::routes(ch.clone()))
        .merge(channels_api::routes(ch.clone()))
        .with_state(env.modules.clone());
    let cb = connections::callback_routes(conns).with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        fake,
        ch,
        user,
        api,
        cb,
    }
}

async fn send(
    r: &Router,
    who: &Caller,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "hub.test:8440");
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = req
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    req.extensions_mut().insert(who.clone());
    let res = r.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

impl H {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        send(&self.api, &caller(WS, ME), method, path, body).await
    }

    fn n(&self, sql: &str, p: &str) -> i64 {
        let (sql, p) = (sql.to_string(), p.to_string());
        self.env
            .store()
            .reader(move |c| {
                Ok::<_, steve_store::StoreError>(c.query_row(&sql, [&p], |r| r.get(0))?)
            })
            .expect("count")
    }

    fn exec(&self, sql: &str) {
        let sql = sql.to_string();
        self.env
            .store()
            .with_conn(move |c| {
                c.execute_batch(&sql)?;
                Ok(())
            })
            .expect("exec");
    }

    async fn new_persona(&self, name: &str) -> String {
        let (st, v) = self
            .call(
                "POST",
                "/personas",
                Some(json!({"name": name, "bot_id": "ops"})),
            )
            .await;
        assert_eq!(st, 200, "{v}");
        s(&v["id"])
    }

    /// `POST /channels` for the bot; returns the ChannelAccount.
    async fn connect_bot(&self, persona: Option<&str>) -> Value {
        let (st, v) = self
            .call(
                "POST",
                "/channels",
                Some(json!({
                    "adapter": "feishu_app",
                    "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
                    "config": {"api_base": self.fake.base_url()},
                    "persona_id": persona,
                })),
            )
            .await;
        assert_eq!(st, 200, "{v}");
        v
    }

    /// “授权你本人” through `POST /channels {adapter:"feishu_user"}` and the fake's consent page;
    /// the user connection's id.
    async fn authorize_me(&self) -> String {
        let (st, v) = self
            .call("POST", "/channels", Some(json!({"adapter": "feishu_user"})))
            .await;
        assert_eq!(st, 200, "{v}");
        let url = s(&v["url"]);
        assert!(
            url.starts_with(self.fake.base_url()),
            "fake hosts only: {url}"
        );
        let res = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
            .get(&url)
            .send()
            .await
            .expect("authorize");
        assert!(res.status().is_redirection());
        let back =
            url::Url::parse(res.headers()[header::LOCATION].to_str().expect("loc")).expect("url");
        let p = format!("{}?{}", back.path(), back.query().unwrap_or_default());
        let (st, _) = send(&self.cb, &caller(WS, ME), "GET", &p, None).await;
        assert_eq!(st, 302);
        let id: String = self
            .env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT id FROM connections WHERE adapter = 'feishu_user'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .expect("user connection");
        id
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------------------------
// Accounts.

#[tokio::test(flavor = "multi_thread")]
async fn bot_account_through_post_channels() {
    let h = setup().await;
    let per = h.new_persona("值班小运").await;

    // a persona of nobody's: refused before anything is created
    let (st, v) = h
        .call(
            "POST",
            "/channels",
            Some(json!({"adapter": "feishu_app", "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET}, "config": {"api_base": h.fake.base_url()}, "persona_id": "per_nope"})),
        )
        .await;
    assert_eq!(st, 404, "{v}");
    assert_eq!(
        h.n(
            "SELECT count(*) FROM connections WHERE workspace_id = ?",
            WS
        ),
        0
    );

    let a = h.connect_bot(Some(&per)).await;
    let con = s(&a["connection_id"]);
    assert_eq!(a["mode"], "bot");
    assert_eq!(a["system"], "feishu");
    assert_eq!(a["provider"], "飞书 / Lark");
    assert_eq!(a["name"], ids::BOT_NAME);
    assert_eq!(
        a["platform_name"],
        format!("{}（应用机器人）", ids::BOT_NAME)
    );
    assert_eq!(a["status"], "ok");
    assert_eq!(a["persona"], json!({"id": per, "name": "值班小运"}));
    assert_eq!(
        a["rules"],
        json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}])
    );
    assert_eq!(a["trigger"], Value::Null);
    assert_eq!(a["features"]["recall"], true);
    assert_eq!(a["spaces"], 0);
    // the permission check of verify, for the ticks
    let perms = a["permissions"].as_array().expect("permissions");
    assert!(!perms.is_empty());
    assert!(
        perms
            .iter()
            .all(|p| p["scope"].is_string() && p["status"].is_string())
    );
    assert!(a["missing_permissions"].is_array());
    assert_eq!(a["self_built"], true);
    assert!(a["permissions_checked_at"].is_i64());
    assert_eq!(
        h.n(
            "SELECT count(*) FROM audit WHERE action = 'channel.create' AND target = ?",
            &con
        ),
        1
    );

    // GET /channels: the Feishu provider and the account
    let (st, v) = h.call("GET", "/channels", None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["providers"][0]["system"], "feishu");
    assert_eq!(v["providers"][0]["adapter_installed"], true);
    assert_eq!(v["accounts"].as_array().map(Vec::len), Some(1));
    assert_eq!(v["accounts"][0]["connection_id"], con);

    // PATCH: duty, rules, another persona — one revision, audited
    let per2 = h.new_persona("小运二号").await;
    let (st, v) = h
        .call(
            "PATCH",
            &format!("/channels/{con}"),
            Some(json!({
                "duty": "  负责运维群的告警排查  ",
                "rules": [{"when": {"kind": "keyword", "words": ["告警"]}, "act": "reply"}, {"when": {"kind": "all"}, "act": "record"}],
                "persona_id": per2,
            })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["duty"], "负责运维群的告警排查");
    assert_eq!(v["persona"]["id"], per2);
    assert_eq!(v["rules"][0]["when"]["words"], json!(["告警"]));
    assert_eq!(v["revision"], 2);
    assert_eq!(
        h.n(
            "SELECT count(*) FROM revisions WHERE object_type = 'channel' AND object_id = ?",
            &con
        ),
        1
    );
    assert_eq!(
        h.n(
            "SELECT count(*) FROM audit WHERE action = 'channel.patch' AND target = ?",
            &con
        ),
        1
    );

    // a bot has no trigger words; bad rules and unknown fields are 400
    for (body, want) in [
        (
            json!({"trigger": {"on": true}}),
            "触发语只用于以你本人接入的账号",
        ),
        (
            json!({"rules": [{"when": {"kind": "nope"}, "act": "reply"}]}),
            "",
        ),
        (json!({"duty": "x".repeat(4001)}), "duty"),
        (json!({"colour": "red"}), ""),
    ] {
        let (st, v) = h
            .call("PATCH", &format!("/channels/{con}"), Some(body))
            .await;
        assert_eq!(st, 400, "{v}");
        assert!(s(&v["error"]).contains(want), "{v}");
    }
    let (st, _) = h
        .call(
            "PATCH",
            &format!("/channels/{con}"),
            Some(json!({"persona_id": "per_nope"})),
        )
        .await;
    assert_eq!(st, 404);

    // GET /channels/:id: the spaces in short form
    h.ch.on_space_event(
        WS,
        &con,
        SpaceEvent::Joined {
            space_ext_id: ids::GROUP_CHAT.into(),
            name: Some("平台值班群".into()),
            has_external: Some(false),
        },
    )
    .await
    .expect("joined");
    let (st, v) = h.call("GET", &format!("/channels/{con}"), None).await;
    assert_eq!(st, 200, "{v}");
    let sp = v["spaces"].as_array().expect("spaces");
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0]["name"], "平台值班群");
    assert_eq!(sp[0]["configured"], false);
    assert_eq!(sp[0]["persona"]["id"], per2);

    let (st, _) = h.call("GET", "/channels/con_nope", None).await;
    assert_eq!(st, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn your_own_account_has_a_trigger_and_no_persona() {
    let h = setup().await;
    let bot = h.connect_bot(None).await;
    // no persona given: the default persona “Steve” speaks
    assert_eq!(bot["persona"]["name"], "Steve");

    // the only bot's app, without naming it
    let ucon = h.authorize_me().await;
    let (st, a) = h.call("GET", &format!("/channels/{ucon}"), None).await;
    assert_eq!(st, 200, "{a}");
    assert_eq!(a["mode"], "self");
    assert_eq!(a["adapter"], "feishu_user");
    assert_eq!(a["persona"], Value::Null);
    assert_eq!(
        a["trigger"],
        json!({"on": false, "words": ["/bot"], "where": ["*"], "mark": true}),
        "nothing chosen reads as everywhere"
    );
    assert!(s(&a["platform_name"]).ends_with("（你本人）"));
    assert_eq!(a["features"]["as_user"], true);
    assert_eq!(a["features"]["edit"], false);
    assert!(a["permissions"].is_array(), "{a}");

    // the other user does not see your account
    let (st, _) = send(
        &h.api,
        &caller(WS, OTHER),
        "GET",
        &format!("/channels/{ucon}"),
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (_, v) = send(&h.api, &caller(WS, OTHER), "GET", "/channels", None).await;
    assert_eq!(v["accounts"].as_array().map(Vec::len), Some(1), "{v}");

    for body in [
        json!({"persona_id": s(&bot["persona"]["id"])}),
        json!({"rules": [{"when": {"kind": "all"}, "act": "reply"}]}),
        json!({"trigger": {"words": ["bot"]}}),
        json!({"trigger": {"words": ["/a", "/b", "/c", "/d", "/e", "/f", "/g", "/h", "/i", "/j", "/k"]}}),
        json!({"trigger": {"where": ["spc_nope"]}}),
    ] {
        let (st, v) = h
            .call("PATCH", &format!("/channels/{ucon}"), Some(body.clone()))
            .await;
        assert_eq!(st, 400, "{body} → {v}");
    }

    // partial trigger patches merge
    let (st, v) = h
        .call(
            "PATCH",
            &format!("/channels/{ucon}"),
            Some(json!({"trigger": {"on": true, "where": ["dm:*"]}})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        v["trigger"],
        json!({"on": true, "words": ["/bot"], "where": ["dm:*"], "mark": true})
    );
    let (st, v) = h
        .call(
            "PATCH",
            &format!("/channels/{ucon}"),
            Some(json!({"trigger": {"words": ["/bot", "/steve", "/bot"], "mark": false}})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        v["trigger"],
        json!({"on": true, "words": ["/bot", "/steve"], "where": ["dm:*"], "mark": false})
    );
    assert_eq!(
        h.n(
            "SELECT count(*) FROM revisions WHERE object_type = 'channel' AND object_id = ?",
            &ucon
        ),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn post_channels_for_you_needs_an_app_and_checks_the_body() {
    let h = setup().await;
    let (st, v) = h
        .call("POST", "/channels", Some(json!({"adapter": "feishu_user"})))
        .await;
    // the same answer as the generic OAuth start: without a bot to pair with, no OAuth app
    assert_eq!(st, 400, "{v}");
    assert_eq!(v["error"], "还没有配置 飞书 的 OAuth 应用");
    let (st, _) = h
        .call("POST", "/channels", Some(json!({"adapter": "slack"})))
        .await;
    assert_eq!(st, 400);
    let (st, _) = h
        .call(
            "POST",
            "/channels",
            Some(json!({"adapter": "feishu_user", "app_connection": "con_nope"})),
        )
        .await;
    assert_eq!(st, 404);
}

// ---------------------------------------------------------------------------------------------
// The “新群” decision.

fn joined(ext: &str, name: &str) -> SpaceEvent {
    SpaceEvent::Joined {
        space_ext_id: ext.into(),
        name: Some(name.into()),
        has_external: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_group_decision_open_mute_undo_and_expiry() {
    let h = setup().await;
    let con = s(&h.connect_bot(None).await["connection_id"]);
    let me = caller(WS, ME);

    // mute
    let c =
        h.ch.on_space_event(WS, &con, joined(ids::GROUP_CHAT, "平台值班群"))
            .await
            .expect("ev")
            .expect("change");
    let d = c.decision_id.expect("新群 decision");
    let r = decisions::dispatch(h.ch.modules(), &me, &d, "mute", "")
        .expect("ours")
        .expect("resolved");
    assert_eq!(r.result["choice"], "mute");
    assert_eq!(r.result["space_id"], c.space_id);
    let (_, sp) = h
        .call("GET", &format!("/spaces/{}", c.space_id), None)
        .await;
    assert_eq!(sp["muted"], true);
    assert_eq!(sp["configured"], true);
    assert_eq!(
        h.n(
            "SELECT count(*) FROM revisions WHERE object_type = 'space' AND object_id = ?",
            &c.space_id
        ),
        1
    );
    // undo: back as it was
    h.env
        .store()
        .transaction(|tx| {
            let d = steve_store::base::decisions::get(tx, &d)?.expect("decision");
            h.ch.modules().registry.before_undo(tx, &d)
        })
        .expect("undo");
    let (_, sp) = h
        .call("GET", &format!("/spaces/{}", c.space_id), None)
        .await;
    assert_eq!(sp["muted"], false);
    assert_eq!(sp["configured"], false);

    // open
    let c2 =
        h.ch.on_space_event(WS, &con, joined(ids::TOPIC_CHAT, "需求讨论"))
            .await
            .expect("ev")
            .expect("change");
    let d2 = c2.decision_id.expect("新群 decision");
    let r = decisions::dispatch(h.ch.modules(), &me, &d2, "open", "")
        .expect("ours")
        .expect("resolved");
    assert_eq!(r.result["space_id"], c2.space_id);
    let (_, sp) = h
        .call("GET", &format!("/spaces/{}", c2.space_id), None)
        .await;
    assert_eq!(sp["muted"], false);
    assert_eq!(sp["configured"], true);
    let bad = decisions::dispatch(h.ch.modules(), &me, &d2, "open", "").expect("ours");
    assert!(bad.is_err(), "already answered");

    // the bot is removed / the group disbanded: an open card expires
    let c3 =
        h.ch.on_space_event(WS, &con, joined(ids::EXTERNAL_CHAT, "客户对接群"))
            .await
            .expect("ev")
            .expect("change");
    let d3 = c3.decision_id.expect("新群 decision");
    h.ch.on_space_event(
        WS,
        &con,
        SpaceEvent::Left {
            space_ext_id: ids::EXTERNAL_CHAT.into(),
            archived: false,
        },
    )
    .await
    .expect("left");
    let st: String = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT status FROM decisions WHERE id = ?",
                [&d3],
                |r| r.get(0),
            )?)
        })
        .expect("status");
    assert_eq!(st, "expired");
    let c4 =
        h.ch.on_space_event(WS, &con, joined("oc_another", "另一个群"))
            .await
            .expect("ev")
            .expect("change");
    let d4 = c4.decision_id.expect("新群 decision");
    h.ch.on_space_event(
        WS,
        &con,
        SpaceEvent::Left {
            space_ext_id: "oc_another".into(),
            archived: true,
        },
    )
    .await
    .expect("disbanded");
    assert_eq!(
        h.n("SELECT count(*) FROM decisions WHERE id = ? AND status = 'expired' AND json_extract(result, '$.reason') = 'disbanded'", &d4),
        1
    );
}

// ---------------------------------------------------------------------------------------------
// Members.

#[tokio::test(flavor = "multi_thread")]
async fn space_members_paged_with_contacts_and_external() {
    let h = setup().await;
    let con = s(&h.connect_bot(None).await["connection_id"]);
    let c =
        h.ch.on_space_event(WS, &con, joined(ids::EXTERNAL_CHAT, "客户对接群"))
            .await
            .expect("ev")
            .expect("change");
    let internal =
        h.ch.on_space_event(WS, &con, joined(ids::GROUP_CHAT, "平台值班群"))
            .await
            .expect("ev")
            .expect("change");

    let (st, v) = h
        .call("GET", &format!("/spaces/{}/members", c.space_id), None)
        .await;
    assert_eq!(st, 200, "{v}");
    // the bot, you, Alice, the customer
    assert_eq!(v["total"], 4, "{v}");
    let ms = v["members"].as_array().expect("members").clone();
    assert_eq!(ms.last().expect("bot")["role"], "bot", "bots last");
    let by = |name: &str| {
        ms.iter()
            .find(|m| m["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}: {ms:?}"))
    };
    let cust = by("赵客户");
    assert!(cust["contact"]["id"].is_string(), "{cust}");
    assert_eq!(cust["contact"]["type"], "contact");
    // nothing says who the customer is yet
    assert_eq!(cust["external"], Value::Null);

    // a customer mark and a job at the own organization decide it
    let cust_party = s(&cust["contact"]["id"]);
    let alice_party = s(&by("王小丽")["contact"]["id"]);
    h.exec(&format!(
        "UPDATE parties SET rels = '[\"customer\"]' WHERE id = '{cust_party}'"
    ));
    h.exec(&format!(
        "INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, props, source, created)
         SELECT 'default', 'party', '{alice_party}', 'works_at', 'party', id, '{{}}', 'sync', 1 FROM parties
          WHERE workspace_id = 'default' AND type = 'org' AND org_kind = 'self'"
    ));
    let (_, v) = h
        .call("GET", &format!("/spaces/{}/members", c.space_id), None)
        .await;
    let ms = v["members"].as_array().expect("members").clone();
    let by = |name: &str| {
        ms.iter()
            .find(|m| m["name"] == name)
            .cloned()
            .expect("member")
    };
    assert_eq!(by("赵客户")["external"], true);
    assert_eq!(by("王小丽")["external"], false);
    assert!(by("王小丽")["org"]["id"].is_string());

    // a group without external members: nobody is
    let (_, v) = h
        .call(
            "GET",
            &format!("/spaces/{}/members", internal.space_id),
            None,
        )
        .await;
    assert!(
        v["members"]
            .as_array()
            .expect("members")
            .iter()
            .all(|m| m["external"] == false),
        "{v}"
    );

    // paging
    let (_, v) = h
        .call(
            "GET",
            &format!("/spaces/{}/members?limit=2&offset=1", c.space_id),
            None,
        )
        .await;
    assert_eq!(v["total"], 4);
    assert_eq!(v["members"].as_array().map(Vec::len), Some(2));
    assert_eq!(v["offset"], 1);
    for q in ["limit=0", "limit=201", "offset=-1", "limit=x"] {
        let (st, _) = h
            .call("GET", &format!("/spaces/{}/members?{q}", c.space_id), None)
            .await;
        assert_eq!(st, 400, "{q}");
    }
    let (st, _) = h.call("GET", "/spaces/spc_nope/members", None).await;
    assert_eq!(st, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn members_of_your_own_spaces_are_yours_alone() {
    let h = setup().await;
    let _bot = h.connect_bot(None).await;
    let ucon = h.authorize_me().await;
    let c =
        h.ch.on_space_event(WS, &ucon, joined("oc_mine", "我的小群"))
            .await
            .expect("ev")
            .expect("change");
    let (st, _) = h
        .call("GET", &format!("/spaces/{}/members", c.space_id), None)
        .await;
    assert_eq!(st, 200);
    let (st, _) = send(
        &h.api,
        &caller(WS, OTHER),
        "GET",
        &format!("/spaces/{}/members", c.space_id),
        None,
    )
    .await;
    assert_eq!(st, 404);

    // still yours alone once your account is disconnected; messages kept
    h.exec(&format!(
        "INSERT INTO chan_messages (id, workspace_id, space_id, direction, text, created) VALUES ('m_keep', '{WS}', '{}', 'in', '留着', 1)",
        c.space_id
    ));
    let (st, v) = h
        .call("DELETE", &format!("/connections/{ucon}"), None)
        .await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = h
        .call("GET", &format!("/spaces/{}", c.space_id), None)
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["status"], "left", "{v}");
    assert_eq!(
        h.n("SELECT count(*) FROM chan_messages WHERE id = ?", "m_keep"),
        1
    );
    for path in [
        format!("/spaces/{}", c.space_id),
        format!("/spaces/{}/members", c.space_id),
    ] {
        let (st, _) = send(&h.api, &caller(WS, OTHER), "GET", &path, None).await;
        assert_eq!(st, 404, "{path}");
    }
    let (_, v) = send(&h.api, &caller(WS, OTHER), "GET", "/spaces", None).await;
    assert!(
        v.as_array()
            .expect("spaces")
            .iter()
            .all(|x| x["id"] != c.space_id.as_str()),
        "{v}"
    );
}

// ---------------------------------------------------------------------------------------------
// The runtime.

/// The connection runtime with the built-in Feishu hooks, and the sender its events come on.
fn runtime(h: &H) -> (ConnectionRuntime, mpsc::UnboundedSender<Value>) {
    let rt = ConnectionRuntime::builder(h.ch.clone())
        .hook(
            "feishu_app",
            Some(ChannelDecl::new("feishu_app")),
            Arc::new(BotHook::new(h.ch.clone())),
        )
        .hook(
            "feishu_user",
            Some(
                ChannelDecl::new("feishu_user")
                    .mode(Mode::Own)
                    .receive(Receive::Poll, Some("msg.receive")),
            ),
            Arc::new(SelfPollHook::new(h.ch.clone(), h.user.clone())),
        )
        .build();
    let (tx, rx) = mpsc::unbounded_channel();
    drop(rt.start(rx));
    (rt, tx)
}

/// The change events published since the last call, as the Hub's bus hands them to the runtime.
fn publish(h: &H, tx: &mpsc::UnboundedSender<Value>) {
    for e in h.env.take_events() {
        let _ = tx.send(serde_json::to_value(e).expect("json"));
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(t0.elapsed() < T, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Polls `rt.running()` until it is `want` (sorted).
async fn running(rt: &ConnectionRuntime, want: &[&str]) {
    let mut want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    want.sort();
    until(&format!("running {want:?}"), || rt.running() == want).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_starts_the_long_connection_and_syncs_chats_without_a_restart() {
    let h = setup().await;
    let _ = h.env.take_events();
    let (rt, tx) = runtime(&h);
    let con = s(&h.connect_bot(None).await["connection_id"]);

    // the connect's `connection` event reaches the runtime
    publish(&h, &tx);
    assert!(h.fake.ws_wait_connections(1, T).await, "long connection up");
    until("the bot's groups became spaces", || {
        h.n("SELECT count(*) FROM spaces WHERE connection_id = ?", &con) == 3
    })
    .await;
    // no 新群 card for groups the bot was already in
    assert_eq!(
        h.n(
            "SELECT count(*) FROM decisions WHERE subject_type = 'space' AND workspace_id = ?",
            WS
        ),
        0
    );
    assert_eq!(rt.running(), [con.as_str()]);
    assert_eq!(
        h.n(
            "SELECT count(*) FROM channel_settings WHERE connection_id = ? AND mode = 'bot'",
            &con
        ),
        1
    );

    // repeated events change nothing
    publish(&h, &tx);
    let _ = tx.send(json!({"type": "connection", "id": con, "ws": WS}));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.fake.ws_connects_total(), 1);

    // paused → stopped; resumed → started again and synced
    let (st, v) = h
        .call(
            "PATCH",
            &format!("/connections/{con}"),
            Some(json!({"paused": true})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    publish(&h, &tx);
    running(&rt, &[]).await;
    h.call(
        "PATCH",
        &format!("/connections/{con}"),
        Some(json!({"paused": false})),
    )
    .await;
    publish(&h, &tx);
    running(&rt, &[&con]).await;
    until("connected again", || h.fake.ws_connects_total() == 2).await;
    // known groups are kept
    assert_eq!(
        h.n("SELECT count(*) FROM spaces WHERE connection_id = ?", &con),
        3
    );

    // disconnecting an account with spaces: the spaces stay, `left` (an archived one stays
    // archived), without settings or a connection; the long connection stops for good
    h.exec(&format!(
        "UPDATE spaces SET status = 'archived' WHERE id = (SELECT min(id) FROM spaces WHERE connection_id = '{con}')"
    ));
    let (_, u) = h
        .call("GET", &format!("/connections/{con}/usage"), None)
        .await;
    assert_eq!(u["spaces"], 2, "{u}");
    let (st, v) = h.call("DELETE", &format!("/connections/{con}"), None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["usage"]["spaces"], 2, "{v}");
    let left = |status: &str| {
        h.n(
            &format!("SELECT count(*) FROM spaces WHERE connection_id IS NULL AND status = '{status}' AND account_id IS NOT NULL AND workspace_id = ?"),
            WS,
        )
    };
    assert_eq!((left("left"), left("archived")), (2, 1));
    assert_eq!(
        h.n(
            "SELECT count(*) FROM space_settings WHERE workspace_id = ?",
            WS
        ),
        0
    );
    assert_eq!(
        h.n(
            "SELECT count(*) FROM channel_settings WHERE connection_id = ?",
            &con
        ),
        0
    );
    let (st, v) = h.call("GET", "/spaces", None).await;
    assert_eq!(st, 200, "{v}");
    let list = v.as_array().expect("spaces").clone();
    assert_eq!(list.len(), 3, "{v}");
    assert!(
        list.iter()
            .all(|x| x["status"] != "active" && x["connection_id"] == "" && x["persona"].is_null()),
        "{v}"
    );
    let one = format!("/spaces/{}", s(&list[0]["id"]));
    let (st, v) = h.call("GET", &one, None).await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = h.call("PATCH", &one, Some(json!({"muted": true}))).await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"], steve_channels::spaces::DISCONNECTED);
    // a bot account's spaces are the workspace's: another member sees them …
    let (st, _) = send(&h.api, &caller(WS, OTHER), "GET", &one, None).await;
    assert_eq!(st, 200);
    // … until the account itself is deleted: then only the workspace owner does
    h.exec(&format!(
        "PRAGMA foreign_keys = OFF;
         DELETE FROM account_handles WHERE account_id IN (SELECT account_id FROM spaces WHERE id = '{id}');
         DELETE FROM accounts WHERE id IN (SELECT account_id FROM spaces WHERE id = '{id}');
         PRAGMA foreign_keys = ON;",
        id = s(&list[0]["id"])
    ));
    let (st, _) = send(&h.api, &caller(WS, OTHER), "GET", &one, None).await;
    assert_eq!(st, 404);
    let (st, _) = h.call("GET", &one, None).await;
    assert_eq!(st, 200);
    publish(&h, &tx);
    running(&rt, &[]).await;
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authorizing_you_starts_your_self_poller() {
    let h = setup().await;
    let (rt, tx) = runtime(&h);
    let bot = s(&h.connect_bot(None).await["connection_id"]);
    publish(&h, &tx);
    running(&rt, &[&bot]).await;
    let ucon = h.authorize_me().await;
    publish(&h, &tx);
    running(&rt, &[&bot, &ucon]).await;
    assert_eq!(
        h.n(
            "SELECT count(*) FROM channel_settings WHERE connection_id = ? AND mode = 'self'",
            &ucon
        ),
        1
    );
    h.call(
        "PATCH",
        &format!("/connections/{ucon}"),
        Some(json!({"paused": true})),
    )
    .await;
    publish(&h, &tx);
    running(&rt, &[&bot]).await;
    // resumed, then disconnected (no spaces): started, then stopped for good
    h.call(
        "PATCH",
        &format!("/connections/{ucon}"),
        Some(json!({"paused": false})),
    )
    .await;
    publish(&h, &tx);
    running(&rt, &[&bot, &ucon]).await;
    let (st, v) = h
        .call("DELETE", &format!("/connections/{ucon}"), None)
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        h.n(
            "SELECT count(*) FROM channel_settings WHERE connection_id = ?",
            &ucon
        ),
        0
    );
    publish(&h, &tx);
    running(&rt, &[&bot]).await;
    rt.shutdown().await;
    assert!(rt.running().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_device_codes_can_be_approved_over_http() {
    let h = setup().await;
    let http = reqwest::Client::new();
    let base = h.fake.base_url().to_string();
    let r = http
        .post(format!("{base}/_fake/device/approve"))
        .send()
        .await
        .expect("approve");
    assert_eq!(r.status().as_u16(), 404, "nothing pending");

    let bot = s(&h.connect_bot(None).await["connection_id"]);
    let dev = steve_channels::adapters::feishu::DeviceFlows::new(
        h.ch.connections().clone(),
        h.user.clone(),
    );
    let api =
        steve_channels::adapters::feishu::device_routes(dev).with_state(h.env.modules.clone());
    let me = caller(WS, ME);
    h.fake.set_device_interval(1);
    let (st, v) = send(
        &api,
        &me,
        "POST",
        "/channels/feishu/device/start",
        Some(json!({"app_connection": bot})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let r = http
        .post(format!("{base}/_fake/device/approve"))
        .json(&json!({}))
        .send()
        .await
        .expect("approve");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("json");
    assert_eq!(body["user_code"], v["user_code"]);
    let path = format!("/channels/feishu/device/{}", s(&v["id"]));
    let mut last = Value::Null;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        last = send(&api, &me, "GET", &path, None).await.1;
        if last["status"] != "pending" {
            break;
        }
    }
    assert_eq!(last["status"], "ok", "{last}");
    // a code that is not pending any more
    let r = http
        .post(format!(
            "{base}/_fake/device/deny?user_code={}",
            s(&v["user_code"])
        ))
        .send()
        .await
        .expect("deny");
    assert_eq!(r.status().as_u16(), 404);
}

// ---------------------------------------------------------------------------------------------
// One space per chat, its voice, and the new-space answers 不提醒 / 屏蔽.

#[tokio::test(flavor = "multi_thread")]
async fn one_space_per_chat_with_one_voice() {
    use steve_channels::channel::{HandleRef, InboundMessage};
    let h = setup().await;
    let bot = s(&h.connect_bot(None).await["connection_id"]);
    let ucon = h.authorize_me().await;
    let c =
        h.ch.on_space_event(WS, &bot, joined(ids::GROUP_CHAT, "平台值班群"))
            .await
            .expect("ev")
            .expect("change");
    // your own account reads the same chat: the same space, now seen by two identities
    let m = InboundMessage::text(
        ids::GROUP_CHAT,
        "om_u1",
        HandleRef::new("feishu", ids::ALICE.open_id),
        "早",
        steve_store::now(),
    );
    let i = h.ch.on_message(WS, &ucon, m).await.expect("message");
    assert_eq!(i.space_id, c.space_id, "one space per (installation, chat)");
    assert_eq!(
        h.n(
            "SELECT count(*) FROM spaces WHERE external_id = ?",
            ids::GROUP_CHAT
        ),
        1
    );
    let path = format!("/spaces/{}", c.space_id);
    let (_, v) = h.call("GET", &path, None).await;
    assert_eq!(v["identities"].as_array().map(Vec::len), Some(2), "{v}");
    // automatic voice: the bot is in the group
    assert_eq!(v["voice"]["connection_id"], bot.as_str());
    assert_eq!(v["voice"]["chosen"], false);
    assert!(
        v["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["key"] == "voice" && x["text"].as_str().unwrap().contains("只观察")),
        "{v}"
    );
    let read = |space: &str| {
        let space = space.to_string();
        h.env
            .store()
            .reader(move |c| {
                Ok::<_, steve_modules::ApiError>((
                    steve_channels::spaces::voice(c, &space)?,
                    steve_channels::spaces::attention(c, &space)?,
                ))
            })
            .expect("read")
    };
    assert_eq!(
        read(&c.space_id),
        (
            Some(bot.clone()),
            steve_channels::spaces::Attention::Realtime
        )
    );
    // choose your own account; an identity that is not in the space is refused
    let (st, v) = h.call("PATCH", &path, Some(json!({"voice": ucon}))).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["voice"]["connection_id"], ucon.as_str());
    assert_eq!(v["voice"]["chosen"], true);
    assert_eq!(read(&c.space_id).0, Some(ucon.clone()));
    let (st, v) = h
        .call("PATCH", &path, Some(json!({"voice": "con_elsewhere"})))
        .await;
    assert_eq!(st, 400, "{v}");
    assert!(v.to_string().contains("不在这个群里"), "{v}");
    let (st, v) = h.call("PATCH", &path, Some(json!({"voice": null}))).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["voice"]["connection_id"], bot.as_str());
}

#[tokio::test(flavor = "multi_thread")]
async fn new_group_decision_quiet_block_and_the_suggestion() {
    let h = setup().await;
    let con = s(&h.connect_bot(None).await["connection_id"]);
    let me = caller(WS, ME);
    let c =
        h.ch.on_space_event(WS, &con, joined(ids::GROUP_CHAT, "线上告警通知"))
            .await
            .expect("ev")
            .expect("change");
    let d = c.decision_id.expect("新群 decision");
    let path = format!("/spaces/{}", c.space_id);
    // the fake names the chat itself; an alert group's name
    h.exec(&format!(
        "UPDATE spaces SET name = '线上告警通知' WHERE id = '{}'",
        c.space_id
    ));
    let (_, sp) = h.call("GET", &path, None).await;
    assert!(
        sp["suggest_quiet"].as_str().unwrap_or("").contains("告警"),
        "{sp}"
    );
    assert_eq!(
        h.n(
            "SELECT count(*) FROM decisions WHERE id = ? AND options = '[\"open\",\"mute\",\"quiet\",\"block\"]'",
            &d
        ),
        1
    );
    let r = decisions::dispatch(h.ch.modules(), &me, &d, "quiet", "")
        .expect("ours")
        .expect("resolved");
    assert_eq!(r.result["choice"], "quiet");
    let (_, sp) = h.call("GET", &path, None).await;
    assert_eq!(sp["attention"], "quiet");
    assert_eq!(sp["muted"], false, "不提醒 is not 静默");
    assert_eq!(sp["configured"], true);
    assert_eq!(sp["suggest_quiet"], Value::Null, "only for a new space");
    h.env
        .store()
        .transaction(|tx| {
            let d = steve_store::base::decisions::get(tx, &d)?.expect("decision");
            h.ch.modules().registry.before_undo(tx, &d)
        })
        .expect("undo");
    let (_, sp) = h.call("GET", &path, None).await;
    assert_eq!(sp["attention"], "realtime");
    assert_eq!(sp["configured"], false);

    // 屏蔽 from the card; and setting a level on the page expires an open card
    let c2 =
        h.ch.on_space_event(WS, &con, joined(ids::TOPIC_CHAT, "需求讨论"))
            .await
            .expect("ev")
            .expect("change");
    let d2 = c2.decision_id.expect("新群 decision");
    decisions::dispatch(h.ch.modules(), &me, &d2, "block", "")
        .expect("ours")
        .expect("resolved");
    let (_, sp) = h
        .call("GET", &format!("/spaces/{}", c2.space_id), None)
        .await;
    assert_eq!(sp["attention"], "blocked");
    assert_eq!(sp["kind"], "topic_group");
    let c3 =
        h.ch.on_space_event(WS, &con, joined(ids::EXTERNAL_CHAT, "客户对接群"))
            .await
            .expect("ev")
            .expect("change");
    let d3 = c3.decision_id.expect("新群 decision");
    let (st, _) = h
        .call(
            "PATCH",
            &format!("/spaces/{}", c3.space_id),
            Some(json!({"attention": "quiet"})),
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(
        h.n(
            "SELECT count(*) FROM decisions WHERE id = ? AND status = 'expired'",
            &d3
        ),
        1
    );
}

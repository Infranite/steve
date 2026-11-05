//! Names for the people the channels meet on the fake Feishu: a sender known
//! only by an `ou_…` id is named through your identity first (chat members), then the bot's; the
//! address book names people by id and a shared `union_id` merges an unconfirmed person into the
//! known one; @mentions and private chats name people without any lookup; `tenant_key` becomes the
//! own organization or one organization per external tenant; a permission granted later is used on
//! the next run; 待确认 shows unknown people with where they were seen and leaves muted spaces out;
//! the directory sync reports the visibility range and the missing permissions.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::auth::{FeishuApp, FeishuUser};
use steve_channels::adapters::feishu::client::is_real_host;
use steve_channels::adapters::feishu::directory::{self, ADAPTER, NAME};
use steve_channels::adapters::feishu::user_routes;
use steve_channels::fake::feishu::{Chat, Endpoint, FakeFeishu, Member, ids};
use steve_channels::{Channels, HandleRef, InboundMessage, Mention, SpaceKind};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::Caller;
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
const ME_BOT_CHAT: &str = "oc_mebot000000000000000000000001";

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: FakeFeishu,
    ch: Channels,
    conns: Connections,
    sink: Arc<dyn steve_flow::EventSink>,
    bot_con: String,
    self_con: String,
}

async fn send(
    r: &Router,
    who: &Caller,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value, axum::http::HeaderMap) {
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
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v, headers)
}

async fn setup() -> H {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    fake.world(|w| w.tenant_name = "示例科技".into());
    assert!(!is_real_host(fake.base_url()));
    // your p2p chat with the bot
    fake.world(|w| {
        w.chats.push(Chat {
            chat_id: ME_BOT_CHAT.into(),
            name: String::new(),
            description: String::new(),
            chat_mode: "p2p".into(),
            chat_type: "private".into(),
            owner_open_id: String::new(),
            external: false,
            bot_in_chat: true,
            chat_status: "normal".into(),
        });
        let tk = w.app.tenant_key.clone();
        w.members.push((
            ME_BOT_CHAT.into(),
            Member {
                ids: ids::OWNER.into(),
                name: "李鹏翔".into(),
                tenant_key: tk,
            },
        ));
    });
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let app = Arc::new(FeishuApp::new());
    let user = Arc::new(
        FeishuUser::with_base(fake.base_url())
            .with_party_hooks(steve_modules::accounts::ports().hooks),
    );
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec![ORIGIN.into()], true, |_| None),
    )
    .provider(app.clone())
    .provider(user.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let sink: Arc<dyn steve_flow::EventSink> = Arc::new(flow);
    let ch = Channels::builder(env.modules.clone(), conns.clone(), sink.clone())
        .adapter(app.clone())
        .profiles(user.clone())
        .profiles(app)
        .build();

    let api: Router = connections::routes(conns.clone())
        .merge(user_routes(conns.clone(), user.claims().clone()))
        .with_state(env.modules.clone());
    let cb: Router = connections::callback_routes(conns.clone()).with_state(env.modules.clone());
    let me = caller(WS, ME);
    let (st, v, _) = send(
        &api,
        &me,
        "POST",
        "/connections",
        Some(json!({
            "adapter": "feishu_app",
            "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
            "config": {"api_base": fake.base_url()},
        })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let bot_con = v["id"].as_str().expect("id").to_string();

    // “授权你本人”: start → the fake's consent page → our callback
    let (st, v, _) = send(
        &api,
        &me,
        "POST",
        "/channels/feishu/user/oauth/start",
        Some(json!({"app_connection": bot_con})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let url = v["url"].as_str().expect("url").to_string();
    assert!(url.starts_with(fake.base_url()), "fake hosts only: {url}");
    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
        .get(&url)
        .send()
        .await
        .expect("authorize");
    let back =
        url::Url::parse(res.headers()[header::LOCATION].to_str().expect("loc")).expect("url");
    let p = format!("{}?{}", back.path(), back.query().unwrap_or_default());
    let (st, _, hd) = send(&cb, &me, "GET", &p, None).await;
    assert_eq!(st, 302);
    let loc = hd[header::LOCATION].to_str().expect("loc").to_string();
    let self_acc = loc
        .strip_prefix("/#/accounts/")
        .unwrap_or_else(|| panic!("not a success: {loc}"))
        .to_string();
    let self_con = str_of(
        &q(
            &env,
            "SELECT id FROM connections WHERE account_id = ?",
            &self_acc,
        )[0][0],
    );
    let caps = str_of(&q(&env, "SELECT caps FROM connections WHERE id = ?", &self_con)[0][0]);
    assert!(caps.contains("msg.receive"), "{caps}");
    let _ = (user, self_acc);
    H {
        env,
        _keys: keys,
        fake,
        ch,
        conns,
        sink,
        bot_con,
        self_con,
    }
}

fn q(env: &TestEnv, sql: &str, p: &str) -> Vec<Vec<Value>> {
    let (sql, p) = (sql.to_string(), p.to_string());
    env.store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let rows = st
                .query_map([&p], |r| {
                    (0..n)
                        .map(|i| {
                            Ok(match r.get_ref(i)? {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(x) => json!(x),
                                rusqlite::types::ValueRef::Text(t) => {
                                    json!(String::from_utf8_lossy(t))
                                }
                                _ => Value::Null,
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(rows)
        })
        .expect("query")
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

const MEMBERS: &str = "im:chat.members:read";
const CONTACT: &str = "contact:user.base:readonly";

/// A message from `who` (a bare `open_id`, as your self poller sees senders) in `chat`.
async fn hear(h: &H, chat: &str, kind: SpaceKind, name: &str, id: &str, who: &str, text: &str) {
    let mut m = InboundMessage::text(
        chat,
        id,
        HandleRef::new("feishu", who),
        text,
        1_793_600_000_000,
    );
    m.space_kind = Some(kind);
    m.space_name = Some(name.into());
    h.ch.on_inbound(WS, &h.self_con, m).await.expect("inbound");
}

/// The account with this handle: `(display_name, party name, contact_kind, party id)`.
fn person(h: &H, handle: &str) -> (String, String, String, String) {
    let r = q(
        &h.env,
        "SELECT a.display_name, p.name, coalesce(p.contact_kind, ''), p.id FROM account_handles x
           JOIN accounts a ON a.id = x.account_id JOIN parties p ON p.id = a.party_id
          WHERE x.handle = ?",
        handle,
    );
    assert!(!r.is_empty(), "no account for {handle}");
    (
        str_of(&r[0][0]),
        str_of(&r[0][1]),
        str_of(&r[0][2]),
        str_of(&r[0][3]),
    )
}

/// The organization a person works at: `(org name, link source)`.
fn employer(h: &H, party: &str) -> Option<(String, String)> {
    q(
        &h.env,
        "SELECT o.name, l.source FROM links l JOIN parties o ON o.id = l.object_id
          WHERE l.subject_id = ? AND l.predicate = 'works_at'",
        party,
    )
    .first()
    .map(|r| (str_of(&r[0]), str_of(&r[1])))
}

/// `status` of a permission row on a connection.
fn permission(h: &H, conn: &str, scope: &str) -> Option<String> {
    let cfg = q(&h.env, "SELECT config FROM connections WHERE id = ?", conn);
    let cfg: Value = serde_json::from_str(&str_of(&cfg[0][0])).expect("config");
    cfg["permissions"]
        .as_array()?
        .iter()
        .find(|r| r["scope"] == scope)
        .map(|r| str_of(&r["status"]))
}

fn requests(h: &H, ep: Endpoint) -> usize {
    h.fake.requests_to(ep).len()
}

#[tokio::test(flavor = "multi_thread")]
async fn your_identity_names_members_first_and_tenants_become_organizations() {
    let h = setup().await;
    // as in production: the bot lacks the members permission, you have it
    h.fake.deny_bot(Endpoint::ChatMembers);
    h.fake.grant_user_scopes(ids::OWNER.open_id, &[MEMBERS]);
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_n1",
        ids::ALICE.open_id,
        "今晚谁值班？",
    )
    .await;
    hear(
        &h,
        ids::EXTERNAL_CHAT,
        SpaceKind::Group,
        "客户对接群",
        "om_n2",
        ids::EXTERNAL_USER.open_id,
        "合同什么时候签",
    )
    .await;
    let (_, name, kind, _) = person(&h, ids::ALICE.open_id);
    assert_eq!(
        (name.as_str(), kind.as_str()),
        (ids::ALICE.open_id, "unconfirmed")
    );

    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.candidates, 2);
    assert_eq!(r.named_remote, 2, "{r:?}");
    assert_eq!(r.promoted, 2);
    assert_eq!(r.nameless, 0);

    let (display, name, kind, alice) = person(&h, ids::ALICE.open_id);
    assert_eq!(
        (display.as_str(), name.as_str(), kind.as_str()),
        ("王小丽", "王小丽", "real")
    );
    // the own tenant is the own organization, renamed from the workspace's placeholder
    // she is of the own organization: the address book is asked too, so the job is the directory's,
    // with her title, and her work email is hers
    let (org, src) = employer(&h, &alice).expect("works somewhere");
    assert_eq!((org.as_str(), src.as_str()), ("示例科技", "sync"));
    assert_eq!(
        q(
            &h.env,
            "SELECT json_extract(props, '$.title') FROM links WHERE subject_id = ? AND predicate = 'works_at'",
            &alice
        )[0][0],
        "后端工程师"
    );
    assert_eq!(
        q(
            &h.env,
            "SELECT handle FROM accounts WHERE party_id = ? AND family = 'email'",
            &alice
        )[0][0],
        "alice@corp.example.com"
    );
    assert_eq!(
        q(
            &h.env,
            "SELECT gaps FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?",
            &alice
        )[0][0],
        "{}"
    );
    assert_eq!(
        q(
            &h.env,
            "SELECT org_kind FROM parties WHERE name = ?",
            "示例科技"
        )[0][0],
        "self"
    );
    // the external member: real, at an organization of their own tenant
    let (_, name, kind, ext) = person(&h, ids::EXTERNAL_USER.open_id);
    assert_eq!((name.as_str(), kind.as_str()), ("赵客户", "real"));
    let (org, _) = employer(&h, &ext).expect("external org");
    assert_eq!(org, "外部组织（客户对接群 里的成员）");
    // their title and email: Feishu gives none for another organization's members; never retried
    let row = &q(
        &h.env,
        "SELECT ap.gaps, ap.next_check, ap.source FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?",
        &ext,
    )[0];
    let gaps: Value = serde_json::from_str(&str_of(&row[0])).unwrap();
    assert_eq!(
        gaps,
        json!({"title": {"reason": "external"}, "email": {"reason": "external"}})
    );
    assert_eq!(
        (row[1].clone(), row[2].clone()),
        (Value::Null, json!("chat"))
    );
    assert_eq!(
        q(
            &h.env,
            "SELECT count(*) FROM account_handles WHERE family = 'feishu_tenant' AND handle = ?",
            ids::EXTERNAL_TENANT_KEY
        )[0][0],
        1
    );
    // the change log says it was the sync
    let rev = q(
        &h.env,
        "SELECT patch FROM revisions WHERE object_id = ? ORDER BY id DESC LIMIT 1",
        &alice,
    );
    assert!(
        str_of(&rev[0][0]).contains(r#""source":"sync""#),
        "{:?}",
        rev
    );

    // you were asked, the bot never had to be (its missing permission is not even touched)
    // (客户对接群 has Alice too: when it is read first, one list names both)
    let calls = h.fake.requests_to(Endpoint::ChatMembers);
    assert!((1..=2).contains(&calls.len()), "{}", calls.len());
    assert!(calls.iter().all(|c| c.code == 0));
    assert_eq!(
        permission(&h, &h.self_con, MEMBERS).as_deref(),
        Some("granted")
    );

    // a second run finds nothing to do and asks nobody
    let before = h.fake.requests().len();
    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.candidates, 0);
    assert_eq!(h.fake.requests().len(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bot_answers_when_you_lack_the_permission_and_that_is_recorded() {
    let h = setup().await;
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_b1",
        ids::BOB.open_id,
        "发布延期了",
    )
    .await;
    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.named_remote, 1, "{r:?}");
    assert_eq!(person(&h, ids::BOB.open_id).1, "张伟");
    // your token was refused for lack of the scope: the 接入 page can offer it
    assert_eq!(
        permission(&h, &h.self_con, MEMBERS).as_deref(),
        Some("missing")
    );
    assert!(
        r.missing
            .contains(&(h.self_con.clone(), MEMBERS.to_string()))
    );
    let cfg = q(
        &h.env,
        "SELECT json_extract(config, '$.missing_permissions') FROM connections WHERE id = ?",
        &h.self_con,
    );
    assert!(str_of(&cfg[0][0]).contains(MEMBERS));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_address_book_names_by_id_and_a_shared_union_id_merges() {
    let h = setup().await;
    // nobody may list members; Bob is already a known contact by his union_id
    h.fake.deny(Endpoint::ChatMembers);
    let known = h
        .env
        .store()
        .transaction(|tx| {
            let id = steve_modules::accounts::identify(
                tx,
                WS,
                ME,
                "feishu",
                ids::BOB.union_id,
                &steve_modules::accounts::Hints {
                    display_name: Some("张伟".into()),
                    ..Default::default()
                },
            )?;
            tx.execute(
                "UPDATE parties SET contact_kind = 'real' WHERE id = ?",
                [&id.party_id],
            )?;
            Ok::<_, steve_modules::ApiError>(id.party_id)
        })
        .expect("known bob");
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_d1",
        ids::BOB.open_id,
        "合同改好了",
    )
    .await;
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_d2",
        ids::ALICE.open_id,
        "收到",
    )
    .await;
    assert_ne!(person(&h, ids::BOB.open_id).3, known);

    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.merged, 1, "{r:?}");
    // the unconfirmed ou_ person went into the known 张伟
    assert_eq!(person(&h, ids::BOB.open_id).3, known);
    // Alice: from the directory, so a synced real contact with her avatar and email account
    let (_, name, kind, alice) = person(&h, ids::ALICE.open_id);
    assert_eq!((name.as_str(), kind.as_str()), ("王小丽", "real"));
    assert_eq!(
        q(&h.env, "SELECT source FROM parties WHERE id = ?", &alice)[0][0],
        "sync"
    );
    assert!(
        str_of(&q(&h.env, "SELECT avatar FROM parties WHERE id = ?", &alice)[0][0])
            .contains("https://")
    );
    assert_eq!(
        person(&h, ids::ALICE.union_id).3,
        alice,
        "the union_id is recorded too"
    );
    // you lack the address-book scope (recorded), the bot has it
    assert_eq!(
        permission(&h, &h.self_con, CONTACT).as_deref(),
        Some("missing")
    );
    assert!(requests(&h, Endpoint::BatchUsers) >= 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn mentions_and_private_chats_name_people_without_any_lookup() {
    let h = setup().await;
    // a Channels without profile identities: only what Steve already has
    let local = Channels::builder(h.env.modules.clone(), h.conns.clone(), h.sink.clone()).build();
    let send = |m: InboundMessage| {
        let local = local.clone();
        let con = h.self_con.clone();
        async move { local.on_inbound(WS, &con, m).await.expect("inbound") }
    };
    // Alice speaks first (no name), later Bob @-mentions her by name
    let mut a = InboundMessage::text(
        ids::GROUP_CHAT,
        "om_m1",
        HandleRef::new("feishu", ids::ALICE.open_id),
        "在",
        1,
    );
    a.space_kind = Some(SpaceKind::Group);
    a.raw = json!({"sender": {"sender_type": "user", "tenant_key": ids::TENANT_KEY}});
    send(a).await;
    let mut b = InboundMessage::text(
        ids::GROUP_CHAT,
        "om_m2",
        HandleRef::new("feishu", ids::BOB.open_id),
        "@王小丽 看下",
        2,
    );
    b.space_kind = Some(SpaceKind::Group);
    b.mentions = vec![Mention {
        handle: Some(HandleRef::new("feishu", ids::ALICE.open_id)),
        name: "王小丽".into(),
        is_connection: false,
    }];
    send(b).await;
    // a private chat named after the other party, whose sender came without a name
    let mut d = InboundMessage::text(
        "oc_dm_old",
        "om_m3",
        HandleRef::new("feishu", ids::CAROL.open_id),
        "周五见",
        3,
    );
    d.space_kind = Some(SpaceKind::Dm);
    d.space_name = Some("陈卡罗".into());
    d.raw = json!({"sender": {"sender_type": "user", "tenant_key": ids::EXTERNAL_TENANT_KEY}});
    send(d).await;
    let before = h.fake.requests().len();

    let r = local.resolve_names(WS).await.expect("names");
    assert_eq!(r.named_local, 2, "{r:?}");
    assert_eq!(r.lookups, 0);
    assert_eq!(h.fake.requests().len(), before, "no request to Feishu");
    assert_eq!(person(&h, ids::ALICE.open_id).1, "王小丽");
    assert_eq!(person(&h, ids::CAROL.open_id).1, "陈卡罗");
    // Bob was never named anywhere: still nameless, and not shown by his id
    assert_eq!(r.nameless, 1);
    // the stored payloads carry the organization: Alice at the own one, Carol at another tenant's
    assert_eq!(r.orgs_linked, 2, "{r:?}");
    let alice = person(&h, ids::ALICE.open_id).3;
    assert_eq!(
        q(
            &h.env,
            "SELECT o.org_kind FROM links l JOIN parties o ON o.id = l.object_id WHERE l.subject_id = ? AND l.predicate = 'works_at'",
            &alice
        )[0][0],
        "self"
    );
    let carol = person(&h, ids::CAROL.open_id).3;
    assert_eq!(
        employer(&h, &carol).map(|e| e.0).as_deref(),
        Some("外部组织（陈卡罗 里的成员）")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_permission_granted_later_is_used_on_the_next_run() {
    let h = setup().await;
    h.fake.deny_bot(Endpoint::ChatMembers);
    h.fake.deny_bot(Endpoint::BatchUsers);
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_r1",
        ids::BOB.open_id,
        "在吗",
    )
    .await;
    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.nameless, 1, "{r:?}");
    assert_eq!(
        permission(&h, &h.self_con, MEMBERS).as_deref(),
        Some("missing")
    );
    assert!(
        r.missing
            .contains(&(h.bot_con.clone(), MEMBERS.to_string()))
    );

    // the user re-authorizes with the scope; the connection event kicks a fresh run
    h.fake.grant_user_scopes(ids::OWNER.open_id, &[MEMBERS]);
    h.ch.kick_names(WS, true);
    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.nameless, 0, "{r:?}");
    assert_eq!(person(&h, ids::BOB.open_id).1, "张伟");
    assert_eq!(
        permission(&h, &h.self_con, MEMBERS).as_deref(),
        Some("granted")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_shows_unknown_people_with_context_and_leaves_muted_spaces_out() {
    let h = setup().await;
    h.fake.deny(Endpoint::ChatMembers);
    h.fake.deny(Endpoint::BatchUsers);
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_p1",
        ids::BOB.open_id,
        "明天的发布要延期吗？",
    )
    .await;
    hear(
        &h,
        ids::TOPIC_CHAT,
        SpaceKind::Channel,
        "需求讨论",
        "om_p2",
        ids::CAROL.open_id,
        "【告警】磁盘 92%",
    )
    .await;
    h.ch.resolve_names(WS).await.expect("names");
    // 需求讨论 is a notification group you set to 不提醒 (migration 019 of fix/channels-ux)
    h.env
        .store()
        .transaction(|tx| {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS space_prefs (space_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
                   attention TEXT NOT NULL DEFAULT 'realtime', voice TEXT, updated INTEGER NOT NULL);
                 INSERT INTO space_prefs (space_id, workspace_id, attention, updated)
                   SELECT id, workspace_id, 'quiet', 0 FROM spaces WHERE name = '需求讨论';",
            )?;
            Ok::<_, steve_store::StoreError>(())
        })
        .expect("prefs");
    let mut ports = steve_modules::accounts::ports();
    ports.spaces = Some(Arc::new(steve_channels::spaces::Spaces));
    let p = steve_modules::contacts::merge::pending(&h.env.modules, &ports, &caller(WS, ME))
        .expect("pending");
    let v = serde_json::to_value(&p).expect("json");
    assert_eq!(v["quiet"], 1, "{v}");
    let list = v["unconfirmed"].as_array().expect("list");
    assert_eq!(list.len(), 1, "{v}");
    let bob = &list[0];
    assert_eq!(bob["label"], "未知飞书用户");
    assert_eq!(bob["nameless"], true);
    assert_eq!(bob["seen"]["spaces"][0]["name"], "平台值班群");
    assert_eq!(bob["seen"]["last"]["text"], "明天的发布要延期吗？");
    assert_eq!(bob["seen"]["messages"], 1);
    assert!(!bob["label"].as_str().unwrap_or("").starts_with("ou_"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_directory_sync_reports_its_visibility_range_and_missing_permissions() {
    let h = setup().await;
    use steve_modules::contacts::sync::{DirectoryConnections, DirectorySync, TableConnections};
    let tables =
        Arc::new(TableConnections::new().with(ADAPTER, NAME, directory::factory(h.conns.clone())));
    let sync = DirectorySync::new(h.env.modules.clone(), tables.clone());
    let conn = h
        .env
        .store()
        .reader(|c| tables.get(c, WS, &h.bot_con))
        .expect("get")
        .expect("a directory connection");

    // the app sees only the platform department: teams with the department path, and a note
    h.fake
        .world(|w| w.scope_department_ids = vec![ids::DEPT_PLATFORM.into()]);
    sync.sync_connection(&conn).await.expect("sync");
    let state = q(
        &h.env,
        "SELECT json_extract(config, '$.directory') FROM connections WHERE id = ?",
        &h.bot_con,
    );
    let state: Value = serde_json::from_str(&str_of(&state[0][0])).expect("state");
    assert_eq!(state["ok"], true);
    assert_eq!(state["scope"]["whole"], false);
    assert_eq!(state["scope"]["departments"], 1);
    let st: steve_modules::contacts::sync::DirectoryState =
        serde_json::from_value(state).expect("state");
    assert_eq!(
        steve_modules::contacts::teams::directory_note(&st).as_deref(),
        Some("应用可见范围只包含 1 个部门")
    );

    // the whole company, with the department path on the job
    h.fake.world(|w| w.scope_department_ids = vec!["0".into()]);
    sync.sync_connection(&conn).await.expect("sync");
    let depts = q(
        &h.env,
        "SELECT json_extract(props, '$.dept') FROM links WHERE predicate = 'works_at' AND source = ?",
        "sync",
    );
    assert!(
        depts.iter().any(|d| str_of(&d[0]).contains(" / ")),
        "a department path: {depts:?}"
    );

    // the departments permission is withdrawn: the sync fails and says which scope
    h.fake.deny(Endpoint::DepartmentChildren);
    assert!(sync.sync_connection(&conn).await.is_err());
    assert_eq!(
        permission(&h, &h.bot_con, "contact:department.base:readonly").as_deref(),
        Some("missing")
    );
    let state = q(
        &h.env,
        "SELECT json_extract(config, '$.directory.missing_scopes') FROM connections WHERE id = ?",
        &h.bot_con,
    );
    assert!(str_of(&state[0][0]).contains("contact:department.base:readonly"));
}

/// Why someone stays unknown comes from the calls that actually asked about them: refused by every
/// identity asked → the permission; a call skipped as cached is no new evidence; once an identity
/// may search, the person is found.
#[tokio::test(flavor = "multi_thread")]
async fn missing_reasons_come_from_the_calls_that_asked() {
    let h = setup().await;
    // no member lists, and no identity may read the address book (you never were granted it)
    h.fake.deny(Endpoint::ChatMembers);
    h.fake.deny_bot(Endpoint::BatchUsers);
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_r1",
        ids::BOB.open_id,
        "在吗",
    )
    .await;
    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.nameless, 1, "{r:?}");
    let gaps_of = |h: &H| {
        let row = q(
            &h.env,
            "SELECT ap.gaps, ap.checked FROM account_profiles ap JOIN account_handles x ON x.account_id = ap.account_id WHERE x.handle = ?",
            ids::BOB.open_id,
        );
        (
            serde_json::from_str::<Value>(&str_of(&row[0][0])).unwrap(),
            row[0][1].clone(),
        )
    };
    let (gaps, checked) = gaps_of(&h);
    assert_eq!(
        gaps["name"],
        json!({"reason": "permission", "scope": CONTACT})
    );

    // again at once: every call is cached, so nothing is asked and nothing is judged anew
    let before = h.fake.requests().len();
    h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(h.fake.requests().len(), before);
    assert_eq!(gaps_of(&h).1, checked);

    // the bot may read the address book now (a connection changed: everything is asked again)
    h.fake.allow(Endpoint::BatchUsers);
    h.ch.kick_names(WS, true);
    let r = h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(r.nameless, 0, "{r:?}");
    assert_eq!(person(&h, ids::BOB.open_id).1, "张伟");
}

/// The `next_check` the contact page shows is kept: a due retry is asked even while the lookup caches
/// (6 h member lists, 24 h address book) still call the last answer fresh; before it is due nothing
/// is asked; a profile that became complete is not retried.
#[tokio::test(flavor = "multi_thread")]
async fn a_due_retry_is_made_and_nothing_else() {
    let h = setup().await;
    // you may list members, nobody may read the address book: 张伟 gets a name and an organization,
    // his title and email stay open for the missing permission
    h.fake.grant_user_scopes(ids::OWNER.open_id, &[MEMBERS]);
    h.fake.deny_bot(Endpoint::ChatMembers);
    h.fake.deny_bot(Endpoint::BatchUsers);
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_t1",
        ids::BOB.open_id,
        "报价发了",
    )
    .await;
    h.ch.resolve_names(WS).await.expect("names");
    let (_, name, _, bob) = person(&h, ids::BOB.open_id);
    assert_eq!(name, "张伟");
    let record = |h: &H| {
        let r = q(
            &h.env,
            "SELECT ap.checked, ap.next_check, ap.gaps FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?",
            &bob,
        );
        (
            r[0][0].as_i64(),
            r[0][1].as_i64(),
            serde_json::from_str::<Value>(&str_of(&r[0][2])).unwrap(),
        )
    };
    let (checked, next, gaps) = record(&h);
    let t0 = checked.expect("checked");
    assert_eq!(next, Some(t0 + 3_600_000), "retried in an hour");
    assert_eq!(
        gaps["title"],
        json!({"reason": "permission", "scope": CONTACT})
    );
    let calls = |h: &H| requests(h, Endpoint::BatchUsers) + requests(h, Endpoint::ChatMembers);
    // your member-list reads (user token): the cache that answered first and is fresh for 6 h
    let yours = |h: &H| {
        h.fake
            .requests_to(Endpoint::ChatMembers)
            .iter()
            .filter(|r| r.token.as_deref().is_some_and(|t| t.starts_with("u-")))
            .count()
    };
    let yours0 = yours(&h);
    assert_eq!(yours0, 1);

    // not due yet: nothing is asked, nothing recorded
    let before = calls(&h);
    h.env.clock.advance(30 * 60_000);
    h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(calls(&h), before);
    assert_eq!(record(&h).0, Some(t0));

    // due (both caches still fresh): asked, recorded, next one 6 h on
    let t1 = h.env.clock.advance(31 * 60_000);
    h.ch.resolve_names(WS).await.expect("names");
    assert!(calls(&h) > before, "the due retry was made");
    assert_eq!(yours(&h), yours0 + 1, "through the fresh cache too");
    let (checked, next, _) = record(&h);
    assert_eq!((checked, next), (Some(t1), Some(t1 + 6 * 3_600_000)));

    // the bot may read the address book now (no reconnect): at the next due retry he is complete …
    h.fake.allow(Endpoint::BatchUsers);
    let before = calls(&h);
    h.env.clock.advance(3 * 3_600_000);
    h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(calls(&h), before, "not due before the 6 h");
    h.env.clock.advance(3 * 3_600_000 + 60_000);
    h.ch.resolve_names(WS).await.expect("names");
    let (_, next, gaps) = record(&h);
    assert_eq!((next, gaps), (None, json!({})), "complete: no retry");
    assert_eq!(
        q(
            &h.env,
            "SELECT json_extract(props, '$.title') FROM links WHERE subject_id = ? AND predicate = 'works_at'",
            &bob
        )[0][0],
        "销售"
    );
    // … and never asked about again
    let before = calls(&h);
    h.env.clock.advance(7 * 24 * 3_600_000);
    h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(calls(&h), before);
}

/// The address book answers but without a title or email (not filled in, or not opened to the app):
/// “not provided”, retried in an hour — and that retry asks the address book again although its
/// answer is cached for 24 h; before it is due, nothing is asked.
#[tokio::test(flavor = "multi_thread")]
async fn a_due_retry_asks_the_address_book_through_its_cache() {
    let h = setup().await;
    // the bot reads the address book, where 张伟 has no title and no email
    h.fake.world(|w| {
        let u = w
            .users
            .iter_mut()
            .find(|u| u.ids.open_id == ids::BOB.open_id)
            .unwrap();
        u.job_title.clear();
        u.email.clear();
        u.enterprise_email.clear();
    });
    hear(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "平台值班群",
        "om_c1",
        ids::BOB.open_id,
        "在",
    )
    .await;
    // the group is no longer active, so no member list is read: he is looked up by id in the address
    // book alone, whose answer is what the 24 h handle cache keeps
    h.env
        .store()
        .transaction(|tx| -> Result<(), steve_store::StoreError> {
            tx.execute(
                "UPDATE spaces SET status = 'left' WHERE external_id = ?",
                [ids::GROUP_CHAT],
            )?;
            Ok(())
        })
        .expect("space left");
    // the bot's address-book reads (tenant token) that found him
    let bots = |h: &H| {
        h.fake
            .requests_to(Endpoint::BatchUsers)
            .iter()
            .filter(|r| r.code == 0 && r.token.as_deref().is_some_and(|t| t.starts_with("t-")))
            .count()
    };
    h.ch.resolve_names(WS).await.expect("names");
    let (_, name, _, bob) = person(&h, ids::BOB.open_id);
    assert_eq!(name, "张伟");
    assert_eq!(bots(&h), 1, "answered: the 24 h cache is written");
    let r = q(
        &h.env,
        "SELECT ap.checked, ap.next_check, ap.gaps, ap.source FROM account_profiles ap JOIN accounts a ON a.id = ap.account_id WHERE a.party_id = ?",
        &bob,
    );
    let t0 = r[0][0].as_i64().unwrap();
    assert_eq!(r[0][1].as_i64(), Some(t0 + 3_600_000));
    assert_eq!(
        serde_json::from_str::<Value>(&str_of(&r[0][2])).unwrap(),
        json!({"title": {"reason": "not_provided"}, "email": {"reason": "not_provided"}})
    );
    assert_eq!(r[0][3], "directory");

    // not due: not asked
    h.env.clock.advance(59 * 60_000);
    h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(bots(&h), 1);
    // due, the cached answer 1 h old: asked again
    h.env.clock.advance(2 * 60_000);
    h.ch.resolve_names(WS).await.expect("names");
    assert_eq!(bots(&h), 2, "the due retry went through the cache");
    assert_eq!(
        requests(&h, Endpoint::ChatMembers),
        0,
        "only the address book was asked"
    );
}

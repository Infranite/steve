//! F3 and F4 on the fake Feishu: the bot connected through the real connect route; sending,
//! replying (`<at>`, `reply_in_thread`), rate-limit retries with one `uuid`, recall; group info,
//! paged members, external members, the bot's groups, and member / chat events arriving over the
//! long connection.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::inbound::LongConnection;
use steve_channels::adapters::feishu::ws::WsState;
use steve_channels::adapters::feishu::{FeishuApp, at_handle, chats, send};
use steve_channels::fake::feishu::{
    Chat, Endpoint, FakeFeishu, Member, UserIds, WsClientConfig, fixture, ids,
};
use steve_channels::{ChannelAdapter, Channels, OutContent, SendTarget, SpaceEvent};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, ConnCx, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const T: Duration = Duration::from_secs(10);

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: FakeFeishu,
    app: Arc<FeishuApp>,
    ch: Channels,
    con: String,
}

async fn setup() -> H {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    fake.ws_set_client_config(WsClientConfig {
        reconnect_count: -1,
        reconnect_interval: 1,
        reconnect_nonce: 0,
        ping_interval: 1,
    });
    // rate-limit answers say “try again now”: the tests do not wait seconds
    fake.set_rate_limit_reset(0);
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
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec!["http://hub.test:8440".into()], true, |_| None),
    )
    .provider(app.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .adapter(app.clone())
        .build();

    let api: Router = connections::routes(conns).with_state(env.modules.clone());
    let body = json!({
        "adapter": "feishu_app",
        "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
        "config": {"api_base": fake.base_url()},
    });
    let mut req = Request::builder()
        .method("POST")
        .uri("/connections")
        .header(header::HOST, "hub.test:8440")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    req.extensions_mut().insert(caller(WS, ME));
    let res = api.oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let v: Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body"),
    )
    .expect("json");
    assert_eq!(status, 200, "{v}");
    H {
        env,
        _keys: keys,
        fake,
        app,
        ch,
        con: v["id"].as_str().expect("id").into(),
    }
}

impl H {
    fn cx(&self) -> ConnCx {
        self.ch.connections().cx(WS, &self.con).expect("cx")
    }

    fn rows(&self, sql: &str, p: &str) -> Vec<Vec<Value>> {
        let (sql, p) = (sql.to_string(), p.to_string());
        self.env
            .store()
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

    fn n(&self, sql: &str, p: &str) -> i64 {
        self.rows(sql, p)[0][0].as_i64().expect("count")
    }

    /// Waits until the count `sql` reaches `want`.
    async fn until(&self, sql: &str, p: &str, want: i64) {
        let t0 = std::time::Instant::now();
        loop {
            let got = self.n(sql, p);
            if got == want {
                return;
            }
            assert!(t0.elapsed() < T, "{sql} [{p}]: {got} != {want}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn start(&self) -> LongConnection {
        let lc = LongConnection::start(&self.ch, WS, &self.con)
            .await
            .expect("long connection");
        lc.ws
            .wait_for(T, |s| matches!(s, WsState::Connected { .. }))
            .await
            .expect("connected");
        lc
    }

    /// Pushes an envelope over the long connection and waits for its ack.
    async fn push(&self, env: Value) {
        let id = self.fake.ws_push_envelope(env);
        let d = self.fake.ws_wait_acked(&id, T).await.expect("acked");
        assert_eq!(d.ack.expect("ack").code, 200);
    }
}

fn with_event(name: &str, event_id: &str, chat: &str) -> Value {
    let mut env = fixture(name).expect("fixture");
    env["header"]["event_id"] = json!(event_id);
    env["event"]["chat_id"] = json!(chat);
    env
}

// ---------------------------------------------------------------------------------------- F3

#[tokio::test(flavor = "multi_thread")]
async fn group_reply_ats_the_sender_and_topics_reply_in_thread() {
    let h = setup().await;
    let cx = h.cx();

    // a reply in a group, @-ing Alice
    let mut c = OutContent::text("昨晚的告警已经恢复了。");
    c.uuid = Some("cm_reply000000000001".into());
    c.at = vec![ids::ALICE.open_id.into()];
    c.space_ext_id = Some(ids::GROUP_CHAT.into());
    let to = SendTarget::Reply {
        ext_msg_id: "om_botearlier00000000000000000001".into(),
        in_thread: false,
    };
    let id = h.app.send(&cx, &to, &c).await.expect("sent");
    let sent = h.fake.sent();
    assert_eq!(sent.len(), 1);
    let s = &sent[0];
    assert_eq!(s.message_id, id);
    assert_eq!(
        s.reply_to.as_deref(),
        Some("om_botearlier00000000000000000001")
    );
    assert_eq!(s.msg_type, "text");
    assert_eq!(s.uuid.as_deref(), Some("cm_reply000000000001"));
    assert!(!s.reply_in_thread);
    assert_eq!(
        s.text().as_deref(),
        Some(
            format!(
                "<at user_id=\"{}\"></at> 昨晚的告警已经恢复了。",
                ids::ALICE.open_id
            )
            .as_str()
        )
    );

    // in a topic: reply_in_thread
    let to = SendTarget::Reply {
        ext_msg_id: "om_topicroot000000000000000000001".into(),
        in_thread: send::reply_in_thread(
            steve_channels::SpaceKind::Channel,
            Some("omt_fake0000000001"),
        ),
    };
    h.app
        .send(&cx, &to, &OutContent::text("收到"))
        .await
        .expect("sent");
    let s = h.fake.sent().pop().expect("sent");
    assert!(s.reply_in_thread);
    assert_eq!(s.chat_id, ids::TOPIC_CHAT);
    assert!(s.uuid.is_some_and(|u| !u.is_empty()), "a uuid is made up");

    // a new message in the group; one to a person by open_id; long text cut
    let to = SendTarget::Space {
        ext_id: ids::GROUP_CHAT.into(),
    };
    h.app
        .send(&cx, &to, &OutContent::text("字".repeat(5000)))
        .await
        .expect("sent");
    let s = h.fake.sent().pop().expect("sent");
    assert_eq!(s.receive_id_type.as_deref(), Some("chat_id"));
    assert_eq!(s.receive_id.as_deref(), Some(ids::GROUP_CHAT));
    let t = s.text().expect("text");
    assert_eq!(t.chars().count(), send::MAX_TEXT);
    assert!(t.ends_with(send::TRUNCATED));
    let to = SendTarget::Space {
        ext_id: ids::ALICE.open_id.into(),
    };
    h.app
        .send(&cx, &to, &OutContent::text("单独和你说一下"))
        .await
        .expect("sent");
    let s = h.fake.sent().pop().expect("sent");
    assert_eq!(s.receive_id_type.as_deref(), Some("open_id"));
    assert_eq!(s.chat_id, ids::P2P_CHAT);
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limits_wait_and_retry_with_one_uuid() {
    let h = setup().await;
    let cx = h.cx();
    let to = SendTarget::Space {
        ext_id: ids::GROUP_CHAT.into(),
    };
    let mut c = OutContent::text("部署完成");
    c.uuid = Some("cm_limited0000000001".into());

    h.fake.rate_limit_next(Endpoint::SendMessage, 2);
    let r = send::send(&cx, &to, &c).await.expect("sent after retries");
    assert_eq!(r.retries, 2);
    assert_eq!(r.uuid, "cm_limited0000000001");
    let reqs = h.fake.requests_to(Endpoint::SendMessage);
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs.iter().filter(|r| r.code != 0).count(), 2);
    assert_eq!(h.fake.sent().len(), 1, "sent once");

    // C6 retrying the same outbound message: Feishu's uuid de-dup returns the same message
    let again = send::send(&cx, &to, &c).await.expect("sent");
    assert_eq!(again.message_id, r.message_id);
    assert_eq!(h.fake.sent().len(), 1);
    assert!(h.fake.send_attempts().last().expect("attempt").deduplicated);

    // more than three refusals: the caller's error, nothing sent
    h.fake.rate_limit_next(Endpoint::ReplyMessage, 4);
    let to = SendTarget::Reply {
        ext_msg_id: r.message_id.clone(),
        in_thread: false,
    };
    let e = h
        .app
        .send(&cx, &to, &OutContent::text("x"))
        .await
        .expect_err("limited");
    assert!(e.to_string().contains("限流"), "{e}");
    assert_eq!(h.fake.requests_to(Endpoint::ReplyMessage).len(), 4);
    assert_eq!(h.fake.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn recall_deletes_the_bots_own_message() {
    let h = setup().await;
    let cx = h.cx();
    let to = SendTarget::Space {
        ext_id: ids::GROUP_CHAT.into(),
    };
    let id = h
        .app
        .send(&cx, &to, &OutContent::text("发错了"))
        .await
        .expect("sent");
    h.app.recall(&cx, &id).await.expect("recalled");
    assert_eq!(h.fake.deleted(), vec![id.clone()]);
    let reqs = h.fake.requests_to(Endpoint::DeleteMessage);
    assert_eq!(reqs[0].method, "DELETE");
    assert_eq!(reqs[0].path, format!("/open-apis/im/v1/messages/{id}"));
    // again: already recalled; someone else's message: refused; a bad id: no call
    assert!(h.app.recall(&cx, &id).await.is_err());
    assert!(
        h.app
            .recall(&cx, "om_topicroot000000000000000000001")
            .await
            .is_err()
    );
    assert!(h.app.recall(&cx, "om_x/../y").await.is_err());
    assert_eq!(h.fake.requests_to(Endpoint::DeleteMessage).len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn reactions_go_on_and_come_off_as_the_bot() {
    let h = setup().await;
    let cx = h.cx();
    let m = "om_groupmsg0000000000000000000001";
    let ack = h
        .app
        .add_reaction(&cx, m, steve_channels::ReactionKind::Ack)
        .await
        .expect("ack");
    assert_eq!(h.fake.reactions_on(m), ["OnIt"]);
    let reqs = h.fake.requests_to(Endpoint::AddReaction);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(
        reqs[0].path,
        format!("/open-apis/im/v1/messages/{m}/reactions")
    );
    assert!(
        !reqs[0].token.as_deref().unwrap_or("").starts_with("u-"),
        "the tenant token"
    );
    h.app.remove_reaction(&cx, m, &ack).await.expect("removed");
    h.app
        .add_reaction(&cx, m, steve_channels::ReactionKind::Done)
        .await
        .expect("done");
    assert_eq!(h.fake.reactions_on(m), ["DONE"]);
    h.app
        .add_reaction(&cx, m, steve_channels::ReactionKind::Failed)
        .await
        .expect("failed");
    assert_eq!(h.fake.reactions_on(m), ["DONE", "CrossMark"]);
    let del = h.fake.requests_to(Endpoint::DeleteReaction);
    assert_eq!(
        del[0].path,
        format!("/open-apis/im/v1/messages/{m}/reactions/{ack}")
    );
    // a reaction id that is not there, a bad id: errors, no panic
    assert!(h.app.remove_reaction(&cx, m, &ack).await.is_err());
    assert!(h.app.remove_reaction(&cx, m, "rc/../x").await.is_err());
    // the permission is not there: an error the caller logs and moves on from
    h.fake.deny(Endpoint::AddReaction);
    assert!(
        h.app
            .add_reaction(&cx, m, steve_channels::ReactionKind::Ack)
            .await
            .is_err()
    );
    assert_eq!(
        h.app.reaction_emoji(steve_channels::ReactionKind::Done),
        "DONE"
    );
}

// ---------------------------------------------------------------------------------------- F4

fn member(n: usize, tenant: &str) -> Member {
    Member {
        ids: UserIds {
            union_id: format!("on_many{n:026}"),
            user_id: format!("u_many{n}"),
            open_id: format!("ou_many{n:026}"),
        },
        name: format!("成员{n}"),
        tenant_key: tenant.into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn members_are_paged_and_external_ones_marked() {
    let h = setup().await;
    let cx = h.cx();
    h.fake.world(|w| {
        for i in 0..250 {
            w.members
                .push((ids::GROUP_CHAT.into(), member(i, ids::TENANT_KEY)));
        }
    });
    let m = chats::members(&cx, ids::GROUP_CHAT).await.expect("members");
    assert_eq!(m.len(), 253);
    assert_eq!(h.fake.requests_to(Endpoint::ChatMembers).len(), 3);
    assert!(m.iter().all(|x| !x.external && x.handle.system == "feishu"));

    let m = chats::members(&cx, ids::EXTERNAL_CHAT)
        .await
        .expect("members");
    let ext: Vec<_> = m.iter().filter(|x| x.external).collect();
    assert_eq!(ext.len(), 1);
    assert_eq!(ext[0].handle.handle, ids::EXTERNAL_USER.union_id);

    let i = chats::space_info(&cx, ids::TOPIC_CHAT).await.expect("info");
    assert_eq!(i.kind, steve_channels::SpaceKind::Channel);
    assert_eq!(i.name, "需求讨论");
    assert!(
        chats::space_info(&cx, ids::EXTERNAL_CHAT)
            .await
            .unwrap()
            .has_external
    );

    // the bot's groups: no p2p chats, not the group it is not in
    let mut g: Vec<String> = chats::bot_chats(&cx)
        .await
        .expect("chats")
        .into_iter()
        .map(|c| c.chat_id)
        .collect();
    g.sort();
    let mut want = vec![
        ids::GROUP_CHAT.to_string(),
        ids::EXTERNAL_CHAT.into(),
        ids::TOPIC_CHAT.into(),
    ];
    want.sort();
    assert_eq!(g, want);

    // made spaces on connect: configured=0, no decisions; a second run creates nothing
    assert_eq!(chats::sync_bot_chats(&h.ch, WS, &h.con).await.unwrap(), 3);
    assert_eq!(chats::sync_bot_chats(&h.ch, WS, &h.con).await.unwrap(), 0);
    assert_eq!(
        h.n(
            "SELECT count(*) FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE ss.configured = 0 AND s.connection_id = ?",
            &h.con
        ),
        3
    );
    assert_eq!(
        h.n(
            "SELECT count(*) FROM decisions WHERE subject_type = 'space' AND ?1 = ?1",
            ""
        ),
        0
    );
    assert_eq!(
        h.rows(
            "SELECT has_external FROM spaces WHERE external_id = ?",
            ids::EXTERNAL_CHAT
        )[0][0],
        json!(1)
    );
}

const NEW_GROUP: &str = "oc_newgroup0000000000000000000001";

#[tokio::test(flavor = "multi_thread")]
async fn member_and_chat_events_over_the_long_connection() {
    let h = setup().await;
    // the group the bot is added to: Owner, Alice and a customer of another tenant
    h.fake.world(|w| {
        w.chats.push(Chat {
            chat_id: NEW_GROUP.into(),
            name: "新项目群".into(),
            description: String::new(),
            chat_mode: "group".into(),
            chat_type: "private".into(),
            owner_open_id: ids::OWNER.open_id.into(),
            external: false,
            bot_in_chat: true,
            chat_status: "normal".into(),
        });
        for (u, n, tk) in [
            (ids::OWNER, "李鹏翔", ids::TENANT_KEY),
            (ids::ALICE, "王小丽", ids::TENANT_KEY),
            (ids::EXTERNAL_USER, "赵客户", ids::EXTERNAL_TENANT_KEY),
        ] {
            w.members.push((
                NEW_GROUP.into(),
                Member {
                    ids: u.into(),
                    name: n.into(),
                    tenant_key: tk.into(),
                },
            ));
        }
    });
    let lc = h.start().await;
    const SPACE: &str = "SELECT count(*) FROM spaces WHERE external_id = ?";

    // bot.added: the space, its members, the “新群” decision; an external member flags it
    h.push(fixture("bot_added").unwrap()).await;
    h.until(SPACE, NEW_GROUP, 1).await;
    let s = &h.rows(
        "SELECT s.id, s.name, s.kind, s.status, s.has_external, s.member_count, ss.configured, ss.disclosure
           FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.external_id = ?",
        NEW_GROUP,
    )[0];
    assert_eq!(
        &s[1..],
        &[
            json!("新项目群"),
            json!("group"),
            json!("active"),
            json!(1),
            json!(3),
            json!(0),
            json!("reasoning")
        ]
    );
    let space = s[0].as_str().unwrap().to_string();
    assert_eq!(
        h.rows(
            "SELECT question, options FROM decisions WHERE subject_type = 'space' AND subject_id = ?",
            &space
        )[0][0],
        json!("值班小运被拉进了「新项目群」，要设置它在这里做什么吗？")
    );
    assert_eq!(
        h.n(
            "SELECT count(*) FROM space_members WHERE space_id = ?",
            &space
        ),
        4,
        "three people and the bot"
    );

    // the plain group, joined the same way; then Bob leaves, a customer joins, it is renamed
    h.push(with_event(
        "bot_added",
        "evt_f4_added_group",
        ids::GROUP_CHAT,
    ))
    .await;
    h.until(SPACE, ids::GROUP_CHAT, 1).await;
    let group = h.rows(
        "SELECT id FROM spaces WHERE external_id = ?",
        ids::GROUP_CHAT,
    )[0][0]
        .as_str()
        .unwrap()
        .to_string();
    const MEMBERS: &str = "SELECT count(*) FROM space_members WHERE space_id = ?";
    assert_eq!(h.n(MEMBERS, &group), 4);
    h.push(fixture("user_deleted").unwrap()).await;
    h.until(MEMBERS, &group, 3).await;
    assert_eq!(
        h.n(
            "SELECT count(*) FROM space_members m JOIN account_handles ah ON ah.account_id = m.account_id
              WHERE m.space_id = ? AND ah.handle = 'on_bob0000000000000000000000000001'",
            &group
        ),
        0
    );
    let mut added = with_event("user_added", "evt_f4_user_added", ids::GROUP_CHAT);
    added["event"]["external"] = json!(true);
    h.push(added).await;
    h.until(MEMBERS, &group, 4).await;
    h.until("SELECT has_external FROM spaces WHERE id = ?", &group, 1)
        .await;
    h.push(fixture("chat_updated").unwrap()).await;
    h.until(
        "SELECT count(*) FROM spaces WHERE name = '平台值班群（夜班）' AND id = ?",
        &group,
        1,
    )
    .await;

    // the bot removed: left; added back while still unconfigured: active and asked again
    h.push(fixture("bot_deleted").unwrap()).await;
    h.until(
        "SELECT count(*) FROM spaces WHERE status = 'left' AND id = ?",
        &group,
        1,
    )
    .await;
    h.push(with_event(
        "bot_added",
        "evt_f4_readded_group",
        ids::GROUP_CHAT,
    ))
    .await;
    h.until(
        "SELECT count(*) FROM spaces WHERE status = 'active' AND id = ?",
        &group,
        1,
    )
    .await;
    assert_eq!(
        h.n(
            "SELECT count(*) FROM decisions WHERE subject_type = 'space' AND status = 'open' AND subject_id = ?",
            &group
        ),
        1
    );

    // disbanded: archived (the topic group, known from a message first)
    h.push(fixture("message_thread_reply").unwrap()).await;
    h.until(SPACE, ids::TOPIC_CHAT, 1).await;
    h.push(fixture("chat_disbanded").unwrap()).await;
    h.until(
        "SELECT count(*) FROM spaces WHERE status = 'archived' AND external_id = ?",
        ids::TOPIC_CHAT,
        1,
    )
    .await;

    // an event about a space Steve does not know changes nothing
    let r =
        h.ch.on_space_event(
            WS,
            &h.con,
            SpaceEvent::Left {
                space_ext_id: "oc_unknown".into(),
                archived: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(r, None);

    // the @ handle of a member, for C6's group replies
    let alice: String = h.rows(
        "SELECT account_id FROM account_handles WHERE handle = ?",
        ids::ALICE.union_id,
    )[0][0]
        .as_str()
        .unwrap()
        .into();
    let at = h
        .env
        .store()
        .reader(|c| at_handle(c, WS, &h.con, &alice))
        .unwrap();
    assert_eq!(at.as_deref(), Some(ids::ALICE.open_id));
    lc.stop().await.unwrap();
}

//! “在飞书中打开”: `open_url` on the space (`GET /spaces/:id`), its messages (`GET
//! /spaces/:id/messages`, `channel_message` in a conversation), the conversation's badge (list and
//! messages) and the links of `chan_message` decisions ([`open_link::message_urls`]):
//! - a connection that reaches no platform with links gives none;
//! - a channel service that names Feishu (`config.platform`) gives the chat's AppLink, on Feishu
//!   unless its config says Lark;
//! - a native Feishu connection's brand (`config.domain`) decides;
//! - a chat id that is not an open chat id gives none;
//! - a space the viewer does not see gives no decision link.
//! Synthetic ids only.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::{Channels, Inbound, SpaceKind, api, conversations, open_link};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::base::Viewer;
use tempfile::TempDir;

const CHAT: &str = "oc_0000test0001";
const FEISHU: &str = "https://applink.feishu.cn/client/chat/open?openChatId=oc_0000test0001";
const LARK: &str = "https://applink.larksuite.com/client/chat/open?openChatId=oc_0000test0001";

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    api: Router,
    con: String,
}

/// `personal`: the connection is your own account (its spaces only you see), else a bot.
fn setup(personal: Option<&str>) -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
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
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
        .adapter(fake.clone())
        .build();
    let api = api::routes(ch.clone()).with_state(env.modules.clone());
    let (con, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), personal).expect("connection");
    fake.space(
        CHAT,
        "测试群",
        SpaceKind::Group,
        false,
        &[("ualice", "测试甲")],
    );
    fake.space(
        "G-plain",
        "另一个群",
        SpaceKind::Group,
        false,
        &[("ualice", "测试甲")],
    );
    H {
        env,
        _keys: keys,
        ch,
        api,
        con,
    }
}

impl H {
    async fn push(&self, space: &str, id: &str) -> Inbound {
        self.ch
            .on_inbound(
                WS,
                &self.con,
                FakeChannel::message(space, id, "ualice", "看一下"),
            )
            .await
            .expect("inbound")
    }

    /// The connection as another adapter / config would leave it.
    fn connection(&self, adapter: &str, config: Value) {
        let (con, config) = (self.con.clone(), config.to_string());
        self.env
            .store()
            .with_conn(move |c| {
                c.execute(
                    "UPDATE connections SET adapter = ?2, config = ?3 WHERE id = ?1",
                    rusqlite::params![con, adapter, config],
                )?;
                Ok(())
            })
            .expect("update");
    }

    async fn get(&self, path: &str) -> Value {
        let (s, v) = call(&self.api, Some(&caller(WS, ME)), "GET", path, None).await;
        assert_eq!(s, 200, "{path}: {v}");
        v
    }

    /// `(space, its messages, the conversation's badge in the list and with its messages,
    /// the conversation message's chat message)`'s `open_url`s.
    async fn urls(&self, space: &str) -> [Value; 5] {
        let s = self.get(&format!("/spaces/{space}")).await;
        let m = self.get(&format!("/spaces/{space}/messages")).await;
        let viewer = Viewer::bare(WS, ME);
        let list = self
            .env
            .store()
            .reader(|c| conversations::list(c, WS, &viewer))
            .expect("list");
        let row = list
            .iter()
            .find(|r| r["space_id"] == json!(space))
            .cloned()
            .unwrap_or(Value::Null);
        let conv = conversations::messages_of(&self.ch, WS, &viewer, &format!("chan:{space}"))
            .expect("conversation");
        let msgs = m
            .as_array()
            .cloned()
            .unwrap_or_else(|| m["messages"].as_array().cloned().unwrap_or_default());
        assert!(!msgs.is_empty(), "{m}");
        for x in &msgs {
            assert_eq!(
                x["open_url"], s["open_url"],
                "a message links to its chat: {x}"
            );
        }
        [
            s["open_url"].clone(),
            msgs[0]["open_url"].clone(),
            row["open_url"].clone(),
            conv["conversation"]["open_url"].clone(),
            conv["messages"][0]["channel_message"]["open_url"].clone(),
        ]
    }

    /// A conversation turn about chat message `cm` in its space.
    fn turn(&self, space: &str, cm: &str) {
        self.env
            .store()
            .messages()
            .add(
                WS,
                "me",
                "看一下",
                None,
                &json!({"chan": {"message": cm, "space": space}}),
                &format!("chan:{space}"),
            )
            .expect("turn");
    }

    fn decision_links(
        &self,
        who: &str,
        ids: &[String],
    ) -> std::collections::HashMap<String, String> {
        let viewer = Viewer::bare(WS, who);
        self.env
            .store()
            .reader(|c| open_link::message_urls(c, &viewer, ids))
            .expect("links")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_feishu_chat_opens_in_the_feishu_client_on_its_brand() {
    let h = setup(None);
    let m = h.push(CHAT, "om_test1").await;
    h.turn(&m.space_id, &m.message_id);

    // a connection that reaches no platform with links: none anywhere
    let none = h.urls(&m.space_id).await;
    assert!(none.iter().all(Value::is_null), "{none:?}");
    assert!(h.decision_links(ME, &[m.message_id.clone()]).is_empty());

    // a channel service that names Feishu: its AppLink, on Feishu
    h.connection("ext_ws", json!({"platform": "feishu"}));
    assert_eq!(h.urls(&m.space_id).await, [(); 5].map(|_| json!(FEISHU)));
    // … on Lark when its config says so
    h.connection("ext_ws", json!({"platform": "feishu", "domain": "lark"}));
    assert_eq!(h.urls(&m.space_id).await, [(); 5].map(|_| json!(LARK)));

    // a native connection: its brand
    h.connection("feishu_app", json!({"domain": "lark"}));
    assert_eq!(h.urls(&m.space_id).await, [(); 5].map(|_| json!(LARK)));
    h.connection("feishu_app", json!({"domain": "feishu"}));
    assert_eq!(h.urls(&m.space_id).await, [(); 5].map(|_| json!(FEISHU)));

    // a chan_message decision's link: the message's (its chat's on Feishu)
    let links = h.decision_links(ME, &[m.message_id.clone(), "cm_missing".into()]);
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[&m.message_id], FEISHU);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_link_without_an_open_chat_id_or_for_a_space_you_do_not_see() {
    let h = setup(None);
    let plain = h.push("G-plain", "om_test2").await;
    h.turn(&plain.space_id, &plain.message_id);
    h.connection("feishu_app", json!({}));
    let none = h.urls(&plain.space_id).await;
    assert!(none.iter().all(Value::is_null), "{none:?}");

    // your own account's chat: someone else sees no link to it
    let h = setup(Some("utest-me"));
    let mine = h.push(CHAT, "om_test3").await;
    h.connection("feishu_app", json!({}));
    let ids = [mine.message_id.clone()];
    assert_eq!(
        h.decision_links(ME, &ids)
            .get(&mine.message_id)
            .map(String::as_str),
        Some(FEISHU)
    );
    assert!(h.decision_links(OTHER, &ids).is_empty());
}

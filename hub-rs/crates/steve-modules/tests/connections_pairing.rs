//! Pairing and claiming in the connection framework: a provider declares which connections its
//! connections pair with ([`ConnectionProvider::pairing`]), checked on every way in and again in the
//! commit, re-paired with `PATCH /connections/:id {app_connection}`; a provider that proves who
//! signed in ([`ConnectionProvider::claims_self`]) merges the unconfirmed contact holding that
//! identity (primary handle, extra handles, aliases) into “我” before the account is attached.

mod conn_support;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conn_support::*;
use serde_json::{Value, json};
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ConnCx, ConnectionProvider, FieldDef, Owner, Pairing, ProviderError,
    StartCx,
};
use steve_modules::accounts::{Hints, identify};
use steve_modules::testkit::{ME, WS, caller};
use zeroize::Zeroizing;

/// A credential side: `test_bot` (an app bot, no pair) or `test_self` (you, paired with a bot).
struct Side {
    id: &'static str,
    system: &'static str,
    pairing: Option<Pairing>,
    claims: bool,
    account: Mutex<AccountInfo>,
    /// The pair `verify` saw ([`ConnCx::paired`]).
    seen_pair: Mutex<Option<String>>,
    /// [`ConnectionProvider::config_version`].
    version: AtomicU32,
}

const FIELDS: [FieldDef; 1] = [FieldDef::secret("token", "Token")];

impl Side {
    fn bot(handle: &str) -> Self {
        Self {
            id: "test_bot",
            system: "feishu_app",
            pairing: None,
            claims: true,
            account: Mutex::new(
                AccountInfo::new(handle, "测试机器人")
                    .kind("app_bot")
                    .owner(Owner::Bot),
            ),
            seen_pair: Mutex::new(None),
            version: AtomicU32::new(1),
        }
    }

    fn me(id: &'static str, required: bool, claims: bool, handle: &str) -> Self {
        Self {
            id,
            system: "feishu",
            pairing: Some(Pairing {
                with: vec!["test_bot".into()],
                required,
            }),
            claims,
            account: Mutex::new(AccountInfo::new(handle, "李雷")),
            seen_pair: Mutex::new(None),
            version: AtomicU32::new(1),
        }
    }

    fn set(&self, a: AccountInfo) {
        *self.account.lock().expect("lock") = a;
    }
}

#[async_trait]
impl ConnectionProvider for Side {
    fn id(&self) -> &str {
        self.id
    }
    fn system(&self) -> &str {
        self.system
    }
    fn name(&self) -> &str {
        self.id
    }
    fn caps(&self) -> &[&'static str] {
        &["msg.receive"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &FIELDS
    }
    fn pairing(&self) -> Option<Pairing> {
        self.pairing.clone()
    }
    fn claims_self(&self) -> bool {
        self.claims
    }
    /// `config.endpoint`, when there is one: https, on a host the Hub allows, nothing secret in it.
    fn check_config(&self, cx: &StartCx<'_>, config: &Value) -> Result<(), ProviderError> {
        let Some(e) = config.get("endpoint").and_then(Value::as_str) else {
            return Ok(());
        };
        let allowed = cx.var("STEVE_TEST_ENDPOINT_HOSTS").unwrap_or_default();
        let host = e
            .strip_prefix("https://")
            .and_then(|r| r.split('/').next())
            .filter(|h| !h.contains('@'));
        match host {
            Some(h) if !e.contains('?') && allowed.split(',').any(|a| a == h) => Ok(()),
            _ => Err(ProviderError::Config("endpoint".into())),
        }
    }
    fn config_version(&self) -> u32 {
        self.version.load(Ordering::SeqCst)
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        *self.seen_pair.lock().expect("lock") = cx.paired().map(|p| p.id.clone());
        Ok(self.account.lock().expect("lock").clone())
    }
}

fn val(h: &H, sql: &str, p: &str) -> Value {
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(sql, [p], |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => json!(x),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    _ => Value::Null,
                })
            })?)
        })
        .expect("query")
}

fn pair_of(h: &H, conn: &str) -> Value {
    val(
        h,
        "SELECT json_extract(config, '$.app_connection') FROM connections WHERE id = ?",
        conn,
    )
}

async fn create(h: &H, adapter: &str, config: Value) -> (u16, Value) {
    let (s, v, _) = send(
        &h.api,
        Some(&caller(WS, ME)),
        "POST",
        "/connections",
        Some(json!({"adapter": adapter, "fields": {"token": "t-1"}, "config": config})),
        None,
    )
    .await;
    (s, v)
}

async fn patch(h: &H, id: &str, body: Value) -> (u16, Value) {
    let (s, v, _) = send(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/connections/{id}"),
        Some(body),
        None,
    )
    .await;
    (s, v)
}

fn sides(me: Side) -> (H, Arc<Side>, Arc<Side>) {
    let bot = Arc::new(Side::bot("cli_one"));
    let me = Arc::new(me);
    let h = setup(vec![bot.clone(), me.clone()]);
    (h, bot, me)
}

fn me_party(h: &H) -> String {
    val(
        h,
        "SELECT id FROM parties WHERE user_id = ? AND type = 'contact'",
        ME,
    )
    .as_str()
    .expect("me")
    .to_string()
}

/// A contact a channel made for `handle` of `system`, before anyone connected it.
fn seen_by_a_channel(h: &H, system: &str, handle: &str) -> String {
    h.env
        .store()
        .transaction(|tx| identify(tx, WS, ME, system, handle, &Hints::default()))
        .expect("identify")
        .party_id
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pair_is_checked_and_completed_on_the_way_in() {
    let (h, bot, me) = sides(Side::me("test_self", true, false, "on_me"));

    // a required pair with no bot yet: refused before anything is stored
    let (s, v) = create(&h, "test_self", json!({})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("要先接入「test_bot」，再接入这个连接"))
    );
    assert_eq!(
        val(&h, "SELECT COUNT(*) FROM connections WHERE 1 = ?", "1"),
        json!(0)
    );

    // a provider that does not pair takes no pair
    let (s, v) = create(&h, "test_bot", json!({"app_connection": "con_x"})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("这种连接不和别的连接配对"))
    );
    let (s, v) = create(&h, "test_bot", json!({})).await;
    assert_eq!(s, 200, "{v}");
    let bot1 = v["id"].as_str().expect("id").to_string();

    // the only bot is picked; verify sees it
    let (s, v) = create(&h, "test_self", json!({})).await;
    assert_eq!(s, 200, "{v}");
    let mine = v["id"].as_str().expect("id").to_string();
    assert_eq!(pair_of(&h, &mine), json!(bot1));
    assert_eq!(
        me.seen_pair.lock().expect("lock").as_deref(),
        Some(bot1.as_str())
    );
    let cx = h.conns.cx(WS, &mine).expect("cx");
    let p = cx.paired().expect("paired");
    assert_eq!(
        (p.id.as_str(), p.adapter.as_str(), p.ok),
        (bot1.as_str(), "test_bot", true)
    );

    // two bots: a required pair must be named
    bot.set(
        AccountInfo::new("cli_two", "第二个机器人")
            .kind("app_bot")
            .owner(Owner::Bot),
    );
    let (s, v) = create(&h, "test_bot", json!({})).await;
    assert_eq!(s, 200, "{v}");
    let bot2 = v["id"].as_str().expect("id").to_string();
    me.set(AccountInfo::new("on_me2", "李雷"));
    let (s, v) = create(&h, "test_self", json!({})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("可以配对的连接不止一个，请选一个"))
    );
    // not a connection of an adapter it pairs with; not one that is there
    let (s, v) = create(&h, "test_self", json!({"app_connection": mine})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("「test_self」不能和这个连接配对"))
    );
    let (s, _) = create(&h, "test_self", json!({"app_connection": "con_nope"})).await;
    assert_eq!(s, 404);
    let (s, _) = create(&h, "test_self", json!({"app_connection": 7})).await;
    assert_eq!(s, 400);
    // a revoked bot is no pair
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE connections SET status = 'revoked' WHERE id = ?",
                [&bot2],
            )?)
        })
        .expect("revoke");
    let (s, v) = create(&h, "test_self", json!({"app_connection": bot2})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("要配对的连接已经断开了"))
    );
    // …and no candidate either: the other bot is picked
    let (s, v) = create(&h, "test_self", json!({})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(pair_of(&h, v["id"].as_str().expect("id")), json!(bot1));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pair_is_checked_again_in_the_commit() {
    let (h, _bot, _me) = sides(Side::me("test_self", true, false, "on_me"));
    let (_, v) = create(&h, "test_bot", json!({})).await;
    let bot = v["id"].as_str().expect("id").to_string();
    // the bot went between the start of a flow and its end: the connect refuses, nothing is kept
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE connections SET status = 'revoked' WHERE id = ?",
                [&bot],
            )?)
        })
        .expect("revoke");
    let e = h
        .conns
        .connect(
            &caller(WS, ME),
            "test_self",
            vec![("token".into(), Zeroizing::new("t-1".into()))],
            json!({"app_connection": bot}),
            Vec::new(),
            None,
            None,
        )
        .await
        .expect_err("revoked pair");
    assert!(e.to_string().contains("要配对的连接已经断开了"), "{e}");
    assert_eq!(
        val(
            &h,
            "SELECT COUNT(*) FROM connections WHERE adapter = ?",
            "test_self"
        ),
        json!(0)
    );

    // a new credential for a connection whose pair went is refused the same way
    h.env
        .store()
        .with_conn(|c| Ok(c.execute("UPDATE connections SET status = 'ok' WHERE id = ?", [&bot])?))
        .expect("back");
    let (_, v) = create(&h, "test_self", json!({})).await;
    let mine = v["id"].as_str().expect("id").to_string();
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE connections SET status = 'revoked' WHERE id = ?",
                [&bot],
            )?)
        })
        .expect("revoke");
    let (s, v, _) = send(
        &h.api,
        Some(&caller(WS, ME)),
        "PUT",
        &format!("/connections/{mine}/credential"),
        Some(json!({"fields": {"token": "t-2"}})),
        None,
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("要配对的连接已经断开了"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_is_paired_again_with_a_patch() {
    let (h, bot, _me) = sides(Side::me("test_self", true, false, "on_me"));
    let (_, v) = create(&h, "test_bot", json!({})).await;
    let bot1 = v["id"].as_str().expect("id").to_string();
    let (_, v) = create(&h, "test_self", json!({})).await;
    let mine = v["id"].as_str().expect("id").to_string();
    bot.set(
        AccountInfo::new("cli_two", "第二个机器人")
            .kind("app_bot")
            .owner(Owner::Bot),
    );
    let (_, v) = create(&h, "test_bot", json!({})).await;
    let bot2 = v["id"].as_str().expect("id").to_string();

    let (s, v) = patch(&h, &mine, json!({"app_connection": bot2})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(pair_of(&h, &mine), json!(bot2));
    assert_eq!(
        val(
            &h,
            "SELECT COUNT(*) FROM audit WHERE action = 'connection.pair' AND target = ?",
            &mine
        ),
        json!(1)
    );
    // a required pair is not dropped; the same checks as connecting
    let (s, _) = patch(&h, &mine, json!({"app_connection": null})).await;
    assert_eq!(s, 400);
    let (s, _) = patch(&h, &mine, json!({"app_connection": mine})).await;
    assert_eq!(s, 400);
    let (s, _) = patch(&h, &bot1, json!({"app_connection": bot2})).await;
    assert_eq!(s, 400, "a bot does not pair");
    assert_eq!(pair_of(&h, &mine), json!(bot2));
    // other fields of the patch still work alongside
    let (s, v) = patch(&h, &mine, json!({"app_connection": bot1, "paused": true})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "paused");
    assert_eq!(pair_of(&h, &mine), json!(bot1));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pair_that_is_not_required_may_be_dropped() {
    let (h, _bot, _me) = sides(Side::me("test_self", false, false, "on_me"));
    // no bot: connected without a pair
    let (s, v) = create(&h, "test_self", json!({})).await;
    assert_eq!(s, 200, "{v}");
    let mine = v["id"].as_str().expect("id").to_string();
    assert_eq!(pair_of(&h, &mine), Value::Null);
    let (_, v) = create(&h, "test_bot", json!({})).await;
    let bot = v["id"].as_str().expect("id").to_string();
    let (s, _) = patch(&h, &mine, json!({"app_connection": bot})).await;
    assert_eq!(s, 200);
    assert_eq!(pair_of(&h, &mine), json!(bot));
    let (s, _) = patch(&h, &mine, json!({"app_connection": null})).await;
    assert_eq!(s, 200);
    assert_eq!(pair_of(&h, &mine), Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_side_claims_into_me_only_when_it_attests_the_handle() {
    // without opting in: the contact a channel made stays, the connect says who holds the account
    let (h, _bot, _me) = sides(Side::me("test_self", false, false, "on_me"));
    let seen = seen_by_a_channel(&h, "feishu", "on_me");
    let (s, v) = create(&h, "test_self", json!({})).await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(
        val(&h, "SELECT status FROM parties WHERE id = ?", &seen),
        json!("active")
    );

    // opted in: merged into 我, then the account is attached there; the extra handle and the alias
    // find their contacts too, and the alias becomes 我's account
    let (h, _bot, me) = sides(Side::me("test_self", false, true, "on_me"));
    let seen = seen_by_a_channel(&h, "feishu", "on_me");
    let by_mail = seen_by_a_channel(&h, "mail", "lilei@example.com");
    me.set(
        AccountInfo::new("on_me", "李雷")
            .extra_handle("ou_me")
            .alias("mail", "LiLei@example.com")
            .alias("github", "lilei"),
    );
    let (s, v) = create(&h, "test_self", json!({})).await;
    assert_eq!(s, 200, "{v}");
    let me_id = me_party(&h);
    for p in [&seen, &by_mail] {
        assert_eq!(
            (
                val(&h, "SELECT status FROM parties WHERE id = ?", p),
                val(&h, "SELECT merged_into FROM parties WHERE id = ?", p)
            ),
            (json!("merged"), json!(me_id))
        );
    }
    let acc = v["account_id"].as_str().expect("account").to_string();
    assert_eq!(
        val(&h, "SELECT party_id FROM accounts WHERE id = ?", &acc),
        json!(me_id)
    );
    assert_eq!(
        val(
            &h,
            "SELECT party_id FROM accounts WHERE system_id = 'github' AND handle = ?",
            "lilei"
        ),
        json!(me_id),
        "an alias nobody held is 我's account now"
    );

    // a contact someone confirmed by hand is never merged
    let (h, _bot, _me) = sides(Side::me("test_self", false, true, "on_me"));
    let seen = seen_by_a_channel(&h, "feishu", "on_me");
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE parties SET contact_kind = 'real', source = 'me' WHERE id = ?",
                [&seen],
            )?)
        })
        .expect("confirm");
    let (s, _) = create(&h, "test_self", json!({})).await;
    assert_eq!(s, 409);
    assert_eq!(
        val(&h, "SELECT status FROM parties WHERE id = ?", &seen),
        json!("active")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_never_claims() {
    // the bot side says it claims, but its account is not yours: nothing is merged into 我
    let (h, _bot, _me) = sides(Side::me("test_self", false, false, "on_me"));
    let seen = seen_by_a_channel(&h, "feishu_app", "cli_one");
    let (s, _) = create(&h, "test_bot", json!({})).await;
    assert_eq!(s, 409);
    assert_eq!(
        val(&h, "SELECT status FROM parties WHERE id = ?", &seen),
        json!("active")
    );
}

/// What a flow's callback does with the config its start took ([`Connections::connect`] with the
/// flow's copy): the connection is re-authorized with it.
async fn finish_flow(h: &H, adapter: &str, config: Value, reauth_of: &str) {
    h.conns
        .connect(
            &caller(WS, ME),
            adapter,
            vec![("token".into(), Zeroizing::new("t-9".into()))],
            config,
            Vec::new(),
            None,
            Some(reauth_of.to_string()),
        )
        .await
        .expect("reauthorized");
}

fn config_of(h: &H, conn: &str) -> Value {
    let raw = val(h, "SELECT config FROM connections WHERE id = ?", conn);
    serde_json::from_str(raw.as_str().expect("config")).expect("json")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_re_pair_made_while_a_flow_was_out_stands() {
    let (h, bot, _me) = sides(Side::me("test_self", true, false, "on_me"));
    let (_, v) = create(&h, "test_bot", json!({})).await;
    let bot1 = v["id"].as_str().expect("id").to_string();
    let (_, v) = create(&h, "test_self", json!({})).await;
    let mine = v["id"].as_str().expect("id").to_string();
    bot.set(
        AccountInfo::new("cli_two", "第二个机器人")
            .kind("app_bot")
            .owner(Owner::Bot),
    );
    let (_, v) = create(&h, "test_bot", json!({})).await;
    let bot2 = v["id"].as_str().expect("id").to_string();

    // a re-authorization starts with the row's config (paired with bot1)…
    let flow = config_of(&h, &mine);
    assert_eq!(flow["app_connection"], json!(bot1));
    // …you pair it with bot2 meanwhile…
    let (s, v) = patch(&h, &mine, json!({"app_connection": bot2})).await;
    assert_eq!(s, 200, "{v}");
    // …and the callback does not put bot1 back
    finish_flow(&h, "test_self", flow, &mine).await;
    assert_eq!(pair_of(&h, &mine), json!(bot2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pair_dropped_while_a_flow_was_out_stays_dropped() {
    let (h, _bot, _me) = sides(Side::me("test_self", false, false, "on_me"));
    let (_, v) = create(&h, "test_bot", json!({})).await;
    let bot = v["id"].as_str().expect("id").to_string();
    let (_, v) = create(&h, "test_self", json!({})).await;
    let mine = v["id"].as_str().expect("id").to_string();
    assert_eq!(pair_of(&h, &mine), json!(bot));

    let flow = config_of(&h, &mine);
    let (s, _) = patch(&h, &mine, json!({"app_connection": null})).await;
    assert_eq!(s, 200);
    // neither the flow's copy nor the only candidate pairs it again
    finish_flow(&h, "test_self", flow.clone(), &mine).await;
    assert_eq!(pair_of(&h, &mine), Value::Null);

    // a pair the commit drops is gone from the row, not left from before (a blank one here)
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE connections SET config = json_set(config, '$.app_connection', '') WHERE id = ?",
                [&mine],
            )?)
        })
        .expect("blank pair");
    finish_flow(&h, "test_self", flow, &mine).await;
    assert!(
        config_of(&h, &mine).get("app_connection").is_none(),
        "{}",
        config_of(&h, &mine)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_provider_checks_the_config_on_the_way_in() {
    let (h, _bot, _me) = sides(Side::me("test_self", false, false, "on_me"));
    for bad in [
        "http://chat.example.com/api",
        "https://evil.example.net/api",
        "https://user:pw@chat.example.com/api",
        "https://chat.example.com/api?token=x",
    ] {
        let (s, v) = create(&h, "test_self", json!({"endpoint": bad})).await;
        assert_eq!(s, 400, "{bad}: {v}");
        assert_eq!(v["error"], "连接配置不对：endpoint", "{bad}");
    }
    assert_eq!(
        val(&h, "SELECT COUNT(*) FROM connections WHERE 1 = ?", "1"),
        json!(0)
    );
    let (s, v) = create(
        &h,
        "test_self",
        json!({"endpoint": "https://chat.example.com/api", "_v": 9}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let mine = v["id"].as_str().expect("id").to_string();
    let c = config_of(&h, &mine);
    assert_eq!(c["endpoint"], "https://chat.example.com/api");
    assert!(
        c.get("_v").is_none(),
        "a request does not set the version: {c}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_stored_under_another_version_waits_for_a_new_authorization() {
    let (h, _bot, me) = sides(Side::me("test_self", false, false, "on_me"));
    let (_, v) = create(&h, "test_self", json!({})).await;
    let mine = v["id"].as_str().expect("id").to_string();
    assert!(
        config_of(&h, &mine).get("_v").is_none(),
        "version 1 stores nothing"
    );
    assert_eq!(h.conns.check_versions().expect("check"), 0);

    // the provider's stored shape changed (or another provider took its id)
    me.version.store(2, Ordering::SeqCst);
    assert_eq!(h.conns.check_versions().expect("check"), 1);
    assert_eq!(
        val(&h, "SELECT status FROM connections WHERE id = ?", &mine),
        json!("reauth")
    );
    assert_eq!(
        val(
            &h,
            "SELECT COUNT(*) FROM decisions WHERE subject_id = ? AND status = 'open'",
            &mine
        ),
        json!(1)
    );
    assert_eq!(h.conns.check_versions().expect("again"), 0, "marked once");

    // a new credential stores the current version; nothing is marked again
    let (s, v, _) = send(
        &h.api,
        Some(&caller(WS, ME)),
        "PUT",
        &format!("/connections/{mine}/credential"),
        Some(json!({"fields": {"token": "t-2"}})),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(config_of(&h, &mine)["_v"], json!(2));
    assert_eq!(
        val(&h, "SELECT status FROM connections WHERE id = ?", &mine),
        json!("ok")
    );
    assert_eq!(h.conns.check_versions().expect("check"), 0);
}

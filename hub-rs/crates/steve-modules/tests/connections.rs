//! A2 acceptance: a fake OAuth round trip creates the account and the
//! connection; tokens are ciphertext in the system vault; a failing health check → `reauth` + one
//! decision; after disconnect the credential's `dek_wrapped` is empty. Plus: PKCE and state rules,
//! public origins, single-use refresh tokens committed before use (Outlook, Feishu), minted tokens
//! (Feishu tenant, GitHub installation), credential forms, backoff, pause, caps, device flow.

mod conn_support;

use std::sync::Arc;

use axum::http::header;
use axum::routing::post;
use axum::{Json, Router};
use conn_support::*;
use rusqlite::params;
use serde_json::{Value, json};
use steve_channels::fake::feishu::{Endpoint, FakeFeishu, World, ids};
use steve_github::fake::{FakeGitHub, defaults};
use steve_mail::fake::{FakeGmail, FakeGraph};
use steve_modules::accounts::connections::{
    AccountInfo, ConnectionProvider, ConnectionUsage, Connections, FakeProvider, Health, OAuthSpec,
    ProviderError, STATE_TTL_MS, Usage, UsageRef,
};
use steve_modules::decisions;
use steve_modules::testkit::{ME, OTHER, WS, caller};
use steve_modules::vault::VaultResult;
use zeroize::Zeroizing;

const EMAIL: &str = "peng@example.test";

/// A read with plain rusqlite errors.
fn q<T>(
    h: &H,
    f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> steve_store::Result<T> {
    h.env
        .store()
        .reader(|c| f(c).map_err(steve_store::StoreError::from))
}

fn row(h: &H, sql: &str, p: &str) -> Option<Vec<Value>> {
    q(h, |c| {
        let mut st = c.prepare(sql)?;
        let n = st.column_count();
        let mut rows = st.query(params![p])?;
        Ok(rows.next()?.map(|r| {
            (0..n)
                .map(|i| match r.get_ref(i).expect("col") {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => json!(x),
                    rusqlite::types::ValueRef::Real(x) => json!(x),
                    rusqlite::types::ValueRef::Text(t) => {
                        json!(String::from_utf8_lossy(t))
                    }
                    rusqlite::types::ValueRef::Blob(b) => json!(format!("blob:{}", b.len())),
                })
                .collect()
        }))
    })
    .expect("query")
}

fn conn_of(h: &H, account: &str) -> Vec<Value> {
    row(
        h,
        "SELECT id, status, caps, credential_item, adapter, health, config FROM connections WHERE account_id = ?",
        account,
    )
    .expect("connection row")
}

fn secret(h: &H, item: &str, key: &str) -> String {
    h.vault
        .with_credential(WS, item, "test", "测试", |c| {
            c.text(key).map(String::from).expect("field")
        })
        .expect("credential")
}

fn db_bytes(h: &H) -> Vec<u8> {
    let mut db = Vec::new();
    for ext in ["", "-wal", "-shm"] {
        if let Ok(b) = std::fs::read(format!("{}{ext}", h.env.store().path().display())) {
            db.extend_from_slice(&b);
        }
    }
    db
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn open_decisions(h: &H, con: &str) -> Vec<Value> {
    q(h, |c| {
            let mut st = c.prepare(
                "SELECT question, options, kind FROM decisions WHERE subject_type = 'connection' AND subject_id = ? AND status = 'open'",
            )?;
            let rows = st.query_map([con], |r| {
                Ok(json!([r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?]))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("decisions")
}

#[tokio::test(flavor = "multi_thread")]
async fn gmail_oauth_round_trip_creates_account_and_connection() {
    let fake = FakeGmail::start(EMAIL).await.expect("fake gmail");
    let h = setup(vec![Arc::new(MailOAuth::gmail(&fake))]);
    let me = caller(WS, ME);

    // the redirect goes back to the origin the browser used
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "gmail"})),
        Some("steve.example.com"),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let u = url::Url::parse(v["url"].as_str().expect("url")).expect("url");
    assert_eq!(
        query(&u, "redirect_uri").as_deref(),
        Some("https://steve.example.com/auth/connect/callback")
    );

    let (_, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "gmail", "caps": ["mail.read", "mail.draft"]})),
        Some("hub.test:8440"),
    )
    .await;
    let u = url::Url::parse(v["url"].as_str().expect("url")).expect("url");
    let redirect = format!("{ORIGIN}/auth/connect/callback");
    assert_eq!(
        query(&u, "redirect_uri").as_deref(),
        Some(redirect.as_str())
    );
    assert_eq!(query(&u, "client_id").as_deref(), Some(GOOGLE_ID));
    assert_eq!(query(&u, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(query(&u, "access_type").as_deref(), Some("offline"));
    assert!(query(&u, "scope").expect("scope").contains("gmail.modify"));
    let challenge = query(&u, "code_challenge").expect("challenge");
    assert!(!v["url"].as_str().expect("url").contains(GOOGLE_SECRET));

    let back = consent(u.as_str()).await;
    let (s, _, hd) = send(&h.cb, Some(&me), "GET", &path_of(&back), None, None).await;
    assert_eq!(s, 302);
    let loc = hd[header::LOCATION].to_str().expect("loc").to_string();
    assert_eq!(hd[header::CACHE_CONTROL], "no-store");
    let acc = account_of(&loc);

    // the token request carried the verifier of that challenge and the same redirect_uri
    let tr = fake.token_requests();
    let code_req = tr
        .iter()
        .find(|r| r.grant_type == "authorization_code")
        .expect("code grant");
    let verifier = &code_req.form["code_verifier"];
    assert_eq!(
        steve_modules::accounts::connections::oauth::pkce_challenge(verifier),
        challenge
    );
    assert_eq!(code_req.form["redirect_uri"], redirect);
    assert_eq!(code_req.form["client_secret"], GOOGLE_SECRET);

    // the account: “我”, from connecting, handle normalized
    let a = row(
        &h,
        "SELECT a.handle, a.source, a.verified, p.user_id, a.system_id FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        &acc,
    )
    .expect("account");
    assert_eq!(
        a,
        vec![
            json!(EMAIL),
            json!("connect"),
            json!(1),
            json!(ME),
            json!("gmail")
        ]
    );

    // the connection: ok, the requested caps, the credential in the system vault
    let c = conn_of(&h, &acc);
    let con = c[0].as_str().expect("id").to_string();
    assert_eq!(c[1], "ok");
    assert_eq!(c[2], json!(r#"["mail.read","mail.draft"]"#));
    let item = c[3].as_str().expect("item").to_string();
    let it = row(
        &h,
        "SELECT v.scope, i.template, i.account_id, i.dek_wrapped IS NOT NULL FROM vault_items i JOIN vaults v ON v.id = i.vault_id WHERE i.id = ?",
        &item,
    )
    .expect("item");
    assert_eq!(
        it,
        vec![
            json!("system"),
            json!("connection_credential"),
            json!(acc),
            json!(1)
        ]
    );
    let access = secret(&h, &item, "access_token");
    let refresh = secret(&h, &item, "refresh_token");
    assert!(access.starts_with("fake-access-") && refresh.starts_with("fake-refresh-"));
    // ciphertext only: no plaintext column, no token bytes anywhere in the database files
    let plain = row(
        &h,
        "SELECT COUNT(*) FROM vault_fields WHERE item_id = ? AND (value_plain IS NOT NULL OR value_ct IS NULL)",
        &item,
    )
    .expect("count");
    assert_eq!(plain[0], 0);
    let db = db_bytes(&h);
    for n in [&access, &refresh, verifier, &GOOGLE_SECRET.to_string()] {
        assert!(!contains(&db, n), "database holds a token");
    }

    // REST shape
    let (s, list, _) = send(&h.api, Some(&me), "GET", "/connections", None, None).await;
    assert_eq!(s, 200);
    assert_eq!(list[0]["id"], con);
    assert_eq!(list[0]["auth"], "oauth");
    assert_eq!(
        list[0]["available_caps"],
        json!(["mail.read", "mail.draft", "mail.send", "events"])
    );
    assert!(list[0]["health"]["last_ok"].as_i64().unwrap_or(0) > 0);
    assert!(!list.to_string().contains(&access));
    // another user can't see or touch my connection (my mailbox is mine)
    let (_, l2, _) = send(
        &h.api,
        Some(&caller(WS, OTHER)),
        "GET",
        "/connections",
        None,
        None,
    )
    .await;
    assert_eq!(l2, json!([]));
    let (s, _, _) = send(
        &h.api,
        Some(&caller(WS, OTHER)),
        "DELETE",
        &format!("/connections/{con}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, 404);
    let (s, _, _) = send(
        &h.api,
        Some(&caller(WS, OTHER)),
        "GET",
        &format!("/connections/{con}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, 404);

    // the state was single use
    let (s, _, hd) = send(&h.cb, Some(&me), "GET", &path_of(&back), None, None).await;
    assert_eq!(s, 302);
    assert!(
        hd[header::LOCATION]
            .to_str()
            .expect("loc")
            .starts_with("/#/accounts?error="),
        "replayed state accepted"
    );
    // a second consent for the same mailbox reconnects the same connection
    let loc2 = oauth_round_trip(&h, &me, json!({"adapter": "gmail"})).await;
    assert_eq!(account_of(&loc2), acc);
    let c2 = conn_of(&h, &acc);
    assert_eq!(c2[0], con);
    assert_ne!(c2[3], json!(item));
    let old = row(
        &h,
        "SELECT dek_wrapped, status FROM vault_items WHERE id = ?",
        &item,
    )
    .expect("old");
    assert_eq!(
        old,
        vec![Value::Null, json!("deleted")],
        "old credential destroyed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn callback_refuses_foreign_expired_unknown_and_denied() {
    let fake = FakeGmail::start(EMAIL).await.expect("fake gmail");
    let h = setup(vec![Arc::new(MailOAuth::gmail(&fake))]);
    let me = caller(WS, ME);
    let start = || async {
        let (_, v, _) = send(
            &h.api,
            Some(&me),
            "POST",
            "/connections/oauth/start",
            Some(json!({"adapter": "gmail"})),
            None,
        )
        .await;
        consent(v["url"].as_str().expect("url")).await
    };
    let err = |hd: &axum::http::HeaderMap| {
        let l = hd[header::LOCATION].to_str().expect("loc").to_string();
        assert!(l.starts_with("/#/accounts?error="), "{l}");
        let q = l.split_once("error=").expect("q").1.to_string();
        url::form_urlencoded::parse(q.as_bytes())
            .next()
            .map(|(k, _)| k.to_string())
            .unwrap_or_default()
    };

    // another user, then nobody signed in: refused, and the state is gone either way
    let back = start().await;
    let (_, _, hd) = send(
        &h.cb,
        Some(&caller(WS, OTHER)),
        "GET",
        &path_of(&back),
        None,
        None,
    )
    .await;
    assert_eq!(err(&hd), "授权链接已经失效，请重新接入");
    let (_, _, hd) = send(&h.cb, Some(&me), "GET", &path_of(&back), None, None).await;
    assert_eq!(err(&hd), "授权链接已经失效，请重新接入");
    let back = start().await;
    let (_, _, hd) = send(&h.cb, None, "GET", &path_of(&back), None, None).await;
    assert_eq!(err(&hd), "请先登录 Steve，再重新接入");

    // expired after 10 minutes
    let back = start().await;
    h.env.clock.advance(STATE_TTL_MS + 1);
    let (_, _, hd) = send(&h.cb, Some(&me), "GET", &path_of(&back), None, None).await;
    assert_eq!(err(&hd), "授权链接已经失效，请重新接入");

    // made up, and denied at the provider
    let (_, _, hd) = send(
        &h.cb,
        Some(&me),
        "GET",
        "/auth/connect/callback?state=x&code=y",
        None,
        None,
    )
    .await;
    assert_eq!(err(&hd), "授权链接已经失效，请重新接入");
    let back = start().await;
    let state = query(&back, "state").expect("state");
    let (_, _, hd) = send(
        &h.cb,
        Some(&me),
        "GET",
        &format!("/auth/connect/callback?state={state}&error=access_denied"),
        None,
        None,
    )
    .await;
    assert_eq!(err(&hd), "你在 Google 那边取消了授权");
    assert_eq!(
        row(
            &h,
            "SELECT COUNT(*) FROM connections WHERE workspace_id = ?",
            WS
        )
        .expect("n")[0],
        0
    );

    // no OAuth client configured; not an OAuth adapter
    let mut spec = OAuthSpec::new("SLACK", "Slack", "http://x/a", "http://x/t", &[]);
    spec.provider = "SLACK".into();
    let h2 = setup(vec![Arc::new(FakeProvider::oauth(
        spec,
        AccountInfo::new("a@b.c", "a"),
    ))]);
    let (s, v, _) = send(
        &h2.api,
        Some(&me),
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "fake_oauth"})),
        None,
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("还没有配置 Slack 的 OAuth 应用"))
    );
    let (s, _, _) = send(
        &h2.api,
        Some(&me),
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "nope"})),
        None,
    )
    .await;
    assert_eq!(s, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn outlook_refresh_rotates_single_use_tokens_then_reauth() {
    let fake = FakeGraph::start(EMAIL).await.expect("fake graph");
    let h = setup(vec![Arc::new(MailOAuth::outlook(&fake))]);
    let me = caller(WS, ME);
    let acc = account_of(&oauth_round_trip(&h, &me, json!({"adapter": "outlook"})).await);
    let c = conn_of(&h, &acc);
    let con = c[0].as_str().expect("id").to_string();
    let item = c[3].as_str().expect("item").to_string();
    let r1 = secret(&h, &item, "refresh_token");
    let health: Value = serde_json::from_str(c[5].as_str().expect("health")).expect("json");
    assert!(
        health["expires_at"].as_i64().is_some(),
        "expiry mirrored: {health}"
    );

    // nothing due yet
    assert_eq!(h.conns.refresh_due().await, 0);
    // 55 minutes later the token expires within 10 minutes: refreshed, rotated, committed
    h.env.clock.advance(55 * 60_000);
    assert_eq!(h.conns.refresh_due().await, 1);
    let r2 = secret(&h, &item, "refresh_token");
    assert_ne!(r1, r2);
    let last = fake.token_requests().last().cloned().expect("refresh");
    assert_eq!(last.grant_type, "refresh_token");
    assert_eq!(last.form["refresh_token"], r1);
    assert!(
        last.form["scope"].contains("Mail.Send"),
        "Microsoft wants scope on refresh"
    );
    // the fake already invalidated r1: this works only because r2 was stored
    h.conns
        .refresh(WS, &con, true)
        .await
        .expect("second refresh");
    assert_eq!(
        fake.token_requests().last().expect("r").form["refresh_token"],
        r2
    );

    // the user revoked access: refresh fails → reauth with one decision
    fake.revoke_tokens();
    let e = h.conns.refresh(WS, &con, true).await.expect_err("revoked");
    assert!(
        matches!(&e, ProviderError::Auth(m) if m.contains("invalid_grant")),
        "{e:?}"
    );
    assert_eq!(conn_of(&h, &acc)[1], "reauth");
    let d = open_decisions(&h, &con);
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0],
        json!([
            format!("{EMAIL} 的授权过期了"),
            r#"["reauth","pause"]"#,
            "fail"
        ])
    );
    let _ = h.conns.refresh(WS, &con, true).await;
    h.conns.record(WS, &con, Health::Auth("again".into())).await;
    assert_eq!(open_decisions(&h, &con).len(), 1, "at most one open");
    let (_, list, _) = send(&h.api, Some(&me), "GET", "/connections", None, None).await;
    assert_eq!(list[0]["status"], "reauth");
    assert_eq!(list[0]["health"]["last_error"], "again");

    // re-authorize from the account page: same connection, new credential, decision settled
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        &format!("/connections/{con}/reauth"),
        None,
        Some("hub.test:8440"),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let back = consent(v["url"].as_str().expect("url")).await;
    let (_, _, hd) = send(&h.cb, Some(&me), "GET", &path_of(&back), None, None).await;
    assert_eq!(account_of(hd[header::LOCATION].to_str().expect("loc")), acc);
    let c = conn_of(&h, &acc);
    assert_eq!(
        (c[0].as_str(), c[1].as_str()),
        (Some(con.as_str()), Some("ok"))
    );
    assert_ne!(c[3], json!(item));
    assert!(open_decisions(&h, &con).is_empty());
    assert_eq!(
        row(
            &h,
            "SELECT dek_wrapped FROM vault_items WHERE id = ?",
            &item
        )
        .expect("old")[0],
        Value::Null
    );

    // the decision card's “pause”
    h.conns.record(WS, &con, Health::Auth("x".into())).await;
    let id = q(&h, |c| {
        c.query_row(
            "SELECT id FROM decisions WHERE subject_id = ? AND status = 'open'",
            [&con],
            |r| r.get::<_, String>(0),
        )
    })
    .expect("decision");
    let r = decisions::dispatch(&h.env.modules, &me, &id, "pause", "").expect("ours");
    r.expect("resolved");
    assert_eq!(conn_of(&h, &acc)[1], "paused");
    // paused connections are not checked or refreshed, and recording leaves them alone
    h.conns.record(WS, &con, Health::Ok).await;
    assert_eq!(conn_of(&h, &acc)[1], "paused");
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/connections/{con}"),
        Some(json!({"paused": false})),
        None,
    )
    .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("ok")));
}

#[tokio::test(flavor = "multi_thread")]
async fn stored_token_401_forces_one_refresh() {
    let fake = FakeGmail::start(EMAIL).await.expect("fake gmail");
    let h = setup(vec![Arc::new(MailOAuth::gmail(&fake))]);
    let me = caller(WS, ME);
    let acc = account_of(&oauth_round_trip(&h, &me, json!({"adapter": "gmail"})).await);
    let con = conn_of(&h, &acc)[0].as_str().expect("id").to_string();
    let n = fake.token_requests().len();
    fake.expire_tokens();
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        &format!("/connections/{con}/test"),
        None,
        None,
    )
    .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("ok")), "{v}");
    let tr = fake.token_requests();
    assert_eq!(tr.len(), n + 1);
    assert_eq!(tr[n].grant_type, "refresh_token");
    // Google keeps the refresh token: the stored one is still there
    let item = conn_of(&h, &acc)[3].as_str().expect("item").to_string();
    assert_eq!(
        secret(&h, &item, "refresh_token"),
        tr[n].form["refresh_token"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn feishu_app_credential_and_user_oauth_with_single_use_refresh() {
    let fake = FakeFeishu::start_with(World::sample())
        .await
        .expect("fake feishu");
    let base = fake.base_url().to_string();
    let h = setup(vec![
        Arc::new(FeishuApp::new()),
        Arc::new(FeishuUser::new(&base)),
    ]);
    let me = caller(WS, ME);

    // the form: by label or key; secrets to the vault, the rest to config
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections",
        Some(json!({"adapter": "feishu_app", "fields": {"App ID": ids::APP_ID, "app_secret": ids::APP_SECRET, "Encrypt Key": "PLAINTEXT-encrypt"}, "config": {"api_base": base}})),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let app_con = v["id"].as_str().expect("id").to_string();
    let acc = v["account_id"].as_str().expect("acc").to_string();
    assert_eq!(v["auth"], "credential");
    let a = row(
        &h,
        "SELECT a.handle, a.kind, p.type, p.org_kind FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        &acc,
    )
    .expect("account");
    assert_eq!(
        a,
        vec![
            json!(ids::APP_ID),
            json!("app_bot"),
            json!("org"),
            json!("self")
        ]
    );
    let c = conn_of(&h, &acc);
    let cfg: Value = serde_json::from_str(c[6].as_str().expect("cfg")).expect("json");
    assert_eq!(cfg["app_id"], ids::APP_ID);
    assert!(!c[6].as_str().expect("cfg").contains(ids::APP_SECRET));
    let item = c[3].as_str().expect("item").to_string();
    assert_eq!(secret(&h, &item, "app_secret"), ids::APP_SECRET);
    assert_eq!(secret(&h, &item, "encrypt_key"), "PLAINTEXT-encrypt");
    // the bot's open_id is a handle under this connection
    let oh = q(&h, |c| {
        c.query_row(
            "SELECT COUNT(*) FROM account_handles WHERE account_id = ? AND connection_id = ?",
            params![acc, app_con],
            |r| r.get::<_, i64>(0),
        )
    })
    .expect("handles");
    assert_eq!(oh, 1);
    // the tenant token was minted once (verify) and is cached
    let asked = fake.requests_to(Endpoint::TenantToken).len();
    assert_eq!(asked, 1);
    let cx = h.conns.cx(WS, &app_con).expect("cx");
    let t1 = cx.access_token().await.expect("token");
    let t2 = cx.access_token().await.expect("token");
    assert_eq!(t1.expose(), t2.expose());
    assert_eq!(fake.requests_to(Endpoint::TenantToken).len(), 1);
    cx.invalidate_token().await;
    let _ = cx.access_token().await.expect("token");
    assert_eq!(
        fake.requests_to(Endpoint::TenantToken).len(),
        2,
        "re-minted after a 401"
    );

    // a wrong secret: 400, nothing left behind
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections",
        Some(json!({"adapter": "feishu_app", "fields": {"app_id": "cli_other", "app_secret": "PLAINTEXT-wrong"}, "config": {"api_base": base}})),
        None,
    )
    .await;
    assert_eq!(s, 400);
    assert!(
        v["error"]
            .as_str()
            .expect("e")
            .starts_with("验证没有通过："),
        "{v}"
    );
    assert!(!v.to_string().contains("PLAINTEXT-wrong"));
    let active = q(&h, |c| {
            c.query_row(
                "SELECT COUNT(*) FROM vault_items WHERE template = 'connection_credential' AND status = 'active'",
                [],
                |r| r.get::<_, i64>(0),
            )
        })
        .expect("n");
    assert_eq!(active, 1);
    // replacing the credential verifies the new one (not the cached token of the old one)
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "PUT",
        &format!("/connections/{app_con}/credential"),
        Some(json!({"fields": {"app_id": ids::APP_ID, "app_secret": "PLAINTEXT-wrong"}})),
        None,
    )
    .await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(
        secret(&h, &item, "app_secret"),
        ids::APP_SECRET,
        "old credential kept"
    );
    assert!(cx.access_token().await.is_ok());
    // missing required field
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections",
        Some(json!({"adapter": "feishu_app", "fields": {"app_id": "x"}})),
        None,
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("参数不对：fields.app_secret"))
    );

    // the user's own token, with the app's id and secret as OAuth client; without a pair named, the
    // only app connection there is gets picked
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "feishu_user"})),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    // a pair the provider does not take, and one that is not there
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "feishu_user", "config": {"app_connection": "con_nope"}})),
        None,
    )
    .await;
    assert_eq!(s, 404, "{v}");
    let loc = oauth_round_trip(
        &h,
        &me,
        json!({"adapter": "feishu_user", "config": {"app_connection": app_con}}),
    )
    .await;
    let uacc = account_of(&loc);
    let a = row(
        &h,
        "SELECT a.handle, a.system_id, p.user_id FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        &uacc,
    )
    .expect("account");
    assert_eq!(
        a,
        vec![json!(ids::OWNER.union_id), json!("feishu"), json!(ME)]
    );
    let uc = conn_of(&h, &uacc);
    let ucon = uc[0].as_str().expect("id").to_string();
    let uitem = uc[3].as_str().expect("item").to_string();
    let r1 = secret(&h, &uitem, "refresh_token");
    assert!(
        secret(&h, &uitem, "refresh_expires_at")
            .parse::<i64>()
            .is_ok()
    );
    // single-use refresh tokens: each refresh consumes the stored one and stores the next first
    h.conns.refresh(WS, &ucon, true).await.expect("refresh 1");
    let r2 = secret(&h, &uitem, "refresh_token");
    assert_ne!(r1, r2);
    h.conns.refresh(WS, &ucon, true).await.expect("refresh 2");
    assert_ne!(secret(&h, &uitem, "refresh_token"), r2);
    // and the token works
    let ucx = h.conns.cx(WS, &ucon).expect("cx");
    let p = h.conns.provider("feishu_user").expect("p");
    assert_eq!(
        p.verify(&ucx).await.expect("verify").handle,
        ids::OWNER.union_id
    );
    // a spent refresh token: invalid_grant → reauth with Feishu's question
    let item2 = uitem.clone();
    h.vault
        .replace_credential(
            WS,
            ME,
            &item2,
            vec![
                ("access_token".into(), Zeroizing::new("u-old".into())),
                ("refresh_token".into(), Zeroizing::new(r1.clone())),
            ],
        )
        .expect("replace");
    assert!(matches!(
        h.conns.refresh(WS, &ucon, true).await,
        Err(ProviderError::Auth(_))
    ));
    assert_eq!(open_decisions(&h, &ucon)[0][0], "飞书需要重新授权");
}

#[tokio::test(flavor = "multi_thread")]
async fn github_app_installation_tokens_are_minted_and_cached() {
    let fake = FakeGitHub::start().await.expect("fake github");
    let h = setup(vec![Arc::new(GitHubApp)]);
    let me = caller(WS, ME);
    // the manifest flow is G1's; it ends here with the key it got
    let done = h
        .conns
        .connect(
            &me,
            "github_app",
            vec![("private_key".into(), Zeroizing::new(defaults::APP_PRIVATE_KEY_PEM.into()))],
            json!({"app_id": defaults::APP_ID, "installation_id": defaults::INSTALLATION_ID, "api_base": fake.url()}),
            Vec::new(),
            None,
            None,
        )
        .await
        .expect("connect");
    assert!(!done.reconnected);
    let a = row(
        &h,
        "SELECT a.handle, a.kind, p.org_kind FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        &done.account_id,
    )
    .expect("account");
    assert_eq!(
        a,
        vec![
            json!(format!("{}[bot]", defaults::APP_SLUG)),
            json!("app_bot"),
            json!("self")
        ]
    );
    let c = conn_of(&h, &done.account_id);
    assert_eq!(c[2], json!(r#"["repo.read","repo.write","events"]"#));
    assert!(c[6].as_str().expect("cfg").contains("account_login"));
    // managed adapters aren't offered in the generic dialog
    let (_, ad, _) = send(
        &h.api,
        Some(&me),
        "GET",
        "/connections/adapters",
        None,
        None,
    )
    .await;
    assert_eq!(ad, json!([]));

    let cx = h.conns.cx(WS, &done.connection_id).expect("cx");
    let t = cx.access_token().await.expect("token");
    assert!(t.expose().starts_with("ghs_"));
    let _ = cx.access_token().await.expect("cached");
    assert_eq!(fake.issued_tokens(), 1);
    let res = cx
        .http()
        .get(format!("{}/installation/repositories", fake.url()))
        .header(header::AUTHORIZATION, t.bearer().as_str())
        .send()
        .await
        .expect("repos");
    assert_eq!(res.status().as_u16(), 200);
    // a 401 later: invalidate, the next one is minted fresh
    cx.invalidate_token().await;
    let t2 = cx.access_token().await.expect("token");
    assert_ne!(t2.expose(), t.expose());
    assert_eq!(fake.issued_tokens(), 2);
    assert_eq!(format!("{t2:?}"), "AccessToken(…)");
    // installing again updates, no second connection
    let again = h
        .conns
        .connect(
            &me,
            "github_app",
            vec![("private_key".into(), Zeroizing::new(defaults::APP_PRIVATE_KEY_PEM.into()))],
            json!({"app_id": defaults::APP_ID, "installation_id": defaults::INSTALLATION_ID, "api_base": fake.url()}),
            Vec::new(),
            None,
            None,
        )
        .await
        .expect("again");
    assert_eq!(
        (again.connection_id.as_str(), again.reconnected),
        (done.connection_id.as_str(), true)
    );
}

struct Uses;

impl ConnectionUsage for Uses {
    fn usage(&self, _: &rusqlite::Connection, _: &str, id: &str) -> VaultResult<Usage> {
        Ok(Usage {
            flows: vec![UsageRef {
                id: "flw_1".into(),
                name: format!("邮件分诊 {}", &id[..4]),
            }],
            spaces: 2,
            sources: Vec::new(),
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn credential_health_backoff_caps_pause_and_disconnect() {
    let mut info = AccountInfo::new("cli_fake", "Fake Bot");
    info.kind = "app_bot".into();
    info.owner = steve_modules::accounts::connections::Owner::SelfOrg;
    info.extra_handles = vec!["ou_fakebot".into()];
    let fp = Arc::new(FakeProvider::credential(info));
    let env_h = setup(vec![fp.clone() as Arc<dyn ConnectionProvider>]);
    // rebuild with a usage reporter
    let conns = Connections::builder(
        env_h.env.modules.clone(),
        env_h.vault.clone(),
        steve_modules::accounts::connections::ConnectionsConfig::new(
            vec![ORIGIN.into()],
            true,
            |_| None,
        ),
    )
    .provider(fp.clone())
    .usage(Arc::new(Uses))
    .build();
    let api = steve_modules::accounts::connections::routes(conns.clone())
        .with_state(env_h.env.modules.clone());
    let h = H {
        api,
        conns,
        ..env_h
    };
    let me = caller(WS, ME);

    fp.fail_verify(ProviderError::Rejected("App Secret 不对".into()));
    let (s, v, _) = send(&h.api, Some(&me), "POST", "/connections", Some(json!({"adapter": "fake_cred", "fields": {"app_id": "cli_fake", "App Secret": "PLAINTEXT-s1"}})), None).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("验证没有通过：App Secret 不对"))
    );
    let (s, v, _) = send(&h.api, Some(&me), "POST", "/connections", Some(json!({"adapter": "fake_cred", "fields": {"app_id": "cli_fake", "App Secret": "PLAINTEXT-s1"}})), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        fp.calls().last().map(String::as_str),
        Some("verify:app_secret")
    );
    let con = v["id"].as_str().expect("id").to_string();
    let acc = v["account_id"].as_str().expect("acc").to_string();
    let item = conn_of(&h, &acc)[3].as_str().expect("item").to_string();

    // auth failure → reauth + decision; recovered → ok and the decision settles
    fp.next_health(Health::Auth("令牌无效".into()));
    assert_eq!(h.conns.check_health().await, 1);
    assert_eq!(conn_of(&h, &acc)[1], "reauth");
    assert_eq!(open_decisions(&h, &con).len(), 1);
    assert_eq!(
        h.conns.check_health().await,
        0,
        "reauth isn't checked by the job"
    );
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        &format!("/connections/{con}/test"),
        None,
        None,
    )
    .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("ok")));
    assert!(open_decisions(&h, &con).is_empty());

    // three failures in a row → error, with backoff between checks
    for i in 0..3 {
        fp.next_health(Health::Failed("连不上".into()));
        assert_eq!(h.conns.check_health().await, 1, "check {i}");
        assert_eq!(h.conns.check_health().await, 0, "backoff after {i}");
        h.env.clock.advance(6 * 60 * 60_000);
    }
    assert_eq!(conn_of(&h, &acc)[1], "error");
    assert!(open_decisions(&h, &con).is_empty(), "errors don't ask");
    assert_eq!(h.conns.check_health().await, 1);
    assert_eq!(conn_of(&h, &acc)[1], "ok", "recovers by itself");

    // caps ⊆ available_caps
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/connections/{con}"),
        Some(json!({"caps": ["repo.write"]})),
        None,
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：caps.0")));
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/connections/{con}"),
        Some(json!({"caps": ["events", "mail.read"], "paused": true})),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(
        (v["caps"].clone(), v["status"].as_str()),
        (json!(["mail.read", "events"]), Some("paused"))
    );
    assert_eq!(h.conns.check_health().await, 0, "paused isn't checked");

    // reauth of a credential connection lists the fields to fill; PUT replaces after verifying
    let (_, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        &format!("/connections/{con}/reauth"),
        None,
        None,
    )
    .await;
    assert_eq!(v, json!({"fields": ["App Secret"]}));
    let (s, _, _) = send(
        &h.api,
        Some(&me),
        "PUT",
        &format!("/connections/{con}/credential"),
        Some(json!({"fields": {"app_id": "cli_fake", "app_secret": "PLAINTEXT-s2"}})),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let item2 = conn_of(&h, &acc)[3].as_str().expect("item").to_string();
    assert_ne!(item, item2);
    assert_eq!(secret(&h, &item2, "app_secret"), "PLAINTEXT-s2");

    // usage, then disconnect
    let (_, u, _) = send(
        &h.api,
        Some(&me),
        "GET",
        &format!("/connections/{con}/usage"),
        None,
        None,
    )
    .await;
    assert_eq!(u["spaces"], 2);
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "DELETE",
        &format!("/connections/{con}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(v["ok"], true);
    assert_eq!(v["usage"]["flows"][0]["id"], "flw_1");
    assert!(fp.calls().iter().any(|c| c == &format!("revoke:{con}")));
    let it = row(&h, "SELECT dek_wrapped, status, (SELECT COUNT(*) FROM vault_fields f WHERE f.item_id = vault_items.id AND f.value_ct IS NOT NULL) FROM vault_items WHERE id = ?", &item2).expect("item");
    assert_eq!(it, vec![Value::Null, json!("deleted"), json!(0)]);
    assert!(row(&h, "SELECT id FROM connections WHERE id = ?", &con).is_none());
    assert!(
        row(&h, "SELECT id FROM accounts WHERE id = ?", &acc).is_some(),
        "the account stays"
    );
    assert!(
        row(
            &h,
            "SELECT handle FROM account_handles WHERE connection_id = ?",
            &con
        )
        .is_none()
    );
    let (s, _, _) = send(
        &h.api,
        Some(&me),
        "GET",
        &format!("/connections/{con}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn device_flow_connects_in_the_background() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let polls = Arc::new(AtomicU32::new(0));
    let p2 = polls.clone();
    let app = Router::new()
        .route(
            "/device",
            post(|| async {
                Json(json!({"device_code": "PLAINTEXT-device-code", "user_code": "ABCD-1234", "verification_uri": "https://example.com/device", "expires_in": 120, "interval": 1}))
            }),
        )
        .route(
            "/token",
            post(move |body: String| {
                let p = p2.clone();
                async move {
                    assert!(body.contains("device_code=PLAINTEXT-device-code"));
                    assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
                    if p.fetch_add(1, Ordering::SeqCst) == 0 {
                        (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "authorization_pending"})))
                    } else {
                        (axum::http::StatusCode::OK, Json(json!({"access_token": "PLAINTEXT-dev-access", "refresh_token": "PLAINTEXT-dev-refresh", "expires_in": 3600})))
                    }
                }
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = l.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(l, app).await.ok() });
    let mut spec = OAuthSpec::new(
        "GOOGLE",
        "Google",
        &format!("http://{addr}/auth"),
        &format!("http://{addr}/token"),
        &["email"],
    );
    spec.device_url = Some(format!("http://{addr}/device"));
    let h = setup(vec![Arc::new(FakeProvider::oauth(
        spec,
        AccountInfo::new("Dev@Example.com", "Dev"),
    ))]);
    let me = caller(WS, ME);
    let (_, ad, _) = send(
        &h.api,
        Some(&me),
        "GET",
        "/connections/adapters",
        None,
        None,
    )
    .await;
    assert_eq!(ad[0]["device"], true);
    let (s, v, _) = send(
        &h.api,
        Some(&me),
        "POST",
        "/connections/device/start",
        Some(json!({"adapter": "fake_oauth"})),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["user_code"], "ABCD-1234");
    assert!(!v.to_string().contains("PLAINTEXT-device-code"));
    let id = v["id"].as_str().expect("id").to_string();
    let (s, _, _) = send(
        &h.api,
        Some(&caller(WS, OTHER)),
        "GET",
        &format!("/connections/device/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, 404);
    let mut st = Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        st = send(
            &h.api,
            Some(&me),
            "GET",
            &format!("/connections/device/{id}"),
            None,
            None,
        )
        .await
        .1;
        if st["status"] != "pending" {
            break;
        }
    }
    assert_eq!(st["status"], "ok", "{st}");
    assert!(polls.load(Ordering::SeqCst) >= 2);
    let acc = st["account_id"].as_str().expect("acc").to_string();
    let a = row(&h, "SELECT handle FROM accounts WHERE id = ?", &acc).expect("acc");
    assert_eq!(a[0], "dev@example.com");
    let item = conn_of(&h, &acc)[3].as_str().expect("item").to_string();
    assert_eq!(secret(&h, &item, "refresh_token"), "PLAINTEXT-dev-refresh");
}

/// An app bot connected with `Owner::Bot` becomes its own bot party; the
/// connection and the bot are owned by the connecting user, unless the provider names the app's
/// owner (a known person's account), who then owns the bot (source `sync`). A reconnect keeps
/// the party.
#[tokio::test(flavor = "multi_thread")]
async fn app_bot_connect_makes_an_owned_bot_party() {
    use steve_modules::accounts::connections::{Owner, StewardHint};
    let mut info = AccountInfo::new("cli_alarm", "告警小助手");
    info.kind = "app_bot".into();
    info.owner = Owner::Bot;
    let fp = Arc::new(FakeProvider::credential(info.clone()));
    let mut hinted = AccountInfo::new("cli_ops", "运维机器人");
    hinted.kind = "app_bot".into();
    hinted.owner = Owner::Bot;
    hinted.steward_hint = Some(StewardHint {
        system: "mail".into(),
        handle: "Zhang@Acme.com".into(),
    });
    let fp2 = Arc::new(FakeProvider::credential(hinted));
    let h = setup(vec![fp.clone() as Arc<dyn ConnectionProvider>]);
    let me = caller(WS, ME);
    let (s, v, _) = send(&h.api, Some(&me), "POST", "/connections", Some(json!({"adapter": "fake_cred", "fields": {"app_id": "cli_alarm", "App Secret": "PLAINTEXT-s1"}})), None).await;
    assert_eq!(s, 200, "{v}");
    let con = v["id"].as_str().expect("id").to_string();
    let acc = v["account_id"].as_str().expect("acc").to_string();
    let party = row(&h, "SELECT p.id, p.name, p.contact_kind FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?", &acc).expect("party");
    assert_eq!(
        (party[1].as_str(), party[2].as_str()),
        (Some("告警小助手"), Some("bot"))
    );
    let bot = party[0].as_str().expect("id").to_string();
    let me_id = row(&h, "SELECT id FROM parties WHERE user_id = ?", ME).expect("me")[0].clone();
    let owner = |t: &str, id: &str| {
        row(
            &h,
            &format!(
                "SELECT person_id, source FROM stewards WHERE object_type = '{t}' AND object_id = ?"
            ),
            id,
        )
    };
    assert_eq!(
        owner("connection", &con),
        Some(vec![me_id.clone(), json!("connect")])
    );
    assert_eq!(
        owner("party", &bot),
        Some(vec![me_id.clone(), json!("connect")])
    );
    // our own bot, so it is not a stranger in chats
    let got = h
        .env
        .store()
        .transaction(|tx| {
            steve_modules::accounts::identify(
                tx,
                WS,
                ME,
                "feishu_app",
                "cli_alarm",
                &Default::default(),
            )
        })
        .expect("identify");
    assert!(got.is_ours && got.party_id == bot);

    // the provider names the owner: 张三, whose mail account is known
    let h2 = setup(vec![fp2.clone() as Arc<dyn ConnectionProvider>]);
    h2.env
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, contact_kind, created_by, created, updated) VALUES ('ct_zhang', 'default', 'contact', '张三', 'real', 'usr_dev', 1, 1);
                 INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES ('acc_z', 'default', 'mail', 'email', 'ct_zhang', 'work_email', 'zhang@acme.com', 'zhang@acme.com', 'sync', 1, 1);
                 INSERT INTO account_handles (workspace_id, family, handle, account_id, created) VALUES ('default', 'email', 'zhang@acme.com', 'acc_z', 1);",
            )?;
            Ok(())
        })
        .expect("seed");
    let (s, v, _) = send(&h2.api, Some(&me), "POST", "/connections", Some(json!({"adapter": "fake_cred", "fields": {"app_id": "cli_ops", "App Secret": "PLAINTEXT-s2"}})), None).await;
    assert_eq!(s, 200, "{v}");
    let acc = v["account_id"].as_str().expect("acc").to_string();
    let bot = row(&h2, "SELECT party_id FROM accounts WHERE id = ?", &acc).expect("party")[0]
        .as_str()
        .expect("id")
        .to_string();
    assert_eq!(
        row(
            &h2,
            "SELECT person_id, source FROM stewards WHERE object_type = 'party' AND object_id = ?",
            &bot
        ),
        Some(vec![json!("ct_zhang"), json!("sync")])
    );
}

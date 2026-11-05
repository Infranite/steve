//! V3 acceptance tests with webauthn-authenticator-rs's software
//! passkey: register and reveal round trip; no passkey 400; replayed and expired challenges fail;
//! another origin fails; a stale login can't register.

use std::sync::Arc;

use axum::Router;
use axum::body::Body as HttpBody;
use axum::http::{Request, header};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::Url;

use super::*;
use crate::testkit::{ME, OTHER, TestEnv, WS, caller};
use crate::vault::Vault;
use crate::vault::items::{NEED_PASSKEY, REVEAL_TTL_S};
use crate::vault::keystore::FileKeyStore;

const PW: &str = "PW-correct-horse-9271";
const A: &str = "https://steve.example.dev";
const A_HOST: &str = "steve.example.dev";
const B: &str = "https://steve.home.arpa:8443";
const B_HOST: &str = "steve.home.arpa:8443";
const FRESH: &str = "tok-fresh";
const OLD: &str = "tok-old";
const OTHERS: &str = "tok-other";

struct Env {
    env: TestEnv,
    _keys: TempDir,
    pk: Passkeys,
    r: Router,
    item: String,
    field: String,
}

struct Res {
    status: u16,
    no_store: bool,
    body: Value,
}

fn session(env: &TestEnv, user: &str, token: &str, created: i64) {
    env.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO auth_sessions (token_hash, user_id, created, expires) VALUES (?, ?, ?, ?)",
                rusqlite::params![steve_store::sha(token), user, created, created + 86_400_000],
            )?;
            Ok(())
        })
        .expect("session");
}

async fn setup() -> Env {
    let env = TestEnv::new(|_| {});
    let keys = tempfile::tempdir().expect("tmp");
    let ks = Arc::new(FileKeyStore::open(keys.path()).expect("keys"));
    let vault = Vault::new(env.store().clone(), ks);
    vault.init(WS, ME).expect("init");
    let urls = [A.to_string(), B.to_string(), "http://127.0.0.1:8440".into()];
    let pk = Passkeys::with_clock(env.store().clone(), &urls, env.clock.clone());
    let r = Router::new()
        .merge(crate::vault::items::routes(
            vault.clone(),
            Arc::new(pk.clone()),
        ))
        .merge(routes(pk.clone()));
    let now = env.clock.now();
    session(&env, ME, FRESH, now - 60_000);
    session(&env, ME, OLD, now - FRESH_LOGIN_MS - 1);
    session(&env, OTHER, OTHERS, now);
    let mut e = Env {
        env,
        _keys: keys,
        pk,
        r,
        item: String::new(),
        field: String::new(),
    };
    let vaults = e.send(A_HOST, None, "GET", "/vaults", None).await;
    let vid = vaults
        .body
        .as_array()
        .expect("vaults")
        .iter()
        .find(|v| v["scope"] == "personal")
        .expect("personal")["id"]
        .as_str()
        .expect("id")
        .to_string();
    let it = e
        .send(
            A_HOST,
            None,
            "POST",
            "/vault/items",
            Some(json!({
                "vault_id": vid, "template": "login", "title": "GitHub", "site": "github.com",
                "fields": [
                    {"label": "用户名", "type": "text", "secret": false, "value": "Alice"},
                    {"label": "密码", "type": "password", "secret": true, "value": PW},
                ]
            })),
        )
        .await;
    assert_eq!(it.status, 200, "{}", it.body);
    e.item = it.body["id"].as_str().expect("item").into();
    e.field = it.body["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .find(|f| f["secret"] == true)
        .expect("secret field")["id"]
        .as_str()
        .expect("fid")
        .into();
    e
}

impl Env {
    async fn send_as(
        &self,
        who: &str,
        host: &str,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Res {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, host);
        if let Some(t) = token {
            req = req.header(header::COOKIE, format!("theme=dark; steve_session={t}"));
        }
        if body.is_some() {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        let mut req = req
            .body(match body {
                Some(b) => HttpBody::from(b.to_string()),
                None => HttpBody::empty(),
            })
            .expect("request");
        req.extensions_mut().insert(caller(WS, who));
        let res = self.r.clone().oneshot(req).await.expect("infallible");
        let status = res.status().as_u16();
        let no_store = res
            .headers()
            .get(header::CACHE_CONTROL)
            .is_some_and(|v| v == "no-store");
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body");
        Res {
            status,
            no_store,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    async fn send(
        &self,
        host: &str,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Res {
        self.send_as(ME, host, token, method, path, body).await
    }

    /// Registers `key` for the RP of `host`/`origin` with a fresh login.
    async fn register(&self, key: &mut SoftPasskey, host: &str, origin: &str) -> Value {
        let s = self
            .send(
                host,
                Some(FRESH),
                "POST",
                "/passkeys/register/start",
                Some(json!({"name": "MacBook Touch ID"})),
            )
            .await;
        assert_eq!(s.status, 200, "{}", s.body);
        assert!(s.no_store);
        let ccr: CreationChallengeResponse = serde_json::from_value(s.body).expect("ccr");
        let cred = key
            .do_registration(Url::parse(origin).expect("url"), ccr)
            .expect("soft registration");
        let f = self
            .send(
                host,
                Some(FRESH),
                "POST",
                "/passkeys/register/finish",
                Some(json!({"credential": cred})),
            )
            .await;
        assert_eq!(f.status, 200, "{}", f.body);
        f.body
    }

    fn reveal_path(&self, step: &str) -> String {
        format!(
            "/vault/items/{}/fields/{}/reveal/{step}",
            self.item, self.field
        )
    }

    async fn reveal_start(&self, host: &str) -> Res {
        self.send(host, None, "POST", &self.reveal_path("start"), None)
            .await
    }

    async fn reveal_finish(&self, host: &str, assertion: &Value, copy: bool) -> Res {
        self.send(
            host,
            None,
            "POST",
            &self.reveal_path("finish"),
            Some(json!({"assertion": assertion, "copy": copy})),
        )
        .await
    }

    /// The software passkey's answer to a reveal challenge, signed for `origin`.
    fn answer(&self, key: &mut SoftPasskey, challenge: Value, origin: &str) -> Value {
        let rcr: RequestChallengeResponse = serde_json::from_value(challenge).expect("rcr");
        let a = key
            .do_authentication(Url::parse(origin).expect("url"), rcr)
            .expect("soft assertion");
        serde_json::to_value(a).expect("assertion json")
    }

    fn count(&self, q: &str) -> i64 {
        self.env
            .store()
            .with_conn(|c| Ok(c.query_row(q, [], |r| r.get(0))?))
            .expect("sql")
    }
}

#[tokio::test]
async fn relying_parties_follow_public_urls() {
    let e = setup().await;
    // the IP origin can't be an RP and is skipped
    assert_eq!(e.pk.rp_ids(), ["steve.example.dev", "steve.home.arpa"]);
    let none = Passkeys::new(e.env.store().clone(), &["http://127.0.0.1:8440".into()]);
    assert!(none.rp_ids().is_empty());
}

#[tokio::test]
async fn register_then_reveal_with_a_software_passkey() {
    let e = setup().await;
    let mut key = SoftPasskey::new(true);
    let out = e.register(&mut key, A_HOST, A).await;
    assert_eq!(out["name"], "MacBook Touch ID");
    assert_eq!(out["rp_id"], "steve.example.dev");
    assert!(out["id"].as_str().expect("id").starts_with("pk"));
    assert_eq!(out["last_used"], Value::Null);
    assert!(out.get("credential").is_none());

    let l = e.send(A_HOST, None, "GET", "/passkeys", None).await;
    assert_eq!(l.status, 200);
    assert_eq!(l.body.as_array().expect("list").len(), 1);
    // not another user's
    let lo = e
        .send_as(OTHER, A_HOST, None, "GET", "/passkeys", None)
        .await;
    assert_eq!(lo.body, json!([]));

    // view
    let s = e.reveal_start(A_HOST).await;
    assert_eq!(s.status, 200, "{}", s.body);
    assert!(s.no_store);
    assert_eq!(s.body["publicKey"]["userVerification"], "required");
    assert_eq!(s.body["publicKey"]["rpId"], "steve.example.dev");
    assert_eq!(s.body["publicKey"]["timeout"], 60_000);
    let a = e.answer(&mut key, s.body, A);
    let f = e.reveal_finish(A_HOST, &a, false).await;
    assert_eq!(f.status, 200, "{}", f.body);
    assert!(f.no_store);
    assert_eq!(f.body, json!({"value": PW, "ttl": REVEAL_TTL_S}));

    // copy
    let s = e.reveal_start(A_HOST).await;
    let a = e.answer(&mut key, s.body, A);
    let f = e.reveal_finish(A_HOST, &a, true).await;
    assert_eq!(f.status, 200, "{}", f.body);

    assert_eq!(
        e.count("SELECT count(*) FROM secret_uses WHERE actor_type = 'me' AND actor_id = 'usr_dev' AND mode = 'view' AND status = 'ok'"),
        1
    );
    assert_eq!(
        e.count("SELECT count(*) FROM secret_uses WHERE actor_type = 'me' AND mode = 'copy'"),
        1
    );
    assert_eq!(
        e.count("SELECT count(*) FROM passkeys WHERE last_used IS NOT NULL"),
        1
    );
    assert_eq!(
        e.count("SELECT count(*) FROM audit WHERE action = 'passkey.add'"),
        1
    );
    // the stored credential is the public key, never the field value
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM passkeys WHERE credential LIKE '%{PW}%'"
        )),
        0
    );
}

#[tokio::test]
async fn without_a_passkey_reveal_is_400() {
    let e = setup().await;
    let s = e.reveal_start(A_HOST).await;
    assert_eq!(s.status, 400);
    assert_eq!(s.body, json!({"error": NEED_PASSKEY}));
    let f = e.reveal_finish(A_HOST, &json!({}), false).await;
    assert_eq!(f.status, 400);

    // a passkey for one domain doesn't count on another: each RP ID is registered once
    let mut key = SoftPasskey::new(true);
    e.register(&mut key, A_HOST, A).await;
    let s = e.reveal_start(B_HOST).await;
    assert_eq!(s.status, 400);
    assert_eq!(s.body["error"], NEED_PASSKEY);
    // and another user has none
    let o = e
        .send_as(OTHER, A_HOST, None, "POST", &e.reveal_path("start"), None)
        .await;
    assert_eq!(o.status, 404, "personal vault of ME: {}", o.body);
}

#[tokio::test]
async fn a_replayed_challenge_fails() {
    let e = setup().await;
    let mut key = SoftPasskey::new(true);
    e.register(&mut key, A_HOST, A).await;
    let s = e.reveal_start(A_HOST).await;
    let a = e.answer(&mut key, s.body, A);
    assert_eq!(e.reveal_finish(A_HOST, &a, false).await.status, 200);

    // the same answer again: the challenge is used up
    let again = e.reveal_finish(A_HOST, &a, false).await;
    assert_eq!(again.status, 400);
    assert_eq!(again.body["error"], CHALLENGE_GONE);
    assert!(again.body.get("value").is_none());

    // the old answer against a new challenge: wrong challenge
    let s2 = e.reveal_start(A_HOST).await;
    assert_eq!(s2.status, 200);
    let old = e.reveal_finish(A_HOST, &a, false).await;
    assert_eq!(old.status, 400);
    assert_eq!(old.body["error"], NOT_VERIFIED);
    // and that failed attempt consumed the new challenge too
    let a2 = e.answer(&mut key, s2.body, A);
    assert_eq!(
        e.reveal_finish(A_HOST, &a2, false).await.body["error"],
        CHALLENGE_GONE
    );
    assert_eq!(
        e.count("SELECT count(*) FROM secret_uses WHERE mode = 'view'"),
        1
    );
    assert!(e.count("SELECT count(*) FROM audit WHERE action = 'vault.reveal_refused'") >= 3);
}

#[tokio::test]
async fn an_expired_challenge_fails() {
    let e = setup().await;
    let mut key = SoftPasskey::new(true);
    e.register(&mut key, A_HOST, A).await;
    let s = e.reveal_start(A_HOST).await;
    let a = e.answer(&mut key, s.body, A);
    e.env.clock.advance(CHALLENGE_TTL_MS + 1);
    let f = e.reveal_finish(A_HOST, &a, false).await;
    assert_eq!(f.status, 400);
    assert_eq!(f.body["error"], CHALLENGE_GONE);

    // just inside the window still works
    let s = e.reveal_start(A_HOST).await;
    let a = e.answer(&mut key, s.body, A);
    e.env.clock.advance(CHALLENGE_TTL_MS - 1);
    assert_eq!(e.reveal_finish(A_HOST, &a, false).await.status, 200);
}

#[tokio::test]
async fn another_origin_fails() {
    let e = setup().await;
    let mut key = SoftPasskey::new(true);
    e.register(&mut key, A_HOST, A).await;

    // signed on another origin under the RP ID (a subdomain page relaying the challenge); a browser
    // refuses origins outside the RP ID, so this is the case the server has to catch
    let s = e.reveal_start(A_HOST).await;
    let rcr: RequestChallengeResponse = serde_json::from_value(s.body).expect("rcr");
    let a = key
        .do_authentication(
            Url::parse("https://evil.steve.example.dev").expect("url"),
            rcr,
        )
        .expect("soft assertion");
    let f = e
        .reveal_finish(A_HOST, &serde_json::to_value(a).expect("json"), false)
        .await;
    assert_eq!(f.status, 400);
    assert_eq!(f.body["error"], NOT_VERIFIED);

    // same RP ID, other port: an origin webauthn-rs doesn't allow
    let s = e.reveal_start(A_HOST).await;
    let a = e.answer(&mut key, s.body, "https://steve.example.dev:9999");
    let f = e.reveal_finish(A_HOST, &a, false).await;
    assert_eq!(f.status, 400);
    assert_eq!(f.body["error"], NOT_VERIFIED);

    // started on A, finished through B
    let s = e.reveal_start(A_HOST).await;
    let a = e.answer(&mut key, s.body, A);
    let f = e.reveal_finish(B_HOST, &a, false).await;
    assert_eq!(f.status, 400);
    assert_eq!(f.body["error"], OTHER_ORIGIN);

    // registering for B through A's host is refused the same way
    let s = e
        .send(
            A_HOST,
            Some(FRESH),
            "POST",
            "/passkeys/register/start",
            Some(json!({"name": "x"})),
        )
        .await;
    let ccr: CreationChallengeResponse = serde_json::from_value(s.body).expect("ccr");
    let mut k2 = SoftPasskey::new(true);
    let cred = k2
        .do_registration(
            Url::parse("https://steve.example.dev:9999").expect("url"),
            ccr,
        )
        .expect("soft registration");
    let f = e
        .send(
            A_HOST,
            Some(FRESH),
            "POST",
            "/passkeys/register/finish",
            Some(json!({"credential": cred})),
        )
        .await;
    assert_eq!(f.status, 400, "{}", f.body);
    assert_eq!(e.count("SELECT count(*) FROM secret_uses"), 0);
    assert_eq!(e.count("SELECT count(*) FROM passkeys"), 1);
}

#[tokio::test]
async fn registration_needs_a_recent_login() {
    let e = setup().await;
    let body = Some(json!({"name": "MacBook"}));
    for token in [Some(OLD), None, Some(OTHERS), Some("tok-unknown")] {
        let r = e
            .send(
                A_HOST,
                token,
                "POST",
                "/passkeys/register/start",
                body.clone(),
            )
            .await;
        assert_eq!(r.status, 401, "{token:?}");
        assert_eq!(r.body, json!({"error": "请重新登录后再登记"}));
    }
    // finish is checked too: the login went stale while the dialog was open
    let s = e
        .send(
            A_HOST,
            Some(FRESH),
            "POST",
            "/passkeys/register/start",
            body.clone(),
        )
        .await;
    assert_eq!(s.status, 200);
    let ccr: CreationChallengeResponse = serde_json::from_value(s.body).expect("ccr");
    let cred = SoftPasskey::new(true)
        .do_registration(Url::parse(A).expect("url"), ccr)
        .expect("reg");
    e.env.clock.advance(FRESH_LOGIN_MS);
    let f = e
        .send(
            A_HOST,
            Some(FRESH),
            "POST",
            "/passkeys/register/finish",
            Some(json!({"credential": cred})),
        )
        .await;
    assert_eq!(f.status, 401);
    assert_eq!(e.count("SELECT count(*) FROM passkeys"), 0);
    // bad name
    let n = e
        .send(
            A_HOST,
            Some(FRESH),
            "POST",
            "/passkeys/register/start",
            Some(json!({"name": " "})),
        )
        .await;
    assert_eq!(n.status, 400);
}

#[tokio::test]
async fn delete_only_own_passkeys() {
    let e = setup().await;
    let mut key = SoftPasskey::new(true);
    let out = e.register(&mut key, A_HOST, A).await;
    let id = out["id"].as_str().expect("id");
    let p = format!("/passkeys/{id}");
    let o = e.send_as(OTHER, A_HOST, None, "DELETE", &p, None).await;
    assert_eq!(o.status, 404);
    assert_eq!(o.body, json!({"error": "没有这个通行密钥"}));
    let d = e.send(A_HOST, None, "DELETE", &p, None).await;
    assert_eq!(d.status, 200);
    assert_eq!(d.body, json!({"ok": true}));
    assert_eq!(e.reveal_start(A_HOST).await.body["error"], NEED_PASSKEY);
    assert_eq!(
        e.count("SELECT count(*) FROM audit WHERE action = 'passkey.delete'"),
        1
    );
}

#[tokio::test]
async fn status_counts_passkeys() {
    let e = setup().await;
    let ks = Arc::new(FileKeyStore::open(e._keys.path()).expect("keys"));
    let v = Vault::new(e.env.store().clone(), ks);
    assert_eq!(v.status(WS).expect("status").passkeys, 0);
    let mut key = SoftPasskey::new(true);
    e.register(&mut key, A_HOST, A).await;
    e.register(&mut key, B_HOST, B).await;
    assert_eq!(v.status(WS).expect("status").passkeys, 2);
}

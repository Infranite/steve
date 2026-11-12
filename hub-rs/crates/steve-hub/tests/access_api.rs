//! A1: signing a client in with a user's name and an access token, and the client it then is.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    db: std::path::PathBuf,
    base: String,
    http: reqwest::Client,
}

impl Env {
    async fn call(
        &self,
        method: &str,
        path: &str,
        auth: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self.http.request(m, format!("{}{path}", self.base));
        if let Some(a) = auth {
            req = req.header("authorization", format!("Bearer {a}"));
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.expect("request");
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    fn store(&self) -> steve_store::Store {
        steve_store::Store::open(&self.db, steve_store::FnPublisher(|_| {})).expect("store")
    }
}

async fn start() -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    assert_ne!(port, 8440);
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_MODELS_FAST", "claude-haiku-5-5".into()),
        ("STEVE_MODELS_SMART", "claude-sonnet-5-5".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_oidc(w.oidc.clone());
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        tokio::spawn(job);
    }
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    // the development login makes the owner
    http.get(format!("{base}/auth/dev"))
        .send()
        .await
        .expect("dev login");
    Env {
        _home: home,
        db: cfg.db_path(),
        base,
        http,
    }
}

#[tokio::test]
async fn a_client_signs_in_with_a_token_and_is_known_by_its_credential() {
    let e = start().await;
    let (s, hello) = e.call("GET", "/auth/hello", None, None).await;
    assert_eq!(s, 200);
    assert_eq!(
        (hello["name"].as_str(), hello["api"].as_u64()),
        (Some("steve"), Some(1))
    );
    assert_eq!(hello["login"]["token"], true);
    // the owner the development login made, and a token for them
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "test", None).unwrap();
    let client = json!({"kind": "web", "name": "测试浏览器", "platform": "test", "version": "1"});
    let (s, bad) = e
        .call(
            "POST",
            "/auth/token",
            None,
            Some(json!({"user": owner.name, "token": "stt_wrong", "client": client})),
        )
        .await;
    assert_eq!(s, 401, "{bad}");
    let (s, ok) = e
        .call(
            "POST",
            "/auth/token",
            None,
            Some(json!({"user": owner.name, "token": token, "client": client})),
        )
        .await;
    assert_eq!(s, 200, "{ok}");
    let cred = ok["credential"].as_str().expect("credential").to_string();
    let id = ok["client_id"].as_str().expect("id").to_string();
    // the credential is the login: who, and with which client by which route
    let (s, me) = e.call("GET", "/api/me", Some(&cred), None).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["user"]["id"], json!(owner.id));
    assert_eq!(me["client"]["id"], json!(id));
    assert_eq!(me["client"]["kind"], "web");
    assert_eq!(me["client"]["route"], "loopback");
    let (s, _) = e.call("GET", "/api/me", Some("cli_x.nope"), None).await;
    assert_eq!(s, 401);
    // revoked: signed out at once
    assert!(st.access().client_revoke(&id).unwrap());
    let (s, _) = e.call("GET", "/api/me", Some(&cred), None).await;
    assert_eq!(s, 401);
}

/// A2: a page served by the Hub signs in with a token and keeps the credential in an HttpOnly cookie.
#[tokio::test]
async fn a_page_signs_in_with_a_token_into_a_cookie_and_out_again() {
    let e = start().await;
    let (s, unauth) = e.call("GET", "/api/me", None, None).await;
    assert_eq!(s, 401);
    assert_eq!(unauth["token"], true, "{unauth}");
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "page", None).unwrap();
    let r = e
        .http
        .post(format!("{}/auth/token", e.base))
        .json(&json!({"user": owner.name, "token": token, "cookie": true, "client": {"kind": "web", "name": "Safari"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let set = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_client="))
        .expect("client cookie")
        .to_string();
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Lax"),
        "{set}"
    );
    let cookie = set.split(';').next().unwrap().to_string();
    let me = |c: String| {
        let (http, base) = (e.http.clone(), e.base.clone());
        async move {
            let r = http
                .get(format!("{base}/api/me"))
                .header("cookie", c)
                .send()
                .await
                .unwrap();
            let s = r.status().as_u16();
            (s, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (s, v) = me(cookie.clone()).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["client"]["name"], "Safari");
    // signing in again in the same browser replaces the cookie: the client the old one named is
    // signed out, not left in the list with a credential nobody holds
    let r = e
        .http
        .post(format!("{}/auth/token", e.base))
        .header("cookie", &cookie)
        .json(&json!({"user": owner.name, "token": token, "cookie": true, "client": {"kind": "web", "name": "Safari again"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let old = cookie;
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_client="))
        .and_then(|c| c.split(';').next())
        .expect("client cookie")
        .to_string();
    let (s, _) = me(old).await;
    assert_eq!(s, 401, "the replaced client is signed out");
    let (s, v) = me(cookie.clone()).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["client"]["name"], "Safari again");
    let r = e
        .http
        .post(format!("{}/auth/client/logout", e.base))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert!(
        r.headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|c| c.starts_with("steve_client=;") && c.contains("Max-Age=0"))
    );
    let (s, _) = me(cookie).await;
    assert_eq!(s, 401, "the client is revoked, not only its cookie cleared");
}

/// A3: a web client served on its own origin calls the Hub across origins once it signed in there.
#[tokio::test]
async fn a_web_client_on_another_origin_is_let_in_once_it_signed_in() {
    let e = start().await;
    let web = "http://127.0.0.1:1"; // the separate web client's origin
    let preflight = |path: &str| {
        let (http, url) = (e.http.clone(), format!("{}{path}", e.base));
        async move {
            let r = http
                .request(reqwest::Method::OPTIONS, url)
                .header("origin", web)
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "x-steve-ws")
                .send()
                .await
                .unwrap();
            (
                r.status().as_u16(),
                r.headers()
                    .get("access-control-allow-origin")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            )
        }
    };
    // signing in is open to any origin; the API is not, yet
    assert_eq!(preflight("/auth/token").await, (204, Some(web.into())));
    assert_eq!(preflight("/api/state").await.0, 403);
    // a 401 is readable from there, or the page could not tell it must sign in
    let r = e
        .http
        .get(format!("{}/api/me", e.base))
        .header("origin", web)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    assert_eq!(r.headers().get("access-control-allow-origin").unwrap(), web);
    // …and that its origin is not let in, so the page offers the token sign-in that lets it in
    assert_eq!(r.headers().get("x-steve-origin").unwrap(), "refused");
    assert!(
        r.headers()["access-control-expose-headers"]
            .to_str()
            .unwrap()
            .contains("x-steve-origin")
    );
    // signed in from that origin: the API answers it
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "web", None).unwrap();
    let r = e
        .http
        .post(format!("{}/auth/token", e.base))
        .header("origin", web)
        .json(
            &json!({"user": owner.name, "token": token, "cookie": true, "client": {"kind": "web"}}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(preflight("/api/state").await, (204, Some(web.into())));
    let r = e
        .http
        .get(format!("{}/api/me", e.base))
        .header("origin", web)
        .send()
        .await
        .unwrap();
    assert!(
        r.headers().get("x-steve-origin").is_none(),
        "a let-in origin is not told it is refused"
    );
    // another origin still is not
    let r = e
        .http
        .request(reqwest::Method::OPTIONS, format!("{}/api/state", e.base))
        .header("origin", "http://evil.example")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

/// A4: 设置 · 访问与客户端 — your own tokens and clients, and only yours.
#[tokio::test]
async fn the_settings_list_and_revoke_your_own_tokens_and_clients() {
    let e = start().await;
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "first", None).unwrap();
    let (_, ok) = e
        .call("POST", "/auth/token", None, Some(json!({"user": owner.name, "token": token, "client": {"kind": "mac", "name": "MacBook"}})))
        .await;
    let cred = ok["credential"].as_str().unwrap().to_string();
    let (s, v) = e.call("GET", "/api/access", Some(&cred), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["current"], ok["client_id"]);
    assert_eq!(v["clients"][0]["kind"], "mac");
    assert_eq!(v["tokens"].as_array().unwrap().len(), 1);
    assert!(
        v["tokens"][0].get("hash").is_none(),
        "no token material listed"
    );
    // a new token is shown once
    let (s, made) = e
        .call(
            "POST",
            "/api/access/tokens",
            Some(&cred),
            Some(json!({"name": "iPhone"})),
        )
        .await;
    assert_eq!(s, 200, "{made}");
    assert!(made["token"].as_str().unwrap().starts_with("stt_"));
    let id = made["item"]["id"].as_str().unwrap().to_string();
    // someone else's token or client is not yours to revoke
    let other = st
        .users()
        .create("local:other", "Other", None, "member")
        .unwrap();
    let (theirs, _) = st.access().token_create(&other, "theirs", None).unwrap();
    let (s, _) = e
        .call(
            "DELETE",
            &format!("/api/access/tokens/{}", theirs.id),
            Some(&cred),
            None,
        )
        .await;
    assert_eq!(s, 404);
    assert!(
        st.access()
            .token(&theirs.id)
            .unwrap()
            .unwrap()
            .revoked
            .is_none()
    );
    // yours: revoked; and revoking the token this client signed in with signs it out
    let (s, _) = e
        .call(
            "DELETE",
            &format!("/api/access/tokens/{id}"),
            Some(&cred),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let first = v["tokens"][0]["id"].as_str().unwrap().to_string();
    let (s, _) = e
        .call(
            "DELETE",
            &format!("/api/access/tokens/{first}"),
            Some(&cred),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = e.call("GET", "/api/access", Some(&cred), None).await;
    assert_eq!(s, 401);
}

/// A4b: single sign-on set up in the settings takes effect at once and can be turned off; only the owner.
#[tokio::test]
async fn single_sign_on_is_set_up_in_the_settings_without_a_restart() {
    let e = start().await;
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "o", None).unwrap();
    let (_, ok) = e
        .call(
            "POST",
            "/auth/token",
            None,
            Some(json!({"user": owner.name, "token": token})),
        )
        .await;
    let cred = ok["credential"].as_str().unwrap().to_string();
    let (s, v) = e.call("GET", "/api/access/oidc", Some(&cred), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["source"].as_str(), v["enabled"].as_bool()),
        (Some("none"), Some(false))
    );
    let (_, unauth) = e.call("GET", "/api/me", None, None).await;
    assert_ne!(unauth["login"], "/auth/login");
    // a plain-http issuer elsewhere is refused
    let (s, _) = e
        .call(
            "PUT",
            "/api/access/oidc",
            Some(&cred),
            Some(json!({"issuer": "http://id.example", "client_id": "steve"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, v) = e
        .call("PUT", "/api/access/oidc", Some(&cred), Some(json!({"issuer": "https://id.example/o/steve/", "client_id": "steve", "secret": "s3cret"})))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["issuer"], "https://id.example/o/steve");
    assert_eq!(
        (v["has_secret"].as_bool(), v["enabled"].as_bool()),
        (Some(true), Some(true))
    );
    assert!(
        !v.to_string().contains("s3cret"),
        "the secret is never shown"
    );
    // in force now: the sign-in page offers it
    let (_, unauth) = e.call("GET", "/api/me", None, None).await;
    assert_eq!(unauth["login"], "/auth/login");
    let (_, hello) = e.call("GET", "/auth/hello", None, None).await;
    assert_eq!(hello["login"]["oidc"], true);
    // a member may not change it
    let m = st
        .users()
        .create("local:m", "Member", None, "member")
        .unwrap();
    let (_, mt) = st.access().token_create(&m, "m", None).unwrap();
    let (_, mok) = e
        .call(
            "POST",
            "/auth/token",
            None,
            Some(json!({"user": "Member", "token": mt})),
        )
        .await;
    let mcred = mok["credential"].as_str().unwrap().to_string();
    let (s, _) = e
        .call("DELETE", "/api/access/oidc", Some(&mcred), None)
        .await;
    assert_eq!(s, 403);
    // off again
    let (s, v) = e
        .call("DELETE", "/api/access/oidc", Some(&cred), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["enabled"], false);
    let (_, unauth) = e.call("GET", "/api/me", None, None).await;
    assert_ne!(unauth["login"], "/auth/login");
}

#[tokio::test]
async fn qr_pairing_mints_a_single_use_client_credential_and_reports_claim() {
    let e = start().await;
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "mac", None).unwrap();
    let (_, ok) = e
        .call(
            "POST",
            "/auth/token",
            None,
            Some(json!({"user": owner.name, "token": token, "client": {"kind": "mac", "name": "MacBook"}})),
        )
        .await;
    let mac_cred = ok["credential"].as_str().unwrap().to_string();

    // Create a QR pairing code from the signed-in macOS/Web client
    let (s, pair) = e
        .call(
            "POST",
            "/api/access/pairings",
            Some(&mac_cred),
            Some(json!({"name": "Home Hub"})),
        )
        .await;
    assert_eq!(s, 200, "{pair}");
    let pair_id = pair["id"].as_str().expect("id").to_string();
    let code = pair["code"].as_str().expect("code").to_string();
    assert!(code.starts_with("stp_"));
    assert_eq!(code.len(), 4 + 32, "short enough for a small QR code");
    assert_eq!(
        pair["loopback_only"], true,
        "test Hub listens on 127.0.0.1 only"
    );
    assert_eq!(
        pair["links"],
        json!([]),
        "no address another device could reach, so no link to show"
    );

    // Status starts as pending
    let (s, status) = e
        .call(
            "GET",
            &format!("/api/access/pairings/{pair_id}"),
            Some(&mac_cred),
            None,
        )
        .await;
    assert_eq!(s, 200, "{status}");
    assert_eq!(status["status"], "pending");

    // Claim the pairing code on iOS via /auth/pair
    let (s, claimed) = e
        .call(
            "POST",
            "/auth/pair",
            None,
            Some(json!({
                "code": code,
                "client": {"kind": "ios", "name": "iPhone", "platform": "iOS 18", "version": "0.3.0"}
            })),
        )
        .await;
    assert_eq!(s, 200, "{claimed}");
    let ios_cred = claimed["credential"]
        .as_str()
        .expect("ios cred")
        .to_string();
    let ios_id = claimed["client_id"].as_str().expect("ios id").to_string();

    // Pairing code is single-use: re-claiming fails with 401
    let (s, _) = e
        .call(
            "POST",
            "/auth/pair",
            None,
            Some(json!({"code": code, "client": {"kind": "ios", "name": "Replay"}})),
        )
        .await;
    assert_eq!(s, 401);

    // Status now reports claimed with the paired client details
    let (s, status) = e
        .call(
            "GET",
            &format!("/api/access/pairings/{pair_id}"),
            Some(&mac_cred),
            None,
        )
        .await;
    assert_eq!(s, 200, "{status}");
    assert_eq!(status["status"], "claimed");
    assert_eq!(status["client"]["id"], json!(ios_id));
    assert_eq!(status["client"]["kind"], "ios");

    // Paired iOS client authenticates to /api/me and can be revoked independently
    let (s, me) = e.call("GET", "/api/me", Some(&ios_cred), None).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["client"]["id"], json!(ios_id));
    assert_eq!(me["client"]["kind"], "ios");

    let (s, _) = e
        .call(
            "DELETE",
            &format!("/api/access/clients/{ios_id}"),
            Some(&mac_cred),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = e.call("GET", "/api/me", Some(&ios_cred), None).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn qr_paired_clients_inherit_token_revocation_and_pending_codes_drop_on_logout() {
    let e = start().await;
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (tok_row, token) = st.access().token_create(&owner.id, "laptop", None).unwrap();
    let (_, ok) = e
        .call(
            "POST",
            "/auth/token",
            None,
            Some(json!({"user": owner.name, "token": token, "client": {"kind": "mac", "name": "Laptop"}})),
        )
        .await;
    let laptop_cred = ok["credential"].as_str().unwrap().to_string();

    // Pair phone A from laptop -> phone A inherits laptop's token_id
    let (_, pair_a) = e
        .call(
            "POST",
            "/api/access/pairings",
            Some(&laptop_cred),
            Some(json!({"name": "Laptop Hub"})),
        )
        .await;
    let code_a = pair_a["code"].as_str().unwrap().to_string();
    let (s, claimed_a) = e
        .call(
            "POST",
            "/auth/pair",
            None,
            Some(json!({"code": code_a, "client": {"kind": "ios", "name": "Phone A"}})),
        )
        .await;
    assert_eq!(s, 200, "{claimed_a}");
    let phone_a_cred = claimed_a["credential"].as_str().unwrap().to_string();
    let phone_a_id = claimed_a["client_id"].as_str().unwrap().to_string();
    assert_eq!(
        st.access()
            .client(&phone_a_id)
            .unwrap()
            .unwrap()
            .token_id
            .as_deref(),
        Some(tok_row.id.as_str()),
        "paired client carries creator's token_id"
    );

    // Create another pending pairing B, then sign out laptop: pending code B is immediately unusable
    let (_, pair_b) = e
        .call("POST", "/api/access/pairings", Some(&laptop_cred), None)
        .await;
    let code_b = pair_b["code"].as_str().unwrap().to_string();
    let (s, _) = e
        .call("POST", "/auth/client/logout", Some(&laptop_cred), None)
        .await;
    assert_eq!(s, 200);
    let (s, _) = e
        .call(
            "POST",
            "/auth/pair",
            None,
            Some(json!({"code": code_b, "client": {"kind": "ios", "name": "Phone B"}})),
        )
        .await;
    assert_eq!(s, 401, "creator logout drops its unclaimed pairing codes");

    // Phone A is still signed in after laptop's individual logout, until token is revoked
    let (s, _) = e.call("GET", "/api/me", Some(&phone_a_cred), None).await;
    assert_eq!(s, 200);

    // Revoking laptop's access token also revokes Phone A
    st.access().token_revoke(&tok_row.id).unwrap();
    let (s, _) = e.call("GET", "/api/me", Some(&phone_a_cred), None).await;
    assert_eq!(s, 401, "revoking creator token revokes paired clients");
}

#[tokio::test]
async fn failed_qr_pair_attempts_do_not_lock_out_token_sign_in() {
    let e = start().await;
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "desk", None).unwrap();

    // Exhaust 10 failed /auth/pair calls from a forwarded external IP
    for _ in 0..10 {
        let r = e
            .http
            .post(format!("{}/auth/pair", e.base))
            .header("x-forwarded-for", "198.51.100.42")
            .json(&json!({"code": "stp_wrong", "client": {"kind": "ios"}}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 401);
    }
    // 11th /auth/pair from that IP hits 429
    let r = e
        .http
        .post(format!("{}/auth/pair", e.base))
        .header("x-forwarded-for", "198.51.100.42")
        .json(&json!({"code": "stp_wrong", "client": {"kind": "ios"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 429);

    // /auth/token from the same IP still succeeds (separate limiter)
    let r = e
        .http
        .post(format!("{}/auth/token", e.base))
        .header("x-forwarded-for", "198.51.100.42")
        .json(&json!({"user": owner.name, "token": token, "client": {"kind": "web"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
}

#[tokio::test]
async fn qr_pair_mint_failure_rolls_back_claim_so_retry_succeeds() {
    let e = start().await;
    let st = e.store();
    let owner = st.access().owner().unwrap().expect("owner");
    let (_, token) = st.access().token_create(&owner.id, "desk", None).unwrap();

    let (_, ok) = e
        .call(
            "POST",
            "/auth/token",
            None,
            Some(json!({"user": owner.name, "token": token, "client": {"kind": "mac", "name": "Desk Mac"}})),
        )
        .await;
    let mac_cred = ok["credential"].as_str().unwrap().to_string();

    let (_, pair_res) = e
        .call("POST", "/api/access/pairings", Some(&mac_cred), None)
        .await;
    let pair_id = pair_res["id"].as_str().unwrap().to_string();
    let code = pair_res["code"].as_str().unwrap().to_string();

    // Force `client_create` to fail inside SQLite during `POST /auth/pair`
    {
        let conn = rusqlite::Connection::open(&e.db).expect("open sqlite");
        conn.execute_batch(
            "CREATE TRIGGER test_fail_client_mint
             BEFORE INSERT ON clients
             BEGIN
                 SELECT RAISE(ABORT, 'simulated mint failure');
             END;",
        )
        .expect("create trigger");
    }

    let (s, _) = e
        .call(
            "POST",
            "/auth/pair",
            None,
            Some(json!({"code": code, "client": {"kind": "ios", "name": "iPhone"}})),
        )
        .await;
    assert_eq!(s, 500, "mint failure returns 500");

    // Pairing status must remain `pending` (not falsely reported as `claimed`)
    let (s, status) = e
        .call(
            "GET",
            &format!("/api/access/pairings/{pair_id}"),
            Some(&mac_cred),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(status["status"], "pending");

    // Drop the fault trigger and confirm the same pairing code can now be claimed cleanly
    {
        let conn = rusqlite::Connection::open(&e.db).expect("open sqlite");
        conn.execute_batch("DROP TRIGGER test_fail_client_mint;")
            .expect("drop trigger");
    }

    let (s, claimed) = e
        .call(
            "POST",
            "/auth/pair",
            None,
            Some(json!({"code": code, "client": {"kind": "ios", "name": "iPhone"}})),
        )
        .await;
    assert_eq!(s, 200, "{claimed}");
    assert!(claimed["credential"].as_str().is_some());

    let (s, status_after) = e
        .call(
            "GET",
            &format!("/api/access/pairings/{pair_id}"),
            Some(&mac_cred),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(status_after["status"], "claimed");
}


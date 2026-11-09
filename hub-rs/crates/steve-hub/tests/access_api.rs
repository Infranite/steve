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
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
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

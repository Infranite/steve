//! API errors in the request's language: English with `Accept-Language: en` (or `?lang=en`), the Chinese text
//! of before without one. Temp STEVE_HOME, adapters `/usr/bin/false`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::Method;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
}

impl Env {
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
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
    let r = http
        .get(format!("{base}/auth/dev"))
        .send()
        .await
        .expect("dev login");
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("session cookie");
    Env {
        _home: home,
        base,
        http,
        cookie,
    }
}

/// `(status, error)` of one call.
async fn error_of(r: reqwest::RequestBuilder) -> (u16, String) {
    let r = r.send().await.expect("send");
    let s = r.status().as_u16();
    let v: Value = r.json().await.expect("json");
    (s, v["error"].as_str().unwrap_or_default().to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn api_errors_follow_accept_language() {
    let e = start().await;
    let tag = json!({"name": "i18n", "cat": "主题", "description": "d"});

    // validation (error.rs `Invalid`)
    let bad = json!({"name": "", "cat": "主题", "description": "d"});
    assert_eq!(
        error_of(e.req(Method::POST, "/api/tags").json(&bad)).await,
        (400, "参数不对：name".into())
    );
    assert_eq!(
        error_of(
            e.req(Method::POST, "/api/tags")
                .header("accept-language", "en-US,en;q=0.9")
                .json(&bad)
        )
        .await,
        (400, "Invalid parameters: name".into())
    );

    // a 404 noun (`own()`, built in spawn_blocking)
    assert_eq!(
        error_of(e.req(Method::DELETE, "/api/tags/tag_nope")).await,
        (404, "没有这个标签".into())
    );
    assert_eq!(
        error_of(
            e.req(Method::DELETE, "/api/tags/tag_nope")
                .header("accept-language", "en")
        )
        .await,
        (404, "No such tag".into())
    );
    assert_eq!(
        error_of(e.req(Method::DELETE, "/api/tags/tag_nope?lang=en")).await,
        (404, "No such tag".into())
    );

    // an explicit refusal
    let r = e
        .req(Method::POST, "/api/tags")
        .json(&tag)
        .send()
        .await
        .expect("create");
    assert_eq!(r.status().as_u16(), 200, "{:?}", r.text().await);
    assert_eq!(
        error_of(e.req(Method::POST, "/api/tags").json(&tag)).await,
        (400, "已有同名标签".into())
    );
    assert_eq!(
        error_of(
            e.req(Method::POST, "/api/tags")
                .header("accept-language", "en")
                .json(&tag)
        )
        .await,
        (400, "A tag with this name already exists".into())
    );
    // a language we don't have is zh-CN
    assert_eq!(
        error_of(
            e.req(Method::POST, "/api/tags")
                .header("accept-language", "fr-FR")
                .json(&tag)
        )
        .await,
        (400, "已有同名标签".into())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_texts_follow_accept_language() {
    let e = start().await;

    // the workspace refusal (`lookup_user`)
    let gone = |lang: Option<&str>| {
        let r = e
            .req(Method::GET, "/api/me")
            .header("x-steve-ws", "ws_ffffffffffffffff");
        match lang {
            Some(l) => r.header("accept-language", l),
            None => r,
        }
    };
    assert_eq!(
        error_of(gone(None)).await,
        (409, "这个页面所在的空间已经不可用了，请刷新页面".into())
    );
    assert_eq!(
        error_of(gone(Some("en"))).await,
        (
            409,
            "The space this page belongs to is no longer available. Please refresh the page."
                .into()
        )
    );

    // a handler's own refusal with a value in it
    assert_eq!(
        error_of(e.req(Method::GET, "/api/system/models?harness=nope")).await,
        (400, "没有这个 harness：nope".into())
    );
    assert_eq!(
        error_of(e.req(Method::GET, "/api/system/models?harness=nope&lang=en")).await,
        (400, "No such harness: nope".into())
    );

    // `/api/state` is cached per language: the runtimes' notes (cached as codes) come out in each
    let state = |lang: &'static str| {
        let r = e
            .req(Method::GET, "/api/state")
            .header("accept-language", lang);
        async move {
            let v: Value = r.send().await.expect("state").json().await.expect("json");
            v["runtimes"][0]["note"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        }
    };
    assert_eq!(state("zh-CN").await, "没有找到 claude");
    assert_eq!(state("en").await, "claude not found");
    assert_eq!(state("zh-CN").await, "没有找到 claude");
}

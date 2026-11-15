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
    assert_eq!(state("zh-CN").await, "通过 ACP 适配器运行");
    assert_eq!(state("en").await, "Runs through the ACP adapter");
    assert_eq!(state("zh-CN").await, "通过 ACP 适配器运行");
}

/// What the installation seeded (the workspace's name, the main Bot's role, speaking style and rules, the
/// first meeting's welcome, the default channel persona) reads in the request's language while it is still
/// as seeded; zh-CN is the stored text; the seed sent back unchanged from an English page stays the seed;
/// what someone wrote is shown as written in both.
#[tokio::test(flavor = "multi_thread")]
async fn seeded_defaults_follow_the_language_until_edited() {
    let e = start().await;
    let get = |path: &'static str, lang: &'static str| {
        let r = e.req(Method::GET, path).header("accept-language", lang);
        async move {
            let v: Value = r.send().await.expect("get").json().await.expect("json");
            v
        }
    };
    let send = |method: Method, path: String, lang: &'static str, body: Value| {
        let r = e
            .req(method, &path)
            .header("accept-language", lang)
            .json(&body);
        async move {
            let r = r.send().await.expect("send");
            assert!(r.status().is_success(), "{path}: {}", r.status());
        }
    };
    let steve = |s: &Value| {
        s["bots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == "steve")
            .cloned()
            .unwrap()
    };
    let welcome = |s: &Value| {
        s["thread"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["meta"]["onboard"] == true)
            .map(|m| m["text"].as_str().unwrap().to_string())
    };

    // the first meeting began in zh-CN: the welcome is stored in Chinese
    send(
        Method::POST,
        "/api/onboard/start".into(),
        "zh-CN",
        json!({}),
    )
    .await;

    let zh = get("/api/state", "zh-CN").await;
    assert_eq!(zh["workspace"]["name"], "个人");
    assert_eq!(zh["workspaces"][0]["name"], "个人");
    assert_eq!(steve(&zh)["role"], "你的助手：理解需求、分派、汇报");
    assert_eq!(steve(&zh)["persona"], "沉稳、简洁，先说结论；不客套");
    let rules_zh = steve(&zh)["rules"].clone();
    assert!(welcome(&zh).unwrap().starts_with("你好，我是 Steve。"));

    let en = get("/api/state", "en").await;
    assert_eq!(en["workspace"]["name"], "Personal");
    assert_eq!(en["workspaces"][0]["name"], "Personal");
    assert_eq!(steve(&en)["name"], "Steve");
    assert_eq!(
        steve(&en)["role"],
        "Your assistant: understands requests, hands out work, reports back"
    );
    assert_eq!(
        steve(&en)["persona"],
        "Calm and concise; conclusion first; no pleasantries"
    );
    assert!(steve(&en)["rules"].as_str().unwrap().is_ascii());
    assert!(welcome(&en).unwrap().starts_with("Hi, I'm Steve."));
    let me = get("/api/me", "en").await;
    assert_eq!(me["workspaces"][0]["name"], "Personal");

    // the default persona
    let personas = get("/api/personas", "en").await;
    let intro = personas[0]["intro"].as_str().unwrap().to_string();
    assert_eq!(intro, "I'm Steve; I take in and hand out work.");
    assert_eq!(
        personas[0]["soul"],
        "Calm and concise; conclusion first; no pleasantries"
    );
    let personas_zh = get("/api/personas", "zh-CN").await;
    let intro_zh = personas_zh[0]["intro"].as_str().unwrap().to_string();
    assert_eq!(intro_zh, "我是 Steve，负责接收和分派事情。");
    let pid = personas[0]["id"].as_str().unwrap().to_string();

    // the English page saves the whole form back unchanged: the seeds stay the seeds
    let b = steve(&en);
    send(
        Method::PATCH,
        "/api/bots/steve".into(),
        "en",
        json!({"role": b["role"], "persona": b["persona"], "rules": b["rules"], "model": "x"}),
    )
    .await;
    send(
        Method::PATCH,
        "/api/workspaces/default".into(),
        "en",
        json!({"name": "Personal"}),
    )
    .await;
    send(
        Method::PATCH,
        format!("/api/personas/{pid}"),
        "en",
        json!({"intro": intro, "soul": personas[0]["soul"]}),
    )
    .await;
    let zh = get("/api/state", "zh-CN").await;
    assert_eq!(zh["workspace"]["name"], "个人");
    assert_eq!(steve(&zh)["role"], "你的助手：理解需求、分派、汇报");
    assert_eq!(steve(&zh)["rules"], rules_zh);
    let personas_zh = get("/api/personas", "zh-CN").await;
    assert_eq!(personas_zh[0]["intro"].as_str().unwrap(), intro_zh);
    assert_eq!(personas_zh[0]["soul"], "沉稳、简洁，先说结论；不客套");

    // what someone wrote is theirs, in either language
    send(
        Method::PATCH,
        "/api/bots/steve".into(),
        "en",
        json!({"role": "我的管家", "persona": "Cheerful"}),
    )
    .await;
    send(
        Method::PATCH,
        "/api/workspaces/default".into(),
        "zh-CN",
        json!({"name": "家里"}),
    )
    .await;
    send(
        Method::PATCH,
        format!("/api/personas/{pid}"),
        "en",
        json!({"intro": "Hi, I'm Lee's helper."}),
    )
    .await;
    for lang in ["zh-CN", "en"] {
        let s = get("/api/state", lang).await;
        assert_eq!(s["workspace"]["name"], "家里", "{lang}");
        assert_eq!(steve(&s)["role"], "我的管家", "{lang}");
        assert_eq!(steve(&s)["persona"], "Cheerful", "{lang}");
        let p = get("/api/personas", lang).await;
        assert_eq!(p[0]["intro"], "Hi, I'm Lee's helper.", "{lang}");
    }
}

//! Security boundary regression tests (P0-01, P0-03, P0-04).
//! Runs the full Axum Hub router in-process via `tower::ServiceExt::oneshot` with no sockets.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::Request;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use tower::ServiceExt;

struct TestHub {
    _home: tempfile::TempDir,
    app: axum::Router,
    cookie: String,
    cfg: Arc<Config>,
}

impl TestHub {
    async fn start() -> Self {
        steve_modules::net::forbid_in_this_process();
        let home = tempfile::tempdir().expect("tmp");
        let h = home.path().to_path_buf();
        let mut vars: HashMap<String, String> = HashMap::new();
        for (k, v) in [
            ("HOME", h.display().to_string()),
            ("STEVE_HOME", h.join("steve").display().to_string()),
            ("STEVE_PORT", "18701".into()),
            ("STEVE_HOST", "127.0.0.1".into()),
            ("STEVE_PUBLIC_URL", "http://127.0.0.1:18701".into()),
            ("STEVE_DEV_LOGIN", "1".into()),
            ("STEVE_REPO", "/nonexistent".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("STEVE_NO_RESUME", "1".into()),
            ("STEVE_NODE_LISTEN", "0".into()),
            ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
            ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
            ("STEVE_DISK_FLOOR_GB", "7".into()),
            ("STEVE_DISK_LOW_GB", "9".into()),
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
        let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
        let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
        st.runtimes.set_harnesses(w.harnesses.clone());
        steve_hub::app::boot(&st).await.expect("boot");
        let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);

        let login = Request::builder()
            .method("GET")
            .uri("/auth/dev")
            .header("host", "127.0.0.1:18701")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 18701))))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(login).await.unwrap();
        let status = resp.status();
        let cookie = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("steve_session="))
            .map(|c| c.split(';').next().unwrap_or_default().to_string())
            .unwrap_or_else(|| panic!("dev login failed ({status})"));

        Self {
            _home: home,
            app,
            cookie,
            cfg,
        }
    }

    async fn call_as(
        &self,
        cookie: &str,
        ws: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut cookie_hdr = cookie.to_string();
        if let Some(ws) = ws {
            cookie_hdr.push_str(&format!("; steve_ws={ws}"));
        }
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:18701")
            .header("cookie", cookie_hdr)
            .header("origin", "http://127.0.0.1:18701")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 18701))));
        let body = match body {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self
            .app
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let val = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, val)
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call_as(&self.cookie, None, method, path, body).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn p0_01_custom_harness_api_refuses_env_injection_and_builtin_override() {
    let hub = TestHub::start().await;

    // 1) Arbitrary env injection through POST /api/system/harnesses must be refused.
    let (status, body) = hub
        .call(
            "POST",
            "/api/system/harnesses",
            Some(json!({
                "id": "my-agent",
                "command": "/opt/my-agent",
                "env": {
                    "ANTHROPIC_API_KEY": "sk-ant-fixture",
                    "LD_PRELOAD": "/tmp/evil.so"
                }
            })),
        )
        .await;
    assert_eq!(status, 400, "env injection must be refused: {body}");
    assert_eq!(body["reason"], "config.harness_env_not_allowed", "{body}");
    assert!(
        !hub.cfg.harness_manifest.exists(),
        "refused env injection must not write manifest"
    );

    // 2) Overwriting a built-in harness id must be refused.
    let (status, body) = hub
        .call(
            "POST",
            "/api/system/harnesses",
            Some(json!({
                "id": "claude-code",
                "command": "/tmp/hijacked-claude"
            })),
        )
        .await;
    assert_eq!(status, 403, "built-in override must be refused: {body}");
    assert_eq!(body["reason"], "config.harness_builtin_protected", "{body}");
    assert!(
        !hub.cfg.harness_manifest.exists(),
        "refused built-in override must not write manifest"
    );

    // 3) Removing a built-in harness id must be refused.
    let (status, body) = hub
        .call("DELETE", "/api/system/harnesses/claude-code", None)
        .await;
    assert_eq!(status, 403, "built-in removal must be refused: {body}");
    assert_eq!(body["reason"], "config.harness_builtin_protected", "{body}");

    // 4) Clean custom harness without env succeeds and can be removed.
    let (status, body) = hub
        .call(
            "POST",
            "/api/system/harnesses",
            Some(json!({
                "id": "my-clean-agent",
                "name": "Clean Agent",
                "command": "/opt/clean-agent",
                "args": ["--acp"]
            })),
        )
        .await;
    assert_eq!(status, 200, "clean custom harness should save: {body}");
    let (status, body) = hub
        .call("DELETE", "/api/system/harnesses/my-clean-agent", None)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["removed"], true, "{body}");
}

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

#[tokio::test(flavor = "multi_thread")]
async fn p0_03_extension_started_enforces_readonly_views_and_scoped_bus() {
    use std::sync::Mutex;
    use steve_hub::Assembly;
    use steve_hub::extension::{
        BusTap, Declare, DomainCommands, DomainViews, EventCapability, Extension,
        ExtensionBoundaryError, ExtensionError, Mounted, Started,
    };
    use steve_modules::accounts::connections::{
        AccountInfo, AuthKind, ConnCx, ConnectionProvider, ProviderError,
    };

    struct ProbeProvider;

    #[async_trait::async_trait]
    impl ConnectionProvider for ProbeProvider {
        fn id(&self) -> &str {
            "probe_ext"
        }
        fn system(&self) -> &str {
            "probe"
        }
        fn name(&self) -> &str {
            "Probe"
        }
        fn caps(&self) -> &[&'static str] {
            &["events"]
        }
        fn auth(&self) -> AuthKind {
            AuthKind::Credential
        }
        async fn verify(&self, _cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
            Err(ProviderError::Unsupported)
        }
    }

    #[derive(Clone, Default)]
    struct Handles {
        bus: Arc<Mutex<Option<BusTap>>>,
        views: Arc<Mutex<Option<DomainViews>>>,
        commands: Arc<Mutex<Option<DomainCommands>>>,
    }

    struct ZeroCapExt(Handles);

    impl Extension for ZeroCapExt {
        fn id(&self) -> &str {
            "zero_cap"
        }
        fn declare(&self, _d: &mut Declare<'_>) -> Result<(), ExtensionError> {
            Ok(())
        }
        fn start(&self, s: &Started) -> Result<Mounted, ExtensionError> {
            *self.0.bus.lock().unwrap() = Some(s.bus.clone());
            *self.0.views.lock().unwrap() = Some(s.views.clone());
            *self.0.commands.lock().unwrap() = Some(s.commands.clone());
            Ok(Mounted::default())
        }
    }

    struct ScopedExt(Handles);

    impl Extension for ScopedExt {
        fn id(&self) -> &str {
            "scoped_ext"
        }
        fn declare(&self, d: &mut Declare<'_>) -> Result<(), ExtensionError> {
            d.system("probe", "Probe")?;
            d.provider(Arc::new(ProbeProvider))?;
            d.event_capabilities([EventCapability::Config]);
            Ok(())
        }
        fn start(&self, s: &Started) -> Result<Mounted, ExtensionError> {
            *self.0.bus.lock().unwrap() = Some(s.bus.clone());
            *self.0.views.lock().unwrap() = Some(s.views.clone());
            *self.0.commands.lock().unwrap() = Some(s.commands.clone());
            // Stateless router only: extension routes cannot extract Hub `State<Modules>`.
            Ok(Mounted::default().routes(
                axum::Router::new().route("/ping", axum::routing::get(|| async { "pong" })),
            ))
        }
    }

    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().unwrap();
    let h = home.path().to_path_buf();
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", "18702".into()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", "http://127.0.0.1:18702".into()),
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
    ] {
        vars.insert(k.into(), v);
    }
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).unwrap());
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).unwrap();
    }
    let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let zero_h = Handles::default();
    let scoped_h = Handles::default();
    let asm = Assembly::empty()
        .with(ZeroCapExt(zero_h.clone()))
        .with(ScopedExt(scoped_h.clone()));
    let w = steve_hub::wire::wire_assembly(&cfg, &bus, Arc::new(get), &asm).unwrap();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus.clone()).with_modules(w.modules);
    st.runtimes.set_harnesses(w.harnesses.clone());
    steve_hub::app::boot(&st).await.unwrap();
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);

    let login = Request::builder()
        .method("GET")
        .uri("/auth/dev")
        .header("host", "127.0.0.1:18702")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 18702))))
        .body(Body::empty())
        .unwrap();
    let login_resp = app.clone().oneshot(login).await.unwrap();
    let cookie = login_resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("dev login cookie");

    let get_json = |path: &str| {
        let app = app.clone();
        let cookie = cookie.clone();
        let path = path.to_string();
        async move {
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(&path)
                        .header("host", "127.0.0.1:18702")
                        .header("cookie", &cookie)
                        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 18702))))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = resp.status().as_u16();
            let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            (
                status,
                serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)),
            )
        }
    };
    let (s, me) = get_json("/api/me").await;
    assert_eq!(s, 200, "{me}");
    let me_id = me["user"]["id"].as_str().unwrap().to_string();
    let (s, contact) = get_json("/api/me/contact").await;
    assert_eq!(s, 200, "{contact}");
    let me_party = contact["id"]
        .as_str()
        .or_else(|| contact["party"]["id"].as_str())
        .unwrap()
        .to_string();

    let zero_bus = zero_h.bus.lock().unwrap().take().unwrap();
    let zero_views = zero_h.views.lock().unwrap().take().unwrap();
    let scoped_bus = scoped_h.bus.lock().unwrap().take().unwrap();
    let scoped_views = scoped_h.views.lock().unwrap().take().unwrap();
    let _scoped_commands = scoped_h.commands.lock().unwrap().take().unwrap();

    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();

    // 1) Zero-capability extension is denied vault, unknown, capability-gated events, and views.
    metrics::with_local_recorder(&recorder, || {
        assert_eq!(
            zero_bus.tap(&["vault"]).err(),
            Some(ExtensionBoundaryError::VaultEventForbidden {
                extension: "zero_cap".into()
            })
        );
        assert_eq!(
            zero_bus.tap(&["patch"]).err(),
            Some(ExtensionBoundaryError::UnknownEventType {
                extension: "zero_cap".into(),
                event_type: "patch".into()
            })
        );
        assert_eq!(
            zero_bus.tap(&["project"]).err(),
            Some(ExtensionBoundaryError::CapabilityDenied {
                extension: "zero_cap".into(),
                capability: EventCapability::Config,
                target: "project".into()
            })
        );
        assert!(zero_views.authorized_workspaces().unwrap().is_empty());
        assert_eq!(
            zero_views.connections_in_workspace("default"),
            Err(ExtensionBoundaryError::UnauthorizedWorkspace {
                extension: "zero_cap".into(),
                workspace: "default".into()
            })
        );
        assert_eq!(
            zero_views.workspace_bots("default"),
            Err(ExtensionBoundaryError::CapabilityDenied {
                extension: "zero_cap".into(),
                capability: EventCapability::Config,
                target: "workspace_bots".into()
            })
        );
    });

    // 2) Scoped extension has Config capability, no active workspaces yet.
    let mut proj_sub = metrics::with_local_recorder(&recorder, || {
        assert_eq!(
            scoped_bus.tap(&["vault"]).err(),
            Some(ExtensionBoundaryError::VaultEventForbidden {
                extension: "scoped_ext".into()
            })
        );
        assert_eq!(
            scoped_bus.tap(&["item"]).err(),
            Some(ExtensionBoundaryError::CapabilityDenied {
                extension: "scoped_ext".into(),
                capability: EventCapability::Work,
                target: "item".into()
            })
        );
        assert!(scoped_views.authorized_workspaces().unwrap().is_empty());
        assert_eq!(
            scoped_views.workspace_bots("default"),
            Err(ExtensionBoundaryError::UnauthorizedWorkspace {
                extension: "scoped_ext".into(),
                workspace: "default".into()
            })
        );
        scoped_bus.tap(&["project"]).unwrap()
    });

    // Without an active connection, even `default` workspace events are dropped.
    metrics::with_local_recorder(&recorder, || {
        bus.publish_batch(vec![
            steve_types::ConfigEvent::Project {
                id: "p_before_auth".into(),
                ws: "default".into(),
            }
            .into(),
        ]);
    });
    assert!(matches!(
        proj_sub.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    // 3) Seed an active connection for `probe_ext` and a foreign `github_pat` connection in `default`.
    let own_conn = steve_store::testkit::hub::wiring::seed_wired_connection(
        &cfg.db_path(),
        &me_id,
        &me_party,
        "probe_ext",
        "probe",
        "probe-handle",
    )
    .unwrap();
    let foreign_conn = steve_store::testkit::hub::wiring::seed_wired_connection(
        &cfg.db_path(),
        &me_id,
        &me_party,
        "github_pat",
        "github",
        "gh-handle",
    )
    .unwrap();

    // Now `default` is authorized for `scoped_ext`, while `ws_other` and `foreign_conn` stay denied.
    assert_eq!(
        scoped_views
            .authorized_workspaces()
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["default".to_string()]
    );
    let conns = scoped_views.connections_in_workspace("default").unwrap();
    assert_eq!(conns.len(), 1);
    assert_eq!(conns[0].id, own_conn);
    assert_eq!(conns[0].adapter, "probe_ext");
    assert_eq!(
        scoped_views.connection("default", &own_conn).unwrap().id,
        own_conn
    );
    let bots = scoped_views.workspace_bots("default").unwrap();
    assert!(
        bots.iter().any(|b| b.id == "steve"),
        "authorized workspace should expose read-only bot views: {bots:?}"
    );

    metrics::with_local_recorder(&recorder, || {
        assert_eq!(
            scoped_views.connection("default", &foreign_conn),
            Err(ExtensionBoundaryError::UnauthorizedAdapter {
                extension: "scoped_ext".into(),
                adapter: "github_pat".into()
            })
        );
        assert_eq!(
            scoped_views.connection("ws_other", &own_conn),
            Err(ExtensionBoundaryError::UnauthorizedWorkspace {
                extension: "scoped_ext".into(),
                workspace: "ws_other".into()
            })
        );
        assert_eq!(
            scoped_views.workspace_bots("ws_other"),
            Err(ExtensionBoundaryError::UnauthorizedWorkspace {
                extension: "scoped_ext".into(),
                workspace: "ws_other".into()
            })
        );
        bus.publish_batch(vec![
            steve_types::ConfigEvent::Project {
                id: "p_other".into(),
                ws: "ws_other".into(),
            }
            .into(),
            steve_types::ConfigEvent::Project {
                id: "p_default".into(),
                ws: "default".into(),
            }
            .into(),
        ]);
    });

    let got = tokio::time::timeout(std::time::Duration::from_secs(2), proj_sub.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got["id"], "p_default");
    assert_eq!(got["ws"], "default");
    assert!(matches!(
        proj_sub.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    // Verify mounted extension route is reachable at `/api/channels/ext/scoped_ext/ping`.
    let (status, pong) = get_json("/api/channels/ext/scoped_ext/ping").await;
    assert_eq!(status, 200, "{pong}");
    assert_eq!(pong, Value::String("pong".into()));

    let metrics_text = handle.render();
    for (boundary, reason) in [
        ("extension_bus", "vault_forbidden"),
        ("extension_bus", "unknown_event_type"),
        ("extension_bus", "capability_denied"),
        ("extension_views", "workspace_denied"),
        ("extension_views", "adapter_denied"),
        ("extension_views", "capability_denied"),
    ] {
        assert!(
            metrics_text.lines().any(|line| {
                line.starts_with("steve_security_refusals_total{")
                    && line.contains(&format!("boundary=\"{boundary}\""))
                    && line.contains(&format!("reason=\"{reason}\""))
            }),
            "missing metric {boundary}/{reason} in:\n{metrics_text}"
        );
    }
}

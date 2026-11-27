use super::*;
use serde_json::{Value, json};
use steve_node_api::testkit::FakeNode;

#[path = "../../../tests/node_harness/mod.rs"]
mod node_harness;
#[path = "../../../tests/support/setup_node.rs"]
mod setup_node;

fn cap(version: &str, model: &str) -> HarnessCapability {
    serde_json::from_value(json!({
        "name":"fixture-adapter", "version":version, "sessions":true, "jobs":false,
        "models":["declared-only"], "resume":true, "cancel":true, "permissions":true,
        "images":false, "enforcement":[],
        "adapter":{"title":"Fixture adapter", "status":"ready", "version":"adapter-2",
            "install_hint":"@example/fixture-acp",
            "session":{"models":[{"id":model}], "default_model":model}},
        "setup":{"state":"ready", "components":[], "can_install":false}
    }))
    .unwrap()
}

fn local(node: Arc<dyn Node>) -> Target {
    Target {
        id: node.id().to_string(),
        node: Some(node),
        remote: None,
        generation: None,
        upgrade_only: false,
    }
}

#[test]
fn readiness_requires_evidence_and_versions_models_belong_to_the_target() {
    let c = cap("cli-1", "actual/model");
    let h = harness(c.clone());
    assert_eq!(h.available, Some(true));
    assert!(h.installed);
    assert_eq!(h.version, "cli-1");
    assert_eq!(h.adapter_version.as_deref(), Some("adapter-2"));
    assert_eq!(h.default_model.as_deref(), Some("actual/model"));
    assert_eq!(h.install_hint.as_deref(), Some("@example/fixture-acp"));
    assert_eq!(h.models, ["actual/model"]);
    let mut unprobed = c.clone();
    unprobed.adapter = None;
    unprobed.setup = None;
    let u = harness(unprobed);
    assert!(
        u.available.is_none(),
        "a declaration without an adapter report is unknown"
    );
    assert_eq!(u.degraded_reason, Some(HarnessDegradedReason::ProbeUnknown));
    let mut missing = c.clone();
    missing.setup.as_mut().unwrap().state = SetupState::NotInstalled;
    assert_eq!(harness(missing).available, Some(false));
    let mut denied = c;
    let a = denied.adapter.as_mut().unwrap();
    a.session = None;
    a.session_error = Some("sign in on this Node".into());
    assert_eq!(
        harness(denied.clone()).degraded_reason,
        Some(HarnessDegradedReason::SessionUnavailable)
    );
    denied.adapter.as_mut().unwrap().session_auth_required = Some(true);
    let h = harness(denied);
    assert_eq!(h.available, Some(false));
    assert_eq!(
        h.degraded_reason,
        Some(HarnessDegradedReason::AuthenticationRequired)
    );
    assert!(h.error.is_none());

    let mut path_mismatch = cap("cli-1", "actual/model");
    path_mismatch.setup = None;
    let a = path_mismatch.adapter.as_mut().unwrap();
    a.status = AdapterStatus::AdapterMissing;
    a.session = None;
    a.error = Some("the adapter's command is missing".into());
    a.missing_command = Some(steve_node_api::MissingCommandDiagnostic {
        kind: steve_node_api::MissingCommandKind::CommandNotInServicePath,
        command: "agy".into(),
        adapter_package: None,
        detected_cli: None,
        cli_service_path: None,
        service_path: "/usr/bin:/bin:/usr/sbin:/sbin".into(),
        login_shell_path: Some("/Users/example/.local/bin/agy".into()),
        login_shell: Some("/bin/zsh".into()),
        refresh_command: "steve node refresh-env".into(),
    });
    let h = harness(path_mismatch.clone());
    assert_eq!(h.available, Some(false));
    assert_eq!(
        h.degraded_reason,
        Some(HarnessDegradedReason::CommandNotInServicePath)
    );
    let diag = h.missing_command.expect("missing command forwarded");
    assert_eq!(
        diag.kind,
        steve_node_api::MissingCommandKind::CommandNotInServicePath
    );
    assert_eq!(diag.command, "agy");
    assert_eq!(diag.service_path, "/usr/bin:/bin:/usr/sbin:/sbin");
    assert_eq!(
        diag.login_shell_path.as_deref(),
        Some("/Users/example/.local/bin/agy")
    );
    assert_eq!(diag.refresh_command, "steve node refresh-env");

    let diag_mut = path_mismatch
        .adapter
        .as_mut()
        .unwrap()
        .missing_command
        .as_mut()
        .unwrap();
    diag_mut.kind = steve_node_api::MissingCommandKind::BothMissing;
    diag_mut.login_shell_path = None;
    let h = harness(path_mismatch);
    assert_eq!(h.available, Some(false));
    assert_eq!(
        h.degraded_reason,
        Some(HarnessDegradedReason::AdapterMissing)
    );
}

#[test]
fn typed_installed_and_degraded_reasons_distinguish_uninstalled_outdated_and_broken() {
    // 1. Built-in PATH binary installed in login shell but missing from service PATH (e.g. agy).
    let path_mismatch_builtin: HarnessCapability = serde_json::from_value(json!({
        "name": "antigravity", "version": "", "sessions": false, "jobs": false,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": {
            "title": "Antigravity CLI",
            "builtin": true,
            "status": "adapter_missing",
            "missing_command": {
                "kind": "command_not_in_service_path",
                "command": "agy",
                "service_path": "/usr/bin:/bin:/usr/sbin:/sbin",
                "login_shell_path": "/Users/example/.local/bin/agy",
                "login_shell": "/bin/zsh",
                "refresh_command": "steve node refresh-env"
            }
        },
        "setup": null
    }))
    .unwrap();
    let v = harness(path_mismatch_builtin);
    assert!(
        v.installed,
        "login-shell hit proves the command is installed"
    );
    assert_eq!(v.available, Some(false));
    assert_eq!(
        v.degraded_reason,
        Some(HarnessDegradedReason::CommandNotInServicePath)
    );
    assert_eq!(
        v.missing_command
            .as_ref()
            .and_then(|m| m.login_shell_path.as_deref()),
        Some("/Users/example/.local/bin/agy")
    );

    // 2. CLI present on service PATH, ACP adapter package missing (MacBook Pi scenario):
    //    classified as `AdapterPackageMissing` (never `CommandNotInServicePath`) and forwards
    //    `setup.blocked_reason` when execution policy forbids remote installs.
    let cli_present_adapter_missing: HarnessCapability = serde_json::from_value(json!({
        "name": "pi", "version": "", "sessions": false, "jobs": false,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": {
            "title": "Pi",
            "builtin": true,
            "status": "adapter_missing",
            "missing_command": {
                "kind": "adapter_package_missing",
                "command": "openma-pi-acp",
                "adapter_package": "@openma/pi-acp",
                "detected_cli": "pi",
                "cli_service_path": "/usr/local/bin/pi",
                "service_path": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                "refresh_command": "steve node refresh-env"
            }
        },
        "setup": {
            "state": "not_installed",
            "components": [
                { "id": "pi", "name": "Pi SDK", "package": "@earendil-works/pi-coding-agent", "status": "missing", "detected": { "path": "/usr/local/bin/pi" } },
                { "id": "adapter", "name": "Pi", "package": "@openma/pi-acp", "status": "missing" }
            ],
            "can_install": false,
            "blocked_reason": "exec_denied"
        }
    }))
    .unwrap();
    let v = harness(cli_present_adapter_missing);
    assert!(v.installed);
    assert_eq!(v.available, Some(false));
    assert_eq!(
        v.degraded_reason,
        Some(HarnessDegradedReason::AdapterPackageMissing)
    );
    let diag = v.missing_command.expect("diagnostic forwarded");
    assert_eq!(
        diag.kind,
        steve_node_api::MissingCommandKind::AdapterPackageMissing
    );
    assert_eq!(diag.adapter_package.as_deref(), Some("@openma/pi-acp"));
    assert_eq!(diag.detected_cli.as_deref(), Some("pi"));
    assert_eq!(diag.cli_service_path.as_deref(), Some("/usr/local/bin/pi"));
    assert_eq!(diag.login_shell_path, None);
    assert_eq!(
        v.setup.as_ref().and_then(|s| s.blocked_reason),
        Some(steve_types::devices::HarnessUpgradeBlockReason::ExecDenied)
    );

    // 3. Truly uninstalled built-in (both CLI and ACP adapter missing) -> not installed.
    let absent_both_missing: HarnessCapability = serde_json::from_value(json!({
        "name": "pi", "version": "", "sessions": false, "jobs": false,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": {
            "title": "Pi",
            "builtin": true,
            "status": "adapter_missing",
            "missing_command": {
                "kind": "both_missing",
                "command": "openma-pi-acp",
                "adapter_package": "@openma/pi-acp",
                "service_path": "/usr/bin:/bin",
                "refresh_command": "steve node refresh-env"
            }
        },
        "setup": {
            "state": "not_installed",
            "components": [
                { "id": "pi", "name": "Pi SDK", "package": "@earendil-works/pi-coding-agent", "status": "missing" },
                { "id": "adapter", "name": "Pi", "package": "@openma/pi-acp", "status": "missing" }
            ],
            "can_install": true
        }
    }))
    .unwrap();
    let v = harness(absent_both_missing);
    assert!(!v.installed);
    assert_eq!(v.degraded_reason, None);

    // 4. Adapter installed but startup / initialize failed -> `InitializeFailed`.
    let startup_failed: HarnessCapability = serde_json::from_value(json!({
        "name": "pi", "version": "1.5.2", "sessions": false, "jobs": false,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": {
            "title": "Pi",
            "builtin": true,
            "status": "initialize_failed",
            "error": "adapter exited before initialize"
        },
        "setup": {
            "state": "unavailable",
            "components": [
                { "id": "pi", "name": "Pi SDK", "package": "@earendil-works/pi-coding-agent", "version": "1.5.2", "status": "installed" },
                { "id": "adapter", "name": "Pi", "package": "@openma/pi-acp", "version": "0.14.0", "status": "installed" }
            ],
            "can_install": true
        }
    }))
    .unwrap();
    let v = harness(startup_failed);
    assert!(v.installed);
    assert_eq!(v.available, Some(false));
    assert_eq!(
        v.degraded_reason,
        Some(HarnessDegradedReason::InitializeFailed)
    );
    assert!(v.missing_command.is_none());

    // Truly uninstalled built-in path-only adapter (no setup, empty version, adapter missing).
    let absent_builtin: HarnessCapability = serde_json::from_value(json!({
        "name": "cursor", "version": "", "sessions": false, "jobs": false,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": { "title": "Cursor", "builtin": true, "status": "adapter_missing" },
        "setup": null
    }))
    .unwrap();
    let v = harness(absent_builtin);
    assert_eq!(v.available, Some(false));
    assert!(!v.installed);
    assert_eq!(v.degraded_reason, None);
    assert!(
        v.install_hint
            .as_deref()
            .is_some_and(|s| s.contains("cursor.com"))
    );

    // Managed harness with no installed/detected components -> not installed.
    let absent_managed: HarnessCapability = serde_json::from_value(json!({
        "name": "codebuddy-code", "version": "", "sessions": false, "jobs": false,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": { "title": "CodeBuddy Code", "builtin": true, "status": "adapter_missing" },
        "setup": {
            "state": "not_installed",
            "components": [{ "id": "adapter", "name": "CodeBuddy", "package": "@tencent-ai/codebuddy-code", "status": "missing" }],
            "can_install": true
        }
    }))
    .unwrap();
    let v = harness(absent_managed);
    assert!(!v.installed);
    assert_eq!(v.degraded_reason, None);

    // Managed harness with detected binary but missing managed adapter -> installed & adapter_missing.
    let partial_managed: HarnessCapability = serde_json::from_value(json!({
        "name": "codebuddy-code", "version": "", "sessions": false, "jobs": false,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": { "title": "CodeBuddy Code", "builtin": true, "status": "adapter_missing" },
        "setup": {
            "state": "not_installed",
            "components": [{ "id": "adapter", "name": "CodeBuddy", "package": "@tencent-ai/codebuddy-code", "status": "missing", "detected": { "path": "/usr/local/bin/codebuddy", "version": "1.0.0" } }],
            "can_install": true
        }
    }))
    .unwrap();
    let v = harness(partial_managed);
    assert!(v.installed);
    assert_eq!(
        v.degraded_reason,
        Some(HarnessDegradedReason::AdapterMissing)
    );

    // Installed component below minimum required version (uses spec minimum without parsing English).
    let outdated: HarnessCapability = serde_json::from_value(json!({
        "name": "pi", "version": "1.0.0", "sessions": true, "jobs": true,
        "models": [], "resume": true, "cancel": true, "permissions": true,
        "images": false, "enforcement": [],
        "adapter": { "title": "Pi", "builtin": true, "status": "ready", "session": { "models": [] } },
        "setup": {
            "state": "unavailable",
            "components": [{ "id": "pi", "name": "Pi SDK", "package": "@earendil-works/pi-coding-agent", "version": "1.0.0", "status": "installed" }],
            "can_install": true
        }
    }))
    .unwrap();
    let v = harness(outdated);
    assert!(v.installed);
    assert_eq!(v.available, Some(false));
    assert_eq!(
        v.degraded_reason,
        Some(HarnessDegradedReason::VersionOutdated)
    );

    // Failed managed install -> InstallFailed.
    let mut failed_install = cap("1.2.0", "m");
    let setup = failed_install.setup.as_mut().unwrap();
    setup.state = SetupState::Unavailable;
    setup.error = Some("npm exit 1".into());
    assert_eq!(
        harness(failed_install).degraded_reason,
        Some(HarnessDegradedReason::InstallFailed)
    );

    // Host runtime incompatibility (unavailable setup with ready adapter and no error/outdated).
    let mut bad_runtime = cap("1.2.0", "m");
    bad_runtime.setup.as_mut().unwrap().state = SetupState::Unavailable;
    assert_eq!(
        harness(bad_runtime).degraded_reason,
        Some(HarnessDegradedReason::RuntimeIncompatible)
    );

    // Installing -> Installing.
    let mut installing = cap("1.2.0", "m");
    installing.setup.as_mut().unwrap().state = SetupState::Installing;
    assert_eq!(
        harness(installing).degraded_reason,
        Some(HarnessDegradedReason::Installing)
    );
}

#[tokio::test]
async fn empty_success_failure_unsupported_and_offline_remain_distinct() {
    let inv = Inventory::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let fake = FakeNode::new("local");
    fake.set_harnesses(vec![cap("1", "local/model")]);
    let node = setup_node::Delayed::new(fake.clone());
    let mut t = local(node.clone());
    assert_eq!(inv.view(&t).await.state, NodeHarnessReportState::Unreported);
    inv.refresh(&t, false).await;
    let success = inv.view(&t).await;
    assert_eq!(success.state, NodeHarnessReportState::Reported);
    node.disconnect(Code::Internal);
    inv.refresh(&t, true).await;
    let failure = inv.view(&t).await;
    assert_eq!(failure.state, NodeHarnessReportState::Failed);
    assert!(failure.stale);
    assert_eq!(failure.checked_at, success.checked_at);
    assert_eq!(failure.harnesses, success.harnesses);
    node.disconnect(Code::Unsupported);
    inv.refresh(&t, true).await;
    assert_eq!(
        inv.view(&t).await.state,
        NodeHarnessReportState::Unsupported
    );
    node.reconnect();
    fake.set_harnesses(vec![]);
    inv.refresh(&t, true).await;
    let empty = inv.view(&t).await;
    assert_eq!(empty.state, NodeHarnessReportState::Reported);
    assert!(empty.harnesses.is_empty());
    assert!(!empty.stale);
    t.node = None;
    assert_eq!(inv.view(&t).await.state, NodeHarnessReportState::Offline);
    assert!(inv.view(&t).await.stale);
    assert!(
        !inv.view(&Target {
            id: "never-seen".into(),
            node: None,
            remote: None,
            generation: None,
            upgrade_only: false,
        })
        .await
        .stale
    );
}

#[tokio::test]
async fn concurrent_refreshes_join_and_an_explicit_later_refresh_reads_again() {
    let inv = Arc::new(Inventory::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    ))));
    let fake = FakeNode::new("local");
    fake.set_harnesses(vec![cap("old", "old/model")]);
    let node = setup_node::Delayed::new(fake.clone());
    node.arm();
    let t = Arc::new(local(node.clone()));
    let a = {
        let (inv, t) = (inv.clone(), t.clone());
        tokio::spawn(async move { inv.refresh(&t, true).await })
    };
    node.entered.notified().await;
    let b = {
        let (inv, t) = (inv.clone(), t.clone());
        tokio::spawn(async move { inv.refresh(&t, true).await })
    };
    tokio::task::yield_now().await;
    fake.set_harnesses(vec![cap("new", "new/model")]);
    node.gate.add_permits(1);
    a.await.unwrap();
    b.await.unwrap();
    assert_eq!(
        inv.view(&t).await.harnesses[0].version,
        "old",
        "only the held snapshot was read"
    );
    inv.refresh(&t, false).await;
    assert_eq!(
        inv.view(&t).await.harnesses[0].version,
        "old",
        "automatic reads honor TTL"
    );
    inv.refresh(&t, true).await;
    assert_eq!(inv.view(&t).await.harnesses[0].version, "new");
}

#[tokio::test(start_paused = true)]
async fn timeout_and_cancellation_release_the_refresh_slot() {
    let inv = Arc::new(Inventory::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    ))));
    let fake = FakeNode::new("local");
    fake.set_harnesses(vec![cap("1", "model")]);
    let node = setup_node::Delayed::new(fake);
    let t = Arc::new(local(node.clone()));
    inv.refresh(&t, true).await;
    node.arm();
    let a = {
        let (inv, t) = (inv.clone(), t.clone());
        tokio::spawn(async move { inv.refresh(&t, true).await })
    };
    node.entered.notified().await;
    tokio::time::advance(PROBE_TIMEOUT + Duration::from_secs(1)).await;
    a.await.unwrap();
    let r = inv.view(&t).await;
    assert_eq!(r.state, NodeHarnessReportState::Failed);
    assert!(r.stale);
    assert!(!r.refreshing);
    assert!(r.error.unwrap().contains("timed out"));
    node.arm();
    let a = {
        let (inv, t) = (inv.clone(), t.clone());
        tokio::spawn(async move { inv.refresh(&t, true).await })
    };
    node.entered.notified().await;
    a.abort();
    let _ = a.await;
    assert!(!inv.view(&t).await.refreshing);
    inv.refresh(&t, true).await;
    assert_eq!(inv.view(&t).await.state, NodeHarnessReportState::Reported);
}

#[tokio::test]
async fn a_manual_fresh_read_does_not_reuse_an_automatic_cached_probe() {
    let inv = Arc::new(Inventory::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    ))));
    let fake = FakeNode::new("local");
    fake.set_harnesses(vec![cap("cached", "cached/model")]);
    let node = setup_node::Delayed::new(fake.clone());
    node.arm();
    let t = Arc::new(local(node.clone()));
    let a = {
        let (inv, t) = (inv.clone(), t.clone());
        tokio::spawn(async move { inv.refresh(&t, false).await })
    };
    node.entered.notified().await;
    let b = {
        let (inv, t) = (inv.clone(), t.clone());
        tokio::spawn(async move { inv.refresh(&t, true).await })
    };
    tokio::task::yield_now().await;
    fake.set_harnesses(vec![cap("fresh", "fresh/model")]);
    node.gate.add_permits(1);
    a.await.unwrap();
    b.await.unwrap();
    assert_eq!(inv.view(&t).await.harnesses[0].version, "fresh");
}

#[tokio::test]
async fn an_answer_with_another_identity_cannot_contaminate_the_cache() {
    let inv = Inventory::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let mut t = local(FakeNode::new("other-node"));
    t.id = "local".into();
    inv.refresh(&t, true).await;
    let r = inv.view(&t).await;
    assert_eq!(r.state, NodeHarnessReportState::Failed);
    assert!(r.harnesses.is_empty());
}

async fn caller(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let me = req
        .headers()
        .get("x-test-user")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("u1")
        .to_string();
    req.extensions_mut().insert(Caller {
        me,
        ws: "default".into(),
    });
    next.run(req).await
}

/// Real HTTP + Node WebSocket transports, isolated storage and deterministic probe substitutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_reports_are_isolated_authorized_and_fenced_across_reconnects() {
    use crate::wire::{node_runtime::NodeRuntime, store::StoreBackend};
    use axum::{Router, middleware, routing::get as route_get};
    let h = node_harness::Hub::start().await;
    let cfg = Arc::new(
        crate::config::Config::from_vars(|k| match k {
            "HOME" | "STEVE_HOME" => Some(h.dir.path().display().to_string()),
            _ => None,
        })
        .unwrap(),
    );
    let fake = FakeNode::new("local");
    fake.set_harnesses(vec![cap("local-cli", "local/model")]);
    let runtime = NodeRuntime::new(h.store.clone(), fake, h.ends.clone());
    let bus = crate::bus::Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let st = AppState::new(
        cfg.clone(),
        StoreBackend(h.store.clone()),
        crate::wire::core::CoreDomain::for_test(&cfg, h.store.clone(), &bus, None),
        bus,
    )
    .with_node(Some(runtime))
    .with_nodes(Some(h.svc.clone()));
    let app = Router::new()
        .route("/node-harnesses", route_get(super::list))
        .route("/node-harnesses/{id}", route_get(super::get))
        .with_state(st.clone())
        .layer(middleware::from_fn(caller));
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(socket, app).await.unwrap();
    });
    let http = reqwest::Client::new();
    let read = |id: String| {
        let (http, base) = (http.clone(), base.clone());
        async move {
            let r = http
                .get(format!("{base}/node-harnesses/{id}?fresh=1"))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            r.json::<Value>().await.unwrap()["report"].clone()
        }
    };
    let (identity, id) = h.paired("u1").await;
    let remote = FakeNode::new(id.as_str());
    remote.set_harnesses(vec![cap("remote-cli", "remote/model")]);
    let delayed = setup_node::Delayed::new(remote.clone());
    let link = node_harness::dial(&h, &identity, &id, delayed.clone(), 1)
        .await
        .unwrap();
    steve_runtime::testkit::eventually_sync("linked", || {
        h.svc.registry.current(id.as_str()).is_some()
    })
    .await;
    let l = read("local".into()).await;
    let r = read(id.to_string()).await;
    assert_eq!(l["harnesses"][0]["version"], "local-cli");
    assert_eq!(l["harnesses"][0]["models"], json!(["local/model"]));
    assert_eq!(r["harnesses"][0]["version"], "remote-cli");
    assert_eq!(r["harnesses"][0]["models"], json!(["remote/model"]));
    assert_eq!(
        http.get(format!("{base}/node-harnesses/{id}?fresh=1"))
            .header("x-test-user", "other")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let listed: Value = http
        .get(format!("{base}/node-harnesses"))
        .header("x-test-user", "other")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed["nodes"].get(id.as_str()).is_none());
    delayed.arm();
    let pending = tokio::spawn(read(id.to_string()));
    delayed.entered.notified().await;
    link.conn
        .close(None, steve_node_wire::close::GOODBYE, "test disconnect");
    let _ = pending.await.unwrap();
    // Registry removal precedes the awaited last-seen write and RemoteNode::detach.
    // Observe the report itself: until detach, a closed-link probe can still report failed.
    steve_runtime::testkit::eventually("offline harness report", || async {
        read(id.to_string()).await["state"] == "offline"
    })
    .await;
    let away = read(id.to_string()).await;
    assert_eq!(away["stale"], true);
    assert_eq!(away["harnesses"], r["harnesses"]);
    remote.set_harnesses(vec![cap("new-cli", "new/model")]);
    let _new = node_harness::dial(&h, &identity, &id, remote.clone(), 2)
        .await
        .unwrap();
    delayed.gate.add_permits(1);
    steve_runtime::testkit::eventually_sync("relinked", || {
        h.svc.registry.current(id.as_str()).is_some()
    })
    .await;
    let new = read(id.to_string()).await;
    assert_eq!(new["state"], "reported");
    assert_eq!(new["stale"], false);
    assert_eq!(new["harnesses"][0]["models"], json!(["new/model"]));
    // An automatic read reuses only this connection's fresh result.
    remote.set_harnesses(vec![]);
    let cached: Value = http
        .get(format!("{base}/node-harnesses/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cached["report"]["harnesses"][0]["version"], "new-cli");
    assert!(
        read(id.to_string()).await["harnesses"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn off_returns_the_probe_response_without_retaining_a_report() {
    let caches = Arc::new(steve_runtime::cache::Caches::new(
        steve_runtime::cache::CacheConfig {
            mode: steve_runtime::cache::CacheMode::Off,
            memory_mb: 1,
        },
    ));
    let inv = Inventory::new(caches);
    let node = FakeNode::new("local");
    node.set_harnesses(vec![cap("direct", "direct/model")]);
    let target = local(node);
    let report = inv.refresh(&target, false).await;
    assert_eq!(report.state, NodeHarnessReportState::Reported);
    assert_eq!(report.harnesses[0].models, ["direct/model"]);
    assert!(inv.cache.peek(&target.id).await.is_none());
    assert_eq!(
        inv.view(&target).await.state,
        NodeHarnessReportState::Unreported
    );
}

#[tokio::test]
async fn upgrade_only_target_returns_upgrade_required_without_calling_probe() {
    let inv = Inventory::new(Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let fake = FakeNode::new("n_old");
    fake.set_harnesses(vec![cap("1", "old/model")]);
    let target = Target {
        id: "n_old".into(),
        node: Some(fake.clone()),
        remote: None,
        generation: Some(1),
        upgrade_only: true,
    };
    let report = inv.refresh(&target, true).await;
    assert_eq!(report.state, NodeHarnessReportState::UpgradeRequired);
    assert!(report.error.is_none());
    assert!(!report.refreshing);
    assert!(
        fake.log().is_empty(),
        "upgrade-only target must never invoke ordinary probe()"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_install_routes_to_remote_node_and_enforces_owner_and_builtin_contract() {
    use crate::wire::{node_runtime::NodeRuntime, store::StoreBackend};
    use axum::{
        Router, middleware,
        routing::{get as route_get, post as route_post},
    };
    let h = node_harness::Hub::start().await;
    h.store
        .write(|w| {
            let mut u = steve_store::UserRows(w);
            u.insert("u1", "default", "u1", "Owner", "owner", 1)?;
            u.insert("other", "default", "other", "Other", "member", 2)?;
            Ok::<(), steve_store::StoreError>(())
        })
        .unwrap();
    let cfg = Arc::new(
        crate::config::Config::from_vars(|k| match k {
            "HOME" | "STEVE_HOME" => Some(h.dir.path().display().to_string()),
            _ => None,
        })
        .unwrap(),
    );
    let fake_local = FakeNode::new("local");
    let runtime = NodeRuntime::new(h.store.clone(), fake_local, h.ends.clone());
    let bus = crate::bus::Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let st = AppState::new(
        cfg.clone(),
        StoreBackend(h.store.clone()),
        crate::wire::core::CoreDomain::for_test(&cfg, h.store.clone(), &bus, None),
        bus,
    )
    .with_node(Some(runtime))
    .with_nodes(Some(h.svc.clone()));
    let app = Router::new()
        .route("/node-harnesses", route_get(super::list))
        .route("/node-harnesses/{id}", route_get(super::get))
        .route(
            "/node-harnesses/{id}/{harness}/install",
            route_post(super::install),
        )
        .with_state(st)
        .layer(middleware::from_fn(caller));
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(socket, app).await.unwrap();
    });
    let http = reqwest::Client::new();

    let (identity, id) = h.paired("u1").await;
    let remote = FakeNode::new(id.as_str());
    remote.set_harnesses(vec![cap("remote-cli", "remote/model")]);
    let delayed = setup_node::Delayed::new(remote.clone());
    let _link = node_harness::dial(&h, &identity, &id, delayed.clone(), 1)
        .await
        .unwrap();
    steve_runtime::testkit::eventually_sync("linked", || {
        h.svc.registry.current(id.as_str()).is_some()
    })
    .await;

    // Non-owner is refused.
    let res = http
        .post(format!("{base}/node-harnesses/{id}/pi/install"))
        .header("x-test-user", "other")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);

    // Arbitrary body parameters are refused.
    let res = http
        .post(format!("{base}/node-harnesses/{id}/pi/install"))
        .json(&json!({"package": "@evil/pkg"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    // Non-managed harness is refused.
    let res = http
        .post(format!("{base}/node-harnesses/{id}/antigravity/install"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    // Owner installing a built-in managed harness on a remote Node succeeds and flips setup to installing.
    let res = http
        .post(format!("{base}/node-harnesses/{id}/pi/install"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["accepted"], true);
    assert!(
        body["operation"]
            .as_str()
            .is_some_and(|s| s.starts_with("install_"))
    );

    server.abort();
    let _ = server.await;
}

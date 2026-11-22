use super::*;
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
            "session":{"models":[{"id":model}]}},
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
    }
}

#[test]
fn readiness_requires_evidence_and_versions_models_belong_to_the_target() {
    let c = cap("cli-1", "actual/model");
    let h = harness(c.clone());
    assert_eq!(h["available"], true);
    assert_eq!(h["version"], "cli-1");
    assert_eq!(h["adapter_version"], "adapter-2");
    assert_eq!(h["models"], json!(["actual/model"]));
    let mut unprobed = c.clone();
    unprobed.adapter = None;
    unprobed.setup = None;
    assert!(
        harness(unprobed)["available"].is_null(),
        "a declaration without an adapter report is unknown"
    );
    let mut missing = c.clone();
    missing.setup.as_mut().unwrap().state = SetupState::NotInstalled;
    assert_eq!(harness(missing)["available"], false);
    let mut denied = c;
    let a = denied.adapter.as_mut().unwrap();
    a.session = None;
    a.session_error = Some("sign in on this Node".into());
    assert_eq!(
        harness(denied.clone())["degraded_reason"],
        "session_unavailable"
    );
    denied.adapter.as_mut().unwrap().session_auth_required = Some(true);
    let h = harness(denied);
    assert_eq!(h["available"], false);
    assert_eq!(h["degraded_reason"], "authentication_required");
    assert!(h["error"].is_null());
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
    assert_eq!(inv.view(&t).await.state, "unreported");
    inv.refresh(&t, false).await;
    let success = inv.view(&t).await;
    assert_eq!(success.state, "reported");
    node.disconnect(ErrorCode::Internal);
    inv.refresh(&t, true).await;
    let failure = inv.view(&t).await;
    assert_eq!(failure.state, "failed");
    assert!(failure.stale);
    assert_eq!(failure.checked_at, success.checked_at);
    assert_eq!(failure.harnesses, success.harnesses);
    node.disconnect(ErrorCode::Unsupported);
    inv.refresh(&t, true).await;
    assert_eq!(inv.view(&t).await.state, "unsupported");
    node.reconnect();
    fake.set_harnesses(vec![]);
    inv.refresh(&t, true).await;
    let empty = inv.view(&t).await;
    assert_eq!(empty.state, "reported");
    assert!(empty.harnesses.is_empty());
    assert!(!empty.stale);
    t.node = None;
    assert_eq!(inv.view(&t).await.state, "offline");
    assert!(inv.view(&t).await.stale);
    assert!(
        !inv.view(&Target {
            id: "never-seen".into(),
            node: None,
            remote: None,
            generation: None
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
        inv.view(&t).await.harnesses[0]["version"],
        "old",
        "only the held snapshot was read"
    );
    inv.refresh(&t, false).await;
    assert_eq!(
        inv.view(&t).await.harnesses[0]["version"],
        "old",
        "automatic reads honor TTL"
    );
    inv.refresh(&t, true).await;
    assert_eq!(inv.view(&t).await.harnesses[0]["version"], "new");
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
    assert_eq!(r.state, "failed");
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
    assert_eq!(inv.view(&t).await.state, "reported");
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
    assert_eq!(inv.view(&t).await.harnesses[0]["version"], "fresh");
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
    assert_eq!(r.state, "failed");
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
    let st = AppState::new(
        cfg,
        Arc::new(StoreBackend::new(h.store.clone()).unwrap()),
        Arc::new(crate::backend::NoDomain),
        crate::bus::Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        ))),
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
    assert_eq!(report.state, "reported");
    assert_eq!(report.harnesses[0]["models"], json!(["direct/model"]));
    assert!(inv.cache.peek(&target.id).await.is_none());
    assert_eq!(inv.view(&target).await.state, "unreported");
}

//! Real isolated Hub HTTP routes, real local Nodes and Wire transport. npm is a fixture.
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use steve_hub::{api::AppState, bus::Bus, config::Config};
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Storage};
use steve_node_api::*;
use steve_runtime::testkit::{eventually, on_timeout};
#[path = "../../steve-node/tests/fixtures/harness_updates.rs"]
mod fixture;
use fixture::*;

struct Domain;

struct LoseUpgradeAnswer {
    node: Arc<dyn Node>,
    accepted: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl steve_node_wire::Handler for LoseUpgradeAnswer {
    async fn handle(&self, method: &str, params: Value) -> ApiResult<Value> {
        if method == steve_node_wire::methods::HARNESS_UPGRADE {
            let command = serde_json::from_value(params).unwrap();
            self.node.upgrade_harness(command).await?;
            self.accepted.notify_one();
            // The installer is accepted on the Node, but the transport never sends its answer.
            std::future::pending::<()>().await;
            unreachable!()
        }
        steve_node_wire::NodeServer::new(self.node.clone())
            .handle(method, params)
            .await
    }
}
impl steve_hub::wire::node_runtime::NodeDomain for Domain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}
struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    store: steve_store::Store,
    svc: Arc<steve_hub::nodes::NodeService>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Env {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Env {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .http
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
                format!("{}{path}", self.base),
            )
            .header("cookie", &self.cookie);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    fn register(&self, node: &str) {
        let owner = self.store.access().owner().unwrap().unwrap().id;
        self.store
            .transaction(|tx| {
                let code = format!("code-{node}");
                steve_store::nodes::create_pairing_as(tx, &code, &owner, 1, i64::MAX, true)?;
                steve_store::nodes::claim_pairing(
                    tx,
                    &code,
                    &steve_store::nodes::NewNode {
                        node_id: node,
                        name: node,
                        pubkey: &[7; 32],
                        fingerprint: node,
                    },
                    2,
                )?;
                Ok::<(), steve_store::StoreError>(())
            })
            .unwrap();
    }
    fn link(
        &self,
        node: Arc<dyn Node>,
        generation: u64,
    ) -> (Arc<steve_node_wire::Conn>, Arc<steve_node_wire::Conn>) {
        self.link_with(
            node.clone(),
            generation,
            steve_node_wire::NodeServer::new(node),
        )
    }
    fn link_with(
        &self,
        node: Arc<dyn Node>,
        generation: u64,
        handler: Arc<dyn steve_node_wire::Handler>,
    ) -> (Arc<steve_node_wire::Conn>, Arc<steve_node_wire::Conn>) {
        use steve_node_wire::*;
        let (hub, remote) = testkit::pair();
        let h = Conn::spawn(
            hub.sink,
            hub.stream,
            generation,
            NodeServer::new(steve_node_api::testkit::FakeNode::new("unused")),
            ConnOptions::default(),
        );
        let n = Conn::spawn(
            remote.sink,
            remote.stream,
            generation,
            handler,
            ConnOptions::default(),
        );
        let target = self.svc.fleet.remote(node.id());
        target.attach(h.clone(), false);
        target.set_ready(generation);
        (h, n)
    }
    async fn report(&self, node: &str) -> Value {
        let (status, r) = self
            .call("GET", &format!("/api/node-harness-updates/{node}"), None)
            .await;
        assert_eq!(status, 200, "{r}");
        r
    }
    async fn wait_receipt(&self, node: &str, operation: &str) -> Value {
        let latest = std::sync::Mutex::new(None::<Value>);
        eventually(
            on_timeout(|| async {
                format!(
                    "Upgrade {operation} for Node {node} has a finished pi receipt; last report: {:?}",
                    latest.lock().unwrap()
                )
            }),
            || async {
                let r = self.report(node).await;
                let finished = r["runs"]["pi"]["operation"] == operation
                    && r["runs"]["pi"]["state"]["state"] == "finished";
                *latest.lock().unwrap() = Some(r);
                finished
            },
        )
        .await;
        latest
            .into_inner()
            .unwrap()
            .expect("finished report observed")
    }
}
async fn start(node: Arc<dyn Node>) -> Env {
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
        ("STEVE_NODE_MODE", "off".into()),
        ("STEVE_NODE_LISTEN", "0".into()),
        (
            "STEVE_ADAPTER_PI",
            h.join("steve/adapters/node_modules/@razorbackdb/pi-acp/dist/index.js")
                .display()
                .to_string(),
        ),
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
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let store = w.modules.as_ref().unwrap().store.clone();
    let fleet = steve_hub::nodes::NodeFleet::new(store.clone(), cfg.data_dir.clone(), None, None);
    let svc = steve_hub::nodes::NodeService::new(store.clone(), fleet, None).unwrap();
    let runtime =
        steve_hub::wire::node_runtime::NodeRuntime::new(store.clone(), node, Arc::new(Domain));
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_node(Some(runtime))
        .with_nodes(Some(svc.clone()));
    st.runtimes.set_harnesses(w.harnesses.clone());
    steve_hub::app::boot(&st).await.expect("boot");
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    let server = tokio::spawn(async move {
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
        store,
        svc,
        server,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_acceptance_answer_is_unknown_and_reconnect_reads_the_same_operation() {
    let local = Fixture::new("local", "1.0.0", "1.0.0", "2.0.0");
    let remote = Fixture::new("remote", "1.5.0", "1.5.0", "2.0.0");
    let (n1, n2) = (Arc::new(local.node()), Arc::new(remote.node()));
    checked(n2.as_ref()).await;
    let e = start(n1.clone()).await;
    e.register("remote");
    let accepted = Arc::new(tokio::sync::Notify::new());
    let (link, _peer) = e.link_with(
        n2.clone(),
        1,
        Arc::new(LoseUpgradeAnswer {
            node: n2.clone(),
            accepted: accepted.clone(),
        }),
    );
    let (http, url, cookie) = (e.http.clone(), e.base.clone(), e.cookie.clone());
    let body = json!({"operation":"lost_answer","versions":{"pi":"2.0.0","adapter":"2.0.0"}});
    let task = tokio::spawn(async move {
        http.post(format!("{url}/api/node-harness-updates/remote/pi"))
            .header("cookie", cookie)
            .json(&body)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), accepted.notified())
        .await
        .unwrap();
    link.close(None, 1000, "accepted answer lost");
    e.svc.fleet.remote(n2.id()).detach(1);
    let response = task.await.unwrap();
    assert_eq!(
        response["receipt"]["state"]["state"], "unknown",
        "{response}"
    );
    let r = e.report("remote").await;
    assert_eq!(r["runs"]["pi"]["state"]["state"], "unknown");
    assert!(r["retryable_unknown"].as_array().unwrap().is_empty());
    finished(n2.as_ref(), OperationId::new("lost_answer").unwrap()).await;
    let (_link, _peer) = e.link(n2.clone(), 2);
    let r = e.wait_receipt("remote", "lost_answer").await;
    assert_eq!(r["runs"]["pi"]["state"]["outcome"], "succeeded");
    assert_eq!(remote.installs(), 1);
    assert_eq!(local.installs(), 0);
    n1.shutdown().await;
    n2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_and_remote_upgrade_receipts_survive_disconnect_and_are_read_back_over_http() {
    let local = Fixture::new("local", "1.0.0", "1.0.0", "2.0.0");
    let remote = Fixture::new("remote", "1.5.0", "1.5.0", "2.0.0");
    let (n1, n2) = (Arc::new(local.node()), Arc::new(remote.node()));
    checked(n1.as_ref()).await;
    checked(n2.as_ref()).await;
    let e = start(n1.clone()).await;
    e.register("remote");
    let (link, _peer) = e.link(n2.clone(), 1);
    let a = e.report("local").await;
    let b = e.report("remote").await;
    assert_eq!(a["report"]["node"], "local");
    assert_eq!(b["report"]["node"], "remote");
    assert_eq!(
        a["report"]["harnesses"][0]["components"][0]["installed"],
        "1.0.0"
    );
    assert_eq!(
        b["report"]["harnesses"][0]["components"][0]["installed"],
        "1.5.0"
    );
    let body = |operation: &str| json!({"operation":operation,"versions":{"pi":"2.0.0","adapter":"2.0.0"}});
    let (s, r) = e
        .call(
            "POST",
            "/api/node-harness-updates/local/pi",
            Some(body("local_upgrade")),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_ne!(r["receipt"]["state"]["state"], "finished");
    let r = e.wait_receipt("local", "local_upgrade").await;
    assert_eq!(r["runs"]["pi"]["state"]["outcome"], "succeeded");
    assert_eq!(local.installs(), 1);
    assert_eq!(remote.installs(), 0);

    remote.mark("hold");
    let (s, r) = e
        .call(
            "POST",
            "/api/node-harness-updates/remote/pi",
            Some(body("remote_upgrade")),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["receipt"]["state"]["state"], "running");
    let (s, r) = e
        .call(
            "POST",
            "/api/node-harness-updates/remote/pi",
            Some(body("double_click")),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["receipt"]["operation"], "remote_upgrade");
    link.close(None, 1000, "fixture disconnect");
    e.svc.fleet.remote(n2.id()).detach(1);
    let offline = e.report("remote").await;
    assert_eq!(offline["code"], "offline");
    assert!(!offline["error"].is_null());
    assert_eq!(
        offline["report"]["node"], "remote",
        "keeps only this Node's cached report"
    );
    remote.unmark("hold");
    finished(n2.as_ref(), OperationId::new("remote_upgrade").unwrap()).await;
    let (_link2, _peer2) = e.link(n2.clone(), 2);
    let r = e.wait_receipt("remote", "remote_upgrade").await;
    assert_eq!(r["runs"]["pi"]["state"]["outcome"], "succeeded");
    assert!(
        r["report"]["harnesses"][0]["components"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["installed"] == "2.0.0")
    );
    let (s, r) = e
        .call(
            "POST",
            "/api/node-harness-updates/remote/pi",
            Some(body("remote_upgrade")),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["receipt"]["state"]["outcome"], "succeeded");
    assert_eq!(local.installs(), 1);
    assert_eq!(remote.installs(), 1);
    let saved: String =
        steve_store::testkit::hub::hub_fixtures::setting_value_harness_updates_remote(&e.store)
            .unwrap();
    assert!(saved.contains("remote_upgrade") && saved.contains("succeeded"));
    n1.shutdown().await;
    n2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authorization_policy_old_nodes_and_failed_installers_are_explicit() {
    let local = Fixture::new("local", "1.0.0", "1.0.0", "2.0.0");
    let n = Arc::new(local.node());
    checked(n.as_ref()).await;
    let e = start(n.clone()).await;
    let req = |op: &str| json!({"operation":op,"versions":{"pi":"2.0.0","adapter":"2.0.0"}});
    let unauth = e
        .http
        .get(format!("{}/api/node-harness-updates/local", e.base))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);
    let (s, _) = e
        .call(
            "POST",
            "/api/node-harness-updates/not-owned/pi",
            Some(req("foreign")),
        )
        .await;
    assert_eq!(s, 403);
    e.register("old");
    let (_a, _b) = e.link(steve_node_api::testkit::FakeNode::new("old"), 1);
    let r = e.report("old").await;
    assert_eq!(r["code"], "unsupported");
    assert!(r["report"].is_null());
    let (s, r) = e
        .call(
            "POST",
            "/api/node-harness-updates/old/pi",
            Some(req("old_upgrade")),
        )
        .await;
    assert_eq!(s, 409, "{r}");
    assert!(r["error"].as_str().unwrap().contains("harness-updates-v1"));

    let mut denied = Fixture::new("denied", "1.0.0", "1.0.0", "2.0.0");
    denied.cfg.ceiling.exec = false;
    let dn = Arc::new(denied.node());
    checked(dn.as_ref()).await;
    e.register("denied");
    let (_a, _b) = e.link(dn.clone(), 1);
    let (s, r) = e
        .call(
            "POST",
            "/api/node-harness-updates/denied/pi",
            Some(req("denied_upgrade")),
        )
        .await;
    assert_eq!(s, 403, "{r}");
    assert_eq!(denied.installs(), 0);
    local.mark("fail");
    let (s, r) = e
        .call(
            "POST",
            "/api/node-harness-updates/local/pi",
            Some(req("failed_upgrade")),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    let r = e.wait_receipt("local", "failed_upgrade").await;
    assert_eq!(r["runs"]["pi"]["state"]["outcome"], "failed");
    assert!(!r.to_string().contains("fixture-secret"));
    let mut conflict = req("failed_upgrade");
    conflict["versions"]["pi"] = json!("9.0.0");
    assert_eq!(
        e.call("POST", "/api/node-harness-updates/local/pi", Some(conflict))
            .await
            .0,
        409
    );
    assert_eq!(local.installs(), 1);
    // Real authorization is re-read from storage, not a capability reported by the Node.
    let owner = e.store.access().owner().unwrap().unwrap().id;
    steve_store::testkit::hub::set_user_role(&e.store, &owner, "member").unwrap();
    assert_eq!(
        e.call(
            "POST",
            "/api/node-harness-updates/local/pi",
            Some(req("member_upgrade"))
        )
        .await
        .0,
        403
    );
    assert_eq!(local.installs(), 1);
    n.shutdown().await;
    dn.shutdown().await;
}

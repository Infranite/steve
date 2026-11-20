//! Harness-maintenance receipts through the public router and a finite Wire peer.
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};
use steve_hub::{
    api::{self, AppState},
    bus::Bus,
    config::Config,
    wire::node_runtime::{NodeDomain, NodeRuntime},
};
use steve_node_api::*;
use steve_node_wire::{Conn, ConnOptions, Handler, RemoteNode, methods};
use steve_store::Store;
use tower::ServiceExt;

#[derive(Default)]
struct Facts {
    inventory: u8,
    unknown: bool,
    receipts: BTreeMap<String, Receipt>,
    sent: Vec<Command<UpgradeHarness>>,
    fresh: Vec<bool>,
}
struct Peer(Arc<Mutex<Facts>>);
#[async_trait::async_trait]
impl Handler for Peer {
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value> {
        let mut s = self.0.lock().unwrap();
        match method {
            methods::HARNESS_UPDATES => {
                s.fresh.push(p["fresh"].as_bool().unwrap_or(false));
                if s.inventory == 1 {
                    return Err(ApiError::new(ErrorCode::Offline, "fixture offline"));
                }
                let report = HarnessUpdateReport {
                    node: NodeId::new(if s.inventory == 2 { "other" } else { "local" }).unwrap(),
                    checking: false,
                    harnesses: vec![HarnessUpdateInfo {
                        harness: "pi".into(),
                        name: "Fixture".into(),
                        components: vec![],
                        can_upgrade: true,
                        reason: None,
                        operation: None,
                    }],
                };
                Ok(serde_json::to_value(report).unwrap())
            }
            methods::OP_INSPECT => {
                let id: OperationId = serde_json::from_value(p).unwrap();
                s.receipts
                    .get(id.as_str())
                    .map(|r| serde_json::to_value(r).unwrap())
                    .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation"))
            }
            methods::HARNESS_UPGRADE => {
                let command: Command<UpgradeHarness> = serde_json::from_value(p).unwrap();
                let receipt = Receipt {
                    operation: command.id.clone(),
                    state: if s.unknown {
                        Observed::Unknown {
                            reason: "accepted but outcome unknown".into(),
                        }
                    } else {
                        Observed::Finished {
                            outcome: Outcome::Succeeded,
                            result: None,
                            error: None,
                        }
                    },
                };
                s.sent.push(command);
                s.receipts
                    .insert(receipt.operation.to_string(), receipt.clone());
                Ok(serde_json::to_value(receipt).unwrap())
            }
            _ => Err(ApiError::new(
                ErrorCode::Unsupported,
                "outside finite maintenance fixture",
            )),
        }
    }
}
struct Domain;
impl NodeDomain for Domain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}
struct Env {
    app: Router,
    st: AppState,
    store: Store,
    user: String,
    credential: String,
    facts: Arc<Mutex<Facts>>,
    conns: (Arc<Conn>, Arc<Conn>),
    remote: Arc<RemoteNode>,
    _dir: tempfile::TempDir,
}
impl Env {
    async fn new() -> Self {
        steve_modules::net::forbid_in_this_process();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vars: HashMap<String, String> = [
            ("HOME", root.display().to_string()),
            ("STEVE_HOME", root.join("hub").display().to_string()),
            ("STEVE_PORT", "18990".into()),
            ("STEVE_NO_RESUME", "1".into()),
            ("STEVE_BACKUP", "off".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_NODE_MODE", "off".into()),
            ("STEVE_REPO", "/nonexistent".into()),
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
            ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("STEVE_WEB_DIR", root.join("no-web").display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let get = Arc::new(move |k: &str| vars.get(k).cloned());
        let cfg = Arc::new(Config::from_vars(&*get).unwrap());
        for d in [&cfg.data_dir, &cfg.scratch, &cfg.worktrees] {
            std::fs::create_dir_all(d).unwrap();
        }
        let bus = Bus::new(Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let w = steve_hub::wire::wire_with(&cfg, &bus, get).unwrap();
        let store = w.modules.as_ref().unwrap().store.clone();
        store.ensure_workspace().unwrap();
        let user = store
            .users()
            .create("sg46-owner", "Owner", None, "owner")
            .unwrap();
        let (_, client, secret) = store
            .access()
            .client_create(
                &user,
                None,
                &steve_store::access::NewClient {
                    kind: "cli",
                    name: "Contract client",
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let credential = steve_store::access::credential(&client.id, &secret);
        let facts = Arc::new(Mutex::new(Facts::default()));
        let (h, n) = steve_node_wire::testkit::pair();
        let hc = Conn::spawn(
            h.sink,
            h.stream,
            1,
            Arc::new(Peer(facts.clone())),
            ConnOptions::default(),
        );
        let nc = Conn::spawn(
            n.sink,
            n.stream,
            1,
            Arc::new(Peer(facts.clone())),
            ConnOptions::default(),
        );
        let remote = RemoteNode::new(NodeId::new("local").unwrap());
        remote.attach(hc.clone(), false);
        remote.set_ready(1);
        let rt = NodeRuntime::new(store.clone(), remote.clone(), Arc::new(Domain));
        let st = AppState::new(cfg, w.backend, w.domain, bus)
            .with_modules(w.modules)
            .with_node(Some(rt));
        let app = api::router(st.clone(), Router::new());
        Self {
            app,
            st,
            store,
            user,
            credential,
            facts,
            conns: (hc, nc),
            remote,
            _dir: dir,
        }
    }
    async fn call_as(
        &self,
        credential: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {credential}"))
            .header("x-steve-ws", "default")
            .body(
                body.map(|v| Body::from(v.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap();
        let r = self.app.clone().oneshot(req).await.unwrap();
        let s = r.status().as_u16();
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        (s, serde_json::from_slice(&b).unwrap())
    }
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call_as(&self.credential, method, path, body).await
    }
    async fn report(&self) -> Value {
        let (s, v) = self
            .call("GET", "/node-harness-updates/local?fresh=1", None)
            .await;
        assert_eq!(s, 200, "{v}");
        v
    }
    async fn start(&self, id: &str, retry: bool) -> (u16, Value) {
        self.call(
            "POST",
            "/node-harness-updates/local/pi",
            Some(json!({"operation":id,"versions":{"pi":"2.0.0"},"retry_unknown":retry})),
        )
        .await
    }
    async fn stop(self) {
        self.st.shutdown().await;
    }
}
impl Drop for Env {
    fn drop(&mut self) {
        self.conns.0.close(None, 1000, "fixture done");
        self.conns.1.close(None, 1000, "fixture done");
        self.remote.detach(1);
    }
}

#[tokio::test]
async fn finished_intents_are_idempotent_and_same_id_content_conflicts_before_send() {
    let e = Env::new().await;
    let (s, r) = e.start("op-first", false).await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["receipt"]["state"]["state"], "finished");
    assert_eq!(e.start("op-first", false).await.0, 200);
    assert_eq!(e.facts.lock().unwrap().sent.len(), 1);
    let (s, _) = e
        .call(
            "POST",
            "/node-harness-updates/local/pi",
            Some(json!({"operation":"op-first","versions":{"pi":"3.0.0"}})),
        )
        .await;
    assert_eq!(s, 409);
    assert_eq!(e.facts.lock().unwrap().sent.len(), 1);
    let r = e.report().await;
    assert_eq!(r["runs"]["pi"]["operation"], "op-first");
    assert!(r["retryable_unknown"].as_array().unwrap().is_empty());
    assert!(e.facts.lock().unwrap().fresh.contains(&true));
    for body in [
        json!({"operation":"empty","versions":{}}),
        json!({"operation":"huge","versions":{"pi":"x".repeat(129)}}),
    ] {
        assert_eq!(
            e.call("POST", "/node-harness-updates/local/pi", Some(body))
                .await
                .0,
            400
        );
    }
    assert_eq!(e.facts.lock().unwrap().sent.len(), 1);
    e.stop().await;
}

#[tokio::test]
async fn cached_inventory_unknown_receipts_and_expired_missing_receipts_keep_retry_rules() {
    let e = Env::new().await;
    let r = e.report().await;
    assert_eq!(r["report"]["node"], "local");
    assert!(r["at"].as_i64().unwrap() > 0);
    e.facts.lock().unwrap().unknown = true;
    assert_eq!(e.start("op-unknown", false).await.0, 200);
    assert_eq!(
        e.start("different-click", false).await.1["receipt"]["operation"],
        "op-unknown"
    );
    assert_eq!(e.facts.lock().unwrap().sent.len(), 1);
    for (inventory, code) in [(1, "offline"), (2, "invalid")] {
        e.facts.lock().unwrap().inventory = inventory;
        let r = e.report().await;
        assert_eq!(r["report"]["node"], "local");
        assert_eq!(r["code"], code);
        assert_eq!(r["runs"]["pi"]["state"]["state"], "unknown");
        assert!(r["retryable_unknown"].as_array().unwrap().is_empty());
    }
    e.facts.lock().unwrap().inventory = 0;
    let r = e.report().await;
    assert_eq!(r["retryable_unknown"], json!(["pi"]));
    let mut saved: Value = e
        .store
        .settings()
        .get("default", "harness.updates.local", Value::Null)
        .unwrap();
    saved["runs"]["pi"]["command"]["accept_before"] = json!(0);
    e.store
        .settings()
        .set(
            "default",
            json!({"harness.updates.local":saved}).as_object().unwrap(),
            &e.user,
        )
        .unwrap();
    e.facts.lock().unwrap().receipts.clear();
    let r = e.report().await;
    assert_eq!(r["runs"]["pi"]["state"]["state"], "finished");
    assert_eq!(r["runs"]["pi"]["state"]["error"]["code"], "expired");
    e.facts.lock().unwrap().unknown = false;
    assert_eq!(e.start("op-retry", true).await.0, 200);
    assert_eq!(e.facts.lock().unwrap().sent.len(), 2);
    e.stop().await;
}

#[tokio::test]
async fn instance_owner_authority_is_checked_on_reads_and_writes() {
    let e = Env::new().await;
    let member = e
        .store
        .users()
        .create("sg46-member", "Member", None, "member")
        .unwrap();
    let (_, c, secret) = e
        .store
        .access()
        .client_create(
            &member,
            None,
            &steve_store::access::NewClient {
                kind: "cli",
                name: "Member",
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    let credential = steve_store::access::credential(&c.id, &secret);
    for (method, path, body) in [
        ("GET", "/node-harness-updates/local", None),
        (
            "POST",
            "/node-harness-updates/local/pi",
            Some(json!({"operation":"not-owner","versions":{"pi":"2.0.0"}})),
        ),
    ] {
        let (status, body) = e.call_as(&credential, method, path, body).await;
        assert_eq!(status, 403);
        assert_eq!(
            body["error"],
            "Only the instance owner may maintain their active Nodes"
        );
    }
    assert!(e.facts.lock().unwrap().sent.is_empty());
    e.stop().await;
}

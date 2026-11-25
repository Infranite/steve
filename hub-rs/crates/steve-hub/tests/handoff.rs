//! End-to-end Hub + Core + LocalNode + fake ACP integration tests for session handoff (#502):
//! 1. Manual same-surface and cross-harness handoff (`OwnerRequest`), W3.1 placement admission,
//!    optional reversible `make_bot_default`, working-memory preservation, per-session usage
//!    accounting with zero double-counting, and read-only `steve_turn_context_peak_tokens` metric.
//! 2. `QuotaExhausted`: waits when recovery is within `quotaWaitMinutes`, otherwise switches to
//!    the Bot's next ordered fallback surface without changing Bot defaults or switching Bots.
//! 3. `ProviderError`: switches to fallback surface after consecutive threshold failures, and when
//!    all surfaces are unavailable opens a coordinator decision card (`handoff_unavailable`)
//!    without auto-switching Bots; owner choosing a peer Bot hands off with working memory.
#![cfg(unix)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use steve_runtime::testkit::{eventually, on_timeout};
use steve_store::testkit::hub::node_statistics as fixtures;
use steve_store::{NoPublisher, Store};
use tower::ServiceExt;

const HUB_FAKE_ACP_PY: &str = r#"#!/usr/bin/env python3
import json, os, sys

ctrl_dir = os.environ["FAKE_HANDOFF_CTRL"]
adapter_kind = os.environ.get("FAKE_ADAPTER_KIND", "primary")
log_path = os.path.join(ctrl_dir, "acp.log")

def log(obj):
    if os.path.basename(os.getcwd()) != "probe":
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(obj) + "\n")

def send(obj):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", **obj}) + "\n")
    sys.stdout.flush()

def update(sid, u):
    send({"method": "session/update", "params": {"sessionId": sid, "update": u}})

sid = ""
active_model = "alt-model-1" if adapter_kind == "alt-harness" else "fake-model"
session_scenario = ""
session_is_peer = False
for raw in sys.stdin:
    if not raw.strip():
        continue
    msg = json.loads(raw)
    log(msg)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        send({"id": mid, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}})
    elif method == "session/new":
        sid = f"fake-{adapter_kind}-{os.getpid()}"
        session_scenario = ""
        session_is_peer = False
        models = (
            {
                "currentModelId": "alt-model-1",
                "availableModels": [
                    {"modelId": "alt-model-1", "name": "Alt Model 1"},
                    {"modelId": "alt-model-2", "name": "Alt Model 2"},
                ],
            }
            if adapter_kind == "alt-harness"
            else {"currentModelId": "fake-model", "availableModels": []}
        )
        send({"id": mid, "result": {
            "sessionId": sid,
            "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "default"}]},
            "models": models,
        }})
    elif method == "session/set_model":
        active_model = msg["params"]["modelId"]
        send({"id": mid, "result": {}})
    elif method == "session/prompt":
        text = "\n".join(p.get("text", "") for p in msg["params"]["prompt"])
        for tag in ("HUB:PROV_SWITCH", "HUB:ALL_DOWN", "HUB:QUOTA_NEAR", "HUB:QUOTA_FAR"):
            if tag in text:
                session_scenario = tag
                break
        if "PeerBot" in text:
            session_is_peer = True
        if "HUB:MANUAL" in text and "主人手动将事项交接" not in text:
            with open(os.path.join(os.getcwd(), "manual1.txt"), "w", encoding="utf-8") as f:
                f.write("m1")
            update(sid, {
                "sessionUpdate": "tool_call",
                "toolCallId": "t-m1",
                "kind": "execute",
                "status": "pending",
                "title": "step1.sh",
                "rawInput": {"command": "sh step1.sh"},
                "_meta": {"claudeCode": {"toolName": "Bash"}},
            })
            update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "t-m1", "status": "completed"})
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Manual step 1 done"}})
            update(sid, {"sessionUpdate": "usage_update", "used": 2000, "size": 10000})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 500, "outputTokens": 100, "cachedReadTokens": 0, "cachedWriteTokens": 0, "totalTokens": 600},
            }})
        elif "HUB:MANUAL" in text and adapter_kind != "alt-harness":
            with open(os.path.join(ctrl_dir, "m2.prompt"), "w", encoding="utf-8") as f:
                f.write(text)
            m1 = open(os.path.join(os.getcwd(), "manual1.txt"), "r", encoding="utf-8").read()
            with open(os.path.join(os.getcwd(), "manual2.txt"), "w", encoding="utf-8") as f:
                f.write(m1 + "+m2")
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Manual step 2 done on primary"}})
            update(sid, {"sessionUpdate": "usage_update", "used": 1500, "size": 10000})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 300, "outputTokens": 60, "cachedReadTokens": 0, "cachedWriteTokens": 0, "totalTokens": 360},
            }})
        elif "HUB:MANUAL" in text and adapter_kind == "alt-harness":
            with open(os.path.join(ctrl_dir, "m3.prompt"), "w", encoding="utf-8") as f:
                f.write(text)
            with open(os.path.join(ctrl_dir, "m3.meta"), "w", encoding="utf-8") as f:
                f.write(json.dumps({"adapter": adapter_kind, "model": active_model, "cwd": os.getcwd()}))
            m2 = open(os.path.join(os.getcwd(), "manual2.txt"), "r", encoding="utf-8").read()
            with open(os.path.join(os.getcwd(), "manual3.txt"), "w", encoding="utf-8") as f:
                f.write(m2 + "+m3")
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Manual step 3 done on alt"}})
            update(sid, {"sessionUpdate": "usage_update", "used": 1100, "size": 20000})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 250, "outputTokens": 50, "cachedReadTokens": 40, "cachedWriteTokens": 0, "totalTokens": 340},
            }})
        elif "HUB:QUOTA_NEAR" in text:
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "near quota hit"}})
            send({"id": mid, "error": {"code": -32000, "message": "API Error: 429 · upstream account quota exhausted; try again in 5m"}})
        elif "HUB:QUOTA_FAR" in text and adapter_kind != "alt-harness":
            with open(os.path.join(os.getcwd(), "quota1.txt"), "w", encoding="utf-8") as f:
                f.write("q1")
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "quota1 done before 429"}})
            send({"id": mid, "error": {"code": -32000, "message": "API Error: 429 · upstream account quota exhausted; try again in 3h"}})
        elif "HUB:QUOTA_FAR" in text and adapter_kind == "alt-harness":
            with open(os.path.join(ctrl_dir, "quota.prompt"), "w", encoding="utf-8") as f:
                f.write(text)
            with open(os.path.join(ctrl_dir, "quota.meta"), "w", encoding="utf-8") as f:
                f.write(json.dumps({"adapter": adapter_kind, "model": active_model, "cwd": os.getcwd()}))
            q1 = open(os.path.join(os.getcwd(), "quota1.txt"), "r", encoding="utf-8").read()
            with open(os.path.join(os.getcwd(), "quota2.txt"), "w", encoding="utf-8") as f:
                f.write(q1 + "+q2")
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "quota fallback completed"}})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 200, "outputTokens": 40, "cachedReadTokens": 20, "cachedWriteTokens": 0, "totalTokens": 260},
            }})
        elif ("HUB:PROV_SWITCH" in text or session_scenario == "HUB:PROV_SWITCH") and adapter_kind != "alt-harness":
            with open(os.path.join(os.getcwd(), "prov1.txt"), "w", encoding="utf-8") as f:
                f.write("p1")
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "prov primary failed"}})
            send({"id": mid, "error": {"code": -32000, "message": "upstream provider error: gateway rejected request"}})
        elif ("HUB:PROV_SWITCH" in text or session_scenario == "HUB:PROV_SWITCH") and adapter_kind == "alt-harness":
            prov_prompt = os.path.join(ctrl_dir, "prov.prompt")
            if not os.path.exists(prov_prompt):
                with open(prov_prompt, "w", encoding="utf-8") as f:
                    f.write(text)
            with open(os.path.join(ctrl_dir, "prov.meta"), "w", encoding="utf-8") as f:
                f.write(json.dumps({"adapter": adapter_kind, "model": active_model, "cwd": os.getcwd()}))
            p1 = open(os.path.join(os.getcwd(), "prov1.txt"), "r", encoding="utf-8").read()
            with open(os.path.join(os.getcwd(), "prov2.txt"), "w", encoding="utf-8") as f:
                f.write(p1 + "+p2")
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "prov fallback completed"}})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 180, "outputTokens": 35, "cachedReadTokens": 10, "cachedWriteTokens": 0, "totalTokens": 225},
            }})
        elif ("HUB:ALL_DOWN" in text or session_scenario == "HUB:ALL_DOWN") and not session_is_peer:
            with open(os.path.join(os.getcwd(), "all1.txt"), "w", encoding="utf-8") as f:
                f.write("a1")
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": f"all_down fail on {adapter_kind}"}})
            send({"id": mid, "error": {"code": -32000, "message": f"upstream provider failure on {adapter_kind}"}})
        elif ("HUB:ALL_DOWN" in text or session_scenario == "HUB:ALL_DOWN") and session_is_peer:
            peer_prompt = os.path.join(ctrl_dir, "peer.prompt")
            if not os.path.exists(peer_prompt):
                with open(peer_prompt, "w", encoding="utf-8") as f:
                    f.write(text)
            a1 = open(os.path.join(os.getcwd(), "all1.txt"), "r", encoding="utf-8").read()
            with open(os.path.join(os.getcwd(), "all2.txt"), "w", encoding="utf-8") as f:
                f.write(a1 + "+peer")
            with open(os.path.join(ctrl_dir, "peer.cwd"), "w", encoding="utf-8") as f:
                f.write(os.getcwd())
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "peer bot finished"}})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 150, "outputTokens": 30, "cachedReadTokens": 0, "cachedWriteTokens": 0, "totalTokens": 180},
            }})
        else:
            send({"id": mid, "result": {"stopReason": "end_turn"}})
    elif mid is not None:
        send({"id": mid, "result": {}})
"#;

struct HubEnv {
    _home: tempfile::TempDir,
    ctrl: PathBuf,
    app: Router,
    cookie: String,
    db: PathBuf,
    store: Store,
}

impl HubEnv {
    async fn start(extra_vars: &[(&str, &str)]) -> Self {
        steve_hub::metrics::install();
        steve_modules::net::forbid_in_this_process();
        let home = tempfile::tempdir().expect("tmp");
        let h = home.path().to_path_buf();
        let ctrl = h.join("ctrl");
        std::fs::create_dir_all(&ctrl).expect("ctrl");

        let script = h.join("hub-fake-acp.py");
        std::fs::write(&script, HUB_FAKE_ACP_PY).expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        let adapter = h.join("adapter.sh");
        std::fs::write(
            &adapter,
            format!(
                "#!/bin/sh\nexport FAKE_HANDOFF_CTRL='{}'\nexport FAKE_ADAPTER_KIND='primary'\nexec python3 '{}' \"$@\"\n",
                ctrl.display(),
                script.display()
            ),
        )
        .expect("write adapter");
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        let alt_adapter = h.join("alt-adapter.sh");
        std::fs::write(
            &alt_adapter,
            format!(
                "#!/bin/sh\nexport FAKE_HANDOFF_CTRL='{}'\nexport FAKE_ADAPTER_KIND='alt-harness'\nexec python3 '{}' \"$@\"\n",
                ctrl.display(),
                script.display()
            ),
        )
        .expect("write alt adapter");
        std::fs::set_permissions(&alt_adapter, std::fs::Permissions::from_mode(0o700))
            .expect("chmod");

        let steve_home = h.join("steve");
        std::fs::create_dir_all(&steve_home).expect("steve_home");
        let manifest_path = steve_home.join("harness.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&json!({
                "adapters": {
                    "primary-harness": {
                        "command": adapter.display().to_string(),
                        "name": "Primary Harness",
                        "properties": {
                            "nested_sandbox": false
                        }
                    },
                    "alt-harness": {
                        "command": alt_adapter.display().to_string(),
                        "name": "Alt Harness",
                        "properties": {
                            "nested_sandbox": false
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .expect("write manifest");

        let port = 18791u16;
        let mut vars: HashMap<String, String> = HashMap::new();
        for (k, v) in [
            ("HOME", h.display().to_string()),
            ("STEVE_HOME", steve_home.display().to_string()),
            (
                "STEVE_HARNESS_MANIFEST",
                manifest_path.display().to_string(),
            ),
            ("STEVE_PORT", port.to_string()),
            ("STEVE_HOST", "127.0.0.1".into()),
            ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
            ("STEVE_DEV_LOGIN", "1".into()),
            ("STEVE_TAKEOVER_CONTINUES", "0".into()),
            ("STEVE_REPO", "/nonexistent".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("STEVE_NODE_MODE", "local".into()),
            ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
            ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
        ] {
            vars.insert(k.into(), v);
        }
        for (k, v) in extra_vars {
            vars.insert((*k).into(), (*v).into());
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
        steve_hub::app::boot(&st).await.expect("boot");
        for job in w.jobs {
            job.start(&st.jobs);
        }
        let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);

        let peer = SocketAddr::from(([127, 0, 0, 1], port));
        let login_req = Request::builder()
            .method("GET")
            .uri("/auth/dev")
            .header("host", format!("127.0.0.1:{port}"))
            .extension(ConnectInfo(peer))
            .body(Body::empty())
            .expect("login req");
        let r = app.clone().oneshot(login_req).await.expect("dev login");
        let status = r.status();
        let headers = r.headers().clone();
        let cookie = headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("steve_session="))
            .map(|c| c.split(';').next().unwrap_or_default().to_string())
            .unwrap_or_else(|| {
                panic!("missing session cookie: status={status} headers={headers:?}")
            });

        let db = cfg.db_path();
        let store = Store::open(&db, NoPublisher).expect("open store");
        HubEnv {
            _home: home,
            ctrl,
            app,
            cookie,
            db,
            store,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let peer = SocketAddr::from(([127, 0, 0, 1], 18791));
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:18791")
            .header("cookie", &self.cookie)
            .extension(ConnectInfo(peer));
        let req_body = if let Some(b) = body {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).expect("json body"))
        } else {
            Body::empty()
        };
        let req = builder.body(req_body).expect("request");
        let r = self.app.clone().oneshot(req).await.expect("oneshot");
        let status = r.status().as_u16();
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn create_bot(&self, name: &str, harness: &str) -> String {
        let body = json!({ "name": name, "harness": harness, "perm_mode": "default" });
        let (s, v) = self.call("POST", "/api/bots", Some(body)).await;
        assert_eq!(s, 200, "create bot failed: {v}");
        v["id"].as_str().expect("bot id").to_string()
    }

    async fn session_views(&self, item_id: &str) -> Vec<Value> {
        let (s, v) = self
            .call(
                "GET",
                &format!("/api/views?kind=session&items={item_id}"),
                None,
            )
            .await;
        assert_eq!(s, 200, "session views failed: {v}");
        v["views"].as_array().cloned().unwrap_or_default()
    }

    async fn refresh_local_capabilities(&self) {
        let (s, v) = self
            .call("GET", "/api/node-harnesses/local?fresh=1", None)
            .await;
        assert_eq!(s, 200, "refresh local node harnesses failed: {v}");
    }

    async fn create_item(&self, bot: &str, title: &str, goal: &str) -> (String, String) {
        let (s, i) = self
            .call(
                "POST",
                "/api/items",
                Some(json!({ "goal": goal, "title": title, "bot": bot, "project": null })),
            )
            .await;
        assert_eq!(s, 200, "create item failed: {i}");
        let iid = i["id"].as_str().expect("item id").to_string();
        let (_, ss) = self
            .call("GET", &format!("/api/views?kind=session&items={iid}"), None)
            .await;
        let sid = ss["views"]
            .as_array()
            .and_then(|arr| arr.iter().find(|s| s["item_id"] == iid.as_str()))
            .and_then(|s| s["id"].as_str())
            .unwrap_or_else(|| panic!("no session for {iid}: {ss}"))
            .to_string();
        (iid, sid)
    }
}

#[allow(clippy::type_complexity)]
fn sum_tokens(
    rows: &[(
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<f64>,
        String,
    )],
) -> (i64, i64, i64, i64) {
    rows.iter().fold((0, 0, 0, 0), |acc, r| {
        (
            acc.0 + r.1.unwrap_or(0),
            acc.1 + r.2.unwrap_or(0),
            acc.2 + r.3.unwrap_or(0),
            acc.3 + r.4.unwrap_or(0),
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_handoff_same_and_cross_harness_preserves_work_and_blocks_invalid_target() {
    let e = HubEnv::start(&[]).await;
    e.refresh_local_capabilities().await;
    let bot = e.create_bot("ManualBot", "primary-harness").await;
    let (iid, s1) = e
        .create_item(&bot, "Manual handoff flow", "HUB:MANUAL")
        .await;

    eventually("session #1 completed", || async {
        fixtures::acp_turns(&e.db, &s1)
            .map(|t| t.len() == 1 && t[0].0.is_some())
            .unwrap_or(false)
    })
    .await;

    // 1. Invalid target harness must be refused via W3.1 placement (409) BEFORE ending session #1.
    let (bad_s, bad_v) = e
        .call(
            "POST",
            &format!("/api/items/{iid}/handoff"),
            Some(json!({ "harness": "missing-harness" })),
        )
        .await;
    assert_eq!(bad_s, 409, "expected 409 placement refusal: {bad_v}");
    assert_eq!(bad_v["placement"]["reason"], "harness", "{bad_v}");
    assert_eq!(e.store.sessions().of_item(&iid).unwrap().len(), 1);
    assert_ne!(
        e.store.sessions().get(&s1).unwrap().unwrap().status,
        "ended"
    );

    // Also reject an unsupported model advertised by alt-harness before ending session #1.
    let (bad_m, bad_mv) = e
        .call(
            "POST",
            &format!("/api/items/{iid}/handoff"),
            Some(json!({ "harness": "alt-harness", "model": "nonexistent-model" })),
        )
        .await;
    assert_eq!(bad_m, 409, "expected 409 model refusal: {bad_mv}");
    assert_eq!(bad_mv["placement"]["reason"], "model", "{bad_mv}");
    assert_eq!(e.store.sessions().of_item(&iid).unwrap().len(), 1);

    // 2. Manual same-harness handoff from item endpoint (default empty target).
    let (hs2, hv2) = e
        .call(
            "POST",
            &format!("/api/items/{iid}/handoff"),
            Some(json!({})),
        )
        .await;
    assert_eq!(hs2, 200, "manual same-harness handoff failed: {hv2}");
    let s2 = hv2["session"].as_str().expect("s2 id").to_string();
    eventually("session #2 completed", || async {
        fixtures::acp_turns(&e.db, &s2)
            .map(|t| t.len() == 1 && t[0].0.is_some())
            .unwrap_or(false)
    })
    .await;

    let m2_prompt = std::fs::read_to_string(e.ctrl.join("m2.prompt")).expect("m2 prompt");
    assert!(m2_prompt.contains("工作记忆"), "{m2_prompt}");
    assert!(m2_prompt.contains("[ok] Bash"), "{m2_prompt}");
    assert!(m2_prompt.contains("Manual step 1 done"), "{m2_prompt}");

    // 3. Manual cross-harness + explicit model handoff from session endpoint with
    // `make_bot_default: true` ("以后这个 Bot 都用这个"), verifying reversible default snapshot.
    let (hs3, hv3) = e
        .call(
            "POST",
            &format!("/api/sessions/{s2}/handoff"),
            Some(json!({
                "harness": "alt-harness",
                "model": "alt-model-2",
                "make_bot_default": true
            })),
        )
        .await;
    assert_eq!(hs3, 200, "manual cross-harness handoff failed: {hv3}");
    let s3 = hv3["session"].as_str().expect("s3 id").to_string();
    assert_eq!(hv3["bot_default_updated"]["bot"], bot);
    assert_eq!(
        hv3["bot_default_updated"]["previous"]["harness"],
        "primary-harness"
    );
    let updated_bot = e.store.bots().get(&bot).unwrap().unwrap();
    assert_eq!(updated_bot.harness, "alt-harness");
    assert_eq!(updated_bot.model.as_deref(), Some("alt-model-2"));

    eventually("session #3 completed", || async {
        fixtures::acp_turns(&e.db, &s3)
            .map(|t| t.len() == 1 && t[0].0.is_some())
            .unwrap_or(false)
    })
    .await;

    let m3_prompt = std::fs::read_to_string(e.ctrl.join("m3.prompt")).expect("m3 prompt");
    assert!(
        m3_prompt.contains("Manual step 2 done on primary"),
        "{m3_prompt}"
    );
    let m3_meta: Value =
        serde_json::from_str(&std::fs::read_to_string(e.ctrl.join("m3.meta")).unwrap()).unwrap();
    assert_eq!(m3_meta["adapter"], "alt-harness");
    assert_eq!(m3_meta["model"], "alt-model-2");
    let m3_cwd = PathBuf::from(m3_meta["cwd"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(m3_cwd.join("manual3.txt")).unwrap(),
        "m1+m2+m3",
        "work across manual same-harness and cross-harness handoffs must be preserved"
    );

    // Verify explicit timeline entries & W3.1 placement visibility across all 3 sessions.
    let timeline = e.store.timeline().list(&iid).unwrap();
    assert!(
        timeline.iter().any(|t| {
            t.text.contains(
                "从 ManualBot Bot（Primary Harness · fake-model）交接给 ManualBot Bot（Primary Harness），原因：主人手动",
            )
        }),
        "missing manual same-harness timeline entry: {timeline:?}"
    );
    assert!(
        timeline.iter().any(|t| {
            t.text.contains(
                "从 ManualBot Bot（Primary Harness · fake-model）交接给 ManualBot Bot（Alt Harness · alt-model-2），原因：主人手动",
            )
        }),
        "missing manual cross-harness timeline entry: {timeline:?}"
    );
    let views = e.session_views(&iid).await;
    let s3_view = views
        .iter()
        .find(|v| v["id"] == s3.as_str())
        .expect("s3 view");
    assert_eq!(s3_view["harness"], "alt-harness");
    assert_eq!(s3_view["model"], "alt-model-2");
    assert_eq!(s3_view["placement"]["bot"], bot);
    assert_eq!(s3_view["placement"]["reason"], "handoff_owner_request");

    // Verify token usage is separated per session with zero double-counting.
    assert_eq!(
        sum_tokens(&fixtures::acp_usage(&e.db, &s1).unwrap()),
        (500, 100, 0, 0)
    );
    assert_eq!(
        sum_tokens(&fixtures::acp_usage(&e.db, &s2).unwrap()),
        (300, 60, 0, 0)
    );
    assert_eq!(
        sum_tokens(&fixtures::acp_usage(&e.db, &s3).unwrap()),
        (250, 50, 40, 0)
    );

    // Verify handoff telemetry and read-only observed turn context peak metric (over Protocol 13).
    let prom = steve_hub::metrics::render().expect("metrics installed");
    assert!(
        prom.contains(
            r#"steve_handoffs_total{reason="owner_request",from_harness="primary-harness",to_harness="alt-harness",switched_bot="false"}"#
        ),
        "missing owner handoff metric:\n{prom}"
    );
    assert!(
        prom.contains(r#"steve_handoff_package_bytes_count{reason="owner_request"}"#),
        "missing handoff package bytes metric:\n{prom}"
    );
    assert!(
        prom.contains(
            r#"steve_handoff_outcomes_total{reason="owner_request",outcome="succeeded"}"#
        ),
        "missing handoff outcome metric:\n{prom}"
    );
    assert!(
        prom.contains(r#"steve_turn_context_peak_tokens_count{harness="primary-harness"}"#),
        "missing read-only context peak metric:\n{prom}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn quota_exhausted_waits_when_near_and_auto_handoffs_fallback_when_far() {
    let e = HubEnv::start(&[]).await;
    e.refresh_local_capabilities().await;
    let bot = e.create_bot("QuotaBot", "primary-harness").await;

    // Configure Bot's ordered fallback execution surface via PATCH /api/bots/{id}.
    let (ps, pv) = e
        .call(
            "PATCH",
            &format!("/api/bots/{bot}"),
            Some(json!({
                "fallback_surfaces": [
                    { "harness": "alt-harness", "model": "alt-model-2" }
                ]
            })),
        )
        .await;
    assert_eq!(ps, 200, "patch fallback_surfaces failed: {pv}");
    let stored_fs = e.store.bots().fallback_surfaces(&bot).unwrap();
    assert_eq!(stored_fs.len(), 1);
    assert_eq!(stored_fs[0].harness, "alt-harness");
    assert_eq!(stored_fs[0].model.as_deref(), Some("alt-model-2"));

    // 1. Near recovery (5m <= default 10m quotaWaitMinutes): wait on current surface, no switch, no card.
    let (iid_near, s_near) = e.create_item(&bot, "Near quota", "HUB:QUOTA_NEAR").await;
    eventually("near quota wait recorded on timeline", || async {
        e.store
            .timeline()
            .list(&iid_near)
            .unwrap_or_default()
            .iter()
            .any(|t| t.text.contains("模型配额用完"))
    })
    .await;
    assert_eq!(
        e.store.sessions().of_item(&iid_near).unwrap().len(),
        1,
        "near quota recovery must wait rather than switch surface"
    );
    assert!(
        e.store
            .decisions()
            .of_item(&iid_near)
            .unwrap()
            .iter()
            .all(|d| d.status != steve_types::DecisionStatus::Open),
        "near quota wait must not open a decision card"
    );
    assert_eq!(
        e.store.sessions().get(&s_near).unwrap().unwrap().harness,
        "primary-harness"
    );

    // 2. Far recovery (3h > 10m threshold): automatically hand off to ordered fallback surface on the SAME Bot.
    let (iid_far, s1) = e.create_item(&bot, "Far quota", "HUB:QUOTA_FAR").await;
    eventually(
        on_timeout(|| async {
            format!(
                "waiting for quota fallback session #2; sessions={:?}, timeline={:?}",
                e.store.sessions().of_item(&iid_far),
                e.store.timeline().list(&iid_far),
            )
        }),
        || async {
            let sessions = e.store.sessions().of_item(&iid_far).unwrap_or_default();
            if sessions.len() < 2 {
                return false;
            }
            fixtures::acp_turns(&e.db, &sessions[1].id)
                .map(|t| !t.is_empty() && t[0].0.is_some())
                .unwrap_or(false)
        },
    )
    .await;

    let mut sessions = e.store.sessions().of_item(&iid_far).unwrap();
    sessions.sort_by_key(|s| s.no);
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    assert_eq!(sessions[0].id, s1);
    assert_eq!(sessions[0].status, "ended");
    let s2 = &sessions[1];
    assert_eq!(s2.bot_id.as_deref(), Some(bot.as_str()));
    assert_eq!(s2.harness, "alt-harness");
    assert_eq!(s2.model.as_deref(), Some("alt-model-2"));

    // Bot default config must remain unchanged.
    let bot_after = e.store.bots().get(&bot).unwrap().unwrap();
    assert_eq!(bot_after.harness, "primary-harness");
    assert_eq!(bot_after.model, None);

    // Work preserved across harness switch.
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(e.ctrl.join("quota.meta")).unwrap()).unwrap();
    assert_eq!(meta["adapter"], "alt-harness");
    assert_eq!(meta["model"], "alt-model-2");
    let cwd = PathBuf::from(meta["cwd"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(cwd.join("quota2.txt")).unwrap(),
        "q1+q2"
    );
    let q_prompt = std::fs::read_to_string(e.ctrl.join("quota.prompt")).unwrap();
    assert!(q_prompt.contains("quota1 done before 429"), "{q_prompt}");

    // Explicit timeline event & W3.1 placement reason.
    let timeline = e.store.timeline().list(&iid_far).unwrap();
    assert!(
        timeline.iter().any(|t| {
            t.text.contains(
                "从 QuotaBot Bot（Primary Harness · fake-model）交接给 QuotaBot Bot（Alt Harness · alt-model-2），原因：额度不足，预计 ",
            ) && t.text.contains("恢复")
        }),
        "missing quota handoff timeline entry: {timeline:?}"
    );
    let views = e.session_views(&iid_far).await;
    let s2_view = views
        .iter()
        .find(|v| v["id"] == s2.id.as_str())
        .expect("s2 view");
    assert_eq!(s2_view["placement"]["bot"], bot);
    assert_eq!(s2_view["placement"]["reason"], "handoff_quota_exhausted");

    let prom = steve_hub::metrics::render().expect("metrics installed");
    assert!(
        prom.contains(
            r#"steve_handoffs_total{reason="quota_exhausted",from_harness="primary-harness",to_harness="alt-harness",switched_bot="false"}"#
        ),
        "missing quota handoff metric:\n{prom}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_error_switches_fallback_surface_and_escalates_card_when_all_unavailable() {
    let e = HubEnv::start(&[
        ("STEVE_TAKEOVER_CONTINUES", "2"),
        ("STEVE_PROVIDER_ERROR_HANDOFF_THRESHOLD", "2"),
    ])
    .await;
    e.refresh_local_capabilities().await;
    let bot = e.create_bot("ErrBot", "primary-harness").await;
    let peer = e.create_bot("PeerBot", "alt-harness").await;
    let (ps, pv) = e
        .call(
            "PATCH",
            &format!("/api/bots/{bot}"),
            Some(json!({
                "fallback_surfaces": [
                    { "harness": "alt-harness", "model": "alt-model-2" }
                ]
            })),
        )
        .await;
    assert_eq!(ps, 200, "patch fallback_surfaces failed: {pv}");

    // 1. Consecutive provider errors on primary surface reach threshold (2) -> switch to fallback surface on SAME Bot.
    let (iid1, _) = e
        .create_item(&bot, "Provider error switch", "HUB:PROV_SWITCH")
        .await;
    eventually(
        on_timeout(|| async {
            format!(
                "waiting for provider-error fallback session #2; sessions={:?}, timeline={:?}",
                e.store.sessions().of_item(&iid1),
                e.store.timeline().list(&iid1),
            )
        }),
        || async {
            let sessions = e.store.sessions().of_item(&iid1).unwrap_or_default();
            if sessions.len() < 2 {
                return false;
            }
            fixtures::acp_turns(&e.db, &sessions[1].id)
                .map(|t| !t.is_empty() && t[0].0.is_some())
                .unwrap_or(false)
        },
    )
    .await;

    let mut sessions = e.store.sessions().of_item(&iid1).unwrap();
    sessions.sort_by_key(|s| s.no);
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    assert_eq!(sessions[1].bot_id.as_deref(), Some(bot.as_str()));
    assert_eq!(sessions[1].harness, "alt-harness");
    assert_eq!(sessions[1].model.as_deref(), Some("alt-model-2"));
    let prov_meta: Value =
        serde_json::from_str(&std::fs::read_to_string(e.ctrl.join("prov.meta")).unwrap()).unwrap();
    let prov_cwd = PathBuf::from(prov_meta["cwd"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(prov_cwd.join("prov2.txt")).unwrap(),
        "p1+p2"
    );
    let tl1 = e.store.timeline().list(&iid1).unwrap();
    assert!(
        tl1.iter().any(|t| {
            t.text.contains(
                "从 ErrBot Bot（Primary Harness · fake-model）交接给 ErrBot Bot（Alt Harness · alt-model-2），原因：上游持续故障",
            )
        }),
        "missing provider-error handoff timeline entry: {tl1:?}"
    );
    let views1 = e.session_views(&iid1).await;
    let s2_view = views1
        .iter()
        .find(|v| v["id"] == sessions[1].id.as_str())
        .expect("s2 view");
    assert_eq!(s2_view["placement"]["bot"], bot);
    assert_eq!(s2_view["placement"]["reason"], "handoff_provider_error");

    // 2. When ALL execution surfaces of ErrBot fail, Steve MUST NOT auto-switch Bot:
    // it opens a coordinator card asking the owner; choosing bot:<PeerBot> hands off cleanly with working memory.
    let bot2 = e.create_bot("ExhaustBot", "primary-harness").await;
    let (ps2, pv2) = e
        .call(
            "PATCH",
            &format!("/api/bots/{bot2}"),
            Some(json!({
                "fallback_surfaces": [
                    { "harness": "alt-harness", "model": "alt-model-2" }
                ]
            })),
        )
        .await;
    assert_eq!(ps2, 200, "patch bot2 fallback_surfaces failed: {pv2}");

    let (iid2, _) = e
        .create_item(&bot2, "All surfaces down", "HUB:ALL_DOWN")
        .await;
    eventually(
        on_timeout(|| async {
            format!(
                "waiting for coordinator handoff_unavailable card; decisions={:?}, sessions={:?}",
                e.store.decisions().of_item(&iid2),
                e.store.sessions().of_item(&iid2),
            )
        }),
        || async {
            e.store
                .decisions()
                .of_item(&iid2)
                .unwrap_or_default()
                .iter()
                .any(|d| {
                    d.status == steve_types::DecisionStatus::Open
                        && d.kind == "fail"
                        && d.subject_type == steve_core::HANDOFF_UNAVAILABLE_SUBJECT
                })
        },
    )
    .await;

    // Must NOT have auto-switched Bot!
    let item_before = e.store.items().get(&iid2).unwrap().unwrap();
    assert_eq!(
        item_before.bot_id.as_deref(),
        Some(bot2.as_str()),
        "Steve must NEVER auto-switch Bot when all execution surfaces are unavailable"
    );
    let card = e
        .store
        .decisions()
        .of_item(&iid2)
        .unwrap()
        .into_iter()
        .find(|d| {
            d.status == steve_types::DecisionStatus::Open
                && d.kind == "fail"
                && d.subject_type == steve_core::HANDOFF_UNAVAILABLE_SUBJECT
        })
        .expect("coordinator card");
    let peer_choice = format!("bot:{peer}");
    assert!(
        card.options.contains(&peer_choice),
        "coordinator card must offer PeerBot: {:?}",
        card.options
    );
    assert!(
        card.options.contains(&"tosteve".to_string()),
        "coordinator card must offer tosteve: {:?}",
        card.options
    );

    // Owner explicitly picks PeerBot on the coordinator card -> full working-memory handoff.
    let (rs, rv) = e
        .call(
            "POST",
            &format!("/api/decisions/{}", card.id),
            Some(json!({ "choice": peer_choice })),
        )
        .await;
    assert_eq!(rs, 200, "resolve coordinator card failed: {rv}");

    eventually(
        on_timeout(|| async {
            format!(
                "waiting for PeerBot session to finish; sessions={:?}",
                e.store.sessions().of_item(&iid2),
            )
        }),
        || async {
            let sessions = e.store.sessions().of_item(&iid2).unwrap_or_default();
            sessions
                .iter()
                .find(|s| s.bot_id.as_deref() == Some(peer.as_str()))
                .and_then(|s| fixtures::acp_turns(&e.db, &s.id).ok())
                .is_some_and(|t| !t.is_empty() && t[0].0.is_some())
        },
    )
    .await;

    let item_after = e.store.items().get(&iid2).unwrap().unwrap();
    assert_eq!(item_after.bot_id.as_deref(), Some(peer.as_str()));
    let peer_prompt = std::fs::read_to_string(e.ctrl.join("peer.prompt")).unwrap();
    assert!(peer_prompt.contains("工作记忆"), "{peer_prompt}");
    let peer_cwd = PathBuf::from(
        std::fs::read_to_string(e.ctrl.join("peer.cwd"))
            .unwrap()
            .trim(),
    );
    assert_eq!(
        std::fs::read_to_string(peer_cwd.join("all2.txt")).unwrap(),
        "a1+peer",
        "work must carry over when owner switches Bot via coordinator card"
    );
    let peer_session = e
        .store
        .sessions()
        .of_item(&iid2)
        .unwrap()
        .into_iter()
        .find(|s| s.bot_id.as_deref() == Some(peer.as_str()))
        .expect("peer session");
    let views2 = e.session_views(&iid2).await;
    let peer_view = views2
        .iter()
        .find(|v| v["id"] == peer_session.id.as_str())
        .expect("peer view");
    assert_eq!(peer_view["placement"]["bot"], peer);
    assert_eq!(peer_view["placement"]["reason"], "handoff_owner_request");
}

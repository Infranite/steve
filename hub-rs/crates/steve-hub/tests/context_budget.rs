//! End-to-end Hub + Core + LocalNode + fake ACP integration tests for turn context budgets (#502):
//! 1. Native-compaction harness: declarative env launch, never cut off by Steve, native compaction
//!    recorded in diagnostics + Prometheus metrics, zero usage double-counting.
//! 2. Non-native harness: deterministic budget cutoff -> Core working-memory handoff (plan,
//!    resolved decision, completed tool, last progress, drained queued message) -> work completed
//!    in fresh session without loss -> zero usage double-counting across turns.
//! 3. Consecutive context-budget handoffs cap at `STEVE_TURN_CONTEXT_HANDOFF_LIMIT` and open a fail
//!    decision card instead of looping indefinitely.
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
use steve_store::{ItemPatch, NewDecision, NoPublisher, PlanStep, Store, WS};
use tower::ServiceExt;

const HUB_FAKE_ACP_PY: &str = r#"#!/usr/bin/env python3
import json, os, select, sys, time

ctrl_dir = os.environ["FAKE_CTX_CTRL"]
log_path = os.path.join(ctrl_dir, "acp.log")

def log(obj):
    if os.path.basename(os.getcwd()) != "probe":
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(obj) + "\n")

log({
    "spawn": {
        "argv": sys.argv[1:],
        "cwd": os.getcwd(),
        "env": {
            "FAKE_COMPACT_PCT": os.environ.get("FAKE_COMPACT_PCT"),
            "FAKE_COMPACT_WINDOW": os.environ.get("FAKE_COMPACT_WINDOW"),
        },
    }
})

def send(obj):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", **obj}) + "\n")
    sys.stdout.flush()

def update(sid, u):
    send({"method": "session/update", "params": {"sessionId": sid, "update": u}})

def wait_for_cancel(timeout_s=5.0):
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        r, _, _ = select.select([sys.stdin], [], [], 0.05)
        if not r:
            continue
        line = sys.stdin.readline()
        if not line:
            return False
        msg = json.loads(line)
        log(msg)
        if msg.get("method") == "session/cancel":
            return True
    return False

def wait_file(path, timeout_s=10.0):
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if os.path.exists(path):
            return True
        time.sleep(0.03)
    return False

sid = ""
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
        sid = f"fake-hub-{os.getpid()}"
        send({"id": mid, "result": {
            "sessionId": sid,
            "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "default"}]},
            "models": {"currentModelId": "fake-model", "availableModels": []},
        }})
    elif method == "session/prompt":
        text = "\n".join(p.get("text", "") for p in msg["params"]["prompt"])
        if "HUB:NATIVE" in text:
            update(sid, {"sessionUpdate": "usage_update", "used": 85000, "size": 100000})
            update(sid, {"sessionUpdate": "tool_call", "toolCallId": "cmp-1", "kind": "think", "status": "pending", "title": "compact", "_meta": {"claudeCode": {"toolName": "compact"}}})
            update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "cmp-1", "status": "completed"})
            update(sid, {"sessionUpdate": "usage_update", "used": 12000, "size": 100000})
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "native completed"}})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 1000, "outputTokens": 200, "cachedReadTokens": 500, "cachedWriteTokens": 50, "totalTokens": 1750},
            }})
        elif "HUB:HANDOFF" in text and "工作记忆" not in text:
            # Session #1 of handoff test: do real workspace work + tool call + progress, wait for
            # test to attach plan/decision/queued follow-up, then cross budget threshold.
            with open(os.path.join(os.getcwd(), "part1.txt"), "w", encoding="utf-8") as f:
                f.write("step1-ok")
            update(sid, {
                "sessionUpdate": "tool_call",
                "toolCallId": "t-prep",
                "kind": "execute",
                "status": "pending",
                "title": "prepare.sh",
                "rawInput": {"command": "sh prepare.sh"},
                "_meta": {"claudeCode": {"toolName": "Bash"}},
            })
            update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "t-prep", "status": "completed"})
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Finished step 1, moving to step 2"}})
            update(sid, {"sessionUpdate": "usage_update", "used": 4000, "size": 10000})
            with open(os.path.join(ctrl_dir, "s1.ready"), "w", encoding="utf-8") as f:
                f.write("ready")
            wait_file(os.path.join(ctrl_dir, "s1.proceed"), 10.0)
            update(sid, {"sessionUpdate": "usage_update", "used": 8500, "size": 10000})
            cancelled = wait_for_cancel(5.0)
            send({"id": mid, "result": {
                "stopReason": "cancelled" if cancelled else "end_turn",
                "usage": {"inputTokens": 600, "outputTokens": 120, "cachedReadTokens": 300, "cachedWriteTokens": 0, "totalTokens": 1020},
            }})
        elif "HUB:HANDOFF" in text or "工作记忆" in text:
            # Session #2 of handoff test: verify part1.txt survived in workspace, write prompt & part2.txt.
            with open(os.path.join(ctrl_dir, "s2.prompt"), "w", encoding="utf-8") as f:
                f.write(text)
            part1 = open(os.path.join(os.getcwd(), "part1.txt"), "r", encoding="utf-8").read()
            with open(os.path.join(os.getcwd(), "part2.txt"), "w", encoding="utf-8") as f:
                f.write(part1 + "+step2-ok")
            with open(os.path.join(ctrl_dir, "s2.cwd"), "w", encoding="utf-8") as f:
                f.write(os.getcwd())
            update(sid, {"sessionUpdate": "usage_update", "used": 1200, "size": 10000})
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Completed step 2"}})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 400, "outputTokens": 80, "cachedReadTokens": 100, "cachedWriteTokens": 0, "totalTokens": 580},
            }})
        elif "HUB:LOOP" in text:
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "loop progress"}})
            update(sid, {"sessionUpdate": "usage_update", "used": 8600, "size": 10000})
            cancelled = wait_for_cancel(5.0)
            send({"id": mid, "result": {
                "stopReason": "cancelled" if cancelled else "end_turn",
                "usage": {"inputTokens": 300, "outputTokens": 50, "cachedReadTokens": 0, "cachedWriteTokens": 0, "totalTokens": 350},
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
                "#!/bin/sh\nexport FAKE_CTX_CTRL='{}'\nexec python3 '{}' \"$@\"\n",
                ctrl.display(),
                script.display()
            ),
        )
        .expect("write adapter");
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        let steve_home = h.join("steve");
        std::fs::create_dir_all(&steve_home).expect("steve_home");
        let manifest_path = steve_home.join("harness.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&json!({
                "adapters": {
                    "native-bot": {
                        "command": adapter.display().to_string(),
                        "name": "Native Bot",
                        "properties": {
                            "nested_sandbox": false,
                            "context_limit": 100000,
                            "context_budget_pct": 80,
                            "native_compaction": {
                                "kind": "env_percent",
                                "var": "FAKE_COMPACT_PCT",
                                "window_var": "FAKE_COMPACT_WINDOW"
                            }
                        }
                    },
                    "budget-bot": {
                        "command": adapter.display().to_string(),
                        "name": "Budget Bot",
                        "properties": {
                            "nested_sandbox": false,
                            "context_limit": 10000,
                            "context_budget_pct": 80
                        }
                    },
                    "loop-bot": {
                        "command": adapter.display().to_string(),
                        "name": "Loop Bot",
                        "properties": {
                            "nested_sandbox": false,
                            "context_limit": 10000,
                            "context_budget_pct": 80
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
        let (s, v) = self
            .call(
                "POST",
                "/api/bots",
                Some(json!({ "name": name, "harness": harness, "perm_mode": "default" })),
            )
            .await;
        assert_eq!(s, 200, "create bot failed: {v}");
        v["id"].as_str().expect("bot id").to_string()
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

    async fn debug_events(&self, item_id: &str, kind: &str) -> Vec<Value> {
        let (s, v) = self
            .call(
                "GET",
                &format!("/api/items/{item_id}/debug/events?kinds={kind}"),
                None,
            )
            .await;
        assert_eq!(s, 200, "debug events failed: {v}");
        v["events"].as_array().cloned().unwrap_or_default()
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
async fn native_compaction_observes_without_handoff_and_keeps_exact_usage() {
    let e = HubEnv::start(&[]).await;
    let bot = e.create_bot("NativeTester", "native-bot").await;
    let (iid, sid) = e.create_item(&bot, "Native compaction", "HUB:NATIVE").await;

    eventually(
        on_timeout(|| async {
            format!(
                "waiting for native turn end: {:?}",
                fixtures::acp_turns(&e.db, &sid)
            )
        }),
        || async {
            fixtures::acp_turns(&e.db, &sid)
                .map(|t| t.len() == 1 && t[0].0.is_some())
                .unwrap_or(false)
        },
    )
    .await;

    let sessions = e.store.sessions().of_item(&iid).unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "native compaction must not spawn a handoff session"
    );
    assert_ne!(sessions[0].status, "ended");

    let turns = fixtures::acp_turns(&e.db, &sid).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].3, "end_turn");

    let usage = fixtures::acp_usage(&e.db, &sid).unwrap();
    assert_eq!(
        sum_tokens(&usage),
        (1000, 200, 500, 50),
        "native compaction must never double-count usage: {usage:?}"
    );

    eventually("native compaction diagnostic recorded", || async {
        !e.debug_events(&iid, "context.compacted").await.is_empty()
    })
    .await;
    let diag = e.debug_events(&iid, "context.compacted").await;
    assert_eq!(diag.len(), 1, "{diag:?}");
    assert_eq!(diag[0]["reason_code"], "native_compaction");
    assert_eq!(diag[0]["details"]["context_before"], 85_000);
    assert_eq!(diag[0]["details"]["context_after"], 12_000);
    assert_eq!(diag[0]["details"]["context_saved"], 73_000);
    assert_eq!(diag[0]["details"]["limit"], 100_000);

    let prom = steve_hub::metrics::render().expect("metrics installed");
    assert!(
        prom.contains(r#"steve_turn_context_compactions_total{harness="native-bot",mode="native",reason="native_compaction"} 1"#),
        "missing native compaction counter:\n{prom}"
    );
    assert!(
        prom.contains(
            r#"steve_turn_context_saved_tokens_total{harness="native-bot",mode="native"} 73000"#
        ),
        "missing native saved tokens counter:\n{prom}"
    );
    assert!(
        prom.contains(r#"steve_turn_context_peak_tokens_sum{harness="native-bot"} 85000"#),
        "missing peak context histogram:\n{prom}"
    );

    // Default launch must constrain 1M-window models via window_var=context_limit (100000).
    let read_spawns = || -> Vec<Value> {
        std::fs::read_to_string(e.ctrl.join("acp.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v.get("spawn").cloned())
            .collect()
    };
    let spawns = read_spawns();
    assert_eq!(spawns.len(), 1, "{spawns:?}");
    assert_eq!(spawns[0]["env"]["FAKE_COMPACT_PCT"], "80");
    assert_eq!(spawns[0]["env"]["FAKE_COMPACT_WINDOW"], "100000");

    // Raising the workspace `contextBudget` setting to 400k scales window_var to 500k (500k*80%=400k).
    let (ps, pv) = e
        .call(
            "PATCH",
            "/api/settings",
            Some(json!({ "contextBudget": "400000" })),
        )
        .await;
    assert_eq!(ps, 200, "patch workspace contextBudget failed: {pv}");
    let (_iid2, sid2) = e.create_item(&bot, "Raised budget", "HUB:NATIVE").await;
    eventually("second native session finished", || async {
        fixtures::acp_turns(&e.db, &sid2)
            .map(|t| t.len() == 1 && t[0].0.is_some())
            .unwrap_or(false)
    })
    .await;
    let spawns = read_spawns();
    assert_eq!(spawns.len(), 2, "{spawns:?}");
    assert_eq!(spawns[1]["env"]["FAKE_COMPACT_PCT"], "80");
    assert_eq!(spawns[1]["env"]["FAKE_COMPACT_WINDOW"], "500000");
}

#[tokio::test(flavor = "multi_thread")]
async fn non_native_budget_cutoff_hands_off_with_working_memory_and_exact_usage() {
    let e = HubEnv::start(&[("STEVE_TURN_CONTEXT_HANDOFF_LIMIT", "2")]).await;
    let bot = e.create_bot("BudgetTester", "budget-bot").await;
    let (iid, s1) = e.create_item(&bot, "Budget handoff", "HUB:HANDOFF").await;

    // Wait until session #1 has run its tool, written part1.txt, and emitted progress.
    eventually("session #1 reached checkpoint", || async {
        e.ctrl.join("s1.ready").exists()
    })
    .await;

    // Attach plan steps, a resolved decision, and a queued user follow-up while session #1 is live.
    e.store
        .items()
        .patch(
            &iid,
            &ItemPatch {
                plan: Some(vec![
                    PlanStep {
                        step: "read config".into(),
                        done: true,
                        left: false,
                    },
                    PlanStep {
                        step: "write output.txt".into(),
                        done: false,
                        left: false,
                    },
                ]),
                ..Default::default()
            },
        )
        .unwrap();
    let dec = e
        .store
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "ask".into(),
            subject_type: "item".into(),
            subject_id: iid.clone(),
            item_id: Some(iid.clone()),
            session_id: Some(s1.clone()),
            question: "target format?".into(),
            draft: None,
            options: Some(vec!["JSON".into(), "YAML".into()]),
            requested_by: "agent".into(),
        })
        .unwrap()
        .expect("decision");
    e.store
        .decisions()
        .resolve(
            &dec.id,
            &json!({"answer": "JSON"}),
            "resolved",
            &Value::Null,
        )
        .unwrap();
    let (status, queued_resp) = e
        .call(
            "POST",
            &format!("/api/sessions/{s1}/messages"),
            Some(json!({ "text": "also include version=2", "mode": "排队" })),
        )
        .await;
    assert_eq!(status, 200, "queue follow-up failed: {queued_resp}");

    // Unblock session #1 to hit 8500/10000 -> Node stops with context_budget -> Core hands off.
    std::fs::write(e.ctrl.join("s1.proceed"), "go").unwrap();

    eventually(
        on_timeout(|| async {
            format!(
                "waiting for handoff session #2 to finish; sessions={:?}",
                e.store.sessions().of_item(&iid)
            )
        }),
        || async {
            let sessions = e.store.sessions().of_item(&iid).unwrap_or_default();
            if sessions.len() < 2 {
                return false;
            }
            let s2_id = &sessions[1].id;
            fixtures::acp_turns(&e.db, s2_id)
                .map(|t| t.len() == 1 && t[0].0.is_some())
                .unwrap_or(false)
        },
    )
    .await;

    let mut sessions = e.store.sessions().of_item(&iid).unwrap();
    sessions.sort_by_key(|s| s.no);
    assert_eq!(
        sessions.len(),
        2,
        "expected exactly 2 sessions: {sessions:?}"
    );
    assert_eq!(sessions[0].id, s1);
    assert_eq!(sessions[0].status, "ended");
    let s2 = &sessions[1].id;
    assert_ne!(sessions[1].status, "ended");

    // Verify working memory & drained queued message arrived in session #2's opening prompt.
    let s2_prompt = std::fs::read_to_string(e.ctrl.join("s2.prompt")).expect("s2 prompt");
    for expected in [
        "工作记忆",
        "[x] read config",
        "[ ] write output.txt",
        "target format? => JSON",
        "[ok] Bash",
        "Finished step 1, moving to step 2",
        "also include version=2",
    ] {
        assert!(
            s2_prompt.contains(expected),
            "session #2 prompt missing {expected:?}:\n{s2_prompt}"
        );
    }

    // Verify work continuity in shared workspace (part1.txt + part2.txt both intact).
    let s2_cwd = PathBuf::from(
        std::fs::read_to_string(e.ctrl.join("s2.cwd"))
            .unwrap()
            .trim(),
    );
    assert_eq!(
        std::fs::read_to_string(s2_cwd.join("part2.txt")).unwrap(),
        "step1-ok+step2-ok"
    );

    // Verify turns & exact non-duplicated usage across both sessions.
    let t1 = fixtures::acp_turns(&e.db, &s1).unwrap();
    let t2 = fixtures::acp_turns(&e.db, s2).unwrap();
    assert_eq!(t1.len(), 1);
    assert_eq!(t1[0].3, "context_budget");
    assert_eq!(t2.len(), 1);
    assert_eq!(t2[0].3, "end_turn");

    let u1 = sum_tokens(&fixtures::acp_usage(&e.db, &s1).unwrap());
    let u2 = sum_tokens(&fixtures::acp_usage(&e.db, s2).unwrap());
    assert_eq!(u1, (600, 120, 300, 0));
    assert_eq!(u2, (400, 80, 100, 0));
    assert_eq!(
        (u1.0 + u2.0, u1.1 + u2.1, u1.2 + u2.2, u1.3 + u2.3),
        (1000, 200, 400, 0),
        "usage across handoff sessions must sum cleanly with zero double-counting"
    );

    // Verify handoff diagnostic & Prometheus metrics.
    eventually("handoff compaction diagnostic recorded", || async {
        !e.debug_events(&iid, "context.compacted").await.is_empty()
    })
    .await;
    let diag = e.debug_events(&iid, "context.compacted").await;
    assert_eq!(diag.len(), 1, "{diag:?}");
    assert_eq!(diag[0]["reason_code"], "budget_reached");
    assert_eq!(diag[0]["details"]["context_before"], 8500);
    assert!(diag[0]["details"]["context_saved"].as_u64().unwrap_or(0) > 0);

    let prom = steve_hub::metrics::render().expect("metrics installed");
    assert!(
        prom.contains(r#"steve_turn_context_compactions_total{harness="budget-bot",mode="handoff",reason="budget_reached"} 1"#),
        "missing handoff compaction counter:\n{prom}"
    );
    assert!(
        prom.contains(
            r#"steve_turn_context_saved_tokens_total{harness="budget-bot",mode="handoff"}"#
        ),
        "missing handoff saved tokens counter:\n{prom}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn consecutive_context_budget_handoffs_stop_at_configured_limit() {
    let e = HubEnv::start(&[("STEVE_TURN_CONTEXT_HANDOFF_LIMIT", "2")]).await;
    let bot = e.create_bot("LoopTester", "loop-bot").await;
    let (iid, _) = e.create_item(&bot, "Budget loop cap", "HUB:LOOP").await;

    eventually(
        on_timeout(|| async {
            format!(
                "waiting for fail card after 2 consecutive handoffs; sessions={:?}, decisions={:?}",
                e.store.sessions().of_item(&iid),
                e.store.decisions().of_item(&iid),
            )
        }),
        || async {
            e.store
                .decisions()
                .of_item(&iid)
                .unwrap_or_default()
                .iter()
                .any(|d| d.status == "open" && d.kind == "fail")
        },
    )
    .await;

    let sessions = e.store.sessions().of_item(&iid).unwrap();
    assert_eq!(
        sessions.len(),
        3,
        "limit=2 allows 2 handoffs (sessions 1->2->3) then stops: {sessions:?}"
    );
    let fail = e
        .store
        .decisions()
        .of_item(&iid)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .expect("fail card");
    assert!(fail.question.contains("2"), "{}", fail.question);
}

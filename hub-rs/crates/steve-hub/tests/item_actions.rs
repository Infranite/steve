//! The item page's quick actions over HTTP on the wired Hub (pause/resume, mark done and its undo, drop,
//! reopen), with the scripted ACP agent of steve-acp (`fake-acp.js`, run by node) as the claude-code
//! adapter. Temp STEVE_HOME, no real agent or host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use steve_runtime::testkit::eventually;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    store: steve_store::Store,
    /// one Hub at a time in this process: the fake agent's log path is a process env var (see [`start`])
    _one: tokio::sync::MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    log: PathBuf,
}

impl Env {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.expect("request");
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// The ACP sessions that received a prompt so far, and how many prompts they had in all. Steve's
    /// own one-shot model calls go to the same fake agent in sessions of their own.
    fn prompts_in(
        &self,
        sessions: Option<&std::collections::BTreeSet<String>>,
    ) -> (std::collections::BTreeSet<String>, usize) {
        let mut seen = std::collections::BTreeSet::new();
        let mut n = 0;
        for m in std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["method"] == "session/prompt")
        {
            let sid = m["params"]["sessionId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if sessions.is_none_or(|s| s.contains(&sid)) {
                n += 1;
            }
            seen.insert(sid);
        }
        (seen, n)
    }

    /// Prompt texts the fake agent received, in order.
    fn prompts(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["method"] == "session/prompt")
            .map(|m| {
                m["params"]["prompt"]
                    .as_array()
                    .map(|ps| {
                        ps.iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    async fn session(&self, id: &str) -> Value {
        let (s, v) = self.call("GET", &format!("/api/sessions/{id}"), None).await;
        assert_eq!(s, 200, "{v}");
        v
    }
}

static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn start() -> Env {
    let one = ONE.lock().await;
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    assert_ne!(port, 8440);
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
    let log = h.join("acp.log");
    // the adapter inherits the process env: the log path goes there
    // SAFETY: the tests here take `ONE` first, so one Hub per process sets it, before any adapter starts
    unsafe { std::env::set_var("FAKE_ACP_LOG", &log) };
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        // the fake agent's stops reach the card as they did; Steve's own continues are tested in steve-core
        ("STEVE_TAKEOVER_CONTINUES", "0".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        // the local Node runs the sessions (the Hub runs none itself)
        ("STEVE_NODE_MODE", "local".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", fake.display().to_string()),
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
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let store = w.modules.as_ref().expect("wired modules").store.clone();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        job.start(&st.jobs);
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
        store,
        _one: one,
        _home: home,
        base,
        http,
        cookie,
        log,
    }
}

/// An item whose first turn runs `goal` on the fake agent; returns its session id.
async fn item(e: &Env, goal: &str) -> String {
    let (_, state) = e.call("GET", "/api/state", None).await;
    let bot = state["bots"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
        .and_then(|b| b["id"].as_str())
        .expect("a claude-code bot")
        .to_string();
    let (s, i) = e
        .call(
            "POST",
            "/api/items",
            Some(json!({"goal": goal, "title": "快捷操作", "bot": bot, "project": null})),
        )
        .await;
    assert_eq!(s, 200, "{i}");
    let iid = i["id"].as_str().expect("item id").to_string();
    let (_, ss) = e.call("GET", "/api/sessions", None).await;
    let sid = ss
        .as_array()
        .and_then(|ss| ss.iter().find(|s| s["item_id"] == iid.as_str()))
        .and_then(|s| s["id"].as_str())
        .unwrap_or_else(|| panic!("no session of {iid} in {ss}"))
        .to_string();
    // Receiving the prompt proves the Node started the turn. The recorded fact survives a quick
    // completion, whereas sampling busy/running can miss the entire turn under contention.
    eventually(
        "the fake adapter receives the first item prompt",
        || async {
            let session = e.session(&sid).await;
            let Some(native) = session["harness_session_id"].as_str() else {
                return false;
            };
            let sessions = std::collections::BTreeSet::from([native.to_owned()]);
            e.prompts_in(Some(&sessions)).1 > 0
        },
    )
    .await;
    sid
}

async fn act(e: &Env, iid: &str, action: &str) -> (u16, Value) {
    e.call(
        "POST",
        &format!("/api/items/{iid}/actions"),
        Some(json!({ "action": action })),
    )
    .await
}

async fn item_of(e: &Env, sid: &str) -> String {
    e.session(sid).await["item_id"]
        .as_str()
        .expect("item id")
        .to_string()
}

async fn lifecycle(e: &Env, iid: &str) -> Value {
    let (s, v) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(s, 200, "{v}");
    v["lifecycle"].clone()
}

async fn command(e: &Env, iid: &str, body: Value) -> (u16, Value) {
    e.call("POST", &format!("/api/items/{iid}/lifecycle"), Some(body))
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn creation_blocks_before_opening_a_session_and_completion_starts_it_once() {
    let e = start().await;
    let (_, state) = e.call("GET", "/api/state", None).await;
    let bot = state["bots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["harness"] == "claude-code")
        .unwrap()["id"]
        .clone();
    let (status, dep) = e.call("POST", "/api/items", Some(json!({
        "goal":"prerequisite", "title":"prerequisite", "bot":bot, "project":null, "mode":"propose"
    }))).await;
    assert_eq!(status, 200, "{dep}");
    let dep = dep["id"].as_str().unwrap();
    let (status, item) = e.call("POST", "/api/items", Some(json!({
        "goal":"dependent @sleep 20000", "title":"dependent", "bot":bot, "project":null, "deps":[dep]
    }))).await;
    assert_eq!(status, 200, "{item}");
    assert_eq!(item["status"], "block", "{item}");
    let id = item["id"].as_str().unwrap();
    let (_, sessions) = e.call("GET", "/api/sessions", None).await;
    assert!(
        !sessions
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["item_id"] == id)
    );
    let (_, detail) = e.call("GET", &format!("/api/items/{id}"), None).await;
    assert_eq!(detail["dependency_wait"]["open_dependencies"], json!([dep]));
    assert!(
        detail["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["status"] != "open")
    );
    let (status, result) = act(&e, dep, "finish").await;
    assert_eq!(status, 200, "{result}");
    eventually(
        "Finishing the dependency starts the dependent item session.",
        || async {
            let (_, ss) = e.call("GET", "/api/sessions", None).await;
            ss.as_array()
                .unwrap()
                .iter()
                .any(|s| s["item_id"] == id && s["busy"] == true && s["status"] == "running")
        },
    )
    .await;
    let (_, ss) = e.call("GET", "/api/sessions", None).await;
    assert_eq!(
        ss.as_array()
            .unwrap()
            .iter()
            .filter(|s| s["item_id"] == id)
            .count(),
        1
    );
    // the fake adapter records a prompt after its session is already running: wait for it
    eventually(
        "The dependent prompt explicitly keeps unrelated items out of scope.",
        || async {
            e.prompts()
                .iter()
                .any(|p| p.contains("无需为核对进度读取其他事项或请求扩大读取范围"))
        },
    )
    .await;
    let (status, result) = act(&e, id, "drop").await;
    assert_eq!(status, 200, "{result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_parent_archives_a_historical_failed_child_over_http() {
    let e = start().await;
    let store = e.store.clone();
    let parent = store
        .items()
        .create(
            "default",
            &steve_store::NewItem {
                tags: vec![
                    store
                        .tags()
                        .kinds("default")
                        .unwrap()
                        .into_iter()
                        .find(|tag| tag.name == "feat")
                        .unwrap()
                        .id,
                ],
                title: "parent".into(),
                goal: "fixture parent".into(),
                bot_id: "steve".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    let child = store
        .items()
        .create(
            "default",
            &steve_store::NewItem {
                tags: vec![
                    store
                        .tags()
                        .kinds("default")
                        .unwrap()
                        .into_iter()
                        .find(|tag| tag.name == "feat")
                        .unwrap()
                        .id,
                ],
                title: "legacy failed child".into(),
                goal: "fixture attempt".into(),
                bot_id: "steve".into(),
                parent: Some(steve_store::Parent {
                    item: Some(Some(parent.id.clone())),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    store.with_conn(|c| {
        c.execute("UPDATE items SET phase='closed',status='fail',outcome='failed',closed_reason='old review failed' WHERE id=?", [&child.id])?;
        Ok(())
    }).unwrap();
    let (status, result) = act(&e, &parent.id, "finish").await;
    assert_eq!(status, 200, "{result}");
    let (status, read) = e
        .call("GET", &format!("/api/items/{}", child.id), None)
        .await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["archived"], true, "{read}");
    assert_eq!(read["outcome"], "failed");
    assert!(
        read["now"]
            .as_str()
            .unwrap()
            .contains(&format!("随父事项 {} 结束", parent.id))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_finish_drop_and_new_cycles() {
    let e = start().await;
    let sid = item(&e, "慢慢做 @sleep 20000").await;
    // the prompt reached the adapter; "finish" must land while the Hub sees the turn running,
    // otherwise it would close an idle item and the test would pass without stopping anything
    eventually("the Hub sees the first turn running", || async {
        let s = e.session(&sid).await;
        s["busy"] == true && s["status"] == "running"
    })
    .await;
    let iid = item_of(&e, &sid).await;
    assert_eq!(lifecycle(&e, &iid).await["phase"], "active");

    // mark done in the middle of a turn: the request returns at once, the turn is stopped outside the
    // lock, and the close commits once it stopped
    let (s, v) = act(&e, &iid, "finish").await;
    assert_eq!(s, 200, "{v}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "Finishing the item cancels its running session; the running turn is cancelled: {} {}",
            e.session(&sid).await,
            lifecycle(&e, &iid).await
        )
    });
    eventually(diagnostic, || async {
        e.session(&sid).await["busy"] == false
    })
    .await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The finished item closes once its runtime turn has stopped; closed once the turn stopped: {}",
            lifecycle(&e, &iid).await
        )
    });
    eventually(diagnostic, || async {
        lifecycle(&e, &iid).await["phase"] == "closed"
    })
    .await;
    let l = lifecycle(&e, &iid).await;
    assert_eq!(
        (
            l["outcome"].as_str(),
            l["acceptance"].as_str(),
            l["cycle"].as_i64()
        ),
        (Some("completed"), Some("human"), Some(1)),
        "{l}"
    );
    let (_, v) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(
        (v["status"].as_str(), v["now"].as_str()),
        (Some("done"), Some("你标记了完成"))
    );
    assert_eq!(v["cycles"].as_array().map(Vec::len), Some(1), "{v}");
    // the derived state rides along read-only, next to today's (#327)
    assert_eq!(
        (
            v["shadow"]["lane"].as_str(),
            v["shadow"]["agrees"].as_bool()
        ),
        (Some("done"), Some(true)),
        "{}",
        v["shadow"]
    );
    let (_, state) = e.call("GET", "/api/state", None).await;
    let row = state["items"]
        .as_array()
        .and_then(|xs| xs.iter().find(|x| x["id"] == iid.as_str()))
        .expect("item in state");
    assert_eq!(row["lifecycle"]["phase"], "closed", "{row}");

    // a closed cycle: no second finish, no undo, no resume, no message, no supplement
    for a in ["finish", "unfinish", "resume"] {
        let (s, v) = act(&e, &iid, a).await;
        assert_eq!(s, 409, "{a} on a closed item: {v}");
    }
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/sessions/{sid}/messages"),
            Some(json!({"text": "还有一件"})),
        )
        .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["error"], "这个事项已经结束了，需要时先新建周期", "{v}");
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/items/{iid}/supplement"),
            Some(json!({"text": "补充"})),
        )
        .await;
    assert_eq!(s, 409, "{v}");

    // the command API: version checked, the answer carries the lifecycle to refresh from
    let (s, v) = command(
        &e,
        &iid,
        json!({"command": "new_cycle", "expected_cycle": 1, "expected_revision": 9}),
    )
    .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["lifecycle"]["phase"], "closed", "{v}");
    let (s, v) = command(&e, &iid, json!({"command": "new_cycle"})).await;
    assert_eq!(s, 400, "versions are required: {v}");
    let rev = l["revision"].as_i64().unwrap();
    let (agent, n) = e.prompts_in(None);
    let (s, v) = command(
        &e,
        &iid,
        json!({"command": "new_cycle", "expected_cycle": 1, "expected_revision": rev}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let l = &v["item"]["lifecycle"];
    assert_eq!(
        (
            l["phase"].as_str(),
            l["cycle"].as_i64(),
            l["revision"].as_i64()
        ),
        (Some("ready"), Some(2), Some(0)),
        "{v}"
    );
    // without a goal it waits for you: nothing is sent to the agent
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(e.prompts_in(Some(&agent)).1, n);

    // pause and resume keep the phase
    let (s, v) = act(&e, &iid, "pause").await;
    assert_eq!((s, v["status"].as_str()), (200, Some("pause")), "{v}");
    let (s, _) = act(&e, &iid, "resume").await;
    assert_eq!(s, 200);
    // no timing here: a rejected or phase-keeping command leaves it ready at once; the whole lifecycle
    // is printed if not, to see what moved it
    let l = lifecycle(&e, &iid).await;
    assert_eq!(l["phase"], "ready", "{l}");

    // fail needs a reason; drop is a cancel: nothing runs, it closes at once
    let (s, v) = command(
        &e,
        &iid,
        json!({"command": "fail", "expected_cycle": 2, "expected_revision": 0}),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    let l = lifecycle(&e, &iid).await;
    assert_eq!(l["phase"], "ready", "{l}");
    let (s, v) = act(&e, &iid, "drop").await;
    /* dropped at once, or (a turn still winding down under load) closing as a cancel */
    assert_eq!(s, 200, "{v}");
    assert!(
        v["status"] == "drop" || (v["phase"] == "closing" && v["closing"]["kind"] == "cancel"),
        "{v}"
    );
    // it closes once nothing of it runs (a turn still winding down under load is waited for)
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The finished item closes once its runtime turn has stopped; {}",
            lifecycle(&e, &iid).await
        )
    });
    eventually(diagnostic, || async {
        lifecycle(&e, &iid).await["phase"] == "closed"
    })
    .await;
    let l = lifecycle(&e, &iid).await;
    assert_eq!(
        (l["phase"].as_str(), l["outcome"].as_str()),
        (Some("closed"), Some("cancelled")),
        "{l}"
    );
    let (s, v) = act(&e, &iid, "finish").await;
    assert_eq!(s, 409, "a dropped item can't be marked done: {v}");

    // reopen is a new cycle with a goal from the same rules; the earlier cycles stay as they closed
    let (s, v) = command(
        &e,
        &iid,
        json!({"command": "new_cycle", "expected_cycle": 2, "expected_revision": 0, "goal": "再做一次"}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    eventually(
        "The adapter receives the request for another work cycle.",
        || async { e.prompts().iter().any(|p| p.contains("再做一次")) },
    )
    .await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The next work cycle returns the item to its active phase; {}",
            lifecycle(&e, &iid).await
        )
    });
    eventually(diagnostic, || async {
        lifecycle(&e, &iid).await["phase"] == "active"
    })
    .await;
    let (_, v) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    let cycles: Vec<(i64, &str)> = v["cycles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["cycle"].as_i64().unwrap(), c["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(cycles, [(1, "completed"), (2, "cancelled")]);

    workspace_is_named_never_guessed(&e).await;
    an_item_keeps_exactly_one_category(&e).await;
    optional_delivery_names_do_not_block_creation(&e).await;
}

async fn optional_delivery_names_do_not_block_creation(e: &Env) {
    let (_, state) = e.call("GET", "/api/state", None).await;
    let report = state["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "报告" && t["cat"] == "交付")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let (status, body) = e.call("DELETE", &format!("/api/tags/{report}"), None).await;
    assert_eq!(status, 200, "{body}");
    let request = json!({
        "title":"调研报告", "goal":"分析结论", "bot":state["bots"][0]["id"],
        "project":null, "mode":"propose"
    });
    let (status, body) = e.call("POST", "/api/items", Some(request.clone())).await;
    assert_eq!(status, 200, "{body}");
    let (_, state) = e.call("GET", "/api/state", None).await;
    assert!(
        !state["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "报告"),
        "task creation must not resurrect the deleted tag"
    );
    let (status, ordinary) = e
        .call(
            "POST",
            "/api/tags",
            Some(json!({
                "name":"报告","cat":"领域","description":"user report"
            })),
        )
        .await;
    assert_eq!(status, 200, "{ordinary}");
    assert_eq!(ordinary["cat"], "领域");
    assert_ne!(ordinary["id"], report);
    let (status, body) = e.call("POST", "/api/items", Some(request)).await;
    assert_eq!(status, 200, "{body}");
    let (_, item) = e
        .call(
            "GET",
            &format!("/api/items/{}", body["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert!(!item["tags"].as_array().unwrap().contains(&ordinary["id"]));
    let (_, state) = e.call("GET", "/api/state", None).await;
    let kept = state["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == ordinary["id"])
        .unwrap();
    assert_eq!(kept["cat"], "领域");
    assert_eq!(kept["description"], "user report");
}

/// ST-34 over HTTP: an item made through the API has one category; no write leaves it with none or two;
/// the categories themselves cannot be deleted, merged or joined.
async fn an_item_keeps_exactly_one_category(e: &Env) {
    let (_, state) = e.call("GET", "/api/state", None).await;
    let tags = state["tags"].as_array().expect("tags").clone();
    let kind = |n: &str| {
        tags.iter()
            .find(|t| t["cat"] == "类型" && t["name"] == n)
            .and_then(|t| t["id"].as_str())
            .unwrap_or_else(|| panic!("no category {n} in {tags:?}"))
            .to_string()
    };
    let (bug, feat) = (kind("bug"), kind("feat"));
    assert_eq!(tags.iter().filter(|t| t["cat"] == "类型").count(), 6);
    let (s, web) = e
        .call(
            "POST",
            "/api/tags",
            Some(json!({"name": "前端", "cat": "领域", "description": "界面"})),
        )
        .await;
    assert_eq!(s, 200, "{web}");
    let web = web["id"].as_str().unwrap().to_string();
    let bot = state["bots"][0]["id"].as_str().unwrap().to_string();
    let (s, i) = e
        .call("POST", "/api/items", Some(json!({"goal": "做一个新页面", "title": "新页面", "bot": bot, "project": null, "mode": "propose"})))
        .await;
    assert_eq!(s, 200, "{i}");
    let iid = i["id"].as_str().unwrap().to_string();
    let tags_of = |v: &Value| -> Vec<String> {
        v["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect()
    };
    let (_, v) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(
        tags_of(&v),
        std::slice::from_ref(&feat),
        "Steve gave it one: {v}"
    );

    let path = format!("/api/items/{iid}");
    for (bad, why) in [
        (json!([]), "必须"),
        (json!([web]), "必须"),
        (json!([bug, feat, web]), "只能有一个"),
    ] {
        let (s, v) = e
            .call("PATCH", &path, Some(json!({"tags": bad, "title": "改了"})))
            .await;
        assert_eq!(s, 409, "{bad} → {v}");
        assert!(v["error"].as_str().unwrap().contains(why), "{v}");
    }
    let (_, v) = e.call("GET", &path, None).await;
    assert_eq!(
        (tags_of(&v), v["title"].as_str()),
        (vec![feat.clone()], Some("新页面")),
        "nothing written"
    );
    // switching is one write, the category first
    let (s, v) = e
        .call("PATCH", &path, Some(json!({"tags": [web, bug]})))
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, v) = e.call("GET", &path, None).await;
    assert_eq!(tags_of(&v), [bug.clone(), web.clone()]);
    let texts: Vec<&str> = v["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["text"].as_str())
        .collect();
    assert!(
        texts.contains(&"分类改为 bug（原来是 feat）") && texts.contains(&"添加标签：前端"),
        "{texts:?}"
    );

    let (s, _) = e.call("DELETE", &format!("/api/tags/{bug}"), None).await;
    assert_eq!(s, 409);
    let (s, _) = e
        .call(
            "POST",
            &format!("/api/tags/{web}/merge"),
            Some(json!({"into": bug})),
        )
        .await;
    assert_eq!(s, 409);
    let (s, _) = e
        .call(
            "PATCH",
            &format!("/api/tags/{bug}"),
            Some(json!({"name": "fix"})),
        )
        .await;
    assert_eq!(s, 409);
    let (s, _) = e
        .call(
            "POST",
            "/api/tags",
            Some(json!({"name": "需求", "cat": "类型", "description": "x"})),
        )
        .await;
    assert_eq!(s, 409);

    // The one-time compatibility API and its review list are gone.
    for (method, path) in [
        ("POST", "/api/tags/kinds/backfill"),
        ("GET", "/api/tags/kinds/review"),
        ("POST", "/api/tags/kinds/review"),
    ] {
        let missing = e
            .call(method, "/api/__missing_kind_workflow__", Some(json!({})))
            .await;
        let removed = e.call(method, path, Some(json!({}))).await;
        assert_eq!(
            removed, missing,
            "{method} {path} must use the ordinary missing-route fallback"
        );
    }
}

/// The page says which workspace a request is for (`x-steve-ws`). A workspace the user is not a member of
/// is refused, never swapped for another; a browser write without it (a page loaded before) is refused
/// with "refresh"; a script without the header keeps the cookie's workspace.
async fn workspace_is_named_never_guessed(e: &Env) {
    let send = |method: &'static str, headers: Vec<(&'static str, String)>| {
        let http = e.http.clone();
        let path = if method == "GET" {
            "/api/me"
        } else {
            "/api/tags"
        };
        let (url, cookie) = (format!("{}{path}", e.base), e.cookie.clone());
        async move {
            let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
            let mut r = http.request(m, url).header("cookie", cookie);
            if method == "POST" {
                r = r.json(&json!({"name": format!("t{}", rand_suffix()), "cat": "主题", "description": "测试"}));
            }
            for (k, v) in headers {
                r = r.header(k, v);
            }
            let res = r.send().await.expect("request");
            let s = res.status().as_u16();
            (s, res.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (_, me) = e.call("GET", "/api/me", None).await;
    let ws = me["workspace"]["id"]
        .as_str()
        .or_else(|| me["ws"].as_str())
        .unwrap_or("default")
        .to_string();
    // named and a member: fine, read or write
    let (s, v) = send("GET", vec![("x-steve-ws", ws.clone())]).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = send(
        "POST",
        vec![
            ("x-steve-ws", ws.clone()),
            ("sec-fetch-site", "same-origin".into()),
        ],
    )
    .await;
    assert_eq!(s, 200, "{v}");
    // a workspace that is not theirs: refused, not another one
    let (s, v) = send("GET", vec![("x-steve-ws", "ws_ffffffffffffffff".into())]).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("这个页面所在的空间已经不可用了，请刷新页面")),
        "{v}"
    );
    // a browser page that doesn't say: may read, may not write
    let (s, _) = send("GET", vec![("sec-fetch-site", "same-origin".into())]).await;
    assert_eq!(s, 200);
    let (s, v) = send("POST", vec![("sec-fetch-site", "same-origin".into())]).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("页面需要刷新后才能继续操作")),
        "{v}"
    );
    // what can't send a header (an EventSource, an <img>) names it as ?ws=, under the same rule
    let (s, _) = e.call("GET", &format!("/api/me?ws={ws}"), None).await;
    assert_eq!(s, 200);
    let (s, v) = e.call("GET", "/api/me?ws=ws_ffffffffffffffff", None).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("这个页面所在的空间已经不可用了，请刷新页面")),
        "{v}"
    );
    // a script (no Fetch Metadata) keeps the cookie's workspace
    let (s, v) = send("POST", vec![]).await;
    assert_eq!(s, 200, "{v}");
}

fn rand_suffix() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000
    )
}

/// A project's completion rule (`done_when`) is set through the API: only that part of its release
/// settings (the command stays as set up), only to `merge` or `tag` (or cleared), and only where a
/// release is set up at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_shadow_state_never_breaks_the_item_page() {
    let e = start().await;
    let sid = item(&e, "慢慢做 @sleep 20000").await;
    let iid = item_of(&e, &sid).await;
    let (s, v) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(s, 200, "{v}");
    assert!(
        v.get("shadow").is_some(),
        "the shadow state rides along: {v}"
    );
    // the shadow state cannot be computed (a table it reads is gone): the page leaves it out
    e.store
        .with_conn(|c| {
            c.execute_batch("DROP TABLE work_policy")?;
            Ok(())
        })
        .expect("drop");
    let (s, v) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(s, 200, "the item page stays up: {v}");
    assert!(v.get("shadow").is_none(), "{v}");
    assert_eq!(v["id"], iid.as_str(), "{v}");
    assert!(v["lifecycle"].is_object(), "{v}");
}

#[tokio::test]
async fn a_projects_completion_rule_is_set_through_the_api() {
    let e = start().await;
    let dir = e._home.path().join("proj-done-when");
    std::fs::create_dir_all(&dir).unwrap();
    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "rel", "path": dir})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let pid = p["id"].as_str().unwrap().to_string();
    let path = format!("/api/projects/{pid}");
    let patch = |v: Value| e.call("PATCH", &path, Some(v));
    // no release set up: nothing to say when it is finished
    let (s, v) = patch(json!({"done_when": "tag"})).await;
    assert_eq!(s, 400, "{v}");
    let store = e.store.clone();
    store
        .projects()
        .patch(
            &pid,
            &steve_store::ProjectPatch {
                release: Some(Some(steve_store::Release {
                    kind: "script".into(),
                    cmd: "/bin/true".into(),
                    done_when: None,
                })),
                ..Default::default()
            },
            "test",
        )
        .unwrap();
    let (s, v) = patch(json!({"done_when": "tag"})).await;
    assert_eq!(s, 200, "{v}");
    let rel = store
        .projects()
        .get(&pid)
        .unwrap()
        .unwrap()
        .release
        .unwrap();
    assert_eq!(
        (rel.cmd.as_str(), rel.done_when.as_deref()),
        ("/bin/true", Some("tag"))
    );
    let (s, _) = patch(json!({"done_when": "deployed"})).await;
    assert_eq!(s, 400, "only merge or tag");
    let (s, _) = patch(json!({"done_when": null})).await;
    assert_eq!(s, 200);
    let rel = store
        .projects()
        .get(&pid)
        .unwrap()
        .unwrap()
        .release
        .unwrap();
    assert_eq!((rel.cmd.as_str(), rel.done_when), ("/bin/true", None));
}

#[tokio::test]
async fn git_project_without_script_can_configure_done_when_on_create_and_patch() {
    let e = start().await;
    let dir = e._home.path().join("git-done-when");
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    let (s, p) = e
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "git-rel", "path": dir, "done_when": "merge"})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["release"]["done_when"], "merge");
    let pid = p["id"].as_str().unwrap().to_string();
    let path = format!("/api/projects/{pid}");

    // Clearing done_when when cmd is empty resets release to null (local acceptance only).
    let (s, v) = e.call("PATCH", &path, Some(json!({"release": null}))).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["release"].is_null(), "{v}");

    // Setting done_when via either `release` object or `done_when` works even when release was null.
    let (s, v) = e
        .call(
            "PATCH",
            &path,
            Some(json!({"release": {"kind": "script", "cmd": "", "done_when": "tag"}})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["release"]["done_when"], "tag");

    let (s, v) = e
        .call("PATCH", &path, Some(json!({"done_when": null})))
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["release"].is_null(), "{v}");
}

/// Q1: the design gate is off on a new item; only the instance's owner switches it, through its own
/// route. The item's general patch does not carry it, and a member is refused.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_owner_switches_the_design_gate() {
    let e = start().await;
    let sid = item(&e, "改进页面布局").await;
    let iid = item_of(&e, &sid).await;
    let gate = |v: &Value| v["design_gate"].clone();
    let (_, i) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(gate(&i), json!(false), "{i}");
    let (s, _) = e
        .call(
            "PATCH",
            &format!("/api/items/{iid}"),
            Some(json!({"design_gate": true})),
        )
        .await;
    assert_eq!(s, 200);
    let (_, i) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(
        gate(&i),
        json!(false),
        "the general patch does not switch it"
    );
    for body in [json!({}), json!({"on": "yes"})] {
        let (s, _) = e
            .call("PUT", &format!("/api/items/{iid}/design-gate"), Some(body))
            .await;
        assert_eq!(s, 400);
    }
    let (s, v) = e
        .call(
            "PUT",
            &format!("/api/items/{iid}/design-gate"),
            Some(json!({"on": true})),
        )
        .await;
    assert_eq!((s, gate(&v)), (200, json!(true)), "{v}");
    let (_, i) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert!(
        i["timeline"]
            .as_array()
            .is_some_and(|t| t.iter().any(|x| x["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("开启设计确认")))),
        "{i}"
    );
    let store = e.store.clone();
    let member = store
        .users()
        .create("design-gate-member", "Member", None, "member")
        .unwrap();
    let token = "design-gate-member-token";
    store
        .auth_sessions()
        .create(&member, token, 3_600_000)
        .unwrap();
    let r = e
        .http
        .put(format!("{}/api/items/{iid}/design-gate", e.base))
        .header("cookie", format!("steve_session={token}"))
        .json(&json!({"on": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    assert!(
        store.items().get(&iid).unwrap().unwrap().design_gate,
        "a member's request changed nothing"
    );
}

#[tokio::test]
async fn project_setup_requires_owner_and_a_current_preview_and_registers_after_creation() {
    let e = start().await;
    let parent = e._home.path().canonicalize().unwrap();
    let mut v = json!({
        "node": "hub", "mode": "empty", "name": "new project",
        "parent": parent, "directory": "fresh", "preview": "not-the-destination",
    });
    for route in ["/api/projects/setup/preview", "/api/projects/setup"] {
        let anon = e
            .http
            .post(format!("{}{route}", e.base))
            .json(&v)
            .send()
            .await
            .unwrap();
        assert_eq!(anon.status(), 401);
    }
    let store = e.store.clone();
    let member = store
        .users()
        .create("setup-member", "Member", None, "member")
        .unwrap();
    let member_token = "project-setup-member-token";
    store
        .auth_sessions()
        .create(&member, member_token, 3_600_000)
        .unwrap();
    for route in ["/api/projects/setup/preview", "/api/projects/setup"] {
        let refused = e
            .http
            .post(format!("{}{route}", e.base))
            .header("cookie", format!("steve_session={member_token}"))
            .json(&v)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 403);
    }
    let (s, preview) = e
        .call("POST", "/api/projects/setup/preview", Some(v.clone()))
        .await;
    let target = parent.join("fresh");
    assert_eq!(s, 200, "{preview}");
    assert_eq!(preview["path"], json!(target));
    assert!(!target.exists());
    let (s, _) = e.call("POST", "/api/projects/setup", Some(v.clone())).await;
    assert_eq!(s, 409);
    assert!(!target.exists());
    v["preview"] = preview["path"].clone();
    v["node"] = json!("remote");
    let (s, _) = e.call("POST", "/api/projects/setup", Some(v.clone())).await;
    assert_eq!(s, 400);
    assert!(!target.exists());
    v["node"] = json!("hub");
    let (s, answer) = e.call("POST", "/api/projects/setup", Some(v.clone())).await;
    assert_eq!(s, 200, "{answer}");
    let answer: Value = serde_json::from_str(
        answer
            .as_str()
            .unwrap()
            .strip_prefix("data: ")
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(answer["project"]["kind"], "dir");
    assert_eq!(answer["project"]["path"], json!(target));
    assert_eq!(target.read_dir().unwrap().count(), 0);
    let (_, state) = e.call("GET", "/api/state", None).await;
    assert!(
        state["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == &answer["project"])
    );
    let (s, _) = e.call("POST", "/api/projects/setup", Some(v)).await;
    assert_eq!(s, 409, "retry must not overwrite or register again");
}

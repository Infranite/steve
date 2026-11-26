//! The colocated local Node (`steve-node --local-node`), end to end on the real `steve-hub` binary: a temp
//! STEVE_HOME, its own port, the fake ACP agent (`steve-acp/tests/fake-acp.js`, which logs every
//! message it gets), real HTTP. Covers D5's acceptance on one Hub home:
//! - a session created with the Node on is bound to it and its transcript is stored as `steve.v1`;
//! - a permission request becomes a card, its answer reaches the agent;
//! - a cancel ends the running turn;
//! - while held (upgrade drain), a message is queued and not drained, and goes once released;
//! - the Hub killed mid-turn comes back with the attempt `unknown` (awaiting reconciliation),
//!   which is never re-run;
//! - on a diagnostic Hub a Node-bound session is parked, and no adapter starts;
//! - with `STEVE_NODE_MODE` unset the Node is on (the default); a `STEVE_NO_RESUME` copy boots
//!   with it off instead, and its Node-bound sessions are parked.
//!
//! - B7: a new session no Node takes is refused visibly, never run on the Hub.
//!
//! And on a second home, with a short attempt deadline, a card nobody answers expires; on a
//! third, an idle Node session goes through two Hub restarts without a resume or a restart notice.
//! With a short idle limit, an idle Node session's agent exits and the next message resumes it.
//!
//! No real host, no real agent, never `~/.steve` or the production port.

use std::fs::{File, TryLockError};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use steve_runtime::testkit::eventually;
use steve_runtime::testkit::eventually_sync;
use steve_runtime::testkit::process::{Command, GroupGuard, ProcessGuard};
use steve_store::testkit::hub::node_process;

use serde_json::{Value, json};

mod handed;
#[path = "support/late_spawn.rs"]
mod late_spawn;

const VERSION: &str = "0.9.0";

fn log_lines(log: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

struct Home {
    // A fixture owns its detached Node across Hub launches. Retire it before TempDir is dropped.
    service: std::sync::OnceLock<ServiceNodeGuard>,
    dir: tempfile::TempDir,
    address: std::sync::Mutex<Option<SocketAddr>>,
    /// The service Node's loopback port: its own socket, not the Hub's port + 3, which on a host
    /// handing out ports in order is often another test's Hub.
    local: handed::Socket,
    logs: std::sync::Mutex<Vec<PathBuf>>,
    web: PathBuf,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::Builder::new()
            .prefix("steve-node-e2e-")
            .tempdir()
            .expect("tmp");
        let web = dir.path().join("web");
        std::fs::create_dir_all(&web).expect("web");
        std::fs::write(web.join("index.html"), "<html><head></head></html>").expect("index");
        std::fs::write(
            web.join("release.json"),
            json!({"version": VERSION}).to_string(),
        )
        .expect("web version");
        Home {
            service: Default::default(),
            address: Default::default(),
            local: handed::Socket::new(),
            logs: std::sync::Mutex::new(vec![]),
            web,
            dir,
        }
    }
    fn steve(&self) -> PathBuf {
        self.dir.path().join("steve")
    }
    fn service_executable(&self) -> &std::path::Path {
        &self
            .service
            .get_or_init(|| {
                ServiceNodeGuard::new(self.dir.path(), self.steve())
                    .expect("fixture service executable")
            })
            .executable
    }
    fn base(&self) -> String {
        format!(
            "http://{}",
            self.address.lock().unwrap().expect("Hub is serving")
        )
    }
    /// An immutable log destination inherited by adapters from exactly one Hub launch.
    fn adapter_log(&self) -> PathBuf {
        let mut logs = self.logs.lock().unwrap();
        let path = self.dir.path().join(format!("acp-{}.log", logs.len()));
        logs.push(path.clone());
        path
    }
    fn lines(&self) -> Vec<Value> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .flat_map(|p| log_lines(p))
            .collect()
    }
    /// Prompt texts across all launches, in launch/log order (including late old records).
    fn prompts(&self) -> Vec<String> {
        self.lines()
            .iter()
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
    fn got(&self, token: &str) -> usize {
        self.prompts().iter().filter(|p| p.contains(token)).count()
    }
    /// Adapter processes started so far (the fake logs one `spawn` line each).
    fn spawns(&self) -> usize {
        self.lines()
            .iter()
            .filter(|m| m.get("spawn").is_some())
            .count()
    }
    /// The test's own database (in the temp home), read-only.
    fn db_path(&self) -> std::path::PathBuf {
        self.steve().join("data/steve.sqlite3")
    }
    /// Stage an already-due command in this isolated fixture. Deadline calculation and restart
    /// recovery are exercised with an injected clock in node_runtime; this subprocess test keeps
    /// the real adapter/Node/Hub/client path without a production clock override.
    fn make_retry_due(&self, session: &str) {
        let changed = node_process::make_unsent_turn_retry_due(&self.db_path(), session).unwrap();
        assert_eq!(changed, 1, "exactly the fixture's unsent retry becomes due");
    }
}

/// A running Hub process and the adapter log owned by this launch.
struct Hub {
    child: Option<ProcessGuard>,
    adapter_log: PathBuf,
    service: bool,
}

impl Hub {
    fn spawns(&self) -> usize {
        log_lines(&self.adapter_log)
            .iter()
            .filter(|m| m.get("spawn").is_some())
            .count()
    }
    /// SIGKILL the Hub and observed direct agents, then reap the Hub. For detached service-mode
    /// Hubs, kill only the Hub's group so `ServiceNodeGuard` retires the detached Node cleanly.
    fn kill(&mut self) {
        if self.service {
            self.kill_hub_group();
        } else if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    /// Simulate a service manager restart, leaving the independent Node running.
    fn kill_hub_group(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.simulate_crash();
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A runnable Hub with its independent colocated service Node, or an inert diagnostic Hub.
fn spawn(h: &Home, node: bool, extra: &[(&str, &str)]) -> Hub {
    if node {
        spawn_mode(h, None, extra)
    } else {
        let mut diagnostic = vec![("STEVE_NO_RESUME", "1")];
        diagnostic.extend_from_slice(extra);
        spawn_mode(h, None, &diagnostic)
    }
}

/// The Hub with `STEVE_NODE_MODE` set to `mode`, or unset (the default since B6) for `None`.
fn spawn_mode(h: &Home, mode: Option<&str>, extra: &[(&str, &str)]) -> Hub {
    let adapter_log = h.adapter_log();
    let fake = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../steve-acp/tests/fake-acp.js"
    );
    *h.address.lock().unwrap() = None;
    let ready = h.steve().join("run/hub-ready.json");
    match std::fs::remove_file(ready) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        result => result.expect("clear the previous test Hub's readiness"),
    }
    // tracing_subscriber writes to stdout by default. Capture both streams so a failed end-to-end
    // assertion retains the Hub's event/permission errors instead of showing an empty log.
    let log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(adapter_log.with_extension("hub.log"))
        .expect("hub log");
    let service = !extra.contains(&("STEVE_NO_RESUME", "1"));
    let executable = if service {
        h.service_executable()
    } else {
        std::path::Path::new(env!("CARGO_BIN_EXE_steve-hub"))
    };
    let mut c = handed::command(executable, &[("local-node", &h.local)]);
    c.env("HOME", h.dir.path())
        .env("STEVE_HOME", h.steve())
        .env("STEVE_PORT", "0")
        .env_remove("STEVE_WEB_PORT")
        .env_remove("STEVE_LOCAL_NODE_PORT")
        .env("STEVE_HOST", "127.0.0.1")
        .env_remove("STEVE_PUBLIC_URL")
        .env_remove("STEVE_PUBLIC_URLS")
        .env("STEVE_DEV_LOGIN", "1")
        .env("STEVE_REPO", "/nonexistent")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_VERSION", VERSION)
        .env("STEVE_WEB_DIR", &h.web)
        .env("FAKE_ACP_LOG", &adapter_log)
        .env("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CLAUDE", fake)
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env_remove("STEVE_NODE_MODE")
        .env_remove("STEVE_NODE")
        .env_remove("STEVE_NODE_TURN_LIMIT_MS")
        .env_remove("STEVE_IDLE_RELEASE_MS")
        .env_remove("STEVE_NO_RESUME")
        .env_remove("INVOCATION_ID")
        .env_remove("STEVE_UPGRADE_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("STEVE_KB_STORE")
        .env_remove("STEVE_OPENVIKING_KEY_FILE")
        .env_remove("STEVE_OPENVIKING_URL")
        .env_remove("STEVE_OPENVIKING_ACCOUNT")
        .env_remove("STEVE_OIDC_ISSUER")
        .stdout(log.try_clone().expect("hub stdout log"))
        .stderr(log)
        .process_group(0);
    if let Some(mode) = mode {
        c.env("STEVE_NODE_MODE", mode);
    }
    for (k, v) in extra {
        c.env(k, v);
    }
    Hub {
        child: Some(c.spawn().expect("spawn hub")),
        adapter_log,
        service,
    }
}

#[derive(serde::Deserialize)]
struct Ready {
    hub: Vec<SocketAddr>,
}

/// Up and serving the full app.
async fn serving(h: &Home) {
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!("the Hub serves; log:\n{}", hub_log(h))
    });
    let path = h.steve().join("run/hub-ready.json");
    eventually(diagnostic, || async {
        let Some(addr) = std::fs::read(&path).ok().and_then(|bytes| {
            let ready: Ready = serde_json::from_slice(&bytes).ok()?;
            ready.hub.first().copied()
        }) else {
            return false;
        };
        let Ok(r) = reqwest::Client::new()
            .get(format!("http://{addr}/healthz"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        else {
            return false;
        };
        let ready = r
            .json::<Value>()
            .await
            .is_ok_and(|v| v["ok"] == true && v["maintenance"] != true);
        if ready {
            *h.address.lock().unwrap() = Some(addr);
        }
        ready
    })
    .await;
}

/// `/healthz`'s `node` block.
async fn node_health(h: &Home) -> Value {
    reqwest::Client::new()
        .get(format!("{}/healthz", h.base()))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("healthz")
        .json::<Value>()
        .await
        .expect("healthz json")["node"]
        .clone()
}

/// Observe the health block that satisfies the expected Node state.
async fn wait_node_health(h: &Home, ok: impl Fn(&Value) -> bool) -> Value {
    let observed = std::sync::Mutex::new(None);
    eventually(
        "The Node health report reaches the state required by this transition.",
        || async {
            let value = node_health(h).await;
            let ready = ok(&value);
            *observed.lock().unwrap() = Some(value);
            ready
        },
    )
    .await;
    observed.into_inner().unwrap().unwrap()
}

fn hub_log_path(h: &Home) -> PathBuf {
    h.logs
        .lock()
        .unwrap()
        .last()
        .expect("Hub launched")
        .with_extension("hub.log")
}

fn hub_log(h: &Home) -> String {
    let s = std::fs::read_to_string(hub_log_path(h)).unwrap_or_default();
    s.lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

struct Client {
    http: reqwest::Client,
    base: String,
    cookie: String,
}

impl Client {
    async fn login(h: &Home) -> Client {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let r = http
            .get(format!("{}/auth/dev", h.base()))
            .send()
            .await
            .expect("dev login");
        let cookie = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("steve_session_"))
            .map(|c| c.split(';').next().unwrap_or_default().to_string())
            .expect("session cookie");
        Client {
            http,
            base: h.base(),
            cookie,
        }
    }
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .http
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).expect("method"),
                format!("{}{path}", self.base),
            )
            .header("cookie", &self.cookie);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.expect("request");
        let st = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            st,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    async fn say(&self, sid: &str, text: &str) -> Value {
        let (st, v) = self
            .call(
                "POST",
                &format!("/api/sessions/{sid}/messages"),
                Some(json!({"text": text})),
            )
            .await;
        assert_eq!(st, 200, "message: {v}");
        v
    }
    /// A new item on the claude-code bot, and the session it opened.
    async fn item(&self, h: &Home, goal: &str) -> String {
        let (_, state) = self.call("GET", "/api/shell", None).await;
        let bot = state["bots"]
            .as_array()
            .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
            .and_then(|b| b["id"].as_str())
            .expect("a claude-code bot")
            .to_string();
        self.item_for_bot(h, goal, &bot).await
    }

    /// These cases cover retry orchestration, not transcript settlement. Pi's native accounting
    /// has no transcript tail window; the very same scripted ACP adapter supplies the failures.
    async fn retry_item(&self, h: &Home, goal: &str) -> String {
        // Finish discovery before admission. Previously these tests passed only if item creation
        // beat the probe: the generic fake lacked the HTTP MCP capability Pi requires, so a
        // completed probe refused the Bot and left a fail card without ever creating a session.
        let (st, probe) = self
            .call("GET", "/api/system/models?harness=pi&fresh=1", None)
            .await;
        assert_eq!(st, 200, "retry adapter discovery: {probe}; {}", hub_log(h));
        let (st, bot) = self
            .call(
                "POST",
                "/api/bots",
                Some(json!({
                    "name": "Retry fixture", "harness": "pi", "perm_mode": "full_access"
                })),
            )
            .await;
        assert_eq!(st, 200, "{bot}");
        self.item_for_bot(h, goal, bot["id"].as_str().expect("retry bot"))
            .await
    }

    async fn item_for_bot(&self, h: &Home, goal: &str, bot: &str) -> String {
        let (st, i) = self
            .call(
                "POST",
                "/api/items",
                Some(json!({"goal": goal, "title": "Node 验收", "bot": bot, "project": null})),
            )
            .await;
        assert_eq!(st, 200, "{i}");
        let item = i["id"].as_str().expect("item").to_string();
        // Creation starts the session synchronously, or returns the item with a failure card.
        // Waiting cannot turn a refused start into a session; report the actual blocker now.
        node_process::session_id_by_item(&h.db_path(), &item)
            .unwrap()
            .unwrap_or_else(|| {
                panic!(
                    "item creation returned without a session: item={i}; failure={:?}; log:\n{}",
                    node_process::decision_question_by_item_fail_open(&h.db_path(), &item).unwrap(),
                    hub_log(h),
                )
            })
    }
}

/// The supervisor's control token (`updates/control-token.json`, a JSON string).
fn control_token(h: &Home) -> String {
    let t = "e2e-control-token-0123456789abcdef0123456789".to_string();
    let p = h.steve().join("updates/control-token.json");
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
    std::fs::write(&p, serde_json::to_vec(&json!(t)).expect("json")).expect("token");
    t
}

async fn drain(h: &Home, method: &str, token: &str) -> Value {
    let r = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).expect("method"),
            format!("{}/internal/upgrade/drain", h.base()),
        )
        .bearer_auth(token)
        .send()
        .await
        .expect("drain");
    assert_eq!(r.status().as_u16(), 200, "drain {method}");
    r.json().await.expect("json")
}

fn steve_v1(h: &Home, sid: &str, needle: &str) -> i64 {
    node_process::agent_output_count(&h.db_path(), sid, needle).unwrap()
}

/// The open permission card of `sid`, if any.
fn open_card(h: &Home, sid: &str) -> Option<String> {
    node_process::decision_id_by_session_tool_call_open(&h.db_path(), sid).unwrap()
}

/// Attempts of `sid` in `state`.
fn attempts(h: &Home, sid: &str, state: &str) -> i64 {
    node_process::attempt_count(&h.db_path(), sid, state).unwrap()
}

/// Each running Hub reports the port it bound itself.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_hubs_report_their_os_assigned_ports() {
    let first = Home::new();
    let second = Home::new();
    let local = first.local.port();
    let _first = spawn(&first, false, &[("STEVE_HOST", "127.0.0.1,::1")]);
    let _second = spawn(&second, false, &[]);
    tokio::join!(serving(&first), serving(&second));
    assert_ne!(first.base(), second.base());
    let ready: Ready =
        serde_json::from_slice(&std::fs::read(first.steve().join("run/hub-ready.json")).unwrap())
            .unwrap();
    let [ipv4, ipv6] = ready.hub.as_slice() else {
        panic!("expected one listener for each configured address");
    };
    assert!(ipv4.is_ipv4() && ipv6.is_ipv6());
    assert_eq!(ipv4.port(), ipv6.port());
    assert_ne!(ipv4.port(), local);
    assert_ne!(ipv4.port(), 8440);
    let health = reqwest::get(format!("http://{ipv6}/healthz"))
        .await
        .unwrap();
    assert!(health.status().is_success());
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_refusals_share_one_turn_and_finish_with_one_service_card() {
    let h = Home::new();
    let _hub = spawn(
        &h,
        true,
        &[(
            "STEVE_ADAPTER_PI",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/retry-acp.mjs"),
        )],
    );
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c
        .retry_item(
            &h,
            "bounded provider retries @fail 4 HTTP 503 Service Unavailable",
        )
        .await;
    let item = node_process::session_item_id_by_id(&h.db_path(), &sid)
        .unwrap()
        .unwrap();
    let retries = || node_process::attempt_count_by_session(&h.db_path(), &sid).unwrap();
    let cards = || node_process::decision_count_by_item_fail_open(&h.db_path(), &item).unwrap();
    for attempts in 2..=4 {
        let waiting = steve_runtime::testkit::on_timeout(|| async {
            format!("attempt {attempts} is persisted; {}", hub_log(&h))
        });
        eventually_sync(waiting, || retries() == attempts).await;
        assert_eq!(cards(), 0);
        h.make_retry_due(&sid);
    }
    let settled = steve_runtime::testkit::on_timeout(|| async {
        format!("one service failure card; {}", hub_log(&h))
    });
    eventually_sync(settled, || cards() == 1).await;
    assert_eq!(retries(), 4);
    assert_eq!(
        node_process::item_takeover_notice_count(&h.db_path(), &item).unwrap(),
        0
    );
    let prompts: Vec<_> = h
        .prompts()
        .into_iter()
        .filter(|p| p.contains("@fail 4"))
        .collect();
    assert_eq!(prompts.len(), 4);
    assert!(prompts.windows(2).all(|ps| ps[0] == ps[1]));
    let question = node_process::decision_question_by_item_fail_open(&h.db_path(), &item)
        .unwrap()
        .unwrap();
    let (status, state) = c.call("GET", "/api/snapshot?kinds=decision", None).await;
    assert_eq!(status, 200);
    assert!(
        state["views"]["decision"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["question"]["text"] == question)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_session_end_to_end() {
    let h = Home::new();
    let token = control_token(&h);
    let mut hub = spawn(&h, true, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let n = wait_node_health(&h, |n| n["reconciled"] == true).await;
    assert_eq!(
        (&n["enabled"], &n["reconciled"], &n["stopped"]),
        (&json!(true), &json!(true), &json!(false)),
        "healthz after boot: {n}"
    );

    // ── a new session binds to the Node; its transcript is steve.v1
    let sid = c.item(&h, "节点验收 @say hello-node-7Q").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session stores the expected reply text; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "hello-node-7Q") > 0).await;
    assert_eq!(
        node_process::binding_count_by_session_local(&h.db_path(), &sid).unwrap(),
        1,
        "bound to the local Node"
    );
    eventually_sync("The session has a finished runtime attempt.", || {
        attempts(&h, &sid, "finished") >= 1
    })
    .await;
    assert_eq!(
        node_process::event_count_by_session_not_finished(&h.db_path(), &sid).unwrap(),
        0,
        "nothing in another format"
    );

    // ── a permission request becomes a card; its answer reaches the agent
    c.say(&sid, "@perm Bash").await;
    let mut card = None;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session has an open tool-call permission card; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        card = open_card(&h, &sid);
        card.is_some()
    })
    .await;
    let card = card.expect("card");
    let (st, v) = c
        .call(
            "POST",
            &format!("/api/decisions/{card}"),
            Some(json!({"choice": "allow_once"})),
        )
        .await;
    assert_eq!(st, 200, "answer: {v}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session transcript contains the selected permission response; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "perm:selected:") > 0).await;
    assert_eq!(
        node_process::decision_status_by_id(&h.db_path(), &card)
            .unwrap()
            .as_deref(),
        Some("resolved")
    );

    // ── a cancel ends the running turn
    c.say(&sid, "取消验收-C4 @sleep 600000").await;
    eventually_sync(
        "The fake adapter records the target prompt exactly once.",
        || h.got("取消验收-C4") == 1,
    )
    .await;
    let (st, v) = c
        .call("POST", &format!("/api/sessions/{sid}/cancel"), None)
        .await;
    assert_eq!((st, &v["ok"]), (200, &json!(true)), "cancel: {v}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The cancelled session has no unfinished runtime turns; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        node_process::runtime_turns_count_by_session_not_finished(&h.db_path(), &sid).unwrap() == 0
    })
    .await;
    assert!(
        h.lines().iter().any(|m| m["method"] == "session/cancel"),
        "the agent was told to cancel"
    );
    eventually_sync(
        "The conversation records that the turn ended as cancelled.",
        || steve_v1(&h, &sid, "cancelled") > 0,
    )
    .await;

    // ── while held, a queued message is not drained; it goes once released
    let d = drain(&h, "POST", &token).await;
    assert_eq!(d["maintenance"], true, "{d}");
    let r = c.say(&sid, "排队验收-H8 @say held-done").await;
    assert_eq!(r["result"], "queued", "{r}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.got("排队验收-H8"), 0, "not drained while held");
    let (_, q) = c
        .call("GET", &format!("/api/sessions/{sid}/queued"), None)
        .await;
    assert!(
        q.as_array().is_some_and(|q| q.iter().any(|m| m["text"]
            .as_str()
            .is_some_and(|t| t.contains("排队验收-H8")))),
        "still queued: {q}"
    );
    let d = drain(&h, "DELETE", &token).await;
    assert_eq!(d["maintenance"], false, "{d}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The held prompt is sent after the session is released; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || h.got("排队验收-H8") == 1).await;
    eventually_sync(
        "The reply from the released held session is stored.",
        || steve_v1(&h, &sid, "held-done") > 0,
    )
    .await;

    // ── the Hub killed mid-turn: the original attempt stays unknown until it can be settled
    c.say(&sid, "崩溃验收-K2 @sleep 600000").await;
    eventually_sync(
        "The fake adapter records the target prompt exactly once.",
        || h.got("崩溃验收-K2") == 1,
    )
    .await;
    hub.kill();
    let mut hub = spawn(&h, true, &[]);
    serving(&h).await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The interrupted Node attempt is held after restart; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || attempts(&h, &sid, "unknown") == 1).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.got("崩溃验收-K2"), 1, "never re-run: {:?}", h.prompts());
    let n = node_health(&h).await;
    assert_eq!(
        (&n["reconciled"], &n["unknown"], &n["parked"]),
        (&json!(true), &json!(1), &json!(0)),
        "healthz after the kill: {n}"
    );
    assert!(n["sessions"].as_i64() >= Some(1), "{n}");
    hub.kill();

    // With a diagnostic Hub, the Node-bound session stays parked.
    let hub = spawn(&h, false, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let r = c.say(&sid, "回滚验收-R5 @say parked").await;
    assert_eq!(r["result"], "queued", "{r}");
    eventually_sync(
        "The session records that it is parked on its required Node.",
        || node_process::event_count_by_session_system(&h.db_path(), &sid).unwrap() > 0,
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.got("回滚验收-R5"), 0, "no runner got it");
    assert_eq!(h.got("崩溃验收-K2"), 1, "still never re-run");
    assert_eq!(hub.spawns(), 0, "no adapter started on the Hub for it");
    let n = node_health(&h).await;
    assert_eq!(
        (&n["enabled"], &n["reconciled"], &n["fallbacks"]),
        (&json!(false), &Value::Null, &json!(0)),
        "healthz with the Node off: {n}"
    );
    assert!(n["parked"].as_i64() >= Some(1), "parked: {n}");
    assert_eq!(n["unknown"], 1, "still held: {n}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_permission_nobody_answers_expires() {
    let h = Home::new();
    let _hub = spawn(&h, true, &[("STEVE_NODE_TURN_LIMIT_MS", "6000")]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "过期验收 @perm Bash").await;
    let mut card = None;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session has an open tool-call permission card; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        card = open_card(&h, &sid);
        card.is_some()
    })
    .await;
    let card = card.expect("card");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The permission card closes after the answer is delivered; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        node_process::decision_status_by_id(&h.db_path(), &card)
            .unwrap()
            .is_some_and(|s| s != "open")
    })
    .await;
    let (status, result) = node_process::decision_state(&h.db_path(), &card).unwrap();
    eprintln!("expired card: {status} {result}");
    assert_eq!(status, "expired", "{result}");
    let (st, v) = c
        .call(
            "POST",
            &format!("/api/decisions/{card}"),
            Some(json!({"choice": "allow_once"})),
        )
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        steve_v1(&h, &sid, "perm:selected:") == 0,
        "a late answer ({st} {v}) never reaches the agent"
    );
}

/// Events of `sid` (any format) mentioning a Hub restart.
fn restart_notices(h: &Home, sid: &str) -> i64 {
    node_process::event_count_by_session(&h.db_path(), sid).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_node_session_is_not_resumed_after_a_restart() {
    let h = Home::new();
    // the turn ends without a delivery: Steve's takeover would continue it later, on its own
    // clock (a prompt that is no resume); with no continues it opens its card once, settled below
    let quiet = [("STEVE_TAKEOVER_CONTINUES", "0")];
    let mut hub = spawn(&h, true, &quiet);
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "重启验收 @say idle-node-3R").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The first Node turn stores its expected reply; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "idle-node-3R") > 0).await;
    eventually_sync("The session has a finished runtime attempt.", || {
        attempts(&h, &sid, "finished") >= 1
    })
    .await;
    let fails = || {
        node_process::decision_count_by_id_fail_open_for_selected_row(&h.db_path(), &sid).unwrap()
    };
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "Exhausted retries produce one service failure card; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || fails() == 1).await;
    let (before, fails_before) = (h.prompts().len(), fails());
    for round in 0..2 {
        hub.kill();
        hub = spawn(&h, true, &quiet);
        serving(&h).await;
        // ready() has run once the Hub serves; give a wrong resume time to show up
        tokio::time::sleep(Duration::from_secs(3)).await;
        let prompts = h.prompts();
        assert!(
            !prompts.iter().any(|p| p.contains("Hub 刚重启")),
            "restart {round}: no RESUME prompt: {prompts:?}"
        );
        assert_eq!(prompts.len(), before, "restart {round}: no new turn");
        assert_eq!(
            restart_notices(&h, &sid),
            0,
            "restart {round}: no 'Hub 重启' event; log:\n{}",
            hub_log(&h)
        );
    }
    assert_eq!(fails(), fails_before, "no failure card");
}

/// A Node session's row shows its run (running during a turn, idle after), and
/// ST-28's resume info names the harness's own session id.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_session_shows_its_status_and_harness_id() {
    let h = Home::new();
    let quiet = [("STEVE_TAKEOVER_CONTINUES", "0")];
    let _hub = spawn(&h, true, &quiet);
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "状态验收 @say status-node-5S").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The first Node turn stores its expected reply; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "status-node-5S") > 0).await;
    // its view under the path model, in shadow (S2), against the local Node's roots today
    let (rw, extra) = node_process::recorded_session_view(&h.db_path(), &sid).unwrap();
    assert!(rw.contains("worktrees") || rw.contains("scratch"), "{rw}");
    assert!(extra.is_some(), "the local Node's roots are known");
    let status = |v: &Value| v["status"].as_str().unwrap_or_default().to_string();
    let get = || async {
        let (st, v) = c.call("GET", &format!("/api/sessions/{sid}"), None).await;
        assert_eq!(st, 200, "{v}");
        v
    };
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The Node session status becomes idle; idle after the turn: {}",
            get().await
        )
    });
    eventually(diagnostic, || async { status(&get().await) == "idle" }).await;

    c.say(&sid, "状态验收-运行 @sleep 600000").await;
    eventually_sync(
        "The fake adapter records the target prompt exactly once.",
        || h.got("状态验收-运行") == 1,
    )
    .await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The Node session status becomes running; running during the turn: {}",
            get().await
        )
    });
    eventually(diagnostic, || async { status(&get().await) == "running" }).await;

    // the id the fake agent's prompts carry is the one ST-28 shows
    let acp = h
        .lines()
        .iter()
        .rev()
        .find(|m| m["method"] == "session/prompt")
        .and_then(|m| m["params"]["sessionId"].as_str().map(str::to_string))
        .expect("a prompt with its session id");
    let (st, v) = c
        .call("GET", &format!("/api/sessions/{sid}/resume"), None)
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["id"].as_str(), Some(acp.as_str()), "resume info: {v}");

    let (st, v) = c
        .call("POST", &format!("/api/sessions/{sid}/cancel"), None)
        .await;
    assert_eq!((st, &v["ok"]), (200, &json!(true)), "cancel: {v}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The Node session status becomes idle; idle after the cancel: {}",
            get().await
        )
    });
    eventually(diagnostic, || async { status(&get().await) == "idle" }).await;
}

/// Polls the upgrade drain (`POST`, as `ops/local-release.py` does) until it reports `ready`.
async fn drained(h: &Home, token: &str, secs: u64) -> Option<Value> {
    let observed = std::sync::Mutex::new(None);
    tokio::time::timeout(
        Duration::from_secs(secs),
        steve_runtime::testkit::eventually("the upgrade drain is ready", || async {
            let d = drain(h, "POST", token).await;
            let ready = d["ready"] == true;
            if ready {
                *observed.lock().unwrap() = Some(d);
            }
            ready
        }),
    )
    .await
    .ok()?;
    observed.into_inner().unwrap()
}

/// The upgrade drain waits for Node work (L3, D1): a running turn, and a turn waiting for a
/// permission answer until it is answered; it seals once they are over, and lifting it still sends
/// what was queued meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn the_upgrade_drain_waits_for_node_turns() {
    let h = Home::new();
    let token = control_token(&h);
    let _hub = spawn(&h, true, &[("STEVE_TAKEOVER_CONTINUES", "0")]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "排空验收 @say drain-node-3D").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The first Node turn stores its expected reply; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "drain-node-3D") > 0).await;
    eventually_sync("The session has a finished runtime attempt.", || {
        attempts(&h, &sid, "finished") >= 1
    })
    .await;

    // ── a running turn holds the drain
    c.say(&sid, "排空验收-运行 @sleep 600000").await;
    eventually_sync(
        "The fake adapter records the target prompt exactly once.",
        || h.got("排空验收-运行") == 1,
    )
    .await;
    let d = drain(&h, "POST", &token).await;
    assert_eq!(d["maintenance"], true, "{d}");
    assert_eq!(d["ready"], false, "not sealed under a running turn: {d}");
    assert_eq!(d["node"], 1, "{d}");
    assert!(
        d["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["kind"] == "node_turn"
                && b["session_id"] == sid
                && b["item_id"].is_string()
                && b["attempt_id"].is_string()),
        "the upgrade identifies the actual waiting item: {d}"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let d = drain(&h, "POST", &token).await;
    assert_eq!((&d["ready"], &d["node"]), (&json!(false), &json!(1)), "{d}");
    let (st, v) = c
        .call("POST", &format!("/api/sessions/{sid}/cancel"), None)
        .await;
    assert_eq!((st, &v["ok"]), (200, &json!(true)), "cancel: {v}");
    let d = drained(&h, &token, 30).await;
    assert!(
        d.is_some(),
        "sealed once the turn is over; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(d.expect("drain")["node"], 0);
    let d = drain(&h, "DELETE", &token).await;
    assert_eq!(d["maintenance"], false, "{d}");

    // ── a turn waiting for a permission answer holds it too, until it is answered
    c.say(&sid, "@perm Bash").await;
    let mut card = None;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session has an open tool-call permission card; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        card = open_card(&h, &sid);
        card.is_some()
    })
    .await;
    let card = card.expect("card");
    let d = drain(&h, "POST", &token).await;
    assert_eq!(
        (&d["ready"], &d["node"]),
        (&json!(false), &json!(1)),
        "a permission wait holds the drain: {d}"
    );
    let r = c.say(&sid, "排空验收-排队 @say drained-done").await;
    assert_eq!(r["result"], "queued", "{r}");
    let (st, v) = c
        .call(
            "POST",
            &format!("/api/decisions/{card}"),
            Some(json!({"choice": "allow_once"})),
        )
        .await;
    assert_eq!(st, 200, "answer: {v}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session transcript contains the selected permission response; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "perm:selected:") > 0).await;
    assert!(
        drained(&h, &token, 30).await.is_some(),
        "sealed once the answered turn is over; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(h.got("排空验收-排队"), 0, "not sent while drained");

    // ── lifting the drain sends what waited
    let d = drain(&h, "DELETE", &token).await;
    assert_eq!(d["maintenance"], false, "{d}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The queued prompt is sent after the upgrade drain is lifted; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || h.got("排空验收-排队") == 1).await;
    eventually_sync("The released drain turn stores its final reply.", || {
        steve_v1(&h, &sid, "drained-done") > 0
    })
    .await;
}

/// A4: past STEVE_IDLE_RELEASE_MS an idle Node session's agent is let go (its process exits),
/// and the next message opens it again on the same conversation (`session/load`).
#[tokio::test(flavor = "multi_thread")]
async fn an_idle_node_session_is_let_go_and_the_next_message_resumes_it() {
    let h = Home::new();
    let env = [
        ("STEVE_TAKEOVER_CONTINUES", "0"),
        ("STEVE_IDLE_RELEASE_MS", "1500"),
    ];
    let _hub = spawn(&h, true, &env);
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "空闲验收 @say idle-release-4A").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session has a finished runtime attempt; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || attempts(&h, &sid, "finished") >= 1).await;
    let pid = h
        .lines()
        .iter()
        .find_map(|m| m["spawn"]["pid"].as_i64())
        .expect("the agent's pid") as i32;
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The idle agent process exits after maintenance; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || !alive(pid)).await;
    assert_eq!(
        node_process::command_count_release_session(&h.db_path(), &sid).unwrap(),
        1,
        "one release"
    );
    let first = h
        .lines()
        .iter()
        .find(|m| m["method"] == "session/prompt")
        .and_then(|m| m["params"]["sessionId"].as_str().map(str::to_string))
        .expect("the first conversation");

    c.say(&sid, "空闲验收-续 @say idle-again-4A").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The next message completes a new Node attempt; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || attempts(&h, &sid, "finished") >= 2).await;
    assert_eq!(h.got("空闲验收-续"), 1);
    assert_eq!(h.spawns(), 2, "a new agent");
    assert!(
        h.lines()
            .iter()
            .any(|m| m["method"] == "session/load" && m["params"]["sessionId"] == first.as_str()),
        "the same conversation, resumed"
    );
}

/// B6: with `STEVE_NODE_MODE` unset the local Node is on and a new session binds to it. A
/// diagnostic Hub can open a copy of that home and park its Node-bound session.
#[tokio::test(flavor = "multi_thread")]
async fn the_node_is_the_default() {
    let h = Home::new();
    // The default is a service Node. Stop this test's own service before reusing its home as
    // an inert copy, otherwise a queued continuation from the first Hub can spawn an adapter.
    let mut hub = spawn_mode(&h, None, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    // the default runs the Node in a process of its own (B1): it starts and links first
    let n = wait_node_health(&h, |n| n["reconciled"] == true).await;
    assert_eq!(
        (&n["enabled"], &n["reconciled"]),
        (&json!(true), &json!(true)),
        "healthz with STEVE_NODE_MODE unset: {n}"
    );
    let sid = c.item(&h, "默认验收 @say default-node-B6").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session stores the expected reply text; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "default-node-B6") > 0).await;
    assert_eq!(
        node_process::binding_count_by_session_local(&h.db_path(), &sid).unwrap(),
        1,
        "bound to the local Node"
    );
    // Wait for actual
    // completion (including bounded sampler settlement and automatic continuations) before
    // measuring the inert copy's process count; an unstarted old child can otherwise log late.
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The original Node has no unfinished turns before the next transition; the original Node's turns settled: {}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        node_process::attempt_count_by_session_finished(&h.db_path(), &sid).unwrap() > 0
            && node_process::attempt_count_by_session_not_finished(&h.db_path(), &sid).unwrap() == 0
    })
    .await;
    assert!(
        h.spawns() >= 1,
        "the initial reply came from a started adapter"
    );
    hub.kill_hub_group();
    h.service
        .get()
        .unwrap()
        .stop()
        .expect("stop the fixture's Node");

    // A diagnostic Hub ignores the copied session Node configuration.
    let hub = spawn_mode(&h, Some("local"), &[("STEVE_NO_RESUME", "1")]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let (status, health) = c.call("GET", "/healthz", None).await;
    assert_eq!(status, 200);
    assert_eq!(health["mode"], "diagnostic");
    let (status, refusal) = c
        .call(
            "POST",
            "/api/bots/draft",
            Some(json!({"desc": "a test bot"})),
        )
        .await;
    assert_eq!(status, 403, "{refusal}");
    assert_eq!(refusal["code"], "forbidden");
    assert_eq!(refusal["reason"], "config.diagnostic_model_disabled");
    assert_eq!(refusal["params"], json!({}));
    let n = node_health(&h).await;
    assert_eq!(
        n["enabled"], false,
        "healthz on a STEVE_NO_RESUME copy: {n}"
    );
    let r = c.say(&sid, "副本验收-C6 @say copy").await;
    assert_eq!(r["result"], "queued", "{r}");
    eventually_sync(
        "The session records that it is parked on its required Node.",
        || node_process::event_count_by_session_system(&h.db_path(), &sid).unwrap() > 0,
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.got("副本验收-C6"), 0, "no runner got it");
    assert_eq!(hub.spawns(), 0, "no adapter started for it");
}

/// B7: a new session the Node path cannot express (its bot's permission mode is one the Node has
/// no mode for, `dontAsk`) is refused visibly, never run on the Hub: it ends at once, says why in
/// it and on its item, and healthz counts it as a fallback since boot.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_item_with_an_incompatible_permission_mode_is_refused_before_opening() {
    let h = Home::new();
    let hub = spawn(&h, true, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let before = wait_node_health(&h, |n| n["reconciled"] == true).await;
    assert_eq!(before["fallbacks"], 0, "{before}");
    node_process::disable_claude_permission_prompts(&h.db_path()).unwrap();
    let (status, blocked) = c.call("POST", "/api/items", Some(json!({
        "goal": "@say must-not-run", "title": "Permission test", "bot": "steve", "project": null
    }))).await;
    assert_eq!(status, 409, "{blocked}");
    assert_eq!(blocked["placement"]["reason"], "permission");
    assert_eq!(h.got("must-not-run"), 0);
    assert_eq!(hub.spawns(), 0);
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(o.status.success(), "git {args:?}: {o:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// N0e-E11a: with the local Node on, a new item of a git project gets its worktree from the Node
/// (made off the core's lock, where Steve keeps worktrees, on `steve/<id>`) and its agent works
/// there; dropped, its clean worktree (and merged branch) is given back by the Node and the
/// timeline says so.
#[tokio::test(flavor = "multi_thread")]
async fn a_git_items_worktree_is_made_and_given_back_by_the_node() {
    git_worktree_case("local").await;
}

/// B1: the same with the Hub's own Node in a process of its own: its roots come through the
/// roots file, and the worktree is made, used and given back by that process.
#[tokio::test(flavor = "multi_thread")]
async fn a_git_items_worktree_on_the_service_node() {
    git_worktree_case("service").await;
}

async fn git_worktree_case(mode: &str) {
    let h = Home::new();
    let repo = h.dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("a.txt"), "a\n").expect("file");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "a"]);
    let _hub = spawn_mode(&h, Some(mode), &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    wait_node_health(&h, |n| n["reconciled"] == true).await;
    let (st, p) = c
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "repo", "path": repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(st, 200, "{p}");
    let (_, state) = c.call("GET", "/api/shell", None).await;
    let bot = state["bots"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
        .and_then(|b| b["id"].as_str())
        .expect("a claude-code bot")
        .to_string();
    let (st, i) = c
        .call(
            "POST",
            "/api/items",
            Some(json!({"goal": "@say in-worktree-8K", "title": "工作区", "bot": bot, "project": p["id"]})),
        )
        .await;
    assert_eq!(st, 200, "{i}");
    let item = i["id"].as_str().expect("item").to_string();
    let mut sid = None;
    eventually_sync("The item receives its session binding.", || {
        sid = node_process::session_id_by_item(&h.db_path(), &item).unwrap();
        sid.is_some()
    })
    .await;
    let sid = sid.expect("session");
    let wt = h.steve().join("worktrees").join(&item);
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session has a finished runtime attempt; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || attempts(&h, &sid, "finished") >= 1).await;
    assert!(wt.join("a.txt").exists(), "the worktree is there");
    assert_eq!(
        node_process::items_cwd_by_id(&h.db_path(), &item)
            .unwrap()
            .as_deref(),
        Some(wt.to_string_lossy().as_ref())
    );
    assert_eq!(
        git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        format!("steve/{}", item.to_lowercase())
    );
    // the Node made it: its prepare names the place, and the agent opened there
    let kind =
        node_process::command_kind_type_prepare_workspace(&h.db_path(), &format!("item:{item}"))
            .unwrap()
            .expect("a prepare");
    assert_eq!(kind, "worktree");
    let real = wt
        .canonicalize()
        .expect("worktree")
        .to_string_lossy()
        .to_string();
    assert!(
        h.lines()
            .iter()
            .any(|m| m["method"] == "session/new"
                && m["params"]["cwd"].as_str() == Some(real.as_str())),
        "the agent works in the worktree"
    );

    // dropped: the clean worktree goes back through the Node
    let (st, v) = c
        .call(
            "POST",
            &format!("/api/items/{item}/actions"),
            Some(json!({"action": "drop"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    // what the ten-minute watcher does, now: once the harness is let go, the Node gives it back
    eventually("the clean worktree is reclaimed", || async {
        let (st, v) = c.call("POST", "/api/system/workspaces/reclaim", None).await;
        assert_eq!(st, 200, "{v}");
        !wt.exists()
    })
    .await;
    eventually_sync(
        "Removing the Node worktree records one cleanup event.",
        || node_process::timeline_count_by_item_matching_text(&h.db_path(), &item).unwrap() == 1,
    )
    .await;
    assert_eq!(
        node_process::command_count_tidy_workspace(&h.db_path(), &format!("tidy:{item}")).unwrap(),
        1
    );
    // nothing was committed on it: its branch is merged, and goes with it, as without the Node
    assert_eq!(
        git(
            &repo,
            &[
                "branch",
                "--list",
                &format!("steve/{}", item.to_lowercase())
            ]
        ),
        ""
    );
    assert_eq!(
        node_process::timeline_count_by_item_matching_text_timeline(&h.db_path(), &item).unwrap(),
        1
    );
}

/// A 1×1 PNG (RGB, no filter), for the attachment case.
fn png(rgb: [u8; 3]) -> Vec<u8> {
    use std::io::Write as _;
    fn crc(bytes: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for b in bytes {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc(&body).to_be_bytes());
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&[0, rgb[0], rgb[1], rgb[2]]).expect("zlib");
    let idat = z.finish().expect("zlib");
    let mut ihdr = Vec::new();
    ihdr.extend(1u32.to_be_bytes());
    ihdr.extend(1u32.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(b))
}

/// A service Node crosses the real authenticated blob RPC link. The browser reads only a Hub
/// object, after the ordinary session's local source has gone, with the original image alt.
#[tokio::test(flavor = "multi_thread")]
async fn reply_images_from_service_node_are_authenticated_durable_deliveries() {
    use base64::Engine as _;
    let h = Home::new();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../steve-node/tests/research-script.mjs");
    let log = h.dir.path().join("inline-fixture");
    let _hub = spawn_mode(
        &h,
        Some("service"),
        &[
            ("STEVE_ADAPTER_CLAUDE", script.to_str().unwrap()),
            ("FAKE_RESEARCH_LOG", log.to_str().unwrap()),
        ],
    );
    serving(&h).await;
    let c = Client::login(&h).await;
    let health = wait_node_health(&h, |n| n["reconciled"] == true).await;
    assert_eq!(health["reconciled"], true, "{health}; {}", hub_log(&h));
    let bytes = png([21, 42, 63]);
    let sid = c
        .item(
            &h,
            &format!(
                "图片持久验收\n@msg image\n@imagecwd {}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            ),
        )
        .await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The Node reply publishes its authenticated file delivery link; Hub: {}\nNode: {}\nEvents: {:?}\nSource: {:?}",
            hub_log(&h),
            std::fs::read_to_string(h.steve().join("data/node/logs/local-node.log"))
                .unwrap_or_default(),
            node_process::session_work_event_trace(&h.db_path(), &sid).unwrap(),
            std::fs::read_to_string(format!("{}.image", log.display()))
        )
    });
    eventually_sync(diagnostic, || {
        steve_v1(&h, &sid, "/api/file-deliveries/") > 0
    })
    .await;
    let text = node_process::latest_event_text_by_session_text(&h.db_path(), &sid)
        .unwrap()
        .unwrap();
    assert!(
        text.starts_with("![授权二维码](/api/file-deliveries/"),
        "{text}"
    );
    assert!(text.contains("?ws=default) ![重复二维码](/api/file-deliveries/"));
    let source = std::fs::read_to_string(format!("{}.image", log.display())).unwrap();
    std::fs::remove_file(&source).unwrap();
    assert!(!std::path::Path::new(&source).exists());
    let path = text.split_once("](").unwrap().1.split(')').next().unwrap();
    let response = c
        .http
        .get(format!("{}{path}", c.base))
        .header("cookie", &c.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
    let unauth = c
        .http
        .get(format!("{}{path}", c.base))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status().as_u16(), 401);
    let (st, other) = c
        .call(
            "POST",
            "/api/workspaces",
            Some(json!({"name": "Other image workspace"})),
        )
        .await;
    assert_eq!(st, 200, "{other}");
    let other = other["id"].as_str().unwrap();
    let wrong = c
        .http
        .get(format!(
            "{}{}",
            c.base,
            path.replace("ws=default", &format!("ws={other}"))
        ))
        .header("cookie", &c.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 404);
    assert_eq!(
        node_process::deliveries_count_by_workspace_with_reply(&h.db_path(), "default").unwrap(),
        1
    );
    // Exercise multiple bulk frames too; a tiny QR alone cannot detect overlarge hex frames.
    let mut state = 42u32;
    let pixels = (0..160 * 160 * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    let image = image::RgbImage::from_raw(160, 160, pixels).unwrap();
    let mut encoded = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let large = encoded.into_inner();
    assert!(large.len() > steve_node_wire::envelope::BULK_FRAME);
    let large_source = std::path::Path::new(&source).with_file_name("large.png");
    std::fs::write(&large_source, &large).unwrap();
    c.say(
        &sid,
        &format!("@msg large\n@say ![多块图片]({})", large_source.display()),
    )
    .await;
    let mut text = None;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The multimodal reply stores both its text and file delivery link; {}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        text = node_process::latest_event_text_by_session_text_text(&h.db_path(), &sid).unwrap();
        text.is_some()
    })
    .await;
    std::fs::remove_file(large_source).unwrap();
    let text = text.unwrap();
    let path = text.split_once("](").unwrap().1.split(')').next().unwrap();
    let r = c
        .http
        .get(format!("{}{path}", c.base))
        .header("cookie", &c.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.bytes().await.unwrap().as_ref(), large);
}

impl Client {
    async fn upload(&self, name: &str, bytes: Vec<u8>) -> String {
        let r = self
            .http
            .post(format!("{}/api/files?name={name}", self.base))
            .header("cookie", &self.cookie)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .expect("upload");
        let st = r.status().as_u16();
        let v: Value = r.json().await.expect("upload json");
        assert_eq!(st, 200, "upload: {v}");
        v["id"].as_str().expect("file id").to_string()
    }
    async fn say_with(&self, sid: &str, text: &str, files: &[&str]) -> Value {
        let (st, v) = self
            .call(
                "POST",
                &format!("/api/sessions/{sid}/messages"),
                Some(json!({"text": text, "files": files})),
            )
            .await;
        assert_eq!(st, 200, "message: {v}");
        v
    }
}

/// The prompt blocks the agent got for the message carrying `token`.
fn prompt_blocks(h: &Home, token: &str) -> Option<Vec<Value>> {
    h.lines()
        .into_iter()
        .filter(|m| m["method"] == "session/prompt")
        .filter_map(|m| m["params"]["prompt"].as_array().cloned())
        .find(|ps| {
            ps.iter()
                .any(|p| p["text"].as_str().is_some_and(|t| t.contains(token)))
        })
}

/// N0e-E9: a Node session's attachments come to the agent through the Node. It fetches them from
/// the Hub (HubFiles, by hash) and places them in the session's workspace under
/// `.steve/attachments` with the ignore file; the image also goes inline. An attachment the Hub no
/// longer has does not hold the turn up: it goes without it, and the agent is told.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_sessions_attachments_are_placed_by_the_node() {
    let h = Home::new();
    let _hub = spawn(&h, true, &[("FAKE_ACP_IMAGE", "1")]);
    serving(&h).await;
    let c = Client::login(&h).await;
    wait_node_health(&h, |n| n["reconciled"] == true).await;
    let sid = c.item(&h, "附件验收 @say ready-A1").await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session stores the expected reply text; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "ready-A1") > 0).await;
    assert_eq!(
        node_process::binding_count_by_session_local(&h.db_path(), &sid).unwrap(),
        1,
        "bound to the local Node"
    );

    let notes = "第一行\nline two\n".repeat(50).into_bytes();
    let image = png([200, 30, 30]);
    let txt = c.upload("notes.txt", notes.clone()).await;
    let pic = c.upload("截图.png", image.clone()).await;
    c.say_with(&sid, "附件验收-F3 @say got-files", &[&txt, &pic])
        .await;
    let mut blocks = None;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The Node agent receives the prompt with its attachment blocks; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        blocks = prompt_blocks(&h, "附件验收-F3");
        blocks.is_some()
    })
    .await;
    let blocks = blocks.expect("blocks");
    let text = blocks[0]["text"].as_str().expect("text").to_string();
    let cwd = node_process::session_cwd_by_id(&h.db_path(), &sid)
        .unwrap()
        .expect("cwd");
    let cwd = PathBuf::from(cwd).canonicalize().expect("cwd exists");
    assert!(
        text.contains("用户附带了 2 个附件，已保存在工作目录里"),
        "{text}"
    );
    for (id, name, bytes) in [(&txt, "notes.txt", &notes), (&pic, "截图.png", &image)] {
        let rel = format!(".steve/attachments/{}-{name}", &id[4..]);
        assert!(text.contains(&format!("- {rel}（")), "{rel} named: {text}");
        let placed = std::fs::read(cwd.join(&rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(
            sha256_hex(&placed),
            sha256_hex(bytes),
            "{rel}: the same bytes"
        );
    }
    assert_eq!(
        std::fs::read_to_string(cwd.join(".steve/attachments/.gitignore")).expect("ignore file"),
        "*\n"
    );
    assert!(
        text.contains("图片也直接附在这条消息里"),
        "the image is inline: {text}"
    );
    assert!(
        blocks
            .iter()
            .any(|b| b["type"] == "image" && b["mimeType"] == "image/png"),
        "an image block: {blocks:?}"
    );
    // fetched by the Node: the bytes are in its cache, by hash
    for bytes in [&notes, &image] {
        let cached = h.steve().join("node/blobs").join(sha256_hex(bytes));
        let cached = if cached.exists() {
            cached
        } else {
            walk(&h.steve())
                .into_iter()
                .find(|p| {
                    p.parent().and_then(|d| d.file_name()) == Some("blobs".as_ref())
                        && p.file_name() == Some(sha256_hex(bytes).as_ref())
                })
                .unwrap_or_else(|| panic!("no cached blob under {}", h.steve().display()))
        };
        assert_eq!(
            sha256_hex(&std::fs::read(cached).expect("cache")),
            sha256_hex(bytes)
        );
    }
    eventually_sync(
        "The agent confirms that the forwarded attachments arrived.",
        || steve_v1(&h, &sid, "got-files") > 0,
    )
    .await;

    // the blob is gone from the Hub: the turn goes on without it, and says so
    let lost = c.upload("lost.txt", b"never placed".to_vec()).await;
    let (ws, sha) = node_process::file_location(&h.db_path(), &lost).unwrap();
    std::fs::remove_file(
        h.steve()
            .join("data/files")
            .join(&ws)
            .join(&sha[..2])
            .join(&lost),
    )
    .expect("remove blob");
    c.say_with(&sid, "附件验收-G4 @say went-on", &[&lost]).await;
    // wait for the prompt itself: the user's own message (which names `went-on`) is a steve.v1
    // event the moment it is sent, before the agent has seen anything
    let mut blocks = None;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The Node agent receives the prompt with its attachment blocks; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        blocks = prompt_blocks(&h, "附件验收-G4");
        blocks.is_some()
    })
    .await;
    let text = blocks.expect("prompt")[0]["text"]
        .as_str()
        .expect("text")
        .to_string();
    assert!(
        text.contains("（用户还附带了 1 个附件，但没能取到：lost.txt）"),
        "{text}"
    );
    assert!(
        !cwd.join(format!(".steve/attachments/{}-lost.txt", &lost[4..]))
            .exists()
    );
}

/// Every file under `dir`.
fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// The fixture retires its own executable, so it never signals an unowned or reused PID; if the
/// detached local Node fails to retire within its deadline while still holding `node.lock`, its
/// lock-proven session group (`setsid`) is force-killed so no `steve-hub --local-node` leaks.
struct ServiceNodeGuard {
    executable: PathBuf,
    node_executable: PathBuf,
    node_lock: PathBuf,
    service_pid: PathBuf,
}

impl ServiceNodeGuard {
    fn link_or_copy(source: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
        match std::fs::hard_link(source, dest) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                std::fs::copy(source, dest)?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn new(dir: &std::path::Path, home: PathBuf) -> std::io::Result<Self> {
        let source = std::path::Path::new(env!("CARGO_BIN_EXE_steve-hub"));
        let node_source = source.with_file_name("steve-node");
        let executable = dir.join("steve-hub");
        let node_executable = dir.join("steve-node");
        Self::link_or_copy(source, &executable)?;
        if node_source.is_file() {
            Self::link_or_copy(&node_source, &node_executable)?;
        }
        Ok(Self {
            executable,
            node_executable,
            node_lock: home.join("data/node/node.lock"),
            service_pid: home.join("data/node/service.pid"),
        })
    }

    fn lock_holder_pid(&self) -> Option<i32> {
        std::fs::read_to_string(&self.node_lock)
            .ok()
            .and_then(|s| s.lines().next()?.trim().parse::<i32>().ok())
            .filter(|pid| *pid > 1)
    }

    fn stop(&self) -> std::io::Result<()> {
        for p in [&self.executable, &self.node_executable] {
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let lock = match File::open(&self.node_lock) {
            Ok(lock) => lock,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // If the Hub started `--local-node` right before panic, wait briefly in case it
                // opens `node.lock` before noticing the removed executable.
                if self.service_pid.exists() {
                    let wait = std::time::Instant::now() + Duration::from_secs(2);
                    loop {
                        if let Ok(lock) = File::open(&self.node_lock) {
                            break lock;
                        }
                        if std::time::Instant::now() >= wait {
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                } else {
                    return Ok(());
                }
            }
            Err(e) => return Err(e),
        };
        // The Node holds its home lock until it shuts down. The executable is gone even if
        // the fixture stopped the Hub before its Node had opened the lock.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(()),
                Err(TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(TryLockError::WouldBlock) => {
                    // Graceful retirement timed out while `node.lock` is still exclusively held:
                    // reading the PID written by the lock holder proves ownership (zero reuse risk).
                    if let Some(pid) = self.lock_holder_pid() {
                        GroupGuard::kill_pgid(pid);
                        let force_deadline = std::time::Instant::now() + Duration::from_secs(5);
                        while std::time::Instant::now() < force_deadline {
                            if lock.try_lock().is_ok() {
                                return Ok(());
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    }
                    return Err(std::io::Error::other("local Node did not shut down"));
                }
                Err(TryLockError::Error(e)) => return Err(e),
            }
        }
    }
}

impl Drop for ServiceNodeGuard {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            if !std::thread::panicking() {
                panic!("retiring the fixture's local Node: {e}");
            }
            eprintln!("retiring the fixture's local Node: {e}");
        }
    }
}

async fn linked_service(h: &Home) -> (Hub, File) {
    let hub = spawn_mode(
        h,
        Some("service"),
        &[
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false"),
            ("STEVE_LOG", "info"),
        ],
    );
    serving(h).await;
    wait_node_health(h, |n| n["reconciled"] == true).await;
    let lock = File::open(h.steve().join("data/node/node.lock")).unwrap();
    assert!(matches!(lock.try_lock(), Err(TryLockError::WouldBlock)));
    (hub, lock)
}

#[tokio::test]
async fn fixture_timeout_stops_its_detached_service_node() {
    let h = Home::new();
    let (hub, lock) = linked_service(&h).await;
    let executable = h.service_executable().to_owned();
    let mut log = File::open(h.steve().join("data/node/logs/local-node.log")).unwrap();
    let timed_out = tokio::time::timeout(Duration::from_millis(10), async move {
        let _home = h;
        let _hub = hub;
        std::future::pending::<()>().await;
    })
    .await;
    assert!(timed_out.is_err());
    assert!(!executable.exists());
    lock.try_lock().expect("the fixture released the Node home");
    let mut written = String::new();
    std::io::Read::read_to_string(&mut log, &mut written).unwrap();
    assert!(written.contains("node shutdown completed"), "{written}");
}

#[tokio::test]
async fn local_service_exits_when_its_home_or_executable_disappears() {
    for remove_home in [true, false] {
        let h = Home::new();
        let (mut hub, lock) = linked_service(&h).await;
        hub.kill_hub_group();
        assert!(matches!(lock.try_lock(), Err(TryLockError::WouldBlock)));
        let executable = h.service_executable();
        if remove_home {
            std::fs::rename(h.steve(), h.dir.path().join("retired-home")).unwrap();
            assert!(executable.exists());
        } else {
            std::fs::remove_file(executable).unwrap();
            assert!(h.steve().is_dir());
        }
        eventually_sync(
            "the local Node releases its home after being retired",
            || match lock.try_lock() {
                Ok(()) => true,
                Err(TryLockError::WouldBlock) => false,
                Err(e) => panic!("checking the Node home lock: {e}"),
            },
        )
        .await;
        if remove_home {
            assert!(
                !h.steve().exists(),
                "the Node did not recreate its Hub home"
            );
        }
    }
}

/// B1: with `STEVE_NODE_MODE=service` the Hub's own Node runs in a process of its own. A turn
/// running there goes on while the Hub is killed (its process group only, as a service manager
/// stops it) and started again on the same HTTP port; the Node must not hold the Hub's listener.
/// The restarted Hub catches up, and the turn is never sent twice.
#[tokio::test(flavor = "multi_thread")]
async fn a_service_node_turn_outlives_the_hub() {
    let h = Home::new();
    // Hold the fixture's Node home until the Hub has accepted the first item. A database
    // generation or HTTP readiness alone cannot guarantee the Node has linked and caught up.
    let node_home = h.steve().join("data/node");
    std::fs::create_dir_all(&node_home).unwrap();
    let boot_gate = File::create(node_home.join("node.lock")).unwrap();
    boot_gate.lock().unwrap();
    let hub = spawn_mode(&h, Some("service"), &[]);
    serving(&h).await;
    let addr = h.address.lock().unwrap().expect("the first Hub's address");
    let c = Client::login(&h).await;
    let sid = c.item(&h, "服务验收 @say hello-service-3S").await;
    assert_eq!(h.got("hello-service-3S"), 0, "the first turn is queued");
    drop(boot_gate);
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The local Node links from its own process with a live generation; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        node_process::node_count_by_node_id_with_local_with_generation(&h.db_path(), "local")
            .unwrap()
            == 1
    })
    .await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The session stores the expected reply text; Hub log:\n{}",
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || steve_v1(&h, &sid, "hello-service-3S") > 0).await;
    assert_eq!(h.got("hello-service-3S"), 1);
    assert_eq!(
        node_process::binding_count_by_session_local(&h.db_path(), &sid).unwrap(),
        1
    );

    // the Hub goes away mid-turn, the way a service manager stops it
    let states = |h: &Home| -> String {
        node_process::attempt_states(&h.db_path(), &sid)
            .unwrap()
            .join(",")
    };
    let logs = |h: &Home| {
        let tail = |p: PathBuf| {
            let s = std::fs::read_to_string(p).unwrap_or_default();
            let l: Vec<&str> = s.lines().collect();
            l[l.len().saturating_sub(40)..].join("\n")
        };
        format!(
            "hub:\n{}\nnode:\n{}",
            tail(hub_log_path(&h)),
            tail(h.steve().join("data/node/logs/local-node.log"))
        )
    };
    // the item's turns so far are done
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The original Node has no unfinished turns before the next transition; states {}; {}",
            states(&h),
            logs(&h)
        )
    });
    eventually_sync(diagnostic, || {
        attempts(&h, &sid, "finished") >= 1 && states(&h).split(',').all(|s| s == "finished")
    })
    .await;
    c.say(&sid, "存活验收-V6 @sleep 12000 @say survived-V6")
        .await;
    eventually_sync(
        "The fake adapter records the target prompt exactly once.",
        || h.got("存活验收-V6") == 1,
    )
    .await;
    let node_pid: i32 = std::fs::read_to_string(h.steve().join("data/node/service.pid"))
        .expect("the Node's pid")
        .lines()
        .next()
        .expect("pid line")
        .trim()
        .parse()
        .expect("pid");
    let started = std::time::Instant::now();
    let mut hub = hub;
    hub.kill_hub_group();
    assert_eq!(
        unsafe { libc::kill(node_pid, 0) },
        0,
        "the Node outlived the Hub"
    );

    let port = addr.port().to_string();
    let _hub = spawn_mode(&h, Some("service"), &[("STEVE_PORT", &port)]);
    serving(&h).await;
    assert_eq!(*h.address.lock().unwrap(), Some(addr));
    let tail = |p: PathBuf| {
        let s = std::fs::read_to_string(p).unwrap_or_default();
        let l: Vec<&str> = s.lines().collect();
        l[l.len().saturating_sub(60)..].join("\n")
    };
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The turn finishes through the Hub restart and stores its reply; the turn finished through the Hub's restart; states {}; hub:\n{}\nnode:\n{}",
            states(&h),
            tail(hub_log_path(&h)),
            tail(h.steve().join("data/node/logs/local-node.log"))
        )
    });
    eventually_sync(diagnostic, || {
        node_process::event_count_by_session_selected(&h.db_path(), &sid).unwrap() > 0
    })
    .await;
    assert!(
        started.elapsed() >= Duration::from_secs(10),
        "it ran its 12 s (seen from here a little later than it began), not cut short"
    );
    assert_eq!(attempts(&h, &sid, "unknown"), 0, "nothing left unknown");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        h.got("存活验收-V6"),
        1,
        "never sent again: {:?}",
        h.prompts()
    );
    assert_eq!(
        unsafe { libc::kill(node_pid, 0) },
        0,
        "the same Node process went on"
    );
}

/// B7: tags on the Hub's own Node; an item asking for one the Hub has keeps its new sessions
/// here, and an unmatched tag prevents placement.
#[tokio::test(flavor = "multi_thread")]
async fn node_tags_send_work_where_it_fits() {
    let h = Home::new();
    let _hub = spawn(&h, true, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    wait_node_health(&h, |n| n["reconciled"] == true).await;
    let (s, v) = c
        .call(
            "PUT",
            "/api/node-tags/local",
            Some(json!({"tags": ["GPU", "bad tag!", "macos"]})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["tags"], json!(["gpu", "macos"]));
    let (_, v) = c.call("GET", "/api/node-tags", None).await;
    assert_eq!(v["tags"]["local"], json!(["gpu", "macos"]));

    // the Hub's default for loading a Node owner's personal agent config: on until the owner says
    let (s, v) = c.call("GET", "/api/node-context", None).await;
    assert_eq!((s, v["owner_context"].clone()), (200, json!("on")), "{v}");
    let (s, _) = c
        .call(
            "PUT",
            "/api/node-context",
            Some(json!({"owner_context": "maybe"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, v) = c
        .call(
            "PUT",
            "/api/node-context",
            Some(json!({"owner_context": "off"})),
        )
        .await;
    assert_eq!((s, v["owner_context"].clone()), (200, json!("off")), "{v}");
    let (_, v) = c.call("GET", "/api/node-context", None).await;
    assert_eq!(v["owner_context"], json!("off"));

    let sid = c.item(&h, "标签验收 @say hello-tags-5T").await;
    let item = node_process::session_item_id_by_id(&h.db_path(), &sid)
        .unwrap()
        .expect("item");
    let (s, _) = c
        .call(
            "PUT",
            &format!("/api/items/{item}/node-tag"),
            Some(json!({"tag": "not ok!"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = c
        .call(
            "PUT",
            &format!("/api/items/{item}/node-tag"),
            Some(json!({"tag": "gpu"})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = c
        .call(
            "POST",
            &format!("/api/sessions/{sid}/continue"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let next = v["session"].as_str().expect("new session").to_string();
    eventually_sync(
        "The session is placed on the local Node matching its required tag.",
        || node_process::binding_count_by_session_local(&h.db_path(), &next).unwrap() == 1,
    )
    .await;

    let (s, _) = c
        .call(
            "PUT",
            &format!("/api/items/{item}/node-tag"),
            Some(json!({"tag": "cuda"})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = c
        .call(
            "POST",
            &format!("/api/sessions/{next}/continue"),
            Some(json!({})),
        )
        .await;
    assert_ne!(s, 200, "{v}");
    assert!(
        v["error"].as_str().is_some_and(|e| e.contains("标签")),
        "{v}"
    );
    assert_eq!(
        node_process::session_count_for_item(&h.db_path(), &item).unwrap(),
        2,
        "no third session"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_reply_keeps_report_and_its_image_without_a_tool_call() {
    use base64::Engine as _;
    let h = Home::new();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../steve-node/tests/research-script.mjs");
    let log = h.dir.path().join("reply-fixture");
    let _hub = spawn_mode(
        &h,
        Some("service"),
        &[
            ("STEVE_ADAPTER_CLAUDE", script.to_str().unwrap()),
            ("FAKE_RESEARCH_LOG", log.to_str().unwrap()),
        ],
    );
    serving(&h).await;
    let c = Client::login(&h).await;
    assert_eq!(
        wait_node_health(&h, |n| n["reconciled"] == true).await["reconciled"],
        true
    );
    let bytes = png([4, 45, 99]);
    let prompt = format!(
        "@msg report\n@replyfiles {}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    let sid = c.item(&h, &prompt).await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The reply stores a durable report link; Hub log: {}\nText: {:?}",
            hub_log(&h),
            node_process::session_text(&h.db_path(), &sid).unwrap()
        )
    });
    eventually_sync(diagnostic, || {
        node_process::session_text(&h.db_path(), &sid)
            .unwrap()
            .is_some_and(|t| t.contains("[Report](/deliveries/"))
    })
    .await;
    let text = node_process::session_text(&h.db_path(), &sid)
        .unwrap()
        .unwrap();
    assert_eq!(
        text.matches("#steve-file-unavailable-scope").count(),
        3,
        "{text}"
    );
    let path = text
        .split_once("[Report](/deliveries/")
        .unwrap()
        .1
        .split(')')
        .next()
        .unwrap();
    let id = path.split('?').next().unwrap();
    let api = format!("/api/file-deliveries/{id}");
    let source = std::fs::read_to_string(log.with_extension("replyfiles")).unwrap();
    std::fs::remove_dir_all(&source).unwrap();
    assert!(!std::path::Path::new(&source).exists());
    let (status, body) = c.call("GET", &api, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["can_manage"], false);
    assert_eq!(body["mime"], "text/markdown");
    let rendered = body["preview"]["text"].as_str().unwrap();
    assert!(
        rendered.contains("![Missing figure](#steve-file-unavailable-missing)"),
        "{rendered}"
    );
    let asset = rendered
        .split_once("![Figure in report](")
        .unwrap()
        .1
        .split(')')
        .next()
        .unwrap();
    assert!(asset.starts_with("assets/"), "{rendered}");
    let response = c
        .http
        .get(format!("{}{api}/content/{asset}?ws=default", c.base))
        .header("cookie", &c.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
    let response = c
        .http
        .get(format!("{}{api}/raw?ws=default", c.base))
        .header("cookie", &c.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("![Figure in report](figure.png)")
    );
    let (status, _) = c
        .call(
            "POST",
            &format!("{api}/shares"),
            Some(json!({"expires_at":steve_store::now()+360_000})),
        )
        .await;
    assert!(matches!(status, 400 | 403));
    assert_eq!(
        node_process::deliveries_count_by_session_with_reply(&h.db_path(), &sid).unwrap(),
        2
    );
    let (status, _) = c
        .call("GET", &format!("{api}/content/not-in-the-report.png"), None)
        .await;
    assert_eq!(status, 404);

    // The colocated Node can resolve a user's durable outputs path (which can differ
    // from the cwd). A different item's path is still refused by the same grant.
    let own = h
        .steve()
        .join("workspaces")
        .join(body["item_id"].as_str().unwrap())
        .join("outputs");
    let other = h.steve().join("workspaces/ST-unrelated/outputs");
    for dir in [&own, &other] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("result.txt"), b"fixture").unwrap();
    }
    c.say(&sid, &format!("@msg durable\n@say [Durable](<file://{}/result.txt>) · [Foreign output](<file://{}/result.txt>)",
        own.display(), other.display())).await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The reply keeps its own durable output and rejects the foreign output link; {:?} {}",
            node_process::session_text(&h.db_path(), &sid).unwrap(),
            hub_log(&h)
        )
    });
    eventually_sync(diagnostic, || {
        node_process::session_text(&h.db_path(), &sid)
            .unwrap()
            .is_some_and(|t| {
                t.contains("[Durable](/deliveries/")
                    && t.contains("[Foreign output](#steve-file-unavailable-scope)")
            })
    })
    .await;
}

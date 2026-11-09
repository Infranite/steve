//! The local Node (`STEVE_NODE_MODE=local`, the default), end to end on the real `steve-hub` binary: a temp
//! STEVE_HOME, its own port, the fake ACP agent (`steve-acp/tests/fake-acp.js`, which logs every
//! message it gets), real HTTP. Covers D5's acceptance on one Hub home:
//! - a session created with the Node on is bound to it and its transcript is stored as `steve.v1`;
//! - a permission request becomes a card, its answer reaches the agent;
//! - a cancel ends the running turn;
//! - while held (upgrade drain), a message is queued and not drained, and goes once released;
//! - the Hub killed mid-turn comes back with the attempt `unknown` (awaiting reconciliation),
//!   which is never re-run;
//! - with `STEVE_NODE_MODE` off, a Node-bound session is parked, and nothing runs on the Hub;
//! - with `STEVE_NODE_MODE` unset the Node is on (the default); a `STEVE_NO_RESUME` copy boots
//!   with it off instead, and its Node-bound sessions are parked.
//!
//! - B7: what an older Hub's legacy runner left moves to the Node at boot (or ends, with why), and
//!   a new session no Node takes is refused visibly, never run on the Hub.
//!
//! And on a second home, with a short attempt deadline, a card nobody answers expires; on a
//! third, an idle Node session goes through two Hub restarts without a resume or a restart notice.
//! With a short idle limit, an idle Node session's agent exits and the next message resumes it.
//!
//! No real host, no real agent, never `~/.steve` or the production port.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::{Value, json};

const VERSION: &str = "0.9.0";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("port")
}

struct Home {
    dir: tempfile::TempDir,
    port: u16,
    /// The service Node's loopback port: its own free port, not the Hub's + 3, which on a host
    /// handing out ports in order is often another test's Hub.
    local_port: u16,
    log: PathBuf,
    web: PathBuf,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().expect("tmp");
        let web = dir.path().join("web");
        std::fs::create_dir_all(&web).expect("web");
        std::fs::write(web.join("index.html"), "<html><head></head></html>").expect("index");
        std::fs::write(
            web.join("release.json"),
            json!({"version": VERSION}).to_string(),
        )
        .expect("web version");
        Home {
            port: free_port(),
            local_port: free_port(),
            log: dir.path().join("acp.log"),
            web,
            dir,
        }
    }
    fn steve(&self) -> PathBuf {
        self.dir.path().join("steve")
    }
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn lines(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect()
    }
    /// Prompt texts the fake agent received, in order.
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
    /// Count adapters by workspace, without unrelated background model jobs. Canonicalize
    /// both sides because the temporary home may use a symlink (e.g. /var on macOS).
    fn spawns_under(&self, dir: &std::path::Path) -> usize {
        let root = dir.canonicalize().expect("adapter workspace");
        self.lines()
            .iter()
            .filter_map(|m| m["spawn"]["cwd"].as_str())
            .filter_map(|cwd| std::path::Path::new(cwd).canonicalize().ok())
            .filter(|cwd| cwd.starts_with(&root))
            .count()
    }
    /// The test's own database (in the temp home), read-only.
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.steve().join("data/steve.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("db")
    }
    fn count(&self, sql: &str, p: &str) -> i64 {
        self.db()
            .query_row(sql, [p], |r| r.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }
    fn one(&self, sql: &str, p: &str) -> Option<String> {
        self.db().query_row(sql, [p], |r| r.get(0)).ok()
    }
}

/// A running Hub process (its own process group: killing it takes its agent along).
struct Hub(Child);

impl Hub {
    /// SIGKILL the Hub and its agents (each agent runs in its own process group).
    fn kill(&mut self) {
        let pid = self.0.id() as i32;
        let children: Vec<i32> = Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
            for c in &children {
                libc::killpg(*c, libc::SIGKILL);
                libc::kill(*c, libc::SIGKILL);
            }
        }
        let _ = self.0.wait();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The Hub with the local Node explicitly on (`local`) or off (`off`, the rollback).
fn spawn(h: &Home, node: bool, extra: &[(&str, &str)]) -> Hub {
    spawn_mode(h, Some(if node { "local" } else { "off" }), extra)
}

/// The Hub with `STEVE_NODE_MODE` set to `mode`, or unset (the default since B6) for `None`.
fn spawn_mode(h: &Home, mode: Option<&str>, extra: &[(&str, &str)]) -> Hub {
    let fake = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../steve-acp/tests/fake-acp.js"
    );
    // tracing_subscriber writes to stdout by default. Capture both streams so a failed end-to-end
    // assertion retains the Hub's event/permission errors instead of showing an empty log.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(h.dir.path().join("hub.log"))
        .expect("hub log");
    let mut c = Command::new(env!("CARGO_BIN_EXE_steve-hub"));
    c.env("HOME", h.dir.path())
        .env("STEVE_HOME", h.steve())
        .env("STEVE_PORT", h.port.to_string())
        .env("STEVE_LOCAL_NODE_PORT", h.local_port.to_string())
        .env("STEVE_HOST", "127.0.0.1")
        .env("STEVE_PUBLIC_URL", h.base())
        .env("STEVE_DEV_LOGIN", "1")
        .env("STEVE_REPO", "/nonexistent")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_VERSION", VERSION)
        .env("STEVE_WEB_DIR", &h.web)
        .env("FAKE_ACP_LOG", &h.log)
        .env("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CLAUDE", fake)
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env_remove("STEVE_NODE_MODE")
        .env_remove("STEVE_NODE")
        .env_remove("STEVE_NODE_JOBS")
        .env_remove("STEVE_NODE_TURN_LIMIT_MS")
        .env_remove("STEVE_IDLE_RELEASE_MS")
        .env_remove("STEVE_LEGACY_MOVE_EVERY_MS")
        .env_remove("STEVE_NO_RESUME")
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
    Hub(c.spawn().expect("spawn hub"))
}

async fn until<F: std::future::Future<Output = bool>>(secs: u64, mut f: impl FnMut() -> F) -> bool {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < end {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn wait(secs: u64, mut f: impl FnMut() -> bool) -> impl std::future::Future<Output = bool> {
    until(secs, move || std::future::ready(f()))
}

/// Up and serving the full app.
async fn serving(h: &Home) {
    let ok = until(60, || async {
        let Ok(r) = reqwest::Client::new()
            .get(format!("{}/healthz", h.base()))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        else {
            return false;
        };
        r.json::<Value>()
            .await
            .is_ok_and(|v| v["ok"] == true && v["maintenance"] != true)
    })
    .await;
    assert!(ok, "the Hub serves; log:\n{}", hub_log(h));
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

/// Until `/healthz`'s `node` block satisfies `ok` (within `secs`); the last one seen.
async fn node_health_until(h: &Home, secs: u64, ok: impl Fn(&Value) -> bool) -> Value {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let v = node_health(h).await;
        if ok(&v) || tokio::time::Instant::now() >= end {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn hub_log(h: &Home) -> String {
    let s = std::fs::read_to_string(h.dir.path().join("hub.log")).unwrap_or_default();
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
            .find(|c| c.starts_with("steve_session="))
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
        let (_, state) = self.call("GET", "/api/state", None).await;
        let bot = state["bots"]
            .as_array()
            .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
            .and_then(|b| b["id"].as_str())
            .expect("a claude-code bot")
            .to_string();
        let (st, i) = self
            .call(
                "POST",
                "/api/items",
                Some(json!({"goal": goal, "title": "Node 验收", "bot": bot, "project": null})),
            )
            .await;
        assert_eq!(st, 200, "{i}");
        let item = i["id"].as_str().expect("item").to_string();
        let mut sid = None;
        assert!(
            wait(30, || {
                sid = h.one("SELECT id FROM sessions WHERE item_id=?", &item);
                sid.is_some()
            })
            .await
        );
        sid.expect("session")
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
    h.db()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND format='steve.v1' AND instr(data, ?) > 0",
            [sid, needle],
            |r| r.get(0),
        )
        .expect("events")
}

/// The open permission card of `sid`, if any.
fn open_card(h: &Home, sid: &str) -> Option<String> {
    h.one(
        "SELECT id FROM decisions WHERE session_id=? AND subject_type='tool_call' AND status='open'",
        sid,
    )
}

/// Attempts of `sid` in `state`.
fn attempts(h: &Home, sid: &str, state: &str) -> i64 {
    h.db()
        .query_row(
            "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND a.state=?",
            [sid, state],
            |r| r.get(0),
        )
        .expect("attempts")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_session_end_to_end() {
    let h = Home::new();
    let token = control_token(&h);
    let mut hub = spawn(&h, true, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let n = node_health_until(&h, 30, |n| n["reconciled"] == true).await;
    assert_eq!(
        (&n["enabled"], &n["reconciled"], &n["stopped"]),
        (&json!(true), &json!(true), &json!(false)),
        "healthz after boot: {n}"
    );

    // ── a new session binds to the Node; its transcript is steve.v1
    let sid = c.item(&h, "节点验收 @say hello-node-7Q").await;
    assert!(
        wait(30, || steve_v1(&h, &sid, "hello-node-7Q") > 0).await,
        "the agent's reply stored as steve.v1; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=? AND node_id='local'",
            &sid
        ),
        1,
        "bound to the local Node"
    );
    assert!(
        wait(10, || attempts(&h, &sid, "finished") >= 1).await,
        "the turn ran as a Node attempt"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND format<>'steve.v1'",
            &sid
        ),
        0,
        "nothing in the legacy format"
    );

    // ── a permission request becomes a card; its answer reaches the agent
    c.say(&sid, "@perm Bash").await;
    let mut card = None;
    assert!(
        wait(30, || {
            card = open_card(&h, &sid);
            card.is_some()
        })
        .await,
        "a permission card; log:\n{}",
        hub_log(&h)
    );
    let card = card.expect("card");
    let (st, v) = c
        .call(
            "POST",
            &format!("/api/decisions/{card}"),
            Some(json!({"choice": "allow_once"})),
        )
        .await;
    assert_eq!(st, 200, "answer: {v}");
    assert!(
        wait(30, || steve_v1(&h, &sid, "perm:selected:") > 0).await,
        "the agent got the answer; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(
        h.one("SELECT status FROM decisions WHERE id=?", &card)
            .as_deref(),
        Some("resolved")
    );

    // ── a cancel ends the running turn
    c.say(&sid, "取消验收-C4 @sleep 600000").await;
    assert!(wait(30, || h.got("取消验收-C4") == 1).await);
    let (st, v) = c
        .call("POST", &format!("/api/sessions/{sid}/cancel"), None)
        .await;
    assert_eq!((st, &v["ok"]), (200, &json!(true)), "cancel: {v}");
    assert!(
        wait(30, || {
            h.count(
                "SELECT COUNT(*) FROM runtime_turns WHERE session_id=? AND state<>'finished'",
                &sid,
            ) == 0
        })
        .await,
        "the cancelled turn is over; log:\n{}",
        hub_log(&h)
    );
    assert!(
        h.lines().iter().any(|m| m["method"] == "session/cancel"),
        "the agent was told to cancel"
    );
    assert!(
        wait(10, || steve_v1(&h, &sid, "cancelled") > 0).await,
        "the turn ended as cancelled"
    );

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
    assert!(
        wait(30, || h.got("排队验收-H8") == 1).await,
        "sent once released; log:\n{}",
        hub_log(&h)
    );
    assert!(wait(30, || steve_v1(&h, &sid, "held-done") > 0).await);

    // ── the Hub killed mid-turn: the attempt is never re-run; the Hub gives it up on its own once
    // the Node confirms the old harness is gone, and the agent is continued (B6)
    c.say(&sid, "崩溃验收-K2 @sleep 600000").await;
    assert!(wait(30, || h.got("崩溃验收-K2") == 1).await);
    hub.kill();
    let mut hub = spawn(&h, true, &[]);
    serving(&h).await;
    assert!(
        wait(30, || {
            h.count(
                "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND json_extract(a.completion,'$.stop_reason')='node_restarted'",
                &sid,
            ) == 1
        })
        .await,
        "the cut attempt is given up; log:\n{}",
        hub_log(&h)
    );
    assert!(
        wait(10, || steve_v1(&h, &sid, "Node 重启了") > 0).await,
        "the session says why"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.got("崩溃验收-K2"), 1, "never re-run: {:?}", h.prompts());
    let n = node_health(&h).await;
    assert_eq!(
        (&n["reconciled"], &n["unknown"], &n["parked"]),
        (&json!(true), &json!(0), &json!(0)),
        "healthz after the kill: {n}"
    );
    assert!(n["sessions"].as_i64() >= Some(1), "{n}");
    hub.kill();

    // ── STEVE_NODE_MODE off: the Node-bound session is parked, never handed to the legacy runner
    let spawns = h.spawns();
    let _hub = spawn(&h, false, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let r = c.say(&sid, "回滚验收-R5 @say legacy").await;
    assert_eq!(r["result"], "queued", "{r}");
    assert!(
        wait(10, || {
            h.count(
                "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='system' AND json_extract(data,'$.parked')=1",
                &sid,
            ) > 0
        })
        .await,
        "told it is parked"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.got("回滚验收-R5"), 0, "no runner got it");
    assert_eq!(h.got("崩溃验收-K2"), 1, "still never re-run");
    assert_eq!(h.spawns(), spawns, "no legacy adapter started for it");
    let n = node_health(&h).await;
    assert_eq!(
        (&n["enabled"], &n["reconciled"], &n["fallbacks"]),
        (&json!(false), &Value::Null, &json!(0)),
        "healthz with the Node off: {n}"
    );
    assert!(n["parked"].as_i64() >= Some(1), "parked: {n}");
    assert_eq!(n["unknown"], 0, "given up before: {n}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_permission_nobody_answers_expires() {
    let h = Home::new();
    let _hub = spawn(&h, true, &[("STEVE_NODE_TURN_LIMIT_MS", "6000")]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "过期验收 @perm Bash").await;
    let mut card = None;
    assert!(
        wait(30, || {
            card = open_card(&h, &sid);
            card.is_some()
        })
        .await,
        "a permission card; log:\n{}",
        hub_log(&h)
    );
    let card = card.expect("card");
    assert!(
        wait(30, || h
            .one("SELECT status FROM decisions WHERE id=?", &card)
            .is_some_and(|s| s != "open"))
        .await,
        "the card closes by itself; log:\n{}",
        hub_log(&h)
    );
    let (status, result): (String, String) = h
        .db()
        .query_row(
            "SELECT status, COALESCE(result,'') FROM decisions WHERE id=?",
            [&card],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("card");
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
    h.count(
        "SELECT COUNT(*) FROM events WHERE session_id=? AND instr(data, 'Hub 重启') > 0",
        sid,
    )
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
    assert!(
        wait(30, || steve_v1(&h, &sid, "idle-node-3R") > 0).await,
        "the first turn ran; log:\n{}",
        hub_log(&h)
    );
    assert!(wait(10, || attempts(&h, &sid, "finished") >= 1).await);
    let fails = || {
        h.count(
            "SELECT COUNT(*) FROM decisions WHERE item_id=(SELECT item_id FROM sessions WHERE id=?) AND kind='fail' AND status='open'",
            &sid,
        )
    };
    assert!(
        wait(10, || fails() == 1).await,
        "the takeover's card; log:\n{}",
        hub_log(&h)
    );
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
        assert_eq!(
            h.count(
                "SELECT COUNT(*) FROM session_resume WHERE session_id=?",
                &sid
            ),
            0,
            "restart {round}: no resume intent"
        );
    }
    assert_eq!(fails(), fails_before, "no failure card");
}

/// A Node session's row shows its run like a legacy one (running during a turn, idle after), and
/// ST-28's resume info names the harness's own session id.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_session_shows_its_status_and_harness_id() {
    let h = Home::new();
    let quiet = [("STEVE_TAKEOVER_CONTINUES", "0")];
    let _hub = spawn(&h, true, &quiet);
    serving(&h).await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "状态验收 @say status-node-5S").await;
    assert!(
        wait(30, || steve_v1(&h, &sid, "status-node-5S") > 0).await,
        "the first turn ran; log:\n{}",
        hub_log(&h)
    );
    // its view under the path model, in shadow (S2), against the local Node's roots today
    let (rw, extra): (String, Option<i64>) = h
        .db()
        .query_row(
            "SELECT json_extract(view,'$.rw'), extra_count FROM view_shadow WHERE session_id=?",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the session's view is recorded at bind");
    assert!(rw.contains("worktrees") || rw.contains("scratch"), "{rw}");
    assert!(extra.is_some(), "the local Node's roots are known");
    let status = |v: &Value| v["status"].as_str().unwrap_or_default().to_string();
    let get = || async {
        let (st, v) = c.call("GET", &format!("/api/sessions/{sid}"), None).await;
        assert_eq!(st, 200, "{v}");
        v
    };
    assert!(
        until(10, || async { status(&get().await) == "idle" }).await,
        "idle after the turn: {}",
        get().await
    );

    c.say(&sid, "状态验收-运行 @sleep 600000").await;
    assert!(wait(30, || h.got("状态验收-运行") == 1).await);
    assert!(
        until(10, || async { status(&get().await) == "running" }).await,
        "running during the turn: {}",
        get().await
    );

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
    assert!(
        until(30, || async { status(&get().await) == "idle" }).await,
        "idle after the cancel: {}",
        get().await
    );
}

/// Polls the upgrade drain (`POST`, as `ops/local-release.py` does) until it reports `ready`.
async fn drained(h: &Home, token: &str, secs: u64) -> Option<Value> {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        let d = drain(h, "POST", token).await;
        if d["ready"] == true {
            return Some(d);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    None
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
    assert!(
        wait(30, || steve_v1(&h, &sid, "drain-node-3D") > 0).await,
        "the first turn ran; log:\n{}",
        hub_log(&h)
    );
    assert!(wait(10, || attempts(&h, &sid, "finished") >= 1).await);

    // ── a running turn holds the drain
    c.say(&sid, "排空验收-运行 @sleep 600000").await;
    assert!(wait(30, || h.got("排空验收-运行") == 1).await);
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
    assert!(
        wait(30, || {
            card = open_card(&h, &sid);
            card.is_some()
        })
        .await,
        "a permission card; log:\n{}",
        hub_log(&h)
    );
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
    assert!(
        wait(30, || steve_v1(&h, &sid, "perm:selected:") > 0).await,
        "the agent got the answer under the drain; log:\n{}",
        hub_log(&h)
    );
    assert!(
        drained(&h, &token, 30).await.is_some(),
        "sealed once the answered turn is over; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(h.got("排空验收-排队"), 0, "not sent while drained");

    // ── lifting the drain sends what waited
    let d = drain(&h, "DELETE", &token).await;
    assert_eq!(d["maintenance"], false, "{d}");
    assert!(
        wait(30, || h.got("排空验收-排队") == 1).await,
        "sent once the drain is lifted; log:\n{}",
        hub_log(&h)
    );
    assert!(wait(30, || steve_v1(&h, &sid, "drained-done") > 0).await);
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
    assert!(
        wait(30, || attempts(&h, &sid, "finished") >= 1).await,
        "the first turn ran; log:\n{}",
        hub_log(&h)
    );
    let pid = h
        .lines()
        .iter()
        .find_map(|m| m["spawn"]["pid"].as_i64())
        .expect("the agent's pid") as i32;
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
    assert!(
        wait(30, || !alive(pid)).await,
        "the idle agent exits; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=?",
            &sid
        ),
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
    assert!(
        wait(30, || attempts(&h, &sid, "finished") >= 2).await,
        "the next message ran; log:\n{}",
        hub_log(&h)
    );
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
/// `STEVE_NO_RESUME` copy of that home boots with the Node off instead of failing, and parks the
/// Node-bound session; an explicit `local` on such a copy refuses to boot.
#[tokio::test(flavor = "multi_thread")]
async fn the_node_is_the_default() {
    let h = Home::new();
    let mut hub = spawn_mode(&h, None, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    // the default runs the Node in a process of its own (B1): it starts and links first
    let n = node_health_until(&h, 90, |n| n["reconciled"] == true).await;
    assert_eq!(
        (&n["enabled"], &n["reconciled"]),
        (&json!(true), &json!(true)),
        "healthz with STEVE_NODE_MODE unset: {n}"
    );
    let sid = c.item(&h, "默认验收 @say default-node-B6").await;
    assert!(
        wait(30, || steve_v1(&h, &sid, "default-node-B6") > 0).await,
        "the reply stored as steve.v1; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=? AND node_id='local'",
            &sid
        ),
        1,
        "bound to the local Node"
    );
    hub.kill();
    drop(hub);

    // a STEVE_NO_RESUME copy: an explicit local is a boot error
    let mut refused = spawn_mode(&h, Some("local"), &[("STEVE_NO_RESUME", "1")]);
    assert!(
        wait(30, || matches!(refused.0.try_wait(), Ok(Some(_)))).await,
        "an explicit local on a STEVE_NO_RESUME copy exits"
    );
    assert!(
        hub_log(&h).contains("STEVE_NO_RESUME"),
        "says why; log:\n{}",
        hub_log(&h)
    );
    drop(refused);

    // unset: it boots with the Node off, and the bound session is parked
    let spawns = h.spawns();
    let _hub = spawn_mode(&h, None, &[("STEVE_NO_RESUME", "1")]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let n = node_health(&h).await;
    assert_eq!(
        n["enabled"], false,
        "healthz on a STEVE_NO_RESUME copy: {n}"
    );
    let r = c.say(&sid, "副本验收-C6 @say copy").await;
    assert_eq!(r["result"], "queued", "{r}");
    assert!(
        wait(10, || {
            h.count(
                "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='system' AND json_extract(data,'$.parked')=1",
                &sid,
            ) > 0
        })
        .await,
        "told it is parked"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.got("副本验收-C6"), 0, "no runner got it");
    assert_eq!(h.spawns(), spawns, "no adapter started for it");
}

/// B7: a new session the Node path cannot express (its bot's permission mode is one the Node has
/// no mode for, `dontAsk`) is refused visibly, never run on the Hub: it ends at once, says why in
/// it and on its item, and healthz counts it as a fallback since boot, never as a legacy session.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_no_node_takes_is_refused_visibly() {
    let h = Home::new();
    let _hub = spawn(&h, true, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    let before = node_health_until(&h, 30, |n| n["reconciled"] == true).await;
    assert_eq!(before["fallbacks"], 0, "{before}");
    let db = rusqlite::Connection::open(h.steve().join("data/steve.sqlite3")).expect("db");
    db.busy_timeout(Duration::from_secs(5)).expect("timeout");
    db.execute(
        "UPDATE bots SET perm_mode='dontAsk' WHERE harness='claude-code'",
        [],
    )
    .expect("dontAsk");
    let spawns = h.spawns();
    let sid = c.item(&h, "回退验收 @say fallback-legacy-3F").await;
    let n = node_health_until(&h, 30, |n| n["fallbacks"] == 1).await;
    assert_eq!(n["fallbacks"], 1, "{n}");
    assert_eq!(n["legacy"]["sessions"], 0, "never a legacy session: {n}");
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?",
            &sid
        ),
        0,
        "not bound"
    );
    assert_eq!(
        h.one("SELECT status FROM sessions WHERE id=?", &sid)
            .as_deref(),
        Some("ended")
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='system' AND json_extract(data,'$.refused')=1 AND instr(data,'dontAsk')>0",
            &sid
        ),
        1,
        "it says why"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM decisions d JOIN sessions s ON s.item_id=d.item_id WHERE s.id=? AND d.kind='fail' AND d.status='open' AND instr(d.question,'dontAsk')>0",
            &sid
        ),
        1,
        "and on its item"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.got("fallback-legacy-3F"), 0, "nothing ran it");
    assert_eq!(h.spawns(), spawns, "no adapter started for it");
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
    let port = free_port().to_string();
    let extra = [("STEVE_LOCAL_NODE_PORT", port.as_str())];
    let _guard = ServiceNodeGuard(h.steve().join("data/node/service.pid"));
    let repo = h.dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("a.txt"), "a\n").expect("file");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "a"]);
    let _hub = spawn_mode(&h, Some(mode), &extra);
    serving(&h).await;
    let c = Client::login(&h).await;
    node_health_until(&h, 30, |n| n["reconciled"] == true).await;
    let (st, p) = c
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "repo", "path": repo.to_string_lossy()})),
        )
        .await;
    assert_eq!(st, 200, "{p}");
    let (_, state) = c.call("GET", "/api/state", None).await;
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
    assert!(
        wait(30, || {
            sid = h.one("SELECT id FROM sessions WHERE item_id=?", &item);
            sid.is_some()
        })
        .await
    );
    let sid = sid.expect("session");
    let wt = h.steve().join("worktrees").join(&item);
    assert!(
        wait(60, || attempts(&h, &sid, "finished") >= 1).await,
        "the agent ran; log:\n{}",
        hub_log(&h)
    );
    assert!(wt.join("a.txt").exists(), "the worktree is there");
    assert_eq!(
        h.one("SELECT cwd FROM items WHERE id=?", &item).as_deref(),
        Some(wt.to_string_lossy().as_ref())
    );
    assert_eq!(
        git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        format!("steve/{}", item.to_lowercase())
    );
    // the Node made it: its prepare names the place, and the agent opened there
    let kind = h
        .one(
            "SELECT json_extract(request,'$.body.kind.type') FROM node_commands WHERE method='prepare_workspace' AND json_extract(request,'$.body.resource')=?",
            &format!("item:{item}"),
        )
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
    let mut gone = false;
    for _ in 0..60 {
        let (st, v) = c.call("POST", "/api/system/workspaces/reclaim", None).await;
        assert_eq!(st, 200, "{v}");
        if wait(1, || !wt.exists()).await {
            gone = true;
            break;
        }
    }
    assert!(gone, "the worktree is given back; log:\n{}", hub_log(&h));
    assert!(
        wait(10, || h.count(
            "SELECT COUNT(*) FROM timeline WHERE item_id=? AND text LIKE '删除了工作区%'",
            &item
        ) == 1)
        .await
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM node_commands WHERE method='tidy_workspace' AND json_extract(request,'$.subject')=?",
            &format!("tidy:{item}")
        ),
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
        h.count(
            "SELECT COUNT(*) FROM timeline WHERE item_id=? AND text LIKE '%删除了已合并的分支%'",
            &item
        ),
        1
    );
}

/// B5: sessions the legacy runner ran before the Node was switched on move to it at an idle
/// boundary, on the same conversation. An idle one moves at its next message (`session/load` of
/// the harness session id the legacy runner kept); one cut mid-turn by the restart is first
/// resumed on the legacy runner and moves at its next message after that; one that can never
/// run on the Node (its directory is gone) is ended at boot with why, once. With the Node off
/// again a moved session is parked, never handed back to the legacy runner.
/// What the store says of a session, for a failure message.
fn debug_session(h: &Home, sid: &str) -> String {
    let db = h.db();
    let rows = |sql: &str| -> Vec<String> {
        let mut st = db.prepare(sql).expect("prepare");
        let n = st.column_count();
        st.query_map([sid], |r| {
            Ok((0..n)
                .map(|i| match r.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Text(t)) => String::from_utf8_lossy(t).into(),
                    Ok(v) => format!("{v:?}"),
                    Err(e) => e.to_string(),
                })
                .collect::<Vec<_>>()
                .join("|"))
        })
        .expect("query")
        .map(Result::unwrap)
        .collect()
    };
    format!(
        "session {:?}\nruntime {:?}\ncommands {:?}\nqueued {:?}\nevents {:?}",
        rows("SELECT status, acp_session_id FROM sessions WHERE id=?"),
        rows("SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?"),
        rows(
            "SELECT method, state, substr(receipt,1,300) FROM node_commands WHERE instr(request, ?)>0"
        ),
        rows("SELECT text, via FROM queued WHERE session_id=?"),
        rows("SELECT kind, substr(data,1,300) FROM events WHERE session_id=?"),
    )
}

/// What an older Hub's legacy runner left, written into a home whose Hub is stopped: items with
/// their sessions as that runner kept them (bound to no Node, its harness's conversation id),
/// its dispatches in hand and an open permission card. The claude-code bot's id is `bot`.
fn legacy_fixture(h: &Home, bot: &str) {
    let db = rusqlite::Connection::open(h.steve().join("data/steve.sqlite3")).expect("db");
    db.busy_timeout(Duration::from_secs(5)).expect("timeout");
    let gone = "/nonexistent-steve-b7/gone";
    // where the old runner worked: each item's scratch directory, as the core made them
    let scratch = |item: &str| {
        let d = h.steve().join("scratch").join(item);
        std::fs::create_dir_all(&d).expect("scratch");
        d.display().to_string()
    };
    let (a, r, i) = (scratch("ST-91"), scratch("ST-93"), scratch("ST-95"));
    db.execute_batch(&format!(
        "INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES
           ('ST-91','default',91,'旧事项甲','run','执行','{bot}',1,1),
           ('ST-92','default',92,'旧事项乙','run','执行','{bot}',1,1),
           ('ST-93','default',93,'旧事项丙','run','执行','{bot}',1,1),
           ('ST-94','default',94,'旧事项丁','run','执行','{bot}',1,1),
           ('ST-95','default',95,'旧事项戊','run','执行','{bot}',1,1);
         INSERT INTO sessions(id,workspace_id,item_id,no,bot_id,harness,cwd,status,acp_session_id,title,created,updated) VALUES
           ('ses_legacy_a','default','ST-91',1,'{bot}','claude-code','{a}','idle','acp-legacy-a','甲',1,1),
           ('ses_legacy_b','default','ST-92',1,'{bot}','claude-code','{gone}','idle','acp-legacy-b','乙',1,1),
           ('ses_legacy_r','default','ST-93',1,'{bot}','claude-code','{r}','running','acp-legacy-r','丙',1,1),
           ('ses_legacy_ro','default','ST-94',1,'{bot}','claude-code','','idle',NULL,'丁',1,1),
           ('ses_legacy_i','default','ST-95',1,'{bot}','claude-code','{i}','idle','acp-legacy-i','戊',1,1);
         UPDATE sessions SET readonly=1 WHERE id='ses_legacy_ro';
         -- claimed, never delivered: goes as it was
         INSERT INTO session_dispatch(id,session_id,text,via,created,files,state,updated) VALUES
           ('q-a1','ses_legacy_a','旧会话甲-已领取 @say moved-a-B7','me',2,NULL,'claimed',2),
         -- delivered when the old Hub stopped: comes back to be checked, behind the resume
           ('q-r1','ses_legacy_r','旧会话丙-已送出 @say moved-r-B7','me',2,NULL,'submitted',2);
         -- a message nothing could ever send where it would run
         INSERT INTO queued(id,session_id,text,via,created) VALUES ('q-b1','ses_legacy_b','旧会话乙-排队','me',3);
         -- the old harness's permission request, open when it stopped
         INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,item_id,session_id,question,status,requested_by,created)
           VALUES ('dec-legacy-r','default','approve','tool_call','tc-1','ST-93','ses_legacy_r','允许执行：Bash','open','{bot}',2);"
    ))
    .expect("fixture");
}

/// B7: boot moves what the legacy runner left. Idle sessions open on the Node with their own
/// conversation (`session/load` of the id the runner kept) and send what it had claimed; a turn
/// the old Hub's stop cut continues once (the resume first, then the message it had delivered,
/// to be checked), its old permission card expired; a session that can never run on the Node
/// ends, with why and its queue; an imported read-only one is left alone. healthz counts them
/// and has no legacy session left. Messages to them afterwards go to the Node or are refused;
/// with the Node off, new sessions are refused and Node sessions park, nothing runs on the Hub.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_sessions_move_to_the_node_at_boot() {
    let h = Home::new();
    let token = control_token(&h);
    let env = [("STEVE_TAKEOVER_CONTINUES", "0")];
    let mut hub = spawn_mode(&h, None, &env);
    serving(&h).await;
    let c = Client::login(&h).await;
    let (_, state) = c.call("GET", "/api/state", None).await;
    let bot = state["bots"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
        .and_then(|b| b["id"].as_str())
        .expect("a claude-code bot")
        .to_string();
    // the old Hub drains for the upgrade (durable) and stops
    let d = drain(&h, "POST", &token).await;
    assert_eq!(d["maintenance"], true, "{d}");
    hub.kill();
    drop(hub);
    legacy_fixture(&h, &bot);
    let scratch = h.steve().join("scratch");

    // the upgrade: the first boot of this build on that home, still behind the drain (the
    // release lifts it once the new Hub answers)
    let spawns = h.spawns();
    let jobs = h.count("SELECT COUNT(*) FROM runtime_jobs WHERE ?<>''", "-");
    let hub = spawn_mode(&h, None, &env);
    serving(&h).await;
    let n = node_health_until(&h, 30, |n| n["reconciled"] == true).await;
    assert_eq!(
        (&n["adopted"], &n["retired"], &n["legacy"]["sessions"]),
        (&json!(3), &json!(1), &json!(0)),
        "every legacy session moved or ended at boot: {n}"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE phase='released' AND ?<>''",
            "-"
        ),
        3,
        "bound, nothing opened"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.count("SELECT COUNT(*) FROM runtime_jobs WHERE ?<>''", "-"),
        jobs,
        "no background model job behind the drain; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(
        h.spawns(),
        spawns,
        "no harness behind the drain; ACP:\n{:?}",
        h.lines()
    );
    let d = drain(&h, "DELETE", &token).await;
    assert_eq!(d["maintenance"], false, "{d}");
    assert!(
        wait(30, || h
            .count("SELECT COUNT(*) FROM runtime_jobs WHERE ?<>''", "-")
            > jobs)
        .await,
        "the background model call resumes after maintenance; log:\n{}",
        hub_log(&h)
    );
    for (s, conv, token) in [
        ("ses_legacy_a", "acp-legacy-a", "moved-a-B7"),
        ("ses_legacy_r", "acp-legacy-r", "moved-r-B7"),
    ] {
        assert!(
            wait(30, || attempts(&h, s, "finished") >= 1).await,
            "{s} ran on the Node: {}; log:\n{}",
            debug_session(&h, s),
            hub_log(&h)
        );
        assert_eq!(
            h.count(
                "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=? AND node_id='local'",
                s
            ),
            1
        );
        assert!(
            h.lines()
                .iter()
                .any(|m| m["method"] == "session/load" && m["params"]["sessionId"] == conv),
            "{s} continues its own conversation"
        );
        assert_eq!(h.got(token), 1, "{s}: what it had in hand goes once");
        assert_eq!(
            h.count(
                "SELECT COUNT(*) FROM events WHERE session_id=? AND json_extract(data,'$.adopted')=1",
                s
            ),
            1,
            "{s} hears it once"
        );
    }
    // the cut turn: the resume first, the delivered message to be checked, never as new
    let r: Vec<String> = h
        .prompts()
        .into_iter()
        .filter(|p| p.contains("moved-r-B7"))
        .collect();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(
        r[0].starts_with("Hub 刚重启") && r[0].contains("以下是 Hub 重启前已经发给你的消息"),
        "{}",
        r[0]
    );
    assert_eq!(
        h.one("SELECT status FROM decisions WHERE id=?", "dec-legacy-r")
            .as_deref(),
        Some("expired"),
        "the old harness's permission died with it"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM session_dispatch WHERE state IN ('claimed','submitted') AND ?<>''",
            "-"
        ),
        0,
        "nothing left in the old runner's hands"
    );
    // Category backfill resumes too, in a Node job directory of its own. It must not count
    // as an extra migrated session, and the idle session must still have no adapter.
    assert_eq!(
        h.spawns_under(&scratch),
        2,
        "one session harness each, none for the idle one; ACP:\n{:?}",
        h.lines()
    );
    for (item, expected) in [("ST-91", 1), ("ST-93", 1), ("ST-95", 0)] {
        assert_eq!(h.spawns_under(&scratch.join(item)), expected, "{item}");
    }
    // the idle one is bound as one the Node let go: its harness opens at its next message
    assert_eq!(
        h.one(
            "SELECT phase FROM runtime_sessions WHERE session_id=?",
            "ses_legacy_i"
        )
        .as_deref(),
        Some("released")
    );
    c.say("ses_legacy_i", "旧会话戊-续 @say moved-i-B7").await;
    assert!(
        wait(30, || h.got("moved-i-B7") == 1).await,
        "it opens on the Node at its message: {}",
        debug_session(&h, "ses_legacy_i")
    );
    assert!(
        h.lines()
            .iter()
            .any(|m| m["method"] == "session/load" && m["params"]["sessionId"] == "acp-legacy-i"),
        "on its own conversation"
    );
    assert_eq!(h.spawns_under(&scratch), 3);
    for item in ["ST-91", "ST-93", "ST-95"] {
        assert_eq!(h.spawns_under(&scratch.join(item)), 1, "{item}");
    }
    // b can never run there: ended, why said once, its queue said and gone
    assert_eq!(
        h.one("SELECT status FROM sessions WHERE id=?", "ses_legacy_b")
            .as_deref(),
        Some("ended")
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND json_extract(data,'$.retired')=1 AND instr(data,'工作目录已不存在')>0",
            "ses_legacy_b"
        ),
        1,
        "b says why, once"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='queued_removed'",
            "ses_legacy_b"
        ),
        1,
        "the dropped message is said"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM queued WHERE session_id=?",
            "ses_legacy_b"
        ),
        0
    );
    // the imported read-only one is no legacy session: as it was
    assert_eq!(
        h.one("SELECT status FROM sessions WHERE id=?", "ses_legacy_ro")
            .as_deref(),
        Some("idle")
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?",
            "ses_legacy_ro"
        ),
        0
    );
    // afterwards: a moved one takes messages on the Node, an ended one refuses them
    c.say("ses_legacy_a", "旧会话甲-续 @say again-a-B7").await;
    assert!(
        wait(30, || h.got("again-a-B7") == 1).await,
        "on the Node; log:\n{}",
        hub_log(&h)
    );
    let (st, v) = c
        .call(
            "POST",
            "/api/sessions/ses_legacy_b/messages",
            Some(json!({"text": "旧会话乙-续 @say revived-b-B7"})),
        )
        .await;
    assert_ne!(st, 200, "refused: {v}");
    drop(hub);

    // a second boot moves nothing again
    let hub = spawn_mode(&h, None, &env);
    serving(&h).await;
    let n = node_health_until(&h, 30, |n| n["reconciled"] == true).await;
    assert_eq!(
        (&n["adopted"], &n["retired"], &n["legacy"]["sessions"]),
        (&json!(0), &json!(0), &json!(0)),
        "{n}"
    );
    assert_eq!(h.got("moved-r-B7"), 1, "the cut turn continued once");
    drop(hub);

    // the Node off: a Node session parks, a new session is refused, nothing runs on the Hub
    let spawns = h.spawns_under(&scratch);
    let _hub = spawn(&h, false, &env);
    serving(&h).await;
    let c = Client::login(&h).await;
    c.say("ses_legacy_a", "旧会话甲-停 @say parked-a-B7").await;
    assert!(
        wait(30, || h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND json_extract(data,'$.parked')=1",
            "ses_legacy_a"
        ) == 1)
        .await,
        "parked; log:\n{}",
        hub_log(&h)
    );
    let sid = c.item(&h, "关闭验收 @say off-new-B7").await;
    assert_eq!(
        h.one("SELECT status FROM sessions WHERE id=?", &sid)
            .as_deref(),
        Some("ended")
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND json_extract(data,'$.refused')=1 AND instr(data,'STEVE_NODE_MODE=off')>0",
            &sid
        ),
        1,
        "a clear refusal"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.got("parked-a-B7"), 0);
    assert_eq!(h.got("off-new-B7"), 0);
    assert_eq!(
        h.spawns_under(&scratch),
        spawns,
        "no session harness on the Hub"
    );
}

/// B7: a session the boot pass could not move yet (here: a resume handle no Node takes, mended
/// afterwards) moves at a later pass and sends what it has queued then, not only at the next
/// message; a turn the restart cut in a session that can never move ends with a failure card on
/// its item, never left looking like work in progress.
#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_session_left_at_boot_moves_later_and_sends_its_queue() {
    let h = Home::new();
    let env = [
        ("STEVE_TAKEOVER_CONTINUES", "0"),
        ("STEVE_LEGACY_MOVE_EVERY_MS", "300"),
    ];
    let mut hub = spawn_mode(&h, None, &env);
    serving(&h).await;
    let c = Client::login(&h).await;
    let (_, state) = c.call("GET", "/api/state", None).await;
    let bot = state["bots"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
        .and_then(|b| b["id"].as_str())
        .expect("a claude-code bot")
        .to_string();
    hub.kill();
    drop(hub);
    {
        let db = rusqlite::Connection::open(h.steve().join("data/steve.sqlite3")).expect("db");
        db.busy_timeout(Duration::from_secs(5)).expect("timeout");
        let d = h.steve().join("scratch").join("ST-96");
        std::fs::create_dir_all(&d).expect("scratch");
        let (l, gone) = (d.display().to_string(), "/nonexistent-steve-b7/cut");
        db.execute_batch(&format!(
            "INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES
               ('ST-96','default',96,'旧事项己','run','执行','{bot}',1,1),
               ('ST-97','default',97,'旧事项庚','run','执行','{bot}',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,no,bot_id,harness,cwd,status,acp_session_id,title,created,updated) VALUES
               ('ses_legacy_l','default','ST-96',1,'{bot}','claude-code','{l}','idle','bad handle','己',1,1),
               ('ses_legacy_x','default','ST-97',1,'{bot}','claude-code','{gone}','running','acp-legacy-x','庚',1,1);
             INSERT INTO session_dispatch(id,session_id,text,via,created,files,state,updated) VALUES
               ('q-l1','ses_legacy_l','旧会话己-已领取 @say later-l-B7','me',2,NULL,'claimed',2);"
        ))
        .expect("fixture");
    }
    let _hub = spawn_mode(&h, None, &env);
    serving(&h).await;
    node_health_until(&h, 30, |n| n["reconciled"] == true).await;
    // the cut turn that can never move: ended, its item carries a card
    assert_eq!(
        h.one("SELECT status FROM sessions WHERE id=?", "ses_legacy_x")
            .as_deref(),
        Some("ended")
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM decisions WHERE item_id=? AND kind='fail' AND status='open' AND instr(question,'工作目录已不存在')>0",
            "ST-97"
        ),
        1,
        "{}",
        debug_session(&h, "ses_legacy_x")
    );
    // left at boot: bound to no Node, its message waits
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?",
            "ses_legacy_l"
        ),
        0,
        "{}",
        debug_session(&h, "ses_legacy_l")
    );
    assert_eq!(h.got("later-l-B7"), 0);
    // mended: the next pass moves it and sends what waited, with no message of yours
    {
        let db = rusqlite::Connection::open(h.steve().join("data/steve.sqlite3")).expect("db");
        db.busy_timeout(Duration::from_secs(5)).expect("timeout");
        db.execute(
            "UPDATE sessions SET acp_session_id='acp-legacy-l' WHERE id='ses_legacy_l'",
            [],
        )
        .expect("mend");
    }
    assert!(
        wait(30, || h.got("later-l-B7") == 1).await,
        "moved later and sent: {}; log:\n{}",
        debug_session(&h, "ses_legacy_l"),
        hub_log(&h)
    );
    assert!(
        h.lines()
            .iter()
            .any(|m| m["method"] == "session/load" && m["params"]["sessionId"] == "acp-legacy-l"),
        "on its own conversation"
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
    node_health_until(&h, 30, |n| n["reconciled"] == true).await;
    let sid = c.item(&h, "附件验收 @say ready-A1").await;
    assert!(
        wait(30, || steve_v1(&h, &sid, "ready-A1") > 0).await,
        "the first turn; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=? AND node_id='local'",
            &sid
        ),
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
    assert!(
        wait(30, || {
            blocks = prompt_blocks(&h, "附件验收-F3");
            blocks.is_some()
        })
        .await,
        "the prompt reached the agent; log:\n{}",
        hub_log(&h)
    );
    let blocks = blocks.expect("blocks");
    let text = blocks[0]["text"].as_str().expect("text").to_string();
    let cwd = h
        .one("SELECT cwd FROM sessions WHERE id=?", &sid)
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
    assert!(wait(30, || steve_v1(&h, &sid, "got-files") > 0).await);

    // the blob is gone from the Hub: the turn goes on without it, and says so
    let lost = c.upload("lost.txt", b"never placed".to_vec()).await;
    let (ws, sha): (String, String) = h
        .db()
        .query_row(
            "SELECT workspace_id, sha256 FROM files WHERE id=?",
            [&lost],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("file row");
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
    assert!(
        wait(30, || {
            blocks = prompt_blocks(&h, "附件验收-G4");
            blocks.is_some()
        })
        .await,
        "the turn ran; log:\n{}",
        hub_log(&h)
    );
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

/// Stops the local Node a service-mode Hub started (its pid file), when the test ends either way.
struct ServiceNodeGuard(PathBuf);

impl Drop for ServiceNodeGuard {
    fn drop(&mut self) {
        if let Some(pid) = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// B1: with `STEVE_NODE_MODE=service` the Hub's own Node runs in a process of its own. A turn
/// running there goes on while the Hub is killed (its process group only, as a service manager
/// stops it) and started again; the restarted Hub catches up, and the turn is never sent twice.
#[tokio::test(flavor = "multi_thread")]
async fn a_service_node_turn_outlives_the_hub() {
    let h = Home::new();
    let port = free_port().to_string();
    let extra = [("STEVE_LOCAL_NODE_PORT", port.as_str())];
    let _guard = ServiceNodeGuard(h.steve().join("data/node/service.pid"));
    let hub = spawn_mode(&h, Some("service"), &extra);
    serving(&h).await;
    let c = Client::login(&h).await;
    assert!(
        wait(60, || h.count(
            "SELECT COUNT(*) FROM nodes WHERE node_id=? AND local=1 AND generation>0",
            "local"
        ) == 1)
        .await,
        "the local Node linked from its own process; log:\n{}",
        hub_log(&h)
    );
    let sid = c.item(&h, "服务验收 @say hello-service-3S").await;
    assert!(
        wait(30, || steve_v1(&h, &sid, "hello-service-3S") > 0).await,
        "a turn on the service Node; log:\n{}",
        hub_log(&h)
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=? AND node_id='local'",
            &sid
        ),
        1
    );

    // the Hub goes away mid-turn, the way a service manager stops it
    let states = |h: &Home| -> String {
        let db = h.db();
        let mut st = db.prepare("SELECT a.state FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?").unwrap();
        st.query_map([&sid], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>()
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
            tail(h.dir.path().join("hub.log")),
            tail(h.steve().join("data/node/logs/local-node.log"))
        )
    };
    // the item's turns so far are done
    assert!(
        wait(10, || attempts(&h, &sid, "finished") >= 1
            && states(&h).split(',').all(|s| s == "finished"))
        .await,
        "states {}; {}",
        states(&h),
        logs(&h)
    );
    c.say(&sid, "存活验收-V6 @sleep 12000 @say survived-V6")
        .await;
    assert!(wait(30, || h.got("存活验收-V6") == 1).await);
    let started = std::time::Instant::now();
    let pid = hub.0.id() as i32;
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    let mut hub = hub;
    let _ = hub.0.wait();
    std::mem::forget(hub);
    let node_pid: i32 = std::fs::read_to_string(h.steve().join("data/node/service.pid"))
        .expect("the Node's pid")
        .trim()
        .parse()
        .expect("pid");
    assert_eq!(
        unsafe { libc::kill(node_pid, 0) },
        0,
        "the Node outlived the Hub"
    );

    let _hub = spawn_mode(&h, Some("service"), &extra);
    serving(&h).await;
    let tail = |p: PathBuf| {
        let s = std::fs::read_to_string(p).unwrap_or_default();
        let l: Vec<&str> = s.lines().collect();
        l[l.len().saturating_sub(60)..].join("\n")
    };
    assert!(
        wait(60, || h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND kind NOT IN ('user','queued') AND instr(data,'survived-V6')>0",
            &sid
        ) > 0)
        .await,
        "the turn finished through the Hub's restart; states {}; hub:\n{}\nnode:\n{}",
        states(&h),
        tail(h.dir.path().join("hub.log")),
        tail(h.steve().join("data/node/logs/local-node.log"))
    );
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
/// here, and one asking for a tag no device has is told so, placed as usual.
#[tokio::test(flavor = "multi_thread")]
async fn node_tags_send_work_where_it_fits() {
    let h = Home::new();
    let _hub = spawn(&h, true, &[]);
    serving(&h).await;
    let c = Client::login(&h).await;
    node_health_until(&h, 30, |n| n["reconciled"] == true).await;
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

    let sid = c.item(&h, "标签验收 @say hello-tags-5T").await;
    let item = h
        .one("SELECT item_id FROM sessions WHERE id=?", &sid)
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
    assert!(
        wait(30, || h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=? AND node_id='local'",
            &next
        ) == 1)
        .await,
        "the Hub has the tag: the session runs here"
    );

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
    assert_eq!(s, 200, "{v}");
    let third = v["session"].as_str().expect("new session").to_string();
    assert!(
        wait(30, || h.count("SELECT COUNT(*) FROM events WHERE session_id=? AND kind='system' AND instr(data,'cuda')>0", &third) == 1).await,
        "told no device has the tag; log:\n{}",
        hub_log(&h)
    );
    assert!(
        wait(30, || h.count(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?",
            &third
        ) == 1)
        .await,
        "placed as usual"
    );
}

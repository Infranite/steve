//! The local Node (`STEVE_NODE=local`), end to end on the real `steve-hub` binary: a temp
//! STEVE_HOME, its own port, the fake ACP agent (`steve-acp/tests/fake-acp.js`, which logs every
//! message it gets), real HTTP. Covers D5's acceptance on one Hub home:
//! - a session created with the Node on is bound to it and its transcript is stored as `steve.v1`;
//! - a permission request becomes a card, its answer reaches the agent;
//! - a cancel ends the running turn;
//! - while held (upgrade drain), a message is queued and not drained, and goes once released;
//! - the Hub killed mid-turn comes back with the attempt `unknown` (awaiting reconciliation),
//!   which is never re-run;
//! - with `STEVE_NODE` off, a Node-bound session is parked, never handed to the legacy runner.
//!
//! And on a second home, with a short attempt deadline, a card nobody answers expires; on a
//! third, an idle Node session goes through two Hub restarts without a resume or a restart notice.
//!
//! No real host, no real agent, never `~/.steve` or the production port.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
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

fn spawn(h: &Home, node: bool, extra: &[(&str, &str)]) -> Hub {
    let fake = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../steve-acp/tests/fake-acp.js"
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_steve-hub"));
    c.env("HOME", h.dir.path())
        .env("STEVE_HOME", h.steve())
        .env("STEVE_PORT", h.port.to_string())
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
        .env_remove("STEVE_NODE")
        .env_remove("STEVE_NODE_TURN_LIMIT_MS")
        .env_remove("STEVE_NO_RESUME")
        .env_remove("STEVE_UPGRADE_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("STEVE_KB_STORE")
        .env_remove("STEVE_OPENVIKING_KEY_FILE")
        .env_remove("STEVE_OPENVIKING_URL")
        .env_remove("STEVE_OPENVIKING_ACCOUNT")
        .env_remove("STEVE_OIDC_ISSUER")
        .stdout(Stdio::null())
        .stderr(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(h.dir.path().join("hub.log"))
                .expect("hub log"),
        )
        .process_group(0);
    if node {
        c.env("STEVE_NODE", "local");
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

    // ── the Hub killed mid-turn: the attempt is unknown afterwards and never re-run
    c.say(&sid, "崩溃验收-K2 @sleep 600000").await;
    assert!(wait(30, || h.got("崩溃验收-K2") == 1).await);
    hub.kill();
    let mut hub = spawn(&h, true, &[]);
    serving(&h).await;
    assert!(
        wait(30, || attempts(&h, &sid, "unknown") == 1).await,
        "the cut attempt is unknown; log:\n{}",
        hub_log(&h)
    );
    assert!(
        wait(10, || steve_v1(&h, &sid, "等待对账") > 0).await,
        "the session says it awaits reconciliation"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.got("崩溃验收-K2"), 1, "never re-run: {:?}", h.prompts());
    hub.kill();

    // ── STEVE_NODE off: the Node-bound session is parked, never handed to the legacy runner
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
    let mut hub = spawn(&h, true, &[]);
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
    let (before, fails_before) = (h.prompts().len(), fails());
    for round in 0..2 {
        hub.kill();
        hub = spawn(&h, true, &[]);
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

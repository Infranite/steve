//! Listener-ready recovery, end to end on the real `steve-hub` binary with the local Node (the
//! default): a temp STEVE_HOME, the fake ACP agent (`steve-acp/tests/fake-acp.js`, which logs every
//! prompt it gets), real HTTP. The Hub is killed (SIGKILL, with its Node and agent) and started
//! again on the same home:
//! - a turn the kill cut is never sent again: the Node's reconcile leaves its attempt unknown, and
//!   nothing resumes it on the Hub (B7: there is no runner of its own to do that);
//! - a recovery that fails does not stop the Hub: `/healthz` answers, `recovery` says what failed,
//!   the rest of the recovery (the Node's reconcile) still runs;
//! - behind an upgrade gate (maintenance) nothing is sent; once the operation is committed, or the
//!   maintenance lifted, the Hub binds its listener and only then sends what waited.
//!
//! No real host, no real agent.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const TOKEN: &str = "恢复验证-UNIQ-7F3";
/// What the old boot resume put before an interrupted turn: never sent any more.
const RESUME_HEAD: &str = "Hub 刚重启，上一回合被中断了";

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
        let log = dir.path().join("acp.log");
        Home {
            port: free_port(),
            log,
            web,
            dir,
        }
    }
    fn steve(&self) -> PathBuf {
        self.dir.path().join("steve")
    }
    fn db(&self) -> PathBuf {
        self.steve().join("data/steve.sqlite3")
    }
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
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
}

const VERSION: &str = "0.9.0";

/// A running Hub process (its own process group: killing it takes its agent along).
struct Hub(Child);

impl Hub {
    /// SIGKILL the Hub and its agents. The Hub puts each agent in its own process group, so they
    /// are found as its children first (and their groups killed), then the Hub's group.
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

fn spawn(h: &Home, upgrade: Option<&str>) -> Hub {
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
        // a turn that ends without a delivery is not continued by Steve: the queue stays the test's
        .env("STEVE_TAKEOVER_CONTINUES", "0")
        .env_remove("STEVE_NO_RESUME")
        // the local Node, the default
        // the Node in the Hub's process: killing the Hub cuts its turns, which is what is tested
        .env("STEVE_NODE_MODE", "local")
        .env_remove("STEVE_NODE")
        .env_remove("STEVE_UPGRADE_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        // the machine's own knowledge store (STEVE_KB_STORE=openviking on a Steve host) is not the test's
        .env_remove("STEVE_KB_STORE")
        .env_remove("STEVE_OPENVIKING_KEY_FILE")
        .env_remove("STEVE_OPENVIKING_URL")
        .env_remove("STEVE_OPENVIKING_ACCOUNT")
        .env_remove("STEVE_OIDC_ISSUER")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(id) = upgrade {
        c.env("STEVE_UPGRADE_ID", id);
    }
    Hub(c.spawn().expect("spawn hub"))
}

async fn health(h: &Home) -> Option<Value> {
    let r = reqwest::Client::new()
        .get(format!("{}/healthz", h.base()))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
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

/// The first `/healthz` answer that satisfies `ok` within `secs`.
async fn health_until(h: &Home, secs: u64, ok: impl Fn(&Value) -> bool) -> Value {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut last = None;
    while tokio::time::Instant::now() < end {
        last = health(h).await;
        if last.as_ref().is_some_and(&ok) {
            return last.expect("health");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no such /healthz within {secs}s: {last:?}");
}

/// Up and serving the full app (not the upgrade gate).
async fn serving(h: &Home) -> Value {
    health_until(h, 60, |v| v["ok"] == true && v["maintenance"] != true).await
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
}

/// The claude-code bot's id.
async fn bot(c: &Client) -> String {
    let (_, state) = c.call("GET", "/api/state", None).await;
    state["bots"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
        .and_then(|b| b["id"].as_str())
        .expect("a claude-code bot")
        .to_string()
}

/// A new item running `goal`; its session once the fake agent got it.
async fn item(h: &Home, c: &Client, goal: &str) -> String {
    let bot = bot(c).await;
    let (st, i) = c
        .call(
            "POST",
            "/api/items",
            Some(json!({"goal": goal, "title": "恢复验证", "bot": bot, "project": null})),
        )
        .await;
    assert_eq!(st, 200, "{i}");
    let item = i["id"].as_str().expect("item").to_string();
    let needle = goal.split(' ').next().unwrap_or(goal).to_string();
    assert!(
        until(30, || async {
            h.prompts().iter().any(|p| p.contains(&needle))
        })
        .await,
        "the agent got the first turn"
    );
    one(h, "SELECT id FROM sessions WHERE item_id=?", &item).expect("session")
}

fn one(h: &Home, sql: &str, p: &str) -> Option<String> {
    rusqlite::Connection::open(h.db())
        .expect("db")
        .query_row(sql, [p], |r| r.get(0))
        .ok()
}

fn count(h: &Home, sql: &str, p: &str) -> i64 {
    rusqlite::Connection::open(h.db())
        .expect("db")
        .query_row(sql, [p], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// First boot: an item whose first turn is running on the fake agent (sleeping), then the Hub dies.
async fn interrupted_turn(h: &Home) -> String {
    let mut hub = spawn(h, None);
    serving(h).await;
    let c = Client::login(h).await;
    let sid = item(h, &c, &format!("{TOKEN} 做一件长任务 @sleep 600000")).await;
    hub.kill();
    sid
}

/// First boot: an item whose first turn has ended (idle on the Node), then the Hub dies.
async fn idle_session(h: &Home) -> String {
    let mut hub = spawn(h, None);
    serving(h).await;
    let c = Client::login(h).await;
    let sid = item(h, &c, "空闲验证-I4 @say done").await;
    assert!(
        until(30, || async {
            count(
                h,
                "SELECT COUNT(*) FROM runtime_turns WHERE session_id=? AND state='finished'",
                &sid,
            ) >= 1
                && count(
                    h,
                    "SELECT COUNT(*) FROM runtime_turns WHERE session_id=? AND state<>'finished'",
                    &sid,
                ) == 0
        })
        .await,
        "its turn ended"
    );
    hub.kill();
    sid
}

/// [`idle_session`], with a message for it left queued, as one held for the restart is.
async fn idle_with_a_queued_message(h: &Home, text: &str) -> String {
    let sid = idle_session(h).await;
    rusqlite::Connection::open(h.db())
        .expect("db")
        .execute(
            "INSERT INTO queued(id,session_id,text,via,created) VALUES ('q-held',?1,?2,'me',1)",
            [&sid, text],
        )
        .expect("queued");
    sid
}

fn plain(h: &Home) -> usize {
    h.prompts().iter().filter(|p| p.contains(TOKEN)).count()
}

fn resumed(h: &Home) -> Vec<String> {
    h.prompts()
        .into_iter()
        .filter(|p| p.starts_with(RESUME_HEAD))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_cut_by_a_crash_is_never_sent_again() {
    let h = Home::new();
    let sid = interrupted_turn(&h).await;
    assert_eq!(plain(&h), 1);
    let _hub = spawn(&h, None);
    let v = serving(&h).await;
    assert_eq!(v["recovery"]["state"], "ok", "{v}");
    health_until(&h, 30, |v| v["node"]["reconciled"] == true).await;
    // B6: the restart cut it; the Hub gives it up (the Node confirms its process is gone)
    assert!(
        until(30, || async {
            count(&h, "SELECT COUNT(*) FROM node_commands WHERE method=?", "abandon") == 1
                && count(&h, "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND a.state='unknown'", &sid) == 0
        })
        .await,
        "the cut attempt is given up, not left unknown"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(plain(&h), 1, "never sent again: {:?}", h.prompts());
    assert!(resumed(&h).is_empty(), "no resume: {:?}", h.prompts());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_recovery_is_reported_and_the_hub_still_serves() {
    let h = Home::new();
    interrupted_turn(&h).await;
    // the main conversation's recovery cannot read its table on the next boot
    let db = rusqlite::Connection::open(h.db()).expect("db");
    db.execute_batch("ALTER TABLE thread_resume RENAME TO thread_resume_gone")
        .expect("break");
    drop(db);
    let _hub = spawn(&h, None);
    let v = serving(&h).await;
    assert_eq!(v["ok"], true);
    assert_eq!(v["recovery"]["state"], "failed", "{v}");
    let c = Client::login(&h).await;
    let (st, _) = c.call("GET", "/api/state", None).await;
    assert_eq!(st, 200, "the app serves");
    // the Node's recovery is not held back by the other one
    health_until(&h, 30, |v| v["node"]["reconciled"] == true).await;
    // and it gave up the turn the kill cut (B6)
    assert!(
        until(30, || async {
            count(&h, "SELECT COUNT(*) FROM node_commands WHERE method=?", "abandon") == 1
        })
        .await,
        "the cut turn is given up"
    );
}

fn atomic(p: &Path, v: &Value) {
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(v).expect("json")).expect("write");
    std::fs::rename(tmp, p).expect("rename");
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_sent_behind_the_upgrade_gate_and_what_waited_goes_once_committed() {
    let h = Home::new();
    let held = "门后排队-G7";
    idle_with_a_queued_message(&h, held).await;
    // an upgrade operation in verification for this version (the gate's maintenance)
    let id = uuid::Uuid::new_v4().to_string();
    let target = h.steve().join("releases").join(format!("{VERSION}-test"));
    let manifest = json!({
        "format_version": 1, "version": VERSION, "commit": "a".repeat(40),
        "hub_version": VERSION, "web_version": VERSION, "api_protocol": 1,
        "schema": {"min_read": steve_store::db::SCHEMA_VERSION, "max_read": steve_store::db::SCHEMA_VERSION, "target": steve_store::db::SCHEMA_VERSION},
        "assets": [],
    });
    atomic(&target.join("release.json"), &manifest);
    let op_path = h
        .steve()
        .join("updates/operations")
        .join(format!("{id}.json"));
    let op = |phase: &str| {
        json!({"id": id, "idempotency_key": "e2e", "version": VERSION, "phase": phase, "created_at": 1, "updated_at": 1,
               "requested_by": "test", "error": null, "previous": null, "target": target.to_string_lossy()})
    };
    atomic(&op_path, &op("verifying"));
    let before = h.prompts().len();
    let _hub = spawn(&h, Some(&id));
    let gate = health_until(&h, 60, |v| v["maintenance"] == true).await;
    assert_eq!(gate["upgrade_id"], json!(id));
    let r = reqwest::get(format!("{}/api/state", h.base()))
        .await
        .expect("request");
    assert_eq!(r.status().as_u16(), 503, "no app behind the gate");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(h.prompts().len(), before, "nothing is sent in maintenance");
    // committed: the gate opens, the app binds, then what waited goes
    atomic(&op_path, &op("committed"));
    let v = serving(&h).await;
    assert_eq!(v["recovery"]["state"], "ok", "{v}");
    assert!(
        until(30, || async {
            h.prompts().iter().any(|p| p.contains(held))
        })
        .await,
        "{:?}",
        h.prompts()
    );
    assert!(resumed(&h).is_empty());
}

/// The supervisor's control token (`updates/control-token.json`, a JSON string).
fn control_token(h: &Home) -> String {
    let t = "e2e-control-token-0123456789abcdef0123456789".to_string();
    atomic(&h.steve().join("updates/control-token.json"), &json!(t));
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

#[tokio::test(flavor = "multi_thread")]
async fn a_hub_stopped_in_maintenance_sends_nothing_until_maintenance_is_lifted() {
    let h = Home::new();
    let token = control_token(&h);
    let held = "维护排队-M2";
    // an idle session; then the upgrade drain starts, a message comes, and the Hub dies
    let sid = idle_session(&h).await;
    let mut hub = spawn(&h, None);
    serving(&h).await;
    let d = drain(&h, "POST", &token).await;
    assert_eq!(d["maintenance"], true, "{d}");
    assert_eq!(
        (&d["acp"], &d["adapters_stopped"]),
        (&json!(0), &json!(true)),
        "{d}"
    );
    // a message during the drain waits in the queue
    let c = Client::login(&h).await;
    let (st, v) = c
        .call(
            "POST",
            &format!("/api/sessions/{sid}/messages"),
            Some(json!({ "text": held })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!h.prompts().iter().any(|p| p.contains(held)), "held");
    hub.kill();
    // the next boot is still in maintenance (durable): it serves, but sends nothing
    let _hub = spawn(&h, None);
    serving(&h).await;
    let before = h.prompts().len();
    let d = drain(&h, "GET", &token).await;
    assert_eq!(d["maintenance"], true, "{d}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        h.prompts().len(),
        before,
        "nothing is sent in maintenance: {:?}",
        h.prompts()
    );
    // lifted: what waited goes
    let d = drain(&h, "DELETE", &token).await;
    assert_eq!(d["maintenance"], false, "{d}");
    assert!(
        until(30, || async {
            h.prompts().iter().any(|p| p.contains(held))
        })
        .await,
        "{:?}",
        h.prompts()
    );
    assert!(resumed(&h).is_empty());
}

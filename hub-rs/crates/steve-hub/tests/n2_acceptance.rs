//! N2 acceptance on the real `steve-hub` binary with remote Nodes on (`STEVE_NODE_LISTEN=1`,
//! `STEVE_NODE_BIND`), and a second Node in its own process with its own `STEVE_HOME`:
//! - the Hub has no harness (its adapters are `/usr/bin/false`); a new item's session is bound to
//!   the remote Node and runs a whole turn there over the real WebSocket (fake ACP), with a
//!   permission card answered on the Hub;
//! - the Hub killed mid-turn: the Node keeps the turn going, the restarted Hub catches up and the
//!   turn is never sent again; the Node killed mid-turn: the attempt is `unknown` afterwards and
//!   never re-run;
//! - another Node cannot speak for this Node's session (events, tools), nor dial as it.
//!
//! The Node process is this test binary run again in child mode ([`node_child`]): it runs what
//! the `steve-node` binary runs (`steve_node_cli::dial::{pair, run}`), since a test of this
//! package cannot rely on another package's binary being built.
//!
//! No real host, no real agent, never `~/.steve` or the production port.

mod node_harness;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;
use steve_node_wire::{Identity, RemoteSink, RemoteTools, identity};

const VERSION: &str = "0.9.0";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("port")
}

/// SIGKILL `pid`'s process group and every descendant's (adapters run in their own groups).
fn kill_tree(pid: i32) {
    fn descendants(pid: i32, out: &mut Vec<i32>) {
        let kids: Vec<i32> = Command::new("pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        for k in kids {
            descendants(k, out);
            out.push(k);
        }
    }
    let mut all = vec![];
    descendants(pid, &mut all);
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
        for c in &all {
            libc::killpg(*c, libc::SIGKILL);
            libc::kill(*c, libc::SIGKILL);
        }
    }
}

fn tail(p: &Path) -> String {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
}

// ── the Hub ──

struct Home {
    dir: tempfile::TempDir,
    port: u16,
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
    fn log(&self) -> String {
        tail(&self.dir.path().join("hub.log"))
    }
}

/// A running Hub process (its own process group).
struct Hub(Child);

impl Hub {
    fn kill(&mut self) {
        kill_tree(self.0.id() as i32);
        let _ = self.0.wait();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The Hub with remote Nodes on and no harness of its own; new sessions go to `bind` if given.
fn spawn_hub(h: &Home, bind: Option<&str>) -> Hub {
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
        .env("STEVE_NODE_LISTEN", "1")
        // no harness on the Hub's machine: whatever the Hub itself would start fails
        .env("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env_remove("FAKE_ACP_LOG")
        .env_remove("STEVE_NODE_MODE")
        .env_remove("STEVE_NODE_BIND")
        .env_remove("STEVE_NODE_TLS_PIN")
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
        .stdout(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(h.dir.path().join("hub.log"))
                .expect("hub log"),
        )
        .stderr(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(h.dir.path().join("hub.log"))
                .expect("hub log"),
        )
        .process_group(0);
    if let Some(b) = bind {
        c.env("STEVE_NODE_BIND", b);
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
        r.json::<Value>().await.is_ok_and(|v| v["ok"] == true)
    })
    .await;
    assert!(ok, "the Hub serves; log:\n{}", h.log());
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
    async fn say(&self, sid: &str, text: &str) {
        let (st, v) = self
            .call(
                "POST",
                &format!("/api/sessions/{sid}/messages"),
                Some(json!({"text": text})),
            )
            .await;
        assert_eq!(st, 200, "message: {v}");
    }
    /// A new item (no project) on the claude-code bot, and the session it opened.
    async fn item(&self, h: &Home, goal: &str) -> String {
        let (_, state) = self.call("GET", "/api/state", None).await;
        let bot = state["bots"]
            .as_array()
            .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
            .and_then(|b| b["id"].as_str())
            .expect("a claude-code bot")
            .to_string();
        self.item_on(h, goal, &bot).await
    }
    /// A new item (no project) on `bot`, and the session it opened.
    async fn item_on(&self, h: &Home, goal: &str, bot: &str) -> String {
        let (st, i) = self
            .call(
                "POST",
                "/api/items",
                Some(json!({"goal": goal, "title": "N2 验收", "bot": bot, "project": null})),
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
    async fn code(&self) -> String {
        let (st, v) = self
            .call("POST", "/api/nodes/pairings", Some(json!({})))
            .await;
        assert_eq!(st, 200, "{v}");
        v["code"].as_str().expect("code").to_string()
    }
    async fn confirm(&self, node: &str) {
        let (st, v) = self
            .call(
                "POST",
                &format!("/api/nodes/{node}/confirm"),
                Some(json!({})),
            )
            .await;
        assert_eq!(st, 200, "confirm: {v}");
    }
}

/// The agent's text entries of `sid` that contain `needle` (stored as `steve.v1`).
fn said(h: &Home, sid: &str, needle: &str) -> i64 {
    h.db()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='text' AND format='steve.v1' AND instr(data, ?) > 0",
            [sid, needle],
            |r| r.get(0),
        )
        .expect("events")
}

/// What the Hub holds about `sid` (for failing assertions).
fn dump(h: &Home, sid: &str) -> String {
    let db = h.db();
    let mut out = String::new();
    for sql in [
        "SELECT seq||' '||kind||' '||substr(data,1,160) FROM events WHERE session_id=?1 ORDER BY seq",
        "SELECT 'attempt '||a.attempt_id||' '||a.state FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1",
        "SELECT 'turn '||turn_id||' '||state FROM runtime_turns WHERE session_id=?1",
        "SELECT 'cmd '||operation_id||' '||method||' '||state FROM node_commands WHERE json_extract(request,'$.body.session')=?1 OR json_extract(request,'$.body')=?1 OR target_attempt IN (SELECT a.attempt_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?1)",
    ] {
        let mut st = db.prepare(sql).expect("dump");
        for r in st
            .query_map([sid], |r| r.get::<_, String>(0))
            .expect("dump")
        {
            out.push_str(&r.expect("row"));
            out.push('\n');
        }
    }
    out
}

fn attempts(h: &Home, sid: &str, state: &str) -> i64 {
    h.db()
        .query_row(
            "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND a.state=?",
            [sid, state],
            |r| r.get(0),
        )
        .expect("attempts")
}

// ── the second Node ──

/// A Node's home: the scripted adapter as its only harness, nothing shared with the Hub's.
struct NodeHome {
    dir: tempfile::TempDir,
}

impl NodeHome {
    fn new() -> NodeHome {
        let dir = tempfile::tempdir().expect("tmp");
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
        let adapter = dir.path().join("adapter.sh");
        std::fs::write(
            &adapter,
            format!(
                "#!/bin/sh\nexport FAKE_ACP_LOG='{}'\nexec node '{}' \"$@\"\n",
                dir.path().join("acp.log").display(),
                fake.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            dir.path().join("harness.json"),
            json!({"adapters": {"claude-code": adapter}}).to_string(),
        )
        .unwrap();
        NodeHome { dir }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    /// Prompts the Node's agent got that contain `token`.
    fn got(&self, token: &str) -> usize {
        std::fs::read_to_string(self.path().join("acp.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["method"] == "session/prompt" && m.to_string().contains(token))
            .count()
    }
    fn log(&self) -> String {
        tail(&self.path().join("node.log"))
    }
    fn cmd(&self, mode: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().expect("test binary"));
        c.args(["--exact", "node_child", "--nocapture", "--test-threads=1"])
            .env("STEVE_N2_CHILD", mode)
            .env("HOME", self.path())
            .env("STEVE_HOME", self.path())
            .env("STEVE_NODE_LOOPBACK_PLAINTEXT", "1")
            .env_remove("STEVE_PAIR_CODE")
            .env_remove("FAKE_ACP_LOG")
            .stdin(Stdio::null());
        c
    }
    /// `steve-node pair <hub>` with the code in the environment: the Node's id.
    fn pair(&self, hub: &Home, code: &str) -> String {
        let out = self
            .cmd("pair")
            .env("STEVE_N2_HUB", hub.base())
            .env("STEVE_PAIR_CODE", code)
            .output()
            .expect("pair");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        text.lines()
            // libtest may print `test node_child ... ` on the same line first
            .find_map(|l| l.split_once("N2-PAIRED ").map(|(_, id)| id))
            .expect("paired")
            .trim()
            .to_string()
    }
    /// `steve-node run`, in its own process group.
    fn run(&self) -> NodeProc {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path().join("node.log"))
            .expect("node log");
        let child = self
            .cmd("run")
            .stdout(log.try_clone().expect("log"))
            .stderr(log)
            .process_group(0)
            .spawn()
            .expect("spawn node");
        NodeProc(child)
    }
}

struct NodeProc(Child);

impl NodeProc {
    fn kill(&mut self) {
        kill_tree(self.0.id() as i32);
        let _ = self.0.wait();
    }
    fn alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for NodeProc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The `steve-node` binary's commands, when this test binary is started as a Node; a no-op in
/// a normal test run.
#[tokio::test(flavor = "multi_thread")]
async fn node_child() {
    let Ok(mode) = std::env::var("STEVE_N2_CHILD") else {
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    let home = steve_node_cli::config::Home::from_env().expect("home");
    match mode.as_str() {
        "pair" => {
            let url = std::env::var("STEVE_N2_HUB").expect("hub url");
            let code = std::env::var("STEVE_PAIR_CODE").expect("code");
            let p = steve_node_cli::dial::pair(&home, &url, &code, None, "second")
                .await
                .expect("pair");
            println!("N2-PAIRED {}", p.node_id);
        }
        "run" => {
            if let Err(e) = steve_node_cli::dial::run(&home).await {
                eprintln!("steve-node: {e:#}");
                std::process::exit(1);
            }
        }
        _ => panic!("unknown child mode {mode}"),
    }
}

/// A Hub whose new sessions go to a freshly paired second Node, which is running and linked.
async fn setup() -> (Home, Hub, NodeHome, NodeProc, String) {
    let h = Home::new();
    let mut hub = spawn_hub(&h, None);
    serving(&h).await;
    let c = Client::login(&h).await;
    let n = NodeHome::new();
    let code = c.code().await;
    let node = n.pair(&h, &code);
    c.confirm(&node).await;
    // the owner names the Node new sessions go to, and restarts the Hub with it
    hub.kill();
    let hub = spawn_hub(&h, Some(&node));
    serving(&h).await;
    let p = n.run();
    assert!(
        wait(30, || {
            h.count("SELECT generation FROM nodes WHERE node_id=?", &node) > 0
        })
        .await,
        "the Node linked; hub:\n{}\nnode:\n{}",
        h.log(),
        n.log()
    );
    (h, hub, n, p, node)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_node_runs_a_whole_item() {
    let (h, _hub, n, mut p, node) = setup().await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "远程验收 @say hello-remote-5R").await;
    assert!(
        wait(60, || said(&h, &sid, "hello-remote-5R") > 0).await,
        "the agent's reply came back over the link; hub:\n{}\nnode:\n{}",
        h.log(),
        n.log()
    );
    assert_eq!(
        h.one(
            "SELECT node_id FROM runtime_sessions WHERE session_id=?",
            &sid
        )
        .as_deref(),
        Some(node.as_str()),
        "bound to the second Node"
    );
    assert!(wait(10, || attempts(&h, &sid, "finished") >= 1).await);
    assert_eq!(n.got("hello-remote-5R"), 1, "the Node's agent got it once");

    // a permission request becomes a card on the Hub; its answer reaches the Node's agent
    c.say(&sid, "@perm Bash").await;
    let mut card = None;
    assert!(
        wait(60, || {
            card = h.one(
                "SELECT id FROM decisions WHERE session_id=? AND subject_type='tool_call' AND status='open'",
                &sid,
            );
            card.is_some()
        })
        .await,
        "a permission card; hub:\n{}\nnode:\n{}",
        h.log(),
        n.log()
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
        wait(60, || said(&h, &sid, "perm:selected:") > 0).await,
        "the Node's agent got the answer; node:\n{}",
        n.log()
    );

    // The default local model-job host may exist, but this item's turns and transcript belong
    // only to the remote Node.
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND format<>'steve.v1'",
            &sid
        ),
        0
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id=? AND a.node_id='local'",
            &sid
        ),
        0,
        "the item's turns never ran on the local model-job host"
    );
    assert!(n.path().join("node/local").exists(), "the Node's own home");
    assert!(p.alive());
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_restart_keeps_the_work_going_and_nothing_runs_twice() {
    let (h, mut hub, n, mut p, node) = setup().await;
    let c = Client::login(&h).await;
    let sid = c
        .item(&h, "重启验收 @say one-8T @sleep 6000 @say two-8T")
        .await;
    assert!(
        wait(60, || said(&h, &sid, "one-8T") > 0).await,
        "the turn started on the Node; hub:\n{}\nnode:\n{}",
        h.log(),
        n.log()
    );
    // the Hub goes away mid-turn; the Node keeps running the turn and comes back to the new Hub
    hub.kill();
    let _hub = spawn_hub(&h, Some(&node));
    serving(&h).await;
    assert!(
        wait(90, || said(&h, &sid, "two-8T") > 0).await,
        "the rest of the turn reached the restarted Hub; hub:\n{}\nnode:\n{}",
        h.log(),
        n.log()
    );
    assert!(
        // the turn, and Steve's own follow-ups after it (the agent delivered nothing)
        wait(30, || attempts(&h, &sid, "finished") >= 1).await,
        "{}\nhub:\n{}\nnode:\n{}",
        dump(&h, &sid),
        h.log(),
        n.log()
    );
    assert!(p.alive(), "the Node kept running");
    assert_eq!(n.got("one-8T"), 1, "never sent again");
    assert_eq!(
        said(&h, &sid, "one-8T"),
        1,
        "stored once: {}",
        dump(&h, &sid)
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM node_commands WHERE method='start_turn' AND instr(request, 'one-8T') > 0 AND json_extract(request,'$.body.session')=?",
            &sid
        ),
        1,
        "{}",
        dump(&h, &sid)
    );
    assert_eq!(attempts(&h, &sid, "unknown"), 0, "{}", dump(&h, &sid));

    // the Node killed mid-turn: what it cannot tell the end of is unknown, never re-run
    let c = Client::login(&h).await;
    c.say(&sid, "断电验收-K7 @sleep 600000").await;
    assert!(wait(60, || n.got("断电验收-K7") == 1).await);
    p.kill();
    let _p = n.run();
    assert!(
        wait(90, || attempts(&h, &sid, "unknown") == 1).await,
        "the cut attempt is unknown; hub:\n{}\nnode:\n{}",
        h.log(),
        n.log()
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(n.got("断电验收-K7"), 1, "never re-run");
}

/// Pairs an in-process Node (a fresh key) the way `steve-node pair` does; its id.
async fn pair_in_process(h: &Home, c: &Client, id: &Identity) -> NodeId {
    let code = c.code().await;
    let pk = id.public();
    let sig = id.sign(&identity::pair_message(&code, &pk));
    let r = reqwest::Client::new()
        .post(format!("{}/node/pair", h.base()))
        .json(&json!({"code": code, "pubkey": hex::encode(pk), "name": "intruder", "sig": hex::encode(sig)}))
        .send()
        .await
        .expect("pair");
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = r.json().await.expect("json");
    let node = v["node_id"].as_str().expect("node id").to_string();
    c.confirm(&node).await;
    NodeId::new(node).expect("id")
}

#[tokio::test(flavor = "multi_thread")]
async fn another_nodes_session_is_refused() {
    let (h, mut hub, n, _p, node) = setup().await;
    let c = Client::login(&h).await;
    let sid = c.item(&h, "隔离验收 @say iso-3M @sleep 600000").await;
    assert!(
        wait(60, || n.got("iso-3M") == 1).await,
        "node:\n{}",
        n.log()
    );
    let exec: ExecutionRef = serde_json::from_str(
        &h.one(
            "SELECT a.execution FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?",
            &sid,
        )
        .expect("attempt"),
    )
    .expect("execution");

    // a second paired Node, linked over the real listener
    let idb = Identity::generate().expect("key");
    let b = pair_in_process(&h, &c, &idb).await;
    let hub_id = h
        .one(
            "SELECT value FROM settings WHERE workspace_id='default' AND key=?",
            "node.hub_id",
        )
        .expect("hub id");
    let ws = format!("ws://127.0.0.1:{}/node/ws", h.port);
    let fb = FakeNode::new(b.as_str());
    let lb = node_harness::dial_url(&ws, &hub_id, &idb, &b, fb.clone(), 1)
        .await
        .expect("B links");
    fb.connect(Arc::new(RemoteSink(lb.link.clone())));
    let before = h.count("SELECT COUNT(*) FROM events WHERE session_id=?", &sid);

    // B reports text for A's attempt: refused whole, nothing stored, audited
    let e = RemoteSink(lb.link.clone())
        .commit(EventBatch {
            events: vec![NodeEvent {
                schema: 1,
                id: EventId::new("forged_1").unwrap(),
                at: Timestamp(1),
                cursor: Watermark {
                    stream: StreamId::new("st_forged").unwrap(),
                    epoch: Epoch::new("e1").unwrap(),
                    seq: 1,
                },
                subject: Subject::Execution(exec.clone()),
                body: EventBody::TextDelta {
                    message: "m".into(),
                    block: "b".into(),
                    utf8_offset: 0,
                    text: "I am A".into(),
                },
            }],
        })
        .await
        .expect_err("refused");
    assert_eq!(e.code, ErrorCode::Denied);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM events WHERE session_id=?", &sid),
        before
    );
    assert_eq!(said(&h, &sid, "I am A"), 0);
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM node_audit WHERE node_id=? AND what='foreign_event'",
            b.as_str()
        ),
        1
    );

    // B calls Steve's tools as A's attempt: refused
    let r = RemoteTools(lb.link.clone())
        .call(ToolCall {
            id: CallId::new("c1").unwrap(),
            execution: exec.clone(),
            catalog: Revision::new("c1").unwrap(),
            name: "steve_update".into(),
            args: json!({"now": "x"}),
            deadline: Timestamp(i64::MAX / 2),
        })
        .await;
    assert!(
        matches!(&r, Err(e) if e.code == ErrorCode::Denied)
            || matches!(&r, Ok(ToolResult::Failed { error }) if error.code == ErrorCode::Denied),
        "{r:?}"
    );

    // B's own work, as the Hub would hold it (a session, turn, attempt and command bound to B),
    // so B's events pass the subject check: only the ids in their bodies point at A's session
    {
        let w = rusqlite::Connection::open(h.steve().join("data/steve.sqlite3")).expect("db");
        w.busy_timeout(Duration::from_secs(10)).unwrap();
        w.execute_batch(&format!(
            "CREATE TEMP TABLE sb AS SELECT * FROM sessions WHERE id='{sid}';
             UPDATE sb SET id='s_b_own', item_id=NULL;
             INSERT INTO sessions SELECT * FROM sb;
             INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES ('s_b_own','{b}','{{}}','s_b_own','r','ready',0);
             INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES ('t_b_own','s_b_own','running',0,0);
             INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated) VALUES ('op_b_own','{b}','start_turn','{{}}','d','finished',0,0);
             INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a_b_own','t_b_own',0,'{b}','op_b_own','{{}}','{{}}','d','running',0,0);"
        ))
        .expect("B's own rows");
    }
    // B has a session here now, so a Hub restart starts B's coordinator: B's events reach it
    hub.kill();
    let _hub = spawn_hub(&h, Some(&node));
    serving(&h).await;
    let fb = FakeNode::new(b.as_str());
    let lb = node_harness::dial_url(&ws, &hub_id, &idb, &b, fb.clone(), 1)
        .await
        .expect("B links again");
    fb.connect(Arc::new(RemoteSink(lb.link.clone())));
    let own_b = ExecutionRef::Turn {
        session: SessionId::new("s_b_own").unwrap(),
        turn: TurnId::new("t_b_own").unwrap(),
        attempt: AttemptId::new("a_b_own").unwrap(),
    };
    let spoof = |n: u64, subject: Subject, body: EventBody| EventBatch {
        events: vec![NodeEvent {
            schema: 1,
            id: EventId::new(format!("spoof_{n}")).unwrap(),
            at: Timestamp(1),
            cursor: Watermark {
                stream: StreamId::new(format!("st_spoof_{n}")).unwrap(),
                epoch: Epoch::new("e1").unwrap(),
                seq: 1,
            },
            subject,
            body,
        }],
    };
    let a_revision = h.one(
        "SELECT node_revision FROM runtime_sessions WHERE session_id=?",
        &sid,
    );
    let a_op = h
        .one(
            "SELECT a.operation_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?",
            &sid,
        )
        .expect("A's operation");
    let a_receipt = h.one(
        "SELECT receipt FROM node_commands WHERE operation_id=?",
        &a_op,
    );
    let action = Action::Delete { paths: vec![] };
    let spoofs = [
        // a card on A's session from B's own turn: the request names A's session
        spoof(
            1,
            Subject::Execution(own_b.clone()),
            EventBody::PermissionRequested {
                request: PermissionRequest {
                    id: PermissionId::new("perm_spoof").unwrap(),
                    execution: ExecutionRef::Turn {
                        session: SessionId::new(sid.clone()).unwrap(),
                        turn: TurnId::new("t_b_own").unwrap(),
                        attempt: AttemptId::new("a_b_own").unwrap(),
                    },
                    invocation: CallId::new("call_spoof").unwrap(),
                    title: "rm -rf spoofed".into(),
                    digest: permission_digest(&action, &[]).unwrap(),
                    action,
                    preconditions: vec![],
                    choices: vec![PermissionDecision::AllowOnce, PermissionDecision::Reject],
                    expires_at: Timestamp(i64::MAX / 2),
                    own_pids: vec![],
                },
            },
        ),
        // B's own session reports a view of A's session (its revision)
        spoof(
            2,
            Subject::Session {
                session: SessionId::new("s_b_own").unwrap(),
            },
            EventBody::SessionChanged {
                view: SessionView {
                    id: SessionId::new(sid.clone()).unwrap(),
                    revision: Revision::new("rev_spoofed").unwrap(),
                    phase: SessionPhase::Released,
                    active: None,
                    effective: HarnessChoice {
                        harness: "codex".into(),
                        model: Some("spoofed-model".into()),
                        effort: None,
                    },
                    effective_permissions: PermissionMode::Ask,
                    guarantees: vec![],
                    resume: None,
                },
            },
        ),
        // B's own operation carries the receipt of A's turn
        spoof(
            3,
            Subject::Operation {
                operation: OperationId::new("op_b_own").unwrap(),
            },
            EventBody::OperationChanged {
                receipt: Receipt {
                    operation: OperationId::new(a_op.clone()).unwrap(),
                    state: Observed::Unknown {
                        reason: "spoofed".into(),
                    },
                },
            },
        ),
    ];
    for batch in spoofs {
        let e = RemoteSink(lb.link.clone())
            .commit(batch.clone())
            .await
            .expect_err("a body naming another node's work is refused");
        assert_eq!(e.code, ErrorCode::Denied, "{batch:?}");
    }
    assert_eq!(
        h.count("SELECT COUNT(*) FROM decisions WHERE session_id=?", &sid),
        0,
        "no card on A's session"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='permission'",
            &sid
        ),
        0
    );
    assert_eq!(
        h.one(
            "SELECT node_revision FROM runtime_sessions WHERE session_id=?",
            &sid
        ),
        a_revision,
        "A's session revision untouched"
    );
    assert_eq!(
        h.one(
            "SELECT receipt FROM node_commands WHERE operation_id=?",
            &a_op
        ),
        a_receipt,
        "A's receipt untouched"
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM node_audit WHERE node_id=? AND what='foreign_event'",
            b.as_str()
        ),
        4
    );

    // another Node's own process claiming to be A is refused at the handshake, and stops
    let nb = NodeHome::new();
    let code = c.code().await;
    let real_b = nb.pair(&h, &code);
    c.confirm(&real_b).await;
    let f = nb.path().join("node/node.json");
    let mut pairing: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
    pairing["node_id"] = json!(node);
    std::fs::write(&f, pairing.to_string()).unwrap();
    let mut impostor = nb.run();
    let t0 = std::time::Instant::now();
    let status = loop {
        if let Ok(Some(s)) = impostor.0.try_wait() {
            break s;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "refused, not retried forever; node:\n{}",
            nb.log()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(!status.success());
    assert!(nb.log().contains("refused"), "{}", nb.log());
    // and A's turn is untouched
    assert_eq!(n.got("iso-3M"), 1);
    assert_eq!(attempts(&h, &sid, "unknown"), 0);
}

/// B1a: a Node of an earlier release (protocol 0.1) cannot decode the `auto` permission mode. A
/// new session of an `auto` Bot is not bound to it (it would refuse the open, and every later
/// message with it), while other Bots' sessions still are; once the Node links speaking the
/// protocol that has `auto`, the next `auto` session is bound to it.
#[tokio::test(flavor = "multi_thread")]
async fn an_auto_bot_session_skips_a_remote_node_that_predates_auto() {
    let h = Home::new();
    let mut hub = spawn_hub(&h, None);
    serving(&h).await;
    let c = Client::login(&h).await;
    let id = Identity::generate().expect("key");
    let node = pair_in_process(&h, &c, &id).await;
    hub.kill();
    let _hub = spawn_hub(&h, Some(node.as_str()));
    serving(&h).await;
    let c = Client::login(&h).await;
    let hub_id = h
        .one(
            "SELECT value FROM settings WHERE workspace_id='default' AND key=?",
            "node.hub_id",
        )
        .expect("hub id");
    let ws = format!("ws://127.0.0.1:{}/node/ws", h.port);
    let old = (PROTOCOL.0, AUTO_MODE.1 - 1);
    let fake = FakeNode::new(node.as_str());
    let link = node_harness::dial_url_as(&ws, &hub_id, &id, &node, fake.clone(), 1, old)
        .await
        .expect("an older Node links");
    fake.connect(Arc::new(RemoteSink(link.link.clone())));
    let bot = |perm: &'static str| {
        let c = &c;
        async move {
            let (st, v) = c
                .call(
                    "POST",
                    "/api/bots",
                    Some(json!({"name": format!("B1a {perm}"), "harness": "claude-code", "perm_mode": perm})),
                )
                .await;
            assert_eq!(st, 200, "{v}");
            v["id"].as_str().expect("bot id").to_string()
        }
    };
    let (auto, ask) = (bot("auto").await, bot("default").await);
    let on_node = |sid: &str| {
        h.count(
            &format!(
                "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?1 AND node_id='{node}'"
            ),
            sid,
        )
    };

    let plain = c.item_on(&h, "B1a 普通", &ask).await;
    assert!(
        wait(30, || on_node(&plain) == 1).await,
        "a default Bot's session goes to the older Node; hub:\n{}",
        h.log()
    );
    let sid = c.item_on(&h, "B1a 自动", &auto).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        on_node(&sid),
        0,
        "an auto Bot's session stays off the older Node; hub:\n{}",
        h.log()
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM node_commands WHERE json_extract(request,'$.body.policy.permissions')='auto' AND node_id=?",
            node.as_str()
        ),
        0,
        "nothing in auto is ever sent to it"
    );

    // the same Node, upgraded: its next link speaks auto
    link.conn
        .close(None, steve_node_wire::close::GOODBYE, "upgrade");
    let fake = FakeNode::new(node.as_str());
    let link = node_harness::dial_url(&ws, &hub_id, &id, &node, fake.clone(), 2)
        .await
        .expect("the upgraded Node links");
    fake.connect(Arc::new(RemoteSink(link.link.clone())));
    let sid = c.item_on(&h, "B1a 自动 升级后", &auto).await;
    assert!(
        wait(30, || on_node(&sid) == 1).await,
        "an auto Bot's session goes to a Node that speaks auto; hub:\n{}",
        h.log()
    );
}

//! A7: the local Node (`STEVE_NODE_MODE=local`) against the real Claude, end to end on the real
//! `steve-hub` binary. They cost model calls and use the user's Claude login (read-only), so every
//! case is `#[ignore]`d and run by hand only:
//!
//! `cargo test -p steve-hub --test node_real -- --ignored --nocapture --test-threads=1`
//!
//! Each case gets a fresh temp STEVE_HOME, its own port and a throwaway git project; never
//! `~/.steve` or the production port. The adapter and the claude executable are the real ones
//! (`STEVE_ADAPTER_CLAUDE` / `CLAUDE_CODE_EXECUTABLE` from the environment, else the Hub's own
//! defaults), and `HOME` / `CLAUDE_CONFIG_DIR` are passed through so claude finds its login.
//! `STEVE_NO_RESUME` is unset: a copy cannot run the Node. `A7_MODEL` / `A7_EFFORT` pick the test
//! Bot's model and effort (default `haiku`, `low`).
//!
//! Cases:
//! 1. a hello turn is stored as steve.v1, with its usage and cost in agent_turns / usage_records;
//! 2. a Steve tool call (read-only) goes through ToolService, with its row in the host journal;
//! 3. a Bash permission card answered allow_once runs; a second one rejected does not;
//! 4. a cancel mid-turn ends the turn;
//! 5. a permission-mode change at idle (YOLO on: `default` → `acceptEdits`) is configured on the
//!    Node and the next edit asks nothing; the Bot's model and effort reach the harness with the
//!    open. Pending: a model/effort change at idle, which has no Hub trigger yet (see the case);
//! 6. a release (the Hub stopped at idle) then a reopen continues the same conversation;
//! 7. an image attachment arrives through HubFiles;
//! 8. the Hub killed mid-turn: the attempt is unknown and never re-run; Stop abandons it and the
//!    next message runs.
//!
//! Two checks of the harness itself run on the scripted agent and are not ignored: the test project
//! is a Node root from the first turn, and case 5's configure path.
//!
//! Cases 9 (a drain waits for the turn) and 10 (YOLO arms from the Node's own_pids) come once
//! their Hub side lands.

use std::io::Write as _;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const VERSION: &str = "0.9.0";
/// A real model turn, at most.
const TURN: u64 = 300;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("port")
}

/// A token no model guesses (time and pid based, unique per call).
fn token(tag: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!(
        "{tag}-{:X}{}",
        (t ^ u128::from(std::process::id())) % 0xFFFF_FFFF,
        n
    )
}

struct Home {
    dir: tempfile::TempDir,
    port: u16,
    web: PathBuf,
    /// The throwaway git project the items work in.
    project: PathBuf,
    /// The tokens in the project's `notes.txt` and `secret.txt` (cases 3 and 8).
    note: String,
    secret: String,
    /// `Some(log)`: the scripted ACP agent (`steve-acp/tests/fake-acp.js`, logging to `log`) stands
    /// in for claude, for the non-ignored checks of the harness itself.
    fake: Option<PathBuf>,
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
        let project = dir.path().join(token("a7proj").to_lowercase());
        std::fs::create_dir_all(&project).expect("project");
        let note = token("A7NOTE");
        std::fs::write(project.join("notes.txt"), format!("{note}\n")).expect("notes");
        let secret = token("A7SECRET");
        std::fs::write(project.join("secret.txt"), format!("{secret}\n")).expect("secret");
        std::fs::write(project.join("README.md"), "# A7 throwaway project\n").expect("readme");
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=a7", "-c", "user.email=a7@example.invalid"])
                .args(args)
                .current_dir(&project)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("git")
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        Home {
            port: free_port(),
            web,
            project,
            note,
            secret,
            fake: None,
            dir,
        }
    }
    fn fake() -> Home {
        let mut h = Home::new();
        h.fake = Some(h.dir.path().join("acp.log"));
        h
    }
    fn steve(&self) -> PathBuf {
        self.dir.path().join("steve")
    }
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    /// The test's own database (in the temp home), read-only.
    fn db(&self) -> rusqlite::Connection {
        let c = rusqlite::Connection::open_with_flags(
            self.steve().join("data/steve.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c
    }
    fn count(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) -> i64 {
        self.db()
            .query_row(sql, p, |r| r.get(0))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }
    fn one(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Option<String> {
        self.db().query_row(sql, p, |r| r.get(0)).ok()
    }
    /// The Node's tool journal (`data/node/host-tools.sqlite3`), read-only.
    fn journal(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) -> i64 {
        rusqlite::Connection::open_with_flags(
            self.steve().join("data/node/host-tools.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .and_then(|c| c.query_row(sql, p, |r| r.get(0)))
        .unwrap_or(0)
    }
}

/// A running Hub process (its own process group: killing it takes its agents along).
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
    /// SIGTERM: the Hub stops its Node and the Node its harnesses; SIGKILL after 60 s.
    fn stop(&mut self) {
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let end = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < end {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        self.kill();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The real Hub with the local Node on. Both switch names are set: `STEVE_NODE` (D5) and
/// `STEVE_NODE_MODE` (its N2 name). On a [`Home::fake`] the scripted agent replaces claude and
/// `HOME` is the temp dir, so nothing of the user's login is read.
fn spawn(h: &Home) -> Hub {
    let mut c = Command::new(env!("CARGO_BIN_EXE_steve-hub"));
    c.env("STEVE_HOME", h.steve())
        .env("STEVE_PORT", h.port.to_string())
        .env("STEVE_HOST", "127.0.0.1")
        .env("STEVE_PUBLIC_URL", h.base())
        .env("STEVE_DEV_LOGIN", "1")
        .env("STEVE_REPO", "/nonexistent")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_VERSION", VERSION)
        .env("STEVE_WEB_DIR", &h.web)
        .env("STEVE_NODE", "local")
        .env("STEVE_NODE_MODE", "local")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env_remove("STEVE_NODE_TURN_LIMIT_MS")
        .env_remove("STEVE_NO_RESUME")
        .env_remove("STEVE_YOLO")
        .env_remove("STEVE_UPGRADE_ID")
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
    if let Some(log) = &h.fake {
        let fake = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../steve-acp/tests/fake-acp.js"
        );
        c.env("HOME", h.dir.path())
            .env("FAKE_ACP_LOG", log)
            .env("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false")
            .env("STEVE_ADAPTER_CLAUDE", fake)
            .env_remove("CLAUDE_CONFIG_DIR");
    }
    Hub(c.spawn().expect("spawn hub"))
}

async fn until<F: std::future::Future<Output = bool>>(secs: u64, mut f: impl FnMut() -> F) -> bool {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < end {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
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
    let lines: Vec<&str> = s.lines().collect();
    let mut out = lines[lines.len().saturating_sub(60)..].join("\n");
    // the Node's side, which the Hub's log may not tell: its commands and their receipts
    if let Ok(db) = rusqlite::Connection::open_with_flags(
        h.steve().join("data/steve.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        let rows = db
            .prepare("SELECT method, state, COALESCE(receipt,'') FROM node_commands ORDER BY created, rowid")
            .and_then(|mut q| {
                q.query_map([], |r| {
                    Ok(format!(
                        "{} {} {}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default();
        out.push_str("\nnode_commands:\n");
        out.push_str(&rows.join("\n"));
    }
    out
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
    async fn send(&self, req: reqwest::RequestBuilder) -> (u16, Value) {
        let r = req
            .header("cookie", &self.cookie)
            .send()
            .await
            .expect("request");
        let st = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            st,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(
            reqwest::Method::from_bytes(method.as_bytes()).expect("method"),
            format!("{}{path}", self.base),
        );
        if let Some(b) = body {
            req = req.json(&b);
        }
        self.send(req).await
    }
    async fn ok(&self, method: &str, path: &str, body: Value) -> Value {
        let (st, v) = self.call(method, path, Some(body)).await;
        assert_eq!(st, 200, "{method} {path}: {v}");
        v
    }
    async fn say(&self, sid: &str, text: &str) -> Value {
        self.say_with(sid, text, &[]).await
    }
    async fn say_with(&self, sid: &str, text: &str, files: &[&str]) -> Value {
        let mut body = json!({"text": text});
        if !files.is_empty() {
            body["files"] = json!(files);
        }
        self.ok("POST", &format!("/api/sessions/{sid}/messages"), body)
            .await
    }
    async fn upload(&self, name: &str, bytes: Vec<u8>) -> String {
        let req = self
            .http
            .post(format!("{}/api/files?name={name}", self.base))
            .header("content-type", "application/octet-stream")
            .body(bytes);
        let (st, v) = self.send(req).await;
        assert_eq!(st, 200, "upload: {v}");
        v["id"].as_str().expect("file id").to_string()
    }
    async fn answer(&self, card: &str, choice: &str) {
        self.ok(
            "POST",
            &format!("/api/decisions/{card}"),
            json!({"choice": choice}),
        )
        .await;
    }
}

/// One case's world: a Hub with the Node on, logged in, a claude-code Bot asking for every
/// permission (`perm_mode` default) and a git project on worktrees.
struct Real {
    h: Home,
    hub: Hub,
    c: Client,
    bot: String,
    project: String,
}

fn model() -> String {
    std::env::var("A7_MODEL").unwrap_or_else(|_| "haiku".into())
}

fn effort() -> String {
    std::env::var("A7_EFFORT").unwrap_or_else(|_| "low".into())
}

impl Real {
    async fn start() -> Real {
        Real::start_on(Home::new()).await
    }

    async fn start_on(h: Home) -> Real {
        let hub = spawn(&h);
        serving(&h).await;
        let c = Client::login(&h).await;
        let mut bot = json!({"name": "A7 实测", "harness": "claude-code", "perm_mode": "default"});
        // the scripted agent reports no effort to set: it would refuse the open
        if h.fake.is_none() {
            bot["model"] = json!(model());
            bot["reasoning_effort"] = json!(effort());
        }
        let bot = c.ok("POST", "/api/bots", bot).await["id"]
            .as_str()
            .expect("bot id")
            .to_string();
        let project = c
            .ok(
                "POST",
                "/api/projects",
                json!({"name": "a7", "path": h.project.display().to_string(), "worktree": true}),
            )
            .await["id"]
            .as_str()
            .expect("project id")
            .to_string();
        eprintln!("A7 home {}", h.dir.path().display());
        // No restart is needed although the Node's roots are read at boot (wire/node_binding.rs
        // `roots`) and the project sits outside them: creating the item makes its worktree under
        // STEVE_HOME/worktrees, a boot root, and the Node prepares that existing directory. The
        // first non-ignored check below keeps it so.
        Real {
            h,
            hub,
            c,
            bot,
            project,
        }
    }

    /// The Hub again on the same home (after a stop or a kill), logged in again.
    async fn restart(&mut self) {
        self.hub = spawn(&self.h);
        serving(&self.h).await;
        self.c = Client::login(&self.h).await;
    }

    /// A new item on the test Bot and project; its goal is the first prompt. The session it opened.
    async fn item(&self, goal: &str) -> String {
        let i = self
            .c
            .ok(
                "POST",
                "/api/items",
                json!({"goal": goal, "title": "A7 实测", "bot": self.bot, "project": self.project}),
            )
            .await;
        let item = i["id"].as_str().expect("item").to_string();
        let mut sid = None;
        assert!(
            wait(60, || {
                sid = self
                    .h
                    .one("SELECT id FROM sessions WHERE item_id=?", &[&item]);
                sid.is_some()
            })
            .await,
            "a session for the item; log:\n{}",
            hub_log(&self.h)
        );
        let sid = sid.expect("session");
        assert!(
            wait(60, || self.h.count(
                "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=? AND node_id='local'",
                &[&sid]
            ) == 1)
            .await,
            "bound to the local Node; log:\n{}",
            hub_log(&self.h)
        );
        sid
    }

    /// The session's last event seq.
    fn seq(&self, sid: &str) -> i64 {
        self.h.count(
            "SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?",
            &[&sid],
        )
    }

    /// steve.v1 events of `sid` after `since`, not the user's own, that contain `needle`.
    fn said(&self, sid: &str, since: i64, needle: &str) -> i64 {
        self.h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND seq>? AND format='steve.v1' AND kind NOT IN ('user','queued') AND instr(data, ?) > 0",
            &[&sid, &since, &needle],
        )
    }

    fn turns(&self, sid: &str) -> i64 {
        self.h.count(
            "SELECT COUNT(*) FROM runtime_turns WHERE session_id=?",
            &[&sid],
        )
    }

    /// `n` turns so far and none open.
    fn done(&self, sid: &str, n: i64) -> bool {
        self.turns(sid) >= n
            && self.h.count(
                "SELECT COUNT(*) FROM runtime_turns WHERE session_id=? AND state<>'finished'",
                &[&sid],
            ) == 0
    }

    fn open_card(&self, sid: &str) -> Option<String> {
        self.h.one(
            "SELECT id FROM decisions WHERE session_id=? AND subject_type='tool_call' AND status='open'",
            &[&sid],
        )
    }

    fn cards(&self, sid: &str) -> i64 {
        self.h.count(
            "SELECT COUNT(*) FROM decisions WHERE session_id=? AND subject_type='tool_call'",
            &[&sid],
        )
    }

    fn attempts(&self, sid: &str, state: &str) -> i64 {
        self.h.count(
            "SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND a.state=?",
            &[&sid, &state],
        )
    }

    /// `configure_session` commands of `sid` the Node finished with success, with `permissions`.
    fn configured(&self, sid: &str, permissions: &str) -> i64 {
        self.h.count(
            "SELECT COUNT(*) FROM node_commands WHERE method='configure_session' AND json_extract(request,'$.body.session')=? AND json_extract(request,'$.body.policy.permissions')=? AND state='finished' AND json_extract(receipt,'$.success')=1",
            &[&sid, &permissions],
        )
    }

    /// Switches YOLO on for the workspace. With the Bot's `default` mode and the item in its own
    /// worktree, the session's harness mode becomes `acceptEdits`: today the only Hub path to
    /// `set_mode`, hence to the Node's `configure_session`.
    async fn yolo_on(&self) {
        self.c.ok("PUT", "/api/yolo", json!({"on": true})).await;
    }

    /// Waits for the session's permission mode to be configured on the Node, failing with the log.
    async fn configure_over(&self, sid: &str, permissions: &str) {
        assert!(
            wait(60, || self.configured(sid, permissions) == 1).await,
            "configured {permissions} on the Node; log:\n{}",
            hub_log(&self.h)
        );
        assert!(
            wait(30, || self
                .h
                .one(
                    "SELECT phase FROM runtime_sessions WHERE session_id=?",
                    &[&sid]
                )
                .as_deref()
                == Some("ready"))
            .await,
            "ready again after the configure; log:\n{}",
            hub_log(&self.h)
        );
    }

    /// Waits for `n` finished turns, failing with the log.
    async fn turn_over(&self, sid: &str, n: i64, what: &str) {
        assert!(
            wait(TURN, || self.done(sid, n)).await,
            "{what}: turn {n} over; log:\n{}",
            hub_log(&self.h)
        );
    }

    /// Waits for a permission card of `sid`.
    async fn card(&self, sid: &str, what: &str) -> String {
        let mut card = None;
        assert!(
            wait(TURN, || {
                card = self.open_card(sid);
                card.is_some()
            })
            .await,
            "{what}: a permission card; log:\n{}",
            hub_log(&self.h)
        );
        card.expect("card")
    }

    /// Waits for `n` finished turns, allowing once every card on the way (Steve's own tools may ask).
    async fn allowing_until_over(&self, sid: &str, n: i64, what: &str) {
        let end = tokio::time::Instant::now() + Duration::from_secs(TURN);
        while tokio::time::Instant::now() < end {
            if self.done(sid, n) {
                return;
            }
            if let Some(card) = self.open_card(sid) {
                self.c.answer(&card, "allow_once").await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("{what}: turn {n} over; log:\n{}", hub_log(&self.h));
    }
}

/// claude's transcripts of the session's working directory (under `$CLAUDE_CONFIG_DIR` or
/// `~/.claude`), read-only: the models that answered.
fn transcript_models(cwd: &str) -> Vec<String> {
    let root = std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").expect("HOME")).join(".claude"))
        .join("projects");
    let base = Path::new(cwd)
        .file_name()
        .map(|b| {
            b.to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>()
        })
        .unwrap_or_default();
    let mut out = vec![];
    for d in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if base.is_empty() || !d.file_name().to_string_lossy().ends_with(&base) {
            continue;
        }
        for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
            let Ok(t) = std::fs::read_to_string(f.path()) else {
                continue;
            };
            out.extend(
                t.lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .filter_map(|l| l.pointer("/message/model")?.as_str().map(str::to_owned)),
            );
        }
    }
    out
}

/// A `w`×`h` PNG of one colour (RGB, no filter), for the attachment case.
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
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
    // each row: filter type 0, then the pixels
    let mut row = vec![0u8];
    row.extend(rgb.repeat(w as usize));
    let raw = row.repeat(h as usize);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).expect("zlib");
    let idat = z.finish().expect("zlib");
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The session's working directory.
fn cwd(r: &Real, sid: &str) -> PathBuf {
    PathBuf::from(
        r.h.one("SELECT cwd FROM sessions WHERE id=?", &[&sid])
            .expect("cwd"),
    )
}

/// The harness itself, on the scripted agent (no model call, so not ignored): an item on the test
/// project, created after boot, runs on the Node from its first turn. The Node's roots are read at
/// boot and do not hold the project; this holds because the item's worktree is made under
/// STEVE_HOME/worktrees (a boot root) and the Node prepares it as an existing directory. If that
/// changes, every real case would fail before its first turn ("not a root this node allows").
#[tokio::test(flavor = "multi_thread")]
async fn the_test_project_is_a_node_root_from_the_first_turn() {
    let r = Real::start_on(Home::fake()).await;
    let sid = r.item("A7 根目录 @say rooted-A7").await;
    r.turn_over(&sid, 1, "first turn").await;
    assert!(r.said(&sid, 0, "rooted-A7") > 0, "the turn ran on the Node");
    assert_eq!(
        r.h.count(
            "SELECT COUNT(*) FROM node_commands WHERE method='prepare_workspace' AND state='finished' AND json_extract(receipt,'$.success')=1",
            &[]
        ),
        1,
        "the workspace prepared on the Node; log:\n{}",
        hub_log(&r.h)
    );
    let dir = cwd(&r, &sid);
    assert!(
        dir.starts_with(r.h.steve().join("worktrees"))
            || dir.starts_with(r.h.steve().canonicalize().expect("home").join("worktrees")),
        "a worktree of the project: {}",
        dir.display()
    );
    assert!(
        dir.join("notes.txt").exists(),
        "checked out from the project"
    );
}

/// Case 5's plumbing, on the scripted agent: YOLO switched on at idle reaches the Node session as
/// a `configure_session` to `accept_edits`, and the next turn runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_mode_change_at_idle_configures_the_node_session() {
    let r = Real::start_on(Home::fake()).await;
    let sid = r.item("A7 配置 @say first-A7").await;
    r.turn_over(&sid, 1, "first turn").await;
    assert_eq!(r.configured(&sid, "accept_edits"), 0);
    r.yolo_on().await;
    r.configure_over(&sid, "accept_edits").await;
    let seq = r.seq(&sid);
    r.c.say(&sid, "A7 配置之后 @say second-A7").await;
    r.turn_over(&sid, 2, "after the configure").await;
    assert!(r.said(&sid, seq, "second-A7") > 0, "the next turn ran");
}

/// 1. A hello turn is stored as steve.v1, with its usage and cost.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn a_hello_turn_is_steve_v1_with_usage_and_cost() {
    let r = Real::start().await;
    let pong = token("PONG");
    let sid = r
        .item(&format!(
            "Reply with exactly the word {pong} and nothing else. Do not use any tools."
        ))
        .await;
    r.turn_over(&sid, 1, "hello").await;
    assert!(
        r.said(&sid, 0, &pong) > 0,
        "the reply is stored as steve.v1"
    );
    assert_eq!(r.cards(&sid), 0, "no permission asked");
    assert!(r.attempts(&sid, "finished") >= 1, "ran as a Node attempt");
    assert_eq!(
        r.h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND format<>'steve.v1'",
            &[&sid]
        ),
        0,
        "nothing in the legacy format"
    );
    assert_eq!(
        r.h.count(
            "SELECT COUNT(*) FROM agent_turns WHERE session_id=? AND node_attempt_id IS NOT NULL AND ended IS NOT NULL",
            &[&sid]
        ),
        1,
        "one statistics row for the attempt"
    );
    let output = r.h.count(
        "SELECT COALESCE(SUM(output),0) FROM usage_records WHERE session_id=?",
        &[&sid],
    );
    let cost = r.h.one(
        "SELECT CAST(MAX(COALESCE(u.cost_reported,0), COALESCE(s.cost,0)) AS TEXT) FROM sessions s LEFT JOIN usage_records u ON u.session_id=s.id WHERE s.id=?",
        &[&sid],
    );
    eprintln!("case 1: output tokens {output}, cost {cost:?}");
    assert!(output > 0, "output tokens recorded");
    assert!(
        cost.as_deref()
            .and_then(|c| c.parse::<f64>().ok())
            .is_some_and(|c| c > 0.0),
        "a cost recorded: {cost:?}"
    );
}

/// 2. A read-only Steve tool goes through ToolService, once, with its row in the host journal.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn a_steve_tool_call_goes_through_tool_service() {
    let r = Real::start().await;
    let q = token("a7-probe");
    let sid = r
        .item(&format!(
            "Call the tool steve_knowledge_search of the steve MCP server exactly once, with the \
             query \"{q}\". Then reply with the word DONE and nothing else. Use no other tool."
        ))
        .await;
    r.allowing_until_over(&sid, 1, "tool call").await;
    let calls = r.h.journal(
        "SELECT COUNT(*) FROM calls WHERE session=? AND instr(name,'steve_knowledge_search')>0",
        &[&sid],
    );
    let done = r.h.journal(
        "SELECT COUNT(*) FROM calls WHERE session=? AND instr(name,'steve_knowledge_search')>0 AND state='done'",
        &[&sid],
    );
    let kinds = r.h.one(
        "SELECT tool_kinds FROM agent_turns WHERE session_id=?",
        &[&sid],
    );
    eprintln!("case 2: journal calls {calls}, done {done}, tool kinds {kinds:?}");
    assert_eq!((calls, done), (1, 1), "one call, done, in the journal");
    assert!(
        r.said(&sid, 0, "DONE") > 0,
        "the turn went on after the tool"
    );
}

/// 3. A Bash permission card answered allow_once runs the command; a second one rejected does not.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn a_bash_card_allowed_runs_and_a_rejected_one_does_not() {
    let r = Real::start().await;
    let ask = |mark: &str, file: &str| {
        format!(
            "Use the Bash tool to run exactly this command: touch {mark} && cat {file}\n\
             Then reply with its output and nothing else. If the command is refused, do not try \
             any other way (no other tool) and reply REFUSED."
        )
    };
    let sid = r.item(&ask("a7-mark-1.txt", "notes.txt")).await;
    let card = r.card(&sid, "first Bash").await;
    r.c.answer(&card, "allow_once").await;
    r.turn_over(&sid, 1, "allowed").await;
    assert_eq!(
        r.h.one("SELECT status FROM decisions WHERE id=?", &[&card])
            .as_deref(),
        Some("resolved")
    );
    assert!(
        r.said(&sid, 0, &r.h.note) > 0,
        "the command's output came back"
    );
    let dir = cwd(&r, &sid);
    assert!(
        dir.join("a7-mark-1.txt").exists(),
        "the allowed command ran"
    );

    let seq = r.seq(&sid);
    r.c.say(&sid, &ask("a7-mark-2.txt", "secret.txt")).await;
    let card = r.card(&sid, "second Bash").await;
    r.c.answer(&card, "reject").await;
    r.turn_over(&sid, 2, "rejected").await;
    assert_eq!(
        r.h.one("SELECT status FROM decisions WHERE id=?", &[&card])
            .as_deref(),
        Some("resolved")
    );
    assert!(
        !dir.join("a7-mark-2.txt").exists(),
        "the rejected command never ran"
    );
    assert_eq!(r.said(&sid, seq, &r.h.secret), 0, "nothing read the file");
}

/// 4. A cancel mid-turn ends the turn, and the session takes the next message.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn a_cancel_mid_turn_ends_it() {
    let r = Real::start().await;
    let sid = r
        .item(
            "Write the integers from 1 to 5000 in English words, one per line, with no other \
             text. Do not use any tools.",
        )
        .await;
    // some output first if it streams; the turn must still be running either way
    let streamed = wait(90, || r.said(&sid, 0, "twenty") > 0).await;
    assert!(!r.done(&sid, 1), "still running (streamed: {streamed})");
    let (st, v) =
        r.c.call("POST", &format!("/api/sessions/{sid}/cancel"), None)
            .await;
    assert_eq!((st, &v["ok"]), (200, &json!(true)), "cancel: {v}");
    assert!(
        wait(60, || r.done(&sid, 1)).await,
        "the cancelled turn is over; log:\n{}",
        hub_log(&r.h)
    );
    assert!(
        r.said(&sid, 0, "cancelled") > 0,
        "the turn ended as cancelled"
    );

    let seq = r.seq(&sid);
    let pong = token("AFTER");
    r.c.say(
        &sid,
        &format!("Reply with exactly the word {pong} and nothing else. Do not use any tools."),
    )
    .await;
    r.turn_over(&sid, 2, "after the cancel").await;
    assert!(r.said(&sid, seq, &pong) > 0);
}

/// 5. A change at idle applies: the permission mode switched at idle (YOLO on: `default` becomes
/// `acceptEdits`) is configured on the Node, and the next turn writes a file with no card. The
/// Bot's model and effort reach the harness with the open (claude answers with that model).
///
/// TODO(model/effort at idle): no Hub trigger sends a model or effort change to an open Node session
/// yet (`RoutedRuntime` configures only from `set_mode`); assert it here once the step that adds
/// the Hub trigger lands.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn a_change_at_idle_applies_and_the_bots_model_reaches_the_harness() {
    let r = Real::start().await;
    let pong = token("MODEL");
    let sid = r
        .item(&format!(
            "Reply with exactly the word {pong} and nothing else. Do not use any tools."
        ))
        .await;
    r.turn_over(&sid, 1, "model").await;
    let open = r
        .h
        .one(
            "SELECT request FROM node_commands WHERE method='open_session' ORDER BY created LIMIT 1",
            &[],
        )
        .expect("an open_session");
    eprintln!("case 5: open {open}");
    assert!(open.contains(&model()), "the open carries the model");
    assert!(open.contains(&effort()), "the open carries the effort");
    assert!(
        open.contains("\"ask\""),
        "opened asking for every permission"
    );
    let models = transcript_models(&cwd(&r, &sid).display().to_string());
    eprintln!("case 5: transcript models {models:?}");
    assert!(
        models.iter().any(|m| m.contains(&model())),
        "claude answered with {}: {models:?}",
        model()
    );

    // at idle: the mode change goes to the Node now, not with a later open
    r.yolo_on().await;
    r.configure_over(&sid, "accept_edits").await;
    let opens = r.h.count(
        "SELECT COUNT(*) FROM node_commands WHERE method='open_session'",
        &[],
    );
    assert_eq!(opens, 1, "configured in place, not reopened");
    let mark = token("A7EDIT");
    r.c.say(
        &sid,
        &format!(
            "Use the Write tool to create the file a7-edit-5.txt in the current directory with \
             exactly this content: {mark}\nThen reply DONE. Use no other tool."
        ),
    )
    .await;
    r.turn_over(&sid, 2, "edit under acceptEdits").await;
    assert_eq!(r.cards(&sid), 0, "acceptEdits: the edit asked nothing");
    assert_eq!(
        std::fs::read_to_string(cwd(&r, &sid).join("a7-edit-5.txt"))
            .unwrap_or_default()
            .trim(),
        mark,
        "the edit ran"
    );
}

/// 6. A release (the Hub stopped while the session is idle) then a reopen continues the same
/// conversation: the model recalls a word from before.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn a_released_session_reopens_with_its_conversation() {
    let mut r = Real::start().await;
    let word = token("OWL");
    let sid = r
        .item(&format!(
            "Remember this code word: {word}. Reply only OK and do not repeat the code word. \
             Do not use any tools."
        ))
        .await;
    r.turn_over(&sid, 1, "remember").await;
    r.hub.stop();
    r.restart().await;
    assert!(
        wait(60, || r
            .h
            .one(
                "SELECT phase FROM runtime_sessions WHERE session_id=?",
                &[&sid]
            )
            .as_deref()
            == Some("released"))
        .await,
        "let go across the restart; log:\n{}",
        hub_log(&r.h)
    );
    let seq = r.seq(&sid);
    r.c.say(
        &sid,
        "What was the code word I gave you? Reply with only the code word. Do not use any tools.",
    )
    .await;
    r.turn_over(&sid, 2, "recall").await;
    let opens = r.h.count(
        "SELECT COUNT(*) FROM node_commands WHERE method='open_session'",
        &[],
    );
    let last = r.h.one(
        "SELECT request FROM node_commands WHERE method='open_session' ORDER BY created DESC LIMIT 1",
        &[],
    );
    eprintln!("case 6: {opens} opens, last {last:?}");
    assert!(opens >= 2, "opened again");
    assert!(
        r.said(&sid, seq, &word) > 0,
        "the same conversation goes on"
    );
}

/// 7. An image attachment reaches claude through HubFiles.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn an_image_attachment_arrives_via_hub_files() {
    let r = Real::start().await;
    let sid = r
        .item("Reply with exactly the word READY and nothing else. Do not use any tools.")
        .await;
    r.turn_over(&sid, 1, "ready").await;
    let file = r.c.upload("a7.png", png(64, 64, [0, 200, 0])).await;
    let seq = r.seq(&sid);
    r.c.say_with(
        &sid,
        "The attached image is one solid colour. Reply with COLOUR- followed by the English name \
         of that colour in capitals (for example COLOUR-PURPLE) and nothing else. Do not use any tools.",
        &[&file],
    )
    .await;
    r.turn_over(&sid, 2, "image").await;
    assert!(
        r.h.count(
            "SELECT COUNT(*) FROM events WHERE session_id=? AND kind='user' AND format='steve.v1' AND instr(data, ?) > 0",
            &[&sid, &file]
        ) > 0,
        "the user's message carries the file"
    );
    assert!(
        r.said(&sid, seq, "COLOUR-GREEN") > 0,
        "claude saw the image; log:\n{}",
        hub_log(&r.h)
    );
}

/// 8. The Hub killed mid-turn: the attempt is unknown and never re-run; Stop abandons it and the
/// next message runs.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn a_hub_killed_mid_turn_is_unknown_never_rerun_and_stop_abandons_it() {
    let mut r = Real::start().await;
    let sid = r
        .item(
            "Use the Bash tool to run exactly this command: touch a7-mark-8.txt\n\
             Then reply DONE. If the command is refused, reply REFUSED and use no other tool.",
        )
        .await;
    // a permission card: the turn is surely in flight
    r.card(&sid, "mid-turn").await;
    let turn =
        r.h.one(
            "SELECT turn_id FROM runtime_turns WHERE session_id=? AND state<>'finished'",
            &[&sid],
        )
        .expect("the open turn");
    r.hub.kill();
    r.restart().await;
    assert!(
        wait(60, || r.attempts(&sid, "unknown") == 1).await,
        "the cut attempt is unknown; log:\n{}",
        hub_log(&r.h)
    );
    assert!(
        wait(30, || r.said(&sid, 0, "等待对账") > 0).await,
        "the session says it awaits reconciliation"
    );
    tokio::time::sleep(Duration::from_secs(10)).await;
    let of_turn = || {
        r.h.count(
            "SELECT COUNT(*) FROM runtime_attempts WHERE turn_id=?",
            &[&turn],
        )
    };
    assert_eq!(of_turn(), 1, "never re-run");
    assert!(
        !cwd(&r, &sid).join("a7-mark-8.txt").exists(),
        "the unanswered command never ran"
    );

    let (st, v) =
        r.c.call("POST", &format!("/api/sessions/{sid}/cancel"), None)
            .await;
    assert_eq!(st, 200, "stop: {v}");
    assert!(
        wait(60, || r.done(&sid, 1)).await,
        "Stop abandons the unknown turn; log:\n{}",
        hub_log(&r.h)
    );
    assert_eq!(of_turn(), 1, "still never re-run");

    let seq = r.seq(&sid);
    let pong = token("NEXT");
    r.c.say(
        &sid,
        &format!("Reply with exactly the word {pong} and nothing else. Do not use any tools."),
    )
    .await;
    r.turn_over(&sid, 2, "next message").await;
    assert!(r.said(&sid, seq, &pong) > 0, "the next message runs");
}

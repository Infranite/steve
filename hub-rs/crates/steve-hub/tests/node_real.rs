//! A7: the colocated service Node against the real Claude, end to end on the real
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
//! Bot's model and effort (default `claude-haiku-5-5`, `low`): an id as the model picker lists it,
//! since the Node holds the harness to exactly the id asked (an alias such as `haiku` comes back as
//! the full id it maps to, and the open is refused). A Hub started from inside a Claude Code session
//! would hand its session variables (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, …) down to the harness;
//! they are removed, as under launchd.
//!
//! Cases:
//! 1. a hello turn is stored as steve.v1, with its usage and cost in agent_turns / usage_records;
//! 2. a Steve tool call (read-only) goes through ToolService, with its row in the host journal;
//! 3. a Bash permission card answered allow_once runs; a second one rejected does not;
//! 4. a cancel mid-turn ends the turn;
//! 5. a permission-mode change at idle (YOLO on: `default` → `auto`) is configured on the
//!    Node and the next edit asks nothing; the Bot's model and effort reach the harness with the
//!    open. Pending: a model/effort change at idle, which has no Hub trigger yet (see the case);
//! 6. a release (the Hub stopped at idle) then a reopen continues the same conversation;
//! 7. an image attachment arrives through HubFiles;
//! 8. the Hub killed mid-turn: the attempt is unknown and never re-run; Stop abandons it and the
//!    next message runs;
//! 9. an upgrade drain waits for a Node turn (its Bash card open, then its command running), a
//!    message sent meanwhile waits, and lifting the drain sends it;
//! The local fake-adapter cases use an isolated home and no model calls.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use steve_runtime::testkit::eventually;
use steve_runtime::testkit::eventually_sync;
use steve_runtime::testkit::process::{Child, Command};
use steve_store::testkit::hub::node_process;

use serde_json::{Value, json};

mod handed;

const VERSION: &str = "0.9.0";
/// A real model turn, at most.
const TURN: u64 = 300;

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
    /// The Hub's port, held by the test for every Hub it starts on this home (#429).
    socket: handed::Socket,
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
        let mut dir = tempfile::tempdir().expect("tmp");
        // `A7_KEEP=1`: the home stays for a look after the run (its path is printed)
        dir.disable_cleanup(std::env::var_os("A7_KEEP").is_some());
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
        let socket = handed::Socket::new();
        Home {
            port: socket.port(),
            socket,
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
    fn db_path(&self) -> std::path::PathBuf {
        self.steve().join("data/steve.sqlite3")
    }
}

/// A running Hub process (its own process group: killing it takes its agents along).
struct Hub(Option<Child>);

impl Hub {
    /// SIGKILL the Hub and its agents (each agent runs in its own process group).
    fn kill(&mut self) {
        if let Some(child) = self.0.take() {
            Self::kill_child(child);
        }
    }
    fn kill_child(mut child: Child) {
        // A completed child no longer pins its cached PID/group number.
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        let pid = child.id() as i32;
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
        let _ = child.wait();
    }
    /// SIGTERM: the Hub stops its Node and the Node its harnesses; SIGKILL after 60 s.
    fn stop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let end = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < end {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Self::kill_child(child);
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The real Hub with its colocated service Node. On a [`Home::fake`] the scripted agent replaces claude and
/// `HOME` is the temp dir, so nothing of the user's login is read.
fn spawn(h: &Home) -> Hub {
    let mut c = handed::command(env!("CARGO_BIN_EXE_steve-hub"), &[("hub", &h.socket)]);
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
        .env("STEVE_HARNESS_MANIFEST", h.steve().join("harness.json"))
        .env_remove("STEVE_NODE")
        .env_remove("STEVE_NODE_MODE")
        .env("STEVE_LOCAL_NODE_PORT", "0")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        // no "you ended without delivering" follow-up turn: each case counts its own turns
        .env("STEVE_TAKEOVER_CONTINUES", "0")
        .env_remove("STEVE_NODE_TURN_LIMIT_MS")
        .env_remove("STEVE_NO_RESUME")
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
    // the variables of a Claude Code session this test may run in: a Hub under launchd has none
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k == "CLAUDECODE"
            || k == "CLAUDE_PID"
            || k == "CLAUDE_EFFORT"
            || k == "AI_AGENT"
            || (k.starts_with("CLAUDE_CODE_") && k != "CLAUDE_CODE_EXECUTABLE")
        {
            c.env_remove(k.as_ref());
        }
    }
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
    Hub(Some(c.spawn().expect("spawn hub")))
}

/// Now, in ms since the epoch (the clock the Hub stamps rows with).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}

/// Up and serving the full app.
async fn serving(h: &Home) {
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!("the Hub serves; log:\n{}", hub_log(h))
    });
    eventually(diagnostic, || async {
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
}

fn hub_log(h: &Home) -> String {
    let s = std::fs::read_to_string(h.dir.path().join("hub.log")).unwrap_or_default();
    let lines: Vec<&str> = s.lines().collect();
    let mut out = lines[lines.len().saturating_sub(60)..].join("\n");
    // the Node's side, which the Hub's log may not tell: its commands and their receipts
    if let Ok(rows) = node_process::command_trace(&h.db_path()) {
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
            .find(|c| c.starts_with("steve_session_"))
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
    std::env::var("A7_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".into())
}

fn effort() -> String {
    std::env::var("A7_EFFORT").unwrap_or_else(|_| "low".into())
}

impl Real {
    async fn start() -> Real {
        Real::start_on(Home::new()).await
    }

    async fn start_on(h: Home) -> Real {
        Real::start_as(h, "ask").await
    }

    /// With the test Bot in `perm_mode`.
    async fn start_as(h: Home, perm_mode: &str) -> Real {
        let hub = spawn(&h);
        serving(&h).await;
        let c = Client::login(&h).await;
        let mut bot = json!({"name": "A7 实测", "harness": "claude-code", "perm_mode": perm_mode});
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
        let diagnostic = steve_runtime::testkit::on_timeout(|| async {
            format!(
                "The item receives its session binding; Hub log:\n{}",
                hub_log(&self.h)
            )
        });
        eventually_sync(diagnostic, || {
            sid = node_process::session_id_by_item(&self.h.db_path(), &item).unwrap();
            sid.is_some()
        })
        .await;
        let sid = sid.expect("session");
        let diagnostic = steve_runtime::testkit::on_timeout(|| async {
            format!(
                "The item session is bound to the local Node; Hub log:\n{}",
                hub_log(&self.h)
            )
        });
        eventually_sync(diagnostic, || {
            node_process::binding_count_by_session_local(&self.h.db_path(), &sid).unwrap() == 1
        })
        .await;
        sid
    }

    /// The session's last event seq.
    fn seq(&self, sid: &str) -> i64 {
        node_process::event_coalesce_max_seq_0_by_session(&self.h.db_path(), &sid).unwrap()
    }

    /// steve.v1 events of `sid` after `since`, not the user's own, that contain `needle`. The
    /// harness's reply streams as `text` chunks (a token may be cut across two), so their texts
    /// count as one, joined in order.
    fn said(&self, sid: &str, since: i64, needle: &str) -> i64 {
        let other = node_process::event_count_by_session_by_seq_steve_v1_selected(
            &self.h.db_path(),
            &sid,
            since,
            &needle,
        )
        .unwrap();
        let text = node_process::output_text_after(&self.h.db_path(), &sid, since)
            .unwrap()
            .unwrap_or_default();
        other + i64::from(text.contains(needle))
    }

    fn turns(&self, sid: &str) -> i64 {
        node_process::runtime_turns_count_by_session(&self.h.db_path(), &sid).unwrap()
    }

    /// `n` turns so far and none open.
    fn done(&self, sid: &str, n: i64) -> bool {
        self.turns(sid) >= n
            && node_process::runtime_turns_count_by_session_not_finished(&self.h.db_path(), &sid)
                .unwrap()
                == 0
    }

    fn open_card(&self, sid: &str) -> Option<String> {
        node_process::decision_id_by_session_tool_call_open(&self.h.db_path(), &sid).unwrap()
    }

    fn cards(&self, sid: &str) -> i64 {
        node_process::decision_count_by_session_tool_call(&self.h.db_path(), &sid).unwrap()
    }

    fn attempts(&self, sid: &str, state: &str) -> i64 {
        node_process::attempt_count_by_session_by_state(&self.h.db_path(), &sid, &state).unwrap()
    }

    /// `configure_session` commands of `sid` the Node finished with success, with `permissions`.
    fn configured(&self, sid: &str, permissions: &str) -> i64 {
        node_process::command_count_configure_session_finished(
            &self.h.db_path(),
            &sid,
            &permissions,
        )
        .unwrap()
    }

    /// Waits for the session's permission mode to be configured on the Node, failing with the log.
    async fn configure_over(&self, sid: &str, permissions: &str) {
        let diagnostic = steve_runtime::testkit::on_timeout(|| async {
            format!(
                "The Node applies the requested permission configuration; configured {permissions} on the Node; log:\n{}",
                hub_log(&self.h)
            )
        });
        eventually_sync(diagnostic, || self.configured(sid, permissions) == 1).await;
        let diagnostic = steve_runtime::testkit::on_timeout(|| async {
            format!(
                "The configured Node session becomes ready again; Hub log:\n{}",
                hub_log(&self.h)
            )
        });
        eventually_sync(diagnostic, || {
            node_process::binding_phase_by_session(&self.h.db_path(), &sid)
                .unwrap()
                .as_deref()
                == Some("ready")
        })
        .await;
    }

    /// Waits for `n` finished turns, failing with the log.
    async fn turn_over(&self, sid: &str, n: i64, what: &str) {
        let since = now_ms();
        let mut failed = None;
        let diagnostic = steve_runtime::testkit::on_timeout(|| async {
            format!(
                "{what}: turn {n} finishes; failed card: {:?}; Hub log:\n{}",
                self.failed(sid, since),
                hub_log(&self.h)
            )
        });
        eventually_sync(diagnostic, || {
            failed = self.failed(sid, since);
            failed.is_some() || self.done(sid, n)
        })
        .await;
        assert!(
            failed.is_none(),
            "{what}: turn {n} failed: {failed:?}; Hub log:\n{}",
            hub_log(&self.h)
        );
    }

    /// The session of `sid` broke since `since` (ms): the text of its item's open `fail` card for a
    /// session error (a refused open, a harness that died). It ends a wait at once rather than
    /// after [`TURN`]. A turn that ends without a delivery opens a `fail` card too; that one is
    /// the item's business, not the session's, and does not count.
    fn failed(&self, sid: &str, since: i64) -> Option<String> {
        node_process::first_decision_d_question_by_id_fail_open(&self.h.db_path(), &sid, since)
            .unwrap()
    }

    /// Waits for a permission card of `sid`.
    async fn card(&self, sid: &str, what: &str) -> String {
        let since = now_ms();
        let (mut card, mut failed) = (None, None);
        let diagnostic = steve_runtime::testkit::on_timeout(|| async {
            format!(
                "{what}: a permission card appears; failed card: {:?}; Hub log:\n{}",
                self.failed(sid, since),
                hub_log(&self.h)
            )
        });
        eventually_sync(diagnostic, || {
            card = self.open_card(sid);
            failed = self.failed(sid, since);
            card.is_some() || failed.is_some()
        })
        .await;
        assert!(
            failed.is_none(),
            "{what}: permission request failed: {failed:?}; Hub log:\n{}",
            hub_log(&self.h)
        );
        card.expect("card")
    }

    /// Waits for `n` finished turns, allowing once every card on the way (Steve's own tools may ask).
    async fn allowing_until_over(&self, sid: &str, n: i64, what: &str) {
        let end = tokio::time::Instant::now() + Duration::from_secs(TURN);
        let since = now_ms();
        while tokio::time::Instant::now() < end {
            if let Some(f) = self.failed(sid, since) {
                panic!("{what}: failed: {f}; log:\n{}", hub_log(&self.h));
            }
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
    // claude names a project's directory after its whole path, every other character a `-`
    let name = |p: &Path| -> String {
        p.to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    };
    let mut names = vec![name(Path::new(cwd))];
    if let Ok(real) = Path::new(cwd).canonicalize() {
        names.push(name(&real));
    }
    let mut out = vec![];
    for d in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if !names.contains(&d.file_name().to_string_lossy().into_owned()) {
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
        node_process::session_cwd_by_id(&r.h.db_path(), &sid)
            .unwrap()
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
        node_process::command_count_prepare_workspace_finished(&r.h.db_path()).unwrap(),
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

/// Owner policy changes reach an idle Node and leave the next turn runnable.
#[tokio::test(flavor = "multi_thread")]
async fn a_mode_change_at_idle_configures_the_node_session() {
    let r = Real::start_as(Home::fake(), "ask").await;
    let sid = r.item("A7 mode @say first-A7").await;
    r.turn_over(&sid, 1, "first turn").await;
    let mode_url = format!("/api/approval-policy/sessions/{sid}");
    r.c.ok(
        "PUT",
        "/api/approval-policy",
        json!({"mode":"harness_auto","revision":0}),
    )
    .await;
    r.configure_over(&sid, "auto").await;
    let applied = r.c.ok("GET", &mode_url, Value::Null).await;
    assert_eq!(applied["status"], "on", "{applied}");
    assert_eq!(applied["applied"], "auto", "{applied}");
    assert_eq!(applied["native_id"], "auto", "{applied}");
    let seq = r.seq(&sid);
    r.c.say(&sid, "A7 mode @say second-A7").await;
    r.turn_over(&sid, 2, "after configure").await;
    assert!(r.said(&sid, seq, "second-A7") > 0);
    let stats = r.c.ok("GET", "/api/stats/summary", Value::Null).await;
    assert_eq!(stats["human"]["approval"]["sessions"], 1, "{stats}");
    assert_eq!(stats["human"]["approval"]["turns"], 1, "{stats}");
    r.c.ok(
        "PUT",
        "/api/approval-policy",
        json!({"mode":"ask_owner","revision":1}),
    )
    .await;
    r.configure_over(&sid, "ask").await;
    let applied = r.c.ok("GET", &mode_url, Value::Null).await;
    assert_eq!(applied["status"], "off", "{applied}");
    assert_eq!(applied["applied"], "ask", "{applied}");
    let log = std::fs::read_to_string(r.h.fake.as_ref().unwrap()).unwrap();
    let sent_modes: Vec<_> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|m| m["method"] == "session/set_mode")
        .filter_map(|m| m["params"]["modeId"].as_str().map(str::to_owned))
        .collect();
    assert!(
        sent_modes.ends_with(&["auto".into(), "default".into()]),
        "{sent_modes:?}"
    );

    r.c.ok("POST", &format!("/api/sessions/{sid}/end"), Value::Null)
        .await;
    let ended = r.c.ok("GET", &mode_url, Value::Null).await;
    assert_eq!(ended["status"], "off", "{ended}");
    assert_eq!(ended["native_id"], "auto", "{ended}");
    assert!(ended["target"].is_null(), "{ended}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_adapter_without_native_auto_reports_unsupported_and_keeps_asking() {
    let home = Home::fake();
    std::fs::create_dir_all(home.steve()).unwrap();
    std::fs::write(
        home.steve().join("harness.json"),
        json!({"adapters":{"claude-code":{
            "command": concat!(env!("CARGO_MANIFEST_DIR"), "/../steve-acp/tests/fake-acp.js"),
            "properties":{"modes":{"auto":""}},
        }}})
        .to_string(),
    )
    .unwrap();
    let r = Real::start_on(home).await;
    r.c.ok(
        "PUT",
        "/api/approval-policy",
        json!({"mode":"harness_auto","revision":0}),
    )
    .await;
    let sid = r.item("Ask-only adapter @say first").await;
    r.turn_over(&sid, 1, "first turn without a native mode")
        .await;
    let mode =
        r.c.ok(
            "GET",
            &format!("/api/approval-policy/sessions/{sid}"),
            Value::Null,
        )
        .await;
    assert_eq!(mode["status"], "unsupported", "{mode}");
    assert_eq!(mode["target"], "ask", "{mode}");
    assert_eq!(mode["applied"], "ask", "{mode}");
    assert!(mode["native_id"].is_null(), "{mode}");
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
        node_process::event_count_by_session_not_finished(&r.h.db_path(), &sid).unwrap(),
        0,
        "nothing in another format"
    );
    assert_eq!(
        node_process::completed_node_turn_count(&r.h.db_path(), &sid).unwrap(),
        1,
        "one statistics row for the attempt"
    );
    let output =
        node_process::usage_records_coalesce_sum_output_0_by_session(&r.h.db_path(), &sid).unwrap();
    let cost = node_process::session_reported_cost_text(&r.h.db_path(), &sid).unwrap();
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
    let calls = node_process::tool_journal_calls_count_by_session(
        &r.h.steve().join("data/node/host-tools.sqlite3"),
        &sid,
    )
    .unwrap();
    let done = node_process::tool_journal_calls_count_by_session_done(
        &r.h.steve().join("data/node/host-tools.sqlite3"),
        &sid,
    )
    .unwrap();
    let kinds = node_process::turn_tool_kinds_by_session(&r.h.db_path(), &sid).unwrap();
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
        node_process::decision_status_by_id(&r.h.db_path(), &card)
            .unwrap()
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
        node_process::decision_status_by_id(&r.h.db_path(), &card)
            .unwrap()
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
    // Wait for the ordinary turn state, never for a harness-specific streaming chunk.
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        let (_, session) = r.c.call("GET", &format!("/api/sessions/{sid}"), None).await;
        format!(
            "The session is running before cancellation; session: {session}; Hub log:\n{}",
            hub_log(&r.h)
        )
    });
    eventually(diagnostic, || async {
        let (_, session) = r.c.call("GET", &format!("/api/sessions/{sid}"), None).await;
        session["busy"] == true && session["status"] == "running"
    })
    .await;
    assert!(!r.done(&sid, 1), "the turn is running before cancellation");
    let (st, v) =
        r.c.call("POST", &format!("/api/sessions/{sid}/cancel"), None)
            .await;
    assert_eq!((st, &v["ok"]), (200, &json!(true)), "cancel: {v}");
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The cancelled session has at least one recorded turn and no unfinished turns; log:\n{}",
            hub_log(&r.h)
        )
    });
    eventually_sync(diagnostic, || r.done(&sid, 1)).await;
    let streamed = r.said(&sid, 0, "twenty") > 0;
    assert!(
        r.said(&sid, 0, "cancelled") > 0,
        "the turn ended as cancelled (streamed: {streamed})"
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

/// 5. A change at idle applies: the session opens in `acceptEdits` (its own worktree, #38); the
/// permission mode switched at idle (native auto) is configured on the Node, and the next turn
/// writes a file with no card. The Bot's model and effort reach the harness with the open (claude
/// answers with that model).
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
    let open = node_process::first_command_request_open_session(&r.h.db_path())
        .unwrap()
        .expect("an open_session");
    eprintln!("case 5: open {open}");
    assert!(open.contains(&model()), "the open carries the model");
    assert!(open.contains(&effort()), "the open carries the effort");
    assert!(open.contains("\"ask\""), "opened in AskOwner");
    let models = transcript_models(&cwd(&r, &sid).display().to_string());
    eprintln!("case 5: transcript models {models:?}");
    assert!(
        models.iter().any(|m| m.contains(&model())),
        "claude answered with {}: {models:?}",
        model()
    );

    // at idle: the mode change goes to the Node now, not with a later open
    r.c.ok(
        "PUT",
        "/api/approval-policy",
        json!({"mode":"harness_auto","revision":0}),
    )
    .await;
    r.configure_over(&sid, "auto").await;
    let opens = node_process::command_count_open_session(&r.h.db_path()).unwrap();
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
    r.turn_over(&sid, 2, "edit under native auto").await;
    assert_eq!(r.cards(&sid), 0, "native auto: the edit asked nothing");
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
            "Remember this code word: {word}. I will ask for it later. For now reply only OK. \
             Do not use any tools."
        ))
        .await;
    r.turn_over(&sid, 1, "remember").await;
    r.hub.stop();
    r.restart().await;
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The idle Node session releases its harness after restart; let go across the restart; log:\n{}",
            hub_log(&r.h)
        )
    });
    eventually_sync(diagnostic, || {
        node_process::binding_phase_by_session(&r.h.db_path(), &sid)
            .unwrap()
            .as_deref()
            == Some("released")
    })
    .await;
    let seq = r.seq(&sid);
    r.c.say(
        &sid,
        "Now please tell me the code word I gave you earlier. Reply with only the code word. Do \
         not use any tools.",
    )
    .await;
    r.turn_over(&sid, 2, "recall").await;
    let opens = node_process::command_count_open_session(&r.h.db_path()).unwrap();
    let last = node_process::latest_command_request_open_session(&r.h.db_path()).unwrap();
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
        node_process::event_count_by_session_user_steve_v1(&r.h.db_path(), &sid, &file).unwrap()
            > 0,
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
    let turn = node_process::runtime_turns_turn_id_by_session_not_finished(&r.h.db_path(), &sid)
        .unwrap()
        .expect("the open turn");
    r.hub.kill();
    r.restart().await;
    // B6: never re-run; the Hub gives the cut attempt up once the Node confirms its harness is
    // gone, and the agent is continued
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!(
            "The interrupted Node attempt is recorded as abandoned after restart; the cut attempt is given up; log:\n{}",
            hub_log(&r.h)
        )
    });
    eventually_sync(diagnostic, || {
        node_process::attempt_count_by_turn(&r.h.db_path(), &turn).unwrap() == 1
    })
    .await;
    eventually_sync(
        "The session records the reason its Node attempt was interrupted.",
        || r.said(&sid, 0, "Node 重启了") > 0,
    )
    .await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let of_turn =
        || node_process::attempt_count_by_turn_runtime_attempts(&r.h.db_path(), &turn).unwrap();
    assert_eq!(of_turn(), 1, "never re-run");
    assert!(
        !cwd(&r, &sid).join("a7-mark-8.txt").exists(),
        "the unanswered command never ran"
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

/// The supervisor's control token (`updates/control-token.json`, a JSON string), read by the Hub on
/// every drain request.
fn control_token(h: &Home) -> String {
    let t = token("a7-control-token-0123456789abcdef");
    let p = h.steve().join("updates/control-token.json");
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
    std::fs::write(&p, serde_json::to_vec(&json!(t)).expect("json")).expect("token");
    t
}

/// The upgrade drain: `POST` holds it (and reports), `DELETE` lifts it.
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

/// Polls the drain (`POST`, as `ops/local-release.py` does) until it reports `ready`.
async fn drained(h: &Home, token: &str, secs: u64) -> Option<Value> {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        let d = drain(h, "POST", token).await;
        if d["ready"] == true {
            return Some(d);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    None
}

/// 9. An upgrade drain waits for a Node turn: while its Bash card is open, and while the allowed
/// command (a 20 s sleep) runs (the `touch` makes Claude Code ask: a read-only `sleep && cat` it
/// runs unasked); it seals once the turn is over. A message sent under the drain
/// waits and goes once the drain is lifted.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real claude: costs model calls"]
async fn an_upgrade_drain_waits_for_a_node_turn() {
    let r = Real::start().await;
    let control = control_token(&r.h);
    let sid = r
        .item(
            "Use the Bash tool to run exactly this command: touch a7-mark-9.txt && sleep 20 && cat notes.txt\n\
             Then reply with its output and nothing else. If the command is refused, reply \
             REFUSED and use no other tool.",
        )
        .await;
    let card = r.card(&sid, "Bash before the drain").await;
    let d = drain(&r.h, "POST", &control).await;
    eprintln!("case 9: drain with the card open {d}");
    assert_eq!(d["maintenance"], true, "{d}");
    assert_eq!(
        (&d["ready"], &d["node"]),
        (&json!(false), &json!(1)),
        "a permission wait holds the drain: {d}"
    );
    let pong = token("QUEUED");
    let q =
        r.c.say(
            &sid,
            &format!("Reply with exactly the word {pong} and nothing else. Do not use any tools."),
        )
        .await;
    assert_eq!(q["result"], "queued", "{q}");
    let answered = std::time::Instant::now();
    r.c.answer(&card, "allow_once").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let d = drain(&r.h, "POST", &control).await;
    eprintln!("case 9: drain 5 s into the command {d}");
    assert_eq!(
        (&d["ready"], &d["node"]),
        (&json!(false), &json!(1)),
        "a running turn holds the drain: {d}"
    );
    let d = drained(&r.h, &control, TURN).await;
    let held = answered.elapsed();
    eprintln!("case 9: sealed {held:?} after the answer: {d:?}");
    let d = d.unwrap_or_else(|| panic!("sealed once the turn is over; log:\n{}", hub_log(&r.h)));
    assert_eq!(d["node"], 0, "{d}");
    assert!(
        held >= Duration::from_secs(20),
        "not before the command is over"
    );
    assert!(r.done(&sid, 1), "the turn is over");
    assert!(
        r.said(&sid, 0, &r.h.note) > 0,
        "the turn finished under the drain with the command's output"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(r.turns(&sid), 1, "the queued message waits while sealed");

    let seq = r.seq(&sid);
    let d = drain(&r.h, "DELETE", &control).await;
    assert_eq!(d["maintenance"], false, "{d}");
    r.turn_over(&sid, 2, "after the drain").await;
    assert!(
        r.said(&sid, seq, &pong) > 0,
        "the queued message went once lifted"
    );
}

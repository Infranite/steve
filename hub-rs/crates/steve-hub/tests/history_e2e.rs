//! E10: harness history import, resume and the harness probes through the local Node, on the real
//! `steve-hub` binary. Two Hubs on one fixture `HOME` (Claude Code and Codex transcripts), one with
//! the local Node off (today's path, the Hub reads the files) and one with `STEVE_NODE_MODE=local`
//! (the Node lists, reads and converts them):
//! - parity: both list the same sessions and store the same transcript rows;
//! - a second import of a key is refused (`已经导入过了`);
//! - a key the Node does not list (any other path, a `..` way out) is refused, nothing read;
//! - an import makes no agent turns and no usage;
//! - resume finds the transcript through the Node; Codex's models come from the Node's probe.
//!
//! No real host, no real agent, never `~/.steve` or the production port.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const CC_ID: &str = "3b2c458f-84be-4589-a22c-5bfd2579c313";
const CX_ID: &str = "01a1a57d-52a0-7111-9c1d-0f8bf59e7c96";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("free port")
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dirs");
    std::fs::write(p, text).expect("write");
}

struct Hub {
    child: Child,
    base: String,
    steve: PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        unsafe {
            libc::killpg(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn spawn(home: &Path, name: &str, node: bool) -> Hub {
    let port = free_port();
    let steve = home.join(name);
    let web = steve.join("web");
    write(&web.join("index.html"), "<html><head></head></html>");
    let base = format!("http://127.0.0.1:{port}");
    let mut c = Command::new(env!("CARGO_BIN_EXE_steve-hub"));
    c.env("HOME", home)
        .env("STEVE_HOME", &steve)
        .env("STEVE_PORT", port.to_string())
        .env("STEVE_HOST", "127.0.0.1")
        .env("STEVE_PUBLIC_URL", &base)
        .env("STEVE_DEV_LOGIN", "1")
        .env("STEVE_REPO", "/nonexistent")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_WEB_DIR", &web)
        .env("CLAUDE_CODE_EXECUTABLE", home.join("bin/claude"))
        .env("CODEX_EXECUTABLE", home.join("bin/codex"))
        .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
        .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
        .env_remove("STEVE_NODE_MODE")
        .env_remove("STEVE_NODE")
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
                .open(home.join(format!("{name}.log")))
                .expect("hub log"),
        )
        .process_group(0);
    if node {
        c.env("STEVE_NODE_MODE", "local");
    }
    Hub {
        child: c.spawn().expect("spawn hub"),
        base,
        steve,
    }
}

struct Client {
    http: reqwest::Client,
    base: String,
    cookie: String,
    db: PathBuf,
}

impl Client {
    async fn login(h: &Hub) -> Client {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let end = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let up = http
                .get(format!("{}/healthz", h.base))
                .timeout(Duration::from_secs(2))
                .send()
                .await;
            if let Ok(r) = up
                && r.json::<Value>()
                    .await
                    .is_ok_and(|v| v["ok"] == true && v["maintenance"] != true)
            {
                break;
            }
            assert!(tokio::time::Instant::now() < end, "the Hub serves");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let r = http
            .get(format!("{}/auth/dev", h.base))
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
            base: h.base.clone(),
            cookie,
            db: h.steve.join("data/steve.sqlite3"),
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
            .timeout(Duration::from_secs(60));
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

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method, path, body).await;
        assert_eq!(s, 200, "{method} {path}: {v}");
        v
    }

    fn db(&self) -> rusqlite::Connection {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c
    }

    /// A session's stored rows: `(kind, data)` in order.
    fn rows(&self, session: &str) -> Vec<(String, Value)> {
        let c = self.db();
        let mut st = c
            .prepare("SELECT kind, data FROM events WHERE session_id=? ORDER BY seq")
            .expect("prepare");
        st.query_map([session], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .expect("query")
        .map(|r| {
            let (k, d) = r.expect("row");
            (k, serde_json::from_str(&d).expect("json"))
        })
        .collect()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).expect("count")
    }

    async fn import(&self, keys: &[&str]) -> Vec<Value> {
        self.ok(
            "POST",
            "/api/local-sessions/import",
            Some(json!({ "keys": keys })),
        )
        .await
        .as_array()
        .cloned()
        .expect("an array")
    }
}

/// The fixture home: a Claude Code and a Codex session that ran in `~/w/app`, a Steve run, a
/// transcript outside the harnesses' stores, and fake harness binaries.
fn fixtures(home: &Path) -> (String, String, PathBuf) {
    let cwd = home.join("w/app").display().to_string();
    std::fs::create_dir_all(&cwd).expect("cwd");
    let claude = [
        format!(r#"{{"type":"user","sessionId":"{CC_ID}","cwd":"{cwd}","message":{{"content":"<command-name>x"}}}}"#),
        r#"{"type":"user","message":{"content":"fix the bug"}}"#.into(),
        format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Looking."}},{{"type":"tool_use","id":"t1","name":"Read","input":{{"file_path":"{cwd}/src/a.rs"}}}},{{"type":"tool_use","id":"t2","name":"Bash","input":{{"command":"cargo test"}}}}]}}}}"#),
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Found it."},{"type":"text","text":" Fixed."}]}}"#.into(),
        r#"{"type":"user","message":{"content":"thanks"}}"#.into(),
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Edit","input":{"file_path":"/elsewhere/b.rs"}},{"type":"text","text":"Done."}]}}"#.into(),
        "not json".into(),
    ]
    .join("\n");
    let cc = home.join(format!(".claude/projects/-w-app/{CC_ID}.jsonl"));
    write(&cc, &claude);
    let codex = [
        format!(r#"{{"type":"session_meta","payload":{{"id":"{CX_ID}","cwd":"{cwd}"}}}}"#),
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"<environment_context>"}]}}"#.into(),
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"hello"},{"text":" there"}]}}"#.into(),
        r#"{"type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"cmd\":\"ls\"}"}}"#.into(),
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":"Two files."}]}}"#.into(),
        r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"c2","name":"apply_patch","input":"*** Begin Patch"}}"#.into(),
    ]
    .join("\n");
    let cx = home.join(format!(
        ".codex/sessions/2026/11/04/rollout-2026-11-04T15-36-56-{CX_ID}.jsonl"
    ));
    write(&cx, &codex);
    let forged = home.join("elsewhere/x.jsonl");
    write(&forged, &claude);
    for (bin, body) in [
        ("claude", "echo '2.1.0 (Claude Code)'".to_string()),
        (
            "codex",
            r#"if [ "$1" = "--version" ]; then echo 'codex-cli 0.80.0'; else echo '{"models":[{"slug":"gpt-node-b","visibility":"list","priority":2},{"slug":"gpt-node-a","visibility":"list","priority":1}]}'; fi"#.into(),
        ),
    ] {
        let p = home.join("bin").join(bin);
        write(&p, &format!("#!/bin/sh\n{body}\n"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    (
        format!("cc:{}", cc.display()),
        format!("cx:{}", cx.display()),
        forged,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_through_the_local_node_matches_the_hubs_own_import() {
    let dir = tempfile::tempdir().expect("tmp");
    let home = dir.path().canonicalize().expect("home");
    let (cc, cx, forged) = fixtures(&home);
    let (legacy_hub, node_hub) = (spawn(&home, "legacy", false), spawn(&home, "node", true));
    let (legacy, node) = (
        Client::login(&legacy_hub).await,
        Client::login(&node_hub).await,
    );

    // the same list, newest first
    let (old_list, new_list) = (
        legacy.ok("GET", "/api/local-sessions", None).await,
        node.ok("GET", "/api/local-sessions", None).await,
    );
    assert_eq!(old_list.as_array().map(Vec::len), Some(2), "{old_list}");
    // a Node's timestamps are whole milliseconds (the Hub's own list keeps `mtimeMs`'s fraction)
    let whole = |mut l: Value| {
        for s in l.as_array_mut().unwrap() {
            s["when"] = json!(s["when"].as_f64().unwrap() as i64);
        }
        l
    };
    assert_eq!(whole(old_list), new_list);

    // the same stored rows
    let (old_ids, new_ids) = (
        legacy.import(&[&cc, &cx]).await,
        node.import(&[&cc, &cx]).await,
    );
    for (o, n) in old_ids.iter().zip(&new_ids) {
        let (o, n) = (o.as_str().expect("an id"), n.as_str().expect("an id"));
        let (old_rows, new_rows) = (legacy.rows(o), node.rows(n));
        assert!(old_rows.len() >= 4, "{old_rows:?}");
        assert_eq!(old_rows, new_rows);
    }
    let session = |c: &Client, id: &str| {
        c.db()
            .query_row(
                "SELECT harness, cwd, title, source, import_key, created, readonly FROM sessions WHERE id=?",
                [id],
                |r| {
                    Ok(json!([
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?
                    ]))
                },
            )
            .expect("session")
    };
    for (o, n) in old_ids.iter().zip(&new_ids) {
        assert_eq!(
            session(&legacy, o.as_str().unwrap()),
            session(&node, n.as_str().unwrap())
        );
    }

    // once only
    assert_eq!(
        node.import(&[&cc]).await,
        vec![json!({ "error": "已经导入过了" })]
    );
    let listed = node.ok("GET", "/api/local-sessions", None).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["imported"] == true)
    );

    // a key the Node does not list is refused, whatever it names
    let projects = home.join(".claude/projects/-w-app");
    let sessions_before = node.count("SELECT COUNT(*) FROM sessions");
    for key in [
        format!("cc:{}", forged.display()),
        format!("cc:{}/../../../elsewhere/x.jsonl", projects.display()),
        cc.replace("cc:", "cx:"),
    ] {
        assert_eq!(
            node.import(&[&key]).await,
            vec![json!({ "error": "不是本机能导入的会话" })],
            "{key}"
        );
    }
    assert_eq!(node.count("SELECT COUNT(*) FROM sessions"), sessions_before);

    // history is no new work: no turns, no usage
    for c in [&legacy, &node] {
        assert_eq!(c.count("SELECT COUNT(*) FROM agent_turns"), 0);
        assert_eq!(c.count("SELECT COUNT(*) FROM usage_records"), 0);
    }

    // resume: the Node finds the transcript by the harness's id
    let id = new_ids[0].as_str().unwrap();
    let r = node
        .ok("GET", &format!("/api/sessions/{id}/resume"), None)
        .await;
    assert_eq!(r["id"], CC_ID);
    assert_eq!(
        r["transcript"]["path"],
        format!("~/.claude/projects/-w-app/{CC_ID}.jsonl")
    );
    assert_eq!(r["transcript"]["matches"], true);
    assert_eq!(r["transcript"]["id"], CC_ID);
    let old = legacy
        .ok(
            "GET",
            &format!("/api/sessions/{}/resume", old_ids[0].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(old["command"], r["command"]);
    assert_eq!(old["transcript"]["updated"], r["transcript"]["updated"]);

    // the harnesses as the Node's probe reports them
    let models = node
        .ok("GET", "/api/system/models?harness=codex&fresh=1", None)
        .await;
    let ids: Vec<&str> = models["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["gpt-node-a", "gpt-node-b"], "{models}");
    let devices = node.ok("GET", "/api/devices?fresh=1", None).await;
    let runtimes = devices["runtimes"].as_array().unwrap();
    assert_eq!(runtimes[0]["version"], "2.1.0");
    assert_eq!(runtimes[1]["version"], "0.80.0");
    let old = legacy.ok("GET", "/api/devices?fresh=1", None).await;
    assert_eq!(old["runtimes"][0]["version"], "2.1.0");
}

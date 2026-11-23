//! Harness history import through the Node on the real `steve-hub` binary. Two isolated Hubs
//! share fixture harness history: Off imports through its jobs Node, Local through its session
//! Node. Both list and import the same entries, reject duplicate/unlisted keys, and create no
//! turns or usage. Resume and runtime observations retain their existing coverage.
//!
//! No real host, no real agent, never production data or ports.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use steve_runtime::testkit::eventually;
use steve_runtime::testkit::process::Child;
use steve_store::testkit::hub::node_process;

use serde_json::{Value, json};

const CC_ID: &str = "3b2c458f-84be-4589-a22c-5bfd2579c313";
const CX_ID: &str = "01a1a57d-52a0-7111-9c1d-0f8bf59e7c96";

mod handed;

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dirs");
    std::fs::write(p, text).expect("write");
}

struct Hub {
    child: Child,
    base: String,
    steve: PathBuf,
    /// Its port, held by the test (#429).
    _socket: handed::Socket,
}

impl Drop for Hub {
    fn drop(&mut self) {
        unsafe {
            libc::killpg(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn spawn(home: &Path, name: &str, diagnostic: bool) -> Hub {
    let socket = handed::Socket::new();
    let port = socket.port();
    let steve = home.join(name);
    let web = steve.join("web");
    write(&web.join("index.html"), "<html><head></head></html>");
    let base = format!("http://127.0.0.1:{port}");
    let mut c = handed::command(env!("CARGO_BIN_EXE_steve-hub"), &[("hub", &socket)]);
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
    if diagnostic {
        c.env("STEVE_NO_RESUME", "1");
    } else {
        c.env("STEVE_NODE_MODE", "local");
    }
    Hub {
        child: c.spawn().expect("spawn hub"),
        base,
        steve,
        _socket: socket,
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
        eventually(
            format!(
                "The isolated Hub at {} serves healthy requests outside maintenance.",
                h.base
            ),
            || async {
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
                    return true;
                }
                assert!(tokio::time::Instant::now() < end, "the Hub serves");
                false
            },
        )
        .await;
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
            .find(|c| c.starts_with("steve_session_"))
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

    /// A session's stored rows: `(kind, data)` in order.
    fn rows(&self, session: &str) -> Vec<(String, Value)> {
        node_process::history_events(&self.db, session)
            .unwrap()
            .into_iter()
            .map(|(kind, data)| (kind, serde_json::from_str(&data).expect("json")))
            .collect()
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
async fn imports_history_through_the_node() {
    let dir = tempfile::tempdir().expect("tmp");
    let home = dir.path().canonicalize().expect("home");
    let (cc, cx, forged) = fixtures(&home);
    let hub = spawn(&home, "node", false);
    let node = Client::login(&hub).await;
    let list = node.ok("GET", "/api/local-sessions", None).await;
    assert_eq!(list.as_array().map(Vec::len), Some(2), "{list}");
    let ids = node.import(&[&cc, &cx]).await;
    for id in &ids {
        assert!(node.rows(id.as_str().expect("an id")).len() >= 4);
    }
    let session = |c: &Client, id: &str| node_process::imported_session(&c.db, id).unwrap();
    for id in &ids {
        assert_eq!(session(&node, id.as_str().unwrap())[6], 1);
    }

    {
        let c = &node;
        // A transcript can only be imported once.
        assert_eq!(
            c.import(&[&cc]).await,
            vec![json!({ "error": "已经导入过了" })]
        );
        let listed = c.ok("GET", "/api/local-sessions", None).await;
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["imported"] == true)
        );

        // The Node rejects unlisted keys, even if they name an existing or traversing path.
        let projects = home.join(".claude/projects/-w-app");
        let sessions_before = node_process::session_count(&c.db).unwrap();
        for key in [
            format!("cc:{}", forged.display()),
            format!("cc:{}/../../../elsewhere/x.jsonl", projects.display()),
            cc.replace("cc:", "cx:"),
        ] {
            assert_eq!(
                c.import(&[&key]).await,
                vec![json!({ "error": "不是本机能导入的会话" })],
                "{key}"
            );
        }
        assert_eq!(node_process::session_count(&c.db).unwrap(), sessions_before);
        assert_eq!(node_process::turn_count(&c.db).unwrap(), 0);
        assert_eq!(node_process::usage_records_count(&c.db).unwrap(), 0);
        assert_eq!(node_process::event_count(&c.db).unwrap(), 0);
    }

    // resume: the Node finds the transcript by the harness's id
    let id = ids[0].as_str().unwrap();
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
    // the harnesses as the Node's probe reports them
    let devices = node.ok("GET", "/api/devices?fresh=1", None).await;
    let runtimes = devices["runtimes"].as_array().unwrap();
    let rt = |id: &str| runtimes.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(rt("claude-code")["version"], "2.1.0");
    assert_eq!(rt("codex")["version"], "0.80.0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inert_hub_has_no_history_node_and_returns_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let (cc, _, _) = fixtures(&home);
    let hub = spawn(&home, "inert", true);
    let client = Client::login(&hub).await;
    for (method, path, body) in [
        ("GET", "/api/local-sessions", None),
        (
            "POST",
            "/api/local-sessions/import",
            Some(json!({"keys":[cc]})),
        ),
    ] {
        let (status, body) = client.call(method, path, body).await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["code"], "unavailable", "{body}");
    }
    assert!(!hub.steve.join("data/node").exists());
    assert!(!hub.steve.join("data/node-jobs").exists());
    assert_eq!(
        node_process::session_count_with_readonly(&client.db).unwrap(),
        0
    );
}

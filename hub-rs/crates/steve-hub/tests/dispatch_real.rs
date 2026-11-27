//! End-to-end real-harness verification for #626: Steve really inspects `steve_bots_list`
//! before selecting the executor Bot, and skips model dispatch when only one Bot is eligible.
//!
//! This test runs the real `steve-hub` binary with its colocated `steve-node --local-node`
//! against a real local harness (`antigravity` / `agy` or `claude-code`) inside a fresh temporary
//! `STEVE_HOME` and an ephemeral loopback port (never `~/.steve`, never port `8440`).
//!
//! Because it costs real model calls and needs host network + OS sandbox outside the agent
//! sandbox, it is `#[ignore]`d and run by hand:
//!
//! ```sh
//! cd hub-rs
//! cargo build -p steve-node-cli --bin steve-node
//! # Default: agy (antigravity, gemini-3.8-flash)
//! cargo test -p steve-hub --test dispatch_real -- --ignored --nocapture --test-threads=1
//! # Or Claude Code:
//! STEVE_REAL_DISPATCH_HARNESS=claude-code \
//!   cargo test -p steve-hub --test dispatch_real -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Environment variables:
//! - `STEVE_REAL_DISPATCH_HARNESS`: `antigravity` (default; alias `agy`) or `claude-code` (alias `claude`)
//! - `STEVE_REAL_DISPATCH_MODEL`: optional model override (default `gemini-3.8-flash` for `antigravity`,
//!   `claude-haiku-5-5` or `A7_MODEL` for `claude-code`)
//! - `STEVE_ADAPTER_ANTIGRAVITY` / `STEVE_ADAPTER_CLAUDE` / `CLAUDE_CODE_EXECUTABLE`: optional binary overrides
//! - `STEVE_KEEP_DISPATCH_REAL=1`: preserve the temp `STEVE_HOME` directory after the test exits

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use steve_runtime::testkit::process::{Command, GroupGuard, ProcessGuard};
use steve_runtime::testkit::{eventually, eventually_sync, on_timeout};
use steve_store::testkit::hub::node_process;
use steve_types::{BotPlacement, PlacementReason};

mod handed;

const VERSION: &str = "0.9.0";
/// Production Hub port that isolated tests must never bind or contact.
const PRODUCTION_PORT: u16 = 8440;
/// Maximum wait for the colocated local Node to probe and report the chosen real harness ready.
const HARNESS_READY_WAIT: Duration = Duration::from_secs(60);
/// Maximum wait for Steve's real async dispatch Think job to finish and settle placement.
const DISPATCH_WAIT: Duration = Duration::from_secs(90);

#[derive(Clone, Debug)]
struct RealHarness {
    id: &'static str,
    model: String,
    agy_bin: Option<PathBuf>,
}

impl RealHarness {
    fn from_env() -> Self {
        let raw = std::env::var("STEVE_REAL_DISPATCH_HARNESS")
            .unwrap_or_else(|_| "antigravity".to_owned());
        match raw.trim().to_ascii_lowercase().as_str() {
            "antigravity" | "agy" => {
                let model = std::env::var("STEVE_REAL_DISPATCH_MODEL")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "gemini-3.8-flash".to_owned());
                let agy_bin = std::env::var_os("STEVE_ADAPTER_ANTIGRAVITY")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME")
                            .map(|h| PathBuf::from(h).join(".local/bin/agy"))
                            .filter(|p| p.is_file())
                    })
                    .unwrap_or_else(|| PathBuf::from("agy"));
                Self {
                    id: "antigravity",
                    model,
                    agy_bin: Some(agy_bin),
                }
            }
            "claude-code" | "claude" => {
                let model = std::env::var("STEVE_REAL_DISPATCH_MODEL")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| {
                        std::env::var("A7_MODEL")
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                    })
                    .unwrap_or_else(|| "claude-haiku-5-5".to_owned());
                Self {
                    id: "claude-code",
                    model,
                    agy_bin: None,
                }
            }
            other => panic!(
                "unsupported STEVE_REAL_DISPATCH_HARNESS={other:?}; expected 'antigravity' (or 'agy') or 'claude-code' (or 'claude')"
            ),
        }
    }
}

struct Home {
    dir: tempfile::TempDir,
    socket: handed::Socket,
    port: u16,
    web: PathBuf,
    project: PathBuf,
}

impl Home {
    fn new() -> Self {
        let mut dir = tempfile::tempdir().expect("tempdir");
        dir.disable_cleanup(std::env::var_os("STEVE_KEEP_DISPATCH_REAL").is_some());
        let web = dir.path().join("web");
        std::fs::create_dir_all(&web).expect("web dir");
        std::fs::write(web.join("index.html"), "<html><head></head></html>").expect("index.html");
        std::fs::write(
            web.join("release.json"),
            json!({"version": VERSION}).to_string(),
        )
        .expect("release.json");

        let project = dir.path().join("demo-repo");
        std::fs::create_dir_all(&project).expect("project dir");
        std::fs::write(project.join("README.md"), "# Dispatch real test repo\n").expect("readme");
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args([
                    "-c",
                    "user.name=dispatch-test",
                    "-c",
                    "user.email=dispatch@example.invalid",
                ])
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
        let port = socket.port();
        assert_ne!(port, PRODUCTION_PORT, "test must never use production port");
        Self {
            dir,
            socket,
            port,
            web,
            project,
        }
    }

    fn steve(&self) -> PathBuf {
        self.dir.path().join("steve")
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn db_path(&self) -> PathBuf {
        self.steve().join("data/steve.sqlite3")
    }

    fn host_tools_path(&self) -> PathBuf {
        self.steve().join("data/node/host-tools.sqlite3")
    }
}

struct Hub {
    child: Option<ProcessGuard>,
    node_pid_file: PathBuf,
}

impl Hub {
    fn kill_local_node(&self) {
        if let Ok(text) = std::fs::read_to_string(&self.node_pid_file)
            && let Some(pid) = text
                .lines()
                .next()
                .and_then(|l| l.trim().parse::<i32>().ok())
                .filter(|p| *p > 1)
        {
            GroupGuard::kill_pgid(pid);
        }
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.kill_local_node();
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.terminate(Duration::from_secs(30));
        }
        self.kill_local_node();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.kill();
    }
}

fn spawn(h: &Home, harness: &RealHarness) -> Hub {
    let hub_exe = PathBuf::from(env!("CARGO_BIN_EXE_steve-hub"));
    let has_node_bin = std::env::var_os("STEVE_NODE_BIN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .is_some_and(|p| p.is_file())
        || hub_exe.with_file_name("steve-node").is_file();
    assert!(
        has_node_bin,
        "sibling `steve-node` binary missing next to {} — build it first with: cargo build -p steve-node-cli --bin steve-node",
        hub_exe.display()
    );

    let mut c = handed::command(&hub_exe, &[("hub", &h.socket)]);
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
                .expect("hub.log"),
        )
        .process_group(0);

    // Strip nested Claude Code session variables, matching launchd and `node_real.rs`.
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

    match harness.id {
        "antigravity" => {
            if let Some(bin) = &harness.agy_bin {
                c.env("STEVE_ADAPTER_ANTIGRAVITY", bin);
            }
            c.env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false");
        }
        "claude-code" => {
            c.env("STEVE_ADAPTER_ANTIGRAVITY", "/usr/bin/false");
        }
        _ => {}
    }

    Hub {
        child: Some(c.spawn().expect("spawn steve-hub")),
        node_pid_file: h.steve().join("data/node/service.pid"),
    }
}

fn diagnostics(h: &Home, item: Option<&str>) -> String {
    let mut out = String::new();
    let s = std::fs::read_to_string(h.dir.path().join("hub.log")).unwrap_or_default();
    let lines: Vec<&str> = s.lines().collect();
    out.push_str("hub.log:\n");
    out.push_str(&lines[lines.len().saturating_sub(60)..].join("\n"));
    if let Ok(ns) = std::fs::read_to_string(h.steve().join("data/node/logs/local-node.log")) {
        let nlines: Vec<&str> = ns.lines().collect();
        out.push_str("\nlocal-node.log:\n");
        out.push_str(&nlines[nlines.len().saturating_sub(50)..].join("\n"));
    }
    if let Ok(jobs) = node_process::dispatch_job_trace(&h.db_path()) {
        out.push_str("\ndispatch_jobs:\n");
        out.push_str(&jobs.join("\n"));
    }
    if let Ok(calls) = node_process::tool_journal_all_calls(&h.host_tools_path()) {
        out.push_str("\nhost-tools calls:\n");
        for (session, name, state, status) in calls {
            out.push_str(&format!(
                "  session={session} name={name} state={state} status={status}\n"
            ));
        }
    }
    if let Some(id) = item {
        if let Ok(Some(wait)) = node_process::item_placement_wait_blocked(&h.db_path(), id) {
            out.push_str("\nitem_placement_wait:\n");
            out.push_str(&wait);
        }
        if let Ok(tl) = node_process::item_timeline_lines(&h.db_path(), id) {
            out.push_str("\nitem timeline:\n");
            out.push_str(&tl.join("\n"));
        }
    }
    out
}

async fn wait_serving(h: &Home) {
    let diag = on_timeout(|| async {
        format!(
            "Hub serving + local Node reconciled\n{}",
            diagnostics(h, None)
        )
    });
    eventually(diag, || async {
        let Ok(r) = reqwest::Client::new()
            .get(format!("{}/healthz", h.base()))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        else {
            return false;
        };
        r.json::<Value>().await.is_ok_and(|v| {
            v["ok"] == true && v["maintenance"] != true && v["node"]["reconciled"] == true
        })
    })
    .await;
}

struct Client {
    http: reqwest::Client,
    base: String,
    cookie: String,
}

impl Client {
    async fn login(h: &Home) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
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
        Self {
            http,
            base: h.base(),
            cookie,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(
            reqwest::Method::from_bytes(method.as_bytes()).expect("method"),
            format!("{}{path}", self.base),
        );
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req
            .header("cookie", &self.cookie)
            .send()
            .await
            .expect("HTTP request");
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn get(&self, path: &str) -> Value {
        let (st, v) = self.call("GET", path, None).await;
        assert_eq!(st, 200, "GET {path}: {v}");
        v
    }

    async fn ok(&self, method: &str, path: &str, body: Value) -> Value {
        let (st, v) = self.call(method, path, Some(body)).await;
        assert_eq!(st, 200, "{method} {path}: {v}");
        v
    }
}

async fn wait_harness_ready(h: &Home, c: &Client, harness: &str) -> Value {
    let deadline = tokio::time::Instant::now() + HARNESS_READY_WAIT;
    loop {
        let view = c.get("/api/settings/models").await;
        let ready = view["harnesses"].as_array().is_some_and(|hs| {
            hs.iter()
                .any(|entry| entry["id"] == harness && entry["ready"] == true)
        });
        if ready {
            return view;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "harness `{harness}` did not report ready within {:?}; models view={view}\n{}",
            HARNESS_READY_WAIT,
            diagnostics(h, None)
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn create_bot(
    c: &Client,
    harness: &RealHarness,
    name: &str,
    role: &str,
    specialties: &[&str],
) -> String {
    let created = c
        .ok(
            "POST",
            "/api/bots",
            json!({
                "name": name,
                "role": role,
                "specialties": specialties,
                "harness": harness.id,
                "model": harness.model,
                "perm_mode": "ask",
                "enabled": true,
            }),
        )
        .await;
    created["id"].as_str().expect("bot id").to_owned()
}

/// Real-harness E2E proof that Steve calls `steve_bots_list` to inspect candidates before choosing
/// the matching Bot (`PlacementReason::SteveSelected` + non-empty `why`), and skips model dispatch
/// completely (`PlacementReason::OnlyEligible`, 0 new dispatch jobs) when only one Bot is eligible.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real harness (agy/claude): costs model calls; run outside sandbox"]
async fn real_steve_inspects_bots_and_dispatches_to_matching_bot() {
    let harness = RealHarness::from_env();
    let h = Home::new();
    eprintln!(
        "dispatch_real: home={} port={} harness={} model={}",
        h.dir.path().display(),
        h.port,
        harness.id,
        harness.model
    );
    let mut hub = spawn(&h, &harness);
    wait_serving(&h).await;
    let c = Client::login(&h).await;

    // 1. Wait until the local Node confirms the real harness is ready, then configure Steve's
    //    primary & dispatch settings to use that exact harness and model.
    let models_view = wait_harness_ready(&h, &c, harness.id).await;
    let revision = models_view["bot_revision"]
        .as_i64()
        .expect("main Steve bot_revision");
    let selected = c
        .ok(
            "POST",
            "/api/settings/models/select",
            json!({
                "harness": harness.id,
                "model": harness.model,
                "expected_revision": revision,
                "follow_purposes": true,
                "lightweight": null,
            }),
        )
        .await;
    let dispatch_eff = selected["purposes"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["key"] == "dispatch"))
        .map(|p| &p["effective"])
        .expect("dispatch purpose in /api/settings/models");
    assert_eq!(
        dispatch_eff["harness"], harness.id,
        "dispatch effective harness must match selected harness: {dispatch_eff}"
    );
    assert_eq!(
        dispatch_eff["models"],
        json!([harness.model]),
        "dispatch effective model must match selected model: {dispatch_eff}"
    );

    let project_id = c
        .ok(
            "POST",
            "/api/projects",
            json!({
                "name": "steve-demo",
                "path": h.project.display().to_string(),
                "worktree": true,
            }),
        )
        .await["id"]
        .as_str()
        .expect("project id")
        .to_owned();

    // 2. Create 3 clearly differentiated Bots on the local Node (DB bot is intentionally created
    //    second so choosing it cannot coincide with first/last creation order).
    let frontend_bot = create_bot(
        &c,
        &harness,
        "前端界面专家",
        "负责 Web 前端界面开发、React 组件、CSS 样式布局与浏览器交互体验优化。",
        &["frontend", "react", "css", "ui"],
    )
    .await;
    let db_bot = create_bot(
        &c,
        &harness,
        "数据库迁移专家",
        "负责 SQLite 数据库表结构设计、steve-store schema 迁移脚本编写、索引优化与数据一致性校验。",
        &["sqlite", "database", "migration", "sql", "steve-store"],
    )
    .await;
    let docs_bot = create_bot(
        &c,
        &harness,
        "技术文档专家",
        "负责撰写 README、架构说明文档、运维手册和用户指南，不做代码与数据库改动。",
        &["docs", "documentation", "readme", "writing"],
    )
    .await;
    eprintln!("dispatch_real: bots frontend={frontend_bot} db={db_bot} docs={docs_bot}");

    // 3. Create an unassigned item via API (title + project supplied so router LLM is skipped and
    //    only the async dispatch Think job runs).
    let item1 = c
        .ok(
            "POST",
            "/api/items",
            json!({
                "title": "给 steve-store 加迁移并补充索引",
                "goal": "给 steve-store 新增一个 SQLite 迁移脚本，把 items 表的 (workspace_id, updated) 加上复合索引，并补充迁移测试验证旧库升级。",
                "project": project_id,
            }),
        )
        .await;
    let item1_id = item1["id"].as_str().expect("item1 id").to_owned();
    assert!(
        item1["bot_id"].is_null(),
        "with 3 eligible bots, item creation must defer bot selection asynchronously: {item1}"
    );

    // Wait for the async dispatch Think job to settle (either placed or blocked in waits).
    let deadline = tokio::time::Instant::now() + DISPATCH_WAIT;
    let (fact_json, blocked_json) = loop {
        if let Some((fact, blocked)) =
            node_process::item_initial_placement(&h.db_path(), &item1_id).unwrap()
            && (fact.is_some() || blocked.is_some())
        {
            break (fact, blocked);
        }
        if let Some(wait_blocked) =
            node_process::item_placement_wait_blocked(&h.db_path(), &item1_id).unwrap()
        {
            break (None, Some(wait_blocked));
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "dispatch job for {item1_id} did not settle within {:?}\n{}",
            DISPATCH_WAIT,
            diagnostics(&h, Some(&item1_id))
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    };

    // Stop item1 right away so the assigned Bot does not burn extra turns/tokens on the demo repo.
    let _ = c
        .call(
            "POST",
            &format!("/api/items/{item1_id}/actions"),
            Some(json!({"action": "drop"})),
        )
        .await;

    // Check for preflight/dispatch failure BEFORE waiting on `runtime_jobs`, because preflight
    // failures (e.g. missing adapter capability) settle directly to blocked without creating a job.
    assert!(
        blocked_json.is_none(),
        "dispatch failed/blocked instead of selecting a Bot: blocked={blocked_json:?}\n{}",
        diagnostics(&h, Some(&item1_id))
    );

    // Wait briefly for `runtime_jobs` to record its finished state alongside the placement commit.
    let diag_jobs = on_timeout(|| async {
        format!(
            "dispatch runtime_job finishes\n{}",
            diagnostics(&h, Some(&item1_id))
        )
    });
    eventually_sync(diag_jobs, || {
        node_process::dispatch_jobs(&h.db_path())
            .unwrap()
            .iter()
            .any(|(_, state)| state == "finished")
    })
    .await;

    // Assertion 1: Verify from storage + tool-call ledger (never model reply text) that the
    // dispatch Think job actually executed `steve_bots_list`.
    let jobs_after_1 = node_process::dispatch_jobs(&h.db_path()).unwrap();
    assert_eq!(
        jobs_after_1.len(),
        1,
        "expected exactly 1 dispatch Think job for item1, got {jobs_after_1:?}\n{}",
        diagnostics(&h, Some(&item1_id))
    );
    let (dispatch_job_id, dispatch_job_state) = &jobs_after_1[0];
    assert_eq!(
        dispatch_job_state, "finished",
        "dispatch job must be finished: {jobs_after_1:?}"
    );
    let tool_calls =
        node_process::tool_journal_calls_for_holder(&h.host_tools_path(), dispatch_job_id).unwrap();
    eprintln!("dispatch_real: job={dispatch_job_id} tool_calls={tool_calls:?}");
    assert!(
        tool_calls.iter().any(|(name, state, status)| {
            name == "steve_bots_list" && state == "done" && status == "completed"
        }),
        "expected completed `steve_bots_list` call in host tool journal for dispatch job {dispatch_job_id}, got {tool_calls:?}\n{}",
        diagnostics(&h, Some(&item1_id))
    );
    let bots_list_calls_after_1 = node_process::tool_journal_all_calls(&h.host_tools_path())
        .unwrap()
        .into_iter()
        .filter(|(_, name, _, _)| name == "steve_bots_list")
        .count();

    // Assertion 2: The stored placement & item row selected the DB migration Bot with
    // `PlacementReason::SteveSelected` and a non-empty `why`.
    let assigned_bot = node_process::item_bot_id(&h.db_path(), &item1_id).unwrap();
    assert_eq!(
        assigned_bot.as_deref(),
        Some(db_bot.as_str()),
        "expected database migration Bot ({db_bot}), got {assigned_bot:?}\n{}",
        diagnostics(&h, Some(&item1_id))
    );
    let fact: BotPlacement =
        serde_json::from_str(fact_json.as_deref().expect("placement fact JSON"))
            .expect("parse BotPlacement");
    eprintln!(
        "dispatch_real: placement bot={} reason={:?} why={:?}",
        fact.explanation.bot, fact.explanation.reason, fact.explanation.why
    );
    assert_eq!(fact.explanation.bot.as_str(), db_bot.as_str());
    assert_eq!(fact.explanation.reason, PlacementReason::SteveSelected);
    assert!(
        fact.explanation
            .why
            .as_deref()
            .is_some_and(|w| !w.trim().is_empty()),
        "BotPlacement.explanation.why must be non-empty: {fact:?}"
    );
    assert_eq!(
        node_process::pending_item_dispatch_jobs(&h.db_path()).unwrap(),
        0,
        "item_dispatch_jobs must be drained after settlement"
    );

    // 4. Assertion 3: Disable 2 Bots so only 1 eligible Bot (`docs_bot`) remains; creating an
    //    unassigned item must place deterministically without sending any dispatch Think job.
    c.ok(
        "PATCH",
        &format!("/api/bots/{frontend_bot}"),
        json!({"enabled": false}),
    )
    .await;
    c.ok(
        "PATCH",
        &format!("/api/bots/{db_bot}"),
        json!({"enabled": false}),
    )
    .await;

    let item2 = c
        .ok(
            "POST",
            "/api/items",
            json!({
                "title": "补充项目部署说明文档",
                "goal": "更新 README.md，补充本地部署步骤与环境变量说明。",
                "project": project_id,
            }),
        )
        .await;
    let item2_id = item2["id"].as_str().expect("item2 id").to_owned();
    assert_eq!(
        item2["bot_id"].as_str(),
        Some(docs_bot.as_str()),
        "single eligible Bot must be assigned synchronously on creation: {item2}"
    );

    // Drop item2 immediately to keep the test fast and clean.
    let _ = c
        .call(
            "POST",
            &format!("/api/items/{item2_id}/actions"),
            Some(json!({"action": "drop"})),
        )
        .await;

    assert_eq!(
        node_process::item_bot_id(&h.db_path(), &item2_id)
            .unwrap()
            .as_deref(),
        Some(docs_bot.as_str())
    );
    let (fact2_json, blocked2_json) = node_process::item_initial_placement(&h.db_path(), &item2_id)
        .unwrap()
        .expect("item2 initial placement");
    assert!(blocked2_json.is_none(), "item2 must not be blocked");
    let fact2: BotPlacement =
        serde_json::from_str(fact2_json.as_deref().expect("item2 placement fact"))
            .expect("parse item2 BotPlacement");
    assert_eq!(fact2.explanation.bot.as_str(), docs_bot.as_str());
    assert_eq!(fact2.explanation.reason, PlacementReason::OnlyEligible);

    // Give background sweep a brief moment to prove nothing asynchronously creates a 2nd job.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let jobs_after_2 = node_process::dispatch_jobs(&h.db_path()).unwrap();
    assert_eq!(
        jobs_after_2.len(),
        1,
        "single-eligible item2 must not spawn a dispatch Think job: {jobs_after_2:?}"
    );
    assert_eq!(
        node_process::pending_item_dispatch_jobs(&h.db_path()).unwrap(),
        0
    );
    let bots_list_calls_after_2 = node_process::tool_journal_all_calls(&h.host_tools_path())
        .unwrap()
        .into_iter()
        .filter(|(_, name, _, _)| name == "steve_bots_list")
        .count();
    assert_eq!(
        bots_list_calls_after_2, bots_list_calls_after_1,
        "single-eligible item2 must not call steve_bots_list"
    );

    hub.stop();
}

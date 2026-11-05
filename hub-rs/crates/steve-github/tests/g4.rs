//! G4 acceptance: an item without a linked repo gets the fixed hint; a fake token
//! (and the connection's own token, and a value injected into the session) in a CI log comes back
//! masked; an agent's comment without a grant is a decision and END, `grant_8h` posts it and lets
//! the second one through directly; the panel JSON has the expected shape; `rerun_failed` reruns the
//! failed workflow; an agent's `steve_github_open_pr` opens a draft PR after your yes and tells
//! the session.
//!
//! Only the fake GitHub and its local bare remote; nothing here talks to github.com.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_github::fake::{FakeGitHub, defaults};
use steve_github::{GitHub, GitHubEnv, RecordingCore, accept, comment, manifest, mcp, repos};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::vault::mcp::ToolOut;
use steve_modules::vault::{SessionSink, Vault};
use steve_modules::{Caller, decisions, panels};
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
const REPO: &str = defaults::REPO;
const PROJECT: &str = "prj_g";
const BRANCH: &str = "steve/st-1";
const BOT: &str = "bot_fe";
const SESSION: &str = "ses_g4";
const FAKE_TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

type Sent = (Option<String>, Option<String>, String);

#[derive(Default)]
struct Sink(Mutex<Vec<Sent>>);

#[async_trait]
impl SessionSink for Sink {
    async fn send(&self, _ws: &str, item: Option<&str>, session: Option<&str>, text: &str) {
        self.0.lock().expect("lock").push((
            item.map(Into::into),
            session.map(Into::into),
            text.into(),
        ));
    }
}

impl Sink {
    fn texts(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("lock")
            .iter()
            .map(|s| s.2.clone())
            .collect()
    }
}

struct H {
    env: TestEnv,
    _keys: TempDir,
    gh: GitHub,
    api: Router,
    fake: FakeGitHub,
    sink: Arc<Sink>,
    work: TempDir,
    conn: String,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, text: &str) -> String {
    std::fs::write(dir.join(file), text).expect("write");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

async fn nav(r: &Router, who: &Caller, uri: &str) -> u16 {
    let mut req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    req.extensions_mut().insert(who.clone());
    let res = r.clone().oneshot(req).await.expect("infallible");
    let _ = res.headers().get(header::LOCATION);
    res.status().as_u16()
}

fn q(h: &H, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Value {
    h.env
        .store()
        .reader(|c| {
            c.query_row(sql, p, |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(i) => json!(i),
                    rusqlite::types::ValueRef::Real(f) => json!(f),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                })
            })
            .map_err(steve_store::StoreError::from)
        })
        .expect(sql)
}

fn exec(h: &H, sql: &str, p: &[&dyn rusqlite::ToSql]) {
    h.env
        .store()
        .with_conn(|c| {
            c.execute(sql, p)?;
            Ok(())
        })
        .expect(sql);
}

/// The fake GitHub with a bare remote, the App installed, the repo linked to a git project whose
/// working copy has `main` and `steve/st-1` one commit ahead; a bot `前端 Bot`.
async fn setup() -> H {
    let fake = FakeGitHub::start().await.expect("fake github");
    let bare = fake.bare_remote(REPO).expect("bare remote");
    let git_root = bare
        .parent()
        .and_then(Path::parent)
        .expect("root")
        .display()
        .to_string();
    let work = tempfile::tempdir().expect("work");
    let w = work.path();
    git(w, &["init", "-q", "-b", "main"]);
    commit(w, "README.md", "demo\n");
    git(
        w,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/steve-test/demo.git",
        ],
    );
    git(w, &["push", "-q", &bare.display().to_string(), "main:main"]);
    git(w, &["update-ref", "refs/remotes/origin/main", "main"]);
    git(w, &["checkout", "-q", "-b", BRANCH]);
    commit(w, "fix.txt", "fixed\n");
    git(w, &["checkout", "-q", "main"]);

    let mut slot = None;
    let env = TestEnv::new(|r| {
        connections::register(r);
        slot = Some(accept::register(r));
    });
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let cfg = ConnectionsConfig::new(vec![ORIGIN.into()], true, |_| None);
    let mut b = Connections::builder(env.modules.clone(), vault.clone(), cfg);
    for p in steve_github::providers() {
        b = b.provider(p);
    }
    let conns = b.build();
    let gh = GitHub::new(
        conns.clone(),
        GitHubEnv {
            api_base: fake.url(),
            git_base: git_root,
            ..Default::default()
        },
    );
    slot.expect("slot").bind(&gh);
    gh.set_core(RecordingCore::new(env.store().clone()));
    let sink = Arc::new(Sink::default());
    gh.set_session_sink(sink.clone());
    let api = manifest::routes(gh.clone())
        .merge(repos::routes(gh.clone()))
        .merge(comment::routes(gh.clone()))
        .merge(connections::routes(conns.clone()))
        .with_state(env.modules.clone());

    env.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES (?, ?, 'Demo', ?, 'git', 1, 1)",
                rusqlite::params![PROJECT, WS, w.display().to_string()],
            )?;
            c.execute(
                "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES ('prj_other', ?, 'Other', '/tmp/none', 'dir', 1, 1)",
                [WS],
            )?;
            for (id, name) in [(BOT, "前端 Bot"), ("bot_be", "后端 Bot")] {
                c.execute(
                    "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES (?, ?, ?, 'claude', 1)",
                    rusqlite::params![id, WS, name],
                )?;
                c.execute(
                    "INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, 1)",
                    rusqlite::params![id, WS],
                )?;
            }
            Ok(())
        })
        .expect("seed");

    let mut h = H {
        env,
        _keys: keys,
        gh,
        api,
        fake,
        sink,
        work,
        conn: String::new(),
    };
    h.conn = install(&h).await;
    let me = caller(WS, ME);
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/repos",
        Some(&json!({"connection_id": h.conn, "gh_id": defaults::REPO_ID, "project_id": PROJECT})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    h.gh.settle().await;
    h
}

async fn install(h: &H) -> String {
    let me = caller(WS, ME);
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/app/manifest",
        Some(&json!({})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let state = v["state"].as_str().expect("state");
    let code = h.fake.manifest_code();
    let uri = format!("/github/app/manifest/callback?code={code}&state={state}");
    assert_eq!(nav(&h.api, &me, &uri).await, 302);
    let uri = format!(
        "/github/app/installed?installation_id={}&setup_action=install",
        defaults::INSTALLATION_ID
    );
    assert_eq!(nav(&h.api, &me, &uri).await, 302);
    q(
        h,
        "SELECT id FROM connections WHERE adapter = 'github_app'",
        &[],
    )
    .as_str()
    .expect("connection")
    .to_string()
}

fn item(h: &H, no: i64, project: Option<&str>, branch: Option<&str>) -> String {
    let id = format!("ST-{no}");
    let cwd = h.work.path().display().to_string();
    exec(
        h,
        "INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, branch, project_id, created, updated)
         VALUES (?, ?, ?, '保留 return_to 参数', 'run', '执行', ?, ?, ?, 1, 1)",
        &[&id, &WS, &no, &cwd, &branch, &project],
    );
    id
}

fn cx(item: Option<&str>, bot: &str) -> McpCx {
    McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: bot.into(),
        item_id: item.map(Into::into),
        session_id: Some(SESSION.into()),
    }
}

async fn tool(h: &H, cx: &McpCx, name: &str, args: Value) -> Result<ToolOut, String> {
    mcp::call(&h.gh, cx, name, &args).await
}

/// A PR from the item's branch with a failed Actions check `build` (log set) and a passed
/// external check `lint`, synced into the Hub. Returns `(number, build job id)`.
async fn pr_with_failure(h: &H, log: &str) -> (u64, u64) {
    let sha = git(h.work.path(), &["rev-parse", BRANCH]);
    let n = h
        .fake
        .open_pr(
            REPO,
            BRANCH,
            &sha,
            "fix: 保留 return_to",
            "",
            defaults::USER,
        )
        .expect("pr");
    let job = h.fake.set_check(REPO, &sha, "build", Some("failure"));
    h.fake.set_job_log(job, log);
    h.fake
        .set_external_check(REPO, &sha, "lint", Some("success"));
    let repo = q(h, "SELECT id FROM gh_repos", &[]);
    h.gh.poll_repo(repo.as_str().expect("repo"))
        .await
        .expect("poll");
    h.gh.settle().await;
    (n, job)
}

async fn answer(h: &H, id: &str, choice: &str, text: &str) -> Value {
    let (m, who, id, choice, text) = (
        h.env.modules.clone(),
        caller(WS, ME),
        id.to_string(),
        choice.to_string(),
        text.to_string(),
    );
    let d = tokio::task::spawn_blocking(move || {
        decisions::dispatch(&m, &who, &id, &choice, &text).expect("a module decision")
    })
    .await
    .expect("join")
    .expect("answered");
    h.gh.settle().await;
    d.result
}

fn open_decision(h: &H, subject_type: &str) -> Option<(String, String, String)> {
    h.env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(
                c.query_row(
                    "SELECT id, question, draft FROM decisions WHERE subject_type = ? AND status = 'open'",
                    [subject_type],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok(),
            )
        })
        .expect("decision")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unlinked_item_gets_the_fixed_hint() {
    let h = setup().await;
    let plain = item(&h, 1, None, Some(BRANCH));
    let other = item(&h, 2, Some("prj_other"), None);
    let calls = [
        (mcp::STATUS, json!({})),
        (mcp::CI_LOG, json!({"job": "build"})),
        (mcp::COMMENT, json!({"body": "LGTM"})),
        (mcp::OPEN_PR, json!({"title": "t", "body": "b"})),
    ];
    for it in [Some(plain.as_str()), Some(other.as_str()), None] {
        for (name, args) in &calls {
            let o = tool(&h, &cx(it, BOT), name, args.clone())
                .await
                .expect("ok");
            assert_eq!(o.text, mcp::NOT_LINKED, "{name} on {it:?}");
            assert!(!o.awaiting_approval);
        }
    }
    assert!(open_decision(&h, comment::COMMENT).is_none());
    assert!(open_decision(&h, accept::PR_OPEN).is_none());
    // argument errors come before the hint, in steve-mcp's wording
    let e = tool(&h, &cx(Some(&plain), BOT), mcp::CI_LOG, json!({}))
        .await
        .expect_err("invalid");
    assert!(
        e.starts_with("MCP error -32602: Input validation error"),
        "{e}"
    );
    assert_eq!(mcp::tools().len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_and_a_redacted_ci_log() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let c = cx(Some(&id), BOT);
    // no PR yet: the repo and its default branch
    let o = tool(&h, &c, mcp::STATUS, json!({})).await.expect("status");
    assert_eq!(
        o.text,
        format!(
            "No pull request is linked to this item yet. Repository {REPO} (default branch main). This item's branch: {BRANCH}."
        )
    );

    // the connection's own token and a value injected into this session, in the log too
    let token =
        h.gh.conn(WS, &h.conn)
            .expect("conn")
            .cx()
            .access_token()
            .await
            .expect("token")
            .expose()
            .to_string();
    steve_modules::vault::redact::register(SESSION, "API 密钥", b"s3cr3t-injected-value");
    let mut log: Vec<String> = (0..400).map(|i| format!("step {i}")).collect();
    log[50] = format!("export GITHUB_TOKEN={FAKE_TOKEN}");
    log[60] = "##[error]Process completed with exit code 1.".into();
    log[61] = format!("auth header: {token}");
    log[62] = "curl -H 'X-Key: s3cr3t-injected-value'".into();
    let (n, job) = pr_with_failure(&h, &log.join("\n")).await;

    let o = tool(&h, &c, mcp::STATUS, json!({})).await.expect("status");
    let sha = git(h.work.path(), &["rev-parse", BRANCH]);
    let first = o.text.lines().next().expect("line");
    assert_eq!(
        first,
        format!(
            "#{n} fix: 保留 return_to · open · head {BRANCH}@{} · checks: failure",
            &sha[..7]
        )
    );
    assert!(
        o.text.contains(&format!("✗ build (job {job})")),
        "{}",
        o.text
    );
    assert!(o.text.contains("✓ lint\n"), "{}", o.text);
    assert!(o.text.contains("review: none"), "{}", o.text);

    for by in ["build", &job.to_string()] {
        let o = tool(&h, &c, mcp::CI_LOG, json!({"job": by}))
            .await
            .expect("log");
        let t = &o.text;
        assert!(
            t.starts_with(&format!(
                "Log of build (job {job}) on PR #{n}, secrets removed:\n"
            )),
            "{t}"
        );
        assert!(t.contains("##[error]Process completed with exit code 1."));
        assert!(!t.contains(FAKE_TOKEN), "the fake token is masked");
        assert!(!t.contains(&token), "the installation token is masked");
        assert!(
            !t.contains("s3cr3t-injected-value"),
            "the injected value is masked"
        );
        assert!(t.contains("［已拦下的机密］"), "{t}");
        assert!(t.contains("[已打码：API 密钥]"), "{t}");
        // error context and the tail; the start of the log is left out
        assert!(t.contains("step 40\n") && t.contains("step 399"));
        assert!(!t.contains("step 39\n"));
    }
    // a check from another CI app has no log here
    let o = tool(&h, &c, mcp::CI_LOG, json!({"job": "lint"}))
        .await
        .expect("lint");
    assert!(
        o.text
            .starts_with("This check is not from GitHub Actions; open "),
        "{}",
        o.text
    );
    let e = tool(&h, &c, mcp::CI_LOG, json!({"job": "e2e"}))
        .await
        .expect_err("unknown");
    assert_eq!(
        e,
        "No check \"e2e\" on this item's pull requests. Call steve_github_status."
    );
    // the log is never stored
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM gh_checks WHERE summary LIKE '%exit code%'",
            &[]
        ),
        json!(0)
    );
    steve_modules::vault::redact::forget(SESSION);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_comment_asks_first_then_grant_8h_lets_the_next_through() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let (n, _) = pr_with_failure(&h, "ok\n").await;
    let c = cx(Some(&id), BOT);

    let o = tool(
        &h,
        &c,
        mcp::COMMENT,
        json!({"body": format!("修好了，令牌 {FAKE_TOKEN} 别管")}),
    )
    .await
    .expect("comment");
    assert_eq!(
        o.text,
        "Approval requested. End your turn now. The user's answer will arrive as your next message."
    );
    assert!(o.awaiting_approval);
    assert!(h.fake.comments(REPO, n).is_empty(), "nothing posted yet");
    let (dec, question, draft) = open_decision(&h, comment::COMMENT).expect("asked");
    assert_eq!(question, format!("前端 Bot 想在 PR #{n} 评论"));
    assert!(
        !draft.contains(FAKE_TOKEN),
        "the draft is redacted: {draft}"
    );
    assert_eq!(
        q(
            &h,
            "SELECT options || ' ' || requested_by || ' ' || session_id || ' ' || item_id FROM decisions WHERE id = ?",
            &[&dec]
        ),
        json!(format!(r#"["yes","no","grant_8h"] {BOT} {SESSION} {id}"#))
    );

    let r = answer(&h, &dec, "grant_8h", "").await;
    let grant = r["grant"].as_str().expect("grant").to_string();
    let posted = h.fake.comments(REPO, n);
    assert_eq!(posted.len(), 1);
    assert!(posted[0].starts_with("修好了") && !posted[0].contains(FAKE_TOKEN));
    assert_eq!(
        q(
            &h,
            "SELECT capability || ' ' || subject_type || ' ' || subject_id || ' ' || resource_type || ' ' || decision_id FROM grants WHERE id = ?",
            &[&grant]
        ),
        json!(format!("github.comment bot {BOT} repo {dec}"))
    );
    let hours = q(
        &h,
        "SELECT (until - created) / 3600000 FROM grants WHERE id = ?",
        &[&grant],
    );
    assert_eq!(hours, json!(8));
    let told = h.sink.texts();
    assert_eq!(told.len(), 1);
    assert!(
        told[0].starts_with(&format!(
            "用户批准了这条评论，已发到 PR #{n}：https://github.com/"
        )),
        "{}",
        told[0]
    );

    // the second comment goes out directly
    let o = tool(&h, &c, mcp::COMMENT, json!({"body": "又推了一次"}))
        .await
        .expect("comment");
    assert!(!o.awaiting_approval);
    assert!(
        o.text.starts_with("Commented: https://github.com/"),
        "{}",
        o.text
    );
    assert_eq!(h.fake.comments(REPO, n).len(), 2);
    assert!(open_decision(&h, comment::COMMENT).is_none());
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM audit WHERE action = 'github.comment'",
            &[]
        ),
        json!(2)
    );

    // another bot is still asked; `no` posts nothing and tells its session
    let o = tool(
        &h,
        &cx(Some(&id), "bot_be"),
        mcp::COMMENT,
        json!({"body": "我也说一句"}),
    )
    .await
    .expect("comment");
    assert!(o.awaiting_approval);
    let (dec, question, _) = open_decision(&h, comment::COMMENT).expect("asked");
    assert_eq!(question, format!("后端 Bot 想在 PR #{n} 评论"));
    answer(&h, &dec, "no", "").await;
    assert_eq!(h.fake.comments(REPO, n).len(), 2);
    assert_eq!(
        h.sink.texts().last().map(String::as_str),
        Some(comment::DENIED)
    );

    // a revoked or expired grant no longer counts
    exec(&h, "UPDATE grants SET until = 1", &[]);
    let o = tool(&h, &c, mcp::COMMENT, json!({"body": "第三条"}))
        .await
        .expect("comment");
    assert!(o.awaiting_approval);

    // no such PR
    let e = tool(&h, &c, mcp::COMMENT, json!({"body": "x", "number": 999}))
        .await
        .expect_err("missing");
    assert_eq!(e, format!("No pull request #999 in {REPO}."));
}

#[tokio::test(flavor = "multi_thread")]
async fn your_own_comment_posts_directly() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let (n, _) = pr_with_failure(&h, "ok\n").await;
    let pr = q(&h, "SELECT id FROM gh_prs WHERE item_id = ?", &[&id]);
    let me = caller(WS, ME);
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/comments",
        Some(&json!({"target": "pr", "id": pr, "body": "看起来不错"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["url"].as_str().expect("url").contains("#issuecomment-"));
    assert_eq!(h.fake.comments(REPO, n), ["看起来不错"]);
    let (s, _) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/comments",
        Some(&json!({"target": "issue", "id": pr, "body": "x"})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/comments",
        Some(&json!({"target": "pr", "id": pr, "body": "  "})),
    )
    .await;
    assert_eq!(s, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panel_matches_section_7_and_reruns_failed_checks() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let (n, job) = pr_with_failure(&h, "boom\n").await;
    let ps = h
        .env
        .store()
        .reader(|c| panels::panels(&h.env.modules, c, WS, ME, &id))
        .expect("panels");
    assert_eq!(ps.len(), 1);
    let pr_id = q(&h, "SELECT id FROM gh_prs WHERE item_id = ?", &[&id]);
    let pr_id = pr_id.as_str().expect("id");
    let url = q(&h, "SELECT url FROM gh_prs WHERE item_id = ?", &[&id]);
    let mut v = serde_json::to_value(&ps[0]).expect("json");
    assert!(v["updated"].as_i64().expect("updated") > 0);
    v["updated"] = json!(0);
    for r in v["rows"].as_array_mut().expect("rows") {
        assert!(
            r["href"]
                .as_str()
                .expect("href")
                .starts_with("https://github.com/")
        );
        r["href"] = json!("…");
    }
    assert_eq!(
        v,
        json!({
            "id": format!("github.pr:{pr_id}"),
            "plugin": "github",
            "title": "PR 与检查",
            "head": {"text": format!("#{n} fix: 保留 return_to"), "href": url, "badges": []},
            "rows": [
                {"state": "fail", "text": "build", "sub": "刚刚", "href": "…"},
                {"state": "ok", "text": "lint", "sub": "刚刚", "href": "…"}
            ],
            "foot": "1 项检查失败",
            "actions": [
                {"id": "rerun_failed", "label": "重跑失败的检查"},
                {"id": "open", "label": "在 GitHub 打开", "href": url}
            ],
            "updated": 0
        })
    );

    let me = caller(WS, ME);
    let panel = format!("github.pr:{pr_id}");
    let run = h.fake.run_of(job).expect("run");
    let o = panels::action(&h.env.modules, &me, &id, &panel, "rerun_failed")
        .await
        .expect("rerun");
    assert_eq!(o.message.as_deref(), Some("已重跑 1 个工作流里失败的检查"));
    assert_eq!(h.fake.reruns(run), 1);
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM audit WHERE action = 'github.rerun'",
            &[]
        ),
        json!(1)
    );
    // another item's panel, or an unknown action: 404
    let other = item(&h, 2, Some(PROJECT), None);
    let e = panels::action(&h.env.modules, &me, &other, &panel, "rerun_failed")
        .await
        .expect_err("not its panel");
    assert_eq!(
        e.to_string(),
        steve_modules::ApiError::not_found("面板").to_string()
    );
    let e = panels::action(&h.env.modules, &me, &id, &panel, "merge")
        .await
        .expect_err("unknown");
    assert_eq!(
        e.to_string(),
        steve_modules::ApiError::not_found("操作").to_string()
    );

    // all green, item waiting for acceptance: no rerun button, the foot
    let sha = git(h.work.path(), &["rev-parse", BRANCH]);
    h.fake.set_check(REPO, &sha, "build", Some("success"));
    let repo = q(&h, "SELECT id FROM gh_repos", &[]);
    h.gh.poll_repo(repo.as_str().expect("repo"))
        .await
        .expect("poll");
    exec(&h, "UPDATE items SET status = 'wait' WHERE id = ?", &[&id]);
    let ps = h
        .env
        .store()
        .reader(|c| panels::panels(&h.env.modules, c, WS, ME, &id))
        .expect("panels");
    assert_eq!(ps[0].foot.as_deref(), Some("2 项检查通过 · 等你验收后合并"));
    assert_eq!(
        ps[0]
            .actions
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["open"]
    );
    let e = panels::action(&h.env.modules, &me, &id, &panel, "rerun_failed")
        .await
        .expect_err("nothing failed");
    assert_eq!(
        e.to_string(),
        steve_modules::ApiError::conflict("没有失败的检查可以重跑").to_string()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_asks_for_a_draft_pr_and_hears_back() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let c = cx(Some(&id), BOT);
    let o = tool(
        &h,
        &c,
        mcp::OPEN_PR,
        json!({"title": "WIP: 保留 return_to", "body": "先跑一下 CI"}),
    )
    .await
    .expect("open_pr");
    assert!(o.awaiting_approval);
    assert!(o.text.starts_with("Approval requested. End your turn now."));
    let (dec, question, draft) = open_decision(&h, accept::PR_OPEN).expect("asked");
    assert_eq!(
        question,
        format!("在验收前开一个草稿 PR（{REPO}）：WIP: 保留 return_to")
    );
    assert_eq!(draft, "WIP: 保留 return_to\n\n先跑一下 CI");
    // asked once at a time
    let e = tool(&h, &c, mcp::OPEN_PR, json!({"title": "again", "body": ""}))
        .await
        .expect_err("pending");
    assert!(e.contains("has not answered"), "{e}");
    assert!(
        !h.fake.branches(REPO).contains_key(BRANCH),
        "nothing pushed yet"
    );

    answer(&h, &dec, "yes", "").await;
    let n = q(&h, "SELECT number FROM gh_prs WHERE item_id = ?", &[&id]);
    let n = n.as_i64().expect("PR opened") as u64;
    let pull = h.fake.pull(REPO, n).expect("PR opened");
    assert!(pull.draft, "an agent's PR is a draft by default");
    assert_eq!(pull.head_ref, BRANCH);
    assert!(h.fake.branches(REPO).contains_key(BRANCH), "pushed");
    let told = h.sink.texts();
    assert_eq!(told.len(), 1);
    assert!(
        told[0].starts_with(&format!("用户批准了开 PR。分支已推送，草稿 PR #{n} 已开：")),
        "{}",
        told[0]
    );
    // the item is not finished: this was before acceptance
    assert_eq!(
        q(&h, "SELECT status FROM items WHERE id = ?", &[&id]),
        json!("run")
    );
    // now there is an open PR from the branch
    let e = tool(&h, &c, mcp::OPEN_PR, json!({"title": "again", "body": ""}))
        .await
        .expect_err("already open");
    assert!(
        e.starts_with(&format!("A pull request from {BRANCH} is already open")),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn draft_false_and_no() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let c = cx(Some(&id), BOT);
    tool(
        &h,
        &c,
        mcp::OPEN_PR,
        json!({"title": "修复", "body": "", "draft": false}),
    )
    .await
    .expect("open_pr");
    let (dec, question, _) = open_decision(&h, accept::PR_OPEN).expect("asked");
    assert_eq!(question, format!("在验收前开一个 PR（{REPO}）：修复"));
    answer(&h, &dec, "no", "").await;
    assert!(
        h.fake
            .calls_to("POST", &format!("/repos/{REPO}/pulls"))
            .is_empty()
    );
    assert_eq!(h.sink.texts(), [accept::PR_DENIED]);

    tool(
        &h,
        &c,
        mcp::OPEN_PR,
        json!({"title": "修复", "body": "", "draft": false}),
    )
    .await
    .expect("open_pr");
    let (dec, _, _) = open_decision(&h, accept::PR_OPEN).expect("asked");
    answer(&h, &dec, "yes", "").await;
    let n = q(&h, "SELECT number FROM gh_prs WHERE item_id = ?", &[&id]);
    let n = n.as_i64().expect("PR opened") as u64;
    assert!(!h.fake.pull(REPO, n).expect("PR").draft);
}

/// Y4: a write is checked when it happens: a paused connection or `repo.write` turned off refuses
/// the comment with the reason, and nothing reaches GitHub; resumed, it goes through.
#[tokio::test(flavor = "multi_thread")]
async fn a_comment_is_refused_while_the_connection_cannot_write() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let (n, _) = pr_with_failure(&h, "ok\n").await;
    let pr = q(&h, "SELECT id FROM gh_prs WHERE item_id = ?", &[&id]);
    let conn = q(&h, "SELECT connection_id FROM gh_repos LIMIT 1", &[]);
    let conn = conn.as_str().expect("connection").to_string();
    let me = caller(WS, ME);
    let set = |status: &str, caps: &str| {
        h.env
            .store()
            .transaction(|tx| {
                tx.execute(
                    "UPDATE connections SET status = ?, caps = ? WHERE id = ?",
                    rusqlite::params![status, caps, conn],
                )?;
                Ok::<_, steve_store::StoreError>(())
            })
            .expect("set");
    };
    for (status, caps, why) in [
        ("paused", r#"["repo.read","repo.write","events"]"#, "已暂停"),
        ("ok", r#"["repo.read","events"]"#, "repo.write"),
    ] {
        set(status, caps);
        let (s, v) = call(
            &h.api,
            Some(&me),
            "POST",
            "/github/comments",
            Some(&json!({"target": "pr", "id": pr, "body": "看起来不错"})),
        )
        .await;
        assert_ne!(s, 200, "{status}: {v}");
        assert!(v.to_string().contains(why), "{status}: {v}");
        assert!(h.fake.comments(REPO, n).is_empty(), "{status}");
    }
    set("ok", r#"["repo.read","repo.write","events"]"#);
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/comments",
        Some(&json!({"target": "pr", "id": pr, "body": "看起来不错"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(h.fake.comments(REPO, n), ["看起来不错"]);
}

/// Y2: GitHub posted the approved comment but the answer was lost: unknown, and the session hears
/// it may be out; the next run finds that comment (same body, no other effect's) and does not post
/// it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_posted_comment_whose_answer_was_lost_is_found_not_posted_twice() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let (n, _) = pr_with_failure(&h, "ok\n").await;
    let c = cx(Some(&id), BOT);
    tool(&h, &c, mcp::COMMENT, json!({"body": "修好了"}))
        .await
        .expect("comment");
    let (dec, _, _) = open_decision(&h, comment::COMMENT).expect("asked");
    h.fake
        .fail_next_on("/comments", steve_github::fake::Fault::LostAnswer);
    answer(&h, &dec, "yes", "").await;
    assert_eq!(h.fake.comments(REPO, n).len(), 1, "it went out");
    assert_eq!(
        q(
            &h,
            "SELECT state FROM gh_effects WHERE decision_id = ?",
            &[&dec]
        ),
        json!("unknown")
    );
    h.env.clock.advance(steve_store::outbox::backoff(1) + 1);
    h.gh.settle().await;
    assert_eq!(
        q(
            &h,
            "SELECT state FROM gh_effects WHERE decision_id = ?",
            &[&dec]
        ),
        json!("done")
    );
    assert_eq!(h.fake.comments(REPO, n).len(), 1, "not posted twice");
}

/// Y2 (Codex E4 review): a comment that may have been posted and is not on GitHub (it failed, or
/// someone deleted it) is not posted again by itself: stuck, and the session hears why.
#[tokio::test(flavor = "multi_thread")]
async fn a_comment_not_found_after_a_failure_is_not_posted_again() {
    let h = setup().await;
    let id = item(&h, 1, Some(PROJECT), Some(BRANCH));
    let (n, _) = pr_with_failure(&h, "ok\n").await;
    let c = cx(Some(&id), BOT);
    tool(&h, &c, mcp::COMMENT, json!({"body": "修好了"}))
        .await
        .expect("comment");
    let (dec, _, _) = open_decision(&h, comment::COMMENT).expect("asked");
    h.fake.fail_next_on(
        "/comments",
        steve_github::fake::Fault::Status {
            status: 502,
            message: "Bad Gateway".into(),
        },
    );
    answer(&h, &dec, "yes", "").await;
    assert!(h.fake.comments(REPO, n).is_empty());
    h.env.clock.advance(steve_store::outbox::backoff(1) + 1);
    h.gh.settle().await;
    assert!(
        h.fake.comments(REPO, n).is_empty(),
        "not posted again by itself"
    );
    assert_eq!(
        q(
            &h,
            "SELECT state || '|' || (next_at IS NULL) FROM gh_effects WHERE decision_id = ?",
            &[&dec]
        ),
        json!("unknown|1")
    );
}

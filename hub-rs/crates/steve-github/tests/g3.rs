//! G3 acceptance (github.md §13): a PR is linked by branch name and by `ST-12` in its title;
//! accepting an item opens a `gh_pr_open` decision and pushes / opens nothing until you say yes;
//! after yes the fake remote has the branch, the fake GitHub has the PR, and the item has the PR
//! artifact; `finish_on=merged` asks to merge only when the checks are green, a new head is a 409,
//! the merge deletes the branch and finishes the item; a project with a release action gets no PR
//! question; a failed check on a linked PR supplements the item through the template flow.
//!
//! Pushes go only to the fake's local bare repo (`git_base` = its temp dir). Nothing here talks
//! to github.com.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_flow::testkit::{CoreCall as FlowCall, FakeCore};
use steve_flow::{FlowCx, FlowSetup, GraphEngine};
use steve_github::core::CoreCall;
use steve_github::fake::{FakeGitHub, defaults};
use steve_github::{GitHub, GitHubEnv, RecordingCore, accept, flow, hooks, manifest, repos};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{Caller, decisions};
use steve_store::NewDecision;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
const REPO: &str = defaults::REPO;
const PROJECT: &str = "prj_g";
const BRANCH: &str = "steve/st-1";

struct H {
    env: TestEnv,
    _keys: TempDir,
    gh: GitHub,
    api: Router,
    fake: FakeGitHub,
    core: Arc<RecordingCore>,
    flow: Arc<FlowCx>,
    flow_core: Arc<FakeCore>,
    /// The project's working copy (and the item's worktree).
    work: TempDir,
    repo: String,
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

async fn nav(r: &Router, who: &Caller, uri: &str) -> (u16, String) {
    let mut req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    req.extensions_mut().insert(who.clone());
    let res = r.clone().oneshot(req).await.expect("infallible");
    let loc = res
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (res.status().as_u16(), loc)
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

/// The fake GitHub with a bare remote for the repo (holding `main`), the Hub side with the App
/// installed and the repo linked to a git project whose working copy has `main` and
/// `steve/st-1` one commit ahead.
async fn setup() -> H {
    let fake = FakeGitHub::start().await.expect("fake github");
    let bare = fake.bare_remote(REPO).expect("bare remote");
    // <root>/steve-test/demo.git → <root>
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

    let flow_core = Arc::new(FakeCore::default());
    let setup = FlowSetup::new(flow_core.clone())
        .item_finder(flow::BY, Arc::new(flow::GhPrFinder))
        .action(Arc::new(flow::GhItemLink));
    let mut slot = None;
    let env = TestEnv::new(|r| {
        connections::register(r);
        setup.register(r);
        slot = Some(accept::register(r));
    });
    *flow_core.store.lock().expect("lock") = Some(env.store().clone());
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
    let flow = setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(GraphEngine));
    let gh = GitHub::new(
        conns.clone(),
        GitHubEnv {
            api_base: fake.url(),
            git_base: git_root,
            ..Default::default()
        },
    );
    slot.expect("slot").bind(&gh);
    let core = RecordingCore::new(env.store().clone());
    gh.set_core(core.clone());
    gh.set_flow(flow.clone());
    gh.set_event_sink(Arc::new(flow::FlowSink(Arc::new(flow.clone()))));
    let api = manifest::routes(gh.clone())
        .merge(repos::routes(gh.clone()))
        .merge(connections::routes(conns.clone()))
        .with_state(env.modules.clone());
    let root: Router = hooks::routes(gh.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, root).await;
    });
    fake.set_webhook_target(&format!("http://{addr}/hooks/github"));

    env.store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES (?, ?, 'Demo', ?, 'git', 1, 1)",
                rusqlite::params![PROJECT, WS, w.display().to_string()],
            )?;
            Ok(())
        })
        .expect("project");

    let mut h = H {
        env,
        _keys: keys,
        gh,
        api,
        fake,
        core,
        flow,
        flow_core,
        work,
        repo: String::new(),
    };
    let app = install(&h).await;
    let me = caller(WS, ME);
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/repos",
        Some(&json!({"connection_id": app, "gh_id": defaults::REPO_ID, "project_id": PROJECT})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    h.gh.settle().await;
    h.repo = v["id"].as_str().expect("repo").to_string();
    h
}

/// Manifest → install: the `github_app` connection.
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
    let (s, _) = nav(
        &h.api,
        &me,
        &format!("/github/app/manifest/callback?code={code}&state={state}"),
    )
    .await;
    assert_eq!(s, 302);
    let (s, _) = nav(
        &h.api,
        &me,
        &format!(
            "/github/app/installed?installation_id={}&setup_action=install",
            defaults::INSTALLATION_ID
        ),
    )
    .await;
    assert_eq!(s, 302);
    q(
        h,
        "SELECT id FROM connections WHERE adapter = 'github_app'",
        &[],
    )
    .as_str()
    .expect("connection")
    .to_string()
}

/// An item of the project, delivered: its acceptance decision carries the summary.
fn item(h: &H, no: i64, branch: Option<&str>) -> String {
    let id = format!("ST-{no}");
    let cwd = h.work.path().display().to_string();
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, branch, project_id, created, updated)
                 VALUES (?, ?, ?, '保留 return_to 参数', 'wait', '交付与验收', ?, ?, ?, 1, 1)",
                rusqlite::params![id, WS, no, cwd, branch, PROJECT],
            )?;
            Ok(())
        })
        .expect("item");
    h.env
        .store()
        .transaction(|tx| {
            steve_store::base::decisions::open(
                tx,
                &NewDecision {
                    workspace_id: WS.into(),
                    kind: "accept".into(),
                    subject_type: "item".into(),
                    subject_id: id.clone(),
                    item_id: Some(id.clone()),
                    session_id: None,
                    question: "做完了，请验收".into(),
                    draft: Some("登录后回到原来的页面。\n✓ 单元测试".into()),
                    options: Some(vec!["pass".into(), "rework".into()]),
                    requested_by: "steve".into(),
                },
            )
        })
        .expect("accept decision");
    id
}

/// `on_accepted`, as steve-core calls it after `resolve(accept, pass)`.
async fn accepted(h: &H, id: &str) -> bool {
    let i = h.env.store().items().get(id).expect("get").expect("item");
    h.env.modules.registry.hooks().accepted(&i).await.is_some()
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

/// `POST /api/decisions/:id` for a module decision (what steve-hub does), then the effects.
async fn answer(h: &H, id: &str, choice: &str, text: &str) -> Result<Value, (u16, String)> {
    let (m, who, id, choice, text) = (
        h.env.modules.clone(),
        caller(WS, ME),
        id.to_string(),
        choice.to_string(),
        text.to_string(),
    );
    let r = tokio::task::spawn_blocking(move || {
        decisions::dispatch(&m, &who, &id, &choice, &text).expect("a module decision")
    })
    .await
    .expect("join");
    h.gh.settle().await;
    match r {
        Ok(d) => Ok(d.result),
        Err(e) => {
            let res = axum::response::IntoResponse::into_response(e);
            let s = res.status().as_u16();
            let b = axum::body::to_bytes(res.into_body(), 1 << 16)
                .await
                .expect("body");
            Err((s, String::from_utf8_lossy(&b).to_string()))
        }
    }
}

fn pull_posts(h: &H) -> usize {
    h.fake
        .calls_to("POST", &format!("/repos/{REPO}/pulls"))
        .len()
}

fn pushed(h: &H) -> Option<String> {
    h.fake.branches(REPO).get(BRANCH).cloned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pr_is_linked_by_branch_and_by_reference() {
    let h = setup().await;
    let a = item(&h, 1, Some(BRANCH));
    let b = item(&h, 2, Some("steve/st-2"));
    let sha = git(h.work.path(), &["rev-parse", BRANCH]);
    // by branch: someone (the agent) opened a PR from the item's branch
    let n1 = h
        .fake
        .open_pr(REPO, BRANCH, &sha, "fix: return_to", "", defaults::USER)
        .expect("pr");
    // by reference: another branch, the title names ST-2 (and an item that does not exist)
    let n2 = h
        .fake
        .open_pr(
            REPO,
            "feature/x",
            &sha,
            "修复 ST-99 和 ST-2 的问题",
            "",
            defaults::USER,
        )
        .expect("pr");
    // neither: stays unlinked
    let n3 = h
        .fake
        .open_pr(REPO, "chore/y", &sha, "整理代码", "", defaults::USER)
        .expect("pr");
    h.gh.poll_repo(&h.repo).await.expect("poll");
    let link = |n: u64| {
        q(
            &h,
            "SELECT coalesce(item_id, '-') || '|' || coalesce(link_source, '-') FROM gh_prs WHERE number = ?",
            &[&(n as i64)],
        )
    };
    assert_eq!(link(n1), json!(format!("{a}|branch")));
    assert_eq!(link(n2), json!(format!("{b}|ref")));
    assert_eq!(link(n3), json!("-|-"));
    // the link, the artifact and the timeline line
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM links WHERE subject_type = 'item' AND subject_id = ? AND predicate = 'output' AND object_type = 'gh_pr'",
            &[&a]
        ),
        json!(1)
    );
    assert_eq!(
        q(
            &h,
            "SELECT title FROM artifacts WHERE item_id = ? AND kind = 'PR'",
            &[&a]
        ),
        json!(format!("#{n1} fix: return_to"))
    );
    assert_eq!(
        q(
            &h,
            "SELECT actor || ' ' || kind || ' ' || text FROM timeline WHERE item_id = ?",
            &[&b]
        ),
        json!(format!("steve sys 关联了 PR #{n2}（{REPO}）"))
    );
    // seeing it again changes nothing
    h.gh.poll_repo(&h.repo).await.expect("poll");
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM timeline WHERE item_id IN (?, ?)",
            &[&a, &b]
        ),
        json!(2)
    );

    // by hand: link the third to ST-1, then unlink the first (it stays unlinked)
    let me = caller(WS, ME);
    let id3 = q(
        &h,
        "SELECT id FROM gh_prs WHERE number = ?",
        &[&(n3 as i64)],
    );
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &format!("/github/prs/{}/link", id3.as_str().expect("id")),
        Some(&json!({"item_id": a})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["item"]["id"].clone(), v["link_source"].clone()),
        (json!(a), json!("manual"))
    );
    let id1 = q(
        &h,
        "SELECT id FROM gh_prs WHERE number = ?",
        &[&(n1 as i64)],
    );
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &format!("/github/prs/{}/link", id1.as_str().expect("id")),
        Some(&json!({"item_id": null})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    h.gh.poll_repo(&h.repo).await.expect("poll");
    assert_eq!(link(n1), json!("-|manual"));
    let (s, _) = call(
        &h.api,
        Some(&me),
        "POST",
        &format!("/github/prs/{}/link", id1.as_str().expect("id")),
        Some(&json!({"item_id": "ST-404"})),
    )
    .await;
    assert_eq!(s, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn accepting_asks_first_and_no_pushes_or_opens_nothing() {
    let h = setup().await;
    let id = item(&h, 1, Some(BRANCH));
    assert!(accepted(&h, &id).await, "GitHub takes the wrap-up over");
    let (dec, question, draft) = open_decision(&h, accept::PR_OPEN).expect("asked");
    assert_eq!(question, format!("在 {REPO} 开 PR：保留 return_to 参数"));
    assert_eq!(
        draft,
        "保留 return_to 参数\n\n登录后回到原来的页面。\n✓ 单元测试\n\nST-1 · 由 Steve 开出"
    );
    assert_eq!(
        h.core.calls(),
        [CoreCall::Progress {
            item: id.clone(),
            stage: "收尾".into(),
            now: "验收通过，等你确认开 PR".into()
        }]
    );
    // asked, not done: nothing on the remote, no PR
    h.gh.settle().await;
    assert_eq!(pushed(&h), None);
    assert_eq!(pull_posts(&h), 0);
    // a second acceptance does not ask twice
    assert!(!accepted(&h, &id).await);

    answer(&h, &dec, "no", "").await.expect("no");
    assert_eq!(pushed(&h), None, "no → nothing pushed");
    assert_eq!(pull_posts(&h), 0, "no → no PR");
    assert_eq!(
        h.core.calls().last(),
        Some(&CoreCall::Finish {
            item: id.clone(),
            now: "已验收，没有开 PR".into()
        })
    );
    // an answered decision cannot be undone
    let d = h
        .env
        .store()
        .reader(|c| steve_store::base::decisions::get(c, &dec))
        .expect("get")
        .expect("decision");
    let refused = h
        .env
        .store()
        .transaction(|tx| {
            Ok::<_, steve_modules::ApiError>(h.env.modules.registry.before_undo(tx, &d).is_err())
        })
        .expect("tx");
    assert!(refused);
}

#[tokio::test(flavor = "multi_thread")]
async fn yes_pushes_the_branch_and_opens_the_pr() {
    let h = setup().await;
    let id = item(&h, 1, Some(BRANCH));
    assert!(accepted(&h, &id).await);
    let (dec, _, _) = open_decision(&h, accept::PR_OPEN).expect("asked");
    answer(&h, &dec, "yes", "fix: 保留 return_to\n登录后回到原页面。")
        .await
        .expect("yes");
    let sha = git(h.work.path(), &["rev-parse", BRANCH]);
    assert_eq!(
        pushed(&h).as_deref(),
        Some(sha.as_str()),
        "on the fake remote"
    );
    let n = steve_pr(&h);
    let pr = h.fake.pull(REPO, n).expect("PR on the fake GitHub");
    assert_eq!(
        (
            pr.head_ref.as_str(),
            pr.title.as_str(),
            pr.body.as_str(),
            pr.draft
        ),
        (BRANCH, "fix: 保留 return_to", "登录后回到原页面。", false)
    );
    assert_eq!(
        q(
            &h,
            "SELECT item_id || '|' || link_source || '|' || opened_by || '|' || head_sha FROM gh_prs WHERE number = ?",
            &[&(n as i64)]
        ),
        json!(format!("{id}|steve|steve|{sha}"))
    );
    assert_eq!(
        q(
            &h,
            "SELECT title || ' ' || location FROM artifacts WHERE item_id = ? AND kind = 'PR'",
            &[&id]
        ),
        json!(format!("#{n} fix: 保留 return_to {}", pr_url(&h)))
    );
    assert_eq!(
        q(
            &h,
            "SELECT text FROM timeline WHERE item_id = ? ORDER BY id DESC LIMIT 1",
            &[&id]
        ),
        json!(format!("开了 PR #{n}"))
    );
    assert_eq!(
        h.core.calls().last(),
        Some(&CoreCall::Finish {
            item: id.clone(),
            now: format!("已验收，PR #{n} 已开")
        })
    );
    // the token went to git through the environment, never in a call the fake saw as a URL
    assert!(
        h.fake
            .calls()
            .iter()
            .all(|c| !c.path.contains("x-access-token"))
    );
}

/// A `pull_request` webhook about PR `n` as the fake renders it.
async fn pr_event(h: &H, n: u64, action: &str) {
    let conn_id = q(
        h,
        "SELECT id FROM connections WHERE adapter = 'github_app'",
        &[],
    );
    let conn = h.gh.conn(WS, conn_id.as_str().expect("id")).expect("conn");
    let pr = conn
        .get_if_changed(&format!("/repos/{REPO}/pulls/{n}"), None)
        .await
        .expect("pr")
        .body;
    let payload = json!({
        "action": action, "number": n, "pull_request": pr,
        "repository": {"id": defaults::REPO_ID, "full_name": REPO},
        "installation": {"id": defaults::INSTALLATION_ID},
        "sender": {"login": defaults::USER, "type": "User"},
    });
    let d = h.fake.emit("pull_request", &payload).await.expect("emit");
    assert_eq!(d.status, 202);
    h.gh.settle().await;
}

/// The number of the PR Steve opened.
fn steve_pr(h: &H) -> u64 {
    q(
        h,
        "SELECT number FROM gh_prs WHERE opened_by = 'steve'",
        &[],
    )
    .as_u64()
    .expect("Steve's PR")
}

fn pr_url(h: &H) -> String {
    q(h, "SELECT url FROM gh_prs WHERE opened_by = 'steve'", &[])
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn finish_on_merged_asks_to_merge_when_green_and_409s_a_new_head() {
    let h = setup().await;
    let me = caller(WS, ME);
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/github/repos/{}", h.repo),
        Some(&json!({"settings": {"finish_on": "merged"}})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let id = item(&h, 1, Some(BRANCH));
    assert!(accepted(&h, &id).await);
    let (dec, _, _) = open_decision(&h, accept::PR_OPEN).expect("asked");
    answer(&h, &dec, "yes", "").await.expect("yes");
    let n = steve_pr(&h);
    assert_eq!(
        h.core.calls().last(),
        Some(&CoreCall::Progress {
            item: id.clone(),
            stage: "收尾".into(),
            now: format!("PR #{n} 等检查")
        })
    );
    assert!(
        open_decision(&h, accept::PR_MERGE).is_none(),
        "no checks yet"
    );
    let sha = git(h.work.path(), &["rev-parse", BRANCH]);

    // pending, then failing: not asked
    h.fake.set_check(REPO, &sha, "build", None);
    h.gh.poll_repo(&h.repo).await.expect("poll");
    assert!(open_decision(&h, accept::PR_MERGE).is_none());
    h.fake.set_check(REPO, &sha, "build", Some("failure"));
    h.gh.poll_repo(&h.repo).await.expect("poll");
    h.gh.settle().await;
    assert!(open_decision(&h, accept::PR_MERGE).is_none());
    // green: asked, once
    h.fake.set_check(REPO, &sha, "build", Some("success"));
    h.gh.poll_repo(&h.repo).await.expect("poll");
    h.gh.poll_repo(&h.repo).await.expect("poll");
    let (merge, question, draft) = open_decision(&h, accept::PR_MERGE).expect("asked to merge");
    assert_eq!(question, format!("合并 PR #{n}（{REPO}）？"));
    assert_eq!(
        draft,
        format!("✓ build\n合并方式：squash，合并后删除分支 {BRANCH}")
    );
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type = 'gh_pr_merge'",
            &[]
        ),
        json!(1)
    );

    // a new commit lands on the PR: its checks are not green → 409, nothing merged
    let w = h.work.path();
    git(w, &["checkout", "-q", BRANCH]);
    let new_sha = commit(w, "more.txt", "more\n");
    git(w, &["checkout", "-q", "main"]);
    let bare = h.fake.bare_remote(REPO).expect("bare");
    git(w, &["push", "-q", &bare.display().to_string(), BRANCH]);
    let new_sha = new_sha.as_str();
    h.fake.push_to_pr(REPO, n, new_sha);
    pr_event(&h, n, "synchronize").await;
    assert_eq!(
        q(
            &h,
            "SELECT head_sha || '|' || checks_state FROM gh_prs WHERE number = ?",
            &[&(n as i64)]
        ),
        json!(format!("{new_sha}|none"))
    );
    let err = answer(&h, &merge, "yes", "").await.expect_err("409");
    assert_eq!(err.0, 409);
    assert!(err.1.contains("PR 有新的提交，等检查重新通过"), "{}", err.1);
    assert!(!h.fake.pull(REPO, n).expect("pr").merged);
    assert!(h.fake.calls_to("PUT", "/repos/").is_empty());

    // green again on the new head: the same open question; yes merges and deletes the branch
    h.fake.set_check(REPO, new_sha, "build", Some("success"));
    h.gh.poll_repo(&h.repo).await.expect("poll");
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type = 'gh_pr_merge'",
            &[]
        ),
        json!(1)
    );
    let r = answer(&h, &merge, "yes", "").await.expect("yes");
    assert_eq!(r["sha"], json!(new_sha));
    let pr = h.fake.pull(REPO, n).expect("pr");
    assert!(pr.merged);
    assert_eq!(pushed(&h), None, "branch deleted after the merge");
    assert_eq!(
        q(
            &h,
            "SELECT state FROM gh_prs WHERE number = ?",
            &[&(n as i64)]
        ),
        json!("merged")
    );
    let finishes: Vec<_> = h
        .core
        .calls()
        .into_iter()
        .filter(|c| matches!(c, CoreCall::Finish { .. }))
        .collect();
    assert_eq!(
        finishes,
        [CoreCall::Finish {
            item: id.clone(),
            now: format!("PR #{n} 已合并")
        }],
        "finished once"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_pr_question_for_a_release_project_or_without_new_commits() {
    let h = setup().await;
    // nothing ahead of main: an ordinary finish
    git(h.work.path(), &["branch", "steve/st-2", "main"]);
    let a = item(&h, 2, Some("steve/st-2"));
    assert!(!accepted(&h, &a).await);
    // a release action: the release path wraps up, not GitHub
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE projects SET release = './release.sh' WHERE id = ?",
                [PROJECT],
            )?)
        })
        .expect("release");
    let b = item(&h, 1, Some(BRANCH));
    assert!(!accepted(&h, &b).await);
    assert!(open_decision(&h, accept::PR_OPEN).is_none());
    assert!(h.core.calls().is_empty());
    assert_eq!(pushed(&h), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_check_supplements_the_item_through_the_template() {
    let h = setup().await;
    assert_eq!(
        q(
            &h,
            "SELECT name || '|' || origin || '|' || enabled FROM flows WHERE origin = 'template:github'",
            &[]
        ),
        json!("PR 检查失败|template:github|1")
    );
    let id = item(&h, 1, Some(BRANCH));
    let sha = git(h.work.path(), &["rev-parse", BRANCH]);
    let n = h
        .fake
        .open_pr(REPO, BRANCH, &sha, "fix: return_to", "", defaults::USER)
        .expect("pr");
    h.fake.set_check(REPO, &sha, "build", Some("failure"));
    h.gh.poll_repo(&h.repo).await.expect("poll");
    h.gh.settle().await;
    let want = format!(
        "PR #{n} 的检查失败：build。用 steve_github_ci_log 读日志后修复，修完推送到同一分支。"
    );
    let mut got = Vec::new();
    for _ in 0..100 {
        h.flow.run_followups().await;
        got = h.flow_core.calls();
        if !got.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        got,
        [FlowCall::Supplement {
            item: id.clone(),
            text: want
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_whose_origin_is_elsewhere_is_not_pushed() {
    let h = setup().await;
    let id = item(&h, 1, Some(BRANCH));
    assert!(accepted(&h, &id).await);
    git(
        h.work.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.com/steve-test/demo.git",
        ],
    );
    let (dec, _, _) = open_decision(&h, accept::PR_OPEN).expect("asked");
    answer(&h, &dec, "yes", "").await.expect("yes");
    assert_eq!(pushed(&h), None);
    assert_eq!(pull_posts(&h), 0);
    assert_eq!(
        h.core.calls().last(),
        Some(&CoreCall::Fail {
            item: id,
            why: format!("开 PR 失败：工作目录的 origin 不是 github.com/{REPO}")
        })
    );
}

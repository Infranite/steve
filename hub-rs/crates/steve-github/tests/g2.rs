//! G2 acceptance: a bad signature is a 401 and writes nothing; a redelivery is
//! processed once; a change seen by the webhook and the poll gives one event; `checks_state`
//! follows the check runs; a repo removed from the installation gets `enabled=0`. Plus the repo endpoints and
//! the PAT repository hook. Everything runs against the fake GitHub; the hooks are served on a real
//! local port so the fake can POST to them.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_github::fake::{FakeGitHub, WebhookSender, defaults, fixture};
use steve_github::{GitHub, GitHubEnv, RecordingSink, hooks, manifest, repos};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{Caller, Clock};
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
const REPO: &str = defaults::REPO;
const PR_SHA: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";

struct H {
    env: TestEnv,
    _keys: TempDir,
    gh: GitHub,
    api: Router,
    fake: FakeGitHub,
    sink: Arc<RecordingSink>,
    /// `http://127.0.0.1:<port>` serving `/hooks/github` and `/hooks/{connection}`.
    hooks: String,
}

async fn setup() -> H {
    let fake = FakeGitHub::start().await.expect("fake github");
    let env = TestEnv::new(connections::register);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    vault.init(WS2, ME).expect("vault init w2");
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
            ..Default::default()
        },
    );
    let sink = RecordingSink::new();
    gh.set_event_sink(sink.clone());
    gh.set_notifier(sink.clone());
    let api = manifest::routes(gh.clone())
        .merge(repos::routes(gh.clone()))
        .merge(connections::routes(conns.clone()))
        .with_state(env.modules.clone());
    // The root routes: the App hook and the PAT hook the signals route would dispatch here.
    let root: Router = hooks::routes(gh.clone()).merge(
        Router::new()
            .route(
                "/hooks/{connection_id}",
                axum::routing::post(hooks::connection_hook),
            )
            .layer(axum::Extension(gh.clone())),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, root).await;
    });
    let hooks = format!("http://{addr}");
    fake.set_webhook_target(&format!("{hooks}/hooks/github"));
    H {
        env,
        _keys: keys,
        gh,
        api,
        fake,
        sink,
        hooks,
    }
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

/// Row count of every table.
fn counts(h: &H) -> BTreeMap<String, i64> {
    h.env
        .store()
        .reader(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut m = BTreeMap::new();
            for n in names {
                let k: i64 = c.query_row(&format!("SELECT count(*) FROM \"{n}\""), [], |r| r.get(0))?;
                m.insert(n, k);
            }
            Ok::<_, steve_store::StoreError>(m)
        })
        .expect("counts")
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
    let (s, loc) = nav(
        &h.api,
        &me,
        &format!("/github/app/manifest/callback?code={code}&state={state}"),
    )
    .await;
    assert_eq!((s, loc.as_str()), (302, "/#/github?app=created"));
    let (s, loc) = nav(
        &h.api,
        &me,
        &format!(
            "/github/app/installed?installation_id={}&setup_action=install",
            defaults::INSTALLATION_ID
        ),
    )
    .await;
    assert_eq!((s, loc.as_str()), (302, "/#/github"));
    q(
        h,
        "SELECT id FROM connections WHERE adapter = 'github_app'",
        &[],
    )
    .as_str()
    .expect("connection")
    .to_string()
}

/// A PAT connection for `lee-test`: also makes `lee-test` “your” GitHub account.
async fn pat(h: &H) -> String {
    let me = caller(WS, ME);
    let (s, c) = call(
        &h.api,
        Some(&me),
        "POST",
        "/connections",
        Some(&json!({"adapter": "github_pat", "fields": {"令牌": defaults::PAT}, "config": {"api_base": h.fake.url()}})),
    )
    .await;
    assert_eq!(s, 200, "{c}");
    c["id"].as_str().expect("id").to_string()
}

async fn link(h: &H, conn: &str, gh_id: u64, project: Option<&str>) -> Value {
    let me = caller(WS, ME);
    let mut body = json!({"connection_id": conn, "gh_id": gh_id});
    if let Some(p) = project {
        body["project_id"] = json!(p);
    }
    let (s, v) = call(&h.api, Some(&me), "POST", "/github/repos", Some(&body)).await;
    assert_eq!(s, 200, "{v}");
    h.gh.settle().await;
    v
}

/// App installed, `steve-test/demo` linked, `lee-test` is mine.
async fn ready() -> (H, String, String) {
    let h = setup().await;
    let app = install(&h).await;
    pat(&h).await;
    let r = link(&h, &app, defaults::REPO_ID, None).await;
    let repo = r["id"].as_str().expect("repo").to_string();
    h.sink.clear();
    (h, app, repo)
}

async fn emit(h: &H, name: &str) -> u16 {
    let d = h.fake.emit_fixture(name).await.expect("emit");
    h.gh.settle().await;
    d.status
}

/// A webhook payload around a PR as the fake renders it.
async fn pr_payload(h: &H, app: &str, number: u64, action: &str) -> Value {
    let conn = h.gh.conn(WS, app).expect("conn");
    let pr = conn
        .get_if_changed(&format!("/repos/{REPO}/pulls/{number}"), None)
        .await
        .expect("pr")
        .body;
    json!({
        "action": action, "number": number, "pull_request": pr,
        "repository": {"id": defaults::REPO_ID, "full_name": REPO},
        "installation": {"id": defaults::INSTALLATION_ID},
        "sender": {"login": defaults::USER, "type": "User"},
    })
}

fn types(h: &H) -> Vec<String> {
    h.sink.types()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_signature_is_401_and_writes_nothing() {
    let (h, _app, _repo) = ready().await;
    // A good delivery first, so the secret is cached and the counts below start settled.
    assert_eq!(emit(&h, "ping").await, 202);
    let before = counts(&h);

    let (event, payload) = fixture("pull_request.opened").expect("fixture");
    let body = serde_json::to_vec(&payload).expect("json");
    let target = format!("{}/hooks/github", h.hooks);
    let forged = WebhookSender::new(&target, b"not the secret".to_vec());
    let d = forged.send(event, &payload).await.expect("send");
    assert_eq!((d.status, d.response.as_str()), (401, ""));

    let right = WebhookSender::new(&target, defaults::WEBHOOK_SECRET.as_bytes().to_vec());
    // tampered body, original signature
    let sig = steve_github::signature::sign(defaults::WEBHOOK_SECRET.as_bytes(), &body);
    let mut tampered = body.clone();
    tampered.extend_from_slice(b" ");
    let d = right
        .send_raw(event, tampered, "d-tampered".into(), Some(sig))
        .await
        .expect("send");
    assert_eq!((d.status, d.response.as_str()), (401, ""));
    // malformed / sha1 header
    for bad in ["", "sha1=abc", "sha256=zz"] {
        let d = right
            .send_raw(event, body.clone(), format!("d-{bad}"), Some(bad.into()))
            .await
            .expect("send");
        assert_eq!(d.status, 401, "{bad:?}");
    }
    // an App that is not ours
    let stranger = WebhookSender::with_target(
        &target,
        defaults::WEBHOOK_SECRET.as_bytes().to_vec(),
        4001,
        "integration",
        999,
    );
    let mut p2 = payload.clone();
    p2["installation"]["id"] = json!(1);
    assert_eq!(stranger.send(event, &p2).await.expect("send").status, 401);
    // no signature at all
    let res = reqwest::Client::new()
        .post(&target)
        .header("X-GitHub-Event", "pull_request")
        .header("X-GitHub-Delivery", "d-nosig")
        .body(body.clone())
        .send()
        .await
        .expect("post");
    assert_eq!(res.status().as_u16(), 401);
    // too big: 413 before anything else
    let router: Router = hooks::routes(h.gh.clone());
    let big = vec![b' '; hooks::BODY_LIMIT + 1];
    let sig = steve_github::signature::sign(defaults::WEBHOOK_SECRET.as_bytes(), &big);
    for declared in [true, false] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("X-GitHub-Event", "ping")
            .header("X-GitHub-Delivery", "d-big")
            .header("X-Hub-Signature-256", &sig);
        if declared {
            req = req.header(header::CONTENT_LENGTH, big.len());
        }
        let res = router
            .clone()
            .oneshot(req.body(Body::from(big.clone())).expect("req"))
            .await
            .expect("infallible");
        assert_eq!(res.status().as_u16(), 413, "declared: {declared}");
    }
    // a PAT hook route for a connection that is not a PAT
    let app_conn = q(
        &h,
        "SELECT id FROM connections WHERE adapter = 'github_app'",
        &[],
    );
    let pat_target = format!("{}/hooks/{}", h.hooks, app_conn.as_str().expect("id"));
    let d = WebhookSender::new(&pat_target, defaults::WEBHOOK_SECRET.as_bytes().to_vec())
        .send(event, &payload)
        .await
        .expect("send");
    assert_eq!(d.status, 401);

    h.gh.settle().await;
    assert_eq!(counts(&h), before, "no new rows anywhere");
    assert!(types(&h).is_empty());
}

/// Headers of an App delivery signed with `secret`.
fn app_headers(event: &str, delivery: &str, secret: &[u8], body: &[u8]) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    let sig = steve_github::signature::sign(secret, body);
    for (k, v) in [
        ("X-GitHub-Event", event.to_string()),
        ("X-GitHub-Delivery", delivery.to_string()),
        (
            "X-GitHub-Hook-Installation-Target-Type",
            "integration".to_string(),
        ),
        (
            "X-GitHub-Hook-Installation-Target-ID",
            defaults::APP_ID.to_string(),
        ),
        ("X-Hub-Signature-256", sig),
    ] {
        h.insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).expect("name"),
            v.parse().expect("value"),
        );
    }
    h
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_signature_on_a_cold_cache_writes_nothing() {
    let (h, _app, repo) = ready().await;
    // A new day for the vault's use log, and a Hub that has not opened the secret yet.
    h.env
        .store()
        .with_conn(|c| Ok(c.execute("DELETE FROM secret_uses", [])?))
        .expect("clear uses");
    let cold = GitHub::new(
        h.gh.connections().clone(),
        GitHubEnv {
            api_base: h.fake.url(),
            ..Default::default()
        },
    );
    let (event, payload) = fixture("ping").expect("fixture");
    let body = serde_json::to_vec(&payload).expect("json");
    let before = counts(&h);
    let res = cold
        .receive_app(&app_headers(event, "d-cold-1", b"forged", &body), &body)
        .await;
    assert_eq!(res.status().as_u16(), 401);
    cold.settle().await;
    assert_eq!(counts(&h), before, "no new rows anywhere, cold cache");

    // The real secret: accepted, and now the use is logged (once).
    let ok = app_headers(
        event,
        "d-cold-2",
        defaults::WEBHOOK_SECRET.as_bytes(),
        &body,
    );
    assert_eq!(cold.receive_app(&ok, &body).await.status().as_u16(), 202);
    cold.settle().await;
    let uses = q(
        &h,
        "SELECT count(*) FROM secret_uses WHERE actor_id LIKE 'ghapp_%'",
        &[],
    );
    assert_eq!(uses, json!(1));
    let ok = app_headers(
        event,
        "d-cold-3",
        defaults::WEBHOOK_SECRET.as_bytes(),
        &body,
    );
    assert_eq!(cold.receive_app(&ok, &body).await.status().as_u16(), 202);
    cold.settle().await;
    assert_eq!(q(&h, "SELECT count(*) FROM secret_uses", &[]), json!(1));
    let _ = repo;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redelivery_is_processed_once() {
    let (h, _app, repo) = ready().await;
    let d = h
        .fake
        .emit_fixture("pull_request.opened")
        .await
        .expect("emit");
    assert_eq!(d.status, 202);
    h.gh.settle().await;
    let again = h.fake.redeliver(&d.id).await.expect("redeliver");
    assert_eq!(again.status, 200);
    h.gh.settle().await;
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(1));
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM gh_deliveries WHERE delivery_id = ?",
            &[&d.id]
        ),
        json!(1)
    );
    assert_eq!(
        q(
            &h,
            "SELECT status || '|' || repo_id || '|' || workspace_id FROM gh_deliveries WHERE delivery_id = ?",
            &[&d.id]
        ),
        json!(format!("ok|{repo}|{WS}"))
    );
    // PR 12 is by the App's bot: Steve opened it.
    assert_eq!(
        q(
            &h,
            "SELECT opened_by || '|' || state || '|' || head_ref || '|' || checks_state FROM gh_prs",
            &[]
        ),
        json!("steve|open|steve/st-12|none")
    );
    // The same change under a new delivery id is applied again but changes nothing.
    let before = h.sink.notices().len();
    assert_eq!(emit(&h, "pull_request.opened").await, 202);
    assert_eq!(h.sink.notices().len(), before, "no change, no notice");
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(1));
    assert!(
        q(
            &h,
            "SELECT last_delivery FROM gh_repos WHERE id = ?",
            &[&repo]
        )
        .is_i64(),
        "last_delivery is set"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_and_poll_seeing_the_same_change_give_one_event() {
    let (h, app, repo) = ready().await;
    let n = h
        .fake
        .open_pr(
            REPO,
            "steve/st-5",
            "5555555555555555555555555555555555555555",
            "fix: five",
            "",
            "someone",
        )
        .expect("pr");
    let opened = pr_payload(&h, &app, n, "opened").await;
    assert_eq!(
        h.fake
            .emit("pull_request", &opened)
            .await
            .expect("emit")
            .status,
        202
    );
    h.gh.settle().await;

    // Merged on GitHub; the webhook arrives, then the poll sees the same.
    let conn = h.gh.conn(WS, &app).expect("conn");
    conn.put(
        &format!("/repos/{REPO}/pulls/{n}/merge"),
        &json!({"merge_method": "squash"}),
    )
    .await
    .expect("merge");
    let closed = pr_payload(&h, &app, n, "closed").await;
    assert_eq!(
        h.fake
            .emit("pull_request", &closed)
            .await
            .expect("emit")
            .status,
        202
    );
    h.gh.settle().await;
    let rep = h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert!(rep.applied >= 1, "{rep:?}");
    assert_eq!(types(&h), ["github.pr_merged"]);
    assert!(
        h.sink.duplicates().is_empty(),
        "not even a duplicate reached S1"
    );
    let (_, conn_id, ev) = &h.sink.events()[0];
    assert_eq!(conn_id, &app);
    assert_eq!(
        ev.thread_key.as_deref().map(|t| t.starts_with("pr:ghp_")),
        Some(true)
    );
    assert_eq!(ev.scope_ext_id.as_deref(), Some(REPO));
    assert_eq!(ev.summary, format!("{REPO} · PR #{n} 已合并：fix: five"));
    // A second poll: GitHub answers 304, nothing new.
    let rep = h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert!(rep.pulls_unchanged, "{rep:?}");

    // The other way round: the poll sees failing checks first, then the webhook arrives.
    let m = h
        .fake
        .open_pr(
            REPO,
            "steve/st-6",
            "6666666666666666666666666666666666666666",
            "fix: six",
            "",
            "someone",
        )
        .expect("pr");
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    h.sink.clear();
    let job = h.fake.set_check(
        REPO,
        "6666666666666666666666666666666666666666",
        "build",
        Some("failure"),
    );
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert_eq!(types(&h), ["github.check_failed"]);
    let ev = h.sink.events()[0].2.clone();
    assert_eq!(ev.summary, format!("{REPO} · PR #{m} 检查失败：build"));
    assert_eq!(ev.payload["failed"][0]["job_id"], json!(job));
    assert_eq!(
        ev.payload["head_sha"],
        json!("6666666666666666666666666666666666666666")
    );
    let run = conn
        .get_if_changed(
            &format!("/repos/{REPO}/commits/6666666666666666666666666666666666666666/check-runs"),
            None,
        )
        .await
        .expect("runs")
        .body["check_runs"][0]
        .clone();
    let payload = json!({"action": "completed", "check_run": run,
        "repository": {"id": defaults::REPO_ID}, "installation": {"id": defaults::INSTALLATION_ID}});
    assert_eq!(
        h.fake
            .emit("check_run", &payload)
            .await
            .expect("emit")
            .status,
        202
    );
    h.gh.settle().await;
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert_eq!(types(&h), ["github.check_failed"], "still one");
    assert!(h.sink.duplicates().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_state_and_signals_follow_the_spec() {
    let (h, _app, repo) = ready().await;
    assert_eq!(emit(&h, "pull_request.opened").await, 202);
    let state = |h: &H| q(h, "SELECT checks_state FROM gh_prs WHERE number = 12", &[]);
    assert_eq!(state(&h), json!("none"));

    // A check run in progress: pending.
    h.fake.set_check(REPO, PR_SHA, "build", None);
    h.fake.set_check(REPO, PR_SHA, "e2e", None);
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert_eq!(state(&h), json!("pending"));
    // build fails while e2e still runs: failure, but no signal until everything is done.
    h.fake.set_check(REPO, PR_SHA, "build", Some("failure"));
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert_eq!(state(&h), json!("failure"));
    assert!(types(&h).is_empty(), "{:?}", types(&h));
    // The suite completes (webhook): the runs are fetched, one event with both failures.
    h.fake.set_check(REPO, PR_SHA, "e2e", Some("timed_out"));
    assert_eq!(emit(&h, "check_suite.completed.failure").await, 202);
    assert_eq!(types(&h), ["github.check_failed"]);
    let ev = h.sink.events()[0].2.clone();
    assert_eq!(ev.summary, format!("{REPO} · PR #12 检查失败：build、e2e"));
    assert_eq!(
        ev.group_key.as_deref(),
        Some(format!("repo:{repo}").as_str())
    );
    assert_eq!(ev.payload["number"], json!(12));
    assert_eq!(ev.payload["failed"].as_array().map(Vec::len), Some(2));
    // job ids of Actions checks are known
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM gh_checks WHERE head_sha = ? AND job_id IS NOT NULL AND run_id IS NOT NULL",
            &[&PR_SHA]
        ),
        json!(2)
    );

    // Reruns pass: success, `check_passed` once.
    h.sink.clear();
    h.fake.set_check(REPO, PR_SHA, "build", Some("success"));
    h.fake.set_check(REPO, PR_SHA, "e2e", Some("skipped"));
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert_eq!(state(&h), json!("success"));
    assert_eq!(emit(&h, "check_suite.completed.success").await, 202);
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    assert_eq!(types(&h), ["github.check_passed"]);
    assert_eq!(h.sink.events()[0].2.summary, "PR #12 检查通过");

    // A legacy status that fails: failure again.
    h.sink.clear();
    let status = json!({"sha": PR_SHA, "context": "ci/legacy", "state": "error", "target_url": "https://ci.example/1",
        "description": "boom", "branches": [], "repository": {"id": defaults::REPO_ID},
        "installation": {"id": defaults::INSTALLATION_ID}, "updated_at": "2026-11-01T00:00:00Z"});
    assert_eq!(
        h.fake.emit("status", &status).await.expect("emit").status,
        202
    );
    h.gh.settle().await;
    assert_eq!(state(&h), json!("failure"));
    assert_eq!(
        q(
            &h,
            "SELECT kind || '|' || status || '|' || conclusion FROM gh_checks WHERE ext_id = 'ci/legacy'",
            &[]
        ),
        json!("status|completed|failure")
    );
    assert_eq!(types(&h), ["github.check_failed"]);

    // A new head: `checks_state` is that commit's.
    assert_eq!(emit(&h, "pull_request.synchronize").await, 202);
    assert_eq!(
        q(
            &h,
            "SELECT head_sha || '|' || checks_state FROM gh_prs WHERE number = 12",
            &[]
        ),
        json!("b2c3d4e5f60718293a4b5c6d7e8f901234567890|none")
    );

    // Failing checks on the default branch without a PR: a branch signal.
    h.sink.clear();
    let main = defaults::BASE_SHA;
    h.fake.set_check(REPO, main, "build", Some("failure"));
    let (_, mut suite) = fixture("check_suite.completed.failure").expect("fixture");
    suite["check_suite"]["head_sha"] = json!(main);
    suite["check_suite"]["head_branch"] = json!("main");
    suite["check_suite"]["pull_requests"] = json!([]);
    assert_eq!(
        h.fake
            .emit("check_suite", &suite)
            .await
            .expect("emit")
            .status,
        202
    );
    h.gh.settle().await;
    assert_eq!(types(&h), ["github.check_failed"]);
    let ev = h.sink.events()[0].2.clone();
    assert_eq!(ev.summary, format!("{REPO} · main 检查失败：build"));
    assert_eq!(ev.thread_key, Some(format!("branch:{repo}:main")));
}

#[tokio::test(flavor = "multi_thread")]
async fn reviews_comments_issues_and_merges() {
    let (h, _app, repo) = ready().await;
    assert_eq!(emit(&h, "pull_request.opened").await, 202);
    // lee-test (my PAT account) is asked to review.
    assert_eq!(emit(&h, "pull_request.review_requested").await, 202);
    assert_eq!(types(&h), ["github.review_requested"]);
    assert_eq!(
        h.sink.events()[0].2.summary,
        "steve-test[bot] 请你评审 PR #12：fix: preserve return_to"
    );
    assert_eq!(
        q(&h, "SELECT review_state FROM gh_prs WHERE number = 12", &[]),
        json!("requested")
    );
    // Approved on a PR Steve opened.
    assert_eq!(emit(&h, "pull_request_review.submitted").await, 202);
    assert_eq!(
        q(&h, "SELECT review_state FROM gh_prs WHERE number = 12", &[]),
        json!("approved")
    );
    assert_eq!(types(&h)[1], "github.review");
    assert_eq!(h.sink.events()[1].2.summary, "lee-test 批准了 PR #12");
    // A comment on a PR not linked to an item: no signal.
    assert_eq!(emit(&h, "issue_comment.created").await, 202);
    assert_eq!(types(&h).len(), 2);
    // Once linked (G3 writes item_id), comments become signals.
    h.env
        .store()
        .transaction(|tx| {
            tx.execute("UPDATE gh_prs SET item_id = 'ST-12' WHERE number = 12", [])?;
            Ok::<_, steve_store::StoreError>(())
        })
        .expect("link");
    assert_eq!(emit(&h, "issue_comment.created").await, 202);
    assert_eq!(types(&h)[2], "github.pr_comment");
    assert!(
        h.sink.events()[2]
            .2
            .summary
            .starts_with("lee-test 评论了 PR #12：")
    );

    // Issues: the fixture's issue is older than the link — stored, no signal; a new one signals.
    assert_eq!(emit(&h, "issues.opened").await, 202);
    assert_eq!(q(&h, "SELECT count(*) FROM gh_issues", &[]), json!(1));
    assert_eq!(types(&h).len(), 3);
    let (_, mut issue) = fixture("issues.opened").expect("fixture");
    let now = chrono::Utc::now() + chrono::Duration::minutes(1);
    issue["issue"]["number"] = json!(40);
    issue["issue"]["id"] = json!(9040);
    issue["issue"]["created_at"] = json!(now.to_rfc3339());
    issue["issue"]["updated_at"] = json!(now.to_rfc3339());
    issue["issue"]["title"] = json!("登录后跳回首页");
    assert_eq!(
        h.fake.emit("issues", &issue).await.expect("emit").status,
        202
    );
    h.gh.settle().await;
    assert_eq!(types(&h)[3], "github.issue_opened");
    assert_eq!(
        h.sink.events()[3].2.summary,
        format!("{REPO} · #40 登录后跳回首页")
    );

    // Merged.
    assert_eq!(emit(&h, "pull_request.closed.merged").await, 202);
    assert_eq!(
        q(&h, "SELECT state FROM gh_prs WHERE number = 12", &[]),
        json!("merged")
    );
    assert_eq!(types(&h)[4], "github.pr_merged");
    assert_eq!(h.sink.events()[4].2.payload["item_id"], json!("ST-12"));
    assert!(
        h.env
            .take_events()
            .iter()
            .any(|e| matches!(e, steve_store::ChangeEvent::Item { id } if id == "ST-12")),
        "the linked item's detail refreshes"
    );

    // REST views.
    let me = caller(WS, ME);
    let (s, prs) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/prs?repo={repo}&state=all"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{prs}");
    assert_eq!(prs[0]["number"], json!(12));
    assert_eq!(prs[0]["state"], json!("merged"));
    assert_eq!(prs[0]["opened_by"], json!("steve"));
    assert_eq!(prs[0]["item"], Value::Null, "ST-12 is not a real item here");
    let (_, open) = call(&h.api, Some(&me), "GET", "/github/prs", None).await;
    assert_eq!(open, json!([]));
    let (s, issues) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/issues?repo={repo}&state=open"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(issues.as_array().map(Vec::len), Some(2));
    let (s, ds) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/deliveries?repo={repo}&limit=3"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(ds.as_array().map(Vec::len), Some(3));
    assert_eq!(ds[0]["event"], json!("pull_request"));
    assert_eq!(ds[0]["status"], json!("ok"));
    let (s, _) = call(
        &h.api,
        Some(&me),
        "GET",
        "/github/deliveries?limit=101",
        None,
    )
    .await;
    assert_eq!(s, 400);
    // another workspace sees nothing of it
    let other = caller(WS2, ME);
    let (s, e) = call(
        &h.api,
        Some(&other),
        "GET",
        &format!("/github/prs?repo={repo}"),
        None,
    )
    .await;
    assert_eq!((s, &e["error"]), (404, &json!("没有这个仓库")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repo_removed_from_the_installation_is_disabled() {
    let (h, app, repo) = ready().await;
    h.fake
        .remove_from_installation(defaults::INSTALLATION_ID, REPO);
    assert_eq!(emit(&h, "installation_repositories.removed").await, 202);
    assert_eq!(
        q(&h, "SELECT enabled FROM gh_repos WHERE id = ?", &[&repo]),
        json!(0)
    );
    let me = caller(WS, ME);
    let (_, all) = call(&h.api, Some(&me), "GET", "/github", None).await;
    let r = &all["repos"][0];
    assert_eq!(r["enabled"], json!(false));
    assert_eq!(r["healthy"], json!(false));
    assert_eq!(r["problem"], json!("GitHub App 不再能访问这个仓库"));
    assert!(
        h.sink
            .notices()
            .iter()
            .any(|(w, id)| w == WS && id == &repo)
    );
    // Later deliveries for it are recorded as ignored.
    let d = h
        .fake
        .emit_fixture("pull_request.opened")
        .await
        .expect("emit");
    h.gh.settle().await;
    assert_eq!(
        q(
            &h,
            "SELECT status || '|' || error FROM gh_deliveries WHERE delivery_id = ?",
            &[&d.id]
        ),
        json!("ignored|这个仓库已停用")
    );
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(0));
    // The list of linkable repos no longer has it.
    let (_, avail) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/available?connection={app}"),
        None,
    )
    .await;
    assert_eq!(avail, json!([]));
    // An unknown installation is ignored.
    let (_, mut p) = fixture("pull_request.opened").expect("fixture");
    p["installation"]["id"] = json!(777);
    let d = h.fake.emit("pull_request", &p).await.expect("emit");
    h.gh.settle().await;
    assert_eq!(
        q(
            &h,
            "SELECT status FROM gh_deliveries WHERE delivery_id = ?",
            &[&d.id]
        ),
        json!("ignored")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn repo_endpoints() {
    let h = setup().await;
    let app = install(&h).await;
    let me = caller(WS, ME);
    // a git project whose origin is the repo: suggested
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().to_string_lossy().to_string();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(args)
            .output()
            .expect("git");
        assert!(ok.status.success());
    };
    git(&["init", "-q"]);
    git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:steve-test/demo.git",
    ]);
    h.env
        .store()
        .transaction(|tx| {
            tx.execute(
                "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES ('prj_demo', 'default', 'Demo', ?, 'git', 1, 1), ('prj_two', 'default', 'Two', '/nonexistent', 'git', 1, 1)",
                [&path],
            )?;
            Ok::<_, steve_store::StoreError>(())
        })
        .expect("projects");
    let (s, all) = call(&h.api, Some(&me), "GET", "/github", None).await;
    assert_eq!(s, 200, "{all}");
    assert_eq!(all["apps"][0]["slug"], json!(defaults::APP_SLUG));
    assert_eq!(all["connections"][0]["kind"], json!("app"));
    assert_eq!(all["connections"][0]["account"], json!("steve-test[bot]"));
    assert_eq!(
        all["suggestions"],
        json!([{"project_id": "prj_demo", "full_name": REPO, "gh_id": defaults::REPO_ID, "connection_id": app}])
    );
    let (s, avail) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/available?connection={app}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(
        avail,
        json!([{"gh_id": defaults::REPO_ID, "full_name": REPO, "private": true, "linked": false}])
    );
    let (s, _) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/github/available?connection={app}"),
        None,
    )
    .await;
    assert_eq!(s, 404);

    // link with the project
    let r = link(&h, &app, defaults::REPO_ID, Some("prj_demo")).await;
    let id = r["id"].as_str().expect("id").to_string();
    assert_eq!(r["project"], json!({"id": "prj_demo", "name": "Demo"}));
    assert_eq!(r["ingest"], json!("app"));
    assert_eq!(r["connection"]["kind"], json!("app"));
    assert_eq!(r["settings"]["merge_method"], json!("squash"));
    // twice → 409; a second repo on the same project → 409; unknown repo → 404
    let (s, e) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/repos",
        Some(&json!({"connection_id": app, "gh_id": defaults::REPO_ID})),
    )
    .await;
    assert_eq!((s, &e["error"]), (409, &json!("这个仓库已经关联过了")));
    let other = h.fake.add_repo("steve-test/other", false, "main");
    h.fake.add_installation(
        defaults::INSTALLATION_ID,
        defaults::ORG,
        "Organization",
        &[REPO, "steve-test/other"],
    );
    let (s, e) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/repos",
        Some(&json!({"connection_id": app, "gh_id": other, "project_id": "prj_demo"})),
    )
    .await;
    assert_eq!(
        (s, &e["error"]),
        (409, &json!(format!("这个项目已经关联了 {REPO}")))
    );
    let (s, e) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/repos",
        Some(&json!({"connection_id": app, "gh_id": 424242})),
    )
    .await;
    assert_eq!((s, &e["error"]), (404, &json!("没有这个仓库")));
    let (_, all) = call(&h.api, Some(&me), "GET", "/github", None).await;
    assert_eq!(all["suggestions"], json!([]));

    // settings
    let (s, e) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/github/repos/{id}"),
        Some(&json!({"settings": {"finish_on": "later"}})),
    )
    .await;
    assert_eq!(
        (s, &e["error"]),
        (400, &json!("参数不对：settings.finish_on"))
    );
    let (s, v) = call(&h.api, Some(&me), "PATCH", &format!("/github/repos/{id}"), Some(&json!({"settings": {"finish_on": "merged", "merge_method": "rebase"}, "project_id": null}))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["settings"]["finish_on"], json!("merged"));
    assert_eq!(v["settings"]["merge_method"], json!("rebase"));
    assert_eq!(v["project"], Value::Null);
    assert_eq!(v["revision"], json!(2));
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM revisions WHERE object_type = 'gh_repo' AND object_id = ?",
            &[&id]
        ),
        json!(1)
    );
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/github/repos/{id}"),
        Some(&json!({"settings": {"finish_on": "merged"}})),
    )
    .await;
    assert_eq!(
        (s, &v["revision"]),
        (200, &json!(2)),
        "no change, no revision"
    );
    let (s, _) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/github/repos/{id}"),
        Some(&json!({"project_id": "prj_nope"})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "PATCH",
        &format!("/github/repos/{id}"),
        Some(&json!({"enabled": false})),
    )
    .await;
    assert_eq!(s, 404);

    // sync now
    h.fake.open_pr(
        REPO,
        "feature/x",
        "7777777777777777777777777777777777777777",
        "feat: x",
        "",
        "someone",
    );
    let (s, _) = call(
        &h.api,
        Some(&me),
        "POST",
        &format!("/github/repos/{id}/sync"),
        None,
    )
    .await;
    assert_eq!(s, 202);
    h.gh.settle().await;
    assert_eq!(
        q(&h, "SELECT title || '|' || opened_by FROM gh_prs", &[]),
        json!("feat: x|other")
    );

    // unlink: rows go
    let (s, v) = call(
        &h.api,
        Some(&me),
        "DELETE",
        &format!("/github/repos/{id}"),
        None,
    )
    .await;
    assert_eq!((s, v), (200, json!({"ok": true})));
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(0));
    assert_eq!(q(&h, "SELECT count(*) FROM gh_repos", &[]), json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn pat_repos_get_a_hook_or_are_polled() {
    let h = setup().await;
    let con = pat(&h).await;
    let r = link(&h, &con, defaults::REPO_ID, None).await;
    let id = r["id"].as_str().expect("id").to_string();
    assert_eq!(r["ingest"], json!("repo_hook"));
    assert_eq!(r["connection"]["kind"], json!("pat"));
    let hooks = h.fake.hooks(REPO);
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].url, format!("{ORIGIN}/hooks/{con}"));
    assert_eq!(hooks[0].content_type, "json");
    assert!(hooks[0].events.iter().any(|e| e == "check_suite"));
    assert_eq!(hooks[0].secret.len(), 64);
    // The secret is in the vault, not in the row.
    let state = q(&h, "SELECT poll_state FROM gh_repos WHERE id = ?", &[&id]);
    assert!(!state.as_str().expect("state").contains(&hooks[0].secret));

    // A delivery on the connection's route, signed with the hook's secret.
    let target = format!("{}/hooks/{con}", h.hooks);
    let sender = WebhookSender::with_target(
        &target,
        hooks[0].secret.as_bytes().to_vec(),
        hooks[0].id,
        "repository",
        defaults::REPO_ID,
    );
    let (event, mut payload) = fixture("pull_request.opened").expect("fixture");
    payload.as_object_mut().expect("obj").remove("installation");
    let d = sender.send(event, &payload).await.expect("send");
    assert_eq!(d.status, 202);
    h.gh.settle().await;
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(1));
    // The App's secret does not open the PAT route.
    let wrong = WebhookSender::with_target(
        &target,
        defaults::WEBHOOK_SECRET.as_bytes().to_vec(),
        1,
        "repository",
        defaults::REPO_ID,
    );
    assert_eq!(wrong.send(event, &payload).await.expect("send").status, 401);

    // Unlink removes the hook.
    let me = caller(WS, ME);
    let (s, _) = call(
        &h.api,
        Some(&me),
        "DELETE",
        &format!("/github/repos/{id}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert!(h.fake.hooks(REPO).is_empty());

    // Without permission to create hooks: polling.
    h.fake.set_hooks_allowed(REPO, false);
    let r = link(&h, &con, defaults::REPO_ID, None).await;
    assert_eq!(r["ingest"], json!("poll"));
    let id = r["id"].as_str().expect("id").to_string();
    let due = h
        .env
        .store()
        .reader(|c| {
            let row = steve_github::repo::repo_get(c, &id)?.expect("row");
            Ok::<_, steve_store::StoreError>(h.gh.poll_is_due(
                c,
                &row,
                h.env.clock.now() + steve_github::poll::POLL_EVERY_MS,
            ))
        })
        .expect("due");
    assert!(due, "poll repos are polled every 2 minutes");
}

#[tokio::test(flavor = "multi_thread")]
async fn app_repos_are_polled_when_webhooks_go_quiet() {
    let (h, _app, repo) = ready().await;
    let is_due = |h: &H, at: i64| {
        h.env
            .store()
            .reader(|c| {
                let row = steve_github::repo::repo_get(c, &repo)?.expect("row");
                Ok::<_, steve_store::StoreError>(h.gh.poll_is_due(c, &row, at))
            })
            .expect("due")
    };
    let now = h.env.clock.now() + steve_github::poll::POLL_EVERY_MS;
    assert!(!is_due(&h, now), "no linked PR: webhooks only");
    assert_eq!(emit(&h, "pull_request.opened").await, 202);
    h.env
        .store()
        .transaction(|tx| {
            tx.execute("UPDATE gh_prs SET item_id = 'ST-12'", [])?;
            Ok::<_, steve_store::StoreError>(())
        })
        .expect("link");
    assert!(!is_due(&h, now), "a fresh delivery");
    assert!(
        is_due(&h, now + steve_github::poll::STALE_MS + 1),
        "6 hours without deliveries"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deliveries_are_rate_limited_per_connection() {
    let (h, _app, _repo) = ready().await;
    let mut last = 0;
    for _ in 0..hooks::RATE_PER_MINUTE {
        last = h
            .fake
            .emit_fixture("issue_comment.created")
            .await
            .expect("emit")
            .status;
        assert_eq!(last, 202);
    }
    let d = h
        .fake
        .emit_fixture("issue_comment.created")
        .await
        .expect("emit");
    assert_eq!((last, d.status), (202, 429));
    h.gh.settle().await;
}

/// The secret of a cold cache is read from the vault without logging; only a delivery that
/// verifies writes the vault's use row (and fills the cache), so a forged one writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_cold_secret_cache_logs_the_vault_use_only_after_the_signature_verifies() {
    let (h, _app, _repo) = ready().await;
    h.gh.settle().await;
    // a second GitHub on the same connections: nothing cached
    let cold = GitHub::new(h.gh.connections().clone(), h.gh.env().clone());
    cold.set_event_sink(h.sink.clone());
    cold.set_notifier(h.sink.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let root: Router = hooks::routes(cold.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, root).await;
    });
    let target = format!("http://{addr}/hooks/github");
    let uses = |h: &H| counts(h).get("secret_uses").copied().unwrap_or(0);
    // nothing logged today yet for the App's webhook secret
    h.env
        .store()
        .with_conn(|c| Ok(c.execute("DELETE FROM secret_uses", [])?))
        .expect("clear");

    let (event, payload) = fixture("pull_request.opened").expect("fixture");
    let forged = WebhookSender::new(&target, b"not the secret".to_vec());
    for _ in 0..3 {
        let d = forged.send(event, &payload).await.expect("send");
        assert_eq!(d.status, 401);
    }
    assert_eq!(uses(&h), 0, "a forged delivery logs no vault use");

    let right = WebhookSender::new(&target, defaults::WEBHOOK_SECRET.as_bytes().to_vec());
    assert_eq!(
        right
            .send(
                "ping",
                &json!({"zen": "x", "hook_id": 1, "installation": payload["installation"].clone()})
            )
            .await
            .expect("send")
            .status,
        202
    );
    assert_eq!(uses(&h), 1, "the first verified delivery logs the use");
    assert_eq!(
        right
            .send(
                "ping",
                &json!({"zen": "y", "hook_id": 1, "installation": payload["installation"].clone()})
            )
            .await
            .expect("send")
            .status,
        202
    );
    assert_eq!(uses(&h), 1, "cached from then on");
}

/// Y1: a delivery accepted (202, the row `received` with its source and payload committed) but not
/// processed before a restart is processed by the recovery, once; the payload is dropped after.
#[tokio::test(flavor = "multi_thread")]
async fn an_accepted_delivery_a_restart_interrupted_is_processed_by_the_recovery() {
    let (h, app, _repo) = ready().await;
    // the PR exists on GitHub; its delivery was accepted, not processed
    let n = h
        .fake
        .open_pr(
            REPO,
            "steve/st-6",
            "6666666666666666666666666666666666666666",
            "fix: six",
            "",
            "someone",
        )
        .expect("pr");
    let payload = pr_payload(&h, &app, n, "opened").await;
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(0));
    // what `accept` committed before the worker got to it
    let row = steve_github::repo::DeliveryRow {
        delivery_id: "d-interrupted".into(),
        workspace_id: Some(WS.into()),
        repo_id: None,
        event: "pull_request".into(),
        action: Some("opened".into()),
        status: steve_github::repo::RECEIVED.into(),
        error: None,
        received: steve_store::now(),
    };
    let source = serde_json::to_string(&steve_github::sync::Source::App { ws: WS.into() }).unwrap();
    h.env
        .store()
        .transaction(|tx| {
            steve_github::repo::insert_delivery(tx, &row, &source, &payload.to_string())
                .map_err(steve_modules::ApiError::from)
        })
        .expect("insert");
    // GitHub redelivers it meanwhile: known, 200, not processed on the request
    assert_eq!(h.gh.recover_deliveries(), 1);
    assert_eq!(h.gh.recover_deliveries(), 0, "queued once");
    h.gh.settle().await;
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(1));
    assert_eq!(
        q(
            &h,
            "SELECT status || '|' || (SELECT count(*) FROM gh_delivery_inputs WHERE delivery_id = 'd-interrupted') FROM gh_deliveries WHERE delivery_id = 'd-interrupted'",
            &[]
        ),
        json!("ok|0")
    );
    assert_eq!(h.gh.recover_deliveries(), 0, "processed: nothing left");
}

/// A delivery still `received` is not pruned with the old ones.
#[tokio::test(flavor = "multi_thread")]
async fn an_unprocessed_delivery_outlives_the_retention() {
    let (h, _app, _repo) = ready().await;
    let row = steve_github::repo::DeliveryRow {
        delivery_id: "d-old".into(),
        workspace_id: Some(WS.into()),
        repo_id: None,
        event: "ping".into(),
        action: None,
        status: steve_github::repo::RECEIVED.into(),
        error: None,
        received: 1,
    };
    h.env
        .store()
        .transaction(|tx| {
            steve_github::repo::insert_delivery(tx, &row, "{\"app\":{\"ws\":\"default\"}}", "{}")
                .map_err(steve_modules::ApiError::from)
        })
        .expect("insert");
    h.gh.prune_deliveries();
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM gh_deliveries WHERE delivery_id = 'd-old'",
            &[]
        ),
        json!(1)
    );
}

/// Y1: a signal follows a committed state change through the outbox: processed but not yet
/// delivered when the Hub stops, a later dispatcher (the restarted Hub) delivers it. Before, it went
/// out in memory after the commit, and processing the change again finds nothing left to signal.
#[tokio::test(flavor = "multi_thread")]
async fn a_signal_is_delivered_after_a_restart_between_its_commit_and_its_delivery() {
    let (h, _app, repo) = ready().await;
    let sha = "7777777777777777777777777777777777777777";
    h.fake
        .open_pr(REPO, "steve/st-7", sha, "fix: seven", "", "someone")
        .expect("pr");
    h.gh.poll_repo(&repo).await.expect("poll");
    h.gh.settle().await;
    h.sink.clear();
    h.fake.set_check(REPO, sha, "build", Some("failure"));
    h.gh.poll_repo(&repo).await.expect("poll"); // committed; the Hub "stops" before delivering
    assert!(types(&h).is_empty(), "nothing delivered yet");
    h.gh.poll_repo(&repo).await.expect("poll"); // seen again: no change, no second signal
    let restarted = Arc::new(steve_store::outbox::Dispatcher::new(vec![
        h.gh.outbox_consumer(),
    ]));
    let store = h.env.store().clone();
    let d = restarted.clone();
    tokio::task::spawn_blocking(move || {
        d.register(&store).unwrap();
        d.pump(&store, steve_store::now()).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(types(&h), ["github.check_failed"]);
}

/// A delivery processed late (the recovery, a retry) carries an older view than the one stored
/// meanwhile: it changes nothing. GitHub's `updated_at` orders them.
#[tokio::test(flavor = "multi_thread")]
async fn an_older_view_processed_late_does_not_undo_a_newer_one() {
    let (h, app, _repo) = ready().await;
    let n = h
        .fake
        .open_pr(
            REPO,
            "steve/st-8",
            "8888888888888888888888888888888888888888",
            "fix: eight",
            "",
            "someone",
        )
        .expect("pr");
    let mut opened = pr_payload(&h, &app, n, "opened").await;
    opened["pull_request"]["updated_at"] = json!("2026-01-01T00:00:00Z");
    let mut closed = opened.clone();
    closed["action"] = json!("closed");
    closed["pull_request"]["state"] = json!("closed");
    closed["pull_request"]["updated_at"] = json!("2026-01-02T00:00:00Z");
    // the opened delivery was accepted first but not processed before a restart ...
    let row = steve_github::repo::DeliveryRow {
        delivery_id: "d-late".into(),
        workspace_id: Some(WS.into()),
        repo_id: None,
        event: "pull_request".into(),
        action: Some("opened".into()),
        status: steve_github::repo::RECEIVED.into(),
        error: None,
        received: 1,
    };
    let source = serde_json::to_string(&steve_github::sync::Source::App { ws: WS.into() }).unwrap();
    h.env
        .store()
        .transaction(|tx| {
            steve_github::repo::insert_delivery(tx, &row, &source, &opened.to_string())
                .map_err(steve_modules::ApiError::from)
        })
        .expect("insert");
    // ... the closed one came after and was processed at once
    assert_eq!(
        h.fake
            .emit("pull_request", &closed)
            .await
            .expect("emit")
            .status,
        202
    );
    h.gh.settle().await;
    assert_eq!(h.gh.recover_deliveries(), 1);
    h.gh.settle().await;
    assert_eq!(
        q(
            &h,
            "SELECT state FROM gh_prs WHERE number = ?",
            &[&(n as i64)]
        ),
        json!("closed")
    );
    assert_eq!(
        q(
            &h,
            "SELECT status FROM gh_deliveries WHERE delivery_id = 'd-late'",
            &[]
        ),
        json!("ok")
    );
}

/// Y1: a processing that fails (here the store refuses) keeps the delivery `received` with its
/// input; the recovery tries it again after the back-off and it goes through.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_processing_is_tried_again() {
    let (h, _app, _repo) = ready().await;
    h.env
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER no_prs BEFORE INSERT ON gh_prs BEGIN SELECT RAISE(ABORT, 'test store fault'); END",
            )?;
            Ok(())
        })
        .unwrap();
    let d = h
        .fake
        .emit_fixture("pull_request.opened")
        .await
        .expect("emit");
    assert_eq!(d.status, 202);
    h.gh.settle().await;
    assert_eq!(
        q(
            &h,
            "SELECT d.status || '|' || i.attempts FROM gh_deliveries d JOIN gh_delivery_inputs i USING (delivery_id) WHERE delivery_id = ?",
            &[&d.id]
        ),
        json!("received|1")
    );
    assert_eq!(h.gh.recover_deliveries(), 0, "backing off");
    h.env
        .store()
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER no_prs")?;
            Ok(())
        })
        .unwrap();
    h.env.clock.advance(steve_store::outbox::backoff(1) + 1);
    assert_eq!(h.gh.recover_deliveries(), 1);
    h.gh.settle().await;
    assert_eq!(q(&h, "SELECT count(*) FROM gh_prs", &[]), json!(1));
    assert_eq!(
        q(
            &h,
            "SELECT status FROM gh_deliveries WHERE delivery_id = ?",
            &[&d.id]
        ),
        json!("ok")
    );
}

/// Panics on every notice: a processing that crashes after its commit.
struct PanickingNotifier;

impl steve_github::GhNotifier for PanickingNotifier {
    fn gh(&self, _: &str, _: &str) {
        panic!("test notifier panic");
    }
}

/// A processing that panics counts as a failed attempt: still `received`, attempts up by one and
/// backing off, so the recovery doesn't queue it again on every tick (it would panic each time and
/// never reach `failed`).
#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_processing_counts_as_a_failed_attempt() {
    let (h, _app, _repo) = ready().await;
    h.gh.set_notifier(Arc::new(PanickingNotifier));
    let d = h
        .fake
        .emit_fixture("pull_request.opened")
        .await
        .expect("emit");
    assert_eq!(d.status, 202);
    h.gh.settle().await;
    assert_eq!(
        q(
            &h,
            "SELECT d.status || '|' || i.attempts || '|' || d.error FROM gh_deliveries d JOIN gh_delivery_inputs i USING (delivery_id) WHERE delivery_id = ?",
            &[&d.id]
        ),
        json!("received|1|处理时崩溃")
    );
    assert_eq!(
        h.gh.recover_deliveries(),
        0,
        "backing off, not queued again at once"
    );
}

/// Concurrent or back-to-back `recover_deliveries` calls while the worker finishes accepted
/// deliveries never count or enqueue an already-claimed or already-finished delivery twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recover_deliveries_racing_with_worker_completion_queues_each_once() {
    let (h, app, _repo) = ready().await;
    let total = 40usize;
    let source = serde_json::to_string(&steve_github::sync::Source::App { ws: WS.into() }).unwrap();
    for idx in 0..total {
        let n = h
            .fake
            .open_pr(
                REPO,
                &format!("steve/race-{idx}"),
                &format!("{idx:040x}"),
                &format!("fix: race {idx}"),
                "",
                "someone",
            )
            .expect("pr");
        let payload = pr_payload(&h, &app, n, "opened").await;
        let row = steve_github::repo::DeliveryRow {
            delivery_id: format!("d-race-{idx}"),
            workspace_id: Some(WS.into()),
            repo_id: None,
            event: "pull_request".into(),
            action: Some("opened".into()),
            status: steve_github::repo::RECEIVED.into(),
            error: None,
            received: steve_store::now(),
        };
        h.env
            .store()
            .transaction(|tx| {
                steve_github::repo::insert_delivery(tx, &row, &source, &payload.to_string())
                    .map_err(steve_modules::ApiError::from)
            })
            .expect("insert");
    }
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let gh = h.gh.clone();
        let done = done.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            let mut n = 0usize;
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                n += gh.recover_deliveries();
            }
            n += gh.recover_deliveries();
            n
        }));
    }
    let mut queued = h.gh.recover_deliveries();
    h.gh.settle().await;
    done.store(true, std::sync::atomic::Ordering::Release);
    for handle in handles {
        queued += handle.await.unwrap();
    }
    h.gh.settle().await;
    assert_eq!(
        queued, total,
        "every accepted delivery is queued at most once"
    );
    assert_eq!(
        q(&h, "SELECT count(*) FROM gh_prs", &[]),
        json!(total as i64)
    );
}

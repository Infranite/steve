//! G5 acceptance: the example as a `gh_repo` draft — its change lines,
//! apply, undo, a stale draft; the `github.comment` grant drafted on the repo and through
//! [`CommentGrants`] (Chinese name, bot / repo / hours parsing, never a guess); an issue turned
//! into an item has a `from` link and is not turned twice; `GET /github/repos/:id`.
//!
//! Only the fake GitHub and a fixed fake model output; nothing here talks to github.com.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, GraphEngine};
use steve_github::config::{COMMENT_LABEL, CommentGrants, GrantCapability, KIND};
use steve_github::fake::{FakeGitHub, defaults};
use steve_github::{GitHub, GitHubEnv, RecordingCore, accept, issues, manifest, repos};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::changes::{self, Origin};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::base::Change;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
const PROJECT: &str = "prj_g";
const BOT: &str = "bot_fe";

struct H {
    env: TestEnv,
    _keys: TempDir,
    gh: GitHub,
    api: Router,
    fake: FakeGitHub,
    core: Arc<FakeCore>,
    repo: String,
    issue_no: u64,
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

async fn nav(r: &Router, uri: &str) -> u16 {
    let mut req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    req.extensions_mut().insert(caller(WS, ME));
    r.clone()
        .oneshot(req)
        .await
        .expect("infallible")
        .status()
        .as_u16()
}

/// The App installed, `steve-test/demo` linked to the project “Demo”, an open issue on GitHub
/// synced by the first poll; bots 前端 Bot (main) and 后端 Bot; flows with a fake steve-core.
async fn setup() -> H {
    let fake = FakeGitHub::start().await.expect("fake github");
    let issue_no = fake
        .add_issue(
            defaults::REPO,
            "登录后跳回首页",
            "登录成功后没有回到 return_to 指的页面。",
            &["bug"],
        )
        .expect("issue");
    let core = Arc::new(FakeCore::default());
    let setup = FlowSetup::new(core.clone());
    let mut slot = None;
    let env = TestEnv::new(|r| {
        connections::register(r);
        setup.register(r);
        slot = Some(accept::register(r));
    });
    *core.store.lock().expect("lock") = Some(env.store().clone());
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let cfg = ConnectionsConfig::new(vec![ORIGIN.into()], true, |_| None);
    let mut b = Connections::builder(env.modules.clone(), vault, cfg);
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
            ..Default::default()
        },
    );
    slot.expect("slot").bind(&gh);
    gh.set_core(RecordingCore::new(env.store().clone()));
    gh.set_flow(flow);
    let api = manifest::routes(gh.clone())
        .merge(repos::routes(gh.clone()))
        .merge(issues::routes(gh.clone()))
        .with_state(env.modules.clone());

    env.store()
        .with_conn(|c| {
            for (id, name) in [(PROJECT, "Demo"), ("prj_other", "Other")] {
                c.execute(
                    "INSERT INTO projects (id, workspace_id, name, path, kind, created, updated) VALUES (?, ?, ?, '/tmp/none', 'dir', 1, 1)",
                    rusqlite::params![id, WS, name],
                )?;
            }
            for (id, name, main) in [(BOT, "前端 Bot", 1), ("bot_be", "后端 Bot", 0)] {
                c.execute(
                    "INSERT INTO bots (id, workspace_id, name, harness, main, updated) VALUES (?, ?, ?, 'claude', ?, 1)",
                    rusqlite::params![id, WS, name, main],
                )?;
                c.execute(
                    "INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES (?, ?, 1)",
                    rusqlite::params![id, WS],
                )?;
            }
            Ok(())
        })
        .expect("seed");

    let me = caller(WS, ME);
    let (s, v) = call(
        &api,
        Some(&me),
        "POST",
        "/github/app/manifest",
        Some(&json!({})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let state = v["state"].as_str().expect("state").to_string();
    let code = fake.manifest_code();
    assert_eq!(
        nav(
            &api,
            &format!("/github/app/manifest/callback?code={code}&state={state}")
        )
        .await,
        302
    );
    assert_eq!(
        nav(
            &api,
            &format!(
                "/github/app/installed?installation_id={}&setup_action=install",
                defaults::INSTALLATION_ID
            )
        )
        .await,
        302
    );
    let conn: String = env
        .store()
        .reader(|c| {
            c.query_row(
                "SELECT id FROM connections WHERE adapter = 'github_app'",
                [],
                |r| r.get(0),
            )
            .map_err(steve_store::StoreError::from)
        })
        .expect("connection");
    let (s, v) = call(
        &api,
        Some(&me),
        "POST",
        "/github/repos",
        Some(&json!({"connection_id": conn, "gh_id": defaults::REPO_ID, "project_id": PROJECT})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let repo = v["id"].as_str().expect("repo id").to_string();
    gh.settle().await;
    H {
        env,
        _keys: keys,
        gh,
        api,
        fake,
        core,
        repo,
        issue_no,
    }
}

fn lines(changes: &[Change]) -> Vec<(String, String, String)> {
    changes
        .iter()
        .map(|c| (c.label.clone(), c.from.clone(), c.to.clone()))
        .collect()
}

fn l(a: &str, b: &str, c: &str) -> (String, String, String) {
    (a.into(), b.into(), c.into())
}

async fn draft(h: &H, request: &str, reply: Value) -> changes::Proposal {
    h.env.gateway.reply(reply.to_string());
    changes::draft(
        &h.env.modules,
        &caller(WS, ME),
        KIND,
        Some(&h.repo),
        request,
        Origin::default(),
    )
    .await
    .expect("draft")
}

async fn repo_json(h: &H) -> Value {
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/github/repos/{}", h.repo),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn repo_detail_has_the_section_8_shape() {
    let h = setup().await;
    let v = repo_json(&h).await;
    assert_eq!(v["id"], json!(h.repo));
    assert_eq!(v["full_name"], defaults::REPO);
    assert_eq!(v["connection"]["kind"], "app");
    assert_eq!(v["project"], json!({"id": PROJECT, "name": "Demo"}));
    assert_eq!(v["settings"]["finish_on"], "accepted");
    assert_eq!(v["open_issues"], 1);
    assert_eq!(v["enabled"], true);
    for k in [
        "private",
        "default_branch",
        "ingest",
        "healthy",
        "last_delivery",
        "open_prs",
        "revision",
    ] {
        assert!(v.get(k).is_some(), "{k} missing: {v}");
    }
    let (s, _) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/github/repos/{}", h.repo),
        None,
    )
    .await;
    assert_eq!(s, 404, "another workspace");
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        "/github/repos/ghr_nope",
        None,
    )
    .await;
    assert_eq!((s, v), (404, json!({"error": "没有这个仓库"})));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_section_9_example_drafts_applies_and_undoes() {
    let h = setup().await;
    let me = caller(WS, ME);
    let (s, _) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/github/repos/{}", h.repo),
        Some(&json!({"settings": {"merge_method": "rebase"}})),
    )
    .await;
    assert_eq!(s, 200);
    let rev = repo_json(&h).await["revision"].clone();

    let p = draft(
        &h,
        "demo 仓库的 PR 合并后再算完成，用 squash",
        json!({"ops": [
            {"op": "set", "key": "finish_on", "value": "合并后才完成"},
            {"op": "set", "key": "merge_method", "value": "squash"},
            {"op": "set", "key": "delete_branch", "value": "是"},
            {"op": "set", "key": "colour", "value": "blue"},
        ]}),
    )
    .await;
    let prompt = h.env.gateway.last_prompt().expect("prompt");
    assert!(
        prompt.contains("the GitHub repository \"steve-test/demo\""),
        "{prompt}"
    );
    assert!(prompt.contains("合并后才完成"), "{prompt}");
    assert!(prompt.contains("grant.add"), "{prompt}");
    assert_eq!(p.title, "仓库 steve-test/demo");
    assert_eq!(p.href, format!("#/github/{}", h.repo));
    assert_eq!(p.target, json!({"kind": "gh_repo", "id": h.repo}));
    // delete_branch is already 是: no line for an unchanged value
    assert_eq!(
        lines(&p.changes),
        vec![
            l("什么时候算完成", "开了 PR 就完成", "合并后才完成"),
            l("合并方式", "rebase", "squash"),
            l("（没认出）", "—", "colour: blue"),
        ]
    );

    let r = changes::apply(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("apply");
    assert_eq!(r["ok"], true);
    let v = repo_json(&h).await;
    assert_eq!(v["settings"]["finish_on"], "merged");
    assert_eq!(v["settings"]["merge_method"], "squash");
    assert_eq!(v["revision"], json!(rev.as_i64().unwrap() + 1));
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM revisions WHERE object_type = 'gh_repo' AND object_id = ?",
            &[&h.repo]
        ),
        json!(2),
        "PATCH, apply"
    );

    changes::undo(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("undo");
    let v = repo_json(&h).await;
    assert_eq!(v["settings"]["finish_on"], "accepted");
    assert_eq!(v["settings"]["merge_method"], "rebase");
    let again = changes::undo(&h.env.modules, &me, &p.id).expect("stored");
    assert!(again.is_err(), "undone once");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_goes_stale_when_the_repo_changes() {
    let h = setup().await;
    let me = caller(WS, ME);
    let p = draft(
        &h,
        "检查失败时只记录",
        json!({"ops": [{"op": "set", "key": "ci_fail", "value": "只记录"}]}),
    )
    .await;
    assert_eq!(
        lines(&p.changes),
        vec![l("检查失败时", "交给事项修", "只记录")]
    );
    let (s, _) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/github/repos/{}", h.repo),
        Some(&json!({"settings": {"draft_pr": true}})),
    )
    .await;
    assert_eq!(s, 200);
    let e = changes::apply(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect_err("stale");
    assert_eq!(e.to_string(), changes::STALE);
    assert_eq!(repo_json(&h).await["settings"]["ci_fail"], "supplement");
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_are_matched_by_exact_name_only() {
    let h = setup().await;
    let me = caller(WS, ME);
    let p = draft(
        &h,
        "改成关联 Oth 项目",
        json!({"ops": [{"op": "set", "key": "project", "value": "Oth"}]}),
    )
    .await;
    assert_eq!(
        lines(&p.changes),
        vec![l("关联的项目", "—", "（没认出：Oth）")]
    );
    let p = draft(
        &h,
        "改成关联 Other 项目",
        json!({"ops": [{"op": "set", "key": "project", "value": "Other"}]}),
    )
    .await;
    assert_eq!(lines(&p.changes), vec![l("关联的项目", "Demo", "Other")]);
    changes::apply(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("apply");
    assert_eq!(repo_json(&h).await["project"]["id"], "prj_other");
    changes::undo(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("undo");
    assert_eq!(repo_json(&h).await["project"]["id"], PROJECT);

    let p = draft(
        &h,
        "不关联项目了",
        json!({"ops": [{"op": "set", "key": "project", "value": null}]}),
    )
    .await;
    assert_eq!(lines(&p.changes), vec![l("关联的项目", "Demo", "（无）")]);
    changes::apply(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("apply");
    assert_eq!(repo_json(&h).await["project"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_comment_grant_is_drafted_applied_and_undone() {
    let h = setup().await;
    let me = caller(WS, ME);
    let p = draft(
        &h,
        "授权前端 Bot 在 demo 仓库评论 8 小时",
        json!({"ops": [{"op": "grant.add", "bot": "前端 Bot", "hours": 8}]}),
    )
    .await;
    assert_eq!(
        lines(&p.changes),
        vec![l(
            "授权 前端 Bot",
            "—",
            "在 GitHub 评论（仓库 steve-test/demo），8 小时"
        )]
    );
    changes::apply(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("apply");
    let now = steve_store::now();
    let g = h
        .env
        .store()
        .reader(|c| steve_github::config::comment_grants(c, WS, now))
        .expect("grants");
    assert_eq!(g.len(), 1);
    assert_eq!((g[0].1.as_str(), g[0].2.as_str()), (BOT, h.repo.as_str()));
    let hours = (g[0].3 - now) as f64 / 3_600_000.0;
    assert!((7.9..=8.01).contains(&hours), "{hours}");
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM audit WHERE action = 'grant.add' AND target = ?",
            &[&h.repo]
        ),
        json!(1)
    );

    changes::undo(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("undo");
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM grants WHERE capability = 'github.comment' AND revoked IS NULL",
            &[]
        ),
        json!(0)
    );

    // Nothing is guessed: an unknown bot, or hours beyond the cap, make no operation.
    let p = draft(
        &h,
        "授权前端在 demo 评论 48 小时",
        json!({"ops": [
            {"op": "grant.add", "bot": "前端", "hours": 8},
            {"op": "grant.add", "bot": "前端 Bot", "hours": 48},
        ]}),
    )
    .await;
    assert_eq!(
        lines(&p.changes),
        vec![
            l("授权", "—", "（没认出：前端）"),
            l("授权 前端 Bot", "—", "（没认出：48，只能 1、8 或 24 小时）"),
        ]
    );
    let r = changes::apply(&h.env.modules, &me, &p.id)
        .expect("stored")
        .expect("apply of nothing");
    assert_eq!(r["ok"], true);
    assert_eq!(
        q(&h, "SELECT count(*) FROM grants", &[]),
        json!(1),
        "only the revoked one"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_capability_parses_grant_drafts_for_the_vault() {
    let h = setup().await;
    let g = CommentGrants;
    assert_eq!(g.capability(), "github.comment");
    assert_eq!(g.label(), COMMENT_LABEL);
    assert_eq!(g.resource_type(), "repo");
    let repo = h.repo.clone();
    h.env
        .store()
        .reader(|c| {
            let err = |e: steve_modules::ApiError| e;
            for said in [
                "demo 仓库",
                "steve-test/demo",
                "Steve-Test/Demo",
                "Demo",
                &repo,
            ] {
                assert_eq!(
                    g.resolve_resource(c, WS, said).map_err(err)?,
                    Some((repo.clone(), "仓库 steve-test/demo".into())),
                    "{said}"
                );
            }
            assert_eq!(g.resolve_resource(c, WS, "dem").map_err(err)?, None);
            assert_eq!(g.resolve_resource(c, WS2, "demo").map_err(err)?, None);
            assert_eq!(
                g.resource_label(c, WS, &repo).map_err(err)?.as_deref(),
                Some("仓库 steve-test/demo")
            );

            let p = g
                .plan_grant(
                    c,
                    WS,
                    &json!({"bot": "前端 Bot", "resource": "demo 仓库", "hours": "8 小时"}),
                )
                .map_err(err)?;
            assert_eq!(
                p.ops,
                vec![json!({"op": "gh.grant.add", "bot": BOT, "repo": repo, "hours": 8})]
            );
            assert_eq!(
                lines(&p.changes),
                vec![l(
                    "授权 前端 Bot",
                    "—",
                    "在 GitHub 评论（仓库 steve-test/demo），8 小时"
                )]
            );
            let p = g
                .plan_grant(
                    c,
                    WS,
                    &json!({"bot": "前端 Bot", "resource": "web", "hours": 1}),
                )
                .map_err(err)?;
            assert!(p.ops.is_empty());
            assert_eq!(
                lines(&p.changes),
                vec![l("授权 前端 Bot", "—", "（没认出：web）")]
            );
            Ok::<_, steve_modules::ApiError>(())
        })
        .expect("checks");
}

#[tokio::test(flavor = "multi_thread")]
async fn repos_are_global_candidates() {
    let h = setup().await;
    let c = changes::candidates(&h.env.modules, &caller(WS, ME)).expect("candidates");
    let line = format!("gh_repo:{} — 仓库 steve-test/demo", h.repo);
    assert!(c.iter().any(|x| x.line() == line), "{c:?}");
    let c = changes::candidates(&h.env.modules, &caller(WS2, ME)).expect("candidates");
    assert!(!c.iter().any(|x| x.code.starts_with("gh_repo:")), "{c:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issue_becomes_an_item_once_with_a_from_link() {
    let h = setup().await;
    let me = caller(WS, ME);
    let (s, list) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/issues?repo={}", h.repo),
        None,
    )
    .await;
    assert_eq!(s, 200, "{list}");
    let issue = list[0]["id"].as_str().expect("issue").to_string();
    assert_eq!(list[0]["number"], json!(h.issue_no));
    assert_eq!(list[0]["item"], Value::Null);

    let uri = format!("/github/issues/{issue}/to-item");
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &uri,
        Some(&json!({"mode": "later"})),
    )
    .await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：mode"})));
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &uri,
        Some(&json!({"bot": "bot_nope"})),
    )
    .await;
    assert_eq!((s, v), (404, json!({"error": "没有这个 Bot"})));
    let (s, _) = call(&h.api, Some(&caller(WS2, ME)), "POST", &uri, None).await;
    assert_eq!(s, 404, "another workspace");
    assert!(h.core.created().is_empty());

    // no body = {}: the main bot, started
    let (s, v) = call(&h.api, Some(&me), "POST", &uri, None).await;
    assert_eq!(s, 200, "{v}");
    let item = v["item_id"].as_str().expect("item").to_string();
    let made = h.core.created();
    assert_eq!(made.len(), 1);
    let r = &made[0];
    assert_eq!(r.title, "登录后跳回首页");
    assert_eq!(r.bot, BOT);
    assert_eq!(r.project.as_deref(), Some(PROJECT));
    assert_eq!(r.source, ("GitHub".to_string(), "sig".to_string()));
    assert!(!r.propose);
    assert!(
        r.goal.starts_with(&format!(
            "{}\nGitHub Issue steve-test/demo#{}：登录后跳回首页",
            steve_modules::outbound::UNTRUSTED_OPEN,
            h.issue_no
        )),
        "{}",
        r.goal
    );
    assert!(r.goal.contains("没有回到 return_to 指的页面"), "{}", r.goal);
    assert!(
        h.fake
            .calls_to(
                "GET",
                &format!("/repos/steve-test/demo/issues/{}", h.issue_no)
            )
            .len()
            == 1
    );
    assert_eq!(
        q(
            &h,
            "SELECT count(*) FROM links WHERE subject_type = 'item' AND subject_id = ? AND predicate = 'from' AND object_type = 'gh_issue' AND object_id = ?",
            &[&item, &issue]
        ),
        json!(1)
    );
    let (_, list) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/issues?repo={}", h.repo),
        None,
    )
    .await;
    assert_eq!(list[0]["item"]["id"], json!(item));

    // again: the same item, nothing new
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        &uri,
        Some(&json!({"bot": "bot_be"})),
    )
    .await;
    assert_eq!((s, v["item_id"].clone()), (200, json!(item)));
    assert_eq!(h.core.created().len(), 1);

    // a poll later keeps the link
    h.gh.poll_repo(&h.repo).await.ok();
    h.gh.settle().await;
    let (_, list) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/github/issues?repo={}", h.repo),
        None,
    )
    .await;
    assert_eq!(list[0]["item"]["id"], json!(item));
}

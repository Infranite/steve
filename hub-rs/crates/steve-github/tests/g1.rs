//! G1 acceptance: on the fake GitHub, manifest → install → connection; the App's key
//! and webhook secret are ciphertext in the system vault; installation tokens are renewed 5 minutes
//! before they expire; a 401 re-mints once, a second one puts the connection in `reauth`; ETag 304s
//! and rate limits; the PAT adapter.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_github::fake::{FakeGitHub, Fault, defaults};
use steve_github::{GhError, GitHub, GitHubEnv, manifest};
use steve_modules::Caller;
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";

struct H {
    env: TestEnv,
    _keys: TempDir,
    vault: Vault,
    gh: GitHub,
    api: Router,
    fake: FakeGitHub,
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
    let cfg = ConnectionsConfig::new(vec![ORIGIN.into()], true, |_| None);
    let mut b = Connections::builder(env.modules.clone(), vault.clone(), cfg);
    let gh_env = GitHubEnv {
        api_base: fake.url(),
        ..Default::default()
    };
    for p in steve_github::providers_for(&gh_env) {
        b = b.provider(p);
    }
    let conns = b.build();
    let gh = GitHub::new(conns.clone(), gh_env);
    let api = manifest::routes(gh.clone())
        .merge(connections::routes(conns.clone()))
        .with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        vault,
        gh,
        api,
        fake,
    }
}

/// A browser navigation: status and `Location`.
async fn nav(r: &Router, who: Option<&Caller>, uri: &str) -> (u16, String) {
    let mut req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    if let Some(c) = who {
        req.extensions_mut().insert(c.clone());
    }
    let res = r.clone().oneshot(req).await.expect("infallible");
    let loc = res
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (res.status().as_u16(), loc)
}

/// The `error` of a `/#/github?error=…` location.
fn decode(loc: &str) -> String {
    let q = loc.split_once('?').map(|x| x.1).unwrap_or_default();
    url::form_urlencoded::parse(q.as_bytes())
        .find(|(k, _)| k == "error")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

fn scalar(h: &H, sql: &str, p: &str) -> Value {
    h.env
        .store()
        .reader(|c| {
            c.query_row(sql, [p], |r| {
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

/// Starts the manifest flow and returns the state.
async fn start(h: &H, who: &Caller, body: Value) -> Value {
    let (s, v) = call(
        &h.api,
        Some(who),
        "POST",
        "/github/app/manifest",
        Some(&body),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    v
}

/// Creates the App (steps 1–2) and installs it (step 3) on the default installation.
async fn create_and_install(h: &H) -> String {
    let me = caller(WS, ME);
    let v = start(h, &me, json!({})).await;
    let state = v["state"].as_str().expect("state").to_string();
    let code = h.fake.manifest_code();
    let (s, loc) = nav(
        &h.api,
        Some(&me),
        &format!("/github/app/manifest/callback?code={code}&state={state}"),
    )
    .await;
    assert_eq!((s, loc.as_str()), (302, "/#/github?app=created"));
    let (s, loc) = nav(
        &h.api,
        Some(&me),
        &format!(
            "/github/app/installed?installation_id={}&setup_action=install",
            defaults::INSTALLATION_ID
        ),
    )
    .await;
    assert_eq!((s, loc.as_str()), (302, "/#/github"));
    scalar(
        h,
        "SELECT id FROM connections WHERE adapter = ?",
        "github_app",
    )
    .as_str()
    .expect("connection")
    .to_string()
}

fn db_bytes(h: &H) -> Vec<u8> {
    let mut all = Vec::new();
    for ext in ["", "-wal", "-shm"] {
        if let Ok(b) = std::fs::read(format!("{}{ext}", h.env.store().path().display())) {
            all.extend(b);
        }
    }
    all
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_install_and_connection_on_the_fake() {
    let h = setup().await;
    let me = caller(WS, ME);

    // step 1: the form
    let v = start(&h, &me, json!({})).await;
    let state = v["state"].as_str().expect("state");
    assert_eq!(
        v["action"],
        json!(format!(
            "https://github.com/settings/apps/new?state={state}"
        ))
    );
    let m = &v["manifest"];
    assert!(m["name"].as_str().expect("name").starts_with("Steve · "));
    assert_eq!(m["url"], ORIGIN);
    assert_eq!(m["public"], false);
    assert_eq!(
        m["hook_attributes"],
        json!({"url": format!("{ORIGIN}/hooks/github"), "active": true})
    );
    assert_eq!(
        m["redirect_url"],
        json!(format!("{ORIGIN}/api/github/app/manifest/callback"))
    );
    assert_eq!(
        m["setup_url"],
        json!(format!("{ORIGIN}/api/github/app/installed"))
    );
    assert_eq!(m["setup_on_update"], true);
    assert_eq!(
        m["default_permissions"],
        json!({"metadata":"read","contents":"write","pull_requests":"write","issues":"write",
               "checks":"read","actions":"write","statuses":"read"})
    );
    let org = start(&h, &me, json!({"owner": "steve-test"})).await;
    assert!(
        org["action"]
            .as_str()
            .expect("action")
            .starts_with("https://github.com/organizations/steve-test/settings/apps/new?state=")
    );
    let (s, bad) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/app/manifest",
        Some(&json!({"owner": "a/b"})),
    )
    .await;
    assert_eq!((s, &bad["error"]), (400, &json!("参数不对：owner")));

    // step 2: the conversion
    let code = h.fake.manifest_code();
    let (s, loc) = nav(
        &h.api,
        Some(&me),
        &format!("/github/app/manifest/callback?code={code}&state={state}"),
    )
    .await;
    assert_eq!((s, loc.as_str()), (302, "/#/github?app=created"));
    assert_eq!(
        h.fake.calls_to("POST", "/app-manifests/")[0].authorization,
        None,
        "the conversion is unauthenticated"
    );
    let app_id = format!("ghapp_{}", defaults::APP_ID);
    assert_eq!(
        scalar(&h, "SELECT slug FROM gh_apps WHERE id = ?", &app_id),
        json!(defaults::APP_SLUG)
    );
    assert_eq!(
        scalar(&h, "SELECT owner FROM gh_apps WHERE id = ?", &app_id),
        json!(defaults::ORG)
    );
    let item = scalar(
        &h,
        "SELECT credential_item FROM gh_apps WHERE id = ?",
        &app_id,
    );
    let item = item.as_str().expect("item");
    // the key and secrets are in the vault, encrypted
    assert_eq!(
        scalar(
            &h,
            "SELECT group_concat(label || ':' || secret || ':' || (value_plain IS NULL) || ':' || (value_ct IS NOT NULL), ',') FROM vault_fields WHERE item_id = ?",
            item
        ),
        json!("private_key:1:1:1,webhook_secret:1:1:1,client_secret:1:1:1")
    );
    let pem_line = defaults::APP_PRIVATE_KEY_PEM
        .lines()
        .nth(5)
        .expect("pem body");
    let db = db_bytes(&h);
    for secret in [pem_line, defaults::WEBHOOK_SECRET, defaults::CLIENT_SECRET] {
        assert!(!contains(&db, secret), "plaintext secret in the database");
    }
    let plain = h
        .vault
        .with_credential(WS, item, "test", "测试", |c| {
            (
                c.text("private_key").map(String::from),
                c.text("webhook_secret").map(String::from),
            )
        })
        .expect("credential");
    assert_eq!(
        plain,
        (
            Some(defaults::APP_PRIVATE_KEY_PEM.to_string()),
            Some(defaults::WEBHOOK_SECRET.to_string())
        )
    );
    // the state was single use
    let (_, loc) = nav(
        &h.api,
        Some(&me),
        &format!(
            "/github/app/manifest/callback?code={}&state={state}",
            h.fake.manifest_code()
        ),
    )
    .await;
    assert_eq!(decode(&loc), "这个链接已经失效，请重新创建 GitHub App");

    // step 3: the installation
    let (s, loc) = nav(
        &h.api,
        Some(&me),
        "/github/app/installed?installation_id=999&setup_action=install",
    )
    .await;
    assert_eq!(
        (s, decode(&loc).as_str()),
        (302, "这个安装不属于本工作空间的 GitHub App")
    );
    let (s, loc) = nav(
        &h.api,
        Some(&me),
        &format!(
            "/github/app/installed?installation_id={}&setup_action=install",
            defaults::INSTALLATION_ID
        ),
    )
    .await;
    assert_eq!((s, loc.as_str()), (302, "/#/github"));
    let con = scalar(
        &h,
        "SELECT id FROM connections WHERE adapter = ?",
        "github_app",
    );
    let con = con.as_str().expect("connection").to_string();
    let cfg: Value = serde_json::from_str(
        scalar(&h, "SELECT config FROM connections WHERE id = ?", &con)
            .as_str()
            .expect("config"),
    )
    .expect("json");
    assert_eq!(cfg["app_id"], json!(defaults::APP_ID));
    assert_eq!(cfg["installation_id"], json!(defaults::INSTALLATION_ID));
    assert_eq!(cfg["account_login"], json!(defaults::ORG));
    assert_eq!(cfg["account_type"], json!("Organization"));
    assert_eq!(cfg["api_base"], json!(h.fake.url()));
    assert_eq!(
        scalar(&h, "SELECT caps FROM connections WHERE id = ?", &con),
        json!(r#"["repo.read","repo.write","events"]"#)
    );
    assert_eq!(
        scalar(
            &h,
            "SELECT a.handle || '|' || a.kind || '|' || p.org_kind FROM connections c JOIN accounts a ON a.id = c.account_id JOIN parties p ON p.id = a.party_id WHERE c.id = ?",
            &con
        ),
        json!(format!("{}[bot]|app_bot|self", defaults::APP_SLUG))
    );
    // the connection's own credential: key and webhook secret, encrypted as well
    let citem = scalar(
        &h,
        "SELECT credential_item FROM connections WHERE id = ?",
        &con,
    );
    assert_eq!(
        scalar(
            &h,
            "SELECT group_concat(label || ':' || (value_plain IS NULL), ',') FROM vault_fields WHERE item_id = ?",
            citem.as_str().expect("item")
        ),
        json!("private_key:1,webhook_secret:1")
    );
    let db = db_bytes(&h);
    assert!(!contains(&db, pem_line) && !contains(&db, defaults::WEBHOOK_SECRET));

    // a second callback for the same installation (setup_on_update) updates, no new connection
    let (_, loc) = nav(
        &h.api,
        Some(&me),
        &format!(
            "/github/app/installed?installation_id={}&setup_action=update",
            defaults::INSTALLATION_ID
        ),
    )
    .await;
    assert_eq!(loc, "/#/github");
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM connections WHERE adapter = ?",
            "github_app"
        ),
        json!(1)
    );
    // another installation of the same App is refused while the first one exists
    h.fake
        .add_installation(4343, "other-org", "Organization", &[]);
    let (_, loc) = nav(
        &h.api,
        Some(&me),
        "/github/app/installed?installation_id=4343&setup_action=install",
    )
    .await;
    assert_eq!(
        decode(&loc),
        "这个 GitHub App 已经装在 steve-test 上了，一个 App 目前只能接一个安装"
    );

    // the connection works: an installation token on the fake
    let conn = h.gh.conn(WS, &con).expect("conn");
    let repos = conn
        .get_all("/installation/repositories", Some("repositories"))
        .await
        .expect("repos");
    assert_eq!(repos[0]["full_name"], json!(defaults::REPO));
    assert!(
        h.fake.calls_to("GET", "/installation/repositories")[0]
            .authorization
            .as_deref()
            .is_some_and(|a| a.starts_with("Bearer ghs_"))
    );
    // other workspaces don't see it
    assert!(h.gh.conn("w2", &con).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_states_belong_to_their_user_and_expire() {
    let h = setup().await;
    let me = caller(WS, ME);
    let other = caller(WS, OTHER);
    let v = start(&h, &me, json!({})).await;
    let state = v["state"].as_str().expect("state").to_string();
    let uri = |s: &str| {
        format!(
            "/github/app/manifest/callback?code={}&state={s}",
            h.fake.manifest_code()
        )
    };
    let (_, loc) = nav(&h.api, Some(&other), &uri(&state)).await;
    assert_eq!(decode(&loc), "这个链接已经失效，请重新创建 GitHub App");
    // taken by the wrong user: gone for the right one too
    let (_, loc) = nav(&h.api, Some(&me), &uri(&state)).await;
    assert_eq!(decode(&loc), "这个链接已经失效，请重新创建 GitHub App");
    let (_, loc) = nav(&h.api, None, &uri("x")).await;
    assert_eq!(decode(&loc), "请先登录 Steve，再重新创建 GitHub App");
    // 10 minutes
    let v = start(&h, &me, json!({})).await;
    h.env.clock.advance(10 * 60_000 + 1);
    let (_, loc) = nav(&h.api, Some(&me), &uri(v["state"].as_str().expect("state"))).await;
    assert_eq!(decode(&loc), "这个链接已经失效，请重新创建 GitHub App");
    // a code GitHub doesn't know
    let v = start(&h, &me, json!({})).await;
    let (_, loc) = nav(
        &h.api,
        Some(&me),
        &format!(
            "/github/app/manifest/callback?code=nope&state={}",
            v["state"].as_str().expect("state")
        ),
    )
    .await;
    assert_eq!(decode(&loc), "这个链接已经失效，请重新创建 GitHub App");
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM gh_apps WHERE workspace_id = ?",
            WS
        ),
        json!(0)
    );
    // installing before any App exists
    let (_, loc) = nav(
        &h.api,
        Some(&me),
        "/github/app/installed?installation_id=4242&setup_action=install",
    )
    .await;
    assert_eq!(decode(&loc), "这个工作空间还没有创建 GitHub App");
    let (_, loc) = nav(
        &h.api,
        Some(&me),
        "/github/app/installed?setup_action=request",
    )
    .await;
    assert_eq!(decode(&loc), "已经向组织管理员申请安装，管理员批准后再回来");
}

#[tokio::test(flavor = "multi_thread")]
async fn installation_tokens_renew_five_minutes_before_expiry() {
    let h = setup().await;
    let con = create_and_install(&h).await;
    let base = h.fake.issued_tokens();
    // tokens that live 6 minutes: cached for one, renewed after
    h.fake.set_token_ttl(6 * 60);
    let conn = h.gh.conn(WS, &con).expect("conn");
    let cx = conn.cx().clone();
    let t1 = cx.access_token().await.expect("token");
    let t1b = cx.access_token().await.expect("cached");
    assert_eq!(t1.expose(), t1b.expose());
    assert_eq!(h.fake.issued_tokens(), base + 1);
    h.env.clock.advance(50_000);
    let _ = cx.access_token().await.expect("still cached");
    assert_eq!(h.fake.issued_tokens(), base + 1, "more than 5 minutes left");
    h.env.clock.advance(20_000);
    let t2 = cx.access_token().await.expect("renewed");
    assert_ne!(t2.expose(), t1.expose());
    assert_eq!(
        h.fake.issued_tokens(),
        base + 2,
        "within 5 minutes of expiry"
    );
    // tokens that live less than 5 minutes are never reused
    h.fake.set_token_ttl(4 * 60);
    h.env.clock.advance(6 * 60_000);
    let _ = cx.access_token().await.expect("token");
    let _ = cx.access_token().await.expect("token");
    assert_eq!(h.fake.issued_tokens(), base + 4);
    // the JWT that minted them: RS256, iss = app id
    let jwt = h.fake.calls_to("POST", "/app/installations/")[0]
        .authorization
        .clone()
        .expect("jwt");
    assert_eq!(jwt.matches('.').count(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_401_re_mints_once_then_reauth() {
    let h = setup().await;
    let con = create_and_install(&h).await;
    let conn = h.gh.conn(WS, &con).expect("conn");
    conn.get("/installation/repositories").await.expect("ok");
    let before = h.fake.issued_tokens();
    // the cached token is revoked: one 401, a new token, success
    h.fake.revoke_tokens();
    let v = conn
        .get("/installation/repositories")
        .await
        .expect("after re-mint");
    assert_eq!(v["total_count"], json!(1));
    assert_eq!(h.fake.issued_tokens(), before + 1);
    assert_eq!(
        scalar(&h, "SELECT status FROM connections WHERE id = ?", &con),
        json!("ok")
    );
    // 401 twice in a row: reauth with one decision
    h.fake
        .fail_next_on("/repos/steve-test/demo", Fault::BadCredentials);
    h.fake
        .fail_next_on("/repos/steve-test/demo", Fault::BadCredentials);
    let e = conn.get("/repos/steve-test/demo").await;
    assert_eq!(e.err(), Some(GhError::Unauthorized));
    assert_eq!(h.fake.issued_tokens(), before + 2, "re-minted exactly once");
    assert_eq!(
        scalar(&h, "SELECT status FROM connections WHERE id = ?", &con),
        json!("reauth")
    );
    assert_eq!(
        scalar(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type = 'connection' AND subject_id = ?",
            &con
        ),
        json!(1)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn etags_and_rate_limits() {
    let h = setup().await;
    let con = create_and_install(&h).await;
    let conn = h.gh.conn(WS, &con).expect("conn");
    let path = "/repos/steve-test/demo";
    let a = conn.get(path).await.expect("first");
    let b = conn.get(path).await.expect("second");
    assert_eq!(a, b);
    let calls = h.fake.calls_to("GET", path);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].if_none_match, None);
    assert!(calls[1].if_none_match.is_some());
    assert_eq!(calls[1].status, 304, "unchanged: a 304, not counted");
    // the caller's own etag (polling state)
    let r = conn
        .get_if_changed(path, calls[1].if_none_match.as_deref())
        .await
        .expect("conditional");
    assert!(r.not_modified());

    // a secondary limit with retry-after: waited out, then success
    h.fake.fail_next_on(
        path,
        Fault::RetryAfter {
            status: 429,
            seconds: 1,
        },
    );
    let t = std::time::Instant::now();
    conn.get(path).await.expect("after waiting");
    assert!(t.elapsed() >= Duration::from_millis(900));
    // the primary limit, reset an hour away: not waited for
    h.fake.set_rate_remaining(0);
    let e = conn.get(path).await.expect_err("limited");
    assert!(
        matches!(e, GhError::RateLimited { wait_secs } if wait_secs > 60),
        "{e:?}"
    );
    assert_eq!(
        scalar(&h, "SELECT status FROM connections WHERE id = ?", &con),
        json!("ok"),
        "a rate limit is not an auth failure"
    );
    // a 403 without rate-limit headers is a refusal, at once
    h.fake.set_rate_remaining(5000);
    h.fake.fail_next_on(
        path,
        Fault::Status {
            status: 403,
            message: "Resource not accessible by integration".into(),
        },
    );
    let e = conn.get(path).await.expect_err("refused");
    assert_eq!(e.status(), Some(403));
}

#[tokio::test(flavor = "multi_thread")]
async fn personal_access_tokens_connect_as_me() {
    let h = setup().await;
    let me = caller(WS, ME);
    let (s, ad) = call(&h.api, Some(&me), "GET", "/connections/adapters", None).await;
    assert_eq!(s, 200);
    let ids: Vec<&str> = ad
        .as_array()
        .expect("list")
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        ["github_pat"],
        "the App is created by the manifest flow"
    );
    let (s, bad) = call(
        &h.api,
        Some(&me),
        "POST",
        "/connections",
        Some(&json!({"adapter": "github_pat", "fields": {"令牌": "github_pat_WRONG"}, "config": {"api_base": h.fake.url()}})),
    )
    .await;
    assert_eq!(s, 400);
    assert_eq!(bad["error"], json!("验证没有通过：GitHub 不认这个令牌"));
    let (s, c) = call(
        &h.api,
        Some(&me),
        "POST",
        "/connections",
        Some(&json!({"adapter": "github_pat", "fields": {"令牌": defaults::PAT}, "config": {"api_base": h.fake.url()}})),
    )
    .await;
    assert_eq!(s, 200, "{c}");
    let con = c["id"].as_str().expect("id").to_string();
    assert_eq!(
        scalar(
            &h,
            "SELECT a.handle || '|' || a.kind || '|' || p.user_id FROM connections c JOIN accounts a ON a.id = c.account_id JOIN parties p ON p.id = a.party_id WHERE c.id = ?",
            &con
        ),
        json!(format!("{}|personal|{ME}", defaults::USER))
    );
    assert!(!contains(&db_bytes(&h), defaults::PAT));
    let conn = h.gh.conn(WS, &con).expect("conn");
    let me_json = conn.get("/user").await.expect("user");
    assert_eq!(me_json["login"], json!(defaults::USER));
    // a PAT can't be re-minted: the first 401 is reauth
    h.fake.fail_next_on("/user", Fault::BadCredentials);
    h.env.clock.advance(1);
    assert_eq!(conn.get("/user").await.err(), Some(GhError::Unauthorized));
    assert_eq!(
        scalar(&h, "SELECT status FROM connections WHERE id = ?", &con),
        json!("reauth")
    );
}

/// The App's secrets go to the vault, so nothing is spent at GitHub while the vault can't take them.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_usable_vault_the_manifest_code_is_not_spent() {
    let h = setup().await;
    let me = caller(WS, ME);
    let v = start(&h, &me, json!({})).await;
    let state = v["state"].as_str().expect("state").to_string();
    // the vault becomes unusable between the form and GitHub's redirect back
    h.env
        .store()
        .with_conn(|c| Ok(c.execute("DELETE FROM vault_keys WHERE workspace_id = ?", [WS])?))
        .expect("drop keys");
    let (_, loc) = nav(
        &h.api,
        Some(&me),
        &format!(
            "/github/app/manifest/callback?code={}&state={state}",
            h.fake.manifest_code()
        ),
    )
    .await;
    assert_eq!(decode(&loc), steve_github::manifest::VAULT_NEEDED);
    assert!(
        loc.ends_with("&error_code=vault_needed"),
        "the web branches on the code, not the text: {loc}"
    );
    assert!(
        h.fake.calls_to("POST", "/app-manifests/").is_empty(),
        "the conversion was not called"
    );
    // and the form is refused up front
    let (s, v) = call(
        &h.api,
        Some(&me),
        "POST",
        "/github/app/manifest",
        Some(&json!({})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some(steve_github::manifest::VAULT_NEEDED))
    );
}

/// `STEVE_GITHUB_API_BASE` reaches the PAT adapter too: a form without `config.api_base` is
/// verified on the module's base (the fake), and the base is recorded on the connection.
#[tokio::test]
async fn personal_access_tokens_use_the_module_api_base() {
    let h = setup().await;
    let me = caller(WS, ME);
    let (s, c) = call(
        &h.api,
        Some(&me),
        "POST",
        "/connections",
        Some(&json!({"adapter": "github_pat", "fields": {"令牌": defaults::PAT}})),
    )
    .await;
    assert_eq!(s, 200, "{c}");
    let con = c["id"].as_str().expect("id").to_string();
    assert_eq!(
        scalar(
            &h,
            "SELECT json_extract(config, '$.api_base') FROM connections WHERE id = ?",
            &con
        ),
        json!(h.fake.url())
    );
    let me_json = h.gh.conn(WS, &con).expect("conn").get("/user").await;
    assert_eq!(me_json.expect("user")["login"], json!(defaults::USER));
}

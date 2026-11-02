//! Runs every endpoint of the fake (github.md §11) with plain reqwest, and
//! checks webhook signatures against an independent HMAC implementation.

use std::process::Command;
use std::sync::{Arc, Mutex};

use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_github::fake::{FIXTURES, FakeGitHub, Fault, defaults, fixture};
use steve_github::signature;

const HEAD: &str = "steve/st-12";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn jwt_with(claims: Value) -> String {
    let key =
        jsonwebtoken::EncodingKey::from_rsa_pem(defaults::APP_PRIVATE_KEY_PEM.as_bytes()).unwrap();
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &key,
    )
    .unwrap()
}

/// What github.md §1.1 step 4 prescribes: iat = now-60, exp = now+540.
fn app_jwt() -> String {
    jwt_with(json!({"iat": now() - 60, "exp": now() + 540, "iss": defaults::APP_ID}))
}

struct T {
    gh: FakeGitHub,
    http: Client,
}

impl T {
    async fn new() -> Self {
        T {
            gh: FakeGitHub::start().await.unwrap(),
            http: Client::new(),
        }
    }
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.gh.url())
    }
    fn req(&self, m: reqwest::Method, p: &str, auth: &str) -> reqwest::RequestBuilder {
        self.http
            .request(m, self.url(p))
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(header::USER_AGENT, "steve-hub")
            .bearer_auth(auth)
    }
    async fn get(&self, p: &str, auth: &str) -> (StatusCode, Value) {
        let r = self
            .req(reqwest::Method::GET, p, auth)
            .send()
            .await
            .unwrap();
        (r.status(), r.json().await.unwrap_or(Value::Null))
    }
    async fn send(
        &self,
        m: reqwest::Method,
        p: &str,
        auth: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let r = self.req(m, p, auth).json(&body).send().await.unwrap();
        (r.status(), r.json().await.unwrap_or(Value::Null))
    }
    async fn token(&self) -> String {
        let p = format!(
            "/app/installations/{}/access_tokens",
            defaults::INSTALLATION_ID
        );
        let (s, v) = self
            .send(reqwest::Method::POST, &p, &app_jwt(), json!({}))
            .await;
        assert_eq!(s, 201, "{v}");
        v["token"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn manifest_conversion_is_single_use_and_needs_no_auth() {
    let t = T::new().await;
    let code = t.gh.manifest_code();
    let p = format!("/app-manifests/{code}/conversions");
    let r = t.http.post(t.url(&p)).send().await.unwrap();
    assert_eq!(r.status(), 201);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["id"], defaults::APP_ID);
    assert_eq!(v["slug"], defaults::APP_SLUG);
    assert_eq!(v["webhook_secret"], defaults::WEBHOOK_SECRET);
    assert_eq!(v["client_secret"], defaults::CLIENT_SECRET);
    assert!(v["pem"].as_str().unwrap().contains("BEGIN RSA PRIVATE KEY"));
    assert_eq!(v["owner"]["login"], defaults::ORG);
    let again = t.http.post(t.url(&p)).send().await.unwrap();
    assert_eq!(again.status(), 404);
}

#[tokio::test]
async fn app_jwt_is_checked_and_exchanged_for_an_installation_token() {
    let t = T::new().await;
    let inst = format!("/app/installations/{}", defaults::INSTALLATION_ID);
    let (s, v) = t.get(&inst, &app_jwt()).await;
    assert_eq!(s, 200);
    assert_eq!(v["account"]["login"], defaults::ORG);
    assert_eq!(v["account"]["type"], "Organization");
    assert_eq!(v["app_id"], defaults::APP_ID);
    let (s, v) = t.get("/app", &app_jwt()).await;
    assert_eq!(
        (s, v["slug"].clone()),
        (StatusCode::OK, json!(defaults::APP_SLUG))
    );

    // iss as a string and as the client id are also accepted.
    let s_iss = jwt_with(
        json!({"iat": now() - 60, "exp": now() + 540, "iss": defaults::APP_ID.to_string()}),
    );
    assert_eq!(t.get(&inst, &s_iss).await.0, 200);
    let c_iss =
        jwt_with(json!({"iat": now() - 60, "exp": now() + 540, "iss": defaults::CLIENT_ID}));
    assert_eq!(t.get(&inst, &c_iss).await.0, 200);

    for bad in [
        jwt_with(json!({"iat": now() - 60, "exp": now() + 540, "iss": 999})),
        jwt_with(json!({"iat": now() - 60, "exp": now() + 3600, "iss": defaults::APP_ID})),
        jwt_with(json!({"iat": now() - 700, "exp": now() - 100, "iss": defaults::APP_ID})),
        jwt_with(json!({"iat": now() + 600, "exp": now() + 650, "iss": defaults::APP_ID})),
        // HS256 with some secret: wrong algorithm and key.
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &json!({"iat": now(), "exp": now() + 60, "iss": defaults::APP_ID}),
            &jsonwebtoken::EncodingKey::from_secret(b"x"),
        )
        .unwrap(),
    ] {
        let (s, v) = t.get(&inst, &bad).await;
        assert_eq!(s, 401, "{v}");
    }

    let p = format!("{inst}/access_tokens");
    let (s, v) = t
        .send(reqwest::Method::POST, &p, &app_jwt(), json!({}))
        .await;
    assert_eq!(s, 201);
    let token = v["token"].as_str().unwrap();
    assert!(token.starts_with("ghs_"));
    assert!(v["expires_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(t.gh.issued_tokens(), 1);
    // The token works on installation routes; JWTs don't.
    assert_eq!(t.get("/installation/repositories", token).await.0, 200);
    assert_eq!(
        t.get(&format!("/repos/{}", defaults::REPO), &app_jwt())
            .await
            .0,
        403
    );
    // Wrong installation id.
    let (s, _) = t
        .send(
            reqwest::Method::POST,
            "/app/installations/1/access_tokens",
            &app_jwt(),
            json!({}),
        )
        .await;
    assert_eq!(s, 404);
}

#[tokio::test]
async fn expired_and_revoked_tokens_get_401() {
    let t = T::new().await;
    let tok = t.token().await;
    assert_eq!(
        t.get(&format!("/repos/{}", defaults::REPO), &tok).await.0,
        200
    );
    t.gh.revoke_tokens();
    let (s, v) = t.get(&format!("/repos/{}", defaults::REPO), &tok).await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::UNAUTHORIZED, json!("Bad credentials"))
    );
    t.gh.set_token_ttl(0);
    let tok = t.token().await;
    assert_eq!(
        t.get(&format!("/repos/{}", defaults::REPO), &tok).await.0,
        401
    );
    // Injected 401 for a valid token.
    t.gh.set_token_ttl(3600);
    let tok = t.token().await;
    t.gh.fail_next_on("/repos/", Fault::BadCredentials);
    assert_eq!(
        t.get(&format!("/repos/{}", defaults::REPO), &tok).await.0,
        401
    );
    assert_eq!(
        t.get(&format!("/repos/{}", defaults::REPO), &tok).await.0,
        200
    );
    assert_eq!(
        t.get(&format!("/repos/{}", defaults::REPO), "nope").await.0,
        401
    );
}

#[tokio::test]
async fn repositories_users_and_link_pagination() {
    let t = T::new().await;
    for n in [
        "steve-test/a",
        "steve-test/b",
        "steve-test/c",
        "steve-test/d",
    ] {
        t.gh.add_repo(n, false, "main");
    }
    t.gh.add_installation(
        defaults::INSTALLATION_ID,
        defaults::ORG,
        "Organization",
        &[
            defaults::REPO,
            "steve-test/a",
            "steve-test/b",
            "steve-test/c",
            "steve-test/d",
        ],
    );
    let tok = t.token().await;

    let r = t
        .req(
            reqwest::Method::GET,
            "/installation/repositories?per_page=2",
            &tok,
        )
        .send()
        .await
        .unwrap();
    let link = r.headers()[header::LINK].to_str().unwrap().to_string();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["total_count"], 5);
    assert_eq!(v["repositories"].as_array().unwrap().len(), 2);
    assert!(link.contains("page=2>; rel=\"next\""), "{link}");
    assert!(link.contains("page=3>; rel=\"last\""), "{link}");
    assert!(link.contains("per_page=2"), "{link}");
    // Follow `next` to the end.
    let mut next = Some(link);
    let mut seen = 0;
    let mut pages = 0;
    let mut url = format!("{}/installation/repositories?per_page=2", t.gh.url());
    while let Some(l) = next.take() {
        let r = t.http.get(&url).bearer_auth(&tok).send().await.unwrap();
        let h = r
            .headers()
            .get(header::LINK)
            .map(|v| v.to_str().unwrap().to_string());
        let v: Value = r.json().await.unwrap();
        seen += v["repositories"].as_array().unwrap().len();
        pages += 1;
        if let Some(n) = h.as_deref().and_then(next_link) {
            url = n;
            next = Some(l);
        }
    }
    assert_eq!((seen, pages), (5, 3));

    let (s, v) = t.get(&format!("/repos/{}", defaults::REPO), &tok).await;
    assert_eq!(s, 200);
    assert_eq!(v["id"], defaults::REPO_ID);
    assert_eq!(v["default_branch"], "main");
    assert_eq!(v["private"], true);
    assert_eq!(t.get("/repos/steve-test/missing", &tok).await.0, 404);
    // Installation tokens can't call /user; PATs can.
    assert_eq!(t.get("/user", &tok).await.0, 403);
    let (s, v) = t.get("/user", defaults::PAT).await;
    assert_eq!(
        (s, v["login"].clone()),
        (StatusCode::OK, json!(defaults::USER))
    );
    let (s, v) = t
        .get("/user/repos?affiliation=owner,collaborator", defaults::PAT)
        .await;
    assert_eq!(s, 200);
    assert_eq!(v.as_array().unwrap().len(), 5);

    // Branches.
    let (s, v) = t
        .get(&format!("/repos/{}/branches", defaults::REPO), &tok)
        .await;
    assert_eq!(s, 200);
    assert_eq!(v[0]["name"], "main");
    assert_eq!(v[0]["commit"]["sha"], defaults::BASE_SHA);
    t.gh.set_branch(defaults::REPO, "feature/x", "abc");
    let (s, v) = t
        .get(
            &format!("/repos/{}/branches/feature/x", defaults::REPO),
            &tok,
        )
        .await;
    assert_eq!(
        (s, v["commit"]["sha"].clone()),
        (StatusCode::OK, json!("abc"))
    );
}

fn next_link(h: &str) -> Option<String> {
    h.split(", ")
        .find(|p| p.ends_with("rel=\"next\""))
        .map(|p| p[1..p.find('>').unwrap()].to_string())
}

fn git(dir: &std::path::Path, args: &[&str], rewrite: &(String, String)) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            &format!("{}={}", rewrite.0, rewrite.1),
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.test",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn pull_request_lifecycle_against_a_bare_remote() {
    let t = T::new().await;
    let tok = t.token().await;
    let repo = defaults::REPO;
    t.gh.bare_remote(repo).unwrap();
    let rw = t.gh.git_url_rewrite();

    // Before anything is pushed the head (and base) don't exist.
    let pulls = format!("/repos/{repo}/pulls");
    let body =
        json!({"title": "fix: preserve return_to", "head": HEAD, "base": "main", "body": "ST-12"});
    let (s, v) = t
        .send(reqwest::Method::POST, &pulls, &tok, body.clone())
        .await;
    assert_eq!(s, 422);
    assert_eq!(v["errors"][0]["field"], "head");

    // A real git push through the insteadOf rewrite.
    let work = tempfile::tempdir().unwrap();
    let w = work.path();
    git(w, &["init", "-q"], &rw);
    std::fs::write(w.join("a.txt"), "a").unwrap();
    git(w, &["add", "a.txt"], &rw);
    git(w, &["commit", "-qm", "base"], &rw);
    git(
        w,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{repo}.git"),
        ],
        &rw,
    );
    git(w, &["push", "-q", "origin", "main:main"], &rw);
    git(w, &["checkout", "-qb", HEAD], &rw);
    std::fs::write(w.join("a.txt"), "b").unwrap();
    git(w, &["commit", "-qam", "fix"], &rw);
    git(w, &["push", "-q", "origin", &format!("{HEAD}:{HEAD}")], &rw);
    let head_sha = git(w, &["rev-parse", "HEAD"], &rw);
    assert_eq!(t.gh.branches(repo).get(HEAD), Some(&head_sha));

    let (s, v) = t.send(reqwest::Method::POST, &pulls, &tok, json!({"title": "fix: preserve return_to", "head": format!("steve-test:{HEAD}"), "base": "main", "body": "ST-12", "draft": false})).await;
    assert_eq!(s, 201, "{v}");
    let n = v["number"].as_u64().unwrap();
    assert_eq!(v["head"]["sha"], head_sha.as_str());
    assert_eq!(v["user"]["login"], "steve-test[bot]");
    assert_eq!(v["user"]["type"], "Bot");
    assert_eq!(v["state"], "open");
    assert!(
        v["html_url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/{repo}/pull/{n}"))
    );
    // Same head again → 422 "already exists".
    let (s, v) = t.send(reqwest::Method::POST, &pulls, &tok, body).await;
    assert_eq!(s, 422);
    assert!(
        v["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );

    let (s, v) = t.get(&format!("{pulls}/{n}"), &tok).await;
    assert_eq!(
        (s, v["mergeable_state"].clone(), v["merged"].clone()),
        (StatusCode::OK, json!("clean"), json!(false))
    );
    let (_, v) = t
        .get(&format!("{pulls}?state=open&head=steve-test:{HEAD}"), &tok)
        .await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    let (_, v) = t.get(&format!("{pulls}?state=closed"), &tok).await;
    assert_eq!(v.as_array().unwrap().len(), 0);

    // Merge: wrong sha → 409, then the right one.
    let merge = format!("{pulls}/{n}/merge");
    let (s, v) = t
        .send(
            reqwest::Method::PUT,
            &merge,
            &tok,
            json!({"merge_method": "squash", "sha": "0".repeat(40)}),
        )
        .await;
    assert_eq!(s, 409, "{v}");
    let (s, v) = t
        .send(
            reqwest::Method::PUT,
            &merge,
            &tok,
            json!({"merge_method": "squash", "sha": head_sha}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["merged"], true);
    let pr = t.gh.pull(repo, n).unwrap();
    assert!(pr.merged && pr.state == "closed" && pr.merged_at.is_some());
    let (s, _) = t.send(reqwest::Method::PUT, &merge, &tok, json!({})).await;
    assert_eq!(s, 405);
    assert_eq!(
        t.gh.calls_to("PUT", &merge)[0].body["merge_method"],
        "squash"
    );

    // Delete the head branch (slashes in the ref).
    let del = format!("/repos/{repo}/git/refs/heads/{HEAD}");
    let r = t
        .req(reqwest::Method::DELETE, &del, &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(!t.gh.branches(repo).contains_key(HEAD));
    let r = t
        .req(reqwest::Method::DELETE, &del, &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}

#[tokio::test]
async fn issues_and_comments() {
    let t = T::new().await;
    let tok = t.token().await;
    let repo = defaults::REPO;
    let pr =
        t.gh.open_pr(
            repo,
            HEAD,
            "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
            "fix",
            "ST-12",
            defaults::USER,
        )
        .unwrap();
    let issue =
        t.gh.add_issue(repo, "Login loop on Safari", "steps…", &["bug"])
            .unwrap();

    let (s, v) = t
        .get(&format!("/repos/{repo}/issues?state=all"), &tok)
        .await;
    assert_eq!(s, 200);
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let as_issue = arr.iter().find(|i| i["number"] == issue).unwrap();
    assert_eq!(as_issue["labels"][0]["name"], "bug");
    assert!(as_issue.get("pull_request").is_none());
    let as_pr = arr.iter().find(|i| i["number"] == pr).unwrap();
    assert!(as_pr["pull_request"]["url"].is_string());
    let (_, v) = t
        .get(
            &format!("/repos/{repo}/issues?state=all&since=2999-01-01T00:00:00Z"),
            &tok,
        )
        .await;
    assert_eq!(v.as_array().unwrap().len(), 0);
    assert_eq!(
        t.get(&format!("/repos/{repo}/issues?since=yesterday"), &tok)
            .await
            .0,
        422
    );
    let (s, v) = t.get(&format!("/repos/{repo}/issues/{issue}"), &tok).await;
    assert_eq!(
        (s, v["title"].clone()),
        (StatusCode::OK, json!("Login loop on Safari"))
    );

    let comments = format!("/repos/{repo}/issues/{pr}/comments");
    let (s, v) = t
        .send(
            reqwest::Method::POST,
            &comments,
            &tok,
            json!({"body": "CI is green."}),
        )
        .await;
    assert_eq!(s, 201);
    assert_eq!(v["user"]["login"], "steve-test[bot]");
    assert!(v["html_url"].as_str().unwrap().contains("#issuecomment-"));
    let (s, _) = t
        .send(reqwest::Method::POST, &comments, &tok, json!({"body": ""}))
        .await;
    assert_eq!(s, 422);
    let (s, _) = t
        .send(
            reqwest::Method::POST,
            &format!("/repos/{repo}/issues/999/comments"),
            &tok,
            json!({"body": "x"}),
        )
        .await;
    assert_eq!(s, 404);
    let (_, v) = t.get(&comments, &tok).await;
    assert_eq!(v[0]["body"], "CI is green.");
    assert_eq!(t.gh.comments(repo, pr), vec!["CI is green."]);
}

#[tokio::test]
async fn checks_statuses_jobs_logs_and_reruns() {
    let t = T::new().await;
    let tok = t.token().await;
    let repo = defaults::REPO;
    let sha = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";
    t.gh.open_pr(repo, HEAD, sha, "fix", "", defaults::USER);
    let build = t.gh.set_check(repo, sha, "build", Some("failure"));
    let e2e = t.gh.set_check(repo, sha, "e2e", Some("success"));
    let ext = t.gh.set_external_check(repo, sha, "lint", Some("success"));
    t.gh.set_job_log(build, "step 1\n##[error]boom: token ghs_SECRET leaked\n");
    let run = t.gh.run_of(build).unwrap();
    assert_eq!(t.gh.run_of(e2e), Some(run));

    let (s, v) = t
        .get(&format!("/repos/{repo}/commits/{sha}/check-runs"), &tok)
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["total_count"], 3);
    let b = v["check_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "build")
        .unwrap()
        .clone();
    assert_eq!(
        (
            b["id"].as_u64(),
            b["status"].clone(),
            b["conclusion"].clone()
        ),
        (Some(build), json!("completed"), json!("failure"))
    );
    assert_eq!(b["app"]["slug"], "github-actions");
    assert!(
        b["details_url"]
            .as_str()
            .unwrap()
            .contains(&format!("/actions/runs/{run}/job/{build}"))
    );
    assert_eq!(b["pull_requests"][0]["head"]["ref"], HEAD);
    let (_, v) = t
        .get(
            &format!("/repos/{repo}/commits/{sha}/check-runs?check_name=e2e"),
            &tok,
        )
        .await;
    assert_eq!(v["total_count"], 1);

    let (_, v) = t
        .get(&format!("/repos/{repo}/commits/{sha}/check-suites"), &tok)
        .await;
    let suites = v["check_suites"].as_array().unwrap();
    assert_eq!(suites.len(), 2);
    let actions = suites
        .iter()
        .find(|s| s["app"]["slug"] == "github-actions")
        .unwrap();
    assert_eq!(
        (actions["status"].clone(), actions["conclusion"].clone()),
        (json!("completed"), json!("failure"))
    );
    assert_eq!(actions["head_branch"], HEAD);

    // Legacy statuses: empty → pending; failure wins; latest per context.
    let status = format!("/repos/{repo}/commits/{sha}/status");
    assert_eq!(t.get(&status, &tok).await.1["state"], "pending");
    t.gh.set_status(
        repo,
        sha,
        "ci/legacy",
        "failure",
        "https://ci.example.test/1",
    );
    assert_eq!(t.get(&status, &tok).await.1["state"], "failure");
    t.gh.set_status(
        repo,
        sha,
        "ci/legacy",
        "success",
        "https://ci.example.test/2",
    );
    let (_, v) = t.get(&status, &tok).await;
    assert_eq!(
        (v["state"].clone(), v["total_count"].clone()),
        (json!("success"), json!(1))
    );

    let (s, v) = t
        .get(&format!("/repos/{repo}/actions/runs/{run}/jobs"), &tok)
        .await;
    assert_eq!((s, v["total_count"].clone()), (StatusCode::OK, json!(2)));
    assert_eq!(
        t.get(&format!("/repos/{repo}/actions/runs/1/jobs"), &tok)
            .await
            .0,
        404
    );

    // Logs: 302 to an unauthenticated URL with the plain text.
    let no_redirect = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let logs = t.url(&format!("/repos/{repo}/actions/jobs/{build}/logs"));
    let r = no_redirect
        .get(&logs)
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 302);
    let loc = r.headers()[header::LOCATION].to_str().unwrap().to_string();
    let text = Client::new()
        .get(&loc)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("##[error]boom"));
    let followed = t.http.get(&logs).bearer_auth(&tok).send().await.unwrap();
    assert_eq!(followed.status(), 200);
    assert!(followed.text().await.unwrap().contains("ghs_SECRET"));
    assert_eq!(
        t.get(&format!("/repos/{repo}/actions/jobs/{ext}/logs"), &tok)
            .await
            .0,
        404
    );

    let (s, _) = t
        .send(
            reqwest::Method::POST,
            &format!("/repos/{repo}/actions/runs/{run}/rerun-failed-jobs"),
            &tok,
            json!({}),
        )
        .await;
    assert_eq!(s, 201);
    assert_eq!(t.gh.reruns(run), 1);
    let (_, v) = t
        .get(
            &format!("/repos/{repo}/commits/{sha}/check-runs?check_name=build"),
            &tok,
        )
        .await;
    assert_eq!(
        (
            v["check_runs"][0]["status"].clone(),
            v["check_runs"][0]["conclusion"].clone()
        ),
        (json!("queued"), Value::Null)
    );
}

#[tokio::test]
async fn repository_hooks_with_and_without_permission() {
    let t = T::new().await;
    let repo = defaults::REPO;
    let hooks = format!("/repos/{repo}/hooks");
    let body = json!({"name": "web", "active": true, "events": ["pull_request", "check_suite"],
                      "config": {"url": "https://steve.example.test/hooks/con_1", "content_type": "json", "secret": "s3cret"}});
    let (s, v) = t
        .send(reqwest::Method::POST, &hooks, defaults::PAT, body.clone())
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["config"]["secret"], "********");
    let id = v["id"].as_u64().unwrap();
    assert_eq!(t.gh.hooks(repo)[0].secret, "s3cret");
    let (_, v) = t.get(&hooks, defaults::PAT).await;
    assert_eq!(v[0]["events"], json!(["pull_request", "check_suite"]));
    let r = t
        .req(
            reqwest::Method::DELETE,
            &format!("{hooks}/{id}"),
            defaults::PAT,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(t.gh.hooks(repo).is_empty());
    t.gh.set_hooks_allowed(repo, false);
    assert_eq!(
        t.send(reqwest::Method::POST, &hooks, defaults::PAT, body)
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn etag_304_and_rate_limits() {
    let t = T::new().await;
    let tok = t.token().await;
    let pulls = format!(
        "/repos/{}/pulls?state=all&sort=updated&direction=desc&per_page=50",
        defaults::REPO
    );
    let r = t
        .req(reqwest::Method::GET, &pulls, &tok)
        .send()
        .await
        .unwrap();
    let etag = r.headers()[header::ETAG].to_str().unwrap().to_string();
    let remaining: u64 = r.headers()["x-ratelimit-remaining"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(r.headers()["x-ratelimit-limit"], "5000");
    let r = t
        .req(reqwest::Method::GET, &pulls, &tok)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304);
    assert_eq!(
        r.headers()["x-ratelimit-remaining"].to_str().unwrap(),
        remaining.to_string(),
        "304 is free"
    );
    assert_eq!(
        t.gh.calls().last().unwrap().if_none_match.as_deref(),
        Some(etag.as_str())
    );
    // A change invalidates the ETag.
    t.gh.open_pr(defaults::REPO, HEAD, "abc", "fix", "", defaults::USER);
    let r = t
        .req(reqwest::Method::GET, &pulls, &tok)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    t.gh.fail_next(Fault::RetryAfter {
        status: 429,
        seconds: 7,
    });
    let r = t
        .req(reqwest::Method::GET, &pulls, &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(
        (
            r.status().as_u16(),
            r.headers()[header::RETRY_AFTER].to_str().unwrap()
        ),
        (429, "7")
    );
    t.gh.fail_next(Fault::Status {
        status: 502,
        message: "Server Error".into(),
    });
    assert_eq!(t.get(&pulls, &tok).await.0, 502);

    t.gh.set_rate_remaining(0);
    let r = t
        .req(reqwest::Method::GET, &pulls, &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(r.headers()["x-ratelimit-remaining"], "0");
    assert!(r.headers().contains_key("x-ratelimit-reset"));
    assert!(r.text().await.unwrap().contains("rate limit"));
    // /rate_limit itself is not counted.
    let (s, v) = t.get("/rate_limit", &tok).await;
    assert_eq!(
        (s, v["resources"]["core"]["remaining"].clone()),
        (StatusCode::OK, json!(0))
    );
}

/// HMAC-SHA256 straight from RFC 2104, independent of the `hmac` crate.
fn rfc2104_hmac_sha256(key: &[u8], msg: &[u8]) -> String {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner = Sha256::new()
        .chain_update(k.map(|b| b ^ 0x36))
        .chain_update(msg)
        .finalize();
    let outer = Sha256::new()
        .chain_update(k.map(|b| b ^ 0x5c))
        .chain_update(inner)
        .finalize();
    format!(
        "sha256={}",
        outer.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

type Got = Vec<(axum::http::HeaderMap, Vec<u8>)>;

#[derive(Clone, Default)]
struct Received(Arc<Mutex<Got>>);

async fn receiver() -> (String, Received) {
    let got = Received::default();
    let g = got.clone();
    let app = axum::Router::new().route(
        "/hooks/github",
        axum::routing::post(move |h: axum::http::HeaderMap, body: axum::body::Bytes| {
            let g = g.clone();
            async move {
                let ok = h
                    .get("x-hub-signature-256")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| {
                        signature::verify(defaults::WEBHOOK_SECRET.as_bytes(), &body, s)
                    });
                g.0.lock().unwrap().push((h, body.to_vec()));
                if ok {
                    axum::http::StatusCode::ACCEPTED
                } else {
                    axum::http::StatusCode::UNAUTHORIZED
                }
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}/hooks/github"), got)
}

#[tokio::test]
async fn emitted_webhooks_are_signed_like_github() {
    let t = T::new().await;
    let (url, got) = receiver().await;
    t.gh.set_webhook_target(&url);

    for (name, event, _) in FIXTURES {
        let d = t.gh.emit_fixture(name).await.unwrap();
        assert_eq!(d.status, 202, "{name}");
        let (h, body) = got.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(h["x-github-event"], *event);
        assert_eq!(h["x-github-delivery"].to_str().unwrap(), d.id);
        assert_eq!(h["x-github-hook-installation-target-type"], "integration");
        assert_eq!(h["content-type"], "application/json");
        assert!(
            h["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("GitHub-Hookshot/")
        );
        let sig = h["x-hub-signature-256"].to_str().unwrap();
        assert_eq!(
            sig,
            rfc2104_hmac_sha256(defaults::WEBHOOK_SECRET.as_bytes(), &body),
            "{name}"
        );
        assert!(signature::verify(
            defaults::WEBHOOK_SECRET.as_bytes(),
            &body,
            sig
        ));
        assert!(!signature::verify(b"wrong", &body, sig));
    }

    // Redelivery: same id, same bytes, same signature.
    let first = t.gh.deliveries()[0].clone();
    let again = t.gh.redeliver(&first.id).await.unwrap();
    assert_eq!(
        (again.id.as_str(), &again.body, &again.signature),
        (first.id.as_str(), &first.body, &first.signature)
    );
    let (h, body) = got.0.lock().unwrap().last().cloned().unwrap();
    assert_eq!(h["x-github-delivery"].to_str().unwrap(), first.id);
    assert_eq!(body, first.body);

    // Custom payload; a long secret (> block size) goes through the hash path.
    let long = vec![b'k'; 100];
    let sender = steve_github::fake::WebhookSender::new(url.clone(), long.clone());
    let d = sender.send("ping", &json!({"zen": "x"})).await.unwrap();
    assert_eq!(d.status, 401, "receiver uses the default secret");
    assert_eq!(d.signature, rfc2104_hmac_sha256(&long, &d.body));
    let ok = t.gh.emit("ping", &json!({"zen": "y"})).await.unwrap();
    assert_eq!(ok.status, 202);
    let bad = sender
        .send_raw("ping", b"{}".to_vec(), "x".into(), Some("sha256=00".into()))
        .await
        .unwrap();
    assert_eq!(bad.status, 401);
}

#[tokio::test]
async fn emit_without_target_is_an_error() {
    let t = T::new().await;
    assert!(t.gh.emit("ping", &json!({})).await.is_err());
}

#[test]
fn fixtures_parse_and_agree_with_the_fake_defaults() {
    assert_eq!(FIXTURES.len(), 16);
    for (name, event, _) in FIXTURES {
        let (e, v) = fixture(name).unwrap();
        assert_eq!(e, *event);
        if *event != "ping" {
            assert_eq!(v["installation"]["id"], defaults::INSTALLATION_ID, "{name}");
        }
        if let Some(r) = v.get("repository") {
            assert_eq!(r["full_name"], defaults::REPO, "{name}");
            assert_eq!(r["id"], defaults::REPO_ID, "{name}");
        }
    }
    let (_, pr) = fixture("pull_request.closed.merged").unwrap();
    assert_eq!(
        (pr["action"].clone(), pr["pull_request"]["merged"].clone()),
        (json!("closed"), json!(true))
    );
    let (_, cs) = fixture("check_suite.completed.failure").unwrap();
    assert_eq!(cs["check_suite"]["conclusion"], "failure");
    assert_eq!(cs["check_suite"]["pull_requests"][0]["number"], 12);
    let (_, inst) = fixture("installation.created").unwrap();
    assert_eq!(inst["installation"]["app_id"], defaults::APP_ID);
    let (_, ping) = fixture("ping").unwrap();
    assert_eq!(ping["hook"]["app_id"], defaults::APP_ID);
    assert!(fixture("nope").is_err());
}

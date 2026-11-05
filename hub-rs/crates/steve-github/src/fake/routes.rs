//! REST handlers: the subset of api.github.com that Steve uses.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::model::{Comment, Hook, Issue, Pull, Repo, Token, World, app_permissions};
use super::time::{iso, now, parse_iso};

pub const DOCS: &str = "https://docs.github.com/rest";

#[derive(Clone)]
pub struct AppState {
    pub world: Arc<Mutex<World>>,
    pub jwt_key: Arc<DecodingKey>,
}

impl AppState {
    pub fn lock(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// GitHub's error shape: `{message, documentation_url, status}` (+ `errors`).
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    pub errors: Option<Value>,
}

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            message: message.into(),
            errors: None,
        }
    }
    fn not_found() -> Self {
        Self::new(404, "Not Found")
    }
    fn bad_credentials() -> Self {
        Self::new(401, "Bad credentials")
    }
    fn integration() -> Self {
        Self::new(403, "Resource not accessible by integration")
    }
    fn validation(errors: Value) -> Self {
        ApiError {
            errors: Some(errors),
            ..Self::new(422, "Validation Failed")
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"message": self.message, "documentation_url": DOCS, "status": self.status.as_u16().to_string()});
        if let Some(e) = self.errors {
            body["errors"] = e;
        }
        (self.status, axum::Json(body)).into_response()
    }
}

type R = Result<Response, ApiError>;

fn ok(status: u16, v: Value) -> R {
    let s = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    Ok((s, axum::Json(v)).into_response())
}

fn no_content() -> R {
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Who is calling, from the `Authorization` header.
#[derive(Debug, Clone, PartialEq)]
enum Principal {
    /// A valid App JWT.
    App,
    Installation(u64),
    User(String),
}

fn principal(s: &AppState, w: &World, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::new(401, "Requires authentication"))?;
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("token "))
        .ok_or_else(ApiError::bad_credentials)?
        .trim();
    if let Some(t) = w.tokens.get(token) {
        if t.revoked || t.expires_at <= now() {
            return Err(ApiError::bad_credentials());
        }
        return Ok(Principal::Installation(t.installation_id));
    }
    if let Some(login) = w.pats.get(token) {
        return Ok(Principal::User(login.clone()));
    }
    if token.matches('.').count() == 2 {
        return verify_jwt(s, w, token).map(|_| Principal::App);
    }
    Err(ApiError::bad_credentials())
}

/// Checks an App JWT the way GitHub documents it: RS256 with the App's key,
/// `iss` = App id (number or string) or client id, `exp` at most 10 minutes
/// out, `iat` not in the future (60 s drift allowed).
fn verify_jwt(s: &AppState, w: &World, token: &str) -> Result<(), ApiError> {
    let mut v = Validation::new(Algorithm::RS256);
    // jsonwebtoken only understands string `iss`; GitHub's is usually a
    // number, so `iss` and `iat` are checked by hand below.
    v.set_required_spec_claims(&["exp"]);
    v.leeway = 0;
    let data = jsonwebtoken::decode::<Value>(token, &s.jwt_key, &v)
        .map_err(|e| ApiError::new(401, format!("A JSON web token could not be decoded ({e})")))?;
    let c = data.claims;
    let iss_ok = match &c["iss"] {
        Value::Number(n) => n.as_u64() == Some(w.app.id),
        Value::String(s) => *s == w.app.id.to_string() || *s == w.app.client_id,
        _ => false,
    };
    if !iss_ok {
        return Err(ApiError::new(
            401,
            "'Issuer' claim ('iss') must be an Integer",
        ));
    }
    let (iat, exp, t) = (
        c["iat"].as_i64().unwrap_or(i64::MAX),
        c["exp"].as_i64().unwrap_or(0),
        now(),
    );
    if iat > t + 60 {
        return Err(ApiError::new(
            401,
            "'Issued at' claim ('iat') must be an Integer representing a time in the past",
        ));
    }
    if exp > t + 600 + 60 {
        return Err(ApiError::new(
            401,
            "'Expiration time' claim ('exp') is too far in the future",
        ));
    }
    Ok(())
}

fn login_of(w: &World, p: &Principal) -> String {
    match p {
        Principal::User(l) => l.clone(),
        _ => format!("{}[bot]", w.app.slug),
    }
}

/// The repo, if the caller can see it (404 otherwise, like github.com).
fn repo<'a>(w: &'a World, p: &Principal, owner: &str, name: &str) -> Result<&'a Repo, ApiError> {
    let full = format!("{owner}/{name}");
    let r = w.repos.get(&full).ok_or_else(ApiError::not_found)?;
    match p {
        Principal::App => Err(ApiError::integration()),
        Principal::Installation(id) => {
            let inst = w.installations.get(id).ok_or_else(ApiError::not_found)?;
            if inst.repos.contains(&full) {
                Ok(r)
            } else {
                Err(ApiError::not_found())
            }
        }
        Principal::User(_) => Ok(r),
    }
}

/// Paginates `items` with `per_page` (default 30, max 100) and `page`, and
/// sets a `Link` header with `first/prev/next/last` like GitHub.
fn paginate(
    w: &World,
    path: &str,
    q: &HashMap<String, String>,
    items: Vec<Value>,
    wrap: Option<(&str, Value)>,
) -> R {
    let per = q
        .get("per_page")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(30)
        .clamp(1, 100);
    let page = q
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let total = items.len();
    let last = total.div_ceil(per).max(1);
    let slice: Vec<Value> = items.into_iter().skip((page - 1) * per).take(per).collect();
    let link_for = |p: usize, rel: &str| {
        let mut pairs: Vec<(String, String)> = q
            .iter()
            .filter(|(k, _)| k.as_str() != "page")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        pairs.sort();
        pairs.push(("page".into(), p.to_string()));
        let qs: Vec<String> = pairs
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencode(v)))
            .collect();
        format!("<{}{path}?{}>; rel=\"{rel}\"", w.base_url, qs.join("&"))
    };
    let mut links = Vec::new();
    if page > 1 {
        links.push(link_for(page - 1, "prev"));
    }
    if page < last {
        links.push(link_for(page + 1, "next"));
        links.push(link_for(last, "last"));
    }
    if page > 1 {
        links.push(link_for(1, "first"));
    }
    let body = match wrap {
        Some((key, mut base)) => {
            base["total_count"] = json!(total);
            base[key] = Value::Array(slice);
            base
        }
        None => Value::Array(slice),
    };
    let mut resp = (StatusCode::OK, axum::Json(body)).into_response();
    if !links.is_empty()
        && let Ok(v) = HeaderValue::from_str(&links.join(", "))
    {
        resp.headers_mut().insert(header::LINK, v);
    }
    Ok(resp)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn fake_sha(seed: &str) -> String {
    hex::encode(Sha256::digest(seed.as_bytes()))[..40].to_string()
}

type Q = Query<HashMap<String, String>>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/app-manifests/{code}/conversions",
            post(manifest_conversion),
        )
        .route("/app", get(get_app))
        .route("/app/installations/{id}", get(get_installation))
        .route("/app/installations/{id}/access_tokens", post(create_token))
        .route("/installation/repositories", get(installation_repos))
        .route("/user", get(get_user))
        .route("/user/repos", get(user_repos))
        .route("/rate_limit", get(rate_limit))
        .route("/repos/{o}/{r}", get(get_repo))
        .route("/repos/{o}/{r}/branches", get(list_branches))
        .route("/repos/{o}/{r}/branches/{*b}", get(get_branch))
        .route("/repos/{o}/{r}/pulls", get(list_pulls).post(create_pull))
        .route("/repos/{o}/{r}/pulls/{n}", get(get_pull))
        .route("/repos/{o}/{r}/pulls/{n}/merge", put(merge_pull))
        .route("/repos/{o}/{r}/git/refs/heads/{*b}", delete(delete_ref))
        .route("/repos/{o}/{r}/git/ref/heads/{*b}", get(get_ref))
        .route("/repos/{o}/{r}/issues", get(list_issues))
        .route("/repos/{o}/{r}/issues/{n}", get(get_issue))
        .route(
            "/repos/{o}/{r}/issues/{n}/comments",
            get(list_comments).post(create_comment),
        )
        .route("/repos/{o}/{r}/commits/{sha}/check-runs", get(check_runs))
        .route(
            "/repos/{o}/{r}/commits/{sha}/check-suites",
            get(check_suites),
        )
        .route("/repos/{o}/{r}/commits/{sha}/status", get(combined_status))
        .route("/repos/{o}/{r}/actions/runs/{id}/jobs", get(run_jobs))
        .route(
            "/repos/{o}/{r}/actions/runs/{id}/rerun-failed-jobs",
            post(rerun_failed),
        )
        .route("/repos/{o}/{r}/actions/jobs/{id}/logs", get(job_logs))
        .route("/repos/{o}/{r}/hooks", get(list_hooks).post(create_hook))
        .route("/repos/{o}/{r}/hooks/{id}", delete(delete_hook))
        .route("/_fake/logs/{id}", get(raw_log))
        .fallback(|| async { ApiError::not_found() })
        .with_state(state)
}

// ---- App ------------------------------------------------------------------

async fn manifest_conversion(State(s): State<AppState>, Path(code): Path<String>) -> R {
    let mut w = s.lock();
    let Some(pos) = w.manifest_codes.iter().position(|c| *c == code) else {
        return Err(ApiError::not_found());
    };
    w.manifest_codes.remove(pos);
    let mut v = w.app_json();
    v["client_secret"] = json!(w.app.client_secret);
    v["webhook_secret"] = json!(w.app.webhook_secret);
    v["pem"] = json!(w.app.pem);
    ok(201, v)
}

async fn get_app(State(s): State<AppState>, h: HeaderMap) -> R {
    let w = s.lock();
    if principal(&s, &w, &h)? != Principal::App {
        return Err(ApiError::new(401, "A JSON web token could not be decoded"));
    }
    ok(200, w.app_json())
}

async fn get_installation(State(s): State<AppState>, h: HeaderMap, Path(id): Path<u64>) -> R {
    let w = s.lock();
    if principal(&s, &w, &h)? != Principal::App {
        return Err(ApiError::new(401, "A JSON web token could not be decoded"));
    }
    let i = w.installations.get(&id).ok_or_else(ApiError::not_found)?;
    ok(200, w.installation_json(i))
}

async fn create_token(State(s): State<AppState>, h: HeaderMap, Path(id): Path<u64>) -> R {
    let mut w = s.lock();
    if principal(&s, &w, &h)? != Principal::App {
        return Err(ApiError::new(401, "A JSON web token could not be decoded"));
    }
    if !w.installations.contains_key(&id) {
        return Err(ApiError::not_found());
    }
    let rand: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(36)
        .map(char::from)
        .collect();
    let token = format!("ghs_{rand}");
    let expires_at = now() + w.token_ttl;
    w.tokens.insert(
        token.clone(),
        Token {
            installation_id: id,
            expires_at,
            revoked: false,
        },
    );
    ok(
        201,
        json!({"token": token, "expires_at": iso(expires_at), "permissions": app_permissions(), "repository_selection": "selected"}),
    )
}

async fn installation_repos(State(s): State<AppState>, h: HeaderMap, Query(q): Q) -> R {
    let w = s.lock();
    let Principal::Installation(id) = principal(&s, &w, &h)? else {
        return Err(ApiError::new(
            403,
            "This endpoint requires an installation access token",
        ));
    };
    let inst = w.installations.get(&id).ok_or_else(ApiError::not_found)?;
    let items = inst
        .repos
        .iter()
        .filter_map(|n| w.repos.get(n))
        .map(|r| w.repo_json(r))
        .collect();
    paginate(
        &w,
        "/installation/repositories",
        &q,
        items,
        Some(("repositories", json!({"repository_selection": "selected"}))),
    )
}

// ---- User -----------------------------------------------------------------

async fn get_user(State(s): State<AppState>, h: HeaderMap) -> R {
    let w = s.lock();
    let Principal::User(login) = principal(&s, &w, &h)? else {
        return Err(ApiError::integration());
    };
    ok(
        200,
        json!({"login": login, "id": super::model::login_id(&login), "type": "User",
               "html_url": format!("https://github.com/{login}"), "name": login, "site_admin": false}),
    )
}

async fn user_repos(State(s): State<AppState>, h: HeaderMap, Query(q): Q) -> R {
    let w = s.lock();
    let Principal::User(_) = principal(&s, &w, &h)? else {
        return Err(ApiError::integration());
    };
    let items = w.repos.values().map(|r| w.repo_json(r)).collect();
    paginate(&w, "/user/repos", &q, items, None)
}

async fn rate_limit(State(s): State<AppState>) -> R {
    let w = s.lock();
    let r = &w.rate;
    let core = json!({"limit": r.limit, "remaining": r.remaining, "reset": r.reset, "used": r.limit - r.remaining, "resource": "core"});
    ok(200, json!({"resources": {"core": core}, "rate": core}))
}

// ---- Repos and branches ---------------------------------------------------

async fn get_repo(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    ok(200, w.repo_json(repo(&w, &p, &o, &r)?))
}

fn branch_json(w: &World, full: &str, name: &str, sha: &str) -> Value {
    json!({"name": name, "commit": {"sha": sha, "url": w.api(&format!("/repos/{full}/commits/{sha}"))}, "protected": false})
}

async fn list_branches(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let items = w
        .branches(&full)
        .iter()
        .map(|(b, sha)| branch_json(&w, &full, b, sha))
        .collect();
    paginate(&w, &format!("/repos/{full}/branches"), &q, items, None)
}

async fn get_branch(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, b)): Path<(String, String, String)>,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let sha = w
        .branches(&full)
        .get(&b)
        .cloned()
        .ok_or_else(|| ApiError::new(404, "Branch not found"))?;
    ok(200, branch_json(&w, &full, &b, &sha))
}

/// `GET /repos/{o}/{r}/git/ref/heads/{branch}`: one branch and its commit.
async fn get_ref(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, b)): Path<(String, String, String)>,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let Some(sha) = w.branches(&full).get(&b).cloned() else {
        return Err(ApiError::not_found());
    };
    ok(
        200,
        json!({"ref": format!("refs/heads/{b}"), "object": {"type": "commit", "sha": sha}}),
    )
}

async fn delete_ref(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, b)): Path<(String, String, String)>,
) -> R {
    let mut w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    if !w.branches(&full).contains_key(&b) {
        return Err(ApiError::new(422, "Reference does not exist"));
    }
    if w.bare.contains(&full) {
        let path = super::git::bare_path(&w.git_root, &full);
        super::git::delete_branch(&path, &b).map_err(|e| ApiError::new(500, e.to_string()))?;
    } else if let Some(repo) = w.repos.get_mut(&full) {
        repo.branches.remove(&b);
    }
    no_content()
}

// ---- Pull requests --------------------------------------------------------

fn find_pull<'a>(w: &'a World, full: &str, n: u64) -> Result<&'a Pull, ApiError> {
    w.pulls
        .iter()
        .find(|p| p.repo == full && p.number == n)
        .ok_or_else(ApiError::not_found)
}

async fn list_pulls(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let state = q.get("state").map(String::as_str).unwrap_or("open");
    let mut pulls: Vec<&Pull> = w
        .pulls
        .iter()
        .filter(|p| p.repo == full)
        .filter(|p| state == "all" || p.state == state)
        .filter(|p| {
            q.get("head")
                .is_none_or(|hd| *hd == format!("{o}:{}", p.head_ref))
        })
        .filter(|p| q.get("base").is_none_or(|b| *b == p.base_ref))
        .collect();
    let by_updated = q.get("sort").map(String::as_str) == Some("updated");
    pulls.sort_by_key(|p| {
        if by_updated {
            (p.updated, p.number)
        } else {
            (p.created, p.number)
        }
    });
    if q.get("direction").map(String::as_str) != Some("asc") {
        pulls.reverse();
    }
    let items = pulls.into_iter().map(|p| w.pull_json(p)).collect();
    paginate(&w, &format!("/repos/{full}/pulls"), &q, items, None)
}

async fn get_pull(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, n)): Path<(String, String, u64)>,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    ok(200, w.pull_json(find_pull(&w, &full, n)?))
}

async fn create_pull(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
    axum::Json(body): axum::Json<Value>,
) -> R {
    let mut w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let field = |k: &str| body[k].as_str().map(str::to_string);
    let missing = |f: &str| {
        ApiError::validation(
            json!([{"resource": "PullRequest", "field": f, "code": "missing_field"}]),
        )
    };
    let title = field("title")
        .filter(|t| !t.is_empty())
        .ok_or_else(|| missing("title"))?;
    let head_raw = field("head").ok_or_else(|| missing("head"))?;
    let base = field("base").ok_or_else(|| missing("base"))?;
    // `head` may be `owner:branch`.
    let head = head_raw
        .split_once(':')
        .map(|(_, b)| b.to_string())
        .unwrap_or(head_raw);
    let branches = w.branches(&full);
    let invalid = |f: &str| {
        ApiError::validation(json!([{"resource": "PullRequest", "field": f, "code": "invalid"}]))
    };
    let head_sha = branches
        .get(&head)
        .cloned()
        .ok_or_else(|| invalid("head"))?;
    let base_sha = branches
        .get(&base)
        .cloned()
        .ok_or_else(|| invalid("base"))?;
    if w.pulls
        .iter()
        .any(|x| x.repo == full && x.head_ref == head && x.state == "open")
    {
        return Err(ApiError::validation(
            json!([{"resource": "PullRequest", "code": "custom",
            "message": format!("A pull request already exists for {o}:{head}.")}]),
        ));
    }
    let user = login_of(&w, &p);
    let id = w.id();
    let t = now();
    let Some(repo) = w.repos.get_mut(&full) else {
        return Err(ApiError::not_found());
    };
    let number = repo.next_number;
    repo.next_number += 1;
    let pull = Pull {
        repo: full,
        number,
        id,
        title,
        body: field("body").unwrap_or_default(),
        state: "open".into(),
        draft: body["draft"].as_bool().unwrap_or(false),
        merged: false,
        head_ref: head,
        head_sha,
        base_ref: base,
        base_sha,
        user,
        mergeable_state: "clean".into(),
        merge_commit_sha: None,
        created: t,
        updated: t,
        closed_at: None,
        merged_at: None,
    };
    let v = w.pull_json(&pull);
    w.pulls.push(pull);
    ok(201, v)
}

async fn merge_pull(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, n)): Path<(String, String, u64)>,
    body: Option<axum::Json<Value>>,
) -> R {
    let body = body.map(|b| b.0).unwrap_or(Value::Null);
    let mut w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let pull = find_pull(&w, &full, n)?.clone();
    let method = body["merge_method"].as_str().unwrap_or("merge");
    if !matches!(method, "merge" | "squash" | "rebase") {
        return Err(ApiError::validation(
            json!([{"resource": "PullRequest", "field": "merge_method", "code": "invalid"}]),
        ));
    }
    if pull.state != "open" || pull.merged || pull.mergeable_state == "dirty" || pull.draft {
        return Err(ApiError::new(405, "Pull Request is not mergeable"));
    }
    if let Some(sha) = body["sha"].as_str()
        && sha != pull.head_sha
    {
        return Err(ApiError::new(
            409,
            "Head branch was modified. Review and try the merge again.",
        ));
    }
    // Bare remotes can't run a real merge here; fast-forward the base instead.
    let merge_sha = if w.bare.contains(&full) {
        let path = super::git::bare_path(&w.git_root, &full);
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .args([
                "update-ref",
                &format!("refs/heads/{}", pull.base_ref),
                &pull.head_sha,
            ])
            .status();
        pull.head_sha.clone()
    } else {
        let sha = fake_sha(&format!("merge:{full}:{n}:{}", pull.head_sha));
        if let Some(repo) = w.repos.get_mut(&full) {
            repo.branches.insert(pull.base_ref.clone(), sha.clone());
        }
        sha
    };
    let t = now();
    if let Some(x) = w.pulls.iter_mut().find(|x| x.repo == full && x.number == n) {
        x.state = "closed".into();
        x.merged = true;
        x.merged_at = Some(t);
        x.closed_at = Some(t);
        x.updated = t;
        x.merge_commit_sha = Some(merge_sha.clone());
    }
    ok(
        200,
        json!({"sha": merge_sha, "merged": true, "message": "Pull Request successfully merged"}),
    )
}

// ---- Issues and comments --------------------------------------------------

async fn list_issues(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let state = q.get("state").map(String::as_str).unwrap_or("open");
    let since = match q.get("since") {
        Some(v) => Some(
            parse_iso(v)
                .ok_or_else(|| ApiError::new(422, "Invalid value for parameter 'since'"))?,
        ),
        None => None,
    };
    let keep = |st: &str, updated: i64| {
        (state == "all" || st == state) && since.is_none_or(|t| updated >= t)
    };
    let mut rows: Vec<(i64, i64, Value)> = Vec::new();
    for i in w
        .issues
        .iter()
        .filter(|i| i.repo == full && keep(&i.state, i.updated))
    {
        rows.push((i.created, i.updated, w.issue_json(i)));
    }
    for pr in w
        .pulls
        .iter()
        .filter(|p| p.repo == full && keep(&p.state, p.updated))
    {
        rows.push((pr.created, pr.updated, w.pull_as_issue_json(pr)));
    }
    let by_updated = q.get("sort").map(String::as_str) == Some("updated");
    rows.sort_by_key(|(c, u, v)| (if by_updated { *u } else { *c }, v["number"].as_u64()));
    if q.get("direction").map(String::as_str) != Some("asc") {
        rows.reverse();
    }
    paginate(
        &w,
        &format!("/repos/{full}/issues"),
        &q,
        rows.into_iter().map(|r| r.2).collect(),
        None,
    )
}

async fn get_issue(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, n)): Path<(String, String, u64)>,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    if let Some(i) = w.issues.iter().find(|i| i.repo == full && i.number == n) {
        return ok(200, w.issue_json(i));
    }
    ok(200, w.pull_as_issue_json(find_pull(&w, &full, n)?))
}

fn issue_exists(w: &World, full: &str, n: u64) -> bool {
    w.issues.iter().any(|i| i.repo == full && i.number == n)
        || w.pulls.iter().any(|p| p.repo == full && p.number == n)
}

async fn list_comments(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, n)): Path<(String, String, u64)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    if !issue_exists(&w, &full, n) {
        return Err(ApiError::not_found());
    }
    let items = w
        .comments
        .iter()
        .filter(|c| c.repo == full && c.issue_number == n)
        .map(|c| w.comment_json(c))
        .collect();
    paginate(
        &w,
        &format!("/repos/{full}/issues/{n}/comments"),
        &q,
        items,
        None,
    )
}

async fn create_comment(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, n)): Path<(String, String, u64)>,
    axum::Json(body): axum::Json<Value>,
) -> R {
    let mut w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    if !issue_exists(&w, &full, n) {
        return Err(ApiError::not_found());
    }
    let text = body["body"]
        .as_str()
        .filter(|b| !b.is_empty())
        .ok_or_else(|| {
            ApiError::validation(
                json!([{"resource": "IssueComment", "field": "body", "code": "missing_field"}]),
            )
        })?;
    let c = Comment {
        id: w.id(),
        repo: full,
        issue_number: n,
        body: text.to_string(),
        user: login_of(&w, &p),
        created: now(),
    };
    let v = w.comment_json(&c);
    w.comments.push(c);
    ok(201, v)
}

// ---- Checks, statuses, Actions --------------------------------------------

async fn check_runs(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, sha)): Path<(String, String, String)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let items = w
        .check_runs
        .iter()
        .filter(|c| c.repo == full && c.head_sha == sha)
        .filter(|c| q.get("check_name").is_none_or(|n| *n == c.name))
        .filter(|c| q.get("status").is_none_or(|st| *st == c.status))
        .map(|c| w.check_run_json(c))
        .collect();
    paginate(
        &w,
        &format!("/repos/{full}/commits/{sha}/check-runs"),
        &q,
        items,
        Some(("check_runs", json!({}))),
    )
}

async fn check_suites(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, sha)): Path<(String, String, String)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let items = w.check_suites_json(&full, &sha);
    paginate(
        &w,
        &format!("/repos/{full}/commits/{sha}/check-suites"),
        &q,
        items,
        Some(("check_suites", json!({}))),
    )
}

async fn combined_status(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, sha)): Path<(String, String, String)>,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let rp = repo(&w, &p, &o, &r)?;
    let full = rp.full_name();
    // Latest status per context.
    let mut latest: Vec<&super::model::CommitStatus> = Vec::new();
    for st in w
        .statuses
        .iter()
        .filter(|st| st.repo == full && st.sha == sha)
        .rev()
    {
        if !latest.iter().any(|x| x.context == st.context) {
            latest.push(st);
        }
    }
    let state = if latest
        .iter()
        .any(|x| x.state == "error" || x.state == "failure")
    {
        "failure"
    } else if latest.is_empty() || latest.iter().any(|x| x.state == "pending") {
        "pending"
    } else {
        "success"
    };
    ok(
        200,
        json!({"state": state, "sha": sha, "total_count": latest.len(),
               "statuses": latest.iter().map(|x| w.status_json(x)).collect::<Vec<_>>(),
               "repository": w.repo_json(rp),
               "commit_url": w.api(&format!("/repos/{full}/commits/{sha}")),
               "url": w.api(&format!("/repos/{full}/commits/{sha}/status"))}),
    )
}

async fn run_jobs(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, id)): Path<(String, String, u64)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let jobs: Vec<Value> = w
        .check_runs
        .iter()
        .filter(|c| c.repo == full && c.run_id == Some(id))
        .map(|c| w.job_json(c))
        .collect();
    if jobs.is_empty() {
        return Err(ApiError::not_found());
    }
    paginate(
        &w,
        &format!("/repos/{full}/actions/runs/{id}/jobs"),
        &q,
        jobs,
        Some(("jobs", json!({}))),
    )
}

async fn rerun_failed(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, id)): Path<(String, String, u64)>,
) -> R {
    let mut w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let mut found = false;
    for c in w
        .check_runs
        .iter_mut()
        .filter(|c| c.repo == full && c.run_id == Some(id))
    {
        found = true;
        if matches!(
            c.conclusion.as_deref(),
            Some("failure" | "cancelled" | "timed_out")
        ) {
            c.status = "queued".into();
            c.conclusion = None;
            c.completed = None;
        }
    }
    if !found {
        return Err(ApiError::not_found());
    }
    *w.reruns.entry(id).or_default() += 1;
    ok(201, json!({}))
}

async fn job_logs(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, id)): Path<(String, String, u64)>,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let is_job = w
        .check_runs
        .iter()
        .any(|c| c.repo == full && c.id == id && c.run_id.is_some());
    if !is_job || !w.jobs.contains_key(&id) {
        return Err(ApiError::not_found());
    }
    // Real logs redirect to a short-lived signed blob URL that needs no auth.
    let location = format!(
        "{}/_fake/logs/{id}?sig={}",
        w.base_url,
        &fake_sha(&format!("log:{id}"))[..16]
    );
    let mut resp = StatusCode::FOUND.into_response();
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    Ok(resp)
}

async fn raw_log(State(s): State<AppState>, Path(id): Path<u64>) -> R {
    let w = s.lock();
    let job = w.jobs.get(&id).ok_or_else(ApiError::not_found)?;
    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        job.log.clone(),
    )
        .into_response())
}

// ---- Repository hooks -----------------------------------------------------

async fn list_hooks(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
    Query(q): Q,
) -> R {
    let w = s.lock();
    let p = principal(&s, &w, &h)?;
    let rp = repo(&w, &p, &o, &r)?;
    if !rp.hooks_allowed {
        return Err(ApiError::not_found());
    }
    let full = rp.full_name();
    let items = w
        .hooks
        .iter()
        .filter(|x| x.repo == full)
        .map(|x| w.hook_json(x))
        .collect();
    paginate(&w, &format!("/repos/{full}/hooks"), &q, items, None)
}

async fn create_hook(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
    axum::Json(body): axum::Json<Value>,
) -> R {
    let mut w = s.lock();
    let p = principal(&s, &w, &h)?;
    let rp = repo(&w, &p, &o, &r)?;
    // Without `Webhooks: write` github.com answers 404.
    if !rp.hooks_allowed {
        return Err(ApiError::not_found());
    }
    let full = rp.full_name();
    let cfg = &body["config"];
    let url = cfg["url"]
        .as_str()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            ApiError::validation(
                json!([{"resource": "Hook", "field": "config.url", "code": "missing_field"}]),
            )
        })?;
    let events = match body["events"].as_array() {
        Some(a) => a
            .iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect(),
        None => vec!["push".to_string()],
    };
    let hook = Hook {
        id: w.id(),
        repo: full,
        url: url.to_string(),
        content_type: cfg["content_type"].as_str().unwrap_or("form").to_string(),
        secret: cfg["secret"].as_str().unwrap_or_default().to_string(),
        events,
        active: body["active"].as_bool().unwrap_or(true),
    };
    let v = w.hook_json(&hook);
    w.hooks.push(hook);
    ok(201, v)
}

async fn delete_hook(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((o, r, id)): Path<(String, String, u64)>,
) -> R {
    let mut w = s.lock();
    let p = principal(&s, &w, &h)?;
    let full = repo(&w, &p, &o, &r)?.full_name();
    let before = w.hooks.len();
    w.hooks.retain(|x| !(x.repo == full && x.id == id));
    if w.hooks.len() == before {
        return Err(ApiError::not_found());
    }
    no_content()
}

/// Keeps `Issue` constructible from outside without exposing the router.
pub fn new_issue(
    w: &mut World,
    repo: &str,
    title: &str,
    body: &str,
    labels: &[&str],
    user: &str,
) -> Option<u64> {
    let id = w.id();
    let t = now();
    let r = w.repos.get_mut(repo)?;
    let number = r.next_number;
    r.next_number += 1;
    w.issues.push(Issue {
        repo: repo.to_string(),
        number,
        id,
        title: title.to_string(),
        body: body.to_string(),
        state: "open".into(),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        user: user.to_string(),
        created: t,
        updated: t,
    });
    Some(number)
}

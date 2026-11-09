//! The fake's in-memory world and how each object renders as GitHub JSON.
//!
//! Field names follow the REST reference (docs.github.com/en/rest); only the
//! fields Steve reads plus the identifying ones are rendered.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde_json::{Value, json};

use super::time::iso;

/// The GitHub App the fake pretends to be registered.
#[derive(Debug, Clone)]
pub struct App {
    pub id: u64,
    pub slug: String,
    pub name: String,
    pub owner_login: String,
    pub client_id: String,
    pub client_secret: String,
    pub webhook_secret: String,
    /// PKCS#1 private key handed out by the manifest conversion.
    pub pem: String,
}

#[derive(Debug, Clone)]
pub struct Installation {
    pub id: u64,
    pub account_login: String,
    pub account_id: u64,
    /// `Organization` or `User`.
    pub account_type: String,
    /// Full names of the repositories the installation can see.
    pub repos: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub installation_id: u64,
    pub expires_at: i64,
    pub revoked: bool,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub id: u64,
    pub owner: String,
    pub name: String,
    pub private: bool,
    pub default_branch: String,
    /// Branch → head sha. Used when the repo has no bare git remote.
    pub branches: BTreeMap<String, String>,
    /// Whether the caller may create repository hooks (PAT without
    /// `Webhooks: write` gets 404 like on github.com).
    pub hooks_allowed: bool,
    pub next_number: u64,
}

impl Repo {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone)]
pub struct Pull {
    pub repo: String,
    pub number: u64,
    pub id: u64,
    pub title: String,
    pub body: String,
    /// `open` or `closed`.
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
    pub user: String,
    /// `clean`, `dirty`, `blocked`, `unknown`… (`mergeable_state`).
    pub mergeable_state: String,
    pub merge_commit_sha: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub closed_at: Option<i64>,
    pub merged_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub repo: String,
    pub number: u64,
    pub id: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
    pub user: String,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub id: u64,
    pub repo: String,
    pub issue_number: u64,
    pub body: String,
    pub user: String,
    pub created: i64,
}

/// A check run. Actions runs carry the workflow run and job (same id as the
/// check run, as on github.com).
#[derive(Debug, Clone)]
pub struct CheckRun {
    pub id: u64,
    pub repo: String,
    pub head_sha: String,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub suite_id: u64,
    /// `None` for checks from other apps (no job, no logs).
    pub run_id: Option<u64>,
    pub started: i64,
    pub completed: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct CommitStatus {
    pub id: u64,
    pub repo: String,
    pub sha: String,
    pub context: String,
    /// `error`, `failure`, `pending`, `success`.
    pub state: String,
    pub target_url: String,
    pub description: String,
    pub updated: i64,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub log: String,
}

#[derive(Debug, Clone)]
pub struct Hook {
    pub id: u64,
    pub repo: String,
    pub url: String,
    pub content_type: String,
    pub secret: String,
    pub events: Vec<String>,
    pub active: bool,
}

/// One request the fake received, for assertions.
#[derive(Debug, Clone)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub query: String,
    /// Raw `Authorization` header, if any.
    pub authorization: Option<String>,
    pub if_none_match: Option<String>,
    /// Parsed JSON body (`Null` when absent or not JSON).
    pub body: Value,
    /// Status the fake answered with.
    pub status: u16,
}

/// An error to inject into the next matching request.
#[derive(Debug, Clone)]
pub enum Fault {
    /// Primary rate limit: 403 with `x-ratelimit-remaining: 0`.
    RateLimited,
    /// Secondary rate limit: 403 or 429 with `retry-after`.
    RetryAfter { status: u16, seconds: u64 },
    /// 401 `Bad credentials`, as for an expired or revoked token.
    BadCredentials,
    /// Any status with a GitHub-style `{message}` body.
    Status { status: u16, message: String },
    /// The request is carried out, but its answer is lost: `502` instead (a write that happened
    /// although the caller can't tell).
    LostAnswer,
}

#[derive(Debug, Clone)]
pub struct PendingFault {
    /// Matches when the request path contains this (`None`: any request).
    pub path_contains: Option<String>,
    pub fault: Fault,
}

#[derive(Debug, Clone)]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    pub reset: i64,
}

/// Everything the fake knows. Tests mutate it through [`super::FakeGitHub`].
#[derive(Debug)]
pub struct World {
    pub base_url: String,
    pub app: App,
    pub manifest_codes: Vec<String>,
    pub installations: BTreeMap<u64, Installation>,
    pub tokens: HashMap<String, Token>,
    pub token_ttl: i64,
    /// PAT → login.
    pub pats: HashMap<String, String>,
    pub repos: BTreeMap<String, Repo>,
    pub pulls: Vec<Pull>,
    pub issues: Vec<Issue>,
    pub comments: Vec<Comment>,
    pub check_runs: Vec<CheckRun>,
    pub statuses: Vec<CommitStatus>,
    pub jobs: HashMap<u64, Job>,
    /// Workflow run id → how many times failed jobs were re-run.
    pub reruns: HashMap<u64, u32>,
    pub hooks: Vec<Hook>,
    /// What was handed to a repository's merge queue (GraphQL `enqueuePullRequest`): repo, number.
    pub merge_queue: Vec<(String, u64)>,
    pub calls: Vec<Call>,
    pub faults: VecDeque<PendingFault>,
    pub rate: RateLimit,
    /// Root of the bare git remotes and the repos that have one.
    pub git_root: std::path::PathBuf,
    pub bare: std::collections::HashSet<String>,
    next_id: u64,
}

impl World {
    pub fn new(base_url: String, app: App, git_root: std::path::PathBuf) -> Self {
        World {
            base_url,
            app,
            manifest_codes: Vec::new(),
            installations: BTreeMap::new(),
            tokens: HashMap::new(),
            token_ttl: 3600,
            pats: HashMap::new(),
            repos: BTreeMap::new(),
            pulls: Vec::new(),
            merge_queue: Vec::new(),
            issues: Vec::new(),
            comments: Vec::new(),
            check_runs: Vec::new(),
            statuses: Vec::new(),
            jobs: HashMap::new(),
            reruns: HashMap::new(),
            hooks: Vec::new(),
            calls: Vec::new(),
            faults: VecDeque::new(),
            rate: RateLimit {
                limit: 5000,
                remaining: 5000,
                reset: super::time::now() + 3600,
            },
            git_root,
            bare: Default::default(),
            next_id: 100_000,
        }
    }

    /// Fresh numeric id shared by every object kind (unique across the fake).
    pub fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Branch → sha, from the bare remote when the repo has one.
    pub fn branches(&self, repo: &str) -> BTreeMap<String, String> {
        if self.bare.contains(repo) {
            let path = super::git::bare_path(&self.git_root, repo);
            return super::git::branches(&path).unwrap_or_default();
        }
        self.repos
            .get(repo)
            .map(|r| r.branches.clone())
            .unwrap_or_default()
    }

    pub fn api(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    // ---- rendering --------------------------------------------------------

    fn user_json(&self, login: &str) -> Value {
        let bot = login.ends_with("[bot]");
        json!({
            "login": login,
            "id": login_id(login),
            "node_id": format!("U_{login}"),
            "type": if bot { "Bot" } else if self.is_org(login) { "Organization" } else { "User" },
            "site_admin": false,
            "url": self.api(&format!("/users/{login}")),
            "html_url": format!("https://github.com/{login}"),
        })
    }

    fn is_org(&self, login: &str) -> bool {
        self.installations
            .values()
            .any(|i| i.account_login == login && i.account_type == "Organization")
    }

    pub fn repo_json(&self, r: &Repo) -> Value {
        let full = r.full_name();
        json!({
            "id": r.id,
            "node_id": format!("R_{}", r.id),
            "name": r.name,
            "full_name": full,
            "private": r.private,
            "owner": self.user_json(&r.owner),
            "html_url": format!("https://github.com/{full}"),
            "url": self.api(&format!("/repos/{full}")),
            "clone_url": format!("https://github.com/{full}.git"),
            "default_branch": r.default_branch,
            "visibility": if r.private { "private" } else { "public" },
            "archived": false,
            "disabled": false,
            "permissions": {"admin": r.hooks_allowed, "maintain": true, "push": true, "triage": true, "pull": true},
        })
    }

    pub fn installation_json(&self, i: &Installation) -> Value {
        json!({
            "id": i.id,
            "account": self.user_json(&i.account_login),
            "app_id": self.app.id,
            "app_slug": self.app.slug,
            "client_id": self.app.client_id,
            "target_id": i.account_id,
            "target_type": i.account_type,
            "repository_selection": "selected",
            "access_tokens_url": self.api(&format!("/app/installations/{}/access_tokens", i.id)),
            "repositories_url": self.api("/installation/repositories"),
            "html_url": format!("https://github.com/organizations/{}/settings/installations/{}", i.account_login, i.id),
            "permissions": app_permissions(),
            "events": app_events(),
            "created_at": iso(0),
            "updated_at": iso(0),
            "single_file_name": null,
            "suspended_at": null,
            "suspended_by": null,
        })
    }

    fn branch_ref_json(&self, repo: &str, r#ref: &str, sha: &str) -> Value {
        let owner = repo.split('/').next().unwrap_or_default();
        let repo_json = self
            .repos
            .get(repo)
            .map(|r| self.repo_json(r))
            .unwrap_or(Value::Null);
        json!({
            "label": format!("{owner}:{}", r#ref),
            "ref": r#ref,
            "sha": sha,
            "user": self.user_json(owner),
            "repo": repo_json,
        })
    }

    pub fn pull_json(&self, p: &Pull) -> Value {
        let url = self.api(&format!("/repos/{}/pulls/{}", p.repo, p.number));
        json!({
            "url": url,
            "id": p.id,
            "node_id": format!("PR_{}", p.id),
            "html_url": format!("https://github.com/{}/pull/{}", p.repo, p.number),
            "diff_url": format!("https://github.com/{}/pull/{}.diff", p.repo, p.number),
            "issue_url": self.api(&format!("/repos/{}/issues/{}", p.repo, p.number)),
            "number": p.number,
            "state": p.state,
            "locked": false,
            "title": p.title,
            "user": self.user_json(&p.user),
            "body": p.body,
            "created_at": iso(p.created),
            "updated_at": iso(p.updated),
            "closed_at": p.closed_at.map(iso),
            "merged_at": p.merged_at.map(iso),
            "merge_commit_sha": p.merge_commit_sha,
            "draft": p.draft,
            "head": self.branch_ref_json(&p.repo, &p.head_ref, &p.head_sha),
            "base": self.branch_ref_json(&p.repo, &p.base_ref, &p.base_sha),
            "merged": p.merged,
            "mergeable": match p.mergeable_state.as_str() { "unknown" => Value::Null, "dirty" => json!(false), _ => json!(true) },
            "mergeable_state": p.mergeable_state,
            "requested_reviewers": [],
            "labels": [],
        })
    }

    pub fn issue_json(&self, i: &Issue) -> Value {
        let mut v = json!({
            "url": self.api(&format!("/repos/{}/issues/{}", i.repo, i.number)),
            "id": i.id,
            "node_id": format!("I_{}", i.id),
            "html_url": format!("https://github.com/{}/issues/{}", i.repo, i.number),
            "number": i.number,
            "state": i.state,
            "title": i.title,
            "body": i.body,
            "user": self.user_json(&i.user),
            "labels": i.labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>(),
            "comments": self.comments.iter().filter(|c| c.repo == i.repo && c.issue_number == i.number).count(),
            "created_at": iso(i.created),
            "updated_at": iso(i.updated),
            "closed_at": null,
        });
        v["closed_at"] = if i.state == "closed" {
            json!(iso(i.updated))
        } else {
            Value::Null
        };
        v
    }

    /// Pull requests listed as issues, as `GET /repos/{o}/{r}/issues` does.
    pub fn pull_as_issue_json(&self, p: &Pull) -> Value {
        let mut v = self.issue_json(&Issue {
            repo: p.repo.clone(),
            number: p.number,
            id: p.id,
            title: p.title.clone(),
            body: p.body.clone(),
            state: p.state.clone(),
            labels: Vec::new(),
            user: p.user.clone(),
            created: p.created,
            updated: p.updated,
        });
        v["html_url"] = json!(format!("https://github.com/{}/pull/{}", p.repo, p.number));
        v["pull_request"] = json!({
            "url": self.api(&format!("/repos/{}/pulls/{}", p.repo, p.number)),
            "html_url": format!("https://github.com/{}/pull/{}", p.repo, p.number),
            "merged_at": p.merged_at.map(iso),
        });
        v
    }

    pub fn comment_json(&self, c: &Comment) -> Value {
        json!({
            "id": c.id,
            "node_id": format!("IC_{}", c.id),
            "url": self.api(&format!("/repos/{}/issues/comments/{}", c.repo, c.id)),
            "html_url": format!("https://github.com/{}/issues/{}#issuecomment-{}", c.repo, c.issue_number, c.id),
            "issue_url": self.api(&format!("/repos/{}/issues/{}", c.repo, c.issue_number)),
            "body": c.body,
            "user": self.user_json(&c.user),
            "created_at": iso(c.created),
            "updated_at": iso(c.created),
            "author_association": "NONE",
        })
    }

    fn run_html(&self, c: &CheckRun) -> String {
        match c.run_id {
            Some(run) => format!(
                "https://github.com/{}/actions/runs/{run}/job/{}",
                c.repo, c.id
            ),
            None => format!("https://github.com/{}/runs/{}", c.repo, c.id),
        }
    }

    fn check_app(c: &CheckRun) -> Value {
        if c.run_id.is_some() {
            json!({"id": 15368, "slug": "github-actions", "name": "GitHub Actions"})
        } else {
            json!({"id": 9999, "slug": "external-ci", "name": "External CI"})
        }
    }

    pub fn check_run_json(&self, c: &CheckRun) -> Value {
        let html = self.run_html(c);
        json!({
            "id": c.id,
            "node_id": format!("CR_{}", c.id),
            "head_sha": c.head_sha,
            "external_id": c.run_id.map(|_| c.id.to_string()).unwrap_or_default(),
            "url": self.api(&format!("/repos/{}/check-runs/{}", c.repo, c.id)),
            "html_url": html,
            "details_url": html,
            "status": c.status,
            "conclusion": c.conclusion,
            "started_at": iso(c.started),
            "completed_at": c.completed.map(iso),
            "output": {"title": null, "summary": null, "text": null, "annotations_count": 0},
            "name": c.name,
            "check_suite": {"id": c.suite_id},
            "app": Self::check_app(c),
            "pull_requests": self.pulls_for_sha(&c.repo, &c.head_sha),
        })
    }

    /// `pull_requests` stubs as embedded in check runs and suites.
    fn pulls_for_sha(&self, repo: &str, sha: &str) -> Vec<Value> {
        self.pulls
            .iter()
            .filter(|p| p.repo == repo && p.head_sha == sha && p.state == "open")
            .map(|p| {
                json!({
                    "url": self.api(&format!("/repos/{}/pulls/{}", p.repo, p.number)),
                    "id": p.id,
                    "number": p.number,
                    "head": {"ref": p.head_ref, "sha": p.head_sha, "repo": {"id": self.repos.get(repo).map(|r| r.id), "url": self.api(&format!("/repos/{repo}")), "name": repo.split('/').nth(1)}},
                    "base": {"ref": p.base_ref, "sha": p.base_sha, "repo": {"id": self.repos.get(repo).map(|r| r.id), "url": self.api(&format!("/repos/{repo}")), "name": repo.split('/').nth(1)}},
                })
            })
            .collect()
    }

    /// One suite per (app, sha); status and conclusion aggregate the runs the
    /// way GitHub does (any non-success conclusion wins over success).
    pub fn check_suites_json(&self, repo: &str, sha: &str) -> Vec<Value> {
        let mut suites: BTreeMap<u64, Vec<&CheckRun>> = BTreeMap::new();
        for c in self
            .check_runs
            .iter()
            .filter(|c| c.repo == repo && c.head_sha == sha)
        {
            suites.entry(c.suite_id).or_default().push(c);
        }
        let branch = self
            .pulls
            .iter()
            .find(|p| p.repo == repo && p.head_sha == sha)
            .map(|p| p.head_ref.clone());
        suites
            .into_iter()
            .map(|(id, runs)| {
                let done = runs.iter().all(|c| c.status == "completed");
                let conclusion = if !done {
                    Value::Null
                } else {
                    let rank = |c: &str| match c {
                        "action_required" => 6,
                        "failure" | "timed_out" => 5,
                        "cancelled" => 4,
                        "stale" => 3,
                        "neutral" => 2,
                        "skipped" => 1,
                        _ => 0,
                    };
                    let worst = runs
                        .iter()
                        .filter_map(|c| c.conclusion.as_deref())
                        .max_by_key(|c| rank(c))
                        .unwrap_or("success");
                    json!(if rank(worst) <= 2 { "success" } else { worst })
                };
                json!({
                    "id": id,
                    "node_id": format!("CS_{id}"),
                    "head_branch": branch,
                    "head_sha": sha,
                    "status": if done { "completed" } else if runs.iter().any(|c| c.status == "in_progress") { "in_progress" } else { "queued" },
                    "conclusion": conclusion,
                    "url": self.api(&format!("/repos/{repo}/check-suites/{id}")),
                    "app": Self::check_app(runs[0]),
                    "pull_requests": self.pulls_for_sha(repo, sha),
                    "latest_check_runs_count": runs.len(),
                    "check_runs_url": self.api(&format!("/repos/{repo}/check-suites/{id}/check-runs")),
                })
            })
            .collect()
    }

    pub fn job_json(&self, c: &CheckRun) -> Value {
        json!({
            "id": c.id,
            "run_id": c.run_id,
            "run_url": c.run_id.map(|r| self.api(&format!("/repos/{}/actions/runs/{r}", c.repo))),
            "node_id": format!("CR_{}", c.id),
            "head_sha": c.head_sha,
            "url": self.api(&format!("/repos/{}/actions/jobs/{}", c.repo, c.id)),
            "html_url": self.run_html(c),
            "status": c.status,
            "conclusion": c.conclusion,
            "started_at": iso(c.started),
            "completed_at": c.completed.map(iso),
            "name": c.name,
            "check_run_url": self.api(&format!("/repos/{}/check-runs/{}", c.repo, c.id)),
            "steps": [],
            "labels": ["ubuntu-latest"],
        })
    }

    pub fn status_json(&self, s: &CommitStatus) -> Value {
        json!({
            "id": s.id,
            "node_id": format!("SC_{}", s.id),
            "url": self.api(&format!("/repos/{}/statuses/{}", s.repo, s.sha)),
            "state": s.state,
            "description": s.description,
            "target_url": s.target_url,
            "context": s.context,
            "created_at": iso(s.updated),
            "updated_at": iso(s.updated),
        })
    }

    pub fn hook_json(&self, h: &Hook) -> Value {
        json!({
            "type": "Repository",
            "id": h.id,
            "name": "web",
            "active": h.active,
            "events": h.events,
            "config": {"url": h.url, "content_type": h.content_type, "insecure_ssl": "0", "secret": "********"},
            "url": self.api(&format!("/repos/{}/hooks/{}", h.repo, h.id)),
            "ping_url": self.api(&format!("/repos/{}/hooks/{}/pings", h.repo, h.id)),
            "created_at": iso(0),
            "updated_at": iso(0),
        })
    }

    pub fn app_json(&self) -> Value {
        json!({
            "id": self.app.id,
            "slug": self.app.slug,
            "node_id": format!("A_{}", self.app.id),
            "client_id": self.app.client_id,
            "owner": self.user_json(&self.app.owner_login),
            "name": self.app.name,
            "description": "",
            "external_url": "https://steve.example.test",
            "html_url": format!("https://github.com/apps/{}", self.app.slug),
            "created_at": iso(0),
            "updated_at": iso(0),
            "permissions": app_permissions(),
            "events": app_events(),
        })
    }
}

/// Deterministic numeric id for a login, so the same user renders the same id.
pub fn login_id(login: &str) -> u64 {
    login
        .bytes()
        .fold(1_000u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64))
        % 90_000_000
        + 1_000
}

/// The permissions Steve's App manifest requests.
pub fn app_permissions() -> Value {
    json!({"metadata": "read", "contents": "write", "pull_requests": "write", "issues": "write",
           "checks": "read", "actions": "write", "statuses": "read"})
}

pub fn app_events() -> Value {
    json!([
        "pull_request",
        "pull_request_review",
        "check_suite",
        "check_run",
        "workflow_run",
        "issues",
        "issue_comment",
        "installation_repositories"
    ])
}

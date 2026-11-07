//! A fake GitHub for tests (feature `fake`).
//!
//! [`FakeGitHub::start`] serves the REST subset Steve uses on a random local
//! port; pass [`FakeGitHub::url`] as the connection's `api_base`. The world
//! starts with one App, one installation on the `steve-test` organisation,
//! one private repo `steve-test/demo` and one PAT (see [`defaults`]); tests
//! add state and inspect recorded calls through the handle. Webhooks go the
//! other way: [`FakeGitHub::emit`] signs a payload with the App's webhook
//! secret and POSTs it to the target set with [`FakeGitHub::set_webhook_target`].
//!
//! Nothing here talks to github.com.

mod git;
mod middleware;
mod model;
mod routes;
mod time;
mod webhook;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use jsonwebtoken::DecodingKey;
use serde_json::Value;
use tokio::sync::oneshot;

pub use model::{App, Call, CheckRun, CommitStatus, Fault, Hook, Installation, Pull, Repo, World};
pub use webhook::{Delivery, FIXTURES, WebhookSender, fixture, new_guid};

use model::{Job, PendingFault};
use routes::AppState;

/// Test values shared by the fake's default world and the recorded fixtures.
pub mod defaults {
    pub const APP_ID: u64 = 1001;
    pub const APP_SLUG: &str = "steve-test";
    pub const APP_NAME: &str = "Steve Test";
    pub const CLIENT_ID: &str = "Iv23liTESTCLIENTID00";
    pub const CLIENT_SECRET: &str = "test-client-secret";
    pub const WEBHOOK_SECRET: &str = "test-webhook-secret";
    pub const INSTALLATION_ID: u64 = 4242;
    pub const ORG: &str = "steve-test";
    pub const ORG_ID: u64 = 5001;
    pub const REPO: &str = "steve-test/demo";
    pub const REPO_ID: u64 = 7001;
    pub const DEFAULT_BRANCH: &str = "main";
    /// Base sha of `main` in the default repo (matches the fixtures).
    pub const BASE_SHA: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c";
    pub const USER: &str = "lee-test";
    pub const PAT: &str = "github_pat_TEST0000000000000000000000";
    /// Throwaway 2048-bit key for the fake App. Test use only.
    pub const APP_PRIVATE_KEY_PEM: &str = include_str!("test_app_key.pem");
    pub const APP_PUBLIC_KEY_PEM: &str = include_str!("test_app_key.pub.pem");
}

#[derive(Debug, thiserror::Error)]
pub enum FakeError {
    #[error("fake github: io: {0}")]
    Io(#[from] std::io::Error),
    #[error("fake github: http: {0}")]
    Http(String),
    #[error("fake github: git: {0}")]
    Git(String),
    #[error("fake github: not found: {0}")]
    NotFound(String),
    #[error("fake github: fixture: {0}")]
    Fixture(String),
    #[error("fake github: no webhook target set")]
    NoTarget,
}

/// Handle to a running fake. Dropping it stops the server and removes the
/// bare git remotes.
pub struct FakeGitHub {
    addr: SocketAddr,
    state: AppState,
    sender: Mutex<Option<Arc<WebhookSender>>>,
    shutdown: Option<oneshot::Sender<()>>,
    _git_root: tempfile::TempDir,
}

impl FakeGitHub {
    /// Starts on `127.0.0.1:<random>` with the default world.
    pub async fn start() -> Result<Self, FakeError> {
        Self::start_on(SocketAddr::from(([127, 0, 0, 1], 0))).await
    }

    /// Starts on a given address (the `steve-fake-github` binary uses this).
    pub async fn start_on(addr: SocketAddr) -> Result<Self, FakeError> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let git_root = tempfile::Builder::new()
            .prefix("steve-fake-github-")
            .tempdir()?;
        let app = App {
            id: defaults::APP_ID,
            slug: defaults::APP_SLUG.into(),
            name: defaults::APP_NAME.into(),
            owner_login: defaults::ORG.into(),
            client_id: defaults::CLIENT_ID.into(),
            client_secret: defaults::CLIENT_SECRET.into(),
            webhook_secret: defaults::WEBHOOK_SECRET.into(),
            pem: defaults::APP_PRIVATE_KEY_PEM.into(),
        };
        let mut world = World::new(format!("http://{addr}"), app, git_root.path().to_path_buf());
        seed(&mut world);
        let jwt_key = DecodingKey::from_rsa_pem(defaults::APP_PUBLIC_KEY_PEM.as_bytes())
            .map_err(|e| FakeError::Fixture(format!("test key: {e}")))?;
        let state = AppState {
            world: Arc::new(Mutex::new(world)),
            jwt_key: Arc::new(jwt_key),
        };
        let app = routes::router(state.clone()).layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::layer,
        ));
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let serve = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            if let Err(e) = serve.await {
                tracing::warn!("fake github stopped: {e}");
            }
        });
        Ok(FakeGitHub {
            addr,
            state,
            sender: Mutex::new(None),
            shutdown: Some(tx),
            _git_root: git_root,
        })
    }

    /// Base URL, used as `api_base` (no trailing slash).
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Runs `f` with the world locked: the escape hatch for any state change
    /// or inspection the helpers below don't cover.
    pub fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        f(&mut self.state.lock())
    }

    // ---- inspection -------------------------------------------------------

    /// Every request received, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.with(|w| w.calls.clone())
    }

    /// Calls with this method whose path starts with `prefix`.
    pub fn calls_to(&self, method: &str, prefix: &str) -> Vec<Call> {
        self.with(|w| {
            w.calls
                .iter()
                .filter(|c| c.method == method && c.path.starts_with(prefix))
                .cloned()
                .collect()
        })
    }

    pub fn clear_calls(&self) {
        self.with(|w| w.calls.clear());
    }

    pub fn pull(&self, repo: &str, number: u64) -> Option<Pull> {
        self.with(|w| {
            w.pulls
                .iter()
                .find(|p| p.repo == repo && p.number == number)
                .cloned()
        })
    }

    /// How many PRs (any state) there are from `head_ref` in `repo`.
    pub fn pulls_of(&self, repo: &str, head_ref: &str) -> usize {
        self.with(|w| {
            w.pulls
                .iter()
                .filter(|p| p.repo == repo && p.head_ref == head_ref)
                .count()
        })
    }

    /// Comment bodies on an issue or PR, oldest first.
    pub fn comments(&self, repo: &str, number: u64) -> Vec<String> {
        self.with(|w| {
            w.comments
                .iter()
                .filter(|c| c.repo == repo && c.issue_number == number)
                .map(|c| c.body.clone())
                .collect()
        })
    }

    pub fn hooks(&self, repo: &str) -> Vec<Hook> {
        self.with(|w| w.hooks.iter().filter(|h| h.repo == repo).cloned().collect())
    }

    /// How many times `rerun-failed-jobs` ran for a workflow run.
    pub fn reruns(&self, run_id: u64) -> u32 {
        self.with(|w| w.reruns.get(&run_id).copied().unwrap_or(0))
    }

    /// Branch → sha (from the bare remote when the repo has one).
    pub fn branches(&self, repo: &str) -> std::collections::BTreeMap<String, String> {
        self.with(|w| w.branches(repo))
    }

    // ---- App, installations, credentials -----------------------------------

    pub fn app(&self) -> App {
        self.with(|w| w.app.clone())
    }

    /// Registers a single-use code for `POST /app-manifests/{code}/conversions`.
    pub fn manifest_code(&self) -> String {
        let code = format!("manifest-{}", &new_guid()[..8]);
        self.with(|w| w.manifest_codes.push(code.clone()));
        code
    }

    pub fn add_installation(
        &self,
        id: u64,
        account_login: &str,
        account_type: &str,
        repos: &[&str],
    ) {
        self.with(|w| {
            w.installations.insert(
                id,
                Installation {
                    id,
                    account_login: account_login.into(),
                    account_id: model::login_id(account_login),
                    account_type: account_type.into(),
                    repos: repos.iter().map(|r| r.to_string()).collect(),
                },
            );
        });
    }

    /// Removes a repo from an installation (pair with an
    /// `installation_repositories.removed` webhook).
    pub fn remove_from_installation(&self, installation: u64, repo: &str) {
        self.with(|w| {
            if let Some(i) = w.installations.get_mut(&installation) {
                i.repos.retain(|r| r != repo);
            }
        });
    }

    pub fn add_pat(&self, token: &str, login: &str) {
        self.with(|w| w.pats.insert(token.into(), login.into()));
    }

    /// Lifetime of new installation tokens (GitHub: 3600 s).
    pub fn set_token_ttl(&self, secs: i64) {
        self.with(|w| w.token_ttl = secs);
    }

    /// Makes every issued installation token answer 401 from now on.
    pub fn revoke_tokens(&self) {
        self.with(|w| w.tokens.values_mut().for_each(|t| t.revoked = true));
    }

    /// Installation tokens issued so far (newest last is not guaranteed).
    pub fn issued_tokens(&self) -> usize {
        self.with(|w| w.tokens.len())
    }

    // ---- repos, branches, git ---------------------------------------------

    /// Adds a repo (and its default branch at a synthetic sha). Returns its id.
    pub fn add_repo(&self, full_name: &str, private: bool, default_branch: &str) -> u64 {
        self.with(|w| {
            let id = w.id();
            add_repo(
                w,
                id,
                full_name,
                private,
                default_branch,
                &format!("{:040x}", id),
            );
            id
        })
    }

    pub fn set_branch(&self, repo: &str, branch: &str, sha: &str) {
        self.with(|w| {
            if let Some(r) = w.repos.get_mut(repo) {
                r.branches.insert(branch.into(), sha.into());
            }
        });
    }

    /// Makes `repo` public or private (as a later visibility change on GitHub would).
    pub fn set_private(&self, repo: &str, private: bool) {
        self.with(|w| {
            if let Some(r) = w.repos.get_mut(repo) {
                r.private = private;
            }
        });
    }

    /// Whether the caller may manage repo hooks (false → 404, like a PAT
    /// without `Webhooks: write`).
    pub fn set_hooks_allowed(&self, repo: &str, allowed: bool) {
        self.with(|w| {
            if let Some(r) = w.repos.get_mut(repo) {
                r.hooks_allowed = allowed;
            }
        });
    }

    /// Creates (once) a bare git remote for `repo` and switches the repo's
    /// branches to come from it. Returns its path.
    pub fn bare_remote(&self, repo: &str) -> Result<PathBuf, FakeError> {
        self.with(|w| {
            let branch = w
                .repos
                .get(repo)
                .map(|r| r.default_branch.clone())
                .ok_or_else(|| FakeError::NotFound(repo.into()))?;
            let path = git::init_bare(&w.git_root, repo, &branch)?;
            w.bare.insert(repo.to_string());
            Ok(path)
        })
    }

    /// `(key, value)` for a test-only git config entry mapping
    /// `https://github.com/<o>/<r>` onto the bare remotes, e.g. via
    /// `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` or `git -c key=value`.
    pub fn git_url_rewrite(&self) -> (String, String) {
        let root = self.with(|w| w.git_root.clone());
        (
            format!("url.{}/.insteadOf", root.display()),
            "https://github.com/".to_string(),
        )
    }

    // ---- pull requests, issues --------------------------------------------

    /// Opens a PR authored by `user` (creating the head branch if missing).
    /// Returns its number.
    pub fn open_pr(
        &self,
        repo: &str,
        head: &str,
        head_sha: &str,
        title: &str,
        body: &str,
        user: &str,
    ) -> Option<u64> {
        self.with(|w| {
            let id = w.id();
            let t = time::now();
            let r = w.repos.get_mut(repo)?;
            r.branches
                .entry(head.into())
                .or_insert_with(|| head_sha.into());
            let base_ref = r.default_branch.clone();
            let base_sha = r.branches.get(&base_ref).cloned().unwrap_or_default();
            let number = r.next_number;
            r.next_number += 1;
            w.pulls.push(Pull {
                repo: repo.into(),
                number,
                id,
                title: title.into(),
                body: body.into(),
                state: "open".into(),
                draft: false,
                merged: false,
                head_ref: head.into(),
                head_sha: head_sha.into(),
                base_ref,
                base_sha,
                user: user.into(),
                mergeable_state: "clean".into(),
                merge_commit_sha: None,
                created: t,
                updated: t,
                closed_at: None,
                merged_at: None,
            });
            Some(number)
        })
    }

    /// A new commit on the PR's head (pair with a `pull_request.synchronize` webhook).
    pub fn push_to_pr(&self, repo: &str, number: u64, sha: &str) {
        self.with(|w| {
            let mut head = None;
            if let Some(p) = w
                .pulls
                .iter_mut()
                .find(|p| p.repo == repo && p.number == number)
            {
                p.head_sha = sha.into();
                p.updated = time::now();
                head = Some(p.head_ref.clone());
            }
            if let (Some(h), Some(r)) = (head, w.repos.get_mut(repo)) {
                r.branches.insert(h, sha.into());
            }
        });
    }

    /// `clean`, `dirty`, `blocked`, `unknown`…
    pub fn set_mergeable(&self, repo: &str, number: u64, state: &str) {
        self.with(|w| {
            if let Some(p) = w
                .pulls
                .iter_mut()
                .find(|p| p.repo == repo && p.number == number)
            {
                p.mergeable_state = state.into();
            }
        });
    }

    pub fn add_issue(&self, repo: &str, title: &str, body: &str, labels: &[&str]) -> Option<u64> {
        self.with(|w| routes::new_issue(w, repo, title, body, labels, defaults::USER))
    }

    // ---- checks -----------------------------------------------------------

    /// Sets a GitHub Actions check on `sha`: creates or updates the check run
    /// `name` (job id = check run id, one workflow run and suite per sha).
    /// `conclusion: None` means in progress. Returns the job id.
    pub fn set_check(&self, repo: &str, sha: &str, name: &str, conclusion: Option<&str>) -> u64 {
        self.upsert_check(repo, sha, name, conclusion, true)
    }

    /// Like [`Self::set_check`] for a check from another CI app (no job, no logs).
    pub fn set_external_check(
        &self,
        repo: &str,
        sha: &str,
        name: &str,
        conclusion: Option<&str>,
    ) -> u64 {
        self.upsert_check(repo, sha, name, conclusion, false)
    }

    fn upsert_check(
        &self,
        repo: &str,
        sha: &str,
        name: &str,
        conclusion: Option<&str>,
        actions: bool,
    ) -> u64 {
        self.with(|w| {
            let t = time::now();
            let (status, completed) = match conclusion {
                Some(_) => ("completed", Some(t)),
                None => ("in_progress", None),
            };
            if let Some(c) = w
                .check_runs
                .iter_mut()
                .find(|c| c.repo == repo && c.head_sha == sha && c.name == name)
            {
                c.status = status.into();
                c.conclusion = conclusion.map(str::to_string);
                c.completed = completed;
                return c.id;
            }
            let same = |c: &&CheckRun| {
                c.repo == repo && c.head_sha == sha && c.run_id.is_some() == actions
            };
            let existing = w
                .check_runs
                .iter()
                .find(same)
                .map(|c| (c.suite_id, c.run_id));
            let (suite_id, run_id) = match existing {
                Some(x) => x,
                None => {
                    let suite = w.id();
                    (suite, if actions { Some(w.id()) } else { None })
                }
            };
            let id = w.id();
            w.check_runs.push(CheckRun {
                id,
                repo: repo.into(),
                head_sha: sha.into(),
                name: name.into(),
                status: status.into(),
                conclusion: conclusion.map(str::to_string),
                suite_id,
                run_id,
                started: t,
                completed,
            });
            if actions {
                w.jobs.insert(
                    id,
                    Job {
                        id,
                        log: format!("{name}: no log set\n"),
                    },
                );
            }
            id
        })
    }

    /// Workflow run id of an Actions job.
    pub fn run_of(&self, job_id: u64) -> Option<u64> {
        self.with(|w| {
            w.check_runs
                .iter()
                .find(|c| c.id == job_id)
                .and_then(|c| c.run_id)
        })
    }

    /// Plain-text log served (after the 302) for an Actions job.
    pub fn set_job_log(&self, job_id: u64, log: &str) {
        self.with(|w| {
            w.jobs.insert(
                job_id,
                Job {
                    id: job_id,
                    log: log.into(),
                },
            );
        });
    }

    /// A legacy commit status (`error|failure|pending|success`).
    pub fn set_status(&self, repo: &str, sha: &str, context: &str, state: &str, target_url: &str) {
        self.with(|w| {
            let id = w.id();
            w.statuses.push(CommitStatus {
                id,
                repo: repo.into(),
                sha: sha.into(),
                context: context.into(),
                state: state.into(),
                target_url: target_url.into(),
                description: String::new(),
                updated: time::now(),
            });
        });
    }

    // ---- faults and limits ------------------------------------------------

    /// Fails the next request (any path) with `fault`.
    pub fn fail_next(&self, fault: Fault) {
        self.with(|w| {
            w.faults.push_back(PendingFault {
                path_contains: None,
                fault,
            })
        });
    }

    /// Fails the next request whose path contains `path` with `fault`.
    pub fn fail_next_on(&self, path: &str, fault: Fault) {
        self.with(|w| {
            w.faults.push_back(PendingFault {
                path_contains: Some(path.into()),
                fault,
            })
        });
    }

    /// Remaining primary rate limit; at 0 every counted request gets 403.
    pub fn set_rate_remaining(&self, remaining: u64) {
        self.with(|w| w.rate.remaining = remaining.min(w.rate.limit));
    }

    // ---- webhooks ---------------------------------------------------------

    /// Where [`Self::emit`] posts (e.g. `http://127.0.0.1:<hub>/hooks/github`).
    pub fn set_webhook_target(&self, url: &str) {
        let secret = self.with(|w| w.app.webhook_secret.clone());
        let sender = WebhookSender::new(url, secret.into_bytes());
        *self.sender.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(sender));
    }

    fn sender(&self) -> Result<Arc<WebhookSender>, FakeError> {
        self.sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(FakeError::NoTarget)
    }

    /// Signs `payload` with the App's webhook secret and delivers it.
    pub async fn emit(&self, event: &str, payload: &Value) -> Result<Delivery, FakeError> {
        self.sender()?.send(event, payload).await
    }

    /// Delivers a recorded fixture by name (e.g. `check_suite.completed.failure`).
    pub async fn emit_fixture(&self, name: &str) -> Result<Delivery, FakeError> {
        let (event, payload) = fixture(name)?;
        self.emit(event, &payload).await
    }

    /// Sends an earlier delivery again with the same `X-GitHub-Delivery`.
    pub async fn redeliver(&self, delivery_id: &str) -> Result<Delivery, FakeError> {
        self.sender()?.redeliver(delivery_id).await
    }

    /// Deliveries sent so far.
    pub fn deliveries(&self) -> Vec<Delivery> {
        self.sender().map(|s| s.sent()).unwrap_or_default()
    }
}

impl Drop for FakeGitHub {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

fn add_repo(
    w: &mut World,
    id: u64,
    full_name: &str,
    private: bool,
    default_branch: &str,
    sha: &str,
) {
    let (owner, name) = full_name.split_once('/').unwrap_or((full_name, full_name));
    let mut branches = std::collections::BTreeMap::new();
    branches.insert(default_branch.to_string(), sha.to_string());
    w.repos.insert(
        full_name.to_string(),
        Repo {
            id,
            owner: owner.into(),
            name: name.into(),
            private,
            default_branch: default_branch.into(),
            branches,
            hooks_allowed: true,
            next_number: 1,
        },
    );
}

/// The default world, consistent with `tests/fixtures/github/`.
fn seed(w: &mut World) {
    use defaults::*;
    add_repo(w, REPO_ID, REPO, true, DEFAULT_BRANCH, BASE_SHA);
    if let Some(r) = w.repos.get_mut(REPO) {
        // Fixture PR is #12, issue #13: continue after them.
        r.next_number = 14;
    }
    w.installations.insert(
        INSTALLATION_ID,
        Installation {
            id: INSTALLATION_ID,
            account_login: ORG.into(),
            account_id: ORG_ID,
            account_type: "Organization".into(),
            repos: vec![REPO.into()],
        },
    );
    w.pats.insert(PAT.into(), USER.into());
}

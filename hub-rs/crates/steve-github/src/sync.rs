//! State sync and the signals that follow it. One handler for both ways in:
//! webhook deliveries ([`crate::hooks`]) and what the poll ([`crate::poll`]) sees, so a change seen
//! twice is applied twice to the same rows and produces no second event.
//!
//! Events come from *transitions* of the stored state, computed in the same transaction as the
//! write: a PR that was open and is now merged, a commit whose checks were not settled and now are.
//! Seeing the same state again changes nothing. The `external_id` of every signal is derived from
//! the change itself (never from a delivery id), so S1's dedupe holds as a second line.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_modules::accounts::connections::Health;
use steve_store::{ChangeEvent, Tx, uid};

use crate::GitHub;
use crate::events::{Actor, SignalEvent};
use crate::link;
use crate::repo::{self, CheckRow, IssueRow, PrRow, RepoRow};
use crate::repos::Settings;

/// Where a change came from (stored with a delivery as JSON: `{"app":{"ws":…}}`, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// `/hooks/github`: an App delivery; `ws` is the App's workspace.
    App { ws: String },
    /// `/hooks/<connection_id>`: a PAT repository hook.
    Conn { ws: String, connection_id: String },
    /// The poll of one repo.
    Poll { repo_id: String },
}

/// How a delivery ended (`gh_deliveries.status`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// `ok|ignored|failed`.
    pub status: &'static str,
    pub error: Option<String>,
    pub ws: Option<String>,
    pub repo_id: Option<String>,
}

impl Outcome {
    fn ok(ws: &str, repo: Option<&str>) -> Self {
        Self {
            status: "ok",
            error: None,
            ws: Some(ws.into()),
            repo_id: repo.map(String::from),
        }
    }
    fn ignored(ws: Option<&str>, repo: Option<&str>, why: &str) -> Self {
        Self {
            status: "ignored",
            error: Some(why.into()),
            ws: ws.map(String::from),
            repo_id: repo.map(String::from),
        }
    }
    fn failed(ws: Option<&str>, repo: Option<&str>, why: String) -> Self {
        Self {
            status: "failed",
            error: Some(why),
            ws: ws.map(String::from),
            repo_id: repo.map(String::from),
        }
    }
}

/// What one transaction changed, published and ingested after the commit.
#[derive(Default)]
pub(crate) struct Effects {
    pub(crate) gh: BTreeSet<String>,
    pub(crate) signals: Vec<SignalEvent>,
    /// A non-fatal problem (a list GitHub would not give), kept on the delivery.
    pub(crate) note: Option<String>,
    /// Items to finish after the commit: `(item, now)` (a PR merged, `finish_on=merged`).
    pub(crate) finish: Vec<(String, String)>,
    /// What people with a say in the repository wrote on a PR linked to an item, for that item
    /// (2026-11-09: a linked PR's comments and reviews reach its item).
    pub(crate) supplements: Vec<crate::after::PrSupplement>,
    /// Processed late (a retry, the recovery after a restart): its view may be older than what is
    /// stored meanwhile, which then stays.
    pub(crate) late: bool,
}

/// Checks whose conclusion makes the commit fail.
const FAILING: [&str; 4] = ["failure", "timed_out", "cancelled", "action_required"];

/// The aggregate of a commit's checks: `none|pending|success|failure`, and whether every check is
/// done. Reruns: only the newest run per name counts (GitHub's `filter=latest`).
pub fn checks_state(rows: &[CheckRow]) -> (&'static str, bool) {
    let latest = latest_checks(rows);
    if latest.is_empty() {
        return ("none", true);
    }
    let pending = latest.iter().any(|c| c.status != "completed");
    let failed = latest.iter().any(|c| {
        c.conclusion
            .as_deref()
            .is_some_and(|x| FAILING.contains(&x))
    });
    if failed {
        ("failure", !pending)
    } else if pending {
        ("pending", false)
    } else {
        ("success", true)
    }
}

/// The newest row per `(kind, name)`: highest numeric check run id, else latest update.
pub fn latest_checks(rows: &[CheckRow]) -> Vec<&CheckRow> {
    let mut by: BTreeMap<(&str, &str), &CheckRow> = BTreeMap::new();
    for r in rows {
        let key = (r.kind.as_str(), r.name.as_str());
        let newer = |old: &CheckRow| match (r.ext_id.parse::<i64>(), old.ext_id.parse::<i64>()) {
            (Ok(a), Ok(b)) if r.kind == "check_run" => a > b,
            _ => r.updated > old.updated,
        };
        match by.get(&key) {
            Some(old) if !newer(old) => {}
            _ => {
                by.insert(key, r);
            }
        }
    }
    by.into_values().collect()
}

/// A settled outcome worth a signal: `failure` once nothing is pending, or `success`.
fn settled(rows: &[CheckRow]) -> Option<&'static str> {
    match checks_state(rows) {
        ("failure", true) => Some("failure"),
        ("success", _) => Some("success"),
        _ => None,
    }
}

fn failed_list(rows: &[CheckRow]) -> Vec<Value> {
    latest_checks(rows)
        .into_iter()
        .filter(|c| {
            c.conclusion
                .as_deref()
                .is_some_and(|x| FAILING.contains(&x))
        })
        .map(|c| json!({"name": c.name, "job_id": c.job_id, "url": c.url}))
        .collect()
}

/// ISO 8601 → Unix ms.
pub fn ts(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.timestamp_millis()),
        // Some payloads (installation) carry Unix seconds.
        Value::Number(n) => n.as_i64().map(|s| s * 1000),
        _ => None,
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn actor(v: &Value) -> Option<Actor> {
    let login = v["login"].as_str().filter(|l| !l.is_empty())?;
    Some(Actor {
        login: login.into(),
        kind: v["type"].as_str().unwrap_or("User").into(),
    })
}

/// `mergeable_state` → `clean|dirty|blocked|unknown`; `None` keeps what we had (webhooks often
/// leave it out or `null`).
fn mergeable(v: &Value) -> Option<String> {
    let m = v.as_str()?;
    Some(
        match m {
            "clean" | "unstable" | "has_hooks" => "clean",
            "dirty" => "dirty",
            "blocked" | "behind" | "draft" => "blocked",
            _ => "unknown",
        }
        .into(),
    )
}

/// The run and job of an Actions check (`…/actions/runs/<run>/job/<job>`).
fn actions_ids(url: &str) -> (Option<i64>, Option<i64>) {
    let mut run = None;
    let mut job = None;
    let parts: Vec<&str> = url.split('/').collect();
    for w in parts.windows(2) {
        match w[0] {
            "runs" => run = run.or(w[1].parse().ok()),
            "job" | "jobs" => job = job.or(w[1].parse().ok()),
            _ => {}
        }
    }
    (run, job)
}

/// A check run (webhook `check_run`, `GET …/check-runs`) as a row.
pub fn check_from_run(repo: &RepoRow, v: &Value, now: i64) -> Option<CheckRow> {
    let id = v["id"].as_i64()?;
    let html = v["html_url"]
        .as_str()
        .or(v["details_url"].as_str())
        .unwrap_or_default();
    let is_actions = v["app"]["slug"].as_str() == Some("github-actions");
    let (run, job) = actions_ids(v["details_url"].as_str().unwrap_or(html));
    Some(CheckRow {
        repo_id: repo.id.clone(),
        workspace_id: repo.workspace_id.clone(),
        head_sha: v["head_sha"].as_str()?.into(),
        kind: "check_run".into(),
        ext_id: id.to_string(),
        name: s(&v["name"]),
        status: v["status"].as_str().unwrap_or("queued").into(),
        conclusion: v["conclusion"].as_str().map(String::from),
        url: html.into(),
        run_id: if is_actions { run } else { None },
        // Actions check runs share their id with the job.
        job_id: if is_actions { job.or(Some(id)) } else { None },
        summary: cut(v["output"]["title"].as_str().unwrap_or_default(), 200),
        started: ts(&v["started_at"]),
        completed: ts(&v["completed_at"]),
        updated: now,
    })
}

/// An Actions job (`GET …/actions/runs/{id}/jobs`) as a check row: its check run id from
/// `check_run_url`.
pub fn check_from_job(repo: &RepoRow, v: &Value, now: i64) -> Option<CheckRow> {
    let job = v["id"].as_i64()?;
    let ext = v["check_run_url"]
        .as_str()
        .and_then(|u| u.rsplit('/').next())
        .and_then(|x| x.parse::<i64>().ok())
        .unwrap_or(job);
    Some(CheckRow {
        repo_id: repo.id.clone(),
        workspace_id: repo.workspace_id.clone(),
        head_sha: v["head_sha"].as_str()?.into(),
        kind: "check_run".into(),
        ext_id: ext.to_string(),
        name: s(&v["name"]),
        status: v["status"].as_str().unwrap_or("queued").into(),
        conclusion: v["conclusion"].as_str().map(String::from),
        url: s(&v["html_url"]),
        run_id: v["run_id"].as_i64(),
        job_id: Some(job),
        summary: String::new(),
        started: ts(&v["started_at"]),
        completed: ts(&v["completed_at"]),
        updated: now,
    })
}

/// A legacy commit status (`status` event, `GET …/status`) as a row.
pub fn check_from_status(repo: &RepoRow, sha: &str, v: &Value, now: i64) -> Option<CheckRow> {
    let context = v["context"].as_str().filter(|c| !c.is_empty())?;
    let state = v["state"].as_str().unwrap_or("pending");
    let (status, conclusion) = match state {
        "success" => ("completed", Some("success")),
        "failure" | "error" => ("completed", Some("failure")),
        _ => ("in_progress", None),
    };
    Some(CheckRow {
        repo_id: repo.id.clone(),
        workspace_id: repo.workspace_id.clone(),
        head_sha: sha.into(),
        kind: "status".into(),
        ext_id: context.into(),
        name: context.into(),
        status: status.into(),
        conclusion: conclusion.map(String::from),
        url: s(&v["target_url"]),
        run_id: None,
        job_id: None,
        summary: cut(v["description"].as_str().unwrap_or_default(), 200),
        started: ts(&v["created_at"]),
        completed: conclusion.and(ts(&v["updated_at"])),
        updated: now,
    })
}

/// The pieces of a change that need GitHub (fetched before the transaction).
#[derive(Default)]
struct Fetched {
    checks: Vec<CheckRow>,
}

impl GitHub {
    /// Applies one change. Never panics on odd payloads: what cannot be read is `ignored`.
    pub async fn apply(&self, src: &Source, event: &str, payload: &Value) -> Outcome {
        self.apply_as(src, event, payload, false).await
    }

    /// [`GitHub::apply`]; `late`: processed after it was received (a retry, the recovery), so an
    /// older view than the stored one is not applied.
    pub(crate) async fn apply_as(
        &self,
        src: &Source,
        event: &str,
        payload: &Value,
        late: bool,
    ) -> Outcome {
        // Which connection, and in which workspace.
        let resolved = match src {
            Source::App { ws } => {
                let inst = payload["installation"]["id"].as_i64();
                match inst {
                    None => {
                        return if event == "ping" {
                            self.app_ping(ws, payload)
                        } else {
                            Outcome::ignored(Some(ws), None, "没有安装编号")
                        };
                    }
                    Some(i) => match self.store().reader(|c| {
                        repo::installation_connections(c, ws, i).map_err(ApiError::from)
                    }) {
                        Ok(v) if !v.is_empty() => (ws.clone(), v),
                        Ok(_) => {
                            return Outcome::ignored(Some(ws), None, "这个安装没有对应的连接");
                        }
                        Err(e) => return Outcome::failed(Some(ws), None, e.to_string()),
                    },
                }
            }
            Source::Conn { ws, connection_id } => (ws.clone(), vec![connection_id.clone()]),
            Source::Poll { repo_id } => {
                match self
                    .store()
                    .reader(|c| repo::repo_get(c, repo_id).map_err(ApiError::from))
                {
                    Ok(Some(r)) => (r.workspace_id, vec![r.connection_id]),
                    Ok(None) => return Outcome::ignored(None, None, "仓库已经取消关联"),
                    Err(e) => return Outcome::failed(None, None, e.to_string()),
                }
            }
        };
        let (ws, conns) = resolved;
        match event {
            "installation" | "installation_repositories" => {
                return self.installation(&ws, &conns, event, payload).await;
            }
            _ => {}
        }
        // Everything else is about one linked repository of that connection.
        let Some(gh_id) = payload["repository"]["id"].as_i64() else {
            return if event == "ping" {
                self.touch_connections(&ws, &conns)
            } else {
                Outcome::ignored(Some(&ws), None, "没有仓库")
            };
        };
        let repo = match self
            .store()
            .reader(|c| repo::repo_by_gh(c, &ws, gh_id).map_err(ApiError::from))
        {
            Ok(Some(r)) if conns.contains(&r.connection_id) => r,
            Ok(_) => return Outcome::ignored(Some(&ws), None, "这个仓库没有关联"),
            Err(e) => return Outcome::failed(Some(&ws), None, e.to_string()),
        };
        if !repo.enabled {
            return Outcome::ignored(Some(&ws), Some(&repo.id), "这个仓库已停用");
        }
        let from_hook = !matches!(src, Source::Poll { .. });
        let fetched = match self.fetch(&repo, event, payload).await {
            // tried again later rather than applied without it
            Err((e, true)) => return Outcome::failed(Some(&ws), Some(&repo.id), e),
            f => f,
        };
        let note = fetched.as_ref().err().map(|(e, _)| e.clone());
        let fetched = fetched.unwrap_or_default();
        let now = self.now();
        let res = self.store().transaction(|tx| {
            let mut fx = Effects {
                note: note.clone(),
                late,
                ..Default::default()
            };
            if from_hook {
                repo::set_last_delivery(tx, &repo.id, now)?;
            }
            self.dispatch(tx, &repo, event, payload, fetched, now, &mut fx)?;
            self.ask_merges(tx, &repo, &fx)?;
            crate::after::emit(tx, &repo.workspace_id, &repo.connection_id, &mut fx)?;
            Ok::<_, ApiError>(fx)
        });
        match res {
            Ok(fx) => {
                let mut out = Outcome::ok(&ws, Some(&repo.id));
                out.error = fx.note.clone();
                self.after_commit(&ws, &repo.connection_id, fx).await;
                out
            }
            Err(e) => Outcome::failed(Some(&ws), Some(&repo.id), e.to_string()),
        }
    }

    /// Publishes the `gh` notices. The items to finish and the signals went to the outbox in the
    /// transaction ([`crate::after`]).
    pub(crate) async fn after_commit(&self, ws: &str, _connection: &str, fx: Effects) {
        let notifier = self.notifier();
        for id in &fx.gh {
            notifier.gh(ws, id);
        }
    }

    /// The App-level `ping` (no installation): every repo of the App's installations is reachable.
    fn app_ping(&self, ws: &str, payload: &Value) -> Outcome {
        let Some(app_id) = payload["hook"]["app_id"].as_i64() else {
            return Outcome::ok(ws, None);
        };
        let now = self.now();
        let r = self.store().transaction(|tx| {
            let conns: Vec<String> = tx
                .prepare_cached(
                    "SELECT id FROM connections WHERE workspace_id = ? AND adapter = 'github_app'
                     AND CAST(json_extract(config, '$.app_id') AS INTEGER) = ?",
                )?
                .query_map(rusqlite::params![ws, app_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for c in conns {
                for r in repo::repos_of_connection(tx, &c)? {
                    repo::set_last_delivery(tx, &r.id, now)?;
                }
            }
            Ok::<_, ApiError>(())
        });
        match r {
            Ok(()) => Outcome::ok(ws, None),
            Err(e) => Outcome::failed(Some(ws), None, e.to_string()),
        }
    }

    fn touch_connections(&self, ws: &str, conns: &[String]) -> Outcome {
        let now = self.now();
        let r = self.store().transaction(|tx| {
            for c in conns {
                for r in repo::repos_of_connection(tx, c)? {
                    repo::set_last_delivery(tx, &r.id, now)?;
                }
            }
            Ok::<_, ApiError>(())
        });
        match r {
            Ok(()) => Outcome::ok(ws, None),
            Err(e) => Outcome::failed(Some(ws), None, e.to_string()),
        }
    }

    /// `installation` and `installation_repositories`: repos the App can no longer see are
    /// disabled (`enabled=0`, “GitHub App 不再能访问这个仓库”), and come back when re-added.
    async fn installation(
        &self,
        ws: &str,
        conns: &[String],
        event: &str,
        payload: &Value,
    ) -> Outcome {
        let action = payload["action"].as_str().unwrap_or_default();
        let ids = |k: &str| -> Vec<i64> {
            payload[k]
                .as_array()
                .map(|a| a.iter().filter_map(|r| r["id"].as_i64()).collect())
                .unwrap_or_default()
        };
        // (gh ids or all, enable?)
        let (target, enable): (Option<Vec<i64>>, bool) = match (event, action) {
            ("installation_repositories", "removed") => (Some(ids("repositories_removed")), false),
            ("installation_repositories", "added") => (Some(ids("repositories_added")), true),
            ("installation", "deleted" | "suspend") => (None, false),
            ("installation", "unsuspend" | "new_permissions_accepted") => (None, true),
            _ => {
                for c in conns {
                    self.forget_available(c);
                }
                return Outcome::ok(ws, None);
            }
        };
        for c in conns {
            self.forget_available(c);
        }
        let now = self.now();
        let res = self.store().transaction(|tx| {
            let mut fx = Effects::default();
            for c in conns {
                for mut r in repo::repos_of_connection(tx, c)? {
                    if target.as_ref().is_some_and(|t| !t.contains(&r.gh_id)) {
                        continue;
                    }
                    let lost = r.poll_state["lost_access"].as_bool() == Some(true);
                    if enable && !lost {
                        continue;
                    }
                    if !enable && !r.enabled && lost {
                        continue;
                    }
                    if enable {
                        r.enabled = true;
                        if let Some(o) = r.poll_state.as_object_mut() {
                            o.remove("lost_access");
                        }
                    } else {
                        r.enabled = false;
                        r.poll_state["lost_access"] = json!(true);
                    }
                    r.updated = now;
                    repo::update_repo(tx, &r)?;
                    fx.gh.insert(r.id.clone());
                }
            }
            let first = conns.first().map(String::as_str).unwrap_or_default();
            crate::after::emit(tx, ws, first, &mut fx)?;
            Ok::<_, ApiError>(fx)
        });
        match res {
            Ok(fx) => {
                if event == "installation" {
                    for c in conns {
                        if let Ok(cx) = self.connections().cx(ws, c) {
                            let h = if enable {
                                Health::Ok
                            } else {
                                Health::Auth("GitHub App 已经被卸载或暂停了".into())
                            };
                            cx.report(h).await;
                        }
                    }
                }
                let first = conns.first().cloned().unwrap_or_default();
                self.after_commit(ws, &first, fx).await;
                Outcome::ok(ws, None)
            }
            Err(e) => Outcome::failed(Some(ws), None, e.to_string()),
        }
    }

    /// Lists GitHub has to give for a check event: the check runs of a completed suite, the jobs of
    /// a workflow run. Errors are kept on the delivery; the payload's own data is still applied.
    /// `Err((message, transient))`: `transient` when GitHub did not answer or failed itself, so the
    /// delivery is tried again; otherwise the change is applied without what could not be read.
    async fn fetch(
        &self,
        repo: &RepoRow,
        event: &str,
        payload: &Value,
    ) -> Result<Fetched, (String, bool)> {
        let mut f = Fetched::default();
        let path = match event {
            "check_suite" if payload["check_suite"]["status"] == "completed" => {
                let sha = s(&payload["check_suite"]["head_sha"]);
                if sha.is_empty() {
                    return Ok(f);
                }
                format!("/repos/{}/commits/{sha}/check-runs", repo.full_name)
            }
            "workflow_run" if payload["workflow_run"]["status"] == "completed" => {
                let Some(run) = payload["workflow_run"]["id"].as_i64() else {
                    return Ok(f);
                };
                format!("/repos/{}/actions/runs/{run}/jobs", repo.full_name)
            }
            _ => return Ok(f),
        };
        let conn = self
            .conn(&repo.workspace_id, &repo.connection_id)
            .map_err(|e| (e.to_string(), false))?;
        let now = self.now();
        let transient = |e: &crate::api::GhError| {
            matches!(
                e,
                crate::api::GhError::Network
                    | crate::api::GhError::Unreachable
                    | crate::api::GhError::RateLimited { .. }
                    | crate::api::GhError::Status { status: 500.., .. }
            )
        };
        if event == "check_suite" {
            let list = conn
                .get_all(&path, Some("check_runs"))
                .await
                .map_err(|e| (format!("没取到检查列表：{e}"), transient(&e)))?;
            f.checks = list
                .iter()
                .filter_map(|v| check_from_run(repo, v, now))
                .collect();
        } else {
            let list = conn
                .get_all(&path, Some("jobs"))
                .await
                .map_err(|e| (format!("没取到工作流的 job：{e}"), transient(&e)))?;
            f.checks = list
                .iter()
                .filter_map(|v| check_from_job(repo, v, now))
                .collect();
        }
        Ok(f)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        event: &str,
        p: &Value,
        fetched: Fetched,
        now: i64,
        fx: &mut Effects,
    ) -> Result<(), ApiError> {
        let action = p["action"].as_str().unwrap_or_default();
        match event {
            "pull_request" => {
                let requested: Vec<String> = if action == "review_requested" {
                    p["requested_reviewer"]["login"]
                        .as_str()
                        .map(|l| vec![l.to_string()])
                        .unwrap_or_default()
                } else if action == "poll" {
                    p["pull_request"]["requested_reviewers"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|u| u["login"].as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                self.sync_pr(
                    tx,
                    repo,
                    &p["pull_request"],
                    action,
                    &requested,
                    &p["sender"],
                    None,
                    now,
                    fx,
                )?;
            }
            "pull_request_review" => {
                let Some(pr) = self.sync_pr(
                    tx,
                    repo,
                    &p["pull_request"],
                    "",
                    &[],
                    &p["sender"],
                    None,
                    now,
                    fx,
                )?
                else {
                    return Ok(());
                };
                self.review(tx, repo, pr, p, now, fx)?;
            }
            "check_run" => {
                let mut rows: Vec<CheckRow> = check_from_run(repo, &p["check_run"], now)
                    .into_iter()
                    .collect();
                rows.extend(fetched.checks);
                let branch = p["check_run"]["check_suite"]["head_branch"].as_str();
                self.write_checks(tx, repo, rows, branch, now, fx)?;
            }
            "check_suite" | "workflow_run" => {
                let key = if event == "check_suite" {
                    "check_suite"
                } else {
                    "workflow_run"
                };
                let sha = s(&p[key]["head_sha"]);
                if sha.is_empty() {
                    return Ok(());
                }
                let branch = p[key]["head_branch"].as_str();
                // Jobs of a workflow run land on their check run rows (same id), adding
                // `run_id`/`job_id`.
                let mut rows = fetched.checks;
                rows.retain(|r| r.head_sha == sha);
                self.write_checks_for(tx, repo, &sha, rows, branch, now, fx)?;
            }
            "status" => {
                let sha = s(&p["sha"]);
                let branch = p["branches"].as_array().and_then(|b| {
                    b.iter()
                        .filter_map(|x| x["name"].as_str())
                        .find(|n| *n == repo.default_branch)
                });
                if let Some(row) = check_from_status(repo, &sha, p, now) {
                    self.write_checks_for(tx, repo, &sha, vec![row], branch, now, fx)?;
                }
            }
            // The poll's view of one commit: its check runs and statuses.
            "poll_checks" => {
                let sha = s(&p["sha"]);
                let mut rows: Vec<CheckRow> = p["check_runs"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| check_from_run(repo, v, now))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(st) = p["statuses"].as_array() {
                    rows.extend(
                        st.iter()
                            .filter_map(|v| check_from_status(repo, &sha, v, now)),
                    );
                }
                self.write_checks_for(tx, repo, &sha, rows, None, now, fx)?;
            }
            "issues" => self.sync_issue(tx, repo, &p["issue"], &p["sender"], now, fx)?,
            "issue_comment" if action == "created" => self.comment(tx, repo, p, fx)?,
            "pull_request_review_comment" if action == "created" => {
                self.review_comment(tx, repo, p, fx)?
            }
            _ => {}
        }
        Ok(())
    }

    /// The bot login of the repo's connection (`<slug>[bot]`) and its kind.
    fn conn_login(&self, c: &rusqlite::Connection, repo: &RepoRow) -> (String, String) {
        repo::connection_label(c, &repo.connection_id)
            .ok()
            .flatten()
            .map(|(a, h, _)| (a, h))
            .unwrap_or_default()
    }

    /// Upserts a PR from GitHub's object; returns the row as stored. A PR not linked yet is
    /// matched to an item; `steve`: the item of a PR Steve just opened. Signals:
    /// merged/closed after being open, and review requests for one of the workspace's users.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sync_pr(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        v: &Value,
        action: &str,
        requested: &[String],
        sender: &Value,
        steve: Option<&str>,
        now: i64,
        fx: &mut Effects,
    ) -> Result<Option<PrRow>, ApiError> {
        let Some(number) = v["number"].as_i64() else {
            return Ok(None);
        };
        let prev = repo::pr_by_number(tx, &repo.id, number)?;
        // processed late, an older view than the one stored (a newer delivery came first) changes
        // nothing: GitHub's `updated_at` orders them
        if fx.late
            && let (Some(p), Some(t)) = (&prev, ts(&v["updated_at"]))
            && t < p.updated
        {
            return Ok(Some(p.clone()));
        }
        let head_sha = s(&v["head"]["sha"]);
        let merged = v["merged"].as_bool() == Some(true) || !v["merged_at"].is_null();
        let state = match v["state"].as_str() {
            Some("closed") if merged => "merged",
            Some("closed") => "closed",
            _ => "open",
        };
        let author = s(&v["user"]["login"]);
        let (adapter, bot) = self.conn_login(tx, repo);
        let opened_by = match &prev {
            _ if steve.is_some() => "steve".to_string(),
            Some(p) if p.opened_by == "steve" => "steve".to_string(),
            _ if adapter == crate::adapters::github_app::ID && author == bot && !bot.is_empty() => {
                "steve".into()
            }
            _ if repo::is_user_handle(tx, &repo.workspace_id, &author)? => "me".into(),
            _ => "other".into(),
        };
        let checks = repo::checks_for_sha(tx, &repo.id, &head_sha)?;
        let checks_state = checks_state(&checks).0.to_string();
        let mut review_state = prev
            .as_ref()
            .map(|p| p.review_state.clone())
            .unwrap_or_else(|| "none".into());
        if !requested.is_empty() && matches!(review_state.as_str(), "none" | "commented") {
            review_state = "requested".into();
        }
        if action == "review_request_removed"
            && review_state == "requested"
            && v["requested_reviewers"]
                .as_array()
                .is_some_and(|a| a.is_empty())
        {
            review_state = "none".into();
        }
        let updated = ts(&v["updated_at"]).unwrap_or(now);
        let mut row = PrRow {
            id: prev
                .as_ref()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| uid("ghp")),
            workspace_id: repo.workspace_id.clone(),
            repo_id: repo.id.clone(),
            number,
            gh_id: v["id"].as_i64().unwrap_or_default(),
            title: cut(&s(&v["title"]), 500),
            url: s(&v["html_url"]),
            state: state.into(),
            draft: v["draft"].as_bool().unwrap_or(false),
            head_ref: s(&v["head"]["ref"]),
            head_sha: head_sha.clone(),
            base_ref: s(&v["base"]["ref"]),
            author_login: author.clone(),
            author_account_id: prev.as_ref().and_then(|p| p.author_account_id.clone()),
            opened_by,
            body_excerpt: cut(v["body"].as_str().unwrap_or_default(), 2000),
            checks_state,
            review_state,
            mergeable: mergeable(&v["mergeable_state"])
                .or_else(|| prev.as_ref().and_then(|p| p.mergeable.clone())),
            item_id: prev.as_ref().and_then(|p| p.item_id.clone()),
            link_source: prev.as_ref().and_then(|p| p.link_source.clone()),
            created: prev
                .as_ref()
                .map(|p| p.created)
                .or_else(|| ts(&v["created_at"]))
                .unwrap_or(now),
            updated,
            merged_at: ts(&v["merged_at"]),
            closed_at: ts(&v["closed_at"]),
        };
        // Steve's own PR, else a match for one nobody linked (or unlinked) yet.
        let linked = match steve {
            Some(item) => Some((item.to_string(), "steve")),
            None if row.item_id.is_none() && row.link_source.is_none() => {
                link::match_item(tx, repo, &row)?
            }
            None => None,
        };
        if let Some((item, source)) = &linked {
            row.item_id = Some(item.clone());
            row.link_source = Some((*source).into());
        }
        if prev.as_ref() != Some(&row) {
            repo::upsert_pr(tx, &row)?;
            fx.gh.insert(row.id.clone());
            if let Some(item) = &row.item_id {
                tx.publish(ChangeEvent::Item { id: item.clone() });
            }
        }
        if let Some((item, source)) = &linked {
            let line = if *source == "steve" {
                format!("开了 PR #{}", row.number)
            } else {
                link::linked_line(repo, &row)
            };
            link::attach(tx, repo, &row, item, Some(&line), now)?;
        }
        // Closed: after being open here, or new but closed since the repo was linked.
        let was_open = match &prev {
            Some(p) => p.state == "open",
            None => row.closed_at.is_some_and(|t| t >= repo.created),
        };
        if row.state == "merged"
            && was_open
            && let Some(item) = &row.item_id
            && self.waits_for_merge(tx, repo, item)?
        {
            fx.finish
                .push((item.clone(), format!("PR #{} 已合并", row.number)));
        }
        if row.state != "open" && was_open {
            let (ty, verb) = if row.state == "merged" {
                ("github.pr_merged", "已合并")
            } else {
                ("github.pr_closed", "已关闭")
            };
            let ext = format!(
                "{}:{}:{}",
                ty.trim_start_matches("github."),
                row.id,
                row.closed_at.unwrap_or(updated)
            );
            let summary = format!(
                "{} · PR #{} {verb}：{}",
                repo.full_name, row.number, row.title
            );
            fx.signals.push(self.pr_signal(
                repo,
                &row,
                ty,
                ext,
                summary,
                actor(sender).or_else(|| actor(&v["user"])),
                row.closed_at.unwrap_or(updated),
                json!([]),
            ));
        }
        // Review requested from one of the workspace's users.
        let fresh = prev.is_none() && updated >= repo.created;
        let newly = prev.as_ref().is_some_and(|p| p.review_state != "requested") || fresh;
        if row.state == "open" && newly {
            for login in requested {
                if repo::is_user_handle(tx, &repo.workspace_id, login)? {
                    let summary = format!(
                        "{} 请你评审 PR #{}：{}",
                        row.author_login, row.number, row.title
                    );
                    let ext = format!(
                        "review_requested:{}:{}:{}",
                        row.id,
                        login.to_lowercase(),
                        row.head_sha
                    );
                    let mut ev = self.pr_signal(
                        repo,
                        &row,
                        "github.review_requested",
                        ext,
                        summary,
                        actor(sender).or_else(|| actor(&v["user"])),
                        updated,
                        json!([]),
                    );
                    ev.group_key = None;
                    fx.signals.push(ev);
                }
            }
        }
        Ok(Some(row))
    }

    #[allow(clippy::too_many_arguments)]
    fn pr_signal(
        &self,
        repo: &RepoRow,
        pr: &PrRow,
        ty: &str,
        external_id: String,
        summary: String,
        actor: Option<Actor>,
        occurred: i64,
        failed: Value,
    ) -> SignalEvent {
        SignalEvent {
            type_key: ty.into(),
            external_id,
            occurred,
            thread_key: Some(format!("pr:{}", pr.id)),
            group_key: Some(format!("repo:{}", repo.id)),
            scope_ext_id: Some(repo.full_name.clone()),
            actor,
            summary: cut(&summary, 200),
            payload: json!({
                "repo": repo.full_name, "number": pr.number, "title": pr.title, "url": pr.url,
                "head_ref": pr.head_ref, "head_sha": pr.head_sha, "item_id": pr.item_id,
                "failed": failed, "ci_fail": Settings::of(&repo.settings).ci_fail,
            }),
        }
    }

    /// `pull_request_review`: the review state, and `github.review` on PRs Steve opened or that
    /// belong to an item.
    fn review(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        mut pr: PrRow,
        p: &Value,
        now: i64,
        fx: &mut Effects,
    ) -> Result<(), ApiError> {
        let r = &p["review"];
        let state = r["state"].as_str().unwrap_or_default().to_lowercase();
        let new_state = match (p["action"].as_str(), state.as_str()) {
            (Some("dismissed"), _) | (_, "dismissed") => "none",
            (_, "approved") => "approved",
            (_, "changes_requested") => "changes_requested",
            (_, "commented") if pr.review_state == "none" => "commented",
            _ => pr.review_state.as_str(),
        }
        .to_string();
        if new_state != pr.review_state {
            pr.review_state = new_state;
            pr.updated = pr.updated.max(now);
            repo::upsert_pr(tx, &pr)?;
            fx.gh.insert(pr.id.clone());
            if let Some(item) = &pr.item_id {
                tx.publish(ChangeEvent::Item { id: item.clone() });
            }
        }
        if p["action"].as_str() != Some("submitted") {
            return Ok(());
        }
        let (_, bot) = self.conn_login(tx, repo);
        let reviewer = s(&r["user"]["login"]);
        let watched = pr.opened_by == "steve" || pr.item_id.is_some();
        let body = r["body"].as_str().unwrap_or_default().trim();
        let verb = match state.as_str() {
            "approved" => "批准了",
            "changes_requested" => "要求修改",
            "commented" if !body.is_empty() => "评论了",
            _ => return Ok(()),
        };
        if watched && reviewer != bot {
            let ext = format!("review:{}", r["id"].as_i64().unwrap_or_default());
            supplement(
                fx,
                &pr,
                &r["author_association"],
                &ext,
                format!("{reviewer} 在 PR #{} {verb}", pr.number),
                body,
            );
            let summary = format!("{reviewer} {verb} PR #{}", pr.number);
            let at = ts(&r["submitted_at"]).unwrap_or(now);
            let mut ev = self.pr_signal(
                repo,
                &pr,
                "github.review",
                ext,
                summary,
                actor(&r["user"]),
                at,
                json!([]),
            );
            ev.group_key = None;
            fx.signals.push(ev);
        }
        Ok(())
    }

    /// `issue_comment.created` on a PR linked to an item, not by Steve: `github.pr_comment`.
    /// Comment bodies are not stored.
    fn comment(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        p: &Value,
        fx: &mut Effects,
    ) -> Result<(), ApiError> {
        if p["issue"]["pull_request"].is_null() {
            return Ok(());
        }
        let Some(n) = p["issue"]["number"].as_i64() else {
            return Ok(());
        };
        let Some(pr) = repo::pr_by_number(tx, &repo.id, n)? else {
            return Ok(());
        };
        let (_, bot) = self.conn_login(tx, repo);
        let who = s(&p["comment"]["user"]["login"]);
        if pr.item_id.is_none() || who == bot {
            return Ok(());
        }
        let body = p["comment"]["body"].as_str().unwrap_or_default();
        let summary = format!("{who} 评论了 PR #{}：{}", pr.number, cut(body.trim(), 60));
        let ext = format!(
            "comment:{}",
            p["comment"]["id"].as_i64().unwrap_or_default()
        );
        supplement(
            fx,
            &pr,
            &p["comment"]["author_association"],
            &ext,
            format!("{who} 在 PR #{} 评论", pr.number),
            body.trim(),
        );
        let at = ts(&p["comment"]["created_at"]).unwrap_or_else(|| self.now());
        let mut ev = self.pr_signal(
            repo,
            &pr,
            "github.pr_comment",
            ext,
            summary,
            actor(&p["comment"]["user"]),
            at,
            json!([]),
        );
        ev.group_key = None;
        fx.signals.push(ev);
        Ok(())
    }

    /// `pull_request_review_comment.created` (a comment on a line) on a PR linked to an item, not by
    /// Steve: `github.review_comment`, and the comment for the item.
    fn review_comment(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        p: &Value,
        fx: &mut Effects,
    ) -> Result<(), ApiError> {
        let Some(n) = p["pull_request"]["number"].as_i64() else {
            return Ok(());
        };
        let Some(pr) = repo::pr_by_number(tx, &repo.id, n)? else {
            return Ok(());
        };
        let (_, bot) = self.conn_login(tx, repo);
        let c = &p["comment"];
        let who = s(&c["user"]["login"]);
        if pr.item_id.is_none() || who == bot {
            return Ok(());
        }
        let body = c["body"].as_str().unwrap_or_default().trim();
        let place = match (c["path"].as_str(), c["line"].as_i64()) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            (Some(f), None) => f.to_string(),
            _ => String::new(),
        };
        let ext = format!("review_comment:{}", c["id"].as_i64().unwrap_or_default());
        supplement(
            fx,
            &pr,
            &c["author_association"],
            &ext,
            format!("{who} 在 PR #{} 的 {place} 评论", pr.number),
            body,
        );
        let at = ts(&c["created_at"]).unwrap_or_else(|| self.now());
        let mut ev = self.pr_signal(
            repo,
            &pr,
            "github.review_comment",
            ext,
            format!(
                "{who} 在 {place} 评论了 PR #{}：{}",
                pr.number,
                cut(body, 60)
            ),
            actor(&c["user"]),
            at,
            json!([]),
        );
        ev.group_key = None;
        fx.signals.push(ev);
        Ok(())
    }

    /// Issues (not PRs listed as issues). `github.issue_opened` for issues opened since the repo
    /// was linked.
    fn sync_issue(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        v: &Value,
        sender: &Value,
        now: i64,
        fx: &mut Effects,
    ) -> Result<(), ApiError> {
        let Some(number) = v["number"].as_i64() else {
            return Ok(());
        };
        if !v["pull_request"].is_null() {
            return Ok(());
        }
        let prev = repo::issue_by_number(tx, &repo.id, number)?;
        let labels: Vec<String> = v["labels"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let created = ts(&v["created_at"]).unwrap_or(now);
        let row = IssueRow {
            id: prev
                .as_ref()
                .map(|i| i.id.clone())
                .unwrap_or_else(|| uid("ghi")),
            workspace_id: repo.workspace_id.clone(),
            repo_id: repo.id.clone(),
            number,
            title: cut(&s(&v["title"]), 500),
            url: s(&v["html_url"]),
            state: v["state"].as_str().unwrap_or("open").into(),
            labels,
            author_login: s(&v["user"]["login"]),
            author_account_id: prev.as_ref().and_then(|i| i.author_account_id.clone()),
            item_id: prev.as_ref().and_then(|i| i.item_id.clone()),
            created: prev.as_ref().map(|i| i.created).unwrap_or(created),
            updated: ts(&v["updated_at"]).unwrap_or(now),
        };
        if prev.as_ref() != Some(&row) {
            repo::upsert_issue(tx, &row)?;
            fx.gh.insert(row.id.clone());
        }
        if prev.is_none() && row.state == "open" && created >= repo.created {
            fx.signals.push(SignalEvent {
                type_key: "github.issue_opened".into(),
                external_id: format!("issue_opened:{}", row.id),
                occurred: created,
                thread_key: Some(format!("issue:{}", row.id)),
                group_key: Some(format!("repo:{}", repo.id)),
                scope_ext_id: Some(repo.full_name.clone()),
                actor: actor(sender).or_else(|| actor(&v["user"])),
                summary: cut(
                    &format!("{} · #{} {}", repo.full_name, row.number, row.title),
                    200,
                ),
                payload: json!({
                    "repo": repo.full_name, "number": row.number, "title": row.title,
                    "url": row.url, "head_ref": null, "head_sha": null, "item_id": row.item_id,
                    "failed": [],
                }),
            });
        }
        Ok(())
    }

    /// Check rows that may span commits (a `check_run` event plus whatever came with it).
    fn write_checks(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        rows: Vec<CheckRow>,
        branch: Option<&str>,
        now: i64,
        fx: &mut Effects,
    ) -> Result<(), ApiError> {
        let mut by: BTreeMap<String, Vec<CheckRow>> = BTreeMap::new();
        for r in rows {
            by.entry(r.head_sha.clone()).or_default().push(r);
        }
        for (sha, rows) in by {
            self.write_checks_for(tx, repo, &sha, rows, branch, now, fx)?;
        }
        Ok(())
    }

    /// Writes one commit's checks, recomputes `checks_state` of its PRs, and signals a commit whose
    /// checks just settled: on each open PR with that head, else on the default branch.
    #[allow(clippy::too_many_arguments)]
    fn write_checks_for(
        &self,
        tx: &Tx,
        repo: &RepoRow,
        sha: &str,
        rows: Vec<CheckRow>,
        branch: Option<&str>,
        now: i64,
        fx: &mut Effects,
    ) -> Result<(), ApiError> {
        if sha.is_empty() {
            return Ok(());
        }
        let before = repo::checks_for_sha(tx, &repo.id, sha)?;
        for r in &rows {
            // Unchanged rows are not rewritten (no churn on every poll).
            let same = before.iter().any(|b| {
                b.kind == r.kind
                    && b.ext_id == r.ext_id
                    && b.status == r.status
                    && b.conclusion == r.conclusion
                    && b.name == r.name
                    && (r.job_id.is_none() || b.job_id == r.job_id)
            });
            if !same {
                repo::upsert_check(tx, r)?;
            }
        }
        let after = repo::checks_for_sha(tx, &repo.id, sha)?;
        let state = checks_state(&after).0;
        let outcome = settled(&after);
        let fresh = outcome.is_some() && outcome != settled(&before);
        let failed = failed_list(&after);
        let names: Vec<String> = failed
            .iter()
            .filter_map(|f| f["name"].as_str().map(String::from))
            .collect();
        // The rerun that failed is a new failure: key it by the failing runs.
        let fail_key = latest_checks(&after)
            .into_iter()
            .filter(|c| {
                c.conclusion
                    .as_deref()
                    .is_some_and(|x| FAILING.contains(&x))
            })
            .map(|c| c.ext_id.clone())
            .collect::<Vec<_>>()
            .join(",");
        let ext = |scope: &str| match outcome {
            Some("failure") => format!("checks:{scope}:{sha}:failure:{fail_key}"),
            _ => format!("checks:{scope}:{sha}:success"),
        };
        let prs: Vec<PrRow> = repo::prs_with_head(tx, &repo.id, sha)?
            .into_iter()
            .filter(|p| p.state == "open")
            .collect();
        for mut pr in prs.clone() {
            if pr.checks_state != state {
                pr.checks_state = state.into();
                pr.updated = pr.updated.max(now);
                repo::upsert_pr(tx, &pr)?;
                if let Some(item) = &pr.item_id {
                    tx.publish(ChangeEvent::Item { id: item.clone() });
                }
            }
            fx.gh.insert(pr.id.clone());
            if fresh {
                let (ty, summary) = match outcome {
                    Some("failure") => (
                        "github.check_failed",
                        format!(
                            "{} · PR #{} 检查失败：{}",
                            repo.full_name,
                            pr.number,
                            names.join("、")
                        ),
                    ),
                    _ => ("github.check_passed", format!("PR #{} 检查通过", pr.number)),
                };
                fx.signals.push(self.pr_signal(
                    repo,
                    &pr,
                    ty,
                    ext(&pr.id),
                    summary,
                    None,
                    now,
                    json!(failed),
                ));
            }
        }
        if prs.is_empty() && fresh && branch == Some(repo.default_branch.as_str()) {
            let b = &repo.default_branch;
            let (ty, summary) = match outcome {
                Some("failure") => (
                    "github.check_failed",
                    format!("{} · {b} 检查失败：{}", repo.full_name, names.join("、")),
                ),
                _ => (
                    "github.check_passed",
                    format!("{} · {b} 检查通过", repo.full_name),
                ),
            };
            fx.signals.push(SignalEvent {
                type_key: ty.into(),
                external_id: ext(&format!("{}:{b}", repo.id)),
                occurred: now,
                thread_key: Some(format!("branch:{}:{b}", repo.id)),
                group_key: Some(format!("repo:{}", repo.id)),
                scope_ext_id: Some(repo.full_name.clone()),
                actor: None,
                summary: cut(&summary, 200),
                payload: json!({
                    "repo": repo.full_name, "number": null, "title": null,
                    "url": format!("https://github.com/{}/commit/{sha}", repo.full_name),
                    "head_ref": b, "head_sha": sha, "item_id": null, "failed": failed,
                }),
            });
            fx.gh.insert(repo.id.clone());
        }
        Ok(())
    }
}

/// Who may speak to an item through its PR: the repository's owner, members and collaborators.
/// Anyone else's comment stays a signal (as an IM thread passes on only its item's raiser and you).
const SAY_IN_REPO: [&str; 3] = ["OWNER", "MEMBER", "COLLABORATOR"];

/// The longest comment passed on to an item, in characters.
const SUPPLEMENT_MAX: usize = 4000;

/// Queues `body` for the PR's item when its author has a say in the repository: quoted, as text from
/// outside is for the agent.
fn supplement(
    fx: &mut Effects,
    pr: &PrRow,
    association: &Value,
    ext: &str,
    who: String,
    body: &str,
) {
    let (Some(item), Some(a)) = (&pr.item_id, association.as_str()) else {
        return;
    };
    if body.is_empty() || !SAY_IN_REPO.contains(&a) {
        return;
    }
    fx.supplements.push(crate::after::PrSupplement {
        item: item.clone(),
        external_id: ext.into(),
        text: format!(
            "GitHub：{who}（{}）：\n{}",
            pr.url,
            steve_modules::outbound::quote_untrusted(&cut(body, SUPPLEMENT_MAX))
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, ext: &str, status: &str, conclusion: Option<&str>) -> CheckRow {
        CheckRow {
            repo_id: "ghr_1".into(),
            workspace_id: "default".into(),
            head_sha: "abc".into(),
            kind: "check_run".into(),
            ext_id: ext.into(),
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.map(String::from),
            url: String::new(),
            run_id: None,
            job_id: None,
            summary: String::new(),
            started: None,
            completed: None,
            updated: 0,
        }
    }

    #[test]
    fn checks_state_follows_the_spec() {
        assert_eq!(checks_state(&[]), ("none", true));
        let ok = row("build", "1", "completed", Some("success"));
        let skip = row("lint", "2", "completed", Some("skipped"));
        let neutral = row("docs", "3", "completed", Some("neutral"));
        assert_eq!(
            checks_state(&[ok.clone(), skip.clone(), neutral.clone()]),
            ("success", true)
        );
        let running = row("e2e", "4", "in_progress", None);
        assert_eq!(
            checks_state(&[ok.clone(), running.clone()]),
            ("pending", false)
        );
        for bad in FAILING {
            let f = row("e2e", "5", "completed", Some(bad));
            assert_eq!(
                checks_state(&[ok.clone(), f.clone()]),
                ("failure", true),
                "{bad}"
            );
            // failure wins over pending, but is not settled yet
            assert_eq!(
                checks_state(&[f, row("x", "6", "queued", None)]),
                ("failure", false)
            );
        }
        let status_pending = CheckRow {
            kind: "status".into(),
            ..row("ci/legacy", "ci/legacy", "in_progress", None)
        };
        assert_eq!(checks_state(&[ok, status_pending]), ("pending", false));
    }

    #[test]
    fn a_rerun_replaces_the_failed_run() {
        let failed = row("build", "10", "completed", Some("failure"));
        let rerun = row("build", "11", "completed", Some("success"));
        assert_eq!(checks_state(&[failed.clone(), rerun]), ("success", true));
        let queued = row("build", "12", "queued", None);
        assert_eq!(checks_state(&[failed, queued]), ("pending", false));
    }

    #[test]
    fn actions_ids_from_urls() {
        assert_eq!(
            actions_ids("https://github.com/o/r/actions/runs/8001/job/9001"),
            (Some(8001), Some(9001))
        );
        assert_eq!(actions_ids("https://ci.example/build/1"), (None, None));
    }

    #[test]
    fn mergeable_states() {
        assert_eq!(mergeable(&json!("clean")).as_deref(), Some("clean"));
        assert_eq!(mergeable(&json!("dirty")).as_deref(), Some("dirty"));
        assert_eq!(mergeable(&json!("behind")).as_deref(), Some("blocked"));
        assert_eq!(mergeable(&json!("unknown")).as_deref(), Some("unknown"));
        assert_eq!(mergeable(&Value::Null), None);
    }
}

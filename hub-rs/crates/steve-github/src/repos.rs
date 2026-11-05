//! Linked repositories: what a connection can see, linking a repo (with a
//! repository hook for PATs, else polling), its settings, and the read endpoints the GitHub page uses
//! (`GET /github`, PRs, issues, deliveries). Routes are relative to `/api`, behind the `Caller` layer.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rand::RngExt;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use steve_modules::vault::VaultResult;
use steve_modules::{ApiError, Body, Caller, Modules};
use steve_store::{base, uid};
use zeroize::Zeroizing;

use crate::GitHub;
use crate::adapters::{github_app, github_pat};
use crate::api::GhError;
use crate::hooks::HOOK_SECRET_FIELD;
use crate::repo::{self, CheckRow, PrRow, RepoRow};
use crate::sync::latest_checks;

/// How long the list of repos a connection can see is reused.
const AVAILABLE_TTL_MS: i64 = 5 * 60_000;

/// Events of a PAT repository hook: the App's list without the App-only
/// `installation_repositories`, plus `status` for legacy CI.
pub const REPO_HOOK_EVENTS: [&str; 8] = [
    "pull_request",
    "pull_request_review",
    "check_suite",
    "check_run",
    "workflow_run",
    "issues",
    "issue_comment",
    "status",
];

/// The repo settings, with defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `ask_pr|none`.
    pub after_accept: String,
    /// `accepted|merged`.
    pub finish_on: String,
    /// `squash|merge|rebase`.
    pub merge_method: String,
    pub delete_branch: bool,
    /// `supplement|signal`.
    pub ci_fail: String,
    pub draft_pr: bool,
}

const ENUMS: [(&str, &[&str]); 4] = [
    ("after_accept", &["ask_pr", "none"]),
    ("finish_on", &["accepted", "merged"]),
    ("merge_method", &["squash", "merge", "rebase"]),
    ("ci_fail", &["supplement", "signal"]),
];
const BOOLS: [&str; 2] = ["delete_branch", "draft_pr"];

impl Settings {
    /// From `gh_repos.settings`; missing or odd values take the default.
    pub fn of(v: &Value) -> Self {
        let e = |k: &str, d: &str| {
            let allowed = ENUMS
                .iter()
                .find(|(n, _)| *n == k)
                .map(|x| x.1)
                .unwrap_or(&[]);
            v[k].as_str()
                .filter(|x| allowed.contains(x))
                .unwrap_or(d)
                .to_string()
        };
        Self {
            after_accept: e("after_accept", "ask_pr"),
            finish_on: e("finish_on", "accepted"),
            merge_method: e("merge_method", "squash"),
            delete_branch: v["delete_branch"].as_bool().unwrap_or(true),
            ci_fail: e("ci_fail", "supplement"),
            draft_pr: v["draft_pr"].as_bool().unwrap_or(false),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "after_accept": self.after_accept, "finish_on": self.finish_on,
            "merge_method": self.merge_method, "delete_branch": self.delete_branch,
            "ci_fail": self.ci_fail, "draft_pr": self.draft_pr,
        })
    }

    /// Checks a patch key by key (`400 参数不对：settings.<key>`) and returns the merged settings.
    pub fn patched(current: &Value, patch: &Map<String, Value>) -> Result<Value, ApiError> {
        let mut bad = Vec::new();
        let mut out = Self::of(current).to_json();
        for (k, v) in patch {
            let ok = match ENUMS.iter().find(|(n, _)| n == k) {
                Some((_, allowed)) => v.as_str().is_some_and(|s| allowed.contains(&s)),
                None => BOOLS.contains(&k.as_str()) && v.is_boolean(),
            };
            if ok {
                out[k] = v.clone();
            } else {
                bad.push(format!("settings.{k}"));
            }
        }
        if bad.is_empty() {
            Ok(out)
        } else {
            Err(ApiError::Invalid(bad))
        }
    }
}

/// A repo a connection can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub gh_id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub default_branch: String,
}

fn available_of(v: &Value) -> Option<Available> {
    let full = v["full_name"].as_str()?;
    let (owner, name) = full.split_once('/')?;
    Some(Available {
        gh_id: v["id"].as_i64()?,
        owner: owner.into(),
        name: name.into(),
        full_name: full.into(),
        private: v["private"].as_bool().unwrap_or(true),
        default_branch: v["default_branch"].as_str().unwrap_or("main").into(),
    })
}

#[derive(Default)]
pub(crate) struct AvailableCache(Mutex<HashMap<String, (i64, Vec<Available>)>>);

impl AvailableCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (i64, Vec<Available>)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `owner/name` of a GitHub remote URL (https, `git@github.com:`, `ssh://`), lowercased.
pub fn parse_remote(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = u
        .strip_prefix("https://github.com/")
        .or_else(|| u.strip_prefix("http://github.com/"))
        .or_else(|| u.strip_prefix("git@github.com:"))
        .or_else(|| u.strip_prefix("ssh://git@github.com/"))
        .or_else(|| u.strip_prefix("git://github.com/"))
        .or_else(|| {
            // https://user@github.com/o/r
            u.strip_prefix("https://")
                .and_then(|r| r.split_once('@'))
                .and_then(|(_, r)| r.strip_prefix("github.com/"))
        })?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (o, n) = rest.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    };
    (ok(o) && ok(n) && !n.contains('/')).then(|| format!("{o}/{n}").to_lowercase())
}

/// `git -C <path> remote get-url origin`, parsed. `None` on any problem.
fn project_remote(path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "origin"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_remote(&String::from_utf8_lossy(&out.stdout))
}

fn gh_err(e: GhError) -> ApiError {
    match e {
        GhError::Status { status: 404, .. } => ApiError::not_found("仓库"),
        e => ApiError::rule(e.to_string()),
    }
}

/// A check row as the web shows it: `ok|fail|pending|skipped`.
pub fn check_state(c: &CheckRow) -> &'static str {
    if c.status != "completed" {
        return "pending";
    }
    match c.conclusion.as_deref() {
        Some("success") => "ok",
        Some("skipped" | "neutral") => "skipped",
        Some(_) => "fail",
        None => "pending",
    }
}

/// `Pr`.
pub fn pr_out(c: &rusqlite::Connection, repo: &RepoRow, p: &PrRow) -> rusqlite::Result<Value> {
    let rows = repo::checks_for_sha(c, &repo.id, &p.head_sha)?;
    let checks: Vec<Value> = latest_checks(&rows)
        .into_iter()
        .map(|k| json!({"name": k.name, "state": check_state(k), "url": k.url, "job_id": k.job_id}))
        .collect();
    let item = match &p.item_id {
        Some(id) => repo::item_brief(c, &p.workspace_id, id)?
            .map(|(title, status)| json!({"id": id, "title": title, "status": status})),
        None => None,
    };
    Ok(json!({
        "id": p.id, "repo": repo.full_name, "number": p.number, "title": p.title, "url": p.url,
        "state": p.state, "draft": p.draft, "head_ref": p.head_ref, "head_sha": p.head_sha,
        "checks_state": p.checks_state, "checks": checks, "review_state": p.review_state,
        "mergeable": p.mergeable, "item": item, "link_source": p.link_source,
        "opened_by": p.opened_by, "updated": p.updated,
    }))
}

/// `Repo`.
pub fn repo_out(c: &rusqlite::Connection, r: &RepoRow) -> rusqlite::Result<Value> {
    let (adapter, account, status) =
        repo::connection_label(c, &r.connection_id)?.unwrap_or_default();
    let project = match &r.project_id {
        Some(p) => repo::project(c, &r.workspace_id, p)?.map(|x| json!({"id": p, "name": x.name})),
        None => None,
    };
    let (open_prs, open_issues) = repo::open_counts(c, &r.id)?;
    let lost = r.poll_state["lost_access"].as_bool() == Some(true);
    Ok(json!({
        "id": r.id, "full_name": r.full_name, "private": r.private, "default_branch": r.default_branch,
        "connection": {"id": r.connection_id, "kind": if adapter == github_app::ID { "app" } else { "pat" }, "account": account},
        "project": project, "ingest": r.ingest,
        "healthy": r.enabled && status == "ok",
        "problem": if lost { json!(steve_i18n::t!("github.repo.lost_access")) } else { Value::Null },
        "last_delivery": r.last_delivery, "settings": Settings::of(&r.settings).to_json(),
        "open_prs": open_prs, "open_issues": open_issues, "enabled": r.enabled, "revision": r.revision,
    }))
}

impl GitHub {
    /// A GitHub connection the caller may use (`404 没有这个连接` otherwise): `(adapter, account)`.
    fn own_connection(&self, who: &Caller, id: &str) -> VaultResult<String> {
        let c = self.connections().get(who, id)?;
        if c.adapter != github_app::ID && c.adapter != github_pat::ID {
            return Err(ApiError::not_found("连接").into());
        }
        Ok(c.adapter)
    }

    /// A repo of the caller's workspace (`404 没有这个仓库`).
    pub fn own_repo(&self, ws: &str, id: &str) -> VaultResult<RepoRow> {
        Ok(self
            .store()
            .reader(|c| repo::repo_get(c, id).map_err(ApiError::from))?
            .filter(|r| r.workspace_id == ws)
            .ok_or_else(|| ApiError::not_found("仓库"))?)
    }

    pub(crate) fn forget_available(&self, connection: &str) {
        self.0.available.lock().remove(connection);
    }

    /// The repos a connection can see (cached 5 minutes unless `fresh`). App:
    /// `GET /installation/repositories`; PAT: `GET /user/repos?affiliation=owner,collaborator`.
    pub async fn available(
        &self,
        ws: &str,
        connection: &str,
        fresh: bool,
    ) -> VaultResult<Vec<Available>> {
        let now = self.now();
        if !fresh
            && let Some((at, list)) = self.0.available.lock().get(connection)
            && now - at < AVAILABLE_TTL_MS
        {
            return Ok(list.clone());
        }
        let conn = self.conn(ws, connection)?;
        let list = if conn.cx().adapter() == github_app::ID {
            conn.get_all("/installation/repositories", Some("repositories"))
                .await
        } else {
            conn.get_all("/user/repos?affiliation=owner,collaborator", None)
                .await
        }
        .map_err(gh_err)?;
        let list: Vec<Available> = list.iter().filter_map(available_of).collect();
        self.0
            .available
            .lock()
            .insert(connection.into(), (now, list.clone()));
        Ok(list)
    }

    /// `GET /github`.
    pub async fn overview(&self, who: &Caller) -> VaultResult<Value> {
        let ws = who.ws.clone();
        let conns: Vec<_> = self
            .connections()
            .list(who)?
            .into_iter()
            .filter(|c| c.adapter == github_app::ID || c.adapter == github_pat::ID)
            .collect();
        let (apps, conns_out, repos, projects) = self.store().reader(|c| {
            let apps: Vec<Value> = repo::apps(c, &ws)?
                .iter()
                .map(|a| json!({"slug": a.slug, "owner": a.owner, "html_url": a.html_url}))
                .collect();
            let mut co = Vec::new();
            for x in &conns {
                let account = repo::connection_label(c, &x.id)?
                    .map(|l| l.1)
                    .unwrap_or_default();
                co.push(json!({
                    "id": x.id, "kind": if x.adapter == github_app::ID { "app" } else { "pat" },
                    "account": account, "status": x.status,
                }));
            }
            let rows = repo::repos(c, &ws)?;
            let mut repos = Vec::new();
            for r in &rows {
                repos.push(repo_out(c, r)?);
            }
            let projects: Vec<(String, String)> = repo::git_projects(c, &ws)?
                .into_iter()
                .filter(|(id, _, _)| !rows.iter().any(|r| r.project_id.as_deref() == Some(id)))
                .map(|(id, _, path)| (id, path))
                .collect();
            Ok::<_, ApiError>((apps, co, (rows, repos), projects))
        })?;
        let (rows, repos) = repos;
        let mut suggestions = Vec::new();
        if !projects.is_empty() {
            let remotes = tokio::task::spawn_blocking(move || {
                projects
                    .into_iter()
                    .filter_map(|(id, path)| project_remote(&path).map(|r| (id, r)))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            if !remotes.is_empty() {
                for c in &conns {
                    let Ok(list) = self.available(&ws, &c.id, false).await else {
                        continue;
                    };
                    for (project, full) in &remotes {
                        if let Some(a) = list.iter().find(|a| a.full_name.to_lowercase() == *full)
                            && !rows.iter().any(|r| r.gh_id == a.gh_id)
                            && !suggestions
                                .iter()
                                .any(|s: &Value| s["project_id"] == json!(project))
                        {
                            suggestions.push(json!({
                                "project_id": project, "full_name": a.full_name,
                                "gh_id": a.gh_id, "connection_id": c.id,
                            }));
                        }
                    }
                }
            }
        }
        Ok(
            json!({"apps": apps, "connections": conns_out, "repos": repos, "suggestions": suggestions}),
        )
    }

    /// `GET /github/available?connection=`.
    pub async fn available_out(&self, who: &Caller, connection: &str) -> VaultResult<Value> {
        self.own_connection(who, connection)?;
        let list = self.available(&who.ws, connection, false).await?;
        let linked: Vec<i64> = self
            .store()
            .reader(|c| repo::repos(c, &who.ws).map_err(ApiError::from))?
            .iter()
            .map(|r| r.gh_id)
            .collect();
        Ok(Value::Array(
            list.iter()
                .map(|a| {
                    json!({"gh_id": a.gh_id, "full_name": a.full_name, "private": a.private,
                           "linked": linked.contains(&a.gh_id)})
                })
                .collect(),
        ))
    }

    /// `POST /github/repos`. PAT: a repository hook if the token may create one, else polling.
    pub async fn link_repo(
        &self,
        who: &Caller,
        connection: &str,
        gh_id: i64,
        project: Option<&str>,
    ) -> VaultResult<Value> {
        let adapter = self.own_connection(who, connection)?;
        let ws = who.ws.clone();
        let pre = |gh: &GitHub| {
            gh.store().reader(|c| {
                if let Some(p) = project {
                    check_project(c, &ws, p, None)?;
                }
                if repo::repo_by_gh(c, &ws, gh_id)
                    .map_err(ApiError::from)?
                    .is_some()
                {
                    return Err(
                        ApiError::conflict(steve_i18n::t!("github.repo.already_linked")).into(),
                    );
                }
                Ok::<_, steve_modules::vault::VaultError>(())
            })
        };
        pre(self)?;
        let mut a = self
            .available(&ws, connection, false)
            .await?
            .into_iter()
            .find(|a| a.gh_id == gh_id);
        if a.is_none() {
            a = self
                .available(&ws, connection, true)
                .await?
                .into_iter()
                .find(|a| a.gh_id == gh_id);
        }
        let a = a.ok_or_else(|| ApiError::not_found("仓库"))?;
        let now = self.now();
        let mut row = RepoRow {
            id: uid("ghr"),
            workspace_id: ws.clone(),
            connection_id: connection.into(),
            gh_id,
            owner: a.owner.clone(),
            name: a.name.clone(),
            full_name: a.full_name.clone(),
            default_branch: a.default_branch.clone(),
            private: a.private,
            project_id: project.map(String::from),
            ingest: "app".into(),
            hook_id: None,
            last_delivery: None,
            poll_state: json!({}),
            settings: json!({}),
            enabled: true,
            revision: 1,
            created: now,
            updated: now,
        };
        let mut hook_item = None;
        if adapter == github_pat::ID {
            match self.create_hook(&ws, &who.me, connection, &a).await {
                Ok((hook, item)) => {
                    row.ingest = "repo_hook".into();
                    row.hook_id = Some(hook);
                    row.poll_state = json!({"hook_item": item});
                    hook_item = Some(item);
                }
                Err(e) => {
                    tracing::info!(repo = %a.full_name, error = %e, "no repository hook, polling");
                    row.ingest = "poll".into();
                }
            }
        }
        let res = self.store().transaction(|tx| {
            if let Some(p) = project {
                check_project(tx, &ws, p, None)?;
            }
            if repo::repo_by_gh(tx, &ws, gh_id).map_err(ApiError::from)?.is_some() {
                return Err(ApiError::conflict(steve_i18n::t!("github.repo.already_linked")).into());
            }
            repo::insert_repo(tx, &row).map_err(ApiError::from)?;
            base::audit(
                tx,
                &ws,
                &who.me,
                "github.repo_link",
                &row.id,
                &json!({"full_name": row.full_name, "connection": connection, "project": project, "ingest": row.ingest}),
            )
            .map_err(ApiError::from)?;
            Ok::<_, steve_modules::vault::VaultError>(repo_out(tx, &row).map_err(ApiError::from)?)
        });
        if res.is_err() {
            // Lost a race: undo the hook.
            self.remove_hook(&ws, &who.me, &row, hook_item.as_deref())
                .await;
        }
        let out = res?;
        self.notifier().gh(&ws, &row.id);
        // The first repo of the workspace brings the “PR 检查失败” flow.
        match self.install_template(&ws, &who.me) {
            Ok(Some(f)) => tracing::info!(flow = %f, "github flow template installed"),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "github flow template not installed"),
        }
        tracing::info!(repo = %row.full_name, ingest = %row.ingest, "github repo linked");
        Ok(out)
    }

    /// Creates the PAT repository hook and its secret item: `(hook id, vault item)`.
    async fn create_hook(
        &self,
        ws: &str,
        me: &str,
        connection: &str,
        a: &Available,
    ) -> Result<(i64, String), String> {
        // checked before the secret item exists, so a refusal leaves nothing behind
        let conn = self
            .conn_for_write(ws, connection)
            .map_err(|e| e.to_string())?;
        let bytes: [u8; 32] = rand::rng().random();
        let secret = Zeroizing::new(hex::encode(bytes));
        let (gh, w, m, title, s) = (
            self.clone(),
            ws.to_string(),
            me.to_string(),
            format!("GitHub Webhook {}", a.full_name),
            secret.clone(),
        );
        let item = tokio::task::spawn_blocking(move || {
            gh.connections().vault().create_credential(
                &w,
                &m,
                &title,
                None,
                vec![(HOOK_SECRET_FIELD.into(), s)],
            )
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let body = json!({
            "name": "web", "active": true, "events": REPO_HOOK_EVENTS,
            "config": {
                "url": format!("{}/hooks/{connection}", self.public_url()),
                "content_type": "json", "insecure_ssl": "0", "secret": secret.as_str(),
            },
        });
        match conn
            .post(&format!("/repos/{}/hooks", a.full_name), &body)
            .await
        {
            Ok(v) if v["id"].as_i64().is_some() => {
                self.remember_secret(&item, secret);
                Ok((v["id"].as_i64().unwrap_or_default(), item))
            }
            r => {
                self.destroy_item(ws, me, &item).await;
                Err(match r {
                    Err(e) => e.to_string(),
                    Ok(_) => "GitHub 没有给出 hook 编号".into(),
                })
            }
        }
    }

    async fn destroy_item(&self, ws: &str, me: &str, item: &str) {
        self.forget_secret(item);
        let (gh, ws, me, item) = (
            self.clone(),
            ws.to_string(),
            me.to_string(),
            item.to_string(),
        );
        let r = tokio::task::spawn_blocking(move || {
            gh.store()
                .transaction(|tx| gh.connections().vault().destroy_item(tx, &ws, &me, &item))
        })
        .await;
        if !matches!(r, Ok(Ok(_))) {
            tracing::error!("could not destroy a GitHub webhook secret item");
        }
    }

    /// Deletes a repo's hook on GitHub (best effort) and its secret.
    async fn remove_hook(&self, ws: &str, me: &str, r: &RepoRow, item: Option<&str>) {
        if let Some(h) = r.hook_id
            && let Ok(conn) = self.conn_for_write(ws, &r.connection_id)
        {
            match conn
                .delete(&format!("/repos/{}/hooks/{h}", r.full_name))
                .await
            {
                Ok(()) | Err(GhError::Status { status: 404, .. }) => {}
                Err(e) => {
                    tracing::warn!(repo = %r.full_name, error = %e, "github hook not deleted")
                }
            }
        }
        if let Some(i) = item {
            self.destroy_item(ws, me, i).await;
        }
    }

    /// `GET /github/repos/:id`: one `Repo`.
    pub fn repo_detail(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let r = self.own_repo(&who.ws, id)?;
        Ok(self
            .store()
            .reader(|c| repo_out(c, &r).map_err(ApiError::from))?)
    }

    /// `PATCH /github/repos/:id {project_id?, settings?, enabled?}`.
    pub fn patch_repo(&self, who: &Caller, id: &str, body: &Value) -> VaultResult<Value> {
        let p = RepoPatch::parse(body)?;
        let ws = who.ws.clone();
        let now = self.now();
        let (out, changed) = self.store().transaction(|tx| {
            let (r, changed) = patch_repo_tx(tx, &ws, &who.me, id, &p, now)?;
            Ok::<_, steve_modules::vault::VaultError>((
                repo_out(tx, &r).map_err(ApiError::from)?,
                changed,
            ))
        })?;
        if changed {
            self.notifier().gh(&ws, id);
        }
        Ok(out)
    }

    /// `DELETE /github/repos/:id`: the PAT hook and its secret go too; PR and issue rows are
    /// deleted, items keep their artifacts and links.
    pub async fn unlink_repo(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let r = self.own_repo(&who.ws, id)?;
        let item = r.poll_state["hook_item"].as_str().map(String::from);
        self.remove_hook(&who.ws, &who.me, &r, item.as_deref())
            .await;
        self.store().transaction(|tx| {
            repo::delete_repo(tx, &r.id).map_err(ApiError::from)?;
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "github.repo_unlink",
                &r.id,
                &json!({"full_name": r.full_name}),
            )
            .map_err(ApiError::from)?;
            Ok::<_, ApiError>(())
        })?;
        self.notifier().gh(&who.ws, id);
        Ok(json!({"ok": true}))
    }

    /// `GET /github/prs?repo=&state=open|all&item=`.
    pub fn prs_out(&self, who: &Caller, q: &ListQuery) -> VaultResult<Value> {
        if let Some(r) = &q.repo {
            self.own_repo(&who.ws, r)?;
        }
        let open_only = match q.state.as_deref() {
            None | Some("open") => true,
            Some("all") => false,
            Some(_) => return Err(ApiError::invalid("state").into()),
        };
        Ok(self.store().reader(|c| {
            let mut out = Vec::new();
            let mut repos: HashMap<String, RepoRow> = HashMap::new();
            for p in repo::prs(c, &who.ws, q.repo.as_deref(), open_only, q.item.as_deref())? {
                if !repos.contains_key(&p.repo_id)
                    && let Some(r) = repo::repo_get(c, &p.repo_id)?
                {
                    repos.insert(r.id.clone(), r);
                }
                if let Some(r) = repos.get(&p.repo_id) {
                    out.push(pr_out(c, r, &p)?);
                }
            }
            Ok::<_, ApiError>(Value::Array(out))
        })?)
    }

    /// `GET /github/issues?repo=&state=`.
    pub fn issues_out(&self, who: &Caller, q: &ListQuery) -> VaultResult<Value> {
        if let Some(r) = &q.repo {
            self.own_repo(&who.ws, r)?;
        }
        let state = match q.state.as_deref() {
            None | Some("all") => None,
            Some(s @ ("open" | "closed")) => Some(s),
            Some(_) => return Err(ApiError::invalid("state").into()),
        };
        Ok(self.store().reader(|c| {
            let mut out = Vec::new();
            for i in repo::issues(c, &who.ws, q.repo.as_deref(), state)? {
                let item = match &i.item_id {
                    Some(id) => repo::item_brief(c, &who.ws, id)?
                        .map(|(t, s)| json!({"id": id, "title": t, "status": s})),
                    None => None,
                };
                out.push(json!({
                    "id": i.id, "repo_id": i.repo_id, "number": i.number, "title": i.title,
                    "state": i.state, "labels": i.labels, "url": i.url, "item": item,
                }));
            }
            Ok::<_, ApiError>(Value::Array(out))
        })?)
    }

    /// `GET /github/deliveries?repo=&limit=` (≤100, default 50).
    pub fn deliveries_out(&self, who: &Caller, q: &ListQuery) -> VaultResult<Value> {
        if let Some(r) = &q.repo {
            self.own_repo(&who.ws, r)?;
        }
        let limit = match q.limit.as_deref() {
            None => 50,
            Some(l) => l
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=100).contains(n))
                .ok_or_else(|| ApiError::invalid("limit"))?,
        };
        let rows = self.store().reader(|c| {
            repo::deliveries(c, &who.ws, q.repo.as_deref(), limit).map_err(ApiError::from)
        })?;
        Ok(Value::Array(
            rows.iter()
                .map(|d| {
                    json!({"id": d.delivery_id, "received": d.received, "event": d.event,
                           "action": d.action, "status": d.status, "error": d.error, "repo_id": d.repo_id})
                })
                .collect(),
        ))
    }
}

/// The project may take this repo: it exists in `ws`, is not archived, and has no other repo
/// (`409 这个项目已经关联了 <repo>`). `except`: the repo being patched.
pub(crate) fn check_project(
    c: &rusqlite::Connection,
    ws: &str,
    project: &str,
    except: Option<&str>,
) -> Result<(), ApiError> {
    match repo::project(c, ws, project)? {
        Some(p) if !p.archived => {}
        _ => return Err(ApiError::not_found("项目")),
    }
    if let Some(other) = repo::repo_of_project(c, ws, project)?
        && Some(other.id.as_str()) != except
    {
        return Err(ApiError::conflict(steve_i18n::t!(
            "github.repo.project_taken",
            repo = other.full_name
        )));
    }
    Ok(())
}

/// A checked `PATCH /github/repos/:id` body (also the `gh.repo.patch` operation's parameters).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RepoPatch {
    /// `Some(None)`: unlink the project.
    pub project_id: Option<Option<String>>,
    pub settings: Option<Map<String, Value>>,
    pub enabled: Option<bool>,
}

impl RepoPatch {
    /// `ignore`: keys allowed besides the three (an operation's `op`, `id`).
    pub fn parse_with(body: &Value, ignore: &[&str]) -> Result<Self, ApiError> {
        let Some(obj) = body.as_object() else {
            return Err(ApiError::invalid("body"));
        };
        let bad: Vec<String> = obj
            .keys()
            .filter(|k| {
                !["project_id", "settings", "enabled"].contains(&k.as_str())
                    && !ignore.contains(&k.as_str())
            })
            .cloned()
            .collect();
        if !bad.is_empty() {
            return Err(ApiError::Invalid(bad));
        }
        let project_id = match obj.get("project_id") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(s)) if !s.is_empty() && s.len() <= 100 => Some(Some(s.clone())),
            Some(_) => return Err(ApiError::invalid("project_id")),
        };
        let settings = match obj.get("settings") {
            None => None,
            Some(Value::Object(m)) => Some(m.clone()),
            Some(_) => return Err(ApiError::invalid("settings")),
        };
        let enabled = match obj.get("enabled") {
            None => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => return Err(ApiError::invalid("enabled")),
        };
        Ok(Self {
            project_id,
            settings,
            enabled,
        })
    }

    pub fn parse(body: &Value) -> Result<Self, ApiError> {
        Self::parse_with(body, &[])
    }

    fn keys(&self) -> Vec<&'static str> {
        let mut k = Vec::new();
        if self.project_id.is_some() {
            k.push("project_id");
        }
        if self.settings.is_some() {
            k.push("settings");
        }
        if self.enabled.is_some() {
            k.push("enabled");
        }
        k
    }
}

/// What a repo patch can change, for revisions and undo.
pub fn repo_snapshot(r: &RepoRow) -> Value {
    json!({"project_id": r.project_id, "settings": r.settings, "enabled": r.enabled})
}

/// Applies a patch inside `tx`: checks, revision + 1 and a `revisions` row when something changed,
/// audit `github.repo_patch`. Returns the row after and whether it changed. `404 没有这个仓库`.
pub fn patch_repo_tx(
    tx: &steve_store::Tx,
    ws: &str,
    actor: &str,
    id: &str,
    p: &RepoPatch,
    now: i64,
) -> Result<(RepoRow, bool), ApiError> {
    let mut r = repo::repo_get(tx, id)?
        .filter(|r| r.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("仓库"))?;
    let before = repo_snapshot(&r);
    if let Some(project) = &p.project_id {
        if let Some(project) = project {
            check_project(tx, ws, project, Some(&r.id))?;
        }
        r.project_id = project.clone();
    }
    if let Some(m) = &p.settings {
        r.settings = Settings::patched(&r.settings, m)?;
    }
    if let Some(e) = p.enabled {
        r.enabled = e;
        if e && let Some(o) = r.poll_state.as_object_mut() {
            o.remove("lost_access");
        }
    }
    let after = repo_snapshot(&r);
    let changed = after != before;
    if changed {
        r.revision += 1;
        r.updated = now;
        repo::update_repo(tx, &r)?;
        base::revision(tx, ws, "gh_repo", &r.id, r.revision, &before, &after, actor)?;
        base::audit(
            tx,
            ws,
            actor,
            "github.repo_patch",
            &r.id,
            &json!({"keys": p.keys()}),
        )?;
    }
    Ok((r, changed))
}

/// Query of the list endpoints.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub repo: Option<String>,
    pub state: Option<String>,
    pub item: Option<String>,
    pub limit: Option<String>,
}

#[derive(Deserialize)]
struct AvailableQuery {
    connection: Option<String>,
}

#[derive(Deserialize)]
struct LinkBody {
    connection_id: String,
    gh_id: i64,
    #[serde(default)]
    project_id: Option<String>,
}

/// The routes, relative to `/api`, behind the `Caller` layer.
pub fn routes(gh: GitHub) -> Router<Modules> {
    Router::new()
        .route("/github", get(overview))
        .route("/github/available", get(available))
        .route("/github/repos", post(link))
        .route(
            "/github/repos/{id}",
            get(detail).patch(patch).delete(unlink),
        )
        .route("/github/repos/{id}/sync", post(sync_now))
        .route("/github/prs", get(prs))
        .route("/github/issues", get(issues))
        .route("/github/deliveries", get(deliveries))
        .layer(Extension(gh.clone()))
        .merge(crate::link::routes(gh))
}

async fn overview(Extension(gh): Extension<GitHub>, who: Caller) -> VaultResult<Json<Value>> {
    Ok(Json(gh.overview(&who).await?))
}

async fn available(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Query(q): Query<AvailableQuery>,
) -> VaultResult<Json<Value>> {
    let c = q
        .connection
        .filter(|c| !c.is_empty())
        .ok_or_else(|| ApiError::invalid("connection"))?;
    Ok(Json(gh.available_out(&who, &c).await?))
}

async fn link(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Body(b): Body<LinkBody>,
) -> VaultResult<Json<Value>> {
    let mut c = steve_modules::Check::new();
    c.len("connection_id", &b.connection_id, 1, 100)
        .that("gh_id", b.gh_id > 0)
        .opt_len("project_id", b.project_id.as_deref(), 1, 100);
    c.done()?;
    let out = gh
        .link_repo(&who, &b.connection_id, b.gh_id, b.project_id.as_deref())
        .await?;
    // First sync in the background, so existing PRs show up without waiting for a webhook.
    if let Some(id) = out["id"].as_str() {
        gh.spawn_poll(id.to_string());
    }
    Ok(Json(out))
}

async fn detail(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    Ok(Json(gh.repo_detail(&who, &id)?))
}

async fn patch(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<Value>,
) -> VaultResult<Json<Value>> {
    Ok(Json(gh.patch_repo(&who, &id, &b)?))
}

async fn unlink(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    Ok(Json(gh.unlink_repo(&who, &id).await?))
}

async fn sync_now(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Response> {
    gh.own_repo(&who.ws, &id)?;
    gh.spawn_poll(id);
    Ok((StatusCode::ACCEPTED, Json(json!({"ok": true}))).into_response())
}

async fn prs(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Query(q): Query<ListQuery>,
) -> VaultResult<Json<Value>> {
    Ok(Json(gh.prs_out(&who, &q)?))
}

async fn issues(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Query(q): Query<ListQuery>,
) -> VaultResult<Json<Value>> {
    Ok(Json(gh.issues_out(&who, &q)?))
}

async fn deliveries(
    Extension(gh): Extension<GitHub>,
    who: Caller,
    Query(q): Query<ListQuery>,
) -> VaultResult<Json<Value>> {
    Ok(Json(gh.deliveries_out(&who, &q)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes() {
        for u in [
            "https://github.com/Steve-Test/demo.git",
            "https://github.com/steve-test/demo",
            "git@github.com:steve-test/demo.git",
            "ssh://git@github.com/steve-test/demo.git",
            "https://lee@github.com/steve-test/demo.git\n",
        ] {
            assert_eq!(parse_remote(u).as_deref(), Some("steve-test/demo"), "{u}");
        }
        assert_eq!(parse_remote("https://gitlab.com/a/b.git"), None);
        assert_eq!(parse_remote("https://github.com/a"), None);
        assert_eq!(parse_remote("https://github.com/a/b/c"), None);
    }

    #[test]
    fn settings_defaults_and_patch() {
        let d = Settings::of(&json!({}));
        assert_eq!(
            d.to_json(),
            json!({"after_accept":"ask_pr","finish_on":"accepted","merge_method":"squash",
                   "delete_branch":true,"ci_fail":"supplement","draft_pr":false})
        );
        let m = json!({"finish_on": "merged", "draft_pr": true});
        let p = Settings::patched(&json!({}), m.as_object().unwrap()).unwrap();
        assert_eq!(p["finish_on"], "merged");
        assert_eq!(p["draft_pr"], true);
        let bad = json!({"finish_on": "never", "delete_branch": "yes", "x": 1});
        assert_eq!(
            Settings::patched(&json!({}), bad.as_object().unwrap()),
            Err(ApiError::Invalid(vec![
                "settings.finish_on".into(),
                "settings.delete_branch".into(),
                "settings.x".into()
            ]))
        );
    }
}

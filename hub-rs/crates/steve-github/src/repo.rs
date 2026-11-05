//! SQL of the GitHub tables (migration 010). Parameterized queries only; functions take a
//! `&rusqlite::Connection` (or a `Tx`, which derefs to one).

use rusqlite::{Connection, OptionalExtension, Result, Row, params};
use serde_json::Value;

/// A `gh_apps` row: a GitHub App created through the manifest flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRow {
    /// `ghapp_<app id>`.
    pub id: String,
    pub workspace_id: String,
    pub app_id: i64,
    pub slug: String,
    /// Login of the user or organization that owns the App.
    pub owner: String,
    pub client_id: String,
    /// System vault item with `private_key`, `webhook_secret`, `client_secret`.
    pub credential_item: String,
    pub html_url: String,
    pub created: i64,
}

const APP_COLS: &str =
    "id, workspace_id, app_id, slug, owner, client_id, credential_item, html_url, created";

fn app_row(r: &Row) -> Result<AppRow> {
    Ok(AppRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        app_id: r.get(2)?,
        slug: r.get(3)?,
        owner: r.get(4)?,
        client_id: r.get(5)?,
        credential_item: r.get(6)?,
        html_url: r.get(7)?,
        created: r.get(8)?,
    })
}

pub fn insert_app(c: &Connection, a: &AppRow) -> Result<()> {
    c.prepare_cached(&format!(
        "INSERT INTO gh_apps ({APP_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))?
    .execute(params![
        a.id,
        a.workspace_id,
        a.app_id,
        a.slug,
        a.owner,
        a.client_id,
        a.credential_item,
        a.html_url,
        a.created
    ])?;
    Ok(())
}

/// The workspace's Apps, oldest first.
pub fn apps(c: &Connection, ws: &str) -> Result<Vec<AppRow>> {
    c.prepare_cached(&format!(
        "SELECT {APP_COLS} FROM gh_apps WHERE workspace_id = ? ORDER BY created, id"
    ))?
    .query_map(params![ws], app_row)?
    .collect()
}

/// By GitHub's App id (unique across workspaces).
pub fn app_by_app_id(c: &Connection, app_id: i64) -> Result<Option<AppRow>> {
    c.prepare_cached(&format!("SELECT {APP_COLS} FROM gh_apps WHERE app_id = ?"))?
        .query_row(params![app_id], app_row)
        .optional()
}

/// The `github_app` connection of an App in a workspace, with its installation id.
pub fn app_connection(c: &Connection, ws: &str, app_id: i64) -> Result<Option<(String, i64)>> {
    c.prepare_cached(
        "SELECT id, CAST(json_extract(config, '$.installation_id') AS INTEGER) FROM connections
         WHERE workspace_id = ? AND adapter = 'github_app'
           AND CAST(json_extract(config, '$.app_id') AS INTEGER) = ?
         ORDER BY created LIMIT 1",
    )?
    .query_row(params![ws, app_id], |r| Ok((r.get(0)?, r.get(1)?)))
    .optional()
}

/// Every `github_app` connection of an installation in a workspace (normally one).
pub fn installation_connections(
    c: &Connection,
    ws: &str,
    installation_id: i64,
) -> Result<Vec<String>> {
    c.prepare_cached(
        "SELECT id FROM connections
         WHERE workspace_id = ? AND adapter = 'github_app'
           AND CAST(json_extract(config, '$.installation_id') AS INTEGER) = ?
         ORDER BY created",
    )?
    .query_map(params![ws, installation_id], |r| r.get(0))?
    .collect()
}

/// `(workspace, adapter)` of a connection.
pub fn connection_kind(c: &Connection, id: &str) -> Result<Option<(String, String)>> {
    c.prepare_cached("SELECT workspace_id, adapter FROM connections WHERE id = ?")?
        .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
}

/// `(adapter, account handle_display)` of a connection, for the repo JSON.
pub fn connection_label(c: &Connection, id: &str) -> Result<Option<(String, String, String)>> {
    c.prepare_cached(
        "SELECT c.adapter, COALESCE(a.handle_display, ''), c.status FROM connections c
         LEFT JOIN accounts a ON a.id = c.account_id WHERE c.id = ?",
    )?
    .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .optional()
}

/// Is `login` the GitHub account of one of the workspace's users (“你”)?
pub fn is_user_handle(c: &Connection, ws: &str, login: &str) -> Result<bool> {
    c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM accounts a JOIN parties p ON p.id = a.party_id
         WHERE a.workspace_id = ? AND a.family = 'github_user' AND a.status = 'active'
           AND lower(a.handle) = lower(?) AND p.user_id IS NOT NULL)",
    )?
    .query_row(params![ws, login], |r| r.get(0))
}

/// The columns of a project this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectBrief {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub archived: bool,
    pub device_id: String,
}

/// A project in a workspace.
pub fn project(c: &Connection, ws: &str, id: &str) -> Result<Option<ProjectBrief>> {
    c.prepare_cached(
        "SELECT name, kind, path, archived, device_id FROM projects WHERE id = ? AND workspace_id = ?",
    )?
    .query_row(params![id, ws], |r| {
        Ok(ProjectBrief {
            name: r.get(0)?,
            kind: r.get(1)?,
            path: r.get(2)?,
            archived: r.get::<_, i64>(3)? != 0,
            device_id: r.get(4)?,
        })
    })
    .optional()
}

/// `(id, name, path)` of the workspace's git projects on the Hub machine, not archived.
pub fn git_projects(c: &Connection, ws: &str) -> Result<Vec<(String, String, String)>> {
    c.prepare_cached(
        "SELECT id, name, path FROM projects
         WHERE workspace_id = ? AND kind = 'git' AND archived = 0 AND device_id = 'hub'
         ORDER BY name",
    )?
    .query_map(params![ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
    .collect()
}

// ---- gh_repos ---------------------------------------------------------------

/// A `gh_repos` row: a repository linked in a workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoRow {
    /// `ghr_…`.
    pub id: String,
    pub workspace_id: String,
    pub connection_id: String,
    pub gh_id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub default_branch: String,
    pub private: bool,
    pub project_id: Option<String>,
    /// `app|repo_hook|poll`.
    pub ingest: String,
    pub hook_id: Option<i64>,
    pub last_delivery: Option<i64>,
    /// `{pulls_etag, issues_since, last_poll, last_issues_poll, branch_checks, hook_item, lost_access}`.
    pub poll_state: Value,
    /// The repo settings, only the keys set (defaults applied when read, see `crate::repos::Settings`).
    pub settings: Value,
    pub enabled: bool,
    pub revision: i64,
    pub created: i64,
    pub updated: i64,
}

const REPO_COLS: &str = "id, workspace_id, connection_id, gh_id, owner, name, full_name, default_branch, private, project_id, ingest, hook_id, last_delivery, poll_state, settings, enabled, revision, created, updated";

fn json_col(s: String) -> Value {
    serde_json::from_str(&s).unwrap_or_else(|_| Value::Object(Default::default()))
}

fn repo_row(r: &Row) -> Result<RepoRow> {
    Ok(RepoRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        connection_id: r.get(2)?,
        gh_id: r.get(3)?,
        owner: r.get(4)?,
        name: r.get(5)?,
        full_name: r.get(6)?,
        default_branch: r.get(7)?,
        private: r.get::<_, i64>(8)? != 0,
        project_id: r.get(9)?,
        ingest: r.get(10)?,
        hook_id: r.get(11)?,
        last_delivery: r.get(12)?,
        poll_state: json_col(r.get(13)?),
        settings: json_col(r.get(14)?),
        enabled: r.get::<_, i64>(15)? != 0,
        revision: r.get(16)?,
        created: r.get(17)?,
        updated: r.get(18)?,
    })
}

pub fn insert_repo(c: &Connection, r: &RepoRow) -> Result<()> {
    c.prepare_cached(&format!(
        "INSERT INTO gh_repos ({REPO_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))?
    .execute(params![
        r.id,
        r.workspace_id,
        r.connection_id,
        r.gh_id,
        r.owner,
        r.name,
        r.full_name,
        r.default_branch,
        r.private as i64,
        r.project_id,
        r.ingest,
        r.hook_id,
        r.last_delivery,
        r.poll_state.to_string(),
        r.settings.to_string(),
        r.enabled as i64,
        r.revision,
        r.created,
        r.updated
    ])?;
    Ok(())
}

/// Writes every mutable column of `r` back.
pub fn update_repo(c: &Connection, r: &RepoRow) -> Result<()> {
    c.prepare_cached(
        "UPDATE gh_repos SET connection_id = ?, owner = ?, name = ?, full_name = ?, default_branch = ?,
           private = ?, project_id = ?, ingest = ?, hook_id = ?, last_delivery = ?, poll_state = ?,
           settings = ?, enabled = ?, revision = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![
        r.connection_id,
        r.owner,
        r.name,
        r.full_name,
        r.default_branch,
        r.private as i64,
        r.project_id,
        r.ingest,
        r.hook_id,
        r.last_delivery,
        r.poll_state.to_string(),
        r.settings.to_string(),
        r.enabled as i64,
        r.revision,
        r.updated,
        r.id
    ])?;
    Ok(())
}

pub fn repo_get(c: &Connection, id: &str) -> Result<Option<RepoRow>> {
    c.prepare_cached(&format!("SELECT {REPO_COLS} FROM gh_repos WHERE id = ?"))?
        .query_row(params![id], repo_row)
        .optional()
}

pub fn repo_by_gh(c: &Connection, ws: &str, gh_id: i64) -> Result<Option<RepoRow>> {
    c.prepare_cached(&format!(
        "SELECT {REPO_COLS} FROM gh_repos WHERE workspace_id = ? AND gh_id = ?"
    ))?
    .query_row(params![ws, gh_id], repo_row)
    .optional()
}

pub fn repo_of_project(c: &Connection, ws: &str, project: &str) -> Result<Option<RepoRow>> {
    c.prepare_cached(&format!(
        "SELECT {REPO_COLS} FROM gh_repos WHERE workspace_id = ? AND project_id = ?"
    ))?
    .query_row(params![ws, project], repo_row)
    .optional()
}

pub fn repos(c: &Connection, ws: &str) -> Result<Vec<RepoRow>> {
    c.prepare_cached(&format!(
        "SELECT {REPO_COLS} FROM gh_repos WHERE workspace_id = ? ORDER BY full_name, id"
    ))?
    .query_map(params![ws], repo_row)?
    .collect()
}

pub fn repos_of_connection(c: &Connection, connection: &str) -> Result<Vec<RepoRow>> {
    c.prepare_cached(&format!(
        "SELECT {REPO_COLS} FROM gh_repos WHERE connection_id = ? ORDER BY full_name, id"
    ))?
    .query_map(params![connection], repo_row)?
    .collect()
}

/// Every enabled repo in every workspace (the poll job).
pub fn enabled_repos(c: &Connection) -> Result<Vec<RepoRow>> {
    c.prepare_cached(&format!(
        "SELECT {REPO_COLS} FROM gh_repos WHERE enabled = 1 ORDER BY workspace_id, id"
    ))?
    .query_map([], repo_row)?
    .collect()
}

pub fn set_last_delivery(c: &Connection, id: &str, at: i64) -> Result<()> {
    c.prepare_cached("UPDATE gh_repos SET last_delivery = ? WHERE id = ?")?
        .execute(params![at, id])?;
    Ok(())
}

pub fn set_poll_state(c: &Connection, id: &str, state: &Value) -> Result<()> {
    c.prepare_cached("UPDATE gh_repos SET poll_state = ? WHERE id = ?")?
        .execute(params![state.to_string(), id])?;
    Ok(())
}

/// Deletes the repo with its PRs, checks and issues (links and artifacts on items stay).
pub fn delete_repo(c: &Connection, id: &str) -> Result<()> {
    for sql in [
        "DELETE FROM gh_checks WHERE repo_id = ?",
        "DELETE FROM gh_prs WHERE repo_id = ?",
        "DELETE FROM gh_issues WHERE repo_id = ?",
        "DELETE FROM gh_repos WHERE id = ?",
    ] {
        c.prepare_cached(sql)?.execute(params![id])?;
    }
    Ok(())
}

/// `(open PRs, open issues)` of a repo.
pub fn open_counts(c: &Connection, repo: &str) -> Result<(i64, i64)> {
    c.prepare_cached(
        "SELECT (SELECT count(*) FROM gh_prs WHERE repo_id = ?1 AND state = 'open'),
                (SELECT count(*) FROM gh_issues WHERE repo_id = ?1 AND state = 'open')",
    )?
    .query_row(params![repo], |r| Ok((r.get(0)?, r.get(1)?)))
}

/// Has the repo an open PR linked to an item (the 6-hour fallback poll)?
pub fn has_open_linked_pr(c: &Connection, repo: &str) -> Result<bool> {
    c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM gh_prs WHERE repo_id = ? AND state = 'open' AND item_id IS NOT NULL)",
    )?
    .query_row(params![repo], |r| r.get(0))
}

// ---- gh_prs -----------------------------------------------------------------

/// A `gh_prs` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRow {
    /// `ghp_…`.
    pub id: String,
    pub workspace_id: String,
    pub repo_id: String,
    pub number: i64,
    pub gh_id: i64,
    pub title: String,
    pub url: String,
    /// `open|closed|merged`.
    pub state: String,
    pub draft: bool,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub author_login: String,
    pub author_account_id: Option<String>,
    /// `steve|me|other`.
    pub opened_by: String,
    pub body_excerpt: String,
    /// `none|pending|success|failure`.
    pub checks_state: String,
    /// `none|requested|approved|changes_requested|commented`.
    pub review_state: String,
    /// `clean|dirty|blocked|unknown`.
    pub mergeable: Option<String>,
    pub item_id: Option<String>,
    /// `branch|ref|steve|manual`.
    pub link_source: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub merged_at: Option<i64>,
    pub closed_at: Option<i64>,
}

const PR_COLS: &str = "id, workspace_id, repo_id, number, gh_id, title, url, state, draft, head_ref, head_sha, base_ref, author_login, author_account_id, opened_by, body_excerpt, checks_state, review_state, mergeable, item_id, link_source, created, updated, merged_at, closed_at";

fn pr_row(r: &Row) -> Result<PrRow> {
    Ok(PrRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        repo_id: r.get(2)?,
        number: r.get(3)?,
        gh_id: r.get(4)?,
        title: r.get(5)?,
        url: r.get(6)?,
        state: r.get(7)?,
        draft: r.get::<_, i64>(8)? != 0,
        head_ref: r.get(9)?,
        head_sha: r.get(10)?,
        base_ref: r.get(11)?,
        author_login: r.get(12)?,
        author_account_id: r.get(13)?,
        opened_by: r.get(14)?,
        body_excerpt: r.get(15)?,
        checks_state: r.get(16)?,
        review_state: r.get(17)?,
        mergeable: r.get(18)?,
        item_id: r.get(19)?,
        link_source: r.get(20)?,
        created: r.get(21)?,
        updated: r.get(22)?,
        merged_at: r.get(23)?,
        closed_at: r.get(24)?,
    })
}

/// Inserts or replaces the row with `p.id` (the `(repo_id, number)` key must match it).
pub fn upsert_pr(c: &Connection, p: &PrRow) -> Result<()> {
    c.prepare_cached(&format!(
        "INSERT INTO gh_prs ({PR_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET gh_id = excluded.gh_id, title = excluded.title, url = excluded.url,
           state = excluded.state, draft = excluded.draft, head_ref = excluded.head_ref,
           head_sha = excluded.head_sha, base_ref = excluded.base_ref, author_login = excluded.author_login,
           author_account_id = excluded.author_account_id, opened_by = excluded.opened_by,
           body_excerpt = excluded.body_excerpt, checks_state = excluded.checks_state,
           review_state = excluded.review_state, mergeable = excluded.mergeable,
           item_id = excluded.item_id, link_source = excluded.link_source, updated = excluded.updated,
           merged_at = excluded.merged_at, closed_at = excluded.closed_at"
    ))?
    .execute(params![
        p.id,
        p.workspace_id,
        p.repo_id,
        p.number,
        p.gh_id,
        p.title,
        p.url,
        p.state,
        p.draft as i64,
        p.head_ref,
        p.head_sha,
        p.base_ref,
        p.author_login,
        p.author_account_id,
        p.opened_by,
        p.body_excerpt,
        p.checks_state,
        p.review_state,
        p.mergeable,
        p.item_id,
        p.link_source,
        p.created,
        p.updated,
        p.merged_at,
        p.closed_at
    ])?;
    Ok(())
}

pub fn pr_get(c: &Connection, id: &str) -> Result<Option<PrRow>> {
    c.prepare_cached(&format!("SELECT {PR_COLS} FROM gh_prs WHERE id = ?"))?
        .query_row(params![id], pr_row)
        .optional()
}

pub fn pr_by_number(c: &Connection, repo: &str, number: i64) -> Result<Option<PrRow>> {
    c.prepare_cached(&format!(
        "SELECT {PR_COLS} FROM gh_prs WHERE repo_id = ? AND number = ?"
    ))?
    .query_row(params![repo, number], pr_row)
    .optional()
}

/// PRs whose head is `sha` (any state).
pub fn prs_with_head(c: &Connection, repo: &str, sha: &str) -> Result<Vec<PrRow>> {
    c.prepare_cached(&format!(
        "SELECT {PR_COLS} FROM gh_prs WHERE repo_id = ? AND head_sha = ? ORDER BY number"
    ))?
    .query_map(params![repo, sha], pr_row)?
    .collect()
}

/// A repo's PRs, newest update first. `open_only`; `item`: linked to that item.
pub fn prs(
    c: &Connection,
    ws: &str,
    repo: Option<&str>,
    open_only: bool,
    item: Option<&str>,
) -> Result<Vec<PrRow>> {
    c.prepare_cached(&format!(
        "SELECT {PR_COLS} FROM gh_prs WHERE workspace_id = ?1
           AND (?2 IS NULL OR repo_id = ?2) AND (?3 = 0 OR state = 'open') AND (?4 IS NULL OR item_id = ?4)
         ORDER BY updated DESC, number DESC LIMIT 500"
    ))?
    .query_map(params![ws, repo, open_only as i64, item], pr_row)?
    .collect()
}

// ---- gh_checks --------------------------------------------------------------

/// A `gh_checks` row: one check run (or legacy status context) on a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRow {
    pub repo_id: String,
    pub workspace_id: String,
    pub head_sha: String,
    /// `check_run|status`.
    pub kind: String,
    /// Check run id; the context name for statuses.
    pub ext_id: String,
    pub name: String,
    /// `queued|in_progress|completed`.
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
    pub run_id: Option<i64>,
    pub job_id: Option<i64>,
    pub summary: String,
    pub started: Option<i64>,
    pub completed: Option<i64>,
    pub updated: i64,
}

const CHECK_COLS: &str = "repo_id, workspace_id, head_sha, kind, ext_id, name, status, conclusion, url, run_id, job_id, summary, started, completed, updated";

fn check_row(r: &Row) -> Result<CheckRow> {
    Ok(CheckRow {
        repo_id: r.get(0)?,
        workspace_id: r.get(1)?,
        head_sha: r.get(2)?,
        kind: r.get(3)?,
        ext_id: r.get(4)?,
        name: r.get(5)?,
        status: r.get(6)?,
        conclusion: r.get(7)?,
        url: r.get(8)?,
        run_id: r.get(9)?,
        job_id: r.get(10)?,
        summary: r.get(11)?,
        started: r.get(12)?,
        completed: r.get(13)?,
        updated: r.get(14)?,
    })
}

/// Inserts or updates; a known `run_id`/`job_id` is never overwritten with `NULL`.
pub fn upsert_check(c: &Connection, k: &CheckRow) -> Result<()> {
    c.prepare_cached(&format!(
        "INSERT INTO gh_checks ({CHECK_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(repo_id, head_sha, kind, ext_id) DO UPDATE SET name = excluded.name,
           status = excluded.status, conclusion = excluded.conclusion,
           url = CASE WHEN excluded.url = '' THEN gh_checks.url ELSE excluded.url END,
           run_id = COALESCE(excluded.run_id, gh_checks.run_id),
           job_id = COALESCE(excluded.job_id, gh_checks.job_id),
           summary = CASE WHEN excluded.summary = '' THEN gh_checks.summary ELSE excluded.summary END,
           started = COALESCE(excluded.started, gh_checks.started),
           completed = excluded.completed, updated = excluded.updated"
    ))?
    .execute(params![
        k.repo_id,
        k.workspace_id,
        k.head_sha,
        k.kind,
        k.ext_id,
        k.name,
        k.status,
        k.conclusion,
        k.url,
        k.run_id,
        k.job_id,
        k.summary,
        k.started,
        k.completed,
        k.updated
    ])?;
    Ok(())
}

pub fn checks_for_sha(c: &Connection, repo: &str, sha: &str) -> Result<Vec<CheckRow>> {
    c.prepare_cached(&format!(
        "SELECT {CHECK_COLS} FROM gh_checks WHERE repo_id = ? AND head_sha = ? ORDER BY kind, name, ext_id"
    ))?
    .query_map(params![repo, sha], check_row)?
    .collect()
}

// ---- gh_issues --------------------------------------------------------------

/// A `gh_issues` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRow {
    /// `ghi_…`.
    pub id: String,
    pub workspace_id: String,
    pub repo_id: String,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub labels: Vec<String>,
    pub author_login: String,
    pub author_account_id: Option<String>,
    pub item_id: Option<String>,
    pub created: i64,
    pub updated: i64,
}

const ISSUE_COLS: &str = "id, workspace_id, repo_id, number, title, url, state, labels, author_login, author_account_id, item_id, created, updated";

fn issue_row(r: &Row) -> Result<IssueRow> {
    Ok(IssueRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        repo_id: r.get(2)?,
        number: r.get(3)?,
        title: r.get(4)?,
        url: r.get(5)?,
        state: r.get(6)?,
        labels: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        author_login: r.get(8)?,
        author_account_id: r.get(9)?,
        item_id: r.get(10)?,
        created: r.get(11)?,
        updated: r.get(12)?,
    })
}

pub fn upsert_issue(c: &Connection, i: &IssueRow) -> Result<()> {
    c.prepare_cached(&format!(
        "INSERT INTO gh_issues ({ISSUE_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, url = excluded.url, state = excluded.state,
           labels = excluded.labels, author_login = excluded.author_login,
           author_account_id = excluded.author_account_id, item_id = excluded.item_id,
           updated = excluded.updated"
    ))?
    .execute(params![
        i.id,
        i.workspace_id,
        i.repo_id,
        i.number,
        i.title,
        i.url,
        i.state,
        serde_json::to_string(&i.labels).unwrap_or_else(|_| "[]".into()),
        i.author_login,
        i.author_account_id,
        i.item_id,
        i.created,
        i.updated
    ])?;
    Ok(())
}

/// Sets (or clears) the item an issue was turned into.
pub fn set_issue_item(c: &Connection, id: &str, item: Option<&str>, now: i64) -> Result<()> {
    c.prepare_cached("UPDATE gh_issues SET item_id = ?, updated = max(updated, ?) WHERE id = ?")?
        .execute(params![item, now, id])?;
    Ok(())
}

pub fn issue_by_number(c: &Connection, repo: &str, number: i64) -> Result<Option<IssueRow>> {
    c.prepare_cached(&format!(
        "SELECT {ISSUE_COLS} FROM gh_issues WHERE repo_id = ? AND number = ?"
    ))?
    .query_row(params![repo, number], issue_row)
    .optional()
}

pub fn issues(
    c: &Connection,
    ws: &str,
    repo: Option<&str>,
    state: Option<&str>,
) -> Result<Vec<IssueRow>> {
    c.prepare_cached(&format!(
        "SELECT {ISSUE_COLS} FROM gh_issues WHERE workspace_id = ?1
           AND (?2 IS NULL OR repo_id = ?2) AND (?3 IS NULL OR state = ?3)
         ORDER BY updated DESC, number DESC LIMIT 500"
    ))?
    .query_map(params![ws, repo, state], issue_row)?
    .collect()
}

// ---- gh_deliveries ----------------------------------------------------------

/// A `gh_deliveries` row.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRow {
    pub delivery_id: String,
    pub workspace_id: Option<String>,
    pub repo_id: Option<String>,
    pub event: String,
    pub action: Option<String>,
    /// `received` (not processed yet) | `ok|ignored|failed`.
    pub status: String,
    pub error: Option<String>,
    pub received: i64,
}

/// Not processed yet: the worker (and the recovery on start) takes these.
pub const RECEIVED: &str = "received";

const DELIVERY_COLS: &str =
    "delivery_id, workspace_id, repo_id, event, action, status, error, received";

fn delivery_row(r: &Row) -> Result<DeliveryRow> {
    Ok(DeliveryRow {
        delivery_id: r.get(0)?,
        workspace_id: r.get(1)?,
        repo_id: r.get(2)?,
        event: r.get(3)?,
        action: r.get(4)?,
        status: r.get(5)?,
        error: r.get(6)?,
        received: r.get(7)?,
    })
}

/// Inserts unless the id is known, with what processing it needs (`source`, `payload`: JSON).
/// `false`: a redelivery.
pub fn insert_delivery(
    c: &Connection,
    d: &DeliveryRow,
    source: &str,
    payload: &str,
) -> Result<bool> {
    let n = c
        .prepare_cached(&format!(
            "INSERT INTO gh_deliveries ({DELIVERY_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(delivery_id) DO NOTHING"
        ))?
        .execute(params![
            d.delivery_id,
            d.workspace_id,
            d.repo_id,
            d.event,
            d.action,
            d.status,
            d.error,
            d.received
        ])?;
    if n == 1 {
        c.prepare_cached(
            "INSERT INTO gh_delivery_inputs (delivery_id, source, payload) VALUES (?, ?, ?)",
        )?
        .execute(params![d.delivery_id, source, payload])?;
    }
    Ok(n == 1)
}

/// The deliveries still `received` and due at `now`, oldest first: `(id, event, source, payload)`.
pub fn received_deliveries(
    c: &Connection,
    now: i64,
    limit: i64,
) -> Result<Vec<(String, String, String, String)>> {
    c.prepare_cached(
        "SELECT d.delivery_id, d.event, i.source, i.payload
           FROM gh_deliveries d JOIN gh_delivery_inputs i ON i.delivery_id = d.delivery_id
          WHERE d.status = 'received' AND i.next_at <= ?
          ORDER BY d.received, d.rowid LIMIT ?",
    )?
    .query_map(params![now, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?
    .collect()
}

/// Failed processings of a delivery so far.
pub fn delivery_attempts(c: &Connection, id: &str) -> Result<i64> {
    Ok(
        c.prepare_cached("SELECT attempts FROM gh_delivery_inputs WHERE delivery_id = ?")?
            .query_row(params![id], |r| r.get(0))
            .optional()?
            .unwrap_or(0),
    )
}

/// A processing failed and will be tried again from `next_at`: still `received`, the error kept.
pub fn delivery_retry(c: &Connection, id: &str, error: &str, next_at: i64) -> Result<()> {
    c.prepare_cached(
        "UPDATE gh_delivery_inputs SET attempts = attempts + 1, next_at = ? WHERE delivery_id = ?",
    )?
    .execute(params![next_at, id])?;
    c.prepare_cached("UPDATE gh_deliveries SET error = ? WHERE delivery_id = ?")?
        .execute(params![error, id])?;
    Ok(())
}

pub fn delivery_exists(c: &Connection, id: &str) -> Result<bool> {
    c.prepare_cached("SELECT EXISTS (SELECT 1 FROM gh_deliveries WHERE delivery_id = ?)")?
        .query_row(params![id], |r| r.get(0))
}

pub fn set_delivery_result(
    c: &Connection,
    id: &str,
    ws: Option<&str>,
    repo: Option<&str>,
    status: &str,
    error: Option<&str>,
) -> Result<()> {
    c.prepare_cached(
        "UPDATE gh_deliveries SET workspace_id = COALESCE(?, workspace_id), repo_id = COALESCE(?, repo_id),
           status = ?, error = ? WHERE delivery_id = ?",
    )?
    .execute(params![ws, repo, status, error, id])?;
    // processed: its input is not needed any more (one that failed for good is kept to look at)
    if status != "failed" {
        c.prepare_cached("DELETE FROM gh_delivery_inputs WHERE delivery_id = ?")?
            .execute(params![id])?;
    }
    Ok(())
}

/// The newest first; `repo` filters.
pub fn deliveries(
    c: &Connection,
    ws: &str,
    repo: Option<&str>,
    limit: i64,
) -> Result<Vec<DeliveryRow>> {
    c.prepare_cached(&format!(
        "SELECT {DELIVERY_COLS} FROM gh_deliveries WHERE workspace_id = ?1 AND (?2 IS NULL OR repo_id = ?2)
         ORDER BY received DESC, rowid DESC LIMIT ?3"
    ))?
    .query_map(params![ws, repo, limit], delivery_row)?
    .collect()
}

pub fn delivery(c: &Connection, id: &str) -> Result<Option<DeliveryRow>> {
    c.prepare_cached(&format!(
        "SELECT {DELIVERY_COLS} FROM gh_deliveries WHERE delivery_id = ?"
    ))?
    .query_row(params![id], delivery_row)
    .optional()
}

/// Drops deliveries received before `before` (their dedupe window is over); one still `received`
/// stays until it was processed.
pub fn prune_deliveries(c: &Connection, before: i64) -> Result<usize> {
    c.prepare_cached(
        "DELETE FROM gh_deliveries WHERE received < ? AND status != 'received'
           AND delivery_id NOT IN (SELECT delivery_id FROM gh_delivery_inputs)",
    )?
    .execute(params![before])
}

/// `(title, status)` of an item in a workspace.
pub fn item_brief(c: &Connection, ws: &str, id: &str) -> Result<Option<(String, String)>> {
    c.prepare_cached("SELECT title, status FROM items WHERE id = ? AND workspace_id = ?")?
        .query_row(params![id, ws], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
}

/// The App id of an installation, from any workspace's `github_app` connection.
pub fn app_of_installation(c: &Connection, installation_id: i64) -> Result<Option<i64>> {
    c.prepare_cached(
        "SELECT CAST(json_extract(config, '$.app_id') AS INTEGER) FROM connections
         WHERE adapter = 'github_app' AND CAST(json_extract(config, '$.installation_id') AS INTEGER) = ?
         ORDER BY created LIMIT 1",
    )?
    .query_row(params![installation_id], |r| r.get(0))
    .optional()
}

// ---- items, for linking and the wrap-up (G3) ---------------------------------

/// The workspace's item id prefix (`ST`).
pub fn item_prefix(c: &Connection, ws: &str) -> Result<String> {
    Ok(
        c.prepare_cached("SELECT item_prefix FROM workspaces WHERE id = ?")?
            .query_row(params![ws], |r| r.get::<_, String>(0))
            .optional()?
            .unwrap_or_else(|| "ST".into()),
    )
}

/// The item whose branch is `branch` in `project` (newest first).
pub fn item_for_branch(
    c: &Connection,
    ws: &str,
    branch: &str,
    project: &str,
) -> Result<Option<String>> {
    c.prepare_cached(
        "SELECT id FROM items WHERE workspace_id = ? AND branch = ? AND project_id = ?
         ORDER BY created DESC LIMIT 1",
    )?
    .query_row(params![ws, branch, project], |r| r.get(0))
    .optional()
}

/// `status` of an item in a workspace.
pub fn item_status(c: &Connection, ws: &str, id: &str) -> Result<Option<String>> {
    c.prepare_cached("SELECT status FROM items WHERE id = ? AND workspace_id = ?")?
        .query_row(params![id, ws], |r| r.get(0))
        .optional()
}

/// A timeline line `steve sys <text>` (what the TS Hub writes for system notes).
pub fn add_timeline(c: &Connection, item: &str, text: &str, now: i64) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO timeline (item_id, ts, actor, kind, text, session_id) VALUES (?, ?, 'steve', 'sys', ?, NULL)",
    )?
    .execute(params![item, now, text])?;
    Ok(())
}

/// An artifact, unless the item already has one of that kind at that location. `true` if added.
pub fn add_artifact_once(
    c: &Connection,
    item: &str,
    kind: &str,
    title: &str,
    location: &str,
    now: i64,
) -> Result<bool> {
    let has: bool = c
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM artifacts WHERE item_id = ? AND kind = ? AND location = ?)",
        )?
        .query_row(params![item, kind, location], |r| r.get(0))?;
    if has {
        return Ok(false);
    }
    c.prepare_cached(
        "INSERT INTO artifacts (id, item_id, kind, title, location, created) VALUES (?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        steve_store::uid("art"),
        item,
        kind,
        title,
        location,
        now
    ])?;
    Ok(true)
}

/// The project's release action, if it has one (accepted ≠ finished there).
pub fn project_release(c: &Connection, ws: &str, id: &str) -> Result<Option<String>> {
    Ok(
        c.prepare_cached("SELECT release FROM projects WHERE id = ? AND workspace_id = ?")?
            .query_row(params![id, ws], |r| r.get::<_, Option<String>>(0))
            .optional()?
            .flatten()
            .filter(|r| !r.trim().is_empty()),
    )
}

/// An open PR from `head_ref` in the repo?
pub fn has_open_pr_from(c: &Connection, repo: &str, head_ref: &str) -> Result<bool> {
    c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM gh_prs WHERE repo_id = ? AND head_ref = ? AND state = 'open')",
    )?
    .query_row(params![repo, head_ref], |r| r.get(0))
}

/// The draft of the item's latest acceptance decision: the delivery summary and its checks.
pub fn delivery_summary(c: &Connection, item: &str) -> Result<Option<String>> {
    c.prepare_cached(
        "SELECT draft FROM decisions WHERE item_id = ? AND kind = 'accept' ORDER BY created DESC LIMIT 1",
    )?
    .query_row(params![item], |r| r.get(0))
    .optional()
}

/// Did you approve the PR Steve offered when accepting this item (`gh_pr_open`, `yes`)? Then
/// the item waits for its PR (`finish_on=merged`).
pub fn wrap_up_by_pr(c: &Connection, ws: &str, item: &str) -> Result<bool> {
    c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM decisions WHERE workspace_id = ? AND subject_type = 'gh_pr_open'
           AND item_id = ? AND requested_by = 'steve' AND status = 'resolved'
           AND json_extract(result, '$.choice') = 'yes')",
    )?
    .query_row(params![ws, item], |r| r.get(0))
}

/// Was merging this PR at `sha` already asked (open, or answered for that head)?
pub fn merge_asked(c: &Connection, ws: &str, pr: &str, sha: &str) -> Result<bool> {
    c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM decisions WHERE workspace_id = ? AND subject_type = 'gh_pr_merge'
           AND subject_id = ? AND (status = 'open' OR json_extract(result, '$.sha') = ?))",
    )?
    .query_row(params![ws, pr, sha], |r| r.get(0))
}

// ---- G4: issues by id, bots, comment grants ----------------------------------

pub fn issue_get(c: &Connection, id: &str) -> Result<Option<IssueRow>> {
    c.prepare_cached(&format!("SELECT {ISSUE_COLS} FROM gh_issues WHERE id = ?"))?
        .query_row(params![id], issue_row)
        .optional()
}

/// The issues linked to an item (newest update first).
pub fn issues_of_item(c: &Connection, ws: &str, item: &str) -> Result<Vec<IssueRow>> {
    c.prepare_cached(&format!(
        "SELECT {ISSUE_COLS} FROM gh_issues WHERE workspace_id = ? AND item_id = ?
         ORDER BY updated DESC, number DESC"
    ))?
    .query_map(params![ws, item], issue_row)?
    .collect()
}

/// A bot's display name.
pub fn bot_name(c: &Connection, bot: &str) -> Result<Option<String>> {
    c.prepare_cached("SELECT name FROM bots WHERE id = ?")?
        .query_row(params![bot], |r| r.get(0))
        .optional()
}

/// The capability of “this bot may comment in this repo without asking” (`grants.capability`).
pub const COMMENT_CAP: &str = "github.comment";

/// The open `github.comment` grant of `bot` on `repo` at `now`, longest-lasting first.
pub fn comment_grant(
    c: &Connection,
    ws: &str,
    bot: &str,
    repo: &str,
    now: i64,
) -> Result<Option<String>> {
    c.prepare_cached(
        "SELECT id FROM grants
          WHERE workspace_id = ? AND capability = 'github.comment' AND resource_type = 'repo'
            AND resource_id = ? AND subject_type = 'bot' AND subject_id = ?
            AND revoked IS NULL AND until > ?
          ORDER BY until DESC, id LIMIT 1",
    )?
    .query_row(params![ws, repo, bot, now], |r| r.get(0))
    .optional()
}

/// Writes a `github.comment` grant of `bot` on `repo` until `until`.
#[allow(clippy::too_many_arguments)]
pub fn insert_comment_grant(
    c: &Connection,
    id: &str,
    ws: &str,
    bot: &str,
    repo: &str,
    until: i64,
    created_by: &str,
    decision_id: Option<&str>,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id,
           scope, until, created_by, decision_id, revoked, created)
         VALUES (?, ?, 'bot', ?, 'github.comment', 'repo', ?, '{}', ?, ?, ?, NULL, ?)",
    )?
    .execute(params![id, ws, bot, repo, until, created_by, decision_id, now])?;
    Ok(())
}

// ---- gh_effects ------------------------------------------------------------

/// A `gh_effects` row (migration 29).
#[derive(Debug, Clone, PartialEq)]
pub struct EffectRow {
    pub id: String,
    pub workspace_id: String,
    pub decision_id: String,
    /// `open|merge|comment`.
    pub kind: String,
    /// `pending|running|done|failed|unknown`.
    pub state: String,
    pub attempts: i64,
    pub next_at: Option<i64>,
    pub last_error: Option<String>,
    pub result: Value,
}

const EFFECT_COLS: &str =
    "id, workspace_id, decision_id, kind, state, attempts, next_at, last_error, result";

fn effect_row(r: &Row) -> Result<EffectRow> {
    Ok(EffectRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        decision_id: r.get(2)?,
        kind: r.get(3)?,
        state: r.get(4)?,
        attempts: r.get(5)?,
        next_at: r.get(6)?,
        last_error: r.get(7)?,
        result: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or(Value::Null),
    })
}

/// A pending effect for `decision`, once (an answer given twice is still one write).
pub fn insert_effect(c: &Connection, ws: &str, decision: &str, kind: &str, now: i64) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO gh_effects (id, workspace_id, decision_id, kind, state, next_at, created, updated)
         VALUES (?, ?, ?, ?, 'pending', ?, ?, ?) ON CONFLICT(decision_id) DO NOTHING",
    )?
    .execute(params![steve_store::uid("ghe"), ws, decision, kind, now, now, now])?;
    Ok(())
}

pub fn effect_of(c: &Connection, decision: &str) -> Result<Option<EffectRow>> {
    c.prepare_cached(&format!(
        "SELECT {EFFECT_COLS} FROM gh_effects WHERE decision_id = ?"
    ))?
    .query_row(params![decision], effect_row)
    .optional()
}

/// The effects to try now: pending or unknown, due, oldest first.
pub fn due_effects(c: &Connection, now: i64, limit: i64) -> Result<Vec<EffectRow>> {
    c.prepare_cached(&format!(
        "SELECT {EFFECT_COLS} FROM gh_effects
          WHERE state IN ('pending', 'unknown') AND next_at IS NOT NULL AND next_at <= ?
          ORDER BY next_at, created LIMIT ?"
    ))?
    .query_map(params![now, limit], effect_row)?
    .collect()
}

/// `pending|unknown → running` if it is still as read (`attempts`) and due; `false`: another took
/// it, or it changed meanwhile.
pub fn claim_effect(c: &Connection, id: &str, attempts: i64, now: i64) -> Result<bool> {
    Ok(c.prepare_cached(
        "UPDATE gh_effects SET state = 'running', attempts = attempts + 1, updated = ?1
          WHERE id = ?2 AND state IN ('pending', 'unknown') AND attempts = ?3
            AND next_at IS NOT NULL AND next_at <= ?1",
    )?
    .execute(params![now, id, attempts])?
        == 1)
}

/// What a run is about, merged into `result` (the commit to push).
pub fn effect_intent(c: &Connection, decision: &str, intent: &Value) -> Result<()> {
    c.prepare_cached("UPDATE gh_effects SET result = json_patch(result, ?) WHERE decision_id = ?")?
        .execute(params![intent.to_string(), decision])?;
    Ok(())
}

/// How a run of an effect ended.
pub fn settle_effect(
    c: &Connection,
    id: &str,
    state: &str,
    next_at: Option<i64>,
    error: Option<&str>,
    result: &Value,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "UPDATE gh_effects SET state = ?, next_at = ?, last_error = ?, result = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![state, next_at, error, result.to_string(), now, id])?;
    Ok(())
}

/// After a restart: what was running may or may not have happened on GitHub.
/// Reconciled from `next_at` on: after a request the restart cut off could still have landed.
pub fn interrupted_effects(c: &Connection, now: i64, next_at: i64) -> Result<usize> {
    c.prepare_cached(
        "UPDATE gh_effects SET state = 'unknown', next_at = ?, last_error = '重启时还在进行', updated = ?
          WHERE state = 'running'",
    )?
    .execute(params![next_at, now])
}

/// Whether a comment id is already the result of another effect (reconciling a comment).
pub fn comment_claimed(c: &Connection, comment_id: i64) -> Result<bool> {
    c.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM gh_effects WHERE kind = 'comment' AND state = 'done'
          AND json_extract(result, '$.comment') = ?)",
    )?
    .query_row(params![comment_id], |r| r.get(0))
}

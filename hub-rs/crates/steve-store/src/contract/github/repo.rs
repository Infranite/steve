use super::{DELIVERY_COLS, DeliveryRow};
use crate::contract::{Q, Row};
use crate::{Read, Result, W};
use sea_query::{Asterisk, Expr, ExprTrait, Func, OnConflict, Order, Query};
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

const APP_COLS: [&str; 9] = [
    "id",
    "workspace_id",
    "app_id",
    "slug",
    "owner",
    "client_id",
    "credential_item",
    "html_url",
    "created",
];

fn app_row(r: &Row<'_>) -> Result<AppRow> {
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

const REPO_COLS: [&str; 19] = [
    "id",
    "workspace_id",
    "connection_id",
    "gh_id",
    "owner",
    "name",
    "full_name",
    "default_branch",
    "private",
    "project_id",
    "ingest",
    "hook_id",
    "last_delivery",
    "poll_state",
    "settings",
    "enabled",
    "revision",
    "created",
    "updated",
];

fn json_col(s: String) -> Value {
    serde_json::from_str(&s).unwrap_or_else(|_| Value::Object(Default::default()))
}

fn repo_row(r: &Row<'_>) -> Result<RepoRow> {
    Ok(RepoRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        connection_id: r.get(2)?,
        gh_id: r.get(3)?,
        owner: r.get(4)?,
        name: r.get(5)?,
        full_name: r.get(6)?,
        default_branch: r.get(7)?,
        private: r.get::<i64>(8)? != 0,
        project_id: r.get(9)?,
        ingest: r.get(10)?,
        hook_id: r.get(11)?,
        last_delivery: r.get(12)?,
        poll_state: json_col(r.get(13)?),
        settings: json_col(r.get(14)?),
        enabled: r.get::<i64>(15)? != 0,
        revision: r.get(16)?,
        created: r.get(17)?,
        updated: r.get(18)?,
    })
}

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

const PR_COLS: [&str; 25] = [
    "id",
    "workspace_id",
    "repo_id",
    "number",
    "gh_id",
    "title",
    "url",
    "state",
    "draft",
    "head_ref",
    "head_sha",
    "base_ref",
    "author_login",
    "author_account_id",
    "opened_by",
    "body_excerpt",
    "checks_state",
    "review_state",
    "mergeable",
    "item_id",
    "link_source",
    "created",
    "updated",
    "merged_at",
    "closed_at",
];

fn pr_row(r: &Row<'_>) -> Result<PrRow> {
    Ok(PrRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        repo_id: r.get(2)?,
        number: r.get(3)?,
        gh_id: r.get(4)?,
        title: r.get(5)?,
        url: r.get(6)?,
        state: r.get(7)?,
        draft: r.get::<i64>(8)? != 0,
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

const CHECK_COLS: [&str; 15] = [
    "repo_id",
    "workspace_id",
    "head_sha",
    "kind",
    "ext_id",
    "name",
    "status",
    "conclusion",
    "url",
    "run_id",
    "job_id",
    "summary",
    "started",
    "completed",
    "updated",
];

fn check_row(r: &Row<'_>) -> Result<CheckRow> {
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

const ISSUE_COLS: [&str; 13] = [
    "id",
    "workspace_id",
    "repo_id",
    "number",
    "title",
    "url",
    "state",
    "labels",
    "author_login",
    "author_account_id",
    "item_id",
    "created",
    "updated",
];

fn issue_row(r: &Row<'_>) -> Result<IssueRow> {
    Ok(IssueRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        repo_id: r.get(2)?,
        number: r.get(3)?,
        title: r.get(4)?,
        url: r.get(5)?,
        state: r.get(6)?,
        labels: serde_json::from_str(&r.get::<String>(7)?).unwrap_or_default(),
        author_login: r.get(8)?,
        author_account_id: r.get(9)?,
        item_id: r.get(10)?,
        created: r.get(11)?,
        updated: r.get(12)?,
    })
}

fn delivery_row(r: &Row<'_>) -> Result<DeliveryRow> {
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
    pub item_cycle: Option<i64>,
    pub item_scope: Option<i64>,
}

const EFFECT_COLS: [&str; 11] = [
    "id",
    "workspace_id",
    "decision_id",
    "kind",
    "state",
    "attempts",
    "next_at",
    "last_error",
    "result",
    "item_cycle",
    "item_scope",
];

fn effect_row(r: &Row<'_>) -> Result<EffectRow> {
    Ok(EffectRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        decision_id: r.get(2)?,
        kind: r.get(3)?,
        state: r.get(4)?,
        attempts: r.get(5)?,
        next_at: r.get(6)?,
        last_error: r.get(7)?,
        result: serde_json::from_str(&r.get::<String>(8)?).unwrap_or(Value::Null),
        item_cycle: r.get(9)?,
        item_scope: r.get(10)?,
    })
}

pub fn insert_app(c: &mut W<'_>, a: &AppRow) -> Result<()> {
    c.exec(
        Query::insert()
            .into_table("gh_apps")
            .columns(APP_COLS)
            .values_panic([
                Expr::val(a.id.as_str()),
                Expr::val(a.workspace_id.as_str()),
                Expr::val(a.app_id),
                Expr::val(a.slug.as_str()),
                Expr::val(a.owner.as_str()),
                Expr::val(a.client_id.as_str()),
                Expr::val(a.credential_item.as_str()),
                Expr::val(a.html_url.as_str()),
                Expr::val(a.created),
            ]),
    )?;
    Ok(())
}

/// The workspace's Apps, oldest first.
pub fn apps(c: &mut impl Read, ws: &str) -> Result<Vec<AppRow>> {
    c.all(
        Query::select()
            .columns(APP_COLS)
            .from("gh_apps")
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by("created", Order::Asc)
            .order_by("id", Order::Asc),
        app_row,
    )
}

/// By GitHub's App id (unique across workspaces).
pub fn app_by_app_id(c: &mut impl Read, app_id: i64) -> Result<Option<AppRow>> {
    c.one(
        Query::select()
            .columns(APP_COLS)
            .from("gh_apps")
            .and_where(Expr::col("app_id").eq(app_id)),
        app_row,
    )
}

pub fn insert_repo(c: &mut W<'_>, r: &RepoRow) -> Result<()> {
    c.exec(
        Query::insert()
            .into_table("gh_repos")
            .columns(REPO_COLS)
            .values_panic([
                Expr::val(r.id.as_str()),
                Expr::val(r.workspace_id.as_str()),
                Expr::val(r.connection_id.as_str()),
                Expr::val(r.gh_id),
                Expr::val(r.owner.as_str()),
                Expr::val(r.name.as_str()),
                Expr::val(r.full_name.as_str()),
                Expr::val(r.default_branch.as_str()),
                Expr::val(r.private as i64),
                Expr::val(r.project_id.as_deref()),
                Expr::val(r.ingest.as_str()),
                Expr::val(r.hook_id),
                Expr::val(r.last_delivery),
                Expr::val(r.poll_state.to_string()),
                Expr::val(r.settings.to_string()),
                Expr::val(r.enabled as i64),
                Expr::val(r.revision),
                Expr::val(r.created),
                Expr::val(r.updated),
            ]),
    )?;
    Ok(())
}

/// Writes every mutable column of `r` back.
pub fn update_repo(c: &mut W<'_>, r: &RepoRow) -> Result<()> {
    c.exec(
        Query::update()
            .table("gh_repos")
            .value("connection_id", r.connection_id.as_str())
            .value("owner", r.owner.as_str())
            .value("name", r.name.as_str())
            .value("full_name", r.full_name.as_str())
            .value("default_branch", r.default_branch.as_str())
            .value("private", r.private as i64)
            .value("project_id", r.project_id.as_deref())
            .value("ingest", r.ingest.as_str())
            .value("hook_id", r.hook_id)
            .value("last_delivery", r.last_delivery)
            .value("poll_state", r.poll_state.to_string())
            .value("settings", r.settings.to_string())
            .value("enabled", r.enabled as i64)
            .value("revision", r.revision)
            .value("updated", r.updated)
            .and_where(Expr::col("id").eq(r.id.as_str())),
    )?;
    Ok(())
}

pub fn repo_get(c: &mut impl Read, id: &str) -> Result<Option<RepoRow>> {
    c.one(
        Query::select()
            .columns(REPO_COLS)
            .from("gh_repos")
            .and_where(Expr::col("id").eq(id)),
        repo_row,
    )
}

pub fn repo_by_gh(c: &mut impl Read, ws: &str, gh_id: i64) -> Result<Option<RepoRow>> {
    c.one(
        Query::select()
            .columns(REPO_COLS)
            .from("gh_repos")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("gh_id").eq(gh_id)),
        repo_row,
    )
}

pub fn repo_of_project(c: &mut impl Read, ws: &str, project: &str) -> Result<Option<RepoRow>> {
    c.one(
        Query::select()
            .columns(REPO_COLS)
            .from("gh_repos")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("project_id").eq(project)),
        repo_row,
    )
}

pub fn repos(c: &mut impl Read, ws: &str) -> Result<Vec<RepoRow>> {
    c.all(
        Query::select()
            .columns(REPO_COLS)
            .from("gh_repos")
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by("full_name", Order::Asc)
            .order_by("id", Order::Asc),
        repo_row,
    )
}

pub fn repos_of_connection(c: &mut impl Read, connection: &str) -> Result<Vec<RepoRow>> {
    c.all(
        Query::select()
            .columns(REPO_COLS)
            .from("gh_repos")
            .and_where(Expr::col("connection_id").eq(connection))
            .order_by("full_name", Order::Asc)
            .order_by("id", Order::Asc),
        repo_row,
    )
}

pub fn set_last_delivery(c: &mut W<'_>, id: &str, at: i64) -> Result<()> {
    c.exec(
        Query::update()
            .table("gh_repos")
            .value("last_delivery", at)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

/// Deletes the repo with its PRs, checks and issues (links and artifacts on items stay).
pub fn delete_repo(c: &mut W<'_>, id: &str) -> Result<()> {
    for table in ["gh_checks", "gh_prs", "gh_issues"] {
        c.exec(
            Query::delete()
                .from_table(table)
                .and_where(Expr::col("repo_id").eq(id)),
        )?;
    }
    c.exec(
        Query::delete()
            .from_table("gh_repos")
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

/// `(open PRs, open issues)` of a repo.
pub fn open_counts(c: &mut impl Read, repo: &str) -> Result<(i64, i64)> {
    Ok((
        c.one(
            Query::select()
                .expr(Expr::from(Func::count(Expr::col(Asterisk))))
                .from("gh_prs")
                .and_where(Expr::col("repo_id").eq(repo))
                .and_where(Expr::col("state").eq("open")),
            |r| r.get(0),
        )?
        .unwrap_or(0),
        c.one(
            Query::select()
                .expr(Expr::from(Func::count(Expr::col(Asterisk))))
                .from("gh_issues")
                .and_where(Expr::col("repo_id").eq(repo))
                .and_where(Expr::col("state").eq("open")),
            |r| r.get(0),
        )?
        .unwrap_or(0),
    ))
}

/// Inserts or replaces the row with `p.id` (the `(repo_id, number)` key must match it).
pub fn upsert_pr(c: &mut W<'_>, p: &PrRow) -> Result<()> {
    c.exec(
        Query::insert()
            .into_table("gh_prs")
            .columns(PR_COLS)
            .values_panic([
                Expr::val(p.id.as_str()),
                Expr::val(p.workspace_id.as_str()),
                Expr::val(p.repo_id.as_str()),
                Expr::val(p.number),
                Expr::val(p.gh_id),
                Expr::val(p.title.as_str()),
                Expr::val(p.url.as_str()),
                Expr::val(p.state.as_str()),
                Expr::val(p.draft as i64),
                Expr::val(p.head_ref.as_str()),
                Expr::val(p.head_sha.as_str()),
                Expr::val(p.base_ref.as_str()),
                Expr::val(p.author_login.as_str()),
                Expr::val(p.author_account_id.as_deref()),
                Expr::val(p.opened_by.as_str()),
                Expr::val(p.body_excerpt.as_str()),
                Expr::val(p.checks_state.as_str()),
                Expr::val(p.review_state.as_str()),
                Expr::val(p.mergeable.as_deref()),
                Expr::val(p.item_id.as_deref()),
                Expr::val(p.link_source.as_deref()),
                Expr::val(p.created),
                Expr::val(p.updated),
                Expr::val(p.merged_at),
                Expr::val(p.closed_at),
            ])
            .on_conflict(
                OnConflict::columns(["id"])
                    .update_columns([
                        "gh_id",
                        "title",
                        "url",
                        "state",
                        "draft",
                        "head_ref",
                        "head_sha",
                        "base_ref",
                        "author_login",
                        "author_account_id",
                        "opened_by",
                        "body_excerpt",
                        "checks_state",
                        "review_state",
                        "mergeable",
                        "item_id",
                        "link_source",
                        "updated",
                        "merged_at",
                        "closed_at",
                    ])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn pr_get(c: &mut impl Read, id: &str) -> Result<Option<PrRow>> {
    c.one(
        Query::select()
            .columns(PR_COLS)
            .from("gh_prs")
            .and_where(Expr::col("id").eq(id)),
        pr_row,
    )
}

pub fn pr_by_number(c: &mut impl Read, repo: &str, number: i64) -> Result<Option<PrRow>> {
    c.one(
        Query::select()
            .columns(PR_COLS)
            .from("gh_prs")
            .and_where(Expr::col("repo_id").eq(repo))
            .and_where(Expr::col("number").eq(number)),
        pr_row,
    )
}

/// PRs whose head is `sha` (any state).
pub fn prs_with_head(c: &mut impl Read, repo: &str, sha: &str) -> Result<Vec<PrRow>> {
    c.all(
        Query::select()
            .columns(PR_COLS)
            .from("gh_prs")
            .and_where(Expr::col("repo_id").eq(repo))
            .and_where(Expr::col("head_sha").eq(sha))
            .order_by("number", Order::Asc),
        pr_row,
    )
}

/// A repo's PRs, newest update first. `open_only`; `item`: linked to that item.
pub fn prs(
    c: &mut impl Read,
    ws: &str,
    repo: Option<&str>,
    open_only: bool,
    item: Option<&str>,
) -> Result<Vec<PrRow>> {
    c.all(
        Query::select()
            .columns(PR_COLS)
            .from("gh_prs")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::val(repo).is_null().or(Expr::col("repo_id").eq(repo)))
            .and_where(
                Expr::val(open_only as i64)
                    .eq(0_i64)
                    .or(Expr::col("state").eq("open")),
            )
            .and_where(Expr::val(item).is_null().or(Expr::col("item_id").eq(item)))
            .order_by("updated", Order::Desc)
            .order_by("number", Order::Desc)
            .limit(500),
        pr_row,
    )
}

/// Inserts or updates; a known `run_id`/`job_id` is never overwritten with `NULL`.
pub fn upsert_check(c: &mut W<'_>, k: &CheckRow) -> Result<()> {
    c.exec(
        Query::insert()
            .into_table("gh_checks")
            .columns(CHECK_COLS)
            .values_panic([
                Expr::val(k.repo_id.as_str()),
                Expr::val(k.workspace_id.as_str()),
                Expr::val(k.head_sha.as_str()),
                Expr::val(k.kind.as_str()),
                Expr::val(k.ext_id.as_str()),
                Expr::val(k.name.as_str()),
                Expr::val(k.status.as_str()),
                Expr::val(k.conclusion.as_deref()),
                Expr::val(k.url.as_str()),
                Expr::val(k.run_id),
                Expr::val(k.job_id),
                Expr::val(k.summary.as_str()),
                Expr::val(k.started),
                Expr::val(k.completed),
                Expr::val(k.updated),
            ])
            .on_conflict(
                OnConflict::columns(["repo_id", "head_sha", "kind", "ext_id"])
                    .update_columns(["name", "status", "conclusion", "completed", "updated"])
                    .value(
                        "url",
                        Expr::case(
                            Expr::col(("excluded", "url")).eq(""),
                            Expr::col(("gh_checks", "url")),
                        )
                        .finally(Expr::col(("excluded", "url"))),
                    )
                    .value(
                        "run_id",
                        Expr::from(Func::coalesce([
                            Expr::col(("excluded", "run_id")),
                            Expr::col(("gh_checks", "run_id")),
                        ])),
                    )
                    .value(
                        "job_id",
                        Expr::from(Func::coalesce([
                            Expr::col(("excluded", "job_id")),
                            Expr::col(("gh_checks", "job_id")),
                        ])),
                    )
                    .value(
                        "summary",
                        Expr::case(
                            Expr::col(("excluded", "summary")).eq(""),
                            Expr::col(("gh_checks", "summary")),
                        )
                        .finally(Expr::col(("excluded", "summary"))),
                    )
                    .value(
                        "started",
                        Expr::from(Func::coalesce([
                            Expr::col(("excluded", "started")),
                            Expr::col(("gh_checks", "started")),
                        ])),
                    )
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn checks_for_sha(c: &mut impl Read, repo: &str, sha: &str) -> Result<Vec<CheckRow>> {
    c.all(
        Query::select()
            .columns(CHECK_COLS)
            .from("gh_checks")
            .and_where(Expr::col("repo_id").eq(repo))
            .and_where(Expr::col("head_sha").eq(sha))
            .order_by("kind", Order::Asc)
            .order_by("name", Order::Asc)
            .order_by("ext_id", Order::Asc),
        check_row,
    )
}

pub fn upsert_issue(c: &mut W<'_>, i: &IssueRow) -> Result<()> {
    c.exec(
        Query::insert()
            .into_table("gh_issues")
            .columns(ISSUE_COLS)
            .values_panic([
                Expr::val(i.id.as_str()),
                Expr::val(i.workspace_id.as_str()),
                Expr::val(i.repo_id.as_str()),
                Expr::val(i.number),
                Expr::val(i.title.as_str()),
                Expr::val(i.url.as_str()),
                Expr::val(i.state.as_str()),
                Expr::val(serde_json::to_string(&i.labels).unwrap_or_else(|_| "[]".into())),
                Expr::val(i.author_login.as_str()),
                Expr::val(i.author_account_id.as_deref()),
                Expr::val(i.item_id.as_deref()),
                Expr::val(i.created),
                Expr::val(i.updated),
            ])
            .on_conflict(
                OnConflict::columns(["id"])
                    .update_columns([
                        "title",
                        "url",
                        "state",
                        "labels",
                        "author_login",
                        "author_account_id",
                        "item_id",
                        "updated",
                    ])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

/// Sets (or clears) the item an issue was turned into.
pub fn set_issue_item(c: &mut W<'_>, id: &str, item: Option<&str>, now: i64) -> Result<()> {
    let dialect = c.db().dialect();
    c.exec(
        Query::update()
            .table("gh_issues")
            .value("item_id", item)
            .value(
                "updated",
                dialect.greatest(Expr::col("updated"), Expr::val(now)),
            )
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn issue_by_number(c: &mut impl Read, repo: &str, number: i64) -> Result<Option<IssueRow>> {
    c.one(
        Query::select()
            .columns(ISSUE_COLS)
            .from("gh_issues")
            .and_where(Expr::col("repo_id").eq(repo))
            .and_where(Expr::col("number").eq(number)),
        issue_row,
    )
}

pub fn issues(
    c: &mut impl Read,
    ws: &str,
    repo: Option<&str>,
    state: Option<&str>,
) -> Result<Vec<IssueRow>> {
    c.all(
        Query::select()
            .columns(ISSUE_COLS)
            .from("gh_issues")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::val(repo).is_null().or(Expr::col("repo_id").eq(repo)))
            .and_where(Expr::val(state).is_null().or(Expr::col("state").eq(state)))
            .order_by("updated", Order::Desc)
            .order_by("number", Order::Desc)
            .limit(500),
        issue_row,
    )
}

/// The newest first; `repo` filters.
pub fn deliveries(
    c: &mut impl Read,
    ws: &str,
    repo: Option<&str>,
    limit: i64,
    automation_only: bool,
) -> Result<Vec<DeliveryRow>> {
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns(DELIVERY_COLS)
            .from("gh_deliveries")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::val(repo).is_null().or(Expr::col("repo_id").eq(repo)))
            .and_where(
                Expr::val(i64::from(automation_only))
                    .eq(0_i64)
                    .or(Expr::col("event").is_in(["pull_request", "issues"])),
            )
            .order_by("received", Order::Desc)
            .order_by_expr(dialect.insertion_order(Some("gh_deliveries")), Order::Desc)
            .limit(limit as u64),
        delivery_row,
    )
}

/// An open PR from `head_ref` in the repo?
pub fn has_open_pr_from(c: &mut impl Read, repo: &str, head_ref: &str) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("gh_prs")
            .and_where(Expr::col("repo_id").eq(repo))
            .and_where(Expr::col("head_ref").eq(head_ref))
            .and_where(Expr::col("state").eq("open"))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn issue_get(c: &mut impl Read, id: &str) -> Result<Option<IssueRow>> {
    c.one(
        Query::select()
            .columns(ISSUE_COLS)
            .from("gh_issues")
            .and_where(Expr::col("id").eq(id)),
        issue_row,
    )
}

/// The issues linked to an item (newest update first).
pub fn issues_of_item(c: &mut impl Read, ws: &str, item: &str) -> Result<Vec<IssueRow>> {
    c.all(
        Query::select()
            .columns(ISSUE_COLS)
            .from("gh_issues")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("item_id").eq(item))
            .order_by("updated", Order::Desc)
            .order_by("number", Order::Desc),
        issue_row,
    )
}

/// A pending effect for `decision`, once (an answer given twice is still one write).
pub fn insert_effect(c: &mut W<'_>, ws: &str, decision: &str, kind: &str, now: i64) -> Result<()> {
    let item = crate::decision_repo::get(c, decision)?.and_then(|d| d.item_id);
    let scope = match item.as_deref().filter(|_| matches!(kind, "open" | "merge")) {
        Some(item) => Some((
            crate::item_repo::open_cycle(c, ws, item)?
                .ok_or_else(|| crate::StoreError::Conflict("item cycle is closed".into()))?,
            crate::item_delivery::scope(c, item)?,
        )),
        None => None,
    };
    c.exec(
        Query::insert()
            .into_table("gh_effects")
            .columns([
                "id",
                "workspace_id",
                "decision_id",
                "kind",
                "state",
                "next_at",
                "created",
                "updated",
                "item_cycle",
                "item_scope",
            ])
            .values_panic([
                Expr::val(crate::uid("ghe")),
                Expr::val(ws),
                Expr::val(decision),
                Expr::val(kind),
                Expr::val("pending"),
                Expr::val(now),
                Expr::val(now),
                Expr::val(now),
                Expr::val(scope.map(|(cycle, _)| cycle)),
                Expr::val(scope.map(|(_, scope)| scope)),
            ])
            .on_conflict(OnConflict::columns(["decision_id"]).do_nothing().to_owned()),
    )?;
    Ok(())
}

pub fn effect_of(c: &mut impl Read, decision: &str) -> Result<Option<EffectRow>> {
    c.one(
        Query::select()
            .columns(EFFECT_COLS)
            .from("gh_effects")
            .and_where(Expr::col("decision_id").eq(decision)),
        effect_row,
    )
}

/// The effects to try now: pending or unknown, due, oldest first.
pub fn due_effects(c: &mut impl Read, now: i64, limit: i64) -> Result<Vec<EffectRow>> {
    c.all(
        Query::select()
            .columns(EFFECT_COLS)
            .from("gh_effects")
            .and_where(Expr::col("state").is_in(["pending", "unknown"]))
            .and_where(Expr::col("next_at").is_not_null())
            .and_where(Expr::col("next_at").lte(now))
            .order_by("next_at", Order::Asc)
            .order_by("created", Order::Asc)
            .limit(limit as u64),
        effect_row,
    )
}

/// `pending|unknown → running` if it is still as read (`attempts`) and due; `false`: another took
/// it, or it changed meanwhile.
pub fn claim_effect(c: &mut W<'_>, id: &str, attempts: i64, now: i64) -> Result<bool> {
    Ok(c.exec(
        Query::update()
            .table("gh_effects")
            .value("state", "running")
            .value("attempts", Expr::col("attempts").add(1_i64))
            .value("updated", now)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("state").is_in(["pending", "unknown"]))
            .and_where(Expr::col("attempts").eq(attempts))
            .and_where(Expr::col("next_at").is_not_null())
            .and_where(Expr::col("next_at").lte(now)),
    )? == 1)
}

/// What a run is about, merged into `result` (the commit to push).
pub fn effect_intent(c: &mut W<'_>, decision: &str, intent: &Value) -> Result<()> {
    let dialect = c.db().dialect();
    c.exec(
        Query::update()
            .table("gh_effects")
            .value(
                "result",
                dialect.json_merge_patch(Expr::col("result"), Expr::val(intent.to_string())),
            )
            .and_where(Expr::col("decision_id").eq(decision)),
    )?;
    Ok(())
}

/// How a run of an effect ended.
pub fn settle_effect(
    c: &mut W<'_>,
    id: &str,
    state: &str,
    next_at: Option<i64>,
    error: Option<&str>,
    result: &Value,
    now: i64,
) -> Result<()> {
    c.exec(
        Query::update()
            .table("gh_effects")
            .value("state", state)
            .value("next_at", next_at)
            .value("last_error", error)
            .value("result", result.to_string())
            .value("updated", now)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

/// After a restart: what was running may or may not have happened on GitHub.
/// Reconciled from `next_at` on: after a request the restart cut off could still have landed.
pub fn interrupted_effects(c: &mut W<'_>, now: i64, next_at: i64) -> Result<usize> {
    Ok(c.exec(
        Query::update()
            .table("gh_effects")
            .value("state", "unknown")
            .value("next_at", next_at)
            .value("last_error", "重启时还在进行")
            .value("updated", now)
            .and_where(Expr::col("state").eq("running")),
    )? as usize)
}

/// Whether a comment id is already the result of another effect (reconciling a comment).
pub fn comment_claimed(c: &mut impl Read, comment_id: i64) -> Result<bool> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("gh_effects")
            .and_where(Expr::col("kind").eq("comment"))
            .and_where(Expr::col("state").eq("done"))
            .and_where(
                dialect
                    .json_field(Expr::col("result"), "comment")
                    .eq(comment_id),
            )
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn automation(c: &mut impl Read, id: &str, ws: Option<&str>) -> Result<Option<Value>> {
    c.one(
        Query::select()
            .columns(["automation"])
            .from("gh_deliveries")
            .and_where(Expr::col("delivery_id").eq(id))
            .and_where(Expr::val(ws).is_null().or(Expr::col("workspace_id").eq(ws))),
        |r| {
            Ok(r.get::<Option<crate::contract::Json<Value>>>(0)?
                .map(|v| v.0))
        },
    )
    .map(|row| row.flatten())
}

pub fn previously_disabled(c: &mut impl Read, repo: &str, key: &str) -> Result<bool> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("gh_deliveries")
            .and_where(Expr::col("repo_id").eq(repo))
            .and_where(Expr::col("automation").is_not_null())
            .and_where(dialect.json_field(Expr::col("automation"), "key").eq(key))
            .and_where(
                dialect
                    .json_field(Expr::col("automation"), "reason")
                    .is_in(["disabled", "repo_disabled"]),
            )
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn admits_event(c: &mut impl Read, ws: &str, repo: &str, event: &str) -> Result<bool> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("gh_deliveries")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("repo_id").eq(repo))
            .and_where(Expr::col("automation").is_not_null())
            .and_where(
                dialect
                    .json_field(Expr::col("automation"), "state")
                    .eq("matched"),
            )
            .and_where(
                dialect
                    .json_field(Expr::col("automation"), "event_id")
                    .eq(event),
            )
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn repo_workspaces(c: &mut impl Read) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .columns(["workspace_id"])
            .distinct()
            .from("gh_repos"),
        |r| r.get(0),
    )
}

pub fn set_automation(c: &mut W<'_>, ws: &str, repo: &str, id: &str, data: &Value) -> Result<()> {
    c.exec(
        Query::update()
            .table("gh_deliveries")
            .value("workspace_id", ws)
            .value("repo_id", repo)
            .value("automation", data.to_string())
            .and_where(Expr::col("delivery_id").eq(id)),
    )?;
    Ok(())
}

//! MCP tools for agents: `steve_github_status`, `steve_github_ci_log`,
//! `steve_github_comment`, `steve_github_open_pr`.
//!
//! The tools are always listed; each call first checks that the session's item belongs to a
//! project linked to a repository, or has a linked PR — otherwise [`NOT_LINKED`]. Status and logs
//! read only; a comment posts directly only under a `github.comment` grant, otherwise it (and every
//! PR request) opens a decision and ends the turn.
//!
//! Serving them is the integrator's (like the vault tools): list [`tools`] after the Steve tools,
//! route names for which [`handles`] is true to [`call`] with the session's
//! [`McpCx`]; when the result has `awaiting_approval`, mark the session pending=approval and set
//! the item's `now` exactly as for `steve_request_approval` (the text already ends with END).

use std::sync::LazyLock;

use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_modules::contacts::mcp::McpCx;
use steve_modules::vault::mcp::ToolOut;
use steve_modules::vault::r#use::END;
use steve_modules::vault::{guard, redact};
use steve_store::base::DecisionKind;
use steve_store::base::decisions as base_decisions;
use steve_store::{Item, NewDecision};

use crate::GitHub;
use crate::accept::{self, OPTIONS, PR_OPEN};
use crate::comment::{self, BODY_MAX, Target};
use crate::repo::{self, CheckRow, PrRow, RepoRow};
use crate::repos::check_state;
use crate::sync::latest_checks;

pub const STATUS: &str = "steve_github_status";
pub const CI_LOG: &str = "steve_github_ci_log";
pub const COMMENT: &str = "steve_github_comment";
pub const OPEN_PR: &str = "steve_github_open_pr";

/// The answer when the session's item has nothing to do with GitHub, verbatim.
pub const NOT_LINKED: &str = "This item is not linked to a GitHub repository.";
/// Characters of one log answer.
pub const LOG_MAX: usize = 20_000;
/// Lines kept around each error line, and at the end.
pub const LOG_CONTEXT: usize = 20;
pub const LOG_TAIL: usize = 200;
/// Bytes of a log downloaded (the end is kept).
const LOG_DOWNLOAD: usize = 8 << 20;
const TITLE_MAX: usize = 256;
const INTERNAL: &str = "Steve could not do this right now. Try again later.";

static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let schema = |props: Value, required: &[&str]| {
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": props,
            "required": required,
        })
    };
    vec![
        json!({
            "name": STATUS,
            "description": "Show the pull requests and CI checks linked to this item.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": CI_LOG,
            "description": "Read the log of a failed CI job on this item's pull request.",
            "inputSchema": schema(
                json!({"job": {"type": "string", "description": "a check name or job id from steve_github_status"}}),
                &["job"],
            )
        }),
        json!({
            "name": COMMENT,
            "description": "Post a comment on this item's pull request or issue. The user approves it first unless they granted you permission.",
            "inputSchema": schema(
                json!({
                    "body": {"type": "string"},
                    "target": {"type": "string", "enum": ["pr", "issue"]},
                    "number": {"type": "number"}
                }),
                &["body"],
            )
        }),
        json!({
            "name": OPEN_PR,
            "description": "Ask the user to open a pull request from this item's branch before the item is accepted, for example to run CI or get a review.",
            "inputSchema": schema(
                json!({
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "draft": {"type": "boolean", "description": "default true"}
                }),
                &["title", "body"],
            )
        }),
    ]
});

/// The tool definitions, `[{name, description, inputSchema}]`, in listing order.
pub fn tools() -> &'static [Value] {
    &TOOLS
}

/// Is `name` one of this module's tools?
pub fn handles(name: &str) -> bool {
    matches!(name, STATUS | CI_LOG | COMMENT | OPEN_PR)
}

/// Tools that only read (safe to auto-allow in the harness, like steve-acp's `OWN_TOOLS`). The
/// other two have outward effects only after your approval (or grant), and end the turn.
pub const READ_ONLY: [&str; 2] = [STATUS, CI_LOG];

fn received(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// zod-style argument errors, like the other tools.
fn validate(tool: &str, args: &Value, fields: &[(&str, bool, &str)]) -> Result<(), String> {
    let mut issues = Vec::new();
    for (k, required, ty) in fields {
        let v = args.get(*k);
        let ok = match (v, *ty) {
            (None, _) => !required,
            (Some(Value::String(_)), "string") => true,
            (Some(Value::Number(_)), "number") => true,
            (Some(Value::Bool(_)), "boolean") => true,
            _ => false,
        };
        if !ok {
            issues.push(format!(
                "Invalid input: expected {ty}, received {} at {k}",
                received(v)
            ));
        }
    }
    if let Some(t) = args.get("target").and_then(Value::as_str)
        && tool == COMMENT
        && t != "pr"
        && t != "issue"
    {
        issues.push(format!(
            "Invalid option: expected one of \"pr\"|\"issue\" at target (received \"{t}\")"
        ));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {tool}: {}",
            issues.join("\n")
        ))
    }
}

fn s<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn internal(e: impl std::fmt::Display) -> String {
    tracing::error!(error = %e, "github tool failed");
    INTERNAL.into()
}

fn out(text: impl Into<String>) -> ToolOut {
    ToolOut {
        text: text.into(),
        awaiting_approval: false,
    }
}

/// What the session's item has on GitHub.
struct Linked {
    item: Item,
    /// The repo of the item's project, when linked and enabled.
    project_repo: Option<RepoRow>,
    /// The item's PRs with their repos: open first, then the newest.
    prs: Vec<(RepoRow, PrRow)>,
}

impl Linked {
    /// Every repo involved: the project's, then those of the PRs.
    fn repos(&self) -> Vec<&RepoRow> {
        let mut v: Vec<&RepoRow> = self.project_repo.iter().collect();
        for (r, _) in &self.prs {
            if !v.iter().any(|x| x.id == r.id) {
                v.push(r);
            }
        }
        v
    }
}

fn linked(gh: &GitHub, cx: &McpCx) -> Result<Option<Linked>, String> {
    let Some(item_id) = cx.item_id.as_deref() else {
        return Ok(None);
    };
    let Some(item) = gh
        .store()
        .items()
        .get(item_id)
        .map_err(internal)?
        .filter(|i| i.workspace_id == cx.ws)
    else {
        return Ok(None);
    };
    let ws = cx.ws.clone();
    let (project_repo, prs) = gh
        .store()
        .reader(|c| {
            let pr = match &item.project_id {
                Some(p) => repo::repo_of_project(c, &ws, p)?.filter(|r| r.enabled),
                None => None,
            };
            let mut prs = Vec::new();
            for p in repo::prs(c, &ws, None, false, Some(&item.id))? {
                if let Some(r) = repo::repo_get(c, &p.repo_id)? {
                    prs.push((r, p));
                }
            }
            prs.sort_by_key(|(_, p)| (p.state != "open", -p.updated));
            Ok::<_, ApiError>((pr, prs))
        })
        .map_err(internal)?;
    if project_repo.is_none() && prs.is_empty() {
        return Ok(None);
    }
    Ok(Some(Linked {
        item,
        project_repo,
        prs,
    }))
}

/// Runs one tool. `Err` is the `isError` text (steve-mcp's convention).
pub async fn call(gh: &GitHub, cx: &McpCx, name: &str, args: &Value) -> Result<ToolOut, String> {
    let fields: &[(&str, bool, &str)] = match name {
        STATUS => &[],
        CI_LOG => &[("job", true, "string")],
        COMMENT => &[
            ("body", true, "string"),
            ("target", false, "string"),
            ("number", false, "number"),
        ],
        OPEN_PR => &[
            ("title", true, "string"),
            ("body", true, "string"),
            ("draft", false, "boolean"),
        ],
        _ => return Err(format!("MCP error -32602: Tool {name} not found")),
    };
    validate(name, args, fields)?;
    let Some(l) = linked(gh, cx)? else {
        return Ok(out(NOT_LINKED));
    };
    let r = match name {
        STATUS => status(gh, &l).map(out),
        CI_LOG => ci_log(gh, cx, &l, s(args, "job")).await.map(out),
        COMMENT => {
            let number = args.get("number").and_then(Value::as_f64);
            let issue = s(args, "target") == "issue";
            comment_tool(gh, cx, &l, s(args, "body"), issue, number).await
        }
        _ => {
            let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(true);
            open_pr(gh, cx, &l, s(args, "title"), s(args, "body"), draft)
        }
    };
    if let Err(e) = &r {
        tracing::info!(tool = name, error = %e, "github tool refused");
    }
    r
}

fn icon(k: &CheckRow) -> &'static str {
    match check_state(k) {
        "ok" => "✓",
        "fail" => "✗",
        "skipped" => "–",
        _ => "…",
    }
}

fn sha7(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// `steve_github_status`.
fn status(gh: &GitHub, l: &Linked) -> Result<String, String> {
    if l.prs.is_empty() {
        let r = l.project_repo.as_ref().ok_or(NOT_LINKED)?;
        let mut s = format!(
            "No pull request is linked to this item yet. Repository {} (default branch {}).",
            r.full_name, r.default_branch
        );
        if let Some(b) = l.item.branch.as_deref().filter(|b| !b.is_empty()) {
            s.push_str(&format!(" This item's branch: {b}."));
        }
        return Ok(s);
    }
    let mut blocks = Vec::new();
    for (r, p) in &l.prs {
        let rows = gh
            .store()
            .reader(|c| repo::checks_for_sha(c, &r.id, &p.head_sha).map_err(ApiError::from))
            .map_err(internal)?;
        let state = if p.draft && p.state == "open" {
            "open (draft)"
        } else {
            p.state.as_str()
        };
        let mut lines = vec![format!(
            "#{} {} · {state} · head {}@{} · checks: {}",
            p.number,
            p.title,
            p.head_ref,
            sha7(&p.head_sha),
            p.checks_state
        )];
        if l.repos().len() > 1 {
            lines.push(format!("repository: {}", r.full_name));
        }
        for k in latest_checks(&rows) {
            match k.job_id {
                Some(j) => lines.push(format!("{} {} (job {j})", icon(k), k.name)),
                None => lines.push(format!("{} {}", icon(k), k.name)),
            }
        }
        lines.push(format!("review: {}", p.review_state));
        if let Some(m) = &p.mergeable {
            lines.push(format!("mergeable: {m}"));
        }
        blocks.push(lines.join("\n"));
    }
    Ok(blocks.join("\n\n"))
}

/// The check `job` names among the item's PRs (job id, check run id, or name), failed ones first.
fn find_check(
    gh: &GitHub,
    l: &Linked,
    job: &str,
) -> Result<Option<(RepoRow, PrRow, CheckRow)>, String> {
    let job = job.trim().trim_start_matches('#');
    let mut found: Option<(RepoRow, PrRow, CheckRow)> = None;
    for (r, p) in &l.prs {
        let rows = gh
            .store()
            .reader(|c| repo::checks_for_sha(c, &r.id, &p.head_sha).map_err(ApiError::from))
            .map_err(internal)?;
        for k in latest_checks(&rows) {
            let hit = k.job_id.is_some_and(|j| j.to_string() == job)
                || k.ext_id == job
                || k.name.eq_ignore_ascii_case(job);
            if !hit {
                continue;
            }
            let better = match &found {
                None => true,
                Some((_, _, f)) => check_state(f) != "fail" && check_state(k) == "fail",
            };
            if better {
                found = Some((r.clone(), p.clone(), k.clone()));
            }
        }
    }
    Ok(found)
}

fn is_error_line(l: &str) -> bool {
    l.contains("##[error]")
        || l.contains("FAIL")
        || l.contains("panicked")
        || l.to_lowercase().contains("error")
}

/// The part of a log worth reading: every error line with [`LOG_CONTEXT`] lines around it,
/// plus the last [`LOG_TAIL`] lines; gaps shown as `…`; at most [`LOG_MAX`] characters (the end
/// wins).
pub fn excerpt(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let n = lines.len();
    let mut keep = vec![false; n];
    for (i, l) in lines.iter().enumerate() {
        if is_error_line(l) {
            for k in &mut keep[i.saturating_sub(LOG_CONTEXT)..(i + LOG_CONTEXT + 1).min(n)] {
                *k = true;
            }
        }
    }
    for k in &mut keep[n.saturating_sub(LOG_TAIL)..] {
        *k = true;
    }
    let mut out = String::new();
    let mut gap = false;
    for (i, l) in lines.iter().enumerate() {
        if keep[i] {
            if gap {
                out.push_str("…\n");
            }
            out.push_str(l);
            out.push('\n');
            gap = false;
        } else {
            gap = true;
        }
    }
    let count = out.chars().count();
    if count > LOG_MAX {
        let tail: String = out.chars().skip(count - (LOG_MAX - 2)).collect();
        out = format!("…\n{tail}");
    }
    out
}

/// `steve_github_ci_log`: the job's log, secrets removed, cut to what matters.
async fn ci_log(gh: &GitHub, cx: &McpCx, l: &Linked, job: &str) -> Result<String, String> {
    if job.trim().is_empty() {
        return Err("Give a check name or job id from steve_github_status.".into());
    }
    let Some((r, p, k)) = find_check(gh, l, job)? else {
        return Err(format!(
            "No check \"{}\" on this item's pull requests. Call steve_github_status.",
            job.trim()
        ));
    };
    let Some(job_id) = k.job_id else {
        return Ok(format!(
            "This check is not from GitHub Actions; open {}.",
            k.url
        ));
    };
    let conn = gh.conn(&cx.ws, &r.connection_id).map_err(internal)?;
    let raw = conn
        .get_text(
            &format!("/repos/{}/actions/jobs/{job_id}/logs", r.full_name),
            LOG_DOWNLOAD,
        )
        .await
        .map_err(|e| format!("Could not read the log: {e}"))?;
    // The connection's own token, the values injected into this session, then the paste guard.
    let token = conn.cx().access_token().await.ok();
    let mut text = match &token {
        Some(t) => redact::Redactor::new([("GitHub 令牌", t.expose().as_bytes())])
            .scrub_str(&raw)
            .into_owned(),
        None => raw,
    };
    if let Some(sid) = cx.session_id.as_deref() {
        text = redact::scrub_text(sid, &text).into_owned();
    }
    if let Some(t) = guard::redact(&text) {
        text = t;
    }
    Ok(format!(
        "Log of {} (job {job_id}) on PR #{}, secrets removed:\n{}",
        k.name,
        p.number,
        excerpt(&text).trim_end()
    ))
}

/// The PR or issue a comment goes on.
fn comment_target(
    gh: &GitHub,
    l: &Linked,
    issue: bool,
    number: Option<f64>,
) -> Result<Target, String> {
    let ws = &l.item.workspace_id;
    let repos = l.repos();
    let names = repos
        .iter()
        .map(|r| r.full_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let number = number.map(|n| n as i64).filter(|n| *n > 0);
    let subject = gh
        .store()
        .reader(|c| {
            Ok::<_, ApiError>(match (issue, number) {
                (false, None) => l.prs.first().map(|(_, p)| p.id.clone()),
                (true, None) => repo::issues_of_item(c, ws, &l.item.id)?
                    .into_iter()
                    .next()
                    .map(|i| i.id),
                (false, Some(n)) => {
                    let mut hit = None;
                    for r in &repos {
                        if let Some(p) = repo::pr_by_number(c, &r.id, n)? {
                            hit = Some(p.id);
                            break;
                        }
                    }
                    hit
                }
                (true, Some(n)) => {
                    let mut hit = None;
                    for r in &repos {
                        if let Some(i) = repo::issue_by_number(c, &r.id, n)? {
                            hit = Some(i.id);
                            break;
                        }
                    }
                    hit
                }
            })
        })
        .map_err(internal)?;
    let Some(subject) = subject else {
        return Err(match (issue, number) {
            (false, None) => {
                "This item has no pull request yet. Ask for one with steve_github_open_pr.".into()
            }
            (true, None) => format!("No issue is linked to this item. Give its number in {names}."),
            (false, Some(n)) => format!("No pull request #{n} in {names}."),
            (true, Some(n)) => format!("No issue #{n} in {names}."),
        });
    };
    gh.store()
        .reader(|c| comment::target(c, ws, &subject))
        .map_err(internal)?
        .ok_or_else(|| INTERNAL.to_string())
}

/// `steve_github_comment`.
async fn comment_tool(
    gh: &GitHub,
    cx: &McpCx,
    l: &Linked,
    body: &str,
    issue: bool,
    number: Option<f64>,
) -> Result<ToolOut, String> {
    if body.trim().is_empty() {
        return Err("The comment is empty.".into());
    }
    if body.chars().count() > BODY_MAX {
        return Err(format!("The comment is longer than {BODY_MAX} characters."));
    }
    let t = comment_target(gh, l, issue, number)?;
    let m = gh.connections().modules().clone();
    gh.agent_comment(&m, cx, t, body).await
}

/// `steve_github_open_pr`: a `gh_pr_open` decision; Steve pushes and opens after your yes.
fn open_pr(
    gh: &GitHub,
    cx: &McpCx,
    l: &Linked,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<ToolOut, String> {
    let r = l.project_repo.as_ref().ok_or(NOT_LINKED)?;
    let Some(branch) = l.item.branch.as_deref().filter(|b| !b.is_empty()) else {
        return Err("This item has no branch to open a pull request from.".into());
    };
    let title: String = comment::clean(cx.session_id.as_deref(), title.trim())
        .chars()
        .take(TITLE_MAX)
        .collect();
    if title.trim().is_empty() || title.contains('\n') {
        return Err("Give a one-line title.".into());
    }
    if let Some((_, p)) = l
        .prs
        .iter()
        .find(|(x, p)| x.id == r.id && p.state == "open" && p.head_ref == branch)
    {
        return Err(format!(
            "A pull request from {branch} is already open: {}",
            p.url
        ));
    }
    let body = comment::clean(cx.session_id.as_deref(), body.trim());
    let ws = cx.ws.clone();
    let d = NewDecision {
        workspace_id: ws.clone(),
        kind: DecisionKind::Approve.code().into(),
        subject_type: PR_OPEN.into(),
        subject_id: l.item.id.clone(),
        item_id: Some(l.item.id.clone()),
        session_id: cx.session_id.clone(),
        question: accept::agent_question(&r.full_name, &title, draft),
        draft: Some(format!("{title}\n\n{body}")),
        options: Some(OPTIONS.map(String::from).to_vec()),
        requested_by: if cx.bot_id.is_empty() {
            "bot".into()
        } else {
            cx.bot_id.clone()
        },
    };
    let opened = gh
        .store()
        .transaction(|tx| {
            if repo::has_open_pr_from(tx, &r.id, branch)? {
                return Ok(Err(format!(
                    "A pull request from {branch} is already open."
                )));
            }
            if base_decisions::open_for(tx, &ws, PR_OPEN, &l.item.id)?.is_some() {
                return Ok(Err(
                    "The user has not answered the earlier request to open a pull request yet."
                        .to_string(),
                ));
            }
            base_decisions::open(tx, &d)?;
            Ok::<_, ApiError>(Ok(()))
        })
        .map_err(internal)?;
    opened?;
    Ok(ToolOut {
        text: format!("Approval requested. {END}"),
        awaiting_approval: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_keeps_errors_with_context_and_the_tail() {
        let mut log: Vec<String> = (0..1000).map(|i| format!("line {i}")).collect();
        log[100] = "##[error]Process completed with exit code 1.".into();
        let e = excerpt(&log.join("\n"));
        assert!(e.contains("line 80\n") && e.contains("line 120\n"));
        assert!(!e.contains("line 79\n") && !e.contains("line 121\n"));
        assert!(e.contains("line 800\n") && e.contains("line 999\n"));
        assert!(!e.contains("line 799\n"));
        assert!(e.starts_with("…\nline 80\n"), "{}", &e[..40]);
        assert!(e.contains("line 120\n…\nline 800\n"));
    }

    #[test]
    fn excerpt_is_capped() {
        let log: String = (0..5000)
            .map(|i| format!("error {i} {}\n", "x".repeat(50)))
            .collect();
        let e = excerpt(&log);
        assert!(e.chars().count() <= LOG_MAX);
        assert!(e.contains("error 4999"));
    }

    #[test]
    fn the_four_tools_are_listed() {
        let names: Vec<&str> = tools().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, [STATUS, CI_LOG, COMMENT, OPEN_PR]);
        assert!(names.iter().all(|n| handles(n)));
        assert!(!handles("steve_update"));
    }
}

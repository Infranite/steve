//! What Steve's merge queue asks of GitHub (E1, S6): a pull request per batch (found again when it
//! is there), the checks on a commit, a merge commit with the head locked, closing it and its
//! branch, and handing a pull request to the repository's own merge queue. Plain REST (and one
//! GraphQL mutation) over [`Rest`]: a connection ([`GhConn`]) in the Hub, a bearer token in tests.

use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};

use crate::api::{GhClient, GhConn, GhError};

/// One request, its body as JSON.
#[async_trait]
pub trait Rest: Send + Sync {
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, GhError>;
}

#[async_trait]
impl Rest for GhConn {
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, GhError> {
        Ok(self.send(method, path, body, None).await?.body)
    }
}

/// A client and a whole `Authorization` value.
pub struct Bearer {
    pub client: GhClient,
    pub auth: String,
}

#[async_trait]
impl Rest for Bearer {
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, GhError> {
        Ok(self
            .client
            .request(method, path, Some(&self.auth), body, None)
            .await?
            .body)
    }
}

/// The open pull request from `head` into `base` of `full` (`owner/name`), opened now when there
/// is none.
pub async fn open_pr(
    r: &dyn Rest,
    full: &str,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<Value, GhError> {
    let owner = full.split('/').next().unwrap_or_default();
    let open = r
        .call(
            Method::GET,
            &format!("/repos/{full}/pulls?state=open&head={owner}:{head}&base={base}"),
            None,
        )
        .await?;
    if let Some(p) = open.as_array().and_then(|l| l.first()) {
        return Ok(p.clone());
    }
    r.call(
        Method::POST,
        &format!("/repos/{full}/pulls"),
        Some(&json!({"title": title, "head": head, "base": base, "body": body, "maintainer_can_modify": false})),
    )
    .await
}

pub async fn pull(r: &dyn Rest, full: &str, number: u64) -> Result<Value, GhError> {
    r.call(Method::GET, &format!("/repos/{full}/pulls/{number}"), None)
        .await
}

/// What the checks on `sha` say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checks {
    None,
    /// Some still run; how many ended.
    Pending {
        done: usize,
    },
    Passed,
    /// The ones that failed, a line each, with where to read why.
    Failed(Vec<String>),
}

/// The check runs and commit statuses on `sha`, taken together.
pub async fn checks(r: &dyn Rest, full: &str, sha: &str) -> Result<Checks, GhError> {
    let runs = r
        .call(
            Method::GET,
            &format!("/repos/{full}/commits/{sha}/check-runs?per_page=100"),
            None,
        )
        .await?;
    let status = r
        .call(
            Method::GET,
            &format!("/repos/{full}/commits/{sha}/status"),
            None,
        )
        .await?;
    let runs = runs["check_runs"].as_array().cloned().unwrap_or_default();
    let statuses = status["statuses"].as_array().cloned().unwrap_or_default();
    if runs.is_empty() && statuses.is_empty() {
        return Ok(Checks::None);
    }
    let (mut failed, mut pending, mut done) = (vec![], false, 0);
    for c in &runs {
        let name = c["name"].as_str().unwrap_or("check");
        match (c["status"].as_str(), c["conclusion"].as_str()) {
            (Some("completed"), Some("success" | "neutral" | "skipped")) => done += 1,
            (Some("completed"), Some(other)) => failed.push(format!(
                "{name}: {other} {}",
                c["html_url"].as_str().unwrap_or_default()
            )),
            _ => pending = true,
        }
    }
    for s in &statuses {
        let name = s["context"].as_str().unwrap_or("status");
        match s["state"].as_str() {
            Some("success") => done += 1,
            Some("pending") | None => pending = true,
            Some(other) => failed.push(format!(
                "{name}: {other} {}",
                s["target_url"].as_str().unwrap_or_default()
            )),
        }
    }
    Ok(if !failed.is_empty() {
        Checks::Failed(failed)
    } else if pending {
        Checks::Pending { done }
    } else {
        Checks::Passed
    })
}

/// Merged with a merge commit, only while its head is `sha`: the merge commit's sha.
pub async fn merge(
    r: &dyn Rest,
    full: &str,
    number: u64,
    sha: &str,
    title: &str,
) -> Result<String, GhError> {
    let v = r
        .call(
            Method::PUT,
            &format!("/repos/{full}/pulls/{number}/merge"),
            Some(&json!({"merge_method": "merge", "sha": sha, "commit_title": title})),
        )
        .await?;
    Ok(v["sha"].as_str().unwrap_or(sha).to_string())
}

/// Closed unless merged, and its branch deleted (both best effort).
pub async fn close(r: &dyn Rest, full: &str, number: u64, branch: &str) {
    if let Ok(v) = pull(r, full, number).await
        && v["state"] == "open"
    {
        let _ = r
            .call(
                Method::PATCH,
                &format!("/repos/{full}/pulls/{number}"),
                Some(&json!({"state": "closed"})),
            )
            .await;
    }
    let _ = r
        .call(
            Method::DELETE,
            &format!("/repos/{full}/git/refs/heads/{branch}"),
            None,
        )
        .await;
}

/// The pull request (`node_id`, GraphQL's id of it) handed to the repository's merge queue.
pub async fn enqueue(r: &dyn Rest, node_id: &str) -> Result<(), GhError> {
    let q = json!({
        "query": "mutation($id: ID!) { enqueuePullRequest(input: {pullRequestId: $id}) { mergeQueueEntry { id } } }",
        "variables": {"id": node_id},
    });
    let v = r.call(Method::POST, "/graphql", Some(&q)).await?;
    match &v["errors"] {
        Value::Null => Ok(()),
        e => Err(GhError::Status {
            status: 422,
            message: e.to_string(),
        }),
    }
}

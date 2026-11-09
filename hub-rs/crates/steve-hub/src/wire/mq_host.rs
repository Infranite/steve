//! The merge queue's host (E1, S6): [`steve_core::mq::host::MqHost`] on GitHub, through the
//! project's linked repository and its connection (what any write of the GitHub module uses:
//! refused while the connection is paused or may not write). One pull request per batch, the
//! host's checks read off the batch's commit, a merge commit with the commit locked, and the
//! token a push from the Node goes with.

use async_trait::async_trait;
use serde_json::Value;
use steve_core::mq::exec::Step;
use steve_core::mq::host::{Checks, MqHost, Pr};
use steve_github::api::{GhConn, GhError};
use steve_github::mq as gh;
use steve_github::repo::RepoRow;
use steve_store::Store;

pub struct GitHubHost {
    store: Store,
    gh: steve_github::GitHub,
}

impl GitHubHost {
    pub fn new(store: Store, gh: steve_github::GitHub) -> GitHubHost {
        GitHubHost { store, gh }
    }

    /// The project's linked repository and a connection that may write to it.
    fn conn(&self, project: &str) -> Result<(RepoRow, GhConn), String> {
        let ws: Option<String> = self
            .store
            .reader(|c| {
                use rusqlite::OptionalExtension;
                c.query_row(
                    "SELECT workspace_id FROM projects WHERE id = ?",
                    [project],
                    |r| r.get(0),
                )
                .optional()
                .map_err(steve_store::StoreError::from)
            })
            .map_err(|e| e.to_string())?;
        let ws = ws.ok_or_else(|| format!("no project {project}"))?;
        let repo = self
            .store
            .reader(|c| {
                steve_github::repo::repo_of_project(c, &ws, project)
                    .map_err(steve_store::StoreError::from)
            })
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "这个项目没有关联 GitHub 仓库".to_string())?;
        let conn = self
            .gh
            .conn_for_write(&ws, &repo.connection_id)
            .map_err(|e| e.to_string())?;
        Ok((repo, conn))
    }
}

fn pr_of(v: &Value) -> Pr {
    Pr {
        number: v["number"].as_u64().unwrap_or_default(),
        state: v["state"].as_str().unwrap_or_default().to_string(),
        merged: v["merged"].as_bool().unwrap_or(false) || v["merged_at"].is_string(),
        merge_commit: v["merge_commit_sha"].as_str().map(String::from),
        head_sha: v["head"]["sha"].as_str().unwrap_or_default().to_string(),
        node_id: v["node_id"].as_str().map(String::from),
    }
}

/// A write whose answer did not come may have happened: unknown; any other error is a refusal.
fn step_err<T>(e: GhError) -> Step<T> {
    match e {
        GhError::Network => Step::Unknown(e.to_string()),
        GhError::Status { message, .. } => Step::Failed(message),
        e => Step::Failed(e.to_string()),
    }
}

#[async_trait]
impl MqHost for GitHubHost {
    async fn token(&self, project: &str) -> Option<String> {
        let (_, conn) = self.conn(project).ok()?;
        let t = conn.cx().access_token().await.ok()?;
        Some(t.expose().to_string())
    }

    async fn open_pr(
        &self,
        project: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Step<Pr> {
        match self.conn(project) {
            Ok((r, conn)) => {
                match gh::open_pr(&conn, &r.full_name, head, base, title, body).await {
                    Ok(v) => Step::Done(pr_of(&v)),
                    Err(e) => step_err(e),
                }
            }
            Err(e) => Step::Failed(e),
        }
    }

    async fn pr(&self, project: &str, number: u64) -> Step<Pr> {
        match self.conn(project) {
            Ok((r, conn)) => match gh::pull(&conn, &r.full_name, number).await {
                Ok(v) => Step::Done(pr_of(&v)),
                Err(e) => step_err(e),
            },
            Err(e) => Step::Failed(e),
        }
    }

    async fn checks(&self, project: &str, sha: &str) -> Step<Checks> {
        match self.conn(project) {
            Ok((r, conn)) => match gh::checks(&conn, &r.full_name, sha).await {
                Ok(gh::Checks::None) => Step::Done(Checks::None),
                Ok(gh::Checks::Pending { done }) => Step::Done(Checks::Pending { done }),
                Ok(gh::Checks::Passed) => Step::Done(Checks::Passed),
                Ok(gh::Checks::Failed(f)) => Step::Done(Checks::Failed(f.join("\n"))),
                Err(e) => step_err(e),
            },
            Err(e) => Step::Failed(e),
        }
    }

    async fn merge(&self, project: &str, number: u64, sha: &str, title: &str) -> Step<String> {
        match self.conn(project) {
            Ok((r, conn)) => match gh::merge(&conn, &r.full_name, number, sha, title).await {
                Ok(c) => Step::Done(c),
                Err(e) => step_err(e),
            },
            Err(e) => Step::Failed(e),
        }
    }

    async fn close(&self, project: &str, number: u64, branch: &str) {
        if let Ok((r, conn)) = self.conn(project) {
            gh::close(&conn, &r.full_name, number, branch).await;
        }
    }

    async fn enqueue(&self, project: &str, pr: &Pr) -> Step<()> {
        let Some(id) = &pr.node_id else {
            return Step::Failed("the pull request has no node id".into());
        };
        match self.conn(project) {
            Ok((_, conn)) => match gh::enqueue(&conn, id).await {
                Ok(()) => Step::Done(()),
                Err(e) => step_err(e),
            },
            Err(e) => Step::Failed(e),
        }
    }
}

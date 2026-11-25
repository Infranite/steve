//! The core's way to an item's Node for its git (N0e-E11b, [`steve_core::node_git`]): facts are
//! read in the item's bound workspace with the Node's git face, and a git action (the rebase
//! before a release) goes through the Hub's ledger, so a restart asks the Node what became of it
//! instead of doing it again.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use steve_core::node_git::{Applied, Ask, NodeGit};
use steve_node_api::*;

use super::workspaces::{At, PlacementError, Subject, Workspaces};

/// How long an applied action is waited for before it counts as unknown.
const APPLY_WAIT: Duration = Duration::from_secs(170);

pub struct ItemGit {
    ws: Arc<Workspaces>,
}

impl ItemGit {
    pub fn new(ws: Arc<Workspaces>) -> ItemGit {
        ItemGit { ws }
    }

    /// The item's Node and workspace there.
    fn place(&self, item: &str) -> ApiResult<(super::workspaces::Placement, WorkspaceRef)> {
        let p = self
            .ws
            .placement_of(Subject::Item(item))
            .map_err(|e| match e {
                PlacementError::Offline { .. } => Error::new(Code::Unavailable, e.to_string()),
                e => Error::new(Code::Internal, e.to_string()),
            })?;
        match &p.at {
            At::Workspace(w) => {
                let w = w.clone();
                Ok((p, w))
            }
            At::Root(_) => Err(Error::new(
                Code::Internal,
                "the item is in no workspace on its Node now",
            )),
        }
    }
}

#[async_trait]
impl NodeGit for ItemGit {
    fn on_node(&self, item: &str) -> bool {
        self.ws.item_on_node(item)
    }

    async fn facts(&self, item: &str, q: Ask) -> ApiResult<GitFacts> {
        let (p, w) = self.place(item)?;
        p.node
            .git_facts(GitQuery {
                workspace: w,
                refs: q.refs,
                ranges: q.ranges,
                fetch: q.fetch,
                reads: q.reads,
                heads_only: q.heads_only,
            })
            .await
    }

    async fn apply(
        &self,
        item: &str,
        operation: &str,
        subject: &str,
        on: &GitFacts,
        action: GitAction,
    ) -> Applied {
        let (p, w) = match self.place(item) {
            Ok(x) => x,
            Err(e) => return Applied::Failed(e),
        };
        let body = GitApply {
            workspace: w,
            expected: on.revision.clone(),
            expected_head: on.head.clone(),
            action,
        };
        if let Err(e) = p.runtime.submit(operation, subject, "git_apply", &body) {
            return Applied::Failed(Error::new(Code::Internal, e.to_string()));
        }
        p.runtime
            .wait_ledger_until(operation, APPLY_WAIT, |res| match res {
                Ok(Some(e)) if e.is_settled() => std::ops::ControlFlow::Break(settled(e)),
                Ok(_) => std::ops::ControlFlow::Continue(()),
                Err(e) => std::ops::ControlFlow::Break(Applied::Unknown(e.to_string())),
            })
            .await
            .unwrap_or_else(|| Applied::Unknown("still running on its Node".into()))
    }
}

/// What a settled ledger entry says.
fn settled(e: super::node_runtime::LedgerEntry) -> Applied {
    match e.state.as_str() {
        "rejected" => Applied::Failed(
            e.refusal
                .unwrap_or_else(|| Error::new(Code::Internal, "refused")),
        ),
        "unknown" => Applied::Unknown(match e.receipt.map(|r| r.state) {
            Some(Observed::Unknown { reason }) => reason,
            _ => "its Node does not know".into(),
        }),
        _ => match e.receipt.map(|r| r.state) {
            Some(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Git(f)),
                ..
            }) => Applied::Done(f),
            Some(Observed::Finished { error: Some(e), .. }) => Applied::Failed(e),
            Some(Observed::Unknown { reason }) => Applied::Unknown(reason),
            other => Applied::Failed(Error::new(
                Code::Internal,
                format!("ended without its facts: {other:?}"),
            )),
        },
    }
}

//! An item's worktree made and given back by its Node (N0e-E11a). The core plans: it names where
//! the worktree goes and whether it may be given back; the Node does the git work off the core's
//! lock, and what it came to is told back ([`WorkspaceFact`]) once committed.
//!
//! - A session bound with a worktree plan is prepared before it opens (the bind's two commands).
//!   A prepare that fails or is refused ends the open that waited on it (the session is let go
//!   with a note, and the business side hears why); the next message prepares again.
//! - A worktree it made and that was tidied since is prepared again before the session opens
//!   again: the Node makes it from the kept branch, where it was.
//! - A tidy goes through the ledger (`tidy_workspace`, subject `tidy:<item>`): written before it
//!   is sent, never sent blind twice, refused by the Node when the workspace changed since the
//!   prepare it names or a harness in it would not stop.

use super::{
    intent::{Advance, next_revision},
    *,
};
use steve_store::node_commands::dispatch;
use steve_store::node_sessions::worktree::{self, Tidy as TidyState};

/// What a workspace prepare or tidy came to.
#[derive(Debug, Clone)]
pub enum WorkspaceFact {
    /// The workspace `session` waits for is there.
    Prepared {
        session: String,
        view: WorkspaceView,
    },
    /// It could not be made: `error` is the Node's.
    PrepareFailed {
        session: String,
        error: Option<Error>,
    },
    /// The tidy of the item's worktree is over: `removed`, or it is kept (dirty, changed since,
    /// refused). `note`: why it was kept, or what else could not be done.
    Tidied {
        item: String,
        removed: bool,
        note: Option<String>,
    },
}

/// Resolve from this session's recorded binding, never from the item's current project.
fn resource_in(
    r: &mut impl steve_store::Read,
    session: &str,
    node: &str,
) -> Result<Option<(Option<String>, String)>, StoreError> {
    let Some(worktree::Binding { workspace, item }) = worktree::binding(r, session, node)? else {
        return Ok(None);
    };
    if workspace.node.as_str() != node {
        return Ok(None);
    }
    let resource = match worktree::initial_prepare(r, session, node)? {
        Some(prepare) => Some(prepare.resource),
        None => item.as_ref().map(|i| format!("item:{i}")),
    };
    Ok(resource
        .filter(|r| workspace.id.as_str() == workspace_id(r))
        .map(|r| (item, r)))
}

/// The workspace id the Node gives `resource` (as the binding names it).
fn workspace_id(resource: &str) -> String {
    derived_id("w", resource)
}

impl NodeRuntime {
    /// `session` is bound to this Node and works in a worktree the Node makes (and makes again
    /// after a tidy).
    pub fn makes_workspace(&self, session: &str) -> Result<bool, StoreError> {
        let node = self.node_id.as_str();
        self.store.read(|r| {
            let Some((_, resource)) = resource_in(r, session, node)? else {
                return Ok(false);
            };
            Ok(worktree::last_prepare(r, node, &resource)?
                .is_some_and(|p| matches!(p.body.kind, PrepareKind::Worktree { .. })))
        })
    }

    /// True when `session`'s worktree is created by this Node and does not currently exist on disk
    /// because its initial prepare has not succeeded yet or it was tidied and will be prepared
    /// again on the next flush.
    pub fn pending_workspace_prepare(&self, session: &str) -> Result<bool, StoreError> {
        let node = self.node_id.as_str();
        self.store.read(|r| {
            let Some((_, resource)) = resource_in(r, session, node)? else {
                return Ok(false);
            };
            let Some(last) = worktree::last_prepare(r, node, &resource)? else {
                return Ok(false);
            };
            if !matches!(last.body.kind, PrepareKind::Worktree { .. }) {
                return Ok(false);
            }
            if last.state != "finished" || !last.success {
                return Ok(true);
            }
            Ok(
                worktree::tidy_since(r, node, &workspace_id(&resource), last.rowid)?
                    == Some(TidyState::Removed),
            )
        })
    }

    /// Before `session` opens again: when the worktree it works in is one this Node makes, and
    /// it was tidied since it was made, or its last prepare failed or was refused, a new prepare of
    /// the same spec (the open waits for it). Its operation, or `None`: nothing to prepare.
    pub(super) fn prepare_again_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        advance: Advance,
    ) -> Result<Option<String>, StoreError> {
        let node = self.node_id.as_str();
        let prepare = self.store.read(|r| {
            let Some((_, resource)) = resource_in(r, session, node)? else {
                return Ok::<_, StoreError>(None);
            };
            let Some(last) = worktree::last_prepare(r, node, &resource)? else {
                return Ok(None);
            };
            if !matches!(last.body.kind, PrepareKind::Worktree { .. }) {
                return Ok(None);
            }
            let again = match last.state.as_str() {
                "finished" if last.success => {
                    worktree::tidy_since(r, node, &workspace_id(&resource), last.rowid)?
                        == Some(TidyState::Removed)
                }
                "finished" | "rejected" => matches!(advance, Advance::ExplicitRetry),
                _ => false,
            };
            Ok(again.then_some(last.body))
        })?;
        let Some(prepare) = prepare else {
            return Ok(None);
        };
        let key = next_revision(tx, Intent::Prepare(idv(session.to_string())?), advance)?;
        let op = key.op().to_string();
        let (req, digest) = self.command(&op, prepare)?;
        rt::insert_intent_in(
            tx,
            &key,
            &rt::NewCommand {
                operation: &op,
                node,
                method: "prepare_workspace",
                request: &req,
                digest: &digest,
                target_attempt: None,
                dependency: None,
            },
            self.clock.now(),
        )?;
        Ok(Some(op))
    }

    /// Gives back the worktree this Node made for `item`, through the ledger; `keep_branch`: its
    /// branch stays. `true`: on its way (or one is already, or one whose outcome nobody knows
    /// waits for reconciliation, or kept after a non-retryable failure). `false`: this Node has no worktree of
    /// the item now (none made, or given back already): nothing was done.
    pub fn tidy_workspace(&self, item: &str, keep_branch: bool) -> Result<bool, StoreError> {
        self.running()?;
        let session = self
            .store
            .read(|r| worktree::latest_item_session(r, self.node_id.as_str(), item))?;
        let Some(session) = session else {
            return Ok(false);
        };
        self.tidy_session_workspace(&session, keep_branch)
    }

    /// Tidy exactly the workspace this session used; earlier project generations are retained.
    pub fn tidy_session_workspace(
        &self,
        session: &str,
        keep_branch: bool,
    ) -> Result<bool, StoreError> {
        self.running()?;
        let node = self.node_id.as_str();
        let result = self.write(|tx| {
            let plan = self.store.read(|r| -> Result<_, StoreError> {
                let Some((Some(item), resource)) = resource_in(r, session, node)? else {
                    return Ok(None);
                };
                let Some(last) = worktree::last_prepare(r, node, &resource)? else {
                    return Ok(None);
                };
                let (true, Some(view), PrepareKind::Worktree { .. }) =
                    (last.success, last.view, &last.body.kind)
                else {
                    return Ok(None);
                };
                let prior = worktree::tidy_since(r, node, &workspace_id(&resource), last.rowid)?;
                Ok(Some((item, view, prior)))
            })?;
            let Some((item, view, prior)) = plan else {
                return Ok(false);
            };
            match prior {
                Some(TidyState::Removed) => return Ok(false),
                Some(TidyState::Kept) => return Ok(true),
                None => {}
            }
            let key = next_revision(
                tx,
                Intent::Tidy(view.workspace.clone()),
                Advance::NewWorkspace,
            )?;
            let op = key.op().to_string();
            let body = Tidy {
                workspace: view.workspace,
                expected: view.revision,
                keep_branch,
            };
            let (mut request, _) = self.command(&op, body)?;
            request["subject"] = format!("tidy:{item}").into();
            let digest = digest_of(&request).map_err(conflict)?.to_string();
            rt::insert_intent_in(
                tx,
                &key,
                &rt::NewCommand {
                    operation: &op,
                    node,
                    method: "tidy_workspace",
                    request: &request,
                    digest: &digest,
                    target_attempt: None,
                    dependency: None,
                },
                self.clock.now(),
            )?;
            Ok(true)
        })?;
        if result {
            self.wake_send.notify_one();
        }
        Ok(result)
    }

    /// A prepare finished: made (the session waiting on it hears so), or not.
    pub(super) fn prepared_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        receipt: &Receipt,
        detail: &str,
    ) -> Result<Option<End>, StoreError> {
        let (view, error) = match &receipt.state {
            Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Workspace(v)),
                ..
            } => (Some(v.clone()), None),
            Observed::Finished { error, .. } => (None, error.clone()),
            _ => (None, None),
        };
        match view {
            Some(view) => {
                if let Some(session) = command_any_session_in(&self.store, op)? {
                    self.staged_workspace
                        .lock()
                        .unwrap()
                        .push(WorkspaceFact::Prepared { session, view });
                }
                Ok(None)
            }
            None => self.prepare_failed_in(tx, op, error.as_ref(), detail),
        }
    }

    /// A prepare that failed or was refused: the open waiting on it never goes (it would wait
    /// forever), the session is let go with a note, and the next message prepares again. The
    /// business side hears it as the turn's end when a turn waited, otherwise as a failed prepare
    /// (not as a turn end too).
    pub(super) fn prepare_failed_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        error: Option<&Error>,
        detail: &str,
    ) -> Result<Option<End>, StoreError> {
        let Some(session) = command_any_session_in(&self.store, op)? else {
            return Ok(None);
        };
        let opens = self.store.read(|r| dispatch::pending_dependents(r, op))?;
        for o in &opens {
            node_write::merge_receipt_in(
                tx,
                o,
                ReceiptState::Rejected,
                false,
                &json!({ "reason": "its prepare failed" }),
                self.clock.now(),
            )?;
        }
        if opens.is_empty() {
            return Ok(None);
        }
        // a turn that waited for the open is over (its end says why); with none, the session's
        // start could not prepare its worktree, which the business side says in its own words
        let end = self
            .opened_in(tx, &session, false, detail)?
            .filter(|e| e.attempt.is_some());
        if end.is_none() {
            self.staged_workspace
                .lock()
                .unwrap()
                .push(WorkspaceFact::PrepareFailed {
                    session,
                    error: error.cloned(),
                });
        }
        Ok(end)
    }

    /// A tidy finished or was refused (`removed`: it succeeded).
    pub(super) fn tidied_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        removed: bool,
        detail: &str,
    ) -> Result<(), StoreError> {
        let subject = dispatch::subject(tx, op)?;
        let Some(item) = subject.and_then(|s| s.strip_prefix("tidy:").map(String::from)) else {
            return Ok(());
        };
        self.staged_workspace
            .lock()
            .unwrap()
            .push(WorkspaceFact::Tidied {
                item,
                removed,
                note: (!detail.is_empty()).then(|| detail.to_string()),
            });
        Ok(())
    }
}

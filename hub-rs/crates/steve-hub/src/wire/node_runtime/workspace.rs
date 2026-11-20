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

use super::*;

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

/// The last prepare of a resource on a Node.
struct LastPrepare {
    rowid: i64,
    body: Prepare,
    state: String,
    success: bool,
    view: Option<WorkspaceView>,
}

/// Resolve from this session's recorded binding, never from the item's current project.
/// The initial prepare survives retries; old bindings without that command may use the legacy ID.
fn resource_in(
    tx: &rusqlite::Connection,
    session: &str,
    node: &str,
) -> Result<Option<String>, StoreError> {
    let bound: Option<(String, String, Option<String>)> = tx.query_row(
        "SELECT r.node_id,r.workspace_ref,s.item_id FROM runtime_sessions r JOIN sessions s ON s.id=r.session_id WHERE r.session_id=? AND r.node_id=?",
        [session, node], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    let Some((node, reference, item)) = bound else {
        return Ok(None);
    };
    let reference: WorkspaceRef = serde_json::from_str(&reference)?;
    if reference.node.as_str() != node {
        return Ok(None);
    }
    let request: Option<String> = tx.query_row(
        "SELECT request FROM node_commands WHERE operation_id=? AND node_id=? AND method='prepare_workspace'",
        rusqlite::params![format!("op_prep_{session}"),node], |r| r.get(0),
    ).optional()?;
    if let Some(request) = request {
        let resource = serde_json::from_str::<Command<Prepare>>(&request)?
            .body
            .resource;
        if reference.id.as_str() == workspace_id(&resource) {
            return Ok(Some(resource));
        }
        return Ok(None);
    }
    Ok(item
        .map(|i| format!("item:{i}"))
        .filter(|r| reference.id.as_str() == workspace_id(r)))
}

fn last_prepare_in(
    tx: &rusqlite::Connection,
    node: &str,
    resource: &str,
) -> Result<Option<LastPrepare>, StoreError> {
    let row: Option<(i64, String, String, Option<String>)> = tx
        .query_row(
            "SELECT rowid, request, state, receipt FROM node_commands
              WHERE node_id=? AND method='prepare_workspace' AND json_extract(request,'$.body.resource')=?
              ORDER BY rowid DESC LIMIT 1",
            [node, resource],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((rowid, request, state, receipt)) = row else {
        return Ok(None);
    };
    let body = serde_json::from_str::<Command<Prepare>>(&request)?.body;
    let receipt: Option<Value> = receipt.map(|r| serde_json::from_str(&r)).transpose()?;
    let success = receipt
        .as_ref()
        .and_then(|r| r.get("success"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let view = receipt
        .and_then(|r| r.get("payload").cloned())
        .and_then(|p| serde_json::from_value::<Receipt>(p).ok())
        .and_then(|r| match r.state {
            Observed::Finished {
                result: Some(OperationResult::Workspace(v)),
                ..
            } => Some(v),
            _ => None,
        });
    Ok(Some(LastPrepare {
        rowid,
        body,
        state,
        success,
        view,
    }))
}

/// The workspace id the Node gives `resource` (as the binding names it).
fn workspace_id(resource: &str) -> String {
    derived_id("w", resource)
}

/// A tidy of the workspace written after the prepare at `rowid`: `Some(true)` it removed it,
/// `Some(false)` one is on its way, unknown, or must be kept after a non-retryable failure;
/// `None` none (or only retryable failures).
fn tidy_since_in(
    tx: &rusqlite::Connection,
    node: &str,
    workspace: &str,
    rowid: i64,
) -> Result<Option<bool>, StoreError> {
    // G4-W31B-RETRY-SHAPE: remove the legacy string fallback with migration 116.
    let (done, open): (bool, bool) = tx.query_row(
        "SELECT
           EXISTS(SELECT 1 FROM node_commands WHERE node_id=?1 AND method='tidy_workspace' AND json_extract(request,'$.body.workspace.id')=?2 AND rowid>?3 AND state='finished' AND json_extract(receipt,'$.success')=1),
           EXISTS(SELECT 1 FROM node_commands WHERE node_id=?1 AND method='tidy_workspace' AND json_extract(request,'$.body.workspace.id')=?2 AND rowid>?3 AND (state NOT IN ('finished','rejected') OR (state='rejected' AND COALESCE(json_extract(receipt,'$.payload.retry.kind'),json_extract(receipt,'$.payload.retry'))='never') OR (state='finished' AND json_extract(receipt,'$.success')=0 AND COALESCE(json_extract(receipt,'$.payload.state.error.retry.kind'),json_extract(receipt,'$.payload.state.error.retry'))='never')))",
        rusqlite::params![node, workspace, rowid],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(if done {
        Some(true)
    } else if open {
        Some(false)
    } else {
        None
    })
}

/// The session's item is paused.
pub(super) fn item_paused_in(tx: &rusqlite::Connection, session: &str) -> Result<bool, StoreError> {
    Ok(tx
        .query_row(
            "SELECT i.status='pause' FROM sessions s JOIN items i ON i.id=s.item_id WHERE s.id=?",
            [session],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

impl NodeRuntime {
    /// `session` is bound to this Node and works in a worktree the Node makes (and makes again
    /// after a tidy).
    pub fn makes_workspace(&self, session: &str) -> Result<bool, StoreError> {
        let node = self.node_id.as_str();
        self.store.reader(|tx| -> Result<_, StoreError> {
            let bound: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
                [session, node],
                |r| r.get(0),
            )?;
            if !bound {
                return Ok(false);
            }
            let Some(resource) = resource_in(tx, session, node)? else {
                return Ok(false);
            };
            Ok(last_prepare_in(tx, node, &resource)?
                .is_some_and(|p| matches!(p.body.kind, PrepareKind::Worktree { .. })))
        })
    }

    /// Before `session` opens again: when the worktree it works in is one this Node makes, and
    /// it was tidied since it was made, or its last prepare failed or was refused, a new prepare of
    /// the same spec (the open waits for it). Its operation, or `None`: nothing to prepare.
    pub(super) fn prepare_again_in(
        &self,
        tx: &Tx,
        session: &str,
    ) -> Result<Option<String>, StoreError> {
        let node = self.node_id.as_str();
        let Some(resource) = resource_in(tx, session, node)? else {
            return Ok(None);
        };
        let Some(last) = last_prepare_in(tx, node, &resource)? else {
            return Ok(None);
        };
        if !matches!(last.body.kind, PrepareKind::Worktree { .. }) {
            return Ok(None);
        }
        let again = match last.state.as_str() {
            "finished" if last.success => {
                tidy_since_in(tx, node, &workspace_id(&resource), last.rowid)? == Some(true)
            }
            "finished" | "rejected" => true,
            // on its way, or unknown (never sent again)
            _ => false,
        };
        if !again {
            return Ok(None);
        }
        let op = uid(&format!("op_prep_{session}"));
        let (req, digest) = command(&op, last.body)?;
        rt::insert_command_in(
            tx,
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
        let session: Option<String> = self.store.reader(|tx| {
            tx.query_row("SELECT s.id FROM sessions s JOIN runtime_sessions r ON r.session_id=s.id WHERE s.item_id=? AND r.node_id=? ORDER BY s.created DESC,s.no DESC LIMIT 1",
                [item, self.node_id.as_str()], |r| r.get(0)).optional().map_err(StoreError::from)
        })?;
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
        let selected = self.store.reader(|tx| {
            let item: Option<String> = tx
                .query_row("SELECT item_id FROM sessions WHERE id=?", [session], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            Ok::<_, StoreError>(item.zip(resource_in(tx, session, node)?))
        })?;
        let Some((item, resource)) = selected else {
            return Ok(false);
        };
        let wid = workspace_id(&resource);
        let plan = self.store.reader(|tx| -> Result<_, StoreError> {
            let Some(last) = last_prepare_in(tx, node, &resource)? else {
                return Ok(None);
            };
            let (true, Some(view), PrepareKind::Worktree { .. }) =
                (last.success, last.view, &last.body.kind)
            else {
                return Ok(None);
            };
            match tidy_since_in(tx, node, &wid, last.rowid)? {
                Some(true) => Ok(None),
                Some(false) => Ok(Some(None)),
                None => Ok(Some(Some(view))),
            }
        })?;
        let view = match plan {
            None => return Ok(false),
            Some(None) => return Ok(true),
            Some(Some(v)) => v,
        };
        self.submit(
            &uid("op_tidy"),
            &format!("tidy:{item}"),
            "tidy_workspace",
            Tidy {
                workspace: view.workspace,
                expected: view.revision,
                keep_branch,
            },
        )?;
        Ok(true)
    }

    /// A prepare finished: made (the session waiting on it hears so), or not.
    pub(super) fn prepared_in(
        &self,
        tx: &Tx,
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
                if let Some(session) = command_any_session_in(tx, op)? {
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
        tx: &Tx,
        op: &str,
        error: Option<&Error>,
        detail: &str,
    ) -> Result<Option<End>, StoreError> {
        let Some(session) = command_any_session_in(tx, op)? else {
            return Ok(None);
        };
        let opens: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT operation_id FROM node_commands WHERE dependency_operation=? AND state='pending'",
            )?;
            st.query_map([op], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for o in &opens {
            rt::merge_receipt_in(
                tx,
                o,
                ReceiptState::Rejected,
                false,
                &json!({ "reason": "its prepare failed" }),
                now(),
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
        tx: &Tx,
        op: &str,
        removed: bool,
        detail: &str,
    ) -> Result<(), StoreError> {
        let subject: Option<String> = tx
            .query_row(
                "SELECT json_extract(request,'$.subject') FROM node_commands WHERE operation_id=?",
                [op],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
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

#[cfg(test)]
mod retry_shape_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tidy_reads_saved_never_and_retryable_failures_in_both_retry_shapes() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE node_commands(node_id TEXT,method TEXT,request TEXT,state TEXT,receipt TEXT)").unwrap();
        for legacy in [false, true] {
            for rejected in [false, true] {
                for never in [false, true] {
                    db.execute("DELETE FROM node_commands", []).unwrap();
                    let retry = if legacy {
                        json!(if never { "never" } else { "after_reconcile" })
                    } else if never {
                        json!({"kind":"never"})
                    } else {
                        json!({"kind":"later","after_ms":null})
                    };
                    let error = json!({"code":"internal","retry":retry,"safe_detail":"retained"});
                    let receipt = if rejected {
                        json!({"success":false,"payload":error})
                    } else {
                        json!({"success":false,"payload":{"state":{"error":error}}})
                    };
                    db.execute(
                        "INSERT INTO node_commands VALUES ('n1','tidy_workspace',?1,?2,?3)",
                        rusqlite::params![
                            json!({"body":{"workspace":{"id":"w1"}}}).to_string(),
                            if rejected { "rejected" } else { "finished" },
                            receipt.to_string()
                        ],
                    )
                    .unwrap();
                    assert_eq!(
                        tidy_since_in(&db, "n1", "w1", 0).unwrap(),
                        if never { Some(false) } else { None },
                        "legacy={legacy}, rejected={rejected}, never={never}"
                    );
                }
            }
        }
    }
}

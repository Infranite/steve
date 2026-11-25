//! The Hub's ledger of workspace operations that change things and must not run twice: a command
//! run (`exec`), a git action (`git_apply`: a merge, a rebase, a push) and giving a worktree back
//! (`tidy_workspace`). Each one is written to
//! `node_commands` with what it is for (its subject, kept in the stored request: D10, no
//! migration) before it is sent, and goes out through the coordinator's sender like every other
//! command.
//!
//! Exactly once: a command written but never sent (the Hub stopped in between) is sent after the
//! restart, once. One that may have reached the Node (`maybe_sent`) is never sent blind again: the
//! Hub asks the Node ([`ExecutionControl::operation`]) and takes what it says; if the Node has no
//! record of it, it becomes `unknown` rather than running a second time. An `unknown` one is never
//! sent again.

use super::*;
use serde::Serialize;
use steve_store::node_commands;

/// The methods that go through the ledger.
pub(super) const LEDGER_METHODS: [&str; 3] = ["exec", "git_apply", "tidy_workspace"];

/// What a ledger operation may be for (`release:<item>`: the rebase before an item's release).
const SUBJECTS: [&str; 5] = ["flow_run:", "vault_use:", "push:", "tidy:", "release:"];

fn valid_subject(s: &str) -> bool {
    SUBJECTS
        .iter()
        .any(|p| s.strip_prefix(p).is_some_and(|rest| !rest.is_empty()))
}

/// A ledger operation as the Hub holds it.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub operation: String,
    pub subject: String,
    pub method: String,
    /// `pending`, `accepted`, `active`, `unknown`, `finished` or `rejected`.
    pub state: String,
    /// The Node's last receipt (accepted, running, unknown or finished with its result).
    pub receipt: Option<Receipt>,
    /// Why the Node refused it before accepting it (`rejected`).
    pub refusal: Option<Error>,
}

impl LedgerEntry {
    /// Its state can no longer change by itself: finished, refused or unknown (which waits for a
    /// person or a reconciliation, never for a resend).
    pub fn is_settled(&self) -> bool {
        matches!(self.state.as_str(), "finished" | "rejected" | "unknown")
    }
}

impl NodeRuntime {
    /// Writes the operation `op` (`method` with `body`, for `subject`) to the ledger; the sender
    /// sends it once the Hub has reconciled with the Node. True: written now. False: it was there
    /// already (the same subject, method and body): nothing new. The same id for anything else is
    /// a conflict.
    pub fn submit<T: Serialize>(
        &self,
        op: &str,
        subject: &str,
        method: &str,
        body: T,
    ) -> Result<bool, StoreError> {
        self.running()?;
        if !LEDGER_METHODS.contains(&method) {
            return Err(conflict(format!("{method} does not go through the ledger")));
        }
        if !valid_subject(subject) {
            return Err(conflict(format!("not a ledger subject: {subject}")));
        }
        let body = serde_json::to_value(body).map_err(conflict)?;
        let (mut request, _) = self.command(op, body.clone())?;
        // the subject is the Hub's: it stays in the stored request and never reaches the Node
        // (the sender parses the request back into the Node's command)
        request["subject"] = Value::from(subject);
        let digest = digest_of(&request).map_err(conflict)?.to_string();
        let fresh = self.write(|tx| {
            if let Some(c) = self.store.read(|r| node_commands::record(r, op, None))? {
                let same = c.node == self.node_id.as_str()
                    && c.method == method
                    && c.subject.as_deref() == Some(subject)
                    && c.request.get("body") == Some(&body);
                return if same {
                    Ok(false)
                } else {
                    Err(conflict("operation identity reused"))
                };
            }
            node_write::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: op,
                    node: self.node_id.as_str(),
                    method,
                    request: &request,
                    digest: &digest,
                    target_attempt: None,
                    dependency: None,
                },
                self.clock.now(),
            )
        })?;
        self.wake_send.notify_one();
        Ok(fresh)
    }

    /// The ledger operation `op` of this Node, as it stands.
    pub fn ledger(&self, op: &str) -> Result<Option<LedgerEntry>, StoreError> {
        let node = self.node_id.as_str();
        let Some(row) = self
            .store
            .read(|r| node_commands::record(r, op, Some(node)))?
        else {
            return Ok(None);
        };
        if !LEDGER_METHODS.contains(&row.method.as_str()) {
            return Ok(None);
        }
        let (receipt, refusal) = match (row.state.as_str(), row.receipt.and_then(|r| r.payload)) {
            (_, None) => (None, None),
            ("rejected", Some(p)) => (None, serde_json::from_value(p).ok()),
            (_, Some(p)) => (serde_json::from_value(p).ok(), None),
        };
        Ok(Some(LedgerEntry {
            operation: row.operation,
            subject: row.subject.unwrap_or_default(),
            method: row.method,
            state: row.state,
            receipt,
            refusal,
        }))
    }

    /// `op`, a ledger `exec` or push the Node can no longer tell the end of, stopped now rather
    /// than at its deadline (you asked for what it was for to run again, or a push is compared): a recover stops what is left of
    /// it and it ends interrupted. True: it is not unknown any more, or a recover of it is on its
    /// way or was planned now.
    pub fn stop_lost(&self, op: &str) -> Result<bool, StoreError> {
        self.running()?;
        let planned = self.write(|tx| {
            if self.recover_exec_in(tx, op, "a step whose command was lost is run again")? {
                return Ok(true);
            }
            let state = self
                .store
                .read(|r| node_commands::state(r, self.node_id.as_str(), op))?;
            if state.as_deref() != Some("unknown") {
                return Ok(true);
            }
            self.retiring_in(op, true)
        })?;
        self.wake_send.notify_one();
        Ok(planned)
    }

    /// Dispatches a workspace mutation after the sender has ruled out a prior uncertain send.
    pub(super) async fn send_ledger(&self, l: &rt::CommandLease) -> ApiResult<Receipt> {
        fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> ApiResult<Command<T>> {
            serde_json::from_value(v.clone())
                .map_err(|e| Error::new(Code::Invalid, format!("stored command: {e}")))
        }
        match l.method.as_str() {
            "exec" => self.node.exec(parse(&l.request)?).await,
            "git_apply" => self.node.git_apply(parse(&l.request)?).await,
            "tidy_workspace" => self.node.tidy_workspace(parse(&l.request)?).await,
            other => Err(Error::new(
                Code::Unsupported,
                format!("{other} is not a ledger operation"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::valid_subject;

    #[test]
    fn subjects_name_what_the_operation_is_for() {
        for s in [
            "flow_run:r1",
            "vault_use:u1",
            "push:i1",
            "tidy:i1",
            "release:i1",
        ] {
            assert!(valid_subject(s), "{s}");
        }
        for s in ["", "flow_run:", "push", "other:x", "session:s1", "release:"] {
            assert!(!valid_subject(s), "{s}");
        }
    }
}

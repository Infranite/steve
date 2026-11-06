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

/// The methods that go through the ledger.
pub(super) const LEDGER_METHODS: [&str; 3] = ["exec", "git_apply", "tidy_workspace"];

/// How long a push may run on a Node (the Node's own deadline, counted there from when it took
/// it): an unknown one written longer ago than this is past it.
pub(crate) const PUSH_DEADLINE: i64 = 5 * 60 * 1000;

/// What a ledger operation may be for.
const SUBJECTS: [&str; 5] = ["flow_run:", "vault_use:", "push:", "lane:", "tidy:"];

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
    pub refusal: Option<ApiError>,
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
        let (mut request, _) = command(op, body.clone())?;
        // the subject is the Hub's: it stays in the stored request and never reaches the Node
        // (the sender parses the request back into the Node's command)
        request["subject"] = Value::from(subject);
        let digest = digest_of(&request).map_err(conflict)?.to_string();
        let fresh = self.tx(|tx| {
            if let Some(c) = rt::command_in(tx, op)? {
                let same = c.node == self.node_id.as_str()
                    && c.method == method
                    && c.request.get("subject").and_then(Value::as_str) == Some(subject)
                    && c.request.get("body") == Some(&body);
                return if same {
                    Ok(false)
                } else {
                    Err(conflict("operation identity reused"))
                };
            }
            rt::insert_command_in(
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
                now(),
            )
        })?;
        self.wake_send.notify_one();
        Ok(fresh)
    }

    /// The ledger operation `op` of this Node, as it stands.
    pub fn ledger(&self, op: &str) -> Result<Option<LedgerEntry>, StoreError> {
        let node = self.node_id.as_str();
        self.store.reader(|c| {
            let row: Option<(String, String, String, Option<String>)> = c
                .query_row(
                    "SELECT method, request, state, receipt FROM node_commands WHERE operation_id=? AND node_id=?",
                    [op, node],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((method, request, state, receipt)) = row else {
                return Ok(None);
            };
            if !LEDGER_METHODS.contains(&method.as_str()) {
                return Ok(None);
            }
            let request: Value = serde_json::from_str(&request)?;
            let payload = receipt
                .map(|r| serde_json::from_str::<Value>(&r))
                .transpose()?
                .and_then(|v| v.get("payload").cloned());
            let (receipt, refusal) = match (state.as_str(), payload) {
                (_, None) => (None, None),
                ("rejected", Some(p)) => (None, serde_json::from_value(p).ok()),
                (_, Some(p)) => (serde_json::from_value(p).ok(), None),
            };
            Ok(Some(LedgerEntry {
                operation: op.into(),
                subject: request
                    .get("subject")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                method,
                state,
                receipt,
                refusal,
            }))
        })
    }

    /// `op`, a ledger `exec` or push the Node can no longer tell the end of, stopped now rather
    /// than at its deadline (you asked for what it was for to run again, or a push is compared): a recover stops what is left of
    /// it and it ends interrupted. True: it is not unknown any more, or a recover of it is on its
    /// way or was planned now.
    pub fn stop_lost(&self, op: &str) -> Result<bool, StoreError> {
        self.running()?;
        let planned = self.tx(|tx| {
            if self.recover_exec_in(tx, op, "a step whose command was lost is run again")? {
                return Ok(true);
            }
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM node_commands WHERE operation_id=? AND node_id=?",
                    [op, self.node_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            if state.as_deref() != Some("unknown") {
                return Ok(true);
            }
            self.retiring_in(tx, op, true)
        })?;
        self.wake_send.notify_one();
        Ok(planned)
    }

    /// Ledger `exec`s and pushes (`git_apply`) the Node can no longer tell the end of (a restart
    /// cut them off) whose deadline has passed: each gets a recover, so its process group does not
    /// outlive the deadline the command was given and its workspace can be tidied, whether or not
    /// what it was for asks about it again. A push's deadline is [`PUSH_DEADLINE`] after it was
    /// written (the Node's starts later, when it takes it). How many were planned.
    pub(crate) fn recover_lost_execs(&self, now: i64) -> Result<usize, StoreError> {
        let due: Vec<String> = self.store.reader(|c| {
            let mut st = c.prepare(
                "SELECT operation_id FROM node_commands WHERE node_id=?1 AND state='unknown'
                   AND ((method='exec' AND COALESCE(json_extract(request,'$.body.limits.deadline'), 0) <= ?2)
                        OR (method='git_apply' AND created + ?3 <= ?2))",
            )?;
            st.query_map(rusqlite::params![self.node_id.as_str(), now, PUSH_DEADLINE], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
        })?;
        let mut n = 0;
        for op in due {
            if self.tx(|tx| {
                self.recover_exec_in(
                    tx,
                    &op,
                    "a command whose outcome is unknown, past its deadline",
                )
            })? {
                n += 1;
            }
        }
        if n > 0 {
            self.wake_send.notify_one();
        }
        Ok(n)
    }

    /// Sends a ledger operation. One that may have been sent before is asked about instead: the
    /// Node's receipt if it has one, `unknown` if it has none (it is never run a second time).
    pub(super) async fn send_ledger(&self, l: &rt::CommandLease) -> ApiResult<Receipt> {
        fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> ApiResult<Command<T>> {
            serde_json::from_value(v.clone())
                .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("stored command: {e}")))
        }
        let id = OperationId::try_from(l.operation.clone())
            .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("stored command: {e}")))?;
        if l.maybe_sent {
            return match self.node.operation(id.clone()).await {
                Ok(r) => Ok(r),
                Err(e) if e.code == ErrorCode::Invalid => Ok(Receipt {
                    operation: id,
                    state: Observed::Unknown {
                        reason: "an earlier send may have reached the Node, which has no record of it: not sent again".into(),
                    },
                }),
                Err(e) => Err(e),
            };
        }
        match l.method.as_str() {
            "exec" => self.node.exec(parse(&l.request)?).await,
            "git_apply" => self.node.git_apply(parse(&l.request)?).await,
            "tidy_workspace" => self.node.tidy_workspace(parse(&l.request)?).await,
            other => Err(ApiError::new(
                ErrorCode::Unsupported,
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
            "lane:l1:u2",
            "tidy:i1",
        ] {
            assert!(valid_subject(s), "{s}");
        }
        for s in ["", "flow_run:", "push", "other:x", "session:s1"] {
            assert!(!valid_subject(s), "{s}");
        }
    }
}

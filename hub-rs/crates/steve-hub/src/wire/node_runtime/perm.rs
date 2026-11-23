//! Permission cards of Node sessions (D5): a request of one attempt becomes one card with one answer
//! command at most, closed by its `expires_at` on the Hub's clock, by the Node's expiry or by the
//! attempt's end. A late answer never reaches a newer attempt: the card, its answer and the
//! resolution are all bound to the request's own attempt.

use super::*;
use steve_core::approval::{Outcome, Request};
use steve_store::node_commands::dispatch;
use steve_store::runtime::PermissionRow;

/// How a card closed by time reads.
const EXPIRED: &str = "审批已过期";
/// How a card closed by its attempt's end reads.
const SUPERSEDED: &str = "这一轮已经结束，授权不再需要";
/// Cards the sweep closes per round.
const SWEEP: i64 = 50;

impl NodeRuntime {
    /// A permission event in `tx`: a request becomes a card (an `approve` decision on the tool
    /// call), its `permission` entry and its row; an answer or an expiry settles the card if it is
    /// still open.
    pub(super) fn permission_event_in(
        &self,
        tx: &mut W<'_>,
        ev: &NodeEvent,
    ) -> Result<Option<After>, StoreError> {
        match &ev.body {
            EventBody::PermissionRequested { request } => self.requested_in(tx, request),
            EventBody::PermissionResolved { resolution } => {
                let attempt = resolution.execution.attempt().as_str();
                let Some(row) = node_write::permission_in(
                    tx,
                    self.node_id.as_str(),
                    attempt,
                    resolution.id.as_str(),
                )?
                else {
                    tracing::warn!(permission = %resolution.id, attempt, "resolution of no known request");
                    return Ok(None);
                };
                let asked: PermissionRequest = serde_json::from_value(row.request.clone())?;
                if asked.digest != resolution.action_digest {
                    tracing::warn!(permission = %resolution.id, "resolution for another action");
                    return Ok(None);
                }
                // expired or refused: the Node confirms the Hub's own reject, the card stays as it
                // is. Applied already: the answer's receipt came before this event, and the card
                // is resolved now.
                let moved = match row.state.as_str() {
                    "waiting" | "answering" => node_write::permission_cas_in(
                        tx,
                        &row.decision,
                        row.revision,
                        "applied",
                        None,
                        now(),
                    )?,
                    "applied" => true,
                    _ => false,
                };
                if moved {
                    let chose = match resolution.decision {
                        PermissionDecision::AllowOnce => Some(ALLOW),
                        PermissionDecision::Reject => None,
                    };
                    steve_store::decision_repo::settle_permission(
                        tx,
                        &row.decision,
                        &json!({ "optionId": chose }),
                        "resolved",
                    )?;
                }
                Ok(None)
            }
            EventBody::PermissionExpired { id } => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some(row) = node_write::permission_in(
                        tx,
                        self.node_id.as_str(),
                        x.attempt().as_str(),
                        id.as_str(),
                    )?
                {
                    self.close_in(tx, &row, SUPERSEDED)?;
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn requested_in(
        &self,
        tx: &mut W<'_>,
        request: &PermissionRequest,
    ) -> Result<Option<After>, StoreError> {
        let Some(session) = exec_session(&request.execution) else {
            return Ok(None);
        };
        let attempt = request.execution.attempt().as_str();
        if node_write::permission_in(tx, self.node_id.as_str(), attempt, request.id.as_str())?
            .is_some()
        {
            // the same request again (a replay): its card is there
            return Ok(None);
        }
        // an attempt this Hub planned on this Node, of that very session (whatever the body says)
        let planned = self.store.read(|r| {
            node_turns::planning::planned_here(r, self.node_id.as_str(), &session, attempt)
        })?;
        if !planned {
            tracing::warn!(permission = %request.id, attempt, "request of an attempt the Hub never planned on this Node");
            return Ok(None);
        }
        let resolution = {
            let desired = self.domain.approval(&session)?;
            let native = desired.as_ref().and_then(|m| m.native);
            let applied =
                rt::session_modes(tx, self.node_id.as_str(), &session)?.and_then(|m| m.applied);
            steve_core::approval::resolve(
                desired.map(|m| m.policy).unwrap_or_default(),
                native.filter(|_| applied.as_deref() == Some("auto")),
                Request::Permission(request),
            )
        };
        let scope = node_sessions::startup::scope(tx, &session)?;
        let options: Vec<String> = request
            .choices
            .iter()
            .map(|c| match c {
                PermissionDecision::AllowOnce => format!("{ALLOW}|允许一次|allow_once"),
                PermissionDecision::Reject => format!("{REJECT}|拒绝|reject_once"),
            })
            .collect();
        let d = steve_store::DecisionRows(tx).open(&steve_store::NewDecision {
            workspace_id: scope.workspace,
            kind: "approve".into(),
            subject_type: "tool_call".into(),
            subject_id: request.id.to_string(),
            item_id: scope.item,
            session_id: Some(session.clone()),
            question: format!("允许执行：{}", request.title),
            draft: Some(
                serde_json::to_string_pretty(&request.action)
                    .unwrap_or_default()
                    .chars()
                    .take(2000)
                    .collect(),
            ),
            options: Some(options),
            requested_by: scope.bot.unwrap_or_else(|| "agent".into()),
        })?;
        event_in(
            tx,
            &session,
            "permission",
            json!({ "decision": d.id, "title": request.title }),
        )?;
        node_write::insert_permission_in(
            tx,
            &rt::NewPermission {
                decision: &d.id,
                node: self.node_id.as_str(),
                session: &session,
                attempt,
                permission: request.id.as_str(),
                request: &serde_json::to_value(request).map_err(conflict)?,
                expires_at: request.expires_at.0,
                resolver: resolution.resolver,
            },
            now(),
        )?;
        if let Some(row) = node_write::permission_of_decision_in(tx, &d.id)? {
            if request.expires_at.0 <= now() {
                self.expire_card_in(tx, &row, EXPIRED)?;
            } else if let Outcome::Answer(choice) = resolution.outcome {
                let op = self.answer_command_in(tx, &row, choice)?;
                if !node_write::permission_cas_in(
                    tx,
                    &d.id,
                    row.revision,
                    "answering",
                    Some(&op),
                    now(),
                )? {
                    return Err(conflict("permission changed while recording the answer"));
                }
                if choice == PermissionDecision::Reject {
                    steve_store::decision_repo::settle_permission(
                        tx,
                        &d.id,
                        &json!({"superseded": resolution.resolver, "unapplied": true}),
                        "expired",
                    )?;
                }
                self.wake_send.notify_one();
            }
        }
        let outcome = if request.expires_at.0 <= now() {
            "expired"
        } else {
            match resolution.outcome {
                Outcome::OwnerDecision => "owner_decision",
                Outcome::Answer(PermissionDecision::Reject) => "reject",
                Outcome::Answer(PermissionDecision::AllowOnce) => "allow_once",
            }
        };
        Ok(Some(After::PermissionIngress {
            resolver: resolution.resolver,
            outcome,
        }))
    }

    /// The one answer command of a card (keyed on the card, so a request id the Node uses again in
    /// another attempt gets its own), bound to the very request: its attempt and action digest.
    fn answer_command_in(
        &self,
        tx: &mut W<'_>,
        row: &PermissionRow,
        choice: PermissionDecision,
    ) -> Result<String, StoreError> {
        let request: PermissionRequest = serde_json::from_value(row.request.clone())?;
        let op = format!("op_answer_{}", row.decision);
        if self.store.read(|r| dispatch::exists(r, &op))? {
            return Ok(op);
        }
        let (req, digest) = command(
            &op,
            PermissionResolution {
                id: request.id.clone(),
                execution: request.execution.clone(),
                action_digest: request.digest.clone(),
                decision_revision: idv(derived_id("d", &row.decision))?,
                decision: choice,
            },
        )?;
        node_write::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "resolve_permission",
                request: &req,
                digest: &digest,
                target_attempt: Some(&row.attempt),
                dependency: None,
            },
            self.clock.now(),
        )?;
        Ok(op)
    }

    /// A waiting card whose time is up: expired, and the Node (if its attempt still runs) is told
    /// to reject the request. True: a command was queued.
    fn expire_card_in(
        &self,
        tx: &mut W<'_>,
        row: &PermissionRow,
        why: &str,
    ) -> Result<bool, StoreError> {
        if row.state != "waiting" {
            return Ok(false);
        }
        let running = self
            .attempt_row(&row.attempt)?
            .is_some_and(|a| a.state != "finished");
        let op = if running {
            Some(self.answer_command_in(tx, row, PermissionDecision::Reject)?)
        } else {
            None
        };
        if !node_write::permission_cas_in(
            tx,
            &row.decision,
            row.revision,
            "expired",
            op.as_deref(),
            now(),
        )? {
            return Err(conflict("the permission changed while expiring it"));
        }
        steve_store::decision_repo::settle_permission(
            tx,
            &row.decision,
            &json!({ "superseded": why }),
            "expired",
        )?;
        Ok(op.is_some())
    }

    /// An open card the Node no longer waits for (it expired the request, or its attempt is over):
    /// expired, with no command. An answer that was on its way was never applied.
    pub(super) fn close_in(
        &self,
        tx: &mut W<'_>,
        row: &PermissionRow,
        why: &str,
    ) -> Result<(), StoreError> {
        if !matches!(row.state.as_str(), "waiting" | "answering") {
            return Ok(());
        }
        node_write::permission_cas_in(tx, &row.decision, row.revision, "expired", None, now())?;
        if row.state == "answering" {
            steve_store::decision_repo::expire_unapplied(
                tx,
                &row.decision,
                &json!({ "superseded": why, "unapplied": true }),
            )?;
        } else {
            steve_store::decision_repo::settle_permission(
                tx,
                &row.decision,
                &json!({ "superseded": why }),
                "expired",
            )?;
        }
        Ok(())
    }

    /// The attempt is over: nothing of it can be answered any more, whatever the Node said.
    pub(super) fn close_cards_of_attempt_in(
        &self,
        tx: &mut W<'_>,
        attempt: &str,
    ) -> Result<(), StoreError> {
        for row in node_write::permissions_of_attempt_in(tx, attempt)? {
            self.close_in(tx, &row, SUPERSEDED)?;
        }
        Ok(())
    }

    /// What the Node said about an answer command: applied, or refused (the card never claims an
    /// answer the Node did not take).
    pub(super) fn answer_receipt_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        success: bool,
        error: Option<&ApiError>,
    ) -> Result<(), StoreError> {
        let Some(row) = node_write::permission_of_answer_in(tx, op)? else {
            return Ok(());
        };
        if row.state != "answering" {
            // our own reject of an expired card, refused or not: the card is closed already
            return Ok(());
        }
        if success {
            node_write::permission_cas_in(tx, &row.decision, row.revision, "applied", None, now())?;
            return Ok(());
        }
        node_write::permission_cas_in(tx, &row.decision, row.revision, "refused", None, now())?;
        let code = error
            .map(|e| serde_json::to_value(e.code).unwrap_or(Value::Null))
            .unwrap_or(Value::Null);
        steve_store::decision_repo::expire_unapplied(
            tx,
            &row.decision,
            &json!({ "expired": true, "refused": code }),
        )?;
        Ok(())
    }

    /// Closes the cards whose `expires_at` has come (Hub clock). True: a reject was queued.
    pub(super) fn expire_due(&self) -> bool {
        let r = self.write(|tx| {
            let mut queued = false;
            for row in node_write::due_permissions_in(tx, self.node_id.as_str(), now(), SWEEP)? {
                queued |= self.expire_card_in(tx, &row, EXPIRED)?;
            }
            Ok(queued)
        });
        match r {
            Ok(q) => {
                if q {
                    self.wake_send.notify_one();
                }
                q
            }
            Err(e) => {
                tracing::error!("expire permission cards: {e}");
                false
            }
        }
    }

    /// The answer to a permission card of a Node session, as a command to the Node bound to the
    /// very request (its attempt, its action digest). `false`: the request is no longer waiting
    /// (answered, expired, or its attempt is over).
    pub fn answer_permission(
        &self,
        decision: &str,
        option: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.running()?;
        let (answered, queued) = self.write(|tx| {
            let Some(row) = node_write::permission_of_decision_in(tx, decision)? else {
                return Ok((false, false));
            };
            let card = self
                .store
                .decisions()
                .get(decision)?
                .ok_or_else(|| StoreError::Rejected(format!("missing decision {decision}")))?;
            if card.status == "open" && row.state == "answering" {
                return Ok((true, false)); // already submitted, only the Node's receipt settles
            }
            if card.status != "open" || row.state != "waiting" {
                return Ok((false, false));
            }
            if let Some(why) = self
                .store
                .read(|c| steve_store::workflow_gate::session_blocker(c, &row.session))?
            {
                // A pause/cancel/replan can precede a late human answer. Commit the
                // expired card and a rejection together; never grant the old attempt new access.
                return Ok((false, self.expire_card_in(tx, &row, &why)?));
            }
            if row.expires_at <= now() {
                // too late: closed now (committed), not answered
                return Ok((false, self.expire_card_in(tx, &row, EXPIRED)?));
            }
            let request: PermissionRequest = serde_json::from_value(row.request.clone())?;
            let choice = match option {
                Some(ALLOW) if request.choices.contains(&PermissionDecision::AllowOnce) => {
                    PermissionDecision::AllowOnce
                }
                _ => PermissionDecision::Reject,
            };
            let op = self.answer_command_in(tx, &row, choice)?;
            if !node_write::permission_cas_in(
                tx,
                &row.decision,
                row.revision,
                "answering",
                Some(&op),
                now(),
            )? {
                return Err(conflict("the permission changed while answering it"));
            }
            Ok((true, true))
        })?;
        if queued {
            self.wake_send.notify_one();
        }
        Ok(answered)
    }
}

//! Permission cards of Node sessions (D5): a request of one attempt becomes one card with one answer
//! command at most, closed by its `expires_at` on the Hub's clock, by the Node's expiry or by the
//! attempt's end. A late answer never reaches a newer attempt: the card, its answer and the
//! resolution are all bound to the request's own attempt.

use super::{
    intent::{Advance, next_revision},
    *,
};
use steve_core::approval::{Outcome, Request};
use steve_core::{PermissionAction, PermissionAnswer};
use steve_store::decision_repo::DecisionChange;
use steve_store::{DecisionRows, runtime::PermissionRow};
use steve_types::DecisionStatus;

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
                // A Node event corroborates an answer already recorded by the Hub.
                if row.state == "answering" {
                    node_write::permission_cas_in(
                        tx,
                        &row.decision,
                        row.revision,
                        "applied",
                        None,
                        self.clock.now(),
                    )?;
                } else if row.state == "waiting" {
                    // The Node closed the request before the Hub recorded a choice.
                    node_write::permission_cas_in(
                        tx,
                        &row.decision,
                        row.revision,
                        "expired",
                        None,
                        self.clock.now(),
                    )?;
                    steve_store::decision_repo::expire_unapplied(
                        tx,
                        &row.decision,
                        &json!({ "expired": true, "reason": "closed_by_node" }),
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
        // an attempt this Hub planned on this Node, of that very session (whatever the body says)
        let planned =
            node_turns::planning::planned_here(tx, self.node_id.as_str(), &session, attempt)?;
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
        let (d, opened) = steve_store::DecisionRows(tx).open_in(&steve_store::NewDecision {
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
            question_template: None,
            source: Some(steve_store::DecisionSource::Permission {
                node: self.node_id.to_string(),
                attempt: attempt.into(),
            }),
        })?;
        if !opened {
            return Ok(None);
        }
        event_in(
            tx,
            &session,
            "permission",
            json!({ "decision": d.id, "title": request.title }),
        )?;
        let at = self.clock.now();
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
            at,
        )?;
        if let Some(row) = node_write::permission_of_decision_in(tx, &d.id)? {
            if request.expires_at.0 <= at {
                Self::expire_card_in(tx, &row, EXPIRED, at)?;
            } else if let Outcome::Answer(choice) = resolution.outcome {
                let op = Self::answer_command_in(tx, &row, choice, at)?;
                if !node_write::permission_cas_in(
                    tx,
                    &d.id,
                    row.revision,
                    "answering",
                    Some(&op),
                    at,
                )? {
                    return Err(conflict("permission changed while recording the answer"));
                }
                if choice == PermissionDecision::Reject
                    && !DecisionRows(tx).apply(
                        &d,
                        DecisionChange::Expire,
                        &json!({"superseded": resolution.resolver, "unapplied": true}),
                        None,
                    )?
                {
                    return Err(conflict(
                        "permission decision changed while settling reject",
                    ));
                }
            }
        }
        let outcome = if request.expires_at.0 <= at {
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
        tx: &mut W<'_>,
        row: &PermissionRow,
        choice: PermissionDecision,
        at: i64,
    ) -> Result<String, StoreError> {
        let request: PermissionRequest = serde_json::from_value(row.request.clone())?;
        if let Some(op) = &row.answer_operation {
            return Ok(op.clone());
        }
        let revision = DecisionRows(tx)
            .get(&row.decision)?
            .ok_or_else(|| conflict("missing permission decision"))?
            .revision;
        let key = next_revision(
            tx,
            Intent::Answer(row.decision.clone().into()),
            Advance::NewInput,
        )?;
        let op = key.op().to_string();
        let (req, digest) = command_at(
            &op,
            PermissionResolution {
                id: request.id.clone(),
                execution: request.execution.clone(),
                action_digest: request.digest.clone(),
                decision_revision: idv(derived_id("d", &format!("{}:{revision}", row.decision)))?,
                decision: choice,
            },
            at,
        )?;
        rt::insert_intent_in(
            tx,
            &key,
            &rt::NewCommand {
                operation: &op,
                node: &row.node,
                method: "resolve_permission",
                request: &req,
                digest: &digest,
                target_attempt: Some(&row.attempt),
                dependency: None,
            },
            at,
        )?;
        Ok(op)
    }

    /// A waiting card whose time is up: expired, and the Node (if its attempt still runs) is told
    /// to reject the request. True: a command was queued.
    fn expire_card_in(
        tx: &mut W<'_>,
        row: &PermissionRow,
        why: &str,
        at: i64,
    ) -> Result<bool, StoreError> {
        if row.state != "waiting" {
            return Ok(false);
        }
        let running = node_turns::attempt(tx, &row.attempt)?.is_some_and(|a| a.state != "finished");
        let op = if running {
            Some(Self::answer_command_in(
                tx,
                row,
                PermissionDecision::Reject,
                at,
            )?)
        } else {
            None
        };
        if !node_write::permission_cas_in(
            tx,
            &row.decision,
            row.revision,
            "expired",
            op.as_deref(),
            at,
        )? {
            return Err(conflict("the permission changed while expiring it"));
        }
        steve_store::DecisionRows(tx).settle(
            &row.decision,
            &json!({ "superseded": why }),
            DecisionChange::Expire,
            None,
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
        node_write::permission_cas_in(
            tx,
            &row.decision,
            row.revision,
            "expired",
            None,
            self.clock.now(),
        )?;
        if row.state == "answering" {
            steve_store::decision_repo::expire_unapplied(
                tx,
                &row.decision,
                &json!({ "superseded": why, "unapplied": true }),
            )?;
        } else {
            steve_store::DecisionRows(tx).settle(
                &row.decision,
                &json!({ "superseded": why }),
                DecisionChange::Expire,
                None,
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
        error: Option<&Error>,
    ) -> Result<(), StoreError> {
        let Some(row) = node_write::permission_of_answer_in(tx, op)? else {
            return Ok(());
        };
        if row.state != "answering" {
            // our own reject of an expired card, refused or not: the card is closed already
            return Ok(());
        }
        let at = self.clock.now();
        if success {
            node_write::permission_cas_in(tx, &row.decision, row.revision, "applied", None, at)?;
            return Ok(());
        }
        node_write::permission_cas_in(tx, &row.decision, row.revision, "refused", None, at)?;
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

    /// Each due permission closes with its rejection intent in the same write.
    pub fn expire_permissions(store: &Store, at: i64) -> Result<(), StoreError> {
        let due = store.read(|r| node_write::due_permission_ids(r, at, SWEEP))?;
        let mut failure = None;
        for id in due {
            let result = store.write(|w| {
                if let Some(row) = node_write::permission_of_decision_in(w, &id)?
                    && row.expires_at <= at
                {
                    Self::expire_card_in(w, &row, EXPIRED, at)?;
                }
                Ok::<_, StoreError>(())
            });
            if let Err(e) = result {
                failure = Some(e);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// The answer to a permission card of a Node session, as a command to the Node bound to the
    /// very request (its attempt, its action digest). `false`: the request is no longer waiting
    /// (answered, expired, or its attempt is over).
    pub fn answer_permission(&self, answer: PermissionAnswer<'_>) -> Result<bool, StoreError> {
        let decision = answer.decision_id;
        self.running()?;
        let (answered, queued) = self.write(|tx| {
            let Some(row) = node_write::permission_of_decision_in(tx, decision)? else {
                return Ok((false, false));
            };
            let card = DecisionRows(tx)
                .get(decision)?
                .ok_or_else(|| StoreError::Rejected(format!("missing decision {decision}")))?;
            if card.revision != answer.revision {
                return Err(conflict("the decision changed while answering it"));
            }
            if card.status != DecisionStatus::Open || row.state != "waiting" {
                return Ok((false, false));
            }
            let at = self.clock.now();
            if let Some(why) = steve_store::workflow_gate::session_blocker(tx, &row.session)? {
                // A pause/cancel/replan can precede a late human answer. Commit the
                // expired card and a rejection together; never grant the old attempt new access.
                return Ok((false, Self::expire_card_in(tx, &row, &why, at)?));
            }
            if row.expires_at <= at {
                // too late: closed now (committed), not answered
                return Ok((false, Self::expire_card_in(tx, &row, EXPIRED, at)?));
            }
            let request: PermissionRequest = serde_json::from_value(row.request.clone())?;
            let choice = match answer.option() {
                Some(ALLOW) => PermissionDecision::AllowOnce,
                None => PermissionDecision::Reject,
                Some(_) => return Err(conflict("invalid permission option")),
            };
            if !request.choices.contains(&choice) {
                return Err(conflict("permission did not offer this option"));
            }
            let op = Self::answer_command_in(tx, &row, choice, at)?;
            if !node_write::permission_cas_in(
                tx,
                &row.decision,
                row.revision,
                "answering",
                Some(&op),
                at,
            )? {
                return Err(conflict("the permission changed while answering it"));
            }
            let mut rows = DecisionRows(tx);
            let changed = match answer.action {
                PermissionAction::Resolve { choice, text } => rows.resolve_choice(
                    &card,
                    answer.actor,
                    choice,
                    text,
                    &json!({"optionId": answer.option(), "operation_id": op}),
                    None,
                )?,
                PermissionAction::Dismiss => {
                    let changed = rows.apply(
                        &card,
                        DecisionChange::Dismiss,
                        &json!({"dismissed": true, "operation_id": op}),
                        None,
                    )?;
                    if changed {
                        tx.audit(
                            &card.workspace_id,
                            answer.actor,
                            "decision.dismiss",
                            decision,
                            &json!({"revision": card.revision}),
                        )?;
                    }
                    changed
                }
            };
            if !changed {
                return Err(conflict("the decision changed while answering it"));
            }
            Ok((true, true))
        })?;
        if queued {
            self.wake_send.notify_one();
        }
        Ok(answered)
    }
}

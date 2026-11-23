//! T2, sending: command leasing, Node RPC dispatch, receipt merging, retry backoff and
//! refusal/outcome transitions.

use super::*;
use steve_store::node_commands::dispatch;

/// A worker's hold on a command it sends.
const LEASE_MS: i64 = 60_000;
const MAX_SENDS_PER_REVISION: i64 = 4;

fn retry_at(lease: &rt::CommandLease, at: i64, retry: Retry) -> Option<i64> {
    let Retry::Later { after_ms } = retry else {
        return None;
    };
    let hint = after_ms
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(if after_ms.is_some() { i64::MAX } else { 0 });
    let delay = exhausted_delay(lease.generation).max(hint);
    let due = at.saturating_add(delay);
    let accept_before = lease.request["accept_before"].as_i64()?;
    (lease.generation < MAX_SENDS_PER_REVISION && due <= accept_before).then_some(due)
}

impl NodeRuntime {
    // ── T2: sending ──

    /// A transaction for the send loop, which is an async task. A write waiting for SQLite's
    /// writer (another process holding it, a long transaction) must not hold an async worker
    /// meanwhile, or the runtime's timers and I/O wait with it, and so may the very writer it waits
    /// for. On a multi-thread runtime the worker hands its other tasks on while this blocks; the
    /// transaction still runs here and now, so nothing else happens between it and the send.
    fn send_tx<T>(
        &self,
        f: impl FnOnce(&mut W<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(tokio::runtime::RuntimeFlavor::MultiThread) => {
                tokio::task::block_in_place(|| self.write(f))
            }
            _ => self.write(f),
        }
    }

    /// Poll no later than one second, shortened to a known future retry deadline. The shared
    /// Clock sleep is a full elapsed wait; it does not promise an early wake on wall changes.
    pub(super) fn sender_delay(&self) -> std::time::Duration {
        let now = self.clock.now();
        let next = self
            .store
            .read(|r| dispatch::next_retry(r, self.node_id.as_str(), now))
            .unwrap_or(None);
        let ms = next.map_or(1_000, |at| {
            at.saturating_sub(self.clock.now()).clamp(0, 1_000)
        });
        std::time::Duration::from_millis(ms as u64)
    }

    /// Leases one command, sends it and merges what came back. False: nothing to send.
    pub(super) async fn send_one(&self) -> bool {
        let lease = match self.send_tx(|tx| {
            self.note_waiting_in(tx)?;
            match node_write::lease_command_in(
                tx,
                self.node_id.as_str(),
                &self.owner,
                self.clock.now(),
                self.clock.now() + LEASE_MS,
            )? {
                Some(l) if l.generation == 1 => {
                    freeze_first_envelope_in(&self.store, l, self.clock.now()).map(Some)
                }
                l => Ok(l),
            }
        }) {
            Ok(Some(l)) => l,
            Ok(None) => return false,
            Err(e) => {
                tracing::error!("node command lease: {e}");
                return false;
            }
        };
        let sending = std::time::Instant::now();
        let sent = self.send(&lease).await.and_then(|receipt| {
            if receipt.operation.as_str() == lease.operation {
                return Ok(receipt);
            }
            Err(Error::outcome_unknown(
                &lease.operation,
                "receipt for another operation",
            ))
        });
        let at = self.clock.now();
        let due = sent
            .as_ref()
            .err()
            .and_then(|e| retry_at(&lease, at, e.retry));
        self.meters.command(
            &lease.method,
            sent.as_ref().map(|_| ()).map_err(|e| e.code),
            sending.elapsed().as_secs_f64(),
        );
        // a turn end to report, and a session whose configure was refused for a stale revision
        let r = self.send_tx(|tx| match &sent {
            Ok(receipt) => self.receipt_in(tx, receipt).map(|end| (end, None)),
            Err(e) if (lease.maybe_sent || e.code == Code::OutcomeUnknown) && due.is_none() => {
                // Leasing already persisted maybe_sent=1. Do not resend or call this a refusal:
                // the Node may have executed it. A later receipt/reconcile can settle unknown.
                self.receipt_in(
                    tx,
                    &Receipt {
                        operation: idv(lease.operation.clone())?,
                        state: Observed::Unknown { reason: said(e) },
                    },
                )
                .map(|end| (end, None))
            }
            Err(e) if let Some(due) = due => {
                // not refused for good, or maybe accepted: the very same command again later
                let uncertain = lease.maybe_sent || e.code == Code::OutcomeUnknown;
                if e.code == Code::Exhausted && !uncertain {
                    self.exhausted_in(tx, &lease, e)?;
                }
                node_write::retry_command_in(tx, &lease, at, due, uncertain)?;
                self.meters.retry(&lease.method);
                if !uncertain && !lease.maybe_sent {
                    self.store
                        .write(|w| dispatch::record_retry_error(w, &lease.operation, e))?;
                }
                // a start_turn the user stopped meanwhile, refused before acceptance every time
                // it went: it provably never ran, so it ends here instead of going out again. One
                // earlier uncertain send may still arrive: the cancel waits for the Node instead
                if !uncertain
                    && !lease.maybe_sent
                    && lease.method == "start_turn"
                    && let Some((attempt, session)) = cancelled_start_in(tx, &lease.operation)?
                {
                    self.cancel_unsent_in(tx, &session, &attempt, &lease.operation)?;
                }
                // a job start given up meanwhile, every send of it refused: it never ran either
                if !uncertain
                    && !lease.maybe_sent
                    && lease.method == "start_job"
                    && let Some(attempt) = self
                        .store
                        .read(|r| steve_store::node_jobs::command_attempt(r, &lease.operation))?
                    && steve_store::node_jobs::attempt_in(tx, &attempt)?
                        .is_some_and(|a| a.cancel_requested)
                {
                    self.stop_job_in(tx, &attempt)?;
                }
                Ok((None, None))
            }
            Err(e) => {
                if !rt::refused_before_acceptance_in(tx, &lease)? {
                    return Ok((None, None));
                }
                let e = &e.clone().retry(Retry::Never);
                node_write::merge_receipt_in(
                    tx,
                    &lease.operation,
                    ReceiptState::Rejected,
                    false,
                    &serde_json::to_value(e).map_err(conflict)?,
                    now(),
                )?;
                // refused before it was accepted: it certainly did not run
                if let Some(attempt) = self
                    .store
                    .read(|r| steve_store::node_jobs::command_attempt(r, &lease.operation))?
                {
                    self.job_refused_in(tx, &lease.method, &attempt, e)?;
                    return Ok((None, None));
                }
                let end = match lease.method.as_str() {
                    "start_turn" => self.refused_attempt_in(tx, &lease.operation, e)?,
                    "release_session" => {
                        let s = command_session_in(&self.store, &lease.operation)?;
                        self.released_in(tx, &s, &lease.operation, false)?;
                        None
                    }
                    "open_session" => {
                        let s = command_session_in(&self.store, &lease.operation)?;
                        self.config_opened_in(tx, &s, &lease.operation, false)?;
                        self.opened_in(tx, &s, false, &said(e))?
                    }
                    "abandon" => self.abandon_failed_in(tx, &lease.operation, Some(e))?,
                    "prepare_workspace" => {
                        self.prepare_failed_in(tx, &lease.operation, Some(e), &e.safe_detail)?
                    }
                    "tidy_workspace" => {
                        self.tidied_in(tx, &lease.operation, false, &e.safe_detail)?;
                        None
                    }
                    "resolve_permission" => {
                        self.answer_receipt_in(tx, &lease.operation, false, Some(e))?;
                        None
                    }
                    "configure_session" => {
                        return Ok((None, self.refused_configure_in(tx, &lease.operation, e)?));
                    }
                    "recover" => {
                        self.recover_failed_in(tx, &lease.operation, &e.safe_detail)?;
                        None
                    }
                    _ => None,
                };
                Ok((end, None))
            }
        });
        match r {
            Ok((end, stale)) => {
                if let Some(end) = end {
                    self.tell_end(end);
                }
                if let Some(session) = stale {
                    // a configure refused for a stale revision: sent again with the Node's own
                    let view = match SessionId::try_from(session.clone()) {
                        Ok(sid) => self.node.session(sid).await.ok(),
                        Err(_) => None,
                    };
                    if let Err(e) = self.send_tx(|tx| {
                        self.refresh_configure_in(tx, &session, &lease.operation, view.clone())
                    }) {
                        tracing::error!("refresh configure of {session}: {e}");
                    }
                }
            }
            Err(e) => tracing::error!("node command {}: {e}", lease.operation),
        }
        self.wake_upkeep.notify_one();
        true
    }

    /// Turns waiting because this Node runs as many as its owner set ([`rt::session_room`]): each
    /// session is told once, with the numbers, and its turn goes by itself once one ends. The
    /// mark is on the command, so a restart does not tell it again.
    fn note_waiting_in(&self, tx: &mut W<'_>) -> Result<(), StoreError> {
        let room = node_write::session_room(tx, self.node_id.as_str())?;
        if !room.full() || room.waiting == 0 {
            return Ok(());
        }
        let fresh = node_write::waiting_commands(tx, self.node_id.as_str())?;
        for (op, session) in fresh {
            self.store.write(|w| dispatch::mark_waiting(w, &op))?;
            if let Some(session) = session {
                system_in(
                    tx,
                    &session,
                    &steve_i18n::t!(
                        "hub.node.waiting_for_room",
                        running = room.running,
                        max = room.max
                    ),
                )?;
            }
        }
        Ok(())
    }

    /// A command the Node refused because it is out of room: the refusal is kept on the command
    /// with how many came in a row, which sets its next delay (2 s doubling, at most 60 s). The
    /// first one tells the session, once; the command itself stays the same and is not counted
    /// as an attempt again.
    fn exhausted_in(
        &self,
        tx: &mut W<'_>,
        lease: &rt::CommandLease,
        e: &Error,
    ) -> Result<(), StoreError> {
        let n = self
            .store
            .write(|w| dispatch::record_exhausted(w, &lease.operation, e))?;
        if n == 1
            && let Some(session) = command_any_session_in(&self.store, &lease.operation)?
        {
            system_in(
                tx,
                &session,
                &steve_i18n::t!("hub.node.waiting_for_node", detail = e.safe_detail.as_str()),
            )?;
        }
        Ok(())
    }

    async fn send(&self, l: &rt::CommandLease) -> ApiResult<Receipt> {
        if l.maybe_sent {
            let id = OperationId::try_from(l.operation.clone())
                .map_err(|e| Error::new(Code::Invalid, format!("stored command: {e}")))?;
            // The operation might already have changed its Node. An unavailable journal
            // or a missing receipt cannot authorize another business command.
            return self.node.operation(id).await;
        }
        if l.generation > MAX_SENDS_PER_REVISION {
            return Err(Error::new(Code::Exhausted, "command send budget exhausted"));
        }
        if l.request["accept_before"]
            .as_i64()
            .is_none_or(|end| self.clock.now() > end)
        {
            return Err(Error::new(Code::Stale, "command acceptance window ended"));
        }
        fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> ApiResult<Command<T>> {
            serde_json::from_value(v.clone())
                .map_err(|e| Error::new(Code::Invalid, format!("stored command: {e}")))
        }
        let n = &self.node;
        match l.method.as_str() {
            "prepare_workspace" => n.prepare_workspace(parse(&l.request)?).await,
            "open_session" => {
                let cmd: Command<OpenSession> = parse(&l.request)?;
                // Only before first acceptance: an uncertain command keeps its identity and is
                // reconciled, never manufactured into a rejection while it may already run.
                let ended = self
                    .store
                    .sessions()
                    .get(cmd.body.session.as_str())
                    .map_err(|e| Error::new(Code::Invalid, e.to_string()))?
                    .is_none_or(|s| s.status == "ended");
                if ended {
                    return Err(Error::new(Code::Invalid, "已结束的会话不能重开。"));
                }
                if let Ok(report) = n.probe(false).await {
                    let cap = report
                        .harnesses
                        .iter()
                        .find(|h| h.name == cmd.body.choice.harness);
                    let mut h = crate::harnesses::merge(&cmd.body.choice.harness, None, cap);
                    // Without an adapter report nothing is known about selectable models or
                    // efforts; a declared fallback is not a successful capability probe.
                    if cap.is_none_or(|c| c.adapter.is_none()) {
                        h.ready = None;
                    }
                    let selection = json!({"harness":cmd.body.choice.harness,"model":cmd.body.choice.model,"reasoning_effort":cmd.body.choice.effort});
                    steve_core::models::validate_bot_patch(
                        None,
                        selection.as_object().unwrap(),
                        &[h],
                    )
                    .map_err(|why| Error::new(Code::Invalid, why))?;
                }
                n.open_session(cmd).await
            }
            "start_turn" => {
                let command: Command<TurnSpec> = parse(&l.request)?;
                let retry_allowed = self
                    .store
                    .read(|r| {
                        let attempt = node_turns::started_by(r, &l.operation)?
                            .ok_or_else(|| conflict("missing turn attempt"))?;
                        if node_turns::ordinal(r, &attempt.id)? == 0 {
                            return Ok(true);
                        }
                        Ok::<_, StoreError>(
                            node_turns::retry::before_deadline(
                                r,
                                command.body.turn.as_str(),
                                self.clock.now(),
                            )? && node_turns::retry::allowed(r, command.body.session.as_str())?
                                && config::desired_in(&self.store, command.body.session.as_str())?
                                    .is_none(),
                        )
                    })
                    .map_err(|e| Error::new(Code::Internal, e.to_string()))?;
                if !retry_allowed {
                    return Err(Error::new(
                        Code::Stale,
                        "turn retry expired or is no longer authorized",
                    ));
                }
                let blocked = if self.domain.dynamic_workflows() {
                    None
                } else {
                    self.store
                        .read(|r| {
                            let a = node_turns::started_by(r, &l.operation)?
                                .ok_or_else(|| conflict("missing turn attempt"))?;
                            self.disabled_workflow_in(&a.session)
                        })
                        .map_err(|e| Error::new(Code::Unavailable, e.to_string()))?
                };
                if let Some(why) = blocked {
                    return Err(Error::new(Code::Invalid, why));
                }
                let limit = self
                    .store
                    .read(|c| rt::session_limit(c, self.node_id.as_str()))
                    .map_err(|e| Error::new(Code::Unavailable, format!("session capacity: {e}")))?;
                // Reapply before each start, not once per Hub boot: a separately running Node may
                // have restarted/reconnected meanwhile. This is resource scheduling, not policy.
                n.set_session_capacity(limit as u32).await?;
                n.start_turn(command).await
            }
            "start_job" => n.start_job(parse(&l.request)?).await,
            "cancel" => n.cancel(parse(&l.request)?).await,
            "release_session" => n.release_session(parse(&l.request)?).await,
            "configure_session" => n.configure_session(parse(&l.request)?).await,
            "resolve_permission" => n.resolve_permission(parse(&l.request)?).await,
            "abandon" => n.abandon(parse(&l.request)?).await,
            "recover" => n.recover(parse(&l.request)?).await,
            m if ledger::LEDGER_METHODS.contains(&m) => self.send_ledger(l).await,
            other => Err(Error::new(
                Code::Unsupported,
                format!("the coordinator does not send {other}"),
            )),
        }
    }

    /// What the Node says about a command, from its answer or its stream (the same either way):
    /// merged forward, and an open that finished makes the session ready (or released).
    pub(super) fn receipt_in(
        &self,
        tx: &mut W<'_>,
        receipt: &Receipt,
    ) -> Result<Option<End>, StoreError> {
        let (state, success) = receipt_state(&receipt.state);
        let op = receipt.operation.as_str();
        let Some(method) = self
            .store
            .read(|r| dispatch::method(r, op, self.node_id.as_str()))?
        else {
            return Ok(None);
        };
        let moved = node_write::merge_receipt_in(
            tx,
            op,
            state,
            success,
            &serde_json::to_value(receipt).map_err(conflict)?,
            now(),
        )?;
        if !moved || !rt::is_intent_head_in(tx, op)? {
            return Ok(None);
        }
        if let Some(attempt) = self
            .store
            .read(|r| steve_store::node_jobs::command_attempt(r, op))?
        {
            self.job_receipt_in(tx, op, &method, &attempt, receipt)?;
            return Ok(None);
        }
        let detail = match &receipt.state {
            Observed::Finished { error: Some(e), .. } => said(e),
            Observed::Unknown { reason } => reason.clone(),
            _ => String::new(),
        };
        if method == "recover" {
            self.recover_receipt_in(tx, op, state, success, &detail)?;
            return Ok(None);
        }
        // a target a recover retires: what follows comes with the recover's own end
        let interrupted = matches!(
            &receipt.state,
            Observed::Finished {
                outcome: Outcome::Interrupted,
                ..
            }
        );
        if self.retiring_in(op, interrupted)? {
            return Ok(None);
        }
        if method == "resolve_permission" && state == ReceiptState::Finished {
            let error = match &receipt.state {
                Observed::Finished { error, .. } => error.clone(),
                _ => None,
            };
            self.answer_receipt_in(tx, op, success, error.as_ref())?;
        }
        if method == "configure_session" {
            self.configure_receipt_in(tx, op, state, success, receipt)?;
        }
        match (method.as_str(), state) {
            ("prepare_workspace", ReceiptState::Finished) => {
                return self.prepared_in(tx, op, receipt, &detail);
            }
            ("tidy_workspace", ReceiptState::Finished) => {
                self.tidied_in(tx, op, success, &detail)?;
            }
            ("release_session", ReceiptState::Finished) => {
                let s = command_session_in(&self.store, op)?;
                self.released_in(tx, &s, op, success)?;
            }
            ("open_session", ReceiptState::Finished) => {
                let s = command_session_in(&self.store, op)?;
                self.config_opened_in(tx, &s, op, success)?;
                return self.opened_in(tx, &s, success, &detail);
            }
            ("abandon", ReceiptState::Finished) if !success => {
                let error = match &receipt.state {
                    Observed::Finished { error, .. } => error.clone(),
                    _ => None,
                };
                return self.abandon_failed_in(tx, op, error.as_ref());
            }
            ("start_turn", ReceiptState::Accepted | ReceiptState::Active) => {
                // a cancel held back while the start_turn's send was uncertain goes now
                if let Some((attempt, _)) = cancelled_start_in(tx, op)? {
                    if self
                        .attempt_row(&attempt)?
                        .ok_or_else(|| conflict("missing cancelled attempt"))?
                        .state
                        != "unknown"
                    {
                        self.stop_command_in(tx, &attempt, op, "cancel")?;
                    }
                }
            }
            ("start_turn", ReceiptState::Unknown) => {
                // the Node cannot tell what the attempt did: it keeps its session until it is
                // settled, and is never sent again
                if let Some(a) = self
                    .store
                    .read(|r| node_turns::started_by(r, op))?
                    .filter(|a| a.state != "finished")
                {
                    let (attempt, rev, s) = (a.id, a.revision, a.session);
                    node_write::attempt_cas_in(tx, &attempt, rev, "unknown", None, None, now())?;
                    system_in(
                        tx,
                        &s,
                        &format!(
                            "Node 无法确定这次执行的结果（{detail}），等待对账。点停止可请求放弃。"
                        ),
                    )?;
                }
            }
            ("open_session" | "release_session", ReceiptState::Unknown) => {
                // never sent again: the session waits (busy) until it is reconciled
                let s = command_session_in(&self.store, op)?;
                system_in(
                    tx,
                    &s,
                    &steve_i18n::t!(
                        "hub.node.control_unknown",
                        what = recover::what(&method),
                        detail = detail
                    ),
                )?;
            }
            ("prepare_workspace" | "abandon", ReceiptState::Unknown) => {
                // never sent again: what waits on it waits until it is retired
                if let Some(s) = command_any_session_in(&self.store, op)? {
                    system_in(
                        tx,
                        &s,
                        &steve_i18n::t!(
                            "hub.node.control_unknown",
                            what = if method == "abandon" {
                                steve_i18n::stored(|| steve_i18n::t!("hub.node.what.abandon_run"))
                            } else {
                                recover::what(&method)
                            },
                            detail = detail
                        ),
                    )?;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// A release is over: the session is let go, or (refused, failed) still open as it was. What
    /// came in meanwhile goes now (opening it again if it was let go). Only the session's last
    /// release says so: an earlier one's receipt, come late, says nothing of the session now.
    fn released_in(
        &self,
        tx: &mut W<'_>,
        s: &str,
        op: &str,
        success: bool,
    ) -> Result<(), StoreError> {
        if self.phase_in(s)?.is_none_or(|(p, _)| p != "releasing")
            || self
                .store
                .read(|r| dispatch::release(r, s, false))?
                .as_deref()
                != Some(op)
        {
            return Ok(());
        }
        let rev = self.session_revision(s)?;
        node_write::session_cas_in(
            tx,
            s,
            rev,
            &rt::SessionUpdate {
                phase: if success { "released" } else { "ready" },
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        if success {
            self.plan_after_in(tx, s)?;
        }
        Ok(())
    }

    /// An open is over. Opened: a turn that waited for it (its attempt was refused because the
    /// Node had let the session go) goes as a new attempt; otherwise what is queued goes. Not
    /// opened: such a turn ends as failed, and the session stays let go with a durable startup
    /// stop. Only an explicit human retry clears it; queued input never opens it again.
    pub(super) fn opened_in(
        &self,
        tx: &mut W<'_>,
        s: &str,
        ok: bool,
        detail: &str,
    ) -> Result<Option<End>, StoreError> {
        if self.phase_in(s)?.is_none_or(|(p, _)| p != "opening") {
            return Ok(None);
        }
        let waiting = self.store.read(|r| node_turns::waiting_to_open(r, s))?;
        if ok {
            if let Some(turn) = waiting {
                if let Some(why) = self.workflow_blocker_in(s)? {
                    return self.after_attempt_in(
                        tx,
                        s,
                        &turn,
                        &Completion {
                            outcome: Outcome::Interrupted,
                            stop_reason: "cancelled".into(),
                            error: Some(Error::new(Code::Invalid, why)),
                            result: vec![],
                            continuation: None,
                            transient: false,
                            acted: false,
                            job: None,
                        },
                    );
                }
                let (spec, ordinal) = self.store.read(|r| node_turns::latest_spec(r, &turn))?;
                self.attempt_in(tx, s, &turn, ordinal + 1, spec.run.prompt)?;
                return Ok(None);
            }
            let rev = self.session_revision(s)?;
            node_write::session_cas_in(
                tx,
                s,
                rev,
                &rt::SessionUpdate {
                    phase: "ready",
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )?;
            // an item paused while its session was opening (its workspace being prepared, say)
            // starts nothing: what is queued waits for it to continue
            let planned = if self
                .store
                .read(|r| node_sessions::worktree::item_paused(r, s))?
            {
                Planned::Empty
            } else {
                self.plan_after_in(tx, s)?
            };
            if planned != Planned::Created {
                self.release_if_ended_in(tx, s)?;
            }
            return Ok(None);
        }
        // Failed opens never spend the same queued startup message again automatically.
        // The durable stop is independent of the card's UI status and survives Hub restarts.
        let text =
            steve_i18n::stored(|| steve_i18n::t!("hub.node.stop.reopen_failed", detail = detail));
        let mut end = Some(End {
            session: s.to_string(),
            attempt: None,
            stop: steve_store::SESSION_START_FAILURE.to_string(),
            error: Some(text.clone()),
        });
        if let Some(turn) = waiting {
            let c = Completion {
                outcome: Outcome::Failed,
                stop_reason: "error".into(),
                error: Some(Error::new(
                    Code::Invalid,
                    format!("the Node could not open the session again: {detail}"),
                )),
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            };
            self.turn_over_in(tx, s, &turn, &c)?;
            end = Some(End {
                session: s.to_string(),
                attempt: Some(self.last_attempt_in(&turn)?),
                stop: steve_store::SESSION_START_FAILURE.to_string(),
                error: Some(text),
            });
        }
        let rev = self.session_revision(s)?;
        node_write::session_cas_in(
            tx,
            s,
            rev,
            &rt::SessionUpdate {
                phase: "released",
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        system_in(
            tx,
            s,
            &format!(
                "Node 没能打开这个会话（{detail}），已停止自动重试，请修正 Bot 配置后从失败卡重试。"
            ),
        )?;
        self.release_if_ended_in(tx, s)?;
        if self.ended_in(s)? {
            self.store.write(|w| node_sessions::clear_queue(w, s))?;
            return Ok(None);
        }
        self.status_in(s, "error")?;
        self.startup_failed_in(s, detail)?;
        Ok(end)
    }

    pub(super) fn startup_failed_in(&self, session: &str, detail: &str) -> Result<(), StoreError> {
        use node_sessions::startup;
        let reason = steve_store::SESSION_START_FAILURE;
        let question = format!("会话启动失败：{detail}");
        self.store.write(|w| {
            let scope = startup::scope(w, session)?;
            for d in startup::open_decisions(w, session, scope.item.as_deref())? {
                let obsolete = (d.subject_type == reason
                    && d.session_id.as_deref() == Some(session)
                    && d.question != question)
                    || (steve_core::lifecycle::stop_card(&d)
                        && d.session_id.as_deref().is_none_or(|id| id == session));
                if obsolete {
                    steve_store::DecisionRows(w).settle(
                        &d.id,
                        &json!({"superseded":"已保留当前会话启动失败的原因和重试入口"}),
                        "expired",
                        &Value::Null,
                    )?;
                }
            }
            startup::record_stop(w, session, detail, now())?;
            if !startup::reopen_matching(w, session, &scope, &question)? {
                steve_store::DecisionRows(w).open(&steve_store::NewDecision {
                    workspace_id: scope.workspace.clone(), kind: "fail".into(), subject_type: reason.into(),
                    subject_id: scope.bot.unwrap_or_default(), item_id: scope.item.clone(), session_id: Some(session.into()),
                    question: question.clone(), draft: Some("自动重试已停止。请先修正 Bot 的模型或思考强度，再点击重试；你修改执行器后，重试会创建新会话。".into()),
                    options: Some(vec!["retry".into()]), requested_by: "steve".into(),
                })?;
            }
            if let Some(item) = scope.item {
                startup::hold_item(w, &scope.workspace, &item, &question, now())?;
            }
            Ok(())
        })
    }

    /// A refused abandon leaves the target uncertain until a trustworthy receipt arrives.
    fn abandon_failed_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        e: Option<&Error>,
    ) -> Result<Option<End>, StoreError> {
        let Some(a) = self.store.read(|r| node_turns::targeted_by(r, op))? else {
            return Ok(None);
        };
        let s = a.session;
        let detail = e.map(|e| e.safe_detail.clone()).unwrap_or_default();
        system_in(
            tx,
            &s,
            &format!("放弃这次执行没有成功（{detail}），会话仍在等待对账；可以再点一次停止。"),
        )?;
        Ok(None)
    }

    /// The Node refused an attempt before running it. Refused because the Node had let the
    /// session go: nothing ran, the session opens again and the same turn goes as a new attempt
    /// (once). Otherwise the attempt ends as failed, its turn with it.
    fn refused_attempt_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        e: &Error,
    ) -> Result<Option<End>, StoreError> {
        let Some(a) = self.store.read(|r| node_turns::started_by(r, op))? else {
            return Ok(None);
        };
        let gone = e
            .reason
            .as_ref()
            .is_some_and(|r| r.is(steve_types::devices::NodeReason::SessionNotOpen));
        let (attempt, rev, turn, session, cancelled) =
            (a.id, a.revision, a.turn, a.session, a.cancelled);
        let reopened_before = self.store.read(|r| node_turns::was_released(r, &turn))?;
        let reopen =
            gone && !reopened_before && !cancelled && self.workflow_blocker_in(&session)?.is_none();
        let completion = Completion {
            outcome: Outcome::Failed,
            stop_reason: if reopen { "released" } else { "error" }.into(),
            error: Some(e.clone()),
            result: vec![],
            continuation: None,
            transient: false,
            acted: false,
            job: None,
        };
        node_write::attempt_cas_in(
            tx,
            &attempt,
            rev,
            "finished",
            None,
            Some(&serde_json::to_value(&completion).map_err(conflict)?),
            now(),
        )?;
        self.close_cards_of_attempt_in(tx, &attempt)?;
        if reopen {
            // the turn keeps its messages and waits for the open (see opened_in)
            let trev = self.turn_revision(&turn)?;
            let ordinal = self.store.read(|r| node_turns::retry_ordinal(r, &turn))?;
            node_write::turn_cas_in(
                tx,
                &turn,
                trev,
                &rt::TurnUpdate {
                    state: "running",
                    active_attempt: None,
                    retry_ordinal: ordinal,
                    applied: false,
                },
                now(),
            )?;
            self.reopen_in(tx, &session)?;
            return Ok(None);
        }
        self.after_attempt_in(tx, &session, &turn, &completion)
    }
}

pub(super) fn receipt_state(s: &Observed) -> (ReceiptState, bool) {
    match s {
        Observed::Accepted => (ReceiptState::Accepted, false),
        Observed::Running | Observed::Waiting { .. } => (ReceiptState::Active, false),
        Observed::Unknown { .. } => (ReceiptState::Unknown, false),
        Observed::Finished { outcome, .. } => {
            (ReceiptState::Finished, *outcome == Outcome::Succeeded)
        }
    }
}

/// Commands can wait to be issued until the first lease. Save the acceptance window before
/// leaving the transaction so later sends and restart reconciliation see exactly these bytes.
fn freeze_first_envelope_in(
    store: &Store,
    mut l: rt::CommandLease,
    at: i64,
) -> Result<rt::CommandLease, StoreError> {
    let c: Command<Value> = serde_json::from_value(l.request.clone()).map_err(conflict)?;
    if c.accept_before.0 - at >= ACCEPT_WINDOW_MS / 2 {
        return Ok(l);
    }
    l.request["issued_at"] = Value::from(at);
    l.request["accept_before"] = Value::from(at + ACCEPT_WINDOW_MS);
    let digest = digest_of(&l.request).map_err(conflict)?.to_string();
    store.write(|w| dispatch::freeze_envelope(w, &l.operation, &l.request, &digest, at))?;
    l.digest = digest;
    Ok(l)
}

/// The session an open or a release command is for.
fn command_session_in(store: &Store, op: &str) -> Result<String, StoreError> {
    store.read(|r| dispatch::session(r, op))
}

/// The delay before the `n`th send after as many Exhausted refusals in a row.
pub(super) fn exhausted_delay(n: i64) -> i64 {
    (2_000i64 << (n - 1).clamp(0, 5)).min(60_000)
}

/// The session any command is for: its own, its target attempt's, or (a workspace) the open
/// waiting on it. `None`: none of them.
pub(super) fn command_any_session_in(
    store: &Store,
    op: &str,
) -> Result<Option<String>, StoreError> {
    store.read(|r| dispatch::any_session(r, op))
}

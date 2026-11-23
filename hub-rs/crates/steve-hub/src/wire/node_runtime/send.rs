//! T2, sending: command leasing, Node RPC dispatch, receipt merging, retry backoff and
//! refusal/outcome transitions.

use super::*;

/// A worker's hold on a command it sends.
const LEASE_MS: i64 = 60_000;

impl NodeRuntime {
    // ── T2: sending ──

    /// A transaction for the send loop, which is an async task. A write waiting for SQLite's
    /// writer (another process holding it, a long transaction) must not hold an async worker
    /// meanwhile, or the runtime's timers and I/O wait with it, and so may the very writer it waits
    /// for. On a multi-thread runtime the worker hands its other tasks on while this blocks; the
    /// transaction still runs here and now, so nothing else happens between it and the send.
    fn send_tx<T>(&self, f: impl FnOnce(&Tx) -> Result<T, StoreError>) -> Result<T, StoreError> {
        match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(tokio::runtime::RuntimeFlavor::MultiThread) => {
                tokio::task::block_in_place(|| self.tx(f))
            }
            _ => self.tx(f),
        }
    }

    /// Poll no later than one second, shortened to a known future retry deadline. The shared
    /// Clock sleep is a full elapsed wait; it does not promise an early wake on wall changes.
    pub(super) fn sender_delay(&self) -> std::time::Duration {
        let now = self.clock.now();
        let next=self.store.reader::<_,StoreError>(|c| Ok(c.query_row(
            "SELECT min(retry_at) FROM node_commands WHERE node_id=?1 AND state='pending' AND retry_at>?2",
            rusqlite::params![self.node_id.as_str(),now],|r|r.get::<_,Option<i64>>(0)
        )?)).unwrap_or(None);
        let ms = next.map_or(1_000, |at| {
            at.saturating_sub(self.clock.now()).clamp(0, 1_000)
        });
        std::time::Duration::from_millis(ms as u64)
    }

    /// Leases one command, sends it and merges what came back. False: nothing to send.
    pub(super) async fn send_one(&self) -> bool {
        let lease = match self.send_tx(|tx| {
            self.expire_capacity_waits_in(tx)?;
            self.note_waiting_in(tx)?;
            match rt::lease_command_in(
                tx,
                self.node_id.as_str(),
                &self.owner,
                self.clock.now(),
                self.clock.now() + LEASE_MS,
            )? {
                Some(l) if !l.maybe_sent => fresh_envelope_in(tx, l, self.clock.now()).map(Some),
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
        let sent = self.send(&lease).await;
        self.meters.command(
            &lease.method,
            sent.as_ref().map(|_| ()).map_err(|e| e.code),
            sending.elapsed().as_secs_f64(),
        );
        // a turn end to report, and a session whose configure was refused for a stale revision
        let r = self.send_tx(|tx| match &sent {
            Ok(receipt) => self.receipt_in(tx, receipt).map(|end| (end, None)),
            Err(e) if e.code == ErrorCode::OutcomeUnknown && e.retry == RetryClass::Never => {
                // Leasing already persisted maybe_sent=1. Do not resend or call this a refusal:
                // the Node may have executed it. A later receipt/reconcile can settle unknown.
                self.receipt_in(tx, &Receipt {
                    operation: idv(lease.operation.clone())?,
                    state: Observed::Unknown { reason: said(e) },
                }).map(|end| (end, None))
            }
            Err(e)
                if e.retry == RetryClass::SameOperation || e.code == ErrorCode::OutcomeUnknown =>
            {
                // not refused for good, or maybe accepted: the very same command again later
                let uncertain = e.retry != RetryClass::SameOperation;
                let delay = if e.code == ErrorCode::Exhausted {
                    self.exhausted_in(tx, &lease, e)?
                } else {
                    1_000
                };
                rt::retry_command_in(tx, &lease, self.clock.now(), self.clock.now() + delay, uncertain)?;
                self.meters.retry(&lease.method);
                if !uncertain && !lease.maybe_sent {
                    tx.execute(
                        "UPDATE node_commands SET receipt=json_set(COALESCE(receipt,'{}'),'$.last_error',json(?)) WHERE operation_id=? AND state='pending'",
                        rusqlite::params![serde_json::to_string(e).map_err(conflict)?, lease.operation],
                    )?;
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
                    && let Some(attempt) = node_jobs::job_attempt_of(tx, &lease.operation)?
                    && steve_store::jobs::attempt_in(tx, &attempt)?
                        .is_some_and(|a| a.cancel_requested)
                {
                    self.stop_job_in(tx, &attempt)?;
                }
                Ok((None, None))
            }
            Err(e) => {
                rt::merge_receipt_in(
                    tx,
                    &lease.operation,
                    ReceiptState::Rejected,
                    false,
                    &serde_json::to_value(e).map_err(conflict)?,
                    now(),
                )?;
                // refused before it was accepted: it certainly did not run
                if let Some(attempt) = node_jobs::job_attempt_of(tx, &lease.operation)? {
                    self.job_refused_in(tx, &lease.method, &attempt, e)?;
                    return Ok((None, None));
                }
                let end = match lease.method.as_str() {
                    "start_turn" => self.refused_attempt_in(tx, &lease.operation, e)?,
                    "release_session" => {
                        let s = command_session_in(tx, &lease.operation)?;
                        self.released_in(tx, &s, &lease.operation, false)?;
                        None
                    }
                    "open_session" => {
                        let s = command_session_in(tx, &lease.operation)?;
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
    fn note_waiting_in(&self, tx: &Tx) -> Result<(), StoreError> {
        let room = rt::session_room(tx, self.node_id.as_str())?;
        if !room.full() || room.waiting == 0 {
            return Ok(());
        }
        let fresh = rt::waiting_commands(tx, self.node_id.as_str())?;
        for (op, session) in fresh {
            tx.execute(
                "UPDATE node_commands SET receipt=json_set(COALESCE(receipt,'{}'),'$.waiting',1) WHERE operation_id=?",
                [&op],
            )?;
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
        tx: &Tx,
        lease: &rt::CommandLease,
        e: &ApiError,
    ) -> Result<i64, StoreError> {
        let before: Option<i64> = tx
            .query_row(
                "SELECT json_extract(receipt,'$.exhausted') FROM node_commands WHERE operation_id=? AND state='pending'",
                [&lease.operation],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let n = before.unwrap_or(0) + 1;
        tx.execute(
            "UPDATE node_commands SET receipt=json_set(COALESCE(receipt,'{}'),'$.success',json('false'),'$.payload',json(?),'$.exhausted',?) WHERE operation_id=? AND state='pending'",
            rusqlite::params![serde_json::to_string(e).map_err(conflict)?, n, lease.operation],
        )?;
        if n == 1
            && let Some(session) = command_any_session_in(tx, &lease.operation)?
        {
            system_in(
                tx,
                &session,
                &steve_i18n::t!("hub.node.waiting_for_node", detail = e.safe_detail.as_str()),
            )?;
        }
        Ok(exhausted_delay(n))
    }

    async fn send(&self, l: &rt::CommandLease) -> ApiResult<Receipt> {
        fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> ApiResult<Command<T>> {
            serde_json::from_value(v.clone())
                .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("stored command: {e}")))
        }
        let n = &self.node;
        match l.method.as_str() {
            "prepare_workspace" => n.prepare_workspace(parse(&l.request)?).await,
            "open_session" => {
                let cmd: Command<OpenSession> = parse(&l.request)?;
                // Only before first acceptance: an uncertain command keeps its identity and is
                // reconciled, never manufactured into a rejection while it may already run.
                if !l.maybe_sent {
                    let ended = self
                        .store
                        .sessions()
                        .get(cmd.body.session.as_str())
                        .map_err(|e| ApiError::new(ErrorCode::Invalid, e.to_string()))?
                        .is_none_or(|s| s.status == "ended");
                    if ended {
                        return Err(ApiError::new(ErrorCode::Invalid, "已结束的会话不能重开。"));
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
                        .map_err(|why| ApiError::new(ErrorCode::Invalid, why))?;
                    }
                }
                n.open_session(cmd).await
            }
            "start_turn" => {
                let blocked = if self.domain.dynamic_workflows() {
                    None
                } else {
                    self.store.reader(|c| {
                        let session: String = c.query_row(
                            "SELECT t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=?",
                            [&l.operation], |r| r.get(0),
                        )?;
                        self.disabled_workflow_in(c, &session)
                    }).map_err(|e| ApiError::new(ErrorCode::Offline, e.to_string()))?
                };
                if let Some(why) = blocked {
                    // A prior uncertain send may already be running: only reconciliation can
                    // settle it, never manufacture a pre-admission rejection for it.
                    return Err(ApiError::new(
                        if l.maybe_sent {
                            ErrorCode::OutcomeUnknown
                        } else {
                            ErrorCode::Invalid
                        },
                        why,
                    ));
                }
                let limit = self
                    .store
                    .reader(|c| rt::session_limit(c, self.node_id.as_str()))
                    .map_err(|e| {
                        ApiError::new(ErrorCode::Offline, format!("session capacity: {e}"))
                    })?;
                // Reapply before each start, not once per Hub boot: a separately running Node may
                // have restarted/reconnected meanwhile. This is resource scheduling, not policy.
                n.set_session_capacity(limit as u32).await?;
                n.start_turn(parse(&l.request)?).await
            }
            "start_job" => n.start_job(parse(&l.request)?).await,
            "cancel" => n.cancel(parse(&l.request)?).await,
            "release_session" => n.release_session(parse(&l.request)?).await,
            "configure_session" => n.configure_session(parse(&l.request)?).await,
            "resolve_permission" => n.resolve_permission(parse(&l.request)?).await,
            "abandon" => n.abandon(parse(&l.request)?).await,
            "recover" => n.recover(parse(&l.request)?).await,
            m if ledger::LEDGER_METHODS.contains(&m) => self.send_ledger(l).await,
            other => Err(ApiError::new(
                ErrorCode::Unsupported,
                format!("the coordinator does not send {other}"),
            )),
        }
    }

    /// What the Node says about a command, from its answer or its stream (the same either way):
    /// merged forward, and an open that finished makes the session ready (or released).
    pub(super) fn receipt_in(&self, tx: &Tx, receipt: &Receipt) -> Result<Option<End>, StoreError> {
        let (state, success) = receipt_state(&receipt.state);
        let op = receipt.operation.as_str();
        let Some(method) = tx
            .query_row(
                "SELECT method FROM node_commands WHERE operation_id=? AND node_id=?",
                [op, self.node_id.as_str()],
                |r| r.get::<_, String>(0),
            )
            .ok()
        else {
            return Ok(None);
        };
        let moved = rt::merge_receipt_in(
            tx,
            op,
            state,
            success,
            &serde_json::to_value(receipt).map_err(conflict)?,
            now(),
        )?;
        if !moved {
            return Ok(None);
        }
        if let Some(attempt) = node_jobs::job_attempt_of(tx, op)? {
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
        if self.retiring_in(tx, op, interrupted)? {
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
                let s = command_session_in(tx, op)?;
                self.released_in(tx, &s, op, success)?;
            }
            ("open_session", ReceiptState::Finished) => {
                let s = command_session_in(tx, op)?;
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
                    let state: String = tx.query_row(
                        "SELECT state FROM runtime_attempts WHERE attempt_id=?",
                        [&attempt],
                        |r| r.get(0),
                    )?;
                    if state != "unknown" {
                        self.stop_command_in(tx, &attempt, op, "cancel")?;
                    }
                }
            }
            ("start_turn", ReceiptState::Unknown) => {
                // the Node cannot tell what the attempt did: it keeps its session until it is
                // settled, and is never sent again
                match tx.query_row(
                    "SELECT a.attempt_id, a.revision, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=? AND a.state<>'finished'",
                    [op],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)),
                ) {
                    Ok((attempt, rev, s)) => {
                        rt::attempt_cas_in(tx, &attempt, rev, "unknown", None, None, now())?;
                        if detail == steve_node_api::RESTARTED {
                            // the Node restarted under it (#54): its harness is gone with the old
                            // process. Given up here, as Stop would; once the Node confirms it,
                            // the agent is continued (NODE_RESTARTED), never the turn resent
                            self.stop_command_as_in(tx, &attempt, op, "abandon", RESTART_ABANDON)?;
                            system_in(
                                tx,
                                &s,
                                &steve_i18n::stored(|| steve_i18n::t!("hub.node.restart_abandon")),
                            )?;
                        } else {
                            system_in(
                                tx,
                                &s,
                                &format!(
                                    "Node 无法确定这次执行的结果（{detail}），等待对账，不会自动重跑。点停止可放弃这次执行。"
                                ),
                            )?;
                        }
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            ("open_session" | "release_session", ReceiptState::Unknown) => {
                // never sent again: the session waits (busy) until it is reconciled
                let s = command_session_in(tx, op)?;
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
                if let Some(s) = command_any_session_in(tx, op)? {
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
    fn released_in(&self, tx: &Tx, s: &str, op: &str, success: bool) -> Result<(), StoreError> {
        if phase_in(tx, s)?.is_none_or(|(p, _)| p != "releasing")
            || last_release_in(tx, s)?.as_deref() != Some(op)
        {
            return Ok(());
        }
        let rev = session_revision(tx, s)?;
        rt::session_cas_in(
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
        if self.plan_after_in(tx, s)? != Planned::Created && !success {
            self.release_again_in(tx, s, op)?;
        }
        Ok(())
    }

    /// A release of an ended session that a successor waits for came back refused or failed:
    /// the old harness is still open in the shared worktree, so the successor keeps waiting, now
    /// for a new release (after a delay doubling from 1 s up to a minute) that it is moved onto.
    /// The successor is told once. Nothing waits for it: it stays as it is (tidy asks again).
    fn release_again_in(&self, tx: &Tx, s: &str, failed: &str) -> Result<(), StoreError> {
        let waiting: Vec<String> = tx
            .prepare(
                "SELECT operation_id FROM node_commands WHERE dependency_operation=? AND state='pending'",
            )?
            .query_map([failed], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if waiting.is_empty() || !ended_in(tx, s)? {
            return Ok(());
        }
        let Release::Pending(op) = self.release_in(tx, s)? else {
            return Ok(());
        };
        let failures: i64 = tx.query_row(
            "SELECT COUNT(*) FROM node_commands WHERE method='release_session' AND session_id=? AND state IN ('finished','rejected') AND json_extract(receipt,'$.success')=0",
            [s],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE node_commands SET retry_at=?, updated=? WHERE operation_id=? AND state='pending'",
            rusqlite::params![
                self.clock.now() + (1_000i64 << (failures - 1).clamp(0, 6)).min(60_000),
                self.clock.now(),
                op
            ],
        )?;
        for w in &waiting {
            tx.execute(
                "UPDATE node_commands SET dependency_operation=?, updated=? WHERE operation_id=? AND state='pending'",
                rusqlite::params![op, now(), w],
            )?;
            rt::command_changed_in(tx, &w)?;
            if failures == 1
                && let Some(next) = command_any_session_in(tx, w)?
            {
                system_in(
                    tx,
                    &next,
                    &steve_i18n::stored(|| steve_i18n::t!("hub.node.release_retry")),
                )?;
            }
        }
        Ok(())
    }

    /// An open is over. Opened: a turn that waited for it (its attempt was refused because the
    /// Node had let the session go) goes as a new attempt; otherwise what is queued goes. Not
    /// opened: such a turn ends as failed, and the session stays let go with a durable startup
    /// stop. Only an explicit human retry clears it; queued input never opens it again.
    pub(super) fn opened_in(
        &self,
        tx: &Tx,
        s: &str,
        ok: bool,
        detail: &str,
    ) -> Result<Option<End>, StoreError> {
        if phase_in(tx, s)?.is_none_or(|(p, _)| p != "opening") {
            return Ok(None);
        }
        let waiting: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM runtime_turns WHERE session_id=? AND state<>'finished' AND active_attempt IS NULL ORDER BY created LIMIT 1",
                [s],
                |r| r.get(0),
            )
            .optional()?;
        if ok {
            if let Some(turn) = waiting {
                if let Some(why) = self.workflow_blocker_in(tx, s)? {
                    return self.after_attempt_in(
                        tx,
                        s,
                        &turn,
                        &Completion {
                            outcome: Outcome::Interrupted,
                            stop_reason: "cancelled".into(),
                            error: Some(ApiError::new(ErrorCode::Invalid, why)),
                            result: vec![],
                            continuation: None,
                            transient: false,
                            acted: false,
                            job: None,
                        },
                    );
                }
                let (spec, ordinal): (String, i64) = tx.query_row(
                    "SELECT spec, ordinal FROM runtime_attempts WHERE turn_id=? ORDER BY ordinal DESC LIMIT 1",
                    [&turn],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let spec: TurnSpec = serde_json::from_str(&spec)?;
                self.attempt_in(tx, s, &turn, ordinal + 1, spec.run.prompt)?;
                return Ok(None);
            }
            let rev = session_revision(tx, s)?;
            rt::session_cas_in(
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
            let planned = if workspace::item_paused_in(tx, s)? {
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
                error: Some(ApiError::new(
                    ErrorCode::Invalid,
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
                attempt: Some(last_attempt_in(tx, &turn)?),
                stop: steve_store::SESSION_START_FAILURE.to_string(),
                error: Some(text),
            });
        }
        let rev = session_revision(tx, s)?;
        rt::session_cas_in(
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
        if ended_in(tx, s)? {
            steve_store::discard_queue_in(tx, s)?;
            return Ok(None);
        }
        status_in(tx, s, "error")?;
        self.startup_failed_in(tx, s, detail)?;
        Ok(end)
    }

    pub(super) fn startup_failed_in(
        &self,
        tx: &Tx,
        session: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        let reason = steve_store::SESSION_START_FAILURE;
        let (ws, item, bot): (String, Option<String>, Option<String>) = tx.query_row(
            "SELECT workspace_id,item_id,bot_id FROM sessions WHERE id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let question = format!("会话启动失败：{detail}");
        // A changed cause replaces the previous startup blocker, and so does a generic stop card
        // of this session; unknown-effect/release cards remain untouched.
        let previous = tx
            .prepare(
                "SELECT id FROM decisions WHERE status='open' AND (session_id=?1 OR item_id=?2)",
            )?
            .query_map(rusqlite::params![session, item], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in previous {
            let Some(d) = self.store.decisions().get(&id)? else {
                continue;
            };
            let obsolete = (d.subject_type == reason
                && d.session_id.as_deref() == Some(session)
                && d.question != question)
                || (steve_core::lifecycle::stop_card(&d)
                    && d.session_id.as_deref().is_none_or(|id| id == session));
            if obsolete {
                steve_store::decision_resolve_in(
                    tx,
                    &d.id,
                    &json!({"superseded":"已保留当前会话启动失败的原因和重试入口"}),
                    "expired",
                )?;
            }
        }
        tx.execute("INSERT INTO session_stops(session_id,stop_reason,error,ended,updated) VALUES (?1,?2,?3,?4,?4) ON CONFLICT(session_id) DO UPDATE SET stop_reason=?2,error=?3,ended=?4,updated=?4", rusqlite::params![session, reason, detail, now()])?;
        let existing: Option<String> = tx.query_row("SELECT id FROM decisions WHERE session_id=? AND subject_type=? AND question=? ORDER BY created LIMIT 1", [session, reason, &question], |r| r.get(0)).optional()?;
        if let Some(id) = existing {
            tx.execute("UPDATE decisions SET status='open',result=NULL,resolved=NULL,undo=NULL WHERE id=? AND status<>'open'", [&id])?;
            tx.publish(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Decision {
                    ws: ws.clone().into(),
                    id: {
                        let value: String = id;
                        steve_types::DecisionId::from(value)
                    },
                    item_id: {
                        let value: Option<Option<String>> = Some(item.clone());
                        value.map(|value| value.map(Into::into))
                    },
                },
            ));
        } else {
            steve_store::decision_create_in(tx, &steve_store::NewDecision {
                workspace_id: ws.clone(), kind: "fail".into(), subject_type: reason.into(),
                subject_id: bot.unwrap_or_default(), item_id: item.clone(), session_id: Some(session.into()),
                question: question.clone(), draft: Some("自动重试已停止。请先修正 Bot 的模型或思考强度，再点击重试；你修改执行器后，重试会创建新会话。".into()),
                options: Some(vec!["retry".into()]), requested_by: "steve".into(),
            })?;
        }
        if let Some(item) = item {
            tx.execute("UPDATE items SET status='wait',now=?,updated=? WHERE id=? AND phase<>'closed' AND status<>'pause'", rusqlite::params![question,now(),item])?;
            tx.publish(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Item {
                    ws: ws.clone().into(),
                    id: {
                        let value: String = item;
                        steve_types::ItemId::from(value)
                    },
                },
            ));
        }
        Ok(())
    }

    /// An abandon the Node refused or could not finish: the attempt stays unknown, its session
    /// held; the user may give it up again (a new abandon). Refused because the Node has no such
    /// operation: the Node holds no record of the attempt's start_turn, so it never ran there (the
    /// reconciliation that made it unknown saw the same); it ends as interrupted, never sent again.
    fn abandon_failed_in(
        &self,
        tx: &Tx,
        op: &str,
        e: Option<&ApiError>,
    ) -> Result<Option<End>, StoreError> {
        let row: Option<(String, String, String)> = tx
            .query_row(
                "SELECT t.session_id, a.attempt_id, a.state FROM node_commands c JOIN runtime_attempts a ON a.attempt_id=c.target_attempt JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE c.operation_id=?",
                [op],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((s, attempt, state)) = row else {
            return Ok(None);
        };
        let detail = e.map(|e| e.safe_detail.clone()).unwrap_or_default();
        let unknown_to_node = e.is_some_and(|e| {
            e.code == ErrorCode::Invalid && e.safe_detail.contains("no such operation")
        });
        if unknown_to_node && state == "unknown" {
            let completion = Completion {
                outcome: Outcome::Interrupted,
                stop_reason: "error".into(),
                error: Some(ApiError::new(
                    ErrorCode::Invalid,
                    "the Node holds no record of this attempt: it did not run there",
                )),
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            };
            self.settle_in(tx, &s, &attempt, &completion)?;
            system_in(
                tx,
                &s,
                "Node 没有这次执行的记录（它没有在 Node 上运行），已放弃，不会自动重跑。",
            )?;
            return Ok(None);
        }
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
        tx: &Tx,
        op: &str,
        e: &ApiError,
    ) -> Result<Option<End>, StoreError> {
        let row: Option<(String, i64, String, String)> = tx
            .query_row(
                "SELECT a.attempt_id, a.revision, a.turn_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=?",
                [op],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((attempt, rev, turn, session)) = row else {
            return Ok(None);
        };
        let gone = e.code == ErrorCode::Invalid
            && (e.safe_detail.contains("open it again") || e.safe_detail.contains("not open"));
        let reopened_before: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_attempts WHERE turn_id=? AND json_extract(completion,'$.stop_reason')='released')",
            [&turn],
            |r| r.get(0),
        )?;
        let cancelled: bool = tx.query_row(
            "SELECT cancel_requested FROM runtime_attempts WHERE attempt_id=?",
            [&attempt],
            |r| r.get(0),
        )?;
        let reopen = gone
            && !reopened_before
            && !cancelled
            && self.workflow_blocker_in(tx, &session)?.is_none();
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
        rt::attempt_cas_in(
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
            let trev = turn_revision(tx, &turn)?;
            let ordinal: i64 = tx.query_row(
                "SELECT retry_ordinal FROM runtime_turns WHERE turn_id=?",
                [&turn],
                |r| r.get(0),
            )?;
            rt::turn_cas_in(
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

/// A command that never went out and waited long (behind its dependency, say: a successor's
/// prepare behind its predecessor's turn) is issued again now, so the Node does not refuse it as
/// past `accept_before`. Only one no send can have reached: the Node knows nothing of it yet.
fn fresh_envelope_in(
    tx: &Tx,
    mut l: rt::CommandLease,
    at: i64,
) -> Result<rt::CommandLease, StoreError> {
    let mut c: Command<Value> = serde_json::from_value(l.request.clone()).map_err(conflict)?;
    if c.accept_before.0 - at >= ACCEPT_WINDOW_MS / 2 {
        return Ok(l);
    }
    c.issued_at = Timestamp(at);
    c.accept_before = Timestamp(at + ACCEPT_WINDOW_MS);
    let v = serde_json::to_value(&c).map_err(conflict)?;
    let digest = digest_of(&v).map_err(conflict)?.to_string();
    tx.execute(
        "UPDATE node_commands SET request=?, digest=?, updated=? WHERE operation_id=? AND state='pending'",
        rusqlite::params![
            serde_json::to_string(&v).map_err(conflict)?,
            digest,
            at,
            l.operation
        ],
    )?;
    l.request = v;
    l.digest = digest;
    Ok(l)
}

/// The session an open or a release command is for.
fn command_session_in(tx: &Tx, op: &str) -> Result<String, StoreError> {
    Ok(tx.query_row(
        "SELECT session_id FROM node_commands WHERE operation_id=?",
        [op],
        |r| r.get(0),
    )?)
}

/// The delay before the `n`th send after as many Exhausted refusals in a row.
pub(super) fn exhausted_delay(n: i64) -> i64 {
    (2_000i64 << (n - 1).clamp(0, 5)).min(60_000)
}

/// The session any command is for: its own, its target attempt's, or (a workspace) the open
/// waiting on it. `None`: none of them.
pub(super) fn command_any_session_in(tx: &Tx, op: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT COALESCE(
                c.session_id,
                (SELECT t.session_id FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id
                  WHERE a.attempt_id=c.target_attempt),
                (SELECT d.session_id FROM node_commands d
                  WHERE d.dependency_operation=c.operation_id ORDER BY d.created, d.rowid LIMIT 1))
             FROM node_commands c WHERE c.operation_id=?",
            [op],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

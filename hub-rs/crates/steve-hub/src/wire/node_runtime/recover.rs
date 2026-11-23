//! Explicit recover of a control operation whose outcome is unknown (D7). Nothing retires one on
//! its own: the user's Stop ([`NodeRuntime::user_stop`], never the lifecycle's cancels) on a session
//! held by one plans exactly one `recover`, its target and the target's
//! expected digest fixed when it is planned. The target is never sent again. A recover proves only
//! that the control intent is retired: its effect on the Node (a worktree, an answer) stays
//! unknown, and what the session hears never says it succeeded.
//!
//! What a finished recover does to its target:
//! - open, release, configure: the session is let go (the next message opens it again);
//! - prepare: a new prepare under a new id, and the open waiting on it again after it;
//! - abandon: only its slot is freed; the attempt stays unknown, the next Stop abandons it anew;
//! - resolve_permission: its card closes as unknown; the attempt is not touched.
//!
//! A recover that fails (unsupported, or the Node could not prove the target stopped) leaves the
//! session held; the next Stop asks again under a new id.
//!
//! A ledger `exec` left unknown (a restart cut it off) is the one target retired without a Stop:
//! once its deadline passed, or when you ask for its flow step to run again, a recover stops its
//! process group and it ends interrupted ([`NodeRuntime::recover_exec_in`]). It is never run again.

use super::*;
use steve_store::node_commands::{dispatch, recovery};

/// The controls Stop may retire.
const RECOVERABLE: [&str; 6] = [
    "open_session",
    "release_session",
    "configure_session",
    "prepare_workspace",
    "abandon",
    "resolve_permission",
];

/// How many recovers one unknown `exec` gets at most.
const EXEC_RECOVERS: i64 = 3;

/// How a control reads in a session note.
pub(super) fn what(method: &str) -> String {
    match method {
        "open_session" => steve_i18n::stored(|| steve_i18n::t!("hub.node.what.open_session")),
        "release_session" => steve_i18n::stored(|| steve_i18n::t!("hub.node.what.release_session")),
        "configure_session" => {
            steve_i18n::stored(|| steve_i18n::t!("hub.node.what.configure_session"))
        }
        "prepare_workspace" => {
            steve_i18n::stored(|| steve_i18n::t!("hub.node.what.prepare_workspace"))
        }
        "abandon" => steve_i18n::stored(|| steve_i18n::t!("hub.node.what.abandon")),
        "resolve_permission" => {
            steve_i18n::stored(|| steve_i18n::t!("hub.node.what.resolve_permission"))
        }
        _ => steve_i18n::stored(|| steve_i18n::t!("hub.node.what.other")),
    }
}

impl NodeRuntime {
    /// Stop on `session` when no attempt can be cancelled or abandoned (`attempt`: the unknown
    /// attempt whose own abandon is unknown, the only control retired then). `Some(true)`: a
    /// recover was planned, or one is on its way already (nothing new is sent). `None`: no unknown
    /// control holds the session.
    pub(super) fn recover_in(
        &self,
        tx: &Tx,
        session: &str,
        attempt: Option<&str>,
    ) -> Result<Option<bool>, StoreError> {
        // one recover at a time per session: a second Stop while it is on its way sends nothing
        let live = self
            .store
            .read(|r| recovery::live_targets(r, self.node_id.as_str()))?;
        for target in live {
            if command_any_session_in(&self.store, &target)?.as_deref() == Some(session) {
                return Ok(Some(true));
            }
        }
        let candidates = self
            .store
            .read(|r| recovery::unknown(r, self.node_id.as_str()))?;
        for recovery::UnknownCommand {
            id: target,
            method,
            request,
            attempt: target_attempt,
        } in candidates
        {
            if !RECOVERABLE.contains(&method.as_str()) {
                continue;
            }
            if let Some(a) = attempt
                && (method != "abandon" || target_attempt.as_deref() != Some(a))
            {
                continue;
            }
            if command_any_session_in(&self.store, &target)?.as_deref() != Some(session) {
                continue;
            }
            self.plan_recover_in(
                tx,
                &target,
                &method,
                &request,
                target_attempt.as_deref(),
                "the user stopped a session held by a control whose outcome is unknown",
            )?;
            return Ok(Some(true));
        }
        Ok(None)
    }

    /// The `recover` of `target`, frozen now: the target's id and the digest the Node keeps of it.
    fn plan_recover_in(
        &self,
        tx: &Tx,
        target: &str,
        method: &str,
        request: &str,
        target_attempt: Option<&str>,
        reason: &str,
    ) -> Result<(), StoreError> {
        let n = self.store.read(|r| recovery::count(r, target))?;
        let mut op = format!("op_recover_{target}_{n}");
        if op.len() > 120 {
            op = derived_id("op_recover", &op);
        }
        // the Node keeps the command's content hash (method and business fields, not the id)
        let expected = serde_json::from_str::<Command<Value>>(request)
            .map_err(conflict)?
            .digest(method)
            .map_err(conflict)?;
        let (req, digest) = command(
            &op,
            Recover {
                target: idv(target.to_string())?,
                expected,
                reason: reason.into(),
            },
        )?;
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "recover",
                request: &req,
                digest: &digest,
                target_attempt,
                dependency: None,
            },
            self.clock.now(),
        )?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// A recover of `op`, a ledger `exec` or push (`git_apply`) the Node can no longer tell the end of (a restart cut it
    /// off): its process group, still running perhaps, is stopped and it ends interrupted (the
    /// command is never run again). Nothing new while one is on its way or one retired it already,
    /// and at most [`EXEC_RECOVERS`] in all (one the Node could not finish is asked again, a
    /// few times). True: one was planned now.
    pub(super) fn recover_exec_in(
        &self,
        tx: &Tx,
        op: &str,
        reason: &str,
    ) -> Result<bool, StoreError> {
        let Some((method, request)) = self
            .store
            .read(|r| recovery::unknown_exec(r, self.node_id.as_str(), op))?
        else {
            return Ok(false);
        };
        let (live, all) = self
            .store
            .read(|r| recovery::exec_recovers(r, self.node_id.as_str(), op))?;
        if live || all >= EXEC_RECOVERS {
            return Ok(false);
        }
        self.plan_recover_in(tx, op, &method, &request, None, reason)?;
        Ok(true)
    }

    /// `op`'s target is being retired by a recover that may be at the Node already: its own end
    /// (`interrupted`, as a recover ends it) changes nothing here, what follows comes with the
    /// recover's end ([`Self::recover_receipt_in`]). The Node may push that end before the Hub
    /// merged the recover's answer, so a recover still pending counts once it was (maybe) sent.
    /// Any other end is the target's own and is taken as usual (the recover then finds it stale).
    pub(super) fn retiring_in(&self, op: &str, interrupted: bool) -> Result<bool, StoreError> {
        if !interrupted {
            return Ok(false);
        }
        self.store
            .read(|r| recovery::retiring(r, self.node_id.as_str(), op))
    }

    /// What the Node said about a recover (its answer or its stream).
    pub(super) fn recover_receipt_in(
        &self,
        tx: &Tx,
        op: &str,
        state: ReceiptState,
        success: bool,
        detail: &str,
    ) -> Result<(), StoreError> {
        match state {
            ReceiptState::Finished if success => self.retired_in(tx, op),
            ReceiptState::Finished | ReceiptState::Unknown => {
                self.recover_failed_in(tx, op, detail)
            }
            _ => Ok(()),
        }
    }

    /// A recover the Node refused or could not finish: the session stays held, nothing is sent
    /// again on its own; the next Stop asks under a new id.
    pub(super) fn recover_failed_in(
        &self,
        tx: &Tx,
        op: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        let target = self.store.read(|r| recovery::target(r, op))?;
        let Some(session) = command_any_session_in(&self.store, &target)? else {
            return Ok(());
        };
        system_in(
            tx,
            &session,
            &steve_i18n::stored(|| steve_i18n::t!("hub.node.recover_failed", detail = detail)),
        )
    }

    /// A retired release a successor waits for (a handoff, B4): what it did on the Node is
    /// unknown, so the old harness may still be open in the shared worktree. The session is asked
    /// to let go again under a new id and the successor, told once, waits on that one instead (a
    /// refusal of it is asked again as any handoff release is, [`Self::release_again_in`]).
    /// False: nothing waits for the retired release.
    fn release_for_successor_in(
        &self,
        tx: &Tx,
        session: &str,
        retired: &str,
    ) -> Result<bool, StoreError> {
        let waiting = self
            .store
            .read(|r| dispatch::pending_dependents(r, retired))?;
        if waiting.is_empty() {
            return Ok(false);
        }
        // the session stays releasing: this release's end is the session's ([`Self::released_in`])
        let op = self.release_command_in(tx, session)?;
        for w in &waiting {
            self.store
                .write(|writer| dispatch::retarget_dependency(writer, w, &op, now()))?;
            if let Some(next) = command_any_session_in(&self.store, w)? {
                system_in(
                    tx,
                    &next,
                    &steve_i18n::stored(|| steve_i18n::t!("hub.node.release_retry")),
                )?;
            }
        }
        system_in(
            tx,
            session,
            &steve_i18n::t!(
                "hub.node.recovered_release_again",
                what = what("release_session")
            ),
        )?;
        self.wake_send.notify_one();
        Ok(true)
    }

    /// The Node retired the recover's target (interrupted, its effects kept as they are).
    fn retired_in(&self, tx: &Tx, op: &str) -> Result<(), StoreError> {
        let (target, method) = self.store.read(|r| recovery::retired_target(r, op))?;
        let Some(session) = command_any_session_in(&self.store, &target)? else {
            return Ok(());
        };
        let what = what(&method);
        match method.as_str() {
            "open_session" | "release_session" | "configure_session" => {
                self.retired_session_control_in(tx, &session, &target, &method, &what)?;
            }
            "prepare_workspace" => {
                self.retired_prepare_in(tx, &session, &target, &what)?;
            }
            "abandon" => system_in(
                tx,
                &session,
                &steve_i18n::stored(|| steve_i18n::t!("hub.node.recovered_abandon", what = what)),
            )?,
            "resolve_permission" => {
                if let Some(row) = rt::permission_of_answer_in(tx, &target)? {
                    self.close_in(
                        tx,
                        &row,
                        &steve_i18n::stored(|| steve_i18n::t!("hub.node.outcome_unknown")),
                    )?;
                }
                system_in(
                    tx,
                    &session,
                    &steve_i18n::stored(|| {
                        steve_i18n::t!("hub.node.recovered_permission", what = what)
                    }),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Release the session held by a retired control, or preserve its successor's release fence.
    fn retired_session_control_in(
        &self,
        tx: &Tx,
        session: &str,
        target: &str,
        method: &str,
        what: &str,
    ) -> Result<(), StoreError> {
        let held = match method {
            "open_session" => "opening",
            "release_session" => "releasing",
            _ => "configuring",
        };
        if self
            .phase_in(session)?
            .is_none_or(|(p, a)| p != held || a.is_some())
        {
            return Ok(());
        }
        if method == "release_session" && self.release_for_successor_in(tx, session, target)? {
            return Ok(());
        }
        let rev = self.session_revision(session)?;
        rt::session_cas_in(
            tx,
            session,
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
            session,
            &steve_i18n::stored(|| steve_i18n::t!("hub.node.recovered_released", what = what)),
        )?;
        // what waits for the session (queued, or a turn that waited for an open) opens it
        let waiting = self.store.read(|r| {
            Ok::<_, StoreError>(
                node_sessions::planning::has_queued(r, session)?
                    || node_turns::waiting_to_open(r, session)?.is_some(),
            )
        })?;
        if waiting && !self.is_holding() {
            self.reopen_after_in(tx, session, None)?;
        }
        Ok(())
    }

    /// Reject opens tied to the retired prepare and give their replacement a new dependency.
    fn retired_prepare_in(
        &self,
        tx: &Tx,
        session: &str,
        target: &str,
        what: &str,
    ) -> Result<(), StoreError> {
        // the open that waited on it never goes: it waits on a new prepare instead
        let opens = self
            .store
            .read(|r| dispatch::pending_dependents(r, target))?;
        for o in &opens {
            rt::merge_receipt_in(
                tx,
                o,
                ReceiptState::Rejected,
                false,
                &json!({ "reason": "its prepare was retired: it waits on a new one" }),
                now(),
            )?;
        }
        system_in(
            tx,
            session,
            &steve_i18n::stored(|| steve_i18n::t!("hub.node.recovered_prepare", what = what)),
        )?;
        if !opens.is_empty() {
            let req = self.store.read(|r| recovery::request(r, target))?;
            let body = serde_json::from_str::<Command<Prepare>>(&req)?.body;
            let prep = uid(&format!("op_prep_{session}"));
            let (req, digest) = command(&prep, body)?;
            rt::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: &prep,
                    node: self.node_id.as_str(),
                    method: "prepare_workspace",
                    request: &req,
                    digest: &digest,
                    target_attempt: None,
                    dependency: None,
                },
                self.clock.now(),
            )?;
            self.reopen_after_in(tx, session, Some(&prep))?;
            self.wake_send.notify_one();
        }
        Ok(())
    }
}

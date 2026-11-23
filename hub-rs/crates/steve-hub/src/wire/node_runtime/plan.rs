//! T1, planning: message queueing, turn claiming, frozen attempt specs, mid-turn delivery and
//! stop/cancel planning.

use super::{
    intent::{Advance, next_revision},
    *,
};
use steve_store::node_commands::dispatch;
use steve_store::node_sessions::planning;
use steve_store::node_turns::planning as turns;

/// What a plan did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Planned {
    /// A turn was planned: its command is on the way.
    Created,
    /// The session is opening, running or releasing: it waits.
    Busy,
    /// Turns are held (release, maintenance, or the session's stream is faulted).
    Held,
    /// Nothing is queued.
    Empty,
}

impl NodeRuntime {
    // ── what the core asks (AgentRuntime for Node sessions) ──

    /// Queues the message and, if the session can take a turn now, plans it (T1); otherwise it
    /// waits in the queue, and the answer says why.
    pub fn prompt(
        &self,
        session: &str,
        text: &str,
        via: &str,
        files: &[FileRef],
    ) -> Result<Planned, StoreError> {
        self.running()?;
        let planned = self.write(|tx| {
            self.store
                .write(|w| queue_in(w, session, text, via, files, now()))?;
            self.plan_in(tx, session)
        })?;
        if planned == Planned::Created {
            self.wake_send.notify_one();
        }
        Ok(planned)
    }

    /// Plans a turn from what is queued, if the session can take one (T1), led by Steve's own
    /// message when there is one (identical texts go once); otherwise nothing changes.
    pub fn flush(&self, session: &str, lead: Option<(&str, &str)>) -> Result<Planned, StoreError> {
        Ok(self.flush_turn(session, lead)?.0)
    }

    /// [`Self::flush`], with the attempt it planned when it planned one.
    pub fn flush_turn(
        &self,
        session: &str,
        lead: Option<(&str, &str)>,
    ) -> Result<(Planned, Option<String>), StoreError> {
        self.running()?;
        let (planned, attempt) = self.write(|tx| {
            // a released session takes the lead too: once something is queued it opens again
            let released = self.may_plan()
                && !self.is_holding()
                && self
                    .phase_in(session)?
                    .is_some_and(|(p, a)| p == "released" && a.is_none());
            let p = self.plan_state_in(tx, session)?;
            if !released && matches!(p, Planned::Busy | Planned::Held) {
                return Ok((p, None));
            }
            if let Some((text, via)) = lead {
                let (first, waiting) = self
                    .store
                    .read(|r| planning::queue_head(r, session, text))?;
                if !waiting {
                    self.store.write(|w| {
                        queue_in(w, session, text, via, &[], first.map_or(now(), |t| t - 1))
                    })?;
                }
            }
            let p = self.plan_in(tx, session)?;
            let attempt = if p == Planned::Created {
                self.phase_in(session)?.and_then(|(_, active)| active)
            } else {
                None
            };
            Ok((p, attempt))
        })?;
        if planned == Planned::Created {
            self.wake_send.notify_one();
        }
        Ok((planned, attempt))
    }

    /// Attempts `a` and `b` belong to the same turn (one is a gateway retry of the other).
    pub fn same_turn(&self, a: &str, b: &str) -> Result<bool, StoreError> {
        self.store.read(|r| turns::same_turn(r, a, b))
    }

    /// [`Self::plan_state_in`], and a released session with messages waiting is opened again
    /// (the open is what it waits for).
    fn state_in(&self, tx: &mut W<'_>, session: &str) -> Result<Planned, StoreError> {
        let p = self.plan_state_in(tx, session)?;
        if p == Planned::Busy {
            let open = self.store.read(|r| {
                let phase = planning::required_phase(r, session)?;
                Ok::<_, StoreError>(
                    phase.name == "released"
                        && phase.active_attempt.is_none()
                        && planning::has_queued(r, session)?,
                )
            })?;
            if open {
                self.reopen_in(tx, session)?;
            }
        }
        Ok(p)
    }

    /// What [`Self::plan_in`] would answer, without planning (and without writing anything).
    fn plan_state_in(&self, tx: &mut W<'_>, session: &str) -> Result<Planned, StoreError> {
        if self.ended_in(session)? {
            self.store.sessions().retire_queue(session)?;
            return Ok(Planned::Held);
        }
        if startup_blocked_in(tx, session)? || node_write::stall::held(tx, session)? {
            return Ok(Planned::Held);
        }
        // Node facts and reconciliation can schedule the next queued turn without going through
        // RoutedRuntime::flush. Recheck the durable graph in the same transaction as the claim.
        if self.workflow_blocker_in(session)?.is_some() {
            return Ok(Planned::Held);
        }
        // the persisted upgrade maintenance holds too: after a restart mid-drain nobody calls hold
        if self.is_holding() {
            return Ok(Planned::Held);
        }
        if !self.may_plan() {
            // not reconciled with the Node yet: it waits in the queue
            return Ok(Planned::Busy);
        }
        // a faulted stream waits for reconciliation: what the Node does for this session now would
        // never be applied, so nothing new starts there (and a released session is not reopened)
        if self.store.read(|r| faulted(r, &self.node_id, session))? || self.is_held(session) {
            return Ok(Planned::Held);
        }
        let phase = self.store.read(|r| planning::required_phase(r, session))?;
        if phase.name != "ready" || phase.active_attempt.is_some() {
            return Ok(Planned::Busy);
        }
        // a config change goes before the next turn, at this idle boundary
        if self.boundary_in(tx, session)? {
            return Ok(Planned::Busy);
        }
        let any = self.store.read(|r| planning::has_queued(r, session))?;
        Ok(if any {
            Planned::Created
        } else {
            Planned::Empty
        })
    }

    /// T1 inside `tx`: claim, attempt, reservation, command.
    fn plan_in(&self, tx: &mut W<'_>, session: &str) -> Result<Planned, StoreError> {
        let p = self.state_in(tx, session)?;
        if p != Planned::Created {
            return Ok(p);
        }
        if self.node_id.as_str() == "local" && !local_admission(tx)? {
            return Ok(Planned::Held);
        }
        // a member joined the workspace since the session was bound, say: no new turn there
        self.session_owners_work_in(tx, session)?;
        let turn = uid("t");
        let msgs = node_write::claim_in(tx, session, &turn, now())?;
        if msgs.is_empty() {
            return Ok(Planned::Empty);
        }
        let mut prompt = vec![Part::Text {
            text: msgs
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }];
        for m in &msgs {
            event_in(
                tx,
                session,
                "user",
                message_event(&m.text, &m.via, &m.files),
            )?;
            for f in &m.files {
                match file_part(tx, f)? {
                    Some(p) => prompt.push(p),
                    // the file is gone from the Hub (its row too): the message goes without it
                    // instead of staying queued for good
                    None => {
                        tracing::warn!("{session}: attachment {} is gone; sent without it", f.id)
                    }
                }
            }
        }
        self.attempt_in(tx, session, &turn, 0, prompt)?;
        Ok(Planned::Created)
    }

    /// [`Self::plan_in`] while applying a Node fact: a turn that cannot be planned (a bad
    /// attachment, say) must not keep the fact from being applied. Its messages stay queued.
    pub(super) fn plan_after_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
    ) -> Result<Planned, StoreError> {
        let mark = self.staged_mark();
        let mut plan_failed = false;
        match tx.scope(|w| self.plan_in(w, session).inspect_err(|_| plan_failed = true)) {
            Err(e) if plan_failed && !matches!(e, StoreError::Storage(_)) => {
                self.unstage(mark);
                tracing::error!("plan a turn of {session}: {e}");
                Ok(Planned::Empty)
            }
            result => result,
        }
    }

    /// A new attempt of `turn` with its own frozen request and its `start_turn` command.
    pub(super) fn attempt_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        turn: &str,
        ordinal: i64,
        prompt: Vec<Part>,
    ) -> Result<(), StoreError> {
        let open = self.open_of(session)?;
        let attempt = uid("a");
        let advance = if ordinal == 0 {
            Advance::NewInput
        } else {
            Advance::RetryTurn
        };
        let key = next_revision(tx, Intent::StartTurn(idv(turn.to_string())?), advance)?;
        let op = key.op().to_string();
        let limit = self.turn_limit.load(std::sync::atomic::Ordering::Relaxed);
        let spec = TurnSpec {
            session: open.session.clone(),
            turn: idv(turn.to_string())?,
            run: RunSpec {
                attempt: idv(attempt.clone())?,
                choice: open.choice.clone(),
                directory: RunDirectory::Workspace {
                    workspace: open.workspace.clone(),
                },
                prompt,
                policy: open.policy.clone(),
                limits: Limits {
                    deadline: Timestamp(now() + limit),
                    offline_until: Timestamp(now() + limit),
                    /* no total cap on a turn's output: a long turn is not cut for talking much; the
                     * Node's journal budget still keeps its disk (2026-11-09) */
                    max_output_bytes: u64::MAX,
                    max_tokens: None,
                    max_cost: None,
                },
                continuation: None,
            },
        };
        let exec = ExecutionRef::Turn {
            session: open.session.clone(),
            turn: spec.turn.clone(),
            attempt: spec.run.attempt.clone(),
        };
        let (req, digest) = command(&op, spec.clone())?;
        node_write::insert_attempt_in(
            tx,
            &rt::NewAttempt {
                id: &attempt,
                turn,
                ordinal,
                node: self.node_id.as_str(),
                operation: &op,
                execution: &serde_json::to_value(&exec).map_err(conflict)?,
                spec: &serde_json::to_value(&spec).map_err(conflict)?,
                digest: &digest,
            },
            now(),
        )?;
        rt::insert_intent_in(
            tx,
            &key,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "start_turn",
                request: &req,
                digest: &digest,
                target_attempt: Some(&attempt),
                dependency: None,
            },
            self.clock.now(),
        )?;
        let trev = self.turn_revision(turn)?;
        node_write::turn_cas_in(
            tx,
            turn,
            trev,
            &rt::TurnUpdate {
                state: "running",
                active_attempt: Some(&attempt),
                retry_ordinal: ordinal,
                applied: false,
            },
            now(),
        )?;
        let srev = self.session_revision(session)?;
        if !node_write::session_cas_in(
            tx,
            session,
            srev,
            &rt::SessionUpdate {
                phase: "active",
                active_attempt: Some(&attempt),
                release_pending: false,
            },
            now(),
        )? {
            return Err(conflict("the session changed while planning"));
        }
        // the turn is under way from the moment it is planned
        // (opening or loading the harness may take long, and a restart may leave the attempt
        // unknown before it starts); `starting` until the harness has a session of its own. A
        // retry of a turn already running stays running.
        let starting = if self
            .store
            .read(|r| planning::has_running_harness(r, session))?
        {
            "running"
        } else {
            "starting"
        };
        self.status_in(session, starting)?;
        Ok(())
    }

    /// Asks the Node to cancel the attempt running now ([`Self::cancel_attempt`] of it). The
    /// lifecycle's cancels (an item cancelled, done or paused, a steer) come here: they never
    /// retire a control, that is the user's [`Self::user_stop`] only.
    pub fn cancel(&self, session: &str) -> Result<bool, StoreError> {
        match self.active_attempt(session)? {
            Some(a) => self.cancel_attempt(session, &a),
            None => {
                self.running()?;
                Ok(false)
            }
        }
    }

    /// The user's Stop: [`Self::cancel`], and with no attempt to stop, a control whose outcome is
    /// unknown and holds the session is retired by one explicit `recover` ([`recover`]).
    pub fn user_stop(&self, session: &str) -> Result<bool, StoreError> {
        let attempt = self.active_attempt(session)?;
        self.cancel_as(session, attempt.as_deref(), true)
    }

    /// Asks the Node to cancel the turn `attempt` belongs to, only while that turn is still the
    /// session's running one: the stop reaches the attempt active now, which is `attempt` itself or
    /// a gateway retry of it (a retry is a new attempt of the same turn: it keeps the turn's
    /// token). The target is frozen here, never looked up again when the
    /// command goes out. `true`: the turn ran, the cancel is on its way; `false`: that turn is over
    /// (another one may run now), and nothing is queued.
    pub fn cancel_attempt(&self, session: &str, attempt: &str) -> Result<bool, StoreError> {
        self.cancel_as(session, Some(attempt), false)
    }

    /// [`Self::cancel_attempt`]; `stop`: the user asked, a recover may be planned (also with no
    /// attempt to stop).
    fn cancel_as(
        &self,
        session: &str,
        attempt: Option<&str>,
        stop: bool,
    ) -> Result<bool, StoreError> {
        self.running()?;
        let r = self.write(|tx| {
            let row = match attempt {
                Some(attempt) => self
                    .store
                    .read(|r| turns::active_for(r, session, attempt))?,
                None => None,
            };
            let Some(turns::ActiveAttempt {
                id: attempt,
                operation: target,
                revision: rev,
                state,
            }) = row
            else {
                if !stop {
                    return Ok(false);
                }
                return Ok(self.recover_in(tx, session, None)?.unwrap_or(false));
            };
            node_write::cancel_attempt_in(tx, &attempt, rev, now())?;
            let start = self.store.read(|r| dispatch::start_state(r, &target))?;
            match start {
                // every send of it so far was refused before acceptance (or none was made): it
                // provably never ran, so it is settled here and nothing is sent
                Some((st, false)) if st == "pending" => {
                    return self.cancel_unsent_in(tx, session, &attempt, &target);
                }
                // it may be on its way or at the Node: a cancel now could reach the Node first and
                // be refused for a target it does not know yet. The cancel goes once the Node is
                // known to hold it (receipt_in); a send refused before acceptance settles it here
                // (send_one) unless an earlier send was uncertain; a refusal ends it anyway. Once it started on the Node (its stream
                // said so), the Node holds it and the cancel goes now
                Some((st, true)) if st == "pending" && state == "planned" => return Ok(true),
                _ => {}
            }
            // an abandon of it the Node cannot tell the end of is retired first (its slot freed)
            if stop
                && state == "unknown"
                && let Some(r) = self.recover_in(tx, session, Some(&attempt))?
            {
                return Ok(r);
            }
            // what nobody can tell is running is given up, once the Node proves it stopped
            let method = if state == "unknown" {
                "abandon"
            } else {
                "cancel"
            };
            self.stop_command_in(tx, &attempt, &target, method)?;
            Ok(true)
        })?;
        if r {
            self.wake_send.notify_one();
            // a turn settled here is told from the upkeep worker, never under the caller's lock
            self.wake_upkeep.notify_one();
        }
        Ok(r)
    }

    /// Queues a stop of `attempt` (whose `start_turn` is `target`): one at a time; after one that
    /// failed or was refused, a new one (a new intent of the user, never the old one sent again).
    pub(super) fn stop_command_in(
        &self,
        tx: &mut W<'_>,
        attempt: &str,
        target: &str,
        method: &str,
    ) -> Result<(), StoreError> {
        self.stop_command_as_in(
            tx,
            attempt,
            target,
            method,
            "the Hub gives up an attempt nobody can tell the end of",
        )
    }

    /// [`Self::stop_command_in`], with the explicit reason for the requested stop.
    /// marks one the Hub sent on its own after a Node restart).
    pub(super) fn stop_command_as_in(
        &self,
        tx: &mut W<'_>,
        attempt: &str,
        target: &str,
        method: &str,
        why: &str,
    ) -> Result<(), StoreError> {
        // one retired by a recover is over too, even before its own end came
        let (n, live) = self
            .store
            .read(|r| dispatch::stop_commands(r, method, attempt))?;
        if live {
            return Ok(());
        }
        if n != 0 {
            return Ok(());
        }
        let subject = idv(attempt.to_string())?;
        let intent = if method == "abandon" {
            Intent::Abandon(subject)
        } else {
            Intent::Cancel(subject)
        };
        let key = next_revision(tx, intent, Advance::NewInput)?;
        let op = key.op().to_string();
        let target = idv(target.to_string())?;
        let (req, digest) = if method == "abandon" {
            command(
                &op,
                Abandon {
                    target,
                    reason: why.into(),
                },
            )?
        } else {
            command(
                &op,
                Cancel {
                    target,
                    reason: "the Hub cancels the turn".into(),
                },
            )?
        };
        rt::insert_intent_in(
            tx,
            &key,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method,
                request: &req,
                digest: &digest,
                target_attempt: Some(attempt),
                dependency: None,
            },
            self.clock.now(),
        )?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// [`Self::cancel`] of an attempt whose `start_turn` never left the Hub: the command is
    /// rejected locally, the attempt ends cancelled, its turn with it, and what is queued goes next.
    pub(super) fn cancel_unsent_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        attempt: &str,
        op: &str,
    ) -> Result<bool, StoreError> {
        node_write::merge_receipt_in(
            tx,
            op,
            ReceiptState::Rejected,
            false,
            &json!({ "reason": "cancelled before it was sent" }),
            now(),
        )?;
        let completion = Completion {
            outcome: Outcome::Cancelled,
            stop_reason: "cancelled".into(),
            error: None,
            result: vec![],
            continuation: None,
            transient: false,
            acted: false,
            job: None,
        };
        self.settle_in(tx, session, attempt, &completion)?;
        Ok(true)
    }

    /// Ends an attempt the Node provably never ran (or no longer knows) on the Hub's own word: its
    /// turn ends with it (never sent again), and what is queued goes next.
    pub(super) fn settle_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        attempt: &str,
        completion: &Completion,
    ) -> Result<(), StoreError> {
        let (rev, turn) = self.store.read(|r| {
            let row = node_turns::attempt(r, attempt)?
                .ok_or_else(|| StoreError::Rejected(format!("missing attempt {attempt}")))?;
            let (turn, _) = node_turns::for_attempt(r, attempt)?;
            Ok::<_, StoreError>((row.revision, turn))
        })?;
        node_write::attempt_cas_in(
            tx,
            attempt,
            rev,
            "finished",
            None,
            Some(&serde_json::to_value(completion).map_err(conflict)?),
            now(),
        )?;
        self.close_cards_of_attempt_in(tx, attempt)?;
        self.turn_over_in(tx, session, &turn, completion)?;
        // the attempt's statistics row, if it had started, ends with this outcome (with the
        // runtime state); the business side hears it once this transaction committed (staged
        // until then, see `Self::write`)
        let at = now();
        let facts = super::apply::take_facts(tx, attempt)?;
        let measure = super::super::acp_stats::try_node_attempt_finished(
            &self.store,
            session,
            attempt,
            completion,
            &facts,
            at,
        )?;
        let finished = After::Finished {
            session: session.to_string(),
            attempt: attempt.to_string(),
            completion: completion.clone(),
            facts,
            at,
            measure,
        };
        let settled = self.plan_after_in(tx, session)? != Planned::Created;
        if settled {
            self.release_if_ended_in(tx, session)?;
            self.status_in(session, settled_status(&completion.stop_reason))?;
        }
        let ended = settled.then(|| End {
            session: session.to_string(),
            attempt: Some(attempt.to_string()),
            stop: completion.stop_reason.clone(),
            error: completion.error.as_ref().map(said),
        });
        let mut staged = self.staged.lock().unwrap();
        staged.push(Tell::After(finished));
        staged.extend(ended.map(Tell::End));
        Ok(())
    }
}

/// A message for a Node-bound session while its Node is unavailable here: it is
/// queued, never given to another backend, and goes once the Node is back and reconciled.
pub fn park(
    store: &Store,
    session: &str,
    text: &str,
    via: &str,
    files: &[FileRef],
) -> Result<(), StoreError> {
    store.write(|w| {
        queue_in(w, session, text, via, files, now())?;
        if !node_sessions::has_current_park_notice(w, session)? {
            w.events().add(
                session, "system",
                json!({ "text": steve_i18n::stored(|| steve_i18n::t!("hub.session.node_parked")), "parked": true }),
                None, now(),
            )?;
        }
        Ok(())
    })
}

/// Queues a message (with its `queued` event) at `created`.
fn queue_in(
    w: &mut W<'_>,
    session: &str,
    text: &str,
    via: &str,
    files: &[FileRef],
    created: i64,
) -> Result<(), StoreError> {
    if node_sessions::ended(w, session)? {
        return Err(conflict("已结束的会话不能接收新消息，请使用当前会话。"));
    }
    node_sessions::queue(w, &uid("q"), session, text, via, files, created)?;
    w.events().add(
        session,
        "queued",
        message_event(text, via, files),
        None,
        now(),
    )?;
    node_sessions::changed_event(w, session)
}

pub(super) fn startup_blocked_in(tx: &mut W<'_>, session: &str) -> Result<bool, StoreError> {
    node_sessions::startup::active(tx, session)
}

/// Messages said to a session while its turn runs, handed to that turn through a Steve tool call's
/// result instead of waiting for the turn to end or cutting it (2026-11-09: the agent hears you while
/// it works). Only while `attempt` is the running, uncancelled attempt of `turn`; each message leaves
/// the queue as the turn's dispatch and shows as the user's message in the session. Their texts.
pub(crate) fn deliver_mid_turn(
    store: &Store,
    node_session: &str,
    turn: &str,
    attempt: &str,
) -> Result<Vec<String>, StoreError> {
    store.write(|tx| {
        let session =
            store.read(|r| turns::session_for_live_attempt(r, node_session, turn, attempt))?;
        let Some(session) = session else {
            return Ok(vec![]);
        };
        let session = session.as_str();
        let xs = node_write::deliver_into_turn_in(tx, session, turn, now())?;
        for m in &xs {
            let mut ev = message_event(&m.text, &m.via, &[]);
            ev["mid_turn"] = json!(true);
            event_in(tx, session, "user", ev)?;
        }
        if !xs.is_empty() {
            node_sessions::changed_event(tx, session)?;
        }
        Ok(xs.into_iter().map(|m| m.text).collect())
    })
}

/// What the agent reads at the end of a Steve tool's result when messages came while it worked.
pub(crate) fn mid_turn_note(texts: &[String]) -> String {
    let list = texts
        .iter()
        .enumerate()
        .map(|(n, t)| format!("{}. {t}", n + 1))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\n——\n（Steve）你工作时用户发来了新消息，已经送到这里，不会再单独发给你：\n{list}\n按这些消息调整当前的工作；要你停下或换方向，就照做。"
    )
}

/// A message as the session's events show it (the same shape on every backend).
fn message_event(text: &str, via: &str, files: &[FileRef]) -> Value {
    let mut v = json!({ "text": text, "via": via });
    if !files.is_empty() {
        v["files"] = json!(files);
    }
    v
}

/// An attachment for the Node to fetch from the Hub by hash (none when the Hub has no such file).
fn file_part(tx: &mut W<'_>, f: &FileRef) -> Result<Option<Part>, StoreError> {
    let Some((_, sha, _, mime, size)) = steve_store::file_repo::file_row(tx, &f.id)? else {
        return Ok(None);
    };
    Ok(Some(Part::File {
        artifact: ArtifactRef {
            id: idv(f.id.clone())?,
            sha256: idv(if sha.starts_with("sha256:") {
                sha
            } else {
                format!("sha256:{sha}")
            })?,
            size: size.max(0) as u64,
            mime,
        },
        name: f.name.clone(),
    }))
}

/// The attempt (and its session) of a `start_turn` the user stopped, while it is not over.
pub(super) fn cancelled_start_in(
    tx: &mut W<'_>,
    op: &str,
) -> Result<Option<(String, String)>, StoreError> {
    Ok(node_turns::started_by(tx, op)?
        .filter(|a| a.cancelled && a.state != "finished")
        .map(|a| (a.id, a.session)))
}

/// Manual pause gates new logical turns on the local coordinator. Capacity belongs to
/// runtime::lease_command_in/session_room: counting active_attempt here would also count
/// never-sent turns, hiding authoritative room waits and imposing a second limit.
fn local_admission(tx: &mut W<'_>) -> Result<bool, StoreError> {
    planning::local_admits_work(tx)
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use steve_store::testkit::hub::node_planning as fixture;
    struct NoDomain;
    impl NodeDomain for NoDomain {
        fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
    }
    #[test]
    fn connection_ready_uses_the_required_transport_instead_of_local_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        let rt = NodeRuntime::new(
            store,
            steve_node_api::testkit::FakeNode::new("local"),
            Arc::new(NoDomain),
        );
        rt.reconciled.store(true, Ordering::SeqCst);
        assert!(rt.connection_ready(false), "embedded Node has reconciled");
        rt.require_transport_link();
        assert!(
            !rt.connection_ready(false),
            "service identity does not imply a link"
        );
        assert!(rt.connection_ready(true));
    }
    #[test]
    fn paused_admission_retains_prompts_and_flush_leads_without_claiming_a_turn() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .devices()
            .upsert(&steve_store::DeviceInfo {
                id: "hub".into(),
                name: "test".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .devices()
            .patch(
                "hub",
                &steve_store::DevicePatch {
                    status: Some("暂停接活".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        fixture::session(&store, "s1", "ready", None).unwrap();
        let rt = NodeRuntime::new(
            store.clone(),
            steve_node_api::testkit::FakeNode::new("local"),
            Arc::new(NoDomain),
        );
        rt.reconciled.store(true, Ordering::SeqCst);
        assert_eq!(
            rt.prompt("s1", "first", "user", &[]).unwrap(),
            Planned::Held
        );
        assert_eq!(
            rt.flush_turn("s1", Some(("second", "user"))).unwrap().0,
            Planned::Held
        );
        assert_eq!(
            fixture::queue_and_attempt_count(&store, "s1").unwrap(),
            (2, 0)
        );
    }
    #[test]
    fn pause_preserves_reservations_without_a_duplicate_capacity_gate() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .devices()
            .upsert(&steve_store::DeviceInfo {
                id: "hub".into(),
                name: "test".into(),
                ..Default::default()
            })
            .unwrap();
        fixture::session(&store, "s1", "ready", Some("already-running")).unwrap();
        store
            .devices()
            .patch(
                "hub",
                &steve_store::DevicePatch {
                    max_sessions: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.read(|r| planning::local_admits_work(r)).unwrap());
        assert_eq!(fixture::session_room(&store, "local").unwrap().running, 0);
        store
            .devices()
            .patch(
                "hub",
                &steve_store::DevicePatch {
                    max_sessions: Some(2),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.read(|r| planning::local_admits_work(r)).unwrap());
        store
            .devices()
            .patch(
                "hub",
                &steve_store::DevicePatch {
                    status: Some("暂停接活".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!store.read(|r| planning::local_admits_work(r)).unwrap());
        let phase = store.read(|r| planning::required_phase(r, "s1")).unwrap();
        assert_eq!(phase.active_attempt.as_deref(), Some("already-running"));
    }
    #[test]
    fn manual_pause_does_not_count_unsent_reservations_as_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .devices()
            .upsert(&steve_store::DeviceInfo {
                id: "hub".into(),
                name: "test".into(),
                ..Default::default()
            })
            .unwrap();
        for limit in [32, 256] {
            store
                .devices()
                .patch(
                    "hub",
                    &steve_store::DevicePatch {
                        status: Some("正常".into()),
                        max_sessions: Some(limit),
                        ..Default::default()
                    },
                )
                .unwrap();
            fixture::clear_sessions(&store).unwrap();
            for i in 0..limit {
                assert!(
                    store.read(|r| planning::local_admits_work(r)).unwrap(),
                    "reservation {i} below {limit}"
                );
                fixture::session(&store, &format!("s{i}"), "active", Some(&format!("a{i}")))
                    .unwrap();
            }
            assert!(
                store.read(|r| planning::local_admits_work(r)).unwrap(),
                "unsent attempts must reach the canonical command queue"
            );
            let room = fixture::session_room(&store, "local").unwrap();
            assert_eq!(room.max, limit);
            assert_eq!(room.running, 0);
            store
                .devices()
                .patch(
                    "hub",
                    &steve_store::DevicePatch {
                        max_sessions: Some(1),
                        status: Some("暂停接活".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
            assert!(!store.read(|r| planning::local_admits_work(r)).unwrap());
            assert_eq!(fixture::reservations(&store, "local").unwrap(), limit);
        }
    }
}

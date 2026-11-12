//! T1, planning: message queueing, turn claiming, frozen attempt specs, mid-turn delivery and
//! stop/cancel planning.

use super::*;

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
        let planned = self.tx(|tx| {
            queue_in(tx, session, text, via, files, now())?;
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
        let (planned, attempt) = self.tx(|tx| {
            // a released session takes the lead too: once something is queued it opens again
            let released = self.may_plan()
                && !self.is_holding()
                && phase_in(tx, session)?
                    .is_some_and(|(p, a)| p == "released" && a.is_none());
            let p = self.plan_state_in(tx, session)?;
            if !released && matches!(p, Planned::Busy | Planned::Held) {
                return Ok((p, None));
            }
            if let Some((text, via)) = lead {
                let (first, waiting): (Option<i64>, bool) = tx.query_row(
                    "SELECT MIN(created), EXISTS(SELECT 1 FROM queued WHERE session_id=?1 AND text=?2) FROM queued WHERE session_id=?1",
                    rusqlite::params![session, text],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if !waiting {
                    queue_in(tx, session, text, via, &[], first.map_or(now(), |t| t - 1))?;
                }
            }
            let p = self.plan_in(tx, session)?;
            let attempt = if p == Planned::Created {
                tx.query_row(
                    "SELECT active_attempt FROM runtime_sessions WHERE session_id=?",
                    [session],
                    |r| r.get(0),
                )?
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
        self.store.reader(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_attempts x JOIN runtime_attempts y ON y.turn_id = x.turn_id WHERE x.attempt_id=?1 AND y.attempt_id=?2)",
                [a, b],
                |r| r.get(0),
            )?)
        })
    }

    /// [`Self::plan_state_in`], and a released session with messages waiting is opened again
    /// (the open is what it waits for).
    fn state_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        let p = self.plan_state_in(tx, session)?;
        if p == Planned::Busy {
            let (phase, active, any): (String, Option<String>, bool) = tx.query_row(
                "SELECT phase, active_attempt, EXISTS(SELECT 1 FROM queued WHERE session_id=?1) FROM runtime_sessions WHERE session_id=?1",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if phase == "released" && active.is_none() && any {
                self.reopen_in(tx, session)?;
            }
        }
        Ok(p)
    }

    /// What [`Self::plan_in`] would answer, without planning (and without writing anything).
    fn plan_state_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        // Node facts and reconciliation can schedule the next queued turn without going through
        // RoutedRuntime::flush. Recheck the durable graph in the same transaction as the claim.
        if steve_store::workflow_gate::session_blocker(tx, session)?.is_some() {
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
        if faulted(tx, &self.node_id, session)? || self.is_held(session) {
            return Ok(Planned::Held);
        }
        let (phase, active): (String, Option<String>) = tx.query_row(
            "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if phase != "ready" || active.is_some() {
            return Ok(Planned::Busy);
        }
        // a config change goes before the next turn, at this idle boundary
        if self.boundary_in(tx, session)? {
            return Ok(Planned::Busy);
        }
        let any: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM queued WHERE session_id=?)",
            [session],
            |r| r.get(0),
        )?;
        Ok(if any {
            Planned::Created
        } else {
            Planned::Empty
        })
    }

    /// T1 inside `tx`: claim, attempt, reservation, command.
    fn plan_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        let p = self.state_in(tx, session)?;
        if p != Planned::Created {
            return Ok(p);
        }
        // a member joined the workspace since the session was bound, say: no new turn there
        self.session_owners_work_in(tx, session)?;
        let turn = uid("t");
        let msgs = rt::claim_in(tx, session, &turn, now())?;
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
            rt::add_event_in(
                tx,
                session,
                "user",
                message_event(&m.text, &m.via, &m.files),
                V1,
                None,
                now(),
            )?;
            for f in &m.files {
                match file_part(tx, f)? {
                    Some(p) => prompt.push(p),
                    // the file is gone from the Hub (its row too): the message goes without it,
                    // as on the legacy runner, instead of staying queued for good
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
    pub(super) fn plan_after_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        tx.execute_batch("SAVEPOINT plan")?;
        match self.plan_in(tx, session) {
            Ok(p) => {
                tx.execute_batch("RELEASE plan")?;
                Ok(p)
            }
            Err(e) => {
                tracing::error!("plan a turn of {session}: {e}");
                tx.execute_batch("ROLLBACK TO plan; RELEASE plan")?;
                Ok(Planned::Empty)
            }
        }
    }

    /// A new attempt of `turn` with its own frozen request and its `start_turn` command.
    pub(super) fn attempt_in(
        &self,
        tx: &Tx,
        session: &str,
        turn: &str,
        ordinal: i64,
        prompt: Vec<Part>,
    ) -> Result<(), StoreError> {
        let open = self.open_of(tx, session)?;
        let attempt = uid("a");
        let op = format!("op_{attempt}");
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
        rt::insert_attempt_in(
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
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "start_turn",
                request: &req,
                digest: &digest,
                target_attempt: Some(&attempt),
                dependency: None,
            },
            now(),
        )?;
        let trev = turn_revision(tx, turn)?;
        rt::turn_cas_in(
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
        let srev = session_revision(tx, session)?;
        if !rt::session_cas_in(
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
        // as the legacy runner's begin(): the turn is under way from the moment it is planned
        // (opening or loading the harness may take long, and a restart may leave the attempt
        // unknown before it starts); `starting` until the harness has a session of its own. A
        // retry of a turn already running stays running.
        let (acp, status): (bool, Option<String>) = tx
            .query_row(
                "SELECT acp_session_id IS NOT NULL, status FROM sessions WHERE id=?",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((false, None));
        let starting = if acp || status.as_deref() == Some("running") {
            "running"
        } else {
            "starting"
        };
        status_in(tx, session, starting)?;
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
    /// a gateway retry of it (a retry is a new attempt of the same turn, as the legacy runner keeps
    /// its turn token across one). The target is frozen here, never looked up again when the
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
        let r = self.tx(|tx| {
            let row: Option<(String, String, i64)> = match attempt {
                Some(attempt) => tx
                    .query_row(
                        "SELECT a.attempt_id, a.operation_id, a.revision FROM runtime_sessions s JOIN runtime_attempts a ON a.attempt_id = s.active_attempt JOIN runtime_turns t ON t.turn_id = a.turn_id JOIN runtime_attempts asked ON asked.turn_id = a.turn_id WHERE s.session_id=?1 AND asked.attempt_id=?2 AND a.state<>'finished' AND t.state<>'finished'",
                        [session, attempt],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?,
                None => None,
            };
            let Some((attempt, target, rev)) = row else {
                if !stop {
                    return Ok(false);
                }
                return Ok(self.recover_in(tx, session, None)?.unwrap_or(false));
            };
            let state: String = tx.query_row(
                "SELECT state FROM runtime_attempts WHERE attempt_id=?",
                [&attempt],
                |r| r.get(0),
            )?;
            rt::cancel_attempt_in(tx, &attempt, rev, now())?;
            let start: Option<(String, bool)> = tx
                .query_row(
                    "SELECT state, maybe_sent FROM node_commands WHERE operation_id=? AND method='start_turn'",
                    [&target],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
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
            let method = if state == "unknown" { "abandon" } else { "cancel" };
            self.stop_command_in(tx, &attempt, &target, method)?;
            Ok(true)
        })?;
        if r {
            self.wake_send.notify_one();
            // a turn settled here is told from the apply worker, never under the caller's lock
            self.wake_apply.notify_one();
        }
        Ok(r)
    }

    /// Queues a stop of `attempt` (whose `start_turn` is `target`): one at a time; after one that
    /// failed or was refused, a new one (a new intent of the user, never the old one sent again).
    pub(super) fn stop_command_in(
        &self,
        tx: &Tx,
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

    /// [`Self::stop_command_in`], an abandon saying `why` (kept in the command: [`RESTART_ABANDON`]
    /// marks one the Hub sent on its own after a Node restart).
    pub(super) fn stop_command_as_in(
        &self,
        tx: &Tx,
        attempt: &str,
        target: &str,
        method: &str,
        why: &str,
    ) -> Result<(), StoreError> {
        // one retired by a recover is over too, even before its own end came
        let (n, live): (i64, bool) = tx.query_row(
            &format!("SELECT COUNT(*), COALESCE(MAX((c.state NOT IN ('finished','rejected') OR json_extract(c.receipt,'$.success')=1) AND NOT {}), 0) FROM node_commands c WHERE c.method=? AND c.target_attempt=?", recover::RETIRED),
            [method, attempt],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if live {
            return Ok(());
        }
        let op = if n == 0 {
            format!("op_{method}_{attempt}")
        } else {
            format!("op_{method}_{attempt}_{n}")
        };
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
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method,
                request: &req,
                digest: &digest,
                target_attempt: Some(attempt),
                dependency: None,
            },
            now(),
        )?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// [`Self::cancel`] of an attempt whose `start_turn` never left the Hub: the command is
    /// rejected locally, the attempt ends cancelled, its turn with it, and what is queued goes next.
    pub(super) fn cancel_unsent_in(
        &self,
        tx: &Tx,
        session: &str,
        attempt: &str,
        op: &str,
    ) -> Result<bool, StoreError> {
        rt::merge_receipt_in(
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
        tx: &Tx,
        session: &str,
        attempt: &str,
        completion: &Completion,
    ) -> Result<(), StoreError> {
        let (rev, turn): (i64, String) = tx.query_row(
            "SELECT revision, turn_id FROM runtime_attempts WHERE attempt_id=?",
            [attempt],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        rt::attempt_cas_in(
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
        // the attempt's statistics row, if it had started, ends with this outcome; booked by the
        // apply worker once this transaction committed (staged until then, see `Self::tx`)
        let finished = After::Finished {
            session: session.to_string(),
            attempt: attempt.to_string(),
            completion: completion.clone(),
            at: now(),
        };
        let settled = self.plan_after_in(tx, session)? != Planned::Created;
        if settled {
            self.release_if_ended_in(tx, session)?;
            status_in(tx, session, settled_status(&completion.stop_reason))?;
        }
        let ended = settled.then(|| End {
            session: session.to_string(),
            attempt: Some(attempt.to_string()),
            stop: completion.stop_reason.clone(),
            error: completion.error.as_ref().map(said),
        });
        let mut staged = self.staged.lock().unwrap();
        staged.0.push(finished);
        staged.1.extend(ended);
        Ok(())
    }
}

/// A message for a Node-bound session while its Node is not enabled here (STEVE_NODE_MODE off): it is
/// queued, never given to another backend, and goes once the Node is back and reconciled.
pub fn park(
    store: &Store,
    session: &str,
    text: &str,
    via: &str,
    files: &[FileRef],
) -> Result<(), StoreError> {
    store.transaction(|tx| {
        queue_in(tx, session, text, via, files, now())?;
        let told: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND kind='system' AND json_extract(data,'$.parked')=1 AND seq > (SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?1 AND kind IN ('user','turn_end')))",
            [session],
            |r| r.get(0),
        )?;
        if !told {
            rt::add_event_in(
                tx,
                session,
                "system",
                json!({
                    "text": steve_i18n::stored(|| steve_i18n::t!("hub.session.node_parked")),
                    "parked": true
                }),
                V1,
                None,
                now(),
            )?;
        }
        Ok(())
    })
}

/// Queues a message (with its `queued` event) at `created`.
fn queue_in(
    tx: &Tx,
    session: &str,
    text: &str,
    via: &str,
    files: &[FileRef],
    created: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO queued (id, session_id, text, via, created, files) VALUES (?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            uid("q"),
            session,
            text,
            via,
            created,
            serde_json::to_string(files)?
        ],
    )?;
    rt::add_event_in(
        tx,
        session,
        "queued",
        message_event(text, via, files),
        V1,
        None,
        now(),
    )?;
    tx.publish(steve_store::ChangeEvent::Session {
        id: session.into(),
        item_id: None,
    });
    Ok(())
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
    store.transaction(|tx| {
        /* the Hub's session of the Node's, while that attempt is the turn's running one */
        let session: Option<String> = tx
            .query_row(
                "SELECT s.session_id FROM runtime_sessions s
                   JOIN runtime_turns t ON t.session_id=s.session_id
                   JOIN runtime_attempts a ON a.turn_id=t.turn_id
                  WHERE s.node_session_id=?1 AND t.turn_id=?2 AND t.state<>'finished'
                    AND t.active_attempt=?3 AND a.attempt_id=?3 AND a.cancel_requested=0",
                rusqlite::params![node_session, turn, attempt],
                |r| r.get(0),
            )
            .optional()?;
        let Some(session) = session else {
            return Ok(vec![]);
        };
        let session = session.as_str();
        let xs = rt::deliver_into_turn_in(tx, session, turn, now())?;
        for m in &xs {
            let mut ev = message_event(&m.text, &m.via, &[]);
            ev["mid_turn"] = json!(true);
            rt::add_event_in(tx, session, "user", ev, V1, None, now())?;
        }
        if !xs.is_empty() {
            tx.publish(steve_store::ChangeEvent::Session {
                id: session.into(),
                item_id: None,
            });
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
fn file_part(tx: &Tx, f: &FileRef) -> Result<Option<Part>, StoreError> {
    let Some((sha, size, mime)) = tx
        .query_row(
            "SELECT sha256, size, mime FROM files WHERE id=?",
            [&f.id],
            |r| Ok::<(String, i64, String), _>((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
    else {
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
    tx: &Tx,
    op: &str,
) -> Result<Option<(String, String)>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT a.attempt_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=? AND a.cancel_requested=1 AND a.state<>'finished'",
            [op],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

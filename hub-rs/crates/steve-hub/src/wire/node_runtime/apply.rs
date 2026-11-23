//! T4, applying: one Node event's effects (transcript projection, attempt/turn state transitions,
//! gateway retries, attempt statistics facts), and what the business side hears after the commit.

use super::*;
use steve_store::node_commands::{self, dispatch};
use steve_store::node_sessions::observations;
use steve_store::{EventSource, KeptFacts};

/// Retries of a turn after a passing gateway failure or a provider stream interruption.
const RETRY_DELAYS_MS: [i64; 3] = [20_000, 60_000, 120_000];

/// How many message roles the coordinator keeps in memory before it starts over.
const ROLES_KEPT: usize = 4096;

/// What one attempt did, as its statistics row counts it.
#[derive(Debug, Clone, Default)]
pub struct AttemptFacts {
    pub tools: i64,
    pub tool_errors: i64,
    /// Per tool category (`exec`, `edit`…) and `mcp:<tool>` for Steve's own; besides those
    /// `tool:<name>` per harness tool name.
    pub tool_kinds: std::collections::BTreeMap<String, i64>,
    pub permissions: i64,
    pub model: Option<String>,
    /// The attempt's share of the harness's own dollar figure, if it reported one.
    pub cost: Option<f64>,
}

/// What an attempt did so far, kept as its events are applied ([`steve_store::Attempts`]): by
/// call id, the last word on each tool.
type Kept = KeptFacts<HashMap<String, ToolFact>>;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ToolFact {
    name: String,
    category: ToolCategory,
    phase: ToolPhase,
}

impl From<Kept> for AttemptFacts {
    fn from(k: Kept) -> AttemptFacts {
        let mut f = AttemptFacts {
            permissions: k.permissions,
            model: k.model,
            cost: k.cost,
            ..Default::default()
        };
        for t in k.tools.values() {
            f.tools += 1;
            if t.phase == ToolPhase::Failed {
                f.tool_errors += 1;
            }
            let bucket = match t.name.strip_prefix("mcp__steve__") {
                Some(n) => format!("mcp:{n}"),
                None => serde_json::to_value(t.category)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "other".into()),
            };
            *f.tool_kinds.entry(bucket).or_default() += 1;
            if !t.name.is_empty() && !t.name.starts_with("mcp__steve__") {
                *f.tool_kinds.entry(format!("tool:{}", t.name)).or_default() += 1;
            }
        }
        f
    }
}

/// What `attempt` did so far.
fn facts_so_far(w: &mut W<'_>, attempt: &str) -> Result<Kept, StoreError> {
    Ok(w.attempts().facts(attempt)?.unwrap_or_default())
}

/// What `attempt` did, for its statistics row, once it is over (what was kept goes with it).
pub(super) fn take_facts(w: &mut W<'_>, attempt: &str) -> Result<AttemptFacts, StoreError> {
    let k: Kept = w.attempts().take_facts(attempt)?.unwrap_or_default();
    Ok(k.into())
}

impl NodeRuntime {
    // ── T4: applying ──

    /// Everything one event does, in `w` (a scope of the batch's write: it rolls back alone),
    /// and what it tells the business side once committed (staged). `received`: when the Hub
    /// received it.
    pub(super) fn apply_one(
        &self,
        w: &mut W<'_>,
        ev: &NodeEvent,
        received: i64,
    ) -> Result<(), StoreError> {
        let mut after: Vec<After> = self.permission_event_in(w, ev)?.into_iter().collect();
        for a in &after {
            // in memory only, and before the card's commit is seen: the YOLO engine has the
            // tool's input from the first look at the card
            if let After::Asked(a) = a {
                self.domain
                    .permission_arming(&a.session, &a.decision, &a.request);
            }
        }
        after.extend(self.facts_in(w, ev)?);
        self.realtime_in(w, ev, received)?;
        let end = self.apply_in(w, ev)?;
        self.activity_in(w, ev, received)?;
        let mut staged = self.staged.lock().unwrap();
        staged.extend(
            self.diagnostic_of(ev)
                .map(|d| Tell::Diagnostic(Box::new(d))),
        );
        staged.extend(after.into_iter().map(Tell::After));
        staged.extend(end.map(Tell::End));
        Ok(())
    }

    /// The diagnostic record of an adapter that exited under a session.
    fn diagnostic_of(&self, ev: &NodeEvent) -> Option<steve_store::diagnostics::Record> {
        let EventBody::Diagnostic {
            code,
            severity,
            details: Some(details),
            ..
        } = &ev.body
        else {
            return None;
        };
        if code != "adapter_exited" {
            return None;
        }
        let session = ev_session(ev)?;
        let mut d = steve_store::diagnostics::Details {
            exit_code: details.exit_code,
            signal: details.signal,
            generation: details.generation,
            parse_errors: Some(details.parse_errors),
            drain_timed_out: Some(details.drain_timed_out),
            reader_failed: Some(details.reader_failed),
            wait_error: Some(details.wait_error),
            source_at: Some(ev.at.0),
            source_node: Some(self.node_id.to_string()),
            source_epoch: Some(ev.cursor.epoch.to_string()),
            ..Default::default()
        };
        d.code = Some(
            if details.wait_error {
                "wait_error"
            } else if details.reader_failed {
                "reader_failed"
            } else {
                "process_exit"
            }
            .into(),
        );
        Some(steve_store::diagnostics::Record {
            session_id: Some(session),
            cause_id: Some(ev.id.to_string()),
            kind: "adapter.exited".into(),
            reason_code: "process_exit".into(),
            severity: match severity {
                Severity::Info => steve_store::diagnostics::Severity::Info,
                Severity::Warning => steve_store::diagnostics::Severity::Warning,
                Severity::Error => steve_store::diagnostics::Severity::Error,
            },
            details: d,
            ..Default::default()
        })
    }

    /// What committed transactions tell the business side, in commit order: the diagnostics,
    /// cards, attempts and turn ends of applied events, and the attempts and turns the Hub
    /// settled on its own word (a local cancel, an abandon the Node has no record of).
    pub(super) fn tell_later(&self) {
        let later = std::mem::take(&mut *self.later.lock().unwrap());
        for t in later {
            match t {
                Tell::Diagnostic(record) => {
                    let _ = self.store.diagnostics().record(*record);
                }
                Tell::After(a) => self.after(a),
                Tell::End(end) => self.tell_end(end),
                Tell::Stalled(evidence) => {
                    let reason = evidence.reason.as_str();
                    metrics::counter!("steve_orchestration_stalls_total", "reason" => reason)
                        .increment(1);
                    metrics::histogram!("steve_orchestration_stall_seconds", "reason" => reason)
                        .record(
                            evidence
                                .detected_at
                                .saturating_sub(evidence.first_at)
                                .max(0) as f64
                                / 1000.0,
                        );
                }
                Tell::StalledUsage(reason, tokens) => {
                    for (kind, count) in ["input", "cache_read", "cache_write", "output"]
                        .into_iter()
                        .zip(tokens)
                    {
                        if count > 0 {
                            metrics::counter!(
                                "steve_orchestration_stall_tokens_total",
                                "reason" => reason.as_str(),
                                "kind" => kind,
                            )
                            .increment(count as u64);
                        }
                    }
                }
                Tell::StalledAttemptStopped(reason, elapsed_ms) => {
                    metrics::histogram!(
                        "steve_orchestration_stall_stop_seconds",
                        "reason" => reason.as_str(),
                    )
                    .record(elapsed_ms as f64 / 1000.0);
                }
            }
        }
    }

    /// The Hub's side of an applied event or a settle, after its commit.
    fn after(&self, a: After) {
        match a {
            After::Asked(a) => self
                .domain
                .permission_asked(&a.session, &a.decision, &a.request),
            After::Started {
                session,
                attempt,
                ordinal,
                user_seq,
                at,
            } => self
                .domain
                .attempt_started(&session, &attempt, ordinal, user_seq, at),
            After::Finished {
                session,
                attempt,
                completion,
                facts,
                at,
                measure,
            } => {
                if let Some(m) = measure {
                    m.record();
                }
                self.domain
                    .attempt_finished(&session, &attempt, &completion, &facts, at)
            }
            After::Cost { session } => self.domain.cost_changed(&session),
        }
    }

    /// What an event means for the session row and for statistics, in `w` (before it is applied,
    /// so an attempt's start or end counts once): the context and cost it reports, the model in
    /// force, the tools and permissions of its attempt, and the attempts that begin and end.
    fn facts_in(&self, w: &mut W<'_>, ev: &NodeEvent) -> Result<Vec<After>, StoreError> {
        let Some(session) = ev_session(ev) else {
            return Ok(vec![]);
        };
        if let EventBody::Usage { report } = &ev.body {
            // the running cost of its harness process before this report, then with it
            let before = cost::mark(w, &self.node_id, ev, report)?;
            cost::set_mark(w, &self.node_id, ev, report)?;
            return self.usage_in(w, &session, ev, report, before);
        }
        let attempt = match &ev.subject {
            Subject::Execution(ExecutionRef::Turn { attempt, .. }) => Some(attempt.as_str()),
            _ => None,
        };
        let mut out = vec![];
        match (&ev.body, attempt) {
            (EventBody::ToolChanged { tool }, Some(a)) => {
                let mut k = facts_so_far(w, a)?;
                let prior = k.tools.get(tool.id.as_str()).map(|fact| fact.phase);
                if !matches!(prior, Some(ToolPhase::Succeeded | ToolPhase::Failed)) {
                    if prior != Some(tool.phase) {
                        self.observe_tool_in(w, a, tool, ev)?;
                    }
                    k.tools.insert(
                        tool.id.to_string(),
                        ToolFact {
                            name: tool.name.clone(),
                            category: tool.category,
                            phase: tool.phase,
                        },
                    );
                }
                w.attempts().keep_facts(a, &k)?;
            }
            (
                EventBody::WorkspaceChanged {
                    workspace,
                    revision,
                },
                Some(a),
            ) => {
                self.observe_workspace_in(w, a, workspace, revision)?;
            }
            (EventBody::PermissionRequested { .. }, Some(a)) => {
                let mut k = facts_so_far(w, a)?;
                k.permissions += 1;
                w.attempts().keep_facts(a, &k)?;
            }
            (EventBody::SessionChanged { view }, _) => {
                // keyed on the event's subject and this Node, never on the id the body gives
                if view.id.as_str() != session {
                    tracing::warn!(session, view = %view.id, "session view of another session");
                    return Ok(out);
                }
                // whether this view is from before the Hub's last open, before it is kept as seen
                let stale =
                    view.phase == SessionPhase::Released && self.stale_release_in(&session)?;
                // the Node's revision of the session, its resume handle, and the views since the
                // last open ([`Self::stale_release_in`])
                let resume = view.resume.as_ref().map(|r| r.to_string());
                w.runtime().seen_view(
                    &session,
                    self.node_id.as_str(),
                    view.revision.as_str(),
                    resume.as_deref(),
                )?;
                if view.phase == SessionPhase::Released && !stale {
                    if node_sessions::phase(w, &session, None)?.is_some_and(|phase| {
                        phase.name == "ready" && phase.active_attempt.is_none()
                    }) {
                        let rev = self.session_revision(&session)?;
                        node_write::session_cas_in(
                            w,
                            &session,
                            rev,
                            &rt::SessionUpdate {
                                phase: "released",
                                active_attempt: None,
                                release_pending: false,
                            },
                            now(),
                        )?;
                    }
                }
                // the resume handle is the harness's own session id (ACP `sessionId`), what ST-28
                // shows and the legacy runner keeps too
                observations::effective(w, &session, view, now())?;
            }
            (EventBody::AttemptStarted, Some(a)) => {
                if let Some(node_turns::Attempt { state, .. }) = self.attempt_row(a)?
                    && state == "planned"
                {
                    let ordinal = node_turns::ordinal(w, a)?;
                    node_sessions::set_status(w, &session, "running", now())?;
                    // The turn's first message follows the last completed turn.
                    let user_seq = observations::first_user_since_turn(w, &session)?;
                    super::super::acp_stats::try_node_attempt_started(
                        &self.store,
                        &session,
                        a,
                        ordinal,
                        user_seq.filter(|_| ordinal == 0),
                        ev.at.0,
                    )?;
                    out.push(After::Started {
                        session,
                        attempt: a.to_string(),
                        ordinal,
                        user_seq: user_seq.filter(|_| ordinal == 0),
                        at: ev.at.0,
                    });
                }
            }
            (EventBody::AttemptEnded { completion }, Some(a)) => {
                if let Some(node_turns::Attempt { state, .. }) = self.attempt_row(a)?
                    && state != "finished"
                {
                    let facts = take_facts(w, a)?;
                    let measure = super::super::acp_stats::try_node_attempt_finished(
                        &self.store,
                        &session,
                        a,
                        completion,
                        &facts,
                        ev.at.0,
                    )?;
                    out.push(After::Finished {
                        session,
                        attempt: a.to_string(),
                        completion: completion.clone(),
                        facts,
                        at: ev.at.0,
                        measure,
                    });
                }
            }
            _ => {}
        }
        Ok(out)
    }

    /// A usage report of a session: the context and cost it reports go to the session row and the
    /// statistics, its model and cost to its attempt's facts. `before`: the running cost its
    /// harness process reported earlier ([`cost::admit`]).
    fn usage_in(
        &self,
        w: &mut W<'_>,
        session: &str,
        ev: &NodeEvent,
        report: &UsageReport,
        before: Option<f64>,
    ) -> Result<Vec<After>, StoreError> {
        if report.scope == UsageScope::Background {
            return Ok(vec![]);
        }
        let attempt = match &ev.subject {
            Subject::Execution(ExecutionRef::Turn { attempt, .. }) => Some(attempt.as_str()),
            _ => None,
        };
        let mut kept = match attempt {
            Some(a) => facts_so_far(w, a)?,
            None => Kept::default(),
        };
        let caps = *self.cost_caps.lock().unwrap();
        let delta = cost::admit(caps, report, kept.cost.unwrap_or(0.0), before);
        if let Some(a) = attempt {
            let stall = self.stall_evidence_in(w, a)?;
            let tokens = super::super::acp_stats::node_usage(
                &self.store,
                session,
                a,
                &cost::accounting_report(report, delta),
                stall.is_some(),
            )?;
            if let (Some(stall), Some(tokens)) = (stall, tokens) {
                self.staged
                    .lock()
                    .unwrap()
                    .push(Tell::StalledUsage(stall.reason, tokens));
            }
            if report.model.is_some() {
                kept.model = report.model.clone();
            }
            if let Some(d) = delta {
                kept.cost = Some(kept.cost.unwrap_or(0.0) + d);
            }
            w.attempts().keep_facts(a, &kept)?;
        }
        observations::context(
            w,
            session,
            report.context_used.and_then(cost::context),
            report.context_limit.and_then(cost::context),
            delta,
            now(),
        )?;
        Ok(if delta.is_some_and(|d| d > 0.0) {
            vec![After::Cost {
                session: session.to_string(),
            }]
        } else {
            vec![]
        })
    }

    /// The last agent activity of a running attempt (a tool, a permission, a plan, an assistant's
    /// or a thought's message), by when the Hub received it: a workflow's evidence that it runs.
    fn activity_in(&self, w: &mut W<'_>, ev: &NodeEvent, received: i64) -> Result<(), StoreError> {
        let Subject::Execution(ExecutionRef::Turn {
            session, attempt, ..
        }) = &ev.subject
        else {
            return Ok(());
        };
        let agent = |role: Option<Role>| matches!(role, Some(Role::Assistant | Role::Thought));
        let active = match &ev.body {
            EventBody::ToolChanged { .. }
            | EventBody::PermissionRequested { .. }
            | EventBody::PermissionResolved { .. }
            | EventBody::Plan { .. } => true,
            EventBody::MessageStarted { role, .. } => agent(Some(*role)),
            EventBody::TextDelta { message, .. } => {
                agent(self.role_in(w, session.as_str(), message)?)
            }
            _ => false,
        };
        if active {
            w.attempts().touch(attempt.as_str(), received)?;
        }
        Ok(())
    }

    /// One event's effects in `w`. Returns a turn end to tell the business side after the commit.
    fn apply_in(&self, w: &mut W<'_>, ev: &NodeEvent) -> Result<Option<End>, StoreError> {
        if let Subject::Execution(ExecutionRef::Job { attempt, .. }) = &ev.subject {
            self.job_event_in(w, ev, attempt.as_str())?;
            return Ok(None);
        }
        let session = match &ev.subject {
            Subject::Session { session } => Some(session.to_string()),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => Some(session.to_string()),
            _ => None,
        };
        let mut end = None;
        match &ev.body {
            EventBody::OperationChanged { receipt } => end = self.receipt_in(w, receipt)?,
            EventBody::AttemptStarted => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some(node_turns::Attempt { revision: rev, state, .. }) = self.attempt_row(x.attempt().as_str())?
                    // planned -> running only: an attempt reconciled as unknown meanwhile stays
                    // unknown until it ends or is settled (Stop abandons it, never cancels)
                    && state == "planned"
                {
                    if node_write::attempt_cas_in(
                        w,
                        x.attempt().as_str(),
                        rev,
                        "running",
                        None,
                        None,
                        now(),
                    )? {
                        self.observe_attempt_start(w, x.attempt().as_str())?;
                    }
                }
            }
            EventBody::AttemptEnded { completion } => {
                let Subject::Execution(x) = &ev.subject else {
                    return Ok(None);
                };
                let attempt = x.attempt().as_str();
                let Some(node_turns::Attempt {
                    revision: rev,
                    state,
                    ..
                }) = self.attempt_row(attempt)?
                else {
                    return Ok(None);
                };
                if state == "finished" {
                    return Ok(None);
                }
                if let Some(stall) = self.stall_evidence_in(w, attempt)? {
                    let elapsed = self.clock.now().saturating_sub(stall.detected_at).max(0);
                    self.staged
                        .lock()
                        .unwrap()
                        .push(Tell::StalledAttemptStopped(stall.reason, elapsed));
                }
                node_write::attempt_cas_in(
                    w,
                    attempt,
                    rev,
                    "finished",
                    None,
                    Some(&serde_json::to_value(completion).map_err(conflict)?),
                    self.clock.now(),
                )?;
                self.close_cards_of_attempt_in(w, attempt)?;
                let (turn, s) = node_turns::for_attempt(w, attempt)?;
                // the receipt the Node gives this start_turn from now on (unless the Node's own
                // final receipt came first: terminal receipts are never rewritten)
                let op = self
                    .attempt_row(attempt)?
                    .ok_or_else(|| conflict("missing attempt"))?
                    .operation;
                let terminal = node_commands::state(w, self.node_id.as_str(), &op)?
                    .ok_or_else(|| conflict("missing start command"))?;
                let terminal = matches!(terminal.as_str(), "finished" | "rejected");
                if !terminal {
                    self.receipt_in(
                        w,
                        &Receipt {
                            operation: idv(op)?,
                            state: Observed::Finished {
                                outcome: completion.outcome,
                                result: Some(OperationResult::Execution(completion.clone())),
                                error: completion.error.clone(),
                            },
                        },
                    )?;
                }
                self.store_event_in(w, &s, ev)?;
                end = self.after_attempt_in(w, &s, &turn, completion)?;
                return Ok(end);
            }
            EventBody::Diagnostic { code, .. } if code == STEVE_TOOLS_MISSING => {
                if let Some(s) = &session {
                    tools_missing_in(w, s)?;
                }
            }
            _ => {}
        }
        if let Some(s) = session
            && !matches!(ev.body, EventBody::OperationChanged { .. })
        {
            self.store_event_in(w, &s, ev)?;
        }
        Ok(end)
    }

    /// The session's Steve event, once (the Node's stream position is its identity).
    fn store_event_in(
        &self,
        w: &mut W<'_>,
        session: &str,
        ev: &NodeEvent,
    ) -> Result<(), StoreError> {
        let role = match &ev.body {
            EventBody::TextDelta { message, .. } => self.role_in(w, session, message)?,
            EventBody::MessageStarted { message, role, .. } => {
                // a rolled-back start comes again with the same role: keeping it early is harmless
                self.remember_role(session, message, *role);
                None
            }
            _ => None,
        };
        let Some(entry) = steve_transcript::of_node(&ev.body, role) else {
            return Ok(());
        };
        w.events().add(
            session,
            &entry.kind,
            entry.data,
            Some(EventSource {
                node: self.node_id.as_str(),
                stream: ev.cursor.stream.as_str(),
                epoch: ev.cursor.epoch.as_str(),
                seq: ev.cursor.seq as i64,
                attempt: match &ev.subject {
                    Subject::Execution(x) => Some(x.attempt().as_str()),
                    _ => None,
                },
            }),
            ev.at.0,
        )?;
        Ok(())
    }

    /// The role of a message: from memory, else from its `message` row (by index, however long
    /// the message is).
    fn role_in(
        &self,
        w: &mut W<'_>,
        session: &str,
        message: &str,
    ) -> Result<Option<Role>, StoreError> {
        let key = (session.to_string(), message.to_string());
        if let Some(r) = self.roles.lock().unwrap().get(&key) {
            return Ok(Some(*r));
        }
        let role = observations::message_role(w, session, message)?;
        if let Some(r) = role {
            self.remember_role(session, message, r);
        }
        Ok(role)
    }

    fn remember_role(&self, session: &str, message: &str, role: Role) {
        let mut roles = self.roles.lock().unwrap();
        if roles.len() >= ROLES_KEPT {
            roles.clear();
        }
        roles.insert((session.to_string(), message.to_string()), role);
    }

    /// A new attempt requires a completed failure and explicit retry advice from its executor.
    pub(super) fn after_attempt_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        turn: &str,
        c: &Completion,
    ) -> Result<Option<End>, StoreError> {
        let (ordinal, cancelled) = self.store.read(|r| node_turns::retry_state(r, turn))?;
        if c.outcome == Outcome::Failed
            && !c.acted
            && !cancelled
            && let Some(Error {
                retry: Retry::Later { after_ms },
                ..
            }) = &c.error
            && let Some(delay) = RETRY_DELAYS_MS.get(ordinal as usize)
            && config::desired_in(&self.store, session)?.is_none()
            && self.workflow_blocker_in(session)?.is_none()
            && self
                .store
                .read(|r| node_turns::retry::allowed(r, session))?
        {
            let hint = after_ms.map_or(0, |n| i64::try_from(n).unwrap_or(i64::MAX));
            let at = self.clock.now().saturating_add((*delay).max(hint));
            if node_turns::retry::before_deadline(tx, turn, at)? {
                let (spec, _) = self.store.read(|r| node_turns::latest_spec(r, turn))?;
                let srev = self.session_revision(session)?;
                node_write::session_cas_in(
                    tx,
                    session,
                    srev,
                    &rt::SessionUpdate {
                        phase: "active",
                        active_attempt: None,
                        release_pending: false,
                    },
                    self.clock.now(),
                )?;
                self.attempt_in(tx, session, turn, ordinal + 1, spec.run.prompt)?;
                dispatch::schedule_turn_retry(tx, turn, at)?;
                return Ok(None);
            }
        }
        self.turn_over_in(tx, session, turn, c)?;
        // what is queued meanwhile goes next; only when nothing does is the turn over for the item
        if self.plan_after_in(tx, session)? == Planned::Created {
            return Ok(None);
        }
        // a session the business side ended is let go once its last turn is over (the release it
        // asked for while the turn ran waited for this)
        self.release_if_ended_in(tx, session)?;
        self.status_in(session, settled_status(&c.stop_reason))?;
        Ok(Some(End {
            session: session.to_string(),
            attempt: Some(self.last_attempt_in(turn)?),
            stop: c.stop_reason.clone(),
            error: c.error.as_ref().map(said),
        }))
    }

    /// The turn and its dispatch receipts are over; the session is free.
    pub(super) fn turn_over_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        turn: &str,
        c: &Completion,
    ) -> Result<(), StoreError> {
        let rev = self.turn_revision(turn)?;
        let ordinal = self.store.read(|r| node_turns::retry_ordinal(r, turn))?;
        node_write::turn_cas_in(
            tx,
            turn,
            rev,
            &rt::TurnUpdate {
                state: "finished",
                active_attempt: None,
                retry_ordinal: ordinal,
                applied: true,
            },
            now(),
        )?;
        node_write::dispatch_receipt_in(
            tx,
            turn,
            match c.outcome {
                Outcome::Succeeded => "completed",
                Outcome::Cancelled => "cancelled",
                Outcome::Interrupted | Outcome::TimedOut => "interrupted",
                Outcome::Failed => "error",
            },
            now(),
        )?;
        event_in(
            tx,
            session,
            "turn_end",
            json!({ "stopReason": c.stop_reason }),
        )?;
        // an abandoned attempt takes its harness with it: the Node has let the session go, so
        // the next message opens it again rather than starting a turn on a released session
        let abandoned = self.store.read(|r| dispatch::abandoned_turn(r, turn))?;
        let srev = self.session_revision(session)?;
        node_write::session_cas_in(
            tx,
            session,
            srev,
            &rt::SessionUpdate {
                phase: if abandoned { "released" } else { "ready" },
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        Ok(())
    }
}

/// Says once in the session's timeline that its agent ran without Steve's tools.
fn tools_missing_in(w: &mut W<'_>, session: &str) -> Result<(), StoreError> {
    let told = observations::has_system_code(w, session, STEVE_TOOLS_MISSING)?;
    if !told {
        let text = steve_i18n::stored(|| steve_i18n::t!("hub.node.steve_tools_missing"));
        w.events().add(
            session,
            "system",
            json!({ "text": text, "code": STEVE_TOOLS_MISSING }),
            None,
            now(),
        )?;
    }
    Ok(())
}

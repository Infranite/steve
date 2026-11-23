//! T4, applying: one Node event's effects (transcript projection, attempt/turn state transitions,
//! gateway retries, attempt statistics facts), and what the business side hears after the commit.

use super::*;
use steve_store::{EventSource, KeptFacts};

/// Retries of a turn after a passing gateway failure or a provider stream interruption.
const RETRY_DELAYS_MS: [i64; 2] = [20_000, 60_000];

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
        let tx = &w.legacy();
        // an attempt's end as the Hub reads it (a stream interruption it names otherwise)
        let ended;
        let ev = match (&ev.subject, &ev.body) {
            (
                Subject::Execution(ExecutionRef::Turn {
                    session, attempt, ..
                }),
                EventBody::AttemptEnded { completion },
            ) => {
                let completion = self.stream_completion_in(
                    tx,
                    session.as_str(),
                    attempt.as_str(),
                    completion.clone(),
                )?;
                ended = NodeEvent {
                    body: EventBody::AttemptEnded { completion },
                    ..ev.clone()
                };
                &ended
            }
            _ => ev,
        };
        let mut after: Vec<After> = self.permission_event_in(tx, ev)?.into_iter().collect();
        for a in &after {
            // in memory only, and before the card's commit is seen: the YOLO engine has the
            // tool's input from the first look at the card
            if let After::Asked(a) = a {
                self.domain
                    .permission_arming(&a.session, &a.decision, &a.request);
            }
        }
        after.extend(self.facts_in(w, tx, ev)?);
        self.realtime_in(w, ev, received)?;
        let end = self.apply_in(w, tx, ev)?;
        self.activity_in(w, tx, ev, received)?;
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
    fn facts_in(&self, w: &mut W<'_>, tx: &Tx, ev: &NodeEvent) -> Result<Vec<After>, StoreError> {
        let Some(session) = ev_session(ev) else {
            return Ok(vec![]);
        };
        if let EventBody::Usage { report } = &ev.body {
            // the running cost of its harness process before this report, then with it
            let before = cost::mark(w, &self.node_id, ev, report)?;
            cost::set_mark(w, &self.node_id, ev, report)?;
            return self.usage_in(w, tx, &session, ev, report, before);
        }
        let attempt = match &ev.subject {
            Subject::Execution(ExecutionRef::Turn { attempt, .. }) => Some(attempt.as_str()),
            _ => None,
        };
        let mut out = vec![];
        match (&ev.body, attempt) {
            (EventBody::ToolChanged { tool }, Some(a)) => {
                let mut k = facts_so_far(w, a)?;
                k.tools.insert(
                    tool.id.to_string(),
                    ToolFact {
                        name: tool.name.clone(),
                        category: tool.category,
                        phase: tool.phase,
                    },
                );
                w.attempts().keep_facts(a, &k)?;
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
                    view.phase == SessionPhase::Released && self.stale_release_in(tx, &session)?;
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
                    let (phase, active): (String, Option<String>) = tx.query_row(
                        "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
                        [&session],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    if phase == "ready" && active.is_none() {
                        let rev = self.session_revision(&session)?;
                        rt::session_cas_in(
                            tx,
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
                tx.execute(
                    "UPDATE sessions SET model = COALESCE(?, model), reasoning_effort = COALESCE(?, reasoning_effort), acp_session_id = COALESCE(?, acp_session_id), updated = ? WHERE id = ?",
                    rusqlite::params![
                        view.effective.model,
                        view.effective.effort,
                        view.resume.as_ref().map(|r| r.to_string()),
                        now(),
                        session
                    ],
                )?;
                node_sessions::changed_event(w, &session)?;
            }
            (EventBody::AttemptStarted, Some(a)) => {
                if let Some(node_turns::Attempt { state, .. }) = self.attempt_row(a)?
                    && state == "planned"
                {
                    let ordinal: i64 = tx.query_row(
                        "SELECT ordinal FROM runtime_attempts WHERE attempt_id=?",
                        [a],
                        |r| r.get(0),
                    )?;
                    self.status_in(&session, "running")?;
                    // the turn's first message: the first `user` entry since the last turn ended
                    let user_seq: Option<i64> = tx.query_row(
                        "SELECT MIN(seq) FROM events WHERE session_id=?1 AND kind='user' AND seq > (SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id=?1 AND kind='turn_end')",
                        [&session],
                        |r| r.get(0),
                    )?;
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
        tx: &Tx,
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
            super::super::acp_stats::node_usage(
                &self.store,
                session,
                a,
                &cost::accounting_report(report, delta),
            )?;
            if report.model.is_some() {
                kept.model = report.model.clone();
            }
            if let Some(d) = delta {
                kept.cost = Some(kept.cost.unwrap_or(0.0) + d);
            }
            w.attempts().keep_facts(a, &kept)?;
        }
        tx.execute(
            "UPDATE sessions SET used = COALESCE(?, used), size = COALESCE(?, size), cost = CASE WHEN ? IS NULL THEN cost ELSE COALESCE(cost, 0) + ? END, updated = ? WHERE id = ?",
            rusqlite::params![
                report.context_used.and_then(cost::context),
                report.context_limit.and_then(cost::context),
                delta,
                delta,
                now(),
                session
            ],
        )?;
        node_sessions::changed_event(w, session)?;
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
    fn activity_in(
        &self,
        w: &mut W<'_>,
        tx: &Tx,
        ev: &NodeEvent,
        received: i64,
    ) -> Result<(), StoreError> {
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
                agent(self.role_in(tx, session.as_str(), message)?)
            }
            _ => false,
        };
        if active {
            w.attempts().touch(attempt.as_str(), received)?;
        }
        Ok(())
    }

    /// One event's effects in `tx`. Returns a turn end to tell the business side after the commit.
    fn apply_in(&self, w: &mut W<'_>, tx: &Tx, ev: &NodeEvent) -> Result<Option<End>, StoreError> {
        if let Subject::Execution(ExecutionRef::Job { attempt, .. }) = &ev.subject {
            self.job_event_in(tx, ev, attempt.as_str())?;
            return Ok(None);
        }
        let session = match &ev.subject {
            Subject::Session { session } => Some(session.to_string()),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => Some(session.to_string()),
            _ => None,
        };
        let mut end = None;
        match &ev.body {
            EventBody::OperationChanged { receipt } => end = self.receipt_in(tx, receipt)?,
            EventBody::AttemptStarted => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some(node_turns::Attempt { revision: rev, state, .. }) = self.attempt_row(x.attempt().as_str())?
                    // planned -> running only: an attempt reconciled as unknown meanwhile stays
                    // unknown until it ends or is settled (Stop abandons it, never cancels)
                    && state == "planned"
                {
                    rt::attempt_cas_in(
                        tx,
                        x.attempt().as_str(),
                        rev,
                        "running",
                        None,
                        None,
                        now(),
                    )?;
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
                // abandoned by the Hub itself after the Node restarted, nobody having asked to stop
                // it: the core continues the agent (B6)
                let restarted = completion.stop_reason == "abandoned"
                    && tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM node_commands WHERE method='abandon' AND target_attempt=?1 AND json_extract(request,'$.body.reason')=?2) AND NOT EXISTS(SELECT 1 FROM node_commands WHERE method='abandon' AND target_attempt=?1 AND json_extract(request,'$.body.reason')<>?2) AND NOT (SELECT cancel_requested FROM runtime_attempts WHERE attempt_id=?1)",
                        [attempt, RESTART_ABANDON],
                        |r| r.get::<_, bool>(0),
                    )?;
                let mut completion = completion.clone();
                if restarted {
                    completion.stop_reason = steve_node_api::NODE_RESTARTED.into();
                }
                let completion = &completion;
                rt::attempt_cas_in(
                    tx,
                    attempt,
                    rev,
                    "finished",
                    None,
                    Some(&serde_json::to_value(completion).map_err(conflict)?),
                    self.clock.now(),
                )?;
                self.close_cards_of_attempt_in(tx, attempt)?;
                let (turn, s): (String, String) = tx.query_row(
                    "SELECT a.turn_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.attempt_id=?",
                    [attempt],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                // the receipt the Node gives this start_turn from now on (unless the Node's own
                // final receipt came first: terminal receipts are never rewritten)
                let op = self
                    .attempt_row(attempt)?
                    .ok_or_else(|| conflict("missing attempt"))?
                    .operation;
                let terminal: bool = tx.query_row(
                    "SELECT state IN ('finished','rejected') FROM node_commands WHERE operation_id=?",
                    [&op],
                    |r| r.get(0),
                )?;
                if !terminal {
                    self.receipt_in(
                        tx,
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
                self.store_event_in(w, tx, &s, ev)?;
                end = self.after_attempt_in(tx, &s, &turn, completion)?;
                return Ok(end);
            }
            EventBody::Diagnostic { code, .. } if code == STEVE_TOOLS_MISSING => {
                if let Some(s) = &session {
                    tools_missing_in(tx, s)?;
                }
            }
            _ => {}
        }
        if let Some(s) = session
            && !matches!(ev.body, EventBody::OperationChanged { .. })
        {
            self.store_event_in(w, tx, &s, ev)?;
        }
        Ok(end)
    }

    /// The session's Steve event, once (the Node's stream position is its identity).
    fn store_event_in(
        &self,
        w: &mut W<'_>,
        tx: &Tx,
        session: &str,
        ev: &NodeEvent,
    ) -> Result<(), StoreError> {
        let role = match &ev.body {
            EventBody::TextDelta { message, .. } => self.role_in(tx, session, message)?,
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
    fn role_in(&self, tx: &Tx, session: &str, message: &str) -> Result<Option<Role>, StoreError> {
        let key = (session.to_string(), message.to_string());
        if let Some(r) = self.roles.lock().unwrap().get(&key) {
            return Ok(Some(*r));
        }
        let role = role_of_in(tx, session, message)?;
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

    /// An attempt is over: the Hub decides. Passing gateway, stream and capacity failures retry
    /// as new attempts of the same turn. Service failures resume safely even after tools ran.
    pub(super) fn after_attempt_in(
        &self,
        tx: &Tx,
        session: &str,
        turn: &str,
        c: &Completion,
    ) -> Result<Option<End>, StoreError> {
        let (ordinal, cancelled): (i64, bool) = tx.query_row(
            "SELECT t.retry_ordinal, EXISTS(SELECT 1 FROM runtime_attempts WHERE turn_id=t.turn_id AND cancel_requested=1) FROM runtime_turns t WHERE t.turn_id=?",
            [turn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let stream = c.stop_reason == stream_retry::STREAM_INTERRUPTED;
        let capacity = c.stop_reason == capacity_retry::UNAVAILABLE;
        let service = stream || capacity;
        let capacity_wait = if capacity {
            self.capacity_wait_in(tx, turn, ordinal, c)?
        } else {
            None
        };
        let retry = c.outcome == Outcome::Failed
            && ((service && self.stream_retry_allowed_in(tx, session)?)
                || (!service && c.transient && !c.acted))
            && !cancelled
            && self.workflow_blocker_in(tx, session)?.is_none()
            && if capacity {
                capacity_wait.is_some()
            } else {
                (ordinal as usize) < RETRY_DELAYS_MS.len()
            };
        // a retry carries the config of its first attempt: never once a change is waiting
        let waits = retry && config::desired_in(&self.store, session)?.is_some();
        if waits {
            event_in(
                tx,
                session,
                "system",
                json!({ "text": "配置变更待生效，本次失败不自动重试" }),
            )?;
        }
        if retry && !waits {
            // the same messages, a new attempt (its own id and command), after a wait
            let spec: String = tx.query_row(
                "SELECT spec FROM runtime_attempts WHERE turn_id=? ORDER BY ordinal DESC LIMIT 1",
                [turn],
                |r| r.get(0),
            )?;
            let spec: TurnSpec = serde_json::from_str(&spec)?;
            let srev = self.session_revision(session)?;
            // the slot is kept by the turn: the attempt is freed only to be replaced
            rt::session_cas_in(
                tx,
                session,
                srev,
                &rt::SessionUpdate {
                    phase: "active",
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )?;
            let prompt = if capacity {
                capacity_retry::capacity_retry_prompt(
                    c.error
                        .as_ref()
                        .map(|e| e.safe_detail.as_str())
                        .unwrap_or("service unavailable"),
                    ordinal + 1,
                )
            } else if stream {
                stream_retry::stream_retry_prompt(
                    c.error
                        .as_ref()
                        .map(|e| e.safe_detail.as_str())
                        .unwrap_or("stream interrupted"),
                    ordinal + 1,
                )
            } else {
                spec.run.prompt
            };
            self.attempt_in(tx, session, turn, ordinal + 1, prompt)?;
            let at = capacity_wait
                .as_ref()
                .map(|w| w.at)
                .unwrap_or_else(|| self.clock.now() + RETRY_DELAYS_MS[ordinal as usize]);
            let delay = at - self.clock.now();
            tx.execute(
                "UPDATE node_commands SET retry_at=? WHERE target_attempt=(SELECT active_attempt FROM runtime_turns WHERE turn_id=?) AND method='start_turn'",
                rusqlite::params![at, turn],
            )?;
            if let Some(w) = &capacity_wait {
                tx.execute(
                    "UPDATE node_commands SET receipt=json_set(COALESCE(receipt,'{}'),'$.provider_capacity',json(?1)) WHERE target_attempt=(SELECT active_attempt FROM runtime_turns WHERE turn_id=?2) AND method='start_turn'",
                    rusqlite::params![json!({"deadline":w.deadline,"error":w.error,"retries":w.retries}).to_string(),turn],
                )?;
            }
            let text = if capacity {
                let error = c
                    .error
                    .as_ref()
                    .map(|e| e.safe_detail.as_str())
                    .unwrap_or_default();
                if capacity_wait.as_ref().is_some_and(|w| w.at == w.deadline) {
                    format!(
                        "模型服务容量不足，预计恢复时间超出本轮等待上限；等待至 15 分钟上限后提示处理，期间不提前请求；{error}"
                    )
                } else {
                    format!(
                        "模型服务容量不足，{} 秒后自动重试（第 {} 次，上限 3 次 / 15 分钟）；{error}",
                        (delay + 999) / 1000,
                        ordinal + 1
                    )
                }
            } else {
                format!(
                    "{}，{} 秒后自动重试（第 {} 次）",
                    if stream {
                        "模型服务流中断"
                    } else {
                        "推理网关暂时不可用"
                    },
                    delay / 1000,
                    ordinal + 1
                )
            };
            event_in(tx, session, "system", json!({ "text": text }))?;
            return Ok(None);
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
            error: c.error.as_ref().map(|e| {
                if capacity && ordinal >= capacity_retry::RETRIES {
                    format!("已自动重试 {ordinal} 次仍失败，已达次数上限；{}", said(e))
                } else if capacity && capacity_wait.is_none() {
                    format!("等待已达 15 分钟上限；{}", said(e))
                } else if stream && ordinal as usize >= RETRY_DELAYS_MS.len() {
                    format!("已自动重试 {ordinal} 次仍失败：{}", said(e))
                } else {
                    said(e)
                }
            }),
        }))
    }

    /// The turn and its dispatch receipts are over; the session is free.
    pub(super) fn turn_over_in(
        &self,
        tx: &Tx,
        session: &str,
        turn: &str,
        c: &Completion,
    ) -> Result<(), StoreError> {
        let rev = self.turn_revision(turn)?;
        let ordinal: i64 = tx.query_row(
            "SELECT retry_ordinal FROM runtime_turns WHERE turn_id=?",
            [turn],
            |r| r.get(0),
        )?;
        rt::turn_cas_in(
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
        rt::dispatch_receipt_in(
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
        let abandoned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM node_commands c JOIN runtime_attempts a ON a.attempt_id = c.target_attempt WHERE a.turn_id=? AND c.method='abandon')",
            [turn],
            |r| r.get(0),
        )?;
        let srev = self.session_revision(session)?;
        rt::session_cas_in(
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

/// The `message` row that began a message (migration 044's index `events_message_start`).
const ROLE_SQL: &str = "SELECT json_extract(data,'$.role') FROM events WHERE session_id=?1 AND kind='message' AND format='steve.v1' AND json_extract(data,'$.message')=?2 ORDER BY seq DESC LIMIT 1";

/// The role of a message, from the `message` entry that began it.
fn role_of_in(tx: &Tx, session: &str, message: &str) -> Result<Option<Role>, StoreError> {
    Ok(role_lookup_in(tx, session, message)?.0)
}

/// [`role_of_in`] and the SQLite VM steps it took (tests check they do not grow with the message).
pub(super) fn role_lookup_in(
    tx: &Tx,
    session: &str,
    message: &str,
) -> Result<(Option<Role>, i32), StoreError> {
    let mut st = tx.prepare_cached(ROLE_SQL)?;
    st.reset_status(rusqlite::StatementStatus::VmStep);
    let r: Option<Option<String>> = st.query_row([session, message], |r| r.get(0)).optional()?;
    let steps = st.get_status(rusqlite::StatementStatus::VmStep);
    Ok((
        r.flatten()
            .and_then(|r| serde_json::from_value(Value::String(r)).ok()),
        steps,
    ))
}

/// Says once in the session's timeline that its agent ran without Steve's tools.
fn tools_missing_in(tx: &Tx, session: &str) -> Result<(), StoreError> {
    let told: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND kind='system' AND json_extract(data,'$.code')=?2)",
        rusqlite::params![session, STEVE_TOOLS_MISSING],
        |r| r.get(0),
    )?;
    if !told {
        let text = steve_i18n::stored(|| steve_i18n::t!("hub.node.steve_tools_missing"));
        event_in(
            tx,
            session,
            "system",
            json!({ "text": text, "code": STEVE_TOOLS_MISSING }),
        )?;
    }
    Ok(())
}

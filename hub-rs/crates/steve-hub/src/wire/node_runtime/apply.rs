//! T4, applying: ordered stream runs, transcript projection, attempt/turn state transitions,
//! gateway retries and attempt statistics facts.

use super::*;

/// Retries of a turn after a passing failure (gateway 5xx) with no tool run, and their waits.
const RETRY_DELAYS_MS: [i64; 2] = [20_000, 60_000];

/// The most events of one stream applied in one transaction ([`NodeRuntime::apply_some`]).
const APPLY_RUN: usize = 128;
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
    /// The attempt's usage reports (main scope), in order.
    pub usage: Vec<UsageReport>,
    pub model: Option<String>,
    /// The attempt's share of the harness's own dollar figure, if it reported one.
    pub cost: Option<f64>,
}

impl NodeRuntime {
    // ── T4: applying ──

    /// Applies what is ready on every dirty stream. False: nothing was.
    ///
    /// A stream's events go in runs of up to [`APPLY_RUN`] per transaction, the streams taking
    /// turns, so one long message neither costs a commit per delta nor keeps the other sessions
    /// (and their permission cards) waiting. A run is the stream's flow (text, tools, plans) and
    /// ends with the first other event, or the first the Hub does something about after its
    /// commit (statistics, YOLO, the turn's end, a settle): what follows an event's commit still
    /// follows that event's own commit, in order. A run that fails is rolled back and its stream
    /// goes one event per transaction for the rest of the pass, so the events before a bad one are
    /// still applied and the bad one stops its stream as before.
    pub(super) fn apply_some(&self) -> bool {
        self.apply_some_observed(|| {})
    }

    /// The observer runs after an empty inbox observation releases the commit lock. Tests receive
    /// another event at that exact boundary; ordinary workers use the no-op observer above.
    pub(super) fn apply_some_observed(&self, mut empty_observed: impl FnMut()) -> bool {
        let mut streams: std::collections::VecDeque<(String, String)> =
            self.dirty.lock().unwrap().iter().cloned().collect();
        let node = self.node_id.to_string();
        let mut any = false;
        let mut one_by_one: HashSet<(String, String)> = HashSet::new();
        while let Some((stream, epoch)) = streams.pop_front() {
            let k = StreamKey {
                node: &node,
                stream: &stream,
                epoch: &epoch,
            };
            let key = (stream.clone(), epoch.clone());
            let limit = if one_by_one.contains(&key) {
                1
            } else {
                APPLY_RUN
            };
            // turn ends settled outside this worker (a local cancel) before this run's commit are
            // told before its events' ends: taken under the write lock, so every one comes from a
            // transaction that is over, and the item hears its turns in commit order
            let mut earlier = (vec![], vec![]);
            let r = self.tx(|tx| {
                earlier = self.take_settled();
                let pending = rt::next_events_in(tx, k, limit)?;
                let Some(last) = pending.last().map(|p| p.seq) else {
                    // The empty observation and retiring its dirty marker share receive()'s commit
                    // lock. Otherwise a receiver can commit/mark new events between this read and
                    // an out-of-transaction remove, losing the only wakeup for that durable batch.
                    self.dirty.lock().unwrap().remove(&key);
                    return Ok(None);
                };
                let mut told = Vec::with_capacity(pending.len());
                let mut through = last;
                for p in pending {
                    let ev: NodeEvent = serde_json::from_value(p.payload)?;
                    let mut ask: Vec<After> =
                        self.permission_event_in(tx, &ev)?.into_iter().collect();
                    for a in &ask {
                        // in memory only, and before the card's commit is seen: the YOLO
                        // engine has the tool's input from the first look at the card
                        if let After::Asked(a) = a {
                            self.domain
                                .permission_arming(&a.session, &a.decision, &a.request);
                        }
                    }
                    ask.extend(self.facts_in(tx, &ev)?);
                    let out = self.apply_in(tx, &ev)?;
                    // only the stream's flow (text, tools, plans) runs on; anything else (an
                    // attempt's end, a card, a receipt) is the last of its run
                    let flow = matches!(
                        ev.body,
                        EventBody::TextDelta { .. }
                            | EventBody::MessageStarted { .. }
                            | EventBody::MessageCompleted { .. }
                            | EventBody::ToolChanged { .. }
                            | EventBody::Plan { .. }
                    );
                    let tells = out.is_some() || !ask.is_empty() || self.staged_any();
                    let diagnostic = match &ev.body {
                        EventBody::Diagnostic {
                            code,
                            severity,
                            details: Some(details),
                            ..
                        } if code == "adapter_exited" => ev_session(&ev).map(|session| {
                            let mut d = steve_store::diagnostics::Details {
                                exit_code: details.exit_code,
                                signal: details.signal,
                                generation: details.generation,
                                parse_errors: Some(details.parse_errors),
                                drain_timed_out: Some(details.drain_timed_out),
                                reader_failed: Some(details.reader_failed),
                                wait_error: Some(details.wait_error),
                                source_at: Some(ev.at.0),
                                source_node: Some(node.clone()),
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
                            steve_store::diagnostics::Record {
                                session_id: Some(session),
                                cause_id: Some(ev.id.to_string()),
                                kind: "adapter.exited".into(),
                                reason_code: "process_exit".into(),
                                severity: match severity {
                                    Severity::Info => steve_store::diagnostics::Severity::Info,
                                    Severity::Warning => {
                                        steve_store::diagnostics::Severity::Warning
                                    }
                                    Severity::Error => steve_store::diagnostics::Severity::Error,
                                },
                                details: d,
                                ..Default::default()
                            }
                        }),
                        _ => None,
                    };
                    told.push((out, ask, diagnostic));
                    if tells || !flow {
                        through = p.seq;
                        break;
                    }
                }
                rt::mark_applied_through_in(tx, k, through)?;
                Ok(Some(told))
            });
            if matches!(&r, Ok(None)) {
                empty_observed();
            }
            self.book(earlier);
            match r {
                Ok(Some(told)) => {
                    any = true;
                    for (end, ask, diagnostic) in told {
                        if let Some(record) = diagnostic {
                            let _ = self.store.diagnostics().record(record);
                        }
                        self.after(ask);
                        if let Some(end) = end {
                            self.tell_end(end);
                        }
                    }
                    // its turn again once the others had theirs
                    streams.push_back(key);
                }
                Ok(None) => {
                    self.caught_up_on(&stream, &epoch);
                }
                Err(e) if limit > 1 => {
                    tracing::debug!("apply {stream}: {e}; one event at a time");
                    one_by_one.insert(key.clone());
                    streams.push_front(key);
                }
                Err(e) => {
                    tracing::error!("apply {stream}: {e}");
                }
            }
        }
        self.book_settled();
        if any {
            self.wake_send.notify_one();
        }
        any
    }

    /// The open transaction settled something the apply worker tells after its commit.
    fn staged_any(&self) -> bool {
        let (settled, ended) = &*self.staged.lock().unwrap();
        !settled.is_empty()
            || !ended.is_empty()
            || !self.staged_workspace.lock().unwrap().is_empty()
    }

    /// What the Hub settled on its own word (a local cancel, an abandon the Node has no record
    /// of), after the commit: the attempts' statistics rows end, then their turns are told.
    /// Only committed settles are queued here ([`Self::tx`]), so each is booked as it is.
    pub(super) fn book_settled(&self) {
        self.book(self.take_settled());
    }

    /// The committed settles queued so far ([`Self::book_settled`]).
    fn take_settled(&self) -> (Vec<After>, Vec<End>) {
        (
            std::mem::take(&mut *self.settled_later.lock().unwrap()),
            std::mem::take(&mut *self.ended_later.lock().unwrap()),
        )
    }

    fn book(&self, (settled, ended): (Vec<After>, Vec<End>)) {
        self.after(settled);
        for end in ended {
            self.tell_end(end);
        }
    }

    /// The Hub's side of applied events, after their commit.
    fn after(&self, asked: Vec<After>) {
        for a in asked {
            match a {
                After::Asked(a) => {
                    self.domain
                        .permission_asked(&a.session, &a.decision, &a.request)
                }
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
                    at,
                } => {
                    let facts = self.attempt_facts(&session, &attempt);
                    self.domain
                        .attempt_finished(&session, &attempt, &completion, &facts, at);
                }
                After::Cost { session } => self.domain.cost_changed(&session),
            }
        }
    }

    /// What an event means for the session row and for statistics, in `tx` (before it is applied,
    /// so an attempt's start or end counts once): the context and cost it reports, the model in
    /// force, and the attempts that begin and end.
    fn facts_in(&self, tx: &Tx, ev: &NodeEvent) -> Result<Vec<After>, StoreError> {
        let Some(session) = ev_session(ev) else {
            return Ok(vec![]);
        };
        let mut out = vec![];
        match &ev.body {
            EventBody::Usage { report } if report.scope != UsageScope::Background => {
                let delta = self.cost_in(tx, ev, report)?;
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
                tx.publish(steve_store::ChangeEvent::Session {
                    id: session.clone(),
                    item_id: None,
                });
                if delta.is_some_and(|d| d > 0.0) {
                    out.push(After::Cost { session });
                }
            }
            EventBody::SessionChanged { view } => {
                // the Node's revision of the session, kept here (the inbox it came in is pruned)
                // keyed on the event's subject and this Node, never on the id the body gives
                if view.id.as_str() != session {
                    tracing::warn!(session, view = %view.id, "session view of another session");
                    return Ok(out);
                }
                tx.execute(
                    "UPDATE runtime_sessions SET node_revision=? WHERE session_id=? AND node_id=?",
                    [
                        view.revision.as_str(),
                        session.as_str(),
                        self.node_id.as_str(),
                    ],
                )?;
                if view.phase == SessionPhase::Released
                    && !self.stale_release_in(tx, &session, ev)?
                {
                    let (phase, active): (String, Option<String>) = tx.query_row(
                        "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
                        [&session],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    if phase == "ready" && active.is_none() {
                        let rev = session_revision(tx, &session)?;
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
                tx.publish(steve_store::ChangeEvent::Session {
                    id: session,
                    item_id: None,
                });
            }
            EventBody::AttemptStarted => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some((_, state)) = attempt_row(tx, x.attempt().as_str())?
                    && state == "planned"
                {
                    let ordinal: i64 = tx.query_row(
                        "SELECT ordinal FROM runtime_attempts WHERE attempt_id=?",
                        [x.attempt().as_str()],
                        |r| r.get(0),
                    )?;
                    status_in(tx, &session, "running")?;
                    // the turn's first message: the first `user` entry since the last turn ended
                    let user_seq: Option<i64> = tx.query_row(
                        "SELECT MIN(seq) FROM events WHERE session_id=?1 AND kind='user' AND seq > (SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id=?1 AND kind='turn_end')",
                        [&session],
                        |r| r.get(0),
                    )?;
                    out.push(After::Started {
                        session,
                        attempt: x.attempt().to_string(),
                        ordinal,
                        user_seq: user_seq.filter(|_| ordinal == 0),
                        at: ev.at.0,
                    });
                }
            }
            EventBody::AttemptEnded { completion } => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some((_, state)) = attempt_row(tx, x.attempt().as_str())?
                    && state != "finished"
                {
                    out.push(After::Finished {
                        session,
                        attempt: x.attempt().to_string(),
                        completion: completion.clone(),
                        at: ev.at.0,
                    });
                }
            }
            _ => {}
        }
        Ok(out)
    }

    /// What usage report `report` (of `ev`, being applied) adds to its session's cost: checked as
    /// [`cost::Meter`] checks it, after the reports of the same attempt applied before it (their
    /// caps hold across them). `None`: nothing.
    fn cost_in(
        &self,
        tx: &Tx,
        ev: &NodeEvent,
        report: &UsageReport,
    ) -> Result<Option<f64>, StoreError> {
        let mut meter = cost::Meter::new(*self.cost_caps.lock().unwrap());
        if let Subject::Execution(x) = &ev.subject
            && let Some(session) = exec_session(x)
        {
            let earlier: Vec<String> = {
                let mut st = tx.prepare_cached(
                    "SELECT payload FROM node_event_inbox WHERE node_id=? AND applied=1 AND json_extract(payload,'$.body.type')='usage'
                      AND json_extract(payload,'$.subject.attempt')=? AND json_extract(payload,'$.subject.session')=?
                      AND NOT (stream=? AND epoch=? AND seq=?) ORDER BY stream, seq",
                )?;
                st.query_map(
                    rusqlite::params![
                        self.node_id.as_str(),
                        x.attempt().as_str(),
                        session,
                        ev.cursor.stream.as_str(),
                        ev.cursor.epoch.as_str(),
                        ev.cursor.seq as i64
                    ],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
            };
            for r in earlier {
                let Ok(e) = serde_json::from_str::<NodeEvent>(&r) else {
                    continue;
                };
                if let EventBody::Usage { report } = &e.body {
                    meter.add(report, || {
                        cost::earlier_cost_in(tx, &self.node_id, &e, &report.meter_epoch)
                    })?;
                }
            }
        }
        meter.add(report, || {
            cost::earlier_cost_in(tx, &self.node_id, ev, &report.meter_epoch)
        })
    }

    /// What one attempt did, for its statistics row: from the Node's own events of it.
    fn attempt_facts(&self, session: &str, attempt: &str) -> AttemptFacts {
        self.store
            .reader(|c| self.attempt_facts_on(c, session, attempt))
            .unwrap_or_else(|e| {
                tracing::error!("facts of attempt {attempt}: {e}");
                AttemptFacts::default()
            })
    }

    fn attempt_facts_on(
        &self,
        conn: &rusqlite::Connection,
        session: &str,
        attempt: &str,
    ) -> Result<AttemptFacts, StoreError> {
        let rows: Vec<String> = {
            let mut st = conn.prepare(
                "SELECT payload FROM node_event_inbox WHERE node_id=? AND json_extract(payload,'$.subject.attempt')=? AND json_extract(payload,'$.subject.session')=? ORDER BY stream, seq",
            )?;
            st.query_map(
                rusqlite::params![self.node_id.as_str(), attempt, session],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        let mut f = AttemptFacts::default();
        let mut tools: std::collections::HashMap<String, ToolView> = Default::default();
        let mut meter = cost::Meter::new(*self.cost_caps.lock().unwrap());
        for r in rows {
            let Ok(ev) = serde_json::from_str::<NodeEvent>(&r) else {
                continue;
            };
            if let EventBody::Usage { report } = &ev.body
                && let Some(delta) = meter.add(report, || {
                    cost::earlier_cost_in(conn, &self.node_id, &ev, &report.meter_epoch)
                })?
            {
                f.cost = Some(f.cost.unwrap_or(0.0) + delta);
            }
            match ev.body {
                EventBody::ToolChanged { tool } => {
                    tools.insert(tool.id.to_string(), tool);
                }
                EventBody::PermissionRequested { .. } => f.permissions += 1,
                EventBody::Usage { report } if report.scope != UsageScope::Background => {
                    if report.model.is_some() {
                        f.model = report.model.clone();
                    }
                    f.usage.push(report);
                }
                _ => {}
            }
        }
        for t in tools.values() {
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
        Ok(f)
    }

    /// One event's effects in `tx`. Returns a turn end to tell the business side after the commit.
    fn apply_in(&self, tx: &Tx, ev: &NodeEvent) -> Result<Option<End>, StoreError> {
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
                    && let Some((rev, state)) = attempt_row(tx, x.attempt().as_str())?
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
                let Some((rev, state)) = attempt_row(tx, attempt)? else {
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
                    now(),
                )?;
                self.close_cards_of_attempt_in(tx, attempt)?;
                let (turn, s): (String, String) = tx.query_row(
                    "SELECT a.turn_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.attempt_id=?",
                    [attempt],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                // the receipt the Node gives this start_turn from now on (unless the Node's own
                // final receipt came first: terminal receipts are never rewritten)
                let op = op_of_attempt(tx, attempt)?;
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
                self.store_event_in(tx, &s, ev)?;
                end = self.after_attempt_in(tx, &s, &turn, completion)?;
                return Ok(end);
            }
            _ => {}
        }
        if let Some(s) = session
            && !matches!(ev.body, EventBody::OperationChanged { .. })
        {
            self.store_event_in(tx, &s, ev)?;
        }
        Ok(end)
    }

    /// The session's Steve event, once (the Node's stream position is its identity).
    fn store_event_in(&self, tx: &Tx, session: &str, ev: &NodeEvent) -> Result<(), StoreError> {
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
        rt::add_event_in(
            tx,
            session,
            &entry.kind,
            entry.data,
            V1,
            Some(rt::EventSource {
                node: self.node_id.as_str(),
                stream: ev.cursor.stream.as_str(),
                epoch: ev.cursor.epoch.as_str(),
                seq: ev.cursor.seq as i64,
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

    /// An attempt is over: the Hub decides. A passing failure with no tool run is tried again as a
    /// new attempt of the same turn (after a wait); otherwise the turn is over.
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
        let retry = c.outcome == Outcome::Failed
            && c.transient
            && !c.acted
            && !cancelled
            && steve_store::workflow_gate::session_blocker(tx, session)?.is_none()
            && (ordinal as usize) < RETRY_DELAYS_MS.len();
        // a retry carries the config of its first attempt: never once a change is waiting
        let waits = retry && config::desired_in(tx, session)?.is_some();
        if waits {
            rt::add_event_in(
                tx,
                session,
                "system",
                json!({ "text": "配置变更待生效，本次失败不自动重试" }),
                V1,
                None,
                now(),
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
            let srev = session_revision(tx, session)?;
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
            self.attempt_in(tx, session, turn, ordinal + 1, spec.run.prompt)?;
            let delay = RETRY_DELAYS_MS[ordinal as usize];
            tx.execute(
                "UPDATE node_commands SET retry_at=? WHERE target_attempt=(SELECT active_attempt FROM runtime_turns WHERE turn_id=?) AND method='start_turn'",
                rusqlite::params![now() + delay, turn],
            )?;
            let text = format!(
                "推理网关暂时不可用，{} 秒后自动重试（第 {} 次）",
                delay / 1000,
                ordinal + 1
            );
            rt::add_event_in(
                tx,
                session,
                "system",
                json!({ "text": text }),
                V1,
                None,
                now(),
            )?;
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
        status_in(tx, session, settled_status(&c.stop_reason))?;
        Ok(Some(End {
            session: session.to_string(),
            attempt: Some(last_attempt_in(tx, turn)?),
            stop: c.stop_reason.clone(),
            error: c.error.as_ref().map(said),
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
        let rev = turn_revision(tx, turn)?;
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
        rt::add_event_in(
            tx,
            session,
            "turn_end",
            json!({ "stopReason": c.stop_reason }),
            V1,
            None,
            now(),
        )?;
        // an abandoned attempt takes its harness with it: the Node has let the session go, so
        // the next message opens it again rather than starting a turn on a released session
        let abandoned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM node_commands c JOIN runtime_attempts a ON a.attempt_id = c.target_attempt WHERE a.turn_id=? AND c.method='abandon')",
            [turn],
            |r| r.get(0),
        )?;
        let srev = session_revision(tx, session)?;
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

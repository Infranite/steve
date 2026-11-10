//! Startup and restart reconciliation with the Node: stream catch-up, ledger comparison,
//! released sessions, orphaned permission cards and stranger Node work (D7).

use super::*;

impl NodeRuntime {
    // ── reconciling with the Node (Hub start) ──

    /// Whether turns may be planned: only once the Hub has reconciled with the Node.
    pub(super) fn may_plan(&self) -> bool {
        self.reconciled.load(Ordering::SeqCst)
    }

    /// Whether the Hub has reconciled with its Node since it started (turns are planned).
    pub fn is_reconciled(&self) -> bool {
        self.may_plan()
    }

    /// The Node restarted (a new incarnation) while this Hub ran: nothing new is planned until the
    /// Hub has reconciled with it again (what it cannot tell the end of becomes `unknown`, never
    /// re-run); then what waited in the queue goes.
    pub async fn reconcile_again(self: &Arc<Self>) -> ApiResult<usize> {
        self.reconciled.store(false, Ordering::SeqCst);
        let n = self.reconcile().await?;
        if n > 0 {
            tracing::info!(
                "node {}: reconciled after its restart, {n} changed",
                self.node_id
            );
        }
        self.reconciled.store(true, Ordering::SeqCst);
        let me = self.clone();
        let _ = tokio::task::spawn_blocking(move || me.plan_waiting()).await;
        Ok(n)
    }

    /// What waited while the Hub was not reconciled goes now: queued messages, and a desired
    /// config whose idle boundary (a turn's end applied during reconcile) found nothing planned.
    pub(super) fn plan_waiting(&self) {
        let sessions: Vec<String> = self
            .store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT s.session_id FROM runtime_sessions s WHERE s.node_id=?
                       AND (s.desired_config IS NOT NULL OR EXISTS(SELECT 1 FROM queued q WHERE q.session_id=s.session_id))",
                )?;
                let v = st
                    .query_map([self.node_id.as_str()], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap_or_default();
        for s in sessions {
            if let Err(e) = self.tx(|tx| self.plan_after_in(tx, &s)) {
                tracing::error!("plan {s} after reconciling: {e}");
            }
        }
        self.wake_send.notify_one();
    }

    /// Compares the Hub's view with what the Node holds now (its ledger and stream heads as of one
    /// moment, its sessions, its waiting permissions), before anything is sent. First the Hub's
    /// inbox is brought up to the Node's heads and applied ([`catch_up_streams`]: replayed what it
    /// lacks, faulted what cannot be reconciled). The Node may have restarted meanwhile (a local
    /// Node restarts with the Hub): what it cannot tell the end of becomes `unknown` (never sent
    /// again; a person gives it up), a session it let go is `released` (the next message opens it
    /// again), a card nobody waits for any more expires, and a session with Node work the Hub has
    /// no record of is held with a note (that work is never given up on its own). Returns how many
    /// things changed.
    pub async fn reconcile(&self) -> ApiResult<usize> {
        let internal = |e: StoreError| ApiError::new(ErrorCode::Internal, e.to_string());
        // the Node's ledger, all pages of one snapshot
        let NodeSnapshot { entries, heads } = read_snapshot(&*self.node, 200).await?;
        // its events up to that moment, applied before the ledger is compared
        let caught = self.catch_up(&heads).await?;
        if caught.replayed > 0 || caught.faulted > 0 {
            tracing::info!(
                "node {}: reconcile replayed {} events, faulted {} streams",
                self.node_id,
                caught.replayed,
                caught.faulted
            );
        }
        // the sessions as the Node holds them (None: it has no such session)
        let sessions: Vec<String> = self
            .store
            .reader(|c| {
                let mut st =
                    c.prepare("SELECT session_id FROM runtime_sessions WHERE node_id=?")?;
                let v = st
                    .query_map([self.node_id.as_str()], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .map_err(internal)?;
        let mut views: Vec<(String, Option<SessionView>)> = vec![];
        for s in sessions {
            let id: SessionId = match SessionId::try_from(s.clone()) {
                Ok(x) => x,
                Err(_) => continue,
            };
            match self.node.session(id).await {
                Ok(v) => views.push((s, Some(v))),
                Err(e) if e.code == ErrorCode::Invalid => views.push((s, None)),
                Err(e) => return Err(e),
            }
        }
        let waiting: HashSet<String> = entries
            .values()
            .flat_map(|e| e.pending.iter().map(|r| r.id.to_string()))
            .collect();
        let mut ends: Vec<End> = vec![];
        let changed = self
            .tx(|tx| {
                let mut changed = 0;
                // 1. commands: what the Node says of each one in flight
                let cmds: Vec<(String, String, String, String)> = {
                    let mut st = tx.prepare(
                        "SELECT operation_id, state, method, request FROM node_commands WHERE node_id=? AND state IN ('pending','accepted','active','unknown') ORDER BY created, rowid",
                    )?;
                    st.query_map([self.node_id.as_str()], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
                };
                for (op, state, method, request) in cmds {
                    // the Node keeps the command's content hash as it computes it (method and
                    // business fields, not the id), not the Hub's hash of the stored envelope
                    let digest = serde_json::from_str::<Command<Value>>(&request)
                        .map_err(conflict)?
                        .digest(&method)
                        .map_err(conflict)?;
                    let unknown = |reason: &str| -> Result<Receipt, StoreError> {
                        Ok(Receipt {
                            operation: idv(op.clone())?,
                            state: Observed::Unknown {
                                reason: reason.into(),
                            },
                        })
                    };
                    let receipt = match entries.get(&op) {
                        Some(e) if e.digest == digest && e.method == method => e.receipt.clone(),
                        Some(_) => unknown("the Node holds another command under this id")?,
                        // never got there: the sender sends the very same command
                        None if state == "pending" => continue,
                        None => unknown("the Node has no record of it")?,
                    };
                    let before: String = tx.query_row(
                        "SELECT state FROM node_commands WHERE operation_id=?",
                        [&op],
                        |r| r.get(0),
                    )?;
                    if let Some(end) = self.receipt_in(tx, &receipt)? {
                        ends.push(end);
                    }
                    let after: String = tx.query_row(
                        "SELECT state FROM node_commands WHERE operation_id=?",
                        [&op],
                        |r| r.get(0),
                    )?;
                    if before != after {
                        changed += 1;
                    }
                }
                // 2. sessions the Node let go (a restart lets every one go, silently)
                for (s, view) in &views {
                    let gone = view
                        .as_ref()
                        .is_none_or(|v| v.phase == SessionPhase::Released);
                    if gone
                        && phase_in(tx, s)?.is_some_and(|(p, a)| p == "ready" && a.is_none())
                    {
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
                        changed += 1;
                    }
                }
                // 3. cards nobody waits for any more
                let cards: Vec<(String, String)> = {
                    let mut st = tx.prepare(
                        "SELECT d.id, d.subject_id FROM decisions d JOIN runtime_sessions s ON s.session_id=d.session_id WHERE s.node_id=? AND d.subject_type='tool_call' AND d.status='open'",
                    )?;
                    st.query_map([self.node_id.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<Vec<_>, _>>()?
                };
                for (d, perm) in cards {
                    if waiting.contains(&perm) {
                        continue;
                    }
                    // a card with its row closes through it (an answer on its way was never applied)
                    if let Some(row) = rt::permission_of_decision_in(tx, &d)? {
                        if matches!(row.state.as_str(), "waiting" | "answering") {
                            self.close_in(tx, &row, "Node 已不再等待这个授权")?;
                            changed += 1;
                        }
                    } else if steve_store::decision_resolve_in(
                        tx,
                        &d,
                        &json!({"superseded": "Node 已不再等待这个授权"}),
                        "expired",
                    )? {
                        changed += 1;
                    }
                }
                // 4. Node work the Hub has no record of (its database went back, say): counted
                // and logged, its session held with a note; never given up on its own (D7)
                let was = self.held.lock().unwrap().clone();
                let mut held = HashSet::new();
                let mut strangers = 0;
                for (op, e) in &entries {
                    if e.receipt.state.is_finished() {
                        continue;
                    }
                    let known: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM node_commands WHERE operation_id=? AND node_id=?)",
                        rusqlite::params![op, self.node_id.as_str()],
                        |r| r.get(0),
                    )?;
                    if known {
                        continue;
                    }
                    strangers += 1;
                    let session = e.execution.as_ref().and_then(exec_session);
                    tracing::warn!(
                        "node {}: {} {op} ({:?}) is not the Hub's; held",
                        self.node_id,
                        e.method,
                        e.receipt.state.tag()
                    );
                    let Some(session) = session else { continue };
                    let ours: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
                        rusqlite::params![session, self.node_id.as_str()],
                        |r| r.get(0),
                    )?;
                    if ours && held.insert(session.clone()) && !was.contains(&session) {
                        system_in(tx, &session, &steve_i18n::stored(|| steve_i18n::t!("hub.node.stranger")))?;
                        changed += 1;
                    }
                }
                if strangers > 0 {
                    tracing::warn!(
                        "node {}: {strangers} operations the Hub has no record of, {} sessions held",
                        self.node_id,
                        held.len()
                    );
                }
                Ok((changed, held))
            })
            .map_err(internal)?;
        let (changed, held) = changed;
        *self.held.lock().unwrap() = held;
        for end in ends {
            self.tell_end(end);
        }
        self.wake_apply.notify_one();
        Ok(changed)
    }

    /// A `Released` view from before the Hub's last open of the session (that open's own view
    /// has not come on the stream yet): it says nothing of the session open now. Only the event's
    /// own epoch counts: a stream keeps its epoch within one journal, and an older journal's views
    /// say nothing of this one's.
    pub(super) fn stale_release_in(
        &self,
        tx: &Tx,
        session: &str,
        ev: &NodeEvent,
    ) -> Result<bool, StoreError> {
        let r: Option<String> = tx
            .query_row(
                "SELECT json_extract(receipt,'$.payload') FROM node_commands WHERE method='open_session' AND json_extract(request,'$.body.session')=? AND state='finished' AND json_extract(receipt,'$.success')=1 ORDER BY created DESC, rowid DESC LIMIT 1",
                [session],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(Ok(receipt)) = r.map(|r| serde_json::from_str::<Receipt>(&r)) else {
            return Ok(false);
        };
        let Observed::Finished {
            result: Some(OperationResult::Session(opened)),
            ..
        } = receipt.state
        else {
            return Ok(false);
        };
        let seen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM node_event_inbox WHERE node_id=?1 AND stream=?2 AND epoch=?3 AND seq<?4
               AND json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.id')=?5
               AND json_extract(payload,'$.body.view.revision')=?6)",
            rusqlite::params![
                self.node_id.as_str(),
                ev.cursor.stream.as_str(),
                ev.cursor.epoch.as_str(),
                ev.cursor.seq as i64,
                session,
                opened.revision.as_str()
            ],
            |r| r.get(0),
        )?;
        Ok(!seen)
    }
}

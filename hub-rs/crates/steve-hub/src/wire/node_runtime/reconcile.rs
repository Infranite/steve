//! Startup and restart reconciliation with the Node: stream catch-up, ledger comparison,
//! released sessions, orphaned permission cards and stranger Node work (D7).

use super::*;
use steve_store::node_commands::{self, reconcile as commands};
use steve_store::node_sessions::reconcile as sessions;

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
        let _ = tokio::task::spawn_blocking(move || {
            me.plan_waiting();
        })
        .await;
        Ok(n)
    }

    /// What waited while the Hub was not reconciled goes now: queued messages, and a desired
    /// config whose idle boundary (a turn's end applied during reconcile) found nothing planned.
    pub(super) fn plan_waiting(&self) {
        let sessions = self
            .store
            .read(|r| sessions::waiting_sessions(r, self.node_id.as_str()))
            .unwrap_or_default();
        for s in sessions {
            if let Err(e) = self.write(|tx| self.plan_after_in(tx, &s)) {
                tracing::error!("plan {s} after reconciling: {e}");
            }
        }
        self.wake_send.notify_one();
    }

    /// Compares the Hub's view with what the Node holds now (its ledger and stream heads as of one
    /// moment, its sessions, its waiting permissions), before anything is sent. First what the Hub
    /// applied is brought up to the Node's heads ([`catch_up_streams`]: replayed and applied what it
    /// lacks, faulted what cannot be reconciled). The Node may have restarted meanwhile (a local
    /// Node restarts with the Hub): what it cannot tell the end of becomes `unknown` (never sent
    /// again; a person gives it up), a session it let go is `released` (the next message opens it
    /// again), a card nobody waits for any more expires, and a session with Node work the Hub has
    /// no record of is held with a note (that work is never given up on its own). Returns how many
    /// things changed.
    pub async fn reconcile(&self) -> ApiResult<usize> {
        let internal = |e: StoreError| Error::new(Code::Internal, e.to_string());
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
        let sessions = self
            .store
            .read(|r| node_sessions::on_node(r, self.node_id.as_str()))
            .map_err(internal)?;
        let mut views: Vec<(String, Option<SessionView>)> = vec![];
        for s in sessions {
            let id: SessionId = match SessionId::try_from(s.clone()) {
                Ok(x) => x,
                Err(_) => continue,
            };
            match self.node.session(id).await {
                Ok(v) => views.push((s, Some(v))),
                Err(e) if e.code == Code::Invalid => views.push((s, None)),
                Err(e) => return Err(e),
            }
        }
        let waiting: HashSet<String> = entries
            .values()
            .flat_map(|e| e.pending.iter().map(|r| r.id.to_string()))
            .collect();
        let mut ends: Vec<End> = vec![];
        let changed = self
            .write(|tx| {
                let mut changed = 0;
                self.reconcile_commands_in(tx, &entries, &mut ends, &mut changed)?;
                self.reconcile_sessions_in(tx, &views, &mut changed)?;
                self.reconcile_permissions_in(tx, &waiting, &mut changed)?;
                let held = self.hold_stranger_work_in(tx, &entries, &mut changed)?;
                Ok((changed, held))
            })
            .map_err(internal)?;
        let (changed, held) = changed;
        *self.held.lock().unwrap() = held;
        for end in ends {
            self.tell_end(end);
        }
        self.wake_upkeep.notify_one();
        Ok(changed)
    }

    fn reconcile_commands_in(
        &self,
        tx: &mut W<'_>,
        entries: &HashMap<String, SnapshotEntry>,
        ends: &mut Vec<End>,
        changed: &mut usize,
    ) -> Result<(), StoreError> {
        // 1. commands: what the Node says of each one in flight
        let cmds = self
            .store
            .read(|r| commands::unsettled(r, self.node_id.as_str()))?;
        for commands::Command {
            operation: op,
            state,
            method,
            request,
            maybe_sent,
        } in cmds
        {
            let digest = request.digest(&method).map_err(conflict)?;
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
                None if state == "pending" && !maybe_sent => {
                    // A one-shot call whose waiter is gone can settle locally before admission.
                    if method == "start_job"
                        && let Some(attempt) = self
                            .store
                            .read(|r| steve_store::node_jobs::command_attempt(r, &op))?
                        && !self.jobs.has(&attempt)
                    {
                        self.stop_job_in(tx, &attempt)?;
                        *changed += 1;
                    }
                    continue;
                }
                None => unknown("the Node has no record of it")?,
            };
            let before = self
                .store
                .read(|r| commands::required_state(r, self.node_id.as_str(), &op))?;
            if let Some(end) = self.receipt_in(tx, &receipt)? {
                ends.push(end);
            }
            let after = self
                .store
                .read(|r| commands::required_state(r, self.node_id.as_str(), &op))?;
            if before != after {
                *changed += 1;
            }
        }
        Ok(())
    }

    fn reconcile_sessions_in(
        &self,
        tx: &mut W<'_>,
        views: &[(String, Option<SessionView>)],
        changed: &mut usize,
    ) -> Result<(), StoreError> {
        // 2. sessions the Node let go (a restart lets every one go, silently)
        for (s, view) in views {
            let gone = view
                .as_ref()
                .is_none_or(|v| v.phase == SessionPhase::Released);
            if gone
                && self
                    .phase_in(s)?
                    .is_some_and(|(p, a)| p == "ready" && a.is_none())
            {
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
                *changed += 1;
            }
        }
        Ok(())
    }

    fn reconcile_permissions_in(
        &self,
        tx: &mut W<'_>,
        waiting: &HashSet<String>,
        changed: &mut usize,
    ) -> Result<(), StoreError> {
        // 3. cards nobody waits for any more
        let cards = self
            .store
            .read(|r| sessions::open_permissions(r, self.node_id.as_str()))?;
        for (d, perm) in cards {
            if waiting.contains(&perm) {
                continue;
            }
            // a card with its row closes through it (an answer on its way was never applied)
            if let Some(row) = node_write::permission_of_decision_in(tx, &d)? {
                if matches!(row.state.as_str(), "waiting" | "answering") {
                    self.close_in(tx, &row, "Node 已不再等待这个授权")?;
                    *changed += 1;
                }
            } else if steve_store::DecisionRows(tx).settle(
                &d,
                &json!({"superseded": "Node 已不再等待这个授权"}),
                "expired",
                &serde_json::Value::Null,
            )? {
                *changed += 1;
            }
        }
        Ok(())
    }

    fn hold_stranger_work_in(
        &self,
        tx: &mut W<'_>,
        entries: &HashMap<String, SnapshotEntry>,
        changed: &mut usize,
    ) -> Result<HashSet<String>, StoreError> {
        // 4. Node work the Hub has no record of (its database went back, say): counted
        // and logged, its session held with a note; never given up on its own (D7)
        let was = self.held.lock().unwrap().clone();
        let mut held = HashSet::new();
        let mut strangers = 0;
        for (op, e) in entries {
            if e.receipt.state.is_finished() {
                continue;
            }
            if self
                .store
                .read(|r| node_commands::state(r, self.node_id.as_str(), op))?
                .is_some()
            {
                continue;
            }
            strangers += 1;
            let session = e.execution.as_ref().and_then(exec_session);
            tracing::warn!(
                "node {}: {} {op} ({:?}) is not the Hub's; held",
                self.node_id,
                e.method,
                e.receipt.state.state_name()
            );
            let Some(session) = session else { continue };
            let ours = self
                .store
                .read(|r| node_sessions::phase(r, &session, Some(self.node_id.as_str())))?
                .is_some();
            if ours && held.insert(session.clone()) && !was.contains(&session) {
                system_in(
                    tx,
                    &session,
                    &steve_i18n::stored(|| steve_i18n::t!("hub.node.stranger")),
                )?;
                *changed += 1;
            }
        }
        if strangers > 0 {
            tracing::warn!(
                "node {}: {strangers} operations the Hub has no record of, {} sessions held",
                self.node_id,
                held.len()
            );
        }
        Ok(held)
    }

    /// A `Released` view from before the Hub's last open of the session (that open's own view
    /// has not come on the stream yet): it says nothing of the session open now. Asked before the
    /// view is kept as seen.
    pub(super) fn stale_release_in(&self, session: &str) -> Result<bool, StoreError> {
        self.store.read(|r| {
            let Some(revision) = commands::latest_opened(r, session)? else {
                return Ok(false);
            };
            Ok(!sessions::seen_view(
                r,
                self.node_id.as_str(),
                session,
                revision.as_str(),
            )?)
        })
    }
}

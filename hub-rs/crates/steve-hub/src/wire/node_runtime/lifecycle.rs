//! Session binding, open/reopen/release, idle sweeping, occupancy and health queries.

use super::{
    intent::{Advance, next_revision},
    *,
};
use steve_store::node_commands::dispatch;
use steve_store::node_sessions::lifecycle as sessions;

/// What [`NodeRuntime::release`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// No harness is open for the session (never opened, or let go already).
    Gone,
    /// This `release_session` lets it go: queued now, or (an ended session that is busy) queued
    /// for when its turn is over.
    Pending(String),
    /// Busy and not ended: kept.
    Busy,
}

impl NodeRuntime {
    /// The session runs on this Node from now on: its workspace is prepared and it is opened, by
    /// two commands (the open waits for the prepare). Idempotent for the same binding. Refused
    /// once [`Self::stop`] was called: nothing would ever send those commands.
    pub fn bind(&self, b: &Binding) -> Result<(), StoreError> {
        self.bind_after(b, None)
    }

    /// [`Self::bind`] for a session that takes over from another one: its prepare (and so its
    /// open) waits until `after`, that one's `release_session`, is done, so the two harnesses are
    /// never open at once (they share the item's worktree).
    pub fn bind_after(&self, b: &Binding, after: Option<&str>) -> Result<(), StoreError> {
        self.bind_new(b, after, None, None).map(|_| ())
    }

    pub(crate) fn bind_after_with_context(
        &self,
        b: &Binding,
        after: Option<&str>,
        system_context: Option<&str>,
    ) -> Result<(), StoreError> {
        self.bind_new(b, after, None, system_context).map(|_| ())
    }

    pub(crate) fn bind_after_scoped(
        &self,
        b: &Binding,
        after: Option<&str>,
        placement: &steve_types::BotPlacement,
        system_context: Option<&str>,
    ) -> Result<(), StoreError> {
        self.bind_new(b, after, Some(placement), system_context)
            .map(|_| ())
    }

    /// [`Self::bind_after`]. Whether the binding is new.
    fn bind_new(
        &self,
        b: &Binding,
        after: Option<&str>,
        placement: Option<&steve_types::BotPlacement>,
        system_context: Option<&str>,
    ) -> Result<bool, StoreError> {
        self.running()?;
        let workspace = WorkspaceRef {
            node: self.node_id.clone(),
            id: idv(derived_id("w", &b.resource))?,
        };
        let open = OpenSession {
            session: idv(b.session.clone())?,
            workspace: workspace.clone(),
            choice: b.choice.clone(),
            policy: b.policy.clone(),
            resume: None,
            // set in the bind's transaction, once its principal is recorded
            env: vec![],
            state: None,
            artifact_view: None,
            skills: vec![],
            system_context: system_context
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        };
        let fresh = self.write(|tx| {
            if self.ended_in(&b.session)? {
                return Err(conflict("已结束的会话不能绑定或重开。"));
            }
            self.session_owners_work_in(tx, &b.session)?;
            if !bound_strict(&self.store, &b.session)? && placement.is_some() {
                super::super::bot_dispatch::commit_guard(tx, &b.session, self.node_id.as_str())?;
            }
            let fresh = node_write::bind_session_in(
                tx,
                &rt::SessionBinding {
                    session: &b.session,
                    node: self.node_id.as_str(),
                    workspace: &serde_json::to_value(&workspace).map_err(conflict)?,
                    node_session: &b.session,
                    config_revision: open.policy.revision.as_str(),
                },
                self.clock.now(),
            )?;
            if !fresh {
                return Ok(false);
            }
            if let Some(placement) = placement {
                let scope = node_sessions::authority::session(tx, &b.session)?
                    .ok_or(StoreError::NotFound("Session"))?;
                let item = scope
                    .item
                    .ok_or(StoreError::Rejected("placement has no item".into()))?;
                steve_store::bot_dispatch::record(
                    tx,
                    &scope.workspace,
                    &item,
                    Some(&b.session),
                    Ok(placement),
                )?;
            }
            // whose access it runs with and who reads it, fixed now (a remote Node's owner reads it too)
            let owner = if self.is_remote() {
                self.store
                    .read(|r| steve_store::node_directory::owner(r, self.node_id.as_str()))?
            } else {
                None
            };
            super::super::principal::record_in(tx, &b.session, owner.as_deref())?;
            let prep_key =
                next_revision(tx, Intent::Prepare(open.session.clone()), Advance::NewInput)?;
            let prep_op = prep_key.op().to_string();
            let (prep, prep_d) = self.command(
                &prep_op,
                Prepare {
                    resource: b.resource.clone(),
                    kind: b.prepare.clone(),
                },
            )?;
            rt::insert_intent_in(
                tx,
                &prep_key,
                &rt::NewCommand {
                    operation: &prep_op,
                    node: self.node_id.as_str(),
                    method: "prepare_workspace",
                    request: &prep,
                    digest: &prep_d,
                    target_attempt: None,
                    dependency: after,
                },
                self.clock.now(),
            )?;
            let open_key =
                next_revision(tx, Intent::Open(open.session.clone()), Advance::NewInput)?;
            let open_op = open_key.op().to_string();
            // the principal's own variables on this Node, as recorded just now (node_env)
            let mut open = open.clone();
            if let Some(approval) = self.domain.approval(&b.session)? {
                open.policy.permissions = approval.mode;
            }
            open.env = super::super::node_env::for_session(
                &self.store,
                self.node_id.as_str(),
                &b.session,
            )?;
            // and its skills
            open.skills = super::super::bot_skills::for_session(tx, &b.session)?;
            if self.principal_state.load(Ordering::Relaxed) {
                open.state = super::super::principal::harness_state_in(
                    &self.store,
                    tx,
                    &b.session,
                    self.node_id.as_str(),
                    &open.choice.harness,
                )?;
            }
            let (open_v, open_d) = self.command(&open_op, open.clone())?;
            rt::insert_intent_in(
                tx,
                &open_key,
                &rt::NewCommand {
                    operation: &open_op,
                    node: self.node_id.as_str(),
                    method: "open_session",
                    request: &open_v,
                    digest: &open_d,
                    target_attempt: None,
                    dependency: Some(&prep_op),
                },
                self.clock.now(),
            )?;
            self.store.write(|w| sessions::reset_views(w, &b.session))?;
            let rev = self.session_revision(&b.session)?;
            node_write::session_cas_in(
                tx,
                &b.session,
                rev,
                &rt::SessionUpdate {
                    phase: "opening",
                    active_attempt: None,
                    release_pending: false,
                },
                self.clock.now(),
            )?;
            Ok(true)
        })?;
        if fresh {
            self.record_view(&b.session);
        }
        if fresh {
            self.wake_send.notify_one();
        }
        Ok(fresh)
    }

    /// What `session` would be given under the path model, in shadow (S2, [`super::super::view`]), after
    /// its bind committed: never a reason for the bind to fail. The local Node's roots today are
    /// the Hub's to list (with the configured ones, [`Self::set_configured_roots`]); a remote
    /// Node's are its own.
    fn record_view(&self, session: &str) {
        let today = if self.is_remote() {
            None
        } else {
            match self.store.read(|r| node_sessions::placements::roots(r)) {
                Ok((rw, ro)) => {
                    let extra = self.configured_roots.get().cloned().unwrap_or_default();
                    Some(rw.into_iter().chain(ro).chain(extra).collect::<Vec<_>>())
                }
                Err(e) => {
                    tracing::warn!(session, "view shadow: today's roots: {e}");
                    return;
                }
            }
        };
        let node = self.node_id.as_str();
        if let Err(e) =
            self.write(|w| super::super::view::record_in(w, session, node, today.as_deref()))
        {
            tracing::warn!(session, "view shadow: {e}");
        }
    }

    /// The session a decision belongs to.
    /// The session a decision card belongs to (`None`: no such card, or not a session's).
    pub fn decision_session(&self, decision: &str) -> Result<Option<String>, StoreError> {
        decision_session(&self.store, decision)
    }

    /// The session's harness is gone on its Node (released, or abandoned there): open it again,
    /// continuing the same conversation where the Node can.
    pub(super) fn reopen_in(&self, tx: &mut W<'_>, session: &str) -> Result<(), StoreError> {
        self.plan_open_in(tx, session, Advance::NewInput)
    }

    fn plan_open_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        advance: Advance,
    ) -> Result<(), StoreError> {
        if self.ended_in(session)? || super::plan::startup_blocked_in(tx, session)? {
            return Ok(());
        }
        // a worktree it made that was tidied since, or that it could not make: made again first
        let mut open = self.open_of(session)?;
        let key = next_revision(tx, Intent::Open(open.session.clone()), advance)?;
        let prepare = self.prepare_again_in(tx, session, advance)?;
        self.fold_desired_in(session, &mut open)?;
        open.resume = self.store.read(|r| sessions::resume(r, session))?;
        // the variables as they are now: a removed or changed one does not come back
        open.env =
            super::super::node_env::for_session(&self.store, self.node_id.as_str(), session)?;
        open.skills = super::super::bot_skills::for_session(tx, session)?;
        let op = key.op().to_string();
        let (req, digest) = self.command(&op, open)?;
        rt::insert_intent_in(
            tx,
            &key,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "open_session",
                request: &req,
                digest: &digest,
                target_attempt: None,
                dependency: prepare.as_deref(),
            },
            self.clock.now(),
        )?;
        self.store.write(|w| sessions::reset_views(w, session))?;
        let rev = self.session_revision(session)?;
        node_write::session_cas_in(
            tx,
            session,
            rev,
            &rt::SessionUpdate {
                phase: "opening",
                active_attempt: None,
                release_pending: false,
            },
            self.clock.now(),
        )?;
        Ok(())
    }

    /// One explicit retry, with the user's current Bot selection. Automatic planners cannot
    /// clear the durable startup stop. No other harness is chosen on the user's behalf.
    pub fn retry_open(&self, session: &str, choice: &HarnessChoice) -> Result<(), StoreError> {
        self.running()?;
        self.write(|tx| {
            if self.ended_in(session)? {
                return Err(conflict("已结束的会话不能重开，请使用当前会话。"));
            }
            if !super::plan::startup_blocked_in(tx, session)? {
                return Err(conflict("会话已经重试或状态已变化，请刷新。"));
            }
            let mut open = self.open_of(session)?;
            // A binding keeps its adapter identity; this compares user-selected ids, not adapter kinds.
            if !choice.harness.eq(&open.choice.harness) {
                return Err(conflict("Bot 的执行器已改变，请重新指派以创建新会话。"));
            }
            open.choice = choice.clone();
            let mut desired = super::config::desired_in(&self.store, session)?.unwrap_or(
                super::config::Desired {
                    choice: choice.clone(),
                    policy: open.policy,
                    refreshes: 0,
                    expected: None,
                },
            );
            desired.choice = choice.clone();
            super::config::set_desired_in(&self.store, session, Some(&desired))?;
            self.store
                .write(|w| node_sessions::startup::clear_stop(w, session))?;
            self.plan_open_in(tx, session, Advance::ExplicitRetry)?;
            self.status_in(session, "starting")?;
            Ok(())
        })?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// Lets the session's harness go while it is idle (the next message opens it again, continuing
    /// the conversation). A session the business side ended during a turn is let go once that
    /// turn is over: its release is queued now and goes then ([`Release::Pending`] either way).
    pub fn release(&self, session: &str) -> Result<Release, StoreError> {
        self.running()?;
        let r = self.write(|tx| self.release_in(tx, session))?;
        if matches!(r, Release::Pending(_)) {
            self.wake_send.notify_one();
        }
        Ok(r)
    }

    /// Lets go every session idle for [`Self::set_idle_release`] (once the Hub has reconciled
    /// with the Node). Returns how many releases were planned.
    pub(super) fn release_idle(&self) -> usize {
        if !self.may_plan() || self.running().is_err() || self.store.maintenance() {
            return 0;
        }
        let before = self.clock.now() - self.idle_release.load(Ordering::Relaxed);
        let node = self.node_id.as_str();
        let idle = self.store.read(|r| sessions::idle(r, node, before, None));
        let idle = match idle {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("node {}: idle sessions: {e}", self.node_id);
                return 0;
            }
        };
        let mut n = 0;
        for s in idle {
            // checked again in the release's own transaction: a message may have come meanwhile
            let r = self.write(|tx| {
                let still = self
                    .store
                    .read(|r| sessions::idle(r, node, before, Some(&s)))?;
                // a session paused (its stream faulted, stranger Node work held) is sent no new
                // command, a release included
                if still.is_empty()
                    || self.is_held(&s)
                    || self.store.read(|r| faulted(r, &self.node_id, &s))?
                {
                    return Ok(false);
                }
                // only a release that goes out lets a harness go
                Ok(matches!(self.release_in(tx, &s)?, Release::Pending(_)))
            });
            match r {
                Ok(true) => {
                    tracing::info!(session = %s, "node {}: idle, released", self.node_id);
                    n += 1;
                }
                Ok(false) => {}
                Err(e) => tracing::error!("node {}: release idle {s}: {e}", self.node_id),
            }
        }
        if n > 0 {
            self.wake_send.notify_one();
        }
        n
    }

    /// [`Self::release`] inside `tx`.
    pub(super) fn release_in(&self, tx: &mut W<'_>, session: &str) -> Result<Release, StoreError> {
        let Some((phase, active)) = self.phase_in(session)? else {
            return Ok(Release::Gone);
        };
        let waiting = self.store.read(|r| dispatch::release(r, session, true))?;
        match (phase.as_str(), active.is_some()) {
            ("released", false) => {
                // nothing left to let go: a release that waited for the turn is done without
                // going out (what waits for it may go)
                if let Some(op) = waiting {
                    node_write::merge_receipt_in(
                        tx,
                        &op,
                        ReceiptState::Finished,
                        true,
                        &json!({ "reason": "the session was let go already" }),
                        self.clock.now(),
                    )?;
                }
                Ok(Release::Gone)
            }
            ("releasing", _) => Ok(
                match self.store.read(|r| dispatch::release(r, session, false))? {
                    Some(op) => Release::Pending(op),
                    None => Release::Busy,
                },
            ),
            ("ready", false) => {
                let op = match waiting {
                    Some(op) => {
                        let at = self.clock.now();
                        self.store.write(|w| dispatch::postpone(w, &op, at, at))?;
                        op
                    }
                    None => self.release_command_in(tx, session)?,
                };
                let rev = self.session_revision(session)?;
                node_write::session_cas_in(
                    tx,
                    session,
                    rev,
                    &rt::SessionUpdate {
                        phase: "releasing",
                        active_attempt: None,
                        release_pending: false,
                    },
                    self.clock.now(),
                )?;
                Ok(Release::Pending(op))
            }
            _ if self.ended_in(session)? => {
                // busy (a turn, an open, a configure): queued for when that is over
                let op = match waiting {
                    Some(op) => op,
                    None => {
                        let op = self.release_command_in(tx, session)?;
                        self.store.write(|w| dispatch::defer_release(w, &op))?;
                        op
                    }
                };
                Ok(Release::Pending(op))
            }
            _ => Ok(Release::Busy),
        }
    }

    /// A new `release_session` command for `session`, its id.
    pub(super) fn release_command_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
    ) -> Result<String, StoreError> {
        let sid: SessionId = idv(session.to_string())?;
        let key = next_revision(tx, Intent::Release(sid.clone()), Advance::NewInput)?;
        let op = key.op().to_string();
        let (req, digest) = self.command(&op, sid)?;
        rt::insert_intent_in(
            tx,
            &key,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "release_session",
                request: &req,
                digest: &digest,
                target_attempt: None,
                dependency: None,
            },
            self.clock.now(),
        )?;
        Ok(op)
    }

    /// An ended session is let go once it is idle (see [`Self::release`]).
    pub(super) fn release_if_ended_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
    ) -> Result<(), StoreError> {
        if self.ended_in(session)? {
            self.release_in(tx, session)?;
        }
        Ok(())
    }

    /// Whether `session` runs on a Node.
    pub fn owns(&self, session: &str) -> bool {
        self.owns_strict(session).unwrap_or(false)
    }

    /// [`Self::owns`], with a read error as an error: for a decision that must not rest on a guess.
    pub fn owns_strict(&self, session: &str) -> Result<bool, StoreError> {
        self.store
            .read(|r| Ok(node_sessions::phase(r, session, Some(self.node_id.as_str()))?.is_some()))
    }

    /// The session's open request as in force: as last opened or configured (the turn must match
    /// it exactly).
    pub(super) fn open_of(&self, session: &str) -> Result<OpenSession, StoreError> {
        self.store.read(|r| config_in(r, session))
    }

    /// A turn of `session` is in flight (planned, running, waiting, retrying or unknown), or the
    /// session is opening or releasing.
    pub fn is_busy(&self, session: &str) -> bool {
        self.store
            .read(|r| sessions::busy(r, session))
            .unwrap_or(true)
    }

    pub fn running_turns(&self) -> usize {
        self.store
            .read(|r| sessions::running(r, Some(self.node_id.as_str())))
            .unwrap_or(0) as usize
    }

    /// What an upgrade drain waits for: every open turn (running, waiting for a permission answer
    /// until it is answered or expires, retrying, or unknown until settled or abandoned), plus each
    /// session whose harness is opening, configuring or releasing with no open turn. A read error
    /// counts as work: the drain then waits rather than seal under a running turn.
    /// Only this Node's sessions count: a remote Node's turns outlive the Hub's restart.
    /// A stopped Node counts nothing: it runs nothing and refuses the Stop that would settle its
    /// open turns, so a restart cuts nothing (its unknown turns stay unknown across it).
    pub fn working(&self) -> usize {
        if self.stopped.lock().unwrap().is_some() {
            return 0;
        }
        self.store
            .read(|r| sessions::working(r, self.node_id.as_str()))
            .unwrap_or(1) as usize
    }

    /// The same durable work counted by `working`, for the instance administrator's upgrade receipt.
    /// Do not include prompts, command bodies or permission payloads in a maintenance view.
    pub fn working_details(&self) -> Vec<Value> {
        if self.stopped.lock().unwrap().is_some() {
            return vec![];
        }
        self.store.read(|r| sessions::working_details(r, self.node_id.as_str()))
            .map(|rows| rows.into_iter().map(|work| json!({
                "kind": work.kind, "node_id": self.node_id.as_str(),
                "session_id": work.session, "item_id": work.item,
                "attempt_id": work.attempt, "state": work.state,
                "reason": "waiting_for_node_work"
            })).collect())
            .unwrap_or_else(|_| vec![json!({"kind":"node_session","node_id":self.node_id.as_str(),"state":"unknown","reason":"node_state_unavailable"})])
    }

    /// The session's open turn has an outcome nobody can tell yet (reconciliation decides), or its
    /// stream is faulted (its events wait for reconciliation, so what they say is not known).
    /// A read error counts as uncertain: waiting is the safe answer.
    pub fn is_uncertain(&self, session: &str) -> bool {
        self.store
            .read(|r| {
                Ok::<_, StoreError>(
                    node_sessions::uncertain_turn(r, session)?
                        || faulted(r, &self.node_id, session)?,
                )
            })
            .unwrap_or(true)
            || self.is_held(session)
    }

    /// The Node holds work of the session the Hub has no record of ([`Self::reconcile`]).
    pub(super) fn is_held(&self, session: &str) -> bool {
        self.held.lock().unwrap().contains(session)
    }

    /// The attempt the session runs now (not finished yet), if any.
    pub fn active_attempt(&self, session: &str) -> Result<Option<String>, StoreError> {
        self.store.read(|r| sessions::active_attempt(r, session))
    }
}

/// `/healthz`'s `node` block: what rollout and rollback decisions read instead of the database.
/// `local`: the colocated local Node's coordinator; `remote`: remote Nodes are
/// listened for (unless `STEVE_NODE_LISTEN=0`). The counts come from the store, so they are there with
/// the Node off too:
///
/// - `sessions` / `open`: sessions bound to any Node / not released by it; `running`: their turns
///   not finished, plus harnesses restarting for a configure (what a drain waits for);
/// - `unknown`: attempts whose outcome nobody can tell (a person gives them up);
/// - `parked`: sessions with queued messages bound to a Node that is not enabled here.
///
/// `fallbacks` (since boot) counts new sessions bound nowhere although the local Node was on (they
/// are refused); `research_fallbacks` (since boot) research runs that went in process although
/// research runs on the local Node. Public: no reason text, ids or paths, only states and counts.
pub fn health(
    store: &Store,
    local: Option<&NodeRuntime>,
    remote: bool,
) -> Result<Value, StoreError> {
    let counts = store.read(|r| sessions::health(r, local.is_some(), remote))?;
    Ok(json!({
        "enabled": local.is_some(),
        "remote": remote,
        "reconciled": local.map(|n| n.is_reconciled()),
        "stopped": local.is_some_and(|n| n.is_stopped()),
        "holding": local.is_some_and(|n| n.is_holding()),
        "fallbacks": local.map_or(0, |n| n.fallbacks()),
        "research_fallbacks": local.map_or(0, |n| n.research_fallbacks()),
        "sessions": counts.sessions,
        "open": counts.open,
        "running": counts.running,
        "unknown": counts.unknown,
        "parked": counts.parked,
    }))
}

/// The session a decision belongs to.
pub fn decision_session(store: &Store, decision: &str) -> Result<Option<String>, StoreError> {
    store.read(|r| sessions::decision_session(r, decision))
}

/// `decision` is the card of a Node turn still waiting for its answer.
pub fn waiting_card(store: &Store, decision: &str) -> bool {
    store
        .read(|r| sessions::waiting_card(r, decision))
        .unwrap_or(false)
}

/// Whether `session` is bound to a Node (whether or not that Node is enabled in this process).
pub fn bound(store: &Store, session: &str) -> bool {
    // when unsure, as bound: never hand it to another backend
    bound_strict(store, session).unwrap_or(true)
}

/// [`bound`], with a read error as an error: for a decision that must not rest on a guess.
pub fn bound_strict(store: &Store, session: &str) -> Result<bool, StoreError> {
    store.read(|r| Ok(node_sessions::phase(r, session, None)?.is_some()))
}

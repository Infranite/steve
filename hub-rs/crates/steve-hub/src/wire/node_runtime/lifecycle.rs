//! Session binding, open/reopen/release, idle sweeping, occupancy and health queries.

use super::*;

/// The `retry_at` of a release queued for when its session's turn is over: never leased until
/// [`NodeRuntime::release_in`] sends it.
const AFTER_TURN: i64 = i64::MAX;

/// The sessions of node ?1 idle since before ?2 (ms), all of them or only ?3: open with nothing
/// running, no turn open or over since then, nothing queued and no open card.
const IDLE_SESSIONS_SQL: &str = "SELECT s.session_id FROM runtime_sessions s
   WHERE s.node_id=?1 AND s.phase='ready' AND s.active_attempt IS NULL AND s.updated<=?2
     AND (?3 IS NULL OR s.session_id=?3)
     AND NOT EXISTS (SELECT 1 FROM runtime_turns t WHERE t.session_id=s.session_id
           AND (t.state<>'finished' OR t.updated>?2))
     AND NOT EXISTS (SELECT 1 FROM queued q WHERE q.session_id=s.session_id)
     AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.session_id=s.session_id
           AND d.subject_type='tool_call' AND d.status='open')
   ORDER BY s.updated LIMIT 64";

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
    /// never open at once (they share the item's worktree). A release that fails is asked again
    /// and the session waits for that one instead ([`Self::release_again_in`]).
    pub fn bind_after(&self, b: &Binding, after: Option<&str>) -> Result<(), StoreError> {
        self.bind_new(b, after).map(|_| ())
    }

    /// [`Self::bind_after`]. Whether the binding is new.
    fn bind_new(&self, b: &Binding, after: Option<&str>) -> Result<bool, StoreError> {
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
            // set in the bind's transaction, from its Bot
            mcp_servers: vec![],
            skills: vec![],
        };
        let fresh = self.tx(|tx| {
            self.session_owners_work_in(tx, &b.session)?;
            let fresh = rt::bind_session_in(
                tx,
                &rt::SessionBinding {
                    session: &b.session,
                    node: self.node_id.as_str(),
                    workspace: &serde_json::to_value(&workspace).map_err(conflict)?,
                    node_session: &b.session,
                    config_revision: open.policy.revision.as_str(),
                },
                now(),
            )?;
            if !fresh {
                return Ok(false);
            }
            // whose access it runs with and who reads it, fixed now (a remote Node's owner reads it too)
            let owner: Option<String> = if self.is_remote() {
                tx.query_row(
                    "SELECT owner FROM nodes WHERE node_id=?",
                    [self.node_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?
            } else {
                None
            };
            super::super::principal::record_in(tx, &b.session, owner.as_deref())?;
            let (prep, prep_d) = command(
                &format!("op_prep_{}", b.session),
                Prepare {
                    resource: b.resource.clone(),
                    kind: b.prepare.clone(),
                },
            )?;
            let prep_op = format!("op_prep_{}", b.session);
            rt::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: &prep_op,
                    node: self.node_id.as_str(),
                    method: "prepare_workspace",
                    request: &prep,
                    digest: &prep_d,
                    target_attempt: None,
                    dependency: after,
                },
                now(),
            )?;
            let open_op = format!("op_open_{}", b.session);
            // the principal's own variables on this Node, as recorded just now (node_env)
            let mut open = open.clone();
            open.env = super::super::node_env::for_session(tx, self.node_id.as_str(), &b.session)?;
            // its Bot's own MCP servers, secrets by reference
            open.mcp_servers = super::super::bot_mcp::for_session(tx, &b.session)?;
            // and its skills
            open.skills = super::super::bot_skills::for_session(tx, &b.session)?;
            if self.principal_state.load(Ordering::Relaxed) {
                open.state = super::super::principal::harness_state_in(
                    tx,
                    &b.session,
                    self.node_id.as_str(),
                    &open.choice.harness,
                )?;
            }
            let (open_v, open_d) = command(&open_op, open.clone())?;
            rt::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: &open_op,
                    node: self.node_id.as_str(),
                    method: "open_session",
                    request: &open_v,
                    digest: &open_d,
                    target_attempt: None,
                    dependency: Some(&prep_op),
                },
                now(),
            )?;
            let rev = session_revision(tx, &b.session)?;
            rt::session_cas_in(
                tx,
                &b.session,
                rev,
                &rt::SessionUpdate {
                    phase: "opening",
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
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
            match self.store.reader(super::super::node_binding::root_paths_in) {
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
            self.tx(|tx| super::super::view::record_in(tx, session, node, today.as_deref()))
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
    pub(super) fn reopen_in(&self, tx: &Tx, session: &str) -> Result<(), StoreError> {
        self.reopen_after_in(tx, session, None)
    }

    /// [`Self::reopen_in`], the open waiting on `dependency` (a prepare) when there is one.
    pub(super) fn reopen_after_in(
        &self,
        tx: &Tx,
        session: &str,
        dependency: Option<&str>,
    ) -> Result<(), StoreError> {
        // a worktree it made that was tidied since, or that it could not make: made again first
        let again = match dependency {
            None => self.prepare_again_in(tx, session)?,
            Some(_) => None,
        };
        let dependency = again.as_deref().or(dependency);
        let mut open = self.open_of(tx, session)?;
        self.fold_desired_in(tx, session, &mut open)?;
        open.resume = resume_of_in(tx, &self.node_id, session)?;
        // the variables as they are now: a removed or changed one does not come back
        open.env = super::super::node_env::for_session(tx, self.node_id.as_str(), session)?;
        // and the Bot's MCP servers as they are now
        open.mcp_servers = super::super::bot_mcp::for_session(tx, session)?;
        open.skills = super::super::bot_skills::for_session(tx, session)?;
        let op = uid(&format!("op_open_{session}"));
        let (req, digest) = command(&op, open)?;
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "open_session",
                request: &req,
                digest: &digest,
                target_attempt: None,
                dependency,
            },
            now(),
        )?;
        let rev = session_revision(tx, session)?;
        rt::session_cas_in(
            tx,
            session,
            rev,
            &rt::SessionUpdate {
                phase: "opening",
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        Ok(())
    }

    /// Lets the session's harness go while it is idle (the next message opens it again, continuing
    /// the conversation). A session the business side ended during a turn is let go once that
    /// turn is over: its release is queued now and goes then ([`Release::Pending`] either way).
    pub fn release(&self, session: &str) -> Result<Release, StoreError> {
        self.running()?;
        let r = self.tx(|tx| self.release_in(tx, session))?;
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
        let before = now() - self.idle_release.load(Ordering::Relaxed);
        let node = self.node_id.as_str();
        let idle = self.store.reader(|c| {
            let mut st = c.prepare(IDLE_SESSIONS_SQL)?;
            let v = st
                .query_map(rusqlite::params![node, before, None::<String>], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        });
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
            let r = self.tx(|tx| {
                let still: Option<String> = tx
                    .query_row(IDLE_SESSIONS_SQL, rusqlite::params![node, before, s], |r| {
                        r.get(0)
                    })
                    .optional()?;
                // a session paused (its stream faulted, stranger Node work held, its events
                // lagging) is sent no new command, a release included
                if still.is_none() || self.is_held(&s) || faulted(tx, &self.node_id, &s)? {
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
    pub(super) fn release_in(&self, tx: &Tx, session: &str) -> Result<Release, StoreError> {
        let Some((phase, active)) = phase_in(tx, session)? else {
            return Ok(Release::Gone);
        };
        let waiting = after_turn_release_in(tx, session)?;
        match (phase.as_str(), active.is_some()) {
            ("released", false) => {
                // nothing left to let go: a release that waited for the turn is done without
                // going out (what waits for it may go)
                if let Some(op) = waiting {
                    rt::merge_receipt_in(
                        tx,
                        &op,
                        ReceiptState::Finished,
                        true,
                        &json!({ "reason": "the session was let go already" }),
                        now(),
                    )?;
                }
                Ok(Release::Gone)
            }
            ("releasing", _) => Ok(match last_release_in(tx, session)? {
                Some(op) => Release::Pending(op),
                None => Release::Busy,
            }),
            ("ready", false) => {
                let sid = SessionId::try_from(session.to_string()).map_err(conflict)?;
                let op = match waiting {
                    // it never went out: it goes now, issued now
                    Some(op) => {
                        let (req, digest) = command(&op, sid)?;
                        tx.execute(
                            "UPDATE node_commands SET request=?, digest=?, retry_at=?, updated=? WHERE operation_id=? AND state='pending'",
                            rusqlite::params![
                                serde_json::to_string(&req).map_err(conflict)?,
                                digest,
                                now(),
                                now(),
                                op
                            ],
                        )?;
                        op
                    }
                    None => self.release_command_in(tx, session)?,
                };
                let rev = session_revision(tx, session)?;
                rt::session_cas_in(
                    tx,
                    session,
                    rev,
                    &rt::SessionUpdate {
                        phase: "releasing",
                        active_attempt: None,
                        release_pending: false,
                    },
                    now(),
                )?;
                Ok(Release::Pending(op))
            }
            _ if ended_in(tx, session)? => {
                // busy (a turn, an open, a configure): queued for when that is over
                let op = match waiting {
                    Some(op) => op,
                    None => {
                        let op = self.release_command_in(tx, session)?;
                        tx.execute(
                            "UPDATE node_commands SET retry_at=? WHERE operation_id=?",
                            rusqlite::params![AFTER_TURN, op],
                        )?;
                        op
                    }
                };
                Ok(Release::Pending(op))
            }
            _ => Ok(Release::Busy),
        }
    }

    /// A new `release_session` command for `session`, its id.
    pub(super) fn release_command_in(&self, tx: &Tx, session: &str) -> Result<String, StoreError> {
        let op = uid(&format!("op_release_{session}"));
        let (req, digest) = command(
            &op,
            SessionId::try_from(session.to_string()).map_err(conflict)?,
        )?;
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "release_session",
                request: &req,
                digest: &digest,
                target_attempt: None,
                dependency: None,
            },
            now(),
        )?;
        Ok(op)
    }

    /// An ended session is let go once it is idle (see [`Self::release`]).
    pub(super) fn release_if_ended_in(&self, tx: &Tx, session: &str) -> Result<(), StoreError> {
        if ended_in(tx, session)? {
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
        self.store.reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
                [session, self.node_id.as_str()],
                |r| r.get::<_, bool>(0),
            )?)
        })
    }

    /// The session's open request as in force: as last opened or configured (the turn must match
    /// it exactly).
    pub(super) fn open_of(&self, tx: &Tx, session: &str) -> Result<OpenSession, StoreError> {
        self.config_of_in(tx, session)
    }

    /// A turn of `session` is in flight (planned, running, waiting, retrying or unknown), or the
    /// session is opening or releasing.
    pub fn is_busy(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=?1 AND (active_attempt IS NOT NULL OR phase IN ('opening','releasing','active','configuring')))
                       OR EXISTS(SELECT 1 FROM runtime_turns WHERE session_id=?1 AND state<>'finished')",
                    [session],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap_or(true)
    }

    pub fn running_turns(&self) -> usize {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    // a harness restarting for a configure counts: a drain waits for it
                    "SELECT (SELECT COUNT(*) FROM runtime_turns t JOIN runtime_sessions s USING(session_id) WHERE s.node_id=?1 AND t.state<>'finished') + (SELECT COUNT(*) FROM runtime_sessions WHERE node_id=?1 AND phase='configuring')",
                    [self.node_id.as_str()],
                    |r| r.get::<_, i64>(0),
                )?)
            })
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
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT (SELECT COUNT(*) FROM runtime_turns t JOIN runtime_sessions s USING(session_id)
                               WHERE s.node_id=?1 AND t.state<>'finished')
                          + (SELECT COUNT(*) FROM runtime_sessions s
                               WHERE s.node_id=?1 AND s.phase IN ('opening','configuring','releasing')
                                 AND NOT EXISTS(SELECT 1 FROM runtime_turns t
                                                WHERE t.session_id=s.session_id AND t.state<>'finished'))",
                    [self.node_id.as_str()],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap_or(1) as usize
    }

    /// The same durable work counted by `working`, for the instance administrator's upgrade receipt.
    /// Do not include prompts, command bodies or permission payloads in a maintenance view.
    pub fn working_details(&self) -> Vec<Value> {
        if self.stopped.lock().unwrap().is_some() {
            return vec![];
        }
        self.store.reader(|c| {
            let mut q = c.prepare(
                "SELECT r.session_id, s.item_id, t.active_attempt, t.state, 'node_turn'
                 FROM runtime_turns t JOIN runtime_sessions r USING(session_id)
                 LEFT JOIN sessions s ON s.id=r.session_id
                 WHERE r.node_id=?1 AND t.state<>'finished'
                 UNION ALL
                 SELECT r.session_id, s.item_id, r.active_attempt, r.phase, 'node_session'
                 FROM runtime_sessions r LEFT JOIN sessions s ON s.id=r.session_id
                 WHERE r.node_id=?1 AND r.phase IN ('opening','configuring','releasing')
                 AND NOT EXISTS(SELECT 1 FROM runtime_turns t WHERE t.session_id=r.session_id AND t.state<>'finished')
                 ORDER BY 1 LIMIT 100")?;
            let rows = q.query_map([self.node_id.as_str()], |r| Ok(json!({
                "kind": r.get::<_, String>(4)?, "node_id": self.node_id.as_str(),
                "session_id": r.get::<_, String>(0)?, "item_id": r.get::<_, Option<String>>(1)?,
                "attempt_id": r.get::<_, Option<String>>(2)?, "state": r.get::<_, String>(3)?,
                "reason": "waiting_for_node_work"
            })))?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(StoreError::from)
        }).unwrap_or_else(|_| vec![json!({"kind":"node_session","node_id":self.node_id.as_str(),"state":"unknown","reason":"node_state_unavailable"})])
    }

    /// The session's open turn has an outcome nobody can tell yet (reconciliation decides), or its
    /// stream is faulted (its events wait for reconciliation, so what they say is not known).
    /// A read error counts as uncertain: waiting is the safe answer.
    pub fn is_uncertain(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_turns t LEFT JOIN runtime_attempts a ON a.turn_id=t.turn_id AND a.state<>'finished' WHERE t.session_id=?1 AND t.state<>'finished' AND (t.state='unknown' OR a.state='unknown'))",
                    [session],
                    |r| r.get::<_, bool>(0),
                )? || faulted(c, &self.node_id, session)?)
            })
            .unwrap_or(true)
            || self.is_held(session)
    }

    /// The Node holds work of the session the Hub has no record of ([`Self::reconcile`]), or
    /// its stream was not applied in time and has not caught up yet.
    pub(super) fn is_held(&self, session: &str) -> bool {
        self.held.lock().unwrap().contains(session)
            || self.lagging.lock().unwrap().contains_key(session)
    }

    /// The attempt the session runs now (not finished yet), if any.
    pub fn active_attempt(&self, session: &str) -> Result<Option<String>, StoreError> {
        self.store.reader(|c| {
            Ok(c.query_row(
                "SELECT a.attempt_id FROM runtime_sessions s JOIN runtime_attempts a ON a.attempt_id = s.active_attempt WHERE s.session_id=? AND a.state<>'finished'",
                [session],
                |r| r.get(0),
            )
            .optional()?)
        })
    }
}

/// The session's last `release_session` command, whatever became of it.
pub(super) fn last_release_in(tx: &Tx, session: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT operation_id FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=? ORDER BY created DESC, rowid DESC LIMIT 1",
            [session],
            |r| r.get(0),
        )
        .optional()?)
}

/// The session's release queued for when its turn is over (never sent yet), if any.
fn after_turn_release_in(tx: &Tx, session: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT operation_id FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=?1 AND state='pending' AND retry_at=?2",
            rusqlite::params![session, AFTER_TURN],
            |r| r.get(0),
        )
        .optional()?)
}

/// `/healthz`'s `node` block: what rollout and rollback decisions read instead of the database.
/// `local`: the local Node's coordinator when `STEVE_NODE_MODE=local`; `remote`: remote Nodes are
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
    let (sessions, open, running, unknown, parked) = store.reader(|c| {
        Ok::<_, StoreError>(c.query_row(
            "SELECT
               (SELECT COUNT(*) FROM runtime_sessions),
               (SELECT COUNT(*) FROM runtime_sessions WHERE phase<>'released'),
               (SELECT COUNT(*) FROM runtime_turns WHERE state<>'finished')
                 + (SELECT COUNT(*) FROM runtime_sessions WHERE phase='configuring'),
               (SELECT COUNT(*) FROM runtime_attempts WHERE state='unknown'),
               (SELECT COUNT(*) FROM runtime_sessions r
                 WHERE ((r.node_id='local' AND ?1=0) OR (r.node_id<>'local' AND ?2=0))
                   AND EXISTS(SELECT 1 FROM queued q WHERE q.session_id=r.session_id))",
            [local.is_some(), remote],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )?)
    })?;
    Ok(json!({
        "enabled": local.is_some(),
        "remote": remote,
        "reconciled": local.map(|n| n.is_reconciled()),
        "stopped": local.is_some_and(|n| n.is_stopped()),
        "holding": local.is_some_and(|n| n.is_holding()),
        "fallbacks": local.map_or(0, |n| n.fallbacks()),
        "research_fallbacks": local.map_or(0, |n| n.research_fallbacks()),
        "sessions": sessions,
        "open": open,
        "running": running,
        "unknown": unknown,
        "parked": parked,
    }))
}

/// The session a decision belongs to.
pub fn decision_session(store: &Store, decision: &str) -> Result<Option<String>, StoreError> {
    store.reader(|c| {
        Ok::<_, StoreError>(
            c.query_row(
                "SELECT session_id FROM decisions WHERE id=?",
                [decision],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten(),
        )
    })
}

/// `decision` is the card of a Node turn still waiting for its answer.
pub fn waiting_card(store: &Store, decision: &str) -> bool {
    store
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_permissions WHERE decision_id=? AND state='waiting')",
                [decision],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .unwrap_or(false)
}

/// Whether `session` is bound to a Node (whether or not that Node is enabled in this process).
pub fn bound(store: &Store, session: &str) -> bool {
    // when unsure, as bound: never hand it to another backend
    bound_strict(store, session).unwrap_or(true)
}

/// [`bound`], with a read error as an error: for a decision that must not rest on a guess.
pub fn bound_strict(store: &Store, session: &str) -> Result<bool, StoreError> {
    store.reader(|c| {
        Ok::<_, StoreError>(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=?)",
            [session],
            |r| r.get::<_, bool>(0),
        )?)
    })
}

/// Where the session's conversation continues: the resume handle of the Node's last view of it.
fn resume_of_in(tx: &Tx, node: &NodeId, session: &str) -> Result<Option<ResumeHandle>, StoreError> {
    let r: Option<String> = match tx.query_row(
        "SELECT json_extract(payload,'$.body.view.resume') FROM node_event_inbox
           WHERE node_id=? AND json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.id')=?
             AND json_extract(payload,'$.body.view.resume') IS NOT NULL
           ORDER BY received DESC, seq DESC LIMIT 1",
        [node.as_str(), session],
        |r| r.get(0),
    ) {
        Ok(x) => x,
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    Ok(r.and_then(|h| ResumeHandle::try_from(h).ok()))
}

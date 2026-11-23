//! A Node session's harness config (D5): permission mode, model and effort change through the
//! Node's `configure_session`, only at an idle boundary, never under a running attempt.
//!
//! Three configs: the effective one (the last open or configure the Node finished with success:
//! what turns carry), the one in flight (a `configure_session` command, session phase
//! `configuring`) and the desired one (`runtime_sessions.desired_config`, durable). A configure
//! whose outcome is unknown keeps the session `configuring` and is never sent again.

use super::*;
use commands::Method;
use serde::{Deserialize, Serialize};
use steve_store::{Read, node_commands::config as commands, node_sessions};

/// What to change. `None` leaves a field as it is; `Some(None)` sets the harness default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigChange {
    pub model: Option<Option<String>>,
    pub effort: Option<Option<String>>,
    pub permissions: Option<PermissionMode>,
}

/// What [`NodeRuntime::configure`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Configured {
    /// Already so (or already on its way): nothing to do.
    Unchanged,
    /// The session is idle: the configure is on its way.
    Sent,
    /// The session is busy or held: it goes at the next boundary.
    Pending,
    /// The session is released: the next open carries it.
    OnReopen,
}

/// The desired config as stored.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub(super) struct Desired {
    pub choice: HarnessChoice,
    pub policy: ExecutionPolicy,
    /// Configures refused for a stale revision and sent again with the Node's own.
    #[serde(default)]
    pub refreshes: u8,
    /// The revision to send with, once the Node told it (after a refusal).
    #[serde(default)]
    pub expected: Option<Revision>,
}

/// Configures sent again after a stale-revision refusal, at most.
pub(super) const MAX_REFRESHES: u8 = 2;

/// Same choice and policy, the policy's revision aside.
fn same_config(
    a: (&HarnessChoice, &ExecutionPolicy),
    b: (&HarnessChoice, &ExecutionPolicy),
) -> bool {
    let strip = |p: &ExecutionPolicy| {
        let mut p = p.clone();
        p.revision = b.1.revision.clone();
        p
    };
    a.0 == b.0 && strip(a.1) == *b.1
}

fn rank(p: PermissionMode) -> u8 {
    match p {
        PermissionMode::Plan => 0,
        PermissionMode::Ask => 1,
        PermissionMode::AcceptEdits => 2,
        PermissionMode::Auto => 3,
        PermissionMode::FullAccess => 4,
    }
}

/// Steve's name of a permission mode.
pub(crate) fn mode_str(p: PermissionMode) -> &'static str {
    match p {
        PermissionMode::Plan => "plan",
        PermissionMode::Ask => "ask",
        PermissionMode::AcceptEdits => "accept_edits",
        PermissionMode::Auto => "auto",
        PermissionMode::FullAccess => "full_access",
    }
}

fn describe(c: &HarnessChoice, p: &ExecutionPolicy) -> String {
    let mode = mode_str(p.permissions);
    let mut s = format!("权限 {mode}");
    if let Some(m) = &c.model {
        s.push_str(&format!("，模型 {m}"));
    }
    if let Some(e) = &c.effort {
        s.push_str(&format!("，推理 {e}"));
    }
    s
}

fn system_in(tx: &mut W<'_>, session: &str, text: &str) -> Result<(), StoreError> {
    event_in(tx, session, "system", json!({ "text": text }))?;
    Ok(())
}

pub(super) fn desired_in(store: &Store, session: &str) -> Result<Option<Desired>, StoreError> {
    store.read(|r| node_sessions::desired_config(r, session))
}

pub(super) fn set_desired_in(
    store: &Store,
    session: &str,
    desired: Option<&Desired>,
) -> Result<(), StoreError> {
    store.write(|w| node_sessions::set_desired_config(w, session, desired, now()))
}

/// The session's config in force: successful commands override the latest open request.
pub(crate) fn config_in(c: &mut impl Read, session: &str) -> Result<OpenSession, StoreError> {
    match commands::latest(c, session, None, true)? {
        Some(command) if command.method == Method::Open => command.open(),
        Some(command) => {
            let config = command.configure()?;
            let open = match commands::latest(c, session, Some(Method::Open), true)? {
                Some(open) => Some(open),
                None => commands::latest(c, session, Some(Method::Open), false)?,
            };
            let mut open = open
                .ok_or_else(|| conflict("configured but never opened"))?
                .open()?;
            open.choice = config.choice;
            open.policy = config.policy;
            Ok(open)
        }
        None => commands::latest(c, session, Some(Method::Open), false)?
            .ok_or_else(|| conflict("the session was never opened"))?
            .open(),
    }
}

impl NodeRuntime {
    /// What the session will run with once what is in flight lands.
    fn prospective_in(
        &self,
        session: &str,
        phase: &str,
    ) -> Result<(HarnessChoice, ExecutionPolicy), StoreError> {
        self.store.read(|c| {
            if phase == "configuring"
                && let Some(cmd) = commands::latest(c, session, Some(Method::Configure), false)?
                && !matches!(cmd.state.as_str(), "finished" | "rejected" | "unknown")
            {
                let body = cmd.configure()?;
                return Ok((body.choice, body.policy));
            }
            let o = config_in(c, session)?;
            Ok((o.choice, o.policy))
        })
    }

    /// Where the session's permission mode stands, as far as this Hub knows it: the mode of the
    /// last open or configure the Node finished with success (the harness confirmed it), the one
    /// that waits or is on its way, and what the Node or the harness said of a change after it
    /// that did not take. `None`: no such session on this Node.
    pub fn mode_state(&self, session: &str) -> Result<Option<steve_core::ModeState>, StoreError> {
        self.store.read(|r| {
            let Some(phase) = node_sessions::phase(r, session, Some(self.node_id.as_str()))? else {
                return Ok(None);
            };
            let applied = commands::latest(r, session, None, true)?;
            let mut state = steve_core::ModeState {
                applied: applied
                    .as_ref()
                    .map(|cmd| cmd.permissions().map(|mode| mode_str(mode).to_string()))
                    .transpose()?,
                busy: phase.active_attempt.is_some() || node_sessions::unfinished(r, session)?,
                released: phase.name == "released",
                ..Default::default()
            };
            // Only the newest command after the last success describes a pending change.
            if let Some(cmd) = commands::latest(r, session, None, false)?
                && applied
                    .as_ref()
                    .is_none_or(|applied| applied.operation != cmd.operation)
            {
                match cmd.state.as_str() {
                    "rejected" | "finished" => {
                        state.refused = Some(cmd.refusal.unwrap_or_else(|| "没有生效".into()));
                    }
                    "unknown" => state.unknown = true,
                    _ => {
                        state.desired = Some(mode_str(cmd.permissions()?).into());
                        state.sending = true;
                    }
                }
            }
            if !state.sending
                && let Some(desired) = node_sessions::desired_config::<Desired>(r, session)?
            {
                state.desired = Some(mode_str(desired.policy.permissions).into());
            }
            Ok(Some(state))
        })
    }

    /// Changes the session's harness config. It goes to the Node now if the session is idle,
    /// otherwise at its next boundary (never under a running attempt) or with its next open.
    pub fn configure(
        &self,
        session: &str,
        change: &ConfigChange,
    ) -> Result<Configured, StoreError> {
        self.running()?;
        let r = self.write(|tx| {
            let binding = self
                .store
                .read(|r| node_sessions::phase(r, session, None))?
                .ok_or_else(|| conflict("session binding missing"))?;
            let phase = binding.name;
            let active = binding.active_attempt;
            let prospective = self.prospective_in(session, &phase)?;
            let (mut choice, mut policy) = match desired_in(&self.store, session)? {
                Some(d) => (d.choice, d.policy),
                None => prospective.clone(),
            };
            if let Some(m) = &change.model {
                choice.model = m.clone();
            }
            if let Some(e) = &change.effort {
                choice.effort = e.clone();
            }
            if let Some(p) = change.permissions
                && p != policy.permissions
            {
                policy.permissions = p;
                policy.revision = idv(uid("pol"))?;
            }
            if same_config((&choice, &policy), (&prospective.0, &prospective.1)) {
                set_desired_in(&self.store, session, None)?;
                return Ok(Configured::Unchanged);
            }
            let text = describe(&choice, &policy);
            set_desired_in(
                &self.store,
                session,
                Some(&Desired {
                    choice,
                    policy,
                    refreshes: 0,
                    expected: None,
                }),
            )?;
            let unfinished = self.store.read(|r| node_sessions::unfinished(r, session))?;
            if !self.is_holding()
                && self.may_plan()
                && phase == "ready"
                && active.is_none()
                && !unfinished
                && self.configure_in(tx, session)?
            {
                return Ok(Configured::Sent);
            }
            if phase == "released" && active.is_none() {
                return Ok(Configured::OnReopen);
            }
            system_in(
                tx,
                session,
                &format!("配置变更将在当前回合结束后生效：{text}"),
            )?;
            Ok(Configured::Pending)
        })?;
        if r == Configured::Sent {
            self.wake_send.notify_one();
        }
        Ok(r)
    }

    /// Writes the `configure_session` command of the desired config and marks the session
    /// `configuring`. False: nothing desired, or the Node's revision of the session is not known.
    pub(super) fn configure_in(&self, tx: &mut W<'_>, session: &str) -> Result<bool, StoreError> {
        let Some(mut d) = desired_in(&self.store, session)? else {
            return Ok(false);
        };
        let expected = match d.expected.clone() {
            Some(r) => Some(r),
            None => self
                .store
                .read(|r| node_sessions::node_revision(r, session))?,
        };
        let Some(expected) = expected else {
            tracing::warn!(
                session,
                "no Node revision of the session: the configure waits"
            );
            return Ok(false);
        };
        let n = self
            .store
            .read(|r| commands::count(r, session, Method::Configure))?;
        let op = format!("op_cfg_{session}_{n}");
        let (req, digest) = command(
            &op,
            ConfigureSession {
                session: idv(session.to_string())?,
                expected,
                choice: d.choice.clone(),
                policy: d.policy.clone(),
            },
        )?;
        node_write::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "configure_session",
                request: &req,
                digest: &digest,
                target_attempt: None,
                dependency: None,
            },
            self.clock.now(),
        )?;
        d.expected = None;
        set_desired_in(&self.store, session, Some(&d))?;
        let rev = self.session_revision(session)?;
        if !node_write::session_cas_in(
            tx,
            session,
            rev,
            &rt::SessionUpdate {
                phase: "configuring",
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )? {
            return Err(conflict("the session changed while configuring"));
        }
        // the worker leases it once this transaction is committed
        self.wake_send.notify_one();
        Ok(true)
    }

    /// At an idle boundary of a ready session: a desired config goes before any turn. True: the
    /// session is configuring now (or let go to carry a narrowing), no turn may be planned.
    pub(super) fn boundary_in(&self, tx: &mut W<'_>, session: &str) -> Result<bool, StoreError> {
        if desired_in(&self.store, session)?.is_none() {
            return Ok(false);
        }
        if self.configure_in(tx, session)? {
            return Ok(true);
        }
        // it cannot be sent: a narrowing still holds (the session is let go and opens with it),
        // anything else falls back to the config in force
        self.config_not_applied_in(tx, session, false, "Node 没有这个会话的版本")?;
        let phase = self.session_phase(session)?;
        Ok(phase != "ready")
    }

    /// The open about to be sent carries the desired config (no configure needed after it).
    pub(super) fn fold_desired_in(
        &self,
        session: &str,
        open: &mut OpenSession,
    ) -> Result<(), StoreError> {
        if let Some(d) = desired_in(&self.store, session)? {
            open.choice = d.choice;
            open.policy = d.policy;
        }
        Ok(())
    }

    /// An open finished: the desired config it carried is in force (or, refused, falls back unless
    /// it narrows).
    pub(super) fn config_opened_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        op: &str,
        success: bool,
    ) -> Result<(), StoreError> {
        let Some(d) = desired_in(&self.store, session)? else {
            return Ok(());
        };
        let open: OpenSession = self.store.read(|r| commands::body(r, op))?;
        if !same_config((&d.choice, &d.policy), (&open.choice, &open.policy)) {
            return Ok(());
        }
        if success {
            set_desired_in(&self.store, session, None)?;
            system_in(
                tx,
                session,
                &format!("已切换：{}", describe(&d.choice, &d.policy)),
            )?;
        } else if !self.narrows_in(session, &d)? {
            set_desired_in(&self.store, session, None)?;
            system_in(tx, session, "未能切换：会话没能按新配置打开")?;
        }
        Ok(())
    }

    fn narrows_in(&self, session: &str, d: &Desired) -> Result<bool, StoreError> {
        let eff = self.open_of(session)?;
        Ok(rank(d.policy.permissions) < rank(eff.policy.permissions))
    }

    /// What the Node said about a configure (its answer or its stream). Accepted or running changes
    /// nothing; finished applies it or not; unknown keeps the session held, never resent.
    pub(super) fn configure_receipt_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        state: ReceiptState,
        success: bool,
        receipt: &Receipt,
    ) -> Result<(), StoreError> {
        let session = self.store.read(|r| commands::configure_session(r, op))?;
        match state {
            ReceiptState::Finished if success => {
                let sent: ConfigureSession = self.store.read(|r| commands::body(r, op))?;
                let rev = self.session_revision(&session)?;
                node_write::session_cas_in(
                    tx,
                    &session,
                    rev,
                    &rt::SessionUpdate {
                        phase: "ready",
                        active_attempt: None,
                        release_pending: false,
                    },
                    now(),
                )?;
                if let Some(d) = desired_in(&self.store, &session)?
                    && same_config((&d.choice, &d.policy), (&sent.choice, &sent.policy))
                {
                    set_desired_in(&self.store, &session, None)?;
                }
                system_in(
                    tx,
                    &session,
                    &format!("已切换：{}", describe(&sent.choice, &sent.policy)),
                )?;
                self.plan_after_in(tx, &session)?;
            }
            ReceiptState::Finished => {
                // the Node stops the harness to configure it: a configure that failed leaves the
                // session released there
                let detail = match &receipt.state {
                    Observed::Finished { error: Some(e), .. } => e.safe_detail.clone(),
                    _ => "配置没有生效".into(),
                };
                self.config_not_applied_in(tx, &session, true, &detail)?;
                self.plan_after_in(tx, &session)?;
            }
            ReceiptState::Unknown => {
                system_in(
                    tx,
                    &session,
                    &steve_i18n::stored(|| steve_i18n::t!("hub.node.config_unknown")),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The Node refused a configure before accepting it. `Some(session)`: it was for a stale
    /// revision, ask the Node for its view and send it again ([`Self::refresh_configure_in`]).
    pub(super) fn refused_configure_in(
        &self,
        tx: &mut W<'_>,
        op: &str,
        e: &ApiError,
    ) -> Result<Option<String>, StoreError> {
        let session = self.store.read(|r| commands::configure_session(r, op))?;
        if matches!(e.code, ErrorCode::Conflict | ErrorCode::Stale)
            && let Some(mut d) = desired_in(&self.store, &session)?
            && d.refreshes < MAX_REFRESHES
        {
            d.refreshes += 1;
            set_desired_in(&self.store, &session, Some(&d))?;
            return Ok(Some(session));
        }
        self.config_not_applied_in(tx, &session, false, &e.safe_detail)?;
        self.plan_after_in(tx, &session)?;
        Ok(None)
    }

    /// After a stale-revision refusal of `op`: sent again with the Node's own revision, as a new
    /// operation (the refused one was never accepted).
    pub(super) fn refresh_configure_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        op: &str,
        view: Option<SessionView>,
    ) -> Result<(), StoreError> {
        let phase = self.session_phase(session)?;
        if phase != "configuring"
            || self
                .store
                .read(|r| commands::latest(r, session, Some(Method::Configure), false))?
                .map(|command| command.operation)
                .as_deref()
                != Some(op)
        {
            return Ok(());
        }
        let set_phase = |tx: &mut W<'_>, p: &str| -> Result<(), StoreError> {
            let rev = self.session_revision(session)?;
            node_write::session_cas_in(
                tx,
                session,
                rev,
                &rt::SessionUpdate {
                    phase: p,
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )?;
            Ok(())
        };
        match view {
            Some(v) if v.phase == SessionPhase::Ready && v.active.is_none() => {
                if let Some(mut d) = desired_in(&self.store, session)? {
                    d.expected = Some(v.revision);
                    set_desired_in(&self.store, session, Some(&d))?;
                }
                if !self.configure_in(tx, session)? {
                    set_phase(tx, "ready")?;
                    self.plan_after_in(tx, session)?;
                }
            }
            Some(v) if v.phase == SessionPhase::Released => set_phase(tx, "released")?,
            _ => {
                set_phase(tx, "ready")?;
                self.plan_after_in(tx, session)?;
            }
        }
        Ok(())
    }

    /// A configure did not take effect. A narrowing still holds: the session never takes another
    /// turn wider than asked (if the Node keeps it open, it is let go and opens with the narrower
    /// policy). A widening or a model change falls back to the config in force.
    pub(super) fn config_not_applied_in(
        &self,
        tx: &mut W<'_>,
        session: &str,
        node_released: bool,
        detail: &str,
    ) -> Result<(), StoreError> {
        let narrowing = match desired_in(&self.store, session)? {
            Some(d) => self.narrows_in(session, &d)?,
            None => false,
        };
        if !narrowing {
            set_desired_in(&self.store, session, None)?;
        }
        let phase = if node_released {
            "released"
        } else if narrowing {
            let n = self
                .store
                .read(|r| commands::count(r, session, Method::Release))?;
            let op = format!("op_release_{session}_{n}");
            let (req, digest) = command(
                &op,
                SessionId::try_from(session.to_string()).map_err(conflict)?,
            )?;
            node_write::insert_command_in(
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
                self.clock.now(),
            )?;
            self.wake_send.notify_one();
            "releasing"
        } else {
            "ready"
        };
        let rev = self.session_revision(session)?;
        node_write::session_cas_in(
            tx,
            session,
            rev,
            &rt::SessionUpdate {
                phase,
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        let text = if narrowing && !node_released {
            format!("未能切换：{detail}，已释放会话，下次打开时按新配置")
        } else {
            format!("未能切换：{detail}")
        };
        system_in(tx, session, &text)
    }

    fn session_phase(&self, session: &str) -> Result<String, StoreError> {
        self.store
            .read(|r| node_sessions::phase(r, session, None))?
            .map(|phase| phase.name)
            .ok_or_else(|| conflict("session binding missing"))
    }

    /// At start: a configure refused just before the Hub stopped (its refresh never came).
    pub(super) fn settle_configuring(&self) {
        let r = self.write(|tx| {
            let sessions = self
                .store
                .read(|r| node_sessions::configuring_on(r, self.node_id.as_str()))?;
            for s in sessions {
                if self
                    .store
                    .read(|r| commands::latest(r, &s, Some(Method::Configure), false))?
                    .is_some_and(|command| command.state == "rejected")
                {
                    self.config_not_applied_in(tx, &s, false, "配置被拒绝")?;
                    self.plan_after_in(tx, &s)?;
                }
            }
            Ok(())
        });
        if let Err(e) = r {
            tracing::error!("settle configuring sessions: {e}");
        }
    }
}

//! A Node session's harness config (D5): permission mode, model and effort change through the
//! Node's `configure_session`, only at an idle boundary, never under a running attempt.
//!
//! Three configs: the effective one (the last open or configure the Node finished with success:
//! what turns carry), the one in flight (a `configure_session` command, session phase
//! `configuring`) and the desired one (`runtime_sessions.desired_config`, durable). A configure
//! whose outcome is unknown keeps the session `configuring` and is never sent again.

use super::*;
use serde::{Deserialize, Serialize};

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

fn describe(c: &HarnessChoice, p: &ExecutionPolicy) -> String {
    let mode = match p.permissions {
        PermissionMode::Plan => "plan",
        PermissionMode::Ask => "ask",
        PermissionMode::AcceptEdits => "accept_edits",
        PermissionMode::Auto => "auto",
        PermissionMode::FullAccess => "full_access",
    };
    let mut s = format!("权限 {mode}");
    if let Some(m) = &c.model {
        s.push_str(&format!("，模型 {m}"));
    }
    if let Some(e) = &c.effort {
        s.push_str(&format!("，推理 {e}"));
    }
    s
}

fn system_in(tx: &Tx, session: &str, text: &str) -> Result<(), StoreError> {
    rt::add_event_in(
        tx,
        session,
        "system",
        json!({ "text": text }),
        V1,
        None,
        now(),
    )?;
    Ok(())
}

pub(super) fn desired_in(tx: &Tx, session: &str) -> Result<Option<Desired>, StoreError> {
    Ok(match rt::desired_config_in(tx, session)? {
        Some(v) => Some(serde_json::from_value(v)?),
        None => None,
    })
}

fn set_desired_in(tx: &Tx, session: &str, d: Option<&Desired>) -> Result<(), StoreError> {
    let v = d
        .map(|d| serde_json::to_value(d).map_err(conflict))
        .transpose()?;
    rt::set_desired_config_in(tx, session, v.as_ref(), now())
}

/// The latest `configure_session` command of the session: (op, state, body).
fn latest_configure_in(
    tx: &Tx,
    session: &str,
) -> Result<Option<(String, String, ConfigureSession)>, StoreError> {
    let r: Option<(String, String, String)> = match tx.query_row(
        "SELECT operation_id, state, request FROM node_commands WHERE method='configure_session' AND json_extract(request,'$.body.session')=? ORDER BY created DESC, rowid DESC LIMIT 1",
        [session],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ) {
        Ok(x) => Some(x),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    r.map(|(op, state, req)| {
        let c: Command<ConfigureSession> = serde_json::from_str(&req)?;
        Ok((op, state, c.body))
    })
    .transpose()
}

impl NodeRuntime {
    /// The session's config in force: the last open or configure the Node finished with success
    /// (a configure overrides the open's choice and policy). Before any did, the latest open.
    pub(super) fn config_of_in(&self, tx: &Tx, session: &str) -> Result<OpenSession, StoreError> {
        let latest_open = |only_ok: bool| -> Result<Option<OpenSession>, StoreError> {
            let sql = if only_ok {
                "SELECT request FROM node_commands WHERE method='open_session' AND json_extract(request,'$.body.session')=? AND state='finished' AND json_extract(receipt,'$.success')=1 ORDER BY created DESC, rowid DESC LIMIT 1"
            } else {
                "SELECT request FROM node_commands WHERE method='open_session' AND json_extract(request,'$.body.session')=? ORDER BY created DESC, rowid DESC LIMIT 1"
            };
            match tx.query_row(sql, [session], |r| r.get::<_, String>(0)) {
                Ok(req) => Ok(Some(
                    serde_json::from_str::<Command<OpenSession>>(&req)?.body,
                )),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        let last: Option<(String, String)> = match tx.query_row(
            "SELECT method, request FROM node_commands WHERE method IN ('open_session','configure_session') AND json_extract(request,'$.body.session')=? AND state='finished' AND json_extract(receipt,'$.success')=1 ORDER BY created DESC, rowid DESC LIMIT 1",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(x) => Some(x),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        match last {
            Some((m, req)) if m == "open_session" => {
                Ok(serde_json::from_str::<Command<OpenSession>>(&req)?.body)
            }
            Some((_, req)) => {
                let c: Command<ConfigureSession> = serde_json::from_str(&req)?;
                let mut open = latest_open(true)?
                    .or(latest_open(false)?)
                    .ok_or_else(|| conflict("configured but never opened"))?;
                open.choice = c.body.choice;
                open.policy = c.body.policy;
                Ok(open)
            }
            None => latest_open(false)?.ok_or_else(|| conflict("the session was never opened")),
        }
    }

    /// What the session will run with once what is in flight lands.
    fn prospective_in(
        &self,
        tx: &Tx,
        session: &str,
        phase: &str,
    ) -> Result<(HarnessChoice, ExecutionPolicy), StoreError> {
        if phase == "configuring"
            && let Some((_, state, body)) = latest_configure_in(tx, session)?
            && !matches!(state.as_str(), "finished" | "rejected" | "unknown")
        {
            return Ok((body.choice, body.policy));
        }
        let o = self.config_of_in(tx, session)?;
        Ok((o.choice, o.policy))
    }

    /// The revision of the Node's last view of the session.
    fn node_revision_in(&self, tx: &Tx, session: &str) -> Result<Option<Revision>, StoreError> {
        // as applied from the stream, in its order (the inbox below is pruned)
        let kept: Option<String> = tx
            .query_row(
                "SELECT node_revision FROM runtime_sessions WHERE session_id=?",
                [session],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(r) = kept.and_then(|r| Revision::try_from(r).ok()) {
            return Ok(Some(r));
        }
        let r: Option<String> = match tx.query_row(
            "SELECT json_extract(payload,'$.body.view.revision') FROM node_event_inbox
               WHERE node_id=? AND json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.id')=?
               ORDER BY received DESC, rowid DESC LIMIT 1",
            [self.node_id.as_str(), session],
            |r| r.get(0),
        ) {
            Ok(x) => x,
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(r.and_then(|r| Revision::try_from(r).ok()))
    }

    /// Changes the session's harness config. It goes to the Node now if the session is idle,
    /// otherwise at its next boundary (never under a running attempt) or with its next open.
    pub fn configure(
        &self,
        session: &str,
        change: &ConfigChange,
    ) -> Result<Configured, StoreError> {
        self.running()?;
        let r = self.tx(|tx| {
            let (phase, active): (String, Option<String>) = tx.query_row(
                "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let prospective = self.prospective_in(tx, session, &phase)?;
            let (mut choice, mut policy) = match desired_in(tx, session)? {
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
                set_desired_in(tx, session, None)?;
                return Ok(Configured::Unchanged);
            }
            let text = describe(&choice, &policy);
            set_desired_in(
                tx,
                session,
                Some(&Desired {
                    choice,
                    policy,
                    refreshes: 0,
                    expected: None,
                }),
            )?;
            let unfinished: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_turns WHERE session_id=? AND state<>'finished')",
                [session],
                |r| r.get(0),
            )?;
            // nothing goes out under hold (or persisted maintenance) or before reconciliation
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
    pub(super) fn configure_in(&self, tx: &Tx, session: &str) -> Result<bool, StoreError> {
        let Some(mut d) = desired_in(tx, session)? else {
            return Ok(false);
        };
        let expected = match d.expected.clone() {
            Some(r) => Some(r),
            None => self.node_revision_in(tx, session)?,
        };
        let Some(expected) = expected else {
            tracing::warn!(
                session,
                "no Node revision of the session: the configure waits"
            );
            return Ok(false);
        };
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM node_commands WHERE method='configure_session' AND json_extract(request,'$.body.session')=?",
            [session],
            |r| r.get(0),
        )?;
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
        rt::insert_command_in(
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
            now(),
        )?;
        d.expected = None;
        set_desired_in(tx, session, Some(&d))?;
        let rev = session_revision(tx, session)?;
        if !rt::session_cas_in(
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
    pub(super) fn boundary_in(&self, tx: &Tx, session: &str) -> Result<bool, StoreError> {
        if desired_in(tx, session)?.is_none() {
            return Ok(false);
        }
        if self.configure_in(tx, session)? {
            return Ok(true);
        }
        // it cannot be sent: a narrowing still holds (the session is let go and opens with it),
        // anything else falls back to the config in force
        self.config_not_applied_in(tx, session, false, "Node 没有这个会话的版本")?;
        let phase: String = tx.query_row(
            "SELECT phase FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| r.get(0),
        )?;
        Ok(phase != "ready")
    }

    /// The open about to be sent carries the desired config (no configure needed after it).
    pub(super) fn fold_desired_in(
        &self,
        tx: &Tx,
        session: &str,
        open: &mut OpenSession,
    ) -> Result<(), StoreError> {
        if let Some(d) = desired_in(tx, session)? {
            open.choice = d.choice;
            open.policy = d.policy;
        }
        Ok(())
    }

    /// An open finished: the desired config it carried is in force (or, refused, falls back unless
    /// it narrows).
    pub(super) fn config_opened_in(
        &self,
        tx: &Tx,
        session: &str,
        op: &str,
        success: bool,
    ) -> Result<(), StoreError> {
        let Some(d) = desired_in(tx, session)? else {
            return Ok(());
        };
        let req: String = tx.query_row(
            "SELECT request FROM node_commands WHERE operation_id=?",
            [op],
            |r| r.get(0),
        )?;
        let open: Command<OpenSession> = serde_json::from_str(&req)?;
        if !same_config(
            (&d.choice, &d.policy),
            (&open.body.choice, &open.body.policy),
        ) {
            return Ok(());
        }
        if success {
            set_desired_in(tx, session, None)?;
            system_in(
                tx,
                session,
                &format!("已切换：{}", describe(&d.choice, &d.policy)),
            )?;
        } else if !self.narrows_in(tx, session, &d)? {
            set_desired_in(tx, session, None)?;
            system_in(tx, session, "未能切换：会话没能按新配置打开")?;
        }
        Ok(())
    }

    fn narrows_in(&self, tx: &Tx, session: &str, d: &Desired) -> Result<bool, StoreError> {
        let eff = self.config_of_in(tx, session)?;
        Ok(rank(d.policy.permissions) < rank(eff.policy.permissions))
    }

    /// What the Node said about a configure (its answer or its stream). Accepted or running changes
    /// nothing; finished applies it or not; unknown keeps the session held, never resent.
    pub(super) fn configure_receipt_in(
        &self,
        tx: &Tx,
        op: &str,
        state: ReceiptState,
        success: bool,
        receipt: &Receipt,
    ) -> Result<(), StoreError> {
        let session: String = tx.query_row(
            "SELECT json_extract(request,'$.body.session') FROM node_commands WHERE operation_id=?",
            [op],
            |r| r.get(0),
        )?;
        match state {
            ReceiptState::Finished if success => {
                let req: String = tx.query_row(
                    "SELECT request FROM node_commands WHERE operation_id=?",
                    [op],
                    |r| r.get(0),
                )?;
                let sent: Command<ConfigureSession> = serde_json::from_str(&req)?;
                let rev = session_revision(tx, &session)?;
                rt::session_cas_in(
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
                if let Some(d) = desired_in(tx, &session)?
                    && same_config(
                        (&d.choice, &d.policy),
                        (&sent.body.choice, &sent.body.policy),
                    )
                {
                    set_desired_in(tx, &session, None)?;
                }
                system_in(
                    tx,
                    &session,
                    &format!("已切换：{}", describe(&sent.body.choice, &sent.body.policy)),
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
                system_in(tx, &session, &steve_i18n::t!("hub.node.config_unknown"))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The Node refused a configure before accepting it. `Some(session)`: it was for a stale
    /// revision, ask the Node for its view and send it again ([`Self::refresh_configure_in`]).
    pub(super) fn refused_configure_in(
        &self,
        tx: &Tx,
        op: &str,
        e: &ApiError,
    ) -> Result<Option<String>, StoreError> {
        let session: String = tx.query_row(
            "SELECT json_extract(request,'$.body.session') FROM node_commands WHERE operation_id=?",
            [op],
            |r| r.get(0),
        )?;
        if matches!(e.code, ErrorCode::Conflict | ErrorCode::Stale)
            && let Some(mut d) = desired_in(tx, &session)?
            && d.refreshes < MAX_REFRESHES
        {
            d.refreshes += 1;
            set_desired_in(tx, &session, Some(&d))?;
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
        tx: &Tx,
        session: &str,
        op: &str,
        view: Option<SessionView>,
    ) -> Result<(), StoreError> {
        let phase: String = tx.query_row(
            "SELECT phase FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| r.get(0),
        )?;
        if phase != "configuring"
            || latest_configure_in(tx, session)?
                .map(|(o, _, _)| o)
                .as_deref()
                != Some(op)
        {
            return Ok(());
        }
        let set_phase = |p: &str| -> Result<(), StoreError> {
            let rev = session_revision(tx, session)?;
            rt::session_cas_in(
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
                if let Some(mut d) = desired_in(tx, session)? {
                    d.expected = Some(v.revision);
                    set_desired_in(tx, session, Some(&d))?;
                }
                if !self.configure_in(tx, session)? {
                    set_phase("ready")?;
                    self.plan_after_in(tx, session)?;
                }
            }
            Some(v) if v.phase == SessionPhase::Released => set_phase("released")?,
            _ => {
                set_phase("ready")?;
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
        tx: &Tx,
        session: &str,
        node_released: bool,
        detail: &str,
    ) -> Result<(), StoreError> {
        let narrowing = match desired_in(tx, session)? {
            Some(d) => self.narrows_in(tx, session, &d)?,
            None => false,
        };
        if !narrowing {
            set_desired_in(tx, session, None)?;
        }
        let phase = if node_released {
            "released"
        } else if narrowing {
            let n: i64 = tx.query_row(
                "SELECT COUNT(*) FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=?",
                [session],
                |r| r.get(0),
            )?;
            let op = format!("op_release_{session}_{n}");
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
            self.wake_send.notify_one();
            "releasing"
        } else {
            "ready"
        };
        let rev = session_revision(tx, session)?;
        rt::session_cas_in(
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

    /// At start: a configure refused just before the Hub stopped (its refresh never came).
    pub(super) fn settle_configuring(&self) {
        let r = self.tx(|tx| {
            let mut st = tx.prepare(
                "SELECT session_id FROM runtime_sessions WHERE node_id=? AND phase='configuring'",
            )?;
            let sessions = st
                .query_map([self.node_id.as_str()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(st);
            for s in sessions {
                if let Some((_, state, _)) = latest_configure_in(tx, &s)?
                    && state == "rejected"
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

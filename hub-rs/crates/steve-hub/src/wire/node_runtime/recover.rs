//! Explicit recover of a control operation whose outcome is unknown (D7). Nothing retires one on
//! its own: the user's Stop ([`NodeRuntime::user_stop`], never the lifecycle's cancels) on a session
//! held by one plans exactly one `recover`, its target and the target's
//! expected digest fixed when it is planned. The target is never sent again. A recover proves only
//! that the control intent is retired: its effect on the Node (a worktree, an answer) stays
//! unknown, and what the session hears never says it succeeded.
//!
//! What a finished recover does to its target:
//! - open, release, configure: the session is let go (the next message opens it again);
//! - prepare: a new prepare under a new id, and the open waiting on it again after it;
//! - abandon: only its slot is freed; the attempt stays unknown, the next Stop abandons it anew;
//! - resolve_permission: its card closes as unknown; the attempt is not touched.
//!
//! A recover that fails (unsupported, or the Node could not prove the target stopped) leaves the
//! session held; the next Stop asks again under a new id.

use super::*;

/// The controls Stop may retire.
const RECOVERABLE: [&str; 6] = [
    "open_session",
    "release_session",
    "configure_session",
    "prepare_workspace",
    "abandon",
    "resolve_permission",
];

/// SQL, for a command aliased `c`: a recover of it finished with success.
pub(super) const RETIRED: &str = "EXISTS(SELECT 1 FROM node_commands r WHERE r.method='recover' AND r.node_id=c.node_id AND json_extract(r.request,'$.body.target')=c.operation_id AND r.state='finished' AND json_extract(r.receipt,'$.success')=1)";

/// What a session reads when a recover did not prove anything.
const UNPROVEN: &str = "需要人工核验 Node 状态";

/// How a control reads in a session note.
fn what(method: &str) -> &'static str {
    match method {
        "open_session" => "打开会话",
        "release_session" => "释放会话",
        "configure_session" => "配置切换",
        "prepare_workspace" => "工作区准备",
        "abandon" => "放弃操作",
        "resolve_permission" => "授权回复",
        _ => "操作",
    }
}

impl NodeRuntime {
    /// Stop on `session` when no attempt can be cancelled or abandoned (`attempt`: the unknown
    /// attempt whose own abandon is unknown, the only control retired then). `Some(true)`: a
    /// recover was planned, or one is on its way already (nothing new is sent). `None`: no unknown
    /// control holds the session.
    pub(super) fn recover_in(
        &self,
        tx: &Tx,
        session: &str,
        attempt: Option<&str>,
    ) -> Result<Option<bool>, StoreError> {
        // one recover at a time per session: a second Stop while it is on its way sends nothing
        let live: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT json_extract(request,'$.body.target') FROM node_commands WHERE node_id=? AND method='recover' AND state IN ('pending','accepted','active')",
            )?;
            st.query_map([self.node_id.as_str()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for target in live {
            if command_any_session_in(tx, &target)?.as_deref() == Some(session) {
                return Ok(Some(true));
            }
        }
        let candidates: Vec<(String, String, String, Option<String>)> = {
            let mut st = tx.prepare(&format!(
                "SELECT c.operation_id, c.method, c.request, c.target_attempt FROM node_commands c WHERE c.node_id=? AND c.state='unknown' AND NOT {RETIRED} ORDER BY c.created, c.rowid"
            ))?;
            st.query_map([self.node_id.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        for (target, method, request, target_attempt) in candidates {
            if !RECOVERABLE.contains(&method.as_str()) {
                continue;
            }
            if let Some(a) = attempt
                && (method != "abandon" || target_attempt.as_deref() != Some(a))
            {
                continue;
            }
            if command_any_session_in(tx, &target)?.as_deref() != Some(session) {
                continue;
            }
            self.plan_recover_in(tx, &target, &method, &request, target_attempt.as_deref())?;
            return Ok(Some(true));
        }
        Ok(None)
    }

    /// The `recover` of `target`, frozen now: the target's id and the digest the Node keeps of it.
    fn plan_recover_in(
        &self,
        tx: &Tx,
        target: &str,
        method: &str,
        request: &str,
        target_attempt: Option<&str>,
    ) -> Result<(), StoreError> {
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM node_commands WHERE method='recover' AND json_extract(request,'$.body.target')=?",
            [target],
            |r| r.get(0),
        )?;
        let mut op = format!("op_recover_{target}_{n}");
        if op.len() > 120 {
            op = derived_id("op_recover", &op);
        }
        // the Node keeps the command's content hash (method and business fields, not the id)
        let expected = serde_json::from_str::<Command<Value>>(request)
            .map_err(conflict)?
            .digest(method)
            .map_err(conflict)?;
        let (req, digest) = command(
            &op,
            Recover {
                target: idv(target.to_string())?,
                expected,
                reason: "the user stopped a session held by a control whose outcome is unknown"
                    .into(),
            },
        )?;
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "recover",
                request: &req,
                digest: &digest,
                target_attempt,
                dependency: None,
            },
            now(),
        )?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// `op`'s target is being retired by a recover that may be at the Node already: its own end
    /// (`interrupted`, as a recover ends it) changes nothing here, what follows comes with the
    /// recover's end ([`Self::recover_receipt_in`]). The Node may push that end before the Hub
    /// merged the recover's answer, so a recover still pending counts once it was (maybe) sent.
    /// Any other end is the target's own and is taken as usual (the recover then finds it stale).
    pub(super) fn retiring_in(
        &self,
        tx: &Tx,
        op: &str,
        interrupted: bool,
    ) -> Result<bool, StoreError> {
        if !interrupted {
            return Ok(false);
        }
        Ok(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM node_commands WHERE method='recover' AND node_id=? AND json_extract(request,'$.body.target')=?
               AND (state IN ('accepted','active','unknown')
                    OR (state='pending' AND (maybe_sent=1 OR lease_owner IS NOT NULL))
                    OR (state='finished' AND json_extract(receipt,'$.success')=1)))",
            [self.node_id.as_str(), op],
            |r| r.get(0),
        )?)
    }

    /// What the Node said about a recover (its answer or its stream).
    pub(super) fn recover_receipt_in(
        &self,
        tx: &Tx,
        op: &str,
        state: ReceiptState,
        success: bool,
        detail: &str,
    ) -> Result<(), StoreError> {
        match state {
            ReceiptState::Finished if success => self.retired_in(tx, op),
            ReceiptState::Finished | ReceiptState::Unknown => {
                self.recover_failed_in(tx, op, detail)
            }
            _ => Ok(()),
        }
    }

    /// A recover the Node refused or could not finish: the session stays held, nothing is sent
    /// again on its own; the next Stop asks under a new id.
    pub(super) fn recover_failed_in(
        &self,
        tx: &Tx,
        op: &str,
        detail: &str,
    ) -> Result<(), StoreError> {
        let target: String = tx.query_row(
            "SELECT json_extract(request,'$.body.target') FROM node_commands WHERE operation_id=?",
            [op],
            |r| r.get(0),
        )?;
        let Some(session) = command_any_session_in(tx, &target)? else {
            return Ok(());
        };
        system_in(
            tx,
            &session,
            &format!(
                "撤回结果未知的操作没有成功（{detail}），{UNPROVEN}；会话仍然暂停，可以再点一次停止重试。"
            ),
        )
    }

    /// The Node retired the recover's target (interrupted, its effects kept as they are).
    fn retired_in(&self, tx: &Tx, op: &str) -> Result<(), StoreError> {
        let (target, method): (String, String) = tx.query_row(
            "SELECT c.operation_id, c.method FROM node_commands r JOIN node_commands c ON c.operation_id=json_extract(r.request,'$.body.target') WHERE r.operation_id=?",
            [op],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let Some(session) = command_any_session_in(tx, &target)? else {
            return Ok(());
        };
        let what = what(&method);
        match method.as_str() {
            "open_session" | "release_session" | "configure_session" => {
                let held = match method.as_str() {
                    "open_session" => "opening",
                    "release_session" => "releasing",
                    _ => "configuring",
                };
                if phase_in(tx, &session)?.is_none_or(|(p, a)| p != held || a.is_some()) {
                    return Ok(());
                }
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
                system_in(
                    tx,
                    &session,
                    &format!(
                        "已撤回结果未知的{what}；它在 Node 上的实际效果仍无法确定（不算成功）。会话已释放，下一条消息会重新打开。"
                    ),
                )?;
                // what waits for the session (queued, or a turn that waited for an open) opens it
                let waiting: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM queued WHERE session_id=?1) OR EXISTS(SELECT 1 FROM runtime_turns WHERE session_id=?1 AND state<>'finished' AND active_attempt IS NULL)",
                    [&session],
                    |r| r.get(0),
                )?;
                if waiting && !self.is_holding() {
                    self.reopen_after_in(tx, &session, None)?;
                }
            }
            "prepare_workspace" => {
                // the open that waited on it never goes: it waits on a new prepare instead
                let opens: Vec<String> = {
                    let mut st = tx.prepare(
                        "SELECT operation_id FROM node_commands WHERE dependency_operation=? AND state='pending'",
                    )?;
                    st.query_map([&target], |r| r.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?
                };
                for o in &opens {
                    rt::merge_receipt_in(
                        tx,
                        o,
                        ReceiptState::Rejected,
                        false,
                        &json!({ "reason": "its prepare was retired: it waits on a new one" }),
                        now(),
                    )?;
                }
                system_in(
                    tx,
                    &session,
                    &format!(
                        "已撤回结果未知的{what}；它在 Node 上的实际效果仍无法确定（不算成功），会重新准备工作区再打开会话。"
                    ),
                )?;
                if !opens.is_empty() {
                    let req: String = tx.query_row(
                        "SELECT request FROM node_commands WHERE operation_id=?",
                        [&target],
                        |r| r.get(0),
                    )?;
                    let body = serde_json::from_str::<Command<Prepare>>(&req)?.body;
                    let prep = uid(&format!("op_prep_{session}"));
                    let (req, digest) = command(&prep, body)?;
                    rt::insert_command_in(
                        tx,
                        &rt::NewCommand {
                            operation: &prep,
                            node: self.node_id.as_str(),
                            method: "prepare_workspace",
                            request: &req,
                            digest: &digest,
                            target_attempt: None,
                            dependency: None,
                        },
                        now(),
                    )?;
                    self.reopen_after_in(tx, &session, Some(&prep))?;
                    self.wake_send.notify_one();
                }
            }
            "abandon" => system_in(
                tx,
                &session,
                &format!(
                    "已撤回结果未知的{what}；这次执行的结果仍然未知，可以再点一次停止放弃它。"
                ),
            )?,
            "resolve_permission" => {
                if let Some(row) = rt::permission_of_answer_in(tx, &target)? {
                    self.close_in(tx, &row, "结果未知")?;
                }
                system_in(
                    tx,
                    &session,
                    &format!("已撤回结果未知的{what}；这次授权是否生效仍无法确定。"),
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}

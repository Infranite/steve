//! Capacity waits belong to the logical turn. Both the jittered send time and the absolute
//! ceiling survive restarts in the existing command ledger; the Node protocol stays unchanged.

use super::*;

pub(super) const UNAVAILABLE: &str = steve_core::runtime::MODEL_SERVICE_UNAVAILABLE;
pub(super) const RETRIES: i64 = 3;
const MAX_WAIT_MS: i64 = 15 * 60_000;
const BACKOFF_MS: [i64; 3] = [20_000, 60_000, 120_000];

pub(super) struct CapacityWait {
    pub at: i64,
    pub deadline: i64,
    pub error: String,
    pub retries: i64,
}

impl NodeRuntime {
    pub(super) fn capacity_wait_in(
        &self,
        tx: &Tx,
        turn: &str,
        ordinal: i64,
        c: &Completion,
    ) -> Result<Option<CapacityWait>, StoreError> {
        let first: i64 = tx.query_row(
            "SELECT COALESCE(MIN(updated),?2) FROM runtime_attempts WHERE turn_id=?1 AND json_extract(completion,'$.stop_reason')=?3",
            rusqlite::params![turn, self.clock.now(), UNAVAILABLE],
            |r| r.get(0),
        )?;
        let at = self.clock.now();
        let deadline = first.saturating_add(MAX_WAIT_MS);
        if ordinal >= RETRIES || at >= deadline {
            return Ok(None);
        }
        let error = c
            .error
            .as_ref()
            .map(|e| e.safe_detail.as_str())
            .unwrap_or("预计恢复时间：未提供有效时间");
        let recovery = steve_acp::capacity_recovery_at(error, at);
        // Settlement can reach the Hub after the advertised instant. That is still a valid
        // recovery hint: retry after jitter now, rather than beginning another full backoff.
        let base = recovery
            .map(|t| t.max(at))
            .unwrap_or_else(|| at.saturating_add(BACKOFF_MS[ordinal as usize]));
        // Each attempt samples independently, then persists the result. Shared recovery instants
        // do not become a shared wakeup; replay and restart do not re-roll or extend the wait.
        let jitter = 1_000 + (uuid::Uuid::new_v4().as_u128() % 4_001) as i64;
        Ok(Some(CapacityWait {
            at: base.saturating_add(jitter).min(deadline),
            deadline,
            error: error.to_string(),
            retries: ordinal,
        }))
    }

    /// Expire only commands that provably never left this Hub, before leasing/sending them. If
    /// the advertised recovery lies beyond the ceiling, waiting ends here without an early call.
    /// A Hub restarted after the ceiling likewise raises the card instead of making a late call.
    pub(super) fn expire_capacity_waits_in(&self, tx: &Tx) -> Result<(), StoreError> {
        let rows = tx.prepare(
            "SELECT c.operation_id,a.attempt_id,t.session_id,json_extract(c.receipt,'$.provider_capacity.error'),
                    json_extract(c.receipt,'$.provider_capacity.deadline'),json_extract(c.receipt,'$.provider_capacity.retries')
             FROM node_commands c JOIN runtime_attempts a ON a.attempt_id=c.target_attempt
             JOIN runtime_turns t ON t.turn_id=a.turn_id
             WHERE c.node_id=?1 AND c.method='start_turn' AND c.state='pending' AND c.maybe_sent=0
             AND a.state='planned' AND json_extract(c.receipt,'$.provider_capacity.deadline') IS NOT NULL",
        )?.query_map([self.node_id.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?))
        })?.collect::<Result<Vec<_>, _>>()?;
        for (op, attempt, session, error, deadline, retries) in rows {
            let expired = deadline <= self.clock.now();
            // A receipt/configuration may arrive while the provider wait is pending. Recheck
            // before every lease, rather than relying only on the boundary at failure time.
            if !expired
                && self.stream_retry_allowed_in(tx, &session)?
                && config::desired_in(tx, &session)?.is_none()
            {
                continue;
            }
            let error = if expired {
                format!("已自动重试 {retries} 次，等待已达 15 分钟上限；{error}")
            } else {
                error
            };
            rt::merge_receipt_in(
                tx,
                &op,
                ReceiptState::Rejected,
                false,
                &json!({"reason": error}),
                self.clock.now(),
            )?;
            let completion = Completion {
                outcome: Outcome::Failed,
                stop_reason: UNAVAILABLE.into(),
                error: Some(ApiError::new(ErrorCode::Internal, error)),
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
                job: None,
            };
            self.settle_in(tx, &session, &attempt, &completion)?;
        }
        Ok(())
    }
}

pub(super) fn capacity_retry_prompt(error: &str, retry: i64) -> Vec<Part> {
    vec![Part::Text {
        text: format!(
            "（Steve）上一回合因模型服务容量不足而结束：{error}。这是临时服务执行错误，已按预计恢复时间或退避等待后自动重试（第 {retry} 次），不占用未交付续跑次数。先核对现状再接着完成原目标；已经完成的工具操作、推送、合并、打 tag、发布不要重做，也不要把结果未知的操作当作成功。做完用 steve_deliver 交付，需要用户决定时用 steve_ask。"
        ),
    }]
}

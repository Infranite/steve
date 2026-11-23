//! Provider service failures use the logical turn's durable attempts/backoff, including when tools
//! already ran. They resume with a reminder to inspect effects, never by replaying the original task.

use super::*;
use steve_store::node_turns::retry;

pub(super) const STREAM_INTERRUPTED: &str = steve_core::runtime::MODEL_STREAM_INTERRUPTED;

impl NodeRuntime {
    /// Normalize before saving the attempt, so recovery and the eventual turn end agree. Older
    /// Nodes/adapters can report success with only a diagnostic in the final assistant text.
    pub(super) fn stream_completion_in(
        &self,
        tx: &Tx,
        session: &str,
        attempt: &str,
        mut c: Completion,
    ) -> Result<Completion, StoreError> {
        if c.outcome == Outcome::Cancelled
            || c.outcome == Outcome::TimedOut
            || matches!(c.stop_reason.as_str(), "cancelled" | "abandoned")
            || c.stop_reason == steve_node_api::NODE_RESTARTED
            || c.error.as_ref().is_some_and(|e| NodeStop::of(e).is_some())
        {
            return Ok(c);
        }
        let error = c
            .error
            .as_ref()
            .map(|e| e.safe_detail.as_str())
            .filter(|e| !e.trim().is_empty());
        let tail = if matches!(c.stop_reason.as_str(), "end_turn" | "error") && error.is_none() {
            tx.write(|w| retry::assistant_tail(w, self.node_id.as_str(), session, attempt))?
        } else {
            String::new()
        };
        let capacity = if error.is_some_and(steve_acp::is_capacity_error) {
            error.map(String::from)
        } else if matches!(
            c.stop_reason.as_str(),
            "service_unavailable"
                | "provider_unavailable"
                | "model_service_unavailable"
                | "overloaded"
                | "503"
        ) {
            Some(error.unwrap_or(&c.stop_reason).to_string())
        } else if matches!(c.stop_reason.as_str(), "end_turn" | "error") && error.is_none() {
            steve_acp::capacity_error_tail(&tail).map(String::from)
        } else {
            None
        };
        if let Some(e) = capacity {
            c.outcome = Outcome::Failed;
            c.stop_reason = capacity_retry::UNAVAILABLE.into();
            c.error = Some(ApiError::new(
                ErrorCode::Internal,
                steve_acp::capacity_summary(&e, self.clock.now()),
            ));
            c.transient = true;
            return Ok(c);
        }
        let evidence = if error.is_some_and(steve_acp::is_stream_interruption) {
            error.map(String::from)
        } else if steve_acp::is_stream_interruption(&c.stop_reason) {
            Some(error.unwrap_or(&c.stop_reason).to_string())
        } else if matches!(c.stop_reason.as_str(), "end_turn" | "error") && error.is_none() {
            steve_acp::stream_interruption_tail(&tail).map(String::from)
        } else {
            None
        };
        if let Some(e) = evidence {
            c.outcome = Outcome::Failed;
            c.stop_reason = STREAM_INTERRUPTED.into();
            c.error = Some(ApiError::new(
                ErrorCode::Internal,
                e.chars().take(300).collect::<String>(),
            ));
            c.transient = true;
        }
        Ok(c)
    }

    /// A question, approval or committed delivery is already waiting on its own next step. A
    /// service failure after it must not start work past that boundary (or resurrect an ended item).
    pub(super) fn stream_retry_allowed_in(&self, session: &str) -> Result<bool, StoreError> {
        self.store.read(|r| retry::allowed(r, session))
    }
}

pub(super) fn stream_retry_prompt(error: &str, retry: i64) -> Vec<Part> {
    vec![Part::Text {
        text: format!(
            "（Steve）上一回合因模型服务或传输中断而结束：{error}。这是服务执行错误，正在退避后自动重试（第 {retry} 次），不占用未交付续跑次数。先核对现状再接着完成原目标；已经完成的工具操作、推送、合并、打 tag、发布不要重做，也不要把结果未知的操作当作成功。做完用 steve_deliver 交付，需要用户决定时用 steve_ask。"
        ),
    }]
}

//! Provider service failures use the logical turn's durable attempts/backoff, including when tools
//! already ran. They resume with a reminder to inspect effects, never by replaying the original task.

use super::*;

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
            self.attempt_assistant_tail_in(tx, session, attempt)?
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

    /// Only projected assistant deltas of this attempt, bounded from the end (those applied
    /// earlier in this transaction included). User prompts, tools, thoughts and prior attempts
    /// cannot supply the error.
    fn attempt_assistant_tail_in(
        &self,
        tx: &Tx,
        session: &str,
        attempt: &str,
    ) -> Result<String, StoreError> {
        let mut st = tx.prepare(
            "SELECT json_extract(data,'$.message'), json_extract(data,'$.text') FROM events
             WHERE session_id=?3 AND source_node=?1 AND source_attempt=?2
               AND kind='text' AND json_extract(data,'$.role')='assistant'
             ORDER BY seq DESC",
        )?;
        let rows = st.query_map([self.node_id.as_str(), attempt, session], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut parts = vec![];
        let mut chars = 0;
        let mut last_message = None;
        for row in rows {
            let (message, text) = row?;
            if last_message.as_ref().is_some_and(|m| *m != message) {
                break;
            }
            last_message = Some(message);
            let tail: String = text.chars().rev().take(1024 - chars).collect();
            chars += tail.chars().count();
            parts.push(tail.chars().rev().collect::<String>());
            if chars >= 1024 {
                break;
            }
        }
        parts.reverse();
        Ok(parts.concat())
    }

    /// A question, approval or committed delivery is already waiting on its own next step. A
    /// service failure after it must not start work past that boundary (or resurrect an ended item).
    pub(super) fn stream_retry_allowed_in(
        &self,
        tx: &Tx,
        session: &str,
    ) -> Result<bool, StoreError> {
        Ok(tx.query_row(
            "SELECT s.status<>'ended' AND (i.id IS NULL OR (i.phase='active' AND i.archived=0))
             AND NOT EXISTS(SELECT 1 FROM decisions d WHERE d.item_id=i.id AND d.status='open')
             FROM sessions s LEFT JOIN items i ON i.id=s.item_id WHERE s.id=?",
            [session],
            |r| r.get(0),
        )?)
    }
}

pub(super) fn stream_retry_prompt(error: &str, retry: i64) -> Vec<Part> {
    vec![Part::Text {
        text: format!(
            "（Steve）上一回合因模型服务或传输中断而结束：{error}。这是服务执行错误，正在退避后自动重试（第 {retry} 次），不占用未交付续跑次数。先核对现状再接着完成原目标；已经完成的工具操作、推送、合并、打 tag、发布不要重做，也不要把结果未知的操作当作成功。做完用 steve_deliver 交付，需要用户决定时用 steve_ask。"
        ),
    }]
}

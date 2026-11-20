//! The provider submission boundary and the original definite/uncertain result handling.
use super::*;

pub(super) struct Submission<'a> {
    pub adapter: &'a dyn crate::adapter::MailAdapter,
    pub cx: &'a steve_modules::accounts::connections::ConnCx,
    pub built: &'a Built,
    pub approval: &'a Decision,
    pub now: i64,
}
impl Mail {
    pub(super) async fn submit(
        &self,
        d: &DraftRow,
        attempt: &mut SendGuard,
        submission: Submission<'_>,
    ) -> MailResult<SendOutcome> {
        let Submission {
            adapter,
            cx,
            built,
            approval,
            now,
        } = submission;
        attempt.submitted = true;
        let sent =
            tokio::time::timeout(SEND_TIMEOUT, adapter.send(cx, &built.mime, &built.meta)).await;
        let result = match sent {
            Ok(Ok(r)) => {
                let msg = match self.record_sent(d, &approval.id, built, r.ext_id, now) {
                    Ok(msg) => msg,
                    Err(_) => return self.unknown(d, "服务器已接受邮件，但本地发送记录保存失败"),
                };
                // From here on the send is confirmed and recorded, even if the optional copy
                // hangs, fails, or this task is cancelled. It is outside SEND_TIMEOUT.
                attempt.completed = true;
                if let Some(item) = &d.item_id {
                    self.0.link.hooks().sent(&d.workspace_id, item, &d.subject);
                }
                match tokio::time::timeout(
                    COPY_TIMEOUT,
                    adapter.copy_sent(cx, &built.mime, &built.meta),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    _ => {
                        tracing::warn!(draft = %d.id, "mail sent; optional sent-folder copy did not finish")
                    }
                }
                Ok(SendOutcome::Sent { message: msg })
            }
            Ok(Err(AdapterError::Transient(_) | AdapterError::Store(_))) | Err(_) => {
                self.unknown(d, "等待服务器确认时超时、断连或记录失败")
            }
            Ok(Err(e)) => self.fail(d, &e.to_string()),
        };
        attempt.completed = result.is_ok();
        result
    }
}

//! A successful ACP open does not establish that Steve's MCP attached. The runtime records each
//! open; authenticated tools/list confirms that generation. Missing attachment uses the ordinary
//! session handoff (including the old harness's release fence), never another prompt to it.

use rusqlite::OptionalExtension;
use serde_json::json;

use crate::error::Result;
use crate::repo::{FileRef, ItemExt, NewDecision};
use crate::runtime::Released;
use crate::{Tx, patch};

pub(crate) const ATTACH_GRACE_MS: i64 = 60_000;
const MAX_RECREATIONS: i64 = 2;
const BLOCKED: &str = steve_store::SESSION_MCP_FAILURE;

impl Tx<'_> {
    /// Measured from the first dispatched input of this open, not workspace preparation. Some
    /// adapters fetch the catalog lazily at their first prompt, and the Node waits for that turn.
    pub(crate) fn mcp_overdue(&self, sid: &str, at: i64) -> Result<bool> {
        let Some(a) = self.db().store().sessions().mcp_attachment(sid)? else {
            return Ok(false); // pre-upgrade/foreign runtimes supplied no attachment contract
        };
        if a.fault.as_deref() == Some("missing_attempt") {
            return Ok(true);
        }
        if a.ready {
            return Ok(false);
        }
        let sent: Option<i64> = self.db().store().reader(|c| -> Result<_> {
            Ok(c.query_row(
                "SELECT MIN(ts) FROM events WHERE session_id=? AND seq>? AND kind='user'",
                rusqlite::params![sid, a.seq],
                |r| r.get(0),
            )?)
        })?;
        Ok(sent.is_some_and(|sent| at - sent >= ATTACH_GRACE_MS))
    }

    /// `true`: this missing-tool session is now being replaced or is durably blocked. A manual
    /// retry starts a new bounded chain; a healthy attachment resets the consecutive count.
    pub(crate) fn recover_missing_mcp(&mut self, sid: &str, manual: bool) -> Result<bool> {
        let Some(s) = self.db().session(sid)?.filter(|s| s.status != "ended") else {
            return Ok(false);
        };
        let Some(item) = s.item_id.as_deref() else {
            return Ok(false);
        };
        let i = self.item(item)?;
        if i.is_settled()
            || i.archived
            || i.status == "pause"
            || self.holding()
            || self.st.releasing.is_some()
            || self.dependency_waiting(item)?
            || self.workflow_child(&i)?
            || self.rt().uncertain(sid)
            || self.rt().node_offline(sid).is_some()
            || self.active_session(item)?.is_none_or(|a| a.id != sid)
        {
            return Ok(false);
        }
        if !manual
            && self.db().store().sessions().stop(sid)?.is_some_and(|stop| {
                stop.yours
                    || stop.stop_asked.is_some()
                    || stop.stop_reason.as_deref() == Some("cancelled")
            })
        {
            return Ok(false);
        }
        let Some(a) = self.db().store().sessions().mcp_attachment(sid)? else {
            return Ok(false);
        };
        if a.ready && !manual {
            return Ok(false);
        }
        let cards = self.db().decisions_of_item(item)?;
        if !manual
            && cards
                .iter()
                .any(|d| d.status == "open" && !crate::lifecycle::stop_card(d))
        {
            return Ok(false);
        }
        let retries = if manual {
            0
        } else {
            // A usable MCP call can precede the inherited counter. Discovery alone does not
            // reset a streak: a mounted server may still lack an execution identity.
            self.db().store().reader(|c| -> Result<_> {
                Ok(c.query_row("SELECT json_extract(data,'$.mcp_recovery_attempt') FROM events WHERE session_id=?1 AND kind='system' AND json_type(data,'$.mcp_recovery_attempt')='integer' AND NOT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND kind='system' AND json_type(data,'$.steve_mcp_called')='text') ORDER BY seq DESC LIMIT 1", [sid], |r| r.get::<_,i64>(0)).optional()?.unwrap_or(0))
            })?
        };
        if retries >= MAX_RECREATIONS {
            self.block_missing_mcp(sid, item)?;
            return Ok(true);
        }
        let cause = if a.fault.as_deref() == Some("missing_attempt") {
            "Steve 工具无法关联运行尝试（Node 没有 active attempt，但 Hub 仍记录执行中）"
        } else {
            "没有挂上 Steve 工具"
        };
        let ctx = self.transcript_up_to(sid, i64::MAX)?;
        let waiting = self.db().queued(sid)?;
        let mut files: Vec<FileRef> = vec![];
        // Include attachments already dispatched, as well as user input still in the queue.
        // The successor gets a fresh Steve protocol, never the obsolete queued startup protocol.
        for e in self
            .db()
            .events_after(sid, 0)?
            .into_iter()
            .filter(|e| e.kind == "user")
        {
            for f in
                serde_json::from_value::<Vec<FileRef>>(e.data["files"].clone()).unwrap_or_default()
            {
                if !files.iter().any(|seen| seen.id == f.id) {
                    files.push(f);
                }
            }
        }
        let mut messages = vec![];
        for q in waiting.iter().filter(|q| {
            !q.text
                .starts_with("You are working for Steve, the user's assistant, on item ")
        }) {
            messages.push(q.text.clone());
            for f in &q.files {
                if !files.iter().any(|seen| seen.id == f.id) {
                    files.push(f.clone());
                }
            }
        }
        let opening = format!(
            "上一个会话{cause}，已自动转到新会话。先用 steve_update 确认工具可用。\n\n上一个会话的对话：\n{ctx}\n\n尚未处理的用户消息：\n{}\n\n先核对代码、PR 和实际操作现状，再接着收尾；已经完成的推送、合并、打 tag、发布不要重做。做完用 steve_deliver 交付，需要用户决定时用 steve_ask。",
            messages.join("\n\n")
        );
        // Commit the terminal old session before asking another thread to place/open its successor.
        // A failed release must not let two harnesses modify the same worktree at once.
        let db = self.db().clone();
        db.atomic(|| {
            if manual {
                for d in cards.iter().filter(|d| d.status=="open" && d.subject_type==BLOCKED && d.session_id.as_deref()==Some(sid)) {
                    self.db().decision_resolve(&d.id,json!({"choice":"retry"}),"resolved",None)?;
                }
            }
            crate::lifecycle::supersede_where(self.db(),item,crate::lifecycle::stop_card,"没有挂上 Steve 工具，转到新会话")?;
            self.db().session_patch(sid,patch![status:"ended"])?;
            self.db().event_add(sid, "system", json!({"text":format!("{cause}，已结束此会话并自动重开。")}))?;
            self.db().timeline_add(item, "steve", "sys", &format!("会话 #{} {cause}，自动重开（第 {}/{MAX_RECREATIONS} 次）；保留已有工作并核对后继续",s.no,retries+1),Some(sid))
        })?;
        // Runtime calls acquire their coordinator lock before the store writer. Never call them
        // inside the store transaction: an inbox worker could already hold that lock waiting for
        // our writer, while cancellation waits for it. The ended session cannot claim new input.
        match self.rt().release(sid) {
            Ok(Released::Pending(op)) => {
                self.st.handoffs.insert(item.into(), op);
            }
            Ok(Released::Gone) => {}
            Ok(Released::Busy) => {
                self.fail(
                    item,
                    "旧会话仍在执行，未确认释放，暂不重开；请检查 Node 后重新指派。",
                )?;
                return Ok(true);
            }
            Err(e) => {
                self.fail(
                    item,
                    &format!("Steve 工具恢复未能确认旧会话释放：{e}；请检查 Node 后重新指派。"),
                )?;
                return Ok(true);
            }
        }
        if let Err(e) = self.rt().cancel(sid) {
            self.fail(
                item,
                &format!("Steve 工具恢复未能停止旧会话：{e}；请检查 Node 后重新指派。"),
            )?;
            return Ok(true);
        }
        match self.start_session_files(item, Some(&opening), None, &files) {
            Ok(ns) => {
                self.db()
                    .event_add(&ns.id, "system", json!({"mcp_recovery_attempt":retries+1}))?;
            }
            Err(e) => self.fail(
                item,
                &format!("Steve 工具恢复失败：{e}；请重新指派以新开会话。"),
            )?,
        }
        Ok(true)
    }

    fn block_missing_mcp(&mut self, sid: &str, item: &str) -> Result<()> {
        let question = "Steve 工具挂载或执行身份异常，连续自动重开 2 次仍未恢复；已停止自动重试，请检查 Hub/Node 连接后点击重试，或重新指派。";
        let db = self.db().clone();
        db.atomic(|| {
            self.db()
                .store()
                .sessions()
                .stop_ended(sid, None, BLOCKED, Some(question), false)?;
            let i = self.item(item)?;
            if !self.db().decisions_of_item(item)?.iter().any(|d| {
                d.status == "open"
                    && d.subject_type == BLOCKED
                    && d.session_id.as_deref() == Some(sid)
            }) {
                self.db().decision_create(NewDecision {
                    workspace_id: i.workspace_id,
                    kind: "fail".into(),
                    subject_type: BLOCKED.into(),
                    subject_id: sid.into(),
                    item_id: Some(item.into()),
                    session_id: Some(sid.into()),
                    question: question.into(),
                    options: Some(vec!["retry".into()]),
                    requested_by: "steve".into(),
                    ..Default::default()
                })?;
                self.db()
                    .timeline_add(item, "steve", "sys", question, Some(sid))?;
            }
            self.db()
                .item_patch(item, patch![status:"wait",now:question])?;
            Ok(())
        })?;
        self.rt().cancel(sid)?;
        // A busy session is released once its cancelled turn settles; it cannot take more input.
        let _ = self.rt().release(sid)?;
        Ok(())
    }
}

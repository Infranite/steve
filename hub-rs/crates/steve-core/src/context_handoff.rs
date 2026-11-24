//! Deterministic working-memory handoff for context-budget rollover and workspace `handoff`
//! threshold transitions.
//!
//! No model calls are made to decide whether to roll over or to summarize the ended session:
//! facts come directly from durable item/session records (plan, resolved decisions, delivery
//! refusals, completed/failed tools, bounded last assistant progress, and any queued messages).

use std::collections::HashMap;

use serde_json::json;
use steve_node_api::{ToolPhase, ToolView};

use crate::Tx;
use crate::error::Result;
use crate::repo::{FileRef, Item, Session};
use crate::util::js_head;

const MAX_PLAN_STEPS: usize = 20;
const MAX_DECISIONS: usize = 8;
const MAX_REFUSALS: usize = 3;
const MAX_TOOLS: usize = 8;
const LAST_PROGRESS_CHARS: usize = 800;

/// Conservative deterministic token estimate for an opening prompt (used only for saved-token
/// telemetry; never drives control flow).
pub(crate) fn estimate_prompt_tokens(text: &str) -> u64 {
    let mut ascii = 0u64;
    let mut non_ascii = 0u64;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    (ascii.div_ceil(4) + non_ascii).max(1)
}

impl Tx<'_> {
    /// Handle a turn that the Node stopped deterministically at its context budget
    /// (`stop_reason == steve_node_api::CONTEXT_BUDGET`).
    pub(crate) fn continue_after_context_budget(
        &mut self,
        item: &Item,
        session: &Session,
        attempt: Option<&str>,
    ) -> Result<()> {
        let limit = self.cfg().context_handoff_limit;
        let prior = self.consecutive_context_handoffs(&item.id, &session.id)?;
        if prior >= limit {
            let why = if limit == 0 {
                "会话达到上下文预算上限，已按配置停止自动续跑，请决定如何继续。".to_string()
            } else {
                format!("已连续因上下文预算上限交接续跑 {prior} 次仍未完成，请介入检查后再继续。")
            };
            return self.fail(&item.id, &why);
        }

        let pct = context_pct(session);
        let used = session
            .used
            .and_then(|u| u64::try_from(u).ok())
            .unwrap_or(0);
        let queued = self.db().queue_take(&session.id)?;
        let queued_files: Vec<FileRef> = queued
            .iter()
            .flat_map(|q| q.files.iter().cloned())
            .collect();
        let queued_texts: Vec<String> = queued
            .into_iter()
            .map(|q| q.text)
            .filter(|t| !t.trim().is_empty())
            .collect();

        let last = self.last_agent_text(&session.id)?;
        let working_memory = self.build_working_memory(item, session)?;
        let mut opening = format_handoff_opening(
            &working_memory,
            pct,
            used,
            js_head(&last, LAST_PROGRESS_CHARS),
        );
        if !queued_texts.is_empty() {
            opening.push_str("\n\n待处理消息：\n");
            opening.push_str(&queued_texts.join("\n\n"));
        }
        opening.push_str(
            "\n\n请接着完成剩余工作，做完用 steve_deliver 交付，需要用户决定时用 steve_ask。",
        );

        let sys_note = match pct {
            Some(p) => format!("上下文已达预算上限（{p}%），带工作记忆转到新会话续跑"),
            None => format!("上下文已达预算上限（{used} tokens），带工作记忆转到新会话续跑"),
        };
        let tl_note = match pct {
            Some(p) => format!("上下文达预算上限（{p}%）：带工作记忆开新会话续跑"),
            None => format!("上下文达预算上限（{used} tokens）：带工作记忆开新会话续跑"),
        };
        self.db()
            .event_add(&session.id, "system", json!({"text": sys_note}))?;
        self.hand_off(&item.id, &session.id)?;
        self.db()
            .timeline_add(&item.id, "steve", "sys", &tl_note, None)?;
        self.start_session_files(&item.id, Some(&opening), None, &queued_files)?;
        self.record_handoff_compaction(item, session, attempt, "budget_reached", &opening);
        Ok(())
    }

    /// Perform workspace-threshold (`handoff` setting) session rollover when the user supplements
    /// an idle session that has reached the configured context percentage.
    pub(crate) fn handoff_on_supplement(
        &mut self,
        item: &Item,
        session: &Session,
        pct: i64,
        text: &str,
        files: &[FileRef],
    ) -> Result<()> {
        let last = self.last_agent_text(&session.id)?;
        let working_memory = self.build_working_memory(item, session)?;
        let used = session
            .used
            .and_then(|u| u64::try_from(u).ok())
            .unwrap_or(0);
        let mut opening = format_handoff_opening(
            &working_memory,
            Some(pct),
            used,
            js_head(&last, LAST_PROGRESS_CHARS),
        );
        opening.push_str(&format!("\n\n补充信息：{text}"));

        self.db().event_add(
            &session.id,
            "system",
            json!({"text": format!("上下文已用 {pct}%，带交接摘要转到新会话")}),
        )?;
        self.hand_off(&item.id, &session.id)?;
        self.db().timeline_add(
            &item.id,
            "steve",
            "sys",
            &format!("上下文已用 {pct}%：开新会话"),
            None,
        )?;
        self.start_session_files(&item.id, Some(&opening), None, files)?;
        self.record_handoff_compaction(item, session, None, "handoff_threshold", &opening);
        Ok(())
    }

    /// Count consecutive prior ended sessions of `item_id` that ended with `CONTEXT_BUDGET`
    /// since the user's last action (message or resolved decision).
    fn consecutive_context_handoffs(&self, item_id: &str, current_sid: &str) -> Result<u32> {
        let last_me = self
            .db()
            .timeline(item_id)?
            .iter()
            .filter(|e| e.kind == "me")
            .map(|e| e.ts)
            .max()
            .unwrap_or(0);
        let last_dec = self
            .db()
            .decisions_of_item(item_id)?
            .iter()
            .filter(|d| d.status == "resolved" && d.result["decided_by"] != "yolo")
            .filter_map(|d| d.resolved)
            .max()
            .unwrap_or(0);
        let since = last_me.max(last_dec);

        let mut sessions = self.db().sessions_of_item(item_id)?;
        sessions.sort_by(|a, b| b.no.cmp(&a.no).then_with(|| b.created.cmp(&a.created)));

        let store_sessions = self.db().store().sessions();
        let mut count = 0u32;
        for s in sessions {
            if s.id == current_sid {
                continue;
            }
            if s.status != "ended" {
                continue;
            }
            let stop = store_sessions.stop(&s.id)?;
            let ended_at = stop.as_ref().and_then(|st| st.ended).unwrap_or(s.updated);
            if ended_at <= since {
                break;
            }
            if stop.as_ref().and_then(|st| st.stop_reason.as_deref())
                == Some(steve_node_api::CONTEXT_BUDGET)
            {
                count += 1;
            } else {
                break;
            }
        }
        Ok(count)
    }

    /// Deterministically extract compact working memory from durable item & session state.
    fn build_working_memory(&self, item: &Item, session: &Session) -> Result<String> {
        let mut sections: Vec<String> = vec![];

        // 1. Item plan progress (done vs remaining open steps)
        let active_steps: Vec<_> = item.plan.iter().filter(|s| !s.left).collect();
        if !active_steps.is_empty() {
            let mut lines = vec!["[计划进度]".to_string()];
            for step in active_steps.iter().take(MAX_PLAN_STEPS) {
                let mark = if step.done { "x" } else { " " };
                lines.push(format!("- [{mark}] {}", js_head(step.step.trim(), 160)));
            }
            if active_steps.len() > MAX_PLAN_STEPS {
                lines.push(format!(
                    "- ... 其余 {} 项见事项计划",
                    active_steps.len() - MAX_PLAN_STEPS
                ));
            }
            sections.push(lines.join("\n"));
        }

        // 2. Resolved user decisions (ask / accept / propose)
        let mut resolved: Vec<_> = self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .filter(|d| {
                d.status == "resolved" && matches!(d.kind.as_str(), "ask" | "accept" | "propose")
            })
            .collect();
        if resolved.len() > MAX_DECISIONS {
            let drain = resolved.len() - MAX_DECISIONS;
            resolved.drain(..drain);
        }
        if !resolved.is_empty() {
            let mut lines = vec!["[已确认决定]".to_string()];
            for d in resolved {
                let ans = d
                    .result
                    .get("answer")
                    .or_else(|| d.result.get("choice"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let note = d
                    .result
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                let mut summary = ans.to_string();
                if !note.is_empty() && note != ans {
                    if !summary.is_empty() {
                        summary.push_str(" / ");
                    }
                    summary.push_str(note);
                }
                if !summary.is_empty() {
                    lines.push(format!(
                        "- {} => {}",
                        js_head(d.question.trim(), 120),
                        js_head(&summary, 120)
                    ));
                }
            }
            if lines.len() > 1 {
                sections.push(lines.join("\n"));
            }
        }

        // 3. Recent delivery refusals since last user action / delivery
        let refusals = self.refusals(&item.id)?;
        if !refusals.is_empty() {
            let mut lines = vec!["[最近交付反馈]".to_string()];
            for why in refusals.into_iter().take(MAX_REFUSALS) {
                lines.push(format!("- {}", js_head(why.trim(), 200)));
            }
            sections.push(lines.join("\n"));
        }

        // 4. Recent completed/failed tool calls from ended session transcript
        let tools = self.recent_session_tools(&session.id)?;
        if !tools.is_empty() {
            let mut lines = vec!["[最近工具执行]".to_string()];
            for line in tools {
                lines.push(format!("- {line}"));
            }
            sections.push(lines.join("\n"));
        }

        if sections.is_empty() {
            return Ok(String::new());
        }

        Ok(format!(
            "工作记忆（来自上一会话 #{}）：\n{}\n\n先核对工作区当前文件与 git status/diff 再继续；已经完成的提交、推送、合并、打 tag、发布不要重做。",
            session.no,
            sections.join("\n\n")
        ))
    }

    fn recent_session_tools(&self, sid: &str) -> Result<Vec<String>> {
        let events = self.db().events_after(sid, 0)?;
        let mut order: Vec<String> = vec![];
        let mut latest: HashMap<String, ToolView> = HashMap::new();
        for ev in events {
            if ev.kind != steve_transcript::TOOL {
                continue;
            }
            let Some(raw) = ev.data.get("tool") else {
                continue;
            };
            let Ok(tool) = serde_json::from_value::<ToolView>(raw.clone()) else {
                continue;
            };
            let id = tool.id.to_string();
            if !latest.contains_key(&id) {
                order.push(id.clone());
            }
            latest.insert(id, tool);
        }
        let mut finished: Vec<String> = vec![];
        for id in order {
            let Some(tool) = latest.remove(&id) else {
                continue;
            };
            let tag = match tool.phase {
                ToolPhase::Succeeded => "ok",
                ToolPhase::Failed => "fail",
                _ => continue,
            };
            let detail = tool
                .target
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|t| format!(" {}", js_head(t, 120)))
                .unwrap_or_default();
            finished.push(format!("[{tag}] {}{detail}", tool.name));
        }
        if finished.len() > MAX_TOOLS {
            let drain = finished.len() - MAX_TOOLS;
            finished.drain(..drain);
        }
        Ok(finished)
    }

    fn record_handoff_compaction(
        &self,
        item: &Item,
        session: &Session,
        attempt: Option<&str>,
        reason: &'static str,
        opening: &str,
    ) {
        let before = session
            .used
            .and_then(|u| u64::try_from(u).ok())
            .unwrap_or(0);
        let after = estimate_prompt_tokens(opening);
        let saved = before.saturating_sub(after);
        let limit = session.size.and_then(|z| u64::try_from(z).ok());

        metrics::counter!(
            "steve_turn_context_compactions_total",
            "harness" => session.harness.clone(),
            "mode" => "handoff",
            "reason" => reason,
        )
        .increment(1);
        if saved > 0 {
            metrics::counter!(
                "steve_turn_context_saved_tokens_total",
                "harness" => session.harness.clone(),
                "mode" => "handoff",
            )
            .increment(saved);
        }

        let _ = self
            .db()
            .store()
            .diagnostics()
            .record(steve_store::diagnostics::Record {
                workspace_id: item.workspace_id.clone(),
                item_id: Some(item.id.clone()),
                session_id: Some(session.id.clone()),
                attempt_id: attempt.map(str::to_owned),
                kind: "context.compacted".into(),
                reason_code: reason.into(),
                severity: steve_store::diagnostics::Severity::Info,
                details: steve_store::diagnostics::Details {
                    item_cycle: Some(item.cycle),
                    item_revision: Some(item.delivery_revision),
                    context_before: Some(before),
                    context_after: Some(after),
                    context_saved: Some(saved),
                    limit,
                    ..Default::default()
                },
                ..Default::default()
            });
    }
}

fn context_pct(session: &Session) -> Option<i64> {
    let used = session.used.filter(|u| *u > 0)?;
    let size = session.size.filter(|z| *z > 0)?;
    Some((used as f64 / size as f64 * 100.0).round() as i64)
}

fn format_handoff_opening(working_memory: &str, pct: Option<i64>, used: u64, last: &str) -> String {
    let head = match pct {
        Some(p) => format!("交接：上一个会话的上下文用到了 {p}%。它的最后进展：{last}"),
        None => {
            format!("交接：上一个会话的上下文已达预算上限（{used} tokens）。它的最后进展：{last}")
        }
    };
    if working_memory.is_empty() {
        head
    } else {
        format!("{working_memory}\n\n{head}")
    }
}

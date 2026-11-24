//! Deterministic working-memory handoff for context-budget rollover, workspace `handoff`
//! threshold transitions, and manual owner handoffs (same Bot switching execution surface for this
//! item only, or explicit Bot switch).
//!
//! Design invariants (HANDOFF.md):
//! - Handoff reason is a strong-typed enum determined deterministically (never by a model).
//! - Default: keep the SAME Bot (identity/authority/memory/Team) and switch ONLY its execution
//!   surface (`harness + model + effort`) for THIS item, recording a W3.1 [`BotPlacement`] fact
//!   without altering the Bot's default configuration (unless the owner explicitly checks
//!   `make_bot_default`, which is audited and reversible).
//! - Switching to another Bot happens ONLY on explicit owner choice (or Phase 2 coordinator card).
//! - Handoff package is harness-agnostic and assembled deterministically from durable records.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::json;
use steve_node_api::{HarnessChoice, ToolPhase, ToolView};
use steve_types::{BotPlacement, PlacementBlocked, PlacementReason, PlacementRejection};

use crate::Tx;
use crate::error::{CoreError, Result, msg};
use crate::orchestrator::SessionStart;
use crate::repo::{Bot, BotExt, FileRef, Item, Session};
use crate::util::js_head;

const MAX_PLAN_STEPS: usize = 20;
const MAX_DECISIONS: usize = 8;
const MAX_REFUSALS: usize = 3;
const MAX_TOOLS: usize = 8;
const MAX_WAITS: usize = 5;
const LAST_PROGRESS_CHARS: usize = 800;

/// Workspace setting key for automatic context-budget handoff execution surface (`"same"`,
/// `"harness:<id>"`, `"harness:<id>/<model>"`, or `"harness:<id>/<model>@<effort>"`).
/// Automatic context-budget handoff never switches Bot; it only switches the current Bot's
/// execution surface for the item.
pub const CONTEXT_BUDGET_TARGET_SETTING: &str = "contextBudgetTarget";

/// Target specification for an item/session handoff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffTarget {
    /// Explicit target Bot (only allowed on manual `OwnerRequest` or coordinator card).
    /// `None` keeps the item's current Bot and switches only its execution surface for this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    /// Target harness ID (`None` keeps current session/Bot harness).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Target model ID (`None` uses target harness/Bot default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Target reasoning effort (`None` keeps Bot effort if harness unchanged, else `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Manual only ("以后这个 Bot 都用这个"): explicitly update the receiving Bot's default
    /// execution surface (`harness`, `model`, `reasoning_effort`) as a reversible change.
    #[serde(default)]
    pub make_bot_default: bool,
}

impl HandoffTarget {
    fn normalized(&self) -> Self {
        let clean = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        Self {
            bot: clean(&self.bot),
            harness: clean(&self.harness),
            model: clean(&self.model),
            effort: clean(&self.effort),
            make_bot_default: self.make_bot_default,
        }
    }

    pub fn is_same_default(&self) -> bool {
        self.bot.is_none()
            && self.harness.is_none()
            && self.model.is_none()
            && self.effort.is_none()
    }
}

/// Previous Bot default surface returned when `make_bot_default` was requested, so the caller/UI
/// can offer one-click undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotDefaultSnapshot {
    pub harness: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

/// Audit/undo receipt when a manual handoff explicitly updated a Bot's default execution surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotDefaultUpdated {
    pub bot: String,
    pub previous: BotDefaultSnapshot,
}

/// Result of a manual handoff call.
#[derive(Debug, Clone)]
pub struct HandoffOutcome {
    pub session: Session,
    pub bot_default_updated: Option<BotDefaultUpdated>,
}

/// Strong-typed reason for a session handoff (ASCII codes for metrics/diagnostics; display text
/// only at the timeline/event boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffReason {
    ContextBudget,
    QuotaExhausted { reset_at: Option<i64> },
    ProviderError,
    OwnerRequest,
}

impl HandoffReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ContextBudget => "context_budget",
            Self::QuotaExhausted { .. } => "quota_exhausted",
            Self::ProviderError => "provider_error",
            Self::OwnerRequest => "owner_request",
        }
    }

    pub const fn placement_reason(self) -> PlacementReason {
        match self {
            Self::ContextBudget => PlacementReason::HandoffContextBudget,
            Self::QuotaExhausted { .. } => PlacementReason::HandoffQuotaExhausted,
            Self::ProviderError => PlacementReason::HandoffProviderError,
            Self::OwnerRequest => PlacementReason::HandoffOwnerRequest,
        }
    }

    fn timeline_reason(self, zone: steve_runtime::tz::Zone, now: i64) -> String {
        match self {
            Self::ContextBudget => "上下文预算".to_string(),
            Self::QuotaExhausted { reset_at: Some(at) } => {
                let w = crate::quota::when(at, now, zone);
                if w.is_empty() {
                    "额度不足".to_string()
                } else {
                    format!("额度不足，预计 {w} 恢复")
                }
            }
            Self::QuotaExhausted { reset_at: None } => "额度不足".to_string(),
            Self::ProviderError => "上游持续故障".to_string(),
            Self::OwnerRequest => "主人手动".to_string(),
        }
    }
}

/// Parse the workspace `contextBudgetTarget` setting into a [`HandoffTarget`] execution surface.
///
/// Supported forms (execution surface only — never switches Bot automatically):
/// - `""` or `"same"` -> same Bot, same execution surface, fresh session
/// - `"harness:<harness_id>"` -> switch current Bot's execution surface to `<harness_id>`
/// - `"harness:<harness_id>/<model_id>"` -> switch to `<harness_id>` + `<model_id>`
/// - `"harness:<harness_id>/<model_id>@<effort>"` -> switch to `<harness_id>` + `<model_id>` + `<effort>`
pub fn parse_budget_handoff_target(raw: &str) -> Option<HandoffTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "same" {
        return Some(HandoffTarget::default());
    }
    let rest = trimmed.strip_prefix("harness:")?;
    let (rt_id, model, effort) = match rest.split_once('/') {
        Some((h, tail)) => {
            let (m, eff) = match tail.rsplit_once('@') {
                Some((m, e)) => {
                    let e = e.trim();
                    if e.is_empty() || e.len() > 32 || !valid_effort_id(e) {
                        return None;
                    }
                    (m.trim(), Some(e.to_owned()))
                }
                None => (tail.trim(), None),
            };
            if m.is_empty() || m.len() > 200 || m.chars().any(char::is_control) {
                return None;
            }
            (h.trim(), Some(m.to_owned()), eff)
        }
        None => match rest.rsplit_once('@') {
            Some((h, e)) => {
                let e = e.trim();
                if e.is_empty() || e.len() > 32 || !valid_effort_id(e) {
                    return None;
                }
                (h.trim(), None, Some(e.to_owned()))
            }
            None => (rest.trim(), None, None),
        },
    };
    if !valid_runtime_id(rt_id) {
        return None;
    }
    Some(HandoffTarget {
        bot: None,
        harness: Some(rt_id.to_owned()),
        model,
        effort,
        make_bot_default: false,
    })
}

fn valid_runtime_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_effort_id(id: &str) -> bool {
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

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

fn format_surface_actor(bot_name: &str, harness_display: &str, model: Option<&str>) -> String {
    match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => format!("{bot_name}（{harness_display} · {m}）"),
        None => format!("{bot_name}（{harness_display}）"),
    }
}

impl Tx<'_> {
    /// Record the outcome of the first turn of a session created via handoff.
    pub(crate) fn record_handoff_turn_outcome(&mut self, sid: &str, outcome: &'static str) {
        if let Some(reason) = self.st.handoff_outcomes.remove(sid) {
            metrics::counter!(
                "steve_handoff_outcomes_total",
                "reason" => reason,
                "outcome" => outcome,
            )
            .increment(1);
        }
    }

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

        let raw_target =
            self.db()
                .setting(&item.workspace_id, CONTEXT_BUDGET_TARGET_SETTING, "same")?;
        let target = parse_budget_handoff_target(&raw_target).unwrap_or_default();

        match self.execute_handoff(
            item,
            session,
            &target,
            HandoffReason::ContextBudget,
            "steve",
            attempt,
        ) {
            Ok(_) => Ok(()),
            Err(e) => self.fail(&item.id, &format!("达到上下文预算，无法交接新会话：{e}")),
        }
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
        let working_memory = self.build_working_memory(item, session, true)?;
        let head = format!(
            "交接：上一个会话的上下文用到了 {pct}%。它的最后进展：{}",
            js_head(&last, LAST_PROGRESS_CHARS)
        );
        let mut opening = if working_memory.is_empty() {
            head
        } else {
            format!("{working_memory}\n\n{head}")
        };
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

    /// Manual owner handoff from the item page (`OwnerRequest`).
    pub(crate) fn handoff_item(
        &mut self,
        item_id: &str,
        target: HandoffTarget,
        actor: &str,
    ) -> Result<HandoffOutcome> {
        let item = self.item(item_id)?;
        self.require_open_item(item_id)?;
        let prev = match self.active_session(item_id)? {
            Some(s) => s,
            None => {
                let mut sessions = self.db().sessions_of_item(item_id)?;
                sessions.sort_by(|a, b| b.no.cmp(&a.no).then_with(|| b.created.cmp(&a.created)));
                sessions
                    .into_iter()
                    .find(|s| s.readonly == 0 && s.source == "Steve")
                    .ok_or_else(|| msg("当前事项还没有可交接的会话"))?
            }
        };
        let outcome = self.execute_handoff(
            &item,
            &prev,
            &target,
            HandoffReason::OwnerRequest,
            actor,
            None,
        )?;
        crate::lifecycle::supersede(self.db(), &item.id, &["fail"], "已手动交接新会话")?;
        crate::thread::flowback::recover_quiet(self.db(), &item.id);
        Ok(outcome)
    }

    /// Manual owner handoff starting from a specific session page (`OwnerRequest`).
    pub(crate) fn handoff_session(
        &mut self,
        session_id: &str,
        target: HandoffTarget,
        actor: &str,
    ) -> Result<HandoffOutcome> {
        let prev = self
            .db()
            .session(session_id)?
            .ok_or_else(|| msg(steve_i18n::t!("core.conversation.not_found")))?;
        if prev.readonly != 0 || prev.source != "Steve" {
            return Err(msg("只读或外部导入的会话不能交接"));
        }
        let item_id = prev
            .item_id
            .clone()
            .ok_or_else(|| msg("只有关联事项的会话才能交接"))?;
        let item = self.item(&item_id)?;
        self.require_open_item(&item_id)?;
        let outcome = self.execute_handoff(
            &item,
            &prev,
            &target,
            HandoffReason::OwnerRequest,
            actor,
            None,
        )?;
        crate::lifecycle::supersede(self.db(), &item.id, &["fail"], "已手动交接新会话")?;
        crate::thread::flowback::recover_quiet(self.db(), &item.id);
        Ok(outcome)
    }

    /// Single unified handoff execution path:
    /// 1. Resolve target Bot + execution surface through W3.1 [`crate::placement::select`] and
    ///    verify [`crate::AgentRuntime::can_run_choice`] BEFORE touching the old session.
    /// 2. Stop/hand off old session, expire session-scoped decisions, drain queued messages.
    /// 3. Assemble harness-agnostic deterministic handoff package (goal/acceptance, cwd/branch,
    ///    plan progress, resolved decisions, open waits, delivery feedback, recent tools, last
    ///    progress, queued messages).
    /// 4. Start successor session with W3.1 [`BotPlacement`] (reason = handoff reason) and emit
    ///    the explicit timeline handoff event `"从 A（...）交接给 B（...），原因：..."`.
    /// 5. If `make_bot_default` was explicitly checked on `OwnerRequest`, update Bot defaults
    ///    with an audit record and return the previous snapshot for undo.
    pub(crate) fn execute_handoff(
        &mut self,
        item: &Item,
        prev: &Session,
        raw_target: &HandoffTarget,
        reason: HandoffReason,
        actor: &str,
        attempt: Option<&str>,
    ) -> Result<HandoffOutcome> {
        let target = raw_target.normalized();
        // Automatic handoffs never switch Bot; only OwnerRequest (or Phase 2 coordinator card) may.
        if target.bot.is_some() && !matches!(reason, HandoffReason::OwnerRequest) {
            return Err(msg("自动交接不更换 Bot，只切换当前 Bot 的执行面"));
        }

        let (next_bot, choice, placement) =
            self.resolve_handoff_placement(item, prev, &target, reason)?;

        // Admission check BEFORE stopping or ending the previous session so a rejected target
        // leaves the existing session untouched.
        self.rt().can_run_choice(&item.id, &next_bot.id, &choice)?;

        let prev_bot_id = prev
            .bot_id
            .as_deref()
            .or(item.bot_id.as_deref())
            .unwrap_or("steve");
        let prev_bot_name = self
            .db()
            .bot(prev_bot_id)?
            .map(|b| b.display())
            .unwrap_or_else(|| prev_bot_id.to_string());
        let from_rt_name = self.c.models().harness_name(&prev.harness);
        let to_rt_name = self.c.models().harness_name(&choice.harness);
        let from_actor = format_surface_actor(&prev_bot_name, &from_rt_name, prev.model.as_deref());
        let to_actor =
            format_surface_actor(&next_bot.display(), &to_rt_name, choice.model.as_deref());
        let zone = self
            .db()
            .store()
            .read(|r| steve_modules::zone::workspace_tz(r, &item.workspace_id))
            .ok()
            .and_then(|z| steve_runtime::tz::Zone::parse(z.name()).ok())
            .unwrap_or_else(steve_runtime::tz::Zone::system);
        let handoff_line = format!(
            "从 {from_actor}交接给 {to_actor}，原因：{}",
            reason.timeline_reason(zone, crate::util::now())
        );

        let pct = context_pct(prev);
        let used = prev.used.and_then(|u| u64::try_from(u).ok()).unwrap_or(0);

        let last = self.last_agent_text(&prev.id)?;
        // Drain queued messages BEFORE `hand_off()` ends the session and drops its queue.
        let queued = self.db().queue_take(&prev.id)?;

        if prev.status != "ended" {
            if self.busy(&prev.id) {
                self.stop_now(&prev.id);
            }
            self.hand_off(&item.id, &prev.id)?;
        }

        // Expire open decisions tied to the ended session before snapshotting open waits.
        for d in self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .filter(|d| d.status == "open" && d.session_id.as_deref() == Some(prev.id.as_str()))
        {
            self.db().decision_resolve(
                &d.id,
                json!({"superseded": "会话已交接"}),
                "expired",
                None,
            )?;
        }

        let working_memory = self.build_working_memory(item, prev, false)?;
        let all_files: Vec<FileRef> = queued
            .iter()
            .flat_map(|q| q.files.iter().cloned())
            .collect();
        let queued_texts: Vec<String> = queued
            .into_iter()
            .map(|q| q.text)
            .filter(|t| !t.trim().is_empty())
            .collect();

        let mut opening = format_handoff_opening(
            &working_memory,
            reason,
            pct,
            used,
            js_head(&last, LAST_PROGRESS_CHARS),
        );
        if !queued_texts.is_empty() {
            opening.push_str("\n\n[待处理消息]\n");
            opening.push_str(&queued_texts.join("\n\n"));
        }
        opening.push_str(
            "\n\n请接着完成剩余工作，做完用 steve_deliver 交付，需要用户决定时用 steve_ask。",
        );

        let switched_bot = next_bot.id != prev_bot_id;
        let bot_override =
            (item.bot_id.as_deref() != Some(next_bot.id.as_str())).then_some(next_bot.id.as_str());

        let next_session = self.start_session_with(
            &item.id,
            SessionStart {
                opening: Some(&opening),
                bot: bot_override,
                choice: Some(choice.clone()),
                placement: Some(placement.explanation.clone()),
                pre_timeline: Some((actor, handoff_line.clone())),
                files: &all_files,
            },
        )?;

        // Optional explicit default update ("以后这个 Bot 都用这个"), audited and reversible.
        let bot_default_updated = if target.make_bot_default
            && matches!(reason, HandoffReason::OwnerRequest)
            && (next_bot.harness.as_str().ne(choice.harness.as_str())
                || next_bot.model != choice.model
                || next_bot.reasoning_effort != choice.effort)
        {
            let previous = BotDefaultSnapshot {
                harness: next_bot.harness.clone(),
                model: next_bot.model.clone(),
                reasoning_effort: next_bot.reasoning_effort.clone(),
            };
            let mut patch = serde_json::Map::new();
            patch.insert("harness".into(), json!(choice.harness));
            patch.insert("model".into(), json!(choice.model));
            patch.insert("reasoning_effort".into(), json!(choice.effort));
            self.db().bot_patch(&next_bot.id, &patch, actor)?;
            self.db().audit(
                actor,
                "bot.handoff_default",
                &next_bot.id,
                &json!({
                    "item": item.id,
                    "session": next_session.id,
                    "before": previous,
                    "after": {
                        "harness": choice.harness,
                        "model": choice.model,
                        "reasoning_effort": choice.effort,
                    },
                }),
                &item.workspace_id,
            )?;
            Some(BotDefaultUpdated {
                bot: next_bot.id.clone(),
                previous,
            })
        } else {
            None
        };

        self.db().event_add(
            &prev.id,
            "system",
            json!({
                "text": handoff_line,
                "handoff": {
                    "from_session": prev.id,
                    "to_session": next_session.id,
                    "from_bot": prev_bot_id,
                    "to_bot": next_bot.id,
                    "from_harness": prev.harness,
                    "from_model": prev.model,
                    "to_harness": next_session.harness,
                    "to_model": next_session.model,
                    "to_effort": next_session.reasoning_effort,
                    "reason": reason.code(),
                }
            }),
        )?;

        // Track successor session so its first turn end records post-handoff outcome rate.
        self.st
            .handoff_outcomes
            .insert(next_session.id.clone(), reason.code());

        // Emit structured handoff metrics & diagnostics.
        metrics::counter!(
            "steve_handoffs_total",
            "reason" => reason.code(),
            "from_harness" => prev.harness.clone(),
            "to_harness" => next_session.harness.clone(),
            "switched_bot" => if switched_bot { "true" } else { "false" },
        )
        .increment(1);
        metrics::histogram!(
            "steve_handoff_package_bytes",
            "reason" => reason.code(),
        )
        .record(opening.len() as f64);

        let _ = self
            .db()
            .store()
            .diagnostics()
            .record(steve_store::diagnostics::Record {
                workspace_id: item.workspace_id.clone(),
                item_id: Some(item.id.clone()),
                session_id: Some(next_session.id.clone()),
                attempt_id: attempt.map(str::to_owned),
                kind: "session.handoff".into(),
                reason_code: reason.code().into(),
                severity: steve_store::diagnostics::Severity::Info,
                details: steve_store::diagnostics::Details {
                    item_cycle: Some(item.cycle),
                    item_revision: Some(item.delivery_revision),
                    context_before: Some(used),
                    context_after: Some(estimate_prompt_tokens(&opening)),
                    ..Default::default()
                },
                ..Default::default()
            });

        if matches!(reason, HandoffReason::ContextBudget) {
            self.record_handoff_compaction(item, prev, attempt, "budget_reached", &opening);
        }

        Ok(HandoffOutcome {
            session: next_session,
            bot_default_updated,
        })
    }

    /// Resolve the receiving Bot, [`HarnessChoice`], and W3.1 [`BotPlacement`] through
    /// [`crate::placement::select`]. Default keeps the SAME Bot and switches only its execution
    /// surface (`harness + model + effort`) for this item.
    fn resolve_handoff_placement(
        &self,
        item: &Item,
        prev: &Session,
        target: &HandoffTarget,
        reason: HandoffReason,
    ) -> Result<(Bot, HarnessChoice, BotPlacement)> {
        let ws = item.workspace_id.as_str();
        let (team, required_node, bound_node, required_nodes, prev_placement) =
            self.db().store().read(|r| {
                let team = steve_store::bot_dispatch::item_team(r, &item.id)?.map(|(t, _)| t);
                let required_node = steve_store::bot_dispatch::initial(r, &item.id)?
                    .and_then(|p| p.required_node)
                    .map(|n| n.to_string());
                let bound_node = steve_store::bot_admin::bound_node(r, &item.id)?;
                let required_nodes = match steve_store::nodes::item_tag(r, &item.id)? {
                    Some(tag) => Some(steve_store::nodes::with_tag(r, &tag)?),
                    None => None,
                };
                let prev_placement = steve_store::bot_dispatch::for_session(r, &prev.id)?;
                Ok::<_, steve_store::StoreError>((
                    team,
                    required_node,
                    bound_node,
                    required_nodes,
                    prev_placement,
                ))
            })?;
        let node_constraint = required_node.or(bound_node);
        let tags_for_selection: Vec<String> = self
            .db()
            .tags_of_item(&item.id)?
            .iter()
            .map(|id| self.db().tag(id))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .filter(|tag| tag.workspace_id == ws)
            .map(|tag| tag.name)
            .collect();
        let goal_text = self
            .db()
            .goals(&item.id)?
            .into_iter()
            .last()
            .map(|g| g.text)
            .unwrap_or_else(|| item.title.clone());
        let project_req = Some(item.project_id.clone());

        let cur_bot_id = prev
            .bot_id
            .as_deref()
            .or(item.bot_id.as_deref())
            .unwrap_or("steve");

        // For same-surface handoffs, preserve the explicitly placed model/effort (if any) rather
        // than runtime-reported `prev.model`, which may reflect a harness's implicit default model
        // that cannot be explicitly forced via `session/set_model`.
        let prev_placed_model = prev_placement.as_ref().and_then(|p| p.model.as_deref());
        let prev_placed_effort = prev_placement
            .as_ref()
            .and_then(|p| p.reasoning_effort.as_deref());

        let (bot_filter, rt_filter, model_filter, effort_filter) = match target.bot.as_deref() {
            // Explicit Bot switch (OwnerRequest): use that Bot's surface unless overridden.
            Some(bot_id) => (
                bot_id,
                target.harness.as_deref(),
                target.model.as_deref(),
                target.effort.as_deref(),
            ),
            // Default: keep the SAME Bot; switch only execution surface for this item.
            None => match target.harness.as_deref() {
                Some(req_rt) => (
                    cur_bot_id,
                    Some(req_rt),
                    target.model.as_deref(),
                    target.effort.as_deref(),
                ),
                None => (
                    cur_bot_id,
                    Some(prev.harness.as_str()),
                    target.model.as_deref().or(prev_placed_model),
                    target
                        .effort
                        .as_deref()
                        .or(prev_placed_effort)
                        .or(prev.reasoning_effort.as_deref()),
                ),
            },
        };

        let (mut placement, _) = crate::placement::select(
            self.db(),
            self.rt(),
            crate::placement::DispatchRequest {
                workspace: ws,
                bot: Some(bot_filter),
                harness: rt_filter,
                model: model_filter,
                effort: effort_filter,
                team: team.as_ref(),
                project: &project_req,
                goal: &goal_text,
                tags: &tags_for_selection,
                required_specialties: &[],
                node: node_constraint.as_deref(),
                required_nodes: required_nodes.as_deref(),
            },
            |rt_id| self.c.models().agent_model(ws, rt_id),
        )?;

        // Record the handoff cause directly in the W3.1 placement explanation.
        placement.explanation.reason = reason.placement_reason();

        let bot = self
            .db()
            .bot(placement.explanation.bot.as_str())?
            .ok_or_else(|| {
                CoreError::Placement(PlacementBlocked {
                    reason: PlacementRejection::Bot,
                    rejected: vec![],
                })
            })?;
        let choice = HarnessChoice {
            harness: placement.harness.clone(),
            model: placement.model.clone(),
            effort: placement.reasoning_effort.clone(),
        };
        Ok((bot, choice, placement))
    }

    /// Count consecutive prior ended sessions of `item_id` that ended with `CONTEXT_BUDGET`
    /// since the user's last action (message, manual intervention, or resolved decision).
    fn consecutive_context_handoffs(&self, item_id: &str, current_sid: &str) -> Result<u32> {
        let last_user = self
            .db()
            .timeline(item_id)?
            .iter()
            .filter(|e| e.kind == "me" || (e.kind == "sys" && e.actor != "steve"))
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
        let since = last_user.max(last_dec);

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

    /// Deterministically extract a harness-agnostic handoff package from durable item & session
    /// state: goal & acceptance, working directory & branch, plan progress, resolved decisions,
    /// open waits, delivery feedback, and recent tools.
    ///
    /// When `compact_only` is true (supplement-threshold rollover), omits goal/cwd boilerplate if
    /// no plan/decision/tool state exists so lightweight supplement handoffs stay minimal.
    fn build_working_memory(
        &self,
        item: &Item,
        session: &Session,
        compact_only: bool,
    ) -> Result<String> {
        let mut sections: Vec<String> = vec![];

        if !compact_only {
            // 0a. Goal & acceptance criteria
            let goal_text = self
                .db()
                .goals(&item.id)?
                .into_iter()
                .last()
                .map(|g| g.text)
                .unwrap_or_else(|| item.title.clone());
            let acc_mode = item.accept_mode.as_deref().unwrap_or("inherit");
            let mut goal_lines = vec!["[目标与验收]".to_string()];
            goal_lines.push(format!("- 目标：{}", js_head(goal_text.trim(), 400)));
            goal_lines.push(format!("- 验收方式：{acc_mode}"));
            sections.push(goal_lines.join("\n"));

            // 0b. Working directory & branch
            let cwd = item
                .cwd
                .as_deref()
                .filter(|c| !c.is_empty())
                .unwrap_or(&session.cwd);
            let mut ws_lines = vec!["[工作区与分支]".to_string()];
            if !cwd.is_empty() {
                ws_lines.push(format!("- 目录：{cwd}"));
            }
            if let Some(br) = item.branch.as_deref().filter(|b| !b.is_empty()) {
                ws_lines.push(format!("- 分支：{br}"));
            }
            if ws_lines.len() > 1 {
                sections.push(ws_lines.join("\n"));
            }
        }

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

        // 2. Resolved user decisions (ask / accept / propose) & remaining open waits
        let all_decisions = self.db().decisions_of_item(&item.id)?;
        let mut resolved: Vec<_> = all_decisions
            .iter()
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

        if !compact_only {
            let open_waits: Vec<_> = all_decisions
                .iter()
                .filter(|d| d.status == "open")
                .take(MAX_WAITS)
                .collect();
            if !open_waits.is_empty() {
                let mut lines = vec!["[未结等待]".to_string()];
                for d in open_waits {
                    lines.push(format!(
                        "- [{}] {}",
                        d.kind,
                        js_head(d.question.trim(), 140)
                    ));
                }
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

fn format_handoff_opening(
    working_memory: &str,
    reason: HandoffReason,
    pct: Option<i64>,
    used: u64,
    last: &str,
) -> String {
    let tail = if last.is_empty() {
        String::new()
    } else {
        format!("上一会话的最后进展：{last}")
    };
    let head = match reason {
        HandoffReason::OwnerRequest => {
            format!("交接：主人手动将事项交接给当前新会话继续。{tail}")
        }
        HandoffReason::QuotaExhausted { .. } => {
            format!("交接：上一个执行面因额度不足切换至此新会话继续。{tail}")
        }
        HandoffReason::ProviderError => {
            format!("交接：上一个执行面因上游持续故障切换至此新会话继续。{tail}")
        }
        HandoffReason::ContextBudget => match pct {
            Some(p) => format!("交接：上一个会话的上下文用到了 {p}%。它的最后进展：{last}"),
            None => format!(
                "交接：上一个会话的上下文已达预算上限（{used} tokens）。它的最后进展：{last}"
            ),
        },
    };
    if working_memory.is_empty() {
        head
    } else {
        format!("{working_memory}\n\n{head}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_budget_handoff_target_surface_variants() {
        assert_eq!(
            parse_budget_handoff_target("same"),
            Some(HandoffTarget::default())
        );
        assert_eq!(
            parse_budget_handoff_target(""),
            Some(HandoffTarget::default())
        );
        assert_eq!(
            parse_budget_handoff_target("harness:codex"),
            Some(HandoffTarget {
                bot: None,
                harness: Some("codex".into()),
                model: None,
                effort: None,
                make_bot_default: false,
            })
        );
        assert_eq!(
            parse_budget_handoff_target("harness:openrouter/anthropic/claude-sonnet-4"),
            Some(HandoffTarget {
                bot: None,
                harness: Some("openrouter".into()),
                model: Some("anthropic/claude-sonnet-4".into()),
                effort: None,
                make_bot_default: false,
            })
        );
        assert_eq!(
            parse_budget_handoff_target("harness:codex/gpt-5.4@high"),
            Some(HandoffTarget {
                bot: None,
                harness: Some("codex".into()),
                model: Some("gpt-5.4".into()),
                effort: Some("high".into()),
                make_bot_default: false,
            })
        );
        // Automatic budget handoff never accepts bot:<id> (does not switch Bot automatically).
        assert_eq!(parse_budget_handoff_target("bot:dev"), None);
        assert_eq!(parse_budget_handoff_target("harness:"), None);
        assert_eq!(parse_budget_handoff_target("harness:codex/"), None);
        assert_eq!(parse_budget_handoff_target("harness:codex/gpt-5.4@"), None);
        assert_eq!(parse_budget_handoff_target("invalid"), None);
    }

    #[test]
    fn formats_surface_actor_with_optional_model() {
        assert_eq!(
            format_surface_actor("Dev", "Claude Code", Some("claude-opus-4-6")),
            "Dev（Claude Code · claude-opus-4-6）"
        );
        assert_eq!(format_surface_actor("Dev", "Codex", None), "Dev（Codex）");
        assert_eq!(
            format_surface_actor("Dev", "Codex", Some("  ")),
            "Dev（Codex）"
        );
    }
}

//! Deterministic working-memory handoff for quota exhaustion, sustained provider errors, and
//! manual owner handoffs (same Bot switching execution surface for this item only, or explicit Bot
//! switch).
//!
//! Design invariants (HANDOFF.md):
//! - Handoff reason is a strong-typed enum determined deterministically (never by a model).
//! - Default: keep the SAME Bot (identity/authority/memory/Team) and switch ONLY its execution
//!   surface (`harness + model + effort`) for THIS item, recording a W3.1 [`BotPlacement`] fact
//!   without altering the Bot's default configuration (unless the owner explicitly checks
//!   `make_bot_default`, which is audited and reversible).
//! - Switching to another Bot happens ONLY on explicit owner choice (or Phase 2 coordinator card).
//! - Handoff package is harness-agnostic and assembled deterministically from durable records.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::json;
use steve_node_api::{HarnessChoice, ToolPhase, ToolView};
use steve_types::{BotPlacement, PlacementBlocked, PlacementReason, PlacementRejection};

use crate::Tx;
use crate::error::{CoreError, HandoffError, Result, msg};
use crate::orchestrator::SessionStart;
use crate::repo::{Bot, BotExt, Decision, FileRef, Item, NewDecision, Session};
use crate::thread::flowback::{Update, flow_quiet};
use crate::util::js_head;

const MAX_PLAN_STEPS: usize = 20;
const MAX_DECISIONS: usize = 8;
const MAX_REFUSALS: usize = 3;
const MAX_TOOLS: usize = 8;
const MAX_WAITS: usize = 5;
const LAST_PROGRESS_CHARS: usize = 800;
const DEFAULT_QUOTA_COOLDOWN_MS: i64 = 15 * 60_000;
const DEFAULT_PROVIDER_COOLDOWN_MS: i64 = 5 * 60_000;
const MAX_COORDINATOR_BOT_OPTIONS: usize = 5;

/// Workspace setting: whether `QuotaExhausted` automatically switches to the Bot's next fallback
/// execution surface (`"on"` default / `"off"` = ask owner card only).
pub const QUOTA_AUTO_HANDOFF_SETTING: &str = "quotaAutoHandoff";
/// Workspace setting: if quota recovery is within this many minutes, wait instead of switching (`"10"` default).
pub const QUOTA_WAIT_MINUTES_SETTING: &str = "quotaWaitMinutes";
/// Decision `subject_type` for the coordinator card opened when all execution surfaces of the
/// current Bot are unavailable (never auto-switches Bot).
pub const HANDOFF_UNAVAILABLE_SUBJECT: &str = "handoff_unavailable";

/// Key identifying an execution surface for cooldown and consecutive-failure tracking.
pub(crate) type SurfaceKey = (String, Option<String>);

/// Per-item consecutive error count on the active surface plus surfaces already failed during the
/// current outage streak (guarantees each Bot candidate is tried at most once before escalating).
#[derive(Debug, Default, Clone)]
pub(crate) struct SurfaceFailureState {
    pub last_surface: Option<SurfaceKey>,
    pub consecutive_errors: u32,
    pub failed_surfaces: HashSet<SurfaceKey>,
}

/// Metadata kept for open `handoff_unavailable` coordinator decision cards so choosing `"wait"`
/// immediately arms `quota_wait`.
#[derive(Debug, Clone)]
pub(crate) struct HandoffCardMeta {
    pub session_id: String,
    pub reset_at: Option<i64>,
    pub error: String,
}

/// Outcome of attempting to switch to the next ordered fallback execution surface on the same Bot.
#[derive(Debug)]
pub(crate) enum FallbackTry {
    Switched,
    NoFallbacksConfigured,
    AllUnavailable,
}

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
    /// Target reasoning effort (`None` keeps Bot effort if harness unchanged, else `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<steve_types::ReasoningEffort>,
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
            effort: self.effort,
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
    pub reasoning_effort: steve_types::ReasoningEffort,
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
    QuotaExhausted { reset_at: Option<i64> },
    ProviderError,
    OwnerRequest,
}

impl HandoffReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::QuotaExhausted { .. } => "quota_exhausted",
            Self::ProviderError => "provider_error",
            Self::OwnerRequest => "owner_request",
        }
    }

    pub const fn placement_reason(self) -> PlacementReason {
        match self {
            Self::QuotaExhausted { .. } => PlacementReason::HandoffQuotaExhausted,
            Self::ProviderError => PlacementReason::HandoffProviderError,
            Self::OwnerRequest => PlacementReason::HandoffOwnerRequest,
        }
    }

    fn timeline_reason(self, zone: steve_runtime::tz::Zone, now: i64) -> String {
        match self {
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

/// Conservative deterministic token estimate for an opening prompt (used only for handoff
/// diagnostic telemetry; never drives control flow).
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
    /// Clear per-item consecutive surface error streaks and failed-surface memory after a healthy
    /// turn or manual owner intervention.
    pub(crate) fn clear_surface_failures(&mut self, item_id: &str) {
        self.st.surface_errors.remove(item_id);
    }

    fn session_surface_key(&self, prev: &Session) -> Result<SurfaceKey> {
        let placed = self
            .db()
            .store()
            .read(|r| steve_store::bot_dispatch::for_session(r, &prev.id))?;
        Ok((prev.harness.clone(), placed.and_then(|p| p.model)))
    }

    /// Walk the current Bot's ordered execution-surface candidates (`[primary, ...fallback_surfaces]`)
    /// and hand off to the first online, eligible, non-cooled-down surface via W3.1 placement,
    /// keeping the SAME Bot and leaving Bot defaults unchanged.
    pub(crate) fn try_next_fallback_surface(
        &mut self,
        item: &Item,
        prev: &Session,
        reason: HandoffReason,
        attempt: Option<&str>,
    ) -> Result<FallbackTry> {
        let cur_bot_id = prev
            .bot_id
            .as_deref()
            .or(item.bot_id.as_deref())
            .unwrap_or("steve");
        let Some(bot) = self.db().bot(cur_bot_id)? else {
            return Ok(FallbackTry::NoFallbacksConfigured);
        };
        let candidates = self.db().store().bots().execution_surfaces(&bot)?;
        if candidates.len() <= 1 {
            return Ok(FallbackTry::NoFallbacksConfigured);
        }

        let cur_key = self.session_surface_key(prev)?;
        let now = crate::util::now();
        {
            let streak = self.st.surface_errors.entry(item.id.clone()).or_default();
            streak.failed_surfaces.insert(cur_key.clone());
        }

        for surface in candidates {
            let cand_key: SurfaceKey = (surface.harness.clone(), surface.model.clone());
            if cand_key == cur_key {
                continue;
            }
            if self
                .st
                .surface_errors
                .get(&item.id)
                .is_some_and(|st| st.failed_surfaces.contains(&cand_key))
            {
                continue;
            }
            if self
                .st
                .surface_cooldowns
                .get(&cand_key)
                .is_some_and(|until| *until > now)
            {
                continue;
            }
            let target = HandoffTarget {
                bot: None,
                harness: Some(surface.harness),
                model: surface.model,
                effort: surface.reasoning_effort.non_default(),
                make_bot_default: false,
            };
            match self.execute_handoff(item, prev, &target, reason, "steve", attempt) {
                Ok(_outcome) => {
                    let streak = self.st.surface_errors.entry(item.id.clone()).or_default();
                    streak.last_surface = Some(cand_key);
                    streak.consecutive_errors = 0;
                    return Ok(FallbackTry::Switched);
                }
                Err(err) => {
                    tracing::info!(
                        item = %item.id,
                        bot = %cur_bot_id,
                        reason = reason.code(),
                        error = %err,
                        "fallback execution surface ineligible; trying next candidate"
                    );
                }
            }
        }
        Ok(FallbackTry::AllUnavailable)
    }

    /// Deterministic handler for quota / rate-limit failures (`HANDOFF.md` §2):
    /// 1. If recovery time is known and within workspace `quotaWaitMinutes`, wait on current surface.
    /// 2. Else if `quotaAutoHandoff` is `"on"`, switch to the Bot's next eligible execution surface.
    /// 3. If auto-handoff is `"off"` or all surfaces are unavailable, open the coordinator card
    ///    (never auto-switching Bot).
    ///
    /// Returns `Ok(true)` when handled; `Ok(false)` only for a bare transient 429 with no reset
    /// time, no hard-quota wording, and no available fallback (falls back to ordinary retry).
    pub(crate) fn handle_quota_exhausted(
        &mut self,
        item: &Item,
        session: &Session,
        error: &str,
        reset_at: Option<i64>,
        attempt: Option<&str>,
    ) -> Result<bool> {
        let now = crate::util::now();
        let cur_key = self.session_surface_key(session)?;
        self.st
            .surface_cooldowns
            .insert(cur_key, reset_at.unwrap_or(now + DEFAULT_QUOTA_COOLDOWN_MS));

        let auto_handoff =
            self.db()
                .setting(&item.workspace_id, QUOTA_AUTO_HANDOFF_SETTING, "on")?
                != "off";
        let wait_minutes: i64 = self
            .db()
            .setting(&item.workspace_id, QUOTA_WAIT_MINUTES_SETTING, "10")?
            .parse()
            .unwrap_or(10);

        if let Some(until) = reset_at
            && wait_minutes > 0
            && until - now <= wait_minutes * 60_000
        {
            self.quota_wait(&item.id, &session.id, until, error)?;
            return Ok(true);
        }

        let reason = HandoffReason::QuotaExhausted { reset_at };
        if auto_handoff {
            match self.try_next_fallback_surface(item, session, reason, attempt)? {
                FallbackTry::Switched => return Ok(true),
                FallbackTry::AllUnavailable | FallbackTry::NoFallbacksConfigured => {
                    if reset_at.is_none() && !crate::quota::is_hard_quota_error(error) {
                        return Ok(false);
                    }
                    self.open_surfaces_unavailable_card(item, session, reason, error)?;
                    return Ok(true);
                }
            }
        }

        if reset_at.is_some() || crate::quota::is_hard_quota_error(error) {
            self.open_surfaces_unavailable_card(item, session, reason, error)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Deterministic handler for repeated provider/execution failures on the same surface:
    /// - `MODEL_SERVICE_UNAVAILABLE` / `MODEL_STREAM_INTERRUPTED` (Node/Gateway already retried):
    ///   treated as reaching the consecutive-failure threshold immediately.
    /// - Generic `"error"` stops: counts consecutive failures on the same surface up to
    ///   `provider_error_handoff_threshold` (default 2).
    /// When the threshold is reached, tries the Bot's next fallback surface; if the Bot has
    /// configured fallbacks and ALL are unavailable, opens the coordinator card (never auto-switching
    /// Bot). Single-surface Bots fall through to the standard failure card.
    pub(crate) fn handle_provider_error(
        &mut self,
        item: &Item,
        session: &Session,
        stop_reason: &str,
        error: &str,
        attempt: Option<&str>,
    ) -> Result<bool> {
        let threshold = self.cfg().provider_error_handoff_threshold;
        if threshold == 0 {
            return Ok(false);
        }
        let immediate = crate::runtime::is_model_service_failure(stop_reason);
        let cur_key = self.session_surface_key(session)?;
        let reached = {
            let streak = self.st.surface_errors.entry(item.id.clone()).or_default();
            if immediate {
                streak.last_surface = Some(cur_key.clone());
                streak.consecutive_errors = threshold;
            } else if streak.last_surface.as_ref() == Some(&cur_key) {
                streak.consecutive_errors = streak.consecutive_errors.saturating_add(1);
            } else {
                streak.last_surface = Some(cur_key.clone());
                streak.consecutive_errors = 1;
            }
            streak.consecutive_errors >= threshold
        };
        if !reached {
            return Ok(false);
        }

        self.st
            .surface_cooldowns
            .insert(cur_key, crate::util::now() + DEFAULT_PROVIDER_COOLDOWN_MS);
        match self.try_next_fallback_surface(
            item,
            session,
            HandoffReason::ProviderError,
            attempt,
        )? {
            FallbackTry::Switched => Ok(true),
            FallbackTry::AllUnavailable => {
                self.open_surfaces_unavailable_card(
                    item,
                    session,
                    HandoffReason::ProviderError,
                    error,
                )?;
                Ok(true)
            }
            FallbackTry::NoFallbacksConfigured => Ok(false),
        }
    }

    /// Coordinator card opened when all execution surfaces of the current Bot are unavailable
    /// (or `quotaAutoHandoff` is off). Never switches Bot automatically; lets the owner choose
    /// another Bot, wait for recovery, hand off to Steve, pause, or drop.
    pub(crate) fn open_surfaces_unavailable_card(
        &mut self,
        item: &Item,
        session: &Session,
        reason: HandoffReason,
        error: &str,
    ) -> Result<()> {
        let cur_bot_id = session
            .bot_id
            .as_deref()
            .or(item.bot_id.as_deref())
            .unwrap_or("steve")
            .to_string();
        let cur_bot_name = self
            .db()
            .bot(&cur_bot_id)?
            .map(|b| b.display())
            .unwrap_or_else(|| cur_bot_id.clone());
        let zone = self
            .db()
            .store()
            .read(|r| steve_modules::zone::workspace_tz(r, &item.workspace_id))
            .ok()
            .and_then(|z| steve_runtime::tz::Zone::parse(z.name()).ok())
            .unwrap_or_else(steve_runtime::tz::Zone::system);
        let reason_text = reason.timeline_reason(zone, crate::util::now());
        let why = format!(
            "当前 {cur_bot_name} 的所有执行面都暂时不可用（{reason_text}）：{}。请决定如何继续。",
            js_head(error, 200)
        );
        let reset_at = match reason {
            HandoffReason::QuotaExhausted { reset_at } => reset_at,
            _ => None,
        };

        let db = self.db().clone();
        let created = db.store().write::<_, CoreError>(|_w| {
            let i = self.item(&item.id)?;
            if i.is_closed() {
                return Ok(None);
            }
            let open = self.db().decisions_of_item(&item.id)?;
            if open.iter().any(|d| {
                d.status == steve_types::DecisionStatus::Open
                    && d.kind == "fail"
                    && d.subject_type == HANDOFF_UNAVAILABLE_SUBJECT
                    && d.question == why
            }) {
                return Ok(None);
            }
            crate::lifecycle::supersede_where(
                self.db(),
                &item.id,
                crate::lifecycle::stop_card,
                "有了新的情况",
            )?;

            let mut opts: Vec<String> = Vec::new();
            if reset_at.is_some() {
                opts.push("wait".into());
            }
            let mut peers: Vec<Bot> = self
                .db()
                .bots(&i.workspace_id)?
                .into_iter()
                .filter(|b| b.enabled && b.id != cur_bot_id && b.id != "steve")
                .collect();
            peers.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
            for peer in peers.into_iter().take(MAX_COORDINATOR_BOT_OPTIONS) {
                opts.push(format!("bot:{}", peer.id));
            }
            if cur_bot_id != "steve" {
                opts.push("tosteve".into());
            }
            opts.push("look".into());
            opts.push("drop".into());

            let d = self.db().decision_create(NewDecision {
                workspace_id: i.workspace_id.clone(),
                kind: "fail".into(),
                subject_type: HANDOFF_UNAVAILABLE_SUBJECT.into(),
                subject_id: item.id.clone(),
                item_id: Some(item.id.clone()),
                session_id: Some(session.id.clone()),
                question: why.clone(),
                options: Some(opts),
                requested_by: "steve".into(),
                ..Default::default()
            })?;
            self.mark_fail(&i.workspace_id, &item.id, &why, &d.id);
            self.db()
                .timeline_add(&item.id, "steve", "sys", &why, Some(&session.id))?;
            flow_quiet(
                self.db(),
                &item.id,
                Update {
                    kind: "fail",
                    text: format!("{} 卡住了：{why}", item.id),
                    decision: Some(d.id.clone()),
                    ..Default::default()
                },
            );
            Ok(Some(d.id))
        })?;
        if let Some(decision_id) = created {
            self.st.handoff_cards.insert(
                decision_id,
                HandoffCardMeta {
                    session_id: session.id.clone(),
                    reset_at,
                    error: error.to_string(),
                },
            );
        }
        Ok(())
    }

    /// Resolve the `"wait"` option on a `handoff_unavailable` coordinator card: arms `quota_wait`
    /// when a future recovery time is known, otherwise pauses the item cleanly.
    pub(crate) fn resolve_handoff_unavailable_wait(
        &mut self,
        d: &Decision,
        cx: &steve_modules::decisions::DecisionCx<'_>,
        item_id: &str,
        actor: &str,
    ) -> Result<()> {
        self.db().store().write::<_, CoreError>(|w| {
            steve_modules::decisions::complete_answer(w, d, cx, &json!({"choice": "wait"}), None)?;
            Ok(())
        })?;
        self.clear_surface_failures(item_id);
        let meta = self.st.handoff_cards.remove(&d.id);
        let now = crate::util::now();
        let sid = meta
            .as_ref()
            .map(|m| m.session_id.clone())
            .or_else(|| d.session_id.clone());
        let reset_at = meta
            .as_ref()
            .and_then(|m| m.reset_at)
            .or_else(|| crate::quota::reset_at(&d.question, now));
        let err_text = meta
            .as_ref()
            .map(|m| m.error.clone())
            .unwrap_or_else(|| d.question.clone());
        if let (Some(sid), Some(until)) = (sid, reset_at)
            && until > now
        {
            self.quota_wait(item_id, &sid, until, &err_text)?;
        } else {
            self.pause_item(item_id)?;
            self.db().timeline_add(
                item_id,
                actor,
                "sys",
                "已暂停，等你确认恢复后继续",
                d.session_id.as_deref(),
            )?;
        }
        Ok(())
    }

    /// Resolve a `"bot:<id>"` or `"tosteve"` choice on a `handoff_unavailable` coordinator card by
    /// executing a full working-memory handoff to `target_bot` BEFORE marking the card resolved, so
    /// a blocked target leaves the card open for the owner.
    pub(crate) fn resolve_handoff_to_bot(
        &mut self,
        d: &Decision,
        cx: &steve_modules::decisions::DecisionCx<'_>,
        item_id: &str,
        target_bot: &str,
        choice: &str,
        actor: &str,
    ) -> Result<()> {
        let item = self.item(item_id)?;
        let prev = match self.active_session(item_id)? {
            Some(s) => Some(s),
            None => {
                let mut sessions = self.db().sessions_of_item(item_id)?;
                sessions.sort_by(|a, b| b.no.cmp(&a.no).then_with(|| b.created.cmp(&a.created)));
                sessions
                    .into_iter()
                    .find(|s| s.readonly == 0 && s.source == "Steve")
            }
        };
        if let Some(prev) = prev {
            self.execute_handoff(
                &item,
                &prev,
                &HandoffTarget {
                    bot: Some(target_bot.to_string()),
                    ..Default::default()
                },
                HandoffReason::OwnerRequest,
                actor,
                None,
            )?;
            self.db().store().write::<_, CoreError>(|w| {
                steve_modules::decisions::complete_answer(
                    w,
                    d,
                    cx,
                    &json!({"choice": choice}),
                    None,
                )?;
                Ok(())
            })?;
            crate::lifecycle::supersede(self.db(), item_id, &["fail"], "已手动交接新会话")?;
            crate::thread::flowback::recover_quiet(self.db(), item_id);
        } else {
            self.db().store().write::<_, CoreError>(|w| {
                steve_modules::decisions::complete_answer(
                    w,
                    d,
                    cx,
                    &json!({"choice": choice}),
                    None,
                )?;
                Ok(())
            })?;
            crate::thread::flowback::recover_quiet(self.db(), item_id);
            self.reassign(item_id, target_bot, actor)?;
        }
        self.st.handoff_cards.remove(&d.id);
        self.clear_surface_failures(item_id);
        Ok(())
    }

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
                    .ok_or(CoreError::Handoff(HandoffError::NoEligibleSession))?
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
        self.clear_surface_failures(&item.id);
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
            return Err(CoreError::Handoff(HandoffError::ReadonlySession));
        }
        let item_id = prev
            .item_id
            .clone()
            .ok_or(CoreError::Handoff(HandoffError::SessionNotBoundToItem))?;
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
        self.clear_surface_failures(&item.id);
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
            return Err(CoreError::Handoff(HandoffError::AutoBotSwitchForbidden));
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
        // Leave `handoff_unavailable` coordinator cards open so the resolving call settles them
        // with its explicit choice receipt (and manual handoffs supersede them cleanly afterward).
        for d in self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .filter(|d| {
                d.status == steve_types::DecisionStatus::Open
                    && d.session_id.as_deref() == Some(prev.id.as_str())
                    && d.subject_type != HANDOFF_UNAVAILABLE_SUBJECT
            })
        {
            self.db().decision_resolve(
                &d.id,
                json!({"superseded": "会话已交接"}),
                steve_store::decision_repo::DecisionChange::Expire,
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

        let mut opening =
            format_handoff_opening(&working_memory, reason, js_head(&last, LAST_PROGRESS_CHARS));
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
        let effective_effort = choice.effective_effort();
        let bot_default_updated = if target.make_bot_default
            && matches!(reason, HandoffReason::OwnerRequest)
            && (next_bot.harness.as_str().ne(choice.harness.as_str())
                || next_bot.model != choice.model
                || next_bot.reasoning_effort != effective_effort)
        {
            let previous = BotDefaultSnapshot {
                harness: next_bot.harness.clone(),
                model: next_bot.model.clone(),
                reasoning_effort: next_bot.reasoning_effort,
            };
            let mut patch = serde_json::Map::new();
            patch.insert("harness".into(), json!(choice.harness));
            patch.insert("model".into(), json!(choice.model));
            patch.insert("reasoning_effort".into(), json!(effective_effort));
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
                        "reasoning_effort": effective_effort,
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
            .and_then(|p| p.reasoning_effort.non_default());

        let (bot_filter, rt_filter, model_filter, effort_filter) = match target.bot.as_deref() {
            // Explicit Bot switch (OwnerRequest): use that Bot's surface unless overridden.
            Some(bot_id) => (
                bot_id,
                target.harness.as_deref(),
                target.model.as_deref(),
                target.effort,
            ),
            // Default: keep the SAME Bot; switch only execution surface for this item.
            None => match target.harness.as_deref() {
                Some(req_rt) => (
                    cur_bot_id,
                    Some(req_rt),
                    target.model.as_deref(),
                    target.effort,
                ),
                None => (
                    cur_bot_id,
                    Some(prev.harness.as_str()),
                    target.model.as_deref().or(prev_placed_model),
                    target
                        .effort
                        .or(prev_placed_effort)
                        .or_else(|| prev.reasoning_effort.non_default()),
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
                title: &item.title,
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
            effort: placement.reasoning_effort.non_default(),
        };
        Ok((bot, choice, placement))
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
                d.status == steve_types::DecisionStatus::Resolved
                    && matches!(d.kind.as_str(), "ask" | "accept" | "propose")
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
                .filter(|d| {
                    d.status == steve_types::DecisionStatus::Open
                        && d.subject_type != HANDOFF_UNAVAILABLE_SUBJECT
                })
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
}

fn format_handoff_opening(working_memory: &str, reason: HandoffReason, last: &str) -> String {
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

    #[test]
    fn handoff_errors_expose_ascii_codes_without_localized_sentences() {
        for (err, code, status) in [
            (
                HandoffError::NoEligibleSession,
                "handoff.no_eligible_session",
                409,
            ),
            (
                HandoffError::ReadonlySession,
                "handoff.readonly_session",
                409,
            ),
            (
                HandoffError::SessionNotBoundToItem,
                "handoff.session_not_bound_to_item",
                409,
            ),
            (
                HandoffError::AutoBotSwitchForbidden,
                "handoff.auto_bot_switch_forbidden",
                400,
            ),
        ] {
            let text = CoreError::Handoff(err).to_string();
            assert_eq!(text, code);
            assert!(text.is_ascii());
            assert_eq!(err.http_status(), status);
        }
    }
}

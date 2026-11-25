//! Harness adapter layer: runtime ACP capability negotiation, per-harness event/tool
//! normalization, and native subtask operations.
//!
//! Clear separation from `steve-harness-spec/src/builtins.toml`:
//! - `builtins.toml` owns static installation, launch commands, state-directory isolation,
//!   permission modes, and fallback model/effort lists.
//! - `steve-acp::adapter` owns runtime ACP protocol semantics: which client capabilities to
//!   negotiate, how harness-specific ACP notifications and tool calls normalize into unified
//!   `SubtaskView` / `ToolView.parent` / `UsageScope` facts, and how native subtask operations
//!   (such as per-child `session/cancel`) are encoded.
//!
//! Harness-specific branching stays strictly inside this module tree; Hub business logic and Web
//! only consume unified `SubtaskView` / `ToolView` contracts and [`AdapterCapabilities`].

pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod generic;
pub mod kimi;
pub mod pi;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use steve_node_api::{
    AdapterCapabilities, CallId, Epoch, EventBody, Measurement, Money, Part, SubtaskOrigin,
    SubtaskStatus, SubtaskUsage, SubtaskView, Tokens, ToolView, UsageReport, UsageScope,
};

use crate::normalize::{Fact, ToolFact, bound, call_id};

/// Maximum characters kept for a subtask's title (matches `ToolView.title`).
pub const SUBTASK_TITLE_MAX: usize = 200;
/// Maximum characters kept for bounded subtask progress, summary, or error text.
pub const SUBTASK_TEXT_MAX: usize = 1000;

/// Result of handing a `session/update` payload to a [`HarnessAdapter`].
#[derive(Debug)]
pub enum AdapterOutcome {
    /// The adapter fully normalized this update (skip generic `Normalizer::update` handling).
    Handled(Vec<Fact>),
    /// Let `Normalizer::update` process this update through the standard ACP path.
    PassThrough,
}

/// Per-harness runtime adapter.
pub trait HarnessAdapter: Send + Sync {
    /// Canonical harness id (`"claude-code"`, `"codex"`, `"antigravity"`, `"kimi-code"`, `"pi"`,
    /// or `"generic"` for all unverified builtin/custom harnesses).
    fn id(&self) -> &'static str;

    /// Strongly-typed runtime capabilities advertised to Hub and Web.
    fn capabilities(&self) -> AdapterCapabilities;

    /// Mutate `initialize.params.clientCapabilities` before sending the ACP handshake.
    /// Must never inject behavior-changing overrides (e.g. compaction thresholds).
    fn configure_client_capabilities(&self, _caps: &mut Value) {}

    /// Optional canonical tool-name recovery when a harness omits `name` / `_meta.claudeCode.toolName`
    /// and encodes the tool name only in its initial `tool_call.title`.
    fn infer_tool_name(&self, _raw: &Value, _initial_title: Option<&str>) -> Option<String> {
        None
    }

    /// Optional tool result enrichment from harness-specific `_meta` fields (e.g. terminal deltas).
    fn enrich_tool_result(&self, _raw: &Value, _existing: &mut Vec<Part>) {}

    /// Intercept harness-specific or child-session `session/update` frames before standard
    /// normalization.
    fn normalize_update(
        &self,
        ctx: &mut AdapterContext<'_>,
        session_id: Option<&str>,
        update: &Value,
    ) -> AdapterOutcome;

    /// Observe a merged `tool_call`/`tool_call_update`, attaching `ToolView.parent` and emitting
    /// unified `SubtaskChanged` (and optional `UsageScope::Subtask`) facts when applicable.
    fn observe_tool(
        &self,
        _ctx: &mut AdapterContext<'_>,
        _session_id: Option<&str>,
        _raw_id: &str,
        _fact: &ToolFact,
        _view: &mut ToolView,
    ) -> Vec<Fact> {
        vec![]
    }

    /// Encode a native JSON-RPC notification to cancel a single active subtask without cancelling
    /// the parent turn, when supported by this harness and subtask.
    fn cancel_subtask_request(&self, subtask: &SubtaskView) -> Option<(&'static str, Value)> {
        rfd_cancel_request(subtask)
    }

    /// Upgradeable components for this harness.
    ///
    /// Reserved hook for Node-side harness upgrades: currently delegates to
    /// [`crate::adapters::update_components`] so upgrade definitions stay in one place until the
    /// parallel Node harness-upgrade track merges into per-harness adapters.
    fn update_components(&self, cfg: &crate::AcpConfig) -> Vec<crate::adapters::UpdateComponent> {
        crate::adapters::update_components(cfg, self.id())
    }
}

/// Resolve the runtime [`HarnessAdapter`] for a harness id.
pub fn for_harness(id: &str) -> Arc<dyn HarnessAdapter> {
    match id {
        "claude-code" => Arc::new(claude_code::ClaudeCodeAdapter),
        "codex" => Arc::new(codex::CodexAdapter),
        "antigravity" => Arc::new(antigravity::AntigravityAdapter),
        "kimi-code" => Arc::new(kimi::KimiAdapter),
        "pi" => Arc::new(pi::PiAdapter),
        _ => Arc::new(generic::GenericAdapter),
    }
}

/// Strongly-typed runtime capabilities for `id`.
pub fn capabilities_for(id: &str) -> AdapterCapabilities {
    for_harness(id).capabilities()
}

/// Mutable normalization context passed to [`HarnessAdapter`] methods.
pub struct AdapterContext<'a> {
    pub state: &'a mut SubtaskState,
    pub root_session_id: Option<&'a str>,
    pub prefix: &'a str,
    pub meter: &'a Epoch,
    pub input_includes_cache: bool,
}

impl AdapterContext<'_> {
    /// Whether `session_id` belongs to a child session distinct from the root ACP session.
    pub fn is_child_session(&self, session_id: Option<&str>) -> bool {
        matches!(
            (self.root_session_id, session_id),
            (Some(root), Some(sid)) if !sid.is_empty() && sid != root
        )
    }

    /// Build a content-addressed `UsageScope::Subtask` delta report for a settled subtask.
    pub fn subtask_usage_report(
        &self,
        subtask_id: &CallId,
        model: Option<String>,
        tokens: Tokens,
        cost: Option<Money>,
    ) -> UsageReport {
        let mut report = UsageReport {
            report_id: String::new(),
            model: model.filter(|m| !m.is_empty()),
            meter_epoch: self.meter.clone(),
            scope: UsageScope::Subtask,
            measurement: Measurement::Delta,
            tokens,
            context_used: None,
            context_limit: None,
            cost,
        };
        let scoped_prefix = format!("{}:subtask:{subtask_id}", self.prefix);
        crate::usage::identify(&scoped_prefix, &mut report);
        report
    }
}

#[derive(Debug, Clone)]
pub struct TrackedSubtask {
    pub view: SubtaskView,
    pub started_at: Instant,
    pub emitted_usage: bool,
}

/// Attempt-local subtask & tool-timing state owned by [`crate::normalize::Normalizer`].
#[derive(Debug, Default)]
pub struct SubtaskState {
    /// Logical key -> tracked subtask (`rfd:<sid>`, `tool:<callId>`, `thread:<id>`, etc.).
    pub(crate) subtasks: HashMap<String, TrackedSubtask>,
    /// `SubtaskView.id` -> logical key in `subtasks`.
    pub(crate) by_call_id: HashMap<CallId, String>,
    /// Harness-side external handle (`agentId`, `task_id`, `backgroundTaskId`, output file path)
    /// -> logical key in `subtasks`.
    pub(crate) external_to_key: HashMap<String, String>,
    /// ACP RFD `session_message.messageId` (e.g. Claude's `toolu_...`) -> child `SubtaskView.id`.
    pub(crate) message_to_subtask: HashMap<String, CallId>,
    /// First observed `title` per `toolCallId` (preserved before streaming updates overwrite it).
    pub(crate) initial_tool_titles: HashMap<String, String>,
    /// Monotonic start instant per `toolCallId`.
    pub(crate) tool_started_at: HashMap<String, Instant>,
    /// Settled duration in milliseconds per `toolCallId`.
    pub(crate) tool_duration_ms: HashMap<String, u64>,
}

impl SubtaskState {
    pub fn record_tool_timing(&mut self, raw_id: &str, settled: bool) -> Option<u64> {
        let started = *self
            .tool_started_at
            .entry(raw_id.to_string())
            .or_insert_with(Instant::now);
        if settled {
            let ms = *self
                .tool_duration_ms
                .entry(raw_id.to_string())
                .or_insert_with(|| started.elapsed().as_millis() as u64);
            Some(ms)
        } else {
            self.tool_duration_ms.get(raw_id).copied()
        }
    }

    pub fn record_initial_title(&mut self, raw_id: &str, title: Option<&str>) -> Option<&str> {
        if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
            self.initial_tool_titles
                .entry(raw_id.to_string())
                .or_insert_with(|| t.to_string());
        }
        self.initial_tool_titles.get(raw_id).map(String::as_str)
    }

    pub fn lookup_by_call_id(&self, id: &CallId) -> Option<&SubtaskView> {
        let key = self.by_call_id.get(id)?;
        self.subtasks.get(key).map(|t| &t.view)
    }

    pub fn lookup_by_external(&self, external: &str) -> Option<&SubtaskView> {
        let key = self.external_to_key.get(external)?;
        self.subtasks.get(key).map(|t| &t.view)
    }

    pub fn lookup_key_by_external(&self, external: &str) -> Option<String> {
        self.external_to_key.get(external).cloned()
    }

    pub fn has_rfd_subagent(&self, child_sid: &str) -> bool {
        self.subtasks.contains_key(&format!("rfd:{child_sid}"))
    }

    pub fn register_alias(&mut self, alias: impl Into<String>, key: &str) {
        let alias = alias.into();
        if !alias.is_empty() {
            self.external_to_key.insert(alias, key.to_string());
        }
    }

    /// Upsert a tracked subtask under `key`, keeping its monotonic start time and indexing its
    /// `CallId` and `external_id`.
    pub fn upsert(
        &mut self,
        key: &str,
        id: CallId,
        origin: SubtaskOrigin,
        default_title: &str,
        mutate: impl FnOnce(&mut TrackedSubtask),
    ) -> SubtaskView {
        let entry = self
            .subtasks
            .entry(key.to_string())
            .or_insert_with(|| TrackedSubtask {
                view: SubtaskView {
                    id: id.clone(),
                    parent: None,
                    external_id: None,
                    origin,
                    status: SubtaskStatus::Running,
                    title: bound(default_title, SUBTASK_TITLE_MAX),
                    actor: None,
                    model: None,
                    progress: None,
                    summary: None,
                    error: None,
                    can_cancel: false,
                    duration_ms: None,
                    usage: None,
                },
                started_at: Instant::now(),
                emitted_usage: false,
            });
        mutate(entry);
        if is_terminal(entry.view.status) {
            entry.view.can_cancel = false;
            if entry.view.duration_ms.is_none() {
                entry.view.duration_ms = Some(entry.started_at.elapsed().as_millis() as u64);
            }
        }
        let view = entry.view.clone();
        self.by_call_id.insert(view.id.clone(), key.to_string());
        if let Some(ext) = &view.external_id {
            self.external_to_key.insert(ext.clone(), key.to_string());
        }
        view
    }

    /// Resolve a child tool call's parent `SubtaskView.id` from its ACP `sessionId` or
    /// `parentToolUseId`.
    pub fn resolve_parent(
        &self,
        root_sid: Option<&str>,
        session_id: Option<&str>,
        parent_tool_use_id: Option<&str>,
    ) -> Option<CallId> {
        if let (Some(root), Some(sid)) = (root_sid, session_id)
            && !sid.is_empty()
            && sid != root
        {
            let key = format!("rfd:{sid}");
            if let Some(t) = self.subtasks.get(&key) {
                return Some(t.view.id.clone());
            }
            return Some(call_id(sid));
        }
        if let Some(pid) = parent_tool_use_id.filter(|s| !s.is_empty()) {
            if let Some(mapped) = self.message_to_subtask.get(pid) {
                return Some(mapped.clone());
            }
            let tool_key = format!("tool:{pid}");
            if let Some(t) = self.subtasks.get(&tool_key) {
                return Some(t.view.id.clone());
            }
            return Some(call_id(pid));
        }
        None
    }
}

pub(crate) fn is_terminal(status: SubtaskStatus) -> bool {
    matches!(
        status,
        SubtaskStatus::Done | SubtaskStatus::Failed | SubtaskStatus::Cancelled
    )
}

/// Standard ACP RFD #1992 per-child `session/cancel` encoding.
pub(crate) fn rfd_cancel_request(subtask: &SubtaskView) -> Option<(&'static str, Value)> {
    if !subtask.can_cancel || is_terminal(subtask.status) {
        return None;
    }
    let sid = subtask.external_id.as_deref().filter(|s| !s.is_empty())?;
    Some(("session/cancel", json!({ "sessionId": sid })))
}

/// Standard ACP RFD #1992 `subagent_update` normalizer (shared by Claude Code, Codex 2.12+, and
/// generic forward-compat). Merges partial frames so incremental updates (like Codex sending
/// `title`, `state`, and `capabilities` in separate notifications) preserve prior fields.
pub(crate) fn handle_rfd_subagent_update(
    ctx: &mut AdapterContext<'_>,
    parent_session_id: Option<&str>,
    u: &Value,
) -> Option<Vec<Fact>> {
    let child_sid = u
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let key = format!("rfd:{child_sid}");
    let id = call_id(child_sid);
    let parent_subtask = parent_session_id
        .filter(|sid| ctx.is_child_session(Some(*sid)))
        .map(call_id);
    let view = ctx
        .state
        .upsert(&key, id, SubtaskOrigin::Subagent, "Subagent", |tracked| {
            let v = &mut tracked.view;
            v.external_id = Some(child_sid.to_string());
            if v.parent.is_none() {
                v.parent = parent_subtask;
            }
            if let Some(title) = u
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                v.title = bound(title, SUBTASK_TITLE_MAX);
            }
            if let Some(desc) = u
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                v.actor = Some(bound(desc, SUBTASK_TITLE_MAX));
                if v.title == "Subagent" {
                    v.title = bound(desc, SUBTASK_TITLE_MAX);
                }
            }
            // Note: in ACP RFD #1992 (verified on claude-agent-acp 0.99.1 and codex-acp 2.12.0),
            // `capabilities.cancel` is an empty object `{}` when supported, and `null` or `false`
            // when cleared.
            if let Some(cancel) = u.pointer("/capabilities/cancel") {
                v.can_cancel = matches!(cancel, Value::Bool(true) | Value::Object(_));
            }
            if let Some(state_obj) = u.get("state").and_then(Value::as_object) {
                let kind = state_obj.get("state").and_then(Value::as_str).unwrap_or("");
                let stop_reason = state_obj
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let err_text = state_obj.get("error").and_then(|e| {
                    e.as_str()
                        .map(String::from)
                        .or_else(|| e.get("message").and_then(Value::as_str).map(String::from))
                        .or_else(|| (!e.is_null()).then(|| e.to_string()))
                });
                match kind {
                    "queued" | "pending" => v.status = SubtaskStatus::Queued,
                    "running" | "in_progress" => v.status = SubtaskStatus::Running,
                    "requires_action" | "waiting" => v.status = SubtaskStatus::NeedsYou,
                    "cancelled" | "canceled" => v.status = SubtaskStatus::Cancelled,
                    "failed" | "error" => {
                        v.status = SubtaskStatus::Failed;
                        if let Some(err) = err_text {
                            v.error = Some(bound(&err, SUBTASK_TEXT_MAX));
                        }
                    }
                    "idle" | "completed" | "done" => {
                        if let Some(err) = err_text.filter(|s| !s.is_empty()) {
                            v.status = SubtaskStatus::Failed;
                            v.error = Some(bound(&err, SUBTASK_TEXT_MAX));
                        } else if stop_reason == "cancelled" || stop_reason == "canceled" {
                            v.status = SubtaskStatus::Cancelled;
                        } else {
                            v.status = SubtaskStatus::Done;
                        }
                    }
                    _ => {}
                }
            }
        });
    tracing::debug!(
        subtask_id = %view.id,
        status = ?view.status,
        can_cancel = view.can_cancel,
        "acp adapter: normalized rfd subagent_update"
    );
    Some(vec![Fact::Event(EventBody::SubtaskChanged {
        subtask: view,
    })])
}

/// Standard ACP RFD #1992 `session_message` handler: links `messageId` (used by
/// `parentToolUseId`) to the child subtask and captures bounded prompt/reply previews.
pub(crate) fn handle_rfd_session_message(
    ctx: &mut AdapterContext<'_>,
    u: &Value,
) -> Option<Vec<Fact>> {
    let sender = u
        .get("senderSessionId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let recipient = u
        .get("recipientSessionId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let msg_id = u.get("messageId").and_then(Value::as_str).unwrap_or("");
    let text = extract_content_text(u.get("content"));

    let recipient_key = format!("rfd:{recipient}");
    let sender_key = format!("rfd:{sender}");

    // Parent -> child dispatch message
    if !recipient.is_empty() && ctx.state.subtasks.contains_key(&recipient_key) {
        let child_id = ctx.state.subtasks[&recipient_key].view.id.clone();
        if !msg_id.is_empty() {
            ctx.state
                .message_to_subtask
                .insert(msg_id.to_string(), child_id.clone());
        }
        if let Some(prompt) = text.filter(|t| !t.trim().is_empty()) {
            let tracked = ctx.state.subtasks.get_mut(&recipient_key)?;
            if tracked.view.progress.is_none() {
                tracked.view.progress = Some(bound(prompt.trim(), SUBTASK_TEXT_MAX));
                return Some(vec![Fact::Event(EventBody::SubtaskChanged {
                    subtask: tracked.view.clone(),
                })]);
            }
        }
        return Some(vec![Fact::Nothing]);
    }

    // Child -> parent reply message
    if !sender.is_empty() && ctx.state.subtasks.contains_key(&sender_key) {
        if let Some(reply) = text.filter(|t| !t.trim().is_empty()) {
            let trimmed = reply.trim();
            // Claude Code emits an internal "Async agent launched successfully..." metadata reply
            // on launch; ignore that boilerplate so we don't pollute the user-facing summary.
            if !trimmed.starts_with("Async agent launched successfully") {
                let tracked = ctx.state.subtasks.get_mut(&sender_key)?;
                tracked.view.summary = Some(bound(trimmed, SUBTASK_TEXT_MAX));
                return Some(vec![Fact::Event(EventBody::SubtaskChanged {
                    subtask: tracked.view.clone(),
                })]);
            }
        }
        return Some(vec![Fact::Nothing]);
    }

    Some(vec![Fact::Nothing])
}

/// Handle `session/update` notifications arriving on a registered RFD child session.
/// Keeps child text/config/usage scoped to the child `SubtaskView` so it never leaks into
/// the root session's assistant reply, model selector, or context window meter.
pub(crate) fn handle_rfd_child_update(
    ctx: &mut AdapterContext<'_>,
    child_sid: &str,
    u: &Value,
) -> AdapterOutcome {
    let kind = u.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
    let key = format!("rfd:{child_sid}");
    match kind {
        "tool_call" | "tool_call_update" => AdapterOutcome::PassThrough,
        "subagent_update" => match handle_rfd_subagent_update(ctx, Some(child_sid), u) {
            Some(facts) => AdapterOutcome::Handled(facts),
            None => AdapterOutcome::Handled(vec![Fact::Nothing]),
        },
        "session_message" => match handle_rfd_session_message(ctx, u) {
            Some(facts) => AdapterOutcome::Handled(facts),
            None => AdapterOutcome::Handled(vec![Fact::Nothing]),
        },
        "config_option_update" => {
            let Some(tracked) = ctx.state.subtasks.get_mut(&key) else {
                return AdapterOutcome::Handled(vec![Fact::Nothing]);
            };
            let mut changed = false;
            if let Some(opts) = u.get("configOptions").and_then(Value::as_array) {
                for o in opts {
                    let id = o.get("id").and_then(Value::as_str).unwrap_or("");
                    let cat = o.get("category").and_then(Value::as_str).unwrap_or("");
                    let val = o
                        .get("currentValue")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    if let Some(v) = val {
                        if id == "model" || cat == "model" {
                            if tracked.view.model.as_deref() != Some(v) {
                                tracked.view.model = Some(v.to_string());
                                changed = true;
                            }
                        } else if id == "agent" && tracked.view.actor.is_none() {
                            tracked.view.actor = Some(bound(v, SUBTASK_TITLE_MAX));
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                AdapterOutcome::Handled(vec![Fact::Event(EventBody::SubtaskChanged {
                    subtask: tracked.view.clone(),
                })])
            } else {
                AdapterOutcome::Handled(vec![Fact::Nothing])
            }
        }
        "usage_update" => {
            let Some(tracked) = ctx.state.subtasks.get_mut(&key) else {
                return AdapterOutcome::Handled(vec![Fact::Nothing]);
            };
            let used = u.get("used").and_then(Value::as_u64);
            let size = u.get("size").and_then(Value::as_u64);
            let cost = usd_cost(u.get("cost"));
            if used.is_none() && size.is_none() && cost.is_none() {
                return AdapterOutcome::Handled(vec![Fact::Nothing]);
            }
            let usage = tracked.view.usage.get_or_insert_with(SubtaskUsage::default);
            if used.is_some() {
                usage.context_used = used;
            }
            if size.is_some() {
                usage.context_limit = size;
            }
            if cost.is_some() {
                usage.cost = cost;
            }
            AdapterOutcome::Handled(vec![Fact::Event(EventBody::SubtaskChanged {
                subtask: tracked.view.clone(),
            })])
        }
        "agent_message_chunk" => {
            if let Some(chunk) = u.pointer("/content/text").and_then(Value::as_str)
                && !chunk.is_empty()
                && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
            {
                let buf = tracked.view.summary.get_or_insert_with(String::new);
                if buf.chars().count() < SUBTASK_TEXT_MAX {
                    buf.push_str(chunk);
                    if buf.chars().count() > SUBTASK_TEXT_MAX {
                        *buf = bound(buf, SUBTASK_TEXT_MAX);
                    }
                }
            }
            AdapterOutcome::Handled(vec![Fact::Nothing])
        }
        // Keep child thoughts/modes/commands/info out of the parent transcript stream.
        _ => AdapterOutcome::Handled(vec![Fact::Nothing]),
    }
}

/// Forward-compatible handler for ACP `async_task_started` / `async_task_progress` /
/// `async_task_completed` notifications.
pub(crate) fn handle_async_task_update(
    ctx: &mut AdapterContext<'_>,
    kind: &str,
    u: &Value,
) -> Option<Vec<Fact>> {
    let task_id = u
        .get("taskId")
        .or_else(|| u.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let key = format!("async:{task_id}");
    let id = call_id(&key);
    let view = ctx.state.upsert(
        &key,
        id,
        SubtaskOrigin::BackgroundTask,
        "Background task",
        |tracked| {
            let v = &mut tracked.view;
            v.external_id = Some(task_id.to_string());
            if let Some(title) = u
                .get("title")
                .or_else(|| u.get("description"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                v.title = bound(title, SUBTASK_TITLE_MAX);
            }
            if let Some(actor) = u
                .get("kind")
                .or_else(|| u.get("toolName"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                v.actor = Some(bound(actor, SUBTASK_TITLE_MAX));
            }
            match kind {
                "async_task_started" => v.status = SubtaskStatus::Running,
                "async_task_progress" => {
                    v.status = SubtaskStatus::Running;
                    if let Some(msg) = u
                        .get("message")
                        .or_else(|| u.get("progress"))
                        .or_else(|| u.get("output"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.progress = Some(bound(msg, SUBTASK_TEXT_MAX));
                    }
                }
                "async_task_completed" => {
                    let st = u.get("status").and_then(Value::as_str).unwrap_or("");
                    let exit_code = u.get("exitCode").and_then(Value::as_i64);
                    if st == "cancelled" || st == "canceled" {
                        v.status = SubtaskStatus::Cancelled;
                    } else if st == "failed" || st == "error" || exit_code.is_some_and(|c| c != 0) {
                        v.status = SubtaskStatus::Failed;
                        if let Some(err) = u
                            .get("error")
                            .or_else(|| u.get("output"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            v.error = Some(bound(err, SUBTASK_TEXT_MAX));
                        }
                    } else {
                        v.status = SubtaskStatus::Done;
                        if let Some(out) = u
                            .get("summary")
                            .or_else(|| u.get("output"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            v.summary = Some(bound(out, SUBTASK_TEXT_MAX));
                        }
                    }
                }
                _ => {}
            }
        },
    );
    Some(vec![Fact::Event(EventBody::SubtaskChanged {
        subtask: view,
    })])
}

pub(crate) fn extract_content_text(content: Option<&Value>) -> Option<String> {
    let v = content?;
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_string());
    }
    let arr = v.as_array()?;
    let parts: Vec<&str> = arr
        .iter()
        .filter_map(|item| {
            item.get("text")
                .and_then(Value::as_str)
                .or_else(|| item.pointer("/content/text").and_then(Value::as_str))
        })
        .filter(|s| !s.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn tool_text_output(fact: &ToolFact) -> Option<String> {
    let from_parts: Vec<&str> = fact
        .result
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if !from_parts.is_empty() {
        return Some(from_parts.join("\n"));
    }
    match fact.raw.get("rawOutput") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(Value::Array(_)) => extract_content_text(fact.raw.get("rawOutput")),
        Some(Value::Object(obj)) => obj
            .get("output")
            .or_else(|| obj.get("stdout"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(String::from),
        _ => None,
    }
}

pub(crate) fn usd_cost(cost: Option<&Value>) -> Option<Money> {
    let c = cost?;
    let amount = c.get("amount").and_then(Value::as_f64)?;
    if !amount.is_finite() || amount <= 0.0 {
        return None;
    }
    let currency = match c.get("currency") {
        None => "USD",
        Some(Value::String(s)) if s.trim().eq_ignore_ascii_case("USD") => "USD",
        Some(_) => return None,
    };
    let s = format!("{amount:.10}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    Some(Money {
        decimal: if s.is_empty() { "0".into() } else { s.into() },
        currency: currency.into(),
    })
}

pub(crate) fn usd_amount(amount: f64) -> Option<Money> {
    if !amount.is_finite() || amount <= 0.0 {
        return None;
    }
    let s = format!("{amount:.10}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    Some(Money {
        decimal: if s.is_empty() { "0".into() } else { s.into() },
        currency: "USD".into(),
    })
}

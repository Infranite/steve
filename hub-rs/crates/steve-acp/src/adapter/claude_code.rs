//! Claude Code (`@agentclientprotocol/claude-agent-acp`) runtime adapter.
//!
//! Verified empirically against `claude` 2.1.308 + `claude-agent-acp` 0.99.1:
//! - Negotiating `clientCapabilities.subagents: {}` (WITHOUT `_meta.jetbrains.air`) selects
//!   ACP RFD #1992 mode with `perTaskStopAffordance: true` and per-child `session/cancel`.
//! - Emits `subagent_update` (`capabilities.cancel: {}`, `state: {state, stopReason?, error?}`),
//!   `session_message` (linking parent `toolu_*` `messageId` to child `sessionId`), child
//!   `config_option_update` (model/agent), and child `tool_call`/`tool_call_update` carrying
//!   `_meta.claudeCode.parentToolUseId`.
//! - Fallback mode (when `subagents` is not negotiated): emits root `Agent`/`Task` tool calls
//!   with `_meta.claudeCode.toolResponse.{isAsync, status, agentId, resolvedModel, outputFile}`
//!   and child tool calls in root session with `_meta.claudeCode.parentToolUseId`.
//! - Background tasks: `Bash` with `rawInput.run_in_background == true` (returning
//!   `_meta.claudeCode.toolResponse.backgroundTaskId` and `.output` path), `Monitor`, `TaskStop`,
//!   plus forward-compatible `async_task_*`.
//! - Usage: `PromptResponse.usage` (`Main`) + `_meta.quota.model_usage` (`All`, covers subagents)
//!   + `usage_update` with `_meta["_claude/origin"]` (`Background`).

use serde_json::{Value, json};
use steve_node_api::{
    AUTO_MODE_CLASSIFIER_UNAVAILABLE, AdapterCapabilities, BackgroundTaskCapability, EventBody,
    ResumeCapability, Severity, SubagentCapability, SubtaskOrigin, SubtaskStatus, ToolCategory,
    ToolPhase, ToolView, UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, CLASSIFIER_UNAVAILABLE_WARN_THRESHOLD, HarnessAdapter,
    SUBTASK_TEXT_MAX, SUBTASK_TITLE_MAX, handle_async_task_update, handle_rfd_child_update,
    handle_rfd_session_message, handle_rfd_subagent_update, tool_text_output,
};
use crate::normalize::{Fact, ToolFact, bound, call_id};

/// Lifecycle phase of Claude Code's `system/permission_check_status` notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionCheckPhase {
    Checking,
    Done,
}

impl PermissionCheckPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::Done => "done",
        }
    }
}

/// Strongly-typed `permission_check_status` payload emitted by Claude Code when an auto-mode
/// classifier check takes long enough to report progress (via ACP stdout or `claude-agent-acp`
/// stderr `Unexpected case:` fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionCheckEvent {
    pub phase: PermissionCheckPhase,
    pub tool_use_id: Option<String>,
    pub agent_id: Option<String>,
}

/// Classification of an auto-mode safety classifier failure observed on a settled tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifierDenialKind {
    /// A single tool call was denied because the classifier returned no safety verdict.
    Unavailable,
    /// The harness stopped auto mode for the turn after repeated responses with no safety verdict.
    Stopped,
}

impl ClassifierDenialKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "denied",
            Self::Stopped => "stopped",
        }
    }
}

pub struct ClaudeCodeAdapter;

impl HarnessAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::AcpRfd,
            subagent_cancel: true,
            background_tasks: BackgroundTaskCapability::ToolFallback,
            background_task_cancel: false,
            child_tool_nesting: true,
            usage_attribution: UsageAttribution::TurnAndModelAll,
            resume: ResumeCapability::SessionLoad,
            upgrade: steve_node_api::UpgradeCapability::ManagedNpm,
            effort_mode: steve_node_api::EffortMode::AcpOption,
            verified: true,
        }
    }

    fn configure_client_capabilities(&self, caps: &mut Value) {
        if let Some(obj) = caps.as_object_mut() {
            // Standard ACP RFD #1992 subagents. Deliberately omit `_meta.jetbrains.air` because
            // claude-agent-acp treats its presence as legacy AIR draft and disables per-child cancel.
            obj.insert("subagents".into(), json!({}));
        }
    }

    fn observe_stderr(&self, tag: &str, line: &str) -> bool {
        let Some(ev) = parse_stderr_permission_check(line) else {
            return false;
        };
        record_permission_check_metric(self.id(), ev.phase, "stderr");
        tracing::debug!(
            tag = %tag,
            harness = self.id(),
            status = ev.phase.as_str(),
            tool_use_id = ev.tool_use_id.as_deref().unwrap_or(""),
            agent_id = ev.agent_id.as_deref().unwrap_or(""),
            "acp adapter: permission_check_status (stderr)"
        );
        true
    }

    fn normalize_update(
        &self,
        ctx: &mut AdapterContext<'_>,
        session_id: Option<&str>,
        update: &Value,
    ) -> AdapterOutcome {
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        if kind == "permission_check_status" {
            if let Some(ev) = parse_permission_check_status(update) {
                record_permission_check_metric(self.id(), ev.phase, "acp");
                tracing::debug!(
                    harness = self.id(),
                    status = ev.phase.as_str(),
                    tool_use_id = ev.tool_use_id.as_deref().unwrap_or(""),
                    agent_id = ev.agent_id.as_deref().unwrap_or(""),
                    "acp adapter: permission_check_status"
                );
            }
            return AdapterOutcome::Handled(vec![Fact::Nothing]);
        }
        if ctx.is_child_session(session_id) {
            return handle_rfd_child_update(ctx, session_id.unwrap_or_default(), update);
        }
        match kind {
            "subagent_update" => match handle_rfd_subagent_update(ctx, session_id, update) {
                Some(facts) => AdapterOutcome::Handled(facts),
                None => AdapterOutcome::Handled(vec![Fact::Nothing]),
            },
            "session_message" => match handle_rfd_session_message(ctx, update) {
                Some(facts) => AdapterOutcome::Handled(facts),
                None => AdapterOutcome::Handled(vec![Fact::Nothing]),
            },
            "async_task_started" | "async_task_progress" | "async_task_completed" => {
                match handle_async_task_update(ctx, kind, update) {
                    Some(facts) => AdapterOutcome::Handled(facts),
                    None => AdapterOutcome::Handled(vec![Fact::Nothing]),
                }
            }
            _ => AdapterOutcome::PassThrough,
        }
    }

    fn observe_tool(
        &self,
        ctx: &mut AdapterContext<'_>,
        session_id: Option<&str>,
        raw_id: &str,
        fact: &ToolFact,
        view: &mut ToolView,
    ) -> Vec<Fact> {
        let mut out = observe_classifier_denial(self.id(), ctx, raw_id, fact);

        let parent_tool_use_id = fact
            .raw
            .pointer("/_meta/claudeCode/parentToolUseId")
            .and_then(Value::as_str);
        view.parent = ctx
            .state
            .resolve_parent(ctx.root_session_id, session_id, parent_tool_use_id);

        // Child tool calls belong inside their parent subtask, not as new top-level subtasks.
        if view.parent.is_some() {
            return out;
        }

        out.extend(observe_root_subtask_tool(ctx, raw_id, fact));
        out
    }
}

/// Parse a `permission_check_status` object (from either ACP `session/update` or Claude Code's
/// SDK `{"type":"system","subtype":"permission_check_status",...}` message).
pub fn parse_permission_check_status(v: &Value) -> Option<PermissionCheckEvent> {
    let phase = match v.get("status").and_then(Value::as_str)? {
        "checking" => PermissionCheckPhase::Checking,
        "done" => PermissionCheckPhase::Done,
        _ => return None,
    };
    let tool_use_id = v
        .get("tool_use_id")
        .or_else(|| v.get("toolUseId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let agent_id = v
        .get("agent_id")
        .or_else(|| v.get("agentId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Some(PermissionCheckEvent {
        phase,
        tool_use_id,
        agent_id,
    })
}

/// Parse a `claude-agent-acp` stderr line carrying an unhandled Claude Code SDK
/// `system/permission_check_status` frame (`Unexpected case: {"type":"system","subtype":"permission_check_status",...}`).
pub fn parse_stderr_permission_check(line: &str) -> Option<PermissionCheckEvent> {
    let trimmed = line.trim();
    let payload = trimmed
        .strip_prefix("Unexpected case:")
        .map(str::trim)
        .unwrap_or(trimmed);
    if !payload.starts_with('{') || !payload.contains("permission_check_status") {
        return None;
    }
    let v: Value = serde_json::from_str(payload).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("system")
        || v.get("subtype").and_then(Value::as_str) != Some("permission_check_status")
    {
        return None;
    }
    parse_permission_check_status(&v)
}

fn record_permission_check_metric(
    harness: &'static str,
    phase: PermissionCheckPhase,
    source: &'static str,
) {
    metrics::counter!(
        "steve_acp_permission_check_status_total",
        "harness" => harness,
        "status" => phase.as_str(),
        "source" => source,
    )
    .increment(1);
}

fn tool_requires_classifier(category: ToolCategory) -> bool {
    matches!(
        category,
        ToolCategory::Exec
            | ToolCategory::Edit
            | ToolCategory::Delete
            | ToolCategory::Move
            | ToolCategory::Fetch
            | ToolCategory::Other
    )
}

/// Detect whether a failed tool call was denied because the auto-mode safety classifier was
/// unavailable or halted auto mode (as opposed to a normal safety denial or tool execution error).
pub fn classify_tool_denial(fact: &ToolFact) -> Option<ClassifierDenialKind> {
    if fact.phase != ToolPhase::Failed {
        return None;
    }
    if let Some(resp) = fact.raw.pointer("/_meta/claudeCode/toolResponse") {
        if let Some(host_code) = resp.get("hostCode").and_then(Value::as_str)
            && (host_code == "auto_mode_no_check_in_session"
                || host_code.starts_with("auto_mode_stop_"))
        {
            return Some(ClassifierDenialKind::Stopped);
        }
        let reason_type = resp.get("decisionReasonType").and_then(Value::as_str);
        let reason = resp
            .get("decisionReason")
            .and_then(Value::as_str)
            .unwrap_or("");
        let message = resp.get("message").and_then(Value::as_str).unwrap_or("");
        if let Some(kind) = classify_denial_text(reason, reason_type == Some("classifier"))
            .or_else(|| classify_denial_text(message, reason_type == Some("classifier")))
        {
            return Some(kind);
        }
    }
    if let Some(err) = fact.error.as_deref()
        && let Some(kind) = classify_denial_text(err, false)
    {
        return Some(kind);
    }
    if let Some(out) = tool_text_output(fact)
        && let Some(kind) = classify_denial_text(&out, false)
    {
        return Some(kind);
    }
    None
}

fn classify_denial_text(text: &str, from_classifier_meta: bool) -> Option<ClassifierDenialKind> {
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("auto mode unavailable")
        && (from_classifier_meta
            || lower.contains("no safety verdict")
            || lower.contains("repeated responses")
            || lower.contains("classifier"))
    {
        return Some(ClassifierDenialKind::Stopped);
    }
    if lower.contains("classifier unavailable")
        && (from_classifier_meta || lower.contains("permission denied"))
    {
        return Some(ClassifierDenialKind::Unavailable);
    }
    if from_classifier_meta
        && (lower.contains("gave no verdict") || lower.contains("no safety verdict"))
    {
        return Some(ClassifierDenialKind::Unavailable);
    }
    None
}

fn observe_classifier_denial(
    harness: &'static str,
    ctx: &mut AdapterContext<'_>,
    raw_id: &str,
    fact: &ToolFact,
) -> Vec<Fact> {
    if !matches!(fact.phase, ToolPhase::Succeeded | ToolPhase::Failed) {
        return vec![];
    }
    if !ctx
        .state
        .classifier_evaluated_tools
        .insert(raw_id.to_string())
    {
        return vec![];
    }
    if fact.phase == ToolPhase::Succeeded {
        if tool_requires_classifier(fact.category) {
            ctx.state.classifier_unavailable_streak = 0;
            ctx.state.classifier_warned = false;
        }
        return vec![];
    }
    let Some(kind) = classify_tool_denial(fact) else {
        // A real classifier safety verdict proves the classifier itself responded.
        if fact
            .raw
            .pointer("/_meta/claudeCode/toolResponse/decisionReasonType")
            .and_then(Value::as_str)
            == Some("classifier")
        {
            ctx.state.classifier_unavailable_streak = 0;
            ctx.state.classifier_warned = false;
        }
        return vec![];
    };
    metrics::counter!(
        "steve_acp_classifier_unavailable_total",
        "harness" => harness,
        "kind" => kind.as_str(),
    )
    .increment(1);
    ctx.state.classifier_unavailable_streak =
        ctx.state.classifier_unavailable_streak.saturating_add(1);
    let streak = ctx.state.classifier_unavailable_streak;
    match kind {
        ClassifierDenialKind::Stopped if !ctx.state.classifier_stopped => {
            ctx.state.classifier_stopped = true;
            ctx.state.classifier_warned = true;
            tracing::warn!(
                harness = harness,
                consecutive_denials = streak,
                "acp adapter: auto mode stopped after repeated no-verdict classifier responses"
            );
            vec![Fact::Event(EventBody::Diagnostic {
                severity: Severity::Error,
                code: AUTO_MODE_CLASSIFIER_UNAVAILABLE.into(),
                safe_text: format!(
                    "Auto mode stopped after repeated responses with no safety classifier verdict ({streak} denied)."
                ),
                details: None,
            })]
        }
        ClassifierDenialKind::Unavailable
            if streak >= CLASSIFIER_UNAVAILABLE_WARN_THRESHOLD && !ctx.state.classifier_warned =>
        {
            ctx.state.classifier_warned = true;
            tracing::warn!(
                harness = harness,
                consecutive_denials = streak,
                "acp adapter: auto mode classifier repeatedly returned no safety verdict"
            );
            vec![Fact::Event(EventBody::Diagnostic {
                severity: Severity::Warning,
                code: AUTO_MODE_CLASSIFIER_UNAVAILABLE.into(),
                safe_text: format!(
                    "Auto mode safety classifier returned no verdict for {streak} consecutive tool calls."
                ),
                details: None,
            })]
        }
        _ => vec![],
    }
}

fn observe_root_subtask_tool(
    ctx: &mut AdapterContext<'_>,
    raw_id: &str,
    fact: &ToolFact,
) -> Vec<Fact> {
    let input = fact.input();
    let tool_resp = fact.raw.pointer("/_meta/claudeCode/toolResponse");
    let name = fact.name.as_str();

    // 1) Fallback Agent/Task tool (when ACP RFD subagents was not negotiated or legacy fallback)
    if matches!(name, "Agent" | "Task") {
        let agent_id = tool_resp
            .and_then(|r| r.get("agentId"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        // If RFD already tracks this child session id, avoid duplicating it as a tool subtask.
        if let Some(aid) = agent_id
            && ctx.state.has_rfd_subagent(aid)
        {
            return vec![];
        }
        let key = format!("tool:{raw_id}");
        let default_title = input
            .get("description")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if !fact.title.is_empty() && fact.title != "Task" {
                    fact.title.as_str()
                } else {
                    "Subagent"
                }
            });
        let is_async_launched = tool_resp
            .and_then(|r| r.get("isAsync"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && tool_resp
                .and_then(|r| r.get("status"))
                .and_then(Value::as_str)
                == Some("async_launched");
        let output_file = tool_resp
            .and_then(|r| r.get("outputFile"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let subtask = ctx.state.upsert(
            &key,
            fact.call.clone(),
            SubtaskOrigin::Subagent,
            default_title,
            |tracked| {
                let v = &mut tracked.view;
                if let Some(desc) = input
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    v.title = bound(desc, SUBTASK_TITLE_MAX);
                }
                if let Some(actor) = input
                    .get("subagent_type")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    v.actor = Some(bound(actor, SUBTASK_TITLE_MAX));
                }
                if let Some(model) = tool_resp
                    .and_then(|r| r.get("resolvedModel"))
                    .or_else(|| input.get("model"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    v.model = Some(model.to_string());
                }
                if let Some(aid) = agent_id {
                    v.external_id = Some(aid.to_string());
                }
                if let Some(prompt) = input
                    .get("prompt")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    && v.progress.is_none()
                {
                    v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                }
                v.status = match fact.phase {
                    ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
                    ToolPhase::Succeeded if is_async_launched => SubtaskStatus::Running,
                    ToolPhase::Succeeded => {
                        if let Some(out) = tool_text_output(fact)
                            && !out.trim().starts_with("Async agent launched successfully")
                        {
                            v.summary = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                        }
                        SubtaskStatus::Done
                    }
                    ToolPhase::Failed => {
                        v.error = fact
                            .error
                            .as_deref()
                            .or_else(|| tool_text_output(fact).as_deref().map(|_| ""))
                            .filter(|s| !s.is_empty())
                            .map(|e| bound(e, SUBTASK_TEXT_MAX))
                            .or_else(|| {
                                tool_text_output(fact).map(|s| bound(&s, SUBTASK_TEXT_MAX))
                            });
                        SubtaskStatus::Failed
                    }
                };
            },
        );
        if let Some(path) = output_file {
            ctx.state.register_alias(path, &key);
        }
        return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
    }

    // 2) Background Bash / Monitor
    let bg_id = tool_resp
        .and_then(|r| r.get("backgroundTaskId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| extract_bg_id_from_output(fact));
    let is_bg_bash = name == "Bash"
        && (input
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || bg_id.is_some());
    if is_bg_bash || name == "Monitor" {
        let key = format!("bg:{raw_id}");
        let title = input
            .get("description")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| input.get("command").and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if !fact.title.is_empty() {
                    fact.title.as_str()
                } else {
                    name
                }
            });
        let output_path = extract_output_path_from_output(fact);
        let subtask = ctx.state.upsert(
            &key,
            call_id(&key),
            SubtaskOrigin::BackgroundTask,
            title,
            |tracked| {
                let v = &mut tracked.view;
                v.actor = Some(name.to_string());
                if !title.is_empty() && title != "Terminal" {
                    v.title = bound(title, SUBTASK_TITLE_MAX);
                }
                if let Some(cmd) = input
                    .get("command")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    v.progress = Some(bound(cmd, SUBTASK_TEXT_MAX));
                }
                if let Some(id) = &bg_id {
                    v.external_id = Some(id.clone());
                }
                v.status = match fact.phase {
                    ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
                    // Launching a background Bash succeeds immediately while the background
                    // command keeps running until its output reports exit or TaskStop runs.
                    ToolPhase::Succeeded if is_bg_bash => SubtaskStatus::Running,
                    ToolPhase::Succeeded => {
                        if let Some(out) = tool_text_output(fact) {
                            v.summary = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                        }
                        SubtaskStatus::Done
                    }
                    ToolPhase::Failed => {
                        v.error = fact.error.as_deref().map(|e| bound(e, SUBTASK_TEXT_MAX));
                        SubtaskStatus::Failed
                    }
                };
            },
        );
        if let Some(path) = output_path {
            ctx.state.register_alias(path, &key);
        }
        return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
    }

    // 3) Read of a tracked background task's output file settles its status when exit code is seen.
    if name == "Read" && fact.phase == ToolPhase::Succeeded {
        let file_path = input
            .get("file_path")
            .or_else(|| tool_resp.and_then(|r| r.pointer("/file/filePath")))
            .and_then(Value::as_str);
        if let Some(path) = file_path
            && let Some(key) = ctx.state.lookup_key_by_external(path).or_else(|| {
                extract_task_id_from_path(path).and_then(|id| ctx.state.lookup_key_by_external(id))
            })
        {
            let content = tool_resp
                .and_then(|r| r.pointer("/file/content"))
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| tool_text_output(fact));
            if let Some(text) = content
                && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
            {
                let (status, summary) = parse_bg_output(&text);
                if let Some(st) = status {
                    tracked.view.status = st;
                    if tracked.view.duration_ms.is_none() {
                        tracked.view.duration_ms =
                            Some(tracked.started_at.elapsed().as_millis() as u64);
                    }
                }
                if !summary.is_empty() {
                    if tracked.view.status == SubtaskStatus::Failed {
                        tracked.view.error = Some(bound(&summary, SUBTASK_TEXT_MAX));
                    } else {
                        tracked.view.summary = Some(bound(&summary, SUBTASK_TEXT_MAX));
                    }
                }
                return vec![Fact::Event(EventBody::SubtaskChanged {
                    subtask: tracked.view.clone(),
                })];
            }
        }
    }

    // 4) TaskStop settles a tracked background task to Cancelled.
    if name == "TaskStop" && fact.phase == ToolPhase::Succeeded {
        let target = input
            .get("task_id")
            .or_else(|| input.get("shell_id"))
            .or_else(|| input.get("id"))
            .and_then(Value::as_str);
        if let Some(tid) = target
            && let Some(key) = ctx.state.lookup_key_by_external(tid)
            && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
        {
            tracked.view.status = SubtaskStatus::Cancelled;
            tracked.view.can_cancel = false;
            if tracked.view.duration_ms.is_none() {
                tracked.view.duration_ms = Some(tracked.started_at.elapsed().as_millis() as u64);
            }
            return vec![Fact::Event(EventBody::SubtaskChanged {
                subtask: tracked.view.clone(),
            })];
        }
    }

    vec![]
}

fn extract_bg_id_from_output(fact: &ToolFact) -> Option<String> {
    let text = tool_text_output(fact)?;
    let rest = text.split("background with ID:").nth(1)?;
    let id = rest
        .trim()
        .split(|c: char| c.is_whitespace() || c == '.')
        .next()?
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

fn extract_output_path_from_output(fact: &ToolFact) -> Option<String> {
    let text = tool_text_output(fact)?;
    let rest = text.split("Output is being written to:").nth(1)?;
    let path = rest.trim().split_whitespace().next()?.trim_end_matches('.');
    (!path.is_empty()).then(|| path.to_string())
}

fn extract_task_id_from_path(path: &str) -> Option<&str> {
    let file = path.rsplit('/').next()?;
    file.strip_suffix(".output")
}

fn strip_read_line_number(line: &str) -> &str {
    let trimmed = line.trim_start();
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &trimmed[digits..];
        if let Some(after) = rest.strip_prefix('\t').or_else(|| rest.strip_prefix('→')) {
            return after.trim();
        }
    }
    trimmed.trim()
}

fn parse_bg_output(text: &str) -> (Option<SubtaskStatus>, String) {
    let mut status = None;
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = strip_read_line_number(line);
        if let Some(rest) = trimmed
            .strip_prefix("[exited with code ")
            .and_then(|s| s.strip_suffix(']'))
        {
            status = Some(if rest.trim() == "0" {
                SubtaskStatus::Done
            } else {
                SubtaskStatus::Failed
            });
            continue;
        }
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }
    (status, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::Normalizer;
    use steve_node_api::Epoch;

    fn norm() -> Normalizer {
        Normalizer::new("/ws", "att-1", Epoch::new("m1").unwrap())
            .for_harness("claude-code")
            .root_session("root-1")
    }

    fn failed_tool_update(id: &str, name: &str, kind: &str, text: &str, resp: Value) -> Value {
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "kind": kind,
            "title": name,
            "status": "failed",
            "content": [{"type": "content", "content": {"type": "text", "text": text}}],
            "_meta": {
                "claudeCode": {
                    "toolName": name,
                    "toolResponse": resp,
                }
            }
        })
    }

    fn succeeded_tool_update(id: &str, name: &str, kind: &str) -> Value {
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "kind": kind,
            "title": name,
            "status": "completed",
            "_meta": { "claudeCode": { "toolName": name } }
        })
    }

    #[test]
    fn permission_check_status_parses_acp_and_stderr_without_unsupported_update() {
        let mut n = norm();
        for (sid, status) in [
            (Some("root-1"), "checking"),
            (Some("root-1"), "done"),
            (Some("child-1"), "checking"),
        ] {
            let facts = n.update_for_session(
                sid,
                &json!({
                    "sessionUpdate": "permission_check_status",
                    "tool_use_id": "toolu_123",
                    "agent_id": "agent_1",
                    "status": status,
                }),
            );
            assert_eq!(facts, vec![Fact::Nothing], "status={status}");
        }

        let stderr_checking = r#"Unexpected case: {"type":"system","subtype":"permission_check_status","tool_use_id":"toolu_abc","agent_id":"agent_xyz","status":"checking","uuid":"u1","session_id":"s1"}"#;
        let parsed = parse_stderr_permission_check(stderr_checking).expect("parsed checking");
        assert_eq!(parsed.phase, PermissionCheckPhase::Checking);
        assert_eq!(parsed.tool_use_id.as_deref(), Some("toolu_abc"));
        assert_eq!(parsed.agent_id.as_deref(), Some("agent_xyz"));
        assert!(ClaudeCodeAdapter.observe_stderr("[node s1]", stderr_checking));

        let stderr_done = r#"Unexpected case: {"type":"system","subtype":"permission_check_status","tool_use_id":"toolu_abc","status":"done"}"#;
        let parsed_done = parse_stderr_permission_check(stderr_done).expect("parsed done");
        assert_eq!(parsed_done.phase, PermissionCheckPhase::Done);
        assert!(ClaudeCodeAdapter.observe_stderr("[node s1]", stderr_done));

        // Unrelated stderr lines are left for normal logging
        assert!(!ClaudeCodeAdapter.observe_stderr("[node s1]", "plain info line"));
        assert!(!ClaudeCodeAdapter.observe_stderr(
            "[node s1]",
            r#"Unexpected case: {"type":"system","subtype":"other_event"}"#
        ));
    }

    #[test]
    fn consecutive_classifier_unavailable_denials_warn_and_escalate_on_stop() {
        let mut n = norm();
        let unavail_meta = || {
            json!({
                "decisionReasonType": "classifier",
                "decisionReason": "Classifier unavailable",
                "message": "The server-side auto mode classifier gave no verdict"
            })
        };

        // Denial #1
        let f1 = n.update(&failed_tool_update(
            "t1",
            "Bash",
            "execute",
            "Permission denied: Classifier unavailable",
            unavail_meta(),
        ));
        assert!(
            !f1.iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. }))),
            "single failure should not warn yet"
        );

        // Duplicate terminal update for t1 must not double-count
        let f1_dup = n.update(&failed_tool_update(
            "t1",
            "Bash",
            "execute",
            "Permission denied: Classifier unavailable",
            unavail_meta(),
        ));
        assert!(
            !f1_dup
                .iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. })))
        );

        // Read-only tool succeeding between denials must NOT reset the classifier streak
        let _ = n.update(&succeeded_tool_update("t_read", "Read", "read"));

        // Denial #2
        let f2 = n.update(&failed_tool_update(
            "t2",
            "Edit",
            "edit",
            "Permission denied: Classifier unavailable",
            unavail_meta(),
        ));
        assert!(
            !f2.iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. })))
        );

        // Denial #3 crosses CLASSIFIER_UNAVAILABLE_WARN_THRESHOLD -> Warning diagnostic
        let f3 = n.update(&failed_tool_update(
            "t3",
            "Bash",
            "execute",
            "Permission denied: Classifier unavailable",
            unavail_meta(),
        ));
        assert!(
            f3.iter().any(|f| matches!(
                f,
                Fact::Event(EventBody::Diagnostic {
                    severity: Severity::Warning,
                    code,
                    ..
                }) if code == AUTO_MODE_CLASSIFIER_UNAVAILABLE
            )),
            "expected warning diagnostic on 3rd denial: {f3:?}"
        );

        // Denial #4 does not spam duplicate warnings
        let f4 = n.update(&failed_tool_update(
            "t4",
            "Bash",
            "execute",
            "Permission denied: Classifier unavailable",
            unavail_meta(),
        ));
        assert!(
            !f4.iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. }))),
            "must not spam duplicate warnings"
        );

        // Turn-stopping Auto mode unavailable escalates once to Severity::Error
        let f_stop = n.update(&failed_tool_update(
            "t5",
            "Bash",
            "execute",
            "Permission denied: Auto mode unavailable — stopped after repeated responses with no safety verdict",
            json!({
                "decisionReasonType": "classifier",
                "decisionReason": "Auto mode unavailable — stopped after repeated responses with no safety verdict",
                "hostCode": "auto_mode_stop_no_verdict"
            }),
        ));
        assert!(
            f_stop.iter().any(|f| matches!(
                f,
                Fact::Event(EventBody::Diagnostic {
                    severity: Severity::Error,
                    code,
                    ..
                }) if code == AUTO_MODE_CLASSIFIER_UNAVAILABLE
            )),
            "expected error diagnostic on auto mode stop: {f_stop:?}"
        );
    }

    #[test]
    fn real_safety_denial_and_mutating_success_reset_unavailable_streak() {
        let mut n = norm();
        let unavail_meta = json!({
            "decisionReasonType": "classifier",
            "decisionReason": "Classifier unavailable",
        });

        for id in ["t1", "t2"] {
            let _ = n.update(&failed_tool_update(
                id,
                "Bash",
                "execute",
                "Permission denied: Classifier unavailable",
                unavail_meta.clone(),
            ));
        }
        // A real classifier safety verdict proves the classifier works and resets the streak
        let real_deny = n.update(&failed_tool_update(
            "t_real",
            "Bash",
            "execute",
            "Permission denied: Command would overwrite protected branch",
            json!({
                "decisionReasonType": "classifier",
                "decisionReason": "Command would overwrite protected branch",
            }),
        ));
        assert!(
            !real_deny
                .iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. })))
        );

        // Two more unavailable denials followed by a succeeded Bash reset before reaching threshold
        for id in ["t3", "t4"] {
            let f = n.update(&failed_tool_update(
                id,
                "Bash",
                "execute",
                "Permission denied: Classifier unavailable",
                unavail_meta.clone(),
            ));
            assert!(
                !f.iter()
                    .any(|x| matches!(x, Fact::Event(EventBody::Diagnostic { .. })))
            );
        }
        let _ = n.update(&succeeded_tool_update("t_ok", "Bash", "execute"));
        let f_after_ok = n.update(&failed_tool_update(
            "t5",
            "Bash",
            "execute",
            "Permission denied: Classifier unavailable",
            unavail_meta,
        ));
        assert!(
            !f_after_ok
                .iter()
                .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { .. })))
        );
    }

    #[test]
    fn child_subagent_classifier_stop_is_observed_immediately() {
        let mut n = norm();
        let facts = n.update_for_session(
            Some("root-1"),
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "child-t1",
                "kind": "execute",
                "title": "Bash",
                "status": "failed",
                "content": [{"type": "content", "content": {"type": "text", "text": "Permission denied: Auto mode unavailable — stopped after repeated responses with no safety verdict"}}],
                "_meta": {
                    "claudeCode": {
                        "toolName": "Bash",
                        "parentToolUseId": "parent-subagent-1",
                        "toolResponse": {
                            "hostCode": "auto_mode_no_check_in_session"
                        }
                    }
                }
            }),
        );
        assert!(
            facts.iter().any(|f| matches!(
                f,
                Fact::Event(EventBody::Diagnostic {
                    severity: Severity::Error,
                    code,
                    ..
                }) if code == AUTO_MODE_CLASSIFIER_UNAVAILABLE
            )),
            "expected immediate error diagnostic on hostCode stop: {facts:?}"
        );

// ── Local transcript history & resume lookup ──

use std::path::{Path, PathBuf};

use crate::adapter::history::{
    HistoryAdapter, HistorySession, agent_chunk, first_lines, is_steve, js_string, mtime_ms,
    read_dir_sorted, real_user_text, slice, str_field, truthy,
};

/// Claude Code's project directory name for a working directory.
pub fn claude_project(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

impl HistoryAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn key_prefix(&self) -> &'static str {
        "cc"
    }

    fn default_state_rel(&self) -> &'static str {
        ".claude"
    }

    fn list_files(&self, user_home: &Path) -> Vec<PathBuf> {
        let root = user_home.join(".claude/projects");
        let Ok(dirs) = read_dir_sorted(&root) else {
            return vec![];
        };
        dirs.into_iter()
            .flat_map(|d| {
                let dir = root.join(&d);
                read_dir_sorted(&dir)
                    .map(|fs| {
                        fs.into_iter()
                            .filter(|f| f.ends_with(".jsonl"))
                            .map(|f| dir.join(f))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    fn peek(&self, steve_home: &Path, file: &Path) -> Option<HistorySession> {
        let (mut title, mut cwd) = (String::new(), String::new());
        for l in first_lines(file, 200_000).ok()? {
            if l.is_empty() {
                continue;
            }
            let Ok(m) = serde_json::from_str::<Value>(&l) else {
                continue;
            };
            if cwd.is_empty() && truthy(m.get("cwd")) {
                cwd = js_string(&m["cwd"]);
            }
            if m.get("type").and_then(Value::as_str) == Some("user")
                && let Some(Value::String(c)) = str_field(&m, &["message", "content"])
                && real_user_text(c)
            {
                title = c.clone();
                break;
            }
        }
        if title.is_empty() || is_steve(steve_home, &cwd) {
            return None;
        }
        Some(HistorySession {
            key: format!("{}:{}", self.key_prefix(), file.display()),
            harness_id: HistoryAdapter::id(self),
            harness: self.display_name().into(),
            title: slice(&title, 80),
            cwd,
            when: mtime_ms(file)?,
            size: std::fs::metadata(file).ok()?.len(),
        })
    }

    fn parse_rows(&self, text: &str) -> Vec<(String, Value)> {
        let read = |n: &str| ["Read", "Grep", "Glob"].iter().any(|x| n.contains(x));
        let edit = |n: &str| ["Edit", "Write"].iter().any(|x| n.contains(x));
        let mut out = vec![];
        for l in text.split('\n') {
            if l.is_empty() {
                continue;
            }
            let Ok(m) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            let ty = m.get("type").and_then(Value::as_str);
            if ty == Some("user")
                && let Some(Value::String(c)) = str_field(&m, &["message", "content"])
                && real_user_text(c)
            {
                out.push(("user".into(), json!({ "text": c, "via": "me" })));
            }
            if ty == Some("assistant")
                && let Some(Value::Array(cs)) = str_field(&m, &["message", "content"])
            {
                for c in cs {
                    let cty = c.get("type").and_then(Value::as_str);
                    if cty == Some("text") && truthy(c.get("text")) {
                        out.push(agent_chunk(&js_string(&c["text"])));
                    }
                    if cty == Some("tool_use") {
                        let name = c
                            .get("name")
                            .map(js_string)
                            .unwrap_or_else(|| "undefined".into());
                        let input = c.get("input");
                        let extra = if truthy(input.and_then(|i| i.get("command"))) {
                            format!(
                                " {}",
                                slice(
                                    &js_string(
                                        &input.map(|i| i["command"].clone()).unwrap_or_default()
                                    ),
                                    120
                                )
                            )
                        } else if truthy(input.and_then(|i| i.get("file_path"))) {
                            format!(
                                " {}",
                                js_string(
                                    &input.map(|i| i["file_path"].clone()).unwrap_or_default()
                                )
                            )
                        } else {
                            String::new()
                        };
                        let kind = if read(&name) {
                            "read"
                        } else if edit(&name) {
                            "edit"
                        } else {
                            "execute"
                        };
                        let mut call = serde_json::Map::new();
                        call.insert("sessionUpdate".into(), json!("tool_call"));
                        if let Some(id) = c.get("id") {
                            call.insert("toolCallId".into(), id.clone());
                        }
                        call.insert("title".into(), json!(format!("{name}{extra}")));
                        call.insert("kind".into(), json!(kind));
                        call.insert("status".into(), json!("completed"));
                        out.push(("tool_call".into(), Value::Object(call)));
                    }
                }
            }
        }
        out
    }

    fn find_transcript(&self, state_dir: &Path, id: &str, cwd: &str) -> Option<PathBuf> {
        let projects = state_dir.join("projects");
        let name = format!("{id}.jsonl");
        let direct = projects.join(claude_project(cwd)).join(&name);
        if direct.is_file() {
            return Some(direct);
        }
        std::fs::read_dir(&projects)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path().join(&name))
            .find(|p| p.is_file())
    }

    fn recorded(&self, file: &Path) -> (Option<String>, Option<String>) {
        use std::io::{BufRead, BufReader, Read};
        let Ok(f) = std::fs::File::open(file) else {
            return (None, None);
        };
        let (mut id, mut cwd) = (None, None);
        let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        for line in BufReader::new(f.take(8 << 20)).lines().take(50) {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            id = id.or_else(|| field(&v, "sessionId"));
            cwd = cwd.or_else(|| field(&v, "cwd"));
            if id.is_some() && cwd.is_some() {
                break;
            }
        }
        (id, cwd)
    }

    fn session_id_from_file(&self, file: &Path) -> String {
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    }
}

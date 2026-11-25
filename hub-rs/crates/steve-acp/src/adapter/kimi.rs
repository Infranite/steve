//! Kimi Code (`kimi acp`) runtime adapter.
//!
//! Verified empirically against `kimi` 2.4.1 (`kimi acp`):
//! - Does not advertise ACP RFD subagents.
//! - Omits `name` and `_meta.claudeCode.toolName` on `tool_call` frames: initial `tool_call`
//!   carries the canonical tool name in `title` (`"Agent"`, `"Bash"`, `"Read"`, `"TaskStop"`),
//!   and subsequent `tool_call_update` frames overwrite `title` with human descriptions
//!   (`"Launching explore agent: ..."`, `"Starting background: ..."`).
//! - Subagents: `Agent` tool (`rawInput: {subagent_type, description, prompt}` -> structured
//!   `rawOutput` with `agent_id:`, `actual_subagent_type:`, `status:`, `[summary]`).
//! - Background tasks: `Bash` with `rawInput.run_in_background == true` -> structured `rawOutput`
//!   with `task_id:`, `pid:`, `status: running`; output read from `.../tasks/<task_id>/output.log`;
//!   cancelled via `TaskStop`.
//! - Usage: `usage_update` context meter (`used`/`size`) only; no per-turn or per-subagent tokens.

use serde_json::Value;
use steve_node_api::{
    AdapterCapabilities, BackgroundTaskCapability, EventBody, ResumeCapability, SubagentCapability,
    SubtaskOrigin, SubtaskStatus, ToolPhase, ToolView, UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, HarnessAdapter, SUBTASK_TEXT_MAX, SUBTASK_TITLE_MAX,
    tool_text_output,
};
use crate::normalize::{Fact, ToolFact, bound, call_id};

pub struct KimiAdapter;

impl HarnessAdapter for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi-code"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::ToolFallback,
            subagent_cancel: false,
            background_tasks: BackgroundTaskCapability::ToolFallback,
            background_task_cancel: false,
            child_tool_nesting: false,
            usage_attribution: UsageAttribution::ContextOnly,
            resume: ResumeCapability::SessionLoad,
            upgrade: steve_node_api::UpgradeCapability::ExternalManaged,
            effort_mode: steve_node_api::EffortMode::AcpOption,
            verified: true,
        }
    }

    fn infer_tool_name(&self, _raw: &Value, initial_title: Option<&str>) -> Option<String> {
        let title = initial_title?.trim();
        // Kimi's initial tool_call title is the single-token tool identifier (e.g. "Agent",
        // "Bash", "Read", "TaskStop") before tool_call_update replaces it with a sentence.
        let is_ident = !title.is_empty()
            && title.len() <= 40
            && title
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        is_ident.then(|| title.to_string())
    }

    fn normalize_update(
        &self,
        _ctx: &mut AdapterContext<'_>,
        _session_id: Option<&str>,
        _update: &Value,
    ) -> AdapterOutcome {
        AdapterOutcome::PassThrough
    }

    fn observe_tool(
        &self,
        ctx: &mut AdapterContext<'_>,
        _session_id: Option<&str>,
        raw_id: &str,
        fact: &ToolFact,
        _view: &mut ToolView,
    ) -> Vec<Fact> {
        let input = fact.input();
        let name = fact.name.as_str();

        // 1) Kimi `Agent` subagent tool
        if name == "Agent"
            || (input.get("subagent_type").is_some() && input.get("prompt").is_some())
        {
            let key = format!("tool:{raw_id}");
            let title = input
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if !fact.title.is_empty() && fact.title != "Agent" {
                        fact.title.as_str()
                    } else {
                        "Subagent"
                    }
                });
            let parsed = tool_text_output(fact).map(|t| parse_kimi_agent_output(&t));
            let subtask = ctx.state.upsert(
                &key,
                fact.call.clone(),
                SubtaskOrigin::Subagent,
                title,
                |tracked| {
                    let v = &mut tracked.view;
                    if title != "Subagent" {
                        v.title = bound(title, SUBTASK_TITLE_MAX);
                    }
                    if let Some(actor) = parsed
                        .as_ref()
                        .and_then(|p| p.subagent_type.as_deref())
                        .or_else(|| input.get("subagent_type").and_then(Value::as_str))
                        .filter(|s| !s.is_empty())
                    {
                        v.actor = Some(bound(actor, SUBTASK_TITLE_MAX));
                    }
                    if let Some(aid) = parsed.as_ref().and_then(|p| p.agent_id.as_deref()) {
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
                        ToolPhase::Failed => {
                            v.error = fact.error.as_deref().map(|e| bound(e, SUBTASK_TEXT_MAX));
                            SubtaskStatus::Failed
                        }
                        ToolPhase::Succeeded => {
                            if let Some(p) = &parsed {
                                if let Some(sum) = &p.summary {
                                    v.summary = Some(bound(sum, SUBTASK_TEXT_MAX));
                                }
                                match p.status.as_deref() {
                                    Some("failed" | "error") => SubtaskStatus::Failed,
                                    Some("cancelled" | "canceled") => SubtaskStatus::Cancelled,
                                    _ => SubtaskStatus::Done,
                                }
                            } else {
                                SubtaskStatus::Done
                            }
                        }
                    };
                },
            );
            return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
        }

        // 2) Kimi background `Bash`
        let is_bg_bash = (name == "Bash" || fact.title.starts_with("Starting background:"))
            && input
                .get("run_in_background")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if is_bg_bash {
            let key = format!("bg:{raw_id}");
            let title = input
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| input.get("command").and_then(Value::as_str))
                .unwrap_or("Background command");
            let parsed = tool_text_output(fact).map(|t| parse_kimi_kv(&t));
            let task_id = parsed
                .as_ref()
                .and_then(|m| m.get("task_id").cloned())
                .filter(|s| !s.is_empty());
            let subtask = ctx.state.upsert(
                &key,
                call_id(&key),
                SubtaskOrigin::BackgroundTask,
                title,
                |tracked| {
                    let v = &mut tracked.view;
                    v.actor = Some("Bash".into());
                    v.title = bound(title, SUBTASK_TITLE_MAX);
                    if let Some(cmd) = input
                        .get("command")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.progress = Some(bound(cmd, SUBTASK_TEXT_MAX));
                    }
                    if let Some(tid) = &task_id {
                        v.external_id = Some(tid.clone());
                    }
                    v.status = match fact.phase {
                        ToolPhase::Failed => {
                            v.error = fact.error.as_deref().map(|e| bound(e, SUBTASK_TEXT_MAX));
                            SubtaskStatus::Failed
                        }
                        ToolPhase::Succeeded => {
                            match parsed
                                .as_ref()
                                .and_then(|m| m.get("status"))
                                .map(String::as_str)
                            {
                                Some("completed" | "done") => SubtaskStatus::Done,
                                Some("failed" | "error") => SubtaskStatus::Failed,
                                Some("cancelled" | "canceled") => SubtaskStatus::Cancelled,
                                _ => SubtaskStatus::Running,
                            }
                        }
                        _ => SubtaskStatus::Running,
                    };
                },
            );
            return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
        }

        // 3) Read of `<session>/tasks/<task_id>/output.log` updates the matching background task.
        if name == "Read" && fact.phase == ToolPhase::Succeeded {
            let path = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(Value::as_str);
            if let Some(tid) = path.and_then(extract_kimi_task_id_from_path)
                && let Some(key) = ctx.state.lookup_key_by_external(tid)
                && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
                && let Some(out) = tool_text_output(fact)
            {
                let cleaned = strip_line_numbers(&out);
                if !cleaned.is_empty() {
                    tracked.view.summary = Some(bound(&cleaned, SUBTASK_TEXT_MAX));
                    tracked.view.status = SubtaskStatus::Done;
                    if tracked.view.duration_ms.is_none() {
                        tracked.view.duration_ms =
                            Some(tracked.started_at.elapsed().as_millis() as u64);
                    }
                    return vec![Fact::Event(EventBody::SubtaskChanged {
                        subtask: tracked.view.clone(),
                    })];
                }
            }
        }

        // 4) TaskStop marks a tracked background task Cancelled.
        if name == "TaskStop" && fact.phase == ToolPhase::Succeeded {
            let tid = input
                .get("task_id")
                .or_else(|| input.get("id"))
                .and_then(Value::as_str);
            if let Some(tid) = tid
                && let Some(key) = ctx.state.lookup_key_by_external(tid)
                && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
            {
                tracked.view.status = SubtaskStatus::Cancelled;
                if tracked.view.duration_ms.is_none() {
                    tracked.view.duration_ms =
                        Some(tracked.started_at.elapsed().as_millis() as u64);
                }
                return vec![Fact::Event(EventBody::SubtaskChanged {
                    subtask: tracked.view.clone(),
                })];
            }
        }

        vec![]
    }
}

struct KimiAgentOutput {
    agent_id: Option<String>,
    subagent_type: Option<String>,
    status: Option<String>,
    summary: Option<String>,
}

fn parse_kimi_agent_output(text: &str) -> KimiAgentOutput {
    let kv = parse_kimi_kv(text);
    let summary = text.split("[summary]").nth(1).map(|rest| {
        let body = rest.split("\nresume_hint:").next().unwrap_or(rest);
        body.trim().to_string()
    });
    KimiAgentOutput {
        agent_id: kv.get("agent_id").cloned(),
        subagent_type: kv.get("actual_subagent_type").cloned(),
        status: kv.get("status").cloned(),
        summary: summary.filter(|s| !s.is_empty()),
    }
}

fn parse_kimi_kv(text: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in text.lines() {
        if line.trim().starts_with('[') {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            if !k.is_empty() && !v.is_empty() && !map.contains_key(k) {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}

fn extract_kimi_task_id_from_path(path: &str) -> Option<&str> {
    let after = path.split("/tasks/").nth(1)?;
    let tid = after.split('/').next()?;
    (!tid.is_empty()).then_some(tid)
}

fn strip_line_numbers(text: &str) -> String {
    text.lines()
        .map(|l| match l.split_once('\t') {
            Some((num, rest)) if num.trim().chars().all(|c| c.is_ascii_digit()) => rest,
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

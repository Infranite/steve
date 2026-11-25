//! Antigravity (`agy acp`) runtime adapter.
//!
//! Verified empirically against `agy` 1.5.16 (`agy acp`):
//! - Does not advertise ACP RFD subagents; emits PascalCase root tool calls:
//!   - `invoke_subagent` (`TypeName`, `Role`, `Prompt`, `Model`, `Workspace`)
//!   - `manage_subagents` (`Action`, `ConversationIds`)
//!   - `run_command` (`CommandLine`, `WaitMsBeforeAsync`, `IsDaemon`, `toolSummary`)
//!   - `manage_task` (`Action: status|kill|send_input|list`, `TaskId`)
//!   - `schedule` (`DurationSeconds` / `CronExpression`, `Prompt`, `IsDaemon`)
//!   - `run_workflow` (`Description`, `ScriptPath`)
//!   - `swarm` (`action`, `name`, `brief`, `model`)
//! - Usage: `PromptResponse.usage` (`Main`) + `usage_update` (`used`/`size`). Per-subagent token
//!   attribution is not reported over ACP (remains `None`).

use serde_json::Value;
use steve_node_api::{
    AdapterCapabilities, BackgroundTaskCapability, EventBody, ResumeCapability, SubagentCapability,
    SubtaskOrigin, SubtaskStatus, ToolPhase, ToolView, UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, HarnessAdapter, SUBTASK_TEXT_MAX, SUBTASK_TITLE_MAX,
    handle_rfd_child_update, handle_rfd_session_message, handle_rfd_subagent_update,
    tool_text_output,
};
use crate::normalize::{Fact, ToolFact, bound, call_id};

/// Threshold below which an in-flight `run_command` with `WaitMsBeforeAsync` is intended to
/// background immediately (e.g. `WaitMsBeforeAsync: 500` in agy's tool contract).
const ASYNC_WAIT_HINT_MS: u64 = 1_000;

pub struct AntigravityAdapter;

impl HarnessAdapter for AntigravityAdapter {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::ToolFallback,
            subagent_cancel: false,
            background_tasks: BackgroundTaskCapability::ToolFallback,
            background_task_cancel: false,
            child_tool_nesting: false,
            usage_attribution: UsageAttribution::TurnLevel,
            resume: ResumeCapability::SessionLoad,
            upgrade: steve_node_api::UpgradeCapability::ExternalManaged,
            verified: true,
        }
    }

    fn normalize_update(
        &self,
        ctx: &mut AdapterContext<'_>,
        session_id: Option<&str>,
        update: &Value,
    ) -> AdapterOutcome {
        // Forward-compatible with ACP RFD if a future agy release adds native subagent sessions.
        if ctx.is_child_session(session_id) {
            return handle_rfd_child_update(ctx, session_id.unwrap_or_default(), update);
        }
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        match kind {
            "subagent_update" => match handle_rfd_subagent_update(ctx, session_id, update) {
                Some(facts) => AdapterOutcome::Handled(facts),
                None => AdapterOutcome::Handled(vec![Fact::Nothing]),
            },
            "session_message" => match handle_rfd_session_message(ctx, update) {
                Some(facts) => AdapterOutcome::Handled(facts),
                None => AdapterOutcome::Handled(vec![Fact::Nothing]),
            },
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
        view.parent = ctx
            .state
            .resolve_parent(ctx.root_session_id, session_id, None);
        if view.parent.is_some() {
            return vec![];
        }

        let input = fact.input();
        match fact.name.as_str() {
            "invoke_subagent" => self.observe_subagent(ctx, raw_id, fact, input),
            "manage_subagents" => self.observe_manage_subagents(ctx, fact, input),
            "run_command" => self.observe_run_command(ctx, raw_id, fact, input),
            "manage_task" => self.observe_manage_task(ctx, fact, input),
            "schedule" => self.observe_schedule(ctx, raw_id, fact, input),
            "run_workflow" => self.observe_workflow(ctx, raw_id, fact, input),
            "swarm" => self.observe_swarm(ctx, raw_id, fact, input),
            _ => vec![],
        }
    }
}

impl AntigravityAdapter {
    fn observe_subagent(
        &self,
        ctx: &mut AdapterContext<'_>,
        raw_id: &str,
        fact: &ToolFact,
        input: &Value,
    ) -> Vec<Fact> {
        let key = format!("tool:{raw_id}");
        let role = input
            .get("Role")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let type_name = input
            .get("TypeName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let title = role.or(type_name).unwrap_or("Subagent");
        let subtask = ctx.state.upsert(
            &key,
            fact.call.clone(),
            SubtaskOrigin::Subagent,
            title,
            |tracked| {
                let v = &mut tracked.view;
                v.title = bound(title, SUBTASK_TITLE_MAX);
                if let Some(tn) = type_name {
                    v.actor = Some(bound(tn, SUBTASK_TITLE_MAX));
                }
                if let Some(model) = input
                    .get("Model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && *s != "inherit")
                {
                    v.model = Some(model.to_string());
                }
                if let Some(prompt) = input
                    .get("Prompt")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                }
                v.status = match fact.phase {
                    ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
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
        vec![Fact::Event(EventBody::SubtaskChanged { subtask })]
    }

    fn observe_manage_subagents(
        &self,
        ctx: &mut AdapterContext<'_>,
        fact: &ToolFact,
        input: &Value,
    ) -> Vec<Fact> {
        if fact.phase != ToolPhase::Succeeded {
            return vec![];
        }
        let action = input.get("Action").and_then(Value::as_str).unwrap_or("");
        if action != "kill" && action != "kill_all" {
            return vec![];
        }
        let mut facts = Vec::new();
        if action == "kill"
            && let Some(ids) = input.get("ConversationIds").and_then(Value::as_array)
        {
            for cid in ids.iter().filter_map(Value::as_str) {
                if let Some(key) = ctx.state.lookup_key_by_external(cid)
                    && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
                {
                    tracked.view.status = SubtaskStatus::Cancelled;
                    facts.push(Fact::Event(EventBody::SubtaskChanged {
                        subtask: tracked.view.clone(),
                    }));
                }
            }
        }
        facts
    }

    fn observe_run_command(
        &self,
        ctx: &mut AdapterContext<'_>,
        raw_id: &str,
        fact: &ToolFact,
        input: &Value,
    ) -> Vec<Fact> {
        let is_daemon = input
            .get("IsDaemon")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let wait_ms = input.get("WaitMsBeforeAsync").and_then(Value::as_u64);
        let output = tool_text_output(fact);
        let output_indicates_bg = output.as_deref().is_some_and(|o| {
            o.contains("running in the background")
                || o.contains("background task")
                || o.contains("TaskId")
        });
        let key = format!("bg:{raw_id}");
        let already_tracked = ctx.state.subtasks.contains_key(&key);
        let is_async_candidate = is_daemon
            || output_indicates_bg
            || already_tracked
            || wait_ms.is_some_and(|w| w <= ASYNC_WAIT_HINT_MS);
        if !is_async_candidate {
            return vec![];
        }
        let summary_title = input
            .get("toolSummary")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| input.get("CommandLine").and_then(Value::as_str))
            .unwrap_or("Command");
        let exit_code = fact
            .raw
            .pointer("/rawOutput/exit_code")
            .and_then(Value::as_i64);
        let subtask = ctx.state.upsert(
            &key,
            call_id(&key),
            SubtaskOrigin::BackgroundTask,
            summary_title,
            |tracked| {
                let v = &mut tracked.view;
                v.actor = Some(if is_daemon { "daemon" } else { "run_command" }.into());
                v.title = bound(summary_title, SUBTASK_TITLE_MAX);
                if let Some(cmd) = input
                    .get("CommandLine")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    v.progress = Some(bound(cmd, SUBTASK_TEXT_MAX));
                }
                v.status = match fact.phase {
                    ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
                    ToolPhase::Succeeded if is_daemon || output_indicates_bg => {
                        SubtaskStatus::Running
                    }
                    ToolPhase::Succeeded if exit_code.is_some_and(|c| c != 0) => {
                        if let Some(out) = &output {
                            v.error = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                        }
                        SubtaskStatus::Failed
                    }
                    ToolPhase::Succeeded => {
                        if let Some(out) = &output {
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
        vec![Fact::Event(EventBody::SubtaskChanged { subtask })]
    }

    fn observe_manage_task(
        &self,
        ctx: &mut AdapterContext<'_>,
        fact: &ToolFact,
        input: &Value,
    ) -> Vec<Fact> {
        let Some(task_id) = input
            .get("TaskId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return vec![];
        };
        let action = input.get("Action").and_then(Value::as_str).unwrap_or("");
        let key = ctx
            .state
            .lookup_key_by_external(task_id)
            .unwrap_or_else(|| format!("task:{task_id}"));
        let short_name = task_id.rsplit('/').next().unwrap_or(task_id);
        let subtask = ctx.state.upsert(
            &key,
            call_id(&key),
            SubtaskOrigin::BackgroundTask,
            short_name,
            |tracked| {
                let v = &mut tracked.view;
                v.external_id = Some(task_id.to_string());
                if v.actor.is_none() {
                    v.actor = Some("task".into());
                }
                match action {
                    "kill" if fact.phase == ToolPhase::Succeeded => {
                        v.status = SubtaskStatus::Cancelled;
                    }
                    "status" => {
                        if let Some(out) = tool_text_output(fact) {
                            if out.contains("exited with code 0") || out.contains("completed") {
                                v.status = SubtaskStatus::Done;
                                v.summary = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                            } else if out.contains("exited with code") || out.contains("failed") {
                                v.status = SubtaskStatus::Failed;
                                v.error = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                            } else {
                                v.progress = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                            }
                        }
                    }
                    _ => {}
                }
            },
        );
        vec![Fact::Event(EventBody::SubtaskChanged { subtask })]
    }

    fn observe_schedule(
        &self,
        ctx: &mut AdapterContext<'_>,
        raw_id: &str,
        fact: &ToolFact,
        input: &Value,
    ) -> Vec<Fact> {
        let key = format!("schedule:{raw_id}");
        let label = input
            .get("CronExpression")
            .and_then(Value::as_str)
            .map(|c| format!("cron {c}"))
            .or_else(|| {
                input
                    .get("DurationSeconds")
                    .and_then(Value::as_u64)
                    .map(|s| format!("timer {s}s"))
            })
            .unwrap_or_else(|| "Schedule".into());
        let subtask = ctx.state.upsert(
            &key,
            call_id(&key),
            SubtaskOrigin::BackgroundTask,
            &label,
            |tracked| {
                let v = &mut tracked.view;
                v.actor = Some("schedule".into());
                if let Some(prompt) = input
                    .get("Prompt")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                }
                v.status = match fact.phase {
                    ToolPhase::Failed => SubtaskStatus::Failed,
                    _ => SubtaskStatus::Running,
                };
            },
        );
        vec![Fact::Event(EventBody::SubtaskChanged { subtask })]
    }

    fn observe_workflow(
        &self,
        ctx: &mut AdapterContext<'_>,
        raw_id: &str,
        fact: &ToolFact,
        input: &Value,
    ) -> Vec<Fact> {
        let key = format!("workflow:{raw_id}");
        let title = input
            .get("Description")
            .or_else(|| input.get("ScriptPath"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("Workflow");
        let subtask = ctx.state.upsert(
            &key,
            call_id(&key),
            SubtaskOrigin::NativeWorkflow,
            title,
            |tracked| {
                let v = &mut tracked.view;
                v.actor = Some("workflow".into());
                v.title = bound(title, SUBTASK_TITLE_MAX);
                v.status = match fact.phase {
                    ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
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
        vec![Fact::Event(EventBody::SubtaskChanged { subtask })]
    }

    fn observe_swarm(
        &self,
        ctx: &mut AdapterContext<'_>,
        raw_id: &str,
        fact: &ToolFact,
        input: &Value,
    ) -> Vec<Fact> {
        let action = input.get("action").and_then(Value::as_str).unwrap_or("");
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        match action {
            "add" => {
                let worker = name.unwrap_or("worker");
                let key = format!("swarm:{worker}");
                let subtask = ctx.state.upsert(
                    &key,
                    call_id(&format!("swarm:{raw_id}:{worker}")),
                    SubtaskOrigin::Subagent,
                    worker,
                    |tracked| {
                        let v = &mut tracked.view;
                        v.external_id = Some(worker.to_string());
                        v.actor = Some("swarm".into());
                        if let Some(model) = input
                            .get("model")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            v.model = Some(model.to_string());
                        }
                        if let Some(brief) = input
                            .get("brief")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            v.progress = Some(bound(brief, SUBTASK_TEXT_MAX));
                        }
                        v.status = match fact.phase {
                            ToolPhase::Failed => SubtaskStatus::Failed,
                            _ => SubtaskStatus::Running,
                        };
                    },
                );
                vec![Fact::Event(EventBody::SubtaskChanged { subtask })]
            }
            "kill" if fact.phase == ToolPhase::Succeeded => {
                let Some(worker) = name else { return vec![] };
                let key = format!("swarm:{worker}");
                let Some(tracked) = ctx.state.subtasks.get_mut(&key) else {
                    return vec![];
                };
                tracked.view.status = SubtaskStatus::Cancelled;
                vec![Fact::Event(EventBody::SubtaskChanged {
                    subtask: tracked.view.clone(),
                })]
            }
            _ => vec![],
        }
    }
}

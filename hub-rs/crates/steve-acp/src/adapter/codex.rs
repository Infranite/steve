//! Codex (`@agentclientprotocol/codex-acp`) runtime adapter.
//!
//! Verified empirically against `codex-cli` 0.171.0 with BOTH:
//! - `@agentclientprotocol/codex-acp` 2.12.0 (Steve's builtin package):
//!   - Negotiating `clientCapabilities.subagents: {}` enables ACP RFD #1992 mode:
//!     incremental `subagent_update` frames (`title`, `state`, `capabilities: {cancel: {}}` /
//!     `{cancel: null}`, `cwd`), child session `tool_call`/`tool_call_update` carrying
//!     `_meta.terminal_output_delta.data`, child `usage_update` (child context meter only),
//!     and per-child `session/cancel(childSessionId)`.
//!   - Without `subagents` capability (fallback mode): emits root `tool_call` with
//!     `rawInput: {agentThreadId, agentPath, activityKind: "started" | "completed"}` and
//!     collab `wait`/`spawn_agent`/`send_input`/`close_agent` with
//!     `{senderThreadId, receiverThreadIds, agentsStates}`.
//!   - `PromptResponse` carries both `usage` (`Main`) and `_meta.quota.model_usage` (`All`).
//! - Legacy `@zed-industries/codex-acp` 0.16.0: emits no subagent ACP frames and uses rollout
//!   fallback for usage (configured via `builtins.toml`).

use serde_json::{Value, json};
use steve_node_api::{
    AdapterCapabilities, BackgroundTaskCapability, EventBody, Part, ResumeCapability,
    SubagentCapability, SubtaskOrigin, SubtaskStatus, ToolPhase, ToolView, UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, HarnessAdapter, SUBTASK_TEXT_MAX, SUBTASK_TITLE_MAX,
    handle_rfd_child_update, handle_rfd_session_message, handle_rfd_subagent_update,
};
use crate::normalize::{Fact, TEXT_MAX, ToolFact, bound, call_id};

pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::AcpRfd,
            subagent_cancel: true,
            background_tasks: BackgroundTaskCapability::None,
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
            obj.insert("subagents".into(), json!({}));
        }
    }

    fn enrich_tool_result(&self, raw: &Value, existing: &mut Vec<Part>) {
        let has_text = existing.iter().any(|p| matches!(p, Part::Text { .. }));
        if has_text {
            return;
        }
        if let Some(delta) = raw
            .pointer("/_meta/terminal_output_delta/data")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            existing.push(Part::Text {
                text: bound(delta, TEXT_MAX),
            });
        }
    }

    fn normalize_update(
        &self,
        ctx: &mut AdapterContext<'_>,
        session_id: Option<&str>,
        update: &Value,
    ) -> AdapterOutcome {
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

        // Fallback path 1: codex-acp 2.12 non-RFD subagent lifecycle tool calls:
        // rawInput = { agentThreadId, agentPath, activityKind: "started" | "completed" | ... }
        if let Some(thread_id) = input
            .get("agentThreadId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if is_tracked_rfd_thread(ctx, thread_id) {
                return vec![];
            }
            let key = format!("thread:{thread_id}");
            let agent_name = input
                .get("agentPath")
                .and_then(Value::as_str)
                .and_then(|p| p.rsplit('/').find(|seg| !seg.is_empty()))
                .filter(|s| !s.is_empty());
            let title = agent_name
                .map(|n| n.replace('_', " "))
                .or_else(|| (!fact.title.is_empty()).then(|| fact.title.clone()))
                .unwrap_or_else(|| "Subagent".into());
            let activity = input
                .get("activityKind")
                .and_then(Value::as_str)
                .unwrap_or("");
            let subtask = ctx.state.upsert(
                &key,
                call_id(thread_id),
                SubtaskOrigin::Subagent,
                &title,
                |tracked| {
                    let v = &mut tracked.view;
                    v.external_id = Some(thread_id.to_string());
                    if let Some(actor) = agent_name {
                        v.actor = Some(bound(actor, SUBTASK_TITLE_MAX));
                    }
                    if let Some(model) = input
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.model = Some(model.to_string());
                    }
                    v.status = match activity {
                        "started" | "running" | "in_progress" => SubtaskStatus::Running,
                        "completed" | "done" => SubtaskStatus::Done,
                        "failed" | "error" => SubtaskStatus::Failed,
                        "cancelled" | "canceled" | "closed" => SubtaskStatus::Cancelled,
                        _ => match fact.phase {
                            ToolPhase::Failed => SubtaskStatus::Failed,
                            ToolPhase::Succeeded => SubtaskStatus::Done,
                            _ => SubtaskStatus::Running,
                        },
                    };
                },
            );
            return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
        }

        // Fallback path 2: Codex collabAgentToolCall (`spawn_agent`, `wait`, `send_input`,
        // `resume_agent`, `close_agent`) carrying `agentsStates` or `receiverThreadIds`.
        let name = if !fact.name.is_empty() {
            fact.name.as_str()
        } else {
            fact.title.as_str()
        };
        let is_collab = matches!(
            name,
            "spawn_agent"
                | "send_input"
                | "send_message"
                | "wait"
                | "wait_agent"
                | "resume_agent"
                | "close_agent"
        ) || input.get("agentsStates").is_some()
            || input.get("receiverThreadIds").is_some();
        if !is_collab {
            return vec![];
        }

        let mut facts = Vec::new();
        if let Some(states) = input.get("agentsStates").and_then(Value::as_object) {
            for (tid, st_val) in states {
                if tid.is_empty() || is_tracked_rfd_thread(ctx, tid) {
                    continue;
                }
                let key = format!("thread:{tid}");
                let status_str = st_val
                    .get("status")
                    .and_then(Value::as_str)
                    .or_else(|| st_val.as_str())
                    .unwrap_or("");
                let msg = st_val
                    .get("message")
                    .or_else(|| st_val.get("summary"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let subtask = ctx.state.upsert(
                    &key,
                    call_id(tid),
                    SubtaskOrigin::Subagent,
                    "Subagent",
                    |tracked| {
                        let v = &mut tracked.view;
                        v.external_id = Some(tid.clone());
                        if let Some(model) = input
                            .get("model")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            v.model = Some(model.to_string());
                        }
                        if let Some(prompt) = input
                            .get("prompt")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            && v.progress.is_none()
                        {
                            v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                        }
                        v.status = match status_str {
                            "completed" | "done" => SubtaskStatus::Done,
                            "errored" | "failed" | "error" => SubtaskStatus::Failed,
                            "shutdown" | "closed" | "cancelled" => SubtaskStatus::Cancelled,
                            "pending_init" | "pendingInit" => SubtaskStatus::Queued,
                            _ => SubtaskStatus::Running,
                        };
                        if let Some(m) = msg {
                            if v.status == SubtaskStatus::Failed {
                                v.error = Some(bound(m, SUBTASK_TEXT_MAX));
                            } else {
                                v.summary = Some(bound(m, SUBTASK_TEXT_MAX));
                            }
                        }
                    },
                );
                facts.push(Fact::Event(EventBody::SubtaskChanged { subtask }));
            }
        } else if name == "spawn_agent" {
            let tid = input
                .get("receiverThreadIds")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let key = tid
                .map(|t| format!("thread:{t}"))
                .unwrap_or_else(|| format!("tool:{raw_id}"));
            let id = tid.map(call_id).unwrap_or_else(|| fact.call.clone());
            let title = input
                .get("nickname")
                .or_else(|| input.get("role"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("Subagent");
            let subtask = ctx
                .state
                .upsert(&key, id, SubtaskOrigin::Subagent, title, |tracked| {
                    let v = &mut tracked.view;
                    if let Some(t) = tid {
                        v.external_id = Some(t.to_string());
                    }
                    if let Some(prompt) = input
                        .get("prompt")
                        .or_else(|| input.get("message"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                    }
                    v.status = match fact.phase {
                        ToolPhase::Failed => SubtaskStatus::Failed,
                        _ => SubtaskStatus::Running,
                    };
                });
            facts.push(Fact::Event(EventBody::SubtaskChanged { subtask }));
        }
        facts
    }
}

fn is_tracked_rfd_thread(ctx: &AdapterContext<'_>, thread_id: &str) -> bool {
    ctx.state.subtasks.keys().any(|k| {
        k.starts_with("rfd:") && (k.ends_with(thread_id) || k == &format!("rfd:{thread_id}"))
    })
}

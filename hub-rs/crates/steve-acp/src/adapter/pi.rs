//! Pi (`@openma/pi-acp` + `pi`) runtime adapter.
//!
//! Verified empirically against `pi` 1.5.2 + `@openma/pi-acp` 0.1.8:
//! - Bare Pi over `openma-pi-acp` has no native ACP RFD subagent protocol; only standard tools
//!   (`read`, `bash`, `edit`, `write`, ...) run, so nothing subtask-specific is shown.
//! - When the `pi-subagents` extension is installed (observed locally), Pi exposes `subagent`
//!   (and `bg_wait` for async runs) as standard ACP tool calls whose completed
//!   `rawOutput.details.results[]` carries per-subagent `agent`, `sessionName`, `model`,
//!   `exitCode`, `finalOutput`, `progressSummary.durationMs`, and per-child `usage`
//!   (`input`, `output`, `cacheRead`, `cacheWrite`, `cost`).
//! - Crucially, Pi's `PromptResponse.usage` covers ONLY the parent session (`UsageScope::Main`),
//!   so each child result's `usage` is emitted as `UsageScope::Subtask` (`Measurement::Delta`)
//!   exactly once per settled child without double-counting.

use serde_json::Value;
use steve_node_api::{
    AdapterCapabilities, BackgroundTaskCapability, EventBody, ResumeCapability, SubagentCapability,
    SubtaskOrigin, SubtaskStatus, SubtaskUsage, Tokens, ToolPhase, ToolView, UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, HarnessAdapter, SUBTASK_TEXT_MAX, SUBTASK_TITLE_MAX,
    tool_text_output, usd_amount,
};
use crate::normalize::{Fact, ToolFact, bound, call_id};

pub struct PiAdapter;

impl HarnessAdapter for PiAdapter {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::ToolFallback,
            subagent_cancel: false,
            background_tasks: BackgroundTaskCapability::ToolFallback,
            background_task_cancel: false,
            child_tool_nesting: false,
            usage_attribution: UsageAttribution::SubtaskScoped,
            resume: ResumeCapability::SessionLoad,
            upgrade: steve_node_api::UpgradeCapability::ManagedNpm,
            effort_mode: steve_node_api::EffortMode::AcpOption,
            verified: true,
        }
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
        let name = fact.name.as_str();
        if name != "subagent" && name != "bg_wait" {
            return vec![];
        }
        let input = fact.input();
        let is_async = input.get("async").and_then(Value::as_bool).unwrap_or(false);
        let origin = if is_async || name == "bg_wait" {
            SubtaskOrigin::BackgroundTask
        } else {
            SubtaskOrigin::Subagent
        };

        // Settled `pi-subagents` results carry `rawOutput.details.results[]` (single or fan-out).
        if let Some(results) = fact
            .raw
            .pointer("/rawOutput/details/results")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
        {
            let run_id = fact
                .raw
                .pointer("/rawOutput/details/runId")
                .and_then(Value::as_str);
            let mut out = Vec::new();
            for (i, item) in results.iter().enumerate() {
                let idx = item
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(i as u64);
                let key = if name == "bg_wait" {
                    run_id
                        .map(|r| format!("pi-run:{r}:{idx}"))
                        .unwrap_or_else(|| format!("tool:{raw_id}:{idx}"))
                } else if idx == 0 {
                    format!("tool:{raw_id}")
                } else {
                    format!("tool:{raw_id}:{idx}")
                };
                let id = if idx == 0 && name == "subagent" {
                    fact.call.clone()
                } else {
                    call_id(&key)
                };
                let actor = item
                    .get("agent")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let title = item
                    .get("sessionName")
                    .or_else(|| item.pointer("/progressSummary/sessionName"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .or(actor)
                    .unwrap_or("Subagent");
                let model = item
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let duration_ms = item
                    .pointer("/progressSummary/durationMs")
                    .and_then(Value::as_u64);
                let exit_code = item.get("exitCode").and_then(Value::as_i64);
                let final_output = item
                    .get("finalOutput")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let (sub_usage, report_tokens, report_cost) = parse_pi_usage(item.get("usage"));

                let mut emit_usage = false;
                let subtask = ctx.state.upsert(&key, id, origin, title, |tracked| {
                    let v = &mut tracked.view;
                    v.title = bound(title, SUBTASK_TITLE_MAX);
                    if let Some(a) = actor {
                        v.actor = Some(bound(a, SUBTASK_TITLE_MAX));
                    }
                    if let Some(m) = &model {
                        v.model = Some(m.clone());
                    }
                    if let Some(r) = run_id {
                        v.external_id = Some(format!("{r}:{idx}"));
                    }
                    if duration_ms.is_some() {
                        v.duration_ms = duration_ms;
                    }
                    if sub_usage.is_some() {
                        v.usage = sub_usage.clone();
                    }
                    v.status = match exit_code {
                        Some(0) => SubtaskStatus::Done,
                        Some(_) => SubtaskStatus::Failed,
                        None if fact.phase == ToolPhase::Failed => SubtaskStatus::Failed,
                        None => SubtaskStatus::Done,
                    };
                    if let Some(text) = final_output {
                        if v.status == SubtaskStatus::Failed {
                            v.error = Some(bound(text, SUBTASK_TEXT_MAX));
                        } else {
                            v.summary = Some(bound(text, SUBTASK_TEXT_MAX));
                        }
                    }
                    if !tracked.emitted_usage
                        && (report_tokens != Tokens::default() || report_cost.is_some())
                    {
                        tracked.emitted_usage = true;
                        emit_usage = true;
                    }
                });
                let sub_id = subtask.id.clone();
                out.push(Fact::Event(EventBody::SubtaskChanged { subtask }));
                if emit_usage {
                    let report =
                        ctx.subtask_usage_report(&sub_id, model, report_tokens, report_cost);
                    out.push(Fact::Event(EventBody::Usage { report }));
                }
            }
            return out;
        }

        // Running / pending / unparsed `subagent` tool call
        if name != "subagent" {
            return vec![];
        }
        let key = format!("tool:{raw_id}");
        let actor = input
            .get("agent")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let task = input
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let title = task
            .map(|t| t.lines().next().unwrap_or(t))
            .or(actor)
            .unwrap_or("Subagent");
        let subtask = ctx
            .state
            .upsert(&key, fact.call.clone(), origin, title, |tracked| {
                let v = &mut tracked.view;
                if title != "Subagent" {
                    v.title = bound(title, SUBTASK_TITLE_MAX);
                }
                if let Some(a) = actor {
                    v.actor = Some(bound(a, SUBTASK_TITLE_MAX));
                }
                if let Some(t) = task {
                    v.progress = Some(bound(t, SUBTASK_TEXT_MAX));
                }
                v.status = match fact.phase {
                    ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
                    ToolPhase::Succeeded if is_async => SubtaskStatus::Running,
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
            });
        vec![Fact::Event(EventBody::SubtaskChanged { subtask })]
    }
}

fn parse_pi_usage(
    v: Option<&Value>,
) -> (Option<SubtaskUsage>, Tokens, Option<steve_node_api::Money>) {
    let Some(u) = v.and_then(Value::as_object) else {
        return (None, Tokens::default(), None);
    };
    let input = u.get("input").and_then(Value::as_u64);
    let output = u.get("output").and_then(Value::as_u64);
    let cache_read = u.get("cacheRead").and_then(Value::as_u64);
    let cache_write = u.get("cacheWrite").and_then(Value::as_u64);
    let cost = u.get("cost").and_then(Value::as_f64).and_then(usd_amount);
    let tokens = Tokens {
        input,
        output,
        cache_read,
        cache_write,
        reasoning: None,
    };
    if tokens == Tokens::default() && cost.is_none() {
        return (None, tokens, None);
    }
    let total_tokens = match (input, output) {
        (Some(i), Some(o)) => Some(
            i.saturating_add(o)
                .saturating_add(cache_read.unwrap_or(0))
                .saturating_add(cache_write.unwrap_or(0)),
        ),
        _ => None,
    };
    let sub = SubtaskUsage {
        input,
        output,
        cache_read,
        cache_write,
        reasoning: None,
        total_tokens,
        context_used: None,
        context_limit: None,
        cost: cost.clone(),
    };
    (Some(sub), tokens, cost)
}

//! Generic fallback adapter for all builtin/custom harnesses that have not been empirically
//! verified against real runs (`capabilities.verified == false`).
//!
//! Deliberately does NOT advertise experimental `clientCapabilities.subagents` unprompted, while
//! remaining forward-compatible if an adapter proactively emits standard ACP RFD #1992
//! `subagent_update` / `session_message` notifications.

use serde_json::Value;
use steve_node_api::{
    AdapterCapabilities, BackgroundTaskCapability, ResumeCapability, SubagentCapability, ToolView,
    UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, HarnessAdapter, handle_rfd_child_update,
    handle_rfd_session_message, handle_rfd_subagent_update,
};
use crate::normalize::{Fact, ToolFact};

pub struct GenericAdapter;

impl HarnessAdapter for GenericAdapter {
    fn id(&self) -> &'static str {
        "generic"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::None,
            subagent_cancel: false,
            background_tasks: BackgroundTaskCapability::None,
            background_task_cancel: false,
            child_tool_nesting: false,
            usage_attribution: UsageAttribution::TurnLevel,
            resume: ResumeCapability::SessionLoad,
            upgrade: steve_node_api::UpgradeCapability::Manual,
            verified: false,
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
        _raw_id: &str,
        _fact: &ToolFact,
        view: &mut ToolView,
    ) -> Vec<Fact> {
        view.parent = ctx
            .state
            .resolve_parent(ctx.root_session_id, session_id, None);
        vec![]
    }
}

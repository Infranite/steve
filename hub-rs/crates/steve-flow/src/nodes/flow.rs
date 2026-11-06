//! `flow`: `{flow_id, input}` calls another flow of the workspace synchronously (nesting ≤ 5). The
//! child is a run of its own (`parent_run`, `depth + 1`); its last output is this node's output. A
//! child that has to wait parks this run too, and its end resumes it
//! ([`crate::engine::GraphEngine`]).

use async_trait::async_trait;
use serde_json::Value;

use super::{NodeKind, NodeOutcome, Rule, RunCx, req_str};

pub struct SubFlow;

/// Deepest nesting of `flow` nodes.
pub const MAX_DEPTH: i64 = 5;

#[async_trait]
impl NodeKind for SubFlow {
    fn kind(&self) -> &'static str {
        "flow"
    }

    /// The child decides for itself: its effectful nodes are gated in its own run.
    fn rules(&self) -> Vec<Rule> {
        vec![Rule::new(
            &[
                "flow.node.missing_flow",
                "flow.node.too_long_flow",
                "flow.graph.no_such_flow",
            ],
            "flow_id 必填，只能是本工作空间已有流程的 id；不知道 id 就不要用 flow 节点",
        )]
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        req_str(cfg, "flow_id", super::Required::Flow, 64)?;
        Ok(())
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let child_input = match cfg.get("input") {
            None | Some(Value::Null) => input.clone(),
            Some(v) => cx.render_value(v, input),
        };
        crate::engine::call_child(cx, cfg["flow_id"].as_str().unwrap_or(""), child_input).await
    }
}

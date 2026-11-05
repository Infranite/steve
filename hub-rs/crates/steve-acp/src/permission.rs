//! ACP permission requests in Steve's terms, and Steve's answers back in ACP's. Only a Node's driver
//! uses this; the runner keeps its own decision-card fields ([`crate::permission_request`]).
//!
//! An answer maps to an option of the very request: `allow_once` for [`PermissionDecision::AllowOnce`]
//! (never `allow_always`: nothing is persisted into a harness's settings), a `reject_*` for
//! [`PermissionDecision::Reject`]. A request that offers no such option does not offer that choice.

use std::path::Path;

use serde_json::{Value, json};
use steve_node_api::{Action, PermissionDecision};

use crate::normalize::{self, bound};

/// One `session/request_permission`, read.
#[derive(Debug, Clone, PartialEq)]
pub struct Asked {
    /// The harness's tool call id (or the RPC id when it has none).
    pub tool_call: String,
    pub title: String,
    pub target: Option<String>,
    pub action: Action,
    /// Steve's own tool the request is for (`steve_deliver`), if it is one.
    pub steve_tool: Option<String>,
    /// The choices it offers and the option each answers with.
    pub choices: Vec<(PermissionDecision, String)>,
}

/// Reads a request; `root` is the workspace, to show paths inside it relative.
pub fn asked(rpc_id: &Value, params: &Value, root: &Path) -> Asked {
    let tc = params.get("toolCall").filter(|v| !v.is_null());
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
    let tool_call = s(tc.and_then(|t| t.get("toolCallId"))).unwrap_or_else(|| match rpc_id {
        Value::String(x) => x.clone(),
        other => other.to_string(),
    });
    // the request's tool call, read like a tool_call update
    let mut n = normalize::Normalizer::new(
        root,
        "perm",
        steve_node_api::Epoch::new("perm").expect("valid"),
    );
    let mut update = tc.cloned().unwrap_or_else(|| json!({}));
    if let Some(o) = update.as_object_mut() {
        o.insert("sessionUpdate".into(), json!("tool_call"));
        o.insert("toolCallId".into(), json!(tool_call));
    }
    let view = n.update(&update).into_iter().find_map(|f| match f {
        normalize::Fact::Event(steve_node_api::EventBody::ToolChanged { tool }) => Some(tool),
        _ => None,
    });
    let title = s(tc.and_then(|t| t.get("title")))
        .or_else(|| s(tc.and_then(|t| t.get("kind"))))
        .map(|t| bound(&t, 200))
        .unwrap_or_else(|| "工具调用".into());
    let choices = params
        .get("options")
        .and_then(Value::as_array)
        .map(|os| {
            let mut out: Vec<(PermissionDecision, String)> = vec![];
            for o in os {
                let id = s(o.get("optionId")).unwrap_or_default();
                let d = match o.get("kind").and_then(Value::as_str) {
                    Some("allow_once") => PermissionDecision::AllowOnce,
                    Some("reject_once") | Some("reject_always") => PermissionDecision::Reject,
                    _ => continue,
                };
                // the first option of each kind (reject_once before reject_always when both)
                if !out.iter().any(|(x, _)| *x == d) {
                    out.push((d, id));
                }
            }
            out
        })
        .unwrap_or_default();
    Asked {
        tool_call,
        title,
        target: view.as_ref().and_then(|v| v.target.clone()),
        action: view.and_then(|v| v.action).unwrap_or(Action::Unknown {
            summary: String::new(),
        }),
        steve_tool: crate::steve_tool(params),
        choices,
    }
}

/// The JSON-RPC result answering a request: the option of `decision`, or `cancelled` (no answer, or
/// a choice the request did not offer).
pub fn answer(asked: &Asked, decision: Option<PermissionDecision>) -> Value {
    match decision.and_then(|d| asked.choices.iter().find(|(x, _)| *x == d)) {
        Some((_, option)) => json!({ "outcome": { "outcome": "selected", "optionId": option } }),
        None => json!({ "outcome": { "outcome": "cancelled" } }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: &str = r#"[{"optionId":"allow","name":"Always Allow","kind":"allow_always"},
        {"optionId":"once","name":"Allow","kind":"allow_once"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]"#;

    fn params(tool: Value) -> Value {
        json!({"sessionId": "x", "toolCall": tool, "options": serde_json::from_str::<Value>(OPTIONS).unwrap()})
    }

    #[test]
    fn a_shell_request_reads_as_an_exec() {
        let p = params(
            json!({"toolCallId": "toolu_9", "kind": "execute", "title": "git push",
            "rawInput": {"command": "git push origin main"}, "_meta": {"claudeCode": {"toolName": "Bash"}}}),
        );
        let a = asked(&json!(7), &p, Path::new("/w"));
        assert_eq!(a.tool_call, "toolu_9");
        assert!(
            matches!(&a.action, Action::Exec { command, .. } if command == "git push origin main")
        );
        assert_eq!(
            a.choices,
            vec![
                (PermissionDecision::AllowOnce, "once".into()),
                (PermissionDecision::Reject, "reject".into())
            ]
        );
        assert_eq!(
            answer(&a, Some(PermissionDecision::AllowOnce)),
            json!({"outcome": {"outcome": "selected", "optionId": "once"}})
        );
        assert_eq!(
            answer(&a, None),
            json!({"outcome": {"outcome": "cancelled"}})
        );
    }

    #[test]
    fn allow_always_is_never_chosen() {
        let p = json!({"toolCall": {"toolCallId": "t", "title": "x"},
            "options": [{"optionId": "always", "kind": "allow_always"}, {"optionId": "no", "kind": "reject_once"}]});
        let a = asked(&json!(1), &p, Path::new("/w"));
        assert_eq!(a.choices, vec![(PermissionDecision::Reject, "no".into())]);
        // asked to allow anyway: not offered, so cancelled rather than "always"
        assert_eq!(
            answer(&a, Some(PermissionDecision::AllowOnce)),
            json!({"outcome": {"outcome": "cancelled"}})
        );
    }

    #[test]
    fn steve_tools_are_recognized() {
        let p = params(
            json!({"toolCallId": "t", "title": "mcp__steve__steve_deliver",
            "_meta": {"claudeCode": {"toolName": "mcp__steve__steve_deliver"}}}),
        );
        assert_eq!(
            asked(&json!(1), &p, Path::new("/w")).steve_tool.as_deref(),
            Some("steve_deliver")
        );
    }
}

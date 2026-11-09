//! ACP permission requests in Steve's terms, and Steve's answers back in ACP's, for a Node's driver.
//!
//! An answer maps to an option of the very request: `allow_once` for [`PermissionDecision::AllowOnce`]
//! (never `allow_always`: nothing is persisted into a harness's settings), `reject_once` for
//! [`PermissionDecision::Reject`]. A request that offers no such option does not offer that choice.

use serde_json::{Value, json};
use steve_node_api::{Action, PermissionDecision};

use crate::normalize::{Fact, Normalizer, ToolFact};

/// One `session/request_permission`, read.
#[derive(Debug, Clone, PartialEq)]
pub struct Asked {
    /// Snapshot of the same fact that produces ToolView; use fact.call as permission invocation.
    pub fact: ToolFact,
    /// Ordered message boundary, tool projection and diagnostics from this request. The Node must
    /// publish these before the permission request event, under the same execution ordering.
    pub updates: Vec<Fact>,
    /// The harness's tool call id (or the RPC id when it has none).
    pub tool_call: String,
    pub title: String,
    pub target: Option<String>,
    /// Display compatibility only, not an authorization payload. Bind fact's paths/cwd/argv and
    /// resource versions in the Node before constructing PermissionRequest.action/digest.
    pub action: Action,
    /// Steve's own tool the request is for (`steve_deliver`), if it is one.
    pub steve_tool: Option<String>,
    /// The choices it offers and the option each answers with.
    pub choices: Vec<(PermissionDecision, String)>,
}

/// Merge the request's ToolCallUpdate into the execution's existing facts. This may be the first
/// observation of the tool; missing fields never erase those from earlier session/update messages.
/// Paths/cwd/argv still need binding and preconditions in the Node before approval.
pub fn asked(norm: &mut Normalizer, rpc_id: &Value, params: &Value) -> Asked {
    let tc = params.get("toolCall").filter(|v| v.is_object());
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
    let tool_call = s(tc.and_then(|t| t.get("toolCallId"))).unwrap_or_else(|| match rpc_id {
        Value::String(x) => x.clone(),
        other => other.to_string(),
    });
    let mut update = tc.cloned().unwrap_or_else(|| json!({}));
    if !update.is_object() {
        update = json!({});
    }
    if let Some(o) = update.as_object_mut() {
        o.insert("toolCallId".into(), json!(tool_call));
    }
    update["sessionUpdate"] = json!("tool_call_update");
    let updates = norm.update(&update);
    // the merge always knows the tool now (its id was set above); never panic on a harness's input
    let view = updates
        .iter()
        .find_map(|f| match f {
            Fact::Event(steve_node_api::EventBody::ToolChanged { tool }) => Some(tool.clone()),
            _ => None,
        })
        .or_else(|| norm.view_of(&tool_call));
    let (Some(view), Some(fact)) = (view, norm.fact(&tool_call).cloned()) else {
        let mut fallback = norm_fallback(&tool_call);
        fallback.choices = choices_of(params);
        return fallback;
    };
    let title = if view.title.is_empty() {
        "工具调用".into()
    } else {
        view.title.clone()
    };
    /* a call to Steve's own server, named or not: some harnesses ask about the server with no tool
     * name. The Hub authorizes each Steve tool call itself, so the request needs no one's answer */
    let steve_tool = (fact.origin == steve_node_api::ToolOrigin::Steve).then(|| {
        fact.name
            .strip_prefix("mcp__steve__")
            .unwrap_or(&fact.name)
            .to_string()
    });
    let choices = choices_of(params);
    Asked {
        fact,
        updates,
        tool_call,
        title,
        target: view.target,
        action: view.action.unwrap_or(Action::Unknown {
            summary: String::new(),
        }),
        steve_tool,
        choices,
    }
}

/// The choices a request offers and the option each answers with (`allow_once`, `reject_once`).
fn choices_of(params: &Value) -> Vec<(PermissionDecision, String)> {
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
    params
        .get("options")
        .and_then(Value::as_array)
        .map(|os| {
            let mut out: Vec<(PermissionDecision, String)> = vec![];
            for o in os {
                let id = s(o.get("optionId")).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let d = match o.get("kind").and_then(Value::as_str) {
                    Some("allow_once") => PermissionDecision::AllowOnce,
                    Some("reject_once") => PermissionDecision::Reject,
                    _ => continue,
                };
                // Persistent choices never map to a one-request decision.
                if !out.iter().any(|(x, _)| *x == d) {
                    out.push((d, id));
                }
            }
            out
        })
        .unwrap_or_default()
}

/// A request whose tool the facts do not know (not reachable: the merge always records it).
fn norm_fallback(tool_call: &str) -> Asked {
    let fact = ToolFact {
        raw_id: tool_call.to_string(),
        call: crate::normalize::call_id(tool_call),
        name: String::new(),
        category: steve_node_api::ToolCategory::Other,
        origin: steve_node_api::ToolOrigin::Harness,
        title: String::new(),
        phase: steve_node_api::ToolPhase::Pending,
        paths: vec![],
        cwd: None,
        command: None,
        query: None,
        url: None,
        change: None,
        result: vec![],
        error: None,
        raw: json!({}),
    };
    Asked {
        fact,
        updates: vec![],
        tool_call: tool_call.to_string(),
        title: "工具调用".into(),
        target: None,
        action: Action::Unknown {
            summary: String::new(),
        },
        steve_tool: None,
        choices: vec![],
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

    fn norm() -> Normalizer {
        Normalizer::new(
            "/w",
            "attempt",
            steve_node_api::Epoch::new("meter").unwrap(),
        )
    }

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
        let a = asked(&mut norm(), &json!(7), &p);
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
        let a = asked(&mut norm(), &json!(1), &p);
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
            asked(&mut norm(), &json!(1), &p).steve_tool.as_deref(),
            Some("steve_deliver")
        );
        /* a harness asking about Steve's server with no tool name (Codex's MCP approval) */
        let p = params(json!({"toolCallId": "t2", "title": "Approve MCP tool call",
            "rawInput": {"server_name": "steve", "id": "mcp_tool_call_approval_call_x",
                "request": {"mode": "form", "_meta": {"codex_approval_kind": "mcp_tool_call"}}}}));
        assert!(asked(&mut norm(), &json!(2), &p).steve_tool.is_some());
        /* another server's is not Steve's */
        let p = params(json!({"toolCallId": "t3", "title": "Approve MCP tool call",
            "rawInput": {"server_name": "github", "id": "x"}}));
        assert!(asked(&mut norm(), &json!(3), &p).steve_tool.is_none());
    }
}

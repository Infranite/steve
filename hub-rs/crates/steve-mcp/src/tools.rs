//! Tool definitions and argument checks, kept byte-compatible with the TypeScript Hub.
//!
//! `tools.json` is the `tools/list` result captured from the TS server (zod → JSON Schema),
//! so descriptions and schemas are exactly what the agents saw before. The validator walks
//! those same schemas and reports problems with zod v4's wording, because agents read the
//! error text to fix their call.

use std::sync::{Arc, LazyLock};

use rmcp::model::{JsonObject, Tool};
use serde_json::Value;

/// Appended to every "send and end the turn" result; agents rely on this exact wording.
pub const END: &str = "End your turn now. The user's answer will arrive as your next message.";

pub const UPDATE: &str = "steve_update";
pub const ASK: &str = "steve_ask";
pub const REQUEST_APPROVAL: &str = "steve_request_approval";
pub const DELIVER: &str = "steve_deliver";
pub const PUBLISH: &str = "steve_publish";
pub const LANE_STATUS: &str = "steve_lane_status";

/// Added to the `steve_publish` result: where to look for its progress instead of asking the user.
pub const PUBLISHED: &str = "To see its progress (the merge-lane batch, an approval it waits for, a failure and why, the released version), call steve_lane_status; don't ask the user.";

struct Def {
    name: String,
    description: String,
    schema: Arc<JsonObject>,
}

static DEFS: LazyLock<Vec<Def>> = LazyLock::new(|| {
    let raw: Vec<Value> = serde_json::from_str(include_str!("tools.json"))
        .expect("tools.json is a compile-time constant and must parse");
    raw.into_iter()
        .map(|t| Def {
            name: t["name"].as_str().unwrap_or_default().to_owned(),
            description: t["description"].as_str().unwrap_or_default().to_owned(),
            schema: Arc::new(match &t["inputSchema"] {
                Value::Object(m) => m.clone(),
                _ => JsonObject::new(),
            }),
        })
        .collect()
});

/// The tools in the order the TS server lists them.
pub fn list() -> Vec<Tool> {
    DEFS.iter()
        .map(|d| Tool::new(d.name.clone(), d.description.clone(), d.schema.clone()))
        .collect()
}

/// Input schema of a tool, if it exists.
pub fn schema(name: &str) -> Option<&'static JsonObject> {
    DEFS.iter().find(|d| d.name == name).map(|d| &*d.schema)
}

/// Checks `args` against the tool's schema. `Err` carries the text the TS server returns
/// as an `isError` result (including the `MCP error -32602:` prefix the SDK adds).
pub fn validate(name: &str, args: &JsonObject) -> Result<(), String> {
    let Some(schema) = schema(name) else {
        return Err(format!("MCP error -32602: Tool {name} not found"));
    };
    let mut issues = Vec::new();
    check_object(schema, args, &mut String::new(), &mut issues);
    if issues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {name}: {}",
            issues.join("\n")
        ))
    }
}

/// zod's name for the type of a received value.
fn received(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn issue(expected: &str, got: Option<&Value>, path: &str) -> String {
    let mut s = format!(
        "Invalid input: expected {expected}, received {}",
        received(got)
    );
    if !path.is_empty() {
        s.push_str(" at ");
        s.push_str(path);
    }
    s
}

/// Properties are checked in schema order, the order zod reports them in.
fn check_object(schema: &JsonObject, obj: &JsonObject, path: &mut String, out: &mut Vec<String>) {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(Value::Object(props)) = schema.get("properties") else {
        return;
    };
    for (key, sub) in props {
        let v = obj.get(key);
        if v.is_none() && !required.contains(&key.as_str()) {
            continue;
        }
        let len = path.len();
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(key);
        if let Value::Object(sub) = sub {
            check(sub, v, path, out);
        }
        path.truncate(len);
    }
}

fn check(schema: &JsonObject, v: Option<&Value>, path: &mut String, out: &mut Vec<String>) {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") if !matches!(v, Some(Value::String(_))) => {
            out.push(issue("string", v, path))
        }
        Some("boolean") if !matches!(v, Some(Value::Bool(_))) => {
            out.push(issue("boolean", v, path))
        }
        Some("array") => match v {
            Some(Value::Array(items)) => {
                let Some(Value::Object(item_schema)) = schema.get("items") else {
                    return;
                };
                for (i, item) in items.iter().enumerate() {
                    let len = path.len();
                    path.push_str(&format!("[{i}]"));
                    check(item_schema, Some(item), path, out);
                    path.truncate(len);
                }
            }
            _ => out.push(issue("array", v, path)),
        },
        Some("object") => match v {
            Some(Value::Object(o)) => check_object(schema, o, path, out),
            _ => out.push(issue("object", v, path)),
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> JsonObject {
        match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    #[test]
    fn lists_the_tools_in_ts_order_then_lane_status() {
        let names: Vec<_> = list().into_iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            [UPDATE, ASK, REQUEST_APPROVAL, DELIVER, PUBLISH, LANE_STATUS]
        );
        assert_eq!(validate(LANE_STATUS, &JsonObject::new()), Ok(()));
    }

    #[test]
    fn descriptions_are_verbatim() {
        let t = list();
        assert_eq!(
            t[1].description.as_deref(),
            Some(
                "Ask the user a question you cannot answer yourself. Prefer concrete options. Do not ask in plain text; use this tool, then end your turn."
            )
        );
        let kind =
            &schema(DELIVER).unwrap()["properties"]["outputs"]["items"]["properties"]["kind"];
        assert_eq!(kind["description"], "e.g. 分支, 提交, PR, 文件, 文档");
    }

    // Expected strings below were captured from the TS server (MCP SDK 1.32.2, zod 4.6.7).
    #[test]
    fn zod_wording_for_wrong_types() {
        let e = validate(ASK, &obj(json!({"question": 1}))).unwrap_err();
        assert_eq!(
            e,
            "MCP error -32602: Input validation error: Invalid arguments for tool steve_ask: Invalid input: expected string, received number at question\nInvalid input: expected array, received undefined at options"
        );
        let e = validate(
            ASK,
            &obj(json!({"question": [], "options": [1, true, null]})),
        )
        .unwrap_err();
        assert!(e.ends_with("Invalid input: expected string, received array at question\nInvalid input: expected string, received number at options[0]\nInvalid input: expected string, received boolean at options[1]\nInvalid input: expected string, received null at options[2]"));
        let e = validate(UPDATE, &obj(json!({"now": "x", "plan": [1, {"step": 2}]}))).unwrap_err();
        assert!(e.ends_with(": Invalid input: expected object, received number at plan[0]\nInvalid input: expected string, received number at plan[1].step\nInvalid input: expected boolean, received undefined at plan[1].done"));
        let e = validate(
            DELIVER,
            &obj(json!({"summary": "s", "outputs": [{"kind": 1}], "checks": [{"name": "b", "passed": "yes"}]})),
        )
        .unwrap_err();
        assert!(e.ends_with(": Invalid input: expected string, received number at outputs[0].kind\nInvalid input: expected string, received undefined at outputs[0].title\nInvalid input: expected boolean, received string at checks[0].passed"));
    }

    #[test]
    fn optional_and_extra_keys() {
        let ok = json!({"summary": "s", "outputs": [{"kind": "PR", "title": "t", "x": 1}], "checks": [], "extra": 2});
        assert_eq!(validate(DELIVER, &obj(ok)), Ok(()));
        let null_loc = json!({"summary": "x", "outputs": [{"kind": "a", "title": "t", "location": null}], "checks": []});
        assert!(
            validate(DELIVER, &obj(null_loc))
                .unwrap_err()
                .ends_with("expected string, received null at outputs[0].location")
        );
        let e = validate(
            DELIVER,
            &obj(json!({"summary": "x", "outputs": [], "checks": [], "unfinished": ["a", 3]})),
        );
        assert_eq!(
            e,
            Err("MCP error -32602: Input validation error: Invalid arguments for tool steve_deliver: Invalid input: expected string, received number at unfinished[1]".into())
        );
        let e = validate(
            DELIVER,
            &obj(json!({"summary": "x", "outputs": [], "checks": [], "unfinished": null})),
        );
        assert!(
            e.unwrap_err()
                .ends_with(": Invalid input: expected array, received null at unfinished")
        );
    }

    #[test]
    fn unknown_tool() {
        assert_eq!(
            validate("nope", &JsonObject::new()),
            Err("MCP error -32602: Tool nope not found".into())
        );
    }
}

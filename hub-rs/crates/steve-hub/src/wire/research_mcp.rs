//! Steve's `/mcp` for a research run's token (thread-routing §7.4): only the four read-only tools exist —
//! the knowledge base (`steve_knowledge_search`, `steve_knowledge_read`) and this workspace's items
//! (`steve_items_search`, `steve_item_read`). Anything else is refused. The token is the run's own
//! (`thread_runs.token_hash`), valid while the run is `running`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use steve_store::{Store, ThreadRun};

use super::mcp_extra::Extra;

pub const ITEMS_SEARCH: &str = "steve_items_search";
pub const ITEM_READ: &str = "steve_item_read";
/// The answer to any other tool.
pub const REFUSED: &str = "This tool is not available in a read-only research run.";

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "properties": props, "required": required})
}

/// The tools a research run sees.
pub fn tools() -> Vec<Value> {
    let mut v: Vec<Value> = steve_knowledge::tools::tools()
        .iter()
        .filter(|t| {
            matches!(
                t["name"].as_str(),
                Some(steve_knowledge::tools::SEARCH | steve_knowledge::tools::READ)
            )
        })
        .cloned()
        .collect();
    v.push(json!({
        "name": ITEMS_SEARCH,
        "description": "Search this workspace's items (tracked work) by words in their title, goal or current status. Returns id, title, status and what is happening now.",
        "inputSchema": schema(json!({"query": {"type": "string"}}), &["query"]),
    }));
    v.push(json!({
        "name": ITEM_READ,
        "description": "Read one item of this workspace: goal, plan, the last 20 timeline entries and the latest delivery summary.",
        "inputSchema": schema(json!({"id": {"type": "string", "description": "e.g. ST-20"}}), &["id"]),
    }));
    v
}

fn status_label(s: &str) -> &str {
    match s {
        "run" => "进行中",
        "wait" => "等用户",
        "block" => "等前置",
        "pause" => "暂停",
        "done" => "已完成",
        "drop" => "已放弃",
        o => o,
    }
}

/// `steve_items_search`: up to 20 items whose title, goal or “now” contain every word of the query.
pub fn items_search(store: &Store, ws: &str, query: &str) -> Result<String, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let items = store.items().all(ws).map_err(|e| e.to_string())?;
    let mut out = vec![];
    for i in items {
        let goal = store.items().goal(&i.id).unwrap_or_default();
        let hay = format!("{} {} {} {}", i.id, i.title, goal, i.now).to_lowercase();
        if words.iter().all(|w| hay.contains(w)) {
            out.push(format!(
                "{} · {} · {} · 现在：{}",
                i.id,
                i.title,
                status_label(&i.status),
                i.now
            ));
        }
        if out.len() >= 20 {
            break;
        }
    }
    Ok(if out.is_empty() {
        "No items match.".into()
    } else {
        out.join("\n")
    })
}

/// `steve_item_read`: one item of this workspace.
pub fn item_read(store: &Store, ws: &str, id: &str) -> Result<String, String> {
    let Some(i) = store
        .items()
        .get(id.trim())
        .map_err(|e| e.to_string())?
        .filter(|i| i.workspace_id == ws)
    else {
        return Err(format!("No item {id} in this workspace."));
    };
    let goal = store.items().goal(&i.id).unwrap_or_default();
    let mut s = format!(
        "{} {}\n状态：{}\n现在：{}\n目标：{goal}\n",
        i.id,
        i.title,
        status_label(&i.status),
        i.now
    );
    if !i.plan.is_empty() {
        s.push_str("计划：\n");
        for p in &i.plan {
            s.push_str(&format!(
                "  [{}] {}\n",
                if p.done { "x" } else { " " },
                p.step
            ));
        }
    }
    let tl = store.timeline().list(&i.id).map_err(|e| e.to_string())?;
    if !tl.is_empty() {
        s.push_str("时间线（最近 20 条）：\n");
        for t in tl.iter().rev().take(20).rev() {
            s.push_str(&format!("  {} {}\n", t.actor, t.text));
        }
    }
    let delivered = store
        .decisions()
        .of_item(&i.id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|d| d.kind == "accept" && !d.draft.trim().is_empty())
        .max_by_key(|d| d.created);
    if let Some(d) = delivered {
        s.push_str(&format!("最近一次交付摘要：\n{}\n", d.draft));
    }
    Ok(s)
}

fn rpc(id: &Value, result: Value) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
    )
        .into_response()
}

fn text_result(id: &Value, out: Result<String, String>) -> Response {
    match out {
        Ok(t) => rpc(id, json!({"content": [{"type": "text", "text": t}]})),
        Err(t) => rpc(
            id,
            json!({"content": [{"type": "text", "text": t}], "isError": true}),
        ),
    }
}

/// One JSON-RPC message from a research run.
pub async fn handle(x: &Extra, run: ThreadRun, msg: Value, owner: String) -> Response {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg["method"].as_str().unwrap_or_default();
    match method {
        "initialize" => rpc(
            &id,
            json!({
                "protocolVersion": msg.pointer("/params/protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "steve", "version": "1.0.0"},
            }),
        ),
        m if m.starts_with("notifications/") || msg.get("id").is_none() => {
            StatusCode::ACCEPTED.into_response()
        }
        "ping" => rpc(&id, json!({})),
        "tools/list" => rpc(&id, json!({"tools": tools()})),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let ws = run.workspace_id.clone();
            let out = match name.as_str() {
                steve_knowledge::tools::SEARCH | steve_knowledge::tools::READ => {
                    let cx = steve_modules::contacts::mcp::McpCx {
                        ws,
                        user: run.user_id.clone().unwrap_or(owner),
                        bot_id: "steve".into(),
                        item_id: None,
                        session_id: None,
                    };
                    steve_knowledge::tools::call(&x.modules, &x.kb, &cx, &name, &args).await
                }
                ITEMS_SEARCH => {
                    let (store, q) = (x.store.clone(), args["query"].as_str().unwrap_or("").to_string());
                    tokio::task::spawn_blocking(move || items_search(&store, &ws, &q))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                }
                ITEM_READ => {
                    let (store, i) = (x.store.clone(), args["id"].as_str().unwrap_or("").to_string());
                    tokio::task::spawn_blocking(move || item_read(&store, &ws, &i))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                }
                _ => Err(REFUSED.into()),
            };
            text_result(&id, out)
        }
        _ => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}}).to_string(),
        )
            .into_response(),
    }
}

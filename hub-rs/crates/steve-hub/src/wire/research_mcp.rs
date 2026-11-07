//! Steve's `/mcp` for a research run's token (thread-routing §7.4): only read-only tools exist — the
//! knowledge base (`steve_knowledge_search`, `steve_knowledge_read`), this workspace's items
//! (`steve_items_search`, `steve_item_read`) and its agent sessions (`steve_sessions_list`). Anything
//! else is refused. The token is the run's own
//! (`thread_runs.token_hash`), valid while the run is `running`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use steve_store::{Store, ThreadRun};

use super::mcp_extra::Extra;

pub const ITEMS_SEARCH: &str = "steve_items_search";
pub const ITEM_READ: &str = "steve_item_read";
pub const SESSIONS_LIST: &str = "steve_sessions_list";
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
        "description": "Search this workspace's tracked work by ST item ID or words in title, goal or current status. Use this for task progress, not knowledge search. Returns id, title, status and what is happening now.",
        "inputSchema": schema(json!({"query": {"type": "string"}}), &["query"]),
    }));
    v.push(json!({
        "name": ITEM_READ,
        "description": "Read the current progress of one workspace item by its ST number (for example ST-23): goal, status, plan, the last 20 timeline entries and the latest delivery summary. Prefer this when the user mentions an ST number.",
        "inputSchema": schema(json!({"id": {"type": "string", "description": "e.g. ST-20"}}), &["id"]),
    }));
    v.push(json!({
        "name": SESSIONS_LIST,
        "description": "List this workspace's agent sessions (Claude Code, Codex …) Steve runs or imported: how many are running, idle or failed, and for each its harness, model, bot, item and title. Use this for questions like how many sessions are running or what the agents are doing now. `all: true` also lists ended ones.",
        "inputSchema": schema(json!({"all": {"type": "boolean"}}), &[]),
    }));
    // These tools only retrieve information. Advertise that fact to MCP clients;
    // authorization still comes from the live run token and the server allowlist.
    for tool in &mut v {
        tool["annotations"] = json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "openWorldHint": false
        });
    }
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

/// Which items a run's item tools see. Only the owner reads the answer: the workspace's items, all of
/// them. Anyone else reads it too: only the items this conversation itself produced (its `thread_items`),
/// and of those only what came from it (title, status, goal); no plan, timeline, delivery or “now”.
/// Until the conversation card knows each listener's rights (phase 2), nothing wider is safe to say.
pub enum ItemScope {
    All,
    Conversation(std::collections::HashSet<String>),
}

impl ItemScope {
    /// For the run answering message `origin`, decided again on every call (members change).
    pub fn of(store: &Store, origin: &str) -> Result<Self, String> {
        if own_reader(store, origin)? {
            return Ok(Self::All);
        }
        let Some(m) = store.messages().get(origin).map_err(|e| e.to_string())? else {
            return Ok(Self::Conversation(Default::default()));
        };
        let ids = store
            .reader(|c| -> steve_store::Result<Vec<String>> {
                let mut st = c.prepare_cached(
                    "SELECT ti.item_id FROM thread_items ti JOIN messages m ON m.id = ti.origin_msg
                      WHERE m.space = ?1 AND m.workspace_id = ?2 AND ti.workspace_id = ?2",
                )?;
                let rows = st.query_map([&m.space, &m.workspace_id], |r| r.get(0))?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .map_err(|e| e.to_string())?;
        Ok(Self::Conversation(ids.into_iter().collect()))
    }

    pub(super) fn sees(&self, item: &str) -> bool {
        match self {
            Self::All => true,
            Self::Conversation(ids) => ids.contains(item),
        }
    }
}

/// `steve_items_search`: up to 20 items whose title, goal or “now” contain every word of the query
/// (in a conversation scope: title and goal, among its own items).
pub fn items_search(
    store: &Store,
    ws: &str,
    scope: &ItemScope,
    query: &str,
) -> Result<String, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let items = store.items().all(ws).map_err(|e| e.to_string())?;
    let all = matches!(scope, ItemScope::All);
    let mut out = vec![];
    for i in items.into_iter().filter(|i| scope.sees(&i.id)) {
        let goal = store.items().goal(&i.id).unwrap_or_default();
        let now = if all { i.now.as_str() } else { "" };
        let hay = format!("{} {} {} {}", i.id, i.title, goal, now).to_lowercase();
        if words.iter().all(|w| hay.contains(w)) {
            out.push(if all {
                format!(
                    "{} · {} · {} · 现在：{}",
                    i.id,
                    i.title,
                    status_label(&i.status),
                    i.now
                )
            } else {
                format!("{} · {} · {}", i.id, i.title, status_label(&i.status))
            });
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

fn session_label(s: &str) -> &str {
    match s {
        "running" => "运行中",
        "starting" => "启动中",
        "idle" => "空闲",
        "error" => "出错",
        "ended" => "已结束",
        o => o,
    }
}

/// `steve_sessions_list`: the workspace's sessions by status, then up to 40 of them, newest first
/// (ended ones only with `all`).
pub fn sessions_list(store: &Store, ws: &str, all: bool) -> Result<String, String> {
    let list = store.sessions().list(Some(ws)).map_err(|e| e.to_string())?;
    let mut counts: Vec<(String, usize)> = vec![];
    for s in &list {
        match counts.iter_mut().find(|(k, _)| *k == s.status) {
            Some((_, n)) => *n += 1,
            None => counts.push((s.status.clone(), 1)),
        }
    }
    let busy = list
        .iter()
        .filter(|s| matches!(s.status.as_str(), "running" | "starting"))
        .count();
    let mut out = format!(
        "共 {} 个会话，正在运行 {busy} 个（{}）。\n",
        list.len(),
        counts
            .iter()
            .map(|(k, n)| format!("{} {n}", session_label(k)))
            .collect::<Vec<_>>()
            .join("，")
    );
    let shown: Vec<_> = list
        .iter()
        .filter(|s| all || s.status != "ended")
        .take(40)
        .collect();
    if shown.is_empty() {
        out.push_str("没有未结束的会话。\n");
    }
    for s in shown {
        out.push_str(&format!(
            "{} · {} · {} · {} · bot {} · 事项 {} · {}{}\n",
            s.id,
            s.harness,
            s.model.as_deref().unwrap_or("默认模型"),
            session_label(&s.status),
            s.bot_id.as_deref().unwrap_or("-"),
            s.item_id.as_deref().unwrap_or("-"),
            s.title,
            if s.source.is_empty() || s.source == "steve" {
                String::new()
            } else {
                format!(" · 来源 {}", s.source)
            }
        ));
    }
    Ok(out)
}

/// Only the owner reads the answer to message `origin`: Steve's own conversation, or a chat the channel
/// marked `owner_only` that still is one now (its members are read again, not taken from the mark).
fn own_reader(store: &Store, origin: &str) -> Result<bool, String> {
    let Some(m) = store.messages().get(origin).map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    if m.space == steve_core::thread::MAIN {
        return Ok(true);
    }
    let Some(space_id) = m.space.strip_prefix("chan:") else {
        return Ok(false);
    };
    if m.meta["chan"]["owner_only"] != json!(true) {
        return Ok(false);
    }
    let conn = m.meta["chan"]["connection"].as_str().unwrap_or_default();
    store
        .reader(|c| -> steve_channels::ChannelResult<bool> {
            match steve_channels::spaces::get(c, space_id)? {
                Some(space) => {
                    steve_channels::conversations::owner_only(c, &m.workspace_id, &space, conn)
                }
                None => Ok(false),
            }
        })
        .map_err(|e| e.to_string())
}

/// `steve_item_read`: one item of this workspace (in a conversation scope: one of its own items, title,
/// status and goal only; any other reads as missing).
pub fn item_read(store: &Store, ws: &str, scope: &ItemScope, id: &str) -> Result<String, String> {
    let Some(i) = store
        .items()
        .get(id.trim())
        .map_err(|e| e.to_string())?
        .filter(|i| i.workspace_id == ws && scope.sees(&i.id))
    else {
        return Err(format!("No item {id} in this workspace."));
    };
    let goal = store.items().goal(&i.id).unwrap_or_default();
    if !matches!(scope, ItemScope::All) {
        return Ok(format!(
            "{} {}\n状态：{}\n目标：{goal}\n",
            i.id,
            i.title,
            status_label(&i.status)
        ));
    }
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

/// Runs research tool `name` for thread run `run` (`owner`: the workspace owner, whose view the
/// owner's own conversation reads with): the text the harness reads, or its error.
async fn run_tool(
    x: &Extra,
    run: ThreadRun,
    name: &str,
    args: &Value,
    owner: String,
) -> Result<String, String> {
    let ws = run.workspace_id.clone();
    match name {
        steve_knowledge::tools::SEARCH | steve_knowledge::tools::READ => {
            // a user's view only when only the owner reads the answer; with anyone else
            // listening, what the whole workspace may see (no one's private knowledge)
            let (store, origin) = (x.store.clone(), run.message_id.clone());
            let own = tokio::task::spawn_blocking(move || own_reader(&store, &origin))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let user = match own {
                Ok(true) => run.user_id.clone().unwrap_or(owner),
                Ok(false) => String::new(),
                Err(e) => return Err(e),
            };
            let cx = steve_modules::contacts::mcp::McpCx {
                ws,
                user,
                bot_id: "steve".into(),
                item_id: None,
                session_id: None,
            };
            steve_knowledge::tools::call(&x.modules, &x.kb, &cx, name, args).await
        }
        ITEMS_SEARCH => {
            let (store, q) = (
                x.store.clone(),
                args["query"].as_str().unwrap_or("").to_string(),
            );
            let origin = run.message_id.clone();
            tokio::task::spawn_blocking(move || {
                items_search(&store, &ws, &ItemScope::of(&store, &origin)?, &q)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
        ITEM_READ => {
            let (store, i) = (
                x.store.clone(),
                args["id"].as_str().unwrap_or("").to_string(),
            );
            let origin = run.message_id.clone();
            tokio::task::spawn_blocking(move || {
                item_read(&store, &ws, &ItemScope::of(&store, &origin)?, &i)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
        SESSIONS_LIST => {
            let (store, all) = (x.store.clone(), args["all"].as_bool().unwrap_or(false));
            let origin = run.message_id.clone();
            tokio::task::spawn_blocking(move || {
                // your agents' sessions are yours: not for a run answering someone else
                if !own_reader(&store, &origin)? {
                    return Err(REFUSED.to_string());
                }
                sessions_list(&store, &ws, all)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
        _ => Err(REFUSED.into()),
    }
}

/// The research tools as a Node's catalog (a research job's grant): the very set `/mcp` lists for
/// an in-process run's token.
pub fn descriptors() -> Vec<steve_node_api::ToolDescriptor> {
    tools()
        .into_iter()
        .map(|t| steve_node_api::ToolDescriptor {
            name: t["name"].as_str().unwrap_or_default().to_string(),
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: t["inputSchema"].clone(),
            read_only: true,
        })
        .collect()
}

/// A research job's tool call through its Node, answered as `/mcp` answers an in-process run.
pub async fn call_tool(
    x: &Extra,
    run: ThreadRun,
    name: &str,
    args: Value,
) -> Result<String, String> {
    let (store, ws) = (x.store.clone(), run.workspace_id.clone());
    let owner = tokio::task::spawn_blocking(move || super::mcp_extra::owner(&store, &ws))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    run_tool(x, run, name, &args, owner).await
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
            let out = run_tool(x, run, &name, &args, owner).await;
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

//! Steve as an A2A agent: its Agent Card at the standard well-known path, and the JSON-RPC
//! endpoint that card names.
//!
//! - `GET /.well-known/agent-card.json` (and the older `/.well-known/agent.json`): the card, no
//!   sign-in needed. Its `url` is `<the Hub's address>/a2a`, the address as configured
//!   (`STEVE_PUBLIC_URL`, else the local network's, else the one the request came by).
//! - `POST /a2a` with a user's access token (`Authorization: Bearer stt_…`), JSON-RPC 2.0:
//!   - `message/send`: a new task becomes an item the user asked for (Steve chooses its Bot and
//!     title as for a message); a message with the `taskId` of one of the user's items adds to it.
//!     Answered at once with the task (`submitted` / `working`); its id is the item's.
//!   - `tasks/get {id}`: where the item is, with what Steve reads of it (status, plan, timeline, the
//!     last delivery) as the status message, and the last delivery as an artifact once it is done.
//!   - `tasks/cancel {id}`: drops the item.
//!   Streaming and push notifications are not offered (the card says so).
//!
//! An item's state reads as A2A's: before its first session `submitted`, then `working`; a question,
//! approval or acceptance open for the user → `input-required`; done → `completed`, dropped →
//! `canceled`, failed → `failed`.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::api::AppState;
use crate::backend::CreateItem;

pub const PATH: &str = "/a2a";
pub const CARD_PATHS: &[&str] = &["/.well-known/agent-card.json", "/.well-known/agent.json"];

pub fn routes(st: AppState) -> Router {
    let mut r = Router::new();
    for p in CARD_PATHS {
        r = r.route(p, get(card_h));
    }
    r.route(PATH, post(rpc_h)).with_state(st)
}

/// The Hub's address another agent reaches it by: the configured public one, else one on the
/// local network, else the one this request came by.
pub fn base_url(cfg: &crate::config::Config, headers: &HeaderMap) -> String {
    let ep = crate::access::pair_endpoints(cfg, headers);
    if let Some(u) = ep.public_url.or_else(|| ep.lan_urls.first().cloned()) {
        return u;
    }
    // this machine: the Hub's own port (a page's dev server in front of it does not serve /a2a)
    let local = format!("http://127.0.0.1:{}", cfg.listen.port);
    match headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        Some(h) => {
            let name = h
                .rsplit_once(':')
                .map_or(h, |(n, _)| n)
                .trim_matches(['[', ']']);
            let loopback = name == "localhost"
                || name
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            if loopback {
                local
            } else {
                format!("http://{h}")
            }
        }
        None => local,
    }
}

/// Steve's own Agent Card.
pub fn card(base: &str, version: &str) -> Value {
    json!({
        "protocolVersion": "0.3.0",
        "name": "Steve",
        "description": "Steve, a personal AI work hub: hand it a piece of work and its Bots (coding and other agents) do it; follow the task and add to it until it is done.",
        "url": format!("{base}{PATH}"),
        "preferredTransport": "JSONRPC",
        "version": version,
        "capabilities": {"streaming": false, "pushNotifications": false, "stateTransitionHistory": false},
        "securitySchemes": {"steve": {"type": "http", "scheme": "bearer", "description": "A Steve user access token (stt_…), made in Steve under 能力 → MCP or Settings · Access."}},
        "security": [{"steve": []}],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [
            {"id": "work", "name": "Hand Steve a piece of work",
             "description": "A self-contained request becomes a task Steve tracks; Steve chooses the Bot that does it. Read the task for its progress and result.",
             "tags": ["delegation", "tasks"], "examples": ["Fix the failing login test in the web client", "Write a one-page summary of last week's incidents"]},
            {"id": "follow", "name": "Follow and add to a task",
             "description": "Ask about a task by its id for its state and result, or send a message with its taskId to add detail or answer what it asked.",
             "tags": ["tasks"]}
        ]
    })
}

async fn card_h(State(st): State<AppState>, headers: HeaderMap) -> Json<Value> {
    Json(card(&base_url(&st.cfg, &headers), &st.cfg.version))
}

fn rpc_ok(id: &Value, result: Value) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn rpc_err(id: &Value, code: i64, message: impl Into<String>) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}}))
        .into_response()
}

/// The A2A state of an item: waiting for the user is `input-required` only while a question,
/// approval or acceptance is open for it; before its first session it is `submitted`.
fn state_of(i: &steve_store::Item, open_decision: bool, started: bool) -> &'static str {
    match (i.status.as_str(), i.outcome.as_deref()) {
        ("done", _) => "completed",
        ("drop", _) => "canceled",
        (_, Some("failed")) => "failed",
        _ if open_decision => "input-required",
        _ if !started => "submitted",
        _ => "working",
    }
}

fn task_of(store: &steve_store::Store, ws: &str, id: &str) -> Result<Value, String> {
    let i = store
        .items()
        .get(id)
        .map_err(|e| e.to_string())?
        .filter(|i| i.workspace_id == ws)
        .ok_or_else(|| format!("no task {id}"))?;
    let read = crate::wire::research_mcp::item_read(
        store,
        ws,
        &crate::wire::research_mcp::ItemScope::All,
        &i.id,
    )
    .unwrap_or_default();
    let decisions = store
        .decisions()
        .of_item(&i.id)
        .map_err(|e| e.to_string())?;
    let open = decisions.iter().any(|d| d.status == "open");
    let started = !store
        .sessions()
        .of_item(&i.id)
        .map_err(|e| e.to_string())?
        .is_empty();
    let state = state_of(&i, open, started);
    let mut task = json!({
        "kind": "task", "id": i.id, "contextId": i.id,
        "status": {"state": state, "timestamp": ms_iso(i.updated),
            "message": {"kind": "message", "role": "agent", "messageId": format!("{}-{}", i.id, i.updated),
                "parts": [{"kind": "text", "text": read}]}},
        "metadata": {"title": i.title},
    });
    if state == "completed" {
        let delivered = decisions
            .into_iter()
            .filter(|d| d.kind == "accept" && !d.draft.trim().is_empty())
            .max_by_key(|d| d.created);
        if let Some(d) = delivered {
            task["artifacts"] = json!([{"artifactId": d.id, "name": "delivery", "parts": [{"kind": "text", "text": d.draft}]}]);
        }
    }
    Ok(task)
}

fn ms_iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let d = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs.max(0) as u64);
    rfc3339(d)
}

/// RFC 3339 in UTC without a date library.
fn rfc3339(t: std::time::SystemTime) -> String {
    let s = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

async fn rpc_h(State(st): State<AppState>, req: Request) -> Response {
    let Some(store) = st.modules.as_ref().map(|m| m.store.clone()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let headers = req.headers().clone();
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string();
    let ws_hint = headers
        .get("x-steve-ws")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let s2 = store.clone();
    let who = tokio::task::spawn_blocking(move || {
        crate::wire::outside_mcp::identify(&s2, &token, ws_hint.as_deref())
    })
    .await
    .ok()
    .flatten();
    let Some(who) = who else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({"error": "a Steve access token is needed"})),
        )
            .into_response();
    };
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(msg) = serde_json::from_slice::<Value>(&bytes) else {
        return rpc_err(&Value::Null, -32700, "Parse error");
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match msg["method"].as_str().unwrap_or_default() {
        "message/send" => {
            let m = &params["message"];
            let text: Vec<String> = m["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| match p["kind"].as_str() {
                    Some("text") => p["text"].as_str().map(str::to_string),
                    Some("data") => Some(p["data"].to_string()),
                    Some("file") => p
                        .pointer("/file/uri")
                        .and_then(Value::as_str)
                        .map(|u| format!("[file]({u})")),
                    _ => None,
                })
                .collect();
            let text = text.join("\n").trim().to_string();
            if text.is_empty() {
                return rpc_err(&id, -32602, "the message has no text");
            }
            if let Some(task) = m["taskId"].as_str().filter(|s| !s.is_empty()) {
                let (s3, ws, t) = (store.clone(), who.ws.clone(), task.to_string());
                let known = tokio::task::spawn_blocking(move || {
                    s3.items()
                        .get(&t)
                        .ok()
                        .flatten()
                        .is_some_and(|i| i.workspace_id == ws)
                })
                .await
                .unwrap_or(false);
                if !known {
                    return rpc_err(&id, -32001, format!("Task not found: {task}"));
                }
                if let Err(e) = st.domain.supplement(task, &text, "auto", vec![]).await {
                    return rpc_err(&id, -32603, e.message);
                }
                return match task_of(&store, &who.ws, task) {
                    Ok(t) => rpc_ok(&id, t),
                    Err(e) => rpc_err(&id, -32603, e),
                };
            }
            let r = match st.domain.route(&who.ws, &text).await {
                Ok(r) => r,
                Err(e) => return rpc_err(&id, -32603, e.message),
            };
            let title = if r.title.trim().is_empty() {
                text.chars().take(24).collect()
            } else {
                r.title.clone()
            };
            let made = st
                .domain
                .create_item(CreateItem {
                    ws: who.ws.clone(),
                    goal: text,
                    title,
                    bot: Some(r.bot.clone()),
                    project: Some(r.project.clone()),
                    tag_ids: r.tag_ids.clone(),
                    source: [format!("A2A · {}", who.token), "a2a".into()],
                    mode: "start".into(),
                    actor: who.user.id.clone(),
                    opening: None,
                })
                .await;
            let item = match made {
                Ok(i) => i,
                Err(e) => return rpc_err(&id, -32603, e.message),
            };
            let iid = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match task_of(&store, &who.ws, &iid) {
                Ok(t) => rpc_ok(&id, t),
                Err(e) => rpc_err(&id, -32603, e),
            }
        }
        "tasks/get" => {
            let task = params["id"].as_str().unwrap_or_default().to_string();
            let (s3, ws) = (store.clone(), who.ws.clone());
            match tokio::task::spawn_blocking(move || task_of(&s3, &ws, &task)).await {
                Ok(Ok(t)) => rpc_ok(&id, t),
                Ok(Err(e)) => rpc_err(&id, -32001, format!("Task not found: {e}")),
                Err(e) => rpc_err(&id, -32603, e.to_string()),
            }
        }
        "tasks/cancel" => {
            let task = params["id"].as_str().unwrap_or_default().to_string();
            if task_of(&store, &who.ws, &task).is_err() {
                return rpc_err(&id, -32001, format!("Task not found: {task}"));
            }
            if let Err(e) = st.domain.item_action(&task, "drop", &who.user.id).await {
                return rpc_err(&id, -32002, e.message);
            }
            match task_of(&store, &who.ws, &task) {
                Ok(t) => rpc_ok(&id, t),
                Err(e) => rpc_err(&id, -32603, e),
            }
        }
        "message/stream" | "tasks/resubscribe" => rpc_err(
            &id,
            -32004,
            "Streaming is not supported: use message/send and tasks/get",
        ),
        m if m.starts_with("tasks/pushNotificationConfig") => {
            rpc_err(&id, -32003, "Push notifications are not supported")
        }
        m => rpc_err(&id, -32601, format!("Method not found: {m}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_names_its_endpoint_and_how_to_sign_in() {
        let c = card("https://steve.example", "1.2.3");
        assert_eq!(c["url"], "https://steve.example/a2a");
        assert_eq!(c["securitySchemes"]["steve"]["scheme"], "bearer");
        assert_eq!(c["capabilities"]["streaming"], false);
        assert_eq!(c["skills"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_time_is_rfc3339() {
        assert_eq!(ms_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(ms_iso(1_763_000_000_000), "2025-11-13T02:13:20Z");
    }
}

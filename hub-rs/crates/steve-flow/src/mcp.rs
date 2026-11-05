//! MCP tool of the flow module: `steve_event_get`, the original email,
//! message or alert an item came from. Only events the session's item has a `from` link to
//! (`links(item, 'from', 'event', evt)`, written by `item.create`, `item.link` and “转成事项”)
//! can be read; the text goes through the vault's `guard::scan` first.
//!
//! Serving it is steve-mcp's (integrator), like the knowledge tools: list [`tools`] after the Steve
//! tools and route names for which [`handles`] is true to [`call`] with the session's [`McpCx`].
//! It does not end the turn.

use std::sync::LazyLock;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::{ApiError, ApiResult};

use crate::FlowCx;
use crate::nodes::bot::redact;
use crate::schedules::{local, workspace_tz};
use crate::signals::source_label;

pub const EVENT_GET: &str = "steve_event_get";
/// Characters of original text one answer carries at most.
pub const TEXT_MAX: usize = 20_000;
/// The answer once the original is past the retention period (verbatim).
pub const PURGED: &str =
    "The original was deleted after the retention period; only this summary is left: ";
pub const NO_ITEM: &str = "This session has no item, so there is no source event to read.";
pub const NO_EVENT: &str = "This item did not come from an email, message or alert.";
pub const NOT_LINKED: &str =
    "No such event for this item. Leave out `event` to read the item's source event.";

static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![json!({
        "name": EVENT_GET,
        "description": "Read the original email, message or alert this item came from.",
        "inputSchema": {
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "event": { "type": "string", "description": "the event id (evt_…); default: the item's source event" }
            }
        }
    })]
});

/// The tool definitions, `[{name, description, inputSchema}]`.
pub fn tools() -> &'static [Value] {
    &TOOLS
}

pub fn handles(name: &str) -> bool {
    name == EVENT_GET
}

/// Runs one tool. `Ok` is the result text, `Err` the `isError` text (steve-mcp's convention).
pub async fn call(
    cx: &std::sync::Arc<FlowCx>,
    mcp: &McpCx,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let (cx2, mcp, name, args) = (cx.clone(), mcp.clone(), name.to_string(), args.clone());
    cx.modules
        .blocking(move |_| Ok(call_sync(&cx2, &mcp, &name, &args)))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// [`call`] without the runtime hop.
pub fn call_sync(cx: &FlowCx, mcp: &McpCx, name: &str, args: &Value) -> Result<String, String> {
    if name != EVENT_GET {
        return Err(format!("MCP error -32602: Tool {name} not found"));
    }
    let event = match args.get("event") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.trim()).filter(|s| !s.is_empty()),
        Some(_) => {
            return Err(format!(
                "MCP error -32602: Input validation error: Invalid arguments for tool {EVENT_GET}: Invalid input: expected string at event"
            ));
        }
    };
    let r = event_get(cx, mcp, event).map_err(|e| e.to_string());
    if let Err(e) = &r {
        tracing::warn!(tool = name, error = %e, "flow tool failed");
    }
    r
}

/// The events the item came from, oldest link first.
fn linked_events(c: &Connection, ws: &str, item: &str) -> ApiResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT l.object_id FROM links l JOIN ext_events e ON e.id = l.object_id AND e.workspace_id = l.workspace_id
          WHERE l.workspace_id = ? AND l.subject_type = 'item' AND l.subject_id = ? AND l.predicate = 'from' AND l.object_type = 'event'
          ORDER BY l.created, l.rowid",
    )?;
    let rows = st
        .query_map(params![ws, item], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

struct Ev {
    id: String,
    source: String,
    type_key: String,
    occurred: i64,
    summary: String,
    payload: Option<String>,
    purged: bool,
    party: Option<(String, String)>,
}

fn load(c: &Connection, ws: &str, id: &str) -> ApiResult<Option<Ev>> {
    Ok(c.query_row(
        "SELECT e.id, e.source, e.type, e.occurred, e.summary, e.payload, e.payload_purged, p.name, p.rels
           FROM ext_events e LEFT JOIN parties p ON p.id = e.actor_party_id
          WHERE e.id = ? AND e.workspace_id = ?",
        params![id, ws],
        |r| {
            let name: Option<String> = r.get(7)?;
            let rels: Option<String> = r.get(8)?;
            Ok(Ev {
                id: r.get(0)?,
                source: r.get(1)?,
                type_key: r.get(2)?,
                occurred: r.get(3)?,
                summary: r.get(4)?,
                payload: r.get(5)?,
                purged: r.get::<_, Option<i64>>(6)?.is_some(),
                party: name.map(|n| (n, rels.unwrap_or_default())),
            })
        },
    )
    .optional()?)
}

/// The text of a payload: its `text` (mail bodies, messages), else the JSON (alerts).
fn payload_text(p: &str) -> String {
    let v: Value = serde_json::from_str(p).unwrap_or(Value::String(p.into()));
    match v["text"].as_str() {
        Some(t) => t.to_string(),
        None => match &v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            v => serde_json::to_string_pretty(v).unwrap_or_default(),
        },
    }
}

fn cut(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((i, _)) => (s[..i].to_string(), true),
        None => (s.to_string(), false),
    }
}

/// `steve_event_get`.
pub fn event_get(cx: &FlowCx, mcp: &McpCx, event: Option<&str>) -> ApiResult<String> {
    let item = mcp
        .item_id
        .as_deref()
        .ok_or_else(|| ApiError::rule(NO_ITEM))?;
    let (ev, tz) = cx.store().reader(|c| -> ApiResult<_> {
        let linked = linked_events(c, &mcp.ws, item)?;
        let id = match event {
            Some(e) if linked.iter().any(|l| l == e) => e.to_string(),
            Some(_) => return Err(ApiError::rule(NOT_LINKED)),
            None => linked
                .first()
                .cloned()
                .ok_or_else(|| ApiError::rule(NO_EVENT))?,
        };
        let ev = load(c, &mcp.ws, &id)?.ok_or_else(|| ApiError::rule(NOT_LINKED))?;
        Ok((ev, workspace_tz(c, &mcp.ws)?))
    })?;
    let guard = cx.guard.as_ref();
    let summary = redact(guard, &ev.summary);
    if ev.purged {
        return Ok(format!("{PURGED}{summary}"));
    }
    let zone = crate::cron::parse_tz(&tz).unwrap_or(chrono_tz::Asia::Taipei);
    let mut out = format!(
        "Event {} · {} ({}) · {}\n",
        ev.id,
        source_label(&ev.source),
        ev.source,
        ev.type_key
    );
    if let Some((name, rels)) = &ev.party {
        let rels: Vec<String> = serde_json::from_str(rels).unwrap_or_default();
        if rels.is_empty() {
            out.push_str(&format!("From: {name}\n"));
        } else {
            out.push_str(&format!(
                "From: {name} (relationship: {})\n",
                rels.join(", ")
            ));
        }
    }
    out.push_str(&format!(
        "Time: {} ({tz})\nSummary: {summary}\n",
        local(ev.occurred, zone, "%Y-%m-%d %H:%M")
    ));
    let text = ev.payload.as_deref().map(payload_text).unwrap_or_default();
    if text.trim().is_empty() {
        out.push_str("\n(no original text)");
    } else {
        let (text, more) = cut(&redact(guard, &text), TEXT_MAX);
        out.push_str("\nOriginal:\n");
        out.push_str(&text);
        if more {
            out.push_str("\n…(cut at 20000 characters)");
        }
    }
    Ok(out)
}

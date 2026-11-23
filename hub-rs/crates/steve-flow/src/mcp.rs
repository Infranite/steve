//! MCP tool of the flow module: `steve_event_get`, the original email,
//! message or alert an item came from. Only events the session's item has a `from` link to
//! (`links(item, 'from', 'event', evt)`, written by `item.create`, `item.link` and “转成事项”)
//! can be read.
//!
//! Serving it is steve-mcp's (integrator), like the knowledge tools: list [`tools`] after the Steve
//! tools and route names for which [`handles`] is true to [`call`] with the session's [`McpCx`].
//! It does not end the turn.

use std::sync::LazyLock;

use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::zone::Zone;
use steve_modules::{ApiError, ApiResult};
use steve_store::base::{ObjRef, links::Links};
use steve_store::{Contacts, SignalRows};

use crate::FlowCx;
use crate::schedules::local;
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

/// The text of a payload: its `text` (mail bodies, messages), else the JSON (alerts).
fn payload_text(v: &Value) -> String {
    match v["text"].as_str() {
        Some(t) => t.to_string(),
        None => match v {
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
    let (ev, party, tz) = cx.store().read::<_, steve_store::StoreError>(|r| {
        let links = Links(r).destinations(&mcp.ws, &ObjRef::new("item", item), "from", "event")?;
        let mut signals = SignalRows(r);
        let id = if let Some(id) = event {
            links.iter().any(|linked| linked == id).then_some(id)
        } else {
            let existing = signals.existing_ids(&mcp.ws, links.iter().map(String::as_str))?;
            links
                .iter()
                .find(|id| existing.contains(*id))
                .map(String::as_str)
        };
        let Some(id) = id else {
            return Ok((None, None, None));
        };
        let ev = signals.source(&mcp.ws, id)?;
        let party = match ev.as_ref().and_then(|ev| ev.actor.as_deref()) {
            Some(actor) => Contacts::new(r).name_and_relationships(&mcp.ws, actor)?,
            None => None,
        };
        let tz = Zone::for_user(r, &mcp.ws, None)?.name();
        Ok((ev, party, Some(tz)))
    })?;
    let (Some(ev), Some(tz)) = (ev, tz) else {
        return Err(ApiError::rule(if event.is_some() {
            NOT_LINKED
        } else {
            NO_EVENT
        }));
    };
    let summary = &ev.summary;
    if ev.purged {
        return Ok(format!("{PURGED}{summary}"));
    }
    let zone = crate::cron::zone(&tz).unwrap_or_else(|_| steve_runtime::tz::Zone::system());
    let mut out = format!(
        "Event {} · {} ({}) · {}\n",
        ev.id,
        source_label(&ev.source),
        ev.source,
        ev.type_key
    );
    if let Some((name, rels)) = party {
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
    let text = ev.payload.as_ref().map(payload_text).unwrap_or_default();
    if text.trim().is_empty() {
        out.push_str("\n(no original text)");
    } else {
        let (text, more) = cut(&text, TEXT_MAX);
        out.push_str("\nOriginal:\n");
        out.push_str(&text);
        if more {
            out.push_str("\n…(cut at 20000 characters)");
        }
    }
    Ok(out)
}

//! MCP tools of the knowledge module for agents in a session (knowledge.md §7, slice K4):
//! `steve_knowledge_search` and `steve_knowledge_read`. Results are filtered for the session's
//! workspace and the user its item was raised by ([`crate::context::item_viewer`], the same viewer as
//! the opening prompt); `never` is never returned. (`steve_remember` belongs to K3.)
//!
//! Serving them is steve-mcp's (integrator), like the contacts tools: list [`tools`] after the Steve
//! tools and route names for which [`handles`] is true to [`call`] with the session's [`McpCx`].
//! Neither tool ends the turn.

use std::sync::LazyLock;

use rusqlite::Connection;
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::{ApiError, ApiResult, Modules};
use steve_store::base::{Level, Viewer};

use crate::context::{item_viewer, scrub_query};
use crate::retrieve::{Hit, Purpose, Query, record_use, retrieve};
use crate::service::own_doc;
use crate::state::Knowledge;
use crate::uses::last_n;

pub const SEARCH: &str = "steve_knowledge_search";
pub const READ: &str = "steve_knowledge_read";

/// Hits of one search: default and maximum.
pub const SEARCH_DEFAULT: usize = 5;
pub const SEARCH_MAX: usize = 10;
/// Characters of one `steve_knowledge_read` answer.
pub const READ_MAX: usize = 20_000;
/// The last line of every search answer (§7, verbatim).
pub const LEVELS: &str = "Levels: public = can be quoted; conclusion = state conclusions only; reasoning = use for reasoning, never put it in anything sent to others.";

static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![
        json!({
            "name": SEARCH,
            "description": "Search the user's knowledge base: notes, documents, wiki pages, past deliverables and confirmed memories. Each result has a disclosure level; follow it.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "number", "description": "at most 10 (default 5)" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": READ,
            "description": "Read more of a document found by steve_knowledge_search.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "the document id (kd_…)" },
                    "from": { "type": "number", "description": "chunk number to start at" }
                },
                "required": ["id"]
            }
        }),
    ]
});

/// The tool definitions, `[{name, description, inputSchema}]`, in listing order.
pub fn tools() -> &'static [Value] {
    &TOOLS
}

/// Is `name` one of this module's tools?
pub fn handles(name: &str) -> bool {
    name == SEARCH || name == READ
}

/// Runs one tool. `Ok` is the result text, `Err` the `isError` text (steve-mcp's convention).
pub async fn call(
    m: &Modules,
    kb: &Knowledge,
    cx: &McpCx,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let (kb, cx, name, args) = (kb.clone(), cx.clone(), name.to_string(), args.clone());
    m.blocking(move |m| Ok(call_sync(m, &kb, &cx, &name, &args)))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// [`call`] without the runtime hop (tests, or callers already off the runtime).
pub fn call_sync(
    m: &Modules,
    kb: &Knowledge,
    cx: &McpCx,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let r = match name {
        SEARCH => validate(
            name,
            args,
            &[("query", true, "string"), ("limit", false, "number")],
        )
        .and_then(|_| {
            let limit = args.get("limit").and_then(Value::as_f64);
            search(m, kb, cx, str_arg(args, "query"), limit).map_err(|e| e.to_string())
        }),
        READ => validate(
            name,
            args,
            &[("id", true, "string"), ("from", false, "number")],
        )
        .and_then(|_| {
            let from = args.get("from").and_then(Value::as_f64).unwrap_or(0.0);
            read(m, cx, str_arg(args, "id"), from).map_err(|e| e.to_string())
        }),
        _ => return Err(format!("MCP error -32602: Tool {name} not found")),
    };
    if let Err(e) = &r {
        tracing::warn!(tool = name, error = %e, "knowledge tool failed");
    }
    r
}

fn str_arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or_default()
}

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

/// The argument check in the wording steve-mcp (zod v4) uses, so agents can fix their call.
fn validate(tool: &str, args: &Value, fields: &[(&str, bool, &str)]) -> Result<(), String> {
    let mut issues = Vec::new();
    for (k, required, ty) in fields {
        let v = args.get(*k);
        let ok = match (v, *ty) {
            (None, _) => !required,
            (Some(Value::String(_)), "string") => true,
            (Some(Value::Number(_)), "number") => true,
            _ => false,
        };
        if !ok {
            issues.push(format!(
                "Invalid input: expected {ty}, received {} at {k}",
                received(v)
            ));
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {tool}: {}",
            issues.join("\n")
        ))
    }
}

/// The session's viewer: the user of its item (as for the opening prompt); without an item the user
/// steve-mcp resolved; without either, only what the whole workspace may see.
pub fn session_viewer(m: &Modules, cx: &McpCx) -> ApiResult<Viewer> {
    m.store.reader(|c| match cx.item_id.as_deref() {
        Some(item) => item_viewer(c, &cx.ws, item),
        None if !cx.user.is_empty() => Ok(Viewer::load(c, &cx.ws, &cx.user)?),
        None => Ok(Viewer::bare(&cx.ws, "")),
    })
}

/// One search result (§7): `[K<n>] <title> · <label> · level: <code>`, the document id when there
/// is more to read, then the text.
fn entry(h: &Hit) -> String {
    let id = if h.obj_type == "chunk" {
        format!(" · id: {}", h.obj_id)
    } else {
        String::new()
    };
    format!(
        "[K{}] {} · {} · level: {}{id}\n{}",
        h.n,
        h.title,
        h.label,
        h.level.code(),
        h.text
    )
}

/// `steve_knowledge_search`: `retrieve(query, k=limit, Agent)`, numbered after what the session was
/// already given, recorded in `kb_uses(session)`.
pub fn search(
    m: &Modules,
    kb: &Knowledge,
    cx: &McpCx,
    query: &str,
    limit: Option<f64>,
) -> ApiResult<String> {
    let q = query.trim();
    if q.is_empty() {
        return Err(ApiError::rule("Give a search query."));
    }
    if !kb.guard.scan(q).is_empty() {
        return Err(ApiError::rule(
            "The query looks like it contains a password or token. Search without it.",
        ));
    }
    let k = limit.map_or(SEARCH_DEFAULT, |l| (l.max(1.0) as usize).min(SEARCH_MAX));
    let viewer = session_viewer(m, cx)?;
    let text = scrub_query(kb.guard.as_ref(), q);
    let now = m.clock.now();
    let hits = m.store.reader(|c| {
        let mut query = Query::new(kb.store(), &viewer, &text, Purpose::Agent);
        query.k = k;
        query.now = now;
        let mut hits = retrieve(c, &query)?;
        let base = match cx.session_id.as_deref() {
            Some(sid) => last_n(c, &cx.ws, sid)?,
            None => 0,
        };
        for h in &mut hits {
            h.n += base;
        }
        Ok::<_, ApiError>(hits)
    })?;
    if hits.is_empty() {
        return Ok(format!("No knowledge matches \"{q}\"."));
    }
    if let Some(sid) = cx.session_id.as_deref() {
        m.store
            .transaction(|tx| record_use(tx, &cx.ws, ("session", sid), &text, &hits))?;
    }
    let mut blocks: Vec<String> = hits.iter().map(entry).collect();
    blocks.push(LEVELS.into());
    Ok(blocks.join("\n\n"))
}

type Piece = (i64, String, Option<i64>, Option<i64>);

fn pieces(c: &Connection, doc: &str, from: i64) -> ApiResult<Vec<Piece>> {
    let mut st = c.prepare_cached(
        "SELECT seq, text, start_at, end_at FROM kb_chunks
          WHERE obj_type = 'chunk' AND obj_id = ? AND seq >= ? AND level <> 'never' ORDER BY seq",
    )?;
    let rows = st.query_map(rusqlite::params![doc, from], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `steve_knowledge_read`: the chunks from `from` on, joined without their overlaps, ≤20000
/// characters, then `Continue with from=<n>.` or `End of document.`. Documents the session's user
/// cannot see, or at level `never`, do not exist for it.
pub fn read(m: &Modules, cx: &McpCx, id: &str, from: f64) -> ApiResult<String> {
    let viewer = session_viewer(m, cx)?;
    let missing = || ApiError::rule(format!("No document {id}."));
    let from = from.max(0.0) as i64;
    m.store.reader(|c| {
        let (d, s) = own_doc(c, &viewer, id, "文档").map_err(|_| missing())?;
        let level = d.eff_level(&s);
        if level == Level::Never {
            return Err(missing());
        }
        let mut body = String::new();
        let mut len = 0usize;
        let mut prev_end: Option<i64> = None;
        let mut next: Option<i64> = None;
        for (seq, text, start, end) in pieces(c, &d.id, from)? {
            let skip = match (prev_end, start) {
                (Some(p), Some(s)) if s < p => (p - s) as usize,
                _ => 0,
            };
            let part: String = text.chars().skip(skip).collect();
            let n = part.chars().count();
            if len > 0 && len + n > READ_MAX {
                next = Some(seq);
                break;
            }
            body.push_str(&part);
            len += n;
            prev_end = end.or(prev_end);
        }
        let tail = match next {
            Some(n) => format!("Continue with from={n}."),
            None => "End of document.".into(),
        };
        Ok(format!(
            "{} · level: {}\n\n{}\n\n{tail}",
            d.title,
            level.code(),
            body.trim_end()
        ))
    })
}

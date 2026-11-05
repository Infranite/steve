//! MCP tools of the contacts module for agents in a session:
//! `steve_contacts_find` and `steve_contacts_remember`. Everything is filtered for the session's
//! workspace and the user the agent works for; `never` attributes and memories are never returned.
//!
//! This file has the tool definitions ([`tools`], the same `{name, description, inputSchema}` shape as
//! steve-mcp's `tools.json`) and the handlers ([`call`]); serving them is steve-mcp's (integrator): list
//! them after the five Steve tools and route their names to [`call`] with the session's [`McpCx`].
//! Neither tool ends the turn.

use std::sync::LazyLock;

use rusqlite::Connection;
use serde_json::{Value, json};
use steve_store::base::mems::{self, MemFilter};
use steve_store::base::{LEVELS_NOTE, Level, ObjRef, Viewer};

use super::Ports;
use super::memory::{self, Proposal};
use super::people_repo as prepo;
use super::repo::{self, ContactQuery, PARTY, Party};
use super::service::jobs;
use crate::{ApiError, ApiResult, Caller, Modules};

pub const FIND: &str = "steve_contacts_find";
pub const REMEMBER: &str = "steve_contacts_remember";
/// Ownership: the account → party → 负责人 chain, a person's holdings, and a
/// 负责人 heard in chat as a proposal the user confirms.
pub const WHOIS: &str = "steve_whois";
pub const HOLDINGS: &str = "steve_person_holdings";
pub const STEWARD_PROPOSE: &str = "steve_steward_propose";

/// Matches returned by `steve_contacts_find`.
pub const FIND_LIMIT: usize = 5;
/// Memories per match.
const FIND_MEMS: i64 = 10;
/// The answer of `steve_contacts_remember` (verbatim).
pub const SAVED: &str = "Saved as a candidate; the user will confirm it.";

const KINDS: [&str; 3] = ["fact", "preference", "event"];

static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![
        json!({
            "name": FIND,
            "description": "Look up people, organizations and teams the user works with. Use it before writing to or about someone.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "a name, alias, email or organization" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": REMEMBER,
            "description": "Save something you learned about a person, organization or team. It becomes a candidate the user confirms.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "who": { "type": "string", "description": "id or exact name" },
                    "text": { "type": "string" },
                    "kind": { "type": "string", "enum": KINDS }
                },
                "required": ["who", "text", "kind"]
            }
        }),
        json!({
            "name": WHOIS,
            "description": "Who is behind an account handle, bot, device, persona, connection or group, and who is responsible for it (负责人). Use it for questions like \"谁负责 X\" or \"这个机器人是谁的\".",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "a handle (email, app id, login), a name, or an id" }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": HOLDINGS,
            "description": "Everything a person has: their own accounts, grouped by system, and the bots, mailboxes, devices, connections and personas they are responsible for.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "person": { "type": "string", "description": "id or exact name; \"我\" is the user" }
                },
                "required": ["person"]
            }
        }),
        json!({
            "name": STEWARD_PROPOSE,
            "description": "Record who is responsible for a bot, account, device, connection, persona or group when the conversation says so (\"这是张三的机器人\"). It becomes a proposal the user confirms.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "object": { "type": "string", "description": "the object's id, handle or exact name (as steve_whois finds it)" },
                    "person": { "type": "string", "description": "id or exact name of the responsible person" },
                    "role": { "type": "string", "enum": STEWARD_ROLES },
                    "basis": { "type": "string", "description": "what in the conversation says so" }
                },
                "required": ["object", "person"]
            }
        }),
    ]
});

const STEWARD_ROLES: [&str; 3] = ["owner", "maintainer", "user"];

/// The tool definitions, `[{name, description, inputSchema}]`, in listing order.
pub fn tools() -> &'static [Value] {
    &TOOLS
}

/// Is `name` one of this module's tools?
pub fn handles(name: &str) -> bool {
    name == FIND || name == REMEMBER || name == WHOIS || name == HOLDINGS || name == STEWARD_PROPOSE
}

/// The session a tool call comes from, resolved by steve-mcp from the bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCx {
    pub ws: String,
    /// The user the agent works for (the item's user): visibility is theirs, candidates are theirs.
    pub user: String,
    /// The session's bot (`writer` and `requested_by` of what it saves).
    pub bot_id: String,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
}

/// Runs one tool. `Ok` is the result text, `Err` the `isError` text (the same convention as steve-mcp's
/// own tools). Unknown names are `MCP error -32602: Tool <name> not found`.
pub async fn call(
    m: &Modules,
    ports: &Ports,
    cx: &McpCx,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let (ports, cx, name, args) = (ports.clone(), cx.clone(), name.to_string(), args.clone());
    m.blocking(move |m| Ok(call_sync(m, &ports, &cx, &name, &args)))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// [`call`] without the runtime hop (tests, or callers already off the runtime).
pub fn call_sync(
    m: &Modules,
    ports: &Ports,
    cx: &McpCx,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let r = match name {
        FIND => validate(name, args, &["query"], None)
            .and_then(|_| find(m, ports, cx, str_arg(args, "query")).map_err(|e| e.to_string())),
        REMEMBER => validate(name, args, &["who", "text", "kind"], Some(("kind", &KINDS)))
            .and_then(|_| {
                remember(
                    m,
                    cx,
                    str_arg(args, "who"),
                    str_arg(args, "text"),
                    str_arg(args, "kind"),
                )
            }),
        WHOIS => validate(name, args, &["query"], None).and_then(|_| {
            super::ownership_mcp::whois(m, cx, str_arg(args, "query")).map_err(|e| e.to_string())
        }),
        HOLDINGS => validate(name, args, &["person"], None)
            .and_then(|_| super::ownership_mcp::holdings(m, cx, str_arg(args, "person"))),
        STEWARD_PROPOSE => validate(name, args, &["object", "person"], None)
            .and_then(|_| match args.get("role") {
                None | Some(Value::Null) => Ok(()),
                Some(_) => validate(name, args, &["role"], Some(("role", &STEWARD_ROLES))),
            })
            .and_then(|_| {
                super::ownership_mcp::propose(
                    m,
                    cx,
                    str_arg(args, "object"),
                    str_arg(args, "person"),
                    args.get("role").and_then(Value::as_str).unwrap_or("owner"),
                    str_arg(args, "basis"),
                )
            }),
        _ => return Err(format!("MCP error -32602: Tool {name} not found")),
    };
    if let Err(e) = &r {
        tracing::warn!(tool = name, error = %e, "contacts tool failed");
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

/// The argument check with the wording steve-mcp (zod v4) uses, so agents can fix their call.
fn validate(
    tool: &str,
    args: &Value,
    strings: &[&str],
    choice: Option<(&str, &[&str])>,
) -> Result<(), String> {
    let mut issues = Vec::new();
    for k in strings {
        let v = args.get(*k);
        if !matches!(v, Some(Value::String(_))) {
            issues.push(format!(
                "Invalid input: expected string, received {} at {k}",
                received(v)
            ));
        } else if let Some((ck, allowed)) = choice.filter(|(ck, _)| ck == k)
            && !allowed.contains(&str_arg(args, ck))
        {
            let opts: Vec<String> = allowed.iter().map(|o| format!("\"{o}\"")).collect();
            issues.push(format!(
                "Invalid option: expected one of {} at {ck}",
                opts.join("|")
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

/// The viewer of the session's user, creating “我” and the own organization like any contacts call.
pub(crate) fn viewer(m: &Modules, cx: &McpCx) -> ApiResult<Viewer> {
    let who = Caller {
        ws: cx.ws.clone(),
        me: cx.user.clone(),
    };
    super::service::start(m, &who).map(|(v, _)| v)
}

// ---------------------------------------------------------------- steve_contacts_find

/// `steve_contacts_find`: up to 5 matches (exact names and aliases first), each a block of text, then
/// the fixed `Levels:` line.
pub fn find(m: &Modules, ports: &Ports, cx: &McpCx, query: &str) -> ApiResult<String> {
    let q = query.trim();
    if q.is_empty() {
        return Err(ApiError::rule("Give a name, alias, email or organization."));
    }
    let viewer = viewer(m, cx)?;
    m.store.reader(|c| {
        let found = matches(c, ports, &viewer, q)?;
        let mut blocks = Vec::new();
        for p in &found {
            blocks.push(block(c, ports, &viewer, p)?);
        }
        if blocks.is_empty() {
            blocks.push(format!("No people, organizations or teams match \"{q}\"."));
        }
        blocks.push(LEVELS_NOTE.to_string());
        Ok(blocks.join("\n\n"))
    })
}

pub(crate) fn names_lower(p: &Party) -> Vec<String> {
    std::iter::once(&p.name)
        .chain(p.aliases.iter())
        .map(|n| n.trim().to_lowercase())
        .collect()
}

fn matches(c: &Connection, ports: &Ports, viewer: &Viewer, q: &str) -> ApiResult<Vec<Party>> {
    let ql = q.to_lowercase();
    let handle_ids = match &ports.handles {
        Some(h) => h.parties(c, &viewer.ws, q)?,
        None => Vec::new(),
    };
    let mut found: Vec<Party> = repo::contacts(
        c,
        viewer,
        &ContactQuery {
            q: Some(q.to_string()),
            handle_ids,
            limit: 50,
            ..Default::default()
        },
    )?;
    // an email's domain names its organization
    let domain = ql.rsplit_once('@').map(|(_, d)| d.to_string());
    for p in prepo::parties(c, viewer, &["org", "team"], true)? {
        let by_name = names_lower(&p).iter().any(|n| n.contains(&ql));
        let by_domain = p.kind == "org"
            && repo::domains(c, &viewer.ws, &p.id)?
                .iter()
                .any(|d| Some(d) == domain.as_ref() || d.contains(&ql));
        if by_name || by_domain {
            found.push(p);
        }
    }
    // exact names first; otherwise contacts, organizations, teams as found
    found.sort_by_key(|p| !names_lower(p).contains(&ql));
    found.truncate(FIND_LIMIT);
    Ok(found)
}

fn level(l: Level) -> &'static str {
    l.code()
}

fn block(c: &Connection, ports: &Ports, viewer: &Viewer, p: &Party) -> ApiResult<String> {
    let kind = match p.kind.as_str() {
        "org" => "organization",
        "team" => "team",
        _ => "contact",
    };
    let mut lines = vec![format!("{} ({kind}, id {})", p.name, p.id)];
    if !p.aliases.is_empty() {
        lines.push(format!("Also known as: {}", p.aliases.join(", ")));
    }
    match p.kind.as_str() {
        "org" => {
            if let Some(k) = &p.org_kind {
                lines.push(format!(
                    "Kind: {}",
                    if k == "self" {
                        "the user's own organization"
                    } else {
                        k
                    }
                ));
            }
            let ds = repo::domains(c, &viewer.ws, &p.id)?;
            if !ds.is_empty() {
                lines.push(format!("Domains: {}", ds.join(", ")));
            }
            let people: Vec<String> = repo::org_people(c, viewer, &p.id)?
                .into_iter()
                .take(10)
                .map(|(_, n, t)| match t.filter(|t| !t.is_empty()) {
                    Some(t) => format!("{n} ({t})"),
                    None => n,
                })
                .collect();
            if !people.is_empty() {
                lines.push(format!("People: {}", people.join(", ")));
            }
        }
        "team" => {
            let d = super::teams::repo::details(c, &p.id)?;
            if !d.purpose.is_empty() {
                lines.push(format!("Purpose: {}", d.purpose));
            }
            let card = super::cards::contact_cards(c, viewer, std::slice::from_ref(&p.id))?;
            if let Some(m) = card
                .first()
                .and_then(|k| k.text.lines().find_map(|l| l.strip_prefix("- 成员：")))
            {
                lines.push(format!("Members: {}", m.replace('、', ", ")));
            }
        }
        _ => {
            let js: Vec<String> = jobs(c, viewer, &p.id)?
                .into_iter()
                .filter(|j| j.to.is_none())
                .map(|j| match j.title.filter(|t| !t.is_empty()) {
                    Some(t) => format!("{}, {t}", j.org_name),
                    None => j.org_name,
                })
                .collect();
            if !js.is_empty() {
                lines.push(format!("Works at: {}", js.join("; ")));
            }
            if p.is_me() {
                lines.push("This is the user.".into());
            } else if !p.rels.is_empty() {
                lines.push(format!("Relationship to the user: {}", p.rels.join(", ")));
            }
            if let Some(h) = &ports.handles {
                let hs = h.handles(c, &viewer.ws, &p.id)?;
                if !hs.is_empty() {
                    lines.push(format!("Accounts: {}", hs.join(", ")));
                }
            }
            lines.extend(super::ownership_mcp::find_lines(c, viewer, p)?);
        }
    }
    let attrs: Vec<String> = repo::values(c, &viewer.ws, &p.id)?
        .into_iter()
        .filter(|v| v.status == "confirmed" && v.level != Level::Never)
        .map(|v| format!("- {}: {} (level: {})", v.label, v.value, level(v.level)))
        .collect();
    if !attrs.is_empty() {
        lines.push("Attributes:".into());
        lines.extend(attrs);
    }
    let f = MemFilter {
        limit: FIND_MEMS,
        ..MemFilter::usable()
    };
    let ms: Vec<String> = mems::about(c, viewer, &ObjRef::new(PARTY, &p.id), &f)?
        .into_iter()
        .map(|m| format!("- [{}] {} (level: {})", m.kind, m.text, level(m.level)))
        .collect();
    if !ms.is_empty() {
        lines.push("Memories:".into());
        lines.extend(ms);
    }
    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------- steve_contacts_remember

/// The party `who` names: an id, or else the one active contact, organization or team whose name or
/// alias equals it (case-insensitive). Never a guess: none or several is an error naming the way out.
pub(crate) fn resolve_who(c: &Connection, viewer: &Viewer, who: &str) -> Result<Party, String> {
    let w = who.trim();
    if let Some(p) = repo::get(c, w).map_err(|e| e.to_string())?.filter(|p| {
        p.workspace_id == viewer.ws
            && p.status == "active"
            && viewer.sees(&p.visibility, &p.created_by)
    }) {
        return Ok(p);
    }
    let wl = w.to_lowercase();
    let mut hits: Vec<Party> = prepo::parties(c, viewer, &["contact", "org", "team"], false)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|p| names_lower(p).contains(&wl))
        .collect();
    match hits.len() {
        0 => Err(format!(
            "No person, organization or team is called \"{w}\". Look them up with {FIND} and pass the id."
        )),
        1 => Ok(hits.remove(0)),
        _ => {
            let list: Vec<String> = hits
                .iter()
                .map(|p| format!("{} ({})", p.name, p.id))
                .collect();
            Err(format!(
                "Several are called \"{w}\": {}. Pass the id.",
                list.join(", ")
            ))
        }
    }
}

/// `steve_contacts_remember`: a candidate memory about `who` plus its confirm decision.
pub fn remember(
    m: &Modules,
    cx: &McpCx,
    who: &str,
    text: &str,
    kind: &str,
) -> Result<String, String> {
    let text = text.trim();
    let n = text.chars().count();
    if n == 0 || n > 2000 {
        return Err("text must be 1 to 2000 characters.".into());
    }
    let viewer = viewer(m, cx).map_err(|e| e.to_string())?;
    m.store
        .transaction(|tx| {
            let p = resolve_who(tx, &viewer, who).map_err(ApiError::Rule)?;
            let origin_label = match (&cx.item_id, &cx.session_id) {
                (Some(i), _) => format!("{i} 的会话"),
                _ => "Agent 会话".to_string(),
            };
            memory::propose(
                tx,
                &viewer,
                &Proposal {
                    party_id: p.id,
                    kind: kind.into(),
                    text: text.into(),
                    bot_id: cx.bot_id.clone(),
                    item_id: cx.item_id.clone(),
                    session_id: cx.session_id.clone(),
                    origin_label,
                },
            )
        })
        .map(|_| SAVED.to_string())
        .map_err(|e: ApiError| e.to_string())
}

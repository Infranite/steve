//! Every tool an agent can call, in one registry, described the MCP way (name, description,
//! `inputSchema`, annotations) plus Steve's own metadata: where it comes from ([`Source`]), whether it
//! reads or writes, the kinds of thing it acts on, and whether it acts as you (a write in your own name;
//! carried now, the send tier of the output pipeline will take those later).
//!
//! The sources: Steve's built-in tools (steve-mcp and the modules', [`super::tool_service`]), the
//! library's MCP servers the session's Bot has switched on (proxied by the Hub,
//! [`super::library_mcp`]), the remote A2A agents it uses (`steve_a2a_<name>`), and connector tools
//! (none yet: a connector declares its read and write operations in its handshake, and they become
//! entries of [`Source::Connector`]).
//!
//! A session lists only a small core ([`steve_modules::caps::tools::CORE`]) and what its Bot pinned;
//! every other tool is found with `steve_tools_search` (structured filters and keyword scoring, no
//! model) and run with `steve_tool_call`, which checks the arguments against the tool's schema before it
//! runs it. Steve's MCP is stateless and never sends `tools/list_changed`, so this works for every
//! harness alike; one that supports changing its tool list mid-session could later add what it found
//! directly (an adapter property, not declared yet).

use serde_json::{Map, Value, json};
use steve_modules::caps::tools::{CALL, CORE, SEARCH};
use steve_node_api::ToolDescriptor;

/// Where a tool comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Steve's own (steve-mcp and the modules').
    Steve,
    /// A library MCP server: its id and name.
    Mcp { id: String, name: String },
    /// A remote A2A agent: its id and name.
    A2a { id: String, name: String },
    /// A connector's operation on one account (phase C).
    #[allow(dead_code)]
    Connector { connector: String, account: String },
}

impl Source {
    /// What `source` in a search matches, and what a result says it came from.
    pub fn label(&self) -> String {
        match self {
            Source::Steve => "steve".into(),
            Source::Mcp { name, .. } => format!("mcp:{name}"),
            Source::A2a { name, .. } => format!("a2a:{name}"),
            Source::Connector { connector, account } => format!("connector:{connector}/{account}"),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Source::Steve => 0,
            Source::Connector { .. } => 1,
            Source::Mcp { .. } => 2,
            Source::A2a { .. } => 3,
        }
    }

    fn matches(&self, filter: &str) -> bool {
        let f = filter.trim().to_ascii_lowercase();
        let (kind, name) = match self {
            Source::Steve => ("steve", "steve"),
            Source::Mcp { name, .. } => ("mcp", name.as_str()),
            Source::A2a { name, .. } => ("a2a", name.as_str()),
            Source::Connector { connector, .. } => ("connector", connector.as_str()),
        };
        f == kind
            || f.eq_ignore_ascii_case(name)
            || f == self.label().to_ascii_lowercase()
            || matches!(self, Source::Connector { account, .. } if f.eq_ignore_ascii_case(account))
    }
}

/// One tool in the registry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
    pub destructive: bool,
    pub source: Source,
    /// The kinds of thing it acts on (`item`, `mail`, `github`, …).
    pub resources: Vec<String>,
    /// It writes in your own name (a mail from you, a comment by your GitHub account).
    pub as_you: bool,
    /// An MCP server's own name for the tool, when [`Self::name`] is Steve's for it.
    pub remote: Option<String>,
    /// The server's standard MCP annotations, preserved alongside Steve metadata.
    pub upstream_annotations: Value,
}

impl Entry {
    pub fn kind(&self) -> &'static str {
        if self.read_only { "read" } else { "write" }
    }

    pub fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            read_only: self.read_only,
            annotations: self.annotations(),
        }
    }

    /// The MCP annotations, with Steve's metadata under `steve`.
    pub fn annotations(&self) -> Value {
        let mut a = self
            .upstream_annotations
            .as_object()
            .cloned()
            .unwrap_or_default();
        a.insert("readOnlyHint".into(), json!(self.read_only));
        a.insert("destructiveHint".into(), json!(self.destructive));
        a.insert(
            "steve".into(),
            json!({
                "source": self.source.label(),
                "kind": self.kind(),
                "resources": self.resources,
                "asYou": self.as_you,
            }),
        );
        Value::Object(a)
    }

    /// The MCP `tools/list` entry.
    pub fn mcp(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": self.annotations(),
        })
    }
}

/// The resources a built-in tool acts on, and whether it writes as you.
fn steve_meta(name: &str) -> (&'static [&'static str], bool) {
    let n = name.strip_prefix("steve_").unwrap_or(name);
    let res: &'static [&'static str] = match n {
        "update" | "ask" | "request_approval" | "deliver" => &["item", "turn"],
        "publish" | "lane_status" => &["item", "release", "git"],
        "workflow" => &["workflow", "item"],
        "sessions_list" => &["session"],
        "devices" => &["device"],
        "debug" => &["diagnostics", "session"],
        "upgrade" => &["upgrade", "release"],
        "event_get" => &["event"],
        "reply_source" | "im_read" => &["chat", "message"],
        "whois" | "person_holdings" => &["contact", "person"],
        "remember" | "memory_list" | "memory_tidy" => &["memory", "knowledge"],
        "steward_propose" => &["contact", "steward"],
        "tools_search" | "tool_call" => &["tool"],
        n if n.starts_with("item") => &["item"],
        n if n.starts_with("knowledge") => &["knowledge", "document"],
        n if n.starts_with("contacts") => &["contact", "person"],
        n if n.starts_with("secret") => &["secret", "credential"],
        n if n.starts_with("github") => &["github", "repository", "pull_request"],
        n if n.starts_with("mail") => &["mail", "email"],
        n if n.starts_with("a2a_") => &["agent"],
        _ => &[],
    };
    // writes that go out in your own name: a draft you send, a comment or PR by your GitHub account
    let as_you = matches!(n, "mail_draft" | "github_comment" | "github_open_pr");
    (res, as_you)
}

/// Steve's built-in tools as entries.
pub fn steve_entries(catalog: Vec<ToolDescriptor>) -> Vec<Entry> {
    catalog
        .into_iter()
        .map(|t| {
            let (res, as_you) = steve_meta(&t.name);
            Entry {
                destructive: false,
                resources: res.iter().map(|r| r.to_string()).collect(),
                as_you,
                read_only: t.read_only,
                name: t.name,
                description: t.description,
                input_schema: t.input_schema,
                source: Source::Steve,
                remote: None,
                upstream_annotations: Value::Null,
            }
        })
        .collect()
}

/// A remote agent's tool as an entry.
pub fn a2a_entry(id: &str, name: &str, tool: &Value) -> Entry {
    Entry {
        name: tool["name"].as_str().unwrap_or_default().to_string(),
        description: tool["description"].as_str().unwrap_or_default().to_string(),
        input_schema: tool["inputSchema"].clone(),
        read_only: false,
        destructive: false,
        source: Source::A2a {
            id: id.into(),
            name: name.into(),
        },
        resources: vec!["agent".into(), name.to_string()],
        as_you: false,
        remote: None,
        upstream_annotations: Value::Null,
    }
}

/// The name Steve gives tool `tool` of library server `server`: `<server>__<tool>`, in the characters
/// every model API takes, at most 64 long (a longer one ends in a hash of the whole).
pub fn mcp_name(server: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let full = format!("{}__{}", clean(server), clean(tool));
    if full.len() <= 64 && clean(server) == server && clean(tool) == tool && !server.contains("__")
    {
        return full;
    }
    use sha2::Digest;
    let h = hex::encode(sha2::Sha256::digest(format!("{server}\0{tool}").as_bytes()));
    format!("{}_{}", &full[..full.len().min(55)], &h[..8])
}

/// Whether a single-tool pin could belong to this server (including a truncated namespace).
/// This only chooses which catalogs to load; the final pin is always matched by its exact name.
pub fn pin_may_use_server(server: &str, tool: &str) -> bool {
    let prefix = format!("{server}__");
    tool.starts_with(&prefix[..prefix.len().min(55)])
}

/// A library server's tool as an entry.
pub fn mcp_entry(id: &str, server: &str, t: &super::library_mcp::RemoteTool) -> Entry {
    let a = &t.annotations;
    let read_only = a["readOnlyHint"].as_bool().unwrap_or(false);
    Entry {
        name: mcp_name(server, &t.name),
        description: t.description.clone(),
        input_schema: t.input_schema.clone(),
        read_only,
        destructive: !read_only && a["destructiveHint"].as_bool().unwrap_or(true),
        source: Source::Mcp {
            id: id.into(),
            name: server.into(),
        },
        resources: a
            .pointer("/steve/resources")
            .and_then(Value::as_array)
            .map(|xs| {
                xs.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .filter(|xs: &Vec<String>| !xs.is_empty())
            .unwrap_or_else(|| vec![server.to_ascii_lowercase()]),
        as_you: a
            .pointer("/steve/asYou")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        remote: Some(t.name.clone()),
        upstream_annotations: t.annotations.clone(),
    }
}

/// `steve_tools_search`'s definition.
pub fn search_tool() -> ToolDescriptor {
    ToolDescriptor {
        name: SEARCH.into(),
        description: "Find a tool that is not in your tool list: Steve's own (items, work graph, knowledge, memories, contacts, secrets, GitHub, mail, chats, devices, diagnostics), the MCP servers and remote agents your Bot uses. Give words for what you want to do and, if you like, filters. Returns the best matches with name, a one-line description and inputSchema; run one with steve_tool_call. Deterministic keyword search: an empty query with filters lists everything that matches.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "words for what you want to do, e.g. \"comment on a pull request\""},
                "source": {"type": "string", "description": "steve, mcp, a2a, connector, or the name of one MCP server, agent or connector"},
                "kind": {"type": "string", "enum": ["read", "write"]},
                "resource": {"type": "string", "description": "what it acts on, e.g. item, workflow, knowledge, memory, contact, secret, github, mail, chat, device"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 25, "description": "at most this many (default 8)"}
            },
            "additionalProperties": false
        }),
        read_only: true,
        annotations: serde_json::Value::Null,
    }
}

/// `steve_tool_call`'s definition.
pub fn call_tool() -> ToolDescriptor {
    ToolDescriptor {
        name: CALL.into(),
        description: "Run a Steve tool by name: one steve_tools_search found, or one this session's brief names that is not in your tool list. Steve checks `arguments` against the tool's inputSchema first and answers what is wrong; the result is the tool's own.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "the tool's name, as steve_tools_search gives it"},
                "arguments": {"type": "object", "description": "the tool's arguments, as its inputSchema describes"}
            },
            "required": ["name"],
            "additionalProperties": false
        }),
        read_only: false,
        annotations: serde_json::Value::Null,
    }
}

/// A session's tools: everything it can call, and the names it lists.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub entries: Vec<Entry>,
    /// What the session lists: the core, then its Bot's pins.
    pub listed: Vec<String>,
    /// Library servers whose tools could not be had now, with why.
    pub unreachable: Vec<(String, String)>,
}

impl Registry {
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// What the session lists, in order.
    pub fn listed_entries(&self) -> Vec<&Entry> {
        self.listed.iter().filter_map(|n| self.get(n)).collect()
    }

    /// `listed` from the core and the pins: tools by name, every tool of a pinned server, a pinned
    /// agent's tool; what the session cannot call is left out.
    pub fn list(&mut self, pins: &[steve_store::caps::tools::Pin]) {
        let mut listed: Vec<String> = CORE
            .iter()
            .filter(|n| self.get(n).is_some())
            .map(|n| n.to_string())
            .collect();
        for p in pins {
            let names: Vec<String> = self
                .entries
                .iter()
                .filter(|e| match (p.kind.as_str(), &e.source) {
                    ("tool", _) => e.name == p.target,
                    ("mcp", Source::Mcp { id, .. }) | ("a2a", Source::A2a { id, .. }) => {
                        *id == p.target
                    }
                    _ => false,
                })
                .map(|e| e.name.clone())
                .collect();
            for n in names {
                if !listed.contains(&n) {
                    listed.push(n);
                }
            }
        }
        self.listed = listed;
    }
}

/// What `steve_tools_search` was asked.
#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: String,
    pub source: Option<String>,
    pub kind: Option<String>,
    pub resource: Option<String>,
    pub limit: usize,
}

pub const DEFAULT_LIMIT: usize = 8;
pub const MAX_LIMIT: usize = 25;

impl Query {
    pub fn from_args(args: &Value) -> Query {
        let s = |k: &str| {
            args[k]
                .as_str()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        Query {
            text: s("query").unwrap_or_default(),
            source: s("source"),
            kind: s("kind").map(|k| k.to_ascii_lowercase()),
            resource: s("resource").map(|r| r.to_ascii_lowercase()),
            limit: args["limit"]
                .as_u64()
                .map_or(DEFAULT_LIMIT, |n| (n as usize).clamp(1, MAX_LIMIT)),
        }
    }
}

/// Words people use for Steve's kinds of thing, in Chinese, to the resource they mean.
const ALIASES: &[(&str, &str)] = &[
    ("事项", "item"),
    ("任务", "item"),
    ("会话", "session"),
    ("工作图", "workflow"),
    ("协作", "workflow"),
    ("知识", "knowledge"),
    ("文档", "document"),
    ("记忆", "memory"),
    ("联系人", "contact"),
    ("同事", "person"),
    ("机密", "secret"),
    ("密钥", "secret"),
    ("凭据", "credential"),
    ("邮件", "mail"),
    ("群", "chat"),
    ("聊天", "chat"),
    ("消息", "message"),
    ("设备", "device"),
    ("升级", "upgrade"),
    ("发布", "release"),
    ("诊断", "diagnostics"),
    ("事件", "event"),
    ("评论", "comment"),
];

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF)
}

/// The words of a text: lower-case ASCII words (a plural or `-ing`/`-ed` form cut to its stem) and,
/// for Chinese, each run of characters and its pairs.
fn words(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut ascii = String::new();
    let mut cjk = String::new();
    let flush_ascii = |w: &mut String, out: &mut Vec<String>| {
        if !w.is_empty() {
            out.push(stem(&w.to_ascii_lowercase()));
            w.clear();
        }
    };
    let flush_cjk = |w: &mut String, out: &mut Vec<String>| {
        if !w.is_empty() {
            let cs: Vec<char> = w.chars().collect();
            if cs.len() > 1 {
                out.extend(cs.windows(2).map(|p| p.iter().collect::<String>()));
            }
            out.push(w.clone());
            w.clear();
        }
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(c);
        } else if is_cjk(c) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(c);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

fn stem(w: &str) -> String {
    for suffix in ["ies", "ing", "ed", "s"] {
        if w.len() > suffix.len() + 2
            && let Some(base) = w.strip_suffix(suffix)
        {
            return if suffix == "ies" {
                format!("{base}y")
            } else {
                base.to_string()
            };
        }
    }
    w.to_string()
}

/// How well `e` matches `query`'s words: a word in the name counts most, then its source and
/// resources, then its description; a word that only starts one counts less.
fn score(e: &Entry, query: &[String], whole: &str) -> u32 {
    if e.name.eq_ignore_ascii_case(whole) {
        return 1000;
    }
    let name = words(
        &e.name
            .strip_prefix("steve_")
            .unwrap_or(&e.name)
            .replace(['_', '-'], " "),
    );
    let mut meta = words(&e.resources.join(" "));
    meta.extend(words(&match &e.source {
        Source::Steve => "steve".to_string(),
        Source::Mcp { name, .. } | Source::A2a { name, .. } => name.replace(['_', '-'], " "),
        Source::Connector { connector, account } => format!("{connector} {account}"),
    }));
    let desc = words(&e.description);
    let hit = |field: &[String], q: &str, full: u32, part: u32| -> u32 {
        if field.iter().any(|w| w == q) {
            full
        } else if q.len() >= 3
            && field
                .iter()
                .any(|w| w.starts_with(q) || q.starts_with(w.as_str()) && w.len() >= 3)
        {
            part
        } else {
            0
        }
    };
    let mut total = 0;
    for q in query {
        total += hit(&name, q, 8, 5)
            .max(hit(&meta, q, 6, 3))
            .max(hit(&desc, q, 2, 1));
    }
    total
}

/// The query's words, with the resources its Chinese words stand for.
fn query_words(text: &str) -> Vec<String> {
    let mut q = words(text);
    for (zh, en) in ALIASES {
        if text.contains(zh) {
            q.push(en.to_string());
        }
    }
    q.sort();
    q.dedup();
    q
}

/// The tools matching `q`, best first, at most `q.limit`, leaving out `skip` (the ones the session lists
/// already) unless asked for by name; and how many matched in all.
pub fn search<'a>(entries: &'a [Entry], skip: &[String], q: &Query) -> (Vec<&'a Entry>, usize) {
    let qw = query_words(&q.text);
    let mut found: Vec<(u32, &Entry)> = entries
        .iter()
        .filter(|e| e.name != SEARCH && e.name != CALL)
        .filter(|e| q.source.as_deref().is_none_or(|s| e.source.matches(s)))
        .filter(|e| q.kind.as_deref().is_none_or(|k| e.kind() == k))
        .filter(|e| {
            q.resource.as_deref().is_none_or(|r| {
                let r = stem(r);
                e.resources.iter().any(|x| stem(x) == r)
            })
        })
        .filter_map(|e| {
            let s = if qw.is_empty() {
                1
            } else {
                score(e, &qw, &q.text)
            };
            let listed = skip.contains(&e.name) && s < 1000;
            (s > 0 && !listed).then_some((s, e))
        })
        .collect();
    found.sort_by(|(a, x), (b, y)| {
        b.cmp(a)
            .then(x.source.rank().cmp(&y.source.rank()))
            .then(x.name.cmp(&y.name))
    });
    // An exact-name lookup should not also match every tool merely because "steve" occurs
    // in its source metadata. Filters above still apply; only the exact result bypasses `skip`.
    if found.first().is_some_and(|(score, _)| *score == 1000) {
        found.retain(|(score, _)| *score == 1000);
    }
    let total = found.len();
    (
        found.into_iter().take(q.limit).map(|(_, e)| e).collect(),
        total,
    )
}

/// A description's first sentence, at most 200 characters.
pub fn one_line(d: &str) -> String {
    let end = d
        .char_indices()
        .find(|&(i, c)| (c == '.' && d[i + 1..].starts_with([' ', '\n'])) || c == '。' || c == '\n')
        .map_or(d.len(), |(i, c)| i + c.len_utf8());
    let s = d[..end].trim();
    if s.chars().count() > 200 {
        format!("{}…", s.chars().take(199).collect::<String>())
    } else {
        s.to_string()
    }
}

/// `steve_tools_search`'s answer.
pub fn search_answer(found: &[&Entry], total: usize, unreachable: &[(String, String)]) -> String {
    let tools: Vec<Value> = found
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "description": one_line(&e.description),
                "source": e.source.label(),
                "kind": e.kind(),
                "annotations": e.annotations(),
                "inputSchema": e.input_schema,
            })
        })
        .collect();
    let mut out = Map::new();
    out.insert("tools".into(), Value::Array(tools));
    if total > found.len() {
        out.insert("more".into(), json!(total - found.len()));
    }
    if !unreachable.is_empty() {
        out.insert(
            "unreachable".into(),
            Value::Array(
                unreachable
                    .iter()
                    .map(|(n, why)| json!({"server": n, "why": why}))
                    .collect(),
            ),
        );
    }
    out.insert(
        "next".into(),
        json!(if found.is_empty() {
            "Nothing matched: try other words, fewer filters, or an empty query with a source to list one source's tools."
        } else {
            "Run one with steve_tool_call {\"name\": …, \"arguments\": {…}}."
        }),
    );
    Value::Object(out).to_string()
}

/// Checks a tool's arguments using its JSON Schema dialect (2020-12 by default). Local `$ref`,
/// composition and boolean schemas work too. File and HTTP reference retrieval are disabled in the
/// dependency: a plugin cannot make validation read a local file or contact a second service.
pub fn validate(schema: &Value, v: &Value) -> Result<(), Vec<String>> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| vec![format!("invalid inputSchema: {e}")])?;
    let issues: Vec<String> = validator
        .iter_errors(v)
        .map(|e| format!("arguments{}: {e}", e.instance_path()))
        .collect();
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

/// What Steve's own tools cover, by kind, for the catalog summary.
const STEVE_AREAS: &str = "items, sessions, workflows, releases, knowledge, memories, contacts, secrets, GitHub, mail, chats, events, devices, diagnostics, upgrades";

/// The catalog summary that goes into a session's brief (about 200 tokens): how tools are found and
/// run, what each source offers. `servers`: each library server's name and its tools' names when
/// known; `agents`: each remote agent's tool name and what it is for.
pub fn summary(
    listed: &[String],
    servers: &[(String, Option<Vec<String>>)],
    agents: &[(String, String)],
) -> String {
    let mut lines = vec![
        format!(
            "Tool catalog: core + {} Bot pins are loaded. Find other tools with steve_tools_search (query; source, kind read/write, resource), then steve_tool_call {{name, arguments}}. Tools named elsewhere in this brief remain callable by name.",
            listed.len().saturating_sub(CORE.len())
        ),
        format!("- Steve: {STEVE_AREAS}."),
    ];
    // Source names and descriptions are operator/plugin input; keep the injected catalog bounded
    // even with many capabilities. Full definitions remain available through search.
    let short = |s: &str| s.chars().take(48).collect::<String>();
    for (name, tools) in servers.iter().take(3) {
        let names = tools.as_ref().map(|t| {
            t.iter()
                .take(2)
                .map(|n| short(n))
                .collect::<Vec<_>>()
                .join(", ")
        });
        lines.push(format!(
            "- MCP {}: {}.",
            short(name),
            names
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "search by source for its tools".into())
        ));
    }
    for (tool, what) in agents.iter().take(2) {
        lines.push(format!("- Remote agent {}: {}", short(tool), short(what)));
    }
    if servers.len() > 3 || agents.len() > 2 {
        lines.push(format!("{} MCP servers and {} remote agents enabled in total; search source mcp or a2a for more.", servers.len(), agents.len()));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steve(name: &str, desc: &str, read_only: bool) -> Entry {
        let (res, as_you) = steve_meta(name);
        Entry {
            name: name.into(),
            description: desc.into(),
            input_schema: json!({"type": "object"}),
            read_only,
            destructive: false,
            source: Source::Steve,
            resources: res.iter().map(|r| r.to_string()).collect(),
            as_you,
            remote: None,
            upstream_annotations: Value::Null,
        }
    }

    fn entries() -> Vec<Entry> {
        let mut v = vec![
            steve(
                "steve_github_comment",
                "Post a comment on a pull request or issue.",
                false,
            ),
            steve(
                "steve_github_status",
                "Read a pull request's status and checks.",
                true,
            ),
            steve(
                "steve_mail_draft",
                "Draft a mail for the user to send.",
                false,
            ),
            steve(
                "steve_workflow",
                "Read or replan this item's shared work graph.",
                false,
            ),
            steve("steve_items_search", "Search items.", true),
            steve("steve_contacts_find", "Find people in the contacts.", true),
        ];
        v.push(mcp_entry(
            "bmcp_1",
            "linear",
            &super::super::library_mcp::RemoteTool {
                name: "create_issue".into(),
                description: "Create a Linear issue in a team.".into(),
                input_schema: json!({"type": "object", "properties": {"title": {"type": "string"}}, "required": ["title"]}),
                annotations: json!({"readOnlyHint": false}),
            },
        ));
        v
    }

    fn q(text: &str) -> Query {
        Query {
            text: text.into(),
            limit: DEFAULT_LIMIT,
            ..Default::default()
        }
    }

    fn names(found: &[&Entry]) -> Vec<String> {
        found.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn search_ranks_by_words_and_filters() {
        let e = entries();
        let (found, _) = search(&e, &[], &q("comment on a pull request"));
        assert_eq!(names(&found)[0], "steve_github_comment");
        let (found, _) = search(&e, &[], &q("issues"));
        assert_eq!(
            names(&found)[..2],
            ["linear__create_issue", "steve_github_comment"]
        );
        let (found, _) = search(
            &e,
            &[],
            &Query {
                source: Some("linear".into()),
                ..q("")
            },
        );
        assert_eq!(names(&found), ["linear__create_issue"]);
        let (found, total) = search(
            &e,
            &[],
            &Query {
                kind: Some("read".into()),
                ..q("")
            },
        );
        assert_eq!(total, 3);
        assert!(found.iter().all(|e| e.read_only));
        let (found, _) = search(&e, &[], &q("给同事发邮件"));
        assert_eq!(
            names(&found)[..2],
            ["steve_mail_draft", "steve_contacts_find"]
        );
        let (found, _) = search(
            &e,
            &[],
            &Query {
                resource: Some("mails".into()),
                ..q("")
            },
        );
        assert_eq!(names(&found), ["steve_mail_draft"]);
        // what the session lists already is left out, unless asked for by name
        let skip = vec!["steve_items_search".to_string()];
        assert!(
            search(&e, &skip, &q("search items"))
                .0
                .iter()
                .all(|x| x.name != "steve_items_search")
        );
        assert_eq!(
            names(&search(&e, &skip, &q("steve_items_search")).0),
            ["steve_items_search"]
        );
        assert!(search(&e, &[], &q("zebra")).0.is_empty());
    }

    #[test]
    fn exact_name_lookup_is_narrow_and_still_obeys_filters_and_catalog_scope() {
        let all = entries();
        let skip = vec!["steve_items_search".into()];
        assert_eq!(
            names(&search(&all, &skip, &q("steve_items_search")).0),
            ["steve_items_search"]
        );
        for query in [
            Query {
                source: Some("mcp".into()),
                ..q("steve_items_search")
            },
            Query {
                resource: Some("mail".into()),
                ..q("steve_items_search")
            },
            Query {
                kind: Some("write".into()),
                ..q("steve_items_search")
            },
        ] {
            assert!(
                search(&all, &skip, &query)
                    .0
                    .iter()
                    .all(|e| e.name != "steve_items_search")
            );
        }
        let authorized: Vec<_> = all
            .into_iter()
            .filter(|e| e.name != "linear__create_issue")
            .collect();
        let result = search(&authorized, &[], &q("linear__create_issue"));
        assert!(
            result.0.iter().all(|e| e.name != "linear__create_issue"),
            "a query never imports a tool absent from the authorized registry"
        );
        assert!(
            search(&authorized, &[], &q("给同事发邮件")).0.len() > 1,
            "natural-language queries retain multiple candidates"
        );
    }

    #[test]
    fn the_same_query_answers_the_same() {
        let e = entries();
        let a = search_answer(&search(&e, &[], &q("read")).0, 0, &[]);
        let b = search_answer(&search(&e, &[], &q("read")).0, 0, &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn library_names_fit_every_model_api() {
        assert_eq!(mcp_name("linear", "create_issue"), "linear__create_issue");
        assert!(mcp_name("fs", "read.file").starts_with("fs__read_file_"));
        assert_ne!(mcp_name("fs", "read.file"), mcp_name("fs", "read_file"));
        let long = mcp_name("server", &"x".repeat(100));
        assert_eq!(long.len(), 64);
        assert_ne!(long, mcp_name("server", &"x".repeat(101)));
    }

    #[test]
    fn arguments_are_checked_against_the_schema() {
        let s = json!({"type": "object", "properties": {
            "title": {"type": "string", "minLength": 1},
            "n": {"type": "integer", "minimum": 1},
            "mode": {"enum": ["a", "b"]},
            "tags": {"type": "array", "items": {"type": "string"}},
            "x": {"type": ["string", "null"]}
        }, "required": ["title"], "additionalProperties": false});
        assert_eq!(
            validate(&s, &json!({"title": "t", "n": 2, "x": null})),
            Ok(())
        );
        let errors = validate(
            &s,
            &json!({"n": 0.5, "mode": "c", "tags": ["a", 1], "y": 1}),
        )
        .unwrap_err();
        assert!(errors.len() >= 5, "{errors:?}");
        assert!(
            errors.iter().any(|e| e.contains("arguments/tags/1")),
            "{errors:?}"
        );
        assert!(validate(&json!({"type": "object"}), &json!(3)).is_err());
        assert_eq!(validate(&json!({"type": "integer"}), &json!(2.0)), Ok(()));
    }

    #[test]
    fn listed_is_the_core_then_the_pins() {
        let mut r = Registry {
            entries: entries(),
            ..Default::default()
        };
        r.entries.push(steve(SEARCH, "", true));
        r.entries.push(steve(CALL, "", false));
        r.list(&[
            steve_store::caps::tools::Pin {
                kind: "mcp".into(),
                target: "bmcp_1".into(),
            },
            steve_store::caps::tools::Pin {
                kind: "tool".into(),
                target: "steve_workflow".into(),
            },
            steve_store::caps::tools::Pin {
                kind: "tool".into(),
                target: "steve_gone".into(),
            },
        ]);
        assert_eq!(
            r.listed,
            [
                "steve_items_search",
                SEARCH,
                CALL,
                "linear__create_issue",
                "steve_workflow"
            ]
        );
    }

    #[test]
    fn the_summary_is_short() {
        let s = summary(
            &CORE.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &[
                (
                    "linear".into(),
                    Some(vec![
                        "linear__create_issue".into(),
                        "linear__list_issues".into(),
                    ]),
                ),
                ("postgres".into(), None),
            ],
            &[(
                "steve_a2a_trips".into(),
                "Plans trips. Long text after.".into(),
            )],
        );
        assert!(s.contains("- MCP linear: linear__create_issue, linear__list_issues."));
        assert!(s.contains("- Remote agent steve_a2a_trips: Plans trips."));
        // about 200 tokens: a token is about four characters of English
        assert!(s.len() < 1200, "{} bytes", s.len());
    }
    #[test]
    fn catalog_summary_stays_bounded_with_many_large_sources() {
        let servers = vec![("s".repeat(200), Some(vec!["t".repeat(500); 100])); 32];
        let agents = vec![("a".repeat(100), "description".repeat(1000)); 32];
        let listed = vec!["p".repeat(128); 100];
        let text = summary(&listed, &servers, &agents);
        assert!(text.len() < 1500, "{} bytes", text.len());
        assert!(text.contains("32 MCP servers and 32 remote agents"));
    }

    #[test]
    fn full_schema_validation_never_ignores_references_or_composition() {
        let s = json!({"type":"object", "$defs":{"id":{"type":"string", "pattern":"^ST-[0-9]+$"}},
            "properties":{"id":{"$ref":"#/$defs/id"}, "n":{"type":"integer", "exclusiveMinimum":0}},
            "required":["id"], "additionalProperties":false});
        assert!(validate(&s, &json!({"id":"ST-42", "n":1})).is_ok());
        for value in [json!({"id":"anything"}), json!({"id":"ST-1", "n":0})] {
            assert!(validate(&s, &value).is_err());
        }
        assert!(validate(&json!(false), &json!({})).is_err());
        assert!(
            validate(
                &json!({"oneOf":[{"type":"number"},{"type":"integer"}]}),
                &json!(1)
            )
            .is_err()
        );
        assert!(
            validate(
                &json!({"allOf":[{"type":"string"},{"minLength":3}]}),
                &json!("x")
            )
            .is_err()
        );
        // References cannot read a file or fetch another URL during an invocation.
        for reference in [
            "file:///nonexistent-schema.json",
            "https://example.com/schema.json",
        ] {
            assert!(validate(&json!({"$ref":reference}), &json!({})).is_err());
        }
    }

    #[test]
    fn plugin_annotations_and_steve_metadata_survive_discovery() {
        let e = mcp_entry(
            "id",
            "files",
            &super::super::library_mcp::RemoteTool {
                name: "read".into(),
                description: "Read a file.".into(),
                input_schema: json!({"type":"object"}),
                annotations: json!({"title":"Read", "readOnlyHint":true, "idempotentHint":true, "openWorldHint":false, "steve":{"resources":["document"],"asYou":true}}),
            },
        );
        let a = e.mcp()["annotations"].clone();
        assert_eq!(a["title"], "Read");
        assert_eq!(a["idempotentHint"], true);
        assert_eq!(a["steve"]["source"], "mcp:files");
        assert_eq!(a["steve"]["kind"], "read");
        assert_eq!(a["steve"]["resources"], json!(["document"]));
        assert_eq!(a["steve"]["asYou"], true);
        assert_eq!(e.descriptor().annotations, a);

        assert_eq!(a["destructiveHint"], false);
        assert_eq!(
            serde_json::from_str::<Value>(&search_answer(&[&e], 1, &[])).unwrap()["tools"][0]["annotations"],
            a
        );
        assert_ne!(mcp_name("a__b", "c"), mcp_name("a", "b__c"));
    }
}

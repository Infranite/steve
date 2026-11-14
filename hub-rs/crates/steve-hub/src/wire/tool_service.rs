//! Steve's tools, independent of any tool protocol: the Steve tools (steve-mcp), the modules' tools
//! (`mcp_extra`), the remote agents' and the library MCP servers' tools a session's Bot uses, in one
//! registry ([`tool_registry`]), listed (the core and the Bot's pins) and run for one session. The Hub's MCP
//! endpoint is one way to reach them (for harnesses the in-process runner starts); a Node's
//! `tool.call` is the other. Both get the same definitions, checks and texts.

use std::sync::Arc;

use serde_json::{Value, json};
use steve_modules::caps::tools::{CALL, SEARCH};
use steve_node_api::ToolDescriptor;

use super::core::CoreMcp;
use super::mcp_extra::{self, Extra};
use super::tool_registry::{self, Registry, Source};

pub struct ToolService {
    pub mcp: Arc<CoreMcp>,
    pub extra: Extra,
}

/// Tools the agent only reads with (they change nothing).
const READ_ONLY: &[&str] = &[
    super::diagnostics::TOOL,
    "steve_items_search",
    "steve_item_read",
    steve_mcp::tools::LANE_STATUS,
    "steve_event_get",
    "steve_knowledge_search",
    "steve_knowledge_read",
    steve_knowledge::steward::LIST,
    steve_digest::tools::READ,
    steve_digest::tools::REPORT,
    steve_digest::tools::MATTERS,
    steve_digest::tools::MATTER,
    "steve_contacts_find",
    "steve_contact_get",
    "steve_im_read",
    "steve_sessions_list",
    "steve_whois",
    "steve_person_holdings",
    "steve_secret_list",
    "steve_github_status",
    "steve_github_ci_log",
    "steve_mail_thread",
    "steve_mail_attachment",
];

/// The most a tool's text may take once JSON-escaped: its answer to a Node goes in one frame
/// ([`steve_node_wire::envelope::MAX_FRAME`]), and the envelope around it needs the rest.
pub const RESULT_CAP: usize = steve_node_wire::envelope::BULK_CAP;

/// `text` as it fits in one answer: unchanged when it does, else its beginning and a note of what
/// was cut and how to get the rest. Any tool's result, so that none fails only for its size.
pub fn bound_result(tool: &str, text: String) -> String {
    bound_to(tool, text, RESULT_CAP)
}

fn bound_to(tool: &str, text: String, cap: usize) -> String {
    let escaped = |c: char| match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    };
    let size: usize = text.chars().map(escaped).sum();
    if size <= cap {
        return text;
    }
    let note = |kept: usize| {
        format!(
            "\n\n[Steve cut this {tool} result: it was {} bytes, more than the {cap} one tool \
             result may carry; only the first {kept} bytes are above. To read the rest, call {tool} \
             again for less at a time: a narrower query, a smaller limit or page, or one id or \
             field at a time.]",
            text.len()
        )
    };
    // the note's own size, with room for the numbers in it
    let room = cap.saturating_sub(note(text.len()).chars().map(escaped).sum::<usize>() + 16);
    let (mut used, mut end) = (0, 0);
    for (i, c) in text.char_indices() {
        used += escaped(c);
        if used > room {
            break;
        }
        end = i + c.len_utf8();
    }
    let mut out = text[..end].to_string();
    out.push_str(&note(end));
    out
}

/// How much of the library a registry needs: the servers a session lists (its Bot pinned them), or
/// every server it can use (a search, a call by name).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reach<'a> {
    Listed,
    All,
    Tool(&'a str),
}

/// A listing waits this long for a pinned server it has never listed; a search or call for any.
const LIST_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
const SEARCH_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

pub fn builtin_catalog() -> Vec<ToolDescriptor> {
    let mut out: Vec<ToolDescriptor> = steve_mcp::tools::list()
        .into_iter()
        .map(|t| ToolDescriptor {
            read_only: READ_ONLY.contains(&t.name.as_ref()),
            annotations: serde_json::Value::Null,
            name: t.name.to_string(),
            description: t.description.map(|d| d.to_string()).unwrap_or_default(),
            input_schema: Value::Object((*t.input_schema).clone()),
        })
        .collect();
    out.extend(mcp_extra::module_tools().into_iter().map(|t| {
        let name = t["name"].as_str().unwrap_or_default().to_string();
        ToolDescriptor {
            read_only: READ_ONLY.contains(&name.as_str()),
            annotations: serde_json::Value::Null,
            name,
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: t["inputSchema"].clone(),
        }
    }));
    out
}

impl ToolService {
    /// Every built-in tool, in the order the MCP endpoint lists them.
    pub fn catalog(&self) -> Vec<ToolDescriptor> {
        builtin_catalog()
    }

    /// Use the same recorded session principal and live membership check as ordinary module tools.
    /// Neither a later item requester edit nor an owner audit may lend personal connector access.
    async fn connector_caller(
        &self,
        s: &steve_store::Session,
    ) -> Result<steve_modules::Caller, String> {
        let (store, sid, ws) = (self.mcp.store.clone(), s.id.clone(), s.workspace_id.clone());
        tokio::task::spawn_blocking(move || {
            let current = store
                .sessions()
                .get(&sid)
                .map_err(|e| e.to_string())?
                .filter(|row| row.workspace_id == ws && row.status != "ended")
                .ok_or_else(|| "Connector session no longer exists".to_string())?;
            store
                .reader(|c| {
                    let (session_ws, principal) = super::principal::of_session(c, &current.id)?
                        .filter(|(session_ws, _)| session_ws == &ws)
                        .ok_or(steve_modules::ApiError::Unauthenticated)?;
                    let principal = super::principal::resolve(c, &session_ws, principal.user())?;
                    let user = principal
                        .user()
                        .ok_or(steve_modules::ApiError::Unauthenticated)?;
                    Ok(steve_modules::Caller {
                        ws: session_ws,
                        me: user.to_string(),
                    })
                })
                .map_err(|e: steve_modules::ApiError| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Everything session `s` can call ([`tool_registry`]): the built-in tools with
    /// `steve_tools_search` and `steve_tool_call`, the remote agents and library MCP servers its Bot
    /// has switched on (`reach`: whose tools to list now), and what it lists.
    pub async fn registry(&self, s: &steve_store::Session, reach: Reach<'_>) -> Registry {
        let mut entries = tool_registry::steve_entries(self.catalog());
        entries.extend(tool_registry::steve_entries(vec![
            tool_registry::search_tool(),
            tool_registry::call_tool(),
        ]));
        let (store, sid, bot) = (
            self.mcp.store.clone(),
            s.id.clone(),
            s.bot_id.clone().unwrap_or_default(),
        );
        let found = tokio::task::spawn_blocking(move || {
            store.reader(|c| {
                Ok::<_, steve_store::StoreError>((
                    steve_store::caps::tools::pins(c, &bot)?,
                    steve_store::caps::a2a::enabled_for_session(c, &sid)?,
                    steve_store::caps::mcp::enabled_for_session(c, &sid)?,
                ))
            })
        })
        .await;
        let (pins, agents, servers) = match found {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => {
                tracing::warn!(session = %s.id, "tool registry: {e}");
                Default::default()
            }
            Err(e) => {
                tracing::warn!(session = %s.id, "tool registry: {e}");
                Default::default()
            }
        };
        if let Ok(caller) = self.connector_caller(s).await {
            match self.extra.tools.channels.connector_tools(&caller).await {
                Ok(tools) => entries.extend(tools.into_iter().map(tool_registry::connector_entry)),
                Err(e) => tracing::warn!(session = %s.id, "connector catalog: {e}"),
            }
        }
        for a in &agents {
            entries.push(tool_registry::a2a_entry(
                &a.id,
                &a.name,
                &steve_modules::caps::a2a::tool(a),
            ));
        }
        let who = s.bot_id.clone().unwrap_or_else(|| "agent".into());
        let wanted: Vec<&steve_store::caps::mcp::Server> = servers
            .iter()
            .filter(|x| match reach {
                Reach::All => true,
                Reach::Tool(name) => tool_registry::pin_may_use_server(&x.name, name),
                Reach::Listed => pins.iter().any(|p| {
                    (p.kind == "mcp" && p.target == x.id)
                        || (p.kind == "tool"
                            && tool_registry::pin_may_use_server(&x.name, &p.target))
                }),
            })
            .collect();
        let wait = if reach == Reach::Listed {
            LIST_WAIT
        } else {
            SEARCH_WAIT
        };
        let lists = futures::future::join_all(
            wanted
                .iter()
                .map(|x| self.extra.library.tools_within(x, &who, wait)),
        )
        .await;
        let mut unreachable = vec![];
        for (x, got) in wanted.iter().zip(lists) {
            match got {
                Ok(tools) => entries.extend(
                    tools
                        .iter()
                        .map(|t| tool_registry::mcp_entry(&x.id, &x.name, t)),
                ),
                Err(e) => unreachable.push((x.name.clone(), e)),
            }
        }
        let mut r = Registry {
            entries,
            listed: vec![],
            unreachable,
        };
        r.list(&pins);
        r
    }

    /// What session `s`'s harness lists: the core tools and its Bot's pins.
    pub async fn session_catalog(&self, s: &steve_store::Session) -> Vec<ToolDescriptor> {
        self.registry(s, Reach::Listed)
            .await
            .listed_entries()
            .into_iter()
            .map(|e| e.descriptor())
            .collect()
    }

    /// Runs `name` for session `s`: the success text, or the error text the agent reads.
    /// `steve_tools_search` and `steve_tool_call` are answered here; every call is counted for the
    /// session's Bot under the name of the tool that ran.
    pub async fn call(
        &self,
        s: steve_store::Session,
        name: &str,
        args: Value,
    ) -> Result<String, String> {
        let run = async {
            match name {
                SEARCH => {
                    check_args(SEARCH, &tool_registry::search_tool().input_schema, &args)?;
                    let observation =
                        super::diagnostics::ToolObservation::new(&self.mcp.store, &s, name);
                    observation.run(self.search(s.clone(), args)).await
                }
                CALL => {
                    check_args(CALL, &tool_registry::call_tool().input_schema, &args)?;
                    let inner = args["name"].as_str().unwrap_or_default().trim().to_string();
                    if inner.is_empty() {
                        return Err(format!(
                            "{CALL} needs `name`: the tool to run (find it with {SEARCH})."
                        ));
                    }
                    if inner == CALL || inner == SEARCH {
                        return Err(format!("Call {inner} directly, not through {CALL}."));
                    }
                    let inner_args = match &args["arguments"] {
                        Value::Null => json!({}),
                        v => v.clone(),
                    };
                    let observation =
                        super::diagnostics::ToolObservation::new(&self.mcp.store, &s, &inner);
                    observation
                        .run(self.run_tool(s.clone(), &inner, inner_args, true))
                        .await
                }
                _ => {
                    let observation =
                        super::diagnostics::ToolObservation::new(&self.mcp.store, &s, name);
                    observation
                        .run(self.run_tool(s.clone(), name, args, false))
                        .await
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(115), run).await
            .map_err(|_| format!("Tool {name} did not complete within 115 s (including discovery); its outcome may be unknown. Steve has not retried it."))?
    }

    async fn search(&self, s: steve_store::Session, args: Value) -> Result<String, String> {
        let q = tool_registry::Query::from_args(&args);
        if let Some(k) = q.kind.as_deref()
            && k != "read"
            && k != "write"
        {
            return Err(format!("{SEARCH}: kind is read or write, not {k}."));
        }
        let r = self.registry(&s, Reach::All).await;
        let (found, total) = tool_registry::search(&r.entries, &r.listed, &q);
        let names: Vec<String> = found.iter().map(|e| e.name.clone()).collect();
        let text = tool_registry::search_answer(&found, total, &r.unreachable);
        self.count(&s, names, 1, 0);
        Ok(text)
    }

    /// Runs a tool by name: a built-in one as before, a library server's or a remote agent's from the
    /// registry. `checked`: through `steve_tool_call`, so the arguments are checked against the
    /// tool's schema first (the built-in tools also check their own).
    async fn run_tool(
        &self,
        s: steve_store::Session,
        name: &str,
        args: Value,
        checked: bool,
    ) -> Result<String, String> {
        let builtin = steve_mcp::handles(name)
            || mcp_extra::owner_of(name).is_some_and(|o| !matches!(o, mcp_extra::Owner::A2a));
        let out = if builtin {
            if checked && let Some(t) = self.catalog().into_iter().find(|t| t.name == name) {
                check_args(name, &t.input_schema, &args)?;
            }
            self.call_inner(s.clone(), name, args).await
        } else {
            let r = self.registry(&s, Reach::Tool(name)).await;
            let Some(e) = r.get(name).cloned() else {
                let why = r
                    .unreachable
                    .iter()
                    .find(|(server, _)| name.starts_with(&format!("{server}__")))
                    .map(|(server, why)| {
                        format!(" MCP server {server} cannot be reached now: {why}")
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "MCP error -32602: Tool {name} not found. Find tools with {SEARCH}.{why}"
                ));
            };
            check_args(name, &e.input_schema, &args)?;
            match &e.source {
                Source::Connector { .. } => {
                    let caller = self.connector_caller(&s).await?;
                    // An opaque request key belongs to this invocation, never to model parameters.
                    // Channels rechecks the live declaration, account access and target bindings.
                    let key = uuid::Uuid::new_v4().to_string();
                    self.extra
                        .tools
                        .channels
                        .connector_call(&caller, name, args, &key)
                        .await
                        .map(|v| v.to_string())
                }
                Source::A2a { .. } => self.extra.caps.a2a_call(&s.id, name, &args).await,
                Source::Mcp { id, .. } => {
                    let (store, id, sid) = (self.mcp.store.clone(), id.clone(), s.id.clone());
                    let server = tokio::task::spawn_blocking(move || {
                        store.reader(|c| {
                            Ok::<_, steve_store::StoreError>(
                                steve_store::caps::mcp::enabled_for_session(c, &sid)?
                                    .into_iter()
                                    .find(|server| server.id == id),
                            )
                        })
                    })
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("MCP error -32602: Tool {name} not found"))?;
                    let who = s.bot_id.clone().unwrap_or_else(|| "agent".into());
                    let remote = e.remote.clone().unwrap_or_else(|| name.to_string());
                    self.extra.library.call(&server, &who, &remote, args).await
                }
                _ => Err(format!("MCP error -32602: Tool {name} not found")),
            }
        };
        self.count(&s, vec![name.to_string()], 0, 1);
        out
    }

    /// Counts tools found (`searched`) or run (`called`) for the session's Bot, off the caller's way.
    fn count(&self, s: &steve_store::Session, tools: Vec<String>, searched: i64, called: i64) {
        let Some(bot) = s.bot_id.clone() else {
            return;
        };
        if tools.is_empty() {
            return;
        }
        let store = self.mcp.store.clone();
        tokio::task::spawn_blocking(move || {
            let names: Vec<&str> = tools.iter().map(String::as_str).collect();
            let day = steve_store::caps::tools::day_of(steve_store::now());
            if let Err(e) = store.transaction(|tx| {
                steve_store::caps::tools::record(tx, &bot, day, &names, searched, called)
            }) {
                tracing::warn!(bot, "tool usage: {e}");
            }
        });
    }

    async fn call_inner(
        &self,
        s: steve_store::Session,
        name: &str,
        args: Value,
    ) -> Result<String, String> {
        if let Some(r) = mcp_extra::call_module(&self.extra, s.clone(), name, args.clone()).await {
            return r;
        }
        if steve_mcp::handles(name) {
            let args = match args {
                Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            return steve_mcp::run(&*self.mcp, &s, name, &args).await;
        }
        Err(format!("MCP error -32602: Tool {name} not found"))
    }
}

/// `args` against `name`'s schema: `Err` is what the agent reads to fix the call.
fn check_args(name: &str, schema: &Value, args: &Value) -> Result<(), String> {
    tool_registry::validate(schema, args).map_err(|issues| {
        format!(
            "MCP error -32602: Invalid arguments for tool {name}: {}. Its inputSchema: {schema}",
            issues.join("; ")
        )
    })
}

/// The catalog summary in a session's brief (`steve_modules::context::PromptContext`): what the tools
/// cover and how to reach the ones not listed. It names the tools of a library server as last listed;
/// one never listed yet is listed now in the background, so a search soon after finds it ready.
pub struct CatalogContext {
    pub library: Arc<super::library_mcp::LibraryMcp>,
}

impl steve_modules::context::PromptContext for CatalogContext {
    fn session(
        &self,
        c: &rusqlite::Connection,
        _ws: &str,
        item: &steve_store::Item,
    ) -> Option<String> {
        let bot = item.bot_id.as_deref().unwrap_or("steve");
        let pins = steve_store::caps::tools::pins(c, bot).ok()?;
        let servers: Vec<steve_store::caps::mcp::Server> = steve_store::caps::mcp::of_bot(c, bot)
            .ok()?
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(x, _)| x)
            .collect();
        let agents: Vec<(String, String)> = steve_store::caps::a2a::of_bot(c, bot)
            .ok()?
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(a, _)| {
                let t = steve_modules::caps::a2a::tool(&a);
                (
                    t["name"].as_str().unwrap_or_default().to_string(),
                    t["description"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let mut listed: Vec<String> = steve_modules::caps::tools::CORE
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut shown = vec![];
        for x in &servers {
            let names = match self.library.cached(x) {
                Some(Ok(t)) => Some(
                    t.iter()
                        .map(|t| tool_registry::mcp_name(&x.name, &t.name))
                        .collect::<Vec<_>>(),
                ),
                _ => {
                    self.library.warm(x, bot);
                    None
                }
            };
            if pins.iter().any(|p| p.kind == "mcp" && p.target == x.id) {
                listed.extend(names.clone().unwrap_or_default());
            }
            shown.push((x.name.clone(), names));
        }
        for p in pins.iter().filter(|p| p.kind == "tool") {
            listed.push(p.target.clone());
        }
        for p in pins.iter().filter(|p| p.kind == "a2a") {
            if let Ok(Some(a)) = steve_store::caps::a2a::get(c, &p.target) {
                listed.push(format!(
                    "{}{}",
                    steve_modules::caps::a2a::TOOL_PREFIX,
                    a.name
                ));
            }
        }
        listed.dedup();
        Some(tool_registry::summary(&listed, &shown, &agents))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_tools_use_existing_dispatch_and_discovery_not_a_larger_core() {
        let entries = tool_registry::steve_entries(
            steve_digest::tools::tools()
                .into_iter()
                .map(|t| steve_node_api::ToolDescriptor {
                    name: t["name"].as_str().unwrap().into(),
                    description: t["description"].as_str().unwrap().into(),
                    input_schema: t["inputSchema"].clone(),
                    read_only: READ_ONLY.contains(&t["name"].as_str().unwrap()),
                    annotations: serde_json::Value::Null,
                })
                .collect(),
        );
        for e in &entries {
            assert!(matches!(
                mcp_extra::owner_of(&e.name),
                Some(mcp_extra::Owner::Digest)
            ));
            assert!(e.resources.iter().any(|r| r == "digest"));
            assert!(!steve_modules::caps::tools::CORE.contains(&e.name.as_str()));
            assert_eq!(
                e.read_only,
                [
                    steve_digest::tools::READ,
                    steve_digest::tools::REPORT,
                    steve_digest::tools::MATTERS,
                    steve_digest::tools::MATTER
                ]
                .contains(&e.name.as_str())
            );
        }
        let (found, _) = tool_registry::search(
            &entries,
            &[],
            &tool_registry::Query {
                text: "digest".into(),
                limit: 20,
                ..Default::default()
            },
        );
        assert_eq!(found.len(), 8);
    }

    #[test]
    fn a_result_over_the_cap_is_cut_and_says_so() {
        assert_eq!(bound_to("steve_x", "short".into(), 100), "short");
        let big = "ab\"\n".repeat(400_000);
        let out = bound_result("steve_event_get", big.clone());
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.len() <= RESULT_CAP, "{} escaped bytes", json.len());
        assert!(big.starts_with(out.split("\n\n[Steve cut").next().unwrap()));
        assert!(
            out.contains(&format!("it was {} bytes", big.len())),
            "{out}"
        );
        assert!(out.contains("call steve_event_get again for less"), "{out}");
        // multi-byte text is cut on a character boundary
        let wide = "工作图".repeat(200_000);
        let out = bound_result("steve_workflow", wide);
        assert!(serde_json::to_string(&out).unwrap().len() <= RESULT_CAP);
        assert!(out.starts_with("工作图"));
    }
}

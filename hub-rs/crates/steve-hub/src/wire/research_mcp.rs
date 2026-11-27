//! Steve's `/mcp` for a research run's token (thread-routing §7.4): only read-only tools exist — the
//! knowledge base (`steve_knowledge_search`, `steve_knowledge_read`), this workspace's items
//! (`steve_items_search`, `steve_item_read`) and its agent sessions (`steve_sessions_list`); for a run
//! answering a channel conversation also that chat's thread and media
//! ([`steve_channels::im_conv_mcp`], scoped to its space). Anything else is refused. The token is the run's own
//! (`thread_runs.token_hash`), valid while the run is `running`.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};
use steve_node_api::{AdapterStatus, Discovery, HarnessCapability, NodeId, SetupState};
use steve_store::{Store, ThreadTask};

use super::mcp_extra::Extra;

pub const ITEMS_SEARCH: &str = "steve_items_search";
pub const ITEM_READ: &str = "steve_item_read";
pub const SESSIONS_LIST: &str = "steve_sessions_list";
pub const BOTS_LIST: &str = "steve_bots_list";
pub const NODES_LIST: &str = "steve_nodes_list";
/// The answer to any other tool.
pub const REFUSED: &str = "This tool is not available in a read-only research run.";
/// Maximum recent completed or failed items reported per Bot by `steve_bots_list`.
const MAX_RECENT_ITEMS_PER_BOT: usize = 5;
/// Maximum age for Node vitals snapshots reported by `steve_nodes_list`.
const NODE_VITALS_MAX_AGE_MS: i64 = 5 * 60 * 1000;

/// Live Hub facts read by `steve_nodes_list` (no extra store queries or network probes).
#[derive(Clone)]
pub struct DispatchFacts {
    pub harnesses: Arc<crate::harnesses::HubHarnesses>,
    pub nodes: Option<Arc<crate::nodes::NodeService>>,
    pub local_node: Option<Arc<dyn Discovery>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamedRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotLoadInfo {
    pub active_items: usize,
    pub running_sessions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotRecentItemInfo {
    pub id: String,
    pub title: String,
    pub outcome: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotTeamInfo {
    pub id: String,
    pub name: String,
    /// When `Some`, this Team restricts the Bot to these workspace-visible Nodes;
    /// when `None`, all of the Bot's `bound_nodes` apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_nodes: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub specialties: Vec<String>,
    pub enabled: bool,
    pub main: bool,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub effort: String,
    pub perm_mode: String,
    pub teams: Vec<BotTeamInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_project: Option<NamedRef>,
    pub bound_nodes: Vec<String>,
    pub load: BotLoadInfo,
    pub recent_items: Vec<BotRecentItemInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotsListResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_default_team: Option<NamedRef>,
    pub bots: Vec<BotInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeHarnessInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub permission_modes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeLoadInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_avg: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_available_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_sessions: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeCapacityInfo {
    pub sessions: u32,
    pub jobs: u32,
    pub execs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeDiskInfo {
    pub root: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeInfo {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub tags: Vec<String>,
    pub harnesses: Vec<NodeHarnessInfo>,
    pub load: NodeLoadInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<NodeCapacityInfo>,
    pub disk: Vec<NodeDiskInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodesListResponse {
    pub nodes: Vec<NodeInfo>,
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "properties": props, "required": required})
}

fn dispatch_tool_defs() -> Vec<Value> {
    vec![
        json!({
            "name": BOTS_LIST,
            "description": "List this workspace's Bots with their id, name, role, specialties, harness, model, reasoning effort, permission mode, Teams (with per-Team allowed_nodes restrictions when set), default project, bound execution Nodes, current load (active items and running sessions), and up to 5 recent completed or failed items. Use this before choosing a Bot to dispatch work.",
            "inputSchema": schema(json!({}), &[]),
        }),
        json!({
            "name": NODES_LIST,
            "description": "List execution Nodes visible in this workspace with online status, tags, ready harnesses (version, supported permission modes, default model, and available models), current load, capacity limits (when online), and disk free/total space. Use this with steve_bots_list to verify which Bots can run right now.",
            "inputSchema": schema(json!({}), &[]),
        }),
    ]
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
    v.push(super::diagnostics::tool());
    v.push(json!({
        "name": ITEMS_SEARCH,
        "description": "Search this workspace's tracked work by ST item ID or words in title, goal or current status. Use this for task progress, not knowledge search. Returns id, title, status and what is happening now.",
        "inputSchema": schema(json!({"query": {"type": "string"}}), &["query"]),
    }));
    v.push(json!({
        "name": ITEM_READ,
        "description": "Read one workspace item by its ST number (for example ST-23). For the owner: current goal with its version, original goal with its version when different, status, plan, the last 20 timeline entries and the latest delivery summary. Shared conversations only receive their item's title, status and current goal. Use this when the user mentions an ST number, especially to compare progress against the original goal.",
        "inputSchema": schema(json!({"id": {"type": "string", "description": "e.g. ST-20"}}), &["id"]),
    }));
    v.push(json!({
        "name": SESSIONS_LIST,
        "description": "List this workspace's agent sessions (Claude Code, Codex …) Steve runs or imported: how many are running, idle or failed, and for each its harness, model, bot, item and title. Use this for questions like how many sessions are running or what the agents are doing now. `all: true` also lists ended ones.",
        "inputSchema": schema(json!({"all": {"type": "boolean"}}), &[]),
    }));
    v.extend(dispatch_tool_defs());
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

/// The two read-only inspection tools exposed to an async dispatch Think job.
pub fn dispatch_descriptors() -> Vec<steve_node_api::ToolDescriptor> {
    dispatch_tool_defs()
        .into_iter()
        .map(|t| steve_node_api::ToolDescriptor {
            name: t["name"].as_str().unwrap_or_default().to_string(),
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: t["inputSchema"].clone(),
            read_only: true,
            annotations: serde_json::Value::Null,
        })
        .collect()
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
            .read(|r| steve_store::item_repo::in_conversation(r, &m.workspace_id, &m.space))
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

/// `steve_items_search`: up to 20 items whose title or goal contain every word of the query
/// (in a conversation scope: title and goal, among its own items).
pub fn items_search(
    store: &Store,
    ws: &str,
    scope: &ItemScope,
    query: &str,
) -> Result<String, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let items = store.items().all(ws).map_err(|e| e.to_string())?;
    let mut out = vec![];
    for i in items.into_iter().filter(|i| scope.sees(&i.id)) {
        let goal = store.items().goal(&i.id).unwrap_or_default();
        let hay = format!("{} {} {}", i.id, i.title, goal).to_lowercase();
        if words.iter().all(|w| hay.contains(w)) {
            out.push(format!(
                "{} · {} · {}",
                i.id,
                i.title,
                i.outcome.as_deref().unwrap_or_else(|| i.phase.as_str())
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

/// `steve_bots_list`: strongly-typed workspace Bot facts for Steve's dispatch judgment.
pub fn bots_list(store: &Store, ws: &str) -> Result<BotsListResponse, String> {
    let mut facts = store
        .read(|r| steve_store::bot_dispatch::workspace_bot_facts(r, ws, MAX_RECENT_ITEMS_PER_BOT))
        .map_err(|e| e.to_string())?;
    let bots = facts
        .bots
        .into_iter()
        .map(|bot| {
            let bot_teams = facts
                .teams
                .iter()
                .filter_map(|t| {
                    let member = t.member(&bot.id)?;
                    let allowed_nodes = member.nodes.as_ref().map(|ns| {
                        ns.iter()
                            .filter(|n| facts.visible_nodes.contains(*n))
                            .cloned()
                            .collect::<Vec<_>>()
                    });
                    Some(BotTeamInfo {
                        id: t.id.clone(),
                        name: t.name.clone(),
                        allowed_nodes,
                    })
                })
                .collect();
            let default_project = bot.default_project.as_ref().and_then(|pid| {
                facts.projects.get(pid).map(|name| NamedRef {
                    id: pid.clone(),
                    name: name.clone(),
                })
            });
            let recent_items = facts
                .recent_items_by_bot
                .remove(&bot.id)
                .unwrap_or_default()
                .into_iter()
                .map(|it| BotRecentItemInfo {
                    id: it.id,
                    title: it.title,
                    outcome: it.outcome,
                    updated_at: it.updated,
                })
                .collect();
            let bound_nodes = facts.bindings_by_bot.remove(&bot.id).unwrap_or_default();
            let active_items = facts.active_items_by_bot.get(&bot.id).copied().unwrap_or(0);
            let running_sessions = facts
                .running_sessions_by_bot
                .get(&bot.id)
                .copied()
                .unwrap_or(0);
            BotInfo {
                id: bot.id,
                name: bot.name,
                role: bot.role,
                specialties: bot.specialties,
                enabled: bot.enabled,
                main: bot.main,
                harness: bot.harness,
                model: bot.model,
                effort: bot.reasoning_effort.as_str().to_owned(),
                perm_mode: bot.perm_mode,
                teams: bot_teams,
                default_project,
                bound_nodes,
                load: BotLoadInfo {
                    active_items,
                    running_sessions,
                },
                recent_items,
            }
        })
        .collect();
    Ok(BotsListResponse {
        workspace_default_team: facts.default_team.map(|t| NamedRef {
            id: t.id,
            name: t.name,
        }),
        bots,
    })
}

fn ready_harness_info(cap: &HarnessCapability) -> Option<NodeHarnessInfo> {
    if cap
        .setup
        .as_ref()
        .is_some_and(|s| s.state != SetupState::Ready)
    {
        return None;
    }
    let adapter = cap.adapter.as_ref()?;
    if adapter.status != AdapterStatus::Ready || adapter.session_auth_required == Some(true) {
        return None;
    }
    let offer = adapter.session.as_ref()?;
    let permission_modes = adapter
        .execution_modes
        .as_ref()
        .map(|exec| {
            exec.iter()
                .filter(|mode| offer.modes.contains(mode))
                .map(|mode| mode.as_str().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let version = (!cap.version.trim().is_empty())
        .then(|| cap.version.clone())
        .or_else(|| adapter.version.clone().filter(|v| !v.trim().is_empty()));
    let default_model = offer.default_model.clone().filter(|m| !m.trim().is_empty());
    let mut models = Vec::new();
    let source_models = if !offer.models.is_empty() {
        offer
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
    } else {
        cap.models.iter().map(String::as_str).collect::<Vec<_>>()
    };
    for raw in source_models {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !models.iter().any(|seen| seen == trimmed) {
            models.push(trimmed.to_owned());
        }
    }
    Some(NodeHarnessInfo {
        name: cap.name.clone(),
        version,
        permission_modes,
        default_model,
        models,
    })
}

/// `steve_nodes_list`: strongly-typed workspace-visible Node facts for Steve's dispatch judgment.
pub fn nodes_list(
    store: &Store,
    facts: Option<&DispatchFacts>,
    ws: &str,
) -> Result<NodesListResponse, String> {
    let (visible, tags_by_node) = store
        .read(|r| {
            let local_name =
                steve_store::node_directory::name(r, "local")?.unwrap_or_else(|| "Hub".to_string());
            let mut list = vec![("local".to_string(), local_name)];
            for n in steve_store::node_directory::workspace_candidates(r, ws)? {
                list.push((n.id, n.name));
            }
            let tags = steve_store::nodes::all_tags(r)?;
            Ok::<_, steve_store::StoreError>((list, tags))
        })
        .map_err(|e| e.to_string())?;
    let since = crate::nodes::now() - NODE_VITALS_MAX_AGE_MS;
    let nodes = visible
        .into_iter()
        .map(|(id, name)| {
            let (online, generation, cached_caps) = match facts {
                Some(f) if id == "local" => {
                    let ln = f.local_node.as_ref().filter(|n| !n.is_upgrade_only());
                    let cached = ln.and_then(|n| n.cached_capabilities());
                    match (ln, &f.nodes) {
                        (None, _) => (false, None, None),
                        (Some(_), Some(svc)) => {
                            let link_gen = NodeId::new("local")
                                .ok()
                                .and_then(|nid| svc.fleet.remote(&nid).ready_generation());
                            if let Some(g) = link_gen {
                                (true, Some(g), cached)
                            } else if f.harnesses.node_report("local", None).is_some() {
                                (true, None, cached)
                            } else {
                                (false, None, cached)
                            }
                        }
                        (Some(_), None) => (true, None, cached),
                    }
                }
                Some(f) => match (&f.nodes, NodeId::new(id.clone()).ok()) {
                    (Some(svc), Some(nid)) => {
                        let remote = svc.fleet.remote(&nid);
                        let link_gen = (!remote.is_upgrade())
                            .then(|| remote.ready_generation())
                            .flatten();
                        let cached = remote.cached_capabilities();
                        (link_gen.is_some(), link_gen, cached)
                    }
                    _ => (false, None, None),
                },
                None => (false, None, None),
            };
            let harnesses = if online {
                let raw_caps = facts
                    .and_then(|f| f.harnesses.node_report(&id, generation))
                    .or_else(|| cached_caps.as_ref().map(|c| c.harnesses.clone()))
                    .unwrap_or_default();
                let mut list: Vec<NodeHarnessInfo> =
                    raw_caps.iter().filter_map(ready_harness_info).collect();
                list.sort_by(|a, b| a.name.cmp(&b.name));
                list
            } else {
                vec![]
            };
            let vitals = crate::metrics::latest_node(&id, since);
            let (load, disk) = match vitals {
                Some(v) => (
                    NodeLoadInfo {
                        cpus: v.cpus,
                        load_avg: v.load,
                        mem_available_bytes: v.mem_available,
                        mem_total_bytes: v.mem_total,
                        running_sessions: Some(v.sessions),
                    },
                    v.roots
                        .into_iter()
                        .map(|r| NodeDiskInfo {
                            root: r.root,
                            free_bytes: r.free,
                            total_bytes: r.total,
                        })
                        .collect(),
                ),
                None => (
                    NodeLoadInfo {
                        cpus: None,
                        load_avg: None,
                        mem_available_bytes: None,
                        mem_total_bytes: None,
                        running_sessions: None,
                    },
                    vec![],
                ),
            };
            let capacity = if online {
                cached_caps.map(|c| NodeCapacityInfo {
                    sessions: c.capacity.sessions,
                    jobs: c.capacity.jobs,
                    execs: c.capacity.execs,
                })
            } else {
                None
            };
            NodeInfo {
                tags: tags_by_node.get(&id).cloned().unwrap_or_default(),
                id,
                name,
                online,
                harnesses,
                load,
                capacity,
                disk,
            }
        })
        .collect();
    Ok(NodesListResponse { nodes })
}

/// Dispatch-job tool execution: only the two read-only Bot/Node inspection tools are allowed.
pub async fn dispatch_call(
    x: &Extra,
    ws: &str,
    name: &str,
    _args: &Value,
) -> Result<String, String> {
    let store = x.store.clone();
    let facts = x.dispatch_facts.clone();
    let ws = ws.to_owned();
    match name {
        BOTS_LIST => tokio::task::spawn_blocking(move || {
            let resp = bots_list(&store, &ws)?;
            serde_json::to_string(&resp).map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string())),
        NODES_LIST => tokio::task::spawn_blocking(move || {
            let resp = nodes_list(&store, facts.as_ref(), &ws)?;
            serde_json::to_string(&resp).map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string())),
        _ => Err(REFUSED.into()),
    }
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
        .read(|r| -> steve_channels::ChannelResult<bool> {
            match steve_store::channel_spaces::rows::get_in_workspace(r, &m.workspace_id, space_id)?
            {
                Some(space) => {
                    steve_channels::conversations::owner_only(r, &m.workspace_id, &space, conn)
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
    if !matches!(scope, ItemScope::All) {
        let goal = store.items().goal(&i.id).unwrap_or_default();
        return Ok(format!(
            "{} {}\n状态：{}\n目标：{goal}\n",
            i.id,
            i.title,
            i.outcome.as_deref().unwrap_or_else(|| i.phase.as_str())
        ));
    }
    // Read both ends of the history in one query so a concurrent goal edit cannot
    // pair a version with another version's text. Only the owner sees the history.
    let goals = store.items().goals(&i.id).map_err(|e| e.to_string())?;
    let mut s = format!(
        "{} {}\n状态：{}\n",
        i.id,
        i.title,
        i.outcome.as_deref().unwrap_or_else(|| i.phase.as_str()),
    );
    let tl = store.timeline().list(&i.id).map_err(|e| e.to_string())?;
    if let Some(progress) = tl.iter().rev().find(|entry| entry.kind == "progress") {
        s.push_str(&format!("进展：{}\n", progress.text));
    }
    if let Some(current) = goals.last() {
        s.push_str(&format!(
            "目标：{}\n当前目标版本：{}\n",
            current.text, current.version
        ));
        if let Some(original) = goals.first().filter(|g| g.text != current.text) {
            s.push_str(&format!(
                "原始目标（版本 {}）：{}\n",
                original.version, original.text
            ));
        }
    } else {
        s.push_str("目标：未记录\n");
    }
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

/// The same read-only allowlist for HTTP MCP and authenticated Node research jobs.
/// Callers establish the live run identity; audience checks are repeated inside every tool.
pub async fn call(
    x: &Extra,
    run: &ThreadTask,
    name: &str,
    args: &Value,
    owner: &str,
) -> Result<String, String> {
    let ws = run.workspace_id.clone();
    match name {
        n if super::onboard_mcp::is(n) => super::onboard_mcp::call(x, run, n, args, owner).await,
        super::diagnostics::TOOL => {
            let (store, core, run, args) = (
                x.store.clone(),
                x.tools.core.clone(),
                run.clone(),
                args.clone(),
            );
            tokio::task::spawn_blocking(steve_i18n::carry(move || {
                super::diagnostics::for_research(&store, &core, &run, args)
            }))
            .await
            .map_err(|_| "Diagnostic query unavailable.".to_string())?
        }
        steve_knowledge::tools::SEARCH | steve_knowledge::tools::READ => {
            // A user's view only when only the owner reads the answer.
            let (store, origin) = (x.store.clone(), run.origin_message_id.clone());
            let own = tokio::task::spawn_blocking(move || own_reader(&store, &origin))
                .await
                .map_err(|e| e.to_string())??;
            let user = if own {
                run.user_id.clone().unwrap_or_else(|| owner.into())
            } else {
                String::new()
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
            let origin = run.origin_message_id.clone();
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
            let origin = run.origin_message_id.clone();
            tokio::task::spawn_blocking(move || {
                item_read(&store, &ws, &ItemScope::of(&store, &origin)?, &i)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
        n if steve_channels::im_conv_mcp::handles(n) => {
            im_conv(x, run, n, args).await.map(|a| a.text_only())
        }
        SESSIONS_LIST => {
            let (store, all) = (x.store.clone(), args["all"].as_bool().unwrap_or(false));
            let origin = run.origin_message_id.clone();
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
        BOTS_LIST => {
            let store = x.store.clone();
            let origin = run.origin_message_id.clone();
            tokio::task::spawn_blocking(move || {
                if !own_reader(&store, &origin)? {
                    return Err(REFUSED.to_string());
                }
                let resp = bots_list(&store, &ws)?;
                serde_json::to_string(&resp).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
        NODES_LIST => {
            let store = x.store.clone();
            let facts = x.dispatch_facts.clone();
            let origin = run.origin_message_id.clone();
            tokio::task::spawn_blocking(move || {
                if !own_reader(&store, &origin)? {
                    return Err(REFUSED.to_string());
                }
                let resp = nodes_list(&store, facts.as_ref(), &ws)?;
                serde_json::to_string(&resp).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
        _ => Err(REFUSED.into()),
    }
}

/// The chat read scope of `run`: `Some` only for a run answering a channel conversation's turn.
async fn im_scope(x: &Extra, run: &ThreadTask) -> Option<steve_channels::im_conv_mcp::Scope> {
    let (ch, ws, origin) = (
        x.tools.channels.clone(),
        run.workspace_id.clone(),
        run.origin_message_id.clone(),
    );
    tokio::task::spawn_blocking(move || ch.conv_read_scope(&ws, &origin))
        .await
        .ok()?
        .map_err(|e| tracing::warn!("research chat scope not read: {e}"))
        .ok()
        .flatten()
}

/// A chat read tool for `run`, refused outside a channel conversation.
async fn im_conv(
    x: &Extra,
    run: &ThreadTask,
    name: &str,
    args: &Value,
) -> Result<steve_channels::im_conv_mcp::Answer, String> {
    let Some(scope) = im_scope(x, run).await else {
        return Err(REFUSED.into());
    };
    steve_channels::im_conv_mcp::call(&x.tools.channels, &scope, name, args).await
}

fn im_descriptors() -> Vec<steve_node_api::ToolDescriptor> {
    steve_channels::im_conv_mcp::tools()
        .into_iter()
        .map(|t| steve_node_api::ToolDescriptor {
            name: t["name"].as_str().unwrap_or_default().to_string(),
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: t["inputSchema"].clone(),
            read_only: true,
            annotations: serde_json::Value::Null,
        })
        .collect()
}

/// The first meeting is on for `run` (`onboard_mcp::open`), asked off the async workers.
async fn meeting(x: &Extra, run: &ThreadTask) -> bool {
    let (core, run) = (x.tools.core.clone(), run.clone());
    tokio::task::spawn_blocking(move || super::onboard_mcp::open(&core, &run))
        .await
        .unwrap_or(false)
}

/// What `/mcp` lists for `run`: the research tools, the chat read tools in a channel conversation,
/// and the first meeting's while it is on for it.
async fn listed(x: &Extra, run: &ThreadTask) -> Vec<Value> {
    let mut list = tools();
    if im_scope(x, run).await.is_some() {
        list.extend(steve_channels::im_conv_mcp::tools());
    }
    if meeting(x, run).await {
        list.extend(super::onboard_mcp::tools());
    }
    list
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
            annotations: serde_json::Value::Null,
        })
        .collect()
}

/// The catalog of the research job of `run`: [`descriptors`], the chat read tools in a channel
/// conversation, and the first meeting's tools while it is on for that run.
pub async fn catalog(x: &Extra, run: &ThreadTask) -> Vec<steve_node_api::ToolDescriptor> {
    let mut list = descriptors();
    if im_scope(x, run).await.is_some() {
        list.extend(im_descriptors());
    }
    if meeting(x, run).await {
        list.extend(super::onboard_mcp::descriptors());
    }
    list
}

/// A research job's tool call through its Node, answered as `/mcp` answers an in-process run.
pub async fn call_tool(
    x: &Extra,
    run: ThreadTask,
    name: &str,
    args: Value,
) -> Result<String, String> {
    let (store, ws) = (x.store.clone(), run.workspace_id.clone());
    let owner = tokio::task::spawn_blocking(move || super::mcp_extra::owner(&store, &ws))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    call(x, &run, name, &args, &owner).await
}

/// One JSON-RPC message from a research run.
pub async fn handle(x: &Extra, run: ThreadTask, msg: Value, owner: String) -> Response {
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
        "tools/list" => rpc(&id, json!({"tools": listed(x, &run).await})),
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
            // a chat image goes along as MCP image content; anything else is text
            if name == steve_channels::im_conv_mcp::MEDIA {
                return match im_conv(x, &run, &name, &args).await {
                    Ok(a) => {
                        let mut content = vec![json!({"type": "text", "text": a.text})];
                        content.extend(a.mcp_images());
                        rpc(&id, json!({"content": content}))
                    }
                    Err(t) => text_result(&id, Err(t)),
                };
            }
            let out = call(x, &run, &name, &args, &owner).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The chat read tools are never in the fixed research set (outside clients, the capability
    /// page): only a channel conversation's run lists them, read-only.
    #[test]
    fn the_chat_read_tools_are_listed_only_per_run() {
        let fixed: Vec<String> = tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .chain(descriptors().into_iter().map(|d| d.name))
            .collect();
        for name in [
            steve_channels::im_conv_mcp::THREAD,
            steve_channels::im_conv_mcp::MEDIA,
        ] {
            assert!(!fixed.iter().any(|n| n == name), "{name}");
        }
        let im = im_descriptors();
        assert_eq!(
            im.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            [
                steve_channels::im_conv_mcp::THREAD,
                steve_channels::im_conv_mcp::MEDIA
            ]
        );
        assert!(im.iter().all(|d| d.read_only));
        assert!(fixed.iter().any(|n| n == BOTS_LIST));
        assert!(fixed.iter().any(|n| n == NODES_LIST));
        let dispatch = dispatch_descriptors();
        assert_eq!(
            dispatch.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            [BOTS_LIST, NODES_LIST]
        );
        assert!(dispatch.iter().all(|d| d.read_only));
    }

    #[tokio::test]
    async fn bots_and_nodes_list_return_typed_facts_and_respect_workspace_visibility() {
        use steve_node_api::{
            AdapterInfo, AdapterOffer, Capabilities, Capacity, Command, Discovery, HistoryEntry,
            HistoryImport, HistoryLocation, HistoryRef, ImportedPage, NodeId, OperationId, Page,
            PageRequest, PermissionMode, Receipt, RootAccess, RootInfo,
        };

        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let owner_id = store
            .users()
            .create("sub_owner", "Owner", None, "owner")
            .unwrap();
        let other_id = store
            .users()
            .create("sub_other", "Other", None, "member")
            .unwrap();
        let personal_ws = store
            .workspaces()
            .create(
                &steve_store::NewWorkspace {
                    name: "Personal".into(),
                    description: None,
                    item_prefix: None,
                },
                &owner_id,
            )
            .unwrap();
        let ws = personal_ws.id.as_str();
        let shared_ws = store
            .workspaces()
            .create(
                &steve_store::NewWorkspace {
                    name: "Shared".into(),
                    description: None,
                    item_prefix: None,
                },
                &owner_id,
            )
            .unwrap();
        store
            .write(|w| {
                steve_store::Members::new(w).add(&shared_ws.id, &other_id, "member", 0)?;
                steve_store::nodes::create_pairing_as(w, "hash_remote", &owner_id, 1, 9999, true)?;
                steve_store::nodes::claim_pairing(
                    w,
                    "hash_remote",
                    &steve_store::nodes::NewNode {
                        node_id: "node_remote",
                        name: "Build Box",
                        pubkey: &[7u8; 32],
                        fingerprint: "fp_remote",
                    },
                    2,
                )?;
                steve_store::nodes::set_tags(w, "local", &["gpu".into()])?;
                steve_store::nodes::set_tags(w, "node_remote", &["linux".into()])?;
                Ok::<_, steve_store::StoreError>(())
            })
            .unwrap();
        let proj = store
            .projects()
            .create(
                ws,
                &steve_store::NewProject {
                    name: "Core Repo".into(),
                    description: "main repo".into(),
                    device_id: "hub".into(),
                    path: "/tmp/repo".into(),
                    kind: "git".into(),
                    worktree: true,
                    default_bot: None,
                    release: None,
                },
            )
            .unwrap();
        store
            .bots()
            .seed(
                "dev",
                false,
                &steve_store::NewBot {
                    name: "Dev".into(),
                    role: Some("Rust & frontend".into()),
                    harness: "codex".into(),
                    model: Some("gpt-5".into()),
                    reasoning_effort: steve_types::ReasoningEffort::High,
                    perm_mode: Some("auto".into()),
                    specialties: Some(vec!["rust".into(), "ui".into()]),
                    default_project: Some(proj.id.clone()),
                    ..Default::default()
                },
                ws,
            )
            .unwrap();
        store
            .bots()
            .seed(
                "other_bot",
                false,
                &steve_store::NewBot {
                    name: "Other".into(),
                    harness: "codex".into(),
                    ..Default::default()
                },
                &shared_ws.id,
            )
            .unwrap();
        let team_id = store
            .write(|w| {
                let id = steve_store::bot_admin::save(
                    w,
                    ws,
                    None,
                    "Core Team",
                    &[steve_store::bot_dispatch::Member {
                        bot: "dev".into(),
                        nodes: Some(vec!["local".into(), "hidden_node".into()]),
                    }],
                )?;
                steve_store::bot_admin::set_default(w, ws, Some(&id))?;
                steve_store::bot_admin::set_bindings(
                    w,
                    ws,
                    "dev",
                    &["local".into(), "node_remote".into()],
                )?;
                Ok::<_, steve_store::StoreError>(id)
            })
            .unwrap();
        let kind_tag = store.tags().kinds(ws).unwrap().remove(0).id;
        // Active item + 6 closed items (only 5 most recent completed/failed should be returned).
        let _active = store
            .items()
            .create(
                ws,
                &steve_store::NewItem {
                    title: "Active task".into(),
                    bot_id: "dev".into(),
                    tags: vec![kind_tag.clone()],
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        for i in 0..6 {
            let item = store
                .items()
                .create(
                    ws,
                    &steve_store::NewItem {
                        title: format!("Closed #{i}"),
                        bot_id: "dev".into(),
                        tags: vec![kind_tag.clone()],
                        ..Default::default()
                    },
                )
                .unwrap()
                .unwrap();
            let outcome = if i % 2 == 0 { "completed" } else { "failed" };
            store
                .write(|w| {
                    steve_store::lifecycle_write_in(
                        w,
                        &item.id,
                        &steve_store::LifecycleWrite {
                            phase: steve_types::ItemPhase::Closed,
                            cycle: item.cycle,
                            delivery_revision: item.delivery_revision,
                            outcome: Some(outcome.into()),
                            acceptance: None,
                            closing: None,
                            closed_at: Some(1000 + i as i64),
                            closed_reason: None,
                        },
                    )?;
                    Ok::<_, steve_store::StoreError>(())
                })
                .unwrap();
        }
        let s = store
            .sessions()
            .create(&steve_store::NewSession {
                workspace_id: ws.into(),
                bot_id: Some("dev".into()),
                harness: "codex".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .sessions()
            .patch(
                &s.id,
                &steve_store::SessionPatch {
                    status: Some("running".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let bots = bots_list(&store, ws).unwrap();
        assert_eq!(
            bots.workspace_default_team,
            Some(NamedRef {
                id: team_id.clone(),
                name: "Core Team".into()
            })
        );
        assert!(bots.bots.iter().all(|b| b.id != "other_bot"));
        let dev = bots.bots.iter().find(|b| b.id == "dev").expect("dev bot");
        assert_eq!(dev.role, "Rust & frontend");
        assert_eq!(dev.specialties, vec!["rust", "ui"]);
        assert_eq!(dev.harness, "codex");
        assert_eq!(dev.model.as_deref(), Some("gpt-5"));
        assert_eq!(dev.effort, "high");
        assert_eq!(dev.perm_mode, "auto");
        assert_eq!(
            dev.teams,
            vec![BotTeamInfo {
                id: team_id,
                name: "Core Team".into(),
                allowed_nodes: Some(vec!["local".into()]),
            }]
        );
        assert_eq!(
            dev.default_project,
            Some(NamedRef {
                id: proj.id,
                name: "Core Repo".into()
            })
        );
        assert_eq!(dev.bound_nodes, vec!["local", "node_remote"]);
        assert_eq!(dev.load.active_items, 1);
        assert_eq!(dev.load.running_sessions, 1);
        assert_eq!(dev.recent_items.len(), MAX_RECENT_ITEMS_PER_BOT);
        assert!(dev.recent_items.iter().all(|r| r.title != "Active task"));
        assert!(
            dev.recent_items
                .iter()
                .all(|r| matches!(r.outcome.as_str(), "completed" | "failed"))
        );

        // Shared workspace must not expose private remote Node in either tool.
        let shared_nodes = nodes_list(&store, None, &shared_ws.id).unwrap();
        assert_eq!(
            shared_nodes
                .nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            ["local"]
        );

        // Local node with ready & non-ready harnesses reports only ready harnesses and capacity.
        let ready_cap = HarnessCapability {
            name: "codex".into(),
            version: "1.2.3".into(),
            sessions: true,
            jobs: true,
            models: vec!["gpt-5".into()],
            resume: true,
            cancel: true,
            permissions: true,
            images: false,
            read_attached: false,
            enforcement: vec![],
            adapter: Some(AdapterInfo {
                title: "Codex".into(),
                builtin: true,
                status: AdapterStatus::Ready,
                error: None,
                version: Some("1.2.3".into()),
                load_session: true,
                list_sessions: false,
                mcp_http: true,
                embedded_context: false,
                auth: vec![],
                install_hint: None,
                system_prompt: true,
                session: Some(AdapterOffer {
                    modes: vec![
                        PermissionMode::Plan,
                        PermissionMode::Ask,
                        PermissionMode::Auto,
                    ],
                    models: vec![],
                    default_model: Some("gpt-5".into()),
                    efforts: vec![],
                    variant_models: vec![],
                }),
                session_error: None,
                session_auth_required: Some(false),
                execution_modes: Some(vec![PermissionMode::Plan, PermissionMode::Ask]),
                resume_command: None,
                fork_command: None,
                capabilities: None,
                missing_command: None,
            }),
            setup: None,
        };
        let unready_cap = HarnessCapability {
            name: "claude-code".into(),
            adapter: Some(AdapterInfo {
                status: AdapterStatus::AdapterMissing,
                ..ready_cap.adapter.clone().unwrap()
            }),
            ..ready_cap.clone()
        };
        struct FakeLocalDiscovery(Capabilities);
        #[async_trait::async_trait]
        impl Discovery for FakeLocalDiscovery {
            fn cached_capabilities(&self) -> Option<Capabilities> {
                Some(self.0.clone())
            }
            async fn set_session_capacity(&self, _: u32) -> steve_node_api::ApiResult<()> {
                Ok(())
            }
            async fn probe(&self, _fresh: bool) -> steve_node_api::ApiResult<Capabilities> {
                Ok(self.0.clone())
            }
            async fn history(
                &self,
                _: PageRequest,
            ) -> steve_node_api::ApiResult<Page<HistoryEntry>> {
                unreachable!()
            }
            async fn import_history(
                &self,
                _: Command<HistoryImport>,
            ) -> steve_node_api::ApiResult<Receipt> {
                unreachable!()
            }
            async fn import_page(
                &self,
                _: OperationId,
                _: PageRequest,
            ) -> steve_node_api::ApiResult<ImportedPage> {
                unreachable!()
            }
            async fn locate(&self, _: HistoryRef) -> steve_node_api::ApiResult<HistoryLocation> {
                unreachable!()
            }
        }
        let home_str = dir.path().display().to_string();
        let cfg = crate::config::Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(home_str.clone()),
            _ => None,
        })
        .unwrap();
        let reports = crate::system::RuntimeReports::new(Arc::new(
            steve_runtime::cache::Caches::new(Default::default()),
        ));
        let harnesses = crate::harnesses::HubHarnesses::of(&cfg, reports);
        harnesses
            .update(vec![ready_cap.clone(), unready_cap.clone()])
            .await;
        let fake_local = Arc::new(FakeLocalDiscovery(Capabilities {
            node: NodeId::new("local").unwrap(),
            os: "test".into(),
            version: "0.1.0".into(),
            harnesses: vec![ready_cap, unready_cap],
            roots: vec![RootInfo {
                id: steve_node_api::RootId::new("worktrees").unwrap(),
                display: "~/worktrees".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 16,
                jobs: 8,
                execs: 4,
            },
            features: vec![],
        }));
        let facts = DispatchFacts {
            harnesses,
            nodes: None,
            local_node: Some(fake_local),
        };
        let resp = nodes_list(&store, Some(&facts), ws).unwrap();
        assert_eq!(
            resp.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["local", "node_remote"]
        );
        let local = &resp.nodes[0];
        assert!(local.online);
        assert_eq!(local.tags, vec!["gpu"]);
        assert_eq!(local.harnesses.len(), 1);
        assert_eq!(local.harnesses[0].name, "codex");
        assert_eq!(local.harnesses[0].version.as_deref(), Some("1.2.3"));
        assert_eq!(local.harnesses[0].permission_modes, vec!["plan", "ask"]);
        assert_eq!(local.harnesses[0].default_model.as_deref(), Some("gpt-5"));
        assert_eq!(local.harnesses[0].models, vec!["gpt-5"]);
        assert_eq!(local.load.running_sessions, None);
        assert_eq!(
            local.capacity,
            Some(NodeCapacityInfo {
                sessions: 16,
                jobs: 8,
                execs: 4
            })
        );
        let remote = &resp.nodes[1];
        assert!(!remote.online);
        assert!(remote.harnesses.is_empty());
        assert_eq!(remote.capacity, None);
        assert_eq!(remote.load.running_sessions, None);
        assert_eq!(remote.tags, vec!["linux"]);
    }
}

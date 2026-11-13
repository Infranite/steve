//! The library's remote agents (A2A): each added by its Agent Card URL, its card fetched and shown
//! (name, description, skills), the secret its requests carry kept in the vault
//! (`steve_store::caps::a2a`). A Bot assigned one gets one Steve tool, `steve_a2a_<name>`, that
//! sends the agent a task (`message/send`, JSON-RPC) and answers with its result; the tool is
//! Steve's, so every harness gets it the same way.
//!
//! - `GET /capabilities/a2a-agents` → `AgentOut[]` (each with `bots: [{id, enabled}]`)
//! - `POST /capabilities/a2a-agents/preview {card_url}` → `{card_url, card}`: the card, not kept
//! - `POST /capabilities/a2a-agents {card_url, name?, auth?: {scheme: none|bearer|header, header?},
//!   secret?, bot?}` → `AgentOut`: fetches the card and keeps it
//! - `PATCH /capabilities/a2a-agents/:id {name?, card_url?, auth?, secret?, refresh?}` → `AgentOut`
//!   (`refresh: true`, or a new `card_url`, fetches the card again)
//! - `DELETE /capabilities/a2a-agents/:id` → `{ok:true}`
//!
//! A card URL that names only a host (`https://agent.example.com`) is looked up at the standard
//! well-known path, `/.well-known/agent-card.json`, then the older `/.well-known/agent.json`. The
//! secret is never stored with the agent, answered, logged or audited; saving again without one
//! keeps the one the vault has. Every change is audited (`a2a_agent.create|update|delete`).

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use regex::Regex;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::caps::a2a::{self as repo, Agent, Auth, SECRET_LABEL};
use steve_store::{Tx, base, now, uid};
use zeroize::Zeroizing;

use super::Capabilities;
use crate::vault::{VaultError, VaultResult};
use crate::{ApiError, Body, Caller, Check};

/// The prefix of the tool a Bot gets for each remote agent it uses.
pub const TOOL_PREFIX: &str = "steve_a2a_";
/// The standard place of an agent's card, and the one before it.
pub const CARD_PATHS: &[&str] = &["/.well-known/agent-card.json", "/.well-known/agent.json"];
/// The most of a card that is read.
const MAX_CARD: usize = 512 * 1024;
/// The most of an answer that is read.
const MAX_ANSWER: usize = 4 * 1024 * 1024;
/// How long one call waits for a task, in all: under the two minutes a Node gives a Steve tool
/// call. A task still running then is reported with its `task_id` to ask about later.
const TASK_WAIT: Duration = Duration::from_secs(100);
/// How often a task still working is asked about.
const POLL: Duration = Duration::from_secs(3);

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_]{0,39}$").unwrap());
static HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,128}$").unwrap());

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentIn {
    #[serde(default)]
    pub card_url: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub auth: Option<Auth>,
    /// The secret for `auth`; without it the one in the vault is kept.
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub refresh: Option<bool>,
    /// On create: also assign it to this Bot.
    #[serde(default)]
    pub bot: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreviewIn {
    pub card_url: String,
}

pub(super) async fn list_h(State(s): State<Capabilities>, who: Caller) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.a2a_list(&who.ws))
        .await
        .map(|v| Json(Value::Array(v)))
}

pub(super) async fn preview_h(
    State(s): State<Capabilities>,
    _who: Caller,
    Body(b): Body<PreviewIn>,
) -> VaultResult<Json<Value>> {
    let (url, card) = s.fetch_card(&b.card_url).await?;
    Ok(Json(json!({"card_url": url, "card": card})))
}

pub(super) async fn create_h(
    State(s): State<Capabilities>,
    who: Caller,
    Body(b): Body<AgentIn>,
) -> VaultResult<Json<Value>> {
    let raw = b.card_url.clone().unwrap_or_default();
    if raw.trim().is_empty() {
        let mut c = Check::new();
        c.that("card_url", false);
        c.done()?;
    }
    let (url, card) = s.fetch_card(&raw).await?;
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.a2a_create(&who.ws, &who.me, url, card, b))
        .await
        .map(Json)
}

pub(super) async fn update_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<AgentIn>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    let (ws, sid) = (who.ws.clone(), id.clone());
    let old = s
        .vault
        .blocking(move |_| svc.vault.store().reader(|c| agent_of(c, &ws, &sid)))
        .await?;
    let card_url = b.card_url.clone().filter(|u| u.trim() != old.card_url);
    let fetched = if b.refresh == Some(true) || card_url.is_some() {
        Some(
            s.fetch_card(card_url.as_deref().unwrap_or(&old.card_url))
                .await?,
        )
    } else {
        None
    };
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.a2a_save(&who.ws, &who.me, old, fetched, b, false))
        .await
        .map(Json)
}

pub(super) async fn remove_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.a2a_delete(&who.ws, &who.me, &id))
        .await?;
    Ok(Json(json!({ "ok": true })))
}

fn agent_of(c: &Connection, ws: &str, id: &str) -> VaultResult<Agent> {
    repo::get(c, id)?
        .filter(|a| a.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("远程 Agent").into())
}

/// The card's skills as the page and the tool show them.
fn card_skills(card: &Value) -> Vec<Value> {
    card["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            json!({
                "id": s["id"].as_str().unwrap_or(""),
                "name": s["name"].as_str().or(s["id"].as_str()).unwrap_or(""),
                "description": s["description"].as_str().unwrap_or(""),
                "tags": s["tags"].as_array().cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// What the page sees: never the secret, only whether there is one.
pub(crate) fn out(a: &Agent) -> Value {
    json!({
        "id": a.id, "name": a.name, "tool": format!("{TOOL_PREFIX}{}", a.name),
        "card_url": a.card_url, "title": a.title(), "description": a.description(),
        "url": a.endpoint(), "version": a.card["version"].as_str(),
        "provider": a.card.pointer("/provider/organization").and_then(Value::as_str),
        "protocol": a.card["protocolVersion"].as_str(),
        "skills": card_skills(&a.card), "auth": a.auth, "has_secret": a.credential_item.is_some(),
        "fetched": a.fetched, "created": a.created, "updated": a.updated,
    })
}

fn out_with_users(c: &Connection, a: &Agent) -> VaultResult<Value> {
    let mut v = out(a);
    v["bots"] = super::users(c, "a2a", &a.id)?;
    Ok(v)
}

/// The tool key a card's name suggests: lowercase letters, digits and `_`, at most 40.
pub fn suggested_name(title: &str) -> String {
    let mut s = String::new();
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
        } else if !s.ends_with('_') && !s.is_empty() {
            s.push('_');
        }
    }
    let s: String = s.trim_matches('_').chars().take(40).collect();
    let s = s.trim_end_matches('_').to_string();
    if s.is_empty() { "agent".into() } else { s }
}

/// `raw` as the card URLs to try: as given when it has a path, else the well-known ones.
fn card_candidates(raw: &str) -> Result<Vec<String>, ApiError> {
    let bad = || {
        let mut c = Check::new();
        c.that("card_url", false);
        c.done().unwrap_err()
    };
    let raw = raw.trim();
    let u = url::Url::parse(raw).map_err(|_| bad())?;
    if !matches!(u.scheme(), "http" | "https") || u.host().is_none() || raw.chars().count() > 2000 {
        return Err(bad());
    }
    if u.path() == "/" || u.path().is_empty() {
        let base = raw.trim_end_matches('/');
        return Ok(CARD_PATHS.iter().map(|p| format!("{base}{p}")).collect());
    }
    Ok(vec![raw.to_string()])
}

/// A JSON answer read up to `max` bytes.
async fn read_json(r: reqwest::Response, max: usize) -> Result<Value, String> {
    let mut r = r;
    let mut buf = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|e| e.to_string())? {
        buf.extend_from_slice(&chunk);
        if buf.len() > max {
            return Err(format!("more than {max} bytes"));
        }
    }
    serde_json::from_slice(&buf).map_err(|e| e.to_string())
}

/// The text of a message's or an artifact's parts.
fn parts_text(parts: &Value) -> String {
    let mut out = vec![];
    for p in parts.as_array().into_iter().flatten() {
        let kind = p["kind"].as_str().or(p["type"].as_str()).unwrap_or("");
        match kind {
            "text" => out.push(p["text"].as_str().unwrap_or("").to_string()),
            "data" => out.push(p["data"].to_string()),
            "file" => {
                let f = &p["file"];
                let name = f["name"].as_str().unwrap_or("file");
                match f["uri"].as_str() {
                    Some(u) => out.push(format!("[{name}]({u})")),
                    None => out.push(format!(
                        "[{name}: inline file, {} bytes base64]",
                        f["bytes"].as_str().map_or(0, str::len)
                    )),
                }
            }
            _ => {
                if let Some(t) = p["text"].as_str() {
                    out.push(t.to_string())
                }
            }
        }
    }
    out.join("\n")
}

/// The state of a task that is not going to change by itself any more.
fn settled(state: &str) -> bool {
    !matches!(state, "submitted" | "working" | "unknown" | "")
}

/// What the agent's answer (`result` of `message/send` or `tasks/get`) says, for the model.
pub fn describe(agent: &str, result: &Value) -> String {
    if result["kind"] == "message"
        || (result.get("parts").is_some() && result.get("status").is_none())
    {
        let ctx = result["contextId"]
            .as_str()
            .map(|c| format!(" (context_id {c})"))
            .unwrap_or_default();
        return format!("{agent} replied{ctx}:\n\n{}", parts_text(&result["parts"]));
    }
    let id = result["id"].as_str().unwrap_or("");
    let ctx = result["contextId"].as_str().unwrap_or("");
    let state = result
        .pointer("/status/state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut s = format!("{agent}: task {id} is {state} (task_id {id}, context_id {ctx}).");
    let said = parts_text(
        result
            .pointer("/status/message/parts")
            .unwrap_or(&Value::Null),
    );
    if !said.is_empty() {
        s.push_str(&format!("\n\nThe agent says:\n{said}"));
    }
    for a in result["artifacts"].as_array().into_iter().flatten() {
        let name = a["name"].as_str().unwrap_or("result");
        s.push_str(&format!(
            "\n\nArtifact {name}:\n{}",
            parts_text(&a["parts"])
        ));
    }
    match state {
        "input-required" | "auth-required" => s.push_str(
            "\n\nThe agent is waiting for more from you: call this tool again with your answer as `message` and this `task_id` and `context_id`.",
        ),
        st if !settled(st) => s.push_str(
            "\n\nIt is still running: call this tool again later with only this `task_id` to get its result.",
        ),
        _ => {}
    }
    s
}

/// The tool a Bot gets for `a`.
pub fn tool(a: &Agent) -> Value {
    let skills: Vec<String> = card_skills(&a.card)
        .iter()
        .map(|s| {
            let d = s["description"].as_str().unwrap_or("");
            if d.is_empty() {
                s["name"].as_str().unwrap_or("").to_string()
            } else {
                format!("{}: {d}", s["name"].as_str().unwrap_or(""))
            }
        })
        .filter(|s| !s.is_empty())
        .collect();
    let mut description = format!(
        "Send a task to the remote agent {} over A2A and get its result.",
        a.title()
    );
    let d = a.description();
    if !d.is_empty() {
        description.push_str(&format!(" It says of itself: {d}"));
    }
    if !skills.is_empty() {
        description.push_str(&format!(" Its skills: {}.", skills.join("; ")));
    }
    description.push_str(" Write `message` as a complete request in plain language. To continue the same conversation pass back the `context_id` it gave; to answer a question it asked, or to check a task still running, pass its `task_id`.");
    json!({
        "name": format!("{TOOL_PREFIX}{}", a.name),
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "message": {"type": "string", "description": "What to ask the agent to do, or your answer to its question."},
                "context_id": {"type": "string"},
                "task_id": {"type": "string"}
            },
            "additionalProperties": false
        }
    })
}

impl Capabilities {
    /// Fetches and checks the card at `raw` (or at its host's well-known paths): the URL it was found
    /// at, and the card.
    pub async fn fetch_card(&self, raw: &str) -> Result<(String, Value), ApiError> {
        let mut last = String::new();
        for u in card_candidates(raw)? {
            if let Err(e) = crate::net::check_url(&u) {
                return Err(ApiError::rule(e.to_string()));
            }
            let r = self
                .http
                .get(&u)
                .header("accept", "application/json")
                .timeout(Duration::from_secs(15))
                .send()
                .await;
            let r = match r {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    last = format!("HTTP {}", r.status().as_u16());
                    continue;
                }
                Err(e) => {
                    last = e.to_string();
                    continue;
                }
            };
            let card = match read_json(r, MAX_CARD).await {
                Ok(v) => v,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            let ok = card["name"].as_str().is_some_and(|s| !s.trim().is_empty())
                && card["url"]
                    .as_str()
                    .and_then(|s| url::Url::parse(s).ok())
                    .is_some_and(|x| matches!(x.scheme(), "http" | "https"));
            if !ok {
                return Err(ApiError::rule(steve_i18n::t!("modules.caps.card_invalid")));
            }
            return Ok((u, card));
        }
        Err(ApiError::rule(steve_i18n::t!(
            "modules.caps.card_unreachable",
            why = last
        )))
    }

    pub fn a2a_list(&self, ws: &str) -> VaultResult<Vec<Value>> {
        self.vault.store().reader(|c| {
            repo::list(c, ws)?
                .iter()
                .map(|a| out_with_users(c, a))
                .collect()
        })
    }

    pub fn a2a_create(
        &self,
        ws: &str,
        me: &str,
        card_url: String,
        card: Value,
        b: AgentIn,
    ) -> VaultResult<Value> {
        let at = now();
        let (count, name) = self.vault.store().reader(|c| {
            let list = repo::list(c, ws)?;
            let base = suggested_name(card["name"].as_str().unwrap_or(""));
            let mut name = base.clone();
            let mut n = 2;
            while list.iter().any(|a| a.name == name) {
                let suffix = format!("_{n}");
                name = format!("{}{suffix}", &base[..base.len().min(40 - suffix.len())]);
                n += 1;
            }
            Ok::<_, VaultError>((list.len(), name))
        })?;
        if count >= super::MAX_LIBRARY {
            return Err(ApiError::rule(steve_i18n::t!(
                "modules.caps.library_full",
                max = super::MAX_LIBRARY
            ))
            .into());
        }
        let blank = Agent {
            id: uid("a2a"),
            workspace_id: ws.into(),
            name,
            card_url: card_url.clone(),
            card: Value::Null,
            auth: Auth::None,
            credential_item: None,
            fetched: at,
            created: at,
            updated: at,
        };
        self.a2a_save(ws, me, blank, Some((card_url, card)), b, true)
    }

    /// Merges `b` (and a card just fetched) into `old`, keeps the secret in the vault and writes
    /// the row in one transaction.
    fn a2a_save(
        &self,
        ws: &str,
        me: &str,
        old: Agent,
        fetched: Option<(String, Value)>,
        b: AgentIn,
        fresh: bool,
    ) -> VaultResult<Value> {
        let mut a = old.clone();
        if let Some(n) = &b.name {
            a.name = n.trim().to_string();
        }
        if let Some((u, card)) = fetched {
            a.card_url = u;
            a.card = card;
            a.fetched = now();
        }
        if let Some(auth) = b.auth.clone() {
            a.auth = match auth {
                Auth::Header { header } => Auth::Header {
                    header: header.trim().to_string(),
                },
                x => x,
            };
        }
        let secret = b.secret.clone().filter(|s| !s.is_empty());
        let mut c = Check::new();
        c.pattern("name", &a.name, &NAME);
        if let Auth::Header { header } = &a.auth {
            c.pattern("auth.header", header, &HEADER);
        }
        c.that(
            "secret",
            secret
                .as_deref()
                .is_none_or(|x| x.chars().count() <= 8000 && !x.contains(['\n', '\r', '\0'])),
        );
        c.done()?;
        let keep_old = a.auth.needs_secret() && secret.is_none() && old.credential_item.is_some();
        if a.auth.needs_secret() && secret.is_none() && old.credential_item.is_none() {
            return Err(ApiError::rule(steve_i18n::t!("modules.caps.a2a_secret_needed")).into());
        }
        let prepared = match (&secret, a.auth.needs_secret()) {
            (Some(v), true) => Some(self.vault.prepare_credential(
                ws,
                me,
                &format!("A2A {}", a.name),
                None,
                vec![(SECRET_LABEL.to_string(), Zeroizing::new(v.clone()))],
            )?),
            _ => None,
        };
        a.updated = now();
        self.vault.store().transaction(|tx: &Tx| {
            if repo::name_taken(tx, ws, &a.name, Some(&a.id))? {
                return Err(ApiError::conflict(steve_i18n::t!(
                    "modules.caps.a2a_name_taken",
                    name = a.name
                ))
                .into());
            }
            for x in steve_store::caps::of_entry(tx, "a2a", &a.id)? {
                if super::clashes(tx, &x.bot_id, "a2a", &a.id, &a.name)? {
                    return Err(ApiError::conflict(steve_i18n::t!(
                        "modules.caps.name_used",
                        name = a.name
                    ))
                    .into());
                }
            }
            if !keep_old {
                if let Some(item) = &old.credential_item {
                    self.vault.destroy_item(tx, ws, me, item)?;
                }
                a.credential_item = match &prepared {
                    Some(p) => Some(self.vault.insert_prepared(tx, p)?),
                    None => None,
                };
            }
            if fresh {
                repo::insert(tx, &a)?;
            } else {
                repo::update(tx, &a)?;
            }
            base::audit(
                tx,
                ws,
                me,
                if fresh {
                    "a2a_agent.create"
                } else {
                    "a2a_agent.update"
                },
                &a.id,
                &json!({"name": a.name, "card_url": a.card_url, "auth": a.auth}),
            )?;
            if let (true, Some(bot)) = (fresh, b.bot.as_deref()) {
                super::assign_in(tx, ws, me, bot, "a2a", &a.id, true)?;
            }
            super::publish_users(tx, "a2a", &a.id)?;
            out_with_users(tx, &a)
        })
    }

    pub fn a2a_delete(&self, ws: &str, me: &str, id: &str) -> VaultResult<()> {
        self.vault.store().transaction(|tx| {
            let old = agent_of(tx, ws, id)?;
            if let Some(item) = &old.credential_item {
                self.vault.destroy_item(tx, ws, me, item)?;
            }
            super::publish_users(tx, "a2a", id)?;
            repo::delete(tx, id)?;
            base::audit(
                tx,
                ws,
                me,
                "a2a_agent.delete",
                id,
                &json!({"name": old.name}),
            )?;
            Ok::<_, VaultError>(())
        })
    }

    /// The tools of the remote agents the Bot of `session` has switched on.
    pub fn a2a_tools(&self, session: &str) -> Vec<Value> {
        self.vault
            .store()
            .reader(|c| repo::enabled_for_session(c, session))
            .unwrap_or_default()
            .iter()
            .map(tool)
            .collect()
    }

    /// Runs `steve_a2a_<name>` for `session`: sends the agent the message (or asks about a task)
    /// and waits for its result, at most [`TASK_WAIT`]. `Err` is the text the agent reads.
    pub async fn a2a_call(
        &self,
        session: &str,
        name: &str,
        args: &Value,
    ) -> Result<String, String> {
        let key = name.strip_prefix(TOOL_PREFIX).unwrap_or("");
        let (store, session_id, key_s) = (
            self.vault.store().clone(),
            session.to_string(),
            key.to_string(),
        );
        let found = tokio::task::spawn_blocking(move || {
            store.reader(|c| {
                let bot = steve_store::caps::bot_of_session(c, &session_id)?;
                let agent = repo::enabled_for_session(c, &session_id)?
                    .into_iter()
                    .find(|a| a.name == key_s);
                Ok::<_, steve_store::StoreError>((agent, bot))
            })
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let (Some(agent), bot) = found else {
            return Err(format!("MCP error -32602: Tool {name} not found"));
        };
        let obj = args.as_object().cloned().unwrap_or_default();
        if obj
            .keys()
            .any(|k| !["message", "context_id", "task_id"].contains(&k.as_str()))
        {
            return Err("Invalid arguments: unexpected property.".into());
        }
        let text = |k: &str| {
            obj.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let (message, context, task) = (text("message"), text("context_id"), text("task_id"));
        if message.is_none() && task.is_none() {
            return Err(
                "Invalid arguments: message is required (or task_id to check a task).".into(),
            );
        }
        let endpoint = agent
            .endpoint()
            .ok_or("This agent's card names no endpoint.")?;
        crate::net::check_url(&endpoint).map_err(|e| e.to_string())?;
        // the secret, decrypted for this call only
        let auth: Option<(String, Zeroizing<String>)> = match (&agent.auth, &agent.credential_item)
        {
            (Auth::None, _) => None,
            (_, None) => {
                return Err(
                    "This agent needs a secret Steve does not have; add it in 能力 → A2A.".into(),
                );
            }
            (scheme, Some(item)) => {
                let (vault, ws, item, id) = (
                    self.vault.clone(),
                    agent.workspace_id.clone(),
                    item.clone(),
                    agent.id.clone(),
                );
                let purpose = format!("A2A {} · {}", agent.name, bot.unwrap_or_default());
                let v = tokio::task::spawn_blocking(move || {
                    vault.with_credential(&ws, &item, &id, &purpose, |c| {
                        c.text(SECRET_LABEL).map(|t| Zeroizing::new(t.to_string()))
                    })
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?
                .ok_or("The agent's secret is missing from the vault.")?;
                Some(match scheme {
                    Auth::Header { header } => (header.clone(), v),
                    _ => (
                        "authorization".to_string(),
                        Zeroizing::new(format!("Bearer {}", v.as_str())),
                    ),
                })
            }
        };
        let title = agent.title();
        let started = Instant::now();
        let mut result = match message {
            Some(m) => {
                let mut msg = json!({
                    "kind": "message", "role": "user", "messageId": uid("msg"),
                    "parts": [{"kind": "text", "text": m}],
                });
                if let Some(c) = &context {
                    msg["contextId"] = json!(c);
                }
                if let Some(t) = &task {
                    msg["taskId"] = json!(t);
                }
                self.rpc(&endpoint, auth.as_ref(), "message/send", json!({
                    "message": msg,
                    "configuration": {"blocking": true, "acceptedOutputModes": ["text/plain", "text/markdown", "application/json"]},
                }), TASK_WAIT).await?
            }
            None => {
                let id = task.clone().unwrap_or_default();
                self.rpc(
                    &endpoint,
                    auth.as_ref(),
                    "tasks/get",
                    json!({"id": id}),
                    Duration::from_secs(30),
                )
                .await?
            }
        };
        // a task still running is asked about until it settles or the wait is over
        while result["kind"] == "task"
            && !settled(
                result
                    .pointer("/status/state")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
            && started.elapsed() + POLL < TASK_WAIT
        {
            tokio::time::sleep(POLL).await;
            let id = result["id"].as_str().unwrap_or("").to_string();
            result = self
                .rpc(
                    &endpoint,
                    auth.as_ref(),
                    "tasks/get",
                    json!({"id": id}),
                    Duration::from_secs(30),
                )
                .await?;
        }
        Ok(describe(&title, &result))
    }

    async fn rpc(
        &self,
        endpoint: &str,
        auth: Option<&(String, Zeroizing<String>)>,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": uid("msg"), "method": method, "params": params});
        let mut r = self
            .http
            .post(endpoint)
            .header("accept", "application/json")
            .json(&body)
            .timeout(wait);
        if let Some((h, v)) = auth {
            r = r.header(h.as_str(), v.as_str());
        }
        let r = r
            .send()
            .await
            .map_err(|e| format!("The agent did not answer: {e}"))?;
        let status = r.status();
        let v = read_json(r, MAX_ANSWER).await.map_err(|e| {
            format!(
                "The agent's answer (HTTP {}) is not JSON-RPC: {e}",
                status.as_u16()
            )
        })?;
        if let Some(e) = v.get("error") {
            return Err(format!(
                "The agent refused ({}): {}",
                e["code"],
                e["message"].as_str().unwrap_or("")
            ));
        }
        v.get("result").cloned().ok_or_else(|| {
            format!(
                "The agent's answer (HTTP {}) has no result.",
                status.as_u16()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_comes_from_the_card() {
        assert_eq!(
            suggested_name("Travel Planner Agent"),
            "travel_planner_agent"
        );
        assert_eq!(suggested_name("  ** "), "agent");
        assert_eq!(suggested_name("报销助手 v2"), "v2");
        assert!(suggested_name(&"x".repeat(80)).len() <= 40);
    }

    #[test]
    fn a_host_alone_is_looked_up_at_the_well_known_path() {
        assert_eq!(
            card_candidates("https://agent.example.com").unwrap(),
            [
                "https://agent.example.com/.well-known/agent-card.json",
                "https://agent.example.com/.well-known/agent.json"
            ]
        );
        assert_eq!(
            card_candidates("https://a.example.com/cards/x.json").unwrap(),
            ["https://a.example.com/cards/x.json"]
        );
        assert!(card_candidates("ftp://a.example.com").is_err());
    }

    #[test]
    fn a_task_is_described_with_its_ids_and_what_to_do_next() {
        let t = json!({"kind": "task", "id": "t1", "contextId": "c1",
            "status": {"state": "input-required", "message": {"parts": [{"kind": "text", "text": "Which city?"}]}}});
        let s = describe("Planner", &t);
        assert!(
            s.contains("task_id t1") && s.contains("context_id c1") && s.contains("Which city?"),
            "{s}"
        );
        assert!(s.contains("waiting for more"), "{s}");
        let done = json!({"kind": "task", "id": "t2", "contextId": "c1", "status": {"state": "completed"},
            "artifacts": [{"name": "plan", "parts": [{"kind": "text", "text": "Day 1"}]}]});
        let s = describe("Planner", &done);
        assert!(
            s.contains("Artifact plan:\nDay 1") && !s.contains("still running"),
            "{s}"
        );
        let m = json!({"kind": "message", "contextId": "c9", "parts": [{"kind": "text", "text": "hi"}]});
        assert_eq!(describe("P", &m), "P replied (context_id c9):\n\nhi");
    }
}

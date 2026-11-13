//! A Bot's own MCP servers: what its harness is given in the ACP `mcpServers` of every session,
//! next to Steve's own tools, the same way for every harness (migration 62, `steve_store::bot_mcp`).
//!
//! - `GET /bots/:id/mcp-servers` → `McpServerOut[]`
//! - `POST /bots/:id/mcp-servers {name, transport: stdio|http|sse, command?, args?, env?, url?,
//!   headers?, enabled?}` → `McpServerOut`
//! - `PATCH /bots/:id/mcp-servers/:sid {…any of those}` → `McpServerOut` (`{enabled}` alone switches it)
//! - `DELETE /bots/:id/mcp-servers/:sid` → `{ok:true}`
//!
//! `env` (stdio) and `headers` (http, sse) are `[{name, value?, secret?}]`. A secret's value goes into
//! the workspace's system vault, one `connection_credential` item per server (fields
//! `env:<name>` / `header:<name>`), and is never stored with the server, answered, logged or audited;
//! answers show it as `{name, secret: true}`. Saving a secret again without a value keeps the one
//! the vault has. The session's Node takes the values when its harness opens ([`BotMcp::secret`]).
//!
//! Every change is audited (`bot_mcp.create|update|enable|disable|delete`, names only) and publishes
//! the Bot's change event. A change reaches a session the next time its harness opens. The Hub
//! hands a session's servers to its Node (`steve_hub::wire::bot_mcp`), secrets as [`secret_id`]s.

use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::json;
use steve_store::bot_mcp::{self as repo, Server, Var};
use steve_store::{ChangeEvent, Tx, base, now, uid};
use zeroize::Zeroizing;

use crate::vault::{Vault, VaultError, VaultResult};
use crate::{ApiError, Body, Caller, Check};

/// Servers per Bot, variables per server, arguments per command.
const MAX_SERVERS: usize = 32;
const MAX_VARS: usize = 64;
const MAX_ARGS: usize = 64;

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{1,64}$").unwrap());
static ENV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,127}$").unwrap());
/// An HTTP header name (RFC 9110 token).
static HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,128}$").unwrap());

/// One variable as it comes in: a secret without a value keeps the stored one.
#[derive(Debug, Clone, Deserialize)]
pub struct VarIn {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

/// A new server, or the fields a `PATCH` changes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerIn {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<Vec<VarIn>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<Vec<VarIn>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// The service: the store through the vault, which keeps the secret values.
#[derive(Clone)]
pub struct BotMcp {
    vault: Vault,
}

pub fn routes<S: Clone + Send + Sync + 'static>(svc: BotMcp) -> Router<S> {
    Router::new()
        .route("/bots/{id}/mcp-servers", get(list).post(create))
        .route("/bots/{id}/mcp-servers/{sid}", patch(update).delete(remove))
        .with_state(svc)
}

async fn list(
    State(s): State<BotMcp>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Response> {
    let svc = s.clone();
    let out = s.vault.blocking(move |_| svc.list(&who.ws, &id)).await?;
    Ok(Json(out).into_response())
}

async fn create(
    State(s): State<BotMcp>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<ServerIn>,
) -> VaultResult<Response> {
    let svc = s.clone();
    let out = s
        .vault
        .blocking(move |_| svc.create(&who.ws, &who.me, &id, b))
        .await?;
    Ok(Json(out).into_response())
}

async fn update(
    State(s): State<BotMcp>,
    who: Caller,
    Path((id, sid)): Path<(String, String)>,
    Body(b): Body<ServerIn>,
) -> VaultResult<Response> {
    let svc = s.clone();
    let out = s
        .vault
        .blocking(move |_| svc.update(&who.ws, &who.me, &id, &sid, b))
        .await?;
    Ok(Json(out).into_response())
}

async fn remove(
    State(s): State<BotMcp>,
    who: Caller,
    Path((id, sid)): Path<(String, String)>,
) -> VaultResult<Response> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.delete(&who.ws, &who.me, &id, &sid))
        .await?;
    Ok(Json(json!({ "ok": true })).into_response())
}

/// The Bot, if `ws` sees it (its home or one it serves), not deleted.
fn own_bot(c: &Connection, ws: &str, bot: &str) -> Result<(), ApiError> {
    let seen: Option<i64> = c
        .query_row(
            "SELECT 1 FROM bots b WHERE b.id = ?1 AND b.deleted = 0 AND (b.workspace_id = ?2
               OR EXISTS (SELECT 1 FROM bot_workspaces w WHERE w.bot_id = b.id AND w.workspace_id = ?2))",
            [bot, ws],
            |r| r.get(0),
        )
        .optional()
        .map_err(ApiError::from)?;
    seen.map(|_| ()).ok_or_else(|| ApiError::not_found("Bot"))
}

fn server_of(c: &Connection, bot: &str, sid: &str) -> VaultResult<Server> {
    repo::get(c, sid)?
        .filter(|s| s.bot_id == bot)
        .ok_or_else(|| ApiError::not_found("MCP 服务器").into())
}

/// What the page sees: a secret by name only.
fn out(s: &Server) -> serde_json::Value {
    json!({
        "id": s.id, "bot_id": s.bot_id, "name": s.name, "transport": s.transport,
        "command": s.command, "args": s.args, "env": s.env, "url": s.url, "headers": s.headers,
        "enabled": s.enabled, "created": s.created, "updated": s.updated,
    })
}

/// The id a secret is taken by on the Node: `<server>#<label>`.
pub fn secret_id(server: &str, label: &str) -> String {
    format!("{server}#{label}")
}

/// Checks the merged server; `Err` names every bad field.
fn check(s: &Server, vars_in: &[VarIn]) -> Result<(), ApiError> {
    let mut c = Check::new();
    c.pattern("name", &s.name, &NAME);
    c.that("name", !s.name.eq_ignore_ascii_case("steve"));
    c.one_of("transport", &s.transport, repo::TRANSPORTS);
    let (field, re) = if s.transport == "stdio" {
        let cmd = s.command.as_deref().unwrap_or("");
        c.that(
            "command",
            !cmd.trim().is_empty() && cmd.chars().count() <= 1000,
        );
        c.that("args", s.args.len() <= MAX_ARGS);
        for (i, a) in s.args.iter().enumerate() {
            c.that(
                &format!("args.{i}"),
                a.chars().count() <= 4000 && !a.contains('\0'),
            );
        }
        ("env", &*ENV)
    } else {
        let u = s.url.as_deref().unwrap_or("");
        let ok = u.chars().count() <= 2000
            && url::Url::parse(u)
                .is_ok_and(|x| matches!(x.scheme(), "http" | "https") && x.host().is_some());
        c.that("url", ok);
        ("headers", &*HEADER)
    };
    c.that(field, vars_in.len() <= MAX_VARS);
    let mut seen: Vec<String> = vec![];
    for (i, v) in vars_in.iter().enumerate() {
        let key = if field == "env" {
            v.name.clone()
        } else {
            v.name.to_ascii_lowercase()
        };
        c.that(
            &format!("{field}.{i}.name"),
            re.is_match(&v.name) && !seen.contains(&key),
        );
        seen.push(key);
        let value_ok = v
            .value
            .as_deref()
            .is_none_or(|x| x.chars().count() <= 8000 && !x.contains(['\n', '\r', '\0']));
        c.that(
            &format!("{field}.{i}.value"),
            value_ok && (v.secret || v.value.is_some()),
        );
    }
    c.done()
}

impl BotMcp {
    pub fn new(vault: Vault) -> Self {
        BotMcp { vault }
    }

    pub fn list(&self, ws: &str, bot: &str) -> VaultResult<Vec<serde_json::Value>> {
        self.vault.store().reader(|c| {
            own_bot(c, ws, bot)?;
            Ok::<_, VaultError>(repo::list(c, bot)?.iter().map(out).collect())
        })
    }

    pub fn create(
        &self,
        ws: &str,
        me: &str,
        bot: &str,
        b: ServerIn,
    ) -> VaultResult<serde_json::Value> {
        let count = self.vault.store().reader(|c| {
            own_bot(c, ws, bot)?;
            Ok::<_, VaultError>(repo::list(c, bot)?.len())
        })?;
        let at = now();
        let blank = Server {
            id: uid("bmcp"),
            workspace_id: ws.into(),
            bot_id: bot.into(),
            name: String::new(),
            transport: String::new(),
            command: None,
            args: vec![],
            env: vec![],
            url: None,
            headers: vec![],
            credential_item: None,
            enabled: true,
            created: at,
            updated: at,
        };
        if b.name.is_none() || b.transport.is_none() {
            let mut c = Check::new();
            c.that("name", b.name.is_some());
            c.that("transport", b.transport.is_some());
            c.done()?;
        }
        self.save(me, bot, blank, b, count < MAX_SERVERS, true)
    }

    pub fn update(
        &self,
        ws: &str,
        me: &str,
        bot: &str,
        sid: &str,
        b: ServerIn,
    ) -> VaultResult<serde_json::Value> {
        let old = self.vault.store().reader(|c| {
            own_bot(c, ws, bot)?;
            server_of(c, bot, sid)
        })?;
        // only the switch: no vault work, its own audit verb
        let only_switch = b.name.is_none()
            && b.transport.is_none()
            && b.command.is_none()
            && b.args.is_none()
            && b.env.is_none()
            && b.url.is_none()
            && b.headers.is_none();
        if let (true, Some(on)) = (only_switch, b.enabled) {
            let at = now();
            return self.vault.store().transaction(|tx| {
                repo::set_enabled(tx, sid, on, at)?;
                let verb = if on {
                    "bot_mcp.enable"
                } else {
                    "bot_mcp.disable"
                };
                base::audit(
                    tx,
                    ws,
                    me,
                    verb,
                    sid,
                    &json!({"bot": bot, "name": old.name}),
                )?;
                tx.publish(ChangeEvent::Bot { id: bot.into() });
                let s = server_of(tx, bot, sid)?;
                Ok::<_, VaultError>(out(&s))
            });
        }
        self.save(me, bot, old, b, true, false)
    }

    pub fn delete(&self, ws: &str, me: &str, bot: &str, sid: &str) -> VaultResult<()> {
        let old = self.vault.store().reader(|c| {
            own_bot(c, ws, bot)?;
            server_of(c, bot, sid)
        })?;
        self.vault.store().transaction(|tx| {
            if let Some(item) = &old.credential_item {
                self.vault.destroy_item(tx, &old.workspace_id, me, item)?;
            }
            repo::delete(tx, sid)?;
            base::audit(
                tx,
                ws,
                me,
                "bot_mcp.delete",
                sid,
                &json!({"bot": bot, "name": old.name, "transport": old.transport}),
            )?;
            tx.publish(ChangeEvent::Bot { id: bot.into() });
            Ok::<_, VaultError>(())
        })
    }

    /// Merges `b` into `old`, checks it, keeps the secrets in the vault and writes the row, all in
    /// one transaction (a new credential item replaces the old one when the secrets changed).
    fn save(
        &self,
        me: &str,
        bot: &str,
        old: Server,
        b: ServerIn,
        room: bool,
        fresh: bool,
    ) -> VaultResult<serde_json::Value> {
        let mut s = old.clone();
        if let Some(n) = b.name {
            s.name = n.trim().to_string();
        }
        if let Some(t) = b.transport {
            s.transport = t;
        }
        let stdio = s.transport == "stdio";
        if b.command.is_some() {
            s.command = b.command.map(|x| x.trim().to_string());
        }
        if let Some(a) = b.args {
            s.args = a;
        }
        if b.url.is_some() {
            s.url = b.url.map(|x| x.trim().to_string());
        }
        if let Some(e) = b.enabled {
            s.enabled = e;
        }
        // the variables as given, else the stored ones (a secret's value stays in the vault)
        let given = if stdio { b.env } else { b.headers };
        let kept = |vs: &[Var]| -> Vec<VarIn> {
            vs.iter()
                .map(|v| VarIn {
                    name: v.name.clone(),
                    value: v.value.clone(),
                    secret: v.secret,
                })
                .collect()
        };
        let vars_in = match given {
            Some(v) => v,
            None if old.transport == s.transport => kept(old.vars()),
            None => vec![],
        };
        // the other transport's fields go
        if stdio {
            s.url = None;
            s.headers = vec![];
        } else {
            s.command = None;
            s.args = vec![];
            s.env = vec![];
        }
        let name_ok = self
            .vault
            .store()
            .reader(|c| Ok::<_, VaultError>(!repo::name_taken(c, bot, &s.name, Some(&s.id))?))?;
        if !room {
            return Err(ApiError::rule(steve_i18n::t!(
                "modules.bot_mcp.too_many",
                max = MAX_SERVERS
            ))
            .into());
        }
        check(&s, &vars_in)?;
        if !name_ok {
            return Err(ApiError::conflict(steve_i18n::t!(
                "modules.bot_mcp.name_taken",
                name = s.name
            ))
            .into());
        }
        // the secret values: new ones as given, the others from the old item
        let old_labels = old.secret_labels();
        let labels: Vec<String> = vars_in
            .iter()
            .filter(|v| v.secret)
            .map(|v| repo::secret_label(&s.transport, &v.name))
            .collect();
        for (v, l) in vars_in.iter().filter(|v| v.secret).zip(&labels) {
            if v.value.is_none() && !old_labels.contains(l) {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.bot_mcp.secret_needed",
                    name = v.name
                ))
                .into());
            }
        }
        let vars: Vec<Var> = vars_in
            .iter()
            .map(|v| Var {
                name: v.name.clone(),
                value: if v.secret { None } else { v.value.clone() },
                secret: v.secret,
            })
            .collect();
        if stdio {
            s.env = vars;
        } else {
            s.headers = vars;
        }
        let unchanged =
            labels == old_labels && vars_in.iter().all(|v| !v.secret || v.value.is_none());
        let fields: Option<Vec<(String, Zeroizing<String>)>> = if unchanged || labels.is_empty() {
            None
        } else {
            let kept: Vec<(String, Zeroizing<String>)> = match &old.credential_item {
                Some(item) if !old_labels.is_empty() => {
                    self.vault
                        .with_credential_unlogged(&old.workspace_id, item, |c| {
                            old_labels
                                .iter()
                                .filter_map(|l| {
                                    c.text(l)
                                        .map(|t| (l.clone(), Zeroizing::new(t.to_string())))
                                })
                                .collect()
                        })?
                }
                _ => vec![],
            };
            let mut f = vec![];
            for (v, l) in vars_in.iter().filter(|v| v.secret).zip(&labels) {
                let value = match &v.value {
                    Some(x) => Zeroizing::new(x.clone()),
                    None => kept
                        .iter()
                        .find(|(k, _)| k == l)
                        .map(|(_, x)| x.clone())
                        .ok_or_else(|| {
                            ApiError::rule(steve_i18n::t!(
                                "modules.bot_mcp.secret_needed",
                                name = v.name
                            ))
                        })?,
                };
                f.push((l.clone(), value));
            }
            Some(f)
        };
        let prepared = match fields {
            Some(f) => Some(self.vault.prepare_credential(
                &s.workspace_id,
                me,
                &format!("MCP {} · {bot}", s.name),
                None,
                f,
            )?),
            None => None,
        };
        s.updated = now();
        let ws = s.workspace_id.clone();
        self.vault.store().transaction(|tx: &Tx| {
            let drop_old = |tx: &Tx| -> VaultResult<()> {
                if let Some(item) = &old.credential_item {
                    self.vault.destroy_item(tx, &ws, me, item)?;
                }
                Ok(())
            };
            if let Some(p) = &prepared {
                drop_old(tx)?;
                s.credential_item = Some(self.vault.insert_prepared(tx, p)?);
            } else if labels.is_empty() {
                drop_old(tx)?;
                s.credential_item = None;
            }
            if fresh {
                repo::insert(tx, &s)?;
            } else {
                repo::update(tx, &s)?;
            }
            base::audit(
                tx,
                &ws,
                me,
                if fresh {
                    "bot_mcp.create"
                } else {
                    "bot_mcp.update"
                },
                &s.id,
                &json!({"bot": bot, "name": s.name, "transport": s.transport,
                        "enabled": s.enabled, "secrets": labels}),
            )?;
            tx.publish(ChangeEvent::Bot { id: bot.into() });
            Ok::<_, VaultError>(())
        })?;
        Ok(out(&s))
    }

    /// The value the Node of `session` takes for `id` (`<server>#<label>`) as its harness opens: a
    /// secret of an enabled server of the session's Bot. The caller has checked the session is on
    /// that Node. Each use is in the vault's use log (once per server and day).
    pub fn secret(&self, session: &str, id: &str) -> VaultResult<Zeroizing<Vec<u8>>> {
        let denied = || VaultError::from(ApiError::not_found("MCP 服务器"));
        let (sid, label) = id.split_once('#').ok_or_else(denied)?;
        let (s, bot) = self.vault.store().reader(|c| {
            let bot = repo::bot_of_session(c, session)?;
            Ok::<_, VaultError>((repo::get(c, sid)?, bot))
        })?;
        let s = s
            .filter(|s| s.enabled && Some(&s.bot_id) == bot.as_ref())
            .ok_or_else(denied)?;
        if !s.secret_labels().iter().any(|l| l == label) {
            return Err(denied());
        }
        let item = s.credential_item.as_deref().ok_or_else(denied)?;
        let purpose = format!("MCP {} · {}", s.name, s.bot_id);
        self.vault
            .with_credential(&s.workspace_id, item, &s.id, &purpose, |c| {
                c.get(label).map(|v| Zeroizing::new(v.expose().to_vec()))
            })?
            .ok_or_else(denied)
    }
}

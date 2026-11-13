//! The library's MCP servers: what the harness of a Bot assigned one gets in the ACP `mcpServers` of
//! every session, next to Steve's own tools, the same way for every harness (`steve_store::caps::mcp`).
//!
//! - `GET /capabilities/mcp-servers` → `McpServerOut[]` (each with `bots: [{id, enabled}]`)
//! - `POST /capabilities/mcp-servers {name, transport: stdio|http|sse, command?, args?, env?, url?,
//!   headers?, bot?}` → `McpServerOut` (`bot`: also assign it to that Bot)
//! - `PATCH /capabilities/mcp-servers/:id {…any of those}` → `McpServerOut`
//! - `DELETE /capabilities/mcp-servers/:id` → `{ok:true}` (off every Bot that used it)
//!
//! `env` (stdio) and `headers` (http, sse) are `[{name, value?, secret?}]`. A secret's value goes into
//! the workspace's system vault, one `connection_credential` item per server (fields
//! `env:<name>` / `header:<name>`), and is never stored with the server, answered, logged or audited;
//! answers show it as `{name, secret: true}`. Saving a secret again without a value keeps the one
//! the vault has. The session's Node takes the values when its harness opens
//! ([`Capabilities::mcp_secret`]).
//!
//! Every change is audited (`mcp_server.create|update|delete`, names only) and publishes the change
//! event of every Bot that uses the server. The Hub hands a session's servers to its Node
//! (`steve_hub::wire::bot_mcp`), secrets as [`secret_id`]s.

use std::sync::LazyLock;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use regex::Regex;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;
use steve_store::caps::mcp::{self as repo, Server, Var};
use steve_store::{Tx, base, now, uid};
use zeroize::Zeroizing;

use super::Capabilities;
use crate::vault::{VaultError, VaultResult};
use crate::{ApiError, Body, Caller, Check};

/// Variables per server, arguments per command.
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
    /// On create: also assign it to this Bot.
    #[serde(default)]
    pub bot: Option<String>,
}

pub(super) async fn list_h(State(s): State<Capabilities>, who: Caller) -> VaultResult<Response> {
    let svc = s.clone();
    let out = s.vault.blocking(move |_| svc.mcp_list(&who.ws)).await?;
    Ok(Json(out).into_response())
}

pub(super) async fn create_h(
    State(s): State<Capabilities>,
    who: Caller,
    Body(b): Body<ServerIn>,
) -> VaultResult<Response> {
    let svc = s.clone();
    let out = s
        .vault
        .blocking(move |_| svc.mcp_create(&who.ws, &who.me, b))
        .await?;
    Ok(Json(out).into_response())
}

pub(super) async fn update_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(sid): Path<String>,
    Body(b): Body<ServerIn>,
) -> VaultResult<Response> {
    let svc = s.clone();
    let out = s
        .vault
        .blocking(move |_| svc.mcp_update(&who.ws, &who.me, &sid, b))
        .await?;
    Ok(Json(out).into_response())
}

pub(super) async fn remove_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(sid): Path<String>,
) -> VaultResult<Response> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.mcp_delete(&who.ws, &who.me, &sid))
        .await?;
    Ok(Json(json!({ "ok": true })).into_response())
}

fn server_of(c: &Connection, ws: &str, sid: &str) -> VaultResult<Server> {
    repo::get(c, sid)?
        .filter(|s| s.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("MCP 服务器").into())
}

/// What the page sees: a secret by name only.
pub(crate) fn out(s: &Server) -> serde_json::Value {
    json!({
        "id": s.id, "name": s.name, "transport": s.transport,
        "command": s.command, "args": s.args, "env": s.env, "url": s.url, "headers": s.headers,
        "created": s.created, "updated": s.updated,
    })
}

/// [`out`] with the Bots that use it.
fn out_with_users(c: &Connection, s: &Server) -> VaultResult<serde_json::Value> {
    let mut v = out(s);
    v["bots"] = super::users(c, "mcp", &s.id)?;
    Ok(v)
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

impl Capabilities {
    pub fn mcp_list(&self, ws: &str) -> VaultResult<Vec<serde_json::Value>> {
        self.vault.store().reader(|c| {
            repo::list(c, ws)?
                .iter()
                .map(|s| out_with_users(c, s))
                .collect()
        })
    }

    pub fn mcp_create(&self, ws: &str, me: &str, b: ServerIn) -> VaultResult<serde_json::Value> {
        let count = self
            .vault
            .store()
            .reader(|c| Ok::<_, VaultError>(repo::list(c, ws)?.len()))?;
        let at = now();
        let blank = Server {
            id: uid("bmcp"),
            workspace_id: ws.into(),
            name: String::new(),
            transport: String::new(),
            command: None,
            args: vec![],
            env: vec![],
            url: None,
            headers: vec![],
            credential_item: None,
            created: at,
            updated: at,
        };
        if b.name.is_none() || b.transport.is_none() {
            let mut c = Check::new();
            c.that("name", b.name.is_some());
            c.that("transport", b.transport.is_some());
            c.done()?;
        }
        self.mcp_save(me, blank, b, count < super::MAX_LIBRARY, true)
    }

    pub fn mcp_update(
        &self,
        ws: &str,
        me: &str,
        sid: &str,
        b: ServerIn,
    ) -> VaultResult<serde_json::Value> {
        let old = self.vault.store().reader(|c| server_of(c, ws, sid))?;
        self.mcp_save(me, old, b, true, false)
    }

    pub fn mcp_delete(&self, ws: &str, me: &str, sid: &str) -> VaultResult<()> {
        let old = self.vault.store().reader(|c| server_of(c, ws, sid))?;
        self.vault.store().transaction(|tx| {
            if let Some(item) = &old.credential_item {
                self.vault.destroy_item(tx, &old.workspace_id, me, item)?;
            }
            super::publish_users(tx, "mcp", sid)?;
            repo::delete(tx, sid)?;
            base::audit(
                tx,
                ws,
                me,
                "mcp_server.delete",
                sid,
                &json!({"name": old.name, "transport": old.transport}),
            )?;
            Ok::<_, VaultError>(())
        })
    }

    /// Merges `b` into `old`, checks it, keeps the secrets in the vault and writes the row, all in
    /// one transaction (a new credential item replaces the old one when the secrets changed).
    fn mcp_save(
        &self,
        me: &str,
        old: Server,
        b: ServerIn,
        room: bool,
        fresh: bool,
    ) -> VaultResult<serde_json::Value> {
        let assign_to = b.bot.clone();
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
        let name_ok = self.vault.store().reader(|c| {
            Ok::<_, VaultError>(!repo::name_taken(c, &s.workspace_id, &s.name, Some(&s.id))?)
        })?;
        if !room {
            return Err(ApiError::rule(steve_i18n::t!(
                "modules.caps.library_full",
                max = super::MAX_LIBRARY
            ))
            .into());
        }
        check(&s, &vars_in)?;
        if !name_ok {
            return Err(ApiError::conflict(steve_i18n::t!(
                "modules.caps.mcp_name_taken",
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
                    "modules.caps.secret_needed",
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
                                "modules.caps.secret_needed",
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
                &format!("MCP {}", s.name),
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
            // a Bot that uses it must not see two servers of one name
            for a in steve_store::caps::of_entry(tx, "mcp", &s.id)? {
                if super::clashes(tx, &a.bot_id, "mcp", &s.id, &s.name)? {
                    return Err(ApiError::conflict(steve_i18n::t!(
                        "modules.caps.name_used",
                        name = s.name
                    ))
                    .into());
                }
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
                    "mcp_server.create"
                } else {
                    "mcp_server.update"
                },
                &s.id,
                &json!({"name": s.name, "transport": s.transport, "secrets": labels}),
            )?;
            if let (true, Some(bot)) = (fresh, assign_to.as_deref()) {
                super::assign_in(tx, &ws, me, bot, "mcp", &s.id, true)?;
            }
            super::publish_users(tx, "mcp", &s.id)?;
            out_with_users(tx, &s)
        })
    }

    /// The value the Node of `session` takes for `id` (`<server>#<label>`) as its harness opens: a
    /// secret of a server the session's Bot has switched on. The caller has checked the session is
    /// on that Node. Each use is in the vault's use log (once per server and day).
    pub fn mcp_secret(&self, session: &str, id: &str) -> VaultResult<Zeroizing<Vec<u8>>> {
        let denied = || VaultError::from(ApiError::not_found("MCP 服务器"));
        let (sid, label) = id.split_once('#').ok_or_else(denied)?;
        let (s, on, bot) = self.vault.store().reader(|c| {
            let bot = steve_store::caps::bot_of_session(c, session)?;
            let on = match &bot {
                Some(b) => steve_store::caps::is_enabled(c, b, "mcp", sid)?,
                None => false,
            };
            Ok::<_, VaultError>((repo::get(c, sid)?, on, bot))
        })?;
        let s = s.filter(|_| on).ok_or_else(denied)?;
        if !s.secret_labels().iter().any(|l| l == label) {
            return Err(denied());
        }
        let item = s.credential_item.as_deref().ok_or_else(denied)?;
        let purpose = format!("MCP {} · {}", s.name, bot.unwrap_or_default());
        self.vault
            .with_credential(&s.workspace_id, item, &s.id, &purpose, |c| {
                c.get(label).map(|v| Zeroizing::new(v.expose().to_vec()))
            })?
            .ok_or_else(denied)
    }
}

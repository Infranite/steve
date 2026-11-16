//! Model access for service sessions (#222), and the preflight that says, before anything is
//! created, whether a principal's Bot can run at all.
//!
//! A session of a workspace's service principal (`svc:<ws>`: a schedule, a flow, work nobody in
//! particular asked for) has a harness directory of its own that starts empty: it never inherits
//! the Node owner's personal configuration or logins. A harness pointed nowhere falls back to its
//! own default login and cannot open a session. The Hub's **model access setting** says, per
//! harness, which provider (or gateway) endpoint such sessions use and which vault field holds its
//! key: `settings(workspace 'default', key [`KEY`])`, edited by the instance owner only. The key's
//! item is in the Hub's own workspace, in its shared or system vault (never a personal one, and
//! never a high-sensitivity item): a service session does not run on anyone's personal account.
//!
//! When such a session opens, its `open_session` carries the endpoint and a reference
//! ([`steve_node_api::ModelAccess`]); the Node writes the harness's native provider file (the
//! adapter's declared [`steve_acp::ModelProvider`]) into the session's directory and takes the key
//! from the Hub under `model:<session>` ([`ModelSecrets`]) into that one process's environment.
//! The key is never in the Node's journal, the command ledger or a file.
//!
//! [`Preflight::can_run`] is the deterministic check `can_run(principal, bot, harness)`: the Bot's
//! harness is ready on the Nodes, and for a service principal whose harness Steve can point at a
//! provider, the setting names one and its key can be found in the vault. It runs when a schedule
//! is created or edited and when a Bot is given an item or a schedule (refused, saying why and
//! where to fix it), when a schedule fires (no item is made; the run records the reason), and
//! before a session is opened (no session; one clear error on the item, no retry). It reads only
//! the database and what the Nodes last reported; it decrypts nothing.

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{StoreError, Tx, WS, now};

use super::principal::{self, Principal};

/// The setting's row in `settings` (workspace `default`: the Hub's own).
pub const KEY: &str = "model_access.v1";

/// What the setting holds: per harness id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    #[serde(default)]
    pub harnesses: BTreeMap<String, Entry>,
}

/// One harness's provider for service sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The provider's or gateway's endpoint (`http(s)://`).
    pub base_url: String,
    /// The vault item (of the Hub's own workspace) holding the key, and the field's label.
    pub item: String,
    pub field: String,
    #[serde(default)]
    pub updated_by: String,
    #[serde(default)]
    pub updated: i64,
}

/// The setting as stored (none: empty).
pub fn load(c: &Connection) -> Result<Setting, StoreError> {
    let raw: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id=? AND key=?",
            params![WS, KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

fn save(tx: &Tx, s: &Setting) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO settings(workspace_id,key,value,updated) VALUES (?,?,?,?)
         ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value, updated=excluded.updated",
        params![WS, KEY, serde_json::to_string(s)?, now()],
    )?;
    Ok(())
}

/// A usable endpoint: `http(s)://`, one line, no spaces.
pub fn url_ok(url: &str) -> bool {
    steve_node_api::ModelAccess {
        base_url: url.into(),
        secret: "x".into(),
    }
    .valid()
}

/// What `session`'s `open_session` carries (#222): the endpoint and the key's reference, for a
/// service principal's session with a directory of its own and a provider in the setting for its
/// harness. The reference is the harness id: [`ModelSecrets`] resolves it again when the Node asks.
pub fn for_session_in(
    c: &Connection,
    session: &str,
    harness: &str,
    state: Option<&steve_node_api::HarnessState>,
) -> Result<Option<steve_node_api::ModelAccess>, StoreError> {
    if !state.is_some_and(|s| !s.import_owner) {
        return Ok(None);
    }
    let Some((_, Principal::Svc(_))) = principal::of_session(c, session)? else {
        return Ok(None);
    };
    Ok(load(c)?
        .harnesses
        .get(harness)
        .filter(|e| url_ok(&e.base_url))
        .map(|e| steve_node_api::ModelAccess {
            base_url: e.base_url.clone(),
            secret: harness.to_string(),
        }))
}

/// Why a principal's Bot cannot run now, and where that is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocked {
    /// `harness_not_ready`, `model_access_missing`, `credential_missing`, `credential_not_shared`.
    pub code: &'static str,
    pub bot: String,
    pub harness: String,
    /// In the request's language (built under `steve_i18n::stored` where it is stored).
    pub reason: String,
    /// Where it is fixed: `devices`, `settings.model_access` or `vault`.
    pub fix: &'static str,
}

/// The harness ids the Hub knows, with what the Nodes said of them last (`ready`) and their names.
pub type Readiness = Arc<dyn Fn() -> Vec<(String, String, Option<bool>)> + Send + Sync>;

/// What [`Preflight::can_run`] decides on: the adapters' declared properties, the Nodes' last
/// readiness reports, and whether principals get directories of their own at all (off: every
/// session uses the Node's own state, as before, and the setting is not needed).
pub struct Preflight {
    declared: BTreeMap<String, steve_acp::Declaration>,
    readiness: Option<Readiness>,
    principal_state: bool,
}

impl Preflight {
    pub fn new(
        declared: BTreeMap<String, steve_acp::Declaration>,
        readiness: Option<Readiness>,
        principal_state: bool,
    ) -> Arc<Preflight> {
        Arc::new(Preflight {
            declared,
            readiness,
            principal_state,
        })
    }

    fn props(&self, harness: &str) -> steve_acp::AdapterProperties {
        steve_acp::adapters::spec(harness, self.declared.get(harness)).properties
    }

    /// Harnesses Steve can point at a provider for service sessions: `(id, name)`.
    pub fn configurable(&self) -> Vec<(String, String)> {
        let known = match &self.readiness {
            Some(r) => r().into_iter().map(|(id, name, _)| (id, name)).collect(),
            None => vec![],
        };
        let mut out: Vec<(String, String)> = known
            .into_iter()
            .filter(|(id, _)| {
                let p = self.props(id);
                p.state.is_some() && p.model_provider.is_some()
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Whether `p`'s sessions of `harness` need the setting: a service principal with a directory
    /// of its own, of a harness that has no other way to sign in there (its declared
    /// `model_provider` is `required`). A harness that can be configured but does not need it is
    /// not stopped; a setting given for it is checked all the same.
    pub fn needs_access(&self, c: &Connection, p: &Principal, harness: &str) -> bool {
        if !matches!(p, Principal::Svc(_)) || !self.principal_state {
            return false;
        }
        let props = self.props(harness);
        let Some(provider) = props.model_provider.filter(|_| props.state.is_some()) else {
            return false;
        };
        provider.required
            || load(c)
                .map(|s| s.harnesses.contains_key(harness))
                .unwrap_or(false)
    }

    fn ready(&self, harness: &str) -> Option<bool> {
        let r = self.readiness.as_ref()?;
        r().into_iter()
            .find(|(id, _, _)| id.as_str().eq(harness))
            .and_then(|(_, _, ready)| ready)
    }

    /// `can_run(principal, bot, harness)` in `c`'s view. A Bot that is not there is not refused
    /// here (the callers' own checks say that).
    pub fn can_run(&self, c: &Connection, p: &Principal, bot: &str) -> Result<(), Blocked> {
        let harness: Option<String> = c
            .query_row(
                "SELECT harness FROM bots WHERE id=? AND deleted=0",
                [bot],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        let Some(harness) = harness else {
            return Ok(());
        };
        self.can_run_harness(c, p, bot, &harness)
    }

    /// [`Self::can_run`] with the harness known.
    pub fn can_run_harness(
        &self,
        c: &Connection,
        p: &Principal,
        bot: &str,
        harness: &str,
    ) -> Result<(), Blocked> {
        self.check(c, p, bot, harness, true)
    }

    fn check(
        &self,
        c: &Connection,
        p: &Principal,
        bot: &str,
        harness: &str,
        ready: bool,
    ) -> Result<(), Blocked> {
        let blocked = |code, reason: String, fix| Blocked {
            code,
            bot: bot.to_string(),
            harness: harness.to_string(),
            reason,
            fix,
        };
        if ready && self.ready(harness) == Some(false) {
            return Err(blocked(
                "harness_not_ready",
                steve_i18n::t!("hub.model_access.harness_not_ready", harness = harness),
                "devices",
            ));
        }
        if !self.needs_access(c, p, harness) {
            return Ok(());
        }
        let setting = load(c).unwrap_or_default();
        let Some(e) = setting.harnesses.get(harness) else {
            return Err(blocked(
                "model_access_missing",
                steve_i18n::t!("hub.model_access.missing", harness = harness),
                "settings.model_access",
            ));
        };
        credential(c, e).map_err(|(code, fix)| {
            let reason = match code {
                "credential_not_shared" => {
                    steve_i18n::t!("hub.model_access.credential_not_shared", harness = harness)
                }
                _ => steve_i18n::t!("hub.model_access.credential_missing", harness = harness),
            };
            blocked(code, reason, fix)
        })
    }

    /// [`Self::can_run_harness`] for `bot` given `item`: its principal is the item's.
    pub fn can_run_item(
        &self,
        c: &Connection,
        item: &str,
        bot: &str,
        harness: &str,
    ) -> Result<(), Blocked> {
        match principal::of_item(c, item) {
            Ok(Some((_, p))) => self.can_run_harness(c, &p, bot, harness),
            _ => Ok(()),
        }
    }

    /// Before a message opens (or reopens) `session`'s harness: its model access only (the
    /// harness was there when the session was made; a Node's passing report does not stop it).
    pub fn can_run_session(&self, c: &Connection, session: &str) -> Result<(), Blocked> {
        let row: Option<(Option<String>, String)> = c
            .query_row(
                "SELECT bot_id, harness FROM sessions WHERE id=?",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (Some((bot, harness)), Ok(Some((_, p)))) = (row, principal::of_session(c, session))
        else {
            return Ok(());
        };
        self.check(c, &p, bot.as_deref().unwrap_or(""), &harness, false)
    }

    /// For a schedule: its principal is the named requester while a member, else the service one.
    pub fn can_run_schedule(
        &self,
        c: &Connection,
        ws: &str,
        requester: Option<&str>,
        bot: &str,
    ) -> Result<(), Blocked> {
        match principal::resolve(c, ws, requester) {
            Ok(p) => self.can_run(c, &p, bot),
            Err(_) => Ok(()),
        }
    }

    /// Every workspace's service principal with each of its Bots that cannot run now (the
    /// settings', the schedules' and the devices page's list).
    pub fn blocked_all(&self, c: &Connection) -> Result<Vec<Value>, StoreError> {
        let mut q = c.prepare(
            "SELECT w.id, w.name, b.id, b.name FROM workspaces w
               JOIN bot_workspaces bw ON bw.workspace_id=w.id
               JOIN bots b ON b.id=bw.bot_id AND b.deleted=0 AND b.enabled=1
             ORDER BY w.id, b.id",
        )?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = vec![];
        for (ws, ws_name, bot, bot_name) in rows {
            let p = Principal::Svc(ws.clone());
            if let Err(b) = self.can_run(c, &p, &bot) {
                out.push(json!({
                    "workspace": ws, "workspace_name": ws_name, "principal": p.key(),
                    "bot": bot, "bot_name": bot_name, "harness": b.harness,
                    "code": b.code, "reason": b.reason, "fix": b.fix,
                }));
            }
        }
        Ok(out)
    }
}

/// Whether `e`'s key can be found: an active item of the Hub's workspace in its shared or system
/// vault, not high-sensitivity, with a secret field of that label. `Err((code, fix))`.
fn credential(c: &Connection, e: &Entry) -> Result<(), (&'static str, &'static str)> {
    let row: Option<(String, String, String, bool)> = c
        .query_row(
            "SELECT v.scope, i.status, i.sensitivity,
                    EXISTS(SELECT 1 FROM vault_fields f WHERE f.item_id=i.id AND f.label=?2
                             AND f.secret=1 AND f.value_ct IS NOT NULL)
               FROM vault_items i JOIN vaults v ON v.id=i.vault_id
              WHERE i.id=?1 AND i.workspace_id=?3",
            params![e.item, e.field, WS],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .ok()
        .flatten();
    match row {
        Some((scope, status, sensitivity, has_field)) if status == "active" => {
            if !matches!(scope.as_str(), "shared" | "system") || sensitivity == "high" {
                Err(("credential_not_shared", "vault"))
            } else if !has_field {
                Err(("credential_missing", "settings.model_access"))
            } else {
                Ok(())
            }
        }
        _ => Err(("credential_missing", "settings.model_access")),
    }
}

/// The key a Node takes for a service session under `model:<session>` (#222): asked again each
/// time the harness opens, answered only for a session bound to that Node, recorded as a service
/// principal's, of the harness named by the reference, while the setting covers it. Read from the
/// vault then (its use logged once a day), never kept.
pub struct ModelSecrets {
    pub store: steve_store::Store,
    pub vault: steve_modules::vault::Vault,
}

/// The entry whose key `node` may take for `session` under the reference `id`: the session is
/// bound to that Node, recorded as a service principal's, of the harness `id` names, and the
/// setting still covers that harness with a key that can be found. `None` otherwise.
pub fn entry_for(
    c: &Connection,
    node: &str,
    session: &str,
    id: &str,
) -> Result<Option<Entry>, StoreError> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT s.harness, r.principal FROM sessions s
               JOIN runtime_sessions r ON r.session_id=s.id
              WHERE s.id=? AND r.node_id=?",
            params![session, node],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((harness, Some(p))) = row else {
        return Ok(None);
    };
    if !harness.as_str().eq(id) || !matches!(Principal::parse(&p), Principal::Svc(_)) {
        return Ok(None);
    }
    let e = load(c)?.harnesses.get(id).cloned();
    Ok(e.filter(|e| credential(c, e).is_ok()))
}

impl super::node_secrets::SessionSecrets for ModelSecrets {
    fn value(&self, node: &str, session: &str, id: &str) -> Option<zeroize::Zeroizing<Vec<u8>>> {
        let entry = self
            .store
            .reader(|c| entry_for(c, node, session, id))
            .ok()
            .flatten()?;
        self.vault
            .with_credential(
                WS,
                &entry.item,
                "model_access",
                "服务会话的模型接入",
                |c| {
                    c.text(&entry.field)
                        .map(|t| zeroize::Zeroizing::new(t.trim().as_bytes().to_vec()))
                },
            )
            .map_err(|e| tracing::warn!(session, "model access: the key could not be read: {e}"))
            .ok()
            .flatten()
    }
}

/* ── the API: `GET /model-access`, `PUT|DELETE /model-access/{harness}` ── */

use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use steve_modules::{ApiError, Caller};

#[derive(Clone)]
struct Api {
    store: steve_store::Store,
    preflight: Arc<Preflight>,
}

/// The routes (behind the login and the `Caller` layer).
pub fn routes(store: steve_store::Store, preflight: Arc<Preflight>) -> Router {
    Router::new()
        .route("/model-access", get(view))
        .route("/model-access/{harness}", put(set).delete(clear))
        .with_state(Api { store, preflight })
}

fn owner(c: &Connection, me: &str) -> Result<bool, StoreError> {
    Ok(c.query_row("SELECT role FROM users WHERE id=?", [me], |r| {
        r.get::<_, String>(0)
    })
    .optional()?
    .as_deref()
        == Some("owner"))
}

fn snapshot(api: &Api, c: &Connection, me: &str) -> Result<Value, StoreError> {
    let setting = load(c)?;
    let harnesses: Vec<Value> = api
        .preflight
        .configurable()
        .into_iter()
        .map(|(id, name)| {
            let e = setting.harnesses.get(&id);
            let required = api
                .preflight
                .props(&id)
                .model_provider
                .is_some_and(|p| p.required);
            let status = match e {
                None => required.then_some("model_access_missing"),
                Some(e) => credential(c, e).err().map(|(code, _)| code),
            };
            json!({
                "id": id, "name": name,
                "base_url": e.map(|e| e.base_url.clone()),
                "item": e.map(|e| e.item.clone()),
                "field": e.map(|e| e.field.clone()),
                "updated": e.map(|e| e.updated),
                "required": required,
                "problem": status,
            })
        })
        .collect();
    // the items a key may be taken from: the Hub's shared and system vaults
    let mut q = c.prepare(
        "SELECT i.id, i.title, v.scope FROM vault_items i JOIN vaults v ON v.id=i.vault_id
          WHERE i.workspace_id=? AND i.status='active' AND v.scope IN ('shared','system')
            AND i.sensitivity<>'high' ORDER BY i.title",
    )?;
    let items = q
        .query_map([WS], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut fields = c.prepare(
        "SELECT label FROM vault_fields WHERE item_id=? AND secret=1 AND value_ct IS NOT NULL ORDER BY sort",
    )?;
    let items: Vec<Value> = items
        .into_iter()
        .map(|(id, title, scope)| {
            let labels: Vec<String> = fields
                .query_map([&id], |r| r.get(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default();
            json!({ "id": id, "title": title, "scope": scope, "fields": labels })
        })
        .collect();
    Ok(json!({
        "harnesses": harnesses,
        "items": items,
        "blocked": api.preflight.blocked_all(c)?,
        "principal_state": api.preflight.principal_state,
        "can_edit": owner(c, me)?,
    }))
}

async fn view(State(api): State<Api>, who: Caller) -> Result<Json<Value>, ApiError> {
    let a = api.clone();
    let v = steve_i18n::carry(move || {
        a.store
            .reader(|c| snapshot(&a, c, &who.me))
            .map_err(|e| ApiError::Internal(e.to_string()))
    });
    let v = tokio::task::spawn_blocking(v)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    base_url: String,
    item: String,
    field: String,
}

/// Validates and writes one harness's entry (`None`: removes it). Owner only.
fn write(api: &Api, me: &str, harness: &str, change: Option<&Change>) -> Result<Value, ApiError> {
    let internal = |e: StoreError| ApiError::Internal(e.to_string());
    if !api
        .preflight
        .configurable()
        .iter()
        .any(|(id, _)| id.as_str().eq(harness))
    {
        return Err(ApiError::rule(steve_i18n::t!(
            "hub.model_access.not_configurable",
            harness = harness
        )));
    }
    api.store.transaction(|tx| -> Result<(), ApiError> {
        if !owner(tx, me).map_err(internal)? {
            return Err(ApiError::forbidden(steve_i18n::t!(
                "hub.model_access.owner_only"
            )));
        }
        let mut s = load(tx).map_err(internal)?;
        match change {
            None => {
                s.harnesses.remove(harness);
            }
            Some(ch) => {
                let url = ch.base_url.trim().to_string();
                if !url_ok(&url) {
                    return Err(ApiError::invalid("base_url"));
                }
                let e = Entry {
                    base_url: url,
                    item: ch.item.trim().to_string(),
                    field: ch.field.trim().to_string(),
                    updated_by: me.to_string(),
                    updated: now(),
                };
                if let Err((code, _)) = credential(tx, &e) {
                    return Err(ApiError::rule(if code == "credential_not_shared" {
                        steve_i18n::t!("hub.model_access.credential_not_shared", harness = harness)
                    } else {
                        steve_i18n::t!("hub.model_access.credential_missing", harness = harness)
                    }));
                }
                s.harnesses.insert(harness.to_string(), e);
            }
        }
        save(tx, &s).map_err(internal)?;
        steve_store::base::audit(
            tx,
            WS,
            me,
            "model_access.set",
            harness,
            &json!({ "removed": change.is_none() }),
        )
        .map_err(internal)?;
        Ok(())
    })?;
    api.store.reader(|c| snapshot(api, c, me)).map_err(internal)
}

async fn set(
    State(api): State<Api>,
    who: Caller,
    Path(harness): Path<String>,
    steve_modules::Body(ch): steve_modules::Body<Change>,
) -> Result<Json<Value>, ApiError> {
    let f = steve_i18n::carry(move || write(&api, &who.me, &harness, Some(&ch)));
    let v = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(v))
}

async fn clear(
    State(api): State<Api>,
    who: Caller,
    Path(harness): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let f = steve_i18n::carry(move || write(&api, &who.me, &harness, None));
    let v = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))??;
    Ok(Json(v))
}

#[cfg(test)]
mod tests;

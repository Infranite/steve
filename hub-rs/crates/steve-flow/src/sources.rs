//! Event sources: the registered
//! [`EventAdapter`]s, the per-connection receive settings (`source_settings`), and the source page,
//! one entry per adapter with the `events` capability.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::accounts::connections::{ConnCx, Connections, shown};
use steve_modules::{ApiError, ApiResult, Caller};
use steve_store::Connections as ConnectionRows;
use steve_store::{
    BotRows, Contacts, FlowSources, FlowTriggers, Read, SignalRows, SourceConfig, W,
};

use crate::adapter::{AdapterError, EventAdapter, EventTypeDef};
use crate::{FlowCx, FlowResult};

/// `{"on":true,"param":"30m"}` of one event type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeSetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

/// How a scope is received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeMode {
    /// Stored and dispatched to flows.
    Receive,
    /// Stored as `recorded`, no flow.
    Record,
    /// Dropped.
    Ignore,
}

/// One scope of a connection with its mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSetting {
    pub ext_id: String,
    #[serde(default)]
    pub name: String,
    pub mode: ScopeMode,
}

/// One `source_settings` row (defaults when the connection has none yet).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSettings {
    pub connection_id: String,
    pub workspace_id: String,
    pub paused: bool,
    pub triage_bot: Option<String>,
    pub types: BTreeMap<String, TypeSetting>,
    pub scopes: Vec<ScopeSetting>,
    pub cursor: Option<String>,
    pub revision: i64,
}

impl SourceSettings {
    pub fn defaults(ws: &str, connection_id: &str) -> Self {
        Self {
            connection_id: connection_id.into(),
            workspace_id: ws.into(),
            paused: false,
            triage_bot: None,
            types: BTreeMap::new(),
            scopes: Vec::new(),
            cursor: None,
            revision: 1,
        }
    }

    /// Is `type_key` on? A type without a setting falls back to `default_on`.
    pub fn type_on(&self, type_key: &str, default_on: bool) -> bool {
        self.types
            .get(type_key)
            .and_then(|t| t.on)
            .unwrap_or(default_on)
    }

    /// The mode of a scope; scopes not listed (or no scope at all) are received.
    pub fn scope_mode(&self, ext_id: Option<&str>) -> ScopeMode {
        ext_id
            .and_then(|id| self.scopes.iter().find(|s| s.ext_id == id))
            .map_or(ScopeMode::Receive, |s| s.mode)
    }
}

/// The settings of a connection (defaults when there is no row).
pub fn settings(
    c: &mut impl Read,
    ws: &str,
    connection_id: &str,
) -> steve_store::Result<SourceSettings> {
    let Some(row) = FlowSources(c).get(ws, connection_id)? else {
        return Ok(SourceSettings::defaults(ws, connection_id));
    };
    Ok(SourceSettings {
        connection_id: connection_id.into(),
        workspace_id: ws.into(),
        paused: row.paused,
        triage_bot: row.triage_bot,
        types: serde_json::from_value(row.types)?,
        scopes: serde_json::from_value(row.scopes)?,
        cursor: row.cursor,
        revision: row.revision,
    })
}

/// Writes the settings; `revision + 1` when the row existed.
pub fn save(w: &mut W<'_>, s: &SourceSettings, now: i64) -> FlowResult<()> {
    let types = serde_json::to_value(&s.types)?;
    let scopes = serde_json::to_value(&s.scopes)?;
    FlowSources(w).save(&SourceConfig {
        connection: &s.connection_id,
        ws: &s.workspace_id,
        paused: s.paused,
        triage_bot: s.triage_bot.as_deref(),
        types: &types,
        scopes: &scopes,
        cursor: s.cursor.as_deref(),
        now,
    })?;
    Ok(())
}

/// The registered event adapters and the connections service that makes their [`ConnCx`].
#[derive(Clone, Default)]
pub struct EventAdapters {
    list: Vec<Arc<dyn EventAdapter>>,
    conns: Option<Connections>,
}

impl EventAdapters {
    pub fn add(&mut self, a: Arc<dyn EventAdapter>) {
        assert!(
            self.list.iter().all(|x| x.id() != a.id()),
            "event adapter {} registered twice",
            a.id()
        );
        self.list.push(a);
    }

    pub fn set_connections(&mut self, c: Connections) {
        self.conns = Some(c);
    }

    pub fn get(&self, adapter: &str) -> Option<&Arc<dyn EventAdapter>> {
        self.list.iter().find(|a| a.id() == adapter)
    }

    pub fn all(&self) -> &[Arc<dyn EventAdapter>] {
        &self.list
    }

    /// The default of a type of `adapter` (unknown adapter or type: on).
    pub fn default_on(&self, adapter: &str, type_key: &str) -> bool {
        self.get(adapter)
            .and_then(|a| a.event_types().iter().find(|t| t.key == type_key))
            .is_none_or(|t| t.default_on)
    }

    /// The adapter's view of a connection.
    pub fn cx(&self, ws: &str, connection_id: &str) -> Result<ConnCx, AdapterError> {
        let conns = self.conns.as_ref().ok_or_else(|| {
            AdapterError::Provider(steve_modules::accounts::connections::ProviderError::Config(
                "接入服务还没有准备好".into(),
            ))
        })?;
        conns.cx(ws, connection_id).map_err(|e| {
            AdapterError::Provider(steve_modules::accounts::connections::ProviderError::Config(
                e.to_string(),
            ))
        })
    }
}

/// One connection of a source as the caller may see it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceConn {
    pub id: String,
    pub name: String,
    pub status: String,
    pub last_sync: Option<i64>,
}

/// The connections of `adapter` in `ws` with the `events` capability that `me` may see: someone's
/// personal connection (their “我”) is theirs alone, like on the accounts page.
pub fn connections_of(
    c: &mut impl Read,
    ws: &str,
    me: &str,
    adapter: &str,
) -> steve_store::Result<Vec<SourceConn>> {
    let connections = ConnectionRows(c).for_adapter(ws, adapter)?;
    let accounts =
        Contacts::new(c).account_owners(connections.iter().map(|conn| conn.account.as_str()))?;
    Ok(connections
        .into_iter()
        .filter_map(|conn| {
            let (name, owner) = accounts.get(&conn.account)?;
            (owner.as_deref().is_none_or(|owner| owner == me)
                && conn.caps.iter().any(|cap| cap == "events"))
            .then(|| SourceConn {
                id: conn.id,
                name: name.clone(),
                status: conn.status,
                last_sync: conn.last_sync,
            })
        })
        .collect())
}

/// `Source`.
#[derive(Debug, Clone, Serialize)]
pub struct SourceView {
    pub adapter: String,
    pub source: String,
    pub name: String,
    pub paused: bool,
    pub triage_bot: Option<String>,
    pub connections: Vec<Value>,
    pub scopes: Vec<Value>,
    pub types: Vec<Value>,
}

pub const DAY_MS: i64 = 24 * 3_600_000;

/// The source page entry of one adapter for `me`.
pub fn view(
    cx: &FlowCx,
    c: &mut impl Read,
    who: &Caller,
    a: &dyn EventAdapter,
) -> FlowResult<SourceView> {
    let conns = connections_of(c, &who.ws, &who.me, a.id())?;
    let sets = conns
        .iter()
        .map(|x| settings(c, &who.ws, &x.id))
        .collect::<steve_store::Result<Vec<_>>>()?;
    let first = sets.first();
    let ids: Vec<String> = conns.iter().map(|x| x.id.clone()).collect();
    let since = cx.now() - DAY_MS;
    let stored = SignalRows(c).counts_by_kind(&who.ws, &ids, since)?;
    let triggers = FlowTriggers(c).for_source(&who.ws, a.source())?;
    let types = a
        .event_types()
        .iter()
        .map(|t: &EventTypeDef| -> FlowResult<Value> {
            let set = first.and_then(|s| s.types.get(t.key));
            let n24 = stored.get(t.key).copied().unwrap_or(0) + cx.drops.count(&ids, t.key, since);
            Ok(json!({
                "key": t.key,
                "label": shown(a, t.label),
                "desc": shown(a, t.desc),
                "on": first.map_or(t.default_on, |s| s.type_on(t.key, t.default_on)),
                "param": set.and_then(|s| s.param.clone()).or(t.param.map(String::from)),
                "n24": n24,
                "flows": triggers.iter().filter(|trigger| trigger.kind != "item" && (trigger.types.is_empty() || trigger.types.iter().any(|kind| kind == t.key))).map(|trigger| &trigger.flow).collect::<BTreeSet<_>>().len(),
            }))
        })
        .collect::<FlowResult<Vec<_>>>()?;
    let scopes = sets
        .iter()
        .flat_map(|s| {
            s.scopes.iter().map(|x| {
                json!({"connection_id": s.connection_id, "ext_id": x.ext_id, "name": x.name, "mode": x.mode})
            })
        })
        .collect();
    Ok(SourceView {
        adapter: a.id().into(),
        source: a.source().into(),
        name: shown(a, a.name()),
        paused: !sets.is_empty() && sets.iter().all(|s| s.paused),
        triage_bot: first.and_then(|s| s.triage_bot.clone()),
        connections: conns
            .iter()
            .map(|x| json!({"id": x.id, "name": x.name, "status": x.status, "last_sync": x.last_sync}))
            .collect(),
        scopes,
        types,
    })
}

/// `GET /sources`: every adapter with the `events` capability.
pub fn list(cx: &FlowCx, who: &Caller) -> FlowResult<Vec<SourceView>> {
    cx.read(|c| {
        cx.events
            .all()
            .iter()
            .filter(|a| a.caps().contains(&"events"))
            .map(|a| view(cx, c, who, a.as_ref()))
            .collect()
    })
}

pub(crate) fn own_adapter<'a>(
    cx: &'a FlowCx,
    adapter: &str,
) -> ApiResult<&'a Arc<dyn EventAdapter>> {
    cx.events
        .get(adapter)
        .filter(|a| a.caps().contains(&"events"))
        .ok_or_else(|| ApiError::not_found("事件源"))
}

/// Body of `PATCH /sources/:adapter`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePatch {
    pub paused: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub triage_bot: Option<Option<String>>,
    pub types: Option<BTreeMap<String, TypeSetting>>,
    pub scopes: Option<Vec<ScopePatch>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScopePatch {
    pub connection_id: String,
    pub ext_id: String,
    pub mode: ScopeMode,
}

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// `ownBot`: a bot of the workspace (`404 没有这个 Bot`).
fn own_bot(c: &mut impl Read, ws: &str, bot: &str) -> ApiResult<()> {
    if BotRows(c).local(ws, bot)?.is_some() {
        Ok(())
    } else {
        Err(ApiError::not_found("Bot"))
    }
}

/// `PATCH /sources/:adapter`: changes the settings of all of the adapter's connections the caller
/// sees (scopes: only the listed ones).
pub fn patch(cx: &FlowCx, who: &Caller, adapter: &str, p: &SourcePatch) -> FlowResult<SourceView> {
    let a = own_adapter(cx, adapter)?.clone();
    check_patch(a.as_ref(), p)?;
    let now = cx.now();
    cx.store()
        .write(|tx| patch_tx(tx, &who.ws, &who.me, adapter, p, now))?;
    cx.read(|c| view(cx, c, who, a.as_ref()))
}

/// The checks of a [`SourcePatch`] that need only the adapter (its event types).
pub fn check_patch(a: &dyn EventAdapter, p: &SourcePatch) -> FlowResult<()> {
    if let Some(types) = &p.types
        && types
            .keys()
            .any(|k| !a.event_types().iter().any(|t| t.key == k))
    {
        return Err(ApiError::invalid("types"));
    }
    if let Some(types) = &p.types
        && types
            .values()
            .any(|t| t.param.as_deref().is_some_and(|x| x.chars().count() > 40))
    {
        return Err(ApiError::invalid("types"));
    }
    Ok(())
}

/// [`patch`] inside a transaction (also the `source.patch` change operation), after
/// [`check_patch`]: the references (bot, connections) are checked here.
pub fn patch_tx(
    tx: &mut W<'_>,
    ws: &str,
    me: &str,
    adapter: &str,
    p: &SourcePatch,
    now: i64,
) -> FlowResult<()> {
    let conns = connections_of(tx, ws, me, adapter)?;
    if let Some(Some(bot)) = &p.triage_bot {
        own_bot(tx, ws, bot)?;
    }
    if let Some(scopes) = &p.scopes {
        for s in scopes {
            if !conns.iter().any(|c| c.id == s.connection_id) {
                return Err(ApiError::not_found("连接"));
            }
        }
    }
    for conn in &conns {
        let mut s = settings(tx, ws, &conn.id)?;
        if let Some(v) = p.paused {
            s.paused = v;
        }
        if let Some(v) = &p.triage_bot {
            s.triage_bot = v.clone();
        }
        if let Some(types) = &p.types {
            for (k, v) in types {
                let e = s.types.entry(k.clone()).or_default();
                if v.on.is_some() {
                    e.on = v.on;
                }
                if v.param.is_some() {
                    e.param = v.param.clone();
                }
            }
        }
        for sp in p
            .scopes
            .iter()
            .flatten()
            .filter(|x| x.connection_id == conn.id)
        {
            match s.scopes.iter_mut().find(|x| x.ext_id == sp.ext_id) {
                Some(x) => x.mode = sp.mode,
                None => s.scopes.push(ScopeSetting {
                    ext_id: sp.ext_id.clone(),
                    name: sp.ext_id.clone(),
                    mode: sp.mode,
                }),
            }
        }
        save(tx, &s, now)?;
        tx.emit(steve_types::DomainEvent::from(
            steve_types::ChannelsEvent::Connection {
                id: conn.id.clone().into(),
                ws: ws.into(),
            },
        ));
    }
    tx.audit(
        ws,
        me,
        "source.patch",
        adapter,
        &json!({
            "paused": p.paused,
            "triage_bot": p.triage_bot,
            "types": p.types.as_ref().map(|t| t.keys().collect::<Vec<_>>()),
            "scopes": p.scopes.as_ref().map(Vec::len),
        }),
    )?;
    Ok(())
}

/// `POST /sources/:adapter/refresh-scopes`: asks the adapter again; known scopes keep their mode, new
/// ones are received, vanished ones are dropped.
pub async fn refresh_scopes(cx: &FlowCx, who: &Caller, adapter: &str) -> FlowResult<SourceView> {
    let a = own_adapter(cx, adapter)?.clone();
    let conns = cx
        .store()
        .read(|c| connections_of(c, &who.ws, &who.me, adapter))?;
    let mut found = Vec::new();
    for conn in &conns {
        let ccx = cx
            .events
            .cx(&who.ws, &conn.id)
            .map_err(|e| ApiError::rule(e.to_string()))?;
        let scopes = a
            .scopes(&ccx)
            .await
            .map_err(|e| ApiError::rule(e.to_string()))?;
        found.push((conn.id.clone(), scopes));
    }
    let now = cx.now();
    cx.store().write(|tx| -> FlowResult<()> {
        for (id, scopes) in &found {
            let mut s = settings(tx, &who.ws, id)?;
            s.scopes = scopes
                .iter()
                .map(|d| ScopeSetting {
                    ext_id: d.ext_id.clone(),
                    name: d.name.clone(),
                    mode: s.scope_mode(Some(&d.ext_id)),
                })
                .collect();
            save(tx, &s, now)?;
            tx.emit(steve_types::DomainEvent::from(
                steve_types::ChannelsEvent::Connection {
                    id: id.clone().into(),
                    ws: who.ws.clone().into(),
                },
            ));
        }
        Ok(())
    })?;
    cx.read(|c| view(cx, c, who, a.as_ref()))
}

//! What runs for each Feishu connection, kept in step with the connections as they change — no Hub
//! restart after connecting:
//!
//! - a `feishu_app` connection that is `ok` / `error`: its long connection, and right after it
//!   (re)starts, [`super::sync_bot_chats`] (the groups the bot is already in become spaces);
//! - a `feishu_user` connection that is `ok` / `error` with `msg.receive`: its self poller;
//! - anything else (paused, `reauth`, disconnected): stopped.
//!
//! A new credential on a running bot (re-entered App Secret) restarts its long connection.
//! Both also get their `channel_settings` (bot / self) the moment they connect.
//!
//! Wiring (integrator), once at startup:
//! ```ignore
//! let rt = FeishuRuntime::new(channels.clone(), Some(feishu_user.clone()));
//! rt.start_all().await;                               // replaces LongConnections::start_all
//! rt.spawn_watch(bus.tap(&["connection"]));           // steve-hub's Bus: {type:"connection", id, ws}
//! ```
//! [`FeishuRuntime`] is also a [`ConnectionHook`], for a connections framework that calls hooks
//! itself after connect / resume / pause / disconnect.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

use super::auth::{APP_ADAPTER, FeishuUser, USER_ADAPTER};
use super::inbound::LongConnections;
use super::self_poll::SelfPollers;
use crate::spaces::{self, Mode};
use crate::{ChannelResult, Channels};

/// Something to run when a connection changed (connected, re-authorized, resumed, paused,
/// disconnected). Idempotent: it reads the connection's current state and acts on that.
#[async_trait]
pub trait ConnectionHook: Send + Sync {
    async fn on_connection(&self, ws: &str, connection_id: &str);
}

/// What [`FeishuRuntime::on_connection`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reaction {
    /// The long connection or poller was started now.
    pub started: bool,
    /// It was stopped now.
    pub stopped: bool,
    /// New spaces from `sync_bot_chats` after a bot (re)start.
    pub synced: Option<usize>,
}

struct ConnState {
    ws: String,
    adapter: String,
    status: String,
    caps: Vec<String>,
    credential: Option<String>,
}

/// The long connections and self pollers of all Feishu connections.
pub struct FeishuRuntime {
    channels: Channels,
    long: LongConnections,
    polls: Option<SelfPollers>,
    /// Credential item each running one was started with.
    started_with: Mutex<HashMap<String, Option<String>>>,
    /// Each connection's change signature when last seen (names are looked up again on a change).
    seen: Mutex<HashMap<String, String>>,
}

impl FeishuRuntime {
    /// `user`: the `feishu_user` provider (the same object registered with `Connections`); without
    /// it no self pollers run.
    pub fn new(channels: Channels, user: Option<Arc<FeishuUser>>) -> Arc<Self> {
        Arc::new(Self {
            long: LongConnections::new(channels.clone()),
            polls: user.map(|u| SelfPollers::new(channels.clone(), u)),
            channels,
            started_with: Mutex::default(),
            seen: Mutex::default(),
        })
    }

    pub fn long_connections(&self) -> &LongConnections {
        &self.long
    }

    pub fn self_pollers(&self) -> Option<&SelfPollers> {
        self.polls.as_ref()
    }

    fn state(&self, id: &str) -> ChannelResult<Option<ConnState>> {
        self.channels.store().reader(|c| -> ChannelResult<_> {
            Ok(c.query_row(
                "SELECT workspace_id, adapter, status, caps, credential_item FROM connections WHERE id = ?",
                params![id],
                |r| {
                    Ok(ConnState {
                        ws: r.get(0)?,
                        adapter: r.get(1)?,
                        status: r.get(2)?,
                        caps: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                        credential: r.get(4)?,
                    })
                },
            )
            .optional()?)
        })
    }

    /// Starts what every Feishu connection needs (Hub start). Returns how many were started.
    pub async fn start_all(&self) -> usize {
        let ids = self.channels.store().reader(|c| -> ChannelResult<Vec<(String, String)>> {
            let mut st = c.prepare(
                "SELECT id, workspace_id FROM connections WHERE adapter IN (?, ?) ORDER BY created, id",
            )?;
            let rows = st
                .query_map(params![APP_ADAPTER, USER_ADAPTER], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        });
        let ids = match ids {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("feishu runtime: listing connections failed: {e}");
                return 0;
            }
        };
        let mut n = 0;
        for (id, ws) in ids {
            if self.on_connection(&ws, &id).await.started {
                n += 1;
            }
        }
        // names of the people met: kicked runs and the periodic one
        drop(self.channels.spawn_names());
        n
    }

    /// Brings connection `id` of `ws` in step with its current state. Connections of other
    /// adapters or workspaces are ignored.
    pub async fn on_connection(&self, ws: &str, id: &str) -> Reaction {
        let state = match self.state(id) {
            Ok(s) => s.filter(|s| s.ws == ws),
            Err(e) => {
                tracing::warn!(
                    conn = id,
                    "feishu runtime: reading the connection failed: {e}"
                );
                return Reaction::default();
            }
        };
        let adapter = match &state {
            Some(s) => s.adapter.clone(),
            // gone: whichever kind it was
            None => {
                return Reaction {
                    stopped: self.stop(id).await,
                    ..Reaction::default()
                };
            }
        };
        let s = state.expect("checked above");
        let live = matches!(s.status.as_str(), "ok" | "error");
        match adapter.as_str() {
            APP_ADAPTER => self.bot(ws, id, &s, live).await,
            USER_ADAPTER => {
                let live = live && s.caps.iter().any(|c| c == "msg.receive");
                self.user(ws, id, &s, live).await
            }
            _ => Reaction::default(),
        }
    }

    async fn stop(&self, id: &str) -> bool {
        self.started_with.lock().await.remove(id);
        let a = self.long.stop(id).await;
        let b = match &self.polls {
            Some(p) => p.stop(id).await,
            None => false,
        };
        a || b
    }

    fn settings(&self, ws: &str, id: &str, mode: Mode) {
        let now = self.channels.now();
        if let Err(e) = self
            .channels
            .store()
            .transaction(|tx| spaces::ensure_channel_settings(tx, ws, id, mode, now))
        {
            tracing::warn!(
                conn = id,
                "feishu runtime: channel settings not created: {e}"
            );
        }
    }

    async fn bot(&self, ws: &str, id: &str, s: &ConnState, live: bool) -> Reaction {
        let mut r = Reaction::default();
        if !live {
            r.stopped = self.stop(id).await;
            return r;
        }
        let mut started = self.started_with.lock().await;
        if self.long.is_running(id).await {
            if started.get(id) == Some(&s.credential) {
                return r;
            }
            // a new App Secret: the running client holds the old one
            self.long.stop(id).await;
            r.stopped = true;
        }
        self.settings(ws, id, Mode::Bot);
        match self.long.start_one(ws, id).await {
            Ok(_) => {
                started.insert(id.to_string(), s.credential.clone());
                r.started = true;
            }
            Err(e) => {
                tracing::warn!(
                    conn = id,
                    "feishu runtime: long connection not started: {e}"
                );
                return r;
            }
        }
        drop(started);
        match super::sync_bot_chats(&self.channels, ws, id).await {
            Ok(n) => r.synced = Some(n),
            Err(e) => tracing::warn!(conn = id, "feishu runtime: bot chats not synced: {e}"),
        }
        r
    }

    async fn user(&self, ws: &str, id: &str, s: &ConnState, live: bool) -> Reaction {
        let mut r = Reaction::default();
        let Some(polls) = &self.polls else {
            return r;
        };
        if !live {
            r.stopped = self.stop(id).await;
            return r;
        }
        self.settings(ws, id, Mode::Own);
        let mut started = self.started_with.lock().await;
        r.started = polls.start_one(ws, id).await;
        if r.started {
            started.insert(id.to_string(), s.credential.clone());
        }
        r
    }

    /// Has connection `id` changed since last seen beyond its health counters?
    async fn really_changed(&self, id: &str) -> bool {
        let sig = self
            .channels
            .store()
            .reader(|c| steve_modules::accounts::connections::repo::change_signature(c, id))
            .ok()
            .flatten();
        let mut seen = self.seen.lock().await;
        match sig {
            Some(s) => seen.insert(id.to_string(), s.clone()).as_ref() != Some(&s),
            None => seen.remove(id).is_some(),
        }
    }

    /// Reacts to `{type:"connection", id, ws}` events until the sender goes away (steve-hub's
    /// `Bus::tap(&["connection"])`). Other events are skipped.
    pub fn spawn_watch(
        self: &Arc<Self>,
        mut rx: mpsc::UnboundedReceiver<Value>,
    ) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if ev.get("type").and_then(Value::as_str) != Some("connection") {
                    continue;
                }
                let (Some(id), Some(ws)) = (
                    ev.get("id").and_then(Value::as_str),
                    ev.get("ws").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let r = me.on_connection(ws, id).await;
                // a Feishu identity verified, re-authorized, a permission granted: look the
                // nameless up again. Not on health changes (every failed call is one, and a
                // lookup that fails would kick the next) nor other connections' events.
                if me.really_changed(id).await
                    && let Ok(Some(s)) = me.state(id)
                    && s.status == "ok"
                    && me.channels.profile_adapter(&s.adapter).is_some()
                {
                    me.channels.kick_names(ws, true);
                }
                if r.started || r.stopped {
                    tracing::info!(conn = id, started = r.started, stopped = r.stopped, synced = ?r.synced, "feishu runtime: connection changed");
                }
            }
        })
    }
}

#[async_trait]
impl ConnectionHook for FeishuRuntime {
    async fn on_connection(&self, ws: &str, connection_id: &str) {
        FeishuRuntime::on_connection(self, ws, connection_id).await;
    }
}

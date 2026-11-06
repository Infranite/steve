//! What runs for each Feishu connection, as [`ConnectionHook`]s of the connection runtime
//! ([`crate::lifecycle`]):
//!
//! - [`BotHook`], for each `feishu_app` connection that is `ok` / `error`: its long connection, and
//!   right after it (re)starts, [`super::sync_bot_chats`] (the groups the bot is already in become
//!   spaces). A new credential (re-entered App Secret) restarts it. Each time the long connection
//!   is up it reports the connection healthy, so a failed start that was retried clears.
//! - [`SelfPollHook`], for each `feishu_user` connection that is `ok` / `error` with `msg.receive`:
//!   its self poller.
//!
//! Both hand their messages to the whole pipeline directly, not through the run's sink: a run
//! finishes what it already received before it returns (the long connection's queue, the poller's
//! round in progress), and the runtime starts the next run of a connection only after that.

use std::sync::Arc;

use async_trait::async_trait;
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{Health, ProviderError};

use super::auth::{APP_SECRET, FeishuUser};
use super::client;
use super::inbound::LongConnection;
use super::self_poll::SelfPoller;
use super::ws::{WsOptions, WsState};
use crate::Channels;
use crate::lifecycle::{ConnRun, ConnectionHook};

/// The long connection of one bot connection.
pub struct BotHook {
    channels: Channels,
}

impl BotHook {
    pub fn new(channels: Channels) -> Self {
        Self { channels }
    }
}

#[async_trait]
impl ConnectionHook for BotHook {
    async fn run(&self, run: ConnRun) -> Result<(), AdapterError> {
        let (ws, id) = (run.state.ws.as_str(), run.state.id.as_str());
        let cx = &run.cx;
        let app_id = cx.config_str("app_id").unwrap_or_default().to_string();
        let bot_open_id = cx.config_str("bot_open_id").unwrap_or_default().to_string();
        let secret = cx.secret(APP_SECRET).await?;
        let opts = WsOptions::new(client::api_base(cx.config()), &app_id, secret.as_str());
        drop(secret);
        let lc = LongConnection::start_with(&self.channels, ws, id, opts, &bot_open_id)
            .map_err(|e| ProviderError::Transient(e.to_string()))?;
        tracing::info!(conn = id, "feishu: long connection started");
        let sync = async {
            match super::sync_bot_chats(&self.channels, ws, id).await {
                Ok(n) => tracing::info!(conn = id, synced = n, "feishu: bot chats synced"),
                Err(e) => tracing::warn!(conn = id, "feishu: bot chats not synced: {e}"),
            }
        };
        // connected (again): healthy
        let mut state = lc.ws.subscribe();
        let healthy = async {
            loop {
                let up = matches!(*state.borrow_and_update(), WsState::Connected { .. });
                if up {
                    cx.report(Health::Ok).await;
                }
                if state.changed().await.is_err() {
                    return std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            _ = async { tokio::join!(sync, healthy) } => {}
            _ = run.stop.cancelled() => {}
        }
        // queued messages are still processed before the run returns
        let _ = lc.stop().await;
        Ok(())
    }
}

/// The self poller of one connection of you on Feishu.
pub struct SelfPollHook {
    channels: Channels,
    user: Arc<FeishuUser>,
}

impl SelfPollHook {
    /// `user`: the `feishu_user` provider (the same object registered with `Connections`).
    pub fn new(channels: Channels, user: Arc<FeishuUser>) -> Self {
        Self { channels, user }
    }
}

#[async_trait]
impl ConnectionHook for SelfPollHook {
    async fn run(&self, run: ConnRun) -> Result<(), AdapterError> {
        SelfPoller::new(
            self.channels.clone(),
            self.user.clone(),
            &run.state.ws,
            &run.state.id,
        )
        .run(run.stop.clone())
        .await;
        Ok(())
    }
}

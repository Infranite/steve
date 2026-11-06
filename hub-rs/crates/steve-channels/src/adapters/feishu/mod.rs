//! Feishu / Lark open platform.
//!
//! - [`client`]: the REST client, token sources, the invalid-token retry (F1);
//! - [`auth`]: the bot (`feishu_app`) and user (`feishu_user`) connection providers, tenant token
//!   minting, the lark-cli device flow (F1);
//! - [`verify`]: bot/user verification, the self-built check, the permission table (F1);
//! - [`directory`]: the address book as the contacts sync's `DirectoryAdapter` (F5);
//! - [`crypto`], [`pbbp2`], [`ws`]: webhook crypto and the long connection (F0b/F2a);
//! - [`inbound`]: the bot as a `ChannelAdapter`, `im.message.receive_v1` → C1's inbound pipeline
//!   over the long connection (C1);
//! - [`send`], [`ratelimit`]: sending, replying and recalling as the bot, the send rates and the
//!   rate-limit retries shared with sending as you (F3);
//! - [`chats`]: group info, members, the bot's groups, member and chat events (F4);
//! - [`user_oauth`], [`as_user`]: you — your Feishu account on “我”, sending as you (F6);
//! - [`self_poll`]: you — your chats read with your user token about once a minute (F8);
//! - [`profiles`]: names of people behind ids, through you first, then the bot;
//! - [`hooks`]: the long connection and the self poller of each connection, run by the connection
//!   runtime ([`crate::lifecycle`]) as connections change.

pub mod app_registration;
pub mod as_user;
pub mod auth;
pub mod card;
pub mod chats;
pub mod client;
pub mod crypto;
pub mod directory;
pub mod hooks;
pub mod inbound;
pub mod pbbp2;
pub mod profiles;
pub mod ratelimit;
pub mod resource;
pub mod self_poll;
pub mod send;
pub mod user_oauth;
pub mod verify;
pub mod ws;

pub use as_user::{SentAsUser, UserMessage};
pub use auth::{DeviceFlows, FeishuApp, FeishuUser, device_routes};
pub use chats::{BotChat, parse_chat_event, sync_bot_chats};
pub use client::{Domain, FeishuClient, FeishuEnv, FeishuError, TokenSource, VerifyTokens};
pub use directory::FeishuDirectory;
pub use hooks::{BotHook, SelfPollHook};
pub use resource::FeishuMedia;
pub use self_poll::{RoundReport, SelfPoller, SelfPollers};
pub use send::{Sent, at_handle, reply_in_thread};
pub use user_oauth::{FeishuIds, identify_sender, user_routes};
pub use verify::{PermissionCheck, Status as PermissionStatus};

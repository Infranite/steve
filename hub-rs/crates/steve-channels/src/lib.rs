//! Channels, external personas and the Feishu adapter
//!
//!
//! C1 (spaces and messages): [`channel`] (the [`ChannelAdapter`] seam and [`InboundMessage`]),
//! [`spaces`], [`messages`] (with the `channels.retention` job and the contact timeline),
//! [`inbound`] (steps 1–3 of the inbound pipeline), [`api`] (`/spaces…`), [`Channels`] and
//! [`MessageSink`]; [`fake::channel::FakeChannel`] for tests (feature `fake`).
//!
//! C2 (personas and bindings): [`personas`] (incl. `introOf`, the default persona “Steve”,
//! [`personas::on_bot_deleted`]), [`bindings`] (account default / space binding / effective
//! persona), `/personas…` merged into [`api::routes`], `persona_id` on `PATCH /spaces/:id`.
//!
//! C3 (response rules): [`rules`] (validation, evaluation, the `intent` judgement on the fast tier,
//! the “转成流程” compile), steps 4–7 in [`inbound`] ([`Channels::on_message`]; the
//! [`MessageSink`] of `Channels` now runs the whole pipeline), [`respond`] (the [`Responder`] seam
//! for C4/C6 and C3's never-sending fallback), `rules` on `PATCH /spaces/:id`,
//! `POST /spaces/:id/rules/to-flow`, `DELETE /spaces/:id/flow`, [`spaces::set_account_rules`] for
//! `PATCH /channels/:connection_id`.
//!
//! C4 (reply generation): [`compose`] (the prompt in seven layers, K4 retrieval in layer 7,
//! P3 contact cards in layer 6), [`replies::Replies`] (the [`Responder`]: `reply`/`draft`
//! generate, `create_item` creates and links the item and answers its number), [`deliver`] (the
//! [`deliver::Outbound`] hand-off to C6; [`deliver::DraftOnly`] until then — C4 never sends),
//! [`flow_actions`] (`reply_as`, `send_to_space`).
//! C5 (disclosure and basis): [`disclosure`] (B1's `OutboundCheck`: secrets blocked, levels for
//! the audience, the rule check, the check model, the rewrite prompt; [`disclosure::space_audience`]),
//! [`basis`] (`reply_basis`, the `basis` of a `ChanMessage`).
//! C6 (outbound): [`outbound`] — [`Outbox`] (the real [`deliver::Outbound`]: check → rewrite →
//! authorization → the only adapter `send(` → basis → audit; the approval queue with its undo delay,
//! trigger replies as you, back to the source, `steve_reply_source`, recall) and the `chan_message`
//! decision handler ([`OutLink::handler`]).
//! C8 (conversational configuration): [`drafts`] — the `space`, `persona` and `channel` draft
//! targets with the `chan.space.patch`, `chan.persona.patch`, `chan.channel.patch` operations.
//!
//! Integration (integrator), once at startup:
//! ```ignore
//! steve_channels::drafts::register(&mut registry, clock.clone());   // C8, before `Modules` is built
//! // C5 + C6, before `Modules` is built:
//! let disclosure = steve_channels::Disclosure::new();                  // STEVE_MODELS_CHECK / _FAST
//! registry.outbound_check(Box::new(disclosure.clone()));
//! let link = steve_channels::OutLink::new();
//! registry.decision(steve_channels::respond::SUBJECT_MESSAGE, link.handler());
//! let late = steve_channels::flow_actions::LateChannels::new();
//! let outbox = Arc::new(steve_channels::Outbox::new(link.clone())
//!     .act_as_user(feishu_user.clone()));                              // ActAsUser: send, react, recall as you
//! registry.item_hooks(Box::new(steve_channels::outbound::ReturnToSource::new(outbox.clone(), late.clone())));
//! // … modules = Modules::new(store, gateway, registry) …
//! disclosure.bind(modules.store.clone(), modules.gateway.clone());
//! link.bind_clock(modules.clock.clone());
//! let feishu = Arc::new(steve_channels::adapters::feishu::FeishuApp::new());
//! // `conns` = the A2 Connections service, built with `.provider(feishu.clone())`
//! // C4, before the flow module is built:
//! let replies = Arc::new(steve_channels::Replies::new().outbound(outbox.clone()));   // C6
//! let setup = setup
//!     .action(Arc::new(steve_channels::flow_actions::ReplyAs::new(replies.clone(), late.clone())))
//!     .action(Arc::new(steve_channels::flow_actions::SendToSpace::new(replies.clone(), late.clone())));
//! let sink: Arc<dyn steve_flow::EventSink> = Arc::new(flow.clone());     // S1
//! let channels = steve_channels::Channels::builder(modules.clone(), conns.clone(), sink)
//!     .adapter(feishu.clone())
//!     .media(Arc::new(steve_channels::adapters::feishu::FeishuMedia::new("feishu_app")))
//!     .flow(flow.clone())               // C3: to-flow validates against the registered node kinds
//!     .guard(vault_guard.clone())       // C3: `guard::scan` before the `intent` judgement
//!     .responder(replies.clone())       // C4 + C6
//!     .build();
//! late.set(&channels);                                                 // C4 flow actions, C6 hook
//! outbox.clone().spawn_queue(&channels);                               // C6 approvals, retries
//! api_router.merge(steve_channels::api::routes(channels.clone()));      // behind the Caller layer
//! contacts_ports.timelines.push(steve_channels::messages::MessageTimeline::arc());
//! contacts_ports.spaces = Some(Arc::new(steve_channels::spaces::Spaces));
//! tokio::spawn(steve_channels::messages::retention(modules.clone()));   // `channels.retention`
//! // the Feishu long connection, one per `feishu_app` connection:
//! steve_channels::adapters::feishu::inbound::LongConnections::new(channels.clone()).start_all().await;
//! ```
//!
//! Channel REST gaps (CAPI), replacing the last line above:
//! ```ignore
//! // before building the providers: STEVE_FEISHU_API_BASE (tests only), STEVE_FORBID_REAL_HOSTS
//! let fenv = steve_channels::adapters::feishu::FeishuEnv::from_env(|k| std::env::var(k).ok());
//! fenv.install();
//! let feishu_user = Arc::new(fenv.user_provider(Domain::Feishu));   // instead of FeishuUser::new
//! steve_channels::space_decisions::register(&mut registry);          // the “新群” card (subject_type space)
//! // Connections::builder(..).usage(Arc::new(steve_channels::usage::ChannelUsage)): spaces count,
//! // disconnect: spaces kept as left, their settings, channel_settings and chan_cursors deleted
//! api_router.merge(steve_channels::channels_api::routes(channels.clone()));
//! // long connections + self pollers, started at boot and whenever a connection changes
//! // (connected, re-authorized, resumed → start, plus sync_bot_chats for a bot; paused, reauth,
//! // disconnected → stop). No hook in Connections is needed: it publishes `connection` events.
//! use steve_channels::adapters::feishu::{BotHook, SelfPollHook};
//! let events = bus.tap(&["connection"]);                              // tapped before listing
//! let rt = steve_channels::lifecycle::ConnectionRuntime::builder(channels.clone())
//!     .hook("feishu_app", Some(bot_decl), Arc::new(BotHook::new(channels.clone())))
//!     .hook("feishu_user", Some(user_decl), Arc::new(SelfPollHook::new(channels.clone(), feishu_user.clone())))
//!     .build();
//! rt.start(events);
//! // at shutdown: rt.shutdown().await
//! ```

steve_i18n::init!();

pub mod adapters;
pub mod api;
pub mod arbiter;
pub mod basis;
pub mod bindings;
pub mod channel;
pub mod channel_accounts;
pub mod channels_api;
pub mod cleanup;
pub mod compose;
pub mod conversation_card;
pub mod conversations;
pub mod deliver;
pub mod disclosure;
pub mod drafts;
pub mod ext;
#[cfg(feature = "fake")]
pub mod fake;
pub mod fallback;
pub mod flow_actions;
pub mod inbound;
pub mod lifecycle;
pub mod media;
pub mod media_agent;
pub mod members;
pub mod membership;
pub mod messages;
pub mod names;
pub mod outbound;
pub mod personas;
mod personas_api;
pub mod queries;
pub mod reactions;
pub mod replies;
pub mod respond;
pub mod rules;
pub mod seen;
pub mod send_state;
mod service;
pub mod space_decisions;
pub mod spaces;
pub mod usage;

pub use channel::{
    ActAsUser, ActTarget, ChannelAdapter, ChannelFeatures, HandleRef, InboundMessage, MemberInfo,
    Mention, MessageRef, OutContent, Profile, ProfileAdapter, ProfileAnswer, ProfileQuery,
    PushOutcome, ReactionKind, SendKey, SendTarget, Sender, Sent, SpaceEvent, SpaceInfo, SpaceKind,
    TenantInfo,
};
pub use deliver::{Auth, Delivered, DraftOnly, OutRequest, OutTarget, Outbound};
pub use disclosure::Disclosure;
pub use inbound::{Inbound, Next};
pub use membership::SpaceChange;
pub use outbound::{OutLink, Outbox};
pub use replies::Replies;
pub use respond::{By, Plan, Responder, Response};
pub use service::{Channels, ChannelsBuilder, MessageSink, OtherEvents, WeakChannels};

/// Errors of this crate: the module API error (`400/404/409` with Chinese text, `500` otherwise).
pub type ChannelError = steve_modules::ApiError;
pub type ChannelResult<T> = Result<T, ChannelError>;

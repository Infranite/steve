//! The channel adapter seam and the adapter-neutral inbound message
//! every adapter turns its provider's payloads into before [`crate::Channels::on_inbound`].
//!
//! A [`ChannelAdapter`] extends the connection provider of the accounts module: the same object is
//! registered with `Connections` (how Steve gets in) and with [`crate::ChannelsBuilder::adapter`]
//! (what it receives and sends). Its `id()` is the `connections.adapter` it serves.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_flow::{AdapterError, PushRequest};
use steve_modules::accounts::connections::{ConnCx, ConnectionProvider, ProviderError};

/// What a channel can do; the UI and the rules degrade by it (no buttons → “回复 1/2”, no recall →
/// no “撤销已发送”).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ChannelFeatures {
    pub multi_bot: bool,
    pub as_user: bool,
    pub cards: bool,
    pub buttons: bool,
    pub thread_reply: bool,
    pub edit: bool,
    pub recall: bool,
    pub members: bool,
    /// Emoji reactions on messages (the “收到 → 完成” lifecycle, [`ReactionKind`]).
    pub reactions: bool,
}

/// The reactions Steve puts on a message it answers. Each adapter maps
/// them to its own emoji ([`ChannelAdapter::reaction_emoji`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionKind {
    /// Received, working on it.
    Ack,
    /// Answered.
    Done,
    /// Could not answer.
    Failed,
}

impl ReactionKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Ack => "ack",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ack" => Self::Ack,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// `spaces.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    Group,
    Dm,
    /// A topic group (Feishu `chat_mode=topic`), a Slack channel.
    Channel,
    Thread,
    MailThread,
    Meeting,
    Pr,
    Issue,
}

impl SpaceKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Dm => "dm",
            Self::Channel => "channel",
            Self::Thread => "thread",
            Self::MailThread => "mail_thread",
            Self::Meeting => "meeting",
            Self::Pr => "pr",
            Self::Issue => "issue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "group" => Self::Group,
            "dm" => Self::Dm,
            "channel" => Self::Channel,
            "thread" => Self::Thread,
            "mail_thread" => Self::MailThread,
            "meeting" => Self::Meeting,
            "pr" => Self::Pr,
            "issue" => Self::Issue,
            _ => return None,
        })
    }

    /// Several people talk here (a new one opens the “新群” decision).
    pub fn is_group(self) -> bool {
        matches!(self, Self::Group | Self::Channel | Self::Thread)
    }
}

/// A handle as an adapter saw it: `system` is a `systems.id` (`feishu`, `feishu_app`, `slack`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleRef {
    pub system: String,
    pub handle: String,
    /// More handles of the same identity, valid only under this connection (Feishu `open_id`).
    pub extra: Vec<String>,
}

impl HandleRef {
    pub fn new(system: impl Into<String>, handle: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            handle: handle.into(),
            extra: Vec::new(),
        }
    }

    pub fn with_extra(mut self, h: impl Into<String>) -> Self {
        let h = h.into();
        if !h.is_empty() && h != self.handle && !self.extra.contains(&h) {
            self.extra.push(h);
        }
        self
    }

    /// The primary handle or one of the extra ones.
    pub fn is(&self, h: &str) -> bool {
        !h.is_empty() && (self.handle == h || self.extra.iter().any(|x| x == h))
    }
}

/// `space_info`: what a new space row gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceInfo {
    pub name: String,
    pub kind: SpaceKind,
    pub member_count: i64,
    pub has_external: bool,
}

/// One member of a space (`members`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    pub handle: HandleRef,
    /// Other platform/email handles for this participant, resolved by the common identity path.
    pub aliases: Vec<HandleRef>,
    pub name: String,
    /// `owner | admin | member | bot`
    pub role: String,
    /// Not of the connection's organization (another tenant).
    pub external: bool,
}

/// Where an outbound message goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTarget {
    /// A new message in the space.
    Space { ext_id: String },
    /// A reply to one message, in its thread when `in_thread`.
    Reply { ext_msg_id: String, in_thread: bool },
}

/// What is sent. Phase 1 is plain text. Build it with [`OutContent::text`] and set the rest, so
/// fields added later don't break callers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutContent {
    pub text: String,
    /// Idempotency key (the outbound `cm_…`): a retry does not send twice.
    pub uuid: Option<String>,
    /// People to @ at the start, as the provider's per-connection ids (Feishu `open_id`, found with
    /// `adapters::feishu::send::at_handle`). C6 passes the person replied to in a group; empty in
    /// DMs.
    pub at: Vec<String>,
    /// The space's provider id when known (for a reply: the space of the replied-to message). It
    /// keys the per-space send rate; without it only the per-connection rate applies.
    pub space_ext_id: Option<String>,
}

impl OutContent {
    /// Plain text, nothing else set.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// One stored message, as an adapter needs it to act on it again (recall, react, fetch media).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MessageRef {
    /// The space at the provider.
    pub space_ext_id: String,
    /// The provider's message id; `None` for a message without one (never a Hub-made id).
    pub ext_id: Option<String>,
    /// Unix ms: the provider's time when known, otherwise the Hub's send time.
    pub created: i64,
    /// `created` is the provider's time; `false`: the Hub's send time, match with a window.
    pub created_exact: bool,
    /// Who sent it, when known.
    pub sender: Option<HandleRef>,
    /// Its text; `None` once purged.
    pub text: Option<String>,
    pub thread_key: Option<String>,
    /// The remote's delivery id of the message ([`InboundMessage`]'s `source_id`, or the one a send
    /// returned), when known. Opaque, per connection.
    pub source_id: Option<String>,
}

impl MessageRef {
    /// A message with a provider id; the rest unknown.
    pub fn platform(space_ext_id: impl Into<String>, ext_id: impl Into<String>) -> Self {
        Self {
            space_ext_id: space_ext_id.into(),
            ext_id: Some(ext_id.into()),
            created: 0,
            created_exact: false,
            sender: None,
            text: None,
            thread_key: None,
            source_id: None,
        }
    }

    /// The provider's message id, if it has one.
    pub fn id(&self) -> Option<&str> {
        self.ext_id.as_deref()
    }
}

/// The emoji shown for a reaction kind when an adapter names none of its own.
pub fn default_emoji(kind: ReactionKind) -> &'static str {
    match kind {
        ReactionKind::Ack => "👀",
        ReactionKind::Done => "✅",
        ReactionKind::Failed => "❌",
    }
}

/// The idempotency key of one outbound message: derived from the Hub's outbound row id, the same
/// for every retry of that message and never reused for another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SendKey(pub String);

/// Where a message sent as you goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActTarget {
    /// A new message in the space.
    Space { ext_id: String },
    /// A reply to a stored message, in its thread when `in_thread`.
    Reply { to: MessageRef, in_thread: bool },
}

/// What a send as you returned. Build it with [`Sent::id`] or [`Sent::without_id`] and the
/// setters, so fields added later don't break adapters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Sent {
    /// The provider's message id, when it gives one.
    pub ext_id: Option<String>,
    /// The provider's create time (Unix ms), when it gives one.
    pub created: Option<i64>,
    /// The thread the message landed in, when the provider says.
    pub thread_key: Option<String>,
    /// The remote's delivery id of the send, when it has one: kept on the message so it can be
    /// acted on again (recall, reactions, media) without a provider message id.
    pub source_id: Option<String>,
}

impl Sent {
    /// Sent, with the provider's message id.
    pub fn id(ext_id: impl Into<String>) -> Self {
        Self {
            ext_id: Some(ext_id.into()),
            ..Self::default()
        }
    }

    /// Sent; the provider gave no message id.
    pub fn without_id() -> Self {
        Self::default()
    }

    pub fn created(mut self, ms: i64) -> Self {
        self.created = Some(ms);
        self
    }

    pub fn thread_key(mut self, k: impl Into<String>) -> Self {
        self.thread_key = Some(k.into());
        self
    }

    pub fn source_id(mut self, id: impl Into<String>) -> Self {
        self.source_id = Some(id.into());
        self
    }
}

/// Acting as you on a connection of your own account (`Mode::Own`): sending and replying, and,
/// when supported, reactions and recalling what was sent as you. Only the outbox calls it, after
/// your approval or your own trigger words; nothing here decides to send.
#[async_trait]
pub trait ActAsUser: Send + Sync {
    /// The `connections.adapter` it serves.
    fn adapter_id(&self) -> &str;

    /// Sends `content` as you. `key` is stable for one outbound message across every retry; the
    /// remote side may dedupe on it. Returns the provider's message id and create time (ms) when
    /// it knows them.
    async fn send(
        &self,
        cx: &ConnCx,
        to: &ActTarget,
        content: &OutContent,
        key: &SendKey,
    ) -> Result<Sent, AdapterError>;

    /// How long the provider drops a repeat of a send with the same `key`: within it, a send whose
    /// answer was lost may be sent again as is. `None`: no such guarantee.
    fn send_dedup(&self) -> Option<std::time::Duration> {
        None
    }

    /// Whether [`ActAsUser::add_reaction`] and [`ActAsUser::remove_reaction`] work.
    fn supports_reactions(&self) -> bool {
        false
    }

    /// The provider's emoji for a reaction kind (shown in the UI next to the message).
    fn reaction_emoji(&self, kind: ReactionKind) -> &'static str {
        default_emoji(kind)
    }

    /// Puts a reaction on a message as you; returns the provider's reaction id.
    async fn add_reaction(
        &self,
        cx: &ConnCx,
        msg: &MessageRef,
        kind: ReactionKind,
    ) -> Result<String, AdapterError> {
        let _ = (cx, msg, kind);
        Err(ProviderError::Unsupported.into())
    }

    /// Takes your reaction off again.
    async fn remove_reaction(
        &self,
        cx: &ConnCx,
        msg: &MessageRef,
        reaction_id: &str,
    ) -> Result<(), AdapterError> {
        let _ = (cx, msg, reaction_id);
        Err(ProviderError::Unsupported.into())
    }

    /// Whether [`ActAsUser::recall`] works.
    fn supports_recall(&self) -> bool {
        false
    }

    /// Recalls a message sent as you.
    async fn recall(&self, cx: &ConnCx, msg: &MessageRef) -> Result<(), AdapterError> {
        let _ = (cx, msg);
        Err(ProviderError::Unsupported.into())
    }
}

/// A change of a space or its members, as an adapter saw it (Feishu `im.chat.member.*`,
/// `im.chat.updated_v1`, `im.chat.disbanded_v1`). [`crate::Channels::on_space_event`]
/// applies it.
#[derive(Debug, Clone, PartialEq)]
pub enum SpaceEvent {
    /// The connection's account was added to a space: create it and, for a group a bot is new in,
    /// open the “新群” decision.
    Joined {
        space_ext_id: String,
        /// What the event says, used when `space_info` fails.
        name: Option<String>,
        has_external: Option<bool>,
    },
    /// The account was removed (`status=left`) or the space was disbanded (`archived`).
    Left {
        space_ext_id: String,
        archived: bool,
    },
    /// People joined; `has_external` is the provider's flag for the space when it sends one.
    MembersAdded {
        space_ext_id: String,
        members: Vec<MemberInfo>,
        has_external: Option<bool>,
    },
    /// People left.
    MembersRemoved {
        space_ext_id: String,
        members: Vec<HandleRef>,
        has_external: Option<bool>,
    },
    /// Name (or other settings) changed.
    Updated {
        space_ext_id: String,
        name: Option<String>,
        has_external: Option<bool>,
    },
}

impl SpaceEvent {
    pub fn space_ext_id(&self) -> &str {
        match self {
            Self::Joined { space_ext_id, .. }
            | Self::Left { space_ext_id, .. }
            | Self::MembersAdded { space_ext_id, .. }
            | Self::MembersRemoved { space_ext_id, .. }
            | Self::Updated { space_ext_id, .. } => space_ext_id,
        }
    }
}

/// What a webhook body turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum PushOutcome {
    /// Answer this as JSON and write nothing (URL verification challenge).
    Respond(Value),
    /// Messages for the inbound pipeline.
    Messages(Vec<InboundMessage>),
    /// Valid but nothing to do (an event type Steve does not handle).
    Ignored,
}

/// Who sent an inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Sender {
    /// The primary handle, in any system: the declared `handle_system`, or one of the adapter's own
    /// (its internal user id) when it has none there.
    pub handle: HandleRef,
    /// Chat nickname, when the payload carries one (Feishu's does not: [`crate::Channels`] looks it
    /// up in the members list).
    pub display_name: Option<String>,
    /// An app or bot, not a person.
    pub is_bot: bool,
    /// Of another organization (Feishu: another `tenant_key`).
    pub external: bool,
    /// More handles of the same person, in other systems (an email). The Hub matches an existing
    /// contact through the primary handle and every alias before it makes a new unconfirmed one,
    /// and attaches the handles it did not know to that contact.
    pub aliases: Vec<HandleRef>,
}

impl Sender {
    /// A person known by `handle` (in any system); no name, not a bot, not external, no aliases.
    pub fn new(handle: HandleRef) -> Self {
        Self {
            handle,
            display_name: None,
            is_bot: false,
            external: false,
            aliases: Vec::new(),
        }
    }

    /// One more handle of the same person, in another system (or another one in the same).
    pub fn alias(mut self, h: HandleRef) -> Self {
        if !h.handle.trim().is_empty() && h != self.handle && !self.aliases.contains(&h) {
            self.aliases.push(h);
        }
        self
    }

    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.display_name = (!name.trim().is_empty()).then_some(name);
        self
    }

    pub fn bot(mut self, is_bot: bool) -> Self {
        self.is_bot = is_bot;
        self
    }

    pub fn external(mut self, external: bool) -> Self {
        self.external = external;
        self
    }
}

/// Someone @-mentioned in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// `None` when the adapter knows no usable handle (another app's bot).
    pub handle: Option<HandleRef>,
    pub name: String,
    /// The connection's own account (the bot, or you on a self connection).
    pub is_connection: bool,
}

/// One message as an adapter saw it, before it is stored. Build it with [`InboundMessage::text`]
/// or [`InboundMessage::without_id`] and set the fields, so fields added later don't break
/// adapters.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InboundMessage {
    /// The space at the provider (Feishu `chat_id`).
    pub space_ext_id: String,
    /// What the payload says about the space, used when `space_info` is not available.
    pub space_kind: Option<SpaceKind>,
    pub space_name: Option<String>,
    /// The message id at the provider (`om_…`); unique per space. `""` only for an adapter
    /// without platform message ids ([`crate::ext::MessageIds::None`]).
    pub external_id: String,
    /// The provider's delivery id (Feishu `header.event_id`), for logs.
    pub event_id: Option<String>,
    /// Same thread (`thread_id ?? root_id ?? message_id`).
    pub thread_key: Option<String>,
    /// The message replied to (`parent_id`).
    pub reply_to: Option<String>,
    /// The root of its thread (`root_id`).
    pub root_id: Option<String>,
    pub sender: Sender,
    /// Plain text (mentions as `@名字`); may be empty for an image or file.
    pub text: String,
    /// `[{type:"image"|"file", key, name?}]`
    pub attachments: Vec<Value>,
    pub mentions: Vec<Mention>,
    /// Unix ms at the provider.
    pub created: i64,
    /// `ws` (long connection), `poll`, `push` (webhook): the signals page shows the delay of polls.
    pub via: Option<String>,
    /// The provider's payload, kept as the event's original until the retention period ends.
    pub raw: Value,
    /// The remote's stable id of this delivery (an event id or a sequence number): redeliveries
    /// carry the same one. Opaque, per connection, never shown. The Hub drops a delivery it already
    /// stored with this id, and acts on the message again through it
    /// ([`MessageRef::source_id`]).
    pub source_id: Option<String>,
    /// Unix ms at the provider of the message's last edit, when it was edited and the remote says
    /// so: how late it arrived is counted from there ([`crate::inbound::history_of`]), not from
    /// [`Self::created`].
    pub edited: Option<i64>,
    /// The remote says the connection's own account wrote it (an external channel's `is_mine`):
    /// it is yours, never answered, whatever handle the sender came under.
    pub mine: bool,
    /// Read back to fill a gap or the time before a resume, not delivered live: stored and only
    /// recorded ([`crate::inbound::History::Backfill`]).
    pub backfill: bool,
}

impl InboundMessage {
    /// A text message with the required fields; the rest empty.
    pub fn text(
        space_ext_id: impl Into<String>,
        external_id: impl Into<String>,
        sender: HandleRef,
        text: impl Into<String>,
        created: i64,
    ) -> Self {
        Self {
            space_ext_id: space_ext_id.into(),
            space_kind: None,
            space_name: None,
            external_id: external_id.into(),
            event_id: None,
            thread_key: None,
            reply_to: None,
            root_id: None,
            sender: Sender::new(sender),
            text: text.into(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            created,
            via: None,
            raw: Value::Null,
            source_id: None,
            edited: None,
            mine: false,
            backfill: false,
        }
    }

    /// A text message of an adapter without platform message ids
    /// ([`crate::ext::MessageIds::None`]): the Hub tells it apart by its [`Self::source_id`] and,
    /// failing that, by space, sender, content and time.
    pub fn without_id(
        space_ext_id: impl Into<String>,
        sender: Sender,
        text: impl Into<String>,
        created: i64,
    ) -> Self {
        let mut m = Self::text(space_ext_id, "", sender.handle.clone(), text, created);
        m.sender = sender;
        m
    }

    /// The remote's delivery id ([`Self::source_id`]); an empty one is none.
    pub fn source_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.source_id = (!id.trim().is_empty()).then_some(id);
        self
    }

    /// When the message was last edited at the provider (unix ms; [`Self::edited`]).
    pub fn edited(mut self, at: i64) -> Self {
        self.edited = Some(at);
        self
    }

    /// Written by the connection's own account, as the remote says ([`Self::mine`]).
    pub fn mine(mut self, mine: bool) -> Self {
        self.mine = mine;
        self
    }

    /// Read back, not delivered live: only recorded ([`Self::backfill`]).
    pub fn backfill(mut self) -> Self {
        self.backfill = true;
        self
    }

    /// The connection's account is @-mentioned.
    pub fn mentions_connection(&self) -> bool {
        self.mentions.iter().any(|m| m.is_connection)
    }
}

/// A channel provider. Only Feishu is real in phase 1; [`crate::fake::channel::FakeChannel`]
/// is the scripted one for tests.
#[async_trait]
pub trait ChannelAdapter: ConnectionProvider {
    fn features(&self) -> ChannelFeatures;

    /// A webhook body: verify the signature and decrypt; answer URL challenges as they are. Phase 1
    /// receives over the long connection, so the default refuses.
    async fn parse_push(
        &self,
        cx: &ConnCx,
        req: &PushRequest,
    ) -> Result<PushOutcome, AdapterError> {
        let _ = (cx, req);
        Err(ProviderError::Unsupported.into())
    }

    /// Whether a connection with this `config` receives every message of the spaces it is in (a
    /// bot that only gets the messages that @ it does not). Messages of spaces covered this way need
    /// not be read another way. Default: yes.
    fn receives_all(&self, config: &Value) -> bool {
        let _ = config;
        true
    }

    /// Name, kind, member count, external members.
    async fn space_info(&self, cx: &ConnCx, ext_id: &str) -> Result<SpaceInfo, AdapterError>;

    async fn members(&self, cx: &ConnCx, ext_id: &str) -> Result<Vec<MemberInfo>, AdapterError>;

    /// How long the provider drops a repeat of a send with the same dedupe key
    /// ([`OutContent::uuid`]): within it, a send whose answer was lost may be sent again as is.
    /// `None`: no such guarantee, a lost answer is never sent again by itself.
    fn send_dedup(&self) -> Option<std::time::Duration> {
        None
    }

    /// Sends and returns the provider's message id. Only `outbound::send` (C6) may call it.
    async fn send(
        &self,
        cx: &ConnCx,
        to: &SendTarget,
        content: &OutContent,
    ) -> Result<String, AdapterError>;

    async fn recall(&self, cx: &ConnCx, ext_msg_id: &str) -> Result<(), AdapterError>;

    /// The provider's emoji for a reaction kind (shown in the UI next to the message).
    fn reaction_emoji(&self, kind: ReactionKind) -> &'static str {
        default_emoji(kind)
    }

    /// Puts a reaction on a message as the connection's account; returns the provider's reaction
    /// id (to remove it later). Best-effort for callers: [`crate::reactions`] never lets a failure
    /// here stop a reply. Default: unsupported.
    async fn add_reaction(
        &self,
        cx: &ConnCx,
        ext_msg_id: &str,
        kind: ReactionKind,
    ) -> Result<String, AdapterError> {
        let _ = (cx, ext_msg_id, kind);
        Err(ProviderError::Unsupported.into())
    }

    /// Takes a reaction off again. Default: unsupported.
    async fn remove_reaction(
        &self,
        cx: &ConnCx,
        ext_msg_id: &str,
        reaction_id: &str,
    ) -> Result<(), AdapterError> {
        let _ = (cx, ext_msg_id, reaction_id);
        Err(ProviderError::Unsupported.into())
    }
}

// ---------------------------------------------------------------------------------------------
// Profiles: who is behind a handle.

/// What to look up: people seen in a space (its member list names them all at once), or handles on
/// their own (the provider's directory).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileQuery {
    /// The space at the provider (Feishu `chat_id`) whose members to list.
    pub space_ext_id: Option<String>,
    /// The handles still without a name; with a space, the list is read anyway and these are only
    /// a hint.
    pub handles: Vec<HandleRef>,
    /// Also tell the identity's own organization (key and name), when the provider can.
    pub want_tenant: bool,
}

/// One person as a provider describes them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    /// Every handle of them the answer carried (Feishu: `union_id` primary, `open_id` extra).
    pub handle: Option<HandleRef>,
    pub name: String,
    pub avatar_url: Option<String>,
    pub email: Option<String>,
    /// The provider's organization key (Feishu `tenant_key`).
    pub tenant_key: Option<String>,
    /// Found in the identity's own directory (not only in a chat).
    pub directory: bool,
    /// Directory department ids (to place them in the org chart).
    pub department_ids: Vec<String>,
    /// Job title (Feishu `job_title`; given only with `contact:user.employee:readonly` or a broader
    /// contact scope).
    pub title: Option<String>,
}

/// An identity's own organization.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TenantInfo {
    pub key: String,
    pub name: Option<String>,
}

/// What a lookup found, and which permissions stood in the way.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileAnswer {
    pub profiles: Vec<Profile>,
    /// Provider permissions that would have answered more: `(scope, what it is for)`.
    pub missing_scopes: Vec<(String, String)>,
    /// Permissions a call just proved granted (a call that worked).
    pub granted_scopes: Vec<String>,
    pub tenant: Option<TenantInfo>,
    /// The identity can't see that space (not a member): try the next identity.
    pub not_in_space: bool,
}

impl ProfileAnswer {
    pub fn missing(&mut self, scope: &str, purpose: &str) {
        if !self.missing_scopes.iter().any(|(s, _)| s == scope) {
            self.missing_scopes.push((scope.into(), purpose.into()));
        }
    }

    pub fn granted(&mut self, scope: &str) {
        if !self.granted_scopes.iter().any(|s| s == scope) {
            self.granted_scopes.push(scope.into());
        }
    }
}

/// Looks people up through one identity (a connection): the bot of an app, or you. Registered with
/// [`crate::ChannelsBuilder::profiles`] under the `connections.adapter` it serves; [`crate::names`]
/// asks the identities of one installation in turn — yours first, then the bot's.
#[async_trait]
pub trait ProfileAdapter: Send + Sync {
    /// The `connections.adapter` it serves.
    fn adapter_id(&self) -> &str;

    /// One lookup. Missing permissions are reported in the answer, not as an error; `Err` is for
    /// the connection itself failing (token refused, network).
    async fn resolve_profiles(
        &self,
        cx: &ConnCx,
        q: &ProfileQuery,
    ) -> Result<ProfileAnswer, AdapterError>;
}

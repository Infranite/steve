//! What an event source implements ([`EventAdapter`], signals-automation.md §2.1) and what every
//! producer of events hands to [`crate::ingest()`] ([`RawEvent`]).
//!
//! Two ways in:
//! - an adapter registered with [`crate::FlowSetup::event_adapter`]: the flow module polls it
//!   ([`IngestMode::Poll`]) or feeds it webhook bodies ([`IngestMode::Push`]), lists its scopes and
//!   event types on the source page;
//! - a module that already has the data (mail sync E1, GitHub G2, channels F1) calls
//!   [`crate::ingest()`] / [`EventSink::ingest`] directly with a [`RawEvent`]; it needs no adapter here
//!   unless it wants its own source page.

use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::http::HeaderMap;
use serde::Serialize;
use serde_json::Value;
use steve_modules::accounts::connections::{ConnCx, ConnectionProvider, ProviderError};

use crate::FlowResult;

/// One event type an adapter produces (`mail.new`, `cal.before`, `github.check_failed`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventTypeDef {
    pub key: &'static str,
    /// Chinese label (“收到新邮件”).
    pub label: &'static str,
    pub desc: &'static str,
    /// Default parameter, for types that take one (`cal.before`: the lead time, `"30m"`).
    pub param: Option<&'static str>,
    /// On for a connection that has no setting for this type yet.
    pub default_on: bool,
}

impl EventTypeDef {
    /// A type that is on by default and takes no parameter.
    pub const fn new(key: &'static str, label: &'static str, desc: &'static str) -> Self {
        Self {
            key,
            label,
            desc,
            param: None,
            default_on: true,
        }
    }
    pub const fn with_param(mut self, p: &'static str) -> Self {
        self.param = Some(p);
        self
    }
    pub const fn off(mut self) -> Self {
        self.default_on = false;
        self
    }
}

/// A scope of a connection: a mail folder, a calendar, a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeDef {
    pub ext_id: String,
    pub name: String,
}

impl ScopeDef {
    pub fn new(ext_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            ext_id: ext_id.into(),
            name: name.into(),
        }
    }
}

/// How events reach Steve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestMode {
    /// [`EventAdapter::poll`] every `every` (job `flow.poll`; failures back off to 30 minutes).
    Poll { every: Duration },
    /// Bodies posted to `POST /hooks/:connection_id`, parsed by [`EventAdapter::parse_push`].
    Push,
}

/// One poll's result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Batch {
    pub events: Vec<RawEvent>,
    /// The cursor to pass to the next poll (`None` keeps the old one).
    pub cursor: Option<String>,
}

/// A webhook request as it arrived (body ≤1 MiB, checked before the adapter sees it).
#[derive(Debug, Clone, Default)]
pub struct PushRequest {
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Errors of an adapter call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// Talking to the provider failed (auth → the accounts module's health logic).
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// A push whose signature does not verify: `401`, nothing written.
    #[error("签名不对")]
    BadSignature,
    /// A push body the adapter cannot read: `400`.
    #[error("{0}")]
    Malformed(String),
}

/// Who an event is from. Either a handle to identify (accounts §2.3) or an account the caller already
/// identified itself (mail and GitHub do their own `identify` while storing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorRef {
    /// Identified by [`steve_modules::accounts::identify`] on the connection owner's behalf.
    Handle {
        /// `systems.id` of the handle (`mail`, `github`, `feishu`).
        system: String,
        handle: String,
        display_name: Option<String>,
        /// The organization a mail signature names.
        signature_org: Option<String>,
    },
    /// Already identified.
    Known {
        account_id: String,
        party_id: String,
    },
}

impl ActorRef {
    /// A handle with only a display name.
    pub fn handle(
        system: impl Into<String>,
        handle: impl Into<String>,
        display_name: Option<String>,
    ) -> Self {
        Self::Handle {
            system: system.into(),
            handle: handle.into(),
            display_name,
            signature_org: None,
        }
    }
}

/// One event as its producer saw it (§2.1, plus `also` from email.md §3.5 and the channel fields of
/// channels-personas.md §3).
#[derive(Debug, Clone, PartialEq)]
pub struct RawEvent {
    /// `mail | cal | im | github | webhook | time | item | manual`.
    pub source: String,
    /// The main type (`mail.reply`).
    pub type_key: String,
    /// Other types that also hold (`mail.new` for a reply): a trigger matches when its `types`
    /// intersect `type_key ∪ also`.
    pub also: Vec<String>,
    /// Stable id at the provider; with the connection it is the dedupe key.
    pub external_id: String,
    /// Unix ms.
    pub occurred: i64,
    /// Same conversation (mail thread, PR).
    pub thread_key: Option<String>,
    /// Same kind, merged on the signals page (sender's domain, repository).
    pub group_key: Option<String>,
    /// The scope it came from (`INBOX`, `owner/repo`): matched against the source's scope modes.
    pub scope_ext_id: Option<String>,
    /// Channel messages: the space and message.
    pub space_id: Option<String>,
    pub message_id: Option<String>,
    pub actor: Option<ActorRef>,
    /// One line, kept forever; cut to 200 characters.
    pub summary: String,
    /// The original (JSON), dropped after the retention period. `Null` = none.
    pub payload: Value,
    /// Store it as `recorded` with this label and run no flow (mail backfill: “接入时同步的旧邮件”).
    pub record_only: Option<String>,
    /// Flows this event must not start (an item a flow made finishing does not re-trigger that flow).
    pub skip_flows: Vec<String>,
}

impl RawEvent {
    /// An event with the required fields; the rest empty.
    pub fn new(
        source: impl Into<String>,
        type_key: impl Into<String>,
        external_id: impl Into<String>,
        occurred: i64,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            type_key: type_key.into(),
            also: Vec::new(),
            external_id: external_id.into(),
            occurred,
            thread_key: None,
            group_key: None,
            scope_ext_id: None,
            space_id: None,
            message_id: None,
            actor: None,
            summary: summary.into(),
            payload: Value::Null,
            record_only: None,
            skip_flows: Vec::new(),
        }
    }

    /// `type_key` followed by `also`.
    pub fn types(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.type_key.as_str()).chain(self.also.iter().map(String::as_str))
    }
}

/// An event source (§2.1). Extends the connection adapter of the accounts module: the same object is
/// registered with `Connections` (how Steve gets in) and with [`crate::FlowSetup::event_adapter`]
/// (what it receives). Its `id()` is the `connections.adapter` it serves.
#[async_trait]
pub trait EventAdapter: ConnectionProvider {
    /// `ext_events.source` of its events (`mail`, `cal`, `github`, `webhook`).
    fn source(&self) -> &'static str;
    /// The types it produces, in display order.
    fn event_types(&self) -> &'static [EventTypeDef];
    /// Folders, calendars, repositories (`POST /sources/:adapter/refresh-scopes`).
    async fn scopes(&self, cx: &ConnCx) -> Result<Vec<ScopeDef>, AdapterError>;
    fn mode(&self) -> IngestMode;
    /// Events since `cursor` (`None`: start now; no backlog).
    async fn poll(&self, cx: &ConnCx, cursor: Option<&str>) -> Result<Batch, AdapterError>;
    /// Verifies the signature first ([`AdapterError::BadSignature`] → `401`), then parses.
    async fn parse_push(
        &self,
        cx: &ConnCx,
        req: &PushRequest,
    ) -> Result<Vec<RawEvent>, AdapterError>;
}

/// What [`crate::ingest()`] did with an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ingested {
    /// Stored as `evt_…`; `runs` are the flow runs opened for it (none: it was only recorded).
    Stored { id: String, runs: Vec<String> },
    /// Already there (same connection and `external_id`).
    Duplicate { id: String },
    /// Not stored; only counted.
    Dropped(DropReason),
}

impl Ingested {
    /// The event id, when stored now or before.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Stored { id, .. } | Self::Duplicate { id } => Some(id),
            Self::Dropped(_) => None,
        }
    }
}

/// Why an event was dropped (§2.2 step 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The source is paused (nothing is received meanwhile, nothing caught up later).
    Paused,
    /// The event type is turned off.
    TypeOff,
    /// The scope is set to `ignore`.
    ScopeIgnored,
}

/// The entry point for other modules, object-safe so they can hold `Arc<dyn EventSink>`. Implemented
/// by `Arc<FlowCx>`; mail (E1), GitHub (G2) and channels (F1) code against their own small trait and
/// the integrator forwards it here.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// [`crate::ingest()`]: `conn` is the connection the event came through (`None` for time, item and
    /// manual events).
    async fn ingest(&self, ws: &str, conn: Option<&str>, ev: RawEvent) -> FlowResult<Ingested>;
}

//! The `calendar.v1` profile: what every calendar source answers, the same for the built-in sources
//! (ICS, demo) and for a connector plugin that declares the profile in its hello (ext_ws §17). The
//! Hub defines the methods and fields; a source only translates its provider into them.
//!
//! The methods are called by the calendar service alone. They are never agent tools: an agent reads
//! the cache through `steve_calendar_*`, and later writes go through approvals.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::accounts::connections::ConnCx;

/// The profile id in a connector declaration.
pub const PROFILE: &str = "calendar.v1";

/// The read methods.
pub mod method {
    pub const CALENDARS: &str = "cal.calendars.list";
    pub const CHANGES: &str = "cal.events.changes";
    pub const LIST: &str = "cal.events.list";
    pub const GET: &str = "cal.events.get";
    pub const FREEBUSY: &str = "cal.freebusy";
}

/// The optional parts a source may have (`features`). The Hub falls back where one is missing.
pub mod feature {
    /// Incremental sync with a cursor (`cal.events.changes`); without it the window is listed.
    pub const CHANGES: &str = "changes";
    /// The provider expands recurring events itself; without it the source does.
    pub const INSTANCES: &str = "instances";
    pub const CONDITIONAL_WRITE: &str = "conditional_write";
    pub const EXT_PROPS: &str = "ext_props";
    pub const ICAL_UID: &str = "ical_uid";
    /// `cal.freebusy`: busy times of a calendar whose details the owner may not see.
    pub const FREEBUSY: &str = "freebusy";
    pub const ROOMS: &str = "rooms";
    pub const RSVP: &str = "rsvp";
    /// The source tells when to pull (`cal.changed`); not used for scheduling yet.
    pub const PUSH: &str = "push";
}

/// Most of one answer the Hub takes: occurrences per call, calendars per connection.
pub const MAX_EVENTS: usize = 5000;
pub const MAX_CALENDARS: usize = 100;

/// What a source declared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Features(pub BTreeSet<String>);

impl Features {
    pub fn of(xs: &[&str]) -> Self {
        Self(xs.iter().map(|s| s.to_string()).collect())
    }
    pub fn has(&self, f: &str) -> bool {
        self.0.contains(f)
    }
}

/// A person as a source names them: an email, a provider reference, or the owner themself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ext_ref: Option<String>,
    /// The connection's own user.
    #[serde(default, rename = "self", skip_serializing_if = "std::ops::Not::not")]
    pub me: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    #[serde(flatten)]
    pub who: Person,
    /// `accepted|declined|tentative|needs_action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rsvp: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub organizer: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// A calendar a connection reaches.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCalendar {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// `primary|shared|team|subscribed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// `free_busy|reader|writer|owner`: what the owner may do there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `private|busy|details`: who else sees it at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    /// Whose calendar it is, when not the owner's own (a colleague's shared calendar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Person>,
}

/// A start or end: `ms` for a timed event, `date` (`YYYY-MM-DD`) for an all-day one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct When {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

impl When {
    pub fn at(ms: i64) -> Self {
        Self {
            ms: Some(ms),
            ..Default::default()
        }
    }
    pub fn day(date: &str) -> Self {
        Self {
            date: Some(date.into()),
            ..Default::default()
        }
    }
}

/// One occurrence of an event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEvent {
    /// The provider's id of this occurrence.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_id: Option<String>,
    /// For an occurrence of a series: its start before any move, as the provider writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ical_uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub start: When,
    pub end: When,
    /// `confirmed|tentative|cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_rsvp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Person>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<Attendee>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_modify: bool,
}

/// `cal.events.changes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    #[serde(default)]
    pub events: Vec<RemoteEvent>,
    /// Ids of occurrences that are gone.
    #[serde(default)]
    pub deleted: Vec<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    /// The cursor was not usable: `events` is the whole window.
    #[serde(default)]
    pub full: bool,
}

/// A range in Unix ms, `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub from_ms: i64,
    pub to_ms: i64,
}

/// A busy time from `cal.freebusy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Busy {
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The source has no such method (a feature it did not declare).
    #[error("这个日历来源不支持这个操作")]
    Unsupported,
    /// The credential was refused: the connection needs a new authorization.
    #[error("{0}")]
    Auth(String),
    /// Network, timeout, a provider error: try again later.
    #[error("{0}")]
    Transient(String),
    /// The answer is not what the profile says.
    #[error("{0}")]
    Invalid(String),
}

/// A calendar source for the connections of one adapter.
#[async_trait]
pub trait CalendarSource: Send + Sync {
    /// The `connections.adapter` it serves.
    fn adapter(&self) -> &str;
    /// The features of this connection's calendar side; `None`: the connection is no calendar source
    /// (a connector without the profile).
    async fn features(&self, cx: &ConnCx) -> Result<Option<Features>, SourceError>;
    async fn calendars(&self, cx: &ConnCx) -> Result<Vec<RemoteCalendar>, SourceError>;
    /// With feature `changes`.
    async fn changes(
        &self,
        cx: &ConnCx,
        calendar: &str,
        cursor: Option<&str>,
        w: Window,
    ) -> Result<Changes, SourceError> {
        let _ = (cx, calendar, cursor, w);
        Err(SourceError::Unsupported)
    }
    /// Every occurrence overlapping `w`.
    async fn list(
        &self,
        cx: &ConnCx,
        calendar: &str,
        w: Window,
    ) -> Result<Vec<RemoteEvent>, SourceError>;
    /// One occurrence, fresh from the provider.
    async fn get(
        &self,
        cx: &ConnCx,
        calendar: &str,
        event: &str,
    ) -> Result<Option<RemoteEvent>, SourceError>;
    /// With feature `freebusy`.
    async fn freebusy(
        &self,
        cx: &ConnCx,
        calendar: &str,
        w: Window,
    ) -> Result<Vec<Busy>, SourceError> {
        let _ = (cx, calendar, w);
        Err(SourceError::Unsupported)
    }
}

/// A source that speaks the profile as JSON, one call per method: a connector plugin.
#[async_trait]
pub trait ProfileCall: Send + Sync {
    fn adapter(&self) -> &str;
    /// The `features` of the connection's `calendar.v1` declaration; `None` when it has none.
    async fn declared(&self, cx: &ConnCx) -> Result<Option<Features>, SourceError>;
    async fn call(&self, cx: &ConnCx, method: &str, args: Value) -> Result<Value, SourceError>;
}

/// [`CalendarSource`] over a [`ProfileCall`]: the profile's JSON in and out, checked.
pub struct JsonSource<P>(pub P);

fn parse<T: serde::de::DeserializeOwned>(v: Value, what: &str) -> Result<T, SourceError> {
    serde_json::from_value(v).map_err(|e| SourceError::Invalid(format!("{what}: {e}")))
}

fn bounded<T>(xs: Vec<T>, max: usize, what: &str) -> Result<Vec<T>, SourceError> {
    if xs.len() > max {
        return Err(SourceError::Invalid(format!("{what}: more than {max}")));
    }
    Ok(xs)
}

#[async_trait]
impl<P: ProfileCall> CalendarSource for JsonSource<P> {
    fn adapter(&self) -> &str {
        self.0.adapter()
    }
    async fn features(&self, cx: &ConnCx) -> Result<Option<Features>, SourceError> {
        self.0.declared(cx).await
    }
    async fn calendars(&self, cx: &ConnCx) -> Result<Vec<RemoteCalendar>, SourceError> {
        let mut v = self.0.call(cx, method::CALENDARS, json!({})).await?;
        bounded(
            parse(v["calendars"].take(), method::CALENDARS)?,
            MAX_CALENDARS,
            method::CALENDARS,
        )
    }
    async fn changes(
        &self,
        cx: &ConnCx,
        calendar: &str,
        cursor: Option<&str>,
        w: Window,
    ) -> Result<Changes, SourceError> {
        let v = self
            .0
            .call(
                cx,
                method::CHANGES,
                json!({"calendar": calendar, "cursor": cursor, "from_ms": w.from_ms, "to_ms": w.to_ms}),
            )
            .await?;
        let c: Changes = parse(v, method::CHANGES)?;
        bounded(c.events.clone(), MAX_EVENTS, method::CHANGES)?;
        Ok(c)
    }
    async fn list(
        &self,
        cx: &ConnCx,
        calendar: &str,
        w: Window,
    ) -> Result<Vec<RemoteEvent>, SourceError> {
        let mut v = self
            .0
            .call(
                cx,
                method::LIST,
                json!({"calendar": calendar, "from_ms": w.from_ms, "to_ms": w.to_ms}),
            )
            .await?;
        bounded(
            parse(v["events"].take(), method::LIST)?,
            MAX_EVENTS,
            method::LIST,
        )
    }
    async fn get(
        &self,
        cx: &ConnCx,
        calendar: &str,
        event: &str,
    ) -> Result<Option<RemoteEvent>, SourceError> {
        let mut v = self
            .0
            .call(
                cx,
                method::GET,
                json!({"calendar": calendar, "event": event}),
            )
            .await?;
        parse(v["event"].take(), method::GET)
    }
    async fn freebusy(
        &self,
        cx: &ConnCx,
        calendar: &str,
        w: Window,
    ) -> Result<Vec<Busy>, SourceError> {
        let mut v = self
            .0
            .call(
                cx,
                method::FREEBUSY,
                json!({"calendar": calendar, "from_ms": w.from_ms, "to_ms": w.to_ms}),
            )
            .await?;
        bounded(
            parse(v["busy"].take(), method::FREEBUSY)?,
            MAX_EVENTS,
            method::FREEBUSY,
        )
    }
}

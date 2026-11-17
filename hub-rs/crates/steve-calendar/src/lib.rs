//! Calendars as data sources. A calendar is reached through a connection like any other account:
//! the owner enables a source under 接入, and its calendars are read into a cache the Hub can
//! always rebuild. The provider stays the source of truth.
//!
//! - [`profile`]: the `calendar.v1` contract every source answers, the [`CalendarSource`] trait and
//!   [`profile::JsonSource`] for a connector plugin that declares the profile (ext_ws §17).
//! - [`sources`]: the built-in sources that need no outside service: an ICS subscription or an
//!   uploaded `.ics` file ([`sources::IcsSource`]), and a synthetic demo calendar
//!   ([`sources::DemoSource`], opt-in).
//! - [`ics`]: reading iCalendar files and expanding recurring events.
//! - [`sync`]: the sync engine: cursor changes when the source has them, else the window
//!   ([`Settings::past_days`] back, [`Settings::future_days`] ahead); a failure marks only that
//!   calendar and keeps its cache; event ids stay across resyncs; times only for someone else's
//!   calendar unless the owner chose details. [`merge`]: the same meeting in several calendars.
//! - [`query`]: agenda, search, one event, free/busy and finding free time, the same for the API and
//!   the tools. [`api`]: the routes. [`tools`]: `steve_calendar_agenda` / `_search` / `_get` /
//!   `_find_time`.
//!
//! Every calendar belongs to the connection it came from, is read only by the user who made that
//! connection, and belongs to a person (`party_id`): the owner's own, or someone else's.
//!
//! Integration (integrator): register each built-in source as a connection provider, build
//! [`Calendar`] with the sources (and a [`profile::JsonSource`] per connector adapter), merge
//! [`api::routes`], run [`Calendar::run`], dispatch [`tools`].

steve_i18n::init!();

pub mod api;
pub mod ics;
pub mod merge;
pub mod profile;
pub mod query;
pub mod repo;
pub mod sources;
pub mod sync;
pub mod tools;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use steve_modules::Modules;
use steve_modules::accounts::connections::Connections;

pub use profile::{CalendarSource, Features, SourceError};

/// A short, stable id from `parts` (hex of a SHA-256 over them, 24 characters).
pub fn hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    hex::encode(h.finalize())[..24].to_string()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The tunables, each with its name and default.
#[derive(Debug, Clone)]
pub struct Settings {
    /// How often a source is pulled (ms).
    pub poll_ms: i64,
    /// The longest wait after failures in a row (ms); the wait doubles from `poll_ms`.
    pub backoff_max_ms: i64,
    /// A sync asked for by hand within this time of the last one returns the cache (ms).
    pub manual_min_ms: i64,
    /// The cache window: days back and ahead of now.
    pub past_days: i64,
    pub future_days: i64,
    /// Working hours and days for finding free time (local `HH:MM`, Monday = 1).
    pub work_start: chrono::NaiveTime,
    pub work_end: chrono::NaiveTime,
    pub work_days: Vec<u32>,
    /// Free time is offered on this grid (minutes).
    pub slot_step_min: i64,
    /// How many free times `find_time` offers.
    pub slots: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_ms: 5 * 60_000,
            backoff_max_ms: 6 * 3_600_000,
            manual_min_ms: 60_000,
            past_days: 30,
            future_days: 180,
            work_start: chrono::NaiveTime::from_hms_opt(9, 0, 0).expect("time"),
            work_end: chrono::NaiveTime::from_hms_opt(18, 0, 0).expect("time"),
            work_days: vec![1, 2, 3, 4, 5],
            slot_step_min: 30,
            slots: 3,
        }
    }
}

/// The calendar service. Cheap to clone.
#[derive(Clone)]
pub struct Calendar(Arc<Inner>);

struct Inner {
    m: Modules,
    conns: Connections,
    sources: BTreeMap<String, Arc<dyn CalendarSource>>,
    settings: Settings,
    /// One sync per connection at a time.
    running: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Connection → (next due, failures in a row), for the background job.
    due: Mutex<HashMap<String, (i64, u32)>>,
}

pub struct CalendarBuilder {
    m: Modules,
    conns: Connections,
    sources: BTreeMap<String, Arc<dyn CalendarSource>>,
    settings: Settings,
}

impl CalendarBuilder {
    /// The source of the connections of its adapter.
    pub fn source(mut self, s: Arc<dyn CalendarSource>) -> Self {
        let id = s.adapter().to_string();
        assert!(
            self.sources.insert(id.clone(), s).is_none(),
            "calendar source {id} registered twice"
        );
        self
    }
    pub fn settings(mut self, s: Settings) -> Self {
        self.settings = s;
        self
    }
    pub fn build(self) -> Calendar {
        Calendar(Arc::new(Inner {
            m: self.m,
            conns: self.conns,
            sources: self.sources,
            settings: self.settings,
            running: Mutex::default(),
            due: Mutex::default(),
        }))
    }
}

impl Calendar {
    pub fn builder(m: Modules, conns: Connections) -> CalendarBuilder {
        CalendarBuilder {
            m,
            conns,
            sources: BTreeMap::new(),
            settings: Settings::default(),
        }
    }
    pub fn modules(&self) -> &Modules {
        &self.0.m
    }
    pub fn connections(&self) -> &Connections {
        &self.0.conns
    }
    pub fn settings(&self) -> &Settings {
        &self.0.settings
    }
    pub fn source(&self, adapter: &str) -> Option<Arc<dyn CalendarSource>> {
        self.0.sources.get(adapter).cloned()
    }
    pub fn adapters(&self) -> Vec<String> {
        self.0.sources.keys().cloned().collect()
    }
    fn now(&self) -> i64 {
        self.0.m.clock.now()
    }
}

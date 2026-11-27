//! The sync engine: one connection at a time, its calendars, then each enabled calendar's events.
//!
//! - With feature `changes` the source is asked for what changed since the stored cursor (none: the
//!   whole window); `full: true` replaces the window. A cursor never brings what only moved into
//!   the window, so once the last whole-window pull is [`crate::Settings::full_every_ms`] old the window
//!   is pulled again without one. Without it the window is listed and replaces
//!   what the cache had there.
//! - Someone else's calendar keeps only times (`detail = busy`) unless its owner chose details:
//!   with feature `freebusy` only busy times are asked for, otherwise what comes is stripped before
//!   it is written.
//! - A failure marks only that calendar (`sync_state = error`, `sync_error`); its cache stays and
//!   says when it was last right (`synced_at`).
//! - Event ids are a hash of (calendar, provider id, original start), so a resync keeps them.
//! - `version` goes up only for what matters to the people in a meeting: time, place, attendees,
//!   status, my reply.
//!
//! Nothing here starts a flow yet: calendar triggers come with the calendar automations.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{NaiveDate, NaiveTime};
use regex::Regex;
use serde::Serialize;
use steve_modules::accounts::connections::{ConnCx, Health};
use steve_modules::vault::VaultError;
use steve_modules::{ApiError, ApiResult};
use steve_runtime::tz::Zone;

use crate::profile::{
    Busy, Changes, RemoteCalendar, RemoteEvent, SourceError, When, Window, feature,
};
use crate::{Calendar, hash, ics, lock, merge};
use steve_store::calendar::{AttRow, CalRow, CalendarRepo, EventRow, Meeting, Written};
use steve_store::{
    Read, StoreError, accounts::Accounts, connections::Connections, parties::Parties,
};

/// Bounded calendar source label for metrics.
fn calendar_source_label(adapter: &str) -> &'static str {
    match adapter {
        "ics" => "ics",
        "demo" => "demo",
        "ext_ws" => "ext_ws",
        "ext_ws.bot" => "ext_ws.bot",
        _ => "other",
    }
}

fn record_calendar_sync(
    source: &'static str,
    result: &'static str,
    elapsed_secs: f64,
    changed: usize,
    deleted: usize,
) {
    thread_local! {
        static SYNC_TOTAL: std::cell::RefCell<HashMap<(&'static str, &'static str), metrics::Counter>> =
            std::cell::RefCell::new(HashMap::new());
        static SYNC_DURATION: std::cell::RefCell<HashMap<&'static str, metrics::Histogram>> =
            std::cell::RefCell::new(HashMap::new());
        static EVENTS_SYNCED: std::cell::RefCell<HashMap<(&'static str, &'static str), metrics::Counter>> =
            std::cell::RefCell::new(HashMap::new());
    }
    SYNC_TOTAL.with(|c| {
        c.borrow_mut()
            .entry((source, result))
            .or_insert_with(|| {
                metrics::counter!(
                    "steve_calendar_sync_total",
                    "source" => source,
                    "result" => result
                )
            })
            .increment(1);
    });
    SYNC_DURATION.with(|c| {
        c.borrow_mut()
            .entry(source)
            .or_insert_with(
                || metrics::histogram!("steve_calendar_sync_duration_seconds", "source" => source),
            )
            .record(elapsed_secs);
    });
    if changed > 0 || deleted > 0 {
        EVENTS_SYNCED.with(|c| {
            let mut c = c.borrow_mut();
            if changed > 0 {
                c.entry((source, "changed"))
                    .or_insert_with(|| {
                        metrics::counter!(
                            "steve_calendar_events_synced_total",
                            "source" => source,
                            "op" => "changed"
                        )
                    })
                    .increment(changed as u64);
            }
            if deleted > 0 {
                c.entry((source, "deleted"))
                    .or_insert_with(|| {
                        metrics::counter!(
                            "steve_calendar_events_synced_total",
                            "source" => source,
                            "op" => "deleted"
                        )
                    })
                    .increment(deleted as u64);
            }
        });
    }
}

/// Text kept of an event: title and place, the description's start.
const TITLE_MAX: usize = 500;
const EXCERPT_MAX: usize = 500;
const URL_MAX: usize = 2048;
/// Attendees kept per event.
const ATTENDEES_MAX: usize = 200;

pub(crate) fn api(e: VaultError) -> ApiError {
    match e {
        VaultError::Api(a) => a,
        e => ApiError::Rule(e.to_string()),
    }
}

/// What one sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// The connection is a calendar source.
    pub source: bool,
    pub calendars: usize,
    /// Events written new or changed in a way that matters.
    pub changed: usize,
    pub deleted: usize,
    /// Per calendar that failed: its name and why.
    pub errors: Vec<(String, String)>,
    /// A sync asked for by hand right after another: nothing was pulled.
    pub skipped: bool,
}

/// A meeting link in text: the hosts of the common video meeting services, https only.
pub fn meeting_link(text: &str) -> Option<String> {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"https://(?:[a-z0-9-]+\.)*(?:zoom\.us|zoom\.com\.cn|meet\.google\.com|teams\.microsoft\.com|teams\.live\.com|webex\.com|meeting\.tencent\.com|vc\.feishu\.cn|meetings\.feishu\.cn|vc\.larksuite\.com|meetings\.larksuite\.com|meeting\.dingtalk\.com|whereby\.com|meet\.jit\.si)(?:/[^\s<>\x22')\]]*)?")
            .expect("meeting link pattern")
    });
    RE.find(text)
        .map(|m| m.as_str().to_string())
        .filter(|u| u.len() <= URL_MAX)
}

fn cut(s: Option<&str>, n: usize) -> Option<String> {
    let s = s?.trim();
    if s.is_empty() {
        return None;
    }
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(n)
        .collect();
    if s.chars().count() > n {
        out.push('…');
    }
    Some(out)
}

/// `s` cut to `n` characters (with `…`).
pub(crate) fn cut_text(s: &str, n: usize) -> String {
    cut(Some(s), n).unwrap_or_default()
}

fn excerpt(s: Option<&str>) -> Option<String> {
    let flat = s?.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(Some(&flat), EXCERPT_MAX)
}

/// The contact an email belongs to, following a merge when necessary.
fn party_of_email(c: &mut impl Read, ws: &str, email: &str) -> steve_store::Result<Option<String>> {
    let party = Accounts::new(c).party_by_email(ws, &email.trim().to_ascii_lowercase())?;
    party.map(|id| Parties::new(c).current(id)).transpose()
}

/// A source's zone when supplied, otherwise the owner's.
fn tz_of(c: &mut impl Read, ws: &str, user: &str, name: Option<&str>) -> steve_store::Result<Zone> {
    if let Some(zone) = name.and_then(|n| Zone::parse(n).ok()) {
        return Ok(zone);
    }
    let policy = crate::query::user_tz(c, ws, user)?;
    Ok(Zone::parse(policy.name()).expect("policy IANA zone"))
}

fn day(d: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()
}

struct EventTimes {
    all_day: bool,
    start_ms: i64,
    end_ms: i64,
    start_date: Option<String>,
    end_date: Option<String>,
}

/// Resolve timed/all-day boundaries in the calendar's zone, retaining the provider's defaults.
fn event_times(e: &RemoteEvent, zone: &Zone) -> Result<EventTimes, String> {
    let (all_day, start_ms, end_ms, start_date, end_date) = match (&e.start, &e.end) {
        (When { ms: Some(s), .. }, end) => {
            let end = end.ms.unwrap_or(*s).max(*s);
            (false, *s, end, None, None)
        }
        (When { date: Some(s), .. }, end) => {
            let s = day(s).ok_or("bad start date")?;
            let end = end
                .date
                .as_deref()
                .and_then(day)
                .filter(|d| *d > s)
                .unwrap_or(s + chrono::Duration::days(1));
            let at = |d: NaiveDate| {
                zone.instant(d.and_time(NaiveTime::MIN))
                    .map_err(|e| e.to_string())
            };
            (
                true,
                at(s)?,
                at(end)?,
                Some(s.format("%Y-%m-%d").to_string()),
                Some(end.format("%Y-%m-%d").to_string()),
            )
        }
        _ => return Err("event without a start".into()),
    };
    Ok(EventTimes {
        all_day,
        start_ms,
        end_ms,
        start_date,
        end_date,
    })
}

/// Who the people in an event are.
pub(crate) struct People<'a, C> {
    pub(crate) c: &'a mut C,
    pub(crate) ws: &'a str,
    pub(crate) me: Option<&'a str>,
    pub(crate) cache: HashMap<String, Option<String>>,
}

impl<C: Read> People<'_, C> {
    fn of(&mut self, email: Option<&str>) -> steve_store::Result<Option<String>> {
        let Some(email) = email else { return Ok(None) };
        let key = email.trim().to_ascii_lowercase();
        if let Some(p) = self.cache.get(&key) {
            return Ok(p.clone());
        }
        let party = party_of_email(self.c, self.ws, &key)?;
        self.cache.insert(key, party.clone());
        Ok(party)
    }
}

pub(crate) enum NormalizeError {
    Invalid(String),
    Store(StoreError),
}

impl From<StoreError> for NormalizeError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// `e` as the cache keeps it, or why it is refused. `busy`: only its time.
pub(crate) fn normalize(
    cal: &CalRow,
    e: &RemoteEvent,
    busy: bool,
    people: &mut People<'_, impl Read>,
) -> Result<(EventRow, Vec<AttRow>), NormalizeError> {
    let id = e.id.trim();
    if id.is_empty() || id.len() > 1024 {
        return Err(NormalizeError::Invalid("event without a valid id".into()));
    }
    let zone = tz_of(
        people.c,
        &cal.workspace_id,
        &cal.owner_user,
        cal.tz.as_deref(),
    )?;
    let EventTimes {
        all_day,
        start_ms,
        end_ms,
        start_date,
        end_date,
    } = event_times(e, &zone).map_err(NormalizeError::Invalid)?;
    let status = match e.status.as_deref() {
        Some(s @ ("confirmed" | "tentative" | "cancelled")) => s.to_string(),
        _ => "confirmed".into(),
    };
    let rsvp_ok = |r: Option<&str>| {
        r.filter(|r| matches!(*r, "accepted" | "declined" | "tentative" | "needs_action"))
            .map(str::to_string)
    };
    let mut atts: Vec<AttRow> = Vec::new();
    let (mut org_party, mut org_email, mut org_name, mut i_org) = (None, None, None, false);
    let mut my_rsvp = rsvp_ok(e.my_rsvp.as_deref());
    let (mut title, mut location, mut link, mut text) = (None, None, None, None);
    if !busy {
        if let Some(o) = &e.organizer {
            org_email = o.email.as_deref().map(|x| x.trim().to_ascii_lowercase());
            org_party = people.of(org_email.as_deref())?;
            org_name = cut(o.name.as_deref(), 200);
            i_org = o.me || (org_party.is_some() && org_party.as_deref() == people.me);
        }
        for a in e.attendees.iter().take(ATTENDEES_MAX) {
            let email = a
                .who
                .email
                .as_deref()
                .map(|x| x.trim().to_ascii_lowercase());
            let party = people.of(email.as_deref())?;
            let is_self = a.who.me || (party.is_some() && party.as_deref() == people.me);
            let rsvp = rsvp_ok(a.rsvp.as_deref());
            if is_self && my_rsvp.is_none() {
                my_rsvp = rsvp.clone();
            }
            atts.push(AttRow {
                party_id: party,
                email,
                name: cut(a.who.name.as_deref(), 200),
                ext_ref: cut(a.who.ext_ref.as_deref(), 512),
                rsvp,
                organizer: a.organizer,
                optional: a.optional,
                is_self,
            });
        }
        title = cut(e.title.as_deref(), TITLE_MAX);
        location = cut(e.location.as_deref(), TITLE_MAX);
        link = e
            .meeting_url
            .as_deref()
            .filter(|u| u.starts_with("https://") && u.len() <= URL_MAX)
            .map(str::to_string)
            .or_else(|| e.location.as_deref().and_then(meeting_link))
            .or_else(|| e.description.as_deref().and_then(meeting_link));
        text = excerpt(e.description.as_deref());
    } else {
        my_rsvp = None;
    }
    let mut who: Vec<String> = atts
        .iter()
        .map(|a| {
            format!(
                "{}={}",
                a.email.clone().or(a.ext_ref.clone()).unwrap_or_default(),
                a.rsvp.clone().unwrap_or_default()
            )
        })
        .collect();
    who.sort();
    let fingerprint = hash(&[
        &start_ms.to_string(),
        &end_ms.to_string(),
        &all_day.to_string(),
        start_date.as_deref().unwrap_or(""),
        end_date.as_deref().unwrap_or(""),
        location.as_deref().unwrap_or(""),
        &status,
        my_rsvp.as_deref().unwrap_or(""),
        &who.join(","),
    ]);
    let original = e
        .original_start
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    Ok((
        EventRow {
            id: format!("ev_{}", hash(&[&cal.id, id, original.unwrap_or("")])),
            calendar_id: cal.id.clone(),
            workspace_id: cal.workspace_id.clone(),
            owner_user: cal.owner_user.clone(),
            meeting_id: None,
            ext_id: id.to_string(),
            series_ext_id: cut(e.series_id.as_deref(), 1024),
            original_start: original.map(str::to_string),
            ical_uid: if busy {
                None
            } else {
                cut(e.ical_uid.as_deref(), 1024)
            },
            sequence: e.sequence,
            etag: cut(e.etag.as_deref(), 512),
            version: 1,
            fingerprint,
            title,
            location,
            meeting_url: link,
            excerpt: text,
            start_ms,
            end_ms,
            tz: e.start.tz.clone().filter(|z| ics::zone(z).is_some()),
            all_day,
            start_date,
            end_date,
            status,
            my_rsvp,
            organizer_party: org_party,
            organizer_email: org_email,
            organizer_name: org_name,
            i_am_organizer: i_org,
            can_modify: !busy && e.can_modify && matches!(cal.role.as_str(), "writer" | "owner"),
            busy_only: busy,
            deleted_ms: None,
        },
        atts,
    ))
}

fn busy_event(b: &Busy) -> RemoteEvent {
    RemoteEvent {
        id: format!("busy:{}:{}", b.start_ms, b.end_ms),
        start: When::at(b.start_ms),
        end: When::at(b.end_ms.max(b.start_ms)),
        ..Default::default()
    }
}

fn source_text(e: &SourceError) -> String {
    let s = e.to_string();
    s.chars().take(300).collect()
}

/// What a calendar's events came back as.
enum Pulled {
    /// The whole window.
    Full(Vec<RemoteEvent>, Option<String>),
    /// Changes since the cursor.
    Delta(Changes),
}

impl Calendar {
    fn window(&self) -> Window {
        let now = self.now();
        let s = self.settings();
        Window {
            from_ms: now - s.past_days * 86_400_000,
            to_ms: now + s.future_days * 86_400_000,
        }
    }

    fn running(&self, conn: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.0.running)
            .entry(conn.to_string())
            .or_default()
            .clone()
    }

    /// Whether `conn` is a calendar source now: its adapter has a source, it is up, and it reads
    /// calendars (built-in sources: `cal.read` is on; a connector: it declares the profile).
    pub async fn is_source(
        &self,
        ws: &str,
        conn: &str,
    ) -> ApiResult<Option<(ConnCx, crate::Features)>> {
        let row = self
            .0
            .m
            .store
            .read(|r| Connections::new(r).source(ws, conn))?;
        let Some(row) = row else { return Ok(None) };
        let Some(src) = self.source(&row.adapter) else {
            return Ok(None);
        };
        if !matches!(row.status.as_str(), "ok" | "error") {
            return Ok(None);
        }
        let builtin = self
            .connections()
            .provider(&row.adapter)
            .is_some_and(|p| p.caps().contains(&"cal.read"));
        if builtin && !row.caps.iter().any(|c| c == "cal.read") {
            return Ok(None);
        }
        let cx = self.connections().cx(ws, conn).map_err(api)?;
        match src.features(&cx).await {
            Ok(Some(f)) => Ok(Some((cx, f))),
            Ok(None) => Ok(None),
            Err(e) => {
                tracing::debug!(connection = conn, "calendar features unavailable: {e}");
                Ok(None)
            }
        }
    }

    /// Syncs one connection now. `manual`: asked for by hand, skipped right after another sync.
    pub async fn sync_connection(
        &self,
        ws: &str,
        conn: &str,
        manual: bool,
    ) -> ApiResult<SyncReport> {
        let guard = self.running(conn);
        let _held = guard.lock().await;
        let now = self.now();
        if manual {
            let last = self
                .0
                .m
                .store
                .read(|r| CalendarRepo::new(r).last_synced(conn))?;
            if last.is_some_and(|t| now - t < self.settings().manual_min_ms) {
                return Ok(SyncReport {
                    source: true,
                    skipped: true,
                    ..Default::default()
                });
            }
        }
        let Some((cx, features)) = self.is_source(ws, conn).await? else {
            return Ok(SyncReport::default());
        };
        let src = self
            .source(cx.adapter())
            .expect("is_source checked the adapter");
        let source_label = calendar_source_label(cx.adapter());
        let started = std::time::Instant::now();
        let mut report = SyncReport {
            source: true,
            ..Default::default()
        };
        let remotes = match src.calendars(&cx).await {
            Ok(r) => r,
            Err(e) => {
                let msg = source_text(&e);
                self.0.m.store.write(|tx| {
                    for c in CalendarRepo::new(tx).of_connection(conn)? {
                        CalendarRepo::new(tx).set_failed(&c.id, &msg, now)?;
                        tx.emit(steve_types::AutomationEvent::Calendar {
                            id: c.id.into(),
                            ws: ws.into(),
                        });
                    }
                    Ok::<_, ApiError>(())
                })?;
                cx.report(health(&e)).await;
                report.errors.push((String::new(), msg));
                record_calendar_sync(source_label, "error", started.elapsed().as_secs_f64(), 0, 0);
                return Ok(report);
            }
        };
        let calendars = self.upsert_calendars(ws, conn, &remotes)?;
        report.calendars = calendars.len();
        let w = self.window();
        let mut worst: Option<SourceError> = None;
        let mut touched = Vec::new();
        for cal in calendars.iter().filter(|c| c.enabled && !c.gone) {
            let busy = cal.detail == "busy" || cal.role == "free_busy";
            let pulled = if busy && features.has(feature::FREEBUSY) {
                src.freebusy(&cx, &cal.ext_id, w)
                    .await
                    .map(|b| Pulled::Full(b.iter().map(busy_event).collect(), None))
            } else if features.has(feature::CHANGES) {
                // a cursor only brings what changed, so what newly enters the window never comes
                // by it: the whole window is pulled again once it is a day old
                let cursor = cal.sync_cursor.as_deref().filter(|_| {
                    cal.full_at
                        .is_some_and(|t| now - t < self.settings().full_every_ms)
                });
                src.changes(&cx, &cal.ext_id, cursor, w).await.map(|c| {
                    if c.full || cursor.is_none() {
                        Pulled::Full(c.events, c.cursor)
                    } else {
                        Pulled::Delta(c)
                    }
                })
            } else {
                src.list(&cx, &cal.ext_id, w)
                    .await
                    .map(|e| Pulled::Full(e, None))
            };
            match pulled {
                Ok(p) => {
                    let (changed, deleted) = self.apply(cal, busy, p, w)?;
                    report.changed += changed;
                    report.deleted += deleted;
                    touched.push(cal.id.clone());
                }
                Err(e) => {
                    let msg = source_text(&e);
                    self.0.m.store.write(|tx| {
                        CalendarRepo::new(tx).set_failed(&cal.id, &msg, self.now())?;
                        tx.emit(steve_types::AutomationEvent::Calendar {
                            id: cal.id.clone().into(),
                            ws: ws.into(),
                        });
                        Ok::<_, ApiError>(())
                    })?;
                    report.errors.push((cal.name.clone(), msg));
                    worst = Some(match (worst, e) {
                        (Some(SourceError::Auth(a)), _) | (_, SourceError::Auth(a)) => {
                            SourceError::Auth(a)
                        }
                        (_, e) => e,
                    });
                }
            }
        }
        if let Some(owner) = calendars.first().map(|c| c.owner_user.clone()) {
            self.rematch(ws, &owner)?;
        }
        cx.report(worst.as_ref().map(health).unwrap_or(Health::Ok))
            .await;
        if let Err(e) = self.connections().touch_sync(ws, conn) {
            tracing::debug!(connection = conn, "last sync not recorded: {e}");
        }
        let result = if report.errors.is_empty() {
            "ok"
        } else {
            "partial"
        };
        record_calendar_sync(
            source_label,
            result,
            started.elapsed().as_secs_f64(),
            report.changed,
            report.deleted,
        );
        Ok(report)
    }

    /// Writes the calendars a source lists; those it no longer lists are marked gone (their events go).
    fn upsert_calendars(
        &self,
        ws: &str,
        conn: &str,
        remotes: &[RemoteCalendar],
    ) -> ApiResult<Vec<CalRow>> {
        let now = self.now();
        self.0.m.store.write(|tx| {
            let row = Connections::new(tx)
                .source(ws, conn)?
                .ok_or_else(|| ApiError::not_found("连接"))?;
            let owner = row.created_by;
            let me = Parties::new(tx).active_user(ws, &owner)?;
            let account_party = Accounts::new(tx)
                .party(&row.account_id)?
                .map(|p| Parties::new(tx).current(p))
                .transpose()?;
            let have = CalendarRepo::new(tx).of_connection(conn)?;
            let mut seen = HashSet::new();
            for r in remotes.iter().take(crate::profile::MAX_CALENDARS) {
                let ext = r.id.trim();
                if ext.is_empty() || ext.len() > 1024 || !seen.insert(ext.to_string()) {
                    continue;
                }
                let party = match &r.owner {
                    Some(o) if !o.me => o
                        .email
                        .as_deref()
                        .map(|e| party_of_email(tx, ws, e))
                        .transpose()?
                        .flatten(),
                    _ => account_party.clone().or(me.clone()),
                };
                let mine = party.is_some() && party == me;
                let pick = |v: Option<&str>, ok: &[&str], def: &str| {
                    v.filter(|v| ok.contains(v)).unwrap_or(def).to_string()
                };
                let role = pick(
                    r.role.as_deref(),
                    &["free_busy", "reader", "writer", "owner"],
                    "reader",
                );
                let mut cal = CalRow {
                    id: format!("cal_{}", hash(&[conn, ext])),
                    workspace_id: ws.into(),
                    connection_id: conn.into(),
                    owner_user: owner.clone(),
                    ext_id: ext.into(),
                    name: cut(Some(&r.name), 200)
                        .unwrap_or_else(|| ext.chars().take(200).collect()),
                    color: r
                        .color
                        .as_deref()
                        .filter(|c| c.len() <= 32)
                        .map(str::to_string),
                    kind: pick(
                        r.kind.as_deref(),
                        &["primary", "shared", "team", "subscribed"],
                        if mine { "primary" } else { "shared" },
                    ),
                    share: pick(
                        r.share.as_deref(),
                        &["private", "busy", "details"],
                        "private",
                    ),
                    detail: if mine && role != "free_busy" {
                        "full".into()
                    } else {
                        "busy".into()
                    },
                    role,
                    party_id: party,
                    party_set: false,
                    enabled: true,
                    tz: Some(tz_of(tx, ws, &owner, r.tz.as_deref())?.name().to_string()),
                    sync_cursor: None,
                    synced_at: None,
                    full_at: None,
                    sync_state: "new".into(),
                    sync_error: None,
                    gone: false,
                };
                let differs = match have.iter().find(|h| h.id == cal.id) {
                    None => {
                        CalendarRepo::new(tx).insert_calendar(&cal, now)?;
                        true
                    }
                    Some(h) => {
                        // the owner's choices stay: the person, how much is kept, the colour they set
                        if h.party_set {
                            cal.party_id = h.party_id.clone();
                        }
                        cal.detail = if cal.role == "free_busy" {
                            "busy".into()
                        } else {
                            h.detail.clone()
                        };
                        if h.party_set || h.color.is_some() && cal.color.is_none() {
                            cal.color = h.color.clone();
                        }
                        if cal.detail == "busy" && h.detail != "busy" {
                            CalendarRepo::new(tx).strip_details(&cal.id, now)?;
                        }
                        let differs = (
                            &h.name,
                            &h.color,
                            &h.kind,
                            &h.role,
                            &h.share,
                            &h.tz,
                            &h.party_id,
                            &h.detail,
                            h.gone,
                        ) != (
                            &cal.name,
                            &cal.color,
                            &cal.kind,
                            &cal.role,
                            &cal.share,
                            &cal.tz,
                            &cal.party_id,
                            &cal.detail,
                            false,
                        );
                        if differs {
                            CalendarRepo::new(tx).refresh_calendar(&cal, h.tz != cal.tz, now)?;
                        }
                        differs
                    }
                };
                // pages hear of a calendar only when something about it changed
                if differs {
                    tx.emit(steve_types::AutomationEvent::Calendar {
                        id: cal.id.clone().into(),
                        ws: ws.into(),
                    });
                }
            }
            for h in have.iter().filter(|h| !h.gone && !seen.contains(&h.ext_id)) {
                CalendarRepo::new(tx).set_gone(&h.id, now)?;
                tx.emit(steve_types::AutomationEvent::Calendar {
                    id: h.id.clone().into(),
                    ws: ws.into(),
                });
            }
            Ok(CalendarRepo::new(tx).of_connection(conn)?)
        })
    }

    /// Writes what a calendar's pull brought: (changed, deleted).
    fn apply(&self, cal: &CalRow, busy: bool, p: Pulled, w: Window) -> ApiResult<(usize, usize)> {
        let now = self.now();
        let ws = cal.workspace_id.clone();
        self.0.m.store.write(|tx| {
            let me = Parties::new(tx).active_user(&ws, &cal.owner_user)?;
            let mut people = People {
                c: tx,
                ws: &ws,
                me: me.as_deref(),
                cache: HashMap::new(),
            };
            let (mut changed, mut deleted) = (0, 0);
            let (events, gone, cursor, full) = match p {
                Pulled::Full(e, cursor) => (e, Vec::new(), cursor, true),
                Pulled::Delta(c) => (c.events, c.deleted, c.cursor, false),
            };
            let mut kept = HashSet::new();
            for e in events.iter().take(crate::profile::MAX_EVENTS) {
                let (row, atts) = match normalize(cal, e, busy, &mut people) {
                    Ok(x) => x,
                    Err(NormalizeError::Invalid(why)) => {
                        tracing::debug!(calendar = %cal.id, "event skipped: {why}");
                        continue;
                    }
                    Err(NormalizeError::Store(error)) => return Err(error.into()),
                };
                kept.insert(row.id.clone());
                if matches!(
                    CalendarRepo::new(people.c).write_event(&row, &atts, now)?,
                    Written::New | Written::Changed
                ) {
                    changed += 1;
                }
            }
            if full {
                for old in CalendarRepo::new(tx).events_of(&cal.id)? {
                    if !kept.contains(&old.id)
                        && old.start_ms < w.to_ms
                        && old.end_ms > w.from_ms
                        && CalendarRepo::new(tx).delete_event(&old.id, now)?
                    {
                        deleted += 1;
                    }
                }
            } else {
                for ext in &gone {
                    deleted +=
                        CalendarRepo::new(tx).delete_occurrences(&cal.id, ext, now)? as usize;
                }
            }
            CalendarRepo::new(tx).prune(&cal.id, w.from_ms)?;
            CalendarRepo::new(tx).set_synced(&cal.id, cursor.as_deref(), full, now)?;
            // a sync that found nothing new is not news, unless the calendar was failing or never synced
            if changed + deleted > 0 || cal.sync_state != "ok" {
                tx.emit(steve_types::AutomationEvent::Calendar {
                    id: cal.id.clone().into(),
                    ws: ws.clone().into(),
                });
            }
            Ok((changed, deleted))
        })
    }

    /// Recomputes which occurrences of `owner`'s calendars are one meeting ([`merge`]).
    pub(crate) fn rematch(&self, ws: &str, owner: &str) -> ApiResult<()> {
        let now = self.now();
        self.0.m.store.write(|tx| {
            let cals = CalendarRepo::new(tx).of_user(ws, owner)?;
            let party: HashMap<String, String> = cals
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        c.party_id
                            .clone()
                            .unwrap_or_else(|| format!("cal:{}", c.id)),
                    )
                })
                .collect();
            let mut items = Vec::new();
            for c in cals.iter().filter(|c| !c.gone) {
                for e in CalendarRepo::new(tx).events_of(&c.id)? {
                    items.push(merge::Item {
                        party: party[&c.id].clone(),
                        calendar: c.id.clone(),
                        ical_uid: e.ical_uid.clone(),
                        original_start: e.original_start.clone(),
                        organizer: e.organizer_party.clone().or(e.organizer_email.clone()),
                        start_ms: e.start_ms,
                        title: e.title.clone(),
                        can_modify: e.can_modify,
                        has_link: e.meeting_url.is_some(),
                        busy_only: e.busy_only,
                        meeting: e.meeting_id.clone(),
                        id: e.id,
                    });
                }
            }
            let mut repo = CalendarRepo::new(tx);
            let not_same = repo.different_occurrences(ws, owner)?.into_iter().collect();
            let ms = merge::meetings(&items, &not_same);
            let ids: Vec<&str> = ms.iter().map(|m| m.id.as_str()).collect();
            repo.prune_meetings(ws, owner, &ids)?;
            for m in &ms {
                repo.put_meeting(
                    ws,
                    owner,
                    Meeting {
                        id: &m.id,
                        primary: &m.primary,
                        key: &m.key,
                    },
                    now,
                )?;
                for e in &m.events {
                    repo.set_meeting(e, &m.id)?;
                }
            }
            Ok::<_, ApiError>(())
        })
    }

    /// The background job: every connection of a source adapter, each when it is due; failures
    /// wait longer each time, up to [`crate::Settings::backoff_max_ms`].
    pub async fn run(self) {
        self.task().await;
    }

    /// The background job as a supervised [`steve_runtime::Task`] driven by [`steve_runtime::due::due_loop`].
    pub fn task(self) -> steve_runtime::Task {
        let clock = self.0.m.clock.clone();
        let source = Arc::new(CalendarDueSource(self));
        let backoff = steve_runtime::policy::Backoff {
            base: std::time::Duration::from_secs(5),
            max: std::time::Duration::from_secs(60),
            factor: 2,
            jitter: steve_runtime::policy::Jitter::Equal,
            attempts: None,
            reset_after: None,
        };
        steve_runtime::spawn(
            "calendar.sync",
            steve_runtime::due::due_loop(
                source,
                clock,
                Arc::new(tokio::sync::Notify::new()),
                backoff,
            ),
        )
    }

    /// One round of [`Calendar::run`]: syncs the connections that are due.
    pub async fn tick(&self) {
        let now = self.now();
        if let Err(e) = self.run_due_connections(now).await {
            tracing::warn!("calendar sync: {e}");
        }
    }

    fn next_due_connection(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        let rows = self
            .0
            .m
            .store
            .read(|r| Connections::new(r).live_sources())
            .map_err(|e| steve_runtime::due::DueError(format!("connections not read: {e}")))?;
        let live: Vec<_> = rows
            .into_iter()
            .filter(|r| self.0.sources.contains_key(&r.adapter))
            .collect();
        self.0
            .deadlines
            .retain_live(live.iter().map(|r| r.id.as_str()));
        let live_ids: HashSet<&str> = live.iter().map(|r| r.id.as_str()).collect();
        lock(&self.0.failures).retain(|id, _| live_ids.contains(id.as_str()));
        let earliest = live
            .iter()
            .map(|r| self.0.deadlines.not_before(&r.id).unwrap_or(0))
            .min();
        Ok(earliest)
    }

    async fn run_due_connections(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        let rows = self
            .0
            .m
            .store
            .read(|r| Connections::new(r).live_sources())
            .map_err(|e| steve_runtime::due::DueError(format!("connections not read: {e}")))?;
        let mut due: Vec<_> = rows
            .into_iter()
            .filter(|r| self.0.sources.contains_key(&r.adapter))
            .filter_map(|r| {
                let at = self.0.deadlines.not_before(&r.id).unwrap_or(0);
                (at <= now).then_some((at, r))
            })
            .collect();
        due.sort_by_key(|(at, _)| *at);
        for (_, r) in due {
            let s = self.settings();
            let res = self.sync_connection(&r.workspace_id, &r.id, false).await;
            let ok = matches!(&res, Ok(rep) if rep.errors.is_empty());
            let fails = {
                let mut map = lock(&self.0.failures);
                if ok {
                    map.remove(&r.id);
                    0
                } else {
                    let next = map.get(&r.id).copied().unwrap_or(0).saturating_add(1);
                    map.insert(r.id.clone(), next);
                    next
                }
            };
            let wait = if ok {
                s.poll_ms
            } else {
                (s.poll_ms.saturating_mul(1i64 << fails.min(16))).min(s.backoff_max_ms)
            };
            if let Err(e) = &res {
                tracing::warn!(connection = %r.id, "calendar sync failed: {e}");
            }
            self.0.deadlines.defer(r.id.clone(), self.now() + wait);
        }
        Ok(())
    }
}

struct CalendarDueSource(Calendar);

impl steve_runtime::due::DueSource for CalendarDueSource {
    fn name(&self) -> &'static str {
        "calendar.sync"
    }

    async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        self.0.next_due_connection()
    }

    async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        self.0.run_due_connections(now).await
    }
}

fn health(e: &SourceError) -> Health {
    match e {
        SourceError::Auth(m) => Health::Auth(m.clone()),
        e => Health::Failed(source_text(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_time_normalization_preserves_existing_boundaries() {
        let zone = Zone::parse("UTC").unwrap();
        let timed = |end| Ok((false, 20, end, None, None));
        let all_day = Ok((
            true,
            86_400_000,
            172_800_000,
            Some("1970-01-02"),
            Some("1970-01-03"),
        ));
        let cases = [
            ("timed", When::at(20), When::at(30), timed(30)),
            ("missing end", When::at(20), When::default(), timed(20)),
            ("earlier end", When::at(20), When::at(10), timed(20)),
            (
                "date end ignored for timed",
                When::at(20),
                When::day("1970-01-03"),
                timed(20),
            ),
            (
                "timestamp wins",
                When {
                    ms: Some(20),
                    date: Some("bad".into()),
                    tz: None,
                },
                When::at(30),
                timed(30),
            ),
            (
                "all day",
                When::day("1970-01-02"),
                When::day("1970-01-03"),
                all_day,
            ),
            (
                "missing date end",
                When::day("1970-01-02"),
                When::default(),
                all_day,
            ),
            (
                "bad date end",
                When::day("1970-01-02"),
                When::day("bad"),
                all_day,
            ),
            (
                "equal date end",
                When::day("1970-01-02"),
                When::day("1970-01-02"),
                all_day,
            ),
            (
                "earlier date end",
                When::day("1970-01-02"),
                When::day("1970-01-01"),
                all_day,
            ),
            (
                "timestamp end ignored for date",
                When::day("1970-01-02"),
                When::at(30),
                all_day,
            ),
            (
                "bad start",
                When::day("bad"),
                When::default(),
                Err("bad start date"),
            ),
            (
                "missing start",
                When::default(),
                When::at(30),
                Err("event without a start"),
            ),
        ];
        for (name, start, end, expected) in cases {
            let event = RemoteEvent {
                id: "event".into(),
                start,
                end,
                ..Default::default()
            };
            let actual = event_times(&event, &zone);
            match expected {
                Ok(expected) => {
                    let row = actual.unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(
                        (
                            row.all_day,
                            row.start_ms,
                            row.end_ms,
                            row.start_date.as_deref(),
                            row.end_date.as_deref()
                        ),
                        expected,
                        "{name}"
                    );
                }
                Err(expected) => assert_eq!(actual.err().as_deref(), Some(expected), "{name}"),
            }
        }
    }

    #[test]
    fn finds_meeting_links_in_text() {
        assert_eq!(
            meeting_link("Join: https://acme.zoom.us/j/123?pwd=x (passcode 1)").as_deref(),
            Some("https://acme.zoom.us/j/123?pwd=x")
        );
        assert_eq!(
            meeting_link("<https://meet.google.com/abc-defg-hij>").as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert!(meeting_link("http://zoom.us/j/1").is_none(), "https only");
        assert!(meeting_link("https://example.com/zoom.us").is_none());
    }
}

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
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use steve_modules::accounts::connections::{ConnCx, Health, repo as crepo};
use steve_modules::vault::VaultError;
use steve_modules::{ApiError, ApiResult};
use steve_runtime::tz::Zone;

use crate::profile::{
    Busy, Changes, RemoteCalendar, RemoteEvent, SourceError, When, Window, feature,
};
use crate::repo::{self, AttRow, CalRow, EventRow, Written};
use crate::{Calendar, hash, ics, lock, merge};

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

/// The contact holding a handle now (merged contacts followed).
pub(crate) fn current_party(c: &Connection, mut id: String) -> rusqlite::Result<String> {
    for _ in 0..32 {
        let next: Option<(String, Option<String>)> = c
            .query_row(
                "SELECT status, merged_into FROM parties WHERE id = ?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match next {
            Some((s, Some(n))) if s == "merged" => id = n,
            _ => break,
        }
    }
    Ok(id)
}

/// The contact an email belongs to, when Steve knows it (never creates one).
pub(crate) fn party_of_email(
    c: &Connection,
    ws: &str,
    email: &str,
) -> rusqlite::Result<Option<String>> {
    let p: Option<String> = c
        .query_row(
            "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id
              WHERE h.workspace_id = ? AND h.family = 'email' AND h.handle = ? AND a.status = 'active' LIMIT 1",
            params![ws, email.trim().to_ascii_lowercase()],
            |r| r.get(0),
        )
        .optional()?;
    p.map(|p| current_party(c, p)).transpose()
}

/// The user's own contact (“我”).
pub(crate) fn me_party(c: &Connection, ws: &str, user: &str) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT id FROM parties WHERE workspace_id = ? AND user_id = ? AND status = 'active' ORDER BY created LIMIT 1",
        params![ws, user],
        |r| r.get(0),
    )
    .optional()
}

/// The zone of a calendar's dates and floating times.
fn tz_of(c: &Connection, ws: &str, user: &str, name: Option<&str>) -> rusqlite::Result<Zone> {
    if let Some(zone) = name.and_then(|n| Zone::parse(n).ok()) {
        return Ok(zone);
    }
    let policy = steve_modules::zone::Zone::of(c, ws, user)?;
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
pub(crate) struct People<'a> {
    pub(crate) c: &'a Connection,
    pub(crate) ws: &'a str,
    pub(crate) me: Option<&'a str>,
    pub(crate) cache: HashMap<String, Option<String>>,
}

impl People<'_> {
    fn of(&mut self, email: Option<&str>) -> Option<String> {
        let e = email?.trim().to_ascii_lowercase();
        if let Some(p) = self.cache.get(&e) {
            return p.clone();
        }
        let p = party_of_email(self.c, self.ws, &e).ok().flatten();
        self.cache.insert(e, p.clone());
        p
    }
}

/// `e` as the cache keeps it, or why it is refused. `busy`: only its time.
pub(crate) fn normalize(
    cal: &CalRow,
    e: &RemoteEvent,
    busy: bool,
    people: &mut People<'_>,
) -> Result<(EventRow, Vec<AttRow>), String> {
    let id = e.id.trim();
    if id.is_empty() || id.len() > 1024 {
        return Err("event without a valid id".into());
    }
    // Old cache rows may have no zone; resolve them by the same policy as new calendars.
    let zone = tz_of(
        people.c,
        &cal.workspace_id,
        &cal.owner_user,
        cal.tz.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    let EventTimes {
        all_day,
        start_ms,
        end_ms,
        start_date,
        end_date,
    } = event_times(e, &zone)?;
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
            org_party = people.of(org_email.as_deref());
            org_name = cut(o.name.as_deref(), 200);
            i_org = o.me || (org_party.is_some() && org_party.as_deref() == people.me);
        }
        for a in e.attendees.iter().take(ATTENDEES_MAX) {
            let email = a
                .who
                .email
                .as_deref()
                .map(|x| x.trim().to_ascii_lowercase());
            let party = people.of(email.as_deref());
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
            .reader(|c| crepo::get(c, conn))
            .map_err(ApiError::from)?
            .filter(|r| r.workspace_id == ws);
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
            let last: Option<i64> = self.0.m.store.reader(|c| {
                c.query_row(
                    "SELECT MAX(synced_at) FROM cal_calendars WHERE connection_id = ?",
                    [conn],
                    |r| r.get(0),
                )
                .map_err(steve_store::StoreError::from)
            })?;
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
        let mut report = SyncReport {
            source: true,
            ..Default::default()
        };
        let remotes = match src.calendars(&cx).await {
            Ok(r) => r,
            Err(e) => {
                let msg = source_text(&e);
                self.record_sync_failure(ws, conn, None, msg.clone(), now)
                    .await?;
                cx.report(health(&e)).await;
                report.errors.push((String::new(), msg));
                return Ok(report);
            }
        };
        let calendars = self
            .blocking_sync(ws, conn, move |s, ws, conn| {
                s.upsert_calendars(ws, conn, &remotes)
            })
            .await?;
        report.calendars = calendars.len();
        let w = self.window();
        let mut worst: Option<SourceError> = None;
        let mut touched = Vec::new();
        for cal in calendars.iter().filter(|c| c.enabled && !c.gone) {
            let busy = cal.detail == "busy" || cal.role == "free_busy";
            let pulled = self.pull_calendar(&cx, &features, cal, w, busy, now).await;
            match pulled {
                Ok(p) => {
                    let pulled_cal = cal.clone();
                    let (changed, deleted) = self
                        .blocking_sync(ws, conn, move |s, _, _| s.apply(&pulled_cal, busy, p, w))
                        .await?;
                    report.changed += changed;
                    report.deleted += deleted;
                    touched.push(cal.id.clone());
                }
                Err(e) => {
                    let msg = source_text(&e);
                    self.record_sync_failure(
                        ws,
                        conn,
                        Some(cal.id.clone()),
                        msg.clone(),
                        self.now(),
                    )
                    .await?;
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
            self.blocking_sync(ws, conn, move |s, ws, _| s.rematch(ws, &owner))
                .await?;
        }
        cx.report(worst.as_ref().map(health).unwrap_or(Health::Ok))
            .await;
        if let Err(e) = self
            .blocking_sync(ws, conn, |s, ws, conn| {
                s.connections().touch_sync(ws, conn).map_err(api)
            })
            .await
        {
            tracing::debug!(connection = conn, "last sync not recorded: {e}");
        }
        Ok(report)
    }

    // Provider awaits have no storage authorization. Pulling is separate from
    // the blocking cache apply step so the writer never spans a network await.
    async fn pull_calendar(
        &self,
        cx: &ConnCx,
        features: &crate::Features,
        cal: &CalRow,
        w: Window,
        busy: bool,
        now: i64,
    ) -> Result<Pulled, SourceError> {
        let src = self
            .source(cx.adapter())
            .expect("is_source checked the adapter");
        if busy && features.has(feature::FREEBUSY) {
            src.freebusy(cx, &cal.ext_id, w)
                .await
                .map(|b| Pulled::Full(b.iter().map(busy_event).collect(), None))
        } else if features.has(feature::CHANGES) {
            // a cursor only brings what changed, so what newly enters the window never comes
            // by it: the whole window is pulled again once it is a day old
            let cursor = cal.sync_cursor.as_deref().filter(|_| {
                cal.full_at
                    .is_some_and(|t| now - t < self.settings().full_every_ms)
            });
            src.changes(cx, &cal.ext_id, cursor, w).await.map(|c| {
                if c.full || cursor.is_none() {
                    Pulled::Full(c.events, c.cursor)
                } else {
                    Pulled::Delta(c)
                }
            })
        } else {
            src.list(cx, &cal.ext_id, w)
                .await
                .map(|e| Pulled::Full(e, None))
        }
    }

    // Only synchronous cache work enters this helper; provider awaits stay above.
    async fn blocking_sync<T: Send + 'static>(
        &self,
        ws: &str,
        conn: &str,
        run: impl FnOnce(&Self, &str, &str) -> ApiResult<T> + Send + 'static,
    ) -> ApiResult<T> {
        let (cal, ws, conn) = (self.clone(), ws.to_owned(), conn.to_owned());
        steve_store::blocking(move || run(&cal, &ws, &conn)).await
    }

    async fn record_sync_failure(
        &self,
        ws: &str,
        conn: &str,
        calendar: Option<String>,
        msg: String,
        now: i64,
    ) -> ApiResult<()> {
        self.blocking_sync(ws, conn, move |s, ws, conn| {
            s.modules().store.transaction(|tx| {
                let ids = match calendar {
                    Some(id) => vec![id],
                    None => repo::of_connection(tx, conn)?
                        .into_iter()
                        .map(|c| c.id)
                        .collect(),
                };
                for id in ids {
                    repo::set_failed(tx, &id, &msg, now)?;
                    tx.publish(steve_types::DomainEvent::from(
                        steve_types::AutomationEvent::Calendar {
                            id: id.into(),
                            ws: ws.into(),
                        },
                    ));
                }
                Ok(())
            })
        })
        .await
    }

    /// Writes the calendars a source lists; those it no longer lists are marked gone (their events go).
    fn upsert_calendars(
        &self,
        ws: &str,
        conn: &str,
        remotes: &[RemoteCalendar],
    ) -> ApiResult<Vec<CalRow>> {
        let now = self.now();
        self.0.m.store.transaction(|tx| {
            let row = crepo::get(tx, conn)?.ok_or_else(|| ApiError::not_found("连接"))?;
            let owner = row.created_by.clone();
            let me = me_party(tx, ws, &owner)?;
            let account_party: Option<String> = tx
                .query_row(
                    "SELECT party_id FROM accounts WHERE id = ?",
                    [&row.account_id],
                    |r| r.get(0),
                )
                .optional()?;
            let account_party = account_party.map(|p| current_party(tx, p)).transpose()?;
            let have = repo::of_connection(tx, conn)?;
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
                        repo::insert_calendar(tx, &cal, now)?;
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
                            repo::strip_details(tx, &cal.id, now)?;
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
                            repo::refresh_calendar(tx, &cal, now)?;
                            if h.tz != cal.tz {
                                // Re-normalize cached dates too; a delta may never resend them.
                                tx.execute("UPDATE cal_calendars SET sync_cursor = NULL, full_at = NULL WHERE id = ?", [&cal.id])?;
                            }
                        }
                        differs
                    }
                };
                // pages hear of a calendar only when something about it changed
                if differs {
                    tx.publish(steve_types::DomainEvent::from(
                        steve_types::AutomationEvent::Calendar {
                            id: cal.id.clone().into(),
                            ws: ws.into(),
                        },
                    ));
                }
            }
            for h in have.iter().filter(|h| !h.gone && !seen.contains(&h.ext_id)) {
                repo::set_gone(tx, &h.id, now)?;
                tx.publish(steve_types::DomainEvent::from(
                    steve_types::AutomationEvent::Calendar {
                        id: h.id.clone().into(),
                        ws: ws.into(),
                    },
                ));
            }
            Ok(repo::of_connection(tx, conn)?)
        })
    }

    /// Writes what a calendar's pull brought: (changed, deleted).
    fn apply(&self, cal: &CalRow, busy: bool, p: Pulled, w: Window) -> ApiResult<(usize, usize)> {
        let now = self.now();
        let ws = cal.workspace_id.clone();
        self.0.m.store.transaction(|tx| {
            let me = me_party(tx, &ws, &cal.owner_user)?;
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
                    Err(why) => {
                        tracing::debug!(calendar = %cal.id, "event skipped: {why}");
                        continue;
                    }
                };
                kept.insert(row.id.clone());
                if matches!(
                    repo::write_event(tx, &row, &atts, now)?,
                    Written::New | Written::Changed
                ) {
                    changed += 1;
                }
            }
            if full {
                for old in repo::events_of(tx, &cal.id)? {
                    if !kept.contains(&old.id)
                        && old.start_ms < w.to_ms
                        && old.end_ms > w.from_ms
                        && repo::delete_event(tx, &old.id, now)?
                    {
                        deleted += 1;
                    }
                }
            } else {
                for ext in &gone {
                    let ids: Vec<String> = {
                        let mut st = tx.prepare_cached(
                            "SELECT id FROM cal_events WHERE calendar_id = ? AND ext_id = ?",
                        )?;
                        st.query_map(params![cal.id, ext], |r| r.get(0))?
                            .collect::<rusqlite::Result<_>>()?
                    };
                    for id in ids {
                        if repo::delete_event(tx, &id, now)? {
                            deleted += 1;
                        }
                    }
                }
            }
            repo::prune(tx, &cal.id, w.from_ms)?;
            repo::set_synced(tx, &cal.id, cursor.as_deref(), full, now)?;
            // a sync that found nothing new is not news, unless the calendar was failing or never synced
            if changed + deleted > 0 || cal.sync_state != "ok" {
                tx.publish(steve_types::DomainEvent::from(
                    steve_types::AutomationEvent::Calendar {
                        id: cal.id.clone().into(),
                        ws: ws.clone().into(),
                    },
                ));
            }
            Ok((changed, deleted))
        })
    }

    /// Recomputes which occurrences of `owner`'s calendars are one meeting ([`merge`]).
    pub(crate) fn rematch(&self, ws: &str, owner: &str) -> ApiResult<()> {
        let now = self.now();
        self.0.m.store.transaction(|tx| {
            let cals = repo::of_user(tx, ws, owner)?;
            let party: HashMap<String, String> = cals
                .iter()
                .map(|c| (c.id.clone(), c.party_id.clone().unwrap_or_else(|| format!("cal:{}", c.id))))
                .collect();
            let mut items = Vec::new();
            for c in cals.iter().filter(|c| !c.gone) {
                for e in repo::events_of(tx, &c.id)? {
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
            let not_same: HashSet<(String, String)> = {
                let mut st = tx.prepare_cached("SELECT a, b FROM cal_not_same WHERE workspace_id = ? AND owner_user = ?")?;
                st.query_map(params![ws, owner], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let ms = merge::meetings(&items, &not_same);
            let ids: Vec<&str> = ms.iter().map(|m| m.id.as_str()).collect();
            tx.execute(
                "DELETE FROM cal_meetings WHERE workspace_id = ? AND owner_user = ? AND id NOT IN (SELECT value FROM json_each(?))",
                params![ws, owner, serde_json::to_string(&ids).unwrap_or_default()],
            )?;
            for m in &ms {
                tx.prepare_cached(
                    "INSERT INTO cal_meetings (id, workspace_id, owner_user, primary_event_id, match_key, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(id) DO UPDATE SET primary_event_id = excluded.primary_event_id, match_key = excluded.match_key, updated = excluded.updated
                     WHERE primary_event_id IS NOT excluded.primary_event_id OR match_key IS NOT excluded.match_key",
                )?
                .execute(params![m.id, ws, owner, m.primary, m.key, now, now])?;
                for e in &m.events {
                    repo::set_meeting(tx, e, Some(&m.id))?;
                }
            }
            Ok::<_, ApiError>(())
        })
    }

    /// The background job: every connection of a source adapter, each when it is due; failures
    /// wait longer each time, up to [`crate::Settings::backoff_max_ms`].
    pub async fn run(self) {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            self.tick().await;
        }
    }

    /// One round of [`Calendar::run`]: syncs the connections that are due.
    pub async fn tick(&self) {
        let rows = match self.0.m.store.reader(crepo::live) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("calendar sync: connections not read: {e}");
                return;
            }
        };
        let now = self.now();
        for r in rows
            .into_iter()
            .filter(|r| self.0.sources.contains_key(&r.adapter))
        {
            let (next, fails) = lock(&self.0.due).get(&r.id).copied().unwrap_or((0, 0));
            if next > now {
                continue;
            }
            let s = self.settings();
            let res = self.sync_connection(&r.workspace_id, &r.id, false).await;
            let ok = matches!(&res, Ok(rep) if rep.errors.is_empty());
            let fails = if ok { 0 } else { fails.saturating_add(1) };
            let wait = if ok {
                s.poll_ms
            } else {
                (s.poll_ms.saturating_mul(1i64 << fails.min(16))).min(s.backoff_max_ms)
            };
            if let Err(e) = &res {
                tracing::warn!(connection = %r.id, "calendar sync failed: {e}");
            }
            lock(&self.0.due).insert(r.id.clone(), (self.now() + wait, fails));
        }
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

    fn test_calendar() -> CalRow {
        CalRow {
            id: "cal".into(),
            workspace_id: "w".into(),
            connection_id: "conn".into(),
            owner_user: "me".into(),
            ext_id: "cal".into(),
            name: "Calendar".into(),
            color: None,
            kind: "primary".into(),
            role: "owner".into(),
            share: "private".into(),
            party_id: None,
            party_set: false,
            detail: "full".into(),
            enabled: true,
            tz: None,
            sync_cursor: None,
            synced_at: None,
            full_at: None,
            sync_state: "ok".into(),
            sync_error: None,
            gone: false,
        }
    }

    #[test]
    fn event_time_normalization_preserves_existing_boundaries() {
        let c = Connection::open_in_memory().unwrap();
        let mut cal = test_calendar();
        cal.tz = Some("UTC".into());
        let mut people = People {
            c: &c,
            ws: "w",
            me: None,
            cache: HashMap::new(),
        };
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
            let actual = normalize(&cal, &event, true, &mut people);
            match expected {
                Ok(expected) => {
                    let (row, _) = actual.unwrap_or_else(|e| panic!("{name}: {e}"));
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
    fn missing_cached_calendar_zone_uses_owner_policy_then_workspace_then_system() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notification_policies(workspace_id, user_id, timezone);
            CREATE TABLE settings(workspace_id, key, value);
            INSERT INTO settings VALUES ('w','tz','\"Europe/London\"');",
        )
        .unwrap();
        let mut cal = test_calendar();
        for (name, date) in [
            ("America/New_York", "2026-03-08"),
            ("Europe/London", "2026-10-25"),
            ("America/Sao_Paulo", "2018-11-04"),
        ] {
            c.execute("DELETE FROM notification_policies", []).unwrap();
            c.execute(
                "INSERT INTO notification_policies VALUES ('w','me',?)",
                [name],
            )
            .unwrap();
            let event = RemoteEvent {
                id: "event".into(),
                start: When::day(date),
                end: When::day(&day(date).unwrap().succ_opt().unwrap().to_string()),
                ..Default::default()
            };
            let mut people = People {
                c: &c,
                ws: "w",
                me: None,
                cache: HashMap::new(),
            };
            let (row, _) = normalize(&cal, &event, false, &mut people).unwrap();
            let zone = Zone::parse(name).unwrap();
            assert_eq!(
                row.start_ms,
                zone.instant(day(date).unwrap().and_time(NaiveTime::MIN))
                    .unwrap()
            );
            assert_eq!(row.end_ms, zone.day(row.start_ms).unwrap().1);
        }
        cal.tz = Some("Asia/Tokyo".into());
        assert_eq!(
            tz_of(&c, "w", "me", cal.tz.as_deref()).unwrap().name(),
            "Asia/Tokyo"
        );
        c.execute("DELETE FROM notification_policies", []).unwrap();
        assert_eq!(tz_of(&c, "w", "me", None).unwrap().name(), "Europe/London");
        c.execute("DELETE FROM settings", []).unwrap();
        assert_eq!(
            tz_of(&c, "w", "me", None).unwrap().name(),
            Zone::system().name()
        );
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

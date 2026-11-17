//! The calendar cache's rows (migration 100).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

pub type Result<T> = std::result::Result<T, rusqlite::Error>;

/// `cal_calendars`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalRow {
    pub id: String,
    #[serde(skip)]
    pub workspace_id: String,
    pub connection_id: String,
    #[serde(skip)]
    pub owner_user: String,
    #[serde(skip)]
    pub ext_id: String,
    pub name: String,
    pub color: Option<String>,
    pub kind: String,
    pub role: String,
    pub share: String,
    pub party_id: Option<String>,
    pub party_set: bool,
    pub detail: String,
    pub enabled: bool,
    pub tz: Option<String>,
    #[serde(skip)]
    pub sync_cursor: Option<String>,
    pub synced_at: Option<i64>,
    /// The last whole-window pull that succeeded.
    #[serde(skip)]
    pub full_at: Option<i64>,
    pub sync_state: String,
    pub sync_error: Option<String>,
    pub gone: bool,
}

const CAL_COLS: &str = "id, workspace_id, connection_id, owner_user, ext_id, name, color, kind, role, share, party_id, party_set, detail, enabled, tz, sync_cursor, synced_at, sync_state, sync_error, gone, full_at";

impl CalRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            connection_id: r.get(2)?,
            owner_user: r.get(3)?,
            ext_id: r.get(4)?,
            name: r.get(5)?,
            color: r.get(6)?,
            kind: r.get(7)?,
            role: r.get(8)?,
            share: r.get(9)?,
            party_id: r.get(10)?,
            party_set: r.get::<_, i64>(11)? != 0,
            detail: r.get(12)?,
            enabled: r.get::<_, i64>(13)? != 0,
            tz: r.get(14)?,
            sync_cursor: r.get(15)?,
            synced_at: r.get(16)?,
            sync_state: r.get(17)?,
            sync_error: r.get(18)?,
            gone: r.get::<_, i64>(19)? != 0,
            full_at: r.get(20)?,
        })
    }
}

pub fn calendar(c: &Connection, id: &str) -> Result<Option<CalRow>> {
    c.prepare_cached(&format!(
        "SELECT {CAL_COLS} FROM cal_calendars WHERE id = ?"
    ))?
    .query_row([id], CalRow::from_row)
    .optional()
}

pub fn of_connection(c: &Connection, conn: &str) -> Result<Vec<CalRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {CAL_COLS} FROM cal_calendars WHERE connection_id = ? ORDER BY gone, name, id"
    ))?;
    st.query_map([conn], CalRow::from_row)?.collect()
}

/// The user's calendars, every connection.
pub fn of_user(c: &Connection, ws: &str, user: &str) -> Result<Vec<CalRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {CAL_COLS} FROM cal_calendars WHERE workspace_id = ? AND owner_user = ? ORDER BY connection_id, gone, name, id"
    ))?;
    st.query_map(params![ws, user], CalRow::from_row)?.collect()
}

pub fn insert_calendar(c: &Connection, r: &CalRow, now: i64) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO cal_calendars (id, workspace_id, connection_id, owner_user, ext_id, name, color, kind, role, share, party_id, party_set, detail, enabled, tz, sync_state, gone, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'new', 0, ?, ?)",
    )?
    .execute(params![
        r.id, r.workspace_id, r.connection_id, r.owner_user, r.ext_id, r.name, r.color, r.kind, r.role, r.share,
        r.party_id, r.party_set as i64, r.detail, r.enabled as i64, r.tz, now, now
    ])?;
    Ok(())
}

/// What the source says about a calendar changed (name, color, kind, role, share, zone); listed again.
pub fn refresh_calendar(c: &Connection, r: &CalRow, now: i64) -> Result<()> {
    c.prepare_cached(
        "UPDATE cal_calendars SET name = ?, color = ?, kind = ?, role = ?, share = ?, tz = ?, party_id = ?, detail = ?, gone = 0, updated = ? WHERE id = ?",
    )?
    .execute(params![r.name, r.color, r.kind, r.role, r.share, r.tz, r.party_id, r.detail, now, r.id])?;
    Ok(())
}

pub fn set_gone(c: &Connection, id: &str, now: i64) -> Result<()> {
    c.prepare_cached(
        "UPDATE cal_calendars SET gone = 1, sync_cursor = NULL, updated = ? WHERE id = ?",
    )?
    .execute(params![now, id])?;
    c.prepare_cached("DELETE FROM cal_events WHERE calendar_id = ?")?
        .execute([id])?;
    Ok(())
}

/// A pull succeeded; `full`: it was the whole window.
pub fn set_synced(
    c: &Connection,
    id: &str,
    cursor: Option<&str>,
    full: bool,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "UPDATE cal_calendars SET sync_state = 'ok', sync_error = NULL, sync_cursor = ?, synced_at = ?, full_at = CASE WHEN ? THEN ? ELSE full_at END, updated = ? WHERE id = ?",
    )?
    .execute(params![cursor, now, full, now, now, id])?;
    Ok(())
}

pub fn set_failed(c: &Connection, id: &str, error: &str, now: i64) -> Result<()> {
    c.prepare_cached(
        "UPDATE cal_calendars SET sync_state = 'error', sync_error = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![error, now, id])?;
    Ok(())
}

/// The owner's settings of a calendar.
pub fn set_settings(
    c: &Connection,
    id: &str,
    enabled: bool,
    party: Option<&str>,
    party_set: bool,
    detail: &str,
    color: Option<&str>,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "UPDATE cal_calendars SET enabled = ?, party_id = ?, party_set = ?, detail = ?, color = ?, sync_cursor = CASE WHEN detail = ? THEN sync_cursor ELSE NULL END, updated = ? WHERE id = ?",
    )?
    .execute(params![enabled as i64, party, party_set as i64, detail, color, detail, now, id])?;
    Ok(())
}

/// Keeps only the times of a calendar's events (it became someone else's, or busy only).
pub fn strip_details(c: &Connection, calendar: &str, now: i64) -> Result<()> {
    c.prepare_cached(
        "DELETE FROM cal_attendees WHERE event_id IN (SELECT id FROM cal_events WHERE calendar_id = ?)",
    )?
    .execute([calendar])?;
    c.prepare_cached(
        "UPDATE cal_events SET title = NULL, location = NULL, meeting_url = NULL, excerpt = NULL, organizer_party = NULL, organizer_email = NULL, organizer_name = NULL, busy_only = 1, updated = ? WHERE calendar_id = ?",
    )?
    .execute(params![now, calendar])?;
    Ok(())
}

/// `cal_events` (with its attendees apart).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct EventRow {
    pub id: String,
    pub calendar_id: String,
    #[serde(skip)]
    pub workspace_id: String,
    #[serde(skip)]
    pub owner_user: String,
    pub meeting_id: Option<String>,
    #[serde(skip)]
    pub ext_id: String,
    #[serde(skip)]
    pub series_ext_id: Option<String>,
    #[serde(skip)]
    pub original_start: Option<String>,
    #[serde(skip)]
    pub ical_uid: Option<String>,
    #[serde(skip)]
    pub sequence: Option<i64>,
    #[serde(skip)]
    pub etag: Option<String>,
    pub version: i64,
    #[serde(skip)]
    pub fingerprint: String,
    pub title: Option<String>,
    pub location: Option<String>,
    pub meeting_url: Option<String>,
    pub excerpt: Option<String>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub tz: Option<String>,
    pub all_day: bool,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub status: String,
    pub my_rsvp: Option<String>,
    pub organizer_party: Option<String>,
    pub organizer_email: Option<String>,
    pub organizer_name: Option<String>,
    pub i_am_organizer: bool,
    pub can_modify: bool,
    pub busy_only: bool,
    #[serde(skip)]
    pub deleted_ms: Option<i64>,
}

const EV_COLS: &str = "id, calendar_id, workspace_id, owner_user, meeting_id, ext_id, series_ext_id, original_start, ical_uid, sequence, etag, version, fingerprint, title, location, meeting_url, excerpt, start_ms, end_ms, tz, all_day, start_date, end_date, status, my_rsvp, organizer_party, organizer_email, organizer_name, i_am_organizer, can_modify, busy_only, deleted_ms";

impl EventRow {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            calendar_id: r.get(1)?,
            workspace_id: r.get(2)?,
            owner_user: r.get(3)?,
            meeting_id: r.get(4)?,
            ext_id: r.get(5)?,
            series_ext_id: r.get(6)?,
            original_start: r.get(7)?,
            ical_uid: r.get(8)?,
            sequence: r.get(9)?,
            etag: r.get(10)?,
            version: r.get(11)?,
            fingerprint: r.get(12)?,
            title: r.get(13)?,
            location: r.get(14)?,
            meeting_url: r.get(15)?,
            excerpt: r.get(16)?,
            start_ms: r.get(17)?,
            end_ms: r.get(18)?,
            tz: r.get(19)?,
            all_day: r.get::<_, i64>(20)? != 0,
            start_date: r.get(21)?,
            end_date: r.get(22)?,
            status: r.get(23)?,
            my_rsvp: r.get(24)?,
            organizer_party: r.get(25)?,
            organizer_email: r.get(26)?,
            organizer_name: r.get(27)?,
            i_am_organizer: r.get::<_, i64>(28)? != 0,
            can_modify: r.get::<_, i64>(29)? != 0,
            busy_only: r.get::<_, i64>(30)? != 0,
            deleted_ms: r.get(31)?,
        })
    }
}

/// `cal_attendees`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct AttRow {
    pub party_id: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    #[serde(skip)]
    pub ext_ref: Option<String>,
    pub rsvp: Option<String>,
    pub organizer: bool,
    pub optional: bool,
    pub is_self: bool,
}

pub fn event(c: &Connection, id: &str) -> Result<Option<EventRow>> {
    c.prepare_cached(&format!("SELECT {EV_COLS} FROM cal_events WHERE id = ?"))?
        .query_row([id], EventRow::from_row)
        .optional()
}

/// A calendar's live (not deleted) events, every one.
pub fn events_of(c: &Connection, calendar: &str) -> Result<Vec<EventRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {EV_COLS} FROM cal_events WHERE calendar_id = ? AND deleted_ms IS NULL"
    ))?;
    st.query_map([calendar], EventRow::from_row)?.collect()
}

/// Events of `calendars` overlapping `[from, to)`: timed ones by instant, all-day ones by local date
/// (`from_date` ≤ day < `to_date`, both in the reader's zone).
pub fn in_range(
    c: &Connection,
    calendars: &[String],
    from: i64,
    to: i64,
    from_date: &str,
    to_date: &str,
) -> Result<Vec<EventRow>> {
    if calendars.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(calendars).unwrap_or_else(|_| "[]".into());
    let mut st = c.prepare_cached(&format!(
        "SELECT {EV_COLS} FROM cal_events WHERE calendar_id IN (SELECT value FROM json_each(?1)) AND deleted_ms IS NULL
           AND ((all_day = 0 AND start_ms < ?3 AND end_ms > ?2) OR (all_day = 1 AND start_date < ?5 AND end_date > ?4))
         ORDER BY start_ms, id"
    ))?;
    st.query_map(
        params![ids, from, to, from_date, to_date],
        EventRow::from_row,
    )?
    .collect()
}

/// Text search in the titles, places and excerpts of `calendars`, inside `[from, to)`.
pub fn search(
    c: &Connection,
    calendars: &[String],
    q: &str,
    from: i64,
    to: i64,
    limit: usize,
) -> Result<Vec<EventRow>> {
    if calendars.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(calendars).unwrap_or_else(|_| "[]".into());
    let like = format!(
        "%{}%",
        q.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut st = c.prepare_cached(&format!(
        "SELECT {EV_COLS} FROM cal_events WHERE calendar_id IN (SELECT value FROM json_each(?1)) AND deleted_ms IS NULL
           AND start_ms < ?3 AND end_ms > ?2
           AND (title LIKE ?4 ESCAPE '\\' OR location LIKE ?4 ESCAPE '\\' OR excerpt LIKE ?4 ESCAPE '\\' OR organizer_name LIKE ?4 ESCAPE '\\')
         ORDER BY start_ms, id LIMIT ?5"
    ))?;
    st.query_map(
        params![ids, from, to, like, limit as i64],
        EventRow::from_row,
    )?
    .collect()
}

pub fn attendees(c: &Connection, event: &str) -> Result<Vec<AttRow>> {
    let mut st = c.prepare_cached(
        "SELECT party_id, email, name, ext_ref, rsvp, organizer, optional, is_self FROM cal_attendees WHERE event_id = ? ORDER BY ord",
    )?;
    st.query_map([event], |r| {
        Ok(AttRow {
            party_id: r.get(0)?,
            email: r.get(1)?,
            name: r.get(2)?,
            ext_ref: r.get(3)?,
            rsvp: r.get(4)?,
            organizer: r.get::<_, i64>(5)? != 0,
            optional: r.get::<_, i64>(6)? != 0,
            is_self: r.get::<_, i64>(7)? != 0,
        })
    })?
    .collect()
}

/// What writing an event did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    New,
    /// Something that matters to the people in it changed: `version` went up.
    Changed,
    /// Only other fields (a description, a title, an etag).
    Touched,
    Same,
}

/// Inserts or updates `e` (its id decided by the caller) and replaces its attendees. A row deleted
/// before comes back. `version` goes up only when the fingerprint differs.
pub fn write_event(c: &Connection, e: &EventRow, atts: &[AttRow], now: i64) -> Result<Written> {
    let old = event(c, &e.id)?;
    let written = match &old {
        None => Written::New,
        Some(o) if o.deleted_ms.is_some() || o.fingerprint != e.fingerprint => Written::Changed,
        Some(o) => {
            let mut a = o.clone();
            a.meeting_id = e.meeting_id.clone();
            a.version = e.version;
            if &a == e {
                Written::Same
            } else {
                Written::Touched
            }
        }
    };
    let version = match (&old, written) {
        (Some(o), Written::Changed) => o.version + 1,
        (Some(o), _) => o.version,
        (None, _) => 1,
    };
    if written == Written::Same {
        return Ok(written);
    }
    c.prepare_cached(&format!(
        "INSERT INTO cal_events ({EV_COLS}, created, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, NULL, ?32, ?32)
         ON CONFLICT(id) DO UPDATE SET calendar_id = excluded.calendar_id, ext_id = excluded.ext_id, series_ext_id = excluded.series_ext_id,
           original_start = excluded.original_start, ical_uid = excluded.ical_uid, sequence = excluded.sequence, etag = excluded.etag,
           version = excluded.version, fingerprint = excluded.fingerprint, title = excluded.title, location = excluded.location,
           meeting_url = excluded.meeting_url, excerpt = excluded.excerpt, start_ms = excluded.start_ms, end_ms = excluded.end_ms,
           tz = excluded.tz, all_day = excluded.all_day, start_date = excluded.start_date, end_date = excluded.end_date,
           status = excluded.status, my_rsvp = excluded.my_rsvp, organizer_party = excluded.organizer_party,
           organizer_email = excluded.organizer_email, organizer_name = excluded.organizer_name, i_am_organizer = excluded.i_am_organizer,
           can_modify = excluded.can_modify, busy_only = excluded.busy_only, deleted_ms = NULL, updated = excluded.updated"
    ))?
    .execute(params![
        e.id, e.calendar_id, e.workspace_id, e.owner_user, e.meeting_id, e.ext_id, e.series_ext_id, e.original_start,
        e.ical_uid, e.sequence, e.etag, version, e.fingerprint, e.title, e.location, e.meeting_url, e.excerpt,
        e.start_ms, e.end_ms, e.tz, e.all_day as i64, e.start_date, e.end_date, e.status, e.my_rsvp,
        e.organizer_party, e.organizer_email, e.organizer_name, e.i_am_organizer as i64, e.can_modify as i64,
        e.busy_only as i64, now
    ])?;
    c.prepare_cached("DELETE FROM cal_attendees WHERE event_id = ?")?
        .execute([&e.id])?;
    let mut ins = c.prepare_cached(
        "INSERT INTO cal_attendees (event_id, ord, party_id, email, name, ext_ref, rsvp, organizer, optional, is_self) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for (i, a) in atts.iter().enumerate() {
        ins.execute(params![
            e.id,
            i as i64,
            a.party_id,
            a.email,
            a.name,
            a.ext_ref,
            a.rsvp,
            a.organizer as i64,
            a.optional as i64,
            a.is_self as i64
        ])?;
    }
    Ok(written)
}

/// Marks an event deleted (the row stays, so links to it can say it is gone). `true`: it was live.
pub fn delete_event(c: &Connection, id: &str, now: i64) -> Result<bool> {
    Ok(c
        .prepare_cached(
            "UPDATE cal_events SET deleted_ms = ?, version = version + 1, updated = ? WHERE id = ? AND deleted_ms IS NULL",
        )?
        .execute(params![now, now, id])?
        > 0)
}

/// Drops what lies wholly before `before` (out of the cache window), and deleted rows older than it.
pub fn prune(c: &Connection, calendar: &str, before: i64) -> Result<usize> {
    c.prepare_cached(
        "DELETE FROM cal_events WHERE calendar_id = ? AND (end_ms < ?2 OR (deleted_ms IS NOT NULL AND deleted_ms < ?2))",
    )?
    .execute(params![calendar, before])
}

pub fn set_meeting(c: &Connection, event: &str, meeting: Option<&str>) -> Result<()> {
    c.prepare_cached("UPDATE cal_events SET meeting_id = ? WHERE id = ? AND meeting_id IS NOT ?")?
        .execute(params![meeting, event, meeting])?;
    Ok(())
}

/// The ICS file uploaded to a connection.
pub fn ics_file(c: &Connection, conn: &str) -> Result<Option<(String, String)>> {
    c.prepare_cached("SELECT name, body FROM cal_ics_files WHERE connection_id = ?")?
        .query_row([conn], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
}

pub fn put_ics_file(
    c: &Connection,
    ws: &str,
    conn: &str,
    name: &str,
    body: &str,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO cal_ics_files (connection_id, workspace_id, name, body, uploaded) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(connection_id) DO UPDATE SET name = excluded.name, body = excluded.body, uploaded = excluded.uploaded",
    )?
    .execute(params![conn, ws, name, body, now])?;
    Ok(())
}

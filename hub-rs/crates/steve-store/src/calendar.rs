//! Calendar cache repositories. Provider data and ownership policy are handled by callers.

use sea_query::{Expr, ExprTrait, Func, LikeExpr, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::Row;
use crate::{Read, Result, W};

pub struct CalendarRepo<'a, C>(&'a mut C);

impl<'a, C: Read> CalendarRepo<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }
}

/// A cached source calendar.
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

const CAL_COLS: [&str; 21] = [
    "id",
    "workspace_id",
    "connection_id",
    "owner_user",
    "ext_id",
    "name",
    "color",
    "kind",
    "role",
    "share",
    "party_id",
    "party_set",
    "detail",
    "enabled",
    "tz",
    "sync_cursor",
    "synced_at",
    "sync_state",
    "sync_error",
    "gone",
    "full_at",
];

impl CalRow {
    fn from_row(r: &Row<'_>) -> Result<Self> {
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
            party_set: r.get(11)?,
            detail: r.get(12)?,
            enabled: r.get(13)?,
            tz: r.get(14)?,
            sync_cursor: r.get(15)?,
            synced_at: r.get(16)?,
            sync_state: r.get(17)?,
            sync_error: r.get(18)?,
            gone: r.get(19)?,
            full_at: r.get(20)?,
        })
    }
}

/// One cached occurrence of an event.
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

const EV_COLS: [&str; 32] = [
    "id",
    "calendar_id",
    "workspace_id",
    "owner_user",
    "meeting_id",
    "ext_id",
    "series_ext_id",
    "original_start",
    "ical_uid",
    "sequence",
    "etag",
    "version",
    "fingerprint",
    "title",
    "location",
    "meeting_url",
    "excerpt",
    "start_ms",
    "end_ms",
    "tz",
    "all_day",
    "start_date",
    "end_date",
    "status",
    "my_rsvp",
    "organizer_party",
    "organizer_email",
    "organizer_name",
    "i_am_organizer",
    "can_modify",
    "busy_only",
    "deleted_ms",
];

impl EventRow {
    fn from_row(r: &Row<'_>) -> Result<Self> {
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
            all_day: r.get(20)?,
            start_date: r.get(21)?,
            end_date: r.get(22)?,
            status: r.get(23)?,
            my_rsvp: r.get(24)?,
            organizer_party: r.get(25)?,
            organizer_email: r.get(26)?,
            organizer_name: r.get(27)?,
            i_am_organizer: r.get(28)?,
            can_modify: r.get(29)?,
            busy_only: r.get(30)?,
            deleted_ms: r.get(31)?,
        })
    }
}

/// A cached event attendee.
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

const CALENDARS: &str = "cal_calendars";
const EVENTS: &str = "cal_events";
const ATTENDEES: &str = "cal_attendees";
const MEETINGS: &str = "cal_meetings";
const ICS_FILES: &str = "cal_ics_files";

fn select(table: &'static str, columns: impl IntoIterator<Item = &'static str>) -> SelectStatement {
    Query::select().columns(columns).from(table).take()
}

fn owned(ws: &str, user: &str) -> Expr {
    Expr::col("workspace_id")
        .eq(ws)
        .and(Expr::col("owner_user").eq(user))
}

fn live_events() -> SelectStatement {
    select(EVENTS, EV_COLS)
        .and_where(Expr::col("deleted_ms").is_null())
        .take()
}

/// One meeting's stored identity and its primary occurrence.
pub struct Meeting<'a> {
    pub id: &'a str,
    pub primary: &'a str,
    pub key: &'a str,
}

impl<C: Read> CalendarRepo<'_, C> {
    pub fn calendar(&mut self, id: &str) -> Result<Option<CalRow>> {
        self.0.one(
            select(CALENDARS, CAL_COLS).and_where(Expr::col("id").eq(id)),
            CalRow::from_row,
        )
    }

    pub fn of_connection(&mut self, conn: &str) -> Result<Vec<CalRow>> {
        self.0.all(
            select(CALENDARS, CAL_COLS)
                .and_where(Expr::col("connection_id").eq(conn))
                .order_by_columns([
                    ("gone", Order::Asc),
                    ("name", Order::Asc),
                    ("id", Order::Asc),
                ]),
            CalRow::from_row,
        )
    }

    pub fn of_user(&mut self, ws: &str, user: &str) -> Result<Vec<CalRow>> {
        self.0.all(
            select(CALENDARS, CAL_COLS)
                .and_where(owned(ws, user))
                .order_by_columns([
                    ("connection_id", Order::Asc),
                    ("gone", Order::Asc),
                    ("name", Order::Asc),
                    ("id", Order::Asc),
                ]),
            CalRow::from_row,
        )
    }

    pub fn last_synced(&mut self, conn: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::max(Expr::col("synced_at")))
                    .from(CALENDARS)
                    .and_where(Expr::col("connection_id").eq(conn)),
                |r| r.get(0),
            )?
            .flatten())
    }

    pub fn event(&mut self, id: &str) -> Result<Option<EventRow>> {
        self.0.one(
            select(EVENTS, EV_COLS).and_where(Expr::col("id").eq(id)),
            EventRow::from_row,
        )
    }

    pub fn events_of(&mut self, calendar: &str) -> Result<Vec<EventRow>> {
        self.0.all(
            live_events().and_where(Expr::col("calendar_id").eq(calendar)),
            EventRow::from_row,
        )
    }

    /// Events overlapping the half-open window, with all-day events compared in the reader's dates.
    pub fn in_range(
        &mut self,
        calendars: &[String],
        from: i64,
        to: i64,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<EventRow>> {
        if calendars.is_empty() {
            return Ok(Vec::new());
        }
        let ids = self.0.db().dialect().text_array(calendars)?;
        let timed = Expr::col("start_ms")
            .lt(to)
            .and(Expr::col("end_ms").gt(from));
        let dates = Expr::col("start_date")
            .lt(to_date)
            .and(Expr::col("end_date").gt(from_date));
        self.0.all(
            live_events()
                .and_where(Expr::col("calendar_id").in_subquery(ids))
                .and_where(
                    Expr::col("all_day")
                        .eq(0_i64)
                        .and(timed)
                        .or(Expr::col("all_day").eq(1_i64).and(dates)),
                )
                .order_by_columns([("start_ms", Order::Asc), ("id", Order::Asc)]),
            EventRow::from_row,
        )
    }

    pub fn search(
        &mut self,
        calendars: &[String],
        q: &str,
        from: i64,
        to: i64,
        limit: usize,
    ) -> Result<Vec<EventRow>> {
        if calendars.is_empty() {
            return Ok(Vec::new());
        }
        let ids = self.0.db().dialect().text_array(calendars)?;
        let like = LikeExpr::new(format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        ))
        .escape('\\');
        let text = Expr::col("title")
            .like(like.clone())
            .or(Expr::col("location").like(like.clone()))
            .or(Expr::col("excerpt").like(like.clone()))
            .or(Expr::col("organizer_name").like(like));
        self.0.all(
            live_events()
                .and_where(Expr::col("calendar_id").in_subquery(ids))
                .and_where(Expr::col("start_ms").lt(to))
                .and_where(Expr::col("end_ms").gt(from))
                .and_where(text)
                .order_by_columns([("start_ms", Order::Asc), ("id", Order::Asc)])
                .limit(limit as u64),
            EventRow::from_row,
        )
    }

    pub fn attendees(&mut self, event: &str) -> Result<Vec<AttRow>> {
        self.0.all(
            select(
                ATTENDEES,
                [
                    "party_id",
                    "email",
                    "name",
                    "ext_ref",
                    "rsvp",
                    "organizer",
                    "optional",
                    "is_self",
                ],
            )
            .and_where(Expr::col("event_id").eq(event))
            .order_by("ord", Order::Asc),
            |r| {
                Ok(AttRow {
                    party_id: r.get(0)?,
                    email: r.get(1)?,
                    name: r.get(2)?,
                    ext_ref: r.get(3)?,
                    rsvp: r.get(4)?,
                    organizer: r.get(5)?,
                    optional: r.get(6)?,
                    is_self: r.get(7)?,
                })
            },
        )
    }

    pub fn attendee_count(&mut self, event: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("event_id")))
                    .from(ATTENDEES)
                    .and_where(Expr::col("event_id").eq(event)),
                |r| r.get(0),
            )?
            .unwrap_or_default())
    }

    pub fn meeting_primaries(&mut self, ws: &str, user: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            select(MEETINGS, ["id", "primary_event_id"]).and_where(owned(ws, user)),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn meeting_primary(&mut self, ws: &str, user: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            select(MEETINGS, ["primary_event_id"])
                .and_where(owned(ws, user))
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn other_occurrence_calendars(
        &mut self,
        meeting: &str,
        event: &str,
    ) -> Result<Vec<String>> {
        self.0.all(
            select(EVENTS, ["calendar_id"])
                .and_where(Expr::col("deleted_ms").is_null())
                .and_where(Expr::col("meeting_id").eq(meeting))
                .and_where(Expr::col("id").ne(event)),
            |r| r.get(0),
        )
    }

    pub fn different_occurrences(&mut self, ws: &str, user: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            select("cal_not_same", ["a", "b"]).and_where(owned(ws, user)),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn ics_file(&mut self, conn: &str) -> Result<Option<(String, String)>> {
        self.0.one(
            select(ICS_FILES, ["name", "body"]).and_where(Expr::col("connection_id").eq(conn)),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

pub struct CalendarSettings<'a> {
    pub enabled: bool,
    pub party: Option<&'a str>,
    pub party_set: bool,
    pub detail: &'a str,
    pub color: Option<&'a str>,
}

impl CalendarRepo<'_, W<'_>> {
    pub fn insert_calendar(&mut self, r: &CalRow, now: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table(CALENDARS)
                .columns([
                    "id",
                    "workspace_id",
                    "connection_id",
                    "owner_user",
                    "ext_id",
                    "name",
                    "color",
                    "kind",
                    "role",
                    "share",
                    "party_id",
                    "party_set",
                    "detail",
                    "enabled",
                    "tz",
                    "created",
                    "updated",
                ])
                .values_panic([
                    r.id.as_str().into(),
                    r.workspace_id.as_str().into(),
                    r.connection_id.as_str().into(),
                    r.owner_user.as_str().into(),
                    r.ext_id.as_str().into(),
                    r.name.as_str().into(),
                    r.color.as_deref().into(),
                    r.kind.as_str().into(),
                    r.role.as_str().into(),
                    r.share.as_str().into(),
                    r.party_id.as_deref().into(),
                    i64::from(r.party_set).into(),
                    r.detail.as_str().into(),
                    i64::from(r.enabled).into(),
                    r.tz.as_deref().into(),
                    now.into(),
                    now.into(),
                ]),
        )?;
        Ok(())
    }

    /// Refreshes source metadata. When the zone changes, the next pull must normalize all dates again.
    pub fn refresh_calendar(&mut self, r: &CalRow, zone_changed: bool, now: i64) -> Result<()> {
        let mut q = Query::update();
        q.table(CALENDARS)
            .value("name", r.name.as_str())
            .value("color", r.color.as_deref())
            .value("kind", r.kind.as_str())
            .value("role", r.role.as_str())
            .value("share", r.share.as_str())
            .value("tz", r.tz.as_deref())
            .value("party_id", r.party_id.as_deref())
            .value("detail", r.detail.as_str())
            .value("gone", 0_i64)
            .value("updated", now)
            .and_where(Expr::col("id").eq(&r.id));
        if zone_changed {
            q.value("sync_cursor", None::<String>)
                .value("full_at", None::<i64>);
        }
        self.0.exec(&q)?;
        Ok(())
    }

    pub fn set_gone(&mut self, id: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table(CALENDARS)
                .value("gone", 1_i64)
                .value("sync_cursor", None::<String>)
                .value("updated", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        self.0.exec(
            Query::delete()
                .from_table(EVENTS)
                .and_where(Expr::col("calendar_id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_synced(
        &mut self,
        id: &str,
        cursor: Option<&str>,
        full: bool,
        now: i64,
    ) -> Result<()> {
        let mut q = Query::update();
        q.table(CALENDARS)
            .value("sync_state", "ok")
            .value("sync_error", None::<String>)
            .value("sync_cursor", cursor)
            .value("synced_at", now)
            .value("updated", now)
            .and_where(Expr::col("id").eq(id));
        if full {
            q.value("full_at", now);
        }
        self.0.exec(&q)?;
        Ok(())
    }

    pub fn set_failed(&mut self, id: &str, error: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table(CALENDARS)
                .value("sync_state", "error")
                .value("sync_error", error)
                .value("updated", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_settings(&mut self, id: &str, s: CalendarSettings<'_>, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table(CALENDARS)
                .value("enabled", i64::from(s.enabled))
                .value("party_id", s.party)
                .value("party_set", i64::from(s.party_set))
                .value("color", s.color)
                .value("updated", now)
                .value(
                    "sync_cursor",
                    Expr::case(Expr::col("detail").eq(s.detail), Expr::col("sync_cursor"))
                        .finally(None::<String>),
                )
                .value("detail", s.detail)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    /// Keeps only the times and removes attendees when details become private.
    pub fn strip_details(&mut self, calendar: &str, now: i64) -> Result<()> {
        let events = select(EVENTS, ["id"])
            .and_where(Expr::col("calendar_id").eq(calendar))
            .take();
        self.0.exec(
            Query::delete()
                .from_table(ATTENDEES)
                .and_where(Expr::col("event_id").in_subquery(events)),
        )?;
        let mut q = Query::update();
        q.table(EVENTS)
            .value("busy_only", 1_i64)
            .value("updated", now)
            .and_where(Expr::col("calendar_id").eq(calendar));
        for col in [
            "title",
            "location",
            "meeting_url",
            "excerpt",
            "organizer_party",
            "organizer_email",
            "organizer_name",
        ] {
            q.value(col, None::<String>);
        }
        self.0.exec(&q)?;
        Ok(())
    }

    /// Replaces changed event data and attendees, counting only fingerprint changes and revivals as new versions.
    pub fn write_event(&mut self, e: &EventRow, atts: &[AttRow], now: i64) -> Result<Written> {
        let old = self.event(&e.id)?;
        let (written, version) = match old {
            None => (Written::New, 1),
            Some(o) if o.deleted_ms.is_some() || o.fingerprint != e.fingerprint => {
                (Written::Changed, o.version + 1)
            }
            Some(mut o) => {
                let version = o.version;
                o.meeting_id.clone_from(&e.meeting_id);
                o.version = e.version;
                if o == *e {
                    return Ok(Written::Same);
                }
                (Written::Touched, version)
            }
        };
        let update = EV_COLS
            .into_iter()
            .filter(|c| !matches!(*c, "id" | "workspace_id" | "owner_user" | "meeting_id"))
            .chain(["updated"]);
        self.0.exec(
            Query::insert()
                .into_table(EVENTS)
                .columns(EV_COLS.into_iter().chain(["created", "updated"]))
                .values_panic([
                    e.id.as_str().into(),
                    e.calendar_id.as_str().into(),
                    e.workspace_id.as_str().into(),
                    e.owner_user.as_str().into(),
                    e.meeting_id.as_deref().into(),
                    e.ext_id.as_str().into(),
                    e.series_ext_id.as_deref().into(),
                    e.original_start.as_deref().into(),
                    e.ical_uid.as_deref().into(),
                    e.sequence.into(),
                    e.etag.as_deref().into(),
                    version.into(),
                    e.fingerprint.as_str().into(),
                    e.title.as_deref().into(),
                    e.location.as_deref().into(),
                    e.meeting_url.as_deref().into(),
                    e.excerpt.as_deref().into(),
                    e.start_ms.into(),
                    e.end_ms.into(),
                    e.tz.as_deref().into(),
                    i64::from(e.all_day).into(),
                    e.start_date.as_deref().into(),
                    e.end_date.as_deref().into(),
                    e.status.as_str().into(),
                    e.my_rsvp.as_deref().into(),
                    e.organizer_party.as_deref().into(),
                    e.organizer_email.as_deref().into(),
                    e.organizer_name.as_deref().into(),
                    i64::from(e.i_am_organizer).into(),
                    i64::from(e.can_modify).into(),
                    i64::from(e.busy_only).into(),
                    None::<i64>.into(),
                    now.into(),
                    now.into(),
                ])
                .on_conflict(OnConflict::column("id").update_columns(update).to_owned()),
        )?;
        self.0.exec(
            Query::delete()
                .from_table(ATTENDEES)
                .and_where(Expr::col("event_id").eq(&e.id)),
        )?;
        for (ord, a) in atts.iter().enumerate() {
            self.0.exec(
                Query::insert()
                    .into_table(ATTENDEES)
                    .columns([
                        "event_id",
                        "ord",
                        "party_id",
                        "email",
                        "name",
                        "ext_ref",
                        "rsvp",
                        "organizer",
                        "optional",
                        "is_self",
                    ])
                    .values_panic([
                        e.id.as_str().into(),
                        (ord as i64).into(),
                        a.party_id.as_deref().into(),
                        a.email.as_deref().into(),
                        a.name.as_deref().into(),
                        a.ext_ref.as_deref().into(),
                        a.rsvp.as_deref().into(),
                        i64::from(a.organizer).into(),
                        i64::from(a.optional).into(),
                        i64::from(a.is_self).into(),
                    ]),
            )?;
        }
        Ok(written)
    }

    fn mark_deleted(&mut self, predicate: Expr, now: i64) -> Result<u64> {
        self.0.exec(
            Query::update()
                .table(EVENTS)
                .value("deleted_ms", now)
                .value("updated", now)
                .value("version", Expr::col("version").add(1_i64))
                .and_where(Expr::col("deleted_ms").is_null())
                .and_where(predicate),
        )
    }

    pub fn delete_event(&mut self, id: &str, now: i64) -> Result<bool> {
        Ok(self.mark_deleted(Expr::col("id").eq(id), now)? > 0)
    }

    pub fn delete_occurrences(&mut self, cal: &str, ext_id: &str, now: i64) -> Result<u64> {
        self.mark_deleted(
            Expr::col("calendar_id")
                .eq(cal)
                .and(Expr::col("ext_id").eq(ext_id)),
            now,
        )
    }

    pub fn prune(&mut self, calendar: &str, before: i64) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table(EVENTS)
                .and_where(Expr::col("calendar_id").eq(calendar))
                .and_where(
                    Expr::col("end_ms")
                        .lt(before)
                        .or(Expr::col("deleted_ms").lt(before)),
                ),
        )?;
        Ok(())
    }

    pub fn set_meeting(&mut self, event: &str, meeting: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table(EVENTS)
                .value("meeting_id", meeting)
                .and_where(Expr::col("id").eq(event))
                .and_where(
                    Expr::col("meeting_id")
                        .is_null()
                        .or(Expr::col("meeting_id").ne(meeting)),
                ),
        )?;
        Ok(())
    }

    pub fn prune_meetings(&mut self, ws: &str, user: &str, keep: &[&str]) -> Result<()> {
        let ids = self.0.dialect().text_array(keep)?;
        self.0.exec(
            Query::delete()
                .from_table(MEETINGS)
                .and_where(owned(ws, user))
                .and_where(Expr::col("id").not_in_subquery(ids)),
        )?;
        Ok(())
    }

    pub fn put_meeting(&mut self, ws: &str, user: &str, m: Meeting<'_>, now: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table(MEETINGS)
                .columns([
                    "id",
                    "workspace_id",
                    "owner_user",
                    "primary_event_id",
                    "match_key",
                    "created",
                    "updated",
                ])
                .values_panic([
                    m.id.into(),
                    ws.into(),
                    user.into(),
                    m.primary.into(),
                    m.key.into(),
                    now.into(),
                    now.into(),
                ])
                .on_conflict(
                    OnConflict::column("id")
                        .update_columns(["primary_event_id", "match_key", "updated"])
                        .action_and_where(
                            Expr::col((MEETINGS, "primary_event_id"))
                                .ne(m.primary)
                                .or(Expr::col((MEETINGS, "match_key")).ne(m.key)),
                        )
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    /// Uploading a replacement invalidates the calendar pull cursor in the same transaction.
    pub fn put_ics_file(
        &mut self,
        ws: &str,
        conn: &str,
        name: &str,
        body: &str,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table(ICS_FILES)
                .columns(["connection_id", "workspace_id", "name", "body", "uploaded"])
                .values_panic([conn.into(), ws.into(), name.into(), body.into(), now.into()])
                .on_conflict(
                    OnConflict::column("connection_id")
                        .update_columns(["name", "body", "uploaded"])
                        .to_owned(),
                ),
        )?;
        self.0.exec(
            Query::update()
                .table(CALENDARS)
                .value("sync_cursor", None::<String>)
                .value("synced_at", None::<i64>)
                .and_where(Expr::col("connection_id").eq(conn)),
        )?;
        Ok(())
    }
}

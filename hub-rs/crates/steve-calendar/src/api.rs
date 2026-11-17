//! The calendar routes, relative to `/api`. Everything is the caller's own: their calendar sources
//! (connections they made), their calendars and what is cached of them.
//!
//! - `GET /calendar/sources`, `GET /calendar/sources/{conn}`: the caller's connections that are
//!   calendar sources, each with its features and calendars (sync state, person, detail).
//! - `POST /calendar/sources/{conn}/sync`: sync now (right after another sync, the cache answers).
//! - `PUT /calendar/sources/{conn}/ics {name, text}`: the `.ics` file of an ICS connection without
//!   a subscription address; synced at once.
//! - `PATCH /calendar/calendars/{id} {enabled?, party_id?, detail?, color?}`: whose calendar it is
//!   (`null`: the caller's own), how much is kept (`full` | `busy`; another person's defaults to
//!   `busy`), on or off.
//! - `GET /calendar/events?from&to`, `GET /calendar/events/{id}`, `GET /calendar/search?q&from&to`,
//!   `GET /calendar/freebusy?party&from&to`, `POST /calendar/find-time {people, duration_min, from?,
//!   to?}`: [`crate::query`].

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::ChangeEvent;

use crate::profile::Features;
use crate::query::{self, Range};
use crate::repo::{self, CalRow};
use crate::sources::IcsSource;
use crate::sync::{api, me_party};
use crate::{Calendar, ics};

pub fn routes(cal: Calendar) -> Router<Modules> {
    Router::new()
        .route("/calendar/sources", get(sources))
        .route("/calendar/sources/{conn}", get(source))
        .route("/calendar/sources/{conn}/sync", post(sync_now))
        .route(
            "/calendar/sources/{conn}/ics",
            put(upload).layer(DefaultBodyLimit::max(ics::MAX_BYTES + 64 * 1024)),
        )
        .route("/calendar/calendars", get(calendars))
        .route("/calendar/calendars/{id}", patch(edit))
        .route("/calendar/events", get(events))
        .route("/calendar/events/{id}", get(one))
        .route("/calendar/search", get(search))
        .route("/calendar/freebusy", get(freebusy))
        .route("/calendar/find-time", post(find_time))
        .with_state(cal)
}

/// A calendar with the name of its person.
fn cal_view(c: &rusqlite::Connection, me: Option<&str>, x: &CalRow) -> ApiResult<Value> {
    let mut v = serde_json::to_value(x)?;
    let name: Option<String> = match &x.party_id {
        Some(p) => c
            .query_row("SELECT name FROM parties WHERE id = ?", [p], |r| r.get(0))
            .optional()?,
        None => None,
    };
    v["party_name"] = json!(name);
    v["mine"] = json!(x.party_id.is_some() && x.party_id.as_deref() == me);
    Ok(v)
}

impl Calendar {
    /// The caller's connection `conn` as a calendar source, or 404.
    async fn source_view(&self, who: &Caller, conn: &str, features: Features) -> ApiResult<Value> {
        let out = self.connections().get(who, conn).map_err(api)?;
        let ics = if out.adapter == IcsSource::ID {
            let file: Option<String> = self
                .modules()
                .store
                .reader(|c| Ok::<_, ApiError>(repo::ics_file(c, conn)?.map(|(n, _)| n)))?;
            let cx = self.connections().cx(&who.ws, conn).map_err(api)?;
            let subscribed = IcsSource::subscribed_cx(&cx).await;
            Some(json!({"subscribed": subscribed, "file": file}))
        } else {
            None
        };
        let (ws, me) = (who.ws.clone(), who.me.clone());
        let conn = conn.to_string();
        let calendars = self.modules().store.reader(move |c| {
            let mep = me_party(c, &ws, &me)?;
            repo::of_connection(c, &conn)?
                .iter()
                .filter(|x| x.owner_user == me)
                .map(|x| cal_view(c, mep.as_deref(), x))
                .collect::<ApiResult<Vec<_>>>()
        })?;
        Ok(
            json!({"connection_id": out.id, "adapter": out.adapter, "status": out.status,
            "features": features, "calendars": calendars, "ics": ics}),
        )
    }

    async fn own_source(&self, who: &Caller, conn: &str) -> ApiResult<Features> {
        // visible to the caller and theirs
        let out = self.connections().get(who, conn).map_err(api)?;
        let mine: bool = self.modules().store.reader(|c| {
            Ok::<_, ApiError>(
                c.query_row(
                    "SELECT created_by = ? FROM connections WHERE id = ? AND workspace_id = ?",
                    params![who.me, conn, who.ws],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false),
            )
        })?;
        if !mine {
            return Err(ApiError::not_found("日历来源"));
        }
        match self.is_source(&who.ws, &out.id).await? {
            Some((_, f)) => Ok(f),
            None => Err(ApiError::not_found("日历来源")),
        }
    }
}

async fn sources(State(cal): State<Calendar>, who: Caller) -> ApiResult<Json<Value>> {
    let conns = cal.connections().list(&who).map_err(api)?;
    let mut out = Vec::new();
    for c in conns.iter().filter(|c| cal.source(&c.adapter).is_some()) {
        if let Ok(f) = cal.own_source(&who, &c.id).await {
            out.push(cal.source_view(&who, &c.id, f).await?);
        }
    }
    Ok(Json(json!(out)))
}

async fn source(
    State(cal): State<Calendar>,
    who: Caller,
    Path(conn): Path<String>,
) -> ApiResult<Json<Value>> {
    let f = cal.own_source(&who, &conn).await?;
    Ok(Json(cal.source_view(&who, &conn, f).await?))
}

async fn sync_now(
    State(cal): State<Calendar>,
    who: Caller,
    Path(conn): Path<String>,
) -> ApiResult<Json<Value>> {
    let f = cal.own_source(&who, &conn).await?;
    let report = cal.sync_connection(&who.ws, &conn, true).await?;
    let mut v = cal.source_view(&who, &conn, f).await?;
    v["report"] = serde_json::to_value(&report)?;
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    #[serde(default)]
    name: Option<String>,
    text: String,
}

async fn upload(
    State(cal): State<Calendar>,
    who: Caller,
    Path(conn): Path<String>,
    Body(b): Body<Upload>,
) -> ApiResult<Json<Value>> {
    let f = cal.own_source(&who, &conn).await?;
    let out = cal.connections().get(&who, &conn).map_err(api)?;
    if out.adapter != IcsSource::ID {
        return Err(ApiError::rule(steve_i18n::t!("calendar.api.not_ics")));
    }
    let cx = cal.connections().cx(&who.ws, &conn).map_err(api)?;
    if IcsSource::subscribed_cx(&cx).await {
        return Err(ApiError::rule(steve_i18n::t!("calendar.api.subscribed")));
    }
    let parsed = ics::Calendar::parse(&b.text).map_err(ApiError::Rule)?;
    let name = b
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(200).collect::<String>())
        .or(parsed.name.clone())
        .unwrap_or_else(|| "calendar.ics".into());
    let now = cal.modules().clock.now();
    cal.modules().store.transaction(|tx| {
        repo::put_ics_file(tx, &who.ws, &conn, &name, &b.text, now)?;
        // a new file starts the calendar's cursor over
        tx.execute(
            "UPDATE cal_calendars SET sync_cursor = NULL, synced_at = NULL WHERE connection_id = ?",
            [&conn],
        )?;
        Ok::<_, ApiError>(())
    })?;
    let report = cal.sync_connection(&who.ws, &conn, false).await?;
    let mut v = cal.source_view(&who, &conn, f).await?;
    v["report"] = serde_json::to_value(&report)?;
    Ok(Json(v))
}

async fn calendars(State(cal): State<Calendar>, who: Caller) -> ApiResult<Json<Value>> {
    cal.modules()
        .blocking(move |m| {
            m.store.reader(|c| {
                let me = me_party(c, &who.ws, &who.me)?;
                let xs = repo::of_user(c, &who.ws, &who.me)?
                    .iter()
                    .map(|x| cal_view(c, me.as_deref(), x))
                    .collect::<ApiResult<Vec<_>>>()?;
                Ok(Json(json!(xs)))
            })
        })
        .await
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// A contact id; `null`: the caller's own.
    #[serde(default)]
    pub party_id: Option<Value>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub color: Option<Value>,
}

impl Calendar {
    /// The owner changes a calendar's settings. Another person's calendar keeps only times unless
    /// `detail: "full"` is asked for; turning details off drops them from the cache at once.
    pub async fn edit(&self, who: &Caller, id: &str, e: Edit) -> ApiResult<Value> {
        let now = self.now();
        let (who2, id2) = (who.clone(), id.to_string());
        let (cal, resync) = self
            .modules()
            .blocking(move |m| {
                m.store.transaction(|tx| {
                    let cur = repo::calendar(tx, &id2)?
                        .filter(|c| c.workspace_id == who2.ws && c.owner_user == who2.me)
                        .ok_or_else(|| ApiError::not_found("日历"))?;
                    let me = me_party(tx, &who2.ws, &who2.me)?;
                    let (party, party_set) = match &e.party_id {
                        None => (cur.party_id.clone(), cur.party_set),
                        Some(Value::Null) => (me.clone(), true),
                        Some(Value::String(p)) => {
                            let ok: Option<String> = tx
                                .query_row(
                                    "SELECT id FROM parties WHERE workspace_id = ? AND id = ? AND status = 'active'",
                                    params![who2.ws, p],
                                    |r| r.get(0),
                                )
                                .optional()?;
                            (Some(ok.ok_or_else(|| ApiError::invalid("party_id"))?), true)
                        }
                        Some(_) => return Err(ApiError::invalid("party_id")),
                    };
                    let mine = party.is_some() && party == me;
                    let detail = match e.detail.as_deref() {
                        Some(d @ ("full" | "busy")) => d.to_string(),
                        Some(_) => return Err(ApiError::invalid("detail")),
                        None if e.party_id.is_some() && party != cur.party_id => {
                            if mine { "full".into() } else { "busy".into() }
                        }
                        None => cur.detail.clone(),
                    };
                    if detail == "full" && cur.role == "free_busy" {
                        return Err(ApiError::rule(steve_i18n::t!("calendar.api.free_busy_only")));
                    }
                    let color = match &e.color {
                        None => cur.color.clone(),
                        Some(Value::Null) => None,
                        Some(Value::String(c)) if c.len() <= 32 => Some(c.clone()),
                        Some(_) => return Err(ApiError::invalid("color")),
                    };
                    let enabled = e.enabled.unwrap_or(cur.enabled);
                    repo::set_settings(tx, &cur.id, enabled, party.as_deref(), party_set, &detail, color.as_deref(), now)?;
                    if detail == "busy" && cur.detail != "busy" {
                        repo::strip_details(tx, &cur.id, now)?;
                    }
                    tx.publish(ChangeEvent::Calendar { id: cur.id.clone(), ws: who2.ws.clone() });
                    let resync = (detail == "full" && cur.detail != "full") || (enabled && !cur.enabled);
                    Ok((repo::calendar(tx, &cur.id)?.expect("just updated"), resync))
                })
            })
            .await?;
        self.rematch(&who.ws, &who.me)?;
        if resync {
            let (me, ws, conn) = (self.clone(), who.ws.clone(), cal.connection_id.clone());
            tokio::spawn(async move {
                if let Err(e) = me.sync_connection(&ws, &conn, false).await {
                    tracing::warn!(connection = %conn, "calendar sync after a settings change failed: {e}");
                }
            });
        }
        let (ws, user) = (who.ws.clone(), who.me.clone());
        self.modules()
            .store
            .reader(|c| cal_view(c, me_party(c, &ws, &user)?.as_deref(), &cal))
    }
}

async fn edit(
    State(cal): State<Calendar>,
    who: Caller,
    Path(id): Path<String>,
    Body(e): Body<Edit>,
) -> ApiResult<Json<Value>> {
    Ok(Json(cal.edit(&who, &id, e).await?))
}

#[derive(Deserialize)]
struct RangeQ {
    from: Option<String>,
    to: Option<String>,
    q: Option<String>,
    party: Option<String>,
}

fn resolve(
    c: &rusqlite::Connection,
    who: &Caller,
    q: &RangeQ,
    now: i64,
) -> ApiResult<(chrono_tz::Tz, Range)> {
    let tz = query::user_tz(c, &who.ws, &who.me)?;
    let r = query::range(q.from.as_deref(), q.to.as_deref(), tz, now).map_err(ApiError::Rule)?;
    Ok((tz, r))
}

async fn events(
    State(cal): State<Calendar>,
    who: Caller,
    Query(q): Query<RangeQ>,
) -> ApiResult<Json<Value>> {
    cal.modules()
        .blocking(move |m| {
            m.store.reader(|c| {
                let (tz, r) = resolve(c, &who, &q, m.clock.now())?;
                Ok(Json(query::agenda(c, &who.ws, &who.me, &r, tz)?))
            })
        })
        .await
}

async fn one(
    State(cal): State<Calendar>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    cal.modules()
        .blocking(move |m| {
            m.store.reader(|c| {
                let tz = query::user_tz(c, &who.ws, &who.me)?;
                Ok(Json(query::event(c, &who.ws, &who.me, &id, tz)?.1))
            })
        })
        .await
}

async fn search(
    State(cal): State<Calendar>,
    who: Caller,
    Query(q): Query<RangeQ>,
) -> ApiResult<Json<Value>> {
    let past = cal.settings().past_days;
    let ahead = cal.settings().future_days;
    cal.modules()
        .blocking(move |m| {
            m.store.reader(|c| {
                let now = m.clock.now();
                let tz = query::user_tz(c, &who.ws, &who.me)?;
                let r =
                    query::range(
                        Some(q.from.as_deref().unwrap_or(
                            &query::local(now - past * 86_400_000, tz).replace(' ', "T"),
                        )),
                        Some(q.to.as_deref().unwrap_or(
                            &query::local(now + ahead * 86_400_000, tz).replace(' ', "T"),
                        )),
                        tz,
                        now,
                    )
                    .map_err(ApiError::Rule)?;
                Ok(Json(query::search(
                    c,
                    &who.ws,
                    &who.me,
                    q.q.as_deref().unwrap_or(""),
                    &r,
                    tz,
                    50,
                )?))
            })
        })
        .await
}

async fn freebusy(
    State(cal): State<Calendar>,
    who: Caller,
    Query(q): Query<RangeQ>,
) -> ApiResult<Json<Value>> {
    cal.modules()
        .blocking(move |m| {
            m.store.reader(|c| {
                let (tz, r) = resolve(c, &who, &q, m.clock.now())?;
                let party = query::person(c, &who.ws, q.party.as_deref().unwrap_or("me"))
                    .map_err(ApiError::Rule)?;
                Ok(Json(query::freebusy(
                    c,
                    &who.ws,
                    &who.me,
                    party.as_deref(),
                    &r,
                    tz,
                )?))
            })
        })
        .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindTime {
    #[serde(default)]
    people: Vec<String>,
    duration_min: i64,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

async fn find_time(
    State(cal): State<Calendar>,
    who: Caller,
    Body(b): Body<FindTime>,
) -> ApiResult<Json<Value>> {
    let s = cal.settings().clone();
    cal.modules()
        .blocking(move |m| {
            m.store.reader(|c| {
                let now = m.clock.now();
                let tz = query::user_tz(c, &who.ws, &who.me)?;
                let r = query::range(
                    Some(b.from.as_deref().unwrap_or("now")),
                    Some(
                        b.to.as_deref()
                            .unwrap_or(&query::local(now + 7 * 86_400_000, tz).replace(' ', "T")),
                    ),
                    tz,
                    now,
                )
                .map_err(ApiError::Rule)?;
                let people = b
                    .people
                    .iter()
                    .take(20)
                    .map(|p| query::person(c, &who.ws, p))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(ApiError::Rule)?;
                Ok(Json(query::find_time(
                    c,
                    &who.ws,
                    &who.me,
                    &people,
                    b.duration_min,
                    &r,
                    tz,
                    now,
                    &s,
                )?))
            })
        })
        .await
}

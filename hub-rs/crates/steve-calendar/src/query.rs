//! Reading the cache: the agenda, search, one event, free/busy and free time. The API and the tools
//! call the same functions, in the reader's time zone (their notification setting, else the
//! workspace's).

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult};

use crate::repo::{self, CalRow, EventRow};
use crate::sync::me_party;
use crate::{Settings, ics};

/// The reader's zone: their personal notification setting, else the workspace's.
pub fn user_tz(c: &mut impl steve_store::Read, ws: &str, user: &str) -> ApiResult<Tz> {
    Ok(steve_modules::zone::Zone::for_user(c, ws, Some(user))?.tz)
}

/// `ms` as local `YYYY-MM-DD HH:MM` in `tz`.
pub fn local(ms: i64, tz: Tz) -> String {
    steve_runtime::tz::Zone::parse(tz.name())
        .ok()
        .and_then(|z| z.local(ms).ok())
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn local_date(ms: i64, tz: Tz) -> NaiveDate {
    steve_runtime::tz::Zone::parse(tz.name())
        .ok()
        .and_then(|z| z.local(ms).ok())
        .map(|d| d.date())
        .unwrap_or_default()
}

fn midnight(d: NaiveDate, tz: Tz) -> i64 {
    ics::local_ms(tz, d.and_time(NaiveTime::MIN))
}

/// A time an agent or a page gives: RFC 3339, a local `YYYY-MM-DDTHH:MM`, a date (its start), or
/// `now` / `today` / `tomorrow`. `end`: a date means the end of that day.
pub fn parse_time(s: &str, tz: Tz, now: i64, end: bool) -> Result<i64, String> {
    let s = s.trim();
    let today = local_date(now, tz);
    let day = |d: NaiveDate| midnight(if end { d + Duration::days(1) } else { d }, tz);
    match s.to_ascii_lowercase().as_str() {
        "now" => return Ok(now),
        "today" => return Ok(day(today)),
        "tomorrow" => return Ok(day(today + Duration::days(1))),
        _ => {}
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.timestamp_millis());
    }
    for f in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, f) {
            return Ok(ics::local_ms(tz, t));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(day(d));
    }
    if let Ok(n) = s.parse::<i64>()
        && n > 1_000_000_000_000
    {
        return Ok(n);
    }
    Err(format!(
        "看不懂的时间「{s}」：用 2026-11-18、2026-11-18T14:00（{}）或带时区的 RFC 3339",
        tz.name()
    ))
}

/// A range as resolved, echoed back so the caller can check it.
#[derive(Debug, Clone, Serialize)]
pub struct Range {
    pub from: String,
    pub to: String,
    pub tz: String,
    pub from_ms: i64,
    pub to_ms: i64,
}

impl Range {
    pub fn new(from_ms: i64, to_ms: i64, tz: Tz) -> Self {
        Self {
            from: local(from_ms, tz),
            to: local(to_ms, tz),
            tz: tz.name().into(),
            from_ms,
            to_ms,
        }
    }
    fn dates(&self, tz: Tz) -> (String, String) {
        let f = local_date(self.from_ms, tz);
        // the last day touched, exclusive
        let t = local_date(self.to_ms - 1, tz) + Duration::days(1);
        (
            f.format("%Y-%m-%d").to_string(),
            t.format("%Y-%m-%d").to_string(),
        )
    }
}

/// `from`/`to` as given (either may be missing): by default the day of `from` (today).
pub fn range(from: Option<&str>, to: Option<&str>, tz: Tz, now: i64) -> Result<Range, String> {
    let f = match from {
        Some(s) => parse_time(s, tz, now, false)?,
        None => midnight(local_date(now, tz), tz),
    };
    let t = match to {
        Some(s) => parse_time(s, tz, now, true)?,
        None => midnight(local_date(f, tz) + Duration::days(1), tz),
    };
    if t <= f {
        return Err("结束时间要晚于开始时间".into());
    }
    if t - f > 400 * 86_400_000 {
        return Err("一次最多查 400 天".into());
    }
    Ok(Range::new(f, t, tz))
}

/// The owner's calendars that are theirs (the agenda), and all of them by id (names).
pub struct Mine {
    pub me: Option<String>,
    pub all: Vec<CalRow>,
}

impl Mine {
    pub fn load(c: &Connection, ws: &str, user: &str) -> ApiResult<Self> {
        Ok(Self {
            me: me_party(c, ws, user)?,
            all: repo::of_user(c, ws, user)?,
        })
    }
    /// Enabled calendars of `party` (none: the owner's own).
    pub fn of(&self, party: Option<&str>) -> Vec<String> {
        let want = party.or(self.me.as_deref());
        self.all
            .iter()
            .filter(|c| c.enabled && !c.gone && want.is_some() && c.party_id.as_deref() == want)
            .map(|c| c.id.clone())
            .collect()
    }
    fn name(&self, id: &str) -> String {
        self.all
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }
    fn cal(&self, id: &str) -> Option<&CalRow> {
        self.all.iter().find(|c| c.id == id)
    }
}

/// An event as the API and the tools show it.
pub fn view(e: &EventRow, mine: &Mine, tz: Tz) -> Value {
    let mut v = json!({
        "id": e.id, "meeting_id": e.meeting_id,
        "calendar": {"id": e.calendar_id, "name": mine.name(&e.calendar_id)},
        "title": e.title, "all_day": e.all_day,
        "start_ms": e.start_ms, "end_ms": e.end_ms,
        "status": e.status, "busy_only": e.busy_only, "version": e.version,
    });
    if e.all_day {
        v["start"] = json!(e.start_date);
        // the last day, inclusive
        v["end"] = json!(
            e.end_date
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .map(|d| (d - Duration::days(1)).format("%Y-%m-%d").to_string())
        );
    } else {
        v["start"] = json!(local(e.start_ms, tz));
        v["end"] = json!(local(e.end_ms, tz));
    }
    for (k, x) in [
        ("location", &e.location),
        ("meeting_url", &e.meeting_url),
        ("my_rsvp", &e.my_rsvp),
        ("excerpt", &e.excerpt),
    ] {
        if let Some(x) = x {
            v[k] = json!(x);
        }
    }
    if e.organizer_email.is_some() || e.organizer_name.is_some() || e.i_am_organizer {
        v["organizer"] = json!({"name": e.organizer_name, "email": e.organizer_email,
            "party_id": e.organizer_party, "me": e.i_am_organizer});
    }
    v
}

fn busy_for_me(e: &EventRow) -> bool {
    !e.all_day && e.status != "cancelled" && e.my_rsvp.as_deref() != Some("declined")
}

/// The owner's agenda over `r`: one row per meeting (the shown occurrence, `also_in` the other
/// calendars), cancelled ones left out, and which timed events overlap (`conflicts_with`).
pub fn agenda(c: &Connection, ws: &str, user: &str, r: &Range, tz: Tz) -> ApiResult<Value> {
    let mine = Mine::load(c, ws, user)?;
    let cals = mine.of(None);
    let (fd, td) = r.dates(tz);
    let events = repo::in_range(c, &cals, r.from_ms, r.to_ms, &fd, &td)?;
    let primaries: HashMap<String, String> = {
        let mut st = c.prepare_cached(
            "SELECT id, primary_event_id FROM cal_meetings WHERE workspace_id = ? AND owner_user = ?",
        )?;
        st.query_map(params![ws, user], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let present: HashSet<&str> = events.iter().map(|e| e.id.as_str()).collect();
    let mut also: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut shown: Vec<&EventRow> = Vec::new();
    for e in events.iter().filter(|e| e.status != "cancelled") {
        let primary = e.meeting_id.as_ref().and_then(|m| primaries.get(m));
        match primary {
            Some(p) if p != &e.id && present.contains(p.as_str()) => also
                .entry(p.clone())
                .or_default()
                .push(mine.name(&e.calendar_id)),
            _ => shown.push(e),
        }
    }
    let mut out = Vec::new();
    for e in &shown {
        let mut v = view(e, &mine, tz);
        if let Some(a) = also.get(&e.id) {
            v["also_in"] = json!(a);
        }
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM cal_attendees WHERE event_id = ?",
            [&e.id],
            |r| r.get(0),
        )?;
        if n > 0 {
            v["attendees"] = json!(n);
        }
        if busy_for_me(e) {
            let clash: Vec<&str> = shown
                .iter()
                .filter(|o| {
                    o.id != e.id && busy_for_me(o) && o.start_ms < e.end_ms && o.end_ms > e.start_ms
                })
                .map(|o| o.id.as_str())
                .collect();
            if !clash.is_empty() {
                v["conflicts_with"] = json!(clash);
            }
        }
        out.push(v);
    }
    Ok(json!({"range": r, "events": out, "calendars": status(&mine, &cals, tz)}))
}

/// Which calendars the answer came from and how fresh each is.
fn status(mine: &Mine, ids: &[String], tz: Tz) -> Value {
    json!(
        ids.iter()
            .filter_map(|id| mine.cal(id))
            .map(
                |c| json!({"id": c.id, "name": c.name, "state": c.sync_state,
            "as_of": c.synced_at.map(|t| local(t, tz)), "error": c.sync_error})
            )
            .collect::<Vec<_>>()
    )
}

pub fn search(
    c: &Connection,
    ws: &str,
    user: &str,
    q: &str,
    r: &Range,
    tz: Tz,
    limit: usize,
) -> ApiResult<Value> {
    let q = q.trim();
    if q.is_empty() || q.chars().count() > 200 {
        return Err(ApiError::invalid("query"));
    }
    let mine = Mine::load(c, ws, user)?;
    let cals = mine.of(None);
    let found = repo::search(c, &cals, q, r.from_ms, r.to_ms, limit)?;
    Ok(
        json!({"range": r, "query": q, "events": found.iter().map(|e| view(e, &mine, tz)).collect::<Vec<_>>()}),
    )
}

/// One event of the owner's, by event or meeting id, with its attendees.
pub fn event(
    c: &Connection,
    ws: &str,
    user: &str,
    id: &str,
    tz: Tz,
) -> ApiResult<(EventRow, Value)> {
    let id = if id.starts_with("mtg_") {
        c.query_row(
            "SELECT primary_event_id FROM cal_meetings WHERE id = ? AND workspace_id = ? AND owner_user = ?",
            params![id, ws, user],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| ApiError::not_found("日程"))?
    } else {
        id.to_string()
    };
    let e = repo::event(c, &id)?
        .filter(|e| e.workspace_id == ws && e.owner_user == user)
        .ok_or_else(|| ApiError::not_found("日程"))?;
    let mine = Mine::load(c, ws, user)?;
    let mut v = view(&e, &mine, tz);
    if e.deleted_ms.is_some() {
        v["deleted"] = json!(true);
    }
    v["attendees"] = json!(repo::attendees(c, &e.id)?);
    if let Some(m) = &e.meeting_id {
        let mut st = c.prepare_cached(
            "SELECT calendar_id FROM cal_events WHERE meeting_id = ? AND id != ? AND deleted_ms IS NULL",
        )?;
        let others: Vec<String> = st
            .query_map(params![m, e.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .map(|x| mine.name(x))
            .collect();
        if !others.is_empty() {
            v["also_in"] = json!(others);
        }
    }
    if let Some(cal) = mine.cal(&e.calendar_id) {
        v["as_of"] = json!(cal.synced_at.map(|t| local(t, tz)));
    }
    Ok((e, v))
}

/// Busy times of a person (`None`: the owner) from the owner's calendars bound to them, merged.
pub fn busy(
    c: &Connection,
    mine: &Mine,
    party: Option<&str>,
    from: i64,
    to: i64,
) -> ApiResult<Option<Vec<(i64, i64)>>> {
    let cals = mine.of(party);
    if cals.is_empty() {
        return Ok(None);
    }
    // all-day events do not make anyone busy: an empty date range leaves them out
    let mut xs: Vec<(i64, i64)> = repo::in_range(c, &cals, from, to, "9999", "0000")?
        .iter()
        .filter(|e| busy_for_me(e))
        .map(|e| (e.start_ms.max(from), e.end_ms.min(to)))
        .filter(|(s, e)| e > s)
        .collect();
    xs.sort();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in xs {
        match out.last_mut() {
            Some(l) if s <= l.1 => l.1 = l.1.max(e),
            _ => out.push((s, e)),
        }
    }
    Ok(Some(out))
}

/// The name of a contact.
fn party_name(c: &Connection, id: &str) -> ApiResult<String> {
    Ok(
        c.query_row("SELECT name FROM parties WHERE id = ?", [id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .unwrap_or_else(|| id.to_string()),
    )
}

/// A person as an agent names them: a contact id, `me`, or a name exactly one contact has.
pub fn person(c: &Connection, ws: &str, who: &str) -> Result<Option<String>, String> {
    let who = who.trim();
    if who.is_empty() || who.eq_ignore_ascii_case("me") || who == "我" {
        return Ok(None);
    }
    let by_id: Option<String> = c
        .query_row(
            "SELECT id FROM parties WHERE workspace_id = ? AND id = ? AND status != 'deleted'",
            params![ws, who],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(id) = by_id {
        return crate::sync::current_party(c, id)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    let mut st = c
        .prepare_cached("SELECT id FROM parties WHERE workspace_id = ? AND status = 'active' AND name = ? COLLATE NOCASE LIMIT 2")
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = st
        .query_map(params![ws, who], |r| r.get(0))
        .and_then(|r| r.collect())
        .map_err(|e| e.to_string())?;
    match ids.as_slice() {
        [one] => Ok(Some(one.clone())),
        [] => Err(format!(
            "找不到联系人「{who}」：先用 steve_contacts_find 查到 id"
        )),
        _ => Err(format!(
            "「{who}」有重名的联系人：用 steve_contacts_find 查到 id 再传"
        )),
    }
}

/// Free/busy of one person over `r`.
pub fn freebusy(
    c: &Connection,
    ws: &str,
    user: &str,
    party: Option<&str>,
    r: &Range,
    tz: Tz,
) -> ApiResult<Value> {
    let mine = Mine::load(c, ws, user)?;
    let name = match party {
        Some(p) => party_name(c, p)?,
        None => "me".into(),
    };
    let Some(b) = busy(c, &mine, party, r.from_ms, r.to_ms)? else {
        return Ok(json!({"range": r, "person": {"id": party, "name": name}, "known": false}));
    };
    Ok(
        json!({"range": r, "person": {"id": party, "name": name}, "known": true,
        "busy": b.iter().map(|(s, e)| json!({"start": local(*s, tz), "end": local(*e, tz), "start_ms": s, "end_ms": e})).collect::<Vec<_>>(),
        "calendars": status(&mine, &mine.of(party), tz)}),
    )
}

/// Free times of `duration` minutes for the owner and `people`, inside `r`, in working hours on
/// working days, on the [`Settings::slot_step_min`] grid, not before now: the earliest
/// [`Settings::slots`] that do not overlap each other, each with why it was chosen. People without
/// a calendar are named in `unknown` and not counted.
pub fn find_time(
    c: &Connection,
    ws: &str,
    user: &str,
    people: &[Option<String>],
    minutes: i64,
    r: &Range,
    tz: Tz,
    now: i64,
    s: &Settings,
) -> ApiResult<Value> {
    if !(5..=24 * 60).contains(&minutes) {
        return Err(ApiError::invalid("duration_min"));
    }
    let mine = Mine::load(c, ws, user)?;
    let mut all: Vec<(i64, i64)> = Vec::new();
    let (mut known, mut unknown) = (Vec::new(), Vec::new());
    let mut seen = HashSet::new();
    for p in std::iter::once(None).chain(people.iter().cloned()) {
        let p = p.filter(|p| Some(p) != mine.me.as_ref());
        if !seen.insert(p.clone()) {
            continue;
        }
        let name = match &p {
            Some(id) => party_name(c, id)?,
            None => "me".into(),
        };
        match busy(c, &mine, p.as_deref(), r.from_ms, r.to_ms)? {
            Some(b) => {
                all.extend(b);
                known.push(json!({"id": p, "name": name}));
            }
            None => unknown.push(json!({"id": p, "name": name})),
        }
    }
    all.sort();
    let len = minutes * 60_000;
    let step = s.slot_step_min.max(5) * 60_000;
    let mut slots: Vec<(i64, i64)> = Vec::new();
    let mut d = local_date(r.from_ms.max(now), tz);
    let last = local_date(r.to_ms - 1, tz);
    while d <= last && slots.len() < s.slots {
        if s.work_days.contains(&d.weekday().number_from_monday()) {
            let day_start = ics::local_ms(tz, d.and_time(s.work_start));
            let day_end = ics::local_ms(tz, d.and_time(s.work_end));
            // the first grid point at or after the later of the range start, now and 09:00
            let from = day_start.max(r.from_ms).max(now);
            let mut at = day_start + ((from - day_start + step - 1).max(0) / step) * step;
            while at + len <= day_end.min(r.to_ms) && slots.len() < s.slots {
                let end = at + len;
                let clash = all.iter().find(|(bs, be)| *bs < end && *be > at);
                match clash {
                    Some((_, be)) => {
                        // jump past the busy time, back onto the grid
                        at = day_start + ((be - day_start + step - 1) / step) * step;
                    }
                    None => {
                        slots.push((at, end));
                        at = end + ((step - (end - day_start) % step) % step);
                    }
                }
            }
        }
        d += Duration::days(1);
    }
    let hours = format!(
        "{}–{}",
        s.work_start.format("%H:%M"),
        s.work_end.format("%H:%M")
    );
    let out: Vec<Value> = slots
        .iter()
        .enumerate()
        .map(|(i, (a, b))| {
            let mut why = vec![
                format!("工作日的工作时间内（{hours}，{}）", tz.name()),
                if known.len() > 1 { "参与的人这段时间都没有日程".to_string() } else { "你这段时间没有日程".to_string() },
            ];
            if i == 0 {
                why.push("范围内最早的空档".into());
            }
            json!({"start": local(*a, tz), "end": local(*b, tz), "start_ms": a, "end_ms": b, "why": why})
        })
        .collect();
    Ok(
        json!({"range": r, "duration_min": minutes, "slots": out, "checked": known, "unknown": unknown,
        "note": if unknown.is_empty() { Value::Null } else { json!("unknown 里的人没有接入日历，没有算他们的忙闲") }}),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_dst_table() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
        ] {
            let z = steve_runtime::tz::Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let ms = z.instant(wall).unwrap();
            assert_eq!(
                parse_time(text, name.parse().unwrap(), ms, false).unwrap(),
                ms
            );
            assert_eq!(
                local(ms, name.parse().unwrap()),
                z.local(ms).unwrap().format("%Y-%m-%d %H:%M").to_string()
            );
        }
    }

    use super::*;

    const TZ: Tz = chrono_tz::Asia::Shanghai;

    #[test]
    fn times_parse_in_the_readers_zone() {
        let now = DateTime::parse_from_rfc3339("2026-11-17T10:00:00+08:00")
            .unwrap()
            .timestamp_millis();
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis();
        assert_eq!(
            parse_time("2026-11-18T14:00", TZ, now, false),
            Ok(at("2026-11-18T14:00:00+08:00"))
        );
        assert_eq!(
            parse_time("2026-11-18", TZ, now, false),
            Ok(at("2026-11-18T00:00:00+08:00"))
        );
        assert_eq!(
            parse_time("2026-11-18", TZ, now, true),
            Ok(at("2026-11-19T00:00:00+08:00"))
        );
        assert_eq!(
            parse_time("tomorrow", TZ, now, false),
            Ok(at("2026-11-18T00:00:00+08:00"))
        );
        assert_eq!(
            parse_time("2026-11-18T06:00:00Z", TZ, now, false),
            Ok(at("2026-11-18T14:00:00+08:00"))
        );
        assert!(parse_time("next friday", TZ, now, false).is_err());
        let r = range(None, None, TZ, now).unwrap();
        assert_eq!(
            (r.from.as_str(), r.to.as_str()),
            ("2026-11-17 00:00", "2026-11-18 00:00")
        );
        assert!(range(Some("2026-11-18"), Some("2026-11-17T00:00"), TZ, now).is_err());
    }
}

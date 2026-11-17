//! iCalendar (RFC 5545) reading for the ICS source: unfolding, properties, `VEVENT`s, and the
//! expansion of recurring events into occurrences inside a window. Deterministic, no network.
//!
//! What is understood: `UID`, `SUMMARY`, `LOCATION`, `DESCRIPTION`, `URL`, `DTSTART`, `DTEND`,
//! `DURATION`, `STATUS`, `SEQUENCE`, `ORGANIZER`, `ATTENDEE`, `RRULE`, `EXDATE`, `RECURRENCE-ID` and
//! the calendar's `X-WR-CALNAME` / `X-WR-TIMEZONE`. Times are UTC, floating (the calendar's zone) or
//! in a `TZID` that names an IANA zone (or a common Windows zone name). `RRULE` with `FREQ`
//! daily/weekly/monthly/yearly, `INTERVAL`, `COUNT`, `UNTIL`, `BYDAY`, `BYMONTHDAY` and `BYMONTH`
//! is expanded; a rule with any other part keeps only its first occurrence, rather than guessing.

use chrono::{
    DateTime, Datelike, Days, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
    Weekday,
};
use chrono_tz::Tz;

use crate::profile::{Attendee, Person, RemoteEvent, When, Window};

/// The largest calendar file read (bytes).
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// `VEVENT`s read from one file.
pub const MAX_COMPONENTS: usize = 20_000;
/// Occurrences one series may have inside a window.
pub const MAX_OCCURRENCES: usize = 2_000;
/// Periods a rule is walked through before giving up (a daily rule over ~27 years).
const MAX_PERIODS: usize = 10_000;

#[derive(Debug, Clone, PartialEq)]
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, k: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    }
}

/// A parsed calendar file.
#[derive(Debug, Clone, Default)]
pub struct Calendar {
    pub name: Option<String>,
    /// `X-WR-TIMEZONE`, when it names a known zone.
    pub tz: Option<String>,
    events: Vec<Vec<Prop>>,
}

/// Unfolded content lines (a line starting with a space or tab continues the one before).
fn lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let l = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = l.strip_prefix([' ', '\t'])
            && let Some(last) = out.last_mut()
        {
            last.push_str(rest);
            continue;
        }
        if !l.is_empty() {
            out.push(l.to_string());
        }
    }
    out
}

/// `NAME;P=V;P="a:b":value`.
fn prop(line: &str) -> Option<Prop> {
    let mut quoted = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = Vec::new();
    let (mut cur, mut quoted) = (String::new(), false);
    for c in head.chars() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => parts.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    parts.push(cur);
    let mut it = parts.into_iter();
    let name = it.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = it
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_ascii_uppercase(), v.to_string()))
        })
        .collect();
    Some(Prop {
        name,
        params,
        value: value.to_string(),
    })
}

/// A TEXT value: `\n`, `\,`, `\;`, `\\`.
fn text(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(x) => out.push(x),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Windows zone names some exporters write as `TZID`, to their IANA zone.
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("China Standard Time", "Asia/Shanghai"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("India Standard Time", "Asia/Kolkata"),
    ("GMT Standard Time", "Europe/London"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Eastern Standard Time", "America/New_York"),
    ("Central Standard Time", "America/Chicago"),
    ("Mountain Standard Time", "America/Denver"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("UTC", "UTC"),
];

/// An IANA zone from a `TZID` (quotes and a leading `/` dropped), or a known Windows name.
pub fn zone(name: &str) -> Option<Tz> {
    let n = name.trim().trim_matches('"').trim_start_matches('/');
    n.parse::<Tz>().ok().or_else(|| {
        WINDOWS_ZONES
            .iter()
            .find(|(w, _)| w.eq_ignore_ascii_case(n))
            .and_then(|(_, i)| i.parse().ok())
    })
}

/// A date or time value.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Dt {
    Date(NaiveDate),
    Utc(DateTime<Utc>),
    /// Wall time in a zone (`None`: floating, the calendar's zone).
    Local(NaiveDateTime, Option<Tz>),
}

fn date(v: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d").ok()
}

fn datetime(p: &Prop) -> Option<Dt> {
    let v = p.value.trim();
    if p.param("VALUE")
        .is_some_and(|x| x.eq_ignore_ascii_case("DATE"))
        || v.len() == 8
    {
        return date(v).map(Dt::Date);
    }
    let (body, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let ndt = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M"))
        .ok()?;
    if utc {
        return Some(Dt::Utc(Utc.from_utc_datetime(&ndt)));
    }
    Some(Dt::Local(ndt, p.param("TZID").and_then(zone)))
}

/// `EXDATE` may list several values.
fn datetimes(p: &Prop) -> Vec<Dt> {
    p.value
        .split(',')
        .filter_map(|v| {
            datetime(&Prop {
                value: v.to_string(),
                ..p.clone()
            })
        })
        .collect()
}

/// A wall time in `tz` as an instant: in a gap (spring forward) the time after it, when repeated
/// (fall back) the first.
pub fn local_ms(tz: Tz, t: NaiveDateTime) -> i64 {
    for shift in [0, 30, 60, 90, 120] {
        if let Some(d) = tz
            .from_local_datetime(&(t + Duration::minutes(shift)))
            .earliest()
        {
            return d.timestamp_millis();
        }
    }
    Utc.from_utc_datetime(&t).timestamp_millis()
}

impl Dt {
    fn ms(&self, cal_tz: Tz) -> i64 {
        match *self {
            Dt::Date(d) => local_ms(cal_tz, d.and_time(NaiveTime::MIN)),
            Dt::Utc(t) => t.timestamp_millis(),
            Dt::Local(t, z) => local_ms(z.unwrap_or(cal_tz), t),
        }
    }
    /// The key of an occurrence's original start: `YYYYMMDD` for a date, the UTC instant otherwise.
    fn key(&self, cal_tz: Tz) -> String {
        match self {
            Dt::Date(d) => d.format("%Y%m%d").to_string(),
            _ => DateTime::<Utc>::from_timestamp_millis(self.ms(cal_tz))
                .map(|t| t.format("%Y%m%dT%H%M%SZ").to_string())
                .unwrap_or_default(),
        }
    }
}

/// `P1W`, `PT1H30M`, `-P1D`, `P1DT2H`.
fn duration(v: &str) -> Option<Duration> {
    let (neg, v) = match v.trim().strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.trim().strip_prefix('+').unwrap_or(v.trim())),
    };
    let v = v.strip_prefix('P')?;
    let (mut total, mut num, mut time) = (Duration::zero(), String::new(), false);
    for c in v.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' => time = true,
            u => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (u, time) {
                    ('W', _) => Duration::weeks(n),
                    ('D', _) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone)]
struct Rule {
    freq: Freq,
    interval: u32,
    count: Option<usize>,
    until: Option<Dt>,
    byday: Vec<(Option<i32>, Weekday)>,
    bymonthday: Vec<i32>,
    bymonth: Vec<u32>,
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// `None`: a rule this reader does not expand (only the first occurrence is kept).
fn rule(v: &str) -> Option<Rule> {
    let mut r = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        byday: Vec::new(),
        bymonthday: Vec::new(),
        bymonth: Vec::new(),
    };
    let mut freq = None;
    for part in v.split(';').filter(|p| !p.is_empty()) {
        let (k, val) = part.split_once('=')?;
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match val.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                })
            }
            "INTERVAL" => r.interval = val.parse().ok().filter(|n| *n >= 1)?,
            "COUNT" => r.count = Some(val.parse().ok()?),
            "UNTIL" => {
                r.until = Some(datetime(&Prop {
                    name: "UNTIL".into(),
                    params: Vec::new(),
                    value: val.into(),
                })?)
            }
            "BYDAY" => {
                for d in val.split(',') {
                    let d = d.trim().to_ascii_uppercase();
                    let (n, wd) = d.split_at(d.len().checked_sub(2)?);
                    let n = if n.is_empty() {
                        None
                    } else {
                        Some(n.parse::<i32>().ok().filter(|n| *n != 0)?)
                    };
                    r.byday.push((n, weekday(wd)?));
                }
            }
            "BYMONTHDAY" => {
                for d in val.split(',') {
                    r.bymonthday.push(
                        d.trim()
                            .parse()
                            .ok()
                            .filter(|n: &i32| *n != 0 && n.abs() <= 31)?,
                    );
                }
            }
            "BYMONTH" => {
                for m in val.split(',') {
                    r.bymonth.push(
                        m.trim()
                            .parse()
                            .ok()
                            .filter(|n: &u32| (1..=12).contains(n))?,
                    );
                }
            }
            "WKST" => {}
            _ => return None,
        }
    }
    r.freq = freq?;
    // an ordinal BYDAY only has a meaning in a month or year
    if matches!(r.freq, Freq::Daily | Freq::Weekly) && r.byday.iter().any(|(n, _)| n.is_some()) {
        return None;
    }
    Some(r)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let first = NaiveDate::from_ymd_opt(y, m, 1).expect("valid month");
    let next = first
        .checked_add_months(Months::new(1))
        .expect("next month");
    (next - first).num_days() as u32
}

/// The days of month `(y, m)` that `byday` (ordinal or every) selects.
fn month_weekdays(y: i32, m: u32, byday: &[(Option<i32>, Weekday)]) -> Vec<NaiveDate> {
    let n = days_in_month(y, m);
    let all: Vec<NaiveDate> = (1..=n)
        .filter_map(|d| NaiveDate::from_ymd_opt(y, m, d))
        .collect();
    let mut out = Vec::new();
    for (ord, wd) in byday {
        let these: Vec<NaiveDate> = all.iter().copied().filter(|d| d.weekday() == *wd).collect();
        match ord {
            None => out.extend(these),
            Some(k) if *k > 0 => out.extend(these.get(*k as usize - 1).copied()),
            Some(k) => out.extend(
                these
                    .len()
                    .checked_sub(k.unsigned_abs() as usize)
                    .and_then(|i| these.get(i).copied()),
            ),
        }
    }
    out
}

fn month_days(y: i32, m: u32, bymonthday: &[i32]) -> Vec<NaiveDate> {
    let n = days_in_month(y, m) as i32;
    bymonthday
        .iter()
        .filter_map(|d| {
            let day = if *d > 0 { *d } else { n + 1 + d };
            (1..=n)
                .contains(&day)
                .then(|| NaiveDate::from_ymd_opt(y, m, day as u32))
                .flatten()
        })
        .collect()
}

/// The dates of period `k` of `r` starting from `start`, sorted.
fn period(r: &Rule, start: NaiveDate, k: u32) -> Vec<NaiveDate> {
    let step = k.saturating_mul(r.interval);
    let mut out = match r.freq {
        Freq::Daily => start
            .checked_add_days(Days::new(step as u64))
            .into_iter()
            .filter(|d| r.byday.is_empty() || r.byday.iter().any(|(_, w)| *w == d.weekday()))
            .filter(|d| r.bymonth.is_empty() || r.bymonth.contains(&d.month()))
            .collect::<Vec<_>>(),
        Freq::Weekly => {
            let monday = start - Duration::days(start.weekday().num_days_from_monday() as i64);
            let Some(week) = monday.checked_add_days(Days::new(step as u64 * 7)) else {
                return Vec::new();
            };
            let wds: Vec<Weekday> = if r.byday.is_empty() {
                vec![start.weekday()]
            } else {
                r.byday.iter().map(|(_, w)| *w).collect()
            };
            wds.iter()
                .map(|w| week + Duration::days(w.num_days_from_monday() as i64))
                .collect()
        }
        Freq::Monthly => {
            let Some(first) = start
                .with_day(1)
                .and_then(|d| d.checked_add_months(Months::new(step)))
            else {
                return Vec::new();
            };
            let (y, m) = (first.year(), first.month());
            if !r.bymonth.is_empty() && !r.bymonth.contains(&m) {
                Vec::new()
            } else if !r.bymonthday.is_empty() {
                month_days(y, m, &r.bymonthday)
            } else if !r.byday.is_empty() {
                month_weekdays(y, m, &r.byday)
            } else {
                month_days(y, m, &[start.day() as i32])
            }
        }
        Freq::Yearly => {
            let y = start.year() + step as i32;
            let months = if r.bymonth.is_empty() {
                vec![start.month()]
            } else {
                r.bymonth.clone()
            };
            months
                .into_iter()
                .flat_map(|m| {
                    if !r.bymonthday.is_empty() {
                        month_days(y, m, &r.bymonthday)
                    } else if !r.byday.is_empty() {
                        month_weekdays(y, m, &r.byday)
                    } else {
                        month_days(y, m, &[start.day() as i32])
                    }
                })
                .collect()
        }
    };
    out.sort();
    out.dedup();
    out
}

impl Calendar {
    pub fn parse(body: &str) -> Result<Self, String> {
        if body.len() > MAX_BYTES {
            return Err(format!("日历文件超过 {} MB", MAX_BYTES / 1024 / 1024));
        }
        let mut cal = Calendar::default();
        let mut seen_calendar = false;
        // component stack; properties only of a VEVENT directly or of the calendar are kept
        let mut stack: Vec<String> = Vec::new();
        let mut cur: Option<Vec<Prop>> = None;
        for l in lines(body) {
            let Some(p) = prop(&l) else { continue };
            match p.name.as_str() {
                "BEGIN" => {
                    let c = p.value.trim().to_ascii_uppercase();
                    if c == "VCALENDAR" {
                        seen_calendar = true;
                    }
                    if c == "VEVENT" && stack.last().map(String::as_str) == Some("VCALENDAR") {
                        cur = Some(Vec::new());
                    }
                    stack.push(c);
                }
                "END" => {
                    let c = p.value.trim().to_ascii_uppercase();
                    if c == "VEVENT"
                        && let Some(ev) = cur.take()
                    {
                        if cal.events.len() >= MAX_COMPONENTS {
                            return Err(format!("日历文件里的日程超过 {MAX_COMPONENTS} 个"));
                        }
                        cal.events.push(ev);
                    }
                    stack.pop();
                }
                _ => match stack.last().map(String::as_str) {
                    Some("VEVENT") => {
                        if let Some(ev) = cur.as_mut() {
                            ev.push(p);
                        }
                    }
                    Some("VCALENDAR") => match p.name.as_str() {
                        "X-WR-CALNAME" => {
                            cal.name = Some(text(&p.value)).filter(|s| !s.trim().is_empty())
                        }
                        "X-WR-TIMEZONE" => cal.tz = zone(&p.value).map(|z| z.name().to_string()),
                        _ => {}
                    },
                    _ => {}
                },
            }
        }
        if !seen_calendar {
            return Err("不是 iCalendar 文件（没有 BEGIN:VCALENDAR）".into());
        }
        Ok(cal)
    }

    /// Every occurrence overlapping `w`, recurring events expanded. `default_tz` is the zone of
    /// floating times and dates when the file names none.
    pub fn occurrences(&self, w: Window, default_tz: Tz) -> Vec<RemoteEvent> {
        let tz = self.tz.as_deref().and_then(zone).unwrap_or(default_tz);
        let events: Vec<Ev> = self.events.iter().filter_map(|p| Ev::read(p)).collect();
        // overrides of single occurrences: (uid, original start key) → the event
        let overrides: std::collections::HashMap<(String, String), &Ev> = events
            .iter()
            .filter_map(|e| Some(((e.uid.clone(), e.recurrence_id?.key(tz)), e)))
            .collect();
        let mut out = Vec::new();
        for e in events.iter().filter(|e| e.recurrence_id.is_none()) {
            match &e.rrule {
                None => {
                    if let Some(x) = e.occurrence(tz, e.start, None)
                        && overlaps(&x, w)
                    {
                        out.push(x);
                    }
                }
                Some(r) => {
                    let excluded: Vec<String> = e.exdates.iter().map(|d| d.key(tz)).collect();
                    for start in e.starts(r.as_ref(), tz, w) {
                        let key = start.key(tz);
                        if excluded.contains(&key) {
                            continue;
                        }
                        let x = match overrides.get(&(e.uid.clone(), key.clone())) {
                            Some(o) => o.occurrence(tz, o.start, Some(&key)),
                            None => e.occurrence(tz, start, Some(&key)),
                        };
                        if let Some(x) = x
                            && overlaps(&x, w)
                        {
                            out.push(x);
                        }
                    }
                }
            }
        }
        // an override whose series is not in the file is an event of its own
        for e in events.iter().filter(|e| e.recurrence_id.is_some()) {
            if !events
                .iter()
                .any(|m| m.recurrence_id.is_none() && m.uid == e.uid && m.rrule.is_some())
            {
                let key = e.recurrence_id.map(|r| r.key(tz));
                if let Some(x) = e.occurrence(tz, e.start, key.as_deref())
                    && overlaps(&x, w)
                {
                    out.push(x);
                }
            }
        }
        out.sort_by(|a, b| (bounds(a, tz).0, &a.id).cmp(&(bounds(b, tz).0, &b.id)));
        out
    }
}

/// The instant bounds of an occurrence (dates at midnight in `tz`).
fn bounds(e: &RemoteEvent, tz: Tz) -> (i64, i64) {
    let at = |w: &When| {
        w.ms.or_else(|| {
            w.date
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .map(|d| local_ms(tz, d.and_time(NaiveTime::MIN)))
        })
        .unwrap_or(0)
    };
    (at(&e.start), at(&e.end))
}

fn overlaps(e: &RemoteEvent, w: Window) -> bool {
    // dates: a day either side is enough, the cache filters by local date
    let slack = if e.start.date.is_some() {
        86_400_000
    } else {
        0
    };
    let (s, end) = bounds(e, Tz::UTC);
    s < w.to_ms + slack && end > w.from_ms - slack
}

/// One `VEVENT`, read.
struct Ev {
    uid: String,
    start: Dt,
    end: Option<Dt>,
    duration: Option<Duration>,
    /// `Some(None)`: a rule this reader does not expand.
    rrule: Option<Option<Rule>>,
    exdates: Vec<Dt>,
    recurrence_id: Option<Dt>,
    summary: Option<String>,
    location: Option<String>,
    description: Option<String>,
    url: Option<String>,
    status: Option<String>,
    sequence: Option<i64>,
    organizer: Option<Person>,
    attendees: Vec<Attendee>,
}

fn person(p: &Prop) -> Person {
    let v = p.value.trim();
    let email = v
        .strip_prefix("mailto:")
        .or_else(|| v.strip_prefix("MAILTO:"))
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e.contains('@'));
    Person {
        ext_ref: email
            .is_none()
            .then(|| v.to_string())
            .filter(|s| !s.is_empty()),
        email,
        name: p.param("CN").map(|n| text(n.trim_matches('"'))),
        me: false,
    }
}

fn rsvp(partstat: Option<&str>) -> Option<String> {
    Some(
        match partstat?.to_ascii_uppercase().as_str() {
            "ACCEPTED" => "accepted",
            "DECLINED" => "declined",
            "TENTATIVE" => "tentative",
            "NEEDS-ACTION" => "needs_action",
            _ => return None,
        }
        .into(),
    )
}

impl Ev {
    fn read(props: &[Prop]) -> Option<Self> {
        let get = |n: &str| props.iter().find(|p| p.name == n);
        let start = datetime(get("DTSTART")?)?;
        let summary = get("SUMMARY").map(|p| text(&p.value));
        let uid = get("UID")
            .map(|p| p.value.trim().to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| {
                // no UID: stable from what the event says
                crate::hash(&[
                    "ics",
                    summary.as_deref().unwrap_or(""),
                    &get("DTSTART").map(|p| p.value.clone()).unwrap_or_default(),
                ])
            });
        let organizer = get("ORGANIZER").map(person);
        let attendees = props
            .iter()
            .filter(|p| p.name == "ATTENDEE")
            .map(|p| {
                let who = person(p);
                let organizer = organizer
                    .as_ref()
                    .is_some_and(|o| o.email.is_some() && o.email == who.email);
                Attendee {
                    rsvp: rsvp(p.param("PARTSTAT")),
                    optional: p
                        .param("ROLE")
                        .is_some_and(|r| r.eq_ignore_ascii_case("OPT-PARTICIPANT")),
                    organizer,
                    who,
                }
            })
            .collect();
        Some(Self {
            uid,
            start,
            end: get("DTEND").and_then(datetime),
            duration: get("DURATION").and_then(|p| duration(&p.value)),
            rrule: get("RRULE").map(|p| rule(&p.value)),
            exdates: props
                .iter()
                .filter(|p| p.name == "EXDATE")
                .flat_map(datetimes)
                .collect(),
            recurrence_id: get("RECURRENCE-ID").and_then(datetime),
            summary,
            location: get("LOCATION")
                .map(|p| text(&p.value))
                .filter(|s| !s.trim().is_empty()),
            description: get("DESCRIPTION")
                .map(|p| text(&p.value))
                .filter(|s| !s.trim().is_empty()),
            url: get("URL")
                .map(|p| p.value.trim().to_string())
                .filter(|s| !s.is_empty()),
            status: get("STATUS").and_then(|p| {
                Some(
                    match p.value.trim().to_ascii_uppercase().as_str() {
                        "CONFIRMED" => "confirmed",
                        "TENTATIVE" => "tentative",
                        "CANCELLED" => "cancelled",
                        _ => return None,
                    }
                    .into(),
                )
            }),
            sequence: get("SEQUENCE").and_then(|p| p.value.trim().parse().ok()),
            organizer,
            attendees,
        })
    }

    /// The occurrence starting at `start` (`key`: its original start, for a series).
    fn occurrence(&self, tz: Tz, start: Dt, key: Option<&str>) -> Option<RemoteEvent> {
        // the length comes from this event's own start (an override may be longer)
        let (s, e) = match (self.start, start) {
            (Dt::Date(d0), Dt::Date(d)) => {
                let days = match self.end {
                    Some(Dt::Date(e)) => (e - d0).num_days().max(1),
                    _ => self.duration.map(|x| x.num_days().max(1)).unwrap_or(1),
                };
                let end = d.checked_add_days(Days::new(days as u64))?;
                (
                    When::day(&d.format("%Y-%m-%d").to_string()),
                    When::day(&end.format("%Y-%m-%d").to_string()),
                )
            }
            (_, Dt::Date(_)) | (Dt::Date(_), _) => return None,
            (s0, s) => {
                let len = match (self.end, self.duration) {
                    (Some(e), _) => e.ms(tz) - s0.ms(tz),
                    (None, Some(d)) => d.num_milliseconds(),
                    (None, None) => 0,
                }
                .max(0);
                let at = s.ms(tz);
                let zone = match s {
                    Dt::Local(_, Some(z)) => Some(z.name().to_string()),
                    _ => None,
                };
                (
                    When {
                        ms: Some(at),
                        tz: zone.clone(),
                        ..Default::default()
                    },
                    When {
                        ms: Some(at + len),
                        tz: zone,
                        ..Default::default()
                    },
                )
            }
        };
        Some(RemoteEvent {
            id: match key {
                Some(k) => format!("{}@{k}", self.uid),
                None => self.uid.clone(),
            },
            series_id: key.map(|_| self.uid.clone()),
            original_start: key.map(str::to_string),
            ical_uid: Some(self.uid.clone()),
            sequence: self.sequence,
            etag: None,
            title: self.summary.clone(),
            location: self.location.clone(),
            meeting_url: self
                .url
                .clone()
                .filter(|u| crate::sync::meeting_link(u).is_some()),
            description: self.description.clone(),
            start: s,
            end: e,
            status: self.status.clone(),
            my_rsvp: None,
            organizer: self.organizer.clone(),
            attendees: self.attendees.clone(),
            can_modify: false,
        })
    }

    /// The starts of a recurring event up to the end of `w` (`None` rule: the first only).
    fn starts(&self, r: Option<&Rule>, tz: Tz, w: Window) -> Vec<Dt> {
        let Some(r) = r else {
            return vec![self.start];
        };
        let (d0, time, zone) = match self.start {
            Dt::Date(d) => (d, None, None),
            Dt::Utc(t) => (t.date_naive(), Some(t.time()), None),
            Dt::Local(t, z) => (t.date(), Some(t.time()), Some(z)),
        };
        let make = |d: NaiveDate| match (self.start, time, zone) {
            (Dt::Date(_), _, _) => Dt::Date(d),
            (Dt::Utc(_), Some(t), _) => Dt::Utc(Utc.from_utc_datetime(&d.and_time(t))),
            (_, Some(t), Some(z)) => Dt::Local(d.and_time(t), z),
            _ => Dt::Date(d),
        };
        let until = r.until.map(|u| match u {
            // a date UNTIL includes that whole day
            Dt::Date(d) => Dt::Date(d + Duration::days(1)).ms(tz) - 1,
            u => u.ms(tz),
        });
        let first = self.start.ms(tz);
        // DTSTART is always the first occurrence, whether or not the rule selects it (RFC 5545 §3.8.5.3)
        let mut out = vec![self.start];
        let mut n = 1usize;
        'periods: for k in 0..MAX_PERIODS as u32 {
            for d in period(r, d0, k) {
                if d < d0 {
                    continue;
                }
                let s = make(d);
                let at = s.ms(tz);
                if at <= first {
                    continue;
                }
                if until.is_some_and(|u| at > u) || at >= w.to_ms {
                    break 'periods;
                }
                n += 1;
                if r.count.is_some_and(|c| n > c) {
                    break 'periods;
                }
                out.push(s);
                if out.len() >= MAX_OCCURRENCES * 4 {
                    break 'periods;
                }
            }
        }
        // only the ones that can reach the window (the count above ran over all of them)
        let len = self
            .end
            .map(|e| e.ms(tz) - first)
            .or(self.duration.map(|d| d.num_milliseconds()))
            .unwrap_or(86_400_000)
            .max(86_400_000);
        out.retain(|s| s.ms(tz) + len > w.from_ms);
        out.truncate(MAX_OCCURRENCES);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: Tz = chrono_tz::Asia::Shanghai;

    fn ms(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis()
    }
    fn win(a: &str, b: &str) -> Window {
        Window {
            from_ms: ms(a),
            to_ms: ms(b),
        }
    }
    fn cal(events: &str) -> Calendar {
        Calendar::parse(&format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team\r\n{events}END:VCALENDAR\r\n"
        ))
        .unwrap()
    }

    #[test]
    fn unfolds_escapes_and_reads_people() {
        let c = cal(
            "BEGIN:VEVENT\r\nUID:a1\r\nSUMMARY:Plan\\, review\r\n  and ship\r\nDTSTART;TZID=Asia/Shanghai:20261118T100000\r\nDTEND;TZID=Asia/Shanghai:20261118T110000\r\nLOCATION:Room 3\r\nORGANIZER;CN=\"Lin, Ada\":mailto:Ada@Example.com\r\nATTENDEE;CN=Bo;PARTSTAT=ACCEPTED:mailto:bo@example.com\r\nATTENDEE;ROLE=OPT-PARTICIPANT:mailto:cy@example.com\r\nEND:VEVENT\r\n",
        );
        assert_eq!(c.name.as_deref(), Some("Team"));
        let xs = c.occurrences(win("2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z"), TZ);
        assert_eq!(xs.len(), 1);
        let e = &xs[0];
        assert_eq!(e.title.as_deref(), Some("Plan, review and ship"));
        assert_eq!(e.start.ms, Some(ms("2026-11-18T02:00:00Z")));
        assert_eq!(e.end.ms, Some(ms("2026-11-18T03:00:00Z")));
        let o = e.organizer.as_ref().unwrap();
        assert_eq!(
            (o.email.as_deref(), o.name.as_deref()),
            (Some("ada@example.com"), Some("Lin, Ada"))
        );
        assert_eq!(e.attendees[0].rsvp.as_deref(), Some("accepted"));
        assert!(e.attendees[1].optional);
    }

    #[test]
    fn weekly_rule_with_exdate_override_and_count() {
        let c = cal(
            "BEGIN:VEVENT\r\nUID:w1\r\nSUMMARY:Weekly\r\nDTSTART:20261102T020000Z\r\nDTEND:20261102T030000Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=6\r\nEXDATE:20261104T020000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:w1\r\nRECURRENCE-ID:20261109T020000Z\r\nSUMMARY:Weekly (moved)\r\nDTSTART:20261109T060000Z\r\nDTEND:20261109T070000Z\r\nEND:VEVENT\r\n",
        );
        let xs = c.occurrences(win("2026-11-01T00:00:00Z", "2027-01-01T00:00:00Z"), TZ);
        // Mo 2, (We 4 excluded), Mo 9 moved, We 11, Mo 16, We 18 — six counted, five kept
        let starts: Vec<_> = xs.iter().map(|e| e.start.ms.unwrap()).collect();
        assert_eq!(
            starts,
            [
                ms("2026-11-02T02:00:00Z"),
                ms("2026-11-09T06:00:00Z"),
                ms("2026-11-11T02:00:00Z"),
                ms("2026-11-16T02:00:00Z"),
                ms("2026-11-18T02:00:00Z"),
            ]
        );
        assert_eq!(xs[1].title.as_deref(), Some("Weekly (moved)"));
        assert_eq!(xs[1].id, "w1@20261109T020000Z");
        assert_eq!(xs[1].original_start.as_deref(), Some("20261109T020000Z"));
        // the window cuts, ids stay
        let later = c.occurrences(win("2026-11-15T00:00:00Z", "2027-01-01T00:00:00Z"), TZ);
        assert_eq!(later.len(), 2);
        assert_eq!(later[0].id, "w1@20261116T020000Z");
    }

    #[test]
    fn local_times_keep_wall_clock_across_dst() {
        let c = cal(
            "BEGIN:VEVENT\r\nUID:d1\r\nDTSTART;TZID=America/New_York:20261030T090000\r\nDURATION:PT30M\r\nRRULE:FREQ=DAILY;UNTIL=20261103T235959Z\r\nEND:VEVENT\r\n",
        );
        let xs = c.occurrences(win("2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z"), TZ);
        assert_eq!(xs.len(), 5);
        assert_eq!(xs[0].start.ms, Some(ms("2026-10-30T13:00:00Z")));
        // after the change back to standard time (Nov 1) 09:00 is 14:00 UTC
        assert_eq!(xs[4].start.ms, Some(ms("2026-11-03T14:00:00Z")));
        assert_eq!(xs[4].end.ms, Some(ms("2026-11-03T14:30:00Z")));
        assert_eq!(xs[0].start.tz.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn all_day_monthly_and_unknown_rules() {
        let c = cal(
            "BEGIN:VEVENT\r\nUID:m1\r\nSUMMARY:Pay\r\nDTSTART;VALUE=DATE:20260131\r\nRRULE:FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=3\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:x1\r\nDTSTART:20261105T010000Z\r\nRRULE:FREQ=MONTHLY;BYSETPOS=-1;BYDAY=MO,TU\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:t1\r\nDTSTART:20261105T010000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=2TU;COUNT=2\r\nEND:VEVENT\r\n",
        );
        let xs = c.occurrences(win("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z"), TZ);
        let pay: Vec<_> = xs
            .iter()
            .filter(|e| e.ical_uid.as_deref() == Some("m1"))
            .map(|e| (e.start.date.clone().unwrap(), e.end.date.clone().unwrap()))
            .collect();
        assert_eq!(
            pay,
            [
                ("2026-01-31".into(), "2026-02-01".into()),
                ("2026-02-28".into(), "2026-03-01".into()),
                ("2026-03-31".into(), "2026-04-01".into())
            ]
        );
        // BYSETPOS is not expanded: the first occurrence only
        assert_eq!(
            xs.iter()
                .filter(|e| e.ical_uid.as_deref() == Some("x1"))
                .count(),
            1
        );
        let tue: Vec<_> = xs
            .iter()
            .filter(|e| e.ical_uid.as_deref() == Some("t1"))
            .map(|e| e.start.ms.unwrap())
            .collect();
        // DTSTART (Nov 5) is not a second Tuesday, yet it is the first of the two
        assert_eq!(
            tue,
            [ms("2026-11-05T01:00:00Z"), ms("2026-11-10T01:00:00Z")]
        );
    }

    #[test]
    fn refuses_what_is_not_a_calendar_and_maps_windows_zones() {
        assert!(Calendar::parse("hello").is_err());
        assert_eq!(zone("China Standard Time"), Some(chrono_tz::Asia::Shanghai));
        assert_eq!(zone("\"/Europe/Paris\""), Some(chrono_tz::Europe::Paris));
        assert_eq!(duration("P1DT2H30M"), Some(Duration::minutes(26 * 60 + 30)));
        assert_eq!(duration("-PT15M"), Some(Duration::minutes(-15)));
    }
}

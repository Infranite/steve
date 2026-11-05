//! 5-field cron: validation, next run in a time zone, minimum interval, Chinese description.
//!
//! We validate the syntax ourselves (so the error names the offending
//! fragment and we only accept the subset the spec allows) and let `croner` walk the wall clock.
//! The wall-clock result is then mapped to an instant with our own DST policy, because croner's
//! policy depends on whether a pattern is "fixed time" and the spec wants one rule for all:
//! a skipped local time runs at the first minute that exists after it, a repeated local time
//! runs only on its first pass.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};

/// Workspace default (`SETTINGS.tz.def`).
pub const DEFAULT_TZ: &str = "Asia/Taipei";

/// Values offered by the `tz` setting (`SETTINGS.tz.values`). Tasks may still store any IANA name.
pub const TZ_CHOICES: [&str; 7] = [
    "Asia/Taipei",
    "Asia/Shanghai",
    "Asia/Tokyo",
    "UTC",
    "Europe/London",
    "America/Los_Angeles",
    "America/New_York",
];

/// Shortest allowed gap between two runs.
pub const MIN_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// How many upcoming runs the minimum-interval check looks at.
const MIN_INTERVAL_WINDOW: usize = 20;

/// Bound on wall-clock candidates skipped while resolving one next run (only DST folds skip any).
const MAX_SKIPPED_CANDIDATES: usize = 10_000;

/// Errors shown to the user as `400 <message>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CronError {
    #[error("不支持这种 cron 写法：{0}")]
    Unsupported(String),
    #[error("最短间隔 5 分钟")]
    TooFrequent,
    #[error("这个 cron 不会运行：{0}")]
    Never(String),
    #[error("不支持的时区：{0}")]
    UnknownTz(String),
}

/// Parses an IANA time zone name.
pub fn parse_tz(name: &str) -> Result<Tz, CronError> {
    name.trim()
        .parse::<Tz>()
        .map_err(|_| CronError::UnknownTz(name.to_string()))
}

/// What one cron field allows, kept for `describe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shape {
    /// `*` (or `*/1`).
    Any,
    /// `*/n`, n > 1.
    Step(u8),
    /// Day of month `L`.
    Last,
    /// Explicit values (lists, ranges, stepped ranges), sorted; weekday 7 folded into 0.
    Set(BTreeSet<u8>),
}

impl Shape {
    pub(crate) fn single(&self) -> Option<u8> {
        match self {
            Shape::Set(s) if s.len() == 1 => s.iter().next().copied(),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Minute,
    Hour,
    Dom,
    Month,
    Dow,
}

impl Kind {
    fn bounds(self) -> (u8, u8) {
        match self {
            Kind::Minute => (0, 59),
            Kind::Hour => (0, 23),
            Kind::Dom => (1, 31),
            Kind::Month => (1, 12),
            Kind::Dow => (0, 7),
        }
    }

    fn names(self) -> &'static [&'static str] {
        match self {
            Kind::Month => &[
                "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
            ],
            Kind::Dow => &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"],
            _ => &[],
        }
    }

    /// Value of the first name in `names()`.
    fn name_base(self) -> u8 {
        if matches!(self, Kind::Month) { 1 } else { 0 }
    }
}

const KINDS: [Kind; 5] = [Kind::Minute, Kind::Hour, Kind::Dom, Kind::Month, Kind::Dow];

/// A validated 5-field cron expression.
#[derive(Debug, Clone)]
pub struct CronExpr {
    expr: String,
    shapes: [Shape; 5],
    cron: Cron,
}

impl PartialEq for CronExpr {
    fn eq(&self, other: &Self) -> bool {
        self.expr == other.expr
    }
}

impl CronExpr {
    /// Checks the syntax and normalises it: aliases expanded to 5 fields, month and weekday
    /// names turned into numbers, single spaces. Does not check frequency; see [`validate`].
    pub fn parse(input: &str) -> Result<Self, CronError> {
        let trimmed = input.trim();
        let expanded = match trimmed.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *",
            "@daily" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            _ if trimmed.starts_with('@') => return Err(CronError::Unsupported(trimmed.into())),
            _ => trimmed,
        };
        let parts: Vec<&str> = expanded.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(CronError::Unsupported(trimmed.into()));
        }
        let mut canon = Vec::with_capacity(5);
        let mut shapes = Vec::with_capacity(5);
        for (part, kind) in parts.iter().zip(KINDS) {
            let (c, s) = parse_field(part, kind)?;
            canon.push(c);
            shapes.push(s);
        }
        let expr = canon.join(" ");
        // Vixie "or" semantics when both day fields are restricted, stated explicitly.
        let cron = CronParser::builder()
            .seconds(Seconds::Disallowed)
            .year(Year::Disallowed)
            .dom_and_dow(false)
            .build()
            .parse(&expr)
            .map_err(|_| CronError::Unsupported(trimmed.into()))?;
        let shapes: [Shape; 5] = shapes
            .try_into()
            .map_err(|_| CronError::Unsupported(trimmed.into()))?;
        Ok(Self { expr, shapes, cron })
    }

    /// The normalised expression, as stored in `flow_triggers.cron`.
    pub fn as_str(&self) -> &str {
        &self.expr
    }

    /// First run strictly after `after`, computed on the wall clock of `tz`.
    pub fn next_after(&self, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut wall = after.with_timezone(&tz).naive_local();
        for _ in 0..MAX_SKIPPED_CANDIDATES {
            let candidate = self.cron.find_next_occurrence(&wall, false).ok()?;
            let at = resolve_wall(tz, candidate)?;
            // Candidates that land at or before `after` are the second pass of a repeated hour
            // (already run on the first pass) or extra minutes of a skipped hour collapsed onto
            // the same instant: both run at most once.
            if at > after {
                return Some(at);
            }
            wall = candidate;
        }
        None
    }

    /// `next_after` in UTC milliseconds, the unit of `flow_triggers.next_at`.
    pub fn next_after_ms(&self, tz: Tz, after_ms: i64) -> Option<i64> {
        let after = DateTime::<Utc>::from_timestamp_millis(after_ms)?;
        self.next_after(tz, after).map(|t| t.timestamp_millis())
    }

    /// The next `n` runs after `after`.
    pub fn upcoming(&self, tz: Tz, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
        let mut out = Vec::with_capacity(n);
        let mut cursor = after;
        while out.len() < n {
            match self.next_after(tz, cursor) {
                Some(t) => {
                    out.push(t);
                    cursor = t;
                }
                None => break,
            }
        }
        out
    }

    /// Runs planned in `[from, to]` (both inclusive), counting at most `cap`.
    pub fn count_between(&self, tz: Tz, from: DateTime<Utc>, to: DateTime<Utc>, cap: u32) -> u32 {
        // Runs fall on whole minutes, so one second earlier includes a run exactly at `from`.
        let mut cursor = from - Duration::seconds(1);
        let mut n = 0;
        while n < cap {
            match self.next_after(tz, cursor) {
                Some(t) if t <= to => {
                    n += 1;
                    cursor = t;
                }
                _ => break,
            }
        }
        n
    }

    /// Rejects expressions that never run or that run twice within 5 minutes among the next 20 runs.
    pub fn check_frequency(&self, tz: Tz, now: DateTime<Utc>) -> Result<(), CronError> {
        let runs = self.upcoming(tz, now, MIN_INTERVAL_WINDOW);
        if runs.is_empty() {
            return Err(CronError::Never(self.expr.clone()));
        }
        if runs
            .windows(2)
            .any(|w| (w[1] - w[0]).num_milliseconds() < MIN_INTERVAL_MS)
        {
            return Err(CronError::TooFrequent);
        }
        Ok(())
    }

    /// Chinese description for the UI, or `按 cron「…」` when the
    /// combination has no short wording.
    pub fn describe(&self) -> String {
        describe_shapes(&self.shapes).unwrap_or_else(|| format!("按 cron「{}」", self.expr))
    }
}

/// Parse + frequency check: what `POST /schedules` and `/schedules/parse` accept.
pub fn validate(input: &str, tz: Tz, now: DateTime<Utc>) -> Result<CronExpr, CronError> {
    let expr = CronExpr::parse(input)?;
    expr.check_frequency(tz, now)?;
    Ok(expr)
}

/// Maps a local wall-clock time to the instant it runs at.
fn resolve_wall(tz: Tz, wall: NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&wall) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        // Repeated hour: first pass only.
        LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        // Skipped hour: the first minute that exists after it. Real gaps are at most a few hours.
        LocalResult::None => {
            let mut probe = wall;
            for _ in 0..(24 * 60) {
                probe += Duration::minutes(1);
                if let Some(t) = tz.from_local_datetime(&probe).earliest() {
                    return Some(t.with_timezone(&Utc));
                }
            }
            None
        }
    }
}

fn parse_field(raw: &str, kind: Kind) -> Result<(String, Shape), CronError> {
    let bad = || CronError::Unsupported(raw.to_string());
    let token = raw.to_ascii_uppercase();
    let (min, max) = kind.bounds();
    if token == "*" {
        return Ok(("*".into(), Shape::Any));
    }
    if matches!(kind, Kind::Dom) && token == "L" {
        return Ok(("L".into(), Shape::Last));
    }
    if let Some(step) = token.strip_prefix("*/") {
        let n = parse_num(step).ok_or_else(bad)?;
        if n == 0 || n > max {
            return Err(bad());
        }
        let shape = if n == 1 { Shape::Any } else { Shape::Step(n) };
        return Ok((format!("*/{n}"), shape));
    }
    let mut values = BTreeSet::new();
    let mut canon = Vec::new();
    for item in token.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => (r, Some(parse_num(s).filter(|&n| n > 0).ok_or_else(bad)?)),
            None => (item, None),
        };
        let (lo, hi) = match range.split_once('-') {
            Some((a, b)) => (
                value(a, kind).ok_or_else(bad)?,
                value(b, kind).ok_or_else(bad)?,
            ),
            // `a/n` (start then step) is non-standard; require a range.
            None if step.is_some() => return Err(bad()),
            None => {
                let v = value(range, kind).ok_or_else(bad)?;
                (v, v)
            }
        };
        if lo < min || hi > max || lo > hi {
            return Err(bad());
        }
        let step = step.unwrap_or(1);
        let mut v = lo;
        while v <= hi {
            values.insert(if matches!(kind, Kind::Dow) && v == 7 {
                0
            } else {
                v
            });
            v = match v.checked_add(step) {
                Some(n) => n,
                None => break,
            };
        }
        let mut c = if lo == hi {
            lo.to_string()
        } else {
            format!("{lo}-{hi}")
        };
        if step > 1 {
            let _ = write!(c, "/{step}");
        }
        canon.push(c);
    }
    Ok((canon.join(","), Shape::Set(values)))
}

fn parse_num(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn value(s: &str, kind: Kind) -> Option<u8> {
    parse_num(s).or_else(|| {
        kind.names()
            .iter()
            .position(|n| *n == s)
            .and_then(|i| u8::try_from(i).ok())
            .map(|i| i + kind.name_base())
    })
}

// ---- describe ----

const WEEKDAY_ZH: [&str; 7] = ["日", "一", "二", "三", "四", "五", "六"];

fn describe_shapes(s: &[Shape; 5]) -> Option<String> {
    let [minute, hour, dom, month, dow] = s;
    if *month != Shape::Any {
        return None;
    }
    let day = day_phrase(dom, dow)?;
    let (time, interval) = time_phrase(minute, hour)?;
    Some(match (day.as_str(), interval) {
        ("每天", true) => time,
        _ => format!("{day} {time}"),
    })
}

fn day_phrase(dom: &Shape, dow: &Shape) -> Option<String> {
    let all_week: BTreeSet<u8> = (0..7).collect();
    match (dom, dow) {
        (Shape::Any, Shape::Any) => Some("每天".into()),
        (Shape::Any, Shape::Set(d)) if *d == all_week => Some("每天".into()),
        (Shape::Any, Shape::Set(d)) if *d == (1..=5).collect() => Some("工作日".into()),
        (Shape::Any, Shape::Set(d)) if *d == BTreeSet::from([0, 6]) => Some("周末".into()),
        (Shape::Any, Shape::Set(d)) => {
            // Monday first, Sunday last, as people say it.
            let mut order: Vec<u8> = d.iter().copied().filter(|&v| v != 0).collect();
            if d.contains(&0) {
                order.push(0);
            }
            let names: Vec<&str> = order.iter().map(|&v| WEEKDAY_ZH[usize::from(v)]).collect();
            Some(format!("每周{}", names.join("、")))
        }
        (Shape::Last, Shape::Any) => Some("每月最后一天".into()),
        (Shape::Set(d), Shape::Any) if d.len() <= 6 => {
            let days: Vec<String> = d.iter().map(u8::to_string).collect();
            Some(format!("每月 {} 日", days.join("、")))
        }
        _ => None,
    }
}

/// Returns the wording and whether it is an interval (which reads fine without "每天").
fn time_phrase(minute: &Shape, hour: &Shape) -> Option<(String, bool)> {
    match (minute, hour) {
        (Shape::Any, Shape::Any) => Some(("每分钟".into(), true)),
        (Shape::Step(n), Shape::Any) => Some((format!("每 {n} 分钟"), true)),
        (m, Shape::Any) => {
            let m = m.single()?;
            Some((
                if m == 0 {
                    "每小时（整点）".into()
                } else {
                    format!("每小时第 {m} 分")
                },
                true,
            ))
        }
        (m, Shape::Step(n)) => {
            let m = m.single()?;
            Some((
                if m == 0 {
                    format!("每 {n} 小时（整点）")
                } else {
                    format!("每 {n} 小时（第 {m} 分）")
                },
                true,
            ))
        }
        (m, Shape::Set(hours)) if hours.len() <= 6 => {
            let m = m.single()?;
            let times: Vec<String> = hours.iter().map(|h| format!("{h:02}:{m:02}")).collect();
            Some((times.join("、"), false))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn local(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        tz.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap()
    }

    fn taipei() -> Tz {
        parse_tz(DEFAULT_TZ).unwrap()
    }

    #[test]
    fn accepts_the_allowed_syntax() {
        for (input, canon) in [
            ("30 8 * * *", "30 8 * * *"),
            ("  0   9 * *   1-5 ", "0 9 * * 1-5"),
            ("0 9 * * mon-fri", "0 9 * * 1-5"),
            ("0 9 * * SUN,sat", "0 9 * * 0,6"),
            ("0 10 1 jan,Jul *", "0 10 1 1,7 *"),
            ("*/15 * * * *", "*/15 * * * *"),
            ("0 0-12/3 * * *", "0 0-12/3 * * *"),
            ("0 18 L * *", "0 18 L * *"),
            ("0 9 * * 7", "0 9 * * 7"),
            ("@hourly", "0 * * * *"),
            ("@DAILY", "0 0 * * *"),
            ("@weekly", "0 0 * * 0"),
            ("@monthly", "0 0 1 * *"),
        ] {
            let e = CronExpr::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(e.as_str(), canon, "{input}");
        }
    }

    #[test]
    fn rejects_the_unsupported_syntax_naming_the_fragment() {
        for (input, frag) in [
            ("0 0 9 * * *", "0 0 9 * * *"),
            ("0 0 9 * * * 2026", "0 0 9 * * * 2026"),
            ("0 9 * *", "0 9 * *"),
            ("0 9 * * 5#2", "5#2"),
            ("0 9 15W * *", "15W"),
            ("0 9 ? * 1", "?"),
            ("0 9 * * 5L", "5L"),
            ("0 9 * * L", "L"),
            ("@reboot", "@reboot"),
            ("@yearly", "@yearly"),
            ("60 9 * * *", "60"),
            ("0 24 * * *", "24"),
            ("0 9 0 * *", "0"),
            ("0 9 * 13 *", "13"),
            ("0 9 * * 8", "8"),
            ("0 9-5 * * *", "9-5"),
            ("0/10 * * * *", "0/10"),
            ("/10 * * * *", "/10"),
            ("*/0 * * * *", "*/0"),
            ("0 9 * * MONDAY", "MONDAY"),
            ("", ""),
        ] {
            assert_eq!(
                CronExpr::parse(input),
                Err(CronError::Unsupported(frag.into())),
                "{input}"
            );
        }
        assert_eq!(
            CronExpr::parse("0 9 * * 5#2").unwrap_err().to_string(),
            "不支持这种 cron 写法：5#2"
        );
    }

    #[test]
    fn minimum_interval_is_five_minutes() {
        let now = utc(2026, 11, 1, 0, 0);
        let tz = taipei();
        for bad in ["*/2 * * * *", "* * * * *", "0,3 9 * * *", "*/4 * * * *"] {
            assert_eq!(validate(bad, tz, now), Err(CronError::TooFrequent), "{bad}");
        }
        assert_eq!(
            validate("*/2 * * * *", tz, now).unwrap_err().to_string(),
            "最短间隔 5 分钟"
        );
        for ok in ["*/5 * * * *", "0,5 9 * * *", "0 * * * *", "0 9 * * 1"] {
            assert!(validate(ok, tz, now).is_ok(), "{ok}");
        }
    }

    #[test]
    fn impossible_dates_never_run() {
        let now = utc(2026, 11, 1, 0, 0);
        assert_eq!(
            validate("0 9 31 2 *", taipei(), now),
            Err(CronError::Never("0 9 31 2 *".into()))
        );
    }

    #[test]
    fn next_runs_in_taipei() {
        let tz = taipei();
        let e = CronExpr::parse("30 8 * * *").unwrap();
        // 2026-11-01 00:00 UTC = 08:00 Taipei → 08:30 Taipei same day = 00:30 UTC.
        let now = utc(2026, 11, 1, 0, 0);
        assert_eq!(e.next_after(tz, now), Some(utc(2026, 11, 1, 0, 30)));
        // Exactly at a run: strictly after → the next day.
        assert_eq!(
            e.next_after(tz, utc(2026, 11, 1, 0, 30)),
            Some(utc(2026, 11, 2, 0, 30))
        );
        let ms = utc(2026, 11, 1, 0, 0).timestamp_millis();
        assert_eq!(
            e.next_after_ms(tz, ms),
            Some(utc(2026, 11, 1, 0, 30).timestamp_millis())
        );
    }

    #[test]
    fn weekday_and_last_day_of_month() {
        let tz = taipei();
        // 2026-11-01 is a Sunday.
        let fri = CronExpr::parse("0 9 * * 5").unwrap();
        let next = fri
            .next_after(tz, utc(2026, 11, 1, 0, 0))
            .unwrap()
            .with_timezone(&tz);
        assert_eq!(next, local(tz, 2026, 11, 6, 9, 0));
        let sun = CronExpr::parse("0 9 * * 7").unwrap();
        let next = sun
            .next_after(tz, utc(2026, 10, 31, 0, 0))
            .unwrap()
            .with_timezone(&tz);
        assert_eq!(next, local(tz, 2026, 11, 1, 9, 0));
        let last = CronExpr::parse("0 18 L * *").unwrap();
        let runs: Vec<_> = last
            .upcoming(tz, utc(2027, 1, 1, 0, 0), 3)
            .into_iter()
            .map(|t| t.with_timezone(&tz).date_naive())
            .collect();
        assert_eq!(
            runs,
            [
                NaiveDate::from_ymd_opt(2027, 1, 31).unwrap(),
                NaiveDate::from_ymd_opt(2027, 2, 28).unwrap(),
                NaiveDate::from_ymd_opt(2027, 3, 31).unwrap(),
            ]
        );
    }

    #[test]
    fn day_fields_use_or_semantics() {
        let tz = taipei();
        // Day 15 OR Friday.
        let e = CronExpr::parse("0 9 15 * 5").unwrap();
        let days: Vec<u32> = e
            .upcoming(tz, utc(2026, 11, 1, 0, 0), 4)
            .into_iter()
            .map(|t| chrono::Datelike::day(&t.with_timezone(&tz)))
            .collect();
        assert_eq!(days, [6, 13, 15, 20]);
    }

    #[test]
    fn time_zone_matters() {
        let e = CronExpr::parse("0 9 * * *").unwrap();
        let now = utc(2026, 11, 1, 0, 0);
        let tokyo = parse_tz("Asia/Tokyo").unwrap();
        assert_eq!(e.next_after(taipei(), now), Some(utc(2026, 11, 1, 1, 0)));
        assert_eq!(e.next_after(tokyo, now), Some(utc(2026, 11, 2, 0, 0)));
        assert_eq!(
            parse_tz("Mars/Olympus"),
            Err(CronError::UnknownTz("Mars/Olympus".into()))
        );
        for tz in TZ_CHOICES {
            assert!(parse_tz(tz).is_ok(), "{tz}");
        }
    }

    #[test]
    fn dst_spring_forward_runs_once_at_the_first_existing_minute() {
        // America/Los_Angeles 2027-03-14: 02:00 → 03:00.
        let la = parse_tz("America/Los_Angeles").unwrap();
        let e = CronExpr::parse("30 2 * * *").unwrap();
        let runs: Vec<_> = e
            .upcoming(la, local(la, 2027, 3, 13, 12, 0).with_timezone(&Utc), 3)
            .into_iter()
            .map(|t| t.with_timezone(&la).naive_local().to_string())
            .collect();
        assert_eq!(
            runs,
            [
                "2027-03-14 03:00:00",
                "2027-03-15 02:30:00",
                "2027-03-16 02:30:00"
            ]
        );
        // Non-"fixed" patterns get the same rule (croner alone would skip them).
        let e = CronExpr::parse("30 2,14 * * *").unwrap();
        let runs: Vec<_> = e
            .upcoming(la, local(la, 2027, 3, 14, 0, 0).with_timezone(&Utc), 2)
            .into_iter()
            .map(|t| t.with_timezone(&la).naive_local().to_string())
            .collect();
        assert_eq!(runs, ["2027-03-14 03:00:00", "2027-03-14 14:30:00"]);
        // Every 15 minutes: the four skipped slots collapse onto one run at 03:00.
        let e = CronExpr::parse("*/15 * * * *").unwrap();
        let runs: Vec<_> = e
            .upcoming(la, local(la, 2027, 3, 14, 1, 40).with_timezone(&Utc), 3)
            .into_iter()
            .map(|t| t.with_timezone(&la).naive_local().to_string())
            .collect();
        assert_eq!(
            runs,
            [
                "2027-03-14 01:45:00",
                "2027-03-14 03:00:00",
                "2027-03-14 03:15:00"
            ]
        );
    }

    #[test]
    fn dst_fall_back_runs_only_on_the_first_pass() {
        // America/Los_Angeles 2026-11-01: 02:00 PDT → 01:00 PST, 01:xx happens twice.
        let la = parse_tz("America/Los_Angeles").unwrap();
        let e = CronExpr::parse("30 1 * * *").unwrap();
        let start = local(la, 2026, 10, 31, 12, 0).with_timezone(&Utc);
        let runs = e.upcoming(la, start, 2);
        // 01:30 PDT = 08:30 UTC; the 01:30 PST pass (09:30 UTC) is skipped.
        assert_eq!(runs[0], utc(2026, 11, 1, 8, 30));
        assert_eq!(
            runs[1].with_timezone(&la).naive_local().to_string(),
            "2026-11-02 01:30:00"
        );
        // Asking from inside the second pass does not run it again.
        assert_eq!(e.next_after(la, utc(2026, 11, 1, 9, 10)), Some(runs[1]));
        // Counting across the night sees one run.
        assert_eq!(
            e.count_between(la, start, start + Duration::days(1), 1000),
            1
        );
        // Hourly on that night: 00:00, 01:00 (first pass), 02:00 — three runs, 1 h and 2 h apart.
        let e = CronExpr::parse("0 * * * *").unwrap();
        let runs = e.upcoming(la, utc(2026, 11, 1, 6, 30), 3);
        assert_eq!(
            runs,
            [
                utc(2026, 11, 1, 7, 0),
                utc(2026, 11, 1, 8, 0),
                utc(2026, 11, 1, 10, 0)
            ]
        );
    }

    #[test]
    fn count_between_is_inclusive_and_capped() {
        let tz = taipei();
        let e = CronExpr::parse("30 8 * * *").unwrap();
        let from = local(tz, 2026, 11, 1, 8, 30).with_timezone(&Utc);
        let to = local(tz, 2026, 11, 4, 8, 30).with_timezone(&Utc);
        assert_eq!(e.count_between(tz, from, to, 1000), 4);
        assert_eq!(
            e.count_between(tz, from, to - Duration::minutes(1), 1000),
            3
        );
        assert_eq!(e.count_between(tz, from, to, 2), 2);
        let every5 = CronExpr::parse("*/5 * * * *").unwrap();
        assert_eq!(
            every5.count_between(tz, from, from + Duration::days(30), 1000),
            1000
        );
    }

    #[test]
    fn describe_matches_the_spec() {
        for (cron, text) in [
            ("30 8 * * *", "每天 08:30"),
            ("0 9 * * 1-5", "工作日 09:00"),
            ("0 9 * * 1,2,3,4,5", "工作日 09:00"),
            ("0 9 * * 1,3,5", "每周一、三、五 09:00"),
            ("0 9 * * 5", "每周五 09:00"),
            ("0 9 * * 0,1", "每周一、日 09:00"),
            ("0 9 * * 7", "每周日 09:00"),
            ("0 9 * * 0,6", "周末 09:00"),
            ("0 9 * * 0-6", "每天 09:00"),
            ("0 10 1 * *", "每月 1 日 10:00"),
            ("0 10 1,15 * *", "每月 1、15 日 10:00"),
            ("0 18 L * *", "每月最后一天 18:00"),
            ("0 */2 * * *", "每 2 小时（整点）"),
            ("15 */3 * * *", "每 3 小时（第 15 分）"),
            ("0 */2 * * 1-5", "工作日 每 2 小时（整点）"),
            ("*/15 * * * *", "每 15 分钟"),
            ("0 * * * *", "每小时（整点）"),
            ("20 * * * *", "每小时第 20 分"),
            ("0 9,18 * * *", "每天 09:00、18:00"),
            ("0 0 * * *", "每天 00:00"),
            ("@weekly", "每周日 00:00"),
        ] {
            assert_eq!(CronExpr::parse(cron).unwrap().describe(), text, "{cron}");
        }
        for cron in [
            "0 9 1 1 *",
            "0 9 15 * 5",
            "0,30 9 * * *",
            "0 0-23/2 * * *",
            "*/10 9-17 * * *",
        ] {
            let e = CronExpr::parse(cron).unwrap();
            assert_eq!(e.describe(), format!("按 cron「{}」", e.as_str()), "{cron}");
        }
    }
}

//! Copies flow::cron parsing/calculation (W2b §2), using tz's RFC 5545 rule.
//! Descriptions and setting defaults remain in flow.
use crate::tz::Zone;
use chrono::{DateTime, Duration, Utc};
use croner::{
    Cron,
    parser::{CronParser, Seconds, Year},
};
use std::fmt::Write as _;
/// Shortest allowed gap between two runs.
pub const MIN_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// How many upcoming runs the minimum-interval check looks at.
const MIN_INTERVAL_WINDOW: usize = 20;

/// Bound on wall-clock candidates skipped while resolving one next run (only DST folds skip any).
const MAX_SKIPPED_CANDIDATES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronError {
    Unsupported(String),
    TooFrequent,
    Never(String),
}
impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CronError {}

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
        for (part, kind) in parts.iter().zip(KINDS) {
            let c = parse_field(part, kind)?;
            canon.push(c);
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
        Ok(Self { expr, cron })
    }

    /// The normalised expression, as stored in `flow_triggers.cron`.
    pub fn as_str(&self) -> &str {
        &self.expr
    }

    /// First run strictly after `after`, computed on the wall clock of `tz`.
    pub fn next_after(&self, tz: &Zone, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let mut wall = tz.search_start(after.timestamp_millis()).ok()?;
        let mut best: Option<DateTime<Utc>> = None;
        for _ in 0..MAX_SKIPPED_CANDIDATES {
            let candidate = match self.cron.find_next_occurrence(&wall, false) {
                Ok(c) => c,
                Err(_) => return best,
            };
            if let Some(b) = best {
                if candidate >= tz.local(b.timestamp_millis()).ok()? {
                    return best;
                }
            }
            let at = DateTime::from_timestamp_millis(tz.instant(candidate).ok()?)?;
            if at > after && best.is_none_or(|b| at < b) {
                best = Some(at);
            }
            wall = candidate;
        }
        best
    }

    /// `next_after` in UTC milliseconds, the unit of `flow_triggers.next_at`.
    pub fn next_after_ms(&self, tz: &Zone, after_ms: i64) -> Option<i64> {
        let after = DateTime::<Utc>::from_timestamp_millis(after_ms)?;
        self.next_after(tz, after).map(|t| t.timestamp_millis())
    }

    /// The next `n` runs after `after`.
    pub fn upcoming(&self, tz: &Zone, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
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
    pub fn count_between(
        &self,
        tz: &Zone,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        cap: u32,
    ) -> u32 {
        // Runs fall on whole minutes, so one second earlier includes a run exactly at `from`.
        let Some(mut cursor) = from.checked_sub_signed(Duration::seconds(1)) else {
            return 0;
        };
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
    pub fn check_frequency(&self, tz: &Zone, now: DateTime<Utc>) -> Result<(), CronError> {
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
}

/// Parse + frequency check: what `POST /schedules` and `/schedules/parse` accept.
pub fn validate(input: &str, tz: &Zone, now: DateTime<Utc>) -> Result<CronExpr, CronError> {
    let expr = CronExpr::parse(input)?;
    expr.check_frequency(tz, now)?;
    Ok(expr)
}

fn parse_field(raw: &str, kind: Kind) -> Result<String, CronError> {
    let bad = || CronError::Unsupported(raw.to_string());
    let token = raw.to_ascii_uppercase();
    let (min, max) = kind.bounds();
    if token == "*" {
        return Ok("*".into());
    }
    if matches!(kind, Kind::Dom) && token == "L" {
        return Ok("L".into());
    }
    if let Some(step) = token.strip_prefix("*/") {
        let n = parse_num(step).ok_or_else(bad)?;
        if n == 0 || n > max {
            return Err(bad());
        }
        return Ok(format!("*/{n}"));
    }
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
    Ok(canon.join(","))
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

#[derive(Debug, Clone)]
pub enum Schedule {
    At(i64),
    Every(std::time::Duration),
    Cron(CronExpr, Zone),
}
impl Schedule {
    /// Every is anchored to Unix epoch; zero/submillisecond periods never run.
    pub fn next_after(&self, ms: i64) -> Option<i64> {
        match self {
            Self::At(at) => (*at > ms).then_some(*at),
            Self::Every(d) => {
                let n = i64::try_from(d.as_millis()).ok().filter(|n| *n > 0)?;
                ms.div_euclid(n).checked_add(1)?.checked_mul(n)
            }
            Self::Cron(c, z) => c.next_after_ms(z, ms),
        }
    }
    /// Missed occurrences in (from,to], capped without iterating Every intervals.
    pub fn missed(&self, from: i64, to: i64, cap: usize) -> usize {
        if to <= from || cap == 0 {
            return 0;
        }
        if let Self::Every(d) = self {
            let Ok(n) = i64::try_from(d.as_millis()) else {
                return 0;
            };
            if n == 0 {
                return 0;
            }
            return (i128::from(to.div_euclid(n)) - i128::from(from.div_euclid(n))).min(cap as i128)
                as usize;
        }
        let (mut cursor, mut n) = (from, 0);
        while n < cap {
            match self.next_after(cursor) {
                Some(t) if t <= to => {
                    n += 1;
                    cursor = t;
                }
                _ => break,
            }
        }
        n
    }
}

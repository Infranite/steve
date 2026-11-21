//! Presentation of runtime cron expressions. Parsing, frequency checks and DST live in runtime.
use std::collections::BTreeSet;
use steve_runtime::schedule::{CronError, CronExpr};
use steve_runtime::tz::Zone;

pub const DEFAULT_TZ: &str = steve_modules::zone::DEFAULT_TZ;
pub const TZ_CHOICES: [&str; 8] = [
    "system",
    "Asia/Taipei",
    "Asia/Shanghai",
    "Asia/Tokyo",
    "UTC",
    "Europe/London",
    "America/Los_Angeles",
    "America/New_York",
];

/// The operator's system choice is resolved here; explicit names use the shared zone parser.
pub fn zone(name: &str) -> Result<Zone, String> {
    if name.trim().is_empty() || name.trim() == "system" {
        return Ok(Zone::system());
    }
    Zone::parse(name).map_err(|_| steve_i18n::t!("flow.cron.unknown_tz", tz = name))
}

/// Localized UI wording; runtime errors themselves carry no product language.
pub fn error(e: &CronError) -> String {
    match e {
        CronError::Unsupported(cron) => steve_i18n::t!("flow.cron.unsupported", cron = cron),
        CronError::TooFrequent => steve_i18n::t!("flow.cron.too_frequent"),
        CronError::Never(cron) => steve_i18n::t!("flow.cron.never", cron = cron),
    }
}

pub trait DescribeCron {
    fn describe(&self) -> String;
    fn describe_zh(&self) -> String;
    fn describe_in(&self, language: &str) -> String;
}
impl DescribeCron for CronExpr {
    fn describe(&self) -> String {
        self.describe_in(steve_i18n::current())
    }
    fn describe_zh(&self) -> String {
        self.describe_in(steve_i18n::ZH_CN)
    }
    fn describe_in(&self, language: &str) -> String {
        let shapes: Option<Vec<_>> = self
            .as_str()
            .split_whitespace()
            .enumerate()
            .map(|(i, f)| shape(f, i == 4))
            .collect();
        let shapes: Option<[Shape; 5]> = shapes.and_then(|s| s.try_into().ok());
        shapes
            .and_then(|s| describe_shapes(&s, language))
            .unwrap_or_else(|| t_in!(language, "flow.cron.raw", cron = self.as_str()))
    }
}

// Interpret the already validated, numeric canonical fields only for wording. No grammar validation.
fn shape(field: &str, weekday: bool) -> Option<Shape> {
    if field == "*" || field == "*/1" {
        return Some(Shape::Any);
    }
    if field == "L" {
        return Some(Shape::Last);
    }
    if let Some(step) = field.strip_prefix("*/") {
        return Some(Shape::Step(step.parse().ok()?));
    }
    let mut values = BTreeSet::new();
    for part in field.split(',') {
        let (range, step) = part.split_once('/').map_or((part, "1"), |p| p);
        let step: usize = step.parse().ok()?;
        let (lo, hi) = range.split_once('-').map_or((range, range), |p| p);
        for v in (lo.parse::<u8>().ok()?..=hi.parse::<u8>().ok()?).step_by(step) {
            values.insert(if weekday && v == 7 { 0 } else { v });
        }
    }
    Some(Shape::Set(values))
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

// ---- describe ----

fn weekday_name(l: &str, v: u8) -> String {
    match v {
        1 => t_in!(l, "flow.cron.weekday_1"),
        2 => t_in!(l, "flow.cron.weekday_2"),
        3 => t_in!(l, "flow.cron.weekday_3"),
        4 => t_in!(l, "flow.cron.weekday_4"),
        5 => t_in!(l, "flow.cron.weekday_5"),
        6 => t_in!(l, "flow.cron.weekday_6"),
        _ => t_in!(l, "flow.cron.weekday_0"),
    }
}

/// The wording in language `l`; the English one starts with a capital.
fn describe_shapes(s: &[Shape; 5], l: &str) -> Option<String> {
    let [minute, hour, dom, month, dow] = s;
    if *month != Shape::Any {
        return None;
    }
    let (day, every_day) = day_phrase(dom, dow, l)?;
    let (time, interval) = time_phrase(minute, hour, l)?;
    let text = match (every_day, interval) {
        (true, true) => time,
        (_, true) => t_in!(l, "flow.cron.day_interval", day = day, time = time),
        (_, false) => t_in!(l, "flow.cron.day_time", day = day, time = time),
    };
    let mut cs = text.chars();
    Some(match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => text,
    })
}

/// The day wording and whether it is "every day".
fn day_phrase(dom: &Shape, dow: &Shape, l: &str) -> Option<(String, bool)> {
    let all_week: BTreeSet<u8> = (0..7).collect();
    let sep = t_in!(l, "flow.cron.list_sep");
    match (dom, dow) {
        (Shape::Any, Shape::Any) => Some((t_in!(l, "flow.cron.every_day"), true)),
        (Shape::Any, Shape::Set(d)) if *d == all_week => {
            Some((t_in!(l, "flow.cron.every_day"), true))
        }
        (Shape::Any, Shape::Set(d)) if *d == (1..=5).collect() => {
            Some((t_in!(l, "flow.cron.workdays"), false))
        }
        (Shape::Any, Shape::Set(d)) if *d == BTreeSet::from([0, 6]) => {
            Some((t_in!(l, "flow.cron.weekends"), false))
        }
        (Shape::Any, Shape::Set(d)) => {
            // Monday first, Sunday last, as people say it.
            let mut order: Vec<u8> = d.iter().copied().filter(|&v| v != 0).collect();
            if d.contains(&0) {
                order.push(0);
            }
            let names: Vec<String> = order.iter().map(|&v| weekday_name(l, v)).collect();
            Some((t_in!(l, "flow.cron.weekly", days = names.join(&sep)), false))
        }
        (Shape::Last, Shape::Any) => Some((t_in!(l, "flow.cron.last_of_month"), false)),
        (Shape::Set(d), Shape::Any) if d.len() <= 6 => {
            let days: Vec<String> = d.iter().map(u8::to_string).collect();
            Some((t_in!(l, "flow.cron.monthly", days = days.join(&sep)), false))
        }
        _ => None,
    }
}

/// Returns the wording and whether it is an interval (which reads fine without "每天").
fn time_phrase(minute: &Shape, hour: &Shape, l: &str) -> Option<(String, bool)> {
    match (minute, hour) {
        (Shape::Any, Shape::Any) => Some((t_in!(l, "flow.cron.every_minute"), true)),
        (Shape::Step(n), Shape::Any) => Some((t_in!(l, "flow.cron.every_n_minutes", n = n), true)),
        (m, Shape::Any) => {
            let m = m.single()?;
            Some((
                if m == 0 {
                    t_in!(l, "flow.cron.hourly")
                } else {
                    t_in!(l, "flow.cron.hourly_at", m = m)
                },
                true,
            ))
        }
        (m, Shape::Step(n)) => {
            let m = m.single()?;
            Some((
                if m == 0 {
                    t_in!(l, "flow.cron.every_n_hours", n = n)
                } else {
                    t_in!(l, "flow.cron.every_n_hours_at", n = n, m = m)
                },
                true,
            ))
        }
        (m, Shape::Set(hours)) if hours.len() <= 6 => {
            let m = m.single()?;
            let times: Vec<String> = hours.iter().map(|h| format!("{h:02}:{m:02}")).collect();
            Some((times.join(&t_in!(l, "flow.cron.list_sep")), false))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn describe_and_errors_in_english() {
        steve_i18n::scope(steve_i18n::EN, async {
            for (cron, text) in [
                ("30 8 * * *", "Every day at 08:30"),
                ("0 9 * * 1-5", "Weekdays at 09:00"),
                ("0 9 * * 1,3,5", "Every Mon, Wed, Fri at 09:00"),
                ("0 9 * * 0,6", "Weekends at 09:00"),
                ("0 10 1,15 * *", "Day 1, 15 of every month at 10:00"),
                ("0 18 L * *", "The last day of every month at 18:00"),
                ("0 */2 * * 1-5", "Weekdays, every 2 hours (on the hour)"),
                ("*/15 * * * *", "Every 15 minutes"),
                ("20 * * * *", "Every hour at minute 20"),
                ("0 9,18 * * *", "Every day at 09:00, 18:00"),
            ] {
                let e = CronExpr::parse(cron).unwrap();
                assert_eq!(e.describe(), text, "{cron}");
            }
            // what is stored stays zh-CN inside an English request
            let e = CronExpr::parse("0 9 * * 1,3,5").unwrap();
            assert_eq!(e.describe_zh(), "每周一、三、五 09:00");
            let raw = CronExpr::parse("0 9 1 1 *").unwrap();
            assert_eq!(raw.describe(), "cron \"0 9 1 1 *\"");
            assert_eq!(raw.describe_zh(), "按 cron「0 9 1 1 *」");
            assert_eq!(
                error(&CronError::TooFrequent),
                "The shortest interval is 5 minutes"
            );
            assert_eq!(
                error(&CronError::Unsupported("5#2".into())),
                "Unsupported cron expression: 5#2"
            );
        })
        .await;
        assert_eq!(error(&CronError::TooFrequent), "最短间隔 5 分钟");
    }
}

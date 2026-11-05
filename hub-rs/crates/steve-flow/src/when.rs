//! Chinese time phrases → cron (`POST /schedules/parse`).
//!
//! Deterministic rules first; when none matches the caller asks the model gateway (fast tier,
//! [`MODEL_TIMEOUT_SECS`], prompt from [`model_prompt`]) and passes its answer to
//! [`accept_model_output`], which validates it like any other cron.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde::Serialize;

use crate::cron::{CronError, CronExpr, validate};

/// Gateway timeout for the model fallback.
pub const MODEL_TIMEOUT_SECS: u64 = 30;

/// `assumed` when the phrase names days but no time of day.
pub const ASSUMED_NINE: &str = "没说几点，按 09:00";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WhenError {
    /// No rule matched (try the model), or the model's answer was unusable (show this).
    #[error("没看懂这个时间，换个说法，例如“每周一 09:00”")]
    NotUnderstood,
    /// Understood, but the result is not allowed (too frequent, unsupported cron…).
    #[error(transparent)]
    Cron(#[from] CronError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Rule,
    Model,
}

/// Response of `POST /schedules/parse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Parsed {
    pub cron: String,
    pub tz: String,
    pub describe: String,
    /// Next three runs, UTC milliseconds.
    pub next: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assumed: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub by: By,
}

/// Output of the rules alone, before validation against a time zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleCron {
    pub cron: String,
    pub assumed: Option<String>,
    pub notes: Vec<String>,
}

/// Rules, then validation. `Err(NotUnderstood)` means "ask the model".
pub fn parse_when(when: &str, tz: Tz, now: DateTime<Utc>) -> Result<Parsed, WhenError> {
    let rule = rule_cron(when)?.ok_or(WhenError::NotUnderstood)?;
    let expr = validate(&rule.cron, tz, now)?;
    Ok(build(expr, tz, now, rule.assumed, rule.notes, By::Rule))
}

/// Validates the model's answer. Anything that is not one valid 5-field cron → `NotUnderstood`;
/// a valid but disallowed cron (e.g. every minute) keeps its specific error.
pub fn accept_model_output(raw: &str, tz: Tz, now: DateTime<Utc>) -> Result<Parsed, WhenError> {
    let candidate = raw
        .lines()
        .map(|l| {
            l.trim()
                .trim_matches(|c| matches!(c, '`' | '"' | '\'' | '「' | '」'))
                .trim()
        })
        .find(|l| CronExpr::parse(l).is_ok())
        .ok_or(WhenError::NotUnderstood)?;
    let expr = validate(candidate, tz, now)?;
    Ok(build(expr, tz, now, None, Vec::new(), By::Model))
}

/// Prompt for the fast-tier fallback: the rule table as examples, one cron string back.
pub fn model_prompt(when: &str) -> String {
    format!(
        "把下面这个说法换成一条 5 段 cron 表达式（分 时 日 月 周；周日是 0；每月最后一天写 L）。\n\
         只输出 cron 本身，不要解释。没说几点按 09:00。\n\
         例子：\n\
         每天 08:30 → 30 8 * * *\n\
         每天下午 3 点 → 0 15 * * *\n\
         工作日 09:30 → 30 9 * * 1-5\n\
         周末 10:00 → 0 10 * * 0,6\n\
         每周一、三、五 09:00 → 0 9 * * 1,3,5\n\
         每月 1 日 10:00 → 0 10 1 * *\n\
         每月最后一天 18:00 → 0 18 L * *\n\
         每隔 2 小时 → 0 */2 * * *\n\
         每隔 15 分钟 → */15 * * * *\n\
         每小时第 10 分 → 10 * * * *\n\
         说法：{when}"
    )
}

fn build(
    expr: CronExpr,
    tz: Tz,
    now: DateTime<Utc>,
    assumed: Option<String>,
    notes: Vec<String>,
    by: By,
) -> Parsed {
    Parsed {
        cron: expr.as_str().to_string(),
        tz: tz.name().to_string(),
        describe: expr.describe(),
        next: expr
            .upcoming(tz, now, 3)
            .iter()
            .map(|t| t.timestamp_millis())
            .collect(),
        assumed,
        notes,
        by,
    }
}

/// The deterministic rules. `Ok(None)` = no rule matched. Raw cron input is checked for
/// syntax only (frequency needs a time zone).
pub fn rule_cron(when: &str) -> Result<Option<RuleCron>, WhenError> {
    let half = half_width(when);
    let trimmed = half.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if looks_like_cron(trimmed) {
        let expr = CronExpr::parse(trimmed)?;
        return Ok(Some(RuleCron {
            cron: expr.as_str().into(),
            assumed: None,
            notes: vec![],
        }));
    }
    let compact: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .trim_end_matches(['。', '.', '!', '！'])
        .to_string();
    let s = arabic_numerals(&compact);
    if let Some(r) = interval(&s)? {
        return Ok(Some(r));
    }
    let Some((days, rest)) = day_part(&s) else {
        return Ok(None);
    };
    let (time, assumed) = if rest.is_empty() {
        ((9, 0), Some(ASSUMED_NINE.to_string()))
    } else {
        match time_of_day(rest) {
            Some(t) => (t, None),
            None => return Ok(None),
        }
    };
    let (h, m) = time;
    let mut notes = Vec::new();
    let (dom, dow) = match days {
        Days::Every => ("*".to_string(), "*".to_string()),
        Days::Week(set) => ("*".to_string(), dow_field(&set)),
        Days::MonthDay(n) => {
            if n > 28 {
                notes.push(format!("有的月份没有 {n} 日，那个月不运行"));
            }
            (n.to_string(), "*".to_string())
        }
        Days::LastOfMonth => ("L".to_string(), "*".to_string()),
    };
    Ok(Some(RuleCron {
        cron: format!("{m} {h} {dom} * {dow}"),
        assumed,
        notes,
    }))
}

/// ASCII-only input with cron characters is treated as a cron expression.
fn looks_like_cron(s: &str) -> bool {
    s.is_ascii() && (s.starts_with('@') || s.split_whitespace().count() >= 3) && !s.contains(':')
}

/// Full-width ASCII (digits, colon, letters…) and the ideographic space to half-width.
fn half_width(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

// ---- Chinese numerals ----

const CN_DIGITS: &str = "零〇一二两三四五六七八九十";

fn cn_digit(c: char) -> Option<u32> {
    Some(match c {
        '零' | '〇' => 0,
        '一' => 1,
        '二' | '两' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        _ => return None,
    })
}

/// Values up to 99: `九`, `十`, `十二`, `二十`, `二十三`, `零五`.
fn cn_number(s: &[char]) -> Option<u32> {
    match s {
        [d] if *d == '十' => Some(10),
        [d] => cn_digit(*d),
        ['十', d] => cn_digit(*d).filter(|&v| v > 0).map(|v| 10 + v),
        ['零' | '〇', d] => cn_digit(*d),
        [a, '十'] => cn_digit(*a).filter(|&v| v > 0).map(|v| v * 10),
        [a, '十', b] => {
            let (a, b) = (cn_digit(*a)?, cn_digit(*b)?);
            (a > 0 && b > 0).then_some(a * 10 + b)
        }
        _ => None,
    }
}

/// Converts Chinese numerals that are quantities (before 点/时/小时/个/分/日/号/钟/刻, or after
/// 点/第) to Arabic digits, leaving weekday names (`周一`, `星期三`) alone.
fn arabic_numerals(s: &str) -> String {
    const UNITS: &[char] = &['点', '时', '小', '个', '分', '日', '号', '钟', '刻'];
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if !CN_DIGITS.contains(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && CN_DIGITS.contains(chars[j]) {
            j += 1;
        }
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(j).copied();
        let mut start = i;
        // `周五九点`: the first character names the weekday.
        if matches!(prev, Some('周' | '期' | '拜')) && "一二三四五六".contains(chars[i]) {
            out.push(chars[i]);
            start += 1;
        }
        let quantity =
            next.is_some_and(|c| UNITS.contains(&c)) || matches!(prev, Some('点' | '第'));
        let hour_ctx = matches!(next, Some('点' | '时'));
        let mut converted = false;
        if quantity && start < j {
            // Longest well-formed suffix; an hour must be ≤ 24 (`周五十点` = Friday 10:00).
            for k in start..j {
                if let Some(v) = cn_number(&chars[k..j]).filter(|&v| !hour_ctx || v <= 24) {
                    out.extend(&chars[start..k]);
                    out.push_str(&v.to_string());
                    converted = true;
                    break;
                }
            }
        }
        if !converted {
            out.extend(&chars[start..j]);
        }
        i = j;
    }
    out
}

// ---- day part ----

enum Days {
    Every,
    Week(BTreeSet<u8>),
    MonthDay(u8),
    LastOfMonth,
}

static MONTH_DAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^每个?月的?(\d{1,2})(?:日|号)").expect("static regex"));
static MONTH_LAST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^每个?月的?(?:最后一天|最后1天|最后一日|月底|月末)").expect("static regex")
});

fn day_part(s: &str) -> Option<(Days, &str)> {
    for p in ["每一天", "每天", "每日", "天天"] {
        if let Some(rest) = s.strip_prefix(p) {
            return Some((Days::Every, rest));
        }
    }
    for p in ["每个工作日", "每工作日", "工作日"] {
        if let Some(rest) = s.strip_prefix(p) {
            return Some((Days::Week((1..=5).collect()), rest));
        }
    }
    for p in ["每个周末", "每周末", "周末"] {
        if let Some(rest) = s.strip_prefix(p) {
            return Some((Days::Week(BTreeSet::from([0, 6])), rest));
        }
    }
    if let Some(m) = MONTH_LAST.find(s) {
        return Some((Days::LastOfMonth, &s[m.end()..]));
    }
    if let Some(c) = MONTH_DAY.captures(s) {
        let n: u8 = c[1].parse().ok()?;
        if !(1..=31).contains(&n) {
            return None;
        }
        return Some((Days::MonthDay(n), &s[c.get(0)?.end()..]));
    }
    let rest = s
        .strip_prefix("每个")
        .or_else(|| s.strip_prefix('每'))
        .unwrap_or(s);
    let rest = strip_week_word(rest)?;
    let (set, rest) = weekdays(rest)?;
    Some((Days::Week(set), rest))
}

fn strip_week_word(s: &str) -> Option<&str> {
    ["星期", "礼拜", "周"]
        .iter()
        .find_map(|w| s.strip_prefix(w))
}

/// `一`..`六` → 1..6, `日`/`天`/`七` → 7 (so `周一到周日` is a plain range).
fn weekday(s: &str) -> Option<(u8, &str)> {
    let c = s.chars().next()?;
    let v = match c {
        '一' => 1,
        '二' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '日' | '天' | '七' => 7,
        _ => return None,
    };
    Some((v, &s[c.len_utf8()..]))
}

/// `一、三、五`, `一和周四`, `一到五`, `五至周日`… Ranges may wrap (`周五到周一`).
fn weekdays(s: &str) -> Option<(BTreeSet<u8>, &str)> {
    const LIST: &[&str] = &["、", ",", "和", "与", "及", "跟", "/"];
    const RANGE: &[&str] = &["到", "至", "~", "-", "—"];
    let (first, mut rest) = weekday(s)?;
    let mut set = BTreeSet::from([first % 7]);
    let mut last = first;
    loop {
        if let Some(r) = LIST.iter().find_map(|p| rest.strip_prefix(p)) {
            let r = strip_week_word(r).unwrap_or(r);
            let (d, r) = weekday(r)?;
            set.insert(d % 7);
            last = d;
            rest = r;
        } else if let Some(r) = RANGE.iter().find_map(|p| rest.strip_prefix(p)) {
            let r = strip_week_word(r).unwrap_or(r);
            let (d, r) = weekday(r)?;
            let mut v = last;
            loop {
                set.insert(v % 7);
                if v == d {
                    break;
                }
                v = v % 7 + 1;
            }
            last = d;
            rest = r;
        } else {
            break;
        }
    }
    Some((set, rest))
}

/// `1-5` and `0,6` stay short; other sets are listed.
fn dow_field(set: &BTreeSet<u8>) -> String {
    if *set == (0..7).collect() {
        return "*".into();
    }
    if *set == (1..=5).collect() {
        return "1-5".into();
    }
    set.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
}

// ---- time of day ----

static TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^的?(?P<p>早上|早晨|清晨|上午|凌晨|中午|下午|傍晚|晚上|夜里|夜间|晚间|晚)?的?(?:(?P<h>\d{1,2}):(?P<m>\d{2})|(?P<h2>\d{1,2})(?:点钟?|时)(?:(?P<half>半)|(?P<whole>整)|(?P<q>[13])刻(?:钟)?|(?P<m2>\d{1,2})(?:分钟?)?)?)$",
    )
    .expect("static regex")
});

/// `08:30`, `早上九点半`, `下午3点`, `晚上12点` (= 00:00), `中午12点` (= 12:00), `9点15分`.
fn time_of_day(s: &str) -> Option<(u32, u32)> {
    let c = TIME.captures(s)?;
    let num = |name: &str| c.name(name).and_then(|m| m.as_str().parse::<u32>().ok());
    let (h, m) = match num("h") {
        Some(h) => (h, num("m")?),
        None => {
            let h = num("h2")?;
            let m = if c.name("half").is_some() {
                30
            } else if c.name("whole").is_some() {
                0
            } else if let Some(q) = num("q") {
                q * 15
            } else {
                num("m2").unwrap_or(0)
            };
            (h, m)
        }
    };
    let h = match c.name("p").map(|p| p.as_str()) {
        Some("下午" | "傍晚") if h < 12 => h + 12,
        Some("晚上" | "夜里" | "夜间" | "晚间" | "晚") if h < 12 => h + 12,
        Some("晚上" | "夜里" | "夜间" | "晚间" | "晚" | "凌晨") if h == 12 => 0,
        Some("中午") if (1..=3).contains(&h) => h + 12,
        _ => h,
    };
    (h <= 23 && m <= 59).then_some((h, m))
}

// ---- intervals ----

static EVERY_HOURS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^每隔?(\d{1,3})个?(?:小时|钟头)$").expect("static regex"));
static EVERY_MINUTES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^每隔?(\d{1,4})分钟?$").expect("static regex"));
static HOURLY_AT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^每个?(?:小时|钟头)的?(?:第(\d{1,2})分钟?|(\d{1,2})分钟?|(整点))$")
        .expect("static regex")
});

fn interval(s: &str) -> Result<Option<RuleCron>, WhenError> {
    let plain = |cron: String, notes: Vec<String>| {
        Ok(Some(RuleCron {
            cron,
            assumed: None,
            notes,
        }))
    };
    if matches!(s, "每小时" | "每个小时" | "每隔1小时" | "每1小时") {
        return plain("0 * * * *".into(), vec![]);
    }
    if matches!(s, "每半小时" | "每隔半小时" | "每半个小时" | "每隔半个小时") {
        return plain("*/30 * * * *".into(), vec![]);
    }
    if let Some(c) = HOURLY_AT.captures(s) {
        let m: u32 = match c.get(1).or(c.get(2)) {
            Some(m) => m.as_str().parse().unwrap_or(60),
            None => 0,
        };
        return if m <= 59 {
            plain(format!("{m} * * * *"), vec![])
        } else {
            Ok(None)
        };
    }
    if let Some(c) = EVERY_HOURS.captures(s) {
        let n: u32 = c[1].parse().unwrap_or(0);
        return every_hours(n);
    }
    if let Some(c) = EVERY_MINUTES.captures(s) {
        let n: u32 = c[1].parse().unwrap_or(0);
        return match n {
            0 => Ok(None),
            1..=4 => Err(CronError::TooFrequent.into()),
            5..=59 => {
                let notes = if 60u32.is_multiple_of(n) {
                    vec![]
                } else {
                    vec!["每小时整点重新起算".to_string()]
                };
                plain(format!("*/{n} * * * *"), notes)
            }
            _ if n.is_multiple_of(60) => every_hours(n / 60),
            _ => Ok(None),
        };
    }
    Ok(None)
}

fn every_hours(n: u32) -> Result<Option<RuleCron>, WhenError> {
    Ok(match n {
        1 => Some(RuleCron {
            cron: "0 * * * *".into(),
            assumed: None,
            notes: vec![],
        }),
        2..=23 => Some(RuleCron {
            cron: format!("0 */{n} * * *"),
            assumed: None,
            notes: if 24u32.is_multiple_of(n) {
                vec![]
            } else {
                vec!["每天 0 点重新起算".to_string()]
            },
        }),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::parse_tz;
    use chrono::TimeZone;

    fn cron_of(when: &str) -> String {
        match rule_cron(when) {
            Ok(Some(r)) => r.cron,
            other => panic!("{when}: {other:?}"),
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap()
    }

    /// The §3.3 table, at least two phrasings per row, plus the extra forms we accept.
    #[test]
    fn rule_table() {
        let cases = [
            // 每天 HH:MM / 早上 / 上午 / 下午 / 晚上 / 半 / 中午 12 点 / 晚上 12 点
            ("每天 08:30", "30 8 * * *"),
            ("每天08:30", "30 8 * * *"),
            ("每天 ０８：３０", "30 8 * * *"),
            ("每天　８点", "0 8 * * *"),
            ("每日 7:05", "5 7 * * *"),
            ("每天早上 8 点", "0 8 * * *"),
            ("每天早上九点半", "30 9 * * *"),
            ("每天上午十点", "0 10 * * *"),
            ("每天下午 3 点", "0 15 * * *"),
            ("每天下午三点半", "30 15 * * *"),
            ("每天晚上 9 点", "0 21 * * *"),
            ("每天晚上十点十五分", "15 22 * * *"),
            ("每天中午 12 点", "0 12 * * *"),
            ("每天中午十二点", "0 12 * * *"),
            ("每天中午1点", "0 13 * * *"),
            ("每天晚上 12 点", "0 0 * * *"),
            ("每天晚上十二点", "0 0 * * *"),
            ("每天凌晨两点", "0 2 * * *"),
            ("每天 23:59", "59 23 * * *"),
            ("每天九点零五", "5 9 * * *"),
            ("每天9点一刻", "15 9 * * *"),
            ("每天9点三刻", "45 9 * * *"),
            ("每天9点整", "0 9 * * *"),
            ("每天9点钟", "0 9 * * *"),
            ("天天 18:00", "0 18 * * *"),
            ("每天的 9 点", "0 9 * * *"),
            // 工作日 / 周一到周五
            ("工作日 09:00", "0 9 * * 1-5"),
            ("工作日下午三点", "0 15 * * 1-5"),
            ("工作日早上九点半", "30 9 * * 1-5"),
            ("每个工作日 8:00", "0 8 * * 1-5"),
            ("周一到周五 09:30", "30 9 * * 1-5"),
            ("星期一至星期五 早上8点", "0 8 * * 1-5"),
            ("每周一到五 10:00", "0 10 * * 1-5"),
            // 周末
            ("周末 10:00", "0 10 * * 0,6"),
            ("每个周末上午十点", "0 10 * * 0,6"),
            // 每周 / 每星期 / 每礼拜，单个和多个
            ("每周一 09:00", "0 9 * * 1"),
            ("每周五 9 点", "0 9 * * 5"),
            ("每周五九点", "0 9 * * 5"),
            ("每周五十点", "0 10 * * 5"),
            ("每周一十点", "0 10 * * 1"),
            ("每周一、三、五 09:00", "0 9 * * 1,3,5"),
            ("每周一,三,五 9:00", "0 9 * * 1,3,5"),
            ("每周一和周四 下午2点", "0 14 * * 1,4"),
            ("每星期二 08:00", "0 8 * * 2"),
            ("每星期三和星期六晚上8点", "0 20 * * 3,6"),
            ("每礼拜天 10:00", "0 10 * * 0"),
            ("每周日 晚上9点", "0 21 * * 0"),
            ("周五 17:00", "0 17 * * 5"),
            ("每周二到周四 9:00", "0 9 * * 2,3,4"),
            ("每周五到周一 9:00", "0 9 * * 0,1,5,6"),
            ("每周一到周日 9:00", "0 9 * * *"),
            // 每月 N 日 / 号
            ("每月 1 日 10:00", "0 10 1 * *"),
            ("每月一号上午十点", "0 10 1 * *"),
            ("每个月15号 9:00", "0 9 15 * *"),
            ("每月二十八日 9:00", "0 9 28 * *"),
            // 每月最后一天
            ("每月最后一天 18:00", "0 18 L * *"),
            ("每个月月底 下午六点", "0 18 L * *"),
            ("每月最后一天", "0 9 L * *"),
            // 每隔 N 小时 / 每 N 小时
            ("每隔两小时", "0 */2 * * *"),
            ("每隔 3 小时", "0 */3 * * *"),
            ("每 4 小时", "0 */4 * * *"),
            ("每两个小时", "0 */2 * * *"),
            ("每隔1小时", "0 * * * *"),
            ("每小时", "0 * * * *"),
            ("每隔半小时", "*/30 * * * *"),
            // 每隔 N 分钟
            ("每隔 15 分钟", "*/15 * * * *"),
            ("每隔十分钟", "*/10 * * * *"),
            ("每30分钟", "*/30 * * * *"),
            ("每隔120分钟", "0 */2 * * *"),
            ("每隔60分钟", "0 * * * *"),
            // 每小时第 M 分
            ("每小时第 15 分", "15 * * * *"),
            ("每小时 5 分", "5 * * * *"),
            ("每个小时第十分钟", "10 * * * *"),
            ("每小时整点", "0 * * * *"),
            // 直接写 cron
            ("0 9 * * 1", "0 9 * * 1"),
            ("30 8 * * MON-FRI", "30 8 * * 1-5"),
            ("@daily", "0 0 * * *"),
            ("０ ９ ＊ ＊ １", "0 9 * * 1"),
        ];
        for (when, cron) in cases {
            assert_eq!(cron_of(when), cron, "{when}");
        }
    }

    #[test]
    fn missing_time_assumes_nine() {
        for (when, cron) in [
            ("每天", "0 9 * * *"),
            ("工作日", "0 9 * * 1-5"),
            ("每周五", "0 9 * * 5"),
            ("每月 1 号", "0 9 1 * *"),
        ] {
            let r = rule_cron(when).unwrap().unwrap();
            assert_eq!(r.cron, cron, "{when}");
            assert_eq!(r.assumed.as_deref(), Some(ASSUMED_NINE), "{when}");
        }
        assert_eq!(rule_cron("每天 08:30").unwrap().unwrap().assumed, None);
    }

    #[test]
    fn notes() {
        let r = rule_cron("每月 31 日 9:00").unwrap().unwrap();
        assert_eq!(r.cron, "0 9 31 * *");
        assert_eq!(r.notes, ["有的月份没有 31 日，那个月不运行"]);
        assert_eq!(
            rule_cron("每月 29 号").unwrap().unwrap().notes,
            ["有的月份没有 29 日，那个月不运行"]
        );
        assert!(rule_cron("每月 28 号").unwrap().unwrap().notes.is_empty());
        assert_eq!(
            rule_cron("每隔 5 小时").unwrap().unwrap().notes,
            ["每天 0 点重新起算"]
        );
        assert!(rule_cron("每隔 6 小时").unwrap().unwrap().notes.is_empty());
        assert_eq!(
            rule_cron("每隔 7 分钟").unwrap().unwrap().notes,
            ["每小时整点重新起算"]
        );
        assert!(rule_cron("每隔 20 分钟").unwrap().unwrap().notes.is_empty());
    }

    #[test]
    fn unknown_phrases_go_to_the_model() {
        for when in [
            "",
            "   ",
            "明天下午",
            "每天早上",
            "每两周一次",
            "每月 32 日",
            "每天 25:00",
            "每天 9:75",
            "每隔 24 小时",
            "每隔 90 分钟",
            "下个月第一个周一",
            "每周",
            "每天 9 点 整理周报",
            "08:30",
        ] {
            assert_eq!(rule_cron(when), Ok(None), "{when:?}");
            if !when.trim().is_empty() {
                assert_eq!(
                    parse_when(when, parse_tz("Asia/Taipei").unwrap(), now()),
                    Err(WhenError::NotUnderstood),
                    "{when:?}"
                );
            }
        }
    }

    #[test]
    fn too_frequent_and_bad_cron_are_specific_errors() {
        let tz = parse_tz("Asia/Taipei").unwrap();
        assert_eq!(
            rule_cron("每隔 2 分钟"),
            Err(WhenError::Cron(CronError::TooFrequent))
        );
        assert_eq!(
            parse_when("*/2 * * * *", tz, now()),
            Err(WhenError::Cron(CronError::TooFrequent))
        );
        assert_eq!(
            parse_when("0 9 * * 5#2", tz, now()),
            Err(WhenError::Cron(CronError::Unsupported("5#2".into())))
        );
        assert_eq!(
            parse_when("0 0 9 * * *", tz, now())
                .unwrap_err()
                .to_string(),
            "不支持这种 cron 写法：0 0 9 * * *"
        );
        assert_eq!(
            WhenError::NotUnderstood.to_string(),
            "没看懂这个时间，换个说法，例如“每周一 09:00”"
        );
    }

    #[test]
    fn parse_response_shape() {
        let tz = parse_tz("Asia/Taipei").unwrap();
        let p = parse_when("工作日早上九点半", tz, now()).unwrap();
        assert_eq!(p.cron, "30 9 * * 1-5");
        assert_eq!(p.describe, "工作日 09:30");
        assert_eq!(p.tz, "Asia/Taipei");
        assert_eq!(p.by, By::Rule);
        // 2026-11-01 is a Sunday: Mon 2, Tue 3, Wed 4 at 09:30 Taipei = 01:30 UTC.
        let expect: Vec<i64> = [2, 3, 4]
            .iter()
            .map(|d| {
                Utc.with_ymd_and_hms(2026, 11, *d, 1, 30, 0)
                    .unwrap()
                    .timestamp_millis()
            })
            .collect();
        assert_eq!(p.next, expect);
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["by"], "rule");
        assert!(json.get("assumed").is_none());
        assert!(json.get("notes").is_none());

        let p = parse_when("每月最后一天", tz, now()).unwrap();
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["assumed"], ASSUMED_NINE);
        assert_eq!(json["describe"], "每月最后一天 09:00");
    }

    #[test]
    fn describe_of_parsed_phrases() {
        let tz = parse_tz("Asia/Taipei").unwrap();
        for (when, text) in [
            ("每天 08:30", "每天 08:30"),
            ("每周五 9 点", "每周五 09:00"),
            ("每周一、三、五 09:00", "每周一、三、五 09:00"),
            ("每月 1 日 10:00", "每月 1 日 10:00"),
            ("每月最后一天 18:00", "每月最后一天 18:00"),
            ("每隔两小时", "每 2 小时（整点）"),
            ("每隔 15 分钟", "每 15 分钟"),
            ("周末 10:00", "周末 10:00"),
        ] {
            assert_eq!(
                parse_when(when, tz, now()).unwrap().describe,
                text,
                "{when}"
            );
        }
    }

    #[test]
    fn model_output() {
        let tz = parse_tz("Asia/Taipei").unwrap();
        for raw in [
            "0 9 * * 1",
            "`0 9 * * 1`",
            "```\n0 9 * * 1\n```",
            "cron:\n\"0 9 * * 1\"",
        ] {
            let p = accept_model_output(raw, tz, now()).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(p.cron, "0 9 * * 1");
            assert_eq!(p.by, By::Model);
            assert_eq!(serde_json::to_value(&p).unwrap()["by"], "model");
        }
        for raw in ["", "我不知道", "0 0 9 * * *", "0 9 * * 5#2"] {
            assert_eq!(
                accept_model_output(raw, tz, now()),
                Err(WhenError::NotUnderstood),
                "{raw}"
            );
        }
        assert_eq!(
            accept_model_output("*/2 * * * *", tz, now()),
            Err(WhenError::Cron(CronError::TooFrequent))
        );
        let prompt = model_prompt("每两周的周一");
        assert!(prompt.ends_with("说法：每两周的周一"));
        assert!(prompt.contains("每周一、三、五 09:00 → 0 9 * * 1,3,5"));
    }

    #[test]
    fn numerals() {
        for (input, out) in [
            ("九点", "9点"),
            ("十二点", "12点"),
            ("二十三点", "23点"),
            ("十点十五分", "10点15分"),
            ("九点零五", "9点5"),
            ("两小时", "2小时"),
            ("周一", "周一"),
            ("星期三", "星期三"),
            ("周五九点", "周五9点"),
            ("周五十点", "周五10点"),
            ("最后一天", "最后一天"),
            ("一号", "1号"),
            ("三十一日", "31日"),
        ] {
            assert_eq!(arabic_numerals(input), out, "{input}");
        }
    }
}

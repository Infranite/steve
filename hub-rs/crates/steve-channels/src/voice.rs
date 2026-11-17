//! Writing like you, on your own account ([`crate::spaces::Mode::Own`]). A draft there is you
//! talking, so the composer learns from you, not from a Bot (the Bot behind your account only picks
//! the model, [`crate::compose::gather`]):
//!
//! - **Your own messages as samples** ([`samples`]): what you typed yourself on this account, read by
//!   one deterministic query — the same space first, then the same kind of space (private chat or
//!   group), then any — never a model's summary. Links, attachments, placeholders, redactions and
//!   anything shaped like a key are left out; a sample from another space only when it is short and
//!   has no digits, no @ and no name of anyone in your chats ([`portable`]), so no chat's content
//!   travels to another. Only the tone, length and punctuation are to be learned from them.
//! - **Your habits in this situation** ([`styles`], [`Situation`]): length median and P90, the share
//!   ending in a full stop, the share with a question, how you open, which emoji you use — computed
//!   when the reply is drafted from your recent messages in the same situation (a private chat, a
//!   group where someone @-ed or answered you, a group), never written down as numbers. With too few
//!   messages in a situation, the broader one's stand in ([`Situation::broader`]).
//! - **Your verdicts on earlier drafts** ([`examples`]): the drafts you edited before sending, in
//!   their edited form, as “like this”; and a draft you rejected as “not like this” only when your
//!   being there does not explain the rejection — most drafts you turn down sound like you and are
//!   wrong only because you had answered yourself (the presence gate's business, [`crate::presence`]),
//!   so a rejection counts only when you said nothing yourself in that space from shortly before the
//!   message ([`crate::presence::OWNER_LIVE_MS`]) until you rejected it, and its message was not
//!   merged into a later turn. A draft you approved unchanged teaches nothing (it may have been a
//!   test click); one approved by YOLO is not your verdict.
//! - **The rules of writing as you** ([`RULES`]) and, after the model answered, **the checks**
//!   ([`problems`]): a list of options, more than one question, a draft far longer than you write
//!   (over [`OVER_P90`] × your P90 in the situation), an opening you never use. They apply only to
//!   the composer's own reply on your account (its provenance), never to a Bot's, and never to what
//!   you said to write yourself (your trigger words, a flow step's text, your answer in a
//!   conversation): the composer asks once more with what was wrong ([`feedback`]); still wrong, the
//!   message is handed to you with the reason ([`gave_up`]), never dropped.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;

use crate::ChannelResult;
use crate::context::Why;

/// How many of your messages go into the prompt as samples at most.
pub const SAMPLES_MAX: usize = 8;
/// Rows the sample query reads (best first) before the filters pick [`SAMPLES_MAX`].
pub const SAMPLE_SCAN: usize = 120;
/// Your most recent messages the statistics are computed from.
pub const STATS_POOL: usize = 400;
/// A sample from another space: at most this many characters ([`portable`]).
pub const CROSS_SPACE_MAX: usize = 20;
/// A sample or an example, in characters.
pub const LINE_MAX: usize = 80;
/// The fewest of your messages a situation needs for its own statistics; with fewer it uses the
/// broader situation's ([`Situation::broader`]).
pub const MIN_SUPPORT: usize = 5;
/// Rejected and edited drafts shown, each.
pub const EXAMPLES_MAX: usize = 3;
/// A draft longer than this many times your P90 length in the situation is sent back ([`problems`]).
pub const OVER_P90: f64 = 2.0;
/// Ahead of the reason a message is handed to you after the second try ([`gave_up`]).
pub const GAVE_UP: &str = "起草的回复不像你会说的话，交给你自己回：";

// ---------------------------------------------------------------- situations

/// Where and why a message is answered: a private chat, a group where you were @-ed or answered, a
/// group. Your habits differ between them (a question back is normal when someone @-s you with a
/// work question, rare in banter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Situation {
    Dm,
    GroupAt,
    Group,
    All,
}

impl Situation {
    /// The situation whose statistics stand in when this one has too few messages.
    pub fn broader(self) -> Option<Situation> {
        match self {
            Situation::GroupAt => Some(Situation::Group),
            Situation::Dm | Situation::Group => Some(Situation::All),
            Situation::All => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Situation::Dm => "dm",
            Situation::GroupAt => "group_at",
            Situation::Group => "group",
            Situation::All => "all",
        }
    }

    /// As the prompt says it (“我{said}的消息”).
    pub fn said(self) -> &'static str {
        match self {
            Situation::Dm => "私聊",
            Situation::GroupAt => "群里被 @ 或被回复时",
            Situation::Group => "群聊",
            Situation::All => "各种聊天",
        }
    }

    /// The message's situation from why it reached Steve ([`Why`]) and whether the space is a private
    /// chat.
    pub fn of(dm: bool, why: Why) -> Situation {
        if dm || why == Why::Dm {
            Situation::Dm
        } else if matches!(why, Why::Mentioned | Why::ReplyToYou) {
            Situation::GroupAt
        } else {
            Situation::Group
        }
    }
}

// ---------------------------------------------------------------- your messages

/// One message you typed yourself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Said {
    pub id: String,
    /// When (ms).
    pub at: i64,
    pub space_id: String,
    /// In a private chat.
    pub dm: bool,
    /// The situation it was said in (for a group: whether the message before it @-ed or answered
    /// you).
    pub situation: Option<Situation>,
    pub text: String,
    /// It carries an attachment.
    pub media: bool,
}

/// Something shaped like a key or a token: a long run of letters and digits mixed.
static KEYLIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_\-+/=]{20,}").expect("valid regex"));
/// A chat app's emoji code inside a text (`[Smile]`, `[微笑]`).
static APP_EMOJI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[\p{L}_\-]{1,20}\]").expect("valid regex"));
static UNICODE_EMOJI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\p{Extended_Pictographic}").expect("valid regex"));

fn keylike(t: &str) -> bool {
    KEYLIKE.find_iter(t).any(|m| {
        let s = m.as_str();
        s.chars().any(|c| c.is_ascii_digit()) && s.chars().any(|c| c.is_ascii_alphabetic())
    })
}

/// A message too specific or too risky to learn tone from: empty, a link, an attachment, a
/// provider placeholder, a redaction, something shaped like a key.
pub fn usable(text: &str, media: bool) -> bool {
    let t = text.trim();
    !(media
        || t.is_empty()
        || t.contains("http://")
        || t.contains("https://")
        || t.contains("[REDACTED]")
        || (t.starts_with('[') && t.ends_with(']'))
        || keylike(t))
}

/// Is a sample from another space plain enough to carry over: short ([`CROSS_SPACE_MAX`]), no
/// digits, no @, none of `names` (the people in your chats)?
pub fn portable(t: &str, names: &[String]) -> bool {
    t.chars().count() <= CROSS_SPACE_MAX
        && !t
            .chars()
            .any(|c| c.is_ascii_digit() || c == '@' || c == '＠')
        && !names
            .iter()
            .map(|n| n.trim())
            .filter(|n| n.chars().count() >= 2)
            .any(|n| t.contains(n))
}

/// Your samples for a message in `space` (`dm`: a private chat) said at `before`: messages you sent
/// before it — never one of `excluded` — the same space's most recent first, then the same kind of
/// space, then any; those from other spaces only when [`portable`]. Deduplicated, cut to
/// [`LINE_MAX`], at most `max`.
pub fn samples(
    pool: &[Said],
    space: &str,
    dm: bool,
    before: i64,
    excluded: &HashSet<String>,
    names: &[String],
    max: usize,
) -> Vec<String> {
    let rank = |s: &Said| {
        if s.space_id == space {
            0
        } else if s.dm == dm {
            1
        } else {
            2
        }
    };
    let mut ranked: Vec<&Said> = pool
        .iter()
        .filter(|s| s.at < before && !excluded.contains(&s.id))
        .filter(|s| usable(&s.text, s.media))
        .collect();
    ranked.sort_by_key(|s| (rank(s), std::cmp::Reverse(s.at)));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in ranked {
        let t = s.text.trim();
        if rank(s) > 0 && !portable(t, names) {
            continue;
        }
        let t: String = t.chars().take(LINE_MAX).collect();
        if seen.insert(t.clone()) {
            out.push(t);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------- your habits

/// The messages of a text as they would be sent: one per non-empty line.
pub fn messages_of(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

pub fn chars(t: &str) -> usize {
    t.trim().chars().count()
}

const QUESTION_MARKS: [char; 2] = ['？', '?'];

pub fn ends_with_full_stop(t: &str) -> bool {
    t.trim_end().ends_with('。')
}

pub fn asks(t: &str) -> bool {
    t.contains(QUESTION_MARKS)
}

/// Questions in a text: its question marks, a run of them counted once (“啥？？”).
pub fn question_count(t: &str) -> usize {
    let mut n = 0;
    let mut prev = false;
    for c in t.chars() {
        let q = QUESTION_MARKS.contains(&c);
        if q && !prev {
            n += 1;
        }
        prev = q;
    }
    n
}

/// Openings that announce rather than say something (“收到，”, “明白。”). Measured as your habit
/// ([`Style::openers`]); a draft is sent back for one only when you never open that way yourself.
pub const OPENERS: [&str; 6] = ["收到", "明白", "好的", "了解", "好嘞", "没问题"];

/// The boilerplate opening `t` starts with, if any.
pub fn opener(t: &str) -> Option<&'static str> {
    let t = t.trim_start();
    OPENERS.iter().copied().find(|o| {
        t.strip_prefix(o).is_some_and(|rest| {
            rest.is_empty() || rest.starts_with(['，', ',', '。', '！', '!', '～', '~', ' ', '\n'])
        })
    })
}

/// Your habits in one situation, from your own messages.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Style {
    pub n: usize,
    pub median_len: f64,
    pub p90_len: f64,
    pub full_stop_rate: f64,
    pub question_rate: f64,
    pub opening_rate: f64,
    /// The boilerplate openings ([`OPENERS`]) you do use here.
    pub openers: Vec<String>,
    /// Messages with a chat app's emoji code (`[Smile]`) …
    pub app_emoji_rate: f64,
    /// … and with a Unicode emoji.
    pub unicode_emoji_rate: f64,
}

pub fn quantile(sorted: &[usize], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let pos = q * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    sorted[lo] as f64 + (sorted[hi] as f64 - sorted[lo] as f64) * (pos - lo as f64)
}

fn rate(n: usize, of: usize) -> f64 {
    if of == 0 { 0.0 } else { n as f64 / of as f64 }
}

/// The habits of some messages.
pub fn style_of(texts: &[&str]) -> Style {
    let mut lens: Vec<usize> = texts.iter().map(|t| chars(t)).collect();
    lens.sort_unstable();
    let count = |f: &dyn Fn(&str) -> bool| texts.iter().filter(|t| f(t)).count();
    let mut openers: Vec<String> = texts
        .iter()
        .filter_map(|t| opener(t))
        .map(String::from)
        .collect();
    openers.sort();
    openers.dedup();
    Style {
        n: texts.len(),
        median_len: quantile(&lens, 0.5),
        p90_len: quantile(&lens, 0.9),
        full_stop_rate: rate(count(&ends_with_full_stop), texts.len()),
        question_rate: rate(count(&asks), texts.len()),
        opening_rate: rate(count(&|t| opener(t).is_some()), texts.len()),
        openers,
        app_emoji_rate: rate(count(&|t| APP_EMOJI.is_match(t)), texts.len()),
        unicode_emoji_rate: rate(count(&|t| UNICODE_EMOJI.is_match(t)), texts.len()),
    }
}

/// Your habits per situation, from `pool` (your messages; the usable ones count). A group message
/// where you were @-ed counts for the group too, and everything for [`Situation::All`].
pub fn styles(pool: &[Said]) -> BTreeMap<Situation, Style> {
    let mut by: BTreeMap<Situation, Vec<&str>> = BTreeMap::new();
    for s in pool.iter().filter(|s| usable(&s.text, s.media)) {
        let sit = s.situation.unwrap_or(if s.dm {
            Situation::Dm
        } else {
            Situation::Group
        });
        by.entry(sit).or_default().push(s.text.as_str());
        if sit == Situation::GroupAt {
            by.entry(Situation::Group)
                .or_default()
                .push(s.text.as_str());
        }
        by.entry(Situation::All).or_default().push(s.text.as_str());
    }
    by.into_iter().map(|(s, t)| (s, style_of(&t))).collect()
}

/// The habits that stand for `s`: its own with [`MIN_SUPPORT`] messages, else the broader one's;
/// `None` when no situation has that many.
pub fn style_for(styles: &BTreeMap<Situation, Style>, s: Situation) -> Option<(Situation, &Style)> {
    let mut at = Some(s);
    while let Some(x) = at {
        if let Some(st) = styles.get(&x).filter(|st| st.n >= MIN_SUPPORT) {
            return Some((x, st));
        }
        at = x.broader();
    }
    None
}

// ---------------------------------------------------------------- the prompt

/// What the composer is given to write like you.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Voice {
    /// The message's situation.
    pub situation: Option<Situation>,
    /// Your habits standing for it (and the situation they come from).
    pub style: Option<(Situation, Style)>,
    pub samples: Vec<String>,
    /// Drafts you rejected while you were not there yourself ([`examples`]).
    pub rejected: Vec<String>,
    /// Drafts you edited before sending, as sent.
    pub edited: Vec<String>,
}

/// Fixed rules of writing as you; the numbers come from [`Voice::habits`].
pub const RULES: &str = "- 写一到两行短句；每一行就是一条消息，不写成段落，不加小标题。
- 不用“收到 / 明白 / 好的，”这类开场白，不写公文腔、汇报腔、分析报告腔。
- 不替我承诺去查、去做、给时间或给结果；这条只能用承诺来回时，outcome 用 ask_owner，让我自己决定。
- 最多问一个问题，而且只在不问就接不下去时才问；不盘问对方，不给对方列选项（“A 还是 B”“1. 2.”“可以回复：…”）。
- 不提 Steve、事项、队列、编号，也不说自己在起草。
- 我在对话里已经说过的话是事实，不能和它矛盾；我已经回答过的，不再回答一遍。
- 没有可靠依据时宁可短：一句中性的话，或者 skip。不编没看到的事实。";

fn pct(r: f64) -> String {
    format!("{:.0}%", r * 100.0)
}

fn len(x: f64) -> String {
    format!("{:.0}", x.round())
}

impl Voice {
    /// Your habits as one line, from the numbers; empty without statistics.
    pub fn habits(&self) -> String {
        let Some((from, s)) = &self.style else {
            return String::new();
        };
        let mut parts = vec![
            format!(
                "一条消息一般 {} 字左右，长的也就 {} 字",
                len(s.median_len),
                len(s.p90_len)
            ),
            format!("以句号结尾的占 {}", pct(s.full_stop_rate)),
            format!("带问号的占 {}", pct(s.question_rate)),
        ];
        if s.openers.is_empty() {
            parts.push("从不用“收到 / 好的”这类开场白".into());
        } else {
            parts.push(format!(
                "用“{}”开头的占 {}",
                s.openers.join(" / "),
                pct(s.opening_rate)
            ));
        }
        parts.push(match (s.app_emoji_rate > 0.0, s.unicode_emoji_rate > 0.0) {
            (true, false) => format!(
                "表情只用聊天软件自带的方括号写法（占 {}），不用 Unicode emoji",
                pct(s.app_emoji_rate)
            ),
            (false, true) => format!("表情用 Unicode emoji（占 {}）", pct(s.unicode_emoji_rate)),
            (true, true) => format!(
                "表情：方括号写法占 {}，Unicode emoji 占 {}",
                pct(s.app_emoji_rate),
                pct(s.unicode_emoji_rate)
            ),
            (false, false) => "不用表情".into(),
        });
        let whose = match self.situation {
            Some(sit) if sit != *from => format!(
                "{}的消息太少，按我{}的消息（最近 {} 条）",
                sit.said(),
                from.said(),
                s.n
            ),
            _ => format!("按我{}的消息（最近 {} 条）", from.said(), s.n),
        };
        format!(
            "{whose}算的习惯，会随我说话的习惯变：{}。",
            parts.join("；")
        )
    }

    /// Under the persona: your samples and habits, the drafts you rejected and edited (empty when
    /// there is none of them).
    pub fn render(&self) -> String {
        use crate::compose::{H_EDITED, H_REJECTED, H_STYLE};
        let list = |xs: &[String]| {
            xs.iter()
                .map(|t| format!("- {}", cut(t, LINE_MAX)))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut p = String::new();
        let habits = self.habits();
        if !self.samples.is_empty() || !habits.is_empty() {
            p.push_str(H_STYLE);
            p.push('\n');
            if !self.samples.is_empty() {
                p.push_str("下面是我自己发过的几条消息，只学语气、长度和标点，不要照搬内容：\n");
                p.push_str(&list(&self.samples));
                p.push('\n');
            }
            if !habits.is_empty() {
                p.push_str(&habits);
                p.push('\n');
            }
            p.push('\n');
        }
        if !self.rejected.is_empty() {
            p.push_str(H_REJECTED);
            p.push_str("（当时我没在场，也没让发的，不要这样写）\n");
            p.push_str(&list(&self.rejected));
            p.push_str("\n\n");
        }
        if !self.edited.is_empty() {
            p.push_str(H_EDITED);
            p.push_str("（我改过再发的，像这样写）\n");
            p.push_str(&list(&self.edited));
            p.push_str("\n\n");
        }
        p
    }

    /// Before the output format: the rules of writing as you ([`RULES`]).
    pub fn rules(&self) -> String {
        format!("{}\n{RULES}\n\n", crate::compose::H_WRITE)
    }
}

fn cut(s: &str, n: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

// ---------------------------------------------------------------- the checks

/// Lines that enumerate (`A. `, `1、`, `- `): two or more make a list of options.
pub static ENUMERATED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[A-Da-d][\.、:：\)）]|\d+[\.、\)）]|[-•·])\s*\S").expect("valid regex")
});

/// Whether a text lists options: the options line of a question ([`crate::conversations::OPTIONS_HEAD`])
/// or two or more enumerated lines.
pub fn option_list(text: &str) -> bool {
    text.contains(crate::conversations::OPTIONS_HEAD)
        || messages_of(text)
            .iter()
            .filter(|m| ENUMERATED.is_match(m))
            .count()
            >= 2
}

/// What is wrong with a draft written as you.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum Problem {
    /// It gives the other person options to pick from.
    OptionList,
    /// More than one question.
    Questions { n: usize },
    /// Far longer than you write in the situation.
    TooLong { len: usize, limit: usize },
    /// It opens with a boilerplate you never use here.
    Opener { opener: String },
}

impl Problem {
    pub fn code(&self) -> &'static str {
        match self {
            Problem::OptionList => "option_list",
            Problem::Questions { .. } => "questions",
            Problem::TooLong { .. } => "too_long",
            Problem::Opener { .. } => "opener",
        }
    }

    /// As said to the model and in the reason.
    pub fn said(&self) -> String {
        match self {
            Problem::OptionList => "给对方列了选项".into(),
            Problem::Questions { n } => format!("问了 {n} 个问题，最多一个"),
            Problem::TooLong { len, limit } => {
                format!("{len} 字，我在这种场合一般不超过 {limit} 字")
            }
            Problem::Opener { opener } => format!("用“{opener}”开头，我不这样开头"),
        }
    }
}

/// The checks of a reply written as you: a list of options and more than one question always;
/// with your habits in the situation (`style`), a length over [`OVER_P90`] × your P90 and an
/// opening you never use. Without habits, only the first two.
pub fn problems(text: &str, style: Option<&Style>) -> Vec<Problem> {
    let mut out = Vec::new();
    if option_list(text) {
        out.push(Problem::OptionList);
    }
    let q = question_count(text);
    if q > 1 {
        out.push(Problem::Questions { n: q });
    }
    if let Some(s) = style {
        let total: usize = messages_of(text).iter().map(|m| chars(m)).sum();
        let limit = (OVER_P90 * s.p90_len).ceil() as usize;
        if s.p90_len > 0.0 && total > limit {
            out.push(Problem::TooLong { len: total, limit });
        }
        if let Some(o) = messages_of(text).first().and_then(|m| opener(m))
            && !s.openers.iter().any(|x| x == o)
        {
            out.push(Problem::Opener {
                opener: o.to_string(),
            });
        }
    }
    out
}

/// What the composer is told on its second try.
pub fn feedback(draft: &str, problems: &[Problem]) -> String {
    let said: Vec<String> = problems.iter().map(Problem::said).collect();
    format!(
        "上一稿：{}\n它的问题：{}。\n按【怎么写】重写；如果改短以后没什么可说的，outcome 用 skip。",
        cut(draft, crate::compose::REVISE_DRAFT_MAX),
        said.join("；")
    )
}

/// The note and reason of the hand-off when the second try is still wrong.
pub fn gave_up(problems: &[Problem]) -> String {
    let said: Vec<String> = problems.iter().map(Problem::said).collect();
    format!("{GAVE_UP}{}", said.join("；"))
}

// ---------------------------------------------------------------- over the store

/// Your messages on the connection `conn` before `before`, as SQL over `m` (and `s`, its space) with
/// [`crate::presence::MINE`] in scope (`?1` ws, `?2` the space): typed by you (an inbound message
/// from your contact, not another assistant's automatic reply, which is filtered after), received.
/// The message before each in its space (`p`, with its event `e`) tells its situation.
const POOL: &str = "SELECT m.id, {AT}, m.space_id, s.kind = 'dm', coalesce(m.text, ''),
       coalesce(json_array_length(m.attachments), 0) > 0,
       p.id IS NOT NULL AND NOT (coalesce(p.sender_party_id IN (SELECT id FROM mine), 0)
                                 OR coalesce(p.sender_account_id IN (SELECT id FROM my_accounts), 0)),
       coalesce(json_extract(e.payload, '$.facts.mentioned'), 0),
       coalesce(json_extract(e.payload, '$.facts.reply_to_me'), 0),
       coalesce(p.mentions, '[]')
  FROM chan_messages m JOIN spaces s ON s.id = m.space_id
  LEFT JOIN chan_messages p ON p.id = (
       SELECT q.id FROM chan_messages q WHERE q.space_id = m.space_id
          AND (q.created < m.created OR (q.created = m.created AND q.id < m.id))
        ORDER BY q.created DESC, q.id DESC LIMIT 1)
  LEFT JOIN ext_events e ON e.id = p.event_id
 WHERE m.workspace_id = ?1 AND m.sender_party_id IN (SELECT id FROM mine)
   AND s.connection_id = ?3 AND m.direction = 'in' AND m.status IN ('received', 'sent')
   AND m.created < ?4 AND m.text IS NOT NULL AND m.text <> ''";

/// How [`read_pool`] orders your messages.
#[derive(Debug, Clone, Copy)]
enum Order {
    /// The space's first, then the same kind of space (`dm`: private chats), then any; the most
    /// recent first in each ([`SAMPLE_SCAN`] of them).
    Ranked { dm: bool },
    /// The most recent first ([`STATS_POOL`] of them).
    Recent,
}

fn read_pool(
    c: &Connection,
    ws: &str,
    conn: &str,
    space: &str,
    before: i64,
    order: Order,
) -> ChannelResult<Vec<Said>> {
    let my = crate::context::mine(c, ws, space)?.accounts;
    let (by, limit) = match order {
        Order::Ranked { .. } => (
            "CASE WHEN m.space_id = ?2 THEN 0 WHEN (s.kind = 'dm') = ?6 THEN 1 ELSE 2 END, m.created DESC, m.id DESC",
            SAMPLE_SCAN,
        ),
        Order::Recent => ("m.created DESC, m.id DESC", STATS_POOL),
    };
    let sql = format!(
        "{} {} ORDER BY {by} LIMIT ?5",
        crate::presence::MINE,
        POOL.replace("{AT}", crate::context::AT)
    );
    let mut st = c.prepare_cached(&sql)?;
    let limit = limit as i64;
    let mut args: Vec<&dyn rusqlite::ToSql> = vec![&ws, &space, &conn, &before, &limit];
    // `?6`, whether the message is in a private chat, only where the order ranks by it
    let dm = matches!(order, Order::Ranked { dm: true });
    if let Order::Ranked { .. } = order {
        args.push(&dm);
    }
    let rows = st
        .query_map(args.as_slice(), |r| {
            let dm: bool = r.get(3)?;
            let other_before: bool = r.get(6)?;
            let mentioned: bool = r.get::<_, i64>(7)? != 0;
            let reply_to_me: bool = r.get::<_, i64>(8)? != 0;
            let mentions: String = r.get(9)?;
            let mentions_me = serde_json::from_str::<Vec<Value>>(&mentions)
                .unwrap_or_default()
                .iter()
                .filter_map(|m| m["account_id"].as_str())
                .any(|a| my.iter().any(|x| x == a));
            let situation = if dm {
                Situation::Dm
            } else if other_before && (mentioned || reply_to_me || mentions_me) {
                Situation::GroupAt
            } else {
                Situation::Group
            };
            Ok(Said {
                id: r.get(0)?,
                at: r.get(1)?,
                space_id: r.get(2)?,
                dm,
                situation: Some(situation),
                text: r.get(4)?,
                media: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|s| !crate::arbiter::looks_automated(&s.text))
        .collect())
}

/// The names of the people in your chats on `conn` (members' display names, their contacts' names
/// and aliases, private chats' names): what a sample from another space must not contain.
pub fn people_names(c: &Connection, ws: &str, conn: &str) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT coalesce(a.display_name, ''), coalesce(p.name, ''), coalesce(p.aliases, '[]')
           FROM spaces s JOIN space_members sm ON sm.space_id = s.id
           JOIN accounts a ON a.id = sm.account_id LEFT JOIN parties p ON p.id = a.party_id
          WHERE s.workspace_id = ?1 AND s.connection_id = ?2
         UNION SELECT s.name, '', '[]' FROM spaces s
          WHERE s.workspace_id = ?1 AND s.connection_id = ?2 AND s.kind = 'dm'",
    )?;
    let mut names: Vec<String> = Vec::new();
    let rows = st.query_map(params![ws, conn], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (d, p, aliases) = row?;
        names.push(d);
        names.push(p);
        if let Ok(Value::Array(xs)) = serde_json::from_str::<Value>(&aliases) {
            names.extend(xs.iter().filter_map(|x| x.as_str().map(String::from)));
        }
    }
    names.retain(|n| n.trim().chars().count() >= 2);
    names.sort();
    names.dedup();
    Ok(names)
}

/// Your verdicts on drafts written as you on `conn` (not YOLO's): `(rejected, edited)`, the same
/// space's first, then the most recent; from another space only when [`portable`]. A rejected draft
/// as it was, and only when your being there does not explain it ([`rejected_for_its_words`]); an
/// edited one as you sent it.
pub fn examples(
    c: &Connection,
    ws: &str,
    conn: &str,
    space: &str,
    names: &[String],
) -> ChannelResult<(Vec<String>, Vec<String>)> {
    let mut st = c.prepare_cached(
        "SELECT d.draft, json_extract(d.result, '$.choice'), coalesce(json_extract(d.result, '$.text'), ''),
                m.space_id = ?3, m.id, m.space_id, d.created, coalesce(d.resolved, d.created)
           FROM decisions d JOIN chan_messages m ON m.id = d.subject_id
           JOIN spaces s ON s.id = m.space_id
          WHERE d.workspace_id = ?1 AND d.subject_type = ?4 AND d.kind = ?5 AND d.status = 'resolved'
            AND json_extract(d.result, '$.decided_by') IS NULL AND s.connection_id = ?2
            AND m.direction = 'out' AND m.persona_id IS NULL AND m.on_behalf = 'none'
            AND json_extract(d.result, '$.choice') IN ('yes', 'no')
          ORDER BY m.space_id = ?3 DESC, d.resolved DESC LIMIT ?6",
    )?;
    let rows = st
        .query_map(
            params![
                ws,
                conn,
                space,
                crate::respond::SUBJECT_MESSAGE,
                steve_store::base::DecisionKind::Approve.code(),
                (EXAMPLES_MAX * 20) as i64
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut rejected, mut edited) = (Vec::new(), Vec::new());
    for (draft, choice, text, here, out, out_space, created, resolved) in rows {
        let (list, t) = match choice.as_str() {
            "no" if rejected.len() < EXAMPLES_MAX
                && rejected_for_its_words(c, ws, &out, &out_space, created, resolved)? =>
            {
                (&mut rejected, draft.trim().to_string())
            }
            "no" => continue,
            _ if !text.trim().is_empty() && text.trim() != draft.trim() => {
                (&mut edited, text.trim().to_string())
            }
            _ => continue,
        };
        if t.is_empty() || keylike(&t) || (!here && !portable(&t, names)) {
            continue;
        }
        if list.len() < EXAMPLES_MAX && !list.contains(&t) {
            list.push(t);
        }
    }
    Ok((rejected, edited))
}

/// Whether a rejected draft (outbound message `out` in `space`, its card opened at `created` and
/// answered at `resolved`) can say something about how you write: not when your being there explains
/// it — you said something yourself in the space from [`crate::presence::OWNER_LIVE_MS`] before the
/// message it answers (or the card, without one) until you rejected it, or that message was merged
/// into a later turn. Space-wide on purpose, wider than the presence gate's own rule: a rejection
/// that may have been about presence never teaches voice.
pub fn rejected_for_its_words(
    c: &Connection,
    ws: &str,
    out: &str,
    space: &str,
    created: i64,
    resolved: i64,
) -> ChannelResult<bool> {
    let src = crate::presence::answered_message(c, ws, out)?;
    let from = match &src {
        Some(id) => {
            if crate::presence::merged_into(c, id)?.is_some() {
                return Ok(false);
            }
            crate::presence::said(c, ws, id)?.map_or(created, |(s, _, _)| s.at.min(created))
        }
        None => created,
    };
    Ok(!crate::presence::you_spoke(
        c,
        ws,
        space,
        from - crate::presence::OWNER_LIVE_MS,
        resolved.max(created),
    )?)
}

/// Everything [`Voice`] holds for a message in `space` (`dm`) on `conn`, in `situation`, said at
/// `before`.
pub fn gather(
    c: &Connection,
    ws: &str,
    conn: &str,
    space: &str,
    dm: bool,
    situation: Situation,
    before: i64,
) -> ChannelResult<Voice> {
    let names = people_names(c, ws, conn)?;
    let ranked = read_pool(c, ws, conn, space, before, Order::Ranked { dm })?;
    let recent = read_pool(c, ws, conn, space, before, Order::Recent)?;
    let styles = styles(&recent);
    let (rejected, edited) = examples(c, ws, conn, space, &names)?;
    Ok(Voice {
        situation: Some(situation),
        style: style_for(&styles, situation).map(|(s, st)| (s, st.clone())),
        samples: samples(
            &ranked,
            space,
            dm,
            before,
            &HashSet::new(),
            &names,
            SAMPLES_MAX,
        ),
        rejected,
        edited,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(id: &str, at: i64, space: &str, dm: bool, text: &str) -> Said {
        Said {
            id: id.into(),
            at,
            space_id: space.into(),
            dm,
            situation: None,
            text: text.into(),
            media: false,
        }
    }

    #[test]
    fn samples_are_your_plain_words_the_space_first() {
        let pool = vec![
            said("1", 1, "s1", true, "先这样"),
            said("2", 2, "s2", true, "那个 8080 先别管"),
            said("3", 3, "s3", false, "这是干嘛的"),
            said("4", 4, "s2", true, "你找下老王"),
            said("5", 5, "s1", true, "https://example.com/x 看下"),
            said("6", 6, "s1", true, "key sk0123456789abcdefghij0"),
            said("7", 7, "s1", true, "[Smile]"),
            said("8", 8, "s1", true, "等我下"),
            said("9", 99, "s1", true, "之后的"),
            said("10", 9, "s2", true, "可以的"),
        ];
        let names = vec!["A".to_string(), "老王".to_string()];
        let s = samples(&pool, "s1", true, 50, &HashSet::new(), &names, 10);
        // the space's own first (most recent first), then other private chats, then groups; never
        // a link, a key, a placeholder, or what came after
        assert_eq!(s, vec!["等我下", "先这样", "可以的", "这是干嘛的"]);
        // another space's line with a digit or a name stays there
        assert!(!s.iter().any(|t| t.contains("8080") || t.contains("找下")));
        // excluded ids never
        let ex: HashSet<String> = ["8".to_string()].into();
        assert_eq!(
            samples(&pool, "s1", true, 50, &ex, &names, 1),
            vec!["先这样"]
        );
        // an attachment is not a sample
        let mut m = said("11", 10, "s1", true, "看图");
        m.media = true;
        assert!(samples(&[m], "s1", true, 50, &HashSet::new(), &[], 5).is_empty());
    }

    #[test]
    fn habits_are_per_situation_and_fall_back_when_thin() {
        let mut pool = Vec::new();
        for (i, t) in [
            "等我下",
            "行了",
            "你急不急？",
            "我瞅瞅",
            "嗯 也许吧",
            "先这样[Smile]",
        ]
        .iter()
        .enumerate()
        {
            pool.push(said(&i.to_string(), i as i64, "d", true, t));
        }
        let mut g = said("g", 9, "g", false, "收到，我瞅瞅");
        g.situation = Some(Situation::GroupAt);
        pool.push(g);
        let st = styles(&pool);
        let (from, dm) = style_for(&st, Situation::Dm).unwrap();
        assert_eq!((from, dm.n), (Situation::Dm, 6));
        assert!((dm.question_rate - 1.0 / 6.0).abs() < 1e-9);
        assert_eq!(dm.full_stop_rate, 0.0);
        assert!(dm.app_emoji_rate > 0.0 && dm.unicode_emoji_rate == 0.0);
        assert!(dm.openers.is_empty());
        // one group message is too few: the broader situation's habits stand in
        let (from, _) = style_for(&st, Situation::GroupAt).unwrap();
        assert_eq!(from, Situation::All);
        assert_eq!(st[&Situation::GroupAt].openers, vec!["收到".to_string()]);
        // and nothing at all without enough messages
        assert!(style_for(&styles(&pool[..2]), Situation::Dm).is_none());
    }

    #[test]
    fn the_checks_follow_your_habits() {
        let style = Style {
            n: 20,
            median_len: 8.0,
            p90_len: 15.0,
            openers: vec!["好的".into()],
            ..Style::default()
        };
        let s = Some(&style);
        assert!(problems("等我下 我瞅瞅", s).is_empty());
        assert_eq!(
            problems("你是要 A 还是 B？\n什么时候要？", s),
            vec![Problem::Questions { n: 2 }]
        );
        // a run of question marks is one question
        assert!(problems("啥？？", s).is_empty());
        assert_eq!(
            problems("1. 先重启\n2. 再看日志", s),
            vec![Problem::OptionList]
        );
        assert!(
            problems(&format!("你看下{}", crate::conversations::OPTIONS_HEAD), s)
                .contains(&Problem::OptionList)
        );
        let long = "这个问题我理解下来主要是三个方面的原因，需要分别拆开来一个一个看清楚再说";
        assert_eq!(
            problems(long, s),
            vec![Problem::TooLong {
                len: chars(long),
                limit: 30
            }]
        );
        // an opener you never use is sent back; one you use is yours
        assert_eq!(
            problems("收到，我瞅瞅", s),
            vec![Problem::Opener {
                opener: "收到".into()
            }]
        );
        assert!(problems("好的 我瞅瞅", s).is_empty());
        // without your habits: only the rules that need none
        assert!(problems("收到，我瞅瞅", None).is_empty());
        assert!(problems(long, None).is_empty());
        let fb = feedback("收到，我瞅瞅", &problems("收到，我瞅瞅", s));
        assert!(fb.contains("收到") && fb.contains("skip"), "{fb}");
        assert!(gave_up(&[Problem::OptionList]).starts_with(GAVE_UP));
    }

    #[test]
    fn the_habits_line_says_the_numbers_it_was_given() {
        let v = Voice {
            situation: Some(Situation::GroupAt),
            style: Some((
                Situation::Group,
                Style {
                    n: 12,
                    median_len: 10.5,
                    p90_len: 27.8,
                    question_rate: 0.25,
                    app_emoji_rate: 0.1,
                    ..Style::default()
                },
            )),
            ..Voice::default()
        };
        let h = v.habits();
        assert!(
            h.contains("群里被 @ 或被回复时的消息太少，按我群聊的消息（最近 12 条）"),
            "{h}"
        );
        assert!(h.contains("11 字左右") && h.contains("28 字"), "{h}");
        assert!(
            h.contains("带问号的占 25%") && h.contains("不用 Unicode emoji"),
            "{h}"
        );
        assert!(h.contains("从不用"), "{h}");
        // no statistics, no line; the rules are always there
        let bare = Voice::default();
        assert!(bare.habits().is_empty());
        assert!(bare.render().is_empty());
        let r = bare.rules();
        assert!(r.contains(crate::compose::H_WRITE) && r.contains(RULES));
    }
}

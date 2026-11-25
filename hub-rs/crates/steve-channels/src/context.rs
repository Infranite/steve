//! The conversation as the composer reads it (#203): who “我” is, where the message was said and
//! why it reached Steve, the talk around it in stretches of time, and what came after it.
//!
//! - **Who is who, by id.** A line is yours when its sender account or contact is one of yours
//!   ([`Mine`], the same set the presence gate uses), never by a name. On your own account it reads
//!   `<name>（我）`, and what Steve sent for you `<name>（我，经 Steve 发出）`; on a bot account your
//!   lines read `（主人）` and the persona's own `（你发出的）`. Your names are listed once
//!   ([`owner_block`]) with numeric ids and raw handles left out ([`raw_name`]), so `@name` and “我”
//!   are known to be the same person.
//! - **Time, not threads.** Private chats and groups alike are read as stretches of talk: a gap of
//!   [`SEGMENT_GAP_MS`] starts a new one, and the stretch of the message and the one before it are
//!   shown ([`arrange`]). A provider thread only groups explicit replies (most group messages are
//!   alone in theirs, and a private chat gives every message its own), so it is a hint: lines in the
//!   message's reply chain are marked, and earlier ones of that chain outside the stretches are added
//!   apart.
//! - **Readable lines.** Every line has a relative time; runs of bot messages fold into one line;
//!   provider placeholders and attachments become typed notes ([`typed`]), with what the composer
//!   read in an image before reused ([`DESCRIBED`]) and an unread image said to be unread — nothing
//!   is concluded from it.
//! - **After the message.** Drafting takes minutes: what was said after the message is shown and
//!   marked, and drafts still waiting in the space are listed.
//!
//! [`arrange`] and the renderers are pure, so the offline replay (`crate::reply_eval`) builds the
//! same context from a case as [`load`] does from the store.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, TimeZone};
use serde_json::Value;
use steve_store::{Read, channel_message_repo::dialogue};

use crate::ChannelResult;
use crate::spaces::Mode;

/// A gap this long between two messages ends a stretch of talk. Half an hour: what was said a few
/// minutes earlier is the same conversation, what was said the morning before is not, and the
/// stretch before the current one is shown anyway; the replay (`examples/reply_eval.rs`) measures it.
pub const SEGMENT_GAP_MS: i64 = 30 * 60_000;
/// Stretches shown before the message: its own and the one before.
pub const SEGMENTS: usize = 2;
/// … when the model asks for more context: every stretch, up to the line limit.
pub const SEGMENTS_WIDE: usize = usize::MAX;
/// Earlier messages of the message's reply chain shown apart when outside the stretches.
pub const CHAIN_EARLIER: usize = 10;
/// Messages after the message shown.
pub const AFTER: usize = 10;
/// Drafts still waiting in the space listed.
pub const PENDING: usize = 5;
/// Messages read from the store before folding and cutting into stretches.
const SCAN: usize = 400;
/// Read after the message beyond [`AFTER`]: rows stored after it that were said before it.
const AFTER_SLACK: usize = 20;
/// A line, a folded bot line's sample, a pending draft, in characters.
const LINE_MAX: usize = 500;
const SAMPLE_MAX: usize = 80;
/// The attachment key that keeps what the composer read in an image, to be reused in later
/// conversations ([`remember_described`]).
pub const DESCRIBED: &str = "described";
/// One image description, in characters.
pub const DESCRIBED_MAX: usize = 200;

/// Who wrote a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Other,
    /// You, in your own words.
    Owner,
    /// Sent for you by Steve on your own account.
    OwnerViaSteve,
    /// Posted on your account by another assistant by itself ([`crate::arbiter::looks_automated`]).
    OwnerAuto,
    /// The persona this bot account speaks as.
    Persona,
}

/// A structured `@`-mention carried by a channel message (#258).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionedPerson {
    pub account_id: Option<String>,
    pub name: String,
    pub is_owner: bool,
}

/// An active incoming emoji reaction shown inline on a message in context (#258 / #277).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MsgReaction {
    pub who: String,
    pub emoji: String,
    pub is_owner: bool,
    pub origin: String,
}

/// One message as the context reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Msg {
    pub id: String,
    /// When it was said in the chat (ms): the provider's time when known; for what Steve
    /// sent, when it was sent, not when it was drafted.
    pub at: i64,
    /// The sender's name as shown, never a raw handle (empty: unknown).
    pub name: String,
    pub side: Side,
    /// From a bot.
    pub bot: bool,
    /// The text with typed notes ([`typed`]).
    pub text: String,
    pub thread_key: Option<String>,
    pub external_id: Option<String>,
    pub reply_to: Option<String>,
    /// Structured `@`-mentions parsed deterministically from the channel event (#258).
    pub mentions: Vec<MentionedPerson>,
    /// Whether structured mentions explicitly address one of the owner's accounts.
    pub mentions_owner: bool,
    /// Display label of the sender of the quoted `reply_to` message, when resolved.
    pub reply_to_sender: Option<String>,
    /// Whether the quoted `reply_to` message was written by the owner.
    pub reply_to_is_owner: bool,
    /// Active incoming emoji reactions on this message.
    pub reactions: Vec<MsgReaction>,
}

/// One line of the rendered conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    Line {
        /// Relative time ([`relative`]).
        when: String,
        /// Labelled sender ([`label`]).
        who: String,
        text: String,
        /// In the message's reply chain.
        chain: bool,
    },
    /// Between two stretches of talk.
    Gap(String),
}

impl Shown {
    /// `- 3 分钟前 A：text` (a reply-chain line marked), or the gap note.
    pub fn render(&self) -> String {
        match self {
            Shown::Line {
                when,
                who,
                text,
                chain,
            } => {
                let mark = if *chain { "〔同一回复链〕" } else { "" };
                format!("- {when} {mark}{who}：{}", cut(text, LINE_MAX))
            }
            Shown::Gap(g) => format!("  ……{g}……"),
        }
    }
}

/// The conversation around a message.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    /// The stretches before the message, oldest first.
    pub before: Vec<Shown>,
    /// Earlier messages of its reply chain outside them, oldest first.
    pub chain: Vec<Shown>,
    /// What came after it, oldest first.
    pub after: Vec<Shown>,
    /// The people's texts shown before it (bots left out), for retrieval.
    pub texts: Vec<String>,
}

/// “Now” and the zone times are told in.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub now: i64,
    pub tz: chrono_tz::Tz,
}

fn cut(s: &str, n: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

// ---------------------------------------------------------------- names

/// A name that is really an id: empty, a provider handle (`ou_…`), a long number (a user id), a
/// qualified handle (`realm:id:value`) or an email address.
pub fn raw_name(n: &str) -> bool {
    let n = n.trim().trim_start_matches('@');
    n.is_empty()
        || steve_modules::accounts::looks_like_handle(n)
        || (n.len() >= 5 && n.chars().all(|c| c.is_ascii_digit()))
        || (n.contains(':') && !n.contains(char::is_whitespace))
        || (n.contains('@') && n.contains('.') && !n.contains(char::is_whitespace))
}

/// The first of `names` that is a name and not an id ([`raw_name`]); empty when none is.
pub fn shown_name<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    names
        .into_iter()
        .map(str::trim)
        .find(|n| !raw_name(n))
        .unwrap_or_default()
        .to_string()
}

/// `name（我）` and the like: what a line's sender is called. `owner`: your name (the address the
/// reply uses), shown for your own lines whatever your account's display name is.
pub fn label(name: &str, side: Side, mode: Mode, owner: &str) -> String {
    let some = |n: &str| {
        if n.trim().is_empty() {
            "某人".to_string()
        } else {
            n.trim().to_string()
        }
    };
    let you = if owner.trim().is_empty() {
        some(name)
    } else {
        owner.trim().to_string()
    };
    match (side, mode) {
        (Side::Other, _) => some(name),
        (Side::Owner, Mode::Own) => format!("{you}（我）"),
        (Side::OwnerViaSteve, Mode::Own) => format!("{you}（我，经 Steve 发出）"),
        (Side::OwnerAuto, Mode::Own) => format!("{you}（我的账号上另一个助手自动发的）"),
        (Side::Owner | Side::OwnerViaSteve, Mode::Bot) => format!("{you}（主人）"),
        (Side::OwnerAuto, Mode::Bot) => format!("{you}（主人账号上的自动回复）"),
        (Side::Persona, _) => format!("{}（你发出的）", some(name)),
    }
}

/// The block that says who you are in the chat: your names (ids left out) and what the labels mean.
pub fn owner_block(names: &[String], mode: Mode) -> String {
    let names: Vec<&str> = names
        .iter()
        .map(|n| n.trim().trim_start_matches('@'))
        .filter(|n| !raw_name(n))
        .collect();
    match mode {
        Mode::Own => {
            let mut s = String::new();
            if !names.is_empty() {
                s.push_str(&format!(
                    "我在这里的名字：{}。别人 @ 或叫这些名字，就是在找我。\n",
                    names.join("、")
                ));
            }
            s.push_str(
                "对话里标「（我）」的是我自己说的话，标「（我，经 Steve 发出）」的是之前替我发出去的话；\
                 这些你都看得到，不要再问我说过什么。",
            );
            s
        }
        Mode::Bot => {
            let mut s = String::new();
            if !names.is_empty() {
                s.push_str(&format!(
                    "主人在这里的名字：{}。别人 @ 或叫这些名字是在找主人，不是在找你。\n",
                    names.join("、")
                ));
            }
            s.push_str(
                "对话里标「（主人）」的是主人自己说的，标「（你发出的）」的是你这个身份发过的消息。",
            );
            s
        }
    }
}

// ---------------------------------------------------------------- the occasion

/// Why the message reached Steve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A private chat.
    Dm,
    /// You were @-mentioned.
    Mentioned,
    /// It answers your message.
    ReplyToYou,
    /// You asked with your trigger words.
    TriggerWords,
    /// Said in the space without addressing you.
    Other,
}

impl Why {
    /// From the rules' facts (`dm`, `mentioned`, `reply_to_me`) and whether trigger words asked.
    pub fn of(facts: &Value, trigger: bool) -> Why {
        let on = |k: &str| facts[k].as_bool().unwrap_or(false);
        if trigger {
            Why::TriggerWords
        } else if on("dm") {
            Why::Dm
        } else if on("mentioned") {
            Why::Mentioned
        } else if on("reply_to_me") {
            Why::ReplyToYou
        } else {
            Why::Other
        }
    }
}

/// Where and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Occasion {
    pub dm: bool,
    pub space_name: String,
    /// People in the chat (0: unknown).
    pub members: i64,
    pub external: bool,
    /// The sender, labelled.
    pub sender: String,
    pub why: Why,
    /// When the message was written (ms).
    pub at: i64,
    /// Other people explicitly `@`-mentioned on this message (populated deterministically from
    /// structured mentions, never guessed from prose; #258).
    pub mentions_others: Vec<String>,
}

const WEEKDAYS: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

fn local(at: i64, tz: chrono_tz::Tz) -> Option<chrono::DateTime<chrono_tz::Tz>> {
    tz.timestamp_millis_opt(at).single()
}

/// The 【场合】 block.
pub fn occasion_block(o: &Occasion, mode: Mode, clock: &Clock) -> String {
    let me = if mode == Mode::Own { "我" } else { "你" };
    let mut lines = Vec::new();
    if o.dm {
        lines.push(format!("私聊：{} 和{me}一对一。", o.sender));
    } else {
        let mut g = format!("群聊「{}」", o.space_name.trim());
        let mut facts = Vec::new();
        if o.members > 0 {
            facts.push(format!("{} 人", o.members));
        }
        if o.external {
            facts.push("有外部的人".to_string());
        }
        if !facts.is_empty() {
            g.push_str(&format!("（{}）", facts.join("，")));
        }
        g.push('。');
        lines.push(g);
    }
    lines.push(match o.why {
        Why::Dm => format!("为什么轮到你：{} 私聊{me}。", o.sender),
        Why::Mentioned => format!("为什么轮到你：{} 在群里 @ 了{me}。", o.sender),
        Why::ReplyToYou => format!("为什么轮到你：{} 回复了{me}的消息。", o.sender),
        Why::TriggerWords => "为什么轮到你：我用触发词叫你回这条。".to_string(),
        Why::Other => format!("为什么轮到你：{} 在这里说话，没有直接找{me}。", o.sender),
    });
    if !o.dm && !o.mentions_others.is_empty() {
        let extra = match o.why {
            Why::Mentioned => format!("同时还 @ 了：{}。", o.mentions_others.join("、")),
            _ => format!(
                "这条消息明确 @ 了：{}（没有 @ {me}）。",
                o.mentions_others.join("、")
            ),
        };
        lines.push(extra);
    }
    if let Some(n) = local(clock.now, clock.tz) {
        lines.push(format!(
            "现在是 {} {} {}（本地时间）；这条消息的时间：{}。",
            n.format("%Y-%m-%d"),
            WEEKDAYS[n.weekday().num_days_from_monday() as usize],
            n.format("%H:%M"),
            relative(o.at, clock)
        ));
    }
    lines.join("\n")
}

/// `刚刚`, `5 分钟前`, `今天 09:12`, `昨天 21:40`, `11月3日 10:00`, `2025-12-30 10:00`.
pub fn relative(at: i64, clock: &Clock) -> String {
    let d = clock.now - at;
    if d < 60_000 {
        return "刚刚".into();
    }
    if d < 3_600_000 {
        return format!("{} 分钟前", d / 60_000);
    }
    let (Some(t), Some(n)) = (local(at, clock.tz), local(clock.now, clock.tz)) else {
        return format!("{} 小时前", d / 3_600_000);
    };
    let days = (n.date_naive() - t.date_naive()).num_days();
    let hm = t.format("%H:%M");
    match days {
        0 => format!("今天 {hm}"),
        1 => format!("昨天 {hm}"),
        _ if t.year() == n.year() => format!("{}月{}日 {hm}", t.month(), t.day()),
        _ => format!("{} {hm}", t.format("%Y-%m-%d")),
    }
}

fn gap_note(ms: i64) -> String {
    if ms >= 86_400_000 {
        format!("隔了 {} 天", ms / 86_400_000)
    } else if ms >= 3_600_000 {
        format!("隔了 {} 小时", ms / 3_600_000)
    } else {
        format!("隔了 {} 分钟", ms / 60_000)
    }
}

// ---------------------------------------------------------------- typed notes

/// A provider's placeholder for something it did not render, as a typed note; `None` when the text
/// is not one.
fn placeholder(t: &str) -> Option<String> {
    let t = t.trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    if inner.is_empty() || inner.chars().count() > 40 || inner.contains(['[', ']', '\n']) {
        return None;
    }
    Some(
        match inner {
            "image" | "图片" => "[图片：未读取内容]",
            "sticker" | "表情包" => "[表情包]",
            "system message" | "系统消息" => "[系统消息：入群、撤回、通话之类，没有正文]",
            "unsupported message" => "[卡片或暂不支持显示的消息：看不到内容]",
            "encrypted message" => "[加密消息：看不到内容]",
            "Generating..." => "[对方的 AI 还在生成，内容不完整]",
            "forwarded messages" | "merge_forward" => "[转发的聊天记录：看不到内容]",
            "audio" | "语音" => "[语音：没有转写]",
            "video" | "media" | "视频" => "[视频：未读取内容]",
            "file" | "文件" => "[文件：未读取内容]",
            "shared group" | "share_chat" => "[分享的群名片]",
            "shared user" | "share_user" => "[分享的个人名片]",
            "location" => "[位置]",
            _ if inner
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                && !inner.chars().all(|c| c.is_ascii_digit()) =>
            {
                return Some(format!("[表情：{inner}]"));
            }
            _ => return None,
        }
        .to_string(),
    )
}

/// The notes of a message's attachments: an image the composer read before shows what it saw
/// ([`DESCRIBED`]); anything else the unread note ([`crate::media::unread_note`]).
fn attachment_notes(attachments: &Value) -> Vec<String> {
    attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            if a["type"] == "card" {
                return None;
            }
            if a["type"] == "image"
                && let Some(d) = a[DESCRIBED]
                    .as_str()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
            {
                return Some(format!("[图片（之前读过）：{}]", cut(d, DESCRIBED_MAX)));
            }
            let n = crate::media::unread_note(&Value::Array(vec![a.clone()]));
            (!n.is_empty()).then_some(n)
        })
        .collect()
}

/// What a line shows: its text with a typed note per attachment; a provider placeholder
/// (`[image]`, `[unsupported message]` …) as a typed note — replaced by the attachments' notes when
/// it stands for them.
pub fn typed(text: &str, attachments: &Value) -> String {
    let notes = attachment_notes(attachments);
    let t = text.trim();
    if let Some(p) = placeholder(t) {
        return if notes.is_empty() { p } else { notes.join("") };
    }
    match (t.is_empty(), notes.is_empty()) {
        (_, true) => t.to_string(),
        (true, false) => notes.join(""),
        (false, false) => format!("{t}\n{}", notes.join("")),
    }
}

/// The message being answered: a provider placeholder as its typed note; when it stands for
/// attachments, those are read for the composer apart (`【当前消息的附件材料】`), so it points there.
pub fn typed_target(text: &str, attachments: &Value) -> String {
    let has = attachments
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x["type"] != "card"));
    match placeholder(text) {
        Some(_) if has => "（这条消息只有附件，内容见【当前消息的附件材料】）".into(),
        Some(p) => p,
        None => text.to_string(),
    }
}

/// Whether `t` is solely a provider media/non-text placeholder (`[image]`, `[图片]`, etc.).
pub(crate) fn is_placeholder(t: &str) -> bool {
    placeholder(t).is_some()
}

/// The message itself cannot be read: its text is only a provider placeholder (a card, a system
/// message, an image …), no image of it reached the model (`images_read`), and it carries no file
/// whose text could be read instead.
pub fn unreadable(text: &str, attachments: &Value, images_read: bool) -> bool {
    if placeholder(text).is_none() || images_read {
        return false;
    }
    !attachments
        .as_array()
        .into_iter()
        .flatten()
        .any(|a| a["type"] == "file" && matches!(a["kind"].as_str(), None | Some("file")))
}

// ---------------------------------------------------------------- arranging

/// The ids of `before` in the target's reply chain: the same provider thread when it holds more
/// than the target, sibling replies quoting the same parent, and the messages it answers
/// (`reply_to`, followed up).
fn chain_of(target: &Msg, before: &[Msg]) -> HashSet<String> {
    let mut ids = HashSet::new();
    if let Some(k) = target.thread_key.as_deref().filter(|k| !k.is_empty()) {
        for m in before {
            if m.thread_key.as_deref() == Some(k) {
                ids.insert(m.id.clone());
            }
        }
    }
    if let Some(parent) = target.reply_to.as_deref().filter(|r| !r.is_empty()) {
        for m in before {
            if m.reply_to.as_deref() == Some(parent) {
                ids.insert(m.id.clone());
            }
        }
    }
    let mut up = target.reply_to.clone();
    for _ in 0..before.len() {
        let Some(ext) = up.take().filter(|e| !e.is_empty()) else {
            break;
        };
        let Some(m) = before
            .iter()
            .find(|m| m.external_id.as_deref() == Some(ext.as_str()))
        else {
            break;
        };
        if !ids.insert(m.id.clone()) {
            break;
        }
        up = m.reply_to.clone();
    }
    ids
}

fn format_line_text(m: &Msg) -> String {
    let mut text = if let Some(ref to) = m.reply_to_sender
        && !to.is_empty()
    {
        format!("[回复 {to}] {}", m.text)
    } else {
        m.text.clone()
    };
    if !m.reactions.is_empty() {
        let parts: Vec<String> = m
            .reactions
            .iter()
            .take(8)
            .map(|r| {
                let who = if r.is_owner { "我" } else { r.who.as_str() };
                format!("{who}:{}", r.emoji)
            })
            .collect();
        text.push_str(&format!(" [表情：{}]", parts.join("、")));
    }
    text
}

/// Lines from messages (oldest first): runs of two or more bot messages fold into one line, and a
/// gap of [`SEGMENT_GAP_MS`] between two lines is noted.
fn lines(
    ms: &[&Msg],
    chain: &HashSet<String>,
    label: &dyn Fn(&Msg) -> String,
    clock: &Clock,
) -> Vec<Shown> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut last: Option<i64> = None;
    while i < ms.len() {
        let m = ms[i];
        if let Some(prev) = last
            && m.at - prev > SEGMENT_GAP_MS
        {
            out.push(Shown::Gap(gap_note(m.at - prev)));
        }
        let run = ms[i..].iter().take_while(|x| x.bot).count();
        if run >= 2 {
            let run_ms = &ms[i..i + run];
            let mut names: Vec<String> = Vec::new();
            for x in run_ms {
                let n = label(x);
                if !names.contains(&n) {
                    names.push(n);
                }
            }
            let end = run_ms[run - 1];
            out.push(Shown::Line {
                when: relative(end.at, clock),
                who: names.join("、"),
                text: format!(
                    "[{run} 条机器人消息，最后一条：{}]",
                    cut(&end.text, SAMPLE_MAX)
                ),
                chain: run_ms.iter().any(|x| chain.contains(&x.id)),
            });
            last = Some(end.at);
            i += run;
            continue;
        }
        out.push(Shown::Line {
            when: relative(m.at, clock),
            who: label(m),
            text: format_line_text(m),
            chain: chain.contains(&m.id),
        });
        last = Some(m.at);
        i += 1;
    }
    out
}

/// The conversation around `target`. `before` and `after` are oldest first. Before it: the
/// stretches of talk back from the message, at most `segments` of them and `n` lines by people
/// (bots don't count), then earlier lines of its reply chain apart ([`CHAIN_EARLIER`]); after it, at
/// most [`AFTER`] lines.
pub fn arrange(
    target: &Msg,
    before: &[Msg],
    after: &[Msg],
    n: usize,
    segments: usize,
    label: &dyn Fn(&Msg) -> String,
    clock: &Clock,
) -> Conversation {
    let chain = chain_of(target, before);
    let mut taken: Vec<&Msg> = Vec::new();
    let (mut people, mut segment, mut next_at) = (0usize, 0usize, target.at);
    for m in before.iter().rev() {
        if next_at - m.at > SEGMENT_GAP_MS {
            segment += 1;
        }
        if segment >= segments || (!m.bot && people >= n) {
            break;
        }
        if !m.bot {
            people += 1;
        }
        taken.push(m);
        next_at = m.at;
    }
    taken.reverse();
    let shown: HashSet<&str> = taken.iter().map(|m| m.id.as_str()).collect();
    let mut earlier: Vec<&Msg> = before
        .iter()
        .filter(|m| chain.contains(&m.id) && !shown.contains(m.id.as_str()))
        .collect();
    earlier = earlier.split_off(earlier.len().saturating_sub(CHAIN_EARLIER));
    let after: Vec<&Msg> = after.iter().take(AFTER).collect();
    let none = HashSet::new();
    Conversation {
        texts: taken
            .iter()
            .filter(|m| !m.bot)
            .map(|m| m.text.clone())
            .collect(),
        before: lines(&taken, &chain, label, clock),
        chain: lines(&earlier, &none, label, clock),
        after: lines(&after, &none, label, clock),
    }
}

// ---------------------------------------------------------------- the store

pub use steve_store::channel_message_repo::presence::Mine;

pub fn mine(r: &mut impl Read, ws: &str, space: &str) -> ChannelResult<Mine> {
    Ok(steve_store::channel_message_repo::presence::mine(
        r, ws, space,
    )?)
}

/// Every name you go by here: `address` (what the reply calls you), your contacts' names and
/// aliases, your accounts' display names. Ids and raw handles left out ([`raw_name`]), each once.
pub fn owner_names(r: &mut impl Read, mine: &Mine, address: &str) -> ChannelResult<Vec<String>> {
    let mut all = vec![address.to_string()];
    for p in &mine.parties {
        if let Some(party) = steve_store::Contacts::new(r)
            .party(p)?
            .filter(|p| p.status != "deleted")
        {
            all.push(party.name);
            all.extend(party.aliases);
        }
    }
    for a in &mine.accounts {
        if let Some(account) = steve_store::Accounts(r)
            .get(a)?
            .filter(|a| a.status != "deleted")
        {
            all.push(account.display_name);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for n in all {
        let n = n.trim().trim_start_matches('@').to_string();
        if !raw_name(&n) && n.chars().count() <= 40 && !out.contains(&n) {
            out.push(n);
        }
    }
    Ok(out)
}

fn parse_mentions(raw: &Value, mine: &Mine) -> (Vec<MentionedPerson>, bool) {
    let mut out = Vec::new();
    let mut mentions_owner = false;
    for entry in raw.as_array().into_iter().flatten() {
        let account_id = entry["account_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        let name = entry["name"]
            .as_str()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let is_owner = account_id
            .as_deref()
            .is_some_and(|a| mine.accounts.iter().any(|m| m == a));
        if is_owner {
            mentions_owner = true;
        }
        if account_id.is_some() || !name.is_empty() {
            out.push(MentionedPerson {
                account_id,
                name,
                is_owner,
            });
        }
    }
    (out, mentions_owner)
}

fn msg_of(row: dialogue::Record, mine: &Mine, mode: Mode) -> Msg {
    let dialogue::Record {
        direction: dir,
        account,
        party,
        persona,
        on_behalf,
        text,
        attachments,
        mentions: raw_mentions,
        display_name: display,
        party_name,
        handle,
        account_kind: kind,
        contact_kind,
        persona_name,
        id,
        at,
        thread,
        external,
        reply_to,
        ..
    } = row;
    let yours = party.as_ref().is_some_and(|p| mine.parties.contains(p))
        || account.as_ref().is_some_and(|a| mine.accounts.contains(a));
    let side = if dir == "out" {
        if mode == Mode::Own && persona.is_none() && on_behalf == "none" {
            Side::OwnerViaSteve
        } else {
            Side::Persona
        }
    } else if yours {
        if crate::arbiter::looks_automated(&text) {
            Side::OwnerAuto
        } else {
            Side::Owner
        }
    } else {
        Side::Other
    };
    let name = if side == Side::Persona {
        shown_name([persona_name.as_str(), display.as_str()])
    } else {
        shown_name([display.as_str(), party_name.as_str(), handle.as_str()])
    };
    let (mentions, mentions_owner) = parse_mentions(&raw_mentions, mine);
    Msg {
        id,
        at,
        name,
        side,
        bot: side == Side::Other
            && (matches!(kind.as_str(), "bot" | "app_bot") || contact_kind == "bot"),
        text: typed(&text, &attachments),
        thread_key: thread,
        external_id: external,
        reply_to,
        mentions,
        mentions_owner,
        reply_to_sender: None,
        reply_to_is_owner: false,
        reactions: Vec::new(),
    }
}

/// The target message, the messages before it and the messages after it ([`load`]).
pub type Loaded = (Msg, Vec<Msg>, Vec<Msg>);

/// The target message and the space's messages before (the latest [`SCAN`]) and after it (the
/// first [`AFTER`]), all oldest first, by when they were said. `None` when the target is
/// not in the space.
///
/// The store is read around the target by `created`, which its index orders; the rows are then
/// split and sorted by when they were said: a line Steve sent for you that was drafted before the message but
/// sent after it comes after it, at its sending time.
pub fn load(
    r: &mut impl Read,
    space: &str,
    target: &str,
    mine: &Mine,
    mode: Mode,
) -> ChannelResult<Option<Loaded>> {
    let Some((target, records)) =
        dialogue::window(r, space, target, SCAN as u64, (AFTER + AFTER_SLACK) as u64)?
    else {
        return Ok(None);
    };
    let t_created = target.created;
    let mut t = msg_of(target, mine, mode);
    let mut rows: Vec<_> = records
        .into_iter()
        .map(|row| {
            let created = row.created;
            (msg_of(row, mine, mode), created)
        })
        .collect();
    // Deterministically fetch an out-of-window quoted parent so group quotes never lose context (#258).
    if let Some(ref parent_ext) = t.reply_to
        && !parent_ext.is_empty()
        && !rows
            .iter()
            .any(|(m, _)| m.external_id.as_deref() == Some(parent_ext.as_str()))
        && let Some(parent_row) = dialogue::by_external(r, space, parent_ext)?
        && parent_row.id != t.id
    {
        let created = parent_row.created;
        rows.push((msg_of(parent_row, mine, mode), created));
    }
    rows.sort_by(|(a, ac), (b, bc)| (a.at, *ac, &a.id).cmp(&(b.at, *bc, &b.id)));
    let (before, after): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|(m, created)| (m.at, *created) <= (t.at, t_created));
    let skip = before.len().saturating_sub(SCAN);
    let mut before: Vec<Msg> = before.into_iter().skip(skip).map(|(m, _)| m).collect();
    let mut after: Vec<Msg> = after.into_iter().take(AFTER).map(|(m, _)| m).collect();

    // Resolve quote targets (`reply_to_sender`, `reply_to_is_owner`) deterministically by external_id.
    let mut by_ext: HashMap<String, (String, bool)> = HashMap::new();
    for m in before.iter().chain(std::iter::once(&t)).chain(after.iter()) {
        if let Some(ref ext) = m.external_id
            && !ext.is_empty()
        {
            let owner_side = matches!(m.side, Side::Owner | Side::OwnerViaSteve);
            let who = if owner_side {
                "我".to_string()
            } else if m.name.is_empty() {
                "某人".to_string()
            } else {
                m.name.clone()
            };
            by_ext.insert(ext.clone(), (who, owner_side));
        }
    }
    let resolve_quote = |m: &mut Msg| {
        if let Some(ref parent) = m.reply_to
            && let Some((who, is_owner)) = by_ext.get(parent)
        {
            m.reply_to_sender = Some(who.clone());
            m.reply_to_is_owner = *is_owner;
        }
    };
    resolve_quote(&mut t);
    for m in &mut before {
        resolve_quote(m);
    }
    for m in &mut after {
        resolve_quote(m);
    }

    // Batch-load active incoming reactions for the target and recent window (single query).
    let mut ids: Vec<String> = vec![t.id.clone()];
    for m in before.iter().rev().take(40) {
        ids.push(m.id.clone());
    }
    for m in after.iter().take(AFTER) {
        ids.push(m.id.clone());
    }
    let mut rx_map: HashMap<String, Vec<MsgReaction>> = HashMap::new();
    for (mid, rx) in
        steve_store::channel_message_repo::incoming_reactions::list_for_messages(r, &ids, -1)?
    {
        let is_owner = mine.accounts.iter().any(|a| a == &rx.actor_account_id);
        let who = if is_owner {
            "我".to_string()
        } else if raw_name(&rx.actor_name) {
            "某人".to_string()
        } else {
            rx.actor_name
        };
        rx_map.entry(mid).or_default().push(MsgReaction {
            who,
            emoji: rx.emoji,
            is_owner,
            origin: rx.origin,
        });
    }
    if let Some(rx) = rx_map.remove(&t.id) {
        t.reactions = rx;
    }
    for m in &mut before {
        if let Some(rx) = rx_map.remove(&m.id) {
            m.reactions = rx;
        }
    }
    for m in &mut after {
        if let Some(rx) = rx_map.remove(&m.id) {
            m.reactions = rx;
        }
    }

    Ok(Some((t, before, after)))
}

/// Drafts in the space still waiting (not sent yet), newest first: `(when written, text)`.
pub fn pending(r: &mut impl Read, space: &str) -> ChannelResult<Vec<(i64, String)>> {
    Ok(dialogue::pending(r, space, PENDING as u64)?)
}

/// A pending draft as a line.
pub fn pending_line(at: i64, text: &str, clock: &Clock) -> String {
    format!("- {}：{}", relative(at, clock), cut(text, LINE_MAX))
}

/// The rules' facts recorded with the message's event (`dm`, `mentioned`, `reply_to_me`).
pub fn facts_of(r: &mut impl Read, message: &str) -> ChannelResult<Value> {
    Ok(dialogue::facts(r, message)?)
}

/// Keeps what the composer read in the message's images ([`DESCRIBED`]), so later conversations show
/// it instead of an unread image. Only images not described yet; one line, at most
/// [`DESCRIBED_MAX`] characters.
pub fn remember_described(
    w: &mut steve_store::W<'_>,
    message: &str,
    text: &str,
) -> ChannelResult<bool> {
    let d: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let d = cut(&d, DESCRIBED_MAX);
    if d.is_empty() {
        return Ok(false);
    }
    let Some(mut v) = dialogue::describable(w, message)? else {
        return Ok(false);
    };
    let mut changed = false;
    for a in v.as_array_mut().into_iter().flatten() {
        if a["type"] == "image" && a.get(DESCRIBED).is_none() {
            a[DESCRIBED] = Value::String(d.clone());
            changed = true;
        }
    }
    if changed {
        dialogue::set_attachments(w, message, &v)?;
    }
    Ok(changed)
}

/// Your own account on your own connection shows your name, not an id: when its display name is
/// empty, a number or a handle ([`raw_name`]) — what a provider gives for “me” — it takes the name of
/// another account of the same contact (your Feishu account), else the contact's name. A name that
/// is a real name is never touched. Returns whether it changed.

pub fn settle_own_name(w: &mut steve_store::W<'_>, account: &str) -> ChannelResult<bool> {
    use steve_store::Accounts;

    let Some(current) = Accounts(w).get(account)? else {
        return Ok(false);
    };
    if !raw_name(&current.display_name) {
        return Ok(false);
    }
    let mut siblings = Accounts(w).of_party(&current.workspace_id, &current.party_id)?;
    siblings.retain(|a| a.id != account && a.status != "deleted");
    siblings.sort_by_key(|a| (!a.verified, a.created));
    let party_name = w
        .contacts()
        .party(&current.party_id)?
        .map(|p| p.name)
        .unwrap_or_default();
    let name = shown_name(
        siblings
            .iter()
            .map(|a| a.display_name.as_str())
            .chain([party_name.as_str()]),
    );
    if name.is_empty() {
        return Ok(false);
    }
    Accounts(w).update(
        account,
        &steve_store::account_rows::AccountPatch {
            display_name: Some(name),
            ..Default::default()
        },
        None,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(now: &str) -> Clock {
        Clock {
            now: chrono::DateTime::parse_from_rfc3339(now)
                .unwrap()
                .timestamp_millis(),
            tz: chrono_tz::Asia::Shanghai,
        }
    }

    fn at(t: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(t)
            .unwrap()
            .timestamp_millis()
    }

    fn m(id: &str, t: &str, name: &str, side: Side, text: &str) -> Msg {
        Msg {
            id: id.into(),
            at: at(t),
            name: name.into(),
            side,
            text: text.into(),
            thread_key: Some(format!("th_{id}")),
            ..Default::default()
        }
    }

    fn own_label(x: &Msg) -> String {
        label(&x.name, x.side, Mode::Own, "主人")
    }

    #[test]
    fn ids_are_not_names() {
        for raw in [
            "",
            "7300000000000000001",
            "ou_3f2a9c7d1e",
            "lark-1:id:7300000000000000001",
            "a@b.com",
            "@ou_3f2a9c7d1e",
        ] {
            assert!(raw_name(raw), "{raw}");
        }
        for name in ["主人", "Lee", "A", "王 2", "12"] {
            assert!(!raw_name(name), "{name}");
        }
        assert_eq!(shown_name(["7300000000000000001", "", "主人"]), "主人");
    }

    #[test]
    fn your_lines_are_labelled_by_side_not_by_name() {
        // your account's display name is a number: your lines still read as you
        assert_eq!(
            label("7300000000000000001", Side::Owner, Mode::Own, "主人"),
            "主人（我）"
        );
        assert_eq!(
            label("", Side::OwnerViaSteve, Mode::Own, "主人"),
            "主人（我，经 Steve 发出）"
        );
        assert_eq!(label("A", Side::Other, Mode::Own, "主人"), "A");
        assert_eq!(label("", Side::Other, Mode::Own, "主人"), "某人");
        assert_eq!(
            label("主人", Side::Owner, Mode::Bot, "主人"),
            "主人（主人）"
        );
        assert_eq!(
            label("小助", Side::Persona, Mode::Bot, "主人"),
            "小助（你发出的）"
        );
        let b = owner_block(
            &["主人".into(), "7300000000000000001".into(), "Lee".into()],
            Mode::Own,
        );
        assert!(b.starts_with("我在这里的名字：主人、Lee。"), "{b}");
        assert!(!b.contains("730000"), "{b}");
        assert!(b.contains("（我，经 Steve 发出）"));
    }

    #[test]
    fn placeholders_become_typed_notes() {
        let none = Value::Array(vec![]);
        assert_eq!(
            typed("[unsupported message]", &none),
            "[卡片或暂不支持显示的消息：看不到内容]"
        );
        assert_eq!(
            typed("[system message]", &none),
            "[系统消息：入群、撤回、通话之类，没有正文]"
        );
        assert_eq!(typed("[sticker]", &none), "[表情包]");
        assert_eq!(
            typed("[Lark_Emoji_Smile_0]", &none),
            "[表情：Lark_Emoji_Smile_0]"
        );
        assert_eq!(typed("[12]", &none), "[12]");
        assert_eq!(typed("普通的话 [不是占位]", &none), "普通的话 [不是占位]");
        // an image: unread unless the composer read it before
        let img = serde_json::json!([{"type": "image", "key": "k"}]);
        assert_eq!(typed("[image]", &img), "[图片：未读取内容]");
        let seen = serde_json::json!([{"type": "image", "key": "k", "described": "一张报错截图，写着连接超时"}]);
        assert_eq!(
            typed("[image]", &seen),
            "[图片（之前读过）：一张报错截图，写着连接超时]"
        );
        assert_eq!(
            typed("看这个", &seen),
            "看这个\n[图片（之前读过）：一张报错截图，写着连接超时]"
        );
        // only an attachment, no text: kept, as its note
        assert_eq!(typed("", &img), "[图片：未读取内容]");
    }

    #[test]
    fn relative_times() {
        let c = clock("2026-01-05T10:00:00+08:00");
        assert_eq!(relative(at("2026-01-05T09:59:30+08:00"), &c), "刚刚");
        assert_eq!(relative(at("2026-01-05T09:55:00+08:00"), &c), "5 分钟前");
        assert_eq!(relative(at("2026-01-05T07:10:00+08:00"), &c), "今天 07:10");
        assert_eq!(relative(at("2026-01-04T21:40:00+08:00"), &c), "昨天 21:40");
        assert_eq!(
            relative(at("2026-01-01T08:00:00+08:00"), &c),
            "1月1日 08:00"
        );
        assert_eq!(
            relative(at("2025-12-30T08:00:00+08:00"), &c),
            "2025-12-30 08:00"
        );
    }

    /// A private chat gives every message its own thread: the conversation is read by time, the
    /// stretch of the message and the one before it.
    #[test]
    fn a_private_chat_reads_by_time_not_by_thread() {
        let c = clock("2026-01-05T10:02:00+08:00");
        let before = vec![
            m(
                "old",
                "2026-01-04T15:00:00+08:00",
                "B",
                Side::Other,
                "很早的事",
            ),
            m(
                "p1",
                "2026-01-05T08:00:00+08:00",
                "B",
                Side::Other,
                "上一段",
            ),
            m("p2", "2026-01-05T08:01:00+08:00", "主人", Side::Owner, "好"),
            m("c1", "2026-01-05T09:59:00+08:00", "B", Side::Other, "在吗"),
            m("c2", "2026-01-05T10:00:00+08:00", "主人", Side::Owner, "在"),
        ];
        let target = m(
            "t",
            "2026-01-05T10:01:00+08:00",
            "B",
            Side::Other,
            "那个连的是哪个",
        );
        let conv = arrange(&target, &before, &[], 20, SEGMENTS, &own_label, &c);
        let rendered: Vec<String> = conv.before.iter().map(Shown::render).collect();
        assert_eq!(
            rendered,
            vec![
                "- 今天 08:00 B：上一段",
                "- 今天 08:01 主人（我）：好",
                "  ……隔了 1 小时……",
                "- 3 分钟前 B：在吗",
                "- 2 分钟前 主人（我）：在",
            ],
            "{rendered:#?}"
        );
        assert!(conv.chain.is_empty());
        // wider: every stretch
        let wide = arrange(&target, &before, &[], 80, SEGMENTS_WIDE, &own_label, &c);
        assert_eq!(wide.texts.first().map(String::as_str), Some("很早的事"));
        // the line limit counts people only
        assert_eq!(
            arrange(&target, &before, &[], 1, SEGMENTS, &own_label, &c).texts,
            vec!["在"]
        );
    }

    /// In a group the reply chain is a hint: its lines are marked, and earlier ones outside the
    /// stretches are added apart.
    #[test]
    fn the_reply_chain_is_a_hint_in_a_group() {
        let c = clock("2026-01-05T10:02:00+08:00");
        let mut root = m(
            "r",
            "2026-01-05T07:00:00+08:00",
            "A",
            Side::Other,
            "方案定了吗",
        );
        root.thread_key = Some("th_x".into());
        let mut own = m(
            "o",
            "2026-01-05T10:00:00+08:00",
            "主人",
            Side::Owner,
            "加个定时就好",
        );
        own.thread_key = Some("th_x".into());
        let other = m(
            "y",
            "2026-01-05T10:00:30+08:00",
            "C",
            Side::Other,
            "别的话题",
        );
        let mut target = m(
            "t",
            "2026-01-05T10:01:00+08:00",
            "B",
            Side::Other,
            "@主人 不顶用",
        );
        target.thread_key = Some("th_x".into());
        let before = vec![root, own, other];
        let conv = arrange(&target, &before, &[], 20, 1, &own_label, &c);
        let r: Vec<String> = conv.before.iter().map(Shown::render).collect();
        assert_eq!(
            r,
            vec![
                "- 2 分钟前 〔同一回复链〕主人（我）：加个定时就好",
                "- 1 分钟前 C：别的话题",
            ]
        );
        let ch: Vec<String> = conv.chain.iter().map(Shown::render).collect();
        assert_eq!(ch, vec!["- 今天 07:00 A：方案定了吗"]);
    }

    #[test]
    fn bot_runs_fold_and_what_came_after_is_kept() {
        let c = clock("2026-01-05T10:05:00+08:00");
        let mut before = vec![m(
            "h",
            "2026-01-05T09:58:00+08:00",
            "A",
            Side::Other,
            "看下告警",
        )];
        for i in 0..4 {
            let mut b = m(
                &format!("b{i}"),
                &format!("2026-01-05T09:59:0{i}+08:00"),
                "告警机器人",
                Side::Other,
                &format!("P2 告警 {i}"),
            );
            b.bot = true;
            before.push(b);
        }
        let target = m(
            "t",
            "2026-01-05T10:00:00+08:00",
            "A",
            Side::Other,
            "怎么回事",
        );
        let after = vec![m(
            "a1",
            "2026-01-05T10:03:00+08:00",
            "A",
            Side::Other,
            "好了",
        )];
        let conv = arrange(&target, &before, &after, 1, SEGMENTS, &own_label, &c);
        let r: Vec<String> = conv.before.iter().map(Shown::render).collect();
        assert_eq!(
            r,
            vec![
                "- 7 分钟前 A：看下告警",
                "- 5 分钟前 告警机器人：[4 条机器人消息，最后一条：P2 告警 3]",
            ]
        );
        assert_eq!(conv.texts, vec!["看下告警"]);
        assert_eq!(conv.after[0].render(), "- 2 分钟前 A：好了");
    }

    #[test]
    fn the_occasion_says_where_why_and_when() {
        let c = clock("2026-01-05T10:02:00+08:00");
        let o = Occasion {
            dm: false,
            space_name: "示例群".into(),
            members: 12,
            external: true,
            sender: "B".into(),
            why: Why::of(&serde_json::json!({"mentioned": true}), false),
            at: at("2026-01-05T10:01:00+08:00"),
            mentions_others: Vec::new(),
        };
        let b = occasion_block(&o, Mode::Own, &c);
        assert_eq!(
            b,
            "群聊「示例群」（12 人，有外部的人）。\n为什么轮到你：B 在群里 @ 了我。\n现在是 2026-01-05 周一 10:02（本地时间）；这条消息的时间：1 分钟前。"
        );
        let dm = Occasion {
            dm: true,
            why: Why::of(&serde_json::json!({"dm": true}), false),
            ..o
        };
        assert!(
            occasion_block(&dm, Mode::Own, &c)
                .starts_with("私聊：B 和我一对一。\n为什么轮到你：B 私聊我。")
        );
        assert_eq!(
            Why::of(&serde_json::json!({"dm": true}), true),
            Why::TriggerWords
        );
    }
}

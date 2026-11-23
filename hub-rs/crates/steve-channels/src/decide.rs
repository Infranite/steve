//! When Steve replies for you, says nothing, or asks you — on your own account (`Mode::Own`) — and
//! the card it opens when it asks. Code decides what code can decide; the model only reads the
//! conversation and writes a line ([`crate::compose`]).
//!
//! **Gates before any model**, in order, each one recorded on the message's event:
//! - G0 a bot or the system: recorded ([`crate::inbound`] `LABEL_BOT`). A bot's notice becomes a
//!   proposed item only by its structure or your setting, never by its words ([`bot_notice`]): in a
//!   private chat with the bot, a card that carries an action link, or any message of a bot whose
//!   space you set to `item` (`spaces::BotNotices`); the same link is proposed once.
//! - G1, G2 you answered, or you are chatting there ([`crate::presence`]);
//! - G3 the sender is still going on: their messages are answered as one turn
//!   ([`crate::presence::take_turn`]);
//! - G4 nothing to answer: the turn has no words (a system message, a sticker, emoji, a card Steve
//!   cannot read) and the sender has no question of theirs waiting ([`Gate::NoContent`]);
//! - G5 a nudge: a turn of only emoji or question marks while a card about the sender's earlier
//!   message is still open: no new card, the open one says they asked again ([`Gate::Nudge`]).
//!
//! **The model's answer** is `{outcome, reason, addressee, text, draft, needs_commitment, summary}`
//! ([`crate::compose::OUTPUT`]; a bot's account keeps [`crate::compose::OUTPUT_BOT`]). What it may do is checked here in code ([`settle`]): saying nothing
//! is the default; a question to you only for a commitment (time, money, deleting, privacy, a
//! relationship) or, when the message is for you, a fact only you know or a substantive message;
//! the card never asks you what you said and never carries Steve's own confusion. After the wider
//! history, a message still not understood is a question to you only when it asks you directly
//! ([`crate::compose::after_wide`]); anything else is let go.
//!
//! **Starred contacts** (a contact you marked important, `parties.important` — the star on the
//! contact page, set in Steve, not read from Feishu/Lark): their messages to you always reach you as
//! a card marked [`STARRED`], unless you answered or are there already. The composer still writes a
//! candidate reply for it when the rules let it draft; the star alone never makes a draft. Apart from
//! the card, the message is told to you as a personal notification ([`crate::starred`]).
//!
//! **The card** ([`open_card`]): one sentence on who wants what (the model's, never cut short below
//! [`SUMMARY_MAX`]), why you are needed, the last few lines with yours marked “我”, and a candidate
//! draft when there is one. Its options: send the draft, tell Steve how to answer (your words go to
//! the composer as an instruction), send your words as they are, make it an item, or leave it. It
//! closes when you answer it, when you speak there ([`crate::presence::supersede`]), when a newer
//! card shows its message from the same sender or reply chain ([`supersede_older`]), or — only when
//! you set a lifetime on the account (`card_ttl_h`, none by default) — once it is that old. A starred
//! contact's card and one asking you to commit to something never expire.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_store::base::{self, DecisionKind};
use steve_store::{NewDecision, Tx};

use crate::ChannelResult;
use crate::context::{self, Side, Why};
use crate::deliver::Outcome;
use crate::respond::{Plan, SUBJECT_MESSAGE};
use crate::spaces::{self, BotNotices, Mode};

/// The card's summary, in characters at most (the model is asked for one sentence; this only
/// bounds a runaway answer).
pub const SUMMARY_MAX: usize = 300;
/// Lines of the conversation on a card, the message itself included.
pub const EXCERPT_MAX: usize = 6;
/// One excerpt line, in characters.
pub const EXCERPT_LINE_MAX: usize = 80;
/// The longest lifetime an account may give its cards (`channel_settings.card_ttl_h`, in hours; unset:
/// cards do not expire on time).
pub const CARD_TTL_MAX_H: i64 = 30 * 24;
/// An earlier question of the sender counts as still waiting within this long before their turn
/// (one stretch of talk, [`context::SEGMENT_GAP_MS`]).
pub const OPEN_QUESTION_MS: i64 = context::SEGMENT_GAP_MS;
/// The same action link of a bot is proposed once within this long.
pub const BOT_NOTICE_DEDUPE_MS: i64 = 7 * 86_400_000;

/// The head of a card about a starred contact's message.
pub const STARRED: &str = "⭐ 星标联系人";
/// What a message without words says on its event (G4).
pub const LABEL_NO_CONTENT: &str = "没有要回的内容，只记录";
/// … a nudge added to the open card (G5).
pub const LABEL_NUDGE: &str = "对方在催，加到了之前的卡片上";
/// … a bot's notice proposed as an item (G0).
pub const LABEL_BOT_ITEM: &str = "机器人的通知，提成事项";
/// … a bot's notice whose action link was proposed already.
pub const LABEL_BOT_SEEN: &str = "同一条机器人通知已经提过事项，只记录";
/// `result.superseded` of a card closed by a newer one that shows its message
/// ([`supersede_older`]).
pub const BY_NEWER: &str = "同一个人或同一条回复链有了更新的卡片，这条在新卡片里";
/// `result.superseded` of a card closed after its account's lifetime (`card_ttl_h`).
pub const BY_EXPIRY: &str = "超过这个账号设定的时长没处理，已过期";
/// The line a nudge adds to the open card.
pub const NUDGED: &str = "对方又在催：";
/// Ahead of why you are needed, on a card's second line.
pub const WHY_HEAD: &str = "为什么要你：";

// ---------------------------------------------------------------- the model's answer

/// Why the composer needs you, or why it said nothing: the `reason` of its answer as a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A commitment for you: to look into or do something, a time, money, deleting something,
    /// privacy, a relationship.
    Commitment,
    /// A fact or judgment only you have, asked of you.
    OwnerFact,
    /// A substantive message to you (an analysis, a proposal) that you should read yourself.
    Substantive,
    /// A direct question to you whose context Steve cannot see.
    DirectQuestion,
    /// From a contact you starred.
    Starred,
    /// The draft did not sound like you after a second try ([`crate::voice::GAVE_UP`]).
    Voice,
    /// The reply speaks about you in the third person: it was meant for you, not for them.
    MeantForOwner,
    /// The space's rules hand messages to you.
    Rule,
    /// Your instruction did not come out as a reply.
    Instruction,
}

impl Reason {
    pub const ALL: [Reason; 9] = [
        Self::Commitment,
        Self::OwnerFact,
        Self::Substantive,
        Self::DirectQuestion,
        Self::Starred,
        Self::Voice,
        Self::MeantForOwner,
        Self::Rule,
        Self::Instruction,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::Commitment => "commitment",
            Self::OwnerFact => "owner_fact",
            Self::Substantive => "substantive",
            Self::DirectQuestion => "direct_question",
            Self::Starred => "starred",
            Self::Voice => "voice",
            Self::MeantForOwner => "meant_for_owner",
            Self::Rule => "rule",
            Self::Instruction => "instruction",
        }
    }

    /// The model's `reason` for a question to you, with the words it may use for a commitment
    /// (`time`, `money`, …). `None`: no reason Steve may ask you for.
    pub fn of_ask(s: &str) -> Option<Reason> {
        match s.trim().to_ascii_lowercase().as_str() {
            "commitment" | "promise" | "time" | "schedule" | "money" | "deletion" | "delete"
            | "privacy" | "relationship" => Some(Self::Commitment),
            "owner_fact" | "fact" | "owner_only" => Some(Self::OwnerFact),
            "substantive" | "long" => Some(Self::Substantive),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Option<Reason> {
        Self::ALL
            .into_iter()
            .find(|r| r.code() == s)
            .or_else(|| Self::of_ask(s))
    }

    /// Why you are needed, as the card says it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Commitment => "要答应或安排事情（时间、花钱、删除、隐私或人际关系），得你自己定",
            Self::OwnerFact => "要你才知道的情况或判断",
            Self::Substantive => "直接发给你的长消息，值得你自己看",
            Self::DirectQuestion => "直接问你的问题",
            Self::Starred => "星标联系人找你",
            Self::Voice => "起草的回复不像你会说的话",
            Self::MeantForOwner => "这条要你自己回",
            Self::Rule => "这里的规则把消息转给你",
            Self::Instruction => "按你说的没能写出回复",
        }
    }
}

/// The composer's answer, read ([`crate::compose::parse_answer`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answer {
    pub outcome: Outcome,
    /// A reply: what goes into the chat. Otherwise the model's note (a skip's reason).
    pub text: String,
    /// The older field: the reply needs your approval (it promises, spends or deletes).
    pub needs_approval: bool,
    pub why: String,
    pub reason: String,
    pub addressee: String,
    pub needs_commitment: bool,
    pub summary: String,
    pub draft: String,
}

/// What the gates know about the message an answer is for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Seen {
    /// Your own account (only there is anything settled here).
    pub own: bool,
    /// You said what to write (trigger words, a flow's text, your answer on a card): never
    /// overridden.
    pub instructed: bool,
    /// The message is to you by its facts: a private chat, an @ of you, a reply to you.
    pub addressed: bool,
    /// It asks something (a question mark).
    pub question: bool,
    /// Your names in the chat (ids left out).
    pub owner_names: Vec<String>,
}

impl Seen {
    /// To you: by its facts, or as the model read it (`addressee = owner`).
    pub fn to_owner(&self, a: &Answer) -> bool {
        self.addressed || a.addressee.trim() == "owner"
    }
}

/// A message that asks something: a question mark, or a sentence-final question particle (吗、呢、么)
/// before any trailing punctuation or emoji.
pub fn asks(text: &str) -> bool {
    if crate::voice::asks(text) {
        return true;
    }
    let last = text
        .trim_end_matches(|c: char| c.is_whitespace() || "。.!！~～…)）(（".contains(c) || emoji(c))
        .chars()
        .last();
    matches!(last, Some('吗' | '呢' | '么'))
}

/// Whether facts say the message is to you.
pub fn addressed(why: Why) -> bool {
    matches!(why, Why::Dm | Why::Mentioned | Why::ReplyToYou)
}

/// An explicit question to the owner about handling the incoming message, mislabelled as a reply.
/// A name alone is also valid in an introduction or a factual answer; it is not an addressee.
pub fn names_you(text: &str, names: &[String]) -> bool {
    let names_owner = names
        .iter()
        .map(|n| n.trim().trim_start_matches('@'))
        .filter(|n| n.chars().count() >= 2 && !context::raw_name(n))
        .any(|n| text.contains(n));
    let lower = text.to_ascii_lowercase();
    let private_english = ["this message", "their message", "incoming message"]
        .iter()
        .any(|p| lower.contains(p))
        && [
            "would you like me",
            "do you want me",
            "shall i reply",
            "how should i reply",
            "how should i handle",
        ]
        .iter()
        .any(|p| lower.contains(p))
        && (names_owner || lower.contains("for you") || lower.contains("to you"));
    private_english
        || (names_owner || text.contains("发给你") || text.contains("找你"))
            && ["这条", "这段消息", "对方的消息"]
                .iter()
                .any(|p| text.contains(p))
            && [
                "要不要我",
                "你是想让我",
                "需要我帮",
                "要我帮",
                "让我怎么",
                "怎么回复",
                "怎么处理",
            ]
            .iter()
            .any(|p| text.contains(p))
}

/// Preserve private addressing and safety flags on both accounts. The remaining decision rules
/// run only on your own account, except where you explicitly instructed what to write.
///
/// - An explicit private question about handling the incoming message stays private
///   ([`Reason::MeantForOwner`]); it is never offered as an outward candidate draft.
/// - A question to you must say why ([`Reason::of_ask`]): a commitment always; a fact only you know
///   or a substantive message only when the message is to you. Without a reason it may give, a
///   message to you is still asked about (for what you know); anything else is let go: saying
///   nothing is the default.
/// - `needs_approval` (the safety rules' field, and a bot's) and `needs_commitment` (your own
///   account's) are one flag: either sets both, on any account, so every path that asks for
///   approval by either name sees it.
pub fn settle(mut a: Answer, s: &Seen) -> Answer {
    let commits = a.needs_commitment || a.needs_approval;
    a.needs_commitment = commits;
    a.needs_approval = commits;
    if !s.instructed && a.outcome == Outcome::Reply && names_you(&a.text, &s.owner_names) {
        a.outcome = Outcome::AskOwner;
        a.reason = Reason::MeantForOwner.code().into();
        a.draft.clear();
        return a;
    }
    if !s.own || s.instructed {
        return a;
    }
    let to_owner = s.to_owner(&a);
    if a.outcome == Outcome::AskOwner {
        match Reason::of_ask(&a.reason) {
            Some(Reason::Commitment) => a.reason = Reason::Commitment.code().into(),
            Some(r) if to_owner => a.reason = r.code().into(),
            None if to_owner => a.reason = Reason::OwnerFact.code().into(),
            _ => {
                a.outcome = Outcome::Skip;
                a.why = format!("{NOT_FOR_YOU}{}", a.why.trim());
                a.reason = "not_for_owner".into();
                a.draft.clear();
            }
        }
    }
    a
}

/// Ahead of the reason of a question turned into a skip by [`settle`].
pub const NOT_FOR_YOU: &str = "不是对你说的，也不用你答应什么，不用问你：";

/// Keeps what the composer said about a draft it wrote with the draft (`chan_messages.composed`):
/// its outcome, reason and whether it promises something for you. YOLO reads it: on your own account
/// it approves only a draft the composer wrote, and never one that needs a commitment.
pub fn keep_composed(tx: &Tx, message: &str, g: &crate::deliver::Generation) -> ChannelResult<()> {
    let v = json!({
        "outcome": g.outcome.code(),
        "reason": g.reason,
        "needs_commitment": g.needs_commitment || g.needs_approval,
        "model": g.model,
    });
    let mut v = v;
    // what the composer was shown of how you talk, for the disclosure check that runs later
    if !g.style_samples.is_empty() {
        v["style_samples"] = json!(g.style_samples);
    }
    tx.execute(
        "UPDATE chan_messages SET composed = ? WHERE id = ?",
        params![v.to_string(), message],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- the gates before the model

/// What the gates before the model decided for a message (G4, G5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Nothing to answer: recorded.
    NoContent,
    /// They asked again: the open card `decision` says so, no new one.
    Nudge { decision: String },
}

/// A Unicode emoji, its joiners and selectors.
fn emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF
        | 0xFE00..=0xFE0F | 0x200D | 0x20E3 | 0xE0020..=0xE007F)
}

/// Placeholders the adapters and the external channel write for a message that has no words: a
/// sticker, a system message (someone joined, a call ended), a card with nothing readable, and the
/// external channel's text for a message it could not render at all (when its raw form could not be
/// read either, [`crate::adapters::ext_ws`]). Never the generic `[<类型>消息]` of a type the Feishu
/// reader does not parse ([`crate::adapters::feishu::inbound::placeholder`]): a forwarded chat, a
/// task, a shared chat or a location carry something to read.
pub const WORDLESS_PLACEHOLDERS: [&str; 6] = [
    "[sticker]",
    crate::media::STICKER_NOTE,
    "[system message]",
    "[系统消息]",
    crate::adapters::feishu::card::EMPTY,
    "[unsupported message]",
];

/// A placeholder of something without words ([`WORDLESS_PLACEHOLDERS`], the Feishu reader's
/// wordless message types) or one of the platform's emoji names
/// ([`crate::adapters::feishu::emoji::is_emoji`]).
fn wordless_token(t: &str) -> bool {
    use crate::adapters::feishu::{emoji, inbound};
    if WORDLESS_PLACEHOLDERS.contains(&t)
        || inbound::WORDLESS_TYPES
            .iter()
            .any(|w| inbound::placeholder(w) == t)
    {
        return true;
    }
    t.strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .is_some_and(emoji::is_emoji)
}

/// An attachment without words: a sticker (as the Feishu reader stores it, `{"type": "file",
/// "kind": "sticker"}`, or `{"type": "sticker"}`), or a card with nothing readable (no blocks, not
/// cut short).
fn wordless_attachment(a: &Value) -> bool {
    match a["type"].as_str() {
        Some("sticker") => true,
        Some("file") => a["kind"] == "sticker",
        Some("card") => {
            a["truncated"] != true && a["blocks"].as_array().is_none_or(|b| b.is_empty())
        }
        _ => false,
    }
}

/// A message without words to answer, by its structure: nothing attached but stickers and empty
/// cards ([`wordless_attachment`]), and no text but Unicode emoji, the platform's emoji names and the
/// placeholders of something without words ([`wordless_token`]). An image, a file, a card with
/// text, a forwarded chat or a task has content.
pub fn content_free(text: &str, attachments: &Value) -> bool {
    if !attachments
        .as_array()
        .is_none_or(|a| a.iter().all(wordless_attachment))
    {
        return false;
    }
    let mut rest = text.trim();
    while !rest.is_empty() {
        if let Some(i) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
            let token = &rest[..i + 2];
            if !wordless_token(token) {
                return false;
            }
            rest = rest[i + 2..].trim_start();
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        if !(emoji(c) || c.is_whitespace()) {
            return false;
        }
        rest = &rest[c.len_utf8()..];
    }
    true
}

/// A nudge: [`content_free`], or only question marks, exclamation marks, dots and tildes.
pub fn nudge_only(text: &str, attachments: &Value) -> bool {
    let punct = |c: char| "？?！!。.…~～".contains(c) || c.is_whitespace();
    content_free(text, attachments) || (!text.trim().is_empty() && text.chars().all(punct))
}

fn message_text(c: &Connection, id: &str) -> ChannelResult<(String, Value, i64, String)> {
    Ok(c.query_row(
        "SELECT coalesce(text, ''), coalesce(attachments, '[]'), created,
                coalesce(sender_party_id, sender_account_id, '') FROM chan_messages WHERE id = ?",
        [id],
        |r| {
            let att: String = r.get(1)?;
            Ok((
                r.get(0)?,
                serde_json::from_str(&att).unwrap_or(Value::Null),
                r.get(2)?,
                r.get(3)?,
            ))
        },
    )?)
}

/// The sender asked something within [`OPEN_QUESTION_MS`] before `before` and you have not said
/// anything in the space since.
fn open_question(
    c: &Connection,
    ws: &str,
    space: &str,
    sender: &str,
    before: i64,
    now: i64,
) -> ChannelResult<bool> {
    let mut st = c.prepare_cached(
        "SELECT coalesce(text, ''), created FROM chan_messages
          WHERE space_id = ?1 AND direction = 'in' AND coalesce(sender_party_id, sender_account_id, '') = ?2
            AND created < ?3 AND created >= ?4 ORDER BY created DESC LIMIT 10",
    )?;
    let asked: Option<i64> = st
        .query_map(
            params![space, sender, before, before - OPEN_QUESTION_MS],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .find(|(t, _)| asks(t))
        .map(|(_, at)| at);
    match asked {
        Some(at) => Ok(!crate::presence::you_spoke(c, ws, space, at, now)?),
        None => Ok(false),
    }
}

/// The open card in `space` about an earlier message of `sender` (before `before`), newest first.
fn open_card_of(
    c: &Connection,
    ws: &str,
    space: &str,
    sender: &str,
    before: i64,
) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        &format!(
            "SELECT d.id FROM decisions d JOIN chan_messages m ON m.id = d.subject_id AND m.direction = 'in'
              WHERE d.workspace_id = ?1 AND d.subject_type = '{SUBJECT_MESSAGE}' AND d.kind = 'ask' AND d.status = 'open'
                AND m.space_id = ?2 AND coalesce(m.sender_party_id, m.sender_account_id, '') = ?3 AND m.created < ?4
              ORDER BY d.created DESC LIMIT 1"
        ),
        params![ws, space, sender, before],
        |r| r.get(0),
    )
    .optional()?)
}

/// G4 and G5 for stored inbound message `target` at `now` (its turn read with
/// [`crate::presence::turn`]). `starred`: the sender is starred — their message is never let go for
/// having no words (it may be a missed call).
pub fn gate(
    c: &Connection,
    ws: &str,
    target: &str,
    starred: bool,
    now: i64,
) -> ChannelResult<Option<Gate>> {
    let ids = crate::presence::turn(c, ws, target)?;
    if ids.is_empty() {
        return Ok(None);
    }
    let mut texts = Vec::new();
    for id in &ids {
        texts.push(message_text(c, id)?);
    }
    let (first_at, sender) = (texts[0].2, texts[0].3.clone());
    let space: String = c.query_row(
        "SELECT space_id FROM chan_messages WHERE id = ?",
        [target],
        |r| r.get(0),
    )?;
    if sender.is_empty() {
        return Ok(None);
    }
    if texts.iter().all(|(t, a, _, _)| nudge_only(t, a))
        && let Some(d) = open_card_of(c, ws, &space, &sender, first_at)?
    {
        return Ok(Some(Gate::Nudge { decision: d }));
    }
    if !starred
        && texts.iter().all(|(t, a, _, _)| content_free(t, a))
        && !open_question(c, ws, &space, &sender, first_at, now)?
    {
        return Ok(Some(Gate::NoContent));
    }
    Ok(None)
}

/// G5: the open card `decision` gets a line that the sender asked again (`text`, what they sent).
pub fn nudge(tx: &Tx, decision: &str, text: &str) -> ChannelResult<bool> {
    let said = flat(text, EXCERPT_LINE_MAX);
    let line = if said.is_empty() {
        NUDGED.trim_end_matches('：').to_string()
    } else {
        format!("{NUDGED}{said}")
    };
    let n = tx.execute(
        "UPDATE decisions SET question = question || char(10) || ? WHERE id = ? AND status = 'open'",
        params![line, decision],
    )?;
    if n > 0 {
        // This API has no workspace argument: read the exact row we just changed while the
        // caller's write transaction is still held. Never guess a workspace from the id.
        let ws: String = tx.query_row(
            "SELECT workspace_id FROM decisions WHERE id = ?",
            [decision],
            |row| row.get(0),
        )?;
        tx.publish(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Decision {
                ws: ws.into(),
                id: decision.into(),
                item_id: Some(None),
            },
        ));
    }
    Ok(n > 0)
}

// ---------------------------------------------------------------- starred contacts

/// The sender is a contact you starred (`parties.important`; the contact page's star), directly or
/// through the contact it was merged into.
pub fn starred(c: &Connection, ws: &str, party: Option<&str>) -> ChannelResult<bool> {
    let Some(party) = party else {
        return Ok(false);
    };
    Ok(c.query_row(
        "WITH RECURSIVE up(id, n) AS (SELECT ?2, 0 UNION SELECT p.merged_into, up.n + 1 FROM parties p JOIN up ON p.id = up.id
                                      WHERE p.merged_into IS NOT NULL AND up.n < 5)
         SELECT EXISTS (SELECT 1 FROM parties p JOIN up ON p.id = up.id
                         WHERE p.workspace_id = ?1 AND p.important = 1 AND p.status = 'active'
                           AND coalesce(p.contact_kind, '') <> 'bot' AND p.type <> 'bot')",
        params![ws, party],
        |r| r.get(0),
    )?)
}

// ---------------------------------------------------------------- the card

/// What a card says beyond the message ([`open_card`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// One sentence on who wants what (the model's; else the message itself).
    pub summary: String,
    pub reason: Reason,
    /// Said after the reason instead of its label (the voice check's problems).
    pub note: Option<String>,
    /// The candidate reply (“发这条草稿”); empty: none.
    pub draft: String,
    pub starred: bool,
}

impl Card {
    /// A card the rules opened (no composer): the message is its summary.
    pub fn rule(starred: bool) -> Self {
        Self {
            summary: String::new(),
            reason: if starred {
                Reason::Starred
            } else {
                Reason::Rule
            },
            note: None,
            draft: String::new(),
            starred,
        }
    }

    /// The card of the composer's question to you (`text`, its note). On your own account the card
    /// says why in Steve's fixed words ([`Reason::label`]; the voice check's problems for
    /// [`Reason::Voice`]), never the model's own doubts. Where a bot speaks, the bot was asked and you
    /// may not be in that chat: its question to you is the note.
    pub fn of(g: &crate::deliver::Generation, text: &str, mode: Mode, starred: bool) -> Self {
        let reason = Reason::parse(&g.reason).unwrap_or(if starred {
            Reason::Starred
        } else {
            Reason::OwnerFact
        });
        let note = match mode {
            Mode::Own => (reason == Reason::Voice).then(|| g.why.clone()),
            Mode::Bot => Some(text.trim().to_string()).filter(|t| !t.is_empty()),
        };
        Self {
            summary: g.summary.clone(),
            note,
            reason,
            draft: g.draft.clone(),
            starred,
        }
    }
}

/// One line: whitespace folded, at most `n` characters.
pub fn flat(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

/// How the message reached you, as the card says it.
fn trigger_note(why: Why) -> Option<&'static str> {
    match why {
        Why::Dm => Some("私聊找你"),
        Why::Mentioned => Some("@了你"),
        Why::ReplyToYou => Some("回复了你"),
        Why::TriggerWords | Why::Other => None,
    }
}

/// The card's text: `「空间」里 谁 找你：summary`, why you are needed (and how it reached you), then
/// the last lines of the conversation with yours marked “我” and the message itself marked.
pub fn card_question(space: &str, who: &str, card: &Card, why: Why, excerpt: &[String]) -> String {
    let who = if who.trim().is_empty() { "有人" } else { who };
    let star = if card.starred {
        format!("{STARRED} · ")
    } else {
        String::new()
    };
    let mut q = format!(
        "{star}「{space}」里 {who} 找你：{}",
        flat(&card.summary, SUMMARY_MAX)
    );
    let because = card
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(card.reason.label());
    q.push('\n');
    q.push_str(WHY_HEAD);
    q.push_str(because);
    if let Some(t) = trigger_note(why) {
        q.push_str(&format!("（{t}）"));
    }
    if !excerpt.is_empty() {
        q.push_str("\n最近的对话：");
        for l in excerpt {
            q.push('\n');
            q.push_str(l);
        }
    }
    q
}

/// The last lines of the conversation up to `plan`'s message (at most [`EXCERPT_MAX`], bots left out
/// unless the message is a bot's), yours marked “我”, the message itself last with “→”.
pub fn excerpt(c: &Connection, plan: &Plan) -> ChannelResult<Vec<String>> {
    Ok(shown(c, plan)?.into_iter().map(|(_, l)| l).collect())
}

/// [`excerpt`] with the id of the message each line shows.
fn shown(c: &Connection, plan: &Plan) -> ChannelResult<Vec<(String, String)>> {
    let mine = context::mine(c, &plan.ws, &plan.space_id)?;
    let Some((t, before, _)) =
        context::load(c, &plan.space_id, &plan.message_id, &mine, Mode::Own)?
    else {
        return Ok(Vec::new());
    };
    let who = |m: &context::Msg| match m.side {
        Side::Owner | Side::OwnerViaSteve => "我".to_string(),
        Side::OwnerAuto => "我的账号（自动回复）".to_string(),
        Side::Persona => format!("{}（机器人）", or_someone(&m.name)),
        Side::Other => or_someone(&m.name),
    };
    let mut lines: Vec<(String, String)> = before
        .iter()
        .filter(|m| !m.bot || t.bot)
        .rev()
        .take(EXCERPT_MAX - 1)
        .map(|m| {
            (
                m.id.clone(),
                format!("- {}：{}", who(m), flat(&m.text, EXCERPT_LINE_MAX)),
            )
        })
        .collect();
    lines.reverse();
    lines.push((
        t.id.clone(),
        format!("→ {}：{}", who(&t), flat(&t.text, EXCERPT_LINE_MAX)),
    ));
    Ok(lines)
}

fn or_someone(n: &str) -> String {
    if n.trim().is_empty() {
        "某人".into()
    } else {
        n.trim().to_string()
    }
}

/// Options of a card with a candidate draft, and of one without.
pub use crate::respond::{HANDOFF_NO_DRAFT_OPTIONS, HANDOFF_OPTIONS};

/// Opens the card about `plan`'s message (one per message: an open one is returned as it is), links it
/// from the message, and on your own account closes the older cards it takes over
/// ([`supersede_older`]).
pub fn open_card(tx: &Tx, plan: &Plan, card: &Card) -> ChannelResult<String> {
    if let Some(d) = base::decisions::open_for(tx, &plan.ws, SUBJECT_MESSAGE, &plan.message_id)? {
        return Ok(d.id);
    }
    let mut card = card.clone();
    if card.summary.trim().is_empty() {
        card.summary = if plan.text.trim().is_empty() {
            "[消息]".into()
        } else {
            plan.text.clone()
        };
    }
    let why = Why::of(&context::facts_of(tx, &plan.message_id)?, false);
    let shown = shown(tx, plan)?;
    let lines: Vec<String> = shown.iter().map(|(_, l)| l.clone()).collect();
    let space = crate::spaces::get(tx, &plan.space_id)?
        .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
    let name = crate::spaces::display_title(tx, &space)?;
    let question = card_question(&name, &plan.sender_name, &card, why, &lines);
    let draft = card.draft.trim();
    let options: Vec<String> = if draft.is_empty() {
        HANDOFF_NO_DRAFT_OPTIONS.map(String::from).to_vec()
    } else {
        HANDOFF_OPTIONS.map(String::from).to_vec()
    };
    let d = base::decisions::open(
        tx,
        &NewDecision {
            workspace_id: plan.ws.clone(),
            kind: DecisionKind::Ask.code().into(),
            subject_type: SUBJECT_MESSAGE.into(),
            subject_id: plan.message_id.clone(),
            item_id: None,
            session_id: None,
            question,
            draft: (!draft.is_empty()).then(|| draft.to_string()),
            options: Some(options),
            requested_by: "steve".into(),
        },
    )?;
    tx.execute(
        "UPDATE chan_messages SET decision_id = ? WHERE id = ?",
        params![d.id, plan.message_id],
    )?;
    crate::messages::event(tx, &plan.ws, &plan.space_id, &plan.message_id);
    if plan.mode == Mode::Own {
        let (sender, thread): (String, Option<String>) = tx.query_row(
            "SELECT coalesce(sender_party_id, sender_account_id, ''), thread_key FROM chan_messages WHERE id = ?",
            [&plan.message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        supersede_older(
            tx,
            &plan.ws,
            &plan.space_id,
            &d.id,
            &Newer {
                starred: card.starred,
                commitment: card.reason == Reason::Commitment,
                sender,
                thread_key: thread.filter(|t| !t.is_empty()),
                shown: shown.into_iter().map(|(id, _)| id).collect(),
            },
        )?;
    }
    Ok(d.id)
}

/// Whether an open card is about a starred contact (its head, [`STARRED`]).
pub fn is_starred_card(question: &str) -> bool {
    question.starts_with(STARRED)
}

/// Why a card says you are needed, read back from its line [`WHY_HEAD`] (Steve's fixed words,
/// [`Reason::label`]); `None` when it says something else (the voice check's problems, a bot's
/// question).
pub fn card_reason(question: &str) -> Option<Reason> {
    let why = question.lines().find_map(|l| l.strip_prefix(WHY_HEAD))?;
    Reason::ALL.into_iter().find(|r| why.starts_with(r.label()))
}

/// The newer card, as [`supersede_older`] weighs it.
#[derive(Debug, Clone, Default)]
pub struct Newer {
    pub starred: bool,
    /// It asks you for a commitment ([`Reason::Commitment`]).
    pub commitment: bool,
    /// Who sent its message (contact, else account).
    pub sender: String,
    /// Its message's reply chain (Feishu: thread, else root, else itself).
    pub thread_key: Option<String>,
    /// The messages its excerpt shows.
    pub shown: Vec<String>,
}

/// Closes the open cards about older messages in `space` that the newer card `keep` takes over
/// (`result.superseded` = [`BY_NEWER`]): only one about a message the newer card shows in its
/// excerpt, from the same sender or in the same reply chain — so what they asked is still on the
/// newer card. A starred contact's card is closed only by another starred one, and a commitment's
/// only by another commitment: neither is lost to an unrelated question.
pub fn supersede_older(
    tx: &Tx,
    ws: &str,
    space: &str,
    keep: &str,
    newer: &Newer,
) -> ChannelResult<usize> {
    if newer.shown.is_empty() {
        return Ok(0);
    }
    let open: Vec<(String, String, String, String, Option<String>)> = {
        let mut st = tx.prepare_cached(&format!(
            "SELECT d.id, d.question, m.id, coalesce(m.sender_party_id, m.sender_account_id, ''), m.thread_key
               FROM decisions d JOIN chan_messages m ON m.id = d.subject_id AND m.direction = 'in'
              WHERE d.workspace_id = ?1 AND d.subject_type = '{SUBJECT_MESSAGE}' AND d.kind = 'ask' AND d.status = 'open'
                AND m.space_id = ?2 AND d.id <> ?3"
        ))?;
        st.query_map(params![ws, space, keep], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut n = 0;
    for (d, q, m, sender, thread) in open {
        let same = (!sender.is_empty() && sender == newer.sender)
            || (thread.is_some() && thread == newer.thread_key);
        if !newer.shown.contains(&m) || !same {
            continue;
        }
        if is_starred_card(&q) && !newer.starred {
            continue;
        }
        if card_reason(&q) == Some(Reason::Commitment) && !newer.commitment {
            continue;
        }
        if crate::presence::settle(tx, &d, BY_NEWER)? {
            n += 1;
            base::audit(
                tx,
                ws,
                "steve",
                "chan_message.superseded",
                &m,
                &json!({"why": BY_NEWER, "by": keep, "decision": d}),
            )?;
            crate::messages::event(tx, ws, space, &m);
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------- bot notices (G0)

/// The first action link of a message's card: a button or a clickable block that opens a page
/// (`url` on a `buttons` item or a `group` block of the `card` attachment, as the adapters read it).
pub fn action_link(attachments: &Value) -> Option<String> {
    fn walk(v: &Value) -> Option<String> {
        match v {
            Value::Object(o) => {
                let t = o.get("t").and_then(Value::as_str).unwrap_or_default();
                if t == "buttons" {
                    for it in o
                        .get("items")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(u) = it["url"].as_str().filter(|u| !u.is_empty()) {
                            return Some(u.to_string());
                        }
                    }
                }
                if t == "group"
                    && let Some(u) = o
                        .get("url")
                        .and_then(Value::as_str)
                        .filter(|u| !u.is_empty())
                {
                    return Some(u.to_string());
                }
                o.values().find_map(walk)
            }
            Value::Array(a) => a.iter().find_map(walk),
            _ => None,
        }
    }
    attachments
        .as_array()?
        .iter()
        .filter(|a| a["type"] == "card")
        .find_map(walk)
}

/// What G0 does with a bot's message on your own account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotNotice {
    /// Recorded only.
    Record,
    /// Proposed as an item; `key` is what makes it the same notice (its action link, else its text).
    Item { key: String },
    /// The same notice was proposed already.
    Seen,
}

/// The key of a notice: its action link, else (a space set to `item`) its words.
fn notice_key(text: &str, attachments: &Value) -> String {
    action_link(attachments).unwrap_or_else(|| flat(text, 200))
}

/// G0 for a bot's stored message `message` in `space` on your own account: by its structure (a
/// private chat with the bot and a card with an action link) under `auto`, any message under `item`,
/// nothing under `record` ([`BotNotices`]); the same key within [`BOT_NOTICE_DEDUPE_MS`] once.
pub fn bot_notice(
    r: &mut impl steve_store::Read,
    space: &spaces::SpaceRow,
    message: &str,
    now: i64,
) -> ChannelResult<BotNotice> {
    let setting = steve_store::channel_spaces::preferences::bot_notices(r, &space.id)?
        .as_deref()
        .and_then(BotNotices::parse)
        .unwrap_or_default();
    let source = steve_store::channel_message_repo::media::get(r, message)?
        .ok_or_else(|| steve_modules::ApiError::not_found("消息"))?;
    let text = source.text.unwrap_or_default();
    let att = source.attachments;
    let key = match setting {
        BotNotices::Record => return Ok(BotNotice::Record),
        BotNotices::Auto if space.kind != "dm" => return Ok(BotNotice::Record),
        BotNotices::Auto => match action_link(&att) {
            Some(l) => l,
            None => return Ok(BotNotice::Record),
        },
        BotNotices::Item => notice_key(&text, &att),
    };
    if key.trim().is_empty() {
        return Ok(BotNotice::Record);
    }
    let seen = steve_store::channel_message_repo::transcript::bot_notice_samples(
        r,
        &space.id,
        message,
        now - BOT_NOTICE_DEDUPE_MS,
    )?
    .into_iter()
    .any(|(text, attachments)| notice_key(&text, &attachments) == key);
    Ok(if seen {
        BotNotice::Seen
    } else {
        BotNotice::Item { key }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(addressed: bool) -> Seen {
        Seen {
            own: true,
            instructed: false,
            addressed,
            question: true,
            owner_names: vec!["林知远".into(), "知远".into(), "730000000012".into()],
        }
    }

    fn ask(reason: &str) -> Answer {
        Answer {
            outcome: Outcome::AskOwner,
            text: "要你定".into(),
            reason: reason.into(),
            draft: "周四可以".into(),
            ..Answer::default()
        }
    }

    #[test]
    fn a_question_to_you_needs_a_reason_you_are_needed_for() {
        // a commitment: always, to you or not
        for r in ["commitment", "time", "money", "relationship"] {
            let a = settle(ask(r), &seen(false));
            assert_eq!(
                (a.outcome, a.reason.as_str()),
                (Outcome::AskOwner, "commitment"),
                "{r}"
            );
        }
        // a fact only you know, a substantive message: only when it is to you
        for r in ["owner_fact", "substantive"] {
            assert_eq!(settle(ask(r), &seen(true)).outcome, Outcome::AskOwner);
            let a = settle(ask(r), &seen(false));
            assert_eq!(a.outcome, Outcome::Skip, "{r}");
            assert!(a.draft.is_empty() && a.why.starts_with(NOT_FOR_YOU));
        }
        // the model reading it as to you counts too
        let mut a = ask("owner_fact");
        a.addressee = "owner".into();
        assert_eq!(settle(a, &seen(false)).outcome, Outcome::AskOwner);
        // no reason: to you → asked for what you know; otherwise let go
        let a = settle(ask(""), &seen(true));
        assert_eq!(
            (a.outcome, a.reason.as_str()),
            (Outcome::AskOwner, "owner_fact")
        );
        assert_eq!(settle(ask("unsure"), &seen(false)).outcome, Outcome::Skip);
    }

    #[test]
    fn a_reply_naming_you_was_meant_for_you() {
        let r = |t: &str| Answer {
            outcome: Outcome::Reply,
            text: t.into(),
            ..Answer::default()
        };
        let a = settle(r("这条是发给林知远的，要不要我回"), &seen(true));
        assert_eq!(a.outcome, Outcome::AskOwner);
        assert_eq!(a.reason, "meant_for_owner");
        assert!(
            a.draft.is_empty(),
            "a private question is not an outward draft"
        );
        assert_eq!(a.text, "这条是发给林知远的，要不要我回");
        let mut bot = seen(true);
        bot.own = false;
        let a = settle(r("这条是发给林知远的，要不要我回"), &bot);
        assert_eq!(a.outcome, Outcome::AskOwner);
        assert!(a.draft.is_empty());
        let mut english = seen(true);
        english.owner_names = vec!["Alice Chen".into()];
        let a = settle(
            r("This message is for Alice Chen. Would you like me to handle it?"),
            &english,
        );
        assert_eq!(a.outcome, Outcome::AskOwner);
        assert!(a.draft.is_empty());
        assert_eq!(
            settle(
                r("I'm Steve, Alice Chen's AI assistant. How can I help you?"),
                &english
            )
            .outcome,
            Outcome::Reply
        );
        // words that are no name of yours are a reply, whatever they say ("Steve", "要不要我")
        for t in [
            "要不要我帮你看下",
            "Steve 挺好用",
            "周四可以",
            "我是 Steve，林知远的 AI 助手。有什么可以帮你处理的？",
            "我是林知远。",
            "林知远已经公开介绍过这个功能。",
        ] {
            assert_eq!(settle(r(t), &seen(true)).outcome, Outcome::Reply, "{t}");
        }
        // an id is no name
        assert_eq!(
            settle(r("730000000012 在吗"), &seen(true)).outcome,
            Outcome::Reply
        );
    }

    #[test]
    fn nothing_is_settled_for_a_bot_or_what_you_asked_for() {
        let mut s = seen(false);
        s.own = false;
        assert_eq!(settle(ask("unsure"), &s).outcome, Outcome::AskOwner);
        let mut s = seen(false);
        s.instructed = true;
        assert_eq!(settle(ask("unsure"), &s).outcome, Outcome::AskOwner);
        // needs_approval and needs_commitment are one flag, on any account
        for own in [true, false] {
            let mut s = seen(false);
            s.own = own;
            let a = settle(
                Answer {
                    needs_approval: true,
                    ..Answer::default()
                },
                &s,
            );
            assert!(a.needs_commitment, "own {own}");
            let a = settle(
                Answer {
                    needs_commitment: true,
                    ..Answer::default()
                },
                &s,
            );
            assert!(a.needs_approval, "own {own}");
        }
    }

    #[test]
    fn what_has_no_words_to_answer() {
        let none = json!([]);
        for t in [
            "",
            "[system message]",
            "[system消息]",
            "[sticker]",
            "[表情包]",
            "[OK][赞]",
            "👍",
            "😂😂",
            "[unsupported message]",
            " [FullMoonFace] 👍 ",
            "[THUMBSUP]",
        ] {
            assert!(content_free(t, &none), "{t:?}");
        }
        // words, and the generic placeholders of types that carry them
        for t in [
            "好",
            "[图片]吗",
            "ok",
            "👍 好的",
            "[123]",
            "看下 [OK]",
            "[TODO]",
            "[merge_forward消息]",
            "[todo消息]",
            "[share_chat消息]",
            "[share_user消息]",
            "[location消息]",
            "[vote消息]",
            "[forwarded messages]",
        ] {
            assert!(!content_free(t, &none), "{t:?}");
        }
        // a sticker as the Feishu reader stores it, and the older shape
        let sticker = json!([{"type": "file", "kind": "sticker", "source": "message", "key": "s", "name": null}]);
        assert!(content_free("", &sticker));
        assert!(content_free("", &json!([{"type": "sticker", "key": "s"}])));
        assert!(!content_free(
            "",
            &json!([{"type": "file", "kind": "file", "key": "f"}])
        ));
        assert!(!content_free(
            "[image]",
            &json!([{"type": "image", "key": "i"}])
        ));
        // a card: empty is wordless, with blocks it is not, whatever its text
        assert!(content_free(
            crate::adapters::feishu::card::EMPTY,
            &json!([{"type": "card", "blocks": []}])
        ));
        assert!(!content_free(
            "告警",
            &json!([{"type": "card", "blocks": [{"t": "md", "text": "告警"}]}])
        ));
        assert!(!content_free(
            crate::adapters::feishu::card::EMPTY,
            &json!([{"type": "card", "blocks": [], "truncated": true}])
        ));
        for t in ["？", "??", "。。。", "!", "[OK]"] {
            assert!(nudge_only(t, &none), "{t:?}");
        }
        assert!(!nudge_only("在吗？", &none));
        assert!(!nudge_only("", &json!([{"type": "image"}])));
    }

    #[test]
    fn the_feishu_readers_own_shapes() {
        use crate::adapters::feishu::inbound::parse_receive;
        let env = |ty: &str, content: &str| {
            let mut e = crate::fake::feishu::fixture("message_p2p_text").expect("fixture");
            e["event"]["message"]["message_type"] = json!(ty);
            e["event"]["message"]["content"] = json!(content);
            let m = parse_receive(&e, "ou_bot").unwrap().unwrap();
            (m.text, Value::Array(m.attachments))
        };
        let (t, a) = env("sticker", r#"{"file_key":"x"}"#);
        assert!(content_free(&t, &a), "a sticker: {t:?} {a}");
        let (t, a) = env("system", r#"{"template":"x"}"#);
        assert!(content_free(&t, &a), "a system message: {t:?}");
        for ty in [
            "merge_forward",
            "todo",
            "share_chat",
            "share_user",
            "location",
            "vote",
        ] {
            let (t, a) = env(ty, r#"{"x":"y"}"#);
            assert!(!content_free(&t, &a), "{ty}: {t:?}");
        }
        let (t, a) = env(
            "post",
            r#"{"zh_cn":{"title":"","content":[[{"tag":"emotion","emoji_type":"THUMBSUP"}]]}}"#,
        );
        assert!(content_free(&t, &a), "an emotion element alone: {t:?}");
    }

    #[test]
    fn a_card_says_who_why_and_the_lines_with_yours_marked() {
        let card = Card {
            summary: "A 问周四下午能不能一起过一下方案".into(),
            reason: Reason::Commitment,
            note: None,
            draft: "周四可以".into(),
            starred: false,
        };
        let lines = vec![
            "- 我：方案我晚点发".to_string(),
            "→ A：周四下午能过一下吗".to_string(),
        ];
        let q = card_question("项目群", "A", &card, Why::Mentioned, &lines);
        assert_eq!(
            q,
            "「项目群」里 A 找你：A 问周四下午能不能一起过一下方案\n为什么要你：要答应或安排事情（时间、花钱、删除、隐私或人际关系），得你自己定（@了你）\n最近的对话：\n- 我：方案我晚点发\n→ A：周四下午能过一下吗"
        );
        // a long summary is kept up to the bound, not cut at a few words
        let long = "很".repeat(SUMMARY_MAX + 20);
        let q = card_question(
            "私聊",
            "A",
            &Card {
                summary: long,
                ..card.clone()
            },
            Why::Dm,
            &[],
        );
        assert!(q.lines().next().unwrap().chars().count() > SUMMARY_MAX);
        assert!(q.contains("（私聊找你）"));
        // starred: marked at the head
        let q = card_question(
            "私聊",
            "A",
            &Card {
                starred: true,
                reason: Reason::Starred,
                ..card.clone()
            },
            Why::Dm,
            &[],
        );
        assert!(is_starred_card(&q), "{q}");
        // the voice check's problems stand for the reason
        let q = card_question(
            "私聊",
            "A",
            &Card {
                reason: Reason::Voice,
                note: Some("起草的回复不像你会说的话：太长".into()),
                ..card
            },
            Why::Dm,
            &[],
        );
        assert!(
            q.contains("为什么要你：起草的回复不像你会说的话：太长"),
            "{q}"
        );
    }

    #[test]
    fn an_action_link_is_read_from_the_card_structure() {
        let card = json!([{"type": "card", "blocks": [
            {"t": "md", "text": "请在周五前处理"},
            {"t": "buttons", "items": [{"text": "去处理", "callback": false, "url": "https://example.com/task/1"}]}
        ]}]);
        assert_eq!(
            action_link(&card).as_deref(),
            Some("https://example.com/task/1")
        );
        let nested = json!([{"type": "card", "blocks": [{"t": "panel", "blocks": [{"t": "group", "kind": "clickable", "url": "https://example.com/r/2", "blocks": []}]}]}]);
        assert_eq!(
            action_link(&nested).as_deref(),
            Some("https://example.com/r/2")
        );
        // a card without a link, a callback-only button, a plain message: none, whatever the words
        let plain = json!([{"type": "card", "blocks": [{"t": "md", "text": "失败 整改 截止 审批"}, {"t": "buttons", "items": [{"text": "确认", "callback": true}]}]}]);
        assert_eq!(action_link(&plain), None);
        assert_eq!(action_link(&json!([])), None);
        assert_eq!(
            action_link(&json!([{"type": "image", "url": "https://example.com/i.png"}])),
            None
        );
    }

    #[test]
    fn a_question_without_a_question_mark_asks_too() {
        for t in ["那个呢", "能上线吗", "你看了么 😂", "周四？", "啥意思?"] {
            assert!(asks(t), "{t}");
        }
        for t in ["好的", "先这样吧", "让他飞一会", "呢喃"] {
            assert!(!asks(t), "{t}");
        }
    }

    #[test]
    fn reasons_read_back() {
        for r in Reason::ALL {
            assert_eq!(Reason::parse(r.code()), Some(r));
        }
        assert_eq!(
            Reason::of_ask("starred"),
            None,
            "not a reason the model may ask for"
        );
    }
}

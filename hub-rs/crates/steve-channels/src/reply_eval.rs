//! The owner-voice reply eval, its deterministic part: a case as the eval set stores it, the prompt
//! layers the composer builds for it at the case's time, your style samples, the rule metrics and the
//! judge's prompt. The replay, the checker and the judge (`examples/reply_eval.rs`) and the CI
//! fixtures (`tests/reply_eval.rs`, `tests/fixtures/reply_eval/`) use it. Nothing here reads the
//! store, a file or the network: real cases stay where the operator keeps them and are passed in.
//!
//! A case is one inbound message on your own account (`Mode::Own`) with the space's messages before
//! it, the messages you sent yourself in the hour after it (`gold_owner_messages_60min`) and what
//! Steve did with it then (`steve`). A label says what Steve should have done (`correct_outcome`).
//!
//! What the replay leaves out of the production layers, by design: the knowledge layer (retrieval
//! needs the store), the anchors beyond the sender's name, and the media agent's reading of
//! attachments (an image is said to be unread, as when it cannot be fetched).

use std::collections::{BTreeMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::compose::{self, BotInfo, Layers};
use crate::deliver::system_text_in;

pub use crate::voice::{
    MIN_SUPPORT, SAMPLES_MAX as STYLE_MAX, Situation, Style, Voice, messages_of, style_for,
};
use crate::voice::{asks, chars, ends_with_full_stop, opener, quantile, question_count};
/// You are present when you said something in the space this long before the message (seconds).
/// A parameter of the presence metric, swept in the eval rather than trusted. It is the metric's own,
/// deliberately not the gate's ([`crate::presence::OWNER_LIVE_MS`]): a gate change must not move what
/// it is measured against.
pub const LIVE_WINDOW_SECS: i64 = 180;

// ---------------------------------------------------------------- cases

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Space {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub is_dm: bool,
    #[serde(default)]
    pub name: String,
    /// People in the chat (0: unknown).
    #[serde(default)]
    pub member_count: i64,
    #[serde(default)]
    pub has_external: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Facts {
    #[serde(default)]
    pub dm: Option<bool>,
    #[serde(default)]
    pub mentioned: Option<bool>,
    #[serde(default)]
    pub reply_to_me: Option<bool>,
}

/// One message of a case (the target, one before it, one in its thread).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Line {
    #[serde(default)]
    pub id: String,
    /// RFC 3339.
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub is_owner: bool,
    #[serde(default)]
    pub is_steve_sent: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub thread_key: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// Attachment kinds (`image`, `sticker`, `file`…).
    #[serde(default)]
    pub media: Vec<String>,
    /// The names mentioned.
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub mentions_owner: bool,
    #[serde(default)]
    pub facts: Facts,
}

/// One message you sent yourself after the case's message.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Gold {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub minutes_after: f64,
    #[serde(default)]
    pub same_thread: bool,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Case {
    pub case_id: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub space: Space,
    pub message: Line,
    #[serde(default)]
    pub previous_space_messages: Vec<Line>,
    #[serde(default)]
    pub gold_owner_messages_60min: Vec<Gold>,
    /// What Steve did then (`outcome`: `draft` | `ask` | `no_action_recorded`, its texts and basis).
    #[serde(default)]
    pub steve: Value,
    #[serde(default)]
    pub owner_decision: Option<Value>,
}

/// What Steve should have done with a case.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Label {
    pub case_id: String,
    pub correct_outcome: String,
    #[serde(default)]
    pub failures: Vec<String>,
    #[serde(default)]
    pub owner_confirmed: bool,
}

/// One message you sent (the style corpus).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OwnerLine {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub space_kind: String,
    #[serde(default)]
    pub text: String,
    /// Attachment kinds.
    #[serde(default)]
    pub media: Vec<String>,
    /// The message before yours in the space.
    #[serde(default)]
    pub prev_line: Option<Value>,
}

/// One answer of the replay (or of Steve then, [`recorded_output`]).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Output {
    pub case_id: String,
    #[serde(default)]
    pub sample: usize,
    /// `reply` | `ask_owner` | `skip` | `to_item` | `error`.
    pub outcome: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub model: String,
    /// What the checks of writing as you found in the first draft, when it was sent back
    /// ([`crate::compose::voice_verdict`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revised: Vec<String>,
    /// The reason as a code ([`crate::decide::Reason`]; a skip's code), when the model gave one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// A question to you: the card's one-sentence summary …
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// … and its candidate draft.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft: String,
    /// The reply would promise something for you.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_commitment: bool,
}

/// Reads JSON Lines (blank lines skipped); the error names the line.
pub fn read_jsonl<T: serde::de::DeserializeOwned>(s: &str) -> Result<Vec<T>, String> {
    s.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

/// Milliseconds since the epoch of an RFC 3339 time; `None` when it isn't one.
pub fn millis(t: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(t)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// Every message id of every case's `gold_owner_messages_60min`: what you actually answered must
/// never become a style sample or an example, in any case.
pub fn gold_ids(cases: &[Case]) -> HashSet<String> {
    cases
        .iter()
        .flat_map(|c| c.gold_owner_messages_60min.iter().map(|g| g.id.clone()))
        .collect()
}

// ---------------------------------------------------------------- the layers

/// What the replay knows besides the case: who you are and the Bot behind your account.
#[derive(Debug, Clone, Default)]
pub struct Setting {
    /// Your name as the composer's own-account persona uses it.
    pub owner: String,
    /// Other names you go by (aliases, your accounts' display names), for the owner block. Ids and
    /// raw handles among them are left out as in production ([`crate::context::raw_name`]).
    pub aliases: Vec<String>,
    /// The zone times are told in (UTC when unset).
    pub tz: Option<chrono_tz::Tz>,
    /// The Bot behind your account (the workspace's main Bot unless the space names one): it picks
    /// the model.
    pub bot: Option<BotInfo>,
    /// The ablation of the Bot layer's removal: render the Bot's name, duty, manner and rules on your
    /// own account, as the composer did before it only picked the model.
    pub bot_layer: bool,
    pub account_duty: String,
    pub space_duty: String,
}

/// The attachments of a case line as the Feishu reader stores them in `chan_messages.attachments`
/// (an image as `image`; a sticker, a file, audio or video as a `file` of that kind), for the unread
/// notes and the gates.
fn attachments(media: &[String]) -> Value {
    Value::Array(
        media
            .iter()
            .map(|k| match k.as_str() {
                "image" => json!({"type": "image"}),
                other => json!({"type": "file", "kind": other}),
            })
            .collect(),
    )
}

/// A case line as the context reads it: whose it is from the case's flags (yours, sent by Steve),
/// never from a name.
fn context_msg(l: &Line) -> crate::context::Msg {
    use crate::context::{Msg, Side};
    let text = l.text.clone().unwrap_or_default();
    let side = if l.is_steve_sent {
        Side::OwnerViaSteve
    } else if l.is_owner && crate::arbiter::looks_automated(&text) {
        Side::OwnerAuto
    } else if l.is_owner {
        Side::Owner
    } else {
        Side::Other
    };
    Msg {
        id: l.id.clone(),
        at: millis(&l.time).unwrap_or(0),
        name: crate::context::shown_name([l.sender.as_str()]),
        side,
        bot: false,
        text: crate::context::typed(&text, &attachments(&l.media)),
        thread_key: l.thread_key.clone(),
        external_id: None,
        reply_to: None,
    }
}

/// The composer's layers for `case` at its time, as [`compose::gather`] builds them on your own
/// account: your own-account persona, the Bot behind it, the duties, the sender as Steve knows
/// nothing of them, where and why ([`crate::context::occasion_block`]), your names, and the
/// conversation before the target ([`crate::context::arrange`]: received or sent lines with a text
/// or an attachment, `history` lines by people; more than [`compose::HISTORY`] is the wider pass) at
/// the time Steve answered then ([`observed_latency`]) — then `voice`: your samples, habits and the
/// rules of writing as you ([`voice`]). The case holds nothing said by others after the target, so
/// there is no “after” block; drafts of Steve's still waiting before the target are listed.
pub fn layers(case: &Case, s: &Setting, history: usize, voice: Voice) -> Layers {
    use crate::context::{self, Clock, Occasion, Why};
    use crate::spaces::Mode;
    let target = &case.message;
    let at = millis(&target.time);
    let mut before: Vec<&Line> = case
        .previous_space_messages
        .iter()
        .filter(|l| l.id != target.id)
        .filter(|l| match (millis(&l.time), at) {
            (Some(t), Some(a)) => t <= a,
            _ => true,
        })
        .collect();
    before.sort_by_key(|l| millis(&l.time).unwrap_or(i64::MIN));
    let visible: Vec<context::Msg> = before
        .iter()
        .filter(|l| matches!(l.status.as_str(), "received" | "sent"))
        .filter(|l| l.text.as_deref().is_some_and(|t| !t.is_empty()) || !l.media.is_empty())
        .map(|l| context_msg(l))
        .collect();
    let now = at.unwrap_or(0) + observed_latency(case).unwrap_or(0) * 1000;
    let clock = Clock {
        now,
        tz: s.tz.unwrap_or(chrono_tz::UTC),
    };
    let label = |m: &context::Msg| context::label(&m.name, m.side, Mode::Own, &s.owner);
    let mut t = context_msg(target);
    t.text = context::typed(
        target.text.as_deref().unwrap_or_default(),
        &attachments(&target.media),
    );
    let segments = if history > compose::HISTORY {
        context::SEGMENTS_WIDE
    } else {
        context::SEGMENTS
    };
    let conv = context::arrange(&t, &visible, &[], history, segments, &label, &clock);
    let facts = json!({
        "dm": target.facts.dm.unwrap_or(case.space.is_dm),
        "mentioned": target.facts.mentioned.unwrap_or(target.mentions_owner),
        "reply_to_me": target.facts.reply_to_me.unwrap_or(false),
    });
    let sender = label(&t);
    let occasion = context::occasion_block(
        &Occasion {
            dm: case.space.is_dm,
            space_name: case.space.name.clone(),
            members: case.space.member_count,
            external: case.space.has_external,
            sender: sender.clone(),
            why: Why::of(&facts, false),
            at: t.at,
        },
        Mode::Own,
        &clock,
    );
    let names: Vec<String> = std::iter::once(s.owner.clone())
        .chain(s.aliases.iter().cloned())
        .collect();
    let pending = before
        .iter()
        .rev()
        .filter(|l| l.is_steve_sent && matches!(l.status.as_str(), "draft" | "pending"))
        .filter_map(|l| Some((millis(&l.time)?, l.text.clone()?)))
        .take(context::PENDING)
        .map(|(at, text)| context::pending_line(at, &text, &clock))
        .collect();
    let anchor = if sender.trim().is_empty() {
        "某人".to_string()
    } else {
        sender.clone()
    };
    Layers {
        bot: s.bot.clone().filter(|_| s.bot_layer),
        model: s.bot.as_ref().and_then(|b| b.model.clone()),
        persona: compose::self_layer(&s.owner),
        voice: Some(voice),
        account_duty: s.account_duty.clone(),
        space_duty: s.space_duty.clone(),
        anchors: format!("发送人：{anchor}（Steve 对这个人还不了解）"),
        occasion,
        owner_block: context::owner_block(&names, Mode::Own),
        history: conv.before,
        chain: conv.chain,
        pending,
        sender,
        message: t.text,
        // the replay reads no attachment (as when none can be fetched)
        unreadable: context::unreadable(
            target.text.as_deref().unwrap_or_default(),
            &attachments(&target.media),
            false,
        ),
        owner: s.owner.clone(),
        own: true,
        addressed: crate::decide::addressed(Why::of(&facts, false)),
        question: crate::decide::asks(target.text.as_deref().unwrap_or_default()),
        owner_names: names,
        ..Layers::default()
    }
}

/// The conversation the audience could read when Steve answered `case`, as the disclosure check is
/// given it (`CheckCx::audience_seen`; the outbound path reads it from the store the same way): the
/// stretches of talk before the message and the message itself, one `- 名字：text` line each.
pub fn seen(case: &Case, s: &Setting) -> String {
    let l = layers(case, s, compose::HISTORY, bare_voice(case));
    let mut lines: Vec<String> = l
        .chain
        .iter()
        .chain(&l.history)
        .map(crate::context::Shown::render)
        .collect();
    lines.push(format!("- （这条）{}：{}", l.sender, l.message.trim()));
    lines.join("\n")
}

fn is_dm_kind(kind: &str) -> bool {
    kind == "dm"
}

/// The corpus as the composer's pool of your messages ([`crate::voice::Said`]), each with its
/// situation ([`owner_situation`]).
pub fn pool(corpus: &[OwnerLine], names: &[String]) -> Vec<crate::voice::Said> {
    corpus
        .iter()
        .filter_map(|o| {
            Some(crate::voice::Said {
                id: o.id.clone(),
                at: millis(&o.time)?,
                space_id: o.space_id.clone(),
                dm: is_dm_kind(&o.space_kind),
                situation: Some(owner_situation(o, names)),
                text: o.text.clone(),
                media: !o.media.is_empty(),
            })
        })
        .collect()
}

/// The people's names the cases and the corpus show (senders, mentions): what a sample from another
/// space must not contain ([`crate::voice::portable`]), as the production query's
/// [`crate::voice::people_names`].
pub fn people(cases: &[Case], corpus: &[OwnerLine]) -> Vec<String> {
    let mut n: Vec<String> = cases
        .iter()
        .flat_map(|c| std::iter::once(&c.message).chain(c.previous_space_messages.iter()))
        .filter(|l| !l.is_owner)
        .flat_map(|l| std::iter::once(l.sender.clone()).chain(l.mentions.iter().cloned()))
        .chain(corpus.iter().filter_map(|o| {
            o.prev_line
                .as_ref()
                .filter(|p| p["is_owner"] != true)
                .and_then(|p| p["sender"].as_str().map(String::from))
        }))
        .map(|x| x.trim().trim_start_matches('@').to_string())
        .filter(|x| x.chars().count() >= 2)
        .collect();
    n.sort();
    n.dedup();
    n
}

/// Your style samples for `case`: messages you sent before it — never one of `excluded` (every case's
/// gold answers, [`gold_ids`]) — picked as the composer picks them ([`crate::voice::samples`]).
pub fn style_samples(
    corpus: &[OwnerLine],
    case: &Case,
    excluded: &HashSet<String>,
    names: &[String],
    max: usize,
) -> Vec<String> {
    let Some(at) = millis(&case.message.time) else {
        return Vec::new();
    };
    let mut excluded = excluded.clone();
    excluded.insert(case.message.id.clone());
    crate::voice::samples(
        &pool(corpus, names),
        &case.space.id,
        case.space.is_dm,
        at,
        &excluded,
        names,
        max,
    )
}

/// What the composer is given to write like you for `case` ([`crate::voice::Voice`]): its situation,
/// your samples ([`style_samples`]) and your habits from the corpus before the case, never from
/// `excluded`. No drafts you rejected or edited (the replay has none; the cases' own drafts are the
/// answers being measured). `names`: yours ([`owner_situation`]) and the people's, apart.
pub fn voice(
    corpus: &[OwnerLine],
    case: &Case,
    excluded: &HashSet<String>,
    yours: &[String],
    people: &[String],
) -> Voice {
    let sit = situation(case);
    let at = millis(&case.message.time).unwrap_or(i64::MAX);
    let before: Vec<crate::voice::Said> = pool(corpus, yours)
        .into_iter()
        .filter(|s| s.at < at && !excluded.contains(&s.id))
        .collect();
    let styles = crate::voice::styles(&before);
    Voice {
        situation: Some(sit),
        style: style_for(&styles, sit).map(|(s, st)| (s, st.clone())),
        samples: style_samples(corpus, case, excluded, people, STYLE_MAX),
        ..Voice::default()
    }
}

/// The voice of a replay without your messages: the situation and the rules only.
pub fn bare_voice(case: &Case) -> Voice {
    Voice {
        situation: Some(situation(case)),
        ..Voice::default()
    }
}

/// What Steve did with a case then, as an [`Output`]: a draft is a `reply`, a question to you
/// `ask_owner`, nothing done `skip`.
pub fn recorded_output(case: &Case) -> Output {
    let s = &case.steve;
    let text = |k: &str| s[k].as_str().unwrap_or_default().to_string();
    let (outcome, body) = match s["outcome"].as_str().unwrap_or_default() {
        "draft" => ("reply", text("draft_text")),
        "ask" => ("ask_owner", text("ask_text")),
        _ => ("skip", String::new()),
    };
    Output {
        case_id: case.case_id.clone(),
        sample: 0,
        outcome: outcome.into(),
        reason: text("note"),
        text: body,
        model: s["reply_basis"][0]["answer_model"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        ..Output::default()
    }
}

// ---------------------------------------------------------------- situations and your statistics

/// The case's situation.
pub fn situation(case: &Case) -> Situation {
    if case.space.is_dm || case.message.facts.dm == Some(true) {
        Situation::Dm
    } else if case.message.mentions_owner || case.message.facts.mentioned == Some(true) {
        Situation::GroupAt
    } else {
        Situation::Group
    }
}

/// The situation of one of your messages: its space, and whether the message before it @-ed one of
/// `names` (your names).
pub fn owner_situation(o: &OwnerLine, names: &[String]) -> Situation {
    if is_dm_kind(&o.space_kind) {
        return Situation::Dm;
    }
    let prev = o
        .prev_line
        .as_ref()
        .and_then(|p| p["text"].as_str())
        .unwrap_or_default();
    if names
        .iter()
        .filter(|n| !n.trim().is_empty())
        .any(|n| prev.contains(&format!("@{n}")) || prev.contains(&format!("＠{n}")))
    {
        Situation::GroupAt
    } else {
        Situation::Group
    }
}

/// Your statistics per situation, from `corpus` (your messages, as provisional as the corpus is:
/// derived when the eval runs, never written down as numbers).
pub fn owner_styles(corpus: &[OwnerLine], names: &[String]) -> BTreeMap<Situation, Style> {
    crate::voice::styles(&pool(corpus, names))
}

// ---------------------------------------------------------------- presence

/// Were you there: had you already answered by the time Steve's answer came (`latency_secs` after the
/// message), or said something in the space within `live_secs` before it?
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Presence {
    pub replied: bool,
    pub live: bool,
}

impl Presence {
    pub fn any(self) -> bool {
        self.replied || self.live
    }
}

/// How long after the message Steve's answer came then, when the case says (seconds).
pub fn observed_latency(case: &Case) -> Option<i64> {
    let at = millis(&case.message.time)?;
    let out = case.steve["reply_basis"][0]["time"]
        .as_str()
        .or_else(|| case.owner_decision.as_ref()?["created"].as_str())
        .and_then(millis)?;
    Some(((out - at) / 1000).max(0))
}

pub fn presence(case: &Case, latency_secs: i64, live_secs: i64) -> Presence {
    let replied = case
        .gold_owner_messages_60min
        .iter()
        .any(|g| g.minutes_after * 60.0 <= latency_secs as f64);
    let at = millis(&case.message.time);
    let live = case.previous_space_messages.iter().any(|l| {
        l.is_owner
            && match (millis(&l.time), at) {
                (Some(t), Some(a)) => a >= t && a - t <= live_secs * 1000,
                _ => false,
            }
    });
    Presence { replied, live }
}

// ---------------------------------------------------------------- the presence gate, replayed

/// The case's message as the presence gate sees it, and the space around it: the messages before it
/// and your own after it (`gold_owner_messages_60min`, at their minute). The eval has no provider ids,
/// so a reply chain is only its thread.
pub fn said_lines(case: &Case) -> (crate::presence::Said, Vec<crate::presence::Said>) {
    use crate::presence::Said;
    let at = millis(&case.message.time).unwrap_or(0);
    let line = |l: &Line| Said {
        id: l.id.clone(),
        at: millis(&l.time).unwrap_or(0),
        owner: l.is_owner,
        sender: if l.is_owner {
            "owner".into()
        } else {
            l.sender.clone()
        },
        thread_key: l.thread_key.clone(),
        mentions_owner: l.mentions_owner,
        ..Default::default()
    };
    let target = line(&case.message);
    let mut around: Vec<Said> = case.previous_space_messages.iter().map(line).collect();
    around.extend(case.gold_owner_messages_60min.iter().map(|g| {
        Said {
            id: g.id.clone(),
            at: at + (g.minutes_after * 60_000.0) as i64,
            owner: true,
            sender: "owner".into(),
            thread_key: g
                .same_thread
                .then(|| case.message.thread_key.clone())
                .flatten(),
            ..Default::default()
        }
    }));
    (target, around)
}

/// The gate's parameters in a replay.
#[derive(Debug, Clone, Copy)]
pub struct GateParams {
    pub live_ms: i64,
    pub gap_ms: i64,
    /// How long drafting takes, for the cases that record none (seconds).
    pub latency_secs: i64,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            live_ms: crate::presence::OWNER_LIVE_MS,
            gap_ms: crate::presence::BURST_GAP_MS,
            latency_secs: 0,
        }
    }
}

/// What the gates did to one answer.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Gated {
    pub case_id: String,
    pub sample: usize,
    /// The answer before the gates.
    pub before: String,
    /// Which gate changed it: `owner_live` / `owner_replied` (before drafting or after it),
    /// `merged` (the sender went on; answered with the later message), `no_content` (nothing to
    /// answer, [`crate::decide::content_free`]).
    pub gate: Option<String>,
    pub merged_into: Option<String>,
    /// It still opened, and your own message closed it later (within the hour of the case; in a
    /// group only what you said about it).
    pub closed_later: bool,
}

/// G4 on a case ([`crate::decide::gate`]): its message has nothing to answer, and the sender has no
/// question waiting (one of theirs within [`crate::decide::OPEN_QUESTION_MS`] before it, with no line
/// of yours after).
pub fn no_content(case: &Case) -> bool {
    let t = &case.message;
    if !crate::decide::content_free(
        t.text.as_deref().unwrap_or_default(),
        &attachments(&t.media),
    ) {
        return false;
    }
    let Some(at) = millis(&t.time) else {
        return true;
    };
    let lines: Vec<(&Line, i64)> = case
        .previous_space_messages
        .iter()
        .filter(|l| l.id != t.id)
        .filter_map(|l| Some((l, millis(&l.time)?)))
        .filter(|(_, m)| *m < at)
        .collect();
    let asked = lines
        .iter()
        .filter(|(l, m)| {
            l.sender == t.sender
                && at - m <= crate::decide::OPEN_QUESTION_MS
                && crate::decide::asks(l.text.as_deref().unwrap_or_default())
        })
        .map(|(_, m)| *m)
        .max();
    match asked {
        Some(q) => lines.iter().any(|(l, m)| l.is_owner && *m > q),
        None => true,
    }
}

/// The cases a case's message is merged into: the later case of the same sender in the same space
/// whose turn ([`crate::presence::burst`]) contains it.
fn merged_into(case: &Case, cases: &[Case], gap_ms: i64) -> Option<String> {
    cases
        .iter()
        .filter(|b| {
            b.case_id != case.case_id
                && b.space.id == case.space.id
                && millis(&b.message.time) > millis(&case.message.time)
        })
        .filter(|b| {
            let (t, around) = said_lines(b);
            crate::presence::burst(&t, &around, gap_ms).contains(&case.message.id)
        })
        .min_by_key(|b| millis(&b.message.time))
        .map(|b| b.case_id.clone())
}

/// Replays the gates of [`crate::presence`] on `outputs`, in the order the pipeline runs them: before
/// drafting (at the message's time), after drafting (its observed latency, else `p.latency_secs`),
/// then the merge of one sender's turn into its last message (the later case's answer, gated, is the
/// answer). Returns the gated outputs and what changed.
pub fn apply_gates(
    cases: &[Case],
    outputs: &[Output],
    p: &GateParams,
) -> (Vec<Output>, Vec<Gated>) {
    let by_case: BTreeMap<&str, &Case> = cases.iter().map(|c| (c.case_id.as_str(), c)).collect();
    // first the presence gates, per answer
    // (answer, the gate, whether it stopped the composer from running at all)
    let mut first: BTreeMap<(String, usize), (Output, Option<String>, bool)> = BTreeMap::new();
    for o in outputs {
        let Some(case) = by_case.get(o.case_id.as_str()) else {
            continue;
        };
        let (t, around) = said_lines(case);
        let dm = situation(case) == Situation::Dm;
        let latency = observed_latency(case).unwrap_or(p.latency_secs);
        let mut out = o.clone();
        let mut gate = None;
        let mut before_drafting = false;
        // G4 runs before any model, after the presence gate at the message's time
        let g4 = no_content(case)
            && crate::presence::engaged(dm, &t, &around, t.at, p.live_ms).is_none()
            && matches!(o.outcome.as_str(), "reply" | "ask_owner");
        if g4 {
            out.outcome = "skip".into();
            out.reason = crate::decide::LABEL_NO_CONTENT.into();
            out.text.clear();
            gate = Some("no_content".into());
            before_drafting = true;
        }
        for (n, now) in [t.at, t.at + latency * 1000].into_iter().enumerate() {
            if gate.is_some() {
                break;
            }
            if let Some(e) = crate::presence::engaged(dm, &t, &around, now, p.live_ms) {
                gate = Some(e.reason().to_string());
                before_drafting = n == 0;
                if matches!(o.outcome.as_str(), "reply" | "ask_owner") {
                    out.outcome = "skip".into();
                    out.reason = e.label().into();
                    out.text.clear();
                } else {
                    // it did nothing anyway: the gate changed nothing
                    gate = None;
                }
            }
        }
        first.insert((o.case_id.clone(), o.sample), (out, gate, before_drafting));
    }
    // then the merge: a turn is answered once, with its last message's (gated) answer
    let mut outs = Vec::new();
    let mut gated = Vec::new();
    for o in outputs {
        let Some((mut out, mut gate, _)) = first.get(&(o.case_id.clone(), o.sample)).cloned()
        else {
            continue;
        };
        let case = by_case[o.case_id.as_str()];
        let into = merged_into(case, cases, p.gap_ms);
        // only an answer still open is merged (a skip left nothing to close), and only once the
        // later message's composer ran (its first gate let it)
        if matches!(out.outcome.as_str(), "reply" | "ask_owner")
            && let Some(b) = &into
            && let Some((later, _, stopped)) = first
                .get(&(b.clone(), o.sample))
                .or_else(|| first.get(&(b.clone(), 0)))
            && !stopped
        {
            out.outcome = later.outcome.clone();
            out.reason = format!("merged into {b}");
            out.text = later.text.clone();
            gate = Some("merged".into());
        }
        // closed later as your own message closes it (crate::presence::supersede): you are there
        // for it by the end of the hour (in a group, what you said is about it)
        let (t, around) = said_lines(case);
        let dm = situation(case) == Situation::Dm;
        let closed_later = matches!(out.outcome.as_str(), "reply" | "ask_owner")
            && crate::presence::engaged(dm, &t, &around, t.at + 3_600_000, p.live_ms).is_some();
        gated.push(Gated {
            case_id: o.case_id.clone(),
            sample: o.sample,
            before: o.outcome.clone(),
            merged_into: gate.as_deref().filter(|g| *g == "merged").and(into),
            gate,
            closed_later,
        });
        outs.push(out);
    }
    (outs, gated)
}

/// The median of the latencies the cases record, for the cases that record none.
pub fn median_latency(cases: &[Case]) -> Option<i64> {
    let mut l: Vec<i64> = cases.iter().filter_map(observed_latency).collect();
    if l.is_empty() {
        return None;
    }
    l.sort_unstable();
    Some(l[l.len() / 2])
}

// ---------------------------------------------------------------- the checks of one draft

/// Words that may hint at Steve's own business; only counted for the log, never a verdict: your own
/// messages use them too.
static KEYWORDS: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"Steve|ST-\d+|事项|排队").expect("valid regex"));

/// What the rules see in one drafted text.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DraftCheck {
    pub messages: usize,
    pub len: usize,
    /// The median length of its messages.
    pub line_median_len: f64,
    pub full_stop: bool,
    pub question: bool,
    pub opening: bool,
    /// The system text it contains ([`crate::deliver::SYSTEM_TEXTS`] and the router's).
    pub leak: Option<String>,
    pub option_list: bool,
    /// Log-only ([`KEYWORDS`]).
    pub keywords: Vec<String>,
}

/// Checks `text` against `system` (the system texts' templates).
pub fn check_draft(text: &str, system: &[&str]) -> DraftCheck {
    let msgs = messages_of(text);
    let mut lens: Vec<usize> = msgs.iter().map(|m| chars(m)).collect();
    lens.sort_unstable();
    DraftCheck {
        messages: msgs.len(),
        len: msgs.iter().map(|m| chars(m)).sum(),
        line_median_len: quantile(&lens, 0.5),
        full_stop: msgs.iter().any(|m| ends_with_full_stop(m)),
        question: asks(text),
        opening: msgs.first().is_some_and(|m| opener(m).is_some()),
        leak: system_text_in(text, system).map(String::from),
        option_list: crate::voice::option_list(text) || question_count(text) >= 2,
        keywords: KEYWORDS
            .find_iter(text)
            .map(|m| m.as_str().to_string())
            .collect(),
    }
}

// ---------------------------------------------------------------- the metrics

/// Which half of the set a case is in: by its space, so a chat's messages never sit on both sides
/// (about one space in three goes to `test`). Tune on `dev` only.
pub fn split(case: &Case) -> &'static str {
    let h = Sha256::digest(case.space.id.as_bytes());
    if h[0] % 3 == 0 { "test" } else { "dev" }
}

/// `skip` for what does nothing; the rest as they are.
fn acted(outcome: &str) -> bool {
    matches!(outcome, "reply" | "ask_owner" | "to_item")
}

/// The parameters of a check run.
#[derive(Debug, Clone)]
pub struct CheckParams {
    /// Your names (the @ that makes a group message yours).
    pub names: Vec<String>,
    /// The system texts' templates (this crate's and the router's).
    pub system: Vec<String>,
    pub live_secs: i64,
    /// The latency for cases that record none; `None`: their median.
    pub latency_secs: Option<i64>,
}

/// One answer, judged by the rules.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub case_id: String,
    pub sample: usize,
    pub split: &'static str,
    pub situation: Situation,
    pub expected: String,
    pub outcome: String,
    pub correct: bool,
    pub should_act: bool,
    pub presence: Presence,
    pub draft: Option<DraftCheck>,
    /// The draft's length over your median in the same situation (total, and per message).
    pub length_ratio: Option<f64>,
    pub line_length_ratio: Option<f64>,
}

/// The rule metrics over some rows.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Metrics {
    pub answers: usize,
    pub cases: usize,
    pub decision_accuracy: f64,
    pub act_precision: Option<f64>,
    /// Of the answers to cases where Steve should act, the share that acted…
    pub act_recall: Option<f64>,
    /// … and that did exactly what it should.
    pub act_exact: Option<f64>,
    /// Of the answers to cases where you were there or had answered ([`presence`], from what you
    /// actually said: no labels), the share that skipped. With the gate on it is agreement with a
    /// rule close to the gate's, not accuracy: judge the gate by `decision_accuracy` and
    /// `act_recall`, which come from the labels.
    pub presence_skip_recall: Option<f64>,
    pub drafts: usize,
    pub length_ratio_median: Option<f64>,
    pub line_length_ratio_median: Option<f64>,
    pub full_stop_rate: Option<f64>,
    pub question_rate: Option<f64>,
    pub opening_rate: Option<f64>,
    pub leaks: usize,
    pub option_lists: usize,
    /// Log-only.
    pub keyword_drafts: usize,
    pub errors: usize,
    /// Cases whose samples did not all agree on the outcome.
    pub unstable_cases: usize,
}

/// Judges every output that has a case and a label.
pub fn rows(
    cases: &[Case],
    labels: &[Label],
    outputs: &[Output],
    styles: &BTreeMap<Situation, Style>,
    p: &CheckParams,
) -> Vec<Row> {
    let by_case: BTreeMap<&str, &Case> = cases.iter().map(|c| (c.case_id.as_str(), c)).collect();
    let by_label: BTreeMap<&str, &Label> = labels.iter().map(|l| (l.case_id.as_str(), l)).collect();
    let fallback = p
        .latency_secs
        .or_else(|| median_latency(cases))
        .unwrap_or(0);
    let system: Vec<&str> = p.system.iter().map(String::as_str).collect();
    outputs
        .iter()
        .filter_map(|o| {
            let case = by_case.get(o.case_id.as_str())?;
            let label = by_label.get(o.case_id.as_str())?;
            let situation = situation(case);
            let latency = observed_latency(case).unwrap_or(fallback);
            let draft = (o.outcome == "reply").then(|| check_draft(&o.text, &system));
            let median = style_for(styles, situation)
                .map(|(_, s)| s.median_len)
                .filter(|m| *m > 0.0);
            let (length_ratio, line_length_ratio) = match (&draft, median) {
                (Some(d), Some(m)) => (Some(d.len as f64 / m), Some(d.line_median_len / m)),
                _ => (None, None),
            };
            Some(Row {
                case_id: o.case_id.clone(),
                sample: o.sample,
                split: split(case),
                situation,
                expected: label.correct_outcome.clone(),
                outcome: o.outcome.clone(),
                correct: o.outcome == label.correct_outcome,
                should_act: acted(&label.correct_outcome),
                presence: presence(case, latency, p.live_secs),
                draft,
                length_ratio,
                line_length_ratio,
            })
        })
        .collect()
}

fn share(n: usize, of: usize) -> Option<f64> {
    (of > 0).then(|| n as f64 / of as f64)
}

fn median_f(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// The metrics of `rows` (one split, or all).
pub fn metrics<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Metrics {
    let rows: Vec<&Row> = rows.into_iter().collect();
    let count = |f: &dyn Fn(&Row) -> bool| rows.iter().filter(|r| f(r)).count();
    let acted_n = count(&|r| acted(&r.outcome));
    let should = count(&|r| r.should_act);
    let present = count(&|r| r.presence.any());
    let drafts: Vec<&DraftCheck> = rows.iter().filter_map(|r| r.draft.as_ref()).collect();
    let mut outcomes: BTreeMap<&str, HashSet<&str>> = BTreeMap::new();
    for r in &rows {
        outcomes
            .entry(r.case_id.as_str())
            .or_default()
            .insert(r.outcome.as_str());
    }
    Metrics {
        answers: rows.len(),
        cases: outcomes.len(),
        decision_accuracy: share(count(&|r| r.correct), rows.len()).unwrap_or(0.0),
        act_precision: share(count(&|r| acted(&r.outcome) && r.should_act), acted_n),
        act_recall: share(count(&|r| r.should_act && acted(&r.outcome)), should),
        act_exact: share(count(&|r| r.should_act && r.correct), should),
        presence_skip_recall: share(count(&|r| r.presence.any() && r.outcome == "skip"), present),
        drafts: drafts.len(),
        length_ratio_median: median_f(rows.iter().filter_map(|r| r.length_ratio).collect()),
        line_length_ratio_median: median_f(
            rows.iter().filter_map(|r| r.line_length_ratio).collect(),
        ),
        full_stop_rate: share(drafts.iter().filter(|d| d.full_stop).count(), drafts.len()),
        question_rate: share(drafts.iter().filter(|d| d.question).count(), drafts.len()),
        opening_rate: share(drafts.iter().filter(|d| d.opening).count(), drafts.len()),
        leaks: drafts.iter().filter(|d| d.leak.is_some()).count(),
        option_lists: drafts.iter().filter(|d| d.option_list).count(),
        keyword_drafts: drafts.iter().filter(|d| !d.keywords.is_empty()).count(),
        errors: count(&|r| r.outcome == "error"),
        unstable_cases: outcomes.values().filter(|o| o.len() > 1).count(),
    }
}

/// The case ids `before` got right (every sample) and `after` does not (any sample): what a change
/// broke. Swap the arguments for what it fixed.
pub fn regressions(before: &[Row], after: &[Row]) -> Vec<String> {
    let all_right = |rows: &[Row]| -> BTreeMap<String, bool> {
        let mut m: BTreeMap<String, bool> = BTreeMap::new();
        for r in rows {
            let e = m.entry(r.case_id.clone()).or_insert(true);
            *e &= r.correct;
        }
        m
    };
    let (b, a) = (all_right(before), all_right(after));
    b.iter()
        .filter(|(id, ok)| **ok && a.get(*id) == Some(&false))
        .map(|(id, _)| id.clone())
        .collect()
}

// ---------------------------------------------------------------- the judge

/// The judge's prompt for one answer: the conversation, what you actually said afterwards, your
/// statistics in this situation, and the answer — without saying which version wrote it.
pub fn judge_prompt(case: &Case, out: &Output, style: Option<&Style>, history: usize) -> String {
    let mut p = String::from(
        "你在评估一个替用户本人（下面称“主人”）在聊天软件里起草回复的助手。它能做三件事：reply（以主人的口吻起草一条回复，主人确认后发出）、ask_owner（不起草，私下问主人一句）、skip（什么都不做）。\n\n",
    );
    p.push_str(&format!(
        "【场合】{}\n\n【之前的对话】（从早到晚，“（主人）”是主人自己）\n",
        match situation(case) {
            Situation::Dm => "私聊",
            Situation::GroupAt => "群聊，对方 @ 了主人",
            _ => "群聊",
        }
    ));
    let before: Vec<&Line> = case
        .previous_space_messages
        .iter()
        .filter(|l| !l.is_steve_sent && l.text.as_deref().is_some_and(|t| !t.is_empty()))
        .collect();
    for l in &before[before.len().saturating_sub(history)..] {
        let who = if l.is_owner {
            "（主人）"
        } else {
            l.sender.as_str()
        };
        p.push_str(&format!(
            "- {who}：{}\n",
            l.text.as_deref().unwrap_or_default()
        ));
    }
    p.push_str(&format!(
        "\n【要处理的消息】\n{}：{}\n\n【主人之后实际发的】（分钟数是在这条消息之后多久）\n",
        case.message.sender,
        case.message.text.as_deref().unwrap_or_default()
    ));
    if case.gold_owner_messages_60min.is_empty() {
        p.push_str("（一小时内没有发）\n");
    }
    for g in &case.gold_owner_messages_60min {
        p.push_str(&format!(
            "- {:.1}：{}\n",
            g.minutes_after,
            g.text.as_deref().unwrap_or_default()
        ));
    }
    if let Some(s) = style {
        p.push_str(&format!(
            "\n【主人在这种场合的说话习惯】（{} 条消息）长度中位数 {:.0} 字，P90 {:.0} 字；以句号结尾 {:.0}%；带问号 {:.0}%\n",
            s.n,
            s.median_len,
            s.p90_len,
            s.full_stop_rate * 100.0,
            s.question_rate * 100.0
        ));
    }
    let shown = if out.outcome == "ask_owner" {
        format!("主人看到的卡片：\n{}", card_shown(case, out))
    } else if out.text.trim().is_empty() {
        "（无正文）".to_string()
    } else {
        out.text.trim().to_string()
    };
    p.push_str(&format!(
        "\n【助手的结果】outcome={}\n{shown}\n\n",
        out.outcome
    ));
    p.push_str(
        "逐项评估，只回答 JSON：{\"understanding\": true/false, \"understanding_why\": \"它是否读懂了谁在对谁说、在接哪句、指的是什么\", \
\"voice\": 1-5 或 null（只在 outcome=reply 时打分：像不像主人本人会发的，对照主人实际发的和说话习惯）, \
\"card_answerable\": true/false 或 null（只在 outcome=ask_owner 时：主人只看这张卡能不能直接做决定）, \"note\": \"一句话\"}",
    );
    p
}

/// The card the owner would see for a question to them ([`crate::decide::card_question`]): with the
/// answer's summary, reason and candidate draft, the case's last lines with the owner's marked “我”.
/// An answer without them (the replay of an earlier version, or what Steve did then) is shown as the
/// card was before: the message cut at 60 characters and Steve's question under it.
pub fn card_shown(case: &Case, out: &Output) -> String {
    use crate::context::Why;
    use crate::decide::{self, Card, Reason, flat};
    let t = &case.message;
    let space = if case.space.is_dm {
        "私聊".to_string()
    } else {
        case.space.name.clone()
    };
    if out.summary.trim().is_empty() && out.code.trim().is_empty() && out.draft.trim().is_empty() {
        let mut q = format!(
            "「{space}」里 {} 找你：{}",
            t.sender,
            flat(t.text.as_deref().unwrap_or_default(), 60)
        );
        if !out.text.trim().is_empty() {
            q.push_str(&format!("\nSteve 想先问你：{}", flat(&out.text, 60)));
        }
        return q;
    }
    let card = Card {
        summary: if out.summary.trim().is_empty() {
            t.text.clone().unwrap_or_default()
        } else {
            out.summary.clone()
        },
        reason: Reason::parse(&out.code).unwrap_or(Reason::OwnerFact),
        note: (out.code == Reason::Voice.code()).then(|| out.reason.clone()),
        draft: out.draft.clone(),
        starred: false,
    };
    let facts = json!({
        "dm": t.facts.dm.unwrap_or(case.space.is_dm),
        "mentioned": t.facts.mentioned.unwrap_or(t.mentions_owner),
        "reply_to_me": t.facts.reply_to_me.unwrap_or(false),
    });
    let at = millis(&t.time);
    let mut before: Vec<&Line> = case
        .previous_space_messages
        .iter()
        .filter(|l| l.id != t.id && !l.is_steve_sent)
        .filter(|l| matches!((millis(&l.time), at), (Some(x), Some(a)) if x <= a))
        .filter(|l| l.text.as_deref().is_some_and(|x| !x.is_empty()))
        .collect();
    before.sort_by_key(|l| millis(&l.time).unwrap_or(i64::MIN));
    let who = |l: &Line| {
        if l.is_owner {
            "我".to_string()
        } else {
            l.sender.clone()
        }
    };
    let mut lines: Vec<String> = before[before.len().saturating_sub(decide::EXCERPT_MAX - 1)..]
        .iter()
        .map(|l| {
            format!(
                "- {}：{}",
                who(l),
                flat(
                    l.text.as_deref().unwrap_or_default(),
                    decide::EXCERPT_LINE_MAX
                )
            )
        })
        .collect();
    lines.push(format!(
        "→ {}：{}",
        t.sender,
        flat(
            t.text.as_deref().unwrap_or_default(),
            decide::EXCERPT_LINE_MAX
        )
    ));
    let mut q = decide::card_question(&space, &t.sender, &card, Why::of(&facts, false), &lines);
    if !card.draft.trim().is_empty() {
        q.push_str(&format!("\n可以直接发的草稿：{}", card.draft.trim()));
    }
    q
}

/// The judge's verdict on one answer.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Judgment {
    #[serde(default)]
    pub understanding: Option<bool>,
    #[serde(default)]
    pub understanding_why: String,
    #[serde(default)]
    pub voice: Option<f64>,
    #[serde(default)]
    pub card_answerable: Option<bool>,
    #[serde(default)]
    pub note: String,
}

/// The judge's JSON, anywhere in its answer.
pub fn parse_judgment(answer: &str) -> Option<Judgment> {
    let v = steve_modules::changes::first_json(answer)?;
    serde_json::from_value(v).ok()
}

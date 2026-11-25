//! Response rules: their shape and validation, evaluation against one
//! inbound message (first match wins, none = `record`, muted = everything but `handoff` becomes
//! `record`), the `intent` judgement on the model gateway's fast tier, and the “转成流程” compile
//! into a `kind=response` flow.
//!
//! Rules only decide what happens next; nothing here sends anything.
//!
//! Besides the documented conditions, `mention` (only @) and `dm` (only private chats) are accepted:
//! the prototype's accounts use “被 @” and “私聊” on their own, and your own account's default
//! rules split “私聊你” from “群里 @你”.

use std::time::Duration;

use chrono::{Datelike, Timelike};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_flow::graph::{Edge, Graph, Node};
use steve_modules::{ApiError, Gateway, Tier};

use crate::ChannelResult;
use crate::spaces::Mode;

/// Rules per account or space.
pub const MAX_RULES: usize = 20;
/// `400` for `reply` on your own account (trigger words are the only exception); the zh-CN text of
/// `channels.error.self_no_reply`.
pub const SELF_NO_REPLY: &str = "以你本人接入时不能直接回复，只能起草";
/// Keywords per rule, and their length.
pub const MAX_WORDS: usize = 20;
pub const WORD_MAX: usize = 40;
/// `intent.text` length.
pub const INTENT_MAX: usize = 200;
/// Parties and teams per `member` rule.
pub const MAX_MEMBERS: usize = 50;
/// Messages of context the `intent` judgement sees (the last 5).
pub const INTENT_CONTEXT: usize = 5;
/// The fast tier's limit for one `intent` judgement.
pub const INTENT_TIMEOUT: Duration = Duration::from_secs(30);
/// The start of every `intent` prompt (tests script the fake gateway by it).
pub const INTENT_START: &str = "判断一条聊天消息是否符合条件。";
/// The message text an `intent` prompt carries at most, in characters.
const INTENT_TEXT_MAX: usize = 2000;
const CONTEXT_LINE_MAX: usize = 300;
/// Capacity of the per-Hub intent judgement cache keyed by SHA-256 of normalized input.
pub const INTENT_CACHE_CAPACITY: u64 = 512;
/// TTL of cached intent judgements so identical retries or repeated contexts reuse results.
pub const INTENT_CACHE_TTL: Duration = Duration::from_secs(300);
/// Metric task label for channel intent rule evaluation.
const INTENT_METRIC_TASK: &str = "channels.intent";
/// Maximum consecutive `When::Intent` rules batched into one compiled `bot` flow node
/// (reserving one branch for "none matched").
const COMPILE_INTENT_BATCH_MAX: usize = steve_flow::nodes::bot::BRANCHES_MAX - 1;

/// Bounded TTL cache for channel intent judgements keyed by SHA-256 of normalized input.
#[derive(Clone)]
pub struct IntentCache(steve_runtime::cache::TtlCache<[u8; 32], Option<usize>>);

impl IntentCache {
    pub fn new() -> Self {
        Self(steve_runtime::cache::TtlCache::new(
            INTENT_CACHE_CAPACITY,
            INTENT_CACHE_TTL,
        ))
    }
}

impl Default for IntentCache {
    fn default() -> Self {
        Self::new()
    }
}

/// `when.kind`, with its parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    /// Mentioned or in a private chat (your own account: “@你 或私聊你”).
    MentionOrDm,
    /// Only @-mentioned.
    Mention,
    /// Only a private chat.
    Dm,
    /// A reply to its message, or in a thread it started.
    ReplyToMe,
    /// The text contains one of the words (case-insensitive).
    Keyword {
        words: Vec<String>,
    },
    /// The model gateway says yes.
    Intent {
        text: String,
    },
    /// The sender is one of these contacts or a member of one of these teams; `unconfirmed`
    /// contacts never are.
    Member {
        parties: Vec<String>,
        teams: Vec<String>,
    },
    /// The sender is not of the own organization.
    External,
    /// Inside (or, with `outside`, outside) working hours in the workspace's time zone.
    Hours {
        outside: bool,
        /// Minutes after midnight.
        start: u32,
        end: u32,
        /// ISO weekdays, 1 = Monday.
        days: Vec<u32>,
    },
    All,
    /// Any other message in a group (not a private chat, not mentioning the account).
    OtherGroup,
}

pub use steve_store::channel_sends::{Act, ItemMode};

/// The labels used by the Chinese signal and conversation formats.
pub(crate) fn action_label(act: Act) -> &'static str {
    match act {
        Act::Reply => "直接回复",
        Act::Draft => "起草，等你批准",
        Act::Record => "只记录",
        Act::CreateItem(_) => "建事项",
        Act::Handoff => "转给你",
    }
}

fn quiet_action(act: Act) -> Act {
    match act {
        Act::Handoff => Act::Handoff,
        _ => Act::Record,
    }
}

/// One rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub when: When,
    pub act: Act,
}

fn hhmm(m: u32) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 24 && m < 60 && h * 60 + m <= 24 * 60).then_some(h * 60 + m)
}

impl Rule {
    /// The stored JSON (normalized: defaults filled in).
    pub fn to_value(&self) -> Value {
        let when = match &self.when {
            When::MentionOrDm => json!({"kind": "mention_or_dm"}),
            When::Mention => json!({"kind": "mention"}),
            When::Dm => json!({"kind": "dm"}),
            When::ReplyToMe => json!({"kind": "reply_to_me"}),
            When::Keyword { words } => json!({"kind": "keyword", "words": words}),
            When::Intent { text } => json!({"kind": "intent", "text": text}),
            When::Member { parties, teams } => {
                json!({"kind": "member", "parties": parties, "teams": teams})
            }
            When::External => json!({"kind": "external"}),
            When::Hours {
                outside,
                start,
                end,
                days,
            } => {
                json!({"kind": "hours", "outside": outside, "start": hhmm(*start), "end": hhmm(*end), "days": days})
            }
            When::All => json!({"kind": "all"}),
            When::OtherGroup => json!({"kind": "other_group"}),
        };
        let mut v = json!({"when": when, "act": self.act.code()});
        if let Act::CreateItem(m) = self.act {
            v["mode"] = json!(m.code());
        }
        v
    }
}

/// The stored JSON of a rule list.
pub fn to_value(rules: &[Rule]) -> Value {
    Value::Array(rules.iter().map(Rule::to_value).collect())
}

fn str_list(v: &Value, key: &str, field: &str, c: &mut steve_modules::Check) -> Vec<String> {
    match v.get(key) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .filter_map(|(i, x)| match x.as_str() {
                Some(s) => Some(s.trim().to_string()),
                None => {
                    c.that(&format!("{field}.{key}.{i}"), false);
                    None
                }
            })
            .collect(),
        Some(_) => {
            c.that(&format!("{field}.{key}"), false);
            Vec::new()
        }
    }
}

fn parse_when(v: &Value, field: &str, c: &mut steve_modules::Check) -> Option<When> {
    if !v.is_object() {
        c.that(field, false);
        return None;
    }
    let kind = v["kind"].as_str().unwrap_or("");
    Some(match kind {
        "mention_or_dm" => When::MentionOrDm,
        "mention" => When::Mention,
        "dm" => When::Dm,
        "reply_to_me" => When::ReplyToMe,
        "external" => When::External,
        "all" => When::All,
        "other_group" => When::OtherGroup,
        "keyword" => {
            let mut words = str_list(v, "words", field, c);
            words.retain(|w| !w.is_empty());
            words.dedup();
            c.that(
                &format!("{field}.words"),
                !words.is_empty() && words.len() <= MAX_WORDS,
            );
            for (i, w) in words.iter().enumerate() {
                c.len(&format!("{field}.words.{i}"), w, 1, WORD_MAX);
            }
            When::Keyword { words }
        }
        "intent" => {
            let text = v["text"].as_str().unwrap_or("").trim().to_string();
            c.len(&format!("{field}.text"), &text, 1, INTENT_MAX);
            When::Intent { text }
        }
        "member" => {
            let parties = str_list(v, "parties", field, c);
            let teams = str_list(v, "teams", field, c);
            c.that(
                &format!("{field}.parties"),
                parties.len() <= MAX_MEMBERS && parties.iter().all(|p| !p.is_empty()),
            )
            .that(
                &format!("{field}.teams"),
                teams.len() <= MAX_MEMBERS && teams.iter().all(|t| !t.is_empty()),
            )
            .that(field, !parties.is_empty() || !teams.is_empty());
            When::Member { parties, teams }
        }
        "hours" => {
            let outside = match &v["outside"] {
                Value::Null => false,
                Value::Bool(b) => *b,
                _ => {
                    c.that(&format!("{field}.outside"), false);
                    false
                }
            };
            let time = |key: &str, def: u32, c: &mut steve_modules::Check| match &v[key] {
                Value::Null => def,
                Value::String(s) => parse_hhmm(s).unwrap_or_else(|| {
                    c.that(&format!("{field}.{key}"), false);
                    def
                }),
                _ => {
                    c.that(&format!("{field}.{key}"), false);
                    def
                }
            };
            let start = time("start", 9 * 60, c);
            let end = time("end", 18 * 60, c);
            c.that(&format!("{field}.end"), start != end);
            let days = match &v["days"] {
                Value::Null => vec![1, 2, 3, 4, 5],
                Value::Array(a) => {
                    let mut ds: Vec<u32> = Vec::new();
                    for (i, d) in a.iter().enumerate() {
                        match d.as_u64().filter(|d| (1..=7).contains(d)) {
                            Some(d) if !ds.contains(&(d as u32)) => ds.push(d as u32),
                            Some(_) => {}
                            None => {
                                c.that(&format!("{field}.days.{i}"), false);
                            }
                        }
                    }
                    c.that(&format!("{field}.days"), !ds.is_empty());
                    ds.sort_unstable();
                    ds
                }
                _ => {
                    c.that(&format!("{field}.days"), false);
                    Vec::new()
                }
            };
            When::Hours {
                outside,
                start,
                end,
                days,
            }
        }
        _ => {
            c.that(&format!("{field}.kind"), false);
            return None;
        }
    })
}

/// Validates a rule list: a list of at most 20 `{when, act, mode?}`; your own account
/// (`mode=self`) may not `reply`. Field errors are `400 参数不对：rules.<i>.…`.
pub fn parse(v: &Value, mode: Mode) -> ChannelResult<Vec<Rule>> {
    let Some(items) = v.as_array() else {
        return Err(ApiError::invalid("rules"));
    };
    let mut c = steve_modules::Check::new();
    c.that("rules", items.len() <= MAX_RULES);
    let mut out = Vec::new();
    for (i, r) in items.iter().enumerate() {
        let field = format!("rules.{i}");
        if !r.is_object() {
            c.that(&field, false);
            continue;
        }
        let when = parse_when(&r["when"], &format!("{field}.when"), &mut c);
        let act = match r["act"].as_str().unwrap_or("") {
            "reply" => Some(Act::Reply),
            "draft" => Some(Act::Draft),
            "record" => Some(Act::Record),
            "handoff" => Some(Act::Handoff),
            "create_item" => match r["mode"].as_str() {
                None | Some("start") => Some(Act::CreateItem(ItemMode::Start)),
                Some("propose") => Some(Act::CreateItem(ItemMode::Propose)),
                Some(_) => {
                    c.that(&format!("{field}.mode"), false);
                    None
                }
            },
            _ => {
                c.that(&format!("{field}.act"), false);
                None
            }
        };
        if let (Some(when), Some(act)) = (when, act) {
            out.push(Rule { when, act });
        }
    }
    c.done()?;
    if mode == Mode::Own && out.iter().any(|r| r.act == Act::Reply) {
        return Err(ApiError::rule(steve_i18n::t!(
            "channels.error.self_no_reply"
        )));
    }
    Ok(out)
}

/// The stored rules as they are, leniently: what does not parse (an older or hand-edited row) is
/// skipped, and `reply` on your own account becomes `draft` — it never sends as you.
pub fn load(v: &Value, mode: Mode) -> Vec<Rule> {
    let items = v.as_array().map(Vec::as_slice).unwrap_or_default();
    items
        .iter()
        .take(MAX_RULES)
        .filter_map(|r| parse(&Value::Array(vec![r.clone()]), Mode::Bot).ok())
        .flatten()
        .map(|mut r| {
            if mode == Mode::Own && r.act == Act::Reply {
                r.act = Act::Draft;
            }
            r
        })
        .collect()
}

/// `member` rules must name contacts and teams of this workspace (`404 没有这个联系人/团队`).
pub fn check_refs(r: &mut impl steve_store::Read, ws: &str, rules: &[Rule]) -> ChannelResult<()> {
    for rule in rules {
        if let When::Member { parties, teams } = &rule.when {
            for (ids, ty, noun) in [(parties, "contact", "联系人"), (teams, "team", "团队")] {
                for id in ids {
                    let ok = steve_store::Contacts::new(r)
                        .party(id)?
                        .is_some_and(|party| {
                            party.workspace_id == ws
                                && party.kind == ty
                                && party.merged_into.is_none()
                        });
                    if !ok {
                        return Err(ApiError::not_found(noun));
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- evaluation

/// What the conditions look at, gathered once per message (also put into the event's payload as
/// `facts`, so a compiled flow sees the same).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Facts {
    /// A private chat.
    pub dm: bool,
    /// A space where several people talk.
    pub group: bool,
    /// The connection's account is @-mentioned.
    pub mentioned: bool,
    /// A reply to the account's message, or in a thread it started.
    pub reply_to_me: bool,
    pub text: String,
    pub sender_party: Option<String>,
    /// The sender's contact is not `unconfirmed`.
    pub sender_confirmed: bool,
    /// Teams the sender's contact is a member of.
    pub sender_teams: Vec<String>,
    pub sender_external: bool,
    /// ISO weekday (1 = Monday) and minute of the day of the message, in the workspace's zone.
    pub weekday: u32,
    pub minute: u32,
}

impl Facts {
    /// The `facts` object of the event payload.
    pub fn to_value(&self) -> Value {
        json!({
            "dm": self.dm, "group": self.group, "mentioned": self.mentioned, "reply_to_me": self.reply_to_me,
            "sender_confirmed": self.sender_confirmed, "sender_teams": self.sender_teams,
            "sender_external": self.sender_external, "weekday": self.weekday, "minute": self.minute,
        })
    }

    /// Weekday and minute of `at` (Unix ms) in time zone `tz` (the Hub's default when unknown).
    pub fn set_time(&mut self, at: i64, tz: &str) {
        let zone = steve_runtime::tz::Zone::parse(tz)
            .unwrap_or_else(|_| steve_runtime::tz::Zone::system());
        if let Ok(t) = zone.local(at) {
            self.weekday = t.weekday().number_from_monday();
            self.minute = t.hour() * 60 + t.minute();
        }
    }
}

fn in_hours(f: &Facts, start: u32, end: u32, days: &[u32]) -> bool {
    let m = f.minute;
    if start < end {
        days.contains(&f.weekday) && m >= start && m < end
    } else {
        // across midnight: the part after midnight belongs to the day before
        let prev = if f.weekday <= 1 { 7 } else { f.weekday - 1 };
        (m >= start && days.contains(&f.weekday)) || (m < end && days.contains(&prev))
    }
}

/// Does a condition other than `intent` hold? `None` for `intent` (it needs the gateway).
pub fn holds(when: &When, f: &Facts) -> Option<bool> {
    Some(match when {
        When::MentionOrDm => f.mentioned || f.dm,
        When::Mention => f.mentioned,
        When::Dm => f.dm,
        When::ReplyToMe => f.reply_to_me,
        When::Keyword { words } => {
            let t = f.text.to_lowercase();
            words.iter().any(|w| t.contains(&w.to_lowercase()))
        }
        When::Intent { .. } => return None,
        When::Member { parties, teams } => {
            f.sender_confirmed
                && (f.sender_party.as_ref().is_some_and(|p| parties.contains(p))
                    || f.sender_teams.iter().any(|t| teams.contains(t)))
        }
        When::External => f.sender_external,
        When::Hours {
            outside,
            start,
            end,
            days,
        } => in_hours(f, *start, *end, days) != *outside,
        When::All => true,
        When::OtherGroup => f.group && !f.dm && !f.mentioned,
    })
}

/// `Response` label of a message the [`Gate`] does not admit.
pub const LABEL_UNADDRESSED: &str = "没有 @ 也不是私聊，只记录";

/// Which messages Steve acts on at all (reply, draft, create an item, hand off, start a flow): the rest
/// are only recorded. One gate for every account and space, ahead of flows and rules, so rules only choose
/// how to answer an addressed message. Your own trigger word (`/bot …` on your account) addresses Steve
/// too: it is handled before this gate as your explicit command. A flow already waiting for an answer in
/// the conversation is still woken by the answer (it asked). Answering beyond @ and private chats is deliberately not offered yet
/// (decided 2026-11-03); opening it later is a new variant here, not edits across the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gate {
    /// @-mentioned, or a private chat with the account.
    #[default]
    Addressed,
}

impl Gate {
    pub fn admits(self, f: &Facts) -> bool {
        match self {
            Self::Addressed => f.mentioned || f.dm,
        }
    }
}

/// Answers `intent` conditions.
#[async_trait::async_trait]
pub trait IntentJudge: Send + Sync {
    /// Does the message fit `text`? A failure counts as no.
    async fn judge(&self, text: &str) -> bool;

    /// Returns the 0-based index of the first condition in `conditions` that holds, or `None`.
    /// Model-backed implementations evaluate all conditions in one call rather than resending the
    /// same message and conversation context N times.
    async fn judge_first(&self, conditions: &[&str]) -> Option<usize> {
        for (idx, text) in conditions.iter().enumerate() {
            if self.judge(text).await {
                return Some(idx);
            }
        }
        None
    }
}

/// Returns `true` only when at least one `When::Intent` rule is reachable before the first
/// matching deterministic rule, so callers can skip loading intent context when unused.
pub fn needs_intent_judge(rules: &[Rule], f: &Facts) -> bool {
    for r in rules {
        match holds(&r.when, f) {
            Some(true) => return false,
            Some(false) => continue,
            None => return true,
        }
    }
    false
}

/// What the rules decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// The index of the rule that matched (`None`: none did, so `record`).
    pub rule: Option<usize>,
    /// The action, after muting.
    pub act: Act,
    /// The matched rule's action was turned into `record` by muting.
    pub muted: bool,
}

fn verdict_for(rules: &[Rule], index: usize, muted: bool) -> Verdict {
    let raw = rules[index].act;
    let act = if muted { quiet_action(raw) } else { raw };
    Verdict {
        rule: Some(index),
        act,
        muted: act != raw,
    }
}

/// The first rule that holds decides; none = `record`. Muted: everything but `handoff`
/// becomes `record`. Reachable `intent` rules before the first deterministic match are
/// deduplicated and evaluated together in a single `judge_first` call.
pub async fn evaluate(rules: &[Rule], f: &Facts, muted: bool, judge: &dyn IntentJudge) -> Verdict {
    let mut first_det: Option<usize> = None;
    let mut unique_texts: Vec<&str> = Vec::new();
    let mut text_to_rule: Vec<usize> = Vec::new();
    let mut deduped: u64 = 0;

    for (i, r) in rules.iter().enumerate() {
        match holds(&r.when, f) {
            Some(true) => {
                first_det = Some(i);
                break;
            }
            Some(false) => {}
            None => {
                if let When::Intent { text } = &r.when {
                    if unique_texts.contains(&text.as_str()) {
                        deduped += 1;
                    } else {
                        unique_texts.push(text.as_str());
                        text_to_rule.push(i);
                    }
                }
            }
        }
    }

    if deduped > 0 {
        steve_runtime::jobs::record_model_calls_avoided(
            INTENT_METRIC_TASK,
            steve_runtime::jobs::AvoidReason::Deduplicated,
            deduped,
        );
    }

    if !unique_texts.is_empty()
        && let Some(pos) = judge.judge_first(&unique_texts).await
        && let Some(&rule_idx) = text_to_rule.get(pos)
    {
        return verdict_for(rules, rule_idx, muted);
    }

    if let Some(i) = first_det {
        return verdict_for(rules, i, muted);
    }
    Verdict {
        rule: None,
        act: Act::Record,
        muted: false,
    }
}

/// One message of an `intent` prompt's context.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextLine {
    pub who: String,
    pub text: String,
}

/// [`IntentJudge`] over the model gateway's fast tier, with the last 5 messages as context.
pub struct GatewayJudge<'a> {
    pub gateway: &'a dyn Gateway,
    pub message: &'a str,
    pub sender: &'a str,
    pub space: &'a str,
    /// Oldest first.
    pub context: &'a [ContextLine],
    /// Optional per-Hub bounded TTL cache keyed by SHA-256 of normalized input.
    pub cache: Option<&'a IntentCache>,
}

fn cut(s: &str, n: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn hash_str(h: &mut Sha256, s: &str) {
    h.update((s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

impl GatewayJudge<'_> {
    /// Stable input hash covering space, sender, truncated message, context lines, and conditions.
    fn cache_key(&self, conditions: &[&str]) -> [u8; 32] {
        let mut h = Sha256::new();
        hash_str(&mut h, self.space);
        hash_str(&mut h, self.sender);
        hash_str(&mut h, &cut(self.message, INTENT_TEXT_MAX));
        h.update((self.context.len() as u64).to_le_bytes());
        for l in self.context {
            hash_str(&mut h, &l.who);
            hash_str(&mut h, &cut(&l.text, CONTEXT_LINE_MAX));
        }
        h.update((conditions.len() as u64).to_le_bytes());
        for c in conditions {
            hash_str(&mut h, c);
        }
        h.finalize().into()
    }

    fn push_context(&self, p: &mut String) {
        p.push_str(&format!("空间：{}\n", self.space));
        if !self.context.is_empty() {
            p.push_str("最近的消息（从早到晚）：\n");
            for l in self.context {
                p.push_str(&format!(
                    "- {}：{}\n",
                    l.who,
                    cut(&l.text, CONTEXT_LINE_MAX)
                ));
            }
        }
        p.push_str(&format!(
            "\n要判断的消息（{}）：\n{}\n\n",
            self.sender,
            cut(self.message, INTENT_TEXT_MAX)
        ));
    }

    /// The prompt for a single condition `text`.
    pub fn prompt(&self, text: &str) -> String {
        let mut p = format!("{INTENT_START}\n条件：{text}\n\n");
        self.push_context(&mut p);
        p.push_str("只回答 JSON：{\"match\": true} 或 {\"match\": false}。");
        p
    }

    /// The batched prompt for multiple ordered conditions evaluated in a single model call.
    pub fn batch_prompt(&self, conditions: &[&str]) -> String {
        if conditions.len() == 1 {
            return self.prompt(conditions[0]);
        }
        let mut p = format!(
            "{INTENT_START}\n按顺序判断以下条件，只选出第一个符合的条件编号（都不符合答 null）：\n"
        );
        for (i, text) in conditions.iter().enumerate() {
            p.push_str(&format!("{}. {text}\n", i + 1));
        }
        p.push('\n');
        self.push_context(&mut p);
        p.push_str(&format!(
            "只回答 JSON：{{\"match\": 1}}（1 到 {} 中第一个符合的编号）或 {{\"match\": null}}。",
            conditions.len()
        ));
        p
    }
}

/// Reads the judgement: `{"match": bool}` (anywhere in the answer), else a leading 是/yes/true.
pub fn parse_judgement(answer: &str) -> bool {
    if let Some(v) = steve_modules::changes::first_json(answer)
        && let Some(b) = v.get("match").and_then(Value::as_bool)
    {
        return b;
    }
    let a = answer.trim().to_lowercase();
    a.starts_with('是') || a.starts_with("yes") || a.starts_with("true")
}

/// Reads a batched judgement: `{"match": <1-based index> | null}` -> 0-based index.
pub fn parse_batch_judgement(answer: &str, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    if let Some(v) = steve_modules::changes::first_json(answer)
        && let Some(m) = v.get("match")
    {
        return match m {
            Value::Number(n) => n
                .as_u64()
                .and_then(|idx| usize::try_from(idx).ok())
                .filter(|&idx| (1..=count).contains(&idx))
                .map(|idx| idx - 1),
            Value::String(s) => s
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|&idx| (1..=count).contains(&idx))
                .map(|idx| idx - 1),
            Value::Bool(true) if count == 1 => Some(0),
            Value::Null | Value::Bool(_) => None,
            _ => None,
        };
    }
    let trimmed = answer.trim();
    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        return digits
            .parse::<usize>()
            .ok()
            .filter(|&idx| (1..=count).contains(&idx))
            .map(|idx| idx - 1);
    }
    None
}

#[async_trait::async_trait]
impl IntentJudge for GatewayJudge<'_> {
    async fn judge(&self, text: &str) -> bool {
        self.judge_first(&[text]).await.is_some()
    }

    async fn judge_first(&self, conditions: &[&str]) -> Option<usize> {
        if conditions.is_empty() {
            return None;
        }
        let key = self.cache.map(|_| self.cache_key(conditions));
        if let (Some(cache), Some(k)) = (self.cache, key)
            && let Some(hit) = cache.0.get(&k).await
        {
            steve_runtime::jobs::record_model_calls_avoided(
                INTENT_METRIC_TASK,
                steve_runtime::jobs::AvoidReason::Cached,
                conditions.len() as u64,
            );
            return hit;
        }
        let prompt = if conditions.len() == 1 {
            self.prompt(conditions[0])
        } else {
            self.batch_prompt(conditions)
        };
        match steve_modules::gateway::purpose(
            "intent",
            self.gateway.run(Tier::Fast, &prompt, INTENT_TIMEOUT),
        )
        .await
        {
            Ok(o) => {
                if conditions.len() > 1 {
                    steve_runtime::jobs::record_model_calls_avoided(
                        INTENT_METRIC_TASK,
                        steve_runtime::jobs::AvoidReason::Batched,
                        (conditions.len() - 1) as u64,
                    );
                }
                let matched = if conditions.len() == 1 {
                    parse_judgement(&o.text).then_some(0)
                } else {
                    parse_batch_judgement(&o.text, conditions.len())
                };
                if let (Some(cache), Some(k)) = (self.cache, key) {
                    cache.0.insert(k, matched).await;
                }
                matched
            }
            Err(e) => {
                tracing::warn!("channel rules: intent not judged: {e}");
                None
            }
        }
    }
}

// ---------------------------------------------------------------- trigger words

/// `channel_settings.trigger`: your own account's words that let Steve answer as you.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    pub on: bool,
    pub words: Vec<String>,
    /// Space ids; `dm:*` = every private chat, `*` = everywhere (never empty after
    /// [`Trigger::from_value`]).
    pub r#where: Vec<String>,
    /// Who may use the words: [`WHO_ME`] (you), [`WHO_ANYONE`] (anyone where they work), or
    /// contact (party) ids (never empty after [`Trigger::from_value`]).
    pub who: Vec<String>,
    /// Where the answer goes: [`ReplyStyle`].
    pub reply: ReplyStyle,
    /// The answer, said as you, starts with `[AI] `.
    pub mark: bool,
    /// Additional automatic-message prefixes; built-in markers always remain filtered.
    pub ignore_prefixes: Vec<String>,
}

/// `trigger.who` entry for you.
pub const WHO_ME: &str = "me";
/// `trigger.who` entry for anyone in a space where the words work.
pub const WHO_ANYONE: &str = "*";

/// Where the answer to a trigger word goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplyStyle {
    /// In the thread (话题) of the message, started there when it has none.
    #[default]
    Thread,
    /// A reply quoting the message, in the chat.
    Quote,
    /// A plain new message in the chat.
    Message,
}

impl ReplyStyle {
    pub const ALL: [ReplyStyle; 3] = [Self::Thread, Self::Quote, Self::Message];

    pub fn code(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Quote => "quote",
            Self::Message => "message",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.code() == s)
    }
}

/// `trigger.where` entry for all private chats.
pub const WHERE_ALL_DMS: &str = "dm:*";
/// `trigger.where` entry for every group and private chat.
pub const WHERE_EVERYWHERE: &str = "*";

/// `where` as it counts: empty (switched on without choosing where — rows written before `*`
/// existed) means everywhere, and `*` makes the rest redundant.
pub fn normal_where(list: Vec<String>) -> Vec<String> {
    if list.is_empty() || list.iter().any(|w| w == WHERE_EVERYWHERE) {
        vec![WHERE_EVERYWHERE.to_string()]
    } else {
        list
    }
}

/// The text after leading mentions (`@林知远 /bot 测试` → `/bot 测试`). `names` are the message's
/// mention names, so a name with a space is taken whole; any other `@word` goes up to the next
/// space.
pub fn strip_leading_mentions<'a>(text: &'a str, names: &[String]) -> &'a str {
    let mut names: Vec<&str> = names
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let mut t = text.trim_start();
    while let Some(rest) = t.strip_prefix('@').or_else(|| t.strip_prefix('＠')) {
        let after = names
            .iter()
            .find_map(|n| rest.strip_prefix(n))
            .unwrap_or_else(|| rest.trim_start_matches(|c: char| !c.is_whitespace()));
        t = after.trim_start();
    }
    t
}

impl Trigger {
    pub fn from_value(v: &Value) -> Self {
        let list = |k: &str| -> Vec<String> {
            v[k].as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let who = list("who");
        Self {
            on: v["on"].as_bool().unwrap_or(false),
            words: list("words"),
            r#where: normal_where(list("where")),
            who: if who.is_empty() {
                vec![WHO_ME.to_string()]
            } else {
                who
            },
            reply: v["reply"]
                .as_str()
                .and_then(ReplyStyle::parse)
                .unwrap_or_default(),
            mark: v["mark"].as_bool().unwrap_or(true),
            ignore_prefixes: list("ignore_prefixes"),
        }
    }

    /// May the sender use the words? `you`: the sender is you; `party`: the sender's contact.
    pub fn allows(&self, you: bool, party: Option<&str>) -> bool {
        self.who
            .iter()
            .any(|w| w == WHO_ANYONE || (w == WHO_ME && you) || party.is_some_and(|p| p == w))
    }

    /// Do trigger words work in this space?
    pub fn applies(&self, space_id: &str, dm: bool) -> bool {
        self.r#where.is_empty()
            || self
                .r#where
                .iter()
                .any(|w| w == WHERE_EVERYWHERE || w == space_id || (dm && w == WHERE_ALL_DMS))
    }

    /// The trigger word the text (after leading mentions) starts with and the instruction after
    /// it, wherever the words work or not: what a message said, for the identity block
    /// ([`crate::im_context`]). A word must end there (`/bot` does not start `/bots`).
    pub fn split<'a>(&'a self, text: &str, mentions: &[String]) -> Option<(&'a str, String)> {
        let t = strip_leading_mentions(text, mentions);
        self.words.iter().find_map(|w| {
            let rest = t.strip_prefix(w.as_str())?;
            let ends = rest
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
            ends.then(|| (w.as_str(), rest.trim().to_string()))
        })
    }

    /// The instruction after a trigger word, when the text (after leading mentions) starts with one
    /// in a space where they work. A word must end there (`/bot` does not start `/bots`).
    pub fn instruction(
        &self,
        space_id: &str,
        dm: bool,
        text: &str,
        mentions: &[String],
    ) -> Option<String> {
        if !self.on || !self.applies(space_id, dm) {
            return None;
        }
        self.split(text, mentions).map(|(_, i)| i)
    }
}

// ---------------------------------------------------------------- to-flow

fn quote(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

const FACTS: &str = "event.payload.facts";

/// The flow condition (`expr`) of a condition other than `intent`; it reads the `facts` the
/// pipeline put into the event's payload, so it decides like [`holds`].
pub fn condition(when: &When) -> Option<String> {
    Some(match when {
        When::MentionOrDm => format!("{FACTS}.mentioned or {FACTS}.dm"),
        When::Mention => format!("{FACTS}.mentioned"),
        When::Dm => format!("{FACTS}.dm"),
        When::ReplyToMe => format!("{FACTS}.reply_to_me"),
        When::Keyword { words } => words
            .iter()
            .map(|w| format!("event.payload.text contains {}", quote(w)))
            .collect::<Vec<_>>()
            .join(" or "),
        When::Intent { .. } => return None,
        When::Member { parties, teams } => {
            let mut any = Vec::new();
            if !parties.is_empty() {
                let list: Vec<String> = parties.iter().map(|p| quote(p)).collect();
                any.push(format!("event.actor_party_id in [{}]", list.join(", ")));
            }
            for t in teams {
                any.push(format!("{FACTS}.sender_teams contains {}", quote(t)));
            }
            format!("{FACTS}.sender_confirmed and ({})", any.join(" or "))
        }
        When::External => format!("{FACTS}.sender_external"),
        When::Hours {
            outside,
            start,
            end,
            days,
        } => {
            let ds: Vec<String> = days.iter().map(u32::to_string).collect();
            let day = format!("{FACTS}.weekday in [{}]", ds.join(", "));
            let inside = if start < end {
                format!("({day} and {FACTS}.minute >= {start} and {FACTS}.minute < {end})")
            } else {
                let prev: Vec<String> = days
                    .iter()
                    .map(|d| (if *d == 7 { 1 } else { d + 1 }).to_string())
                    .collect();
                format!(
                    "(({day} and {FACTS}.minute >= {start}) or ({FACTS}.weekday in [{}] and {FACTS}.minute < {end}))",
                    prev.join(", ")
                )
            };
            if *outside {
                format!("not {inside}")
            } else {
                inside
            }
        }
        When::All => "true".into(),
        When::OtherGroup => format!("{FACTS}.group and not {FACTS}.dm and not {FACTS}.mentioned"),
    })
}

/// The `action` node of an act (`None` for `record`: that branch just ends, the event stays
/// recorded).
fn act_node(id: String, act: Act, owner: &str) -> Option<Node> {
    let (title, config) = match act {
        Act::Record => return None,
        Act::Reply => ("直接回复", json!({"action": "reply_as", "generate": true})),
        Act::Draft => (
            "起草，等你批准",
            json!({"action": "reply_as", "generate": true, "approve": true}),
        ),
        Act::CreateItem(m) => ("建事项", json!({"action": "item.create", "mode": m.code()})),
        Act::Handoff => (
            "转给你",
            json!({"action": "notify", "text": format!("有人找{owner}：{{{{event.summary}}}}")}),
        ),
    };
    Some(Node {
        id,
        kind: "action".into(),
        title: title.into(),
        note: String::new(),
        config,
        on_fail: None,
    })
}

/// Compiles rules into a `kind=response` flow graph: `trigger(message, spaces=[space])`,
/// then a `filter` per run of ordinary rules (one branch per rule) and a `bot` judgement per
/// `intent` rule, each falling through to the next; every branch leads to its action node
/// (`reply_as`, `item.create`, `notify`). Muting is compiled in (the flow replaces the rules, so a
/// muted space stays quiet); a handoff's “我请…来看。” is sent only when not muted.
pub fn compile(rules: &[Rule], space_id: &str, muted: bool, owner: &str) -> Graph {
    let mut nodes = vec![Node {
        id: "t".into(),
        kind: "trigger".into(),
        title: "群消息".into(),
        note: String::new(),
        config: json!({"source": "im", "spaces": [space_id]}),
        on_fail: None,
    }];
    let mut edges: Vec<Edge> = Vec::new();
    // where the previous step continues when its rules do not match: (node, branch label)
    let mut open: Option<(String, String)> = Some(("t".into(), String::new()));
    let link = |edges: &mut Vec<Edge>, from: &(String, String), to: &str| {
        edges.push(Edge {
            from: from.0.clone(),
            to: to.into(),
            label: from.1.clone(),
        });
    };
    let mut i = 0;
    while i < rules.len() {
        let Some(from) = open.take() else { break };
        if matches!(rules[i].when, When::Intent { .. }) {
            let start = i;
            while i < rules.len()
                && i - start < COMPILE_INTENT_BATCH_MAX
                && matches!(rules[i].when, When::Intent { .. })
            {
                i += 1;
            }
            let id = format!("b{}", start + 1);
            if i - start == 1 {
                let When::Intent { text } = &rules[start].when else {
                    unreachable!()
                };
                nodes.push(Node {
                    id: id.clone(),
                    kind: "bot".into(),
                    title: cut(&format!("判断：{text}"), 50),
                    note: String::new(),
                    config: json!({
                        "instruction": format!("这条消息是否符合：{text}"),
                        "branches": ["是", "否"],
                    }),
                    // a failed judgement counts as no, when there is something after it
                    on_fail: (i < rules.len()).then(|| "branch:否".to_string()),
                });
                link(&mut edges, &from, &id);
                let act = if muted {
                    quiet_action(rules[start].act)
                } else {
                    rules[start].act
                };
                push_act(
                    &mut nodes,
                    &mut edges,
                    (&id, "是"),
                    start,
                    act,
                    muted,
                    owner,
                );
                open = Some((id, "否".into()));
            } else {
                // Batch consecutive intent rules into a single bot node so one model call
                // evaluates all of them without repeating the message context.
                let mut instruction = String::from(
                    "按顺序判断这条消息最先符合哪一条条件（都不符合选「都不符合」）：",
                );
                let mut branches: Vec<String> = Vec::with_capacity(i - start + 1);
                for (k, r) in rules.iter().enumerate().take(i).skip(start) {
                    let When::Intent { text } = &r.when else {
                        unreachable!()
                    };
                    let label = format!("规则{}", k + 1);
                    instruction.push_str(&format!("\n- {label}：{text}"));
                    branches.push(label);
                }
                branches.push("都不符合".into());
                nodes.push(Node {
                    id: id.clone(),
                    kind: "bot".into(),
                    title: format!("判断规则 {}–{}", start + 1, i),
                    note: String::new(),
                    config: json!({
                        "instruction": instruction,
                        "branches": branches,
                    }),
                    on_fail: (i < rules.len()).then(|| "branch:都不符合".to_string()),
                });
                link(&mut edges, &from, &id);
                for (k, r) in rules.iter().enumerate().take(i).skip(start) {
                    let act = if muted { quiet_action(r.act) } else { r.act };
                    let label = format!("规则{}", k + 1);
                    push_act(&mut nodes, &mut edges, (&id, &label), k, act, muted, owner);
                }
                open = Some((id, "都不符合".into()));
            }
            continue;
        }
        let start = i;
        while i < rules.len() && !matches!(rules[i].when, When::Intent { .. }) {
            i += 1;
        }
        let id = format!("f{}", start + 1);
        let branches: Vec<Value> = (start..i)
            .map(|k| {
                json!({"label": format!("规则{}", k + 1), "when": condition(&rules[k].when).unwrap_or_else(|| "false".into())})
            })
            .collect();
        let tail = i < rules.len();
        let mut cfg = json!({"rules": branches});
        if tail {
            cfg["else_label"] = json!("其他");
        }
        nodes.push(Node {
            id: id.clone(),
            kind: "filter".into(),
            title: format!("规则 {}–{}", start + 1, i),
            note: String::new(),
            config: cfg,
            on_fail: None,
        });
        link(&mut edges, &from, &id);
        for (k, r) in rules.iter().enumerate().take(i).skip(start) {
            let act = if muted { quiet_action(r.act) } else { r.act };
            let label = format!("规则{}", k + 1);
            push_act(&mut nodes, &mut edges, (&id, &label), k, act, muted, owner);
        }
        // an `all` rule ends the chain: nothing after it is reachable
        if tail && !rules[start..i].iter().any(|r| r.when == When::All) {
            open = Some((id, "其他".into()));
        }
    }
    Graph { nodes, edges }
}

fn push_act(
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    from: (&str, &str),
    k: usize,
    act: Act,
    muted: bool,
    owner: &str,
) {
    let Some(n) = act_node(format!("a{}", k + 1), act, owner) else {
        return;
    };
    let id = n.id.clone();
    nodes.push(n);
    edges.push(Edge {
        from: from.0.into(),
        to: id.clone(),
        label: from.1.into(),
    });
    if act == Act::Handoff && !muted {
        let ack = format!("k{}", k + 1);
        nodes.push(Node {
            id: ack.clone(),
            kind: "action".into(),
            title: "告诉对方".into(),
            note: String::new(),
            config: json!({"action": "reply_as", "text": handoff_ack(owner)}),
            on_fail: None,
        });
        edges.push(Edge {
            from: id,
            to: ack,
            label: String::new(),
        });
    }
}

/// The handoff's reply.
pub fn handoff_ack(owner: &str) -> String {
    HANDOFF_ACK.replacen("{}", owner, 1)
}

/// [`handoff_ack`], `{}` for your name.
pub const HANDOFF_ACK: &str = "我请 {} 来看。";

// ---------------------------------------------------------------- to-flow (storage)

/// `flows.kind` of a flow converted from rules.
pub const FLOW_KIND: &str = "response";
/// Its `origin_label`.
pub const FLOW_ORIGIN_LABEL: &str = "由响应规则转成";

/// `POST /spaces/:id/rules/to-flow`: compiles the effective rules into a `kind=response`
/// flow (enabled), writes its id to `flow_id`; a revision of the space, audit `space.to_flow`.
/// With `nodes` the graph is validated against the registered node kinds first (`400` with the
/// flow module's text). Already converted → `409`.
pub fn to_flow(
    w: &mut steve_store::W<'_>,
    viewer: &steve_store::base::Viewer,
    space_id: &str,
    nodes: Option<&steve_flow::nodes::NodeRegistry>,
    now: i64,
) -> ChannelResult<String> {
    let s = crate::spaces::checked_space(w, viewer, space_id)?;
    let set = crate::spaces::settings(w, &s.id)?;
    if let Some(f) = &set.flow_id
        && steve_flow::flows::get(w, f)?.is_some_and(|f| !f.deleted)
    {
        return Err(ApiError::conflict(steve_i18n::t!(
            "channels.error.already_flow",
            flow = steve_flow::flows::name_of(w, f)?
        )));
    }
    if set.attention >= crate::spaces::Attention::Quiet {
        return Err(ApiError::rule(
            if set.attention == crate::spaces::Attention::Blocked {
                steve_i18n::t!("channels.error.to_flow_blocked")
            } else {
                steve_i18n::t!("channels.error.to_flow_quiet")
            },
        ));
    }
    let ws = &viewer.ws;
    let (_, mode) = crate::bindings::connection_account(w, ws, &s.connection_id)?;
    let account = crate::spaces::channel_settings(w, ws, &s.connection_id)?
        .map(|c| c.rules)
        .unwrap_or_else(|| match mode {
            Mode::Own => crate::spaces::default_self_rules(),
            Mode::Bot => crate::spaces::default_bot_rules(),
        });
    let rules = load(set.rules.as_ref().unwrap_or(&account), mode);
    let owner = crate::personas::owner_name_for_space(w, ws, &s.id)?;
    let graph = compile(&rules, &s.id, set.muted, &owner);
    let graph = match nodes {
        Some(reg) => steve_flow::graph::validate(&graph.to_value(), reg).map_err(ApiError::rule)?,
        None => graph,
    };
    let label = crate::spaces::display_title(w, &s)?;
    let name = cut(
        &format!("「{label}」的响应规则"),
        steve_flow::flows::NAME_MAX - 1,
    );
    let description = format!("由「{label}」的 {} 条响应规则转成", rules.len());
    let flow_id = steve_flow::flows::create(
        w,
        ws,
        &viewer.user,
        &steve_flow::flows::NewFlow {
            kind: FLOW_KIND,
            name: &name,
            description: &description,
            graph: &graph,
            origin: "me",
            origin_label: FLOW_ORIGIN_LABEL,
            enabled: true,
        },
        now,
    )?;
    set_flow(w, viewer, &s.id, &set, Some(&flow_id), "space.to_flow", now)?;
    Ok(flow_id)
}

/// `DELETE /spaces/:id/flow` (“改回规则”): clears `flow_id`; the flow is disabled, not deleted.
/// Nothing converted → nothing changes.
pub fn back_to_rules(
    w: &mut steve_store::W<'_>,
    viewer: &steve_store::base::Viewer,
    space_id: &str,
    now: i64,
) -> ChannelResult<()> {
    let s = crate::spaces::checked_space(w, viewer, space_id)?;
    let set = crate::spaces::settings(w, &s.id)?;
    let Some(f) = &set.flow_id else {
        return Ok(());
    };
    if let Some(row) = steve_flow::flows::get(w, f)?
        && row.workspace_id == viewer.ws
        && !row.deleted
        && row.enabled
    {
        steve_flow::flows::patch(w, &row, &viewer.user, None, None, Some(false), now)?;
    }
    set_flow(w, viewer, &s.id, &set, None, "space.flow_off", now)
}

fn set_flow(
    w: &mut steve_store::W<'_>,
    viewer: &steve_store::base::Viewer,
    space: &str,
    before: &crate::spaces::SpaceSettings,
    flow: Option<&str>,
    action: &str,
    now: i64,
) -> ChannelResult<()> {
    let version = before.revision + 1;
    steve_store::channel_spaces::preferences::set_flow(w, space, flow, version, now)?;
    steve_store::audit_repo::revise(
        w,
        &viewer.ws,
        "space",
        space,
        version,
        &json!({ "flow_id": before.flow_id }),
        &json!({ "flow_id": flow }),
        &viewer.user,
    )?;
    steve_store::audit_repo::record(
        w,
        &viewer.ws,
        &viewer.user,
        action,
        space,
        &json!({ "flow_id": flow.or(before.flow_id.as_deref()) }),
    )?;
    steve_store::channel_spaces::registration::emit(w, &viewer.ws, space);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(v: Value) -> Vec<Rule> {
        parse(&v, Mode::Bot).expect("rules")
    }

    #[test]
    fn parse_and_normalize() {
        let r = rules(json!([
            {"when": {"kind": "mention_or_dm"}, "act": "reply"},
            {"when": {"kind": "keyword", "words": ["告警", " 部署 "]}, "act": "reply"},
            {"when": {"kind": "intent", "text": "需要动手操作"}, "act": "create_item", "mode": "start"},
            {"when": {"kind": "hours", "outside": true}, "act": "handoff"},
            {"when": {"kind": "all"}, "act": "record"}
        ]));
        assert_eq!(r.len(), 5);
        assert_eq!(
            r[1].when,
            When::Keyword {
                words: vec!["告警".into(), "部署".into()]
            }
        );
        assert_eq!(
            to_value(&r)[3]["when"],
            json!({"kind": "hours", "outside": true, "start": "09:00", "end": "18:00", "days": [1, 2, 3, 4, 5]})
        );
        assert_eq!(to_value(&r)[2]["mode"], json!("start"));
    }

    #[test]
    fn validation() {
        let bad = |v: Value| parse(&v, Mode::Bot).expect_err("bad").to_string();
        assert_eq!(bad(json!({})), "参数不对：rules");
        assert_eq!(
            bad(json!([{"when": {"kind": "nope"}, "act": "reply"}])),
            "参数不对：rules.0.when.kind"
        );
        assert_eq!(
            bad(json!([{"when": {"kind": "all"}, "act": "shout"}])),
            "参数不对：rules.0.act"
        );
        assert_eq!(
            bad(json!([{"when": {"kind": "keyword", "words": []}, "act": "reply"}])),
            "参数不对：rules.0.when.words"
        );
        assert_eq!(
            bad(json!([{"when": {"kind": "member"}, "act": "reply"}])),
            "参数不对：rules.0.when"
        );
        assert_eq!(
            bad(json!([{"when": {"kind": "hours", "start": "9点"}, "act": "reply"}])),
            "参数不对：rules.0.when.start"
        );
        let many: Vec<Value> = (0..21)
            .map(|_| json!({"when": {"kind": "all"}, "act": "record"}))
            .collect();
        assert_eq!(bad(Value::Array(many)), "参数不对：rules");
        let own = parse(
            &json!([{"when": {"kind": "dm"}, "act": "reply"}]),
            Mode::Own,
        )
        .expect_err("self");
        assert_eq!(own, ApiError::rule(SELF_NO_REPLY));
        assert!(
            parse(
                &json!([{"when": {"kind": "dm"}, "act": "draft"}]),
                Mode::Own
            )
            .is_ok()
        );
    }

    #[test]
    fn load_is_lenient_and_never_replies_as_you() {
        let r = load(
            &json!([{"when": {"kind": "x"}}, {"when": {"kind": "dm"}, "act": "reply"}]),
            Mode::Own,
        );
        assert_eq!(
            r,
            vec![Rule {
                when: When::Dm,
                act: Act::Draft
            }]
        );
    }

    #[test]
    fn hours_across_midnight() {
        let mut f = Facts {
            weekday: 2,
            minute: 30,
            ..Facts::default()
        };
        // Monday 22:00 – Tuesday 06:00 counts for Monday
        assert!(in_hours(&f, 22 * 60, 6 * 60, &[1]));
        assert!(!in_hours(&f, 22 * 60, 6 * 60, &[2]));
        f.minute = 23 * 60;
        assert!(in_hours(&f, 22 * 60, 6 * 60, &[2]));
    }

    #[test]
    fn trigger_words() {
        let t = Trigger::from_value(
            &json!({"on": true, "words": ["/bot", "/steve"], "where": ["spc_dev", "dm:*"], "mark": true}),
        );
        assert_eq!(
            t.instruction("spc_dev", false, " /bot 回复他进度", &[]),
            Some("回复他进度".into())
        );
        assert_eq!(
            t.instruction("spc_x", true, "/steve 说好", &[]),
            Some("说好".into())
        );
        assert_eq!(
            t.instruction("spc_x", false, "/bot 说好", &[]),
            None,
            "not here"
        );
        assert_eq!(t.instruction("spc_dev", false, "/bots 列表", &[]), None);
        assert_eq!(t.instruction("spc_dev", false, "请 /bot", &[]), None);
        let off = Trigger::from_value(&json!({"on": false, "words": ["/bot"], "where": ["dm:*"]}));
        assert_eq!(off.instruction("spc_x", true, "/bot 好", &[]), None);
    }

    /// Prod had `{"on":true,"where":[]}` (switched on without choosing where): that is everywhere,
    /// like `*`.
    #[test]
    fn trigger_without_where_works_everywhere() {
        let prod = Trigger::from_value(
            &json!({"on": true, "words": ["/bot"], "where": [], "mark": false}),
        );
        assert_eq!(prod.r#where, vec![WHERE_EVERYWHERE.to_string()]);
        assert_eq!(
            prod.instruction("spc_any", true, "/bot 你好", &[]),
            Some("你好".into()),
            "a DM (your chat with yourself)"
        );
        assert_eq!(
            prod.instruction("spc_group", false, "/bot 你好", &[]),
            Some("你好".into()),
            "a group"
        );
        let star =
            Trigger::from_value(&json!({"on": true, "words": ["/bot"], "where": ["*", "spc_a"]}));
        assert_eq!(star.r#where, vec!["*".to_string()]);
        assert!(star.applies("spc_b", false));
        let missing = Trigger::from_value(&json!({"on": true, "words": ["/bot"]}));
        assert!(missing.applies("spc_b", false));
        let dms = Trigger::from_value(&json!({"on": true, "words": ["/bot"], "where": ["dm:*"]}));
        assert!(dms.applies("spc_b", true));
        assert!(!dms.applies("spc_b", false));
    }

    #[test]
    fn trigger_after_leading_mentions() {
        let t = Trigger::from_value(&json!({"on": true, "words": ["/bot"], "where": ["*"]}));
        let names = vec!["林知远".to_string()];
        assert_eq!(
            t.instruction("s", true, "@林知远 /bot 测试", &names),
            Some("测试".into())
        );
        assert_eq!(
            t.instruction("s", true, "@林知远 /bot 测试", &[]),
            Some("测试".into()),
            "no mention names: up to the next space"
        );
        assert_eq!(
            t.instruction(
                "s",
                false,
                " @Lee Peng\u{3000}@张三  /bot 说好",
                &["Lee Peng".into(), "张三".into()]
            ),
            Some("说好".into()),
            "names with spaces, several mentions, full-width space"
        );
        assert_eq!(
            t.instruction("s", false, "@_user_1 /bot 好", &[]),
            Some("好".into()),
            "an unreplaced mention key"
        );
        assert_eq!(t.instruction("s", false, "@林知远 请 /bot", &names), None);
        assert_eq!(strip_leading_mentions("＠林知远 /bot", &names), "/bot");
        assert_eq!(strip_leading_mentions("@", &names), "");
    }

    #[test]
    fn judgement_answers() {
        assert!(parse_judgement("{\"match\": true}"));
        assert!(!parse_judgement("好的：{\"match\": false}"));
        assert!(parse_judgement("是"));
        assert!(!parse_judgement("否"));
        assert!(!parse_judgement(""));
    }

    #[test]
    fn conditions_compile_to_the_same_decision() {
        let all = rules(json!([
            {"when": {"kind": "mention_or_dm"}, "act": "reply"},
            {"when": {"kind": "mention"}, "act": "reply"},
            {"when": {"kind": "dm"}, "act": "reply"},
            {"when": {"kind": "reply_to_me"}, "act": "reply"},
            {"when": {"kind": "keyword", "words": ["告\"警", "Deploy"]}, "act": "reply"},
            {"when": {"kind": "member", "parties": ["ct_a"], "teams": ["tm_x"]}, "act": "reply"},
            {"when": {"kind": "external"}, "act": "reply"},
            {"when": {"kind": "hours", "start": "09:00", "end": "18:00"}, "act": "reply"},
            {"when": {"kind": "hours", "outside": true, "start": "22:00", "end": "06:00", "days": [1, 7]}, "act": "reply"},
            {"when": {"kind": "all"}, "act": "reply"},
            {"when": {"kind": "other_group"}, "act": "reply"}
        ]));
        let cases = [
            Facts::default(),
            Facts {
                dm: true,
                ..Facts::default()
            },
            Facts {
                group: true,
                mentioned: true,
                ..Facts::default()
            },
            Facts {
                group: true,
                ..Facts::default()
            },
            Facts {
                reply_to_me: true,
                ..Facts::default()
            },
            Facts {
                text: "今天 deploy 吗".into(),
                ..Facts::default()
            },
            Facts {
                text: "告\"警".into(),
                ..Facts::default()
            },
            Facts {
                sender_party: Some("ct_a".into()),
                sender_confirmed: true,
                ..Facts::default()
            },
            Facts {
                sender_party: Some("ct_a".into()),
                sender_confirmed: false,
                ..Facts::default()
            },
            Facts {
                sender_teams: vec!["tm_x".into()],
                sender_confirmed: true,
                ..Facts::default()
            },
            Facts {
                sender_external: true,
                ..Facts::default()
            },
            Facts {
                weekday: 3,
                minute: 600,
                ..Facts::default()
            },
            Facts {
                weekday: 2,
                minute: 60,
                ..Facts::default()
            },
            Facts {
                weekday: 1,
                minute: 23 * 60,
                ..Facts::default()
            },
        ];
        for f in &cases {
            // weekdays are 1..=7 for real messages
            let mut f = f.clone();
            if f.weekday == 0 {
                f.weekday = 3;
            }
            let f = &f;
            let event = json!({
                "actor_party_id": f.sender_party,
                "payload": {"text": f.text, "facts": f.to_value()},
            });
            let scope = steve_flow::expr::Scope {
                event: Some(&event),
                ..Default::default()
            };
            for r in &all {
                let src = condition(&r.when).expect("condition");
                steve_flow::expr::parse(&src).unwrap_or_else(|e| panic!("{src}: {e}"));
                assert_eq!(
                    steve_flow::expr::eval(&src, &scope),
                    holds(&r.when, f).expect("holds"),
                    "{src} on {f:?}"
                );
            }
        }
    }

    #[test]
    fn compile_shape() {
        let r = rules(json!([
            {"when": {"kind": "mention_or_dm"}, "act": "reply"},
            {"when": {"kind": "keyword", "words": ["告警"]}, "act": "reply"},
            {"when": {"kind": "intent", "text": "需要动手操作"}, "act": "create_item"},
            {"when": {"kind": "mention"}, "act": "handoff"},
            {"when": {"kind": "all"}, "act": "record"}
        ]));
        let g = compile(&r, "spc_1", false, "Alice");
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["t", "f1", "a1", "a2", "b3", "a3", "f4", "a4", "k4"]);
        let e: Vec<(&str, &str, &str)> = g
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.label.as_str()))
            .collect();
        assert_eq!(
            e,
            [
                ("t", "f1", ""),
                ("f1", "a1", "规则1"),
                ("f1", "a2", "规则2"),
                ("f1", "b3", "其他"),
                ("b3", "a3", "是"),
                ("b3", "f4", "否"),
                ("f4", "a4", "规则4"),
                ("a4", "k4", ""),
            ]
        );
        assert!(e.contains(&("f1", "b3", "其他")));
        assert!(e.contains(&("b3", "f4", "否")));
        assert_eq!(g.trigger().expect("t").config["spaces"], json!(["spc_1"]));
        // muted: only the handoff is left, without the ack
        let m = compile(&r, "spc_1", true, "Alice");
        let ids: Vec<&str> = m.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["t", "f1", "b3", "f4", "a4"]);
    }

    #[test]
    fn batch_judgement_answers() {
        assert_eq!(parse_batch_judgement("{\"match\": 1}", 3), Some(0));
        assert_eq!(parse_batch_judgement("好的：{\"match\": 3}", 3), Some(2));
        assert_eq!(parse_batch_judgement("{\"match\": \"2\"}", 3), Some(1));
        assert_eq!(parse_batch_judgement("{\"match\": null}", 3), None);
        assert_eq!(parse_batch_judgement("{\"match\": 0}", 3), None);
        assert_eq!(parse_batch_judgement("{\"match\": 4}", 3), None);
        assert_eq!(parse_batch_judgement("2", 3), Some(1));
        assert_eq!(parse_batch_judgement("", 3), None);
    }

    #[tokio::test]
    async fn evaluate_batches_reachable_intents_dedupes_and_skips_after_det_match() {
        use std::sync::Mutex;

        struct SpyJudge {
            batches: Mutex<Vec<Vec<String>>>,
            reply: Option<usize>,
        }
        #[async_trait::async_trait]
        impl IntentJudge for SpyJudge {
            async fn judge(&self, _text: &str) -> bool {
                panic!("evaluate must batch through judge_first")
            }
            async fn judge_first(&self, conditions: &[&str]) -> Option<usize> {
                self.batches
                    .lock()
                    .unwrap()
                    .push(conditions.iter().map(|s| (*s).to_string()).collect());
                self.reply
            }
        }

        let rs = rules(json!([
            {"when": {"kind": "keyword", "words": ["紧急"]}, "act": "handoff"},
            {"when": {"kind": "intent", "text": "需要排查故障"}, "act": "create_item"},
            {"when": {"kind": "intent", "text": "需要排查故障"}, "act": "reply"},
            {"when": {"kind": "intent", "text": "询问报价"}, "act": "draft"},
            {"when": {"kind": "mention_or_dm"}, "act": "reply"},
            {"when": {"kind": "intent", "text": "不可达条件"}, "act": "handoff"}
        ]));

        // Case 1: Deterministic rule 0 matches -> zero judge calls, needs_intent_judge = false.
        let urgent = Facts {
            text: "紧急故障".into(),
            mentioned: true,
            ..Facts::default()
        };
        assert!(!needs_intent_judge(&rs, &urgent));
        let spy = SpyJudge {
            batches: Mutex::new(vec![]),
            reply: Some(0),
        };
        let v = evaluate(&rs, &urgent, false, &spy).await;
        assert_eq!((v.rule, v.act), (Some(0), Act::Handoff));
        assert!(spy.batches.lock().unwrap().is_empty());

        // Case 2: Rules 1..3 reachable before deterministic rule 4; duplicate rule 2 deduped;
        // rule 5 unreachable after rule 4; one batched call receives ["需要排查故障", "询问报价"].
        let normal = Facts {
            text: "发一下报价单".into(),
            mentioned: true,
            ..Facts::default()
        };
        assert!(needs_intent_judge(&rs, &normal));
        let spy = SpyJudge {
            batches: Mutex::new(vec![]),
            reply: Some(1),
        };
        let v = evaluate(&rs, &normal, false, &spy).await;
        assert_eq!((v.rule, v.act), (Some(3), Act::Draft));
        assert_eq!(
            *spy.batches.lock().unwrap(),
            vec![vec!["需要排查故障".to_string(), "询问报价".to_string()]]
        );

        // Case 3: No intent matches -> falls through to deterministic rule 4.
        let spy = SpyJudge {
            batches: Mutex::new(vec![]),
            reply: None,
        };
        let v = evaluate(&rs, &normal, false, &spy).await;
        assert_eq!((v.rule, v.act), (Some(4), Act::Reply));
    }

    #[tokio::test]
    async fn gateway_judge_batches_once_and_reuses_input_hash_cache() {
        let gw = steve_modules::testkit::FakeGateway::new();
        gw.reply("{\"match\": 2}");
        let cache = IntentCache::new();
        let ctx = [ContextLine {
            who: "Alice".into(),
            text: "昨天刚聊过".into(),
        }];
        let judge = GatewayJudge {
            gateway: gw.as_ref(),
            message: "麻烦帮我查一下部署日志",
            sender: "Bob",
            space: "运维群",
            context: &ctx,
            cache: Some(&cache),
        };
        let conds = ["询问报价", "需要排查故障", "请假申请"];
        assert_eq!(judge.judge_first(&conds).await, Some(1));
        assert_eq!(gw.calls.lock().unwrap().len(), 1);
        let prompt = gw.last_prompt().unwrap();
        assert!(prompt.starts_with(INTENT_START));
        assert!(prompt.contains("1. 询问报价\n2. 需要排查故障\n3. 请假申请"));

        // Identical input reuses cached verdict with zero extra gateway calls.
        assert_eq!(judge.judge_first(&conds).await, Some(1));
        assert_eq!(gw.calls.lock().unwrap().len(), 1);

        // Changed message misses cache and queries gateway again.
        gw.reply("{\"match\": null}");
        let other = GatewayJudge {
            message: "今天天气真好",
            ..judge
        };
        assert_eq!(other.judge_first(&conds).await, None);
        assert_eq!(gw.calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn compile_batches_consecutive_intent_rules_into_one_bot_node() {
        let r = rules(json!([
            {"when": {"kind": "intent", "text": "需要排查故障"}, "act": "create_item"},
            {"when": {"kind": "intent", "text": "询问报价"}, "act": "draft"},
            {"when": {"kind": "mention"}, "act": "reply"}
        ]));
        let g = compile(&r, "spc_1", false, "Alice");
        let bot_nodes: Vec<&Node> = g.nodes.iter().filter(|n| n.kind == "bot").collect();
        assert_eq!(
            bot_nodes.len(),
            1,
            "consecutive intents compile to one bot node"
        );
        assert_eq!(
            bot_nodes[0].config["branches"],
            json!(["规则1", "规则2", "都不符合"])
        );
        assert_eq!(bot_nodes[0].on_fail.as_deref(), Some("branch:都不符合"));
    }
}

//! 我的待办: what you yourself are to do — distinct from items, the work handed to Bots.
//!
//! A todo has its text, the messages it came from (`todo_refs`: conversation + Steve message id), its matter,
//! the people involved (`todo_parties`), and a state: `open`, `done`, `dropped`, or `handed` (given to a Bot
//! as an item, `item_id`). Every member has their own todos (`owner`); nobody reads another's.
//!
//! A todo is `kind = todo` or a goal (`kind = goal`): a result you set yourself, with a target date
//! (`target_date`, a local date) and child todos one level down (`parent_id`) and items linked to it
//! (`links`: item `part_of` todo). A goal's [`progress`] is computed, never stored.
//!
//! A deadline is a moment (`due`, Unix ms) or a local date (`due_date`, `YYYY-MM-DD`): a date-only deadline
//! keeps its day in every time zone. One written from a message (`source = steve`) needs the words that say
//! it (`due_quote`); one you or your agent set needs none. `remind_at` and `snooze_until` are moments;
//! `pri` 0 (none) to [`PRI_MAX`]; `star`; `recur` a small RRULE subset ([`Recur`]): completing a recurring
//! todo makes the next one ([`complete`]), at the first occurrence not already past.
//!
//! A digest writes todos straight in (`source = steve`, marked “Steve 记的”), without asking; every change can
//! be undone ([`crate::changes`]).

use chrono::{Days, Months, NaiveDate, NaiveDateTime, TimeZone};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{StoreError, Tx, uid};

use crate::conv::ConvRef;

type Result<T> = std::result::Result<T, StoreError>;

pub const STATES: [&str; 4] = ["open", "done", "dropped", "handed"];
pub const KINDS: [&str; 2] = ["todo", "goal"];
/// `steve`: written from messages by the digest; `you`: by you; `agent`: by your agent in your own session.
pub const SOURCES: [&str; 3] = ["steve", "you", "agent"];
pub const TEXT_MAX: usize = 500;
/// The highest priority (`pri`); 0 is none.
pub const PRI_MAX: i64 = 3;
/// The most occurrences completing a recurring todo late may step over to reach one not past.
const RECUR_STEPS_MAX: u32 = 1000;
/// The longest gap a daylight-saving change can open in a local clock, in minutes.
const DST_GAP_MAX_MIN: i64 = 180;
/// The link from an item to the goal it is part of.
pub const PART_OF: &str = "part_of";

/// A refused todo value: `StoreError::Hook("todo.<code>")`; [`message`] gives its text.
fn bad(code: &str) -> StoreError {
    StoreError::Hook(format!("todo.{code}"))
}

/// The text of a refused todo value, in the request's language; `None` for other errors.
pub fn message(e: &StoreError) -> Option<String> {
    let StoreError::Hook(s) = e else { return None };
    Some(match s.strip_prefix("todo.")? {
        "text" => steve_i18n::t!("digest.todo.text"),
        "kind" => steve_i18n::t!("digest.todo.kind"),
        "date" => steve_i18n::t!("digest.todo.date"),
        "due_both" => steve_i18n::t!("digest.todo.due_both"),
        "goal_fields" => steve_i18n::t!("digest.todo.goal_fields"),
        "target_date" => steve_i18n::t!("digest.todo.target_date"),
        "parent" => steve_i18n::t!("digest.todo.parent"),
        "has_children" => steve_i18n::t!("digest.todo.has_children"),
        "pri" => steve_i18n::t!("digest.todo.pri", max = PRI_MAX),
        "time" => steve_i18n::t!("digest.todo.time"),
        "recur" => steve_i18n::t!("digest.todo.recur"),
        "recur_due" => steve_i18n::t!("digest.todo.recur_due"),
        "goal_hand" => steve_i18n::t!("digest.todo.goal_hand"),
        "matter" => steve_i18n::t!("digest.todo.matter"),
        "item" => steve_i18n::t!("digest.todo.item"),
        "not_goal" => steve_i18n::t!("digest.todo.not_goal"),
        "digest_only" => steve_i18n::t!("digest.todo.digest_only"),
        _ => return None,
    })
}

#[derive(Default)]
pub struct NewTodo<'a> {
    pub ws: &'a str,
    pub owner: &'a str,
    pub text: &'a str,
    pub matter: Option<&'a str>,
    pub due: Option<i64>,
    pub due_quote: Option<&'a str>,
    /// [`SOURCES`]
    pub source: &'a str,
    pub writer: &'a str,
    pub packet: Option<&'a str>,
    pub refs: Vec<(ConvRef, String)>,
    pub parties: Vec<String>,
    /// `todo` (also when empty) or `goal`
    pub kind: &'a str,
    pub parent: Option<&'a str>,
    pub target_date: Option<&'a str>,
    pub due_date: Option<&'a str>,
    pub remind_at: Option<i64>,
    pub snooze_until: Option<i64>,
    pub pri: i64,
    pub star: bool,
    pub recur: Option<&'a str>,
}

/// What a person sets on a todo; validated as a whole ([`check`]) before it is written.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fields {
    pub text: String,
    pub kind: String,
    pub parent: Option<String>,
    pub matter: Option<String>,
    pub target_date: Option<String>,
    pub due: Option<i64>,
    pub due_date: Option<String>,
    /// not set by hand: a hand-set deadline has none
    pub due_quote: Option<String>,
    pub remind_at: Option<i64>,
    pub snooze_until: Option<i64>,
    pub pri: i64,
    pub star: bool,
    pub recur: Option<String>,
}

/// `PATCH` of [`Fields`]: an absent field stays, `null` clears it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub text: Option<String>,
    pub kind: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub parent: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub matter: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub target_date: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub due: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub due_date: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub remind_at: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub snooze_until: Option<Option<i64>>,
    pub pri: Option<i64>,
    pub star: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub recur: Option<Option<String>>,
}

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

impl Patch {
    /// `f` with this patch applied. A new deadline of one form clears the other, and its quote.
    pub fn apply(&self, f: &Fields) -> Fields {
        let mut f = f.clone();
        if let Some(t) = &self.text {
            f.text = t.clone();
        }
        if let Some(k) = &self.kind {
            f.kind = k.clone();
        }
        let set = |slot: &mut Option<String>, v: &Option<Option<String>>| {
            if let Some(v) = v {
                *slot = v.clone();
            }
        };
        set(&mut f.parent, &self.parent);
        set(&mut f.matter, &self.matter);
        set(&mut f.target_date, &self.target_date);
        set(&mut f.recur, &self.recur);
        if self.due.is_some() || self.due_date.is_some() {
            f.due_quote = None;
            if matches!(self.due, Some(Some(_))) && self.due_date.is_none() {
                f.due_date = None;
            }
            if matches!(self.due_date, Some(Some(_))) && self.due.is_none() {
                f.due = None;
            }
        }
        if let Some(v) = self.due {
            f.due = v;
        }
        set(&mut f.due_date, &self.due_date);
        if let Some(v) = self.remind_at {
            f.remind_at = v;
        }
        if let Some(v) = self.snooze_until {
            f.snooze_until = v;
        }
        if let Some(p) = self.pri {
            f.pri = p;
        }
        if let Some(s) = self.star {
            f.star = s;
        }
        f
    }
}

/// `YYYY-MM-DD`, exactly.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .filter(|d| d.format("%Y-%m-%d").to_string() == s)
}

/// How often a todo comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// The RRULE subset a todo repeats by: `FREQ=DAILY|WEEKLY|MONTHLY|YEARLY`, optionally `;INTERVAL=n`
/// (1 to 366). A month or year without the day (the 31st, 29 February) takes its last day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recur {
    pub freq: Freq,
    pub interval: u32,
}

impl Recur {
    pub fn parse(s: &str) -> Option<Recur> {
        let (mut freq, mut interval) = (None, None);
        for part in s.trim().trim_start_matches("RRULE:").split(';') {
            let (k, v) = part.split_once('=')?;
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" if freq.is_none() => {
                    freq = Some(match v.trim().to_ascii_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        _ => return None,
                    })
                }
                "INTERVAL" if interval.is_none() => {
                    interval = Some(
                        v.trim()
                            .parse::<u32>()
                            .ok()
                            .filter(|n| (1..=366).contains(n))?,
                    )
                }
                _ => return None,
            }
        }
        Some(Recur {
            freq: freq?,
            interval: interval.unwrap_or(1),
        })
    }

    /// The canonical text, as stored.
    pub fn rule(&self) -> String {
        let f = match self.freq {
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
            Freq::Yearly => "YEARLY",
        };
        if self.interval == 1 {
            format!("FREQ={f}")
        } else {
            format!("FREQ={f};INTERVAL={}", self.interval)
        }
    }

    /// The `k`-th occurrence after `d` (counted from `d`, so month ends do not drift).
    pub fn nth(&self, d: NaiveDateTime, k: u32) -> Option<NaiveDateTime> {
        let n = self.interval.checked_mul(k)?;
        match self.freq {
            Freq::Daily => d.checked_add_days(Days::new(n.into())),
            Freq::Weekly => d.checked_add_days(Days::new(u64::from(n) * 7)),
            Freq::Monthly => d.checked_add_months(Months::new(n)),
            Freq::Yearly => d.checked_add_months(Months::new(n.checked_mul(12)?)),
        }
    }
}

/// The time zone a person's local dates and times are in: their notification policy's, else the Hub's.
#[derive(Debug, Clone, Copy)]
pub enum Zone {
    Tz(chrono_tz::Tz),
    Hub,
}

impl Zone {
    pub fn of(c: &Connection, ws: &str, user: &str) -> Zone {
        c.query_row(
            "SELECT timezone FROM notification_policies WHERE workspace_id = ? AND user_id = ?",
            params![ws, user],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| s.parse::<chrono_tz::Tz>().ok())
        .map_or(Zone::Hub, Zone::Tz)
    }

    pub fn name(&self) -> String {
        match self {
            Zone::Tz(t) => t.name().to_string(),
            Zone::Hub => "hub-local".into(),
        }
    }

    pub fn local(&self, ms: i64) -> Option<NaiveDateTime> {
        match self {
            Zone::Tz(t) => t.timestamp_millis_opt(ms).single().map(|d| d.naive_local()),
            Zone::Hub => chrono::Local
                .timestamp_millis_opt(ms)
                .single()
                .map(|d| d.naive_local()),
        }
    }

    /// Unix ms of a local time: the first of a repeated one, and one in a daylight-saving gap moved past it.
    pub fn instant(&self, at: NaiveDateTime) -> Option<i64> {
        fn first<T: TimeZone>(tz: &T, at: NaiveDateTime) -> Option<i64> {
            (0..=DST_GAP_MAX_MIN).find_map(|m| {
                tz.from_local_datetime(&(at + chrono::Duration::minutes(m)))
                    .earliest()
                    .map(|d| d.timestamp_millis())
            })
        }
        match self {
            Zone::Tz(t) => first(t, at),
            Zone::Hub => first(&chrono::Local, at),
        }
    }

    /// RFC 3339 with this zone's offset.
    pub fn show(&self, ms: i64) -> Option<String> {
        match self {
            Zone::Tz(t) => t.timestamp_millis_opt(ms).single().map(|d| d.to_rfc3339()),
            Zone::Hub => chrono::Local
                .timestamp_millis_opt(ms)
                .single()
                .map(|d| d.to_rfc3339()),
        }
    }
}

/// Whether `id` (of `ws`) has child todos.
fn has_children(c: &Connection, id: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM todos WHERE parent_id = ?)",
        [id],
        |r| r.get(0),
    )?)
}

/// Whether `user` is the person the digest is for (the workspace owner). Matters, source messages and
/// people come from the digest of that person's conversations, so only their todos link to them: a todo
/// of any other member links to none and shows none.
pub fn digest_owner(c: &Connection, ws: &str, user: &str) -> Result<bool> {
    Ok(crate::conv::owner(c, ws)?.as_deref() == Some(user))
}

/// Validates and normalizes `f` for a todo of `owner` (`id`: the todo itself, when it exists).
pub fn check(
    c: &Connection,
    ws: &str,
    owner: &str,
    id: Option<&str>,
    f: &mut Fields,
) -> Result<()> {
    f.text = f.text.trim().to_string();
    if f.text.is_empty() || f.text.chars().count() > TEXT_MAX {
        return Err(bad("text"));
    }
    if f.kind.is_empty() {
        f.kind = "todo".into();
    }
    if !KINDS.contains(&f.kind.as_str()) {
        return Err(bad("kind"));
    }
    for d in [&f.target_date, &f.due_date].into_iter().flatten() {
        if parse_date(d).is_none() {
            return Err(bad("date"));
        }
    }
    if f.due.is_some() && f.due_date.is_some() {
        return Err(bad("due_both"));
    }
    if [f.due, f.remind_at, f.snooze_until]
        .into_iter()
        .flatten()
        .any(|t| t < 0)
    {
        return Err(bad("time"));
    }
    if !(0..=PRI_MAX).contains(&f.pri) {
        return Err(bad("pri"));
    }
    if let Some(r) = &f.recur {
        f.recur = Some(Recur::parse(r).ok_or_else(|| bad("recur"))?.rule());
        if f.due.is_none() && f.due_date.is_none() {
            return Err(bad("recur_due"));
        }
    }
    if f.kind == "goal" {
        if f.parent.is_some() || f.due.is_some() || f.due_date.is_some() || f.recur.is_some() {
            return Err(bad("goal_fields"));
        }
    } else {
        if f.target_date.is_some() {
            return Err(bad("target_date"));
        }
        if let Some(id) = id
            && has_children(c, id)?
        {
            return Err(bad("has_children"));
        }
    }
    if let Some(p) = &f.parent {
        let ok: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM todos WHERE workspace_id = ? AND id = ? AND owner = ? AND kind = 'goal')",
            params![ws, p, owner],
            |r| r.get(0),
        )?;
        if !ok || Some(p.as_str()) == id {
            return Err(bad("parent"));
        }
    }
    if let Some(m) = &f.matter {
        // a matter already on the todo stays (the workspace may have changed hands); a new one is the digest owner's
        let kept = match id {
            Some(id) => {
                c.query_row(
                    "SELECT matter_id FROM todos WHERE workspace_id = ? AND id = ?",
                    params![ws, id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .as_deref()
                    == Some(m.as_str())
            }
            None => false,
        };
        if !kept && !digest_owner(c, ws, owner)? {
            return Err(bad("digest_only"));
        }
        f.matter = Some(
            crate::matter::resolve(c, ws, m)?
                .ok_or_else(|| bad("matter"))?
                .0,
        );
    }
    Ok(())
}

pub fn create(tx: &Tx, t: &NewTodo, now: i64) -> Result<String> {
    if !SOURCES.contains(&t.source) {
        return Err(StoreError::Hook("invalid todo source".into()));
    }
    let owner: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
        params![t.ws, t.owner],
        |r| r.get(0),
    )?;
    if !owner {
        return Err(StoreError::Hook("unknown todo owner".into()));
    }
    let mut f = Fields {
        text: t.text.into(),
        kind: t.kind.into(),
        parent: t.parent.map(str::to_string),
        matter: t.matter.map(str::to_string),
        target_date: t.target_date.map(str::to_string),
        due: t.due,
        due_date: t.due_date.map(str::to_string),
        due_quote: t.due_quote.map(str::to_string),
        remind_at: t.remind_at,
        snooze_until: t.snooze_until,
        pri: t.pri,
        star: t.star,
        recur: t.recur.map(str::to_string),
    };
    check(tx, t.ws, t.owner, None, &mut f)?;
    if (!t.refs.is_empty() || !t.parties.is_empty()) && !digest_owner(tx, t.ws, t.owner)? {
        return Err(bad("digest_only"));
    }
    for p in &t.parties {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM parties WHERE workspace_id = ? AND id = ?)",
            params![t.ws, p],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::Hook("unknown todo party".into()));
        }
    }
    for (cv, id) in &t.refs {
        if !crate::conv::owner_sees(tx, t.ws, cv)?
            || crate::conv::by_ids(tx, t.ws, cv, &[id.clone()])?.len() != 1
        {
            return Err(StoreError::Hook("unknown todo message".into()));
        }
    }
    if let Some(packet) = t.packet {
        if crate::queue::get(tx, t.ws, packet)?.is_none() {
            return Err(StoreError::Hook("unknown todo packet".into()));
        }
    }
    // a deadline read from messages must be in their words; one you or your agent set is yours
    if t.source == "steve" && (t.due.is_some() || t.due_date.is_some()) {
        let quote = t
            .due_quote
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| StoreError::Hook("todo deadline needs its source words".into()))?;
        let mut found = false;
        for (cv, id) in &t.refs {
            found |= crate::conv::by_ids(tx, t.ws, cv, &[id.clone()])?
                .iter()
                .any(|m| m.text.contains(quote));
        }
        if !found {
            return Err(StoreError::Hook(
                "todo deadline quote not in source messages".into(),
            ));
        }
    } else if t.source != "steve" {
        f.due_quote = None;
    }
    let id = insert(tx, t.ws, t.owner, &f, t.source, t.writer, t.packet, now)?;
    for (conv, msg) in &t.refs {
        tx.execute(
            "INSERT OR IGNORE INTO todo_refs (todo_id, conv, message_id) VALUES (?, ?, ?)",
            params![id, conv.key(), msg],
        )?;
    }
    for p in &t.parties {
        tx.execute(
            "INSERT OR IGNORE INTO todo_parties (todo_id, party_id) VALUES (?, ?)",
            params![id, p],
        )?;
    }
    Ok(id)
}

fn due_kind(f: &Fields) -> Option<&'static str> {
    if f.due.is_some() {
        Some("time")
    } else if f.due_date.is_some() {
        Some("date")
    } else {
        None
    }
}

/// Inserts checked fields.
#[allow(clippy::too_many_arguments)]
fn insert(
    tx: &Tx,
    ws: &str,
    owner: &str,
    f: &Fields,
    source: &str,
    writer: &str,
    packet: Option<&str>,
    now: i64,
) -> Result<String> {
    let id = uid("td");
    tx.execute(
        "INSERT INTO todos (id, workspace_id, owner, text, matter_id, due, due_quote, source, writer, packet_id, created, updated,
            kind, parent_id, target_date, due_kind, due_date, remind_at, snooze_until, pri, star, recur)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id, ws, owner, f.text, f.matter, f.due, f.due_quote, source, writer, packet, now, now,
            f.kind, f.parent, f.target_date, due_kind(f), f.due_date, f.remind_at, f.snooze_until,
            f.pri, f.star, f.recur
        ],
    )?;
    crate::event(tx, ws, &id);
    if let Some(p) = &f.parent {
        crate::event(tx, ws, p);
    }
    Ok(id)
}

/// `(owner, fields, updated)` of a todo of `ws`.
pub fn fields(c: &Connection, ws: &str, id: &str) -> Result<Option<(String, Fields, i64)>> {
    Ok(c.prepare_cached(
        "SELECT owner, text, kind, parent_id, matter_id, target_date, due, due_date, due_quote, remind_at, snooze_until,
                pri, star, recur, updated
           FROM todos WHERE workspace_id = ? AND id = ?",
    )?
    .query_row(params![ws, id], |r| {
        Ok((
            r.get(0)?,
            Fields {
                text: r.get(1)?,
                kind: r.get(2)?,
                parent: r.get(3)?,
                matter: r.get(4)?,
                target_date: r.get(5)?,
                due: r.get(6)?,
                due_date: r.get(7)?,
                due_quote: r.get(8)?,
                remind_at: r.get(9)?,
                snooze_until: r.get(10)?,
                pri: r.get(11)?,
                star: r.get(12)?,
                recur: r.get(13)?,
            },
            r.get(14)?,
        ))
    })
    .optional()?)
}

/// The owner of a todo of `ws`.
pub fn owner_of(c: &Connection, ws: &str, id: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT owner FROM todos WHERE workspace_id = ? AND id = ?",
        params![ws, id],
        |r| r.get(0),
    )
    .optional()?)
}

/// Writes `f` (checked here) over todo `id`. `false`: no such todo.
pub fn write(tx: &Tx, ws: &str, id: &str, f: &Fields, now: i64) -> Result<bool> {
    let Some((owner, old, _)) = fields(tx, ws, id)? else {
        return Ok(false);
    };
    let mut f = f.clone();
    check(tx, ws, &owner, Some(id), &mut f)?;
    tx.execute(
        "UPDATE todos SET text = ?, kind = ?, parent_id = ?, matter_id = ?, target_date = ?, due = ?, due_kind = ?, due_date = ?,
            due_quote = ?, remind_at = ?, snooze_until = ?, pri = ?, star = ?, recur = ?, updated = ?
          WHERE workspace_id = ? AND id = ?",
        params![
            f.text, f.kind, f.parent, f.matter, f.target_date, f.due, due_kind(&f), f.due_date,
            f.due_quote, f.remind_at, f.snooze_until, f.pri, f.star, f.recur, now, ws, id
        ],
    )?;
    crate::event(tx, ws, id);
    for p in [old.parent, f.parent].into_iter().flatten() {
        crate::event(tx, ws, &p);
    }
    Ok(true)
}

/// Applies `p` to todo `id`: `(before, after)`, or `None` when there is no such todo.
pub fn edit(tx: &Tx, ws: &str, id: &str, p: &Patch, now: i64) -> Result<Option<(Fields, Fields)>> {
    let Some((owner, before, _)) = fields(tx, ws, id)? else {
        return Ok(None);
    };
    let mut after = p.apply(&before);
    check(tx, ws, &owner, Some(id), &mut after)?;
    write(tx, ws, id, &after, now)?;
    Ok(Some((before, after)))
}

/// `(state, item_id, text, due)` of a todo of `ws`.
pub fn current(
    c: &Connection,
    ws: &str,
    id: &str,
) -> Result<Option<(String, Option<String>, String, Option<i64>)>> {
    Ok(c.prepare_cached(
        "SELECT state, item_id, text, due FROM todos WHERE workspace_id = ? AND id = ?",
    )?
    .query_row(params![ws, id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .optional()?)
}

/// Sets the state (and the item when `handed`).
pub fn set_state(
    tx: &Tx,
    ws: &str,
    id: &str,
    state: &str,
    item: Option<&str>,
    now: i64,
) -> Result<bool> {
    if !STATES.contains(&state) || (state == "handed") != item.is_some() {
        return Err(StoreError::Hook("invalid todo state or handoff".into()));
    }
    if let Some(item) = item {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE workspace_id = ? AND id = ?)",
            params![ws, item],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::Hook("unknown todo handoff item".into()));
        }
        let goal: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM todos WHERE workspace_id = ? AND id = ? AND kind = 'goal')",
            params![ws, id],
            |r| r.get(0),
        )?;
        if goal {
            return Err(bad("goal_hand"));
        }
    }
    let closed = (state != "open").then_some(now);
    let n = tx.execute(
        "UPDATE todos SET state = ?, item_id = ?, closed = ?, updated = ? WHERE workspace_id = ? AND id = ?",
        params![state, item, closed, now, ws, id],
    )?;
    crate::event(tx, ws, id);
    if let Some((_, f, _)) = fields(tx, ws, id)?
        && let Some(p) = f.parent
    {
        crate::event(tx, ws, &p);
    }
    Ok(n > 0)
}

/// The next occurrence of a recurring todo once `f`'s is done: the first not already past at `now`
/// (a date-only one: not before today), its reminder moved by as many steps. `None`: it does not recur.
pub fn next_occurrence(z: Zone, f: &Fields, now: i64) -> Option<Fields> {
    let r = Recur::parse(f.recur.as_deref()?)?;
    let remind = f.remind_at.and_then(|t| z.local(t));
    let step = |k: u32| -> Option<i64> {
        match remind {
            Some(at) => z.instant(r.nth(at, k)?),
            None => None,
        }
    };
    let mut next = f.clone();
    next.snooze_until = None;
    next.due_quote = None;
    if let Some(d) = &f.due_date {
        let anchor = parse_date(d)?.and_hms_opt(0, 0, 0)?;
        let today = z.local(now)?.date();
        let k =
            (1..=RECUR_STEPS_MAX).find(|k| r.nth(anchor, *k).is_some_and(|x| x.date() >= today))?;
        next.due_date = Some(r.nth(anchor, k)?.date().format("%Y-%m-%d").to_string());
        next.remind_at = step(k);
    } else {
        let anchor = z.local(f.due?)?;
        let k = (1..=RECUR_STEPS_MAX).find(|k| {
            r.nth(anchor, *k)
                .and_then(|x| z.instant(x))
                .is_some_and(|t| t > now)
        })?;
        next.due = z.instant(r.nth(anchor, k)?);
        next.remind_at = step(k);
    }
    Some(next)
}

/// Sets the state like [`set_state`]; a recurring todo done makes its next occurrence (same owner,
/// source, writer, matter, goal and people), whose id is returned.
pub fn complete(
    tx: &Tx,
    ws: &str,
    id: &str,
    state: &str,
    item: Option<&str>,
    now: i64,
) -> Result<Option<String>> {
    let Some((owner, f, _)) = fields(tx, ws, id)? else {
        return Ok(None);
    };
    let was: String = tx.query_row("SELECT state FROM todos WHERE id = ?", [id], |r| r.get(0))?;
    set_state(tx, ws, id, state, item, now)?;
    if state != "done" || was == "done" {
        return Ok(None);
    }
    let Some(next) = next_occurrence(Zone::of(tx, ws, &owner), &f, now) else {
        return Ok(None);
    };
    let (source, writer): (String, String) =
        tx.query_row("SELECT source, writer FROM todos WHERE id = ?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let mut next = next;
    check(tx, ws, &owner, None, &mut next)?;
    let n = insert(tx, ws, &owner, &next, &source, &writer, None, now)?;
    tx.execute(
        "INSERT INTO todo_parties (todo_id, party_id) SELECT ?, party_id FROM todo_parties WHERE todo_id = ?",
        params![n, id],
    )?;
    Ok(Some(n))
}

/// Links (`on`) or unlinks item `item` to goal `goal`: it counts in the goal's progress. Whether it changed.
pub fn link_item(tx: &Tx, ws: &str, goal: &str, item: &str, on: bool) -> Result<bool> {
    use steve_store::base::{ObjRef, links};
    let is_goal: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM todos WHERE workspace_id = ? AND id = ? AND kind = 'goal')",
        params![ws, goal],
        |r| r.get(0),
    )?;
    if !is_goal {
        return Err(bad("not_goal"));
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM items WHERE workspace_id = ? AND id = ?)",
        params![ws, item],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(bad("item"));
    }
    let (s, o) = (ObjRef::new("item", item), ObjRef::new("todo", goal));
    let changed = if on {
        links::add(tx, ws, &links::NewLink::new(s, PART_OF, o, "you"))?
    } else {
        links::remove(
            tx,
            ws,
            &links::LinkKey {
                subject: s,
                predicate: PART_OF.into(),
                object: o,
            },
        )?
    };
    if changed {
        crate::event(tx, ws, goal);
    }
    Ok(changed)
}

/// Whether item `item` is linked to goal `goal`.
pub fn item_linked(c: &Connection, ws: &str, goal: &str, item: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM links WHERE workspace_id = ? AND subject_type = 'item' AND subject_id = ?
            AND predicate = ? AND object_type = 'todo' AND object_id = ?)",
        params![ws, item, PART_OF, goal],
        |r| r.get(0),
    )?)
}

/// What an item's status counts as in a goal's progress.
fn item_state(c: &Connection, ws: &str, item: &str) -> Result<Option<&'static str>> {
    let status: Option<String> = c
        .query_row(
            "SELECT status FROM items WHERE workspace_id = ? AND id = ?",
            params![ws, item],
            |r| r.get(0),
        )
        .optional()?;
    Ok(status.map(|s| match s.as_str() {
        "done" => "done",
        "drop" => "dropped",
        _ => "open",
    }))
}

/// A goal's progress over its child todos and the items linked to it: a child handed to a Bot counts as
/// its item, and an item once. `pct` = done ÷ (all − dropped), floored; `null` when nothing counts
/// (not split yet, or everything dropped).
pub fn progress(c: &Connection, ws: &str, goal: &str) -> Result<Value> {
    let mut items = std::collections::BTreeSet::new();
    let mut states: Vec<&'static str> = vec![];
    let children: Vec<(String, Option<String>)> = c
        .prepare_cached("SELECT state, item_id FROM todos WHERE workspace_id = ? AND parent_id = ? ORDER BY created")?
        .query_map(params![ws, goal], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (state, item) in children {
        match (state.as_str(), item) {
            ("handed", Some(i)) => {
                if items.insert(i.clone())
                    && let Some(s) = item_state(c, ws, &i)?
                {
                    states.push(s);
                }
            }
            ("done", _) => states.push("done"),
            ("dropped", _) => states.push("dropped"),
            _ => states.push("open"),
        }
    }
    let linked: Vec<String> = c
        .prepare_cached(
            "SELECT subject_id FROM links WHERE workspace_id = ? AND subject_type = 'item' AND predicate = ?
                AND object_type = 'todo' AND object_id = ? ORDER BY created",
        )?
        .query_map(params![ws, PART_OF, goal], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for i in linked {
        if items.insert(i.clone())
            && let Some(s) = item_state(c, ws, &i)?
        {
            states.push(s);
        }
    }
    let n = |s: &str| states.iter().filter(|x| **x == s).count() as i64;
    let (done, dropped, total) = (n("done"), n("dropped"), states.len() as i64);
    let counted = total - dropped;
    Ok(json!({
        "done": done, "open": n("open"), "dropped": dropped, "total": total,
        "pct": (counted > 0).then(|| done * 100 / counted),
    }))
}

/// Deletes a todo (undo of its creation).
pub fn delete(tx: &Tx, ws: &str, id: &str) -> Result<bool> {
    let parent = fields(tx, ws, id)?.and_then(|(_, f, _)| f.parent);
    let n = tx.execute(
        "DELETE FROM todos WHERE workspace_id = ? AND id = ?",
        params![ws, id],
    )?;
    crate::event(tx, ws, id);
    if let Some(p) = parent {
        crate::event(tx, ws, &p);
    }
    Ok(n > 0)
}

/// Whether a todo has child todos (a goal that cannot simply be deleted).
pub fn parent_of_any(c: &Connection, id: &str) -> Result<bool> {
    has_children(c, id)
}

fn conv_name(c: &Connection, conv: &ConvRef) -> Option<String> {
    let sql = match conv.kind.as_str() {
        "space" => "SELECT name FROM spaces WHERE id = ?",
        "mail_thread" => "SELECT subject FROM mail_threads WHERE id = ?",
        _ => return None,
    };
    c.query_row(sql, [&conv.id], |r| r.get(0)).ok()
}

fn json_of(c: &Connection, ws: &str, r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: String = r.get(0)?;
    let matter: Option<String> = r.get(4)?;
    // only the digest owner's todos show what the digest knows: matter titles, messages, people
    let full = digest_owner(c, ws, &r.get::<_, String>(23)?)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let refs: Vec<(String, String)> = c
        .prepare_cached("SELECT conv, message_id FROM todo_refs WHERE todo_id = ? ORDER BY rowid")?
        .query_map([&id], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut sources: Vec<Value> = Vec::new();
    for (k, m) in &refs {
        let cv = ConvRef::parse(k);
        let excerpt: Option<String> = match cv.as_ref().filter(|_| full).map(|x| x.kind.as_str()) {
            Some("space") => c
                .query_row("SELECT text FROM chan_messages WHERE id = ?", [m], |x| {
                    x.get(0)
                })
                .optional()?
                .flatten(),
            Some("mail_thread") => c
                .query_row("SELECT snippet FROM mail_messages WHERE id = ?", [m], |x| {
                    x.get(0)
                })
                .optional()?,
            _ => None,
        };
        sources.push(json!({
            "conv": k, "message": m,
            "name": cv.as_ref().filter(|_| full).and_then(|x| conv_name(c, x)),
            "href": cv.as_ref().map(|x| x.href()),
            "excerpt": excerpt.map(|e| crate::cut(&e, 80)),
        }));
    }
    let people: Vec<Value> = c
        .prepare_cached("SELECT tp.party_id, p.name FROM todo_parties tp LEFT JOIN parties p ON p.id = tp.party_id WHERE tp.todo_id = ?")?
        .query_map([&id], |x| Ok(json!({"id": x.get::<_, String>(0)?, "name": x.get::<_, Option<String>>(1)?.filter(|_| full)})))?
        .collect::<rusqlite::Result<_>>()?;
    let matter_title: Option<String> = match &matter {
        Some(m) if full => c
            .query_row("SELECT title FROM matters WHERE id = ?", [m], |x| x.get(0))
            .optional()?,
        _ => None,
    };
    let kind: String = r.get(12)?;
    let parent: Option<String> = r.get(13)?;
    let parent_text: Option<String> = match &parent {
        Some(p) => c
            .query_row("SELECT text FROM todos WHERE id = ?", [p], |x| x.get(0))
            .optional()?,
        None => None,
    };
    let progress = if kind == "goal" {
        progress(c, ws, &id).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
    } else {
        Value::Null
    };
    Ok(json!({
        "id": id, "text": r.get::<_, String>(1)?, "state": r.get::<_, String>(2)?, "item_id": r.get::<_, Option<String>>(3)?,
        "matter": matter.map(|m| json!({"id": m, "title": matter_title})),
        "due": r.get::<_, Option<i64>>(5)?, "due_quote": r.get::<_, Option<String>>(6)?,
        "source": r.get::<_, String>(7)?, "writer": r.get::<_, String>(8)?,
        "created": r.get::<_, i64>(9)?, "updated": r.get::<_, i64>(10)?, "closed": r.get::<_, Option<i64>>(11)?,
        "kind": kind, "parent": parent.map(|p| json!({"id": p, "text": parent_text})),
        "target_date": r.get::<_, Option<String>>(14)?, "due_kind": r.get::<_, Option<String>>(15)?,
        "due_date": r.get::<_, Option<String>>(16)?, "remind_at": r.get::<_, Option<i64>>(17)?,
        "snooze_until": r.get::<_, Option<i64>>(18)?, "block_event": r.get::<_, Option<String>>(19)?,
        "pri": r.get::<_, i64>(20)?, "star": r.get::<_, bool>(21)?, "recur": r.get::<_, Option<String>>(22)?,
        "progress": progress,
        "sources": sources, "people": people,
    }))
}

const COLS: &str = "id, text, state, item_id, matter_id, due, due_quote, source, writer, created, updated, closed,
    kind, parent_id, target_date, due_kind, due_date, remind_at, snooze_until, block_event, pri, star, recur, owner";

/// Which of a person's todos to list.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// all when empty
    pub states: Vec<String>,
    pub kind: Option<String>,
    /// only the children of this goal
    pub parent: Option<String>,
    pub limit: usize,
}

/// The owner's todos in `states` (all when empty): open ones with a deadline first (by day, then time),
/// then the newest.
pub fn list(
    c: &Connection,
    ws: &str,
    owner: &str,
    states: &[String],
    limit: usize,
) -> Result<Vec<Value>> {
    list_by(
        c,
        ws,
        owner,
        &Filter {
            states: states.to_vec(),
            limit,
            ..Default::default()
        },
    )
}

pub fn list_by(c: &Connection, ws: &str, owner: &str, f: &Filter) -> Result<Vec<Value>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM todos WHERE workspace_id = ?1 AND owner = ?2
            AND (?3 = '[]' OR state IN (SELECT value FROM json_each(?3)))
            AND (?4 IS NULL OR kind = ?4) AND (?5 IS NULL OR parent_id = ?5)
          ORDER BY state <> 'open', (due IS NULL AND due_date IS NULL),
            coalesce(due_date, date(due / 1000, 'unixepoch', 'localtime')), due IS NULL, due, created DESC
          LIMIT {}",
        f.limit.clamp(1, 500)
    ))?;
    let states = serde_json::to_string(&f.states).unwrap_or_else(|_| "[]".into());
    let rows = st.query_map(params![ws, owner, states, f.kind, f.parent], |r| {
        json_of(c, ws, r)
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, ws: &str, id: &str) -> Result<Option<Value>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM todos WHERE workspace_id = ? AND id = ?"
    ))?
    .query_row(params![ws, id], |r| json_of(c, ws, r))
    .optional()?)
}

/// A todo of `owner`'s; `None` for anyone else's.
pub fn get_own(c: &Connection, ws: &str, owner: &str, id: &str) -> Result<Option<Value>> {
    if owner_of(c, ws, id)?.as_deref() != Some(owner) {
        return Ok(None);
    }
    get(c, ws, id)
}

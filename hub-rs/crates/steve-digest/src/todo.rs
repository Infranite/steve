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

use chrono::{Days, Months, NaiveDate, NaiveDateTime};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::base::{
    ObjRef,
    links::{LinkKey, Links, NewLink},
};
use steve_store::digest::{Matters, Todo, TodoInput, Todos};
use steve_store::{ChannelSources, Contacts, ItemReads, MailSources, Members, Read, StoreError, W};

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
pub use steve_store::digest::TodoFields as Fields;

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

/// The time zone a person's local dates and times are in: their notification policy's, else the
/// workspace's ([`steve_modules::zone`], the one rule every module reads).
pub use steve_modules::zone::Zone;

/// Whether `id` (of `ws`) has child todos.
fn has_children(c: &mut impl Read, id: &str) -> Result<bool> {
    Todos(c).has_children(id)
}

/// Whether `user` is the person the digest is for (the workspace owner). Matters, source messages and
/// people come from the digest of that person's conversations, so only their todos link to them: a todo
/// of any other member links to none and shows none.
pub fn digest_owner(c: &mut impl Read, ws: &str, user: &str) -> Result<bool> {
    Ok(crate::conv::owner(c, ws)?.as_deref() == Some(user))
}

/// Validates and normalizes `f` for a todo of `owner` (`id`: the todo itself, when it exists).
pub fn check(
    c: &mut impl Read,
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
        let ok = Todos(c)
            .get(ws, p)?
            .is_some_and(|row| row.owner == owner && row.fields.kind == "goal");
        if !ok || Some(p.as_str()) == id {
            return Err(bad("parent"));
        }
    }
    if let Some(m) = &f.matter {
        // a matter already on the todo stays (the workspace may have changed hands); a new one is the digest owner's
        let kept = match id {
            Some(id) => {
                Todos(c)
                    .get(ws, id)?
                    .and_then(|row| row.fields.matter)
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

pub fn create(tx: &mut W<'_>, t: &NewTodo, now: i64) -> Result<String> {
    if !SOURCES.contains(&t.source) {
        return Err(StoreError::Hook("invalid todo source".into()));
    }
    if Members::new(tx).role(t.ws, t.owner)?.is_none() {
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
        if !Contacts::new(tx).contains(t.ws, p)? {
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
        if tx.digest_packets().get(t.ws, packet)?.is_none() {
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
    // the first todo a member writes themselves turns their morning brief on
    if t.source != "steve" {
        crate::brief::ensure(tx, t.ws, t.owner, now)?;
    }
    let mut todos = Todos(tx);
    for (conv, msg) in &t.refs {
        todos.add_source(&id, &conv.key(), msg)?;
    }
    for p in &t.parties {
        todos.add_party(&id, p)?;
    }
    Ok(id)
}

/// Inserts checked fields.
#[allow(clippy::too_many_arguments)]
fn insert(
    tx: &mut W<'_>,
    ws: &str,
    owner: &str,
    f: &Fields,
    source: &str,
    writer: &str,
    packet: Option<&str>,
    now: i64,
) -> Result<String> {
    let id = Todos(tx).insert(TodoInput {
        ws,
        owner,
        fields: f,
        source,
        writer,
        packet,
        now,
    })?;
    crate::emit(tx, ws, &id);
    if let Some(p) = &f.parent {
        crate::emit(tx, ws, p);
    }
    Ok(id)
}

/// `(owner, fields, updated)` of a todo of `ws`.
pub fn fields(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, Fields, i64)>> {
    Ok(Todos(c)
        .get(ws, id)?
        .map(|row| (row.owner, row.fields, row.updated)))
}

/// The owner of a todo of `ws`.
pub fn owner_of(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<String>> {
    Todos(c).owner(ws, id)
}

/// Writes `f` (checked here) over todo `id`. `false`: no such todo.
pub fn write(tx: &mut W<'_>, ws: &str, id: &str, f: &Fields, now: i64) -> Result<bool> {
    let Some((owner, old, _)) = fields(tx, ws, id)? else {
        return Ok(false);
    };
    let mut f = f.clone();
    check(tx, ws, &owner, Some(id), &mut f)?;
    Todos(tx).change_fields(ws, id, &f, now)?;
    crate::emit(tx, ws, id);
    for p in [old.parent, f.parent].into_iter().flatten() {
        crate::emit(tx, ws, &p);
    }
    Ok(true)
}

/// Applies `p` to todo `id`: `(before, after)`, or `None` when there is no such todo.
pub fn edit(
    tx: &mut W<'_>,
    ws: &str,
    id: &str,
    p: &Patch,
    now: i64,
) -> Result<Option<(Fields, Fields)>> {
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
    c: &mut impl Read,
    ws: &str,
    id: &str,
) -> Result<Option<(String, Option<String>, String, Option<i64>)>> {
    Ok(Todos(c)
        .get(ws, id)?
        .map(|row| (row.state, row.item_id, row.fields.text, row.fields.due)))
}

/// Sets the state (and the item when `handed`).
pub fn set_state(
    tx: &mut W<'_>,
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
        if ItemReads(tx).status(ws, item)?.is_none() {
            return Err(StoreError::Hook("unknown todo handoff item".into()));
        }
        if Todos(tx)
            .get(ws, id)?
            .is_some_and(|row| row.fields.kind == "goal")
        {
            return Err(bad("goal_hand"));
        }
    }
    let changed = Todos(tx).set_state(ws, id, state, item, now)?;
    crate::emit(tx, ws, id);
    if let Some((_, f, _)) = fields(tx, ws, id)?
        && let Some(p) = f.parent
    {
        crate::emit(tx, ws, &p);
    }
    Ok(changed)
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
    tx: &mut W<'_>,
    ws: &str,
    id: &str,
    state: &str,
    item: Option<&str>,
    now: i64,
) -> Result<Option<String>> {
    let Some(row) = Todos(tx).get(ws, id)? else {
        return Ok(None);
    };
    set_state(tx, ws, id, state, item, now)?;
    if state != "done" || row.state == "done" {
        return Ok(None);
    }
    let Some(mut next) =
        next_occurrence(Zone::for_user(tx, ws, Some(&row.owner))?, &row.fields, now)
    else {
        return Ok(None);
    };
    check(tx, ws, &row.owner, None, &mut next)?;
    let n = insert(
        tx,
        ws,
        &row.owner,
        &next,
        &row.source,
        &row.writer,
        None,
        now,
    )?;
    let mut todos = Todos(tx);
    for party in todos.parties(id)? {
        todos.add_party(&n, &party)?;
    }
    Ok(Some(n))
}

/// Links (`on`) or unlinks item `item` to goal `goal`: it counts in the goal's progress. Whether it changed.
pub fn link_item(tx: &mut W<'_>, ws: &str, goal: &str, item: &str, on: bool) -> Result<bool> {
    if !Todos(tx)
        .get(ws, goal)?
        .is_some_and(|row| row.fields.kind == "goal")
    {
        return Err(bad("not_goal"));
    }
    if ItemReads(tx).status(ws, item)?.is_none() {
        return Err(bad("item"));
    }
    let (s, o) = (ObjRef::new("item", item), ObjRef::new("todo", goal));
    let changed = if on {
        Links(tx).add(ws, &NewLink::new(s, PART_OF, o, "you"))?
    } else {
        Links(tx).remove(
            ws,
            &LinkKey {
                subject: s,
                predicate: PART_OF.into(),
                object: o,
            },
        )?
    };
    if changed {
        crate::emit(tx, ws, goal);
    }
    Ok(changed)
}

/// Whether item `item` is linked to goal `goal`.
pub fn item_linked(c: &mut impl Read, ws: &str, goal: &str, item: &str) -> Result<bool> {
    Ok(linked_items(c, ws, goal)?
        .iter()
        .any(|linked| linked == item))
}

/// The items linked to goal `goal` (`part_of`), oldest link first.
pub fn linked_items(c: &mut impl Read, ws: &str, goal: &str) -> Result<Vec<String>> {
    Ok(Links(c)
        .to(ws, &ObjRef::new("todo", goal), Some(PART_OF))?
        .into_iter()
        .filter(|row| row.subject_type == "item")
        .map(|row| row.subject_id)
        .collect())
}

/// What an item's status counts as in a goal's progress.
fn item_state(c: &mut impl Read, ws: &str, item: &str) -> Result<Option<&'static str>> {
    let status = ItemReads(c).status(ws, item)?;
    Ok(status.map(|s| match s.as_str() {
        "done" => "done",
        "drop" => "dropped",
        _ => "open",
    }))
}

/// A goal's progress over its child todos and the items linked to it: a child handed to a Bot counts as
/// its item, and an item once. `pct` = done ÷ (all − dropped), floored; `null` when nothing counts
/// (not split yet, or everything dropped).
pub fn progress(c: &mut impl Read, ws: &str, goal: &str) -> Result<Value> {
    let mut items = std::collections::BTreeSet::new();
    let mut states: Vec<&'static str> = vec![];
    let children = Todos(c).children(ws, goal)?;
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
    for i in linked_items(c, ws, goal)? {
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
pub fn delete(tx: &mut W<'_>, ws: &str, id: &str) -> Result<bool> {
    let parent = fields(tx, ws, id)?.and_then(|(_, f, _)| f.parent);
    let changed = Todos(tx).remove(ws, id)?;
    crate::emit(tx, ws, id);
    if let Some(p) = parent {
        crate::emit(tx, ws, &p);
    }
    Ok(changed)
}

/// Whether a todo has child todos (a goal that cannot simply be deleted).
pub fn parent_of_any(c: &mut impl Read, id: &str) -> Result<bool> {
    has_children(c, id)
}

fn json_of(c: &mut impl Read, ws: &str, row: Todo) -> Result<Value> {
    let id = row.id.as_str();
    let full = digest_owner(c, ws, &row.owner)?;
    let refs = Todos(c).sources(id)?;
    let mut sources = Vec::new();
    for (key, message) in refs {
        let conv = ConvRef::parse(&key);
        let (name, excerpt) = match conv.as_ref().filter(|_| full) {
            Some(cv) => {
                let name = crate::matter::name_of(c, ws, &cv.kind, &cv.id)?;
                let excerpt = match cv.kind.as_str() {
                    "space" => ChannelSources(c).excerpt(ws, &cv.id, &message)?,
                    "mail_thread" => MailSources(c).excerpt(ws, &cv.id, &message)?,
                    _ => None,
                }
                .map(|text| crate::cut(&text, 80));
                (name, excerpt)
            }
            None => (None, None),
        };
        sources.push(json!({
            "conv": key, "message": message, "name": name,
            "href": conv.map(|cv| cv.href()), "excerpt": excerpt,
        }));
    }
    let parties = Todos(c).parties(id)?;
    let names = if full {
        Contacts::new(c).party_names(parties.iter().map(String::as_str))?
    } else {
        Default::default()
    };
    let people: Vec<_> = parties
        .iter()
        .map(|party| json!({"id": party, "name": names.get(party)}))
        .collect();
    let matter_title = match row.fields.matter.as_deref().filter(|_| full) {
        Some(m) => Matters(c).get(ws, m)?.map(|row| row.title),
        None => None,
    };
    let parent_text = match &row.fields.parent {
        Some(p) => Todos(c).get(ws, p)?.map(|row| row.fields.text),
        None => None,
    };
    let mut out = json!(&row);
    out["matter"] = json!(
        row.fields
            .matter
            .map(|m| json!({"id": m, "title": matter_title}))
    );
    out["parent"] = json!(
        row.fields
            .parent
            .map(|p| json!({"id": p, "text": parent_text}))
    );
    out["progress"] = if row.fields.kind == "goal" {
        progress(c, ws, id)?
    } else {
        Value::Null
    };
    out["sources"] = json!(sources);
    out["people"] = json!(people);
    Ok(out)
}

pub use steve_store::digest::TodoFilter as Filter;

/// The owner's todos in `states` (all when empty): open ones with a deadline first (by day, then time),
/// then the newest.
pub fn list(
    c: &mut impl Read,
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

pub fn list_by(c: &mut impl Read, ws: &str, owner: &str, f: &Filter) -> Result<Vec<Value>> {
    Todos(c)
        .list(ws, owner, f)?
        .into_iter()
        .map(|row| json_of(c, ws, row))
        .collect()
}

pub fn get(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<Value>> {
    Todos(c)
        .get(ws, id)?
        .map(|row| json_of(c, ws, row))
        .transpose()
}

/// A todo of `owner`'s; `None` for anyone else's.
pub fn get_own(c: &mut impl Read, ws: &str, owner: &str, id: &str) -> Result<Option<Value>> {
    Todos(c)
        .get(ws, id)?
        .filter(|row| row.owner == owner)
        .map(|row| json_of(c, ws, row))
        .transpose()
}

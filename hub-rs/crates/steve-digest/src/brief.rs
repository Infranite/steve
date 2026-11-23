//! The morning brief: what a member's day holds, put together by rule and sent to them alone.
//!
//! **Its schedule.** A brief is a personal schedule of mode `brief` (steve-flow `schedules`), listed only
//! to its owner on the Automations page, where its time can change or it can be turned off. It is turned
//! on without asking ([`ensure`]) the first time a member writes a todo themselves (not one Steve recorded
//! from a message), workdays at [`DEFAULT_WHEN`], and never made again once they deleted it. Its time is
//! read on its owner's clock (steve-modules `zone`).
//!
//! **Its alarms** (family `brief`, [`BriefAlarms`]): the next [`PLAN_AHEAD`] times of its schedule whose
//! day is not over yet. Migration 103's triggers plan them again when the schedule, its owner's time zone or the
//! workspace's changes, and the alarm clock does after each one fires. A brief waits out its owner's quiet
//! hours; one from a past day is dropped (`stale_at` = the end of its day).
//!
//! **What it says** ([`day_agenda`], no model): overdue and due-today todos, starred todos, this week's
//! goals with their progress, the number of 待处理 cards, the member's running, stuck ([`STUCK`]) and
//! recently finished items, and their unread notifications since the last brief. The first sentence is a
//! template.
//!
//! **Where it goes** ([`BriefIntake`], topic `briefing.ready`): always a `briefing` entry in its owner's
//! feed; also their Bot DM under a notification policy that sends right away, unless the day has nothing
//! due and no 待处理 (then the feed says so in one line) or the brief comes after [`PUSH_UNTIL_MINUTE`] of a
//! brief planned before it (then it is kept in the feed only). One brief per member and local day
//! (`briefings`); each writes a run on its schedule.

use chrono::{Datelike, Days, NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_flow::FlowResult;
use steve_flow::alarms::{ALARM_FIRED_V, AlarmFired, AlarmLine, AlarmSource, BRIEFING_READY, Want};
use steve_flow::policy::{MISSED_GRACE_MS, Staleness};
use steve_flow::schedules::{self, Fields, Mode, Origin, Schedule, Spec};
use steve_modules::ApiError;
use steve_runtime::schedule::CronExpr;
use steve_store::digest::{Briefings, NewBriefing, Todos};
use steve_store::outbox::{Applied, ConsumeError, Consumer, Event, Since};
use steve_store::{
    Alarms, AttentionReads, FeedReads, ItemReads, Members, Read, Schedules, StoreError, W,
};

use crate::todo::Zone;

type Result<T> = std::result::Result<T, StoreError>;

/// The alarm family (`brief:<schedule id>`).
pub const FAMILY: &str = "brief";
/// A new brief's time: workdays 08:30. The brief's own plan (its cron, editable on the Automations page)
/// is the only source of its time afterwards.
pub const DEFAULT_CRON: &str = "30 8 * * 1-5";
/// How the new brief's time is said (what the Automations page shows as typed).
pub const DEFAULT_WHEN: &str = "工作日 08:30";
/// A brief planned before this local minute (12:00) and coming after it is kept in the feed only.
pub const PUSH_UNTIL_MINUTE: u16 = 12 * 60;
/// Lines shown per section; the rest are counted.
pub const SECTION_MAX: usize = 5;
/// Lines kept per section in the stored material.
pub const LIST_MAX: usize = 50;
/// Without an earlier brief, "since the last brief" is this long.
pub const SINCE_DEFAULT_MS: i64 = 24 * 3_600_000;
/// How long a brief's material is kept (as long as a notification's body).
pub const KEEP_MS: i64 = 30 * 86_400_000;
/// Item statuses that count as stuck: it failed or was paused, and waits for someone.
pub const STUCK: [&str; 2] = ["fail", "pause"];
/// Brief times planned ahead: a Hub down for up to this many of them still sends the day's brief when
/// it comes back (late, so the push window decides), the days before it being dropped.
pub const PLAN_AHEAD: usize = 7;
/// Schedule times stepped over looking for the next ones not over yet.
const PLAN_STEPS_MAX: usize = 400;
/// The outbox consumer of [`BRIEFING_READY`] (steve-flow `alarms::TOPICS`).
pub const CONSUMER: &str = "notifications.briefing";

fn store_err(e: ApiError) -> StoreError {
    StoreError::Hook(e.to_string())
}

/// The owner's live brief schedule.
pub fn brief_of(c: &mut impl Read, ws: &str, user: &str) -> Result<Option<Schedule>> {
    match Schedules(c).brief_id(ws, user, true)? {
        Some(id) => schedules::brief(c, ws, &id),
        None => Ok(None),
    }
}

/// Turns `user`'s brief on the first time they use their todos: a schedule at [`DEFAULT_WHEN`] on their
/// clock, unless they have (or had, and deleted) one. Returns the new schedule's id.
pub fn ensure(tx: &mut W<'_>, ws: &str, user: &str, now: i64) -> Result<Option<String>> {
    if user.starts_with("svc:") || Schedules(tx).brief_id(ws, user, false)?.is_some() {
        return Ok(None);
    }
    let zone = Zone::for_user(tx, ws, Some(user))?;
    let spec = Spec {
        requester: Some(user.into()),
        when: DEFAULT_WHEN.into(),
        cron: DEFAULT_CRON.into(),
        tz: zone.name().into(),
        what: String::new(),
        bot: String::new(),
        mode: Mode::Brief,
        project_id: None,
        tags: Vec::new(),
        missed: steve_flow::policy::MissedPolicy::Skip,
        overlap: steve_flow::policy::OverlapPolicy::Skip,
        budget_usd: 0.0,
    };
    let (name, label) = steve_i18n::stored(|| {
        (
            steve_i18n::t!("digest.brief.name"),
            steve_i18n::t!("digest.brief.origin"),
        )
    });
    let id = schedules::insert_brief(
        tx,
        ws,
        user,
        &Fields {
            name: &name,
            spec: &spec,
            enabled: true,
        },
        &Origin {
            origin: "steve".into(),
            label,
        },
        now,
    )
    .map_err(store_err)?;
    Ok(Some(id))
}

/// Unix ms of the local midnight after the day of `ms`.
fn day_end(zone: Zone, ms: i64) -> Option<i64> {
    let next = zone.local(ms)?.date().checked_add_days(Days::new(1))?;
    zone.instant(next.and_time(NaiveTime::MIN))
}

/// The brief alarms of a schedule at `cron`, for `owner` on `zone`'s clock: its next [`PLAN_AHEAD`] times
/// whose day is not over at `now` (today's, even when its time has passed, so one planned before still
/// fires; one not planned before is not added once its time is past, steve-flow `alarms::PAST_GRACE_MS`).
pub fn wants(cron: &CronExpr, zone: Zone, owner: &str, now: i64) -> Vec<Want> {
    let mut out = Vec::new();
    let Ok(cron_zone) = steve_runtime::tz::Zone::parse(zone.name()) else {
        return out;
    };
    let Some(mut t) = zone.day_start(now).map(|s| s - 1) else {
        return out;
    };
    for _ in 0..PLAN_STEPS_MAX {
        let Some(next) = cron.next_after_ms(&cron_zone, t) else {
            break;
        };
        t = next;
        let Some(stale) = day_end(zone, t) else {
            break;
        };
        if stale > now {
            out.push(Want {
                owner: owner.into(),
                kind: "brief",
                planned_for: t,
                fire_at: t,
                stale_at: Some(stale),
            });
            if out.len() == PLAN_AHEAD {
                break;
            }
        }
    }
    out
}

/// The live, enabled brief schedule `id` of a current member, with its owner.
fn live_brief(c: &mut impl Read, ws: &str, id: &str) -> FlowResult<Option<(Schedule, String)>> {
    let Some(s) = schedules::brief(c, ws, id)? else {
        return Ok(None);
    };
    let (true, Mode::Brief, Some(owner)) = (s.enabled, s.spec.mode, s.spec.requester.clone())
    else {
        return Ok(None);
    };
    let member = Members::new(c).role(ws, &owner)?.is_some();
    Ok(member.then_some((s, owner)))
}

/// The `brief` [`AlarmSource`].
pub struct BriefAlarms;

impl AlarmSource for BriefAlarms {
    fn family(&self) -> &'static str {
        FAMILY
    }

    fn plan(&self, c: &mut W<'_>, ws: &str, id: &str, now: i64) -> FlowResult<Vec<Want>> {
        let Some((s, owner)) = live_brief(c, ws, id)? else {
            return Ok(Vec::new());
        };
        let zone = Zone::for_user(c, ws, Some(&owner))?;
        Ok(wants(&s.spec.expr()?, zone, &owner, now))
    }

    fn line(
        &self,
        c: &mut W<'_>,
        ws: &str,
        id: &str,
        _kind: &str,
    ) -> FlowResult<Option<AlarmLine>> {
        Ok(live_brief(c, ws, id)?.map(|(s, _)| AlarmLine {
            subject: format!("{FAMILY}:{id}"),
            text: s.name,
            detail: None,
            link: Some("#/feed?kind=briefing".into()),
            planned_for: None,
        }))
    }
}

/// A todo in a brief.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoLine {
    pub id: String,
    pub text: String,
    /// Its deadline as shown (`18:00` today, `11-17` or `11-17 18:00` another day); none for today's date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    pub star: bool,
    pub pri: i64,
}

/// A goal with a target date this week (or past).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalLine {
    pub id: String,
    pub text: String,
    pub target: String,
    pub done: i64,
    /// What counts toward it: all but the dropped (0: not split yet).
    pub counted: i64,
}

/// One of the member's stuck items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemLine {
    pub id: String,
    pub title: String,
    pub status: String,
}

/// What a member's brief says, made by rule ([`day_agenda`]). Stored with the brief it was sent as.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agenda {
    /// The local day `YYYY-MM-DD`, on the member's clock.
    pub day: String,
    /// When it was made.
    pub at: i64,
    /// "Since the last brief": that brief's time, else [`SINCE_DEFAULT_MS`] before.
    pub since: i64,
    pub overdue: Vec<TodoLine>,
    pub today: Vec<TodoLine>,
    /// Starred open todos not already due.
    pub starred: Vec<TodoLine>,
    /// Open goals whose target date is this week or already past.
    pub goals: Vec<GoalLine>,
    /// 待处理 cards (the same count as the web's).
    pub pending: usize,
    /// Their items running now.
    pub running: usize,
    pub stuck: Vec<ItemLine>,
    /// Their items finished since [`Agenda::since`].
    pub finished: usize,
    /// Their unread personal notifications since [`Agenda::since`] (briefs not counted).
    pub unread: usize,
}

impl Agenda {
    /// Nothing due and no 待处理: not pushed, the feed says so in one line.
    pub fn is_quiet(&self) -> bool {
        self.overdue.is_empty() && self.today.is_empty() && self.pending == 0
    }

    fn ids(&self) -> std::collections::BTreeSet<String> {
        let todos = self.overdue.iter().chain(&self.today).chain(&self.starred);
        todos
            .map(|t| format!("todo:{}", t.id))
            .chain(
                self.goals
                    .iter()
                    .map(|g| format!("goal:{}:{}", g.id, g.done)),
            )
            .chain(
                self.stuck
                    .iter()
                    .map(|i| format!("item:{}:{}", i.id, i.status)),
            )
            .collect()
    }

    /// How many things differ from `other` (a todo, goal progress or stuck item in one and not the
    /// other, and each 待处理 card more or fewer): the "changed since it was sent" of the live view.
    pub fn changes_from(&self, other: &Agenda) -> usize {
        let (a, b) = (self.ids(), other.ids());
        a.symmetric_difference(&b).count() + self.pending.abs_diff(other.pending)
    }
}

fn push_line(v: &mut Vec<TodoLine>, t: TodoLine) {
    if v.len() < LIST_MAX {
        v.push(t);
    }
}

/// What `user`'s brief says at `now`, on `zone`'s clock.
pub fn day_agenda(c: &mut impl Read, ws: &str, user: &str, zone: Zone, now: i64) -> Result<Agenda> {
    let day = zone.day(now);
    let today = zone
        .local(now)
        .map(|d| d.date())
        .ok_or_else(|| StoreError::Hook("brief time out of range".into()))?;
    let end = day_end(zone, now).unwrap_or(now);
    let since = Briefings(c)
        .previous_at(ws, user, &day)?
        .unwrap_or(now - SINCE_DEFAULT_MS);
    let mut a = Agenda {
        day: day.clone(),
        at: now,
        since,
        ..Default::default()
    };

    agenda_todos(c, ws, user, zone, today, end, &mut a)?;
    agenda_goals(c, ws, user, today, &mut a)?;

    a.pending = AttentionReads(c).pending_count(ws, user, now)?;
    let mut items = ItemReads(c);
    a.running = items.running_count(ws, user)?;
    a.finished = items.finished_since(ws, user, since)?;
    a.stuck = items
        .active_by_status(ws, user, &STUCK, LIST_MAX as u64)?
        .into_iter()
        .map(|item| ItemLine {
            id: item.id,
            title: crate::cut(item.title.trim(), crate::todo_alarms::LINE_MAX),
            status: item.status,
        })
        .collect();
    a.unread = FeedReads(c).unread_since(ws, user, since, "briefing")?;
    Ok(a)
}

type AgendaTodoRow = (
    String,
    String,
    Option<i64>,
    Option<String>,
    bool,
    i64,
    Option<i64>,
);

fn agenda_todos(
    c: &mut impl Read,
    ws: &str,
    user: &str,
    zone: Zone,
    today: NaiveDate,
    end: i64,
    a: &mut Agenda,
) -> Result<()> {
    for row in Todos(c).agenda(ws, user)? {
        let f = row.fields;
        agenda_todo(
            (
                row.id,
                f.text,
                f.due,
                f.due_date,
                f.star,
                f.pri,
                f.snooze_until,
            ),
            zone,
            today,
            end,
            a,
        );
    }

    Ok(())
}

fn agenda_todo(row: AgendaTodoRow, zone: Zone, today: NaiveDate, end: i64, a: &mut Agenda) {
    let (id, text, due, due_date, star, pri, snooze) = row;
    let now = a.at;
    let day = &a.day;
    // a snoozed todo is out of the way until it wakes
    if snooze.is_some_and(|s| s > now) {
        return;
    }
    let line = |when: Option<String>| TodoLine {
        id: id.clone(),
        text: crate::cut(text.trim(), crate::todo_alarms::LINE_MAX),
        when,
        star,
        pri,
    };
    match (due_date.as_deref().and_then(crate::todo::parse_date), due) {
        (Some(d), _) if d < today => {
            push_line(&mut a.overdue, line(Some(d.format("%m-%d").to_string())))
        }
        (Some(d), _) if d == today => push_line(&mut a.today, line(None)),
        (None, Some(t)) if t < now => {
            let shown = if zone.day(t) == *day {
                "%H:%M"
            } else {
                "%m-%d %H:%M"
            };
            push_line(&mut a.overdue, line(Some(zone.format(t, shown))));
        }
        (None, Some(t)) if t < end => push_line(&mut a.today, line(Some(zone.format(t, "%H:%M")))),
        _ if star => push_line(&mut a.starred, line(None)),
        _ => {}
    }
}

fn agenda_goals(
    c: &mut impl Read,
    ws: &str,
    user: &str,
    today: NaiveDate,
    a: &mut Agenda,
) -> Result<()> {
    let week_end = today
        .checked_add_days(Days::new(
            7 - u64::from(today.weekday().number_from_monday()),
        ))
        .unwrap_or(today);
    for (id, text, target) in Todos(c).goals(ws, user)? {
        if crate::todo::parse_date(&target).is_none_or(|d: NaiveDate| d > week_end)
            || a.goals.len() >= LIST_MAX
        {
            continue;
        }
        let p = crate::todo::progress(c, ws, &id)?;
        let n = |k: &str| p[k].as_i64().unwrap_or(0);
        a.goals.push(GoalLine {
            id,
            text: crate::cut(text.trim(), crate::todo_alarms::LINE_MAX),
            target,
            done: n("done"),
            counted: n("total") - n("dropped"),
        });
    }

    Ok(())
}

fn section(out: &mut String, head: &str, lines: &[TodoLine]) {
    if lines.is_empty() {
        return;
    }
    out.push_str(&format!("\n\n{head}"));
    for t in lines.iter().take(SECTION_MAX) {
        let star = if t.star { "★ " } else { "" };
        out.push_str(&format!("\n- {star}{}", t.text));
        if let Some(w) = &t.when {
            out.push_str(&format!(" · {w}"));
        }
    }
    if lines.len() > SECTION_MAX {
        out.push_str(&format!(
            "\n{}",
            steve_i18n::t!("digest.brief.more", n = lines.len() - SECTION_MAX)
        ));
    }
}

/// The short name of weekday `n` (1 = Monday).
fn weekday(n: u32) -> String {
    match n {
        1 => steve_i18n::t!("digest.brief.wd1"),
        2 => steve_i18n::t!("digest.brief.wd2"),
        3 => steve_i18n::t!("digest.brief.wd3"),
        4 => steve_i18n::t!("digest.brief.wd4"),
        5 => steve_i18n::t!("digest.brief.wd5"),
        6 => steve_i18n::t!("digest.brief.wd6"),
        _ => steve_i18n::t!("digest.brief.wd7"),
    }
}

/// The first sentence (a template, by what the day holds).
pub fn opener(a: &Agenda) -> String {
    if a.is_quiet() {
        return steve_i18n::t!("digest.brief.quiet");
    }
    let mut parts = Vec::new();
    if !a.today.is_empty() {
        parts.push(steve_i18n::t!("digest.brief.open_today", n = a.today.len()));
    }
    if !a.overdue.is_empty() {
        parts.push(steve_i18n::t!(
            "digest.brief.open_overdue",
            n = a.overdue.len()
        ));
    }
    if a.pending > 0 {
        parts.push(steve_i18n::t!("digest.brief.open_pending", n = a.pending));
    }
    if !a.stuck.is_empty() {
        parts.push(steve_i18n::t!("digest.brief.open_stuck", n = a.stuck.len()));
    }
    format!(
        "{}{}",
        parts.join(&steve_i18n::t!("digest.brief.sep")),
        steve_i18n::t!("digest.brief.end")
    )
}

/// The brief as stored and sent, in the current language: a title line, the first sentence, then each
/// section. Plain text: the same body is the feed entry and a Feishu `text` message in the DM, so it has
/// no link.
pub fn render(a: &Agenda) -> String {
    let date = NaiveDate::parse_from_str(&a.day, "%Y-%m-%d").ok();
    let weekday = date
        .map(|d| weekday(d.weekday().number_from_monday()))
        .unwrap_or_default();
    let shown = date
        .map(|d| d.format("%m-%d").to_string())
        .unwrap_or_default();
    let mut out = steve_i18n::t!("digest.brief.title", day = shown, weekday = weekday);
    out.push('\n');
    out.push_str(&opener(a));
    if a.is_quiet() && a.starred.is_empty() && a.goals.is_empty() && a.stuck.is_empty() {
        return out;
    }
    section(
        &mut out,
        &steve_i18n::t!("digest.brief.overdue"),
        &a.overdue,
    );
    section(&mut out, &steve_i18n::t!("digest.brief.today"), &a.today);
    section(
        &mut out,
        &steve_i18n::t!("digest.brief.starred"),
        &a.starred,
    );
    render_goals(&mut out, a);
    render_items(&mut out, a);
    let mut tail = Vec::new();
    if a.pending > 0 {
        tail.push(steve_i18n::t!("digest.brief.pending", n = a.pending));
    }
    if a.unread > 0 {
        tail.push(steve_i18n::t!("digest.brief.unread", n = a.unread));
    }
    if !tail.is_empty() {
        out.push_str(&format!(
            "\n\n{}",
            tail.join(&steve_i18n::t!("digest.brief.sep"))
        ));
    }
    out
}

fn render_goals(out: &mut String, a: &Agenda) {
    if !a.goals.is_empty() {
        out.push_str(&format!("\n\n{}", steve_i18n::t!("digest.brief.goals")));
        for g in a.goals.iter().take(SECTION_MAX) {
            let target = NaiveDate::parse_from_str(&g.target, "%Y-%m-%d")
                .map(|d| d.format("%m-%d").to_string())
                .unwrap_or_else(|_| g.target.clone());
            let progress = if g.counted > 0 {
                format!("{}/{}", g.done, g.counted)
            } else {
                steve_i18n::t!("digest.brief.not_split")
            };
            out.push_str(&format!("\n- {} · {target} · {progress}", g.text));
        }
        if a.goals.len() > SECTION_MAX {
            out.push_str(&format!(
                "\n{}",
                steve_i18n::t!("digest.brief.more", n = a.goals.len() - SECTION_MAX)
            ));
        }
    }
}

fn render_items(out: &mut String, a: &Agenda) {
    if a.running + a.finished + a.stuck.len() > 0 {
        out.push_str(&format!(
            "\n\n{}\n{}",
            steve_i18n::t!("digest.brief.items"),
            steve_i18n::t!(
                "digest.brief.items_count",
                running = a.running,
                finished = a.finished
            )
        ));
        for i in a.stuck.iter().take(SECTION_MAX) {
            let status = match i.status.as_str() {
                "fail" => steve_i18n::t!("digest.brief.status_fail"),
                "pause" => steve_i18n::t!("digest.brief.status_pause"),
                other => other.to_string(),
            };
            out.push_str(&format!("\n- {} {} · {status}", i.id, i.title));
        }
        if a.stuck.len() > SECTION_MAX {
            out.push_str(&format!(
                "\n{}",
                steve_i18n::t!("digest.brief.more", n = a.stuck.len() - SECTION_MAX)
            ));
        }
    }
}

/// When a brief planned for `planned` stops being pushed: [`PUSH_UNTIL_MINUTE`] of its day, or the end
/// of its day for one planned at or after it.
pub fn push_until(zone: Zone, planned: i64) -> Option<i64> {
    let noon = Staleness::StaleAfter(PUSH_UNTIL_MINUTE).stale_at(
        planned,
        None,
        steve_runtime::tz::Zone::parse(zone.name()).ok()?,
    )?;
    if planned < noon {
        Some(noon)
    } else {
        day_end(zone, planned)
    }
}

/// Why a brief went where it went (its schedule run's result, stored).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pushed,
    Quiet,
    Late,
    FeedOnly,
}

impl Outcome {
    fn label(self, a: &Agenda) -> String {
        match self {
            Outcome::Pushed => {
                steve_i18n::t!("digest.brief.run_pushed", summary = opener(a))
            }
            Outcome::Quiet => steve_i18n::t!("digest.brief.run_quiet"),
            Outcome::Late => steve_i18n::t!("digest.brief.run_late"),
            Outcome::FeedOnly => steve_i18n::t!("digest.brief.run_feed"),
        }
    }
}

/// The notification intake of [`BRIEFING_READY`]: makes the brief of each fired brief alarm (see the
/// module docs).
pub struct BriefIntake;

impl BriefIntake {
    fn brief(
        &self,
        tx: &mut W<'_>,
        ev: &Event,
        a: &AlarmFired,
        line: &AlarmLine,
    ) -> Result<String> {
        let at = a.at.unwrap_or(ev.created);
        let planned = line.planned_for.unwrap_or(at);
        let schedule = line
            .subject
            .strip_prefix(&format!("{FAMILY}:"))
            .unwrap_or_default();
        let zone = Zone::for_user(tx, &ev.workspace_id, Some(&a.owner))?;
        let day = zone.day(planned);
        if Briefings(tx)
            .get(&ev.workspace_id, &a.owner, &day)?
            .is_some()
        {
            return Ok("already briefed that day".into());
        }
        let agenda = day_agenda(tx, &ev.workspace_id, &a.owner, zone, at)?;
        let (mode, state) =
            steve_channels::notifications::brief_delivery(tx, &ev.workspace_id, &a.owner)
                .map_err(|e| StoreError::Hook(e.to_string()))?;
        let late = push_until(zone, planned).is_none_or(|u| at >= u);
        let outcome = match () {
            _ if agenda.is_quiet() => Outcome::Quiet,
            _ if state != "waiting" => Outcome::FeedOnly,
            _ if late => Outcome::Late,
            _ => Outcome::Pushed,
        };
        let pushed = outcome == Outcome::Pushed;
        let (mode, state) = match (pushed, state) {
            (true, _) => (mode, state),
            (false, "waiting") => (steve_channels::notifications::Mode::Log, "held"),
            (false, s) => (mode, s),
        };
        let (body, label) = steve_i18n::stored(|| (render(&agenda), outcome.label(&agenda)));
        steve_channels::notifications::record(
            tx,
            &ev.workspace_id,
            &a.owner,
            ev,
            mode,
            &body,
            state,
            steve_channels::notifications::Topic::BriefingReady,
        )
        .map_err(|e| StoreError::Hook(e.to_string()))?;
        Briefings(tx).insert(NewBriefing {
            ws: &ev.workspace_id,
            owner: &a.owner,
            day: &day,
            schedule,
            event: &ev.id,
            material: &serde_json::to_value(&agenda)?,
            pushed,
            created: at,
        })?;
        Briefings(tx).prune(&ev.workspace_id, &a.owner, at - KEEP_MS)?;
        let version = Schedules(tx).version(&ev.workspace_id, schedule)?;
        if let Some(version) = version {
            let missed = at - planned > MISSED_GRACE_MS;
            steve_store::RunRows(tx).insert(
                &steve_flow::runs::NewRun {
                    ws: &ev.workspace_id,
                    flow_id: schedule,
                    flow_version: version,
                    trigger_kind: if missed { "catch_up" } else { "time" },
                    status: "done",
                    error: None,
                    outcome_label: &label,
                    cursor: None,
                    context: &json!({ "at": planned, "trigger": "time", "day": day, "pushed": pushed, "event": ev.id }),
                    now: at,
                },
            )
            ?;
        }
        Ok(if pushed {
            "morning brief recorded and queued".into()
        } else {
            "morning brief kept in the feed".into()
        })
    }
}

impl Consumer for BriefIntake {
    fn id(&self) -> &'static str {
        CONSUMER
    }
    fn topic(&self) -> &'static str {
        BRIEFING_READY
    }
    fn since(&self) -> Since {
        Since::Start
    }
    fn apply(&self, w: &mut W<'_>, ev: &Event) -> std::result::Result<Applied, ConsumeError> {
        if ev.version != ALARM_FIRED_V {
            return Err(ConsumeError::Reject(format!(
                "unknown {BRIEFING_READY} version"
            )));
        }
        let a: AlarmFired = serde_json::from_value(ev.payload.clone())
            .map_err(|e| ConsumeError::Reject(e.to_string()))?;
        if a.lines.is_empty() || a.owner.starts_with("svc:") {
            return Ok(Applied::noted("nothing to brief"));
        }
        if Members::new(w).role(&ev.workspace_id, &a.owner)?.is_none() {
            return Ok(Applied::noted("the brief's owner is no longer a member"));
        }
        let outcome = a
            .lines
            .iter()
            .map(|line| self.brief(w, ev, &a, line))
            .collect::<Result<Vec<_>>>()?;
        Ok(Applied::noted(outcome.join("; ")))
    }
}

/// `GET /digest/briefing/today`: the caller's brief made again now, the one sent today (if any) and how
/// much has changed since, and their brief schedule.
pub fn today(c: &mut impl Read, ws: &str, user: &str, now: i64) -> Result<Value> {
    let zone = Zone::for_user(c, ws, Some(user))?;
    let live = day_agenda(c, ws, user, zone, now)?;
    let sent = match Briefings(c).get(ws, user, &live.day)? {
        Some(b) => {
            let old: Agenda = serde_json::from_value(b.material)?;
            json!({ "at": b.created, "pushed": b.pushed, "changes": live.changes_from(&old) })
        }
        None => Value::Null,
    };
    let schedule = match brief_of(c, ws, user)? {
        Some(s) => {
            let next = Alarms(c).next(ws, &format!("{FAMILY}:{}", s.id))?;
            json!({ "id": s.id, "name": s.name, "when": s.spec.when, "enabled": s.enabled, "next_at": next.filter(|_| s.enabled) })
        }
        None => Value::Null,
    };
    Ok(json!({
        "day": live.day,
        "tz": zone.name(),
        "quiet": live.is_quiet(),
        "text": render(&live),
        "material": live,
        "sent": sent,
        "schedule": schedule,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn shanghai() -> Zone {
        Zone::new(chrono_tz::Asia::Shanghai)
    }

    #[test]
    fn a_week_of_workday_briefs_ahead_whose_day_is_not_over() {
        let cron = CronExpr::parse(DEFAULT_CRON).unwrap();
        // Wednesday 2026-11-18 07:00 Shanghai = 23:00 UTC the day before
        let now = ms(2026, 11, 17, 23, 0);
        let w = wants(&cron, shanghai(), "me", now);
        let at: Vec<i64> = w.iter().take(2).map(|w| w.planned_for).collect();
        assert_eq!(at, vec![ms(2026, 11, 18, 0, 30), ms(2026, 11, 19, 0, 30)]);
        // a week of workdays ahead: Wednesday to the next Thursday
        assert_eq!(w.len(), PLAN_AHEAD);
        assert_eq!(w[PLAN_AHEAD - 1].planned_for, ms(2026, 11, 26, 0, 30));
        // stale at the end of its day (Shanghai midnight = 16:00 UTC)
        assert_eq!(w[0].stale_at, Some(ms(2026, 11, 18, 16, 0)));
        // 10:00 on the same day: today's is still wanted (one planned before still fires)
        let w = wants(&cron, shanghai(), "me", ms(2026, 11, 18, 2, 0));
        assert_eq!(w[0].planned_for, ms(2026, 11, 18, 0, 30));
        // Saturday: Monday and Tuesday
        let w = wants(&cron, shanghai(), "me", ms(2026, 11, 21, 4, 0));
        assert_eq!(
            w.iter().take(2).map(|w| w.planned_for).collect::<Vec<_>>(),
            vec![ms(2026, 11, 23, 0, 30), ms(2026, 11, 24, 0, 30)]
        );
    }

    #[test]
    fn a_brief_after_noon_is_not_pushed_unless_it_was_planned_after_noon() {
        let z = shanghai();
        let morning = ms(2026, 11, 18, 0, 30);
        assert_eq!(push_until(z, morning), Some(ms(2026, 11, 18, 4, 0)));
        let afternoon = ms(2026, 11, 18, 6, 0); // 14:00
        assert_eq!(push_until(z, afternoon), Some(ms(2026, 11, 18, 16, 0)));
    }

    #[test]
    fn a_quiet_day_is_one_line_and_changes_are_counted() {
        let a = Agenda {
            day: "2026-11-18".into(),
            ..Default::default()
        };
        assert!(a.is_quiet());
        let body = steve_i18n::with("zh-CN", || render(&a));
        assert_eq!(body, "晨报 · 11-18 周三\n今天没有安排。");
        let mut b = a.clone();
        b.pending = 2;
        b.today.push(TodoLine {
            id: "t1".into(),
            text: "交周报".into(),
            when: Some("18:00".into()),
            star: true,
            pri: 0,
        });
        assert!(!b.is_quiet());
        assert_eq!(b.changes_from(&a), 3);
        let body = steve_i18n::with("zh-CN", || render(&b));
        assert!(body.contains("今天到期 1 个待办"), "{body}");
        assert!(body.contains("- ★ 交周报 · 18:00"), "{body}");
        assert!(!body.contains("#/") && !body.contains("]("), "{body}");
        let en = steve_i18n::with("en", || render(&b));
        assert!(en.starts_with("Morning brief · 11-18 Wed"), "{en}");
    }
}

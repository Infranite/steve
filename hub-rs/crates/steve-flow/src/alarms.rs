//! `time_alarms`: one table for every alarm a person has (a todo's reminder, a snooze wake-up, the
//! morning brief; later meeting preparation and reminders). An alarm fires at most once.
//!
//! **Planning.** A module that owns a kind of subject registers an [`AlarmSource`] for its family
//! (`todo` for `todo:<id>`). When a subject changes, its row in `time_alarm_dirty` is written (by
//! triggers on the subject's table, so every write path counts: REST, agent tools, undo, recurrence),
//! and [`replan_dirty`] asks the source which alarms the subject should have now ([`Want`]) and
//! [`reconcile`]s them with the stored rows:
//!
//! - same planned time, other fire time or stale time (a lead time changed): updated in place;
//! - a planned time no longer wanted (moved, completed, deleted): the row is `cancelled`;
//! - a planned time wanted again (moved back): its `cancelled` row is `planned` again;
//! - a row that `fired` or went `stale` never fires again;
//! - a wanted alarm whose time is already more than [`PAST_GRACE_MS`] past is not added (or brought
//!   back): changing a todo does not ring a reminder for a time long gone. One that was planned before
//!   and came due while the Hub was down still fires.
//!
//! A subject whose alarm fired or went stale is marked again, so a source that repeats (a `brief`
//! schedule, family `brief`) plans its next one; a one-off subject (a todo) plans the same rows again,
//! which changes nothing.
//!
//! **Firing.** [`claim_due`] takes the due rows in one transaction. Per row, by
//! [`crate::policy::plan_alarm`]: past its `stale_at` it is `stale`; within its owner's quiet hours (when
//! they would hear it) and its topic waits ([`QuietRule::Postpone`]), it is held until they end
//! (`hold_until`); otherwise it fires. The alarms of one owner and topic that fire together (several due
//! at once, those held through the quiet hours, those missed while the Hub was down) become **one**
//! outbox event ([`AlarmFired`]), and the rows are marked `fired` with that event's id (`run_ref`) in the
//! same transaction. The personal notification intake (steve-channels) turns the event into one
//! notification, its delivery keyed on the event: the outbox delivers it exactly once.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use steve_modules::zone::{Quiet, Zone};
use steve_store::{Alarm, Alarms, NewAlarm, NotificationPolicies, Read, Store, W};

use crate::policy::{AlarmAction, MISSED_GRACE_MS, QuietRule, plan_alarm};
use crate::{FlowCx, FlowResult};

/// `time_alarms.kind`: the morning brief, meeting preparation, a meeting reminder, a todo reminder,
/// a snooze wake-up.
pub const KINDS: [&str; 5] = ["brief", "prep", "remind", "todo", "snooze"];

/// A wanted alarm whose fire time is further past than this is not added or brought back.
pub const PAST_GRACE_MS: i64 = 5 * 60_000;
/// Dirty subjects planned, and due alarms claimed, per transaction.
pub const BATCH: usize = 200;

/// The topic of a todo reminder.
pub const TODO_DUE: &str = "todo.due";
/// The topic of a snooze wake-up.
pub const SNOOZE_WAKE: &str = "snooze.wake";
/// The topic of a morning brief (its alarm is planned from a `brief` schedule; steve-digest `brief`).
pub const BRIEFING_READY: &str = "briefing.ready";
/// Payload version of [`TODO_DUE`], [`SNOOZE_WAKE`] and [`BRIEFING_READY`] ([`AlarmFired`]).
pub const ALARM_FIRED_V: i64 = 1;

/// One personal notification topic: what the feed calls it, how quiet hours treat it, and whether
/// several that come due together are sent as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Topic {
    /// The outbox topic.
    pub name: &'static str,
    /// `feed_entries.kind` (the insert trigger of migrations 102 and 103 maps the topics the same way).
    pub feed_kind: &'static str,
    pub quiet: QuietRule,
    /// Alarms of this topic that fire in the same round, for the same person, are one notification.
    pub merges: bool,
    /// `time_alarms.kind` that fires as this topic (`None`: not an alarm).
    pub alarm: Option<&'static str>,
    /// The id of the notification intake's outbox consumer of this topic (stable: its cursor is kept by it).
    pub consumer: &'static str,
    /// The notification is written from the fired alarms' lines (steve-channels `AlarmIntake`); `false`:
    /// the module that owns the topic has its own intake (the morning brief's composes the day).
    pub from_lines: bool,
}

/// The personal notification topics. Each has its own consumer in the notification intake.
pub const TOPICS: [Topic; 4] = [
    Topic {
        name: steve_work_api::ITEM_DONE,
        feed_kind: "completion",
        quiet: QuietRule::Postpone,
        merges: false,
        alarm: None,
        consumer: "notifications.work_done",
        from_lines: false,
    },
    Topic {
        name: TODO_DUE,
        feed_kind: "reminder",
        quiet: QuietRule::Postpone,
        merges: true,
        alarm: Some("todo"),
        consumer: "notifications.todo_due",
        from_lines: true,
    },
    Topic {
        name: SNOOZE_WAKE,
        feed_kind: "reminder",
        quiet: QuietRule::Postpone,
        merges: true,
        alarm: Some("snooze"),
        consumer: "notifications.snooze_wake",
        from_lines: true,
    },
    Topic {
        name: BRIEFING_READY,
        feed_kind: "briefing",
        quiet: QuietRule::Postpone,
        // one per schedule and day: never several in one message
        merges: false,
        alarm: Some("brief"),
        consumer: "notifications.briefing",
        from_lines: false,
    },
];

/// The topic of `id`.
pub fn topic(id: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.name == id)
}

/// The topic an alarm of `kind` fires as.
pub fn topic_of_kind(kind: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.alarm == Some(kind))
}

/// The payload of an alarm topic's event: who it is for, the alarms it stands for and one line each
/// (as the subject's module wrote it when it fired; nothing is read again).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlarmFired {
    pub owner: String,
    pub kind: String,
    pub alarms: Vec<String>,
    pub lines: Vec<AlarmLine>,
    /// At least one was claimed later than [`MISSED_GRACE_MS`] (held through quiet hours, the Hub down).
    pub late: bool,
    /// When they fired (the round's clock; an intake that reads the subject reads it as of then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

/// What one fired alarm says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmLine {
    pub subject: String,
    pub text: String,
    /// A second part (`截止 11-20 18:00`), already in the owner's time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Where it opens in the web (`#/todos/<id>`). Payload only: the stored notification body is plain
    /// text for the feed and the Bot DM, so it does not carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// The alarm's planned time (the moment it is about), filled by [`claim_due`] when it fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_for: Option<i64>,
}

/// An alarm a subject should have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub owner: String,
    /// [`KINDS`]
    pub kind: &'static str,
    /// The moment it is about (a reminder time, a meeting's start): with the kind, its identity.
    pub planned_for: i64,
    pub fire_at: i64,
    pub stale_at: Option<i64>,
}

/// A module's alarms: plans a subject's alarms and says the line of one that fires.
pub trait AlarmSource: Send + Sync {
    /// The subject family (`todo` for `todo:<id>`).
    fn family(&self) -> &'static str;
    /// The alarms subject `id` of `ws` should have now (none when it is gone).
    fn plan(
        &self,
        c: &mut steve_store::W<'_>,
        ws: &str,
        id: &str,
        now: i64,
    ) -> FlowResult<Vec<Want>>;
    /// The line of an alarm of `kind` on `id` that fires now; `None`: it should not (the subject is
    /// gone or no longer wants it), and the alarm is cancelled.
    fn line(
        &self,
        c: &mut steve_store::W<'_>,
        ws: &str,
        id: &str,
        kind: &str,
    ) -> FlowResult<Option<AlarmLine>>;
}

/// The registered [`AlarmSource`]s.
#[derive(Clone, Default)]
pub struct AlarmSources(Vec<Arc<dyn AlarmSource>>);

impl AlarmSources {
    pub fn add(&mut self, s: Arc<dyn AlarmSource>) {
        self.0.retain(|o| o.family() != s.family());
        self.0.push(s);
    }

    pub fn get(&self, family: &str) -> Option<&Arc<dyn AlarmSource>> {
        self.0.iter().find(|s| s.family() == family)
    }

    pub fn families(&self) -> Vec<&'static str> {
        self.0.iter().map(|s| s.family()).collect()
    }
}

/// What a [`reconcile`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub added: usize,
    pub moved: usize,
    pub restored: usize,
    pub cancelled: usize,
}

/// Makes the alarms of `subject` (in `ws`) the `wants`, by the rules in the module docs.
pub fn reconcile(
    w: &mut W<'_>,
    ws: &str,
    subject: &str,
    wants: &[Want],
    now: i64,
) -> FlowResult<Reconciled> {
    let mut repo = Alarms(w);
    let rows = repo.of_subject(ws, subject)?;
    let mut out = Reconciled::default();
    let fresh = |w: &Want| w.fire_at >= now - PAST_GRACE_MS;
    for w in wants {
        let found = rows
            .iter()
            .find(|r| r.owner == w.owner && r.kind == w.kind && r.planned_for == w.planned_for);
        match found {
            Some(r) if r.state == "planned" => {
                if r.fire_at != w.fire_at || r.stale_at != w.stale_at {
                    let hold = if r.fire_at == w.fire_at {
                        r.hold_until
                    } else {
                        None
                    };
                    repo.plan(&r.id, w.fire_at, w.stale_at, hold, now)?;
                    out.moved += 1;
                }
            }
            Some(r) if r.state == "cancelled" => {
                if fresh(w) {
                    repo.plan(&r.id, w.fire_at, w.stale_at, None, now)?;
                    out.restored += 1;
                }
            }
            // fired or stale: never again
            Some(_) => {}
            None => {
                if fresh(w) {
                    repo.add(&NewAlarm {
                        ws,
                        owner: &w.owner,
                        kind: w.kind,
                        subject,
                        planned_for: w.planned_for,
                        fire_at: w.fire_at,
                        stale_at: w.stale_at,
                        now,
                    })?;
                    out.added += 1;
                }
            }
        }
    }
    for r in rows.iter().filter(|r| r.state == "planned") {
        let wanted = wants
            .iter()
            .any(|w| r.owner == w.owner && r.kind == w.kind && r.planned_for == w.planned_for);
        if !wanted {
            repo.cancel(&r.id, now)?;
            out.cancelled += 1;
        }
    }
    Ok(out)
}

/// Plans `subject` (`<family>:<id>`) again with its source. A subject no source knows keeps its alarms.
pub fn replan(
    w: &mut W<'_>,
    sources: &AlarmSources,
    ws: &str,
    subject: &str,
    now: i64,
) -> FlowResult<Option<Reconciled>> {
    let Some((family, id)) = subject.split_once(':') else {
        return Ok(None);
    };
    let Some(source) = sources.get(family) else {
        return Ok(None);
    };
    let wants = w.scope(|w| source.plan(w, ws, id, now))?;
    reconcile(w, ws, subject, &wants, now).map(Some)
}

/// Plans dirty subjects independently. A broken request cancels only its subject's alarms and
/// records a durable diagnostic; a later subject edit requests planning again.
pub fn replan_dirty(store: &Store, sources: &AlarmSources, now: i64) -> FlowResult<usize> {
    let rows = store.read(|r| Alarms(r).dirty(BATCH as u64))?;
    let mut done = 0;
    for (ws, subject) in rows {
        done += store.write(|w| -> FlowResult<usize> {
            if !Alarms(w).needs_plan(&ws, &subject)? {
                return Ok(0);
            }
            let plan = w.scope(|w| replan(w, sources, &ws, &subject, now));
            if let Err(error) = &plan {
                Alarms(w).cancel_subject(&ws, &subject, now)?;
                crate::claim::record_failure(w, "time_alarm_dirty", &ws, &subject, error, now);
            }
            Alarms(w).clear_dirty(&ws, &subject)?;
            Ok(usize::from(plan.is_ok()))
        })?;
    }
    Ok(done)
}

/// What a [`claim_due`] round did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claimed {
    /// The outbox events emitted (one per owner and topic).
    pub events: Vec<String>,
    pub fired: usize,
    pub held: usize,
    pub stale: usize,
    pub cancelled: usize,
}

/// Whether the owner would hear a notification now, and if they are in quiet hours, when those end:
/// only a policy that sends right away (`enabled`, `mode = now`) is held by its quiet hours. Read on the
/// owner's clock ([`Zone::for_user`]).
pub fn quiet_ends(c: &mut impl Read, ws: &str, user: &str, now: i64) -> FlowResult<Option<i64>> {
    let Some(p) = NotificationPolicies(c).quiet(ws, user)? else {
        return Ok(None);
    };
    if !p.enabled || p.mode != "now" {
        return Ok(None);
    }
    let (start, end) = (
        p.start.and_then(|v| u16::try_from(v).ok()),
        p.end.and_then(|v| u16::try_from(v).ok()),
    );
    let Some(q) = Quiet::from_parts(start, end) else {
        return Ok(None);
    };
    Ok(q.ends(Zone::for_user(c, ws, Some(user))?.tz, now))
}

/// The alarms that fire as one event: one owner's of one topic in a round.
struct Group {
    ws: String,
    owner: String,
    topic: &'static Topic,
    ids: Vec<String>,
    lines: Vec<AlarmLine>,
    late: bool,
}

/// Lower case, runs of white space as one: two alarms saying the same thing at once are one line.
fn same_line(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

enum RowOutcome {
    Cancelled,
    Stale,
    Held,
    Fire {
        due: Alarm,
        topic: &'static Topic,
        line: AlarmLine,
        late: bool,
    },
}

fn claim_one(
    w: &mut W<'_>,
    sources: &AlarmSources,
    id: &str,
    now: i64,
) -> FlowResult<Option<RowOutcome>> {
    let Some(d) = Alarms(w).planned(id)? else {
        return Ok(None);
    };
    let cancel = |w: &mut W<'_>| -> FlowResult<Option<RowOutcome>> {
        Alarms(w).cancel(id, now)?;
        Ok(Some(RowOutcome::Cancelled))
    };
    let Some(topic) = topic_of_kind(&d.kind) else {
        return cancel(w);
    };
    let ends = quiet_ends(w, &d.ws, &d.owner, now)?;
    let out = match plan_alarm(d.fire_at, d.stale_at, now, ends, topic.quiet) {
        AlarmAction::Stale => {
            Alarms(w).stale(id, now)?;
            Alarms(w).mark(&d.ws, &d.subject)?;
            RowOutcome::Stale
        }
        AlarmAction::Hold { until } => {
            if d.hold_until != Some(until) {
                Alarms(w).hold(id, until, now)?;
            }
            RowOutcome::Held
        }
        AlarmAction::Fire { late_ms } => {
            let line = match d.subject.split_once(':') {
                Some((family, id)) => match sources.get(family) {
                    Some(source) => w.scope(|w| source.line(w, &d.ws, id, &d.kind))?,
                    None => None,
                },
                None => None,
            };
            let Some(mut line) = line else {
                return cancel(w);
            };
            line.planned_for = Some(d.planned_for);
            Alarms(w).mark(&d.ws, &d.subject)?;
            RowOutcome::Fire {
                due: d,
                topic,
                line,
                late: late_ms > MISSED_GRACE_MS,
            }
        }
    };
    Ok(Some(out))
}

/// Every row attempt has a Store savepoint. Counters and message groups are merged only after
/// that attempt succeeds; the outer transaction keeps successful claims and their events atomic.
pub fn claim_due(store: &Store, sources: &AlarmSources, now: i64) -> FlowResult<Claimed> {
    store.write(|w| {
        let rows = Alarms(w).due(now, BATCH as u64)?;
        let mut out = Claimed::default();
        let mut groups: Vec<Group> = Vec::new();
        for (id, ws) in rows {
            let outcome = match w.scope(|w| claim_one(w, sources, &id, now)) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let changed = Alarms(w).cancel(&id, now)?;
                    crate::claim::record_failure(w, "time_alarms", &ws, &id, &error, now);
                    changed.then_some(RowOutcome::Cancelled)
                }
            };
            match outcome {
                None => {}
                Some(RowOutcome::Cancelled) => out.cancelled += 1,
                Some(RowOutcome::Stale) => out.stale += 1,
                Some(RowOutcome::Held) => out.held += 1,
                Some(RowOutcome::Fire {
                    due: d,
                    topic,
                    line,
                    late,
                }) => {
                    if let Some(g) = groups.iter_mut().find(|g| {
                        topic.merges
                            && g.ws == d.ws
                            && g.owner == d.owner
                            && g.topic.name == topic.name
                    }) {
                        g.ids.push(d.id);
                        if !g
                            .lines
                            .iter()
                            .any(|l| same_line(&l.text) == same_line(&line.text))
                        {
                            g.lines.push(line);
                        }
                        g.late |= late;
                    } else {
                        groups.push(Group {
                            ws: d.ws,
                            owner: d.owner,
                            topic,
                            ids: vec![d.id],
                            lines: vec![line],
                            late,
                        });
                    }
                }
            }
        }
        for Group {
            ws,
            owner,
            topic,
            ids,
            lines,
            late,
        } in groups
        {
            let payload = AlarmFired {
                owner,
                kind: topic.alarm.unwrap_or_default().into(),
                alarms: ids.clone(),
                lines,
                late,
                at: Some(now),
            };
            let ev = steve_store::outbox::enqueue(
                w,
                &ws,
                topic.name,
                ALARM_FIRED_V,
                &serde_json::to_value(&payload)?,
            )?;
            for id in &ids {
                if !Alarms(w).fire(id, &ev.id, now)? {
                    return Err(steve_modules::ApiError::conflict(
                        "alarm changed during claim",
                    ));
                }
            }
            out.fired += ids.len();
            out.events.push(ev.id);
        }
        Ok(out)
    })
}

// Policy timestamps let the fallback notice a writer that missed its wake.
fn changed_policies(
    c: &mut impl Read,
    now: i64,
) -> steve_store::Result<Vec<(String, String, i64)>> {
    let held = Alarms(c).held_owners()?;
    let Some(first) = held.iter().map(|(_, _, at)| *at).min() else {
        return Ok(Vec::new());
    };
    let updates: HashMap<_, _> = NotificationPolicies(c)
        .updated_between(first, now)?
        .into_iter()
        .map(|(ws, user, at)| ((ws, user), at))
        .collect();
    Ok(held
        .into_iter()
        .filter_map(|(ws, user, held_at)| {
            let changed = updates.get(&(ws.clone(), user.clone())).copied()?;
            (changed > held_at).then_some((ws, user, changed))
        })
        .collect())
}

fn recheck_holds(store: &Store, all: bool, now: i64) -> FlowResult<()> {
    store.write(|w| {
        if all {
            Alarms(w).clear_holds(None)?;
        } else {
            for (ws, user, changed) in changed_policies(w, now)? {
                Alarms(w).clear_holds(Some((&ws, &user, changed)))?;
            }
        }
        Ok(())
    })
}

/// Next stored alarm deadline, including the expiry of a quiet-hour hold.
pub fn next_alarm_at(c: &mut impl Read, now: i64) -> steve_store::Result<Option<i64>> {
    Alarms(c).deadline(now)
}

/// The alarms of `subject` in `ws`, for the subject's own page and for tests.
pub fn of_subject(
    c: &mut impl Read,
    ws: &str,
    subject: &str,
) -> steve_store::Result<Vec<serde_json::Value>> {
    Alarms(c)
        .of_subject(ws, subject)?
        .into_iter()
        .map(|row| Ok(serde_json::to_value(row)?))
        .collect()
}

/// The alarm due source; dirty planning, stale deadlines and quiet-hour holds share one loop.
pub struct AlarmClock {
    cx: Arc<FlowCx>,
}
impl AlarmClock {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx }
    }
    async fn round_at(&self, now: i64) -> FlowResult<Claimed> {
        let _activity =
            self.cx.store().try_activity("flow.alarms").ok_or_else(|| {
                steve_modules::ApiError::Internal("store is in maintenance".into())
            })?;
        let recheck = self
            .cx
            .alarm_recheck
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        let cx = self.cx.clone();
        let result = self
            .cx
            .store()
            .admitted(self.cx.blocking(move |s| {
                recheck_holds(s, recheck, now)?;
                replan_dirty(s, &cx.alarms, now)?;
                let claimed = claim_due(s, &cx.alarms, now)?;
                // Repeating sources plan their next occurrence before next_due is queried again.
                replan_dirty(s, &cx.alarms, now)?;
                Ok(claimed)
            }))
            .await;
        if result.is_err() && recheck {
            self.cx.wake_alarms();
        }
        result
    }
    pub async fn round(&self) -> FlowResult<Claimed> {
        self.round_at(self.cx.now()).await
    }
    pub async fn run(self) {
        let cx = self.cx.clone();
        let clock = cx.modules.clock.clone();
        let wake = cx.alarm_wake.clone();
        // Subscribe before the first due query. Dirty rows include todo/brief trigger
        // writes; direct alarm edits must wake too. Their own commits only re-query.
        let changed = cx.store().commit_wake(&["time_alarm_dirty", "time_alarms"]);
        // Policy/timezone edits must also invalidate already stored quiet-hour holds,
        // even when their timestamps equal the alarm's. Never do this on our own writes.
        let policy = cx
            .store()
            .commit_wake(&["notification_policies", "settings"]);
        let observe = async move {
            loop {
                tokio::select! {
                    _ = changed.notified() => cx.alarm_wake.notify_one(),
                    _ = policy.notified() => cx.wake_alarms(),
                }
            }
        };
        tokio::join!(
            steve_runtime::due::due_loop(Arc::new(self), clock, wake, crate::scheduler::backoff()),
            observe
        );
    }
}
impl steve_runtime::due::DueSource for AlarmClock {
    fn name(&self) -> &'static str {
        "flow.alarms"
    }
    async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        let cx = self.cx.clone();
        self.cx
            .blocking(move |s| {
                if cx.alarm_recheck.load(std::sync::atomic::Ordering::SeqCst) {
                    return Ok(Some(cx.now()));
                }
                Ok(s.read(|c| {
                    let now = cx.now();
                    if !changed_policies(c, now)?.is_empty() {
                        return Ok(Some(now));
                    }
                    let alarm = next_alarm_at(c, now)?;
                    Ok(if Alarms(c).dirty(1)?.is_empty() {
                        alarm
                    } else {
                        Some(alarm.map_or(now, |at| at.min(now)))
                    })
                })?)
            })
            .await
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
    }
    async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        self.round_at(now)
            .await
            .map(|_| ())
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alarm_kind_with_a_topic_is_a_known_kind_and_topics_are_unique() {
        for t in TOPICS {
            if let Some(k) = t.alarm {
                assert!(KINDS.contains(&k), "{k}");
                assert_eq!(topic_of_kind(k).map(|t| t.name), Some(t.name));
            }
            assert_eq!(topic(t.name), Some(&t));
            assert!(matches!(
                t.feed_kind,
                "completion" | "reminder" | "briefing"
            ));
            // an alarm topic that is not written from its lines has its own intake; a non-alarm one has no lines
            assert!(t.alarm.is_some() || !t.from_lines);
        }
        let mut ids: Vec<_> = TOPICS.iter().map(|t| t.name).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TOPICS.len());
        assert_eq!(same_line("  Call  Lin \n"), same_line("call lin"));
    }
}

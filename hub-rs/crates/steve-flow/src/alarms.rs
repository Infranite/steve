//! `time_alarms`: one table for every alarm a person has (a todo's reminder, a snooze wake-up; later
//! the morning brief, meeting preparation and reminders). An alarm fires at most once.
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
//! **Firing.** [`claim_due`] takes the due rows in one transaction. Per row, by
//! [`crate::policy::plan_alarm`]: past its `stale_at` it is `stale`; within its owner's quiet hours (when
//! they would hear it) and its topic waits ([`QuietRule::Postpone`]), it is held until they end
//! (`hold_until`); otherwise it fires. The alarms of one owner and topic that fire together (several due
//! at once, those held through the quiet hours, those missed while the Hub was down) become **one**
//! outbox event ([`AlarmFired`]), and the rows are marked `fired` with that event's id (`run_ref`) in the
//! same transaction. The personal notification intake (steve-channels) turns the event into one
//! notification, its delivery keyed on the event: the outbox delivers it exactly once.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use steve_modules::zone::{Quiet, Zone};
use steve_store::{Store, Tx, uid};

use crate::policy::{AlarmAction, MISSED_GRACE_MS, QuietRule, plan_alarm};
use crate::{FlowCx, FlowResult};

/// `time_alarms.kind`: the morning brief, meeting preparation, a meeting reminder, a todo reminder,
/// a snooze wake-up.
pub const KINDS: [&str; 5] = ["brief", "prep", "remind", "todo", "snooze"];

/// A wanted alarm whose fire time is further past than this is not added or brought back.
pub const PAST_GRACE_MS: i64 = 5 * 60_000;
/// Alarms due this soon are claimed in the current round.
pub const EARLY_MS: i64 = 1_000;
/// Longest sleep between two rounds (a changed subject is planned within it).
pub const IDLE_MS: i64 = 15_000;
/// Dirty subjects planned, and due alarms claimed, per transaction.
pub const BATCH: usize = 200;

/// The topic of a todo reminder.
pub const TODO_DUE: &str = "todo.due";
/// The topic of a snooze wake-up.
pub const SNOOZE_WAKE: &str = "snooze.wake";
/// Payload version of [`TODO_DUE`] and [`SNOOZE_WAKE`] ([`AlarmFired`]).
pub const ALARM_FIRED_V: i64 = 1;

/// One personal notification topic: what the feed calls it, how quiet hours treat it, and whether
/// several that come due together are sent as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Topic {
    /// The outbox topic.
    pub name: &'static str,
    /// `feed_entries.kind` (migration 099's insert trigger maps the topics the same way).
    pub feed_kind: &'static str,
    pub quiet: QuietRule,
    /// Alarms of this topic that fire in the same round, for the same person, are one notification.
    pub merges: bool,
    /// `time_alarms.kind` that fires as this topic (`None`: not an alarm).
    pub alarm: Option<&'static str>,
    /// The id of the notification intake's outbox consumer of this topic (stable: its cursor is kept by it).
    pub consumer: &'static str,
}

/// The personal notification topics. Each has its own consumer in the notification intake.
pub const TOPICS: [Topic; 3] = [
    Topic {
        name: steve_work_api::ITEM_DONE,
        feed_kind: "completion",
        quiet: QuietRule::Postpone,
        merges: false,
        alarm: None,
        consumer: "notifications.work_done",
    },
    Topic {
        name: TODO_DUE,
        feed_kind: "reminder",
        quiet: QuietRule::Postpone,
        merges: true,
        alarm: Some("todo"),
        consumer: "notifications.todo_due",
    },
    Topic {
        name: SNOOZE_WAKE,
        feed_kind: "reminder",
        quiet: QuietRule::Postpone,
        merges: true,
        alarm: Some("snooze"),
        consumer: "notifications.snooze_wake",
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
    fn plan(&self, c: &Connection, ws: &str, id: &str, now: i64) -> FlowResult<Vec<Want>>;
    /// The line of an alarm of `kind` on `id` that fires now; `None`: it should not (the subject is
    /// gone or no longer wants it), and the alarm is cancelled.
    fn line(&self, c: &Connection, ws: &str, id: &str, kind: &str)
    -> FlowResult<Option<AlarmLine>>;
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

struct Row {
    id: String,
    owner: String,
    kind: String,
    planned_for: i64,
    fire_at: i64,
    stale_at: Option<i64>,
    state: String,
}

/// Makes the alarms of `subject` (in `ws`) the `wants`, by the rules in the module docs.
pub fn reconcile(
    tx: &Tx,
    ws: &str,
    subject: &str,
    wants: &[Want],
    now: i64,
) -> FlowResult<Reconciled> {
    let rows: Vec<Row> = {
        let mut st = tx.prepare_cached(
            "SELECT id, owner_user, kind, planned_for, fire_at, stale_at, state FROM time_alarms WHERE workspace_id = ? AND subject = ?",
        )?;
        st.query_map(params![ws, subject], |r| {
            Ok(Row {
                id: r.get(0)?,
                owner: r.get(1)?,
                kind: r.get(2)?,
                planned_for: r.get(3)?,
                fire_at: r.get(4)?,
                stale_at: r.get(5)?,
                state: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut out = Reconciled::default();
    let fresh = |w: &Want| w.fire_at >= now - PAST_GRACE_MS;
    for w in wants {
        let found = rows
            .iter()
            .find(|r| r.owner == w.owner && r.kind == w.kind && r.planned_for == w.planned_for);
        match found {
            Some(r) if r.state == "planned" => {
                if r.fire_at != w.fire_at || r.stale_at != w.stale_at {
                    tx.execute(
                        "UPDATE time_alarms SET fire_at = ?, stale_at = ?, hold_until = CASE WHEN fire_at = ? THEN hold_until END, updated = ? WHERE id = ?",
                        params![w.fire_at, w.stale_at, w.fire_at, now, r.id],
                    )?;
                    out.moved += 1;
                }
            }
            Some(r) if r.state == "cancelled" => {
                if fresh(w) {
                    tx.execute(
                        "UPDATE time_alarms SET state = 'planned', fire_at = ?, stale_at = ?, hold_until = NULL, updated = ? WHERE id = ?",
                        params![w.fire_at, w.stale_at, now, r.id],
                    )?;
                    out.restored += 1;
                }
            }
            // fired or stale: never again
            Some(_) => {}
            None => {
                if fresh(w) {
                    tx.execute(
                        "INSERT INTO time_alarms (id, workspace_id, owner_user, kind, subject, planned_for, fire_at, stale_at, state, created, updated)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'planned', ?, ?)",
                        params![
                            uid("alm"),
                            ws,
                            w.owner,
                            w.kind,
                            subject,
                            w.planned_for,
                            w.fire_at,
                            w.stale_at,
                            now,
                            now
                        ],
                    )?;
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
            tx.execute(
                "UPDATE time_alarms SET state = 'cancelled', hold_until = NULL, updated = ? WHERE id = ? AND state = 'planned'",
                params![now, r.id],
            )?;
            out.cancelled += 1;
        }
    }
    Ok(out)
}

/// Plans `subject` (`<family>:<id>`) again with its source. A subject no source knows keeps its alarms.
pub fn replan(
    tx: &Tx,
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
    let wants = source.plan(tx, ws, id, now)?;
    reconcile(tx, ws, subject, &wants, now).map(Some)
}

/// Plans every subject marked in `time_alarm_dirty` again, each in its own transaction (one that fails
/// is logged and stays marked; the others go on). Returns how many were planned.
pub fn replan_dirty(store: &Store, sources: &AlarmSources, now: i64) -> FlowResult<usize> {
    let mut done = 0;
    loop {
        let batch: Vec<(String, String)> = store.reader(|c| {
            let mut st = c.prepare_cached(
                "SELECT workspace_id, subject FROM time_alarm_dirty ORDER BY rowid LIMIT ?",
            )?;
            let v = st
                .query_map([BATCH as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, crate::FlowError>(v)
        })?;
        if batch.is_empty() {
            return Ok(done);
        }
        let mut progressed = false;
        for (ws, subject) in &batch {
            let r = store.transaction(|tx| -> FlowResult<()> {
                let family = subject.split_once(':').map(|(f, _)| f).unwrap_or_default();
                if sources.get(family).is_none() {
                    tracing::warn!(%subject, "no alarm source for this subject; left as it was");
                }
                replan(tx, sources, ws, subject, now)?;
                tx.execute(
                    "DELETE FROM time_alarm_dirty WHERE workspace_id = ? AND subject = ?",
                    params![ws, subject],
                )?;
                Ok(())
            });
            match r {
                Ok(()) => {
                    done += 1;
                    progressed = true;
                }
                Err(e) => tracing::error!(%ws, %subject, "planning alarms failed: {e}"),
            }
        }
        if !progressed || batch.len() < BATCH {
            return Ok(done);
        }
    }
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
/// owner's clock ([`Zone::of`]).
pub fn quiet_ends(c: &Connection, ws: &str, user: &str, now: i64) -> FlowResult<Option<i64>> {
    let p: Option<(bool, String, Option<u16>, Option<u16>)> = c
        .query_row(
            "SELECT enabled, mode, quiet_start, quiet_end FROM notification_policies WHERE workspace_id = ? AND user_id = ?",
            params![ws, user],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((true, mode, start, end)) = p else {
        return Ok(None);
    };
    if mode != "now" {
        return Ok(None);
    }
    let Some(q) = Quiet::from_parts(start, end) else {
        return Ok(None);
    };
    Ok(q.ends(Zone::of(c, ws, user)?.tz, now))
}

struct Due {
    id: String,
    ws: String,
    owner: String,
    kind: String,
    subject: String,
    fire_at: i64,
    stale_at: Option<i64>,
    hold_until: Option<i64>,
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

/// One round: claims every due alarm (see the module docs). One transaction: nothing fires twice.
pub fn claim_due(store: &Store, sources: &AlarmSources, now: i64) -> FlowResult<Claimed> {
    store.transaction(|tx| {
        let due: Vec<Due> = {
            let mut st = tx.prepare_cached(
                "SELECT id, workspace_id, owner_user, kind, subject, fire_at, stale_at, hold_until FROM time_alarms
                  WHERE state = 'planned' AND fire_at <= ? ORDER BY workspace_id, owner_user, fire_at, id LIMIT ?",
            )?;
            st.query_map(params![now + EARLY_MS, BATCH as i64], |r| {
                Ok(Due {
                    id: r.get(0)?,
                    ws: r.get(1)?,
                    owner: r.get(2)?,
                    kind: r.get(3)?,
                    subject: r.get(4)?,
                    fire_at: r.get(5)?,
                    stale_at: r.get(6)?,
                    hold_until: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut out = Claimed::default();
        let mut groups: Vec<Group> = Vec::new();
        let mut quiet: Vec<((String, String), Option<i64>)> = Vec::new();
        for d in due {
            let cancel = |tx: &Tx| -> FlowResult<()> {
                tx.execute(
                    "UPDATE time_alarms SET state = 'cancelled', hold_until = NULL, updated = ? WHERE id = ?",
                    params![now, d.id],
                )?;
                Ok(())
            };
            let Some(topic) = topic_of_kind(&d.kind) else {
                tracing::warn!(alarm = %d.id, kind = %d.kind, "no notification topic for this alarm kind; cancelled");
                cancel(tx)?;
                out.cancelled += 1;
                continue;
            };
            let key = (d.ws.clone(), d.owner.clone());
            let ends = match quiet.iter().find(|(k, _)| *k == key) {
                Some((_, e)) => *e,
                None => {
                    let e = quiet_ends(tx, &d.ws, &d.owner, now)?;
                    quiet.push((key, e));
                    e
                }
            };
            match plan_alarm(d.fire_at, d.stale_at, now, ends, topic.quiet) {
                AlarmAction::Stale => {
                    tx.execute(
                        "UPDATE time_alarms SET state = 'stale', hold_until = NULL, updated = ? WHERE id = ?",
                        params![now, d.id],
                    )?;
                    out.stale += 1;
                }
                AlarmAction::Hold { until } => {
                    if d.hold_until != Some(until) {
                        tx.execute(
                            "UPDATE time_alarms SET hold_until = ?, updated = ? WHERE id = ?",
                            params![until, now, d.id],
                        )?;
                    }
                    out.held += 1;
                }
                AlarmAction::Fire { late_ms } => {
                    let line = match d.subject.split_once(':') {
                        Some((family, id)) => match sources.get(family) {
                            Some(s) => s.line(tx, &d.ws, id, &d.kind)?,
                            None => None,
                        },
                        None => None,
                    };
                    let Some(line) = line else {
                        cancel(tx)?;
                        out.cancelled += 1;
                        continue;
                    };
                    let late = late_ms > MISSED_GRACE_MS;
                    let at = groups.iter_mut().find(|g| {
                        topic.merges && g.ws == d.ws && g.owner == d.owner && g.topic.name == topic.name
                    });
                    match at {
                        Some(g) => {
                            g.ids.push(d.id.clone());
                            if !g.lines.iter().any(|l| same_line(&l.text) == same_line(&line.text)) {
                                g.lines.push(line);
                            }
                            g.late |= late;
                        }
                        None => groups.push(Group {
                            ws: d.ws.clone(),
                            owner: d.owner.clone(),
                            topic,
                            ids: vec![d.id.clone()],
                            lines: vec![line],
                            late,
                        }),
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
            let kind = topic.alarm.unwrap_or_default();
            let payload = AlarmFired {
                owner,
                kind: kind.into(),
                alarms: ids.clone(),
                lines,
                late,
            };
            let ev = steve_store::outbox::emit(
                tx,
                &ws,
                topic.name,
                ALARM_FIRED_V,
                &serde_json::to_value(&payload)?,
            )?;
            for id in &ids {
                tx.execute(
                    "UPDATE time_alarms SET state = 'fired', fired_at = ?, run_ref = ?, hold_until = NULL, updated = ? WHERE id = ? AND state = 'planned'",
                    params![now, ev.id, now, id],
                )?;
            }
            out.fired += ids.len();
            out.events.push(ev.id);
        }
        Ok(out)
    })
}

/// When the next round is needed: the earliest planned alarm, a held one at its hold.
pub fn earliest(c: &Connection) -> FlowResult<Option<i64>> {
    Ok(c.query_row(
        "SELECT min(max(fire_at, coalesce(hold_until, fire_at))) FROM time_alarms WHERE state = 'planned'",
        [],
        |r| r.get(0),
    )?)
}

/// The alarms of `subject` in `ws`, for the subject's own page and for tests.
pub fn of_subject(c: &Connection, ws: &str, subject: &str) -> FlowResult<Vec<serde_json::Value>> {
    let mut st = c.prepare_cached(
        "SELECT id, owner_user, kind, planned_for, fire_at, stale_at, hold_until, state, run_ref FROM time_alarms
          WHERE workspace_id = ? AND subject = ? ORDER BY planned_for, kind",
    )?;
    let v = st
        .query_map(params![ws, subject], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "owner": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                "planned_for": r.get::<_, i64>(3)?, "fire_at": r.get::<_, i64>(4)?,
                "stale_at": r.get::<_, Option<i64>>(5)?, "hold_until": r.get::<_, Option<i64>>(6)?,
                "state": r.get::<_, String>(7)?, "run_ref": r.get::<_, Option<String>>(8)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// The `flow.alarms` task: plans changed subjects and claims due alarms, sleeping until the earliest
/// one (at most [`IDLE_MS`]) or a wake-up ([`FlowCx::wake_alarms`]). A Hub that was down claims what came
/// due meanwhile in its first round: nothing is lost, and a backlog is one message per person and topic.
pub struct AlarmClock {
    cx: Arc<FlowCx>,
}

impl AlarmClock {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx }
    }

    /// One round, when the store admits work (not during maintenance).
    pub async fn round(&self) -> FlowResult<Claimed> {
        let Some(_activity) = self.cx.store().try_activity("alarms") else {
            return Ok(Claimed::default());
        };
        self.cx
            .store()
            .admitted(async {
                let cx = self.cx.clone();
                self.cx
                    .blocking(move |s| {
                        let now = cx.now();
                        replan_dirty(s, &cx.alarms, now)?;
                        claim_due(s, &cx.alarms, now)
                    })
                    .await
            })
            .await
    }

    pub async fn run(self) {
        loop {
            match self.round().await {
                Ok(c) if c.fired + c.stale > 0 => {
                    tracing::info!(fired = c.fired, stale = c.stale, held = c.held, "alarms")
                }
                Ok(_) => {}
                Err(e) => tracing::error!("alarm round: {e}"),
            }
            let now = self.cx.now();
            let next = self
                .cx
                .store()
                .reader(earliest)
                .unwrap_or(None)
                .unwrap_or(i64::MAX);
            let wait = next.min(now + IDLE_MS).saturating_sub(now).max(0);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(wait as u64)) => {}
                _ = self.cx.alarm_wake.notified() => {}
            }
        }
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
            assert!(matches!(t.feed_kind, "completion" | "reminder"));
        }
        let mut ids: Vec<_> = TOPICS.iter().map(|t| t.name).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TOPICS.len());
        assert_eq!(same_line("  Call  Lin \n"), same_line("call lin"));
    }
}

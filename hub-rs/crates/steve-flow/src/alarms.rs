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

use std::sync::Arc;

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

/// Plans dirty subjects independently. A broken request cancels only its subject's alarms and
/// records a durable diagnostic; a later subject edit requests planning again.
pub fn replan_dirty(store: &Store, sources: &AlarmSources, now: i64) -> FlowResult<usize> {
    let rows = store.reader(|c| -> FlowResult<Vec<i64>> {
        let mut st =
            c.prepare_cached("SELECT rowid FROM time_alarm_dirty ORDER BY rowid LIMIT ?")?;
        Ok(st
            .query_map([BATCH as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })?;
    let mut done = 0;
    for rowid in rows {
        let key = crate::claim::Key::new("time_alarm_dirty", "subject", rowid);
        let planned = crate::claim::row(
            store,
            &key,
            now,
            |tx| {
                let (ws, subject): (String, String) = tx.query_row(
                    "SELECT workspace_id,subject FROM time_alarm_dirty WHERE rowid=?",
                    [rowid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                replan(tx, sources, &ws, &subject, now)?;
                tx.execute("DELETE FROM time_alarm_dirty WHERE rowid=?", [rowid])?;
                Ok(Some(()))
            },
            |tx, _| {
                let (ws, subject): (String, String) = tx.query_row(
                    "SELECT workspace_id,subject FROM time_alarm_dirty WHERE rowid=?",
                    [rowid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                tx.execute("UPDATE time_alarms SET state='cancelled',hold_until=NULL,updated=?3 WHERE workspace_id=?1 AND subject=?2 AND state='planned'",params![ws,subject,now])?;
                tx.execute("DELETE FROM time_alarm_dirty WHERE rowid=?", [rowid])?;
                Ok(None)
            },
        )?;
        if planned.is_some() {
            done += 1;
        }
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
    planned_for: i64,
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

/// Marks `subject` to be planned again: after one of its alarms fired or went stale, a source that
/// repeats (a brief schedule) plans the next one; for the others it changes nothing.
fn mark(tx: &Tx, ws: &str, subject: &str) -> FlowResult<()> {
    tx.execute(
        "INSERT INTO time_alarm_dirty(workspace_id, subject) VALUES (?, ?) ON CONFLICT DO NOTHING",
        params![ws, subject],
    )?;
    Ok(())
}

const INVALID_TIME: &str = "typeof(fire_at)<>'integer' OR (hold_until IS NOT NULL AND typeof(hold_until)<>'integer') OR (stale_at IS NOT NULL AND typeof(stale_at)<>'integer')";

enum RowOutcome {
    Cancelled,
    Stale,
    Held,
    Fire {
        due: Due,
        topic: &'static Topic,
        line: AlarmLine,
        late: bool,
    },
}

fn claim_one(
    tx: &Tx,
    sources: &AlarmSources,
    rowid: i64,
    now: i64,
) -> FlowResult<Option<RowOutcome>> {
    let d=tx.query_row("SELECT id,workspace_id,owner_user,kind,subject,planned_for,fire_at,stale_at,hold_until FROM time_alarms WHERE rowid=? AND state='planned'",[rowid],|r|Ok(Due {id:r.get(0)?,ws:r.get(1)?,owner:r.get(2)?,kind:r.get(3)?,subject:r.get(4)?,planned_for:r.get(5)?,fire_at:r.get(6)?,stale_at:r.get(7)?,hold_until:r.get(8)?})).optional()?;
    let Some(d) = d else { return Ok(None) };
    let cancel = |tx: &Tx| -> FlowResult<Option<RowOutcome>> {
        tx.execute(
            "UPDATE time_alarms SET state='cancelled',hold_until=NULL,updated=?2 WHERE rowid=?1",
            params![rowid, now],
        )?;
        Ok(Some(RowOutcome::Cancelled))
    };
    let Some(topic) = topic_of_kind(&d.kind) else {
        return cancel(tx);
    };
    let ends = quiet_ends(tx, &d.ws, &d.owner, now)?;
    let out = match plan_alarm(d.fire_at, d.stale_at, now, ends, topic.quiet) {
        AlarmAction::Stale => {
            tx.execute(
                "UPDATE time_alarms SET state='stale',hold_until=NULL,updated=?2 WHERE rowid=?1",
                params![rowid, now],
            )?;
            mark(tx, &d.ws, &d.subject)?;
            RowOutcome::Stale
        }
        AlarmAction::Hold { until } => {
            if d.hold_until != Some(until) {
                tx.execute(
                    "UPDATE time_alarms SET hold_until=?2,updated=?3 WHERE rowid=?1",
                    params![rowid, until, now],
                )?;
            }
            RowOutcome::Held
        }
        AlarmAction::Fire { late_ms } => {
            let line = match d.subject.split_once(':') {
                Some((family, id)) => match sources.get(family) {
                    Some(source) => source.line(tx, &d.ws, id, &d.kind)?,
                    None => None,
                },
                None => None,
            };
            let Some(mut line) = line else {
                return cancel(tx);
            };
            line.planned_for = Some(d.planned_for);
            mark(tx, &d.ws, &d.subject)?;
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
    store.transaction(|tx| {
        let rows:Vec<i64>={
            let sql=format!("SELECT rowid FROM time_alarms WHERE state='planned' AND (({INVALID_TIME}) OR (fire_at<=?1 AND (hold_until IS NULL OR hold_until<=?1 OR stale_at<=?1))) ORDER BY fire_at,id LIMIT ?2");
            let mut st=tx.prepare_cached(&sql)?;
            st.query_map(params![now,BATCH as i64],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut out=Claimed::default();
        let mut groups:Vec<Group>=Vec::new();
        for rowid in rows {
            let key=crate::claim::Key::new("time_alarms","id",rowid);
            let outcome=crate::claim::row(store,&key,now,|tx|claim_one(tx,sources,rowid,now),|tx,_| {
                let changed=tx.execute("UPDATE time_alarms SET state='cancelled',hold_until=NULL,updated=?2 WHERE rowid=?1 AND state='planned'",params![rowid,now])?;
                Ok((changed==1).then_some(RowOutcome::Cancelled))
            })?;
            match outcome {
                None=>{},
                Some(RowOutcome::Cancelled)=>out.cancelled+=1,
                Some(RowOutcome::Stale)=>out.stale+=1,
                Some(RowOutcome::Held)=>out.held+=1,
                Some(RowOutcome::Fire{due:d,topic,line,late})=>{
                    if let Some(g)=groups.iter_mut().find(|g|topic.merges&&g.ws==d.ws&&g.owner==d.owner&&g.topic.name==topic.name) {
                        g.ids.push(d.id);
                        if !g.lines.iter().any(|l|same_line(&l.text)==same_line(&line.text)) {g.lines.push(line);}
                        g.late|=late;
                    } else {groups.push(Group{ws:d.ws,owner:d.owner,topic,ids:vec![d.id],lines:vec![line],late});}
                }
            }
        }
        for Group{ws,owner,topic,ids,lines,late} in groups {
            let payload=AlarmFired{owner,kind:topic.alarm.unwrap_or_default().into(),alarms:ids.clone(),lines,late,at:Some(now)};
            let ev=steve_store::outbox::emit(tx,&ws,topic.name,ALARM_FIRED_V,&serde_json::to_value(&payload)?)?;
            for id in &ids {
                let changed=tx.execute("UPDATE time_alarms SET state='fired',fired_at=?2,run_ref=?3,hold_until=NULL,updated=?2 WHERE id=?1 AND state='planned'",params![id,now,ev.id])?;
                if changed!=1 {return Err(steve_modules::ApiError::conflict("alarm changed during claim"));}
            }
            out.fired+=ids.len();out.events.push(ev.id);
        }
        Ok(out)
    })
}

// Policy timestamps let the 60-second fallback notice a writer that missed its wake.
const CHANGED_POLICY: &str = "EXISTS(SELECT 1 FROM notification_policies p WHERE p.workspace_id=time_alarms.workspace_id AND p.user_id=time_alarms.owner_user AND p.updated>time_alarms.updated AND p.updated<=?1)";
fn policy_changed(c: &Connection, now: i64) -> FlowResult<bool> {
    Ok(c.query_row(&format!("SELECT EXISTS(SELECT 1 FROM time_alarms WHERE state='planned' AND hold_until IS NOT NULL AND {CHANGED_POLICY})"), [now], |r|r.get(0))?)
}
fn recheck_holds(store: &Store, all: bool, now: i64) -> FlowResult<()> {
    store.transaction(|tx| -> FlowResult<()> {
        let changed = if all { String::new() } else { format!(" AND {CHANGED_POLICY}") };
        let sql = format!("UPDATE time_alarms SET hold_until=NULL WHERE state='planned' AND hold_until IS NOT NULL{changed}");
        if all { tx.execute(&sql, [])?; } else { tx.execute(&sql, [now])?; }
        Ok(())
    })
}

/// Next stored alarm deadline, including the expiry of a quiet-hour hold.
pub fn next_alarm_at(c: &Connection, now: i64) -> FlowResult<Option<i64>> {
    let base = format!(
        "CASE WHEN {INVALID_TIME} THEN ?1 ELSE max(fire_at,min(coalesce(hold_until,fire_at),coalesce(stale_at,9223372036854775807))) END"
    );
    Ok(c.query_row(
        &format!("SELECT min({base}) FROM time_alarms WHERE state='planned'"),
        [now],
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

/// The alarm due source; dirty planning, stale deadlines and quiet-hour holds share one loop.
pub struct AlarmClock {
    cx: Arc<FlowCx>,
    changes: Option<tokio::sync::mpsc::Receiver<steve_types::Notification>>,
}
impl AlarmClock {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx, changes: None }
    }
    /// The Hub supplies its already committed digest/settings publications, subscribed at assembly.
    pub fn with_changes(
        mut self,
        changes: tokio::sync::mpsc::Receiver<steve_types::Notification>,
    ) -> Self {
        self.changes = Some(changes);
        self
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
    pub async fn run(mut self) {
        let cx = self.cx.clone();
        let clock = cx.modules.clock.clone();
        let wake = cx.alarm_wake.clone();
        let changes = self.changes.take();
        let observe = async move {
            if let Some(mut changes) = changes {
                while let Some(change) = changes.recv().await {
                    if matches!(
                        change.event.as_domain(),
                        Some(steve_types::DomainEvent::Config(
                            steve_types::ConfigEvent::Settings { .. }
                        ))
                    ) {
                        cx.wake_alarms();
                    } else {
                        // Todo triggers already marked exactly their subject. Preserve unrelated holds.
                        cx.alarm_wake.notify_one();
                    }
                }
            }
            std::future::pending::<()>().await;
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
                s.reader(|c| {
                    if policy_changed(c, cx.now())? {
                        return Ok(Some(cx.now()));
                    }
                    let dirty: Option<i64> = c.query_row(
                        "SELECT CASE WHEN EXISTS(SELECT 1 FROM time_alarm_dirty) THEN ?1 END",
                        [cx.now()],
                        |r| r.get(0),
                    )?;
                    let alarm = next_alarm_at(c, cx.now())?;
                    Ok(match (dirty, alarm) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    })
                })
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

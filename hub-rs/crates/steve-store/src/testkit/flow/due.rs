//! Database fixtures for the due-source contract and fault injection.

use crate::{Result, Store};
use rusqlite::params;

pub fn seed_claim_faults(store: &Store) -> Result<()> {
    store.write_locked(|c| {c.execute_batch("CREATE TABLE claim_faults(kind TEXT PRIMARY KEY); INSERT INTO claim_faults VALUES('plan'),('line');")?;Ok(())})
}

pub fn due_row_state(store: &Store, kind: DueKind, id: &str, done: bool) -> Result<bool> {
    store.sqlite_read(|c| {
        let sql = match (kind, done) {
            (DueKind::Trigger, true) => "SELECT EXISTS(SELECT 1 FROM flow_runs WHERE flow_id=?1)",
            (DueKind::Trigger, false) => "SELECT enabled=0 FROM flow_triggers WHERE id=?1",
            (_, true) => "SELECT status='running' AND retry_at IS NULL FROM flow_runs WHERE id=?1",
            (_, false) => "SELECT status='failed' FROM flow_runs WHERE id=?1",
        };
        Ok::<_, crate::StoreError>(c.query_row(sql, [id], |r| r.get(0))?)
    })
}

pub fn seed_due_row(
    store: &Store,
    kind: DueKind,
    ws: &str,
    actor: &str,
    id: &str,
    at: i64,
    poison: bool,
) -> Result<()> {
    store.write(|tx| {
            let graph = serde_json::json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"time","cron":"0 0 * * *","tz":"UTC"}}],"edges":[]});
            tx.conn.execute("INSERT INTO flows(id,workspace_id,name,kind,enabled,version,graph,origin,schedule,created_by,created,updated) VALUES(?1,?2,?1,'schedule',1,1,?3,'me','{\"overlap\":\"run\"}',?4,1,1)", params![id, ws, graph.to_string(), actor])?;
            match kind {
                DueKind::Trigger => {
                    let cron = if poison { "invalid" } else { "0 0 * * *" };
                    tx.conn.execute("INSERT INTO flow_triggers(id,workspace_id,flow_id,node_id,kind,cron,tz,next_at,missed,enabled) VALUES(?1,?2,?1,'t','time',?3,'UTC',?4,'catch_up_once',1)", params![id, ws, cron, at])?;
                }
                DueKind::Wait | DueKind::Retry => {
                    tx.conn.execute("INSERT INTO flow_runs(id,workspace_id,flow_id,flow_version,trigger_kind,status,cursor,context,retry_at,attempts,started) VALUES(?1,?2,?1,1,'manual','waiting',?3,'{}',?4,?5,1)", params![id, ws, if poison { "wrong" } else { "w" }, if matches!(kind,DueKind::Retry) { Some(at) } else { None }, if poison { 2 } else { 1 }])?;
                    if matches!(kind,DueKind::Wait) {
                        tx.conn.execute("INSERT INTO flow_waits(run_id,node_id,workspace_id,mode,until,created) VALUES(?1,'w',?2,'delay',?3,1)", params![id, ws, at])?;
                    }
                }
            }
            Ok(())
        })
}

pub fn alarm_ids(store: &Store, state: &str) -> Result<Vec<String>> {
    store.sqlite_read(|c| {
        let mut q = c.prepare("SELECT id FROM time_alarms WHERE state=? ORDER BY id")?;
        Ok::<_, crate::StoreError>(
            q.query_map([state], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?,
        )
    })
}

pub fn seed_due_alarm(store: &Store, ws: &str, owner: &str, id: &str, at: i64) -> Result<()> {
    store.write(|tx| {
            tx.conn.execute("INSERT INTO time_alarms(id,workspace_id,owner_user,kind,subject,planned_for,fire_at,created,updated) VALUES(?1,?2,?3,'todo',?4,?5,?5,1,1)",params![id, ws, owner,format!("contract:{id}"),at])?;Ok(())
        })
}

pub fn set_todo_reminder(store: &Store, ws: &str, owner: &str, now: i64, at: i64) -> Result<()> {
    store.write(|t| {
            t.conn.execute("INSERT INTO todos(id,workspace_id,owner,text,state,source,writer,created,updated,remind_at) VALUES('change',?1,?2,'remind','open','you',?2,?3,?3,?4) ON CONFLICT(id) DO UPDATE SET remind_at=?4,updated=?3",params![ws,owner,now,at])?;
            Ok(())
        })
}

pub fn hold_due_alarm(store: &Store, id: &str, until: i64) -> Result<usize> {
    store.write_locked(|c| {
        Ok(c.execute(
            "UPDATE time_alarms SET hold_until=?2 WHERE id=?1",
            params![id, until],
        )?)
    })
}

pub fn expire_due_alarm(store: &Store, id: &str, at: i64) -> Result<usize> {
    store.write_locked(|c| {
        Ok(c.execute(
            "UPDATE time_alarms SET stale_at=?2 WHERE id=?1",
            params![id, at],
        )?)
    })
}

pub fn hold_due_alarm_with_timestamp(
    store: &Store,
    id: &str,
    until: i64,
    now: i64,
) -> Result<usize> {
    store.write_locked(|c| {
        Ok(c.execute(
            "UPDATE time_alarms SET hold_until=?1,updated=?2 WHERE id=?3",
            params![until, now, id],
        )?)
    })
}

pub fn clear_failed_alarm_request(store: &Store) -> Result<usize> {
    store.write_locked(|c| {
        Ok(c.execute(
            "DELETE FROM time_alarm_dirty WHERE subject='contract:source_error'",
            [],
        )?)
    })
}

pub fn seed_alarm_line_failure(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("CREATE TABLE claim_leaks(kind TEXT)")?;
        c.execute(
            "UPDATE time_alarms SET subject='todo:line' WHERE id=?",
            [id],
        )?;
        Ok(())
    })
}

pub fn seed_alarm_plan_failure(store: &Store, ws: &str, user: &str, now: i64) -> Result<()> {
    store.write_locked(|c| {
            c.execute_batch("CREATE TABLE claim_leaks(kind TEXT)")?;
            c.execute("INSERT INTO todos(id,workspace_id,owner,text,state,source,writer,created,updated,remind_at) VALUES('plan',?1,?2,'retry planning','open','you',?2,?3,?3,?3)",params![ws,user,now])?;
            Ok(())
        })
}

pub fn repair_alarm_plan(store: &Store, at: i64) -> Result<()> {
    store.write_locked(|c| {
        c.execute("DELETE FROM claim_faults WHERE kind='plan'", [])?;
        c.execute("UPDATE todos SET remind_at=? WHERE id='plan'", [at])?;
        Ok(())
    })
}

pub fn track_signal_settlement(store: &Store, ws: &str, id: &str, now: i64) -> Result<()> {
    store.write_locked(|c| {
        c.execute("INSERT INTO ext_events(id,workspace_id,source,type,dedupe_key,occurred,received,summary) VALUES('settle-event',?,'manual','manual','settle-event',?2,?2,'test')",params![ws,now])?;
        c.execute("UPDATE flow_runs SET event_id='settle-event' WHERE id=?",[id])?;
        c.execute_batch("CREATE TABLE settle_counts(n INTEGER); CREATE TRIGGER count_settle AFTER UPDATE ON ext_events WHEN NEW.id='settle-event' BEGIN INSERT INTO settle_counts(n) VALUES(1); END;")?;Ok(())
    })
}

pub fn change_alarm_policy_or_timezone(
    store: &Store,
    ws: &str,
    user: &str,
    now: i64,
    policy: bool,
) -> Result<()> {
    store.write(|tx| {
            if policy {
                tx.conn.execute("INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated) VALUES(?1,?2,0,'now','UTC',1,0,?3)", params![ws,user,now])?;
            } else {
                tx.conn.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?1,'tz','\"UTC\"',?2) ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value,updated=excluded.updated", params![ws,now])?;
            }
            Ok(())
        })
}

pub fn reschedule_due_alarm(store: &Store, id: &str, at: i64) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE time_alarms SET fire_at=?1 WHERE id=?2",
            params![at, id],
        )?;
        Ok(())
    })
}

pub fn seed_quiet_alarm(store: &Store, ws: &str, user: &str, now: i64) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,quiet_start,quiet_end,consented_at,consent_since_seq,updated) VALUES(?1,?2,1,'now','UTC',0,120,1,0,?3)",params![ws,user,now])?;
        tx.conn.execute("INSERT INTO time_alarms(id,workspace_id,owner_user,kind,subject,planned_for,fire_at,created,updated) VALUES('held',?1,?2,'todo','contract:held',?3,?3,?3,?3)", params![ws,user,now])?;
        Ok(())
    })
}

#[derive(Clone, Copy)]
pub enum DueKind {
    Trigger,
    Wait,
    Retry,
}

pub fn todo_reminder(
    r: &mut impl crate::Read,
    ws: &str,
    id: &str,
) -> Result<Option<(String, i64)>> {
    use sea_query::{Expr, ExprTrait, Query};
    r.one(
        Query::select()
            .from("todos")
            .columns(["owner", "remind_at"])
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("state").eq("open"))
            .and_where(Expr::col("remind_at").is_not_null()),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn disable_notifications_outside_store(
    store: &Store,
    ws: &str,
    user: &str,
    now: i64,
) -> Result<()> {
    let conn = rusqlite::Connection::open(store.path())?;
    conn.execute("INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated) VALUES(?1,?2,0,'now','UTC',1,0,?3)", params![ws,user,now])?;
    Ok(())
}

pub fn record_claim_leak(w: &mut crate::W<'_>, kind: &str) -> Result<()> {
    w.exec(
        sea_query::Query::insert()
            .into_table("claim_leaks")
            .columns(["kind"])
            .values_panic([kind.into()]),
    )?;
    Ok(())
}

pub fn claim_fault(r: &mut impl crate::Read, kind: &str) -> Result<bool> {
    use sea_query::{Expr, ExprTrait, Query};
    Ok(r.one(
        Query::select()
            .from("claim_faults")
            .column("kind")
            .and_where(Expr::col("kind").eq(kind)),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn claim_leaks(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT count(*) FROM claim_leaks", [], |r| r.get(0))?))
}

pub fn settle_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT count(*) FROM settle_counts", [], |r| r.get(0))?))
}

pub fn reject_due_claim(store: &Store, kind: DueKind, id: &str) -> Result<()> {
    assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    let trigger = match kind {
        DueKind::Trigger => format!(
            "CREATE TRIGGER fail_claim BEFORE UPDATE OF next_at ON flow_triggers WHEN OLD.id='{id}' BEGIN SELECT RAISE(ABORT,'persistent claim failure'); END;"
        ),
        DueKind::Wait | DueKind::Retry => format!(
            "CREATE TRIGGER fail_claim BEFORE UPDATE OF status ON flow_runs WHEN OLD.id='{id}' AND NEW.status='running' BEGIN SELECT RAISE(ABORT,'persistent claim failure'); END;"
        ),
    };
    store.write_locked(|c| {
        c.execute_batch(&trigger)?;
        c.execute("UPDATE flow_triggers SET missed='skip'", [])?;
        Ok(())
    })
}

pub fn claim_failure_count(store: &Store, target: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM audit WHERE action='flow.claim_failed' AND target=?",
            [target],
            |r| r.get(0),
        )?)
    })
}

pub fn alarm_state(store: &Store, id: &str) -> Result<(String, Option<i64>)> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT state,hold_until FROM time_alarms WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn seed_schedule_hold(
    w: &mut crate::W<'_>,
    ws: &str,
    owner: &str,
    now: i64,
    until: i64,
) -> Result<()> {
    w.exec(
        sea_query::Query::insert()
            .into_table("time_alarms")
            .columns([
                "id",
                "workspace_id",
                "owner_user",
                "kind",
                "subject",
                "planned_for",
                "fire_at",
                "hold_until",
                "created",
                "updated",
            ])
            .values_panic([
                "schedule-hold".into(),
                ws.into(),
                owner.into(),
                "todo".into(),
                "contract:held".into(),
                now.into(),
                now.into(),
                until.into(),
                now.into(),
                now.into(),
            ]),
    )?;
    Ok(())
}

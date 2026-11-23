use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, Values};

use crate::contract::Q;
use crate::{R, Result, Store, W};

pub fn seed(store: &Store, at: i64) -> Result<()> {
    store.write(|w| {
        let db = w.db();
        let time = Values(vec![at.into()]);
        db.exec("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s','default','custom','.','running',?1,?1)", &time)?;
        db.exec("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated) VALUES ('s','n','{}','s','r','active','a',?)", &time)?;
        db.exec("INSERT INTO runtime_turns(turn_id,session_id,state,active_attempt,created,updated) VALUES ('t','s','active','a',?1,?1)", &time)?;
        db.exec("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a','t',0,'n','op','{}','{}','d','planned',?1,?1)", &time)?;
        db.exec("INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_attempt,state,created,updated) VALUES ('op','n','start_turn','{}','d','a','finished',?1,?1)", &time)?;
        Ok(())
    })
}

pub fn units(r: &mut R<'_>) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(Func::sum(Expr::col("units")))
            .from("realtime_output_samples"),
        |row| row.get::<Option<i64>>(0),
    )?
    .flatten()
    .unwrap_or(0))
}

pub fn ends(r: &mut R<'_>) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col("kind")))
            .from("realtime_output_samples")
            .and_where(Expr::col("kind").eq("end")),
        |row| row.get(0),
    )?
    .unwrap_or(0))
}

pub struct LastSample {
    pub ended: i64,
    pub effective: i64,
    pub source: i64,
    pub received: i64,
    pub kind: String,
}

pub fn last_sample(r: &mut R<'_>) -> Result<Option<LastSample>> {
    r.one(
        Query::select()
            .columns([
                ("a", "ended"),
                ("s", "at"),
                ("s", "source_at"),
                ("s", "received_at"),
                ("s", "kind"),
            ])
            .from_as("realtime_output_samples", "s")
            .join_as(
                JoinType::InnerJoin,
                "realtime_output_attempts",
                "a",
                Expr::col(("a", "attempt_id")).eq(Expr::col(("s", "attempt_id"))),
            )
            .order_by(("s", "seq"), Order::Desc)
            .limit(1),
        |r| {
            Ok(LastSample {
                ended: r.get(0)?,
                effective: r.get(1)?,
                source: r.get(2)?,
                received: r.get(3)?,
                kind: r.get(4)?,
            })
        },
    )
}

pub fn baseline(r: &mut R<'_>) -> Result<Option<(bool, i64)>> {
    r.one(
        Query::select()
            .columns(["baseline", "ended"])
            .from("realtime_output_attempts")
            .and_where(Expr::col("attempt_id").eq("a")),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

pub fn forget_attempt(store: &Store, ended: i64) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::delete()
                .from_table("realtime_output_attempts")
                .and_where(Expr::col("attempt_id").eq("a")),
        )?;
        update_attempt(w, "finished", Some(ended))
    })
}

pub fn set_attempt(store: &Store, state: &str, at: Option<i64>) -> Result<()> {
    store.write(|w| update_attempt(w, state, at))
}

fn update_attempt(w: &mut W<'_>, state: &str, at: Option<i64>) -> Result<()> {
    let mut query = Query::update();
    query
        .table("runtime_attempts")
        .value("state", state)
        .and_where(Expr::col("attempt_id").eq("a"));
    if let Some(at) = at {
        query.value("updated", at);
    }
    w.exec(&query)?;
    Ok(())
}

pub fn reject_watermark(store: &Store) -> Result<()> {
    store.write(|w| {
        w.db().batch(
            "CREATE TRIGGER refuse_telemetry BEFORE UPDATE OF applied ON node_streams
        WHEN NEW.applied>OLD.applied BEGIN SELECT RAISE(ABORT,'rollback sample'); END;",
        )
    })
}

pub fn allow_watermark(store: &Store) -> Result<()> {
    store.write(|w| w.db().batch("DROP TRIGGER refuse_telemetry"))
}

pub fn seed_attribution(store: &Store) -> Result<()> {
    store.write(|w| w.db().batch("INSERT INTO projects(id,workspace_id,name,path,created,updated) VALUES ('p1','default','one','.',1,1),('p2','default','two','.',1,1);
            INSERT INTO items(id,workspace_id,no,title,status,stage,project_id,created,updated) VALUES ('i1','default',1,'one','进行中','执行','p1',1,1),('i2','default',2,'two','进行中','执行','p2',1,1);
            UPDATE sessions SET item_id='i1',bot_id='b1' WHERE id='s';
            INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('s2','default','i2','b2','custom','.','running',1,1);
            INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated) VALUES ('s2','n','{}','s2','r','active','a2',1);
            INSERT INTO runtime_turns(turn_id,session_id,state,active_attempt,created,updated) VALUES ('t2','s2','active','a2',1,1);
            INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a2','t2',0,'n','op2','{}','{}','d','planned',1,1);"))
}

pub fn change_attribution(store: &Store) -> Result<()> {
    store.write(|w| w.db().batch("UPDATE items SET project_id='p2' WHERE id='i1'; UPDATE sessions SET bot_id='b2' WHERE id='s';"))
}

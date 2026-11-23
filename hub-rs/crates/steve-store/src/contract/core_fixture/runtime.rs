//! Fixtures for node reports that the fake Core runtime cannot ingest itself.

use rusqlite::params;
use serde_json::json;

use crate::runtime::{NewAttempt, NewCommand, insert_attempt_on, insert_command_on};
use crate::{Result, Store, StoreError, W};

pub struct FinishedTurn<'a> {
    pub session: &'a str,
    pub turn: &'a str,
    pub attempt: &'a str,
    pub node: &'a str,
    pub node_session: &'a str,
    pub ordinal: i64,
    pub operation: &'a str,
    pub at: i64,
}

pub fn finished_turn(store: &Store, turn: FinishedTurn<'_>) {
    store.transaction::<_, StoreError>(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
             VALUES (?,?,'ws',?,'r1','ready',?)",
             params![turn.session, turn.node, turn.node_session, turn.at],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?,?,'finished',?,?)",
            params![turn.turn, turn.session, turn.at, turn.at],
        )?;
        tx.execute(
            "INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated)
             VALUES (?,?,?,?,?,'{}','{}','d','finished',?,?)",
             params![turn.attempt, turn.turn, turn.ordinal, turn.node, turn.operation, turn.at, turn.at],
        )?;
        Ok(())
    }).unwrap();
}

pub fn reported_text(store: &Store, session: &str, attempt: &str, text: &str, at: i64) {
    store.transaction::<_, StoreError>(|tx| {
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(source_seq),0)+1 FROM events WHERE source_node='node-a' AND source_stream='events' AND source_epoch='e1'",
            [],
            |r| r.get(0),
        )?;
        let data = json!({"role": "assistant", "text": text});
        tx.execute(
            "INSERT INTO events(session_id,ts,kind,data,format,source_node,source_stream,source_epoch,source_seq,source_attempt)
             VALUES (?,?,'text',?,'steve.v1','node-a','events','e1',?,?)",
            params![session, at, data.to_string(), seq, attempt],
        )?;
        Ok(())
    }).unwrap();
}

pub fn accept_node_command(store: &Store, op: &str) {
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE node_commands SET state='accepted',receipt='{}' WHERE operation_id=?",
                [op],
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn node_command_count(store: &Store) -> i64 {
    store
        .reader::<_, StoreError>(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM node_commands", [], |r| r.get(0))?)
        })
        .unwrap()
}

pub fn active_workflow_session(store: &Store, session: &str, at: i64) {
    store.transaction::<_, StoreError>(|tx| {
        tx.execute("INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created,last_seen) VALUES ('n','me','node',zeroblob(32),'fingerprint','active',?1,?1)", [at])?;
        tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated) VALUES (?1,'n','{}',?1,'r','active','a_current',?2)", params![session, at])?;
        tx.execute("INSERT INTO runtime_turns(turn_id,session_id,state,active_attempt,created,updated) VALUES ('t_current',?1,'running','a_current',?2,?2)", params![session, at])?;
        tx.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a_current','t_current',0,'n','op','{}','{}','digest','running',?1,?1)", [at])?;
        tx.execute("INSERT INTO node_streams(node_id,stream,epoch,applied,session_id) VALUES ('n','stream','epoch',10,?1)", [session])?;
        Ok(())
    }).unwrap();
}

pub fn mark_activity(store: &Store, received: i64) {
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE runtime_attempts SET active_at=? WHERE attempt_id='a_current'",
                [received],
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn clear_inbound_heartbeat(store: &Store) {
    store
        .with_conn(|c| {
            c.execute("UPDATE nodes SET last_seen=0 WHERE node_id='n'", [])?;
            Ok(())
        })
        .unwrap();
}

pub fn stream_fault(store: &Store, fault: Option<&str>) {
    store
        .with_conn(|c| {
            c.execute("UPDATE node_streams SET fault=? WHERE node_id='n'", [fault])?;
            Ok(())
        })
        .unwrap();
}

pub fn cancel_requested(store: &Store, requested: bool) {
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE runtime_attempts SET cancel_requested=? WHERE attempt_id='a_current'",
                [i64::from(requested)],
            )?;
            Ok(())
        })
        .unwrap();
}

pub fn release_binding(store: &Store, session: &str) {
    store.transaction::<_, StoreError>(|tx| {
        tx.execute("INSERT OR IGNORE INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created,last_seen) VALUES ('local','me','node',zeroblob(32),'fingerprint','active',1,1)", [])?;
        tx.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,'local','{}',?1,'r','released',1)", [session])?;
        Ok(())
    }).unwrap();
}

pub fn open_turn(store: &Store, session: &str) {
    store.with_conn(|c| {
        c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES ('t_open',?1,'running',1,1)", [session])?;
        Ok(())
    }).unwrap();
}

pub fn pending_start(
    w: &mut W<'_>,
    session: &str,
    attempt: &NewAttempt<'_>,
    command: &NewCommand<'_>,
    at: i64,
) -> Result<()> {
    let tx = w.conn;
    tx.execute(
        "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
         VALUES (?,?,'ws','ns-1','r1','open',?)",
        params![session, attempt.node, at],
    )?;
    tx.execute(
        "INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?,?,'planned',?,?)",
        params![attempt.turn, session, at, at],
    )?;
    insert_attempt_on(tx, attempt, at)?;
    insert_command_on(tx, command, at)?;
    Ok(())
}

pub fn put_node_command(w: &mut W<'_>, command: &NewCommand<'_>, at: i64) -> Result<bool> {
    insert_command_on(w.conn, command, at)
}

pub fn mark_command_sent(w: &mut crate::W<'_>, op: &str, at: i64) -> crate::Result<()> {
    use sea_query::{Expr, ExprTrait, Query};
    w.exec(
        Query::update()
            .table("node_commands")
            .values([("maybe_sent", 1_i64.into()), ("retry_at", at.into())])
            .and_where(Expr::col("operation_id").eq(op)),
    )?;
    Ok(())
}

pub fn retime_session(store: &Store, session: &str, at: i64) {
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute(
                "UPDATE sessions SET updated=? WHERE id=?",
                params![at, session],
            )?;
            tx.execute(
                "UPDATE events SET ts=? WHERE session_id=?",
                params![at, session],
            )?;
            Ok(())
        })
        .unwrap();
}

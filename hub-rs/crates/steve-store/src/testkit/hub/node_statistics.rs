//! Fixtures for boot-time interruption of model statistics.

use rusqlite::Connection;

use crate::{Result, Store};

pub fn mark_node_attempt(store: &Store, turn: &str, attempt: &str) -> Result<()> {
    let db = Connection::open(store.path())?;
    db.pragma_update(None, "foreign_keys", false)?;
    db.execute(
        "UPDATE agent_turns SET node_attempt_id = ? WHERE id = ?",
        [attempt, turn],
    )?;
    Ok(())
}

pub fn turn_end(store: &Store, turn: &str) -> Result<(Option<i64>, Option<String>)> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT ended, stop_reason FROM agent_turns WHERE id = ?",
            [turn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn usage_count(store: &Store) -> Result<i64> {
    store.reader(|c| Ok(c.query_row("SELECT count(*) FROM usage_records", [], |r| r.get(0))?))
}

use std::path::Path;

pub fn seed_realtime_filters(path: &std::path::Path, at: i64) -> Result<()> {
    let c = super::api::writable(path)?;
    c.execute(
        "INSERT INTO workspaces(id,name,created) VALUES ('private','private',?)",
        [at],
    )?;
    for (id, ws) in [("p1", "default"), ("p2", "default"), ("px", "private")] {
        c.execute("INSERT INTO projects(id,workspace_id,name,path,created,updated) VALUES (?,?,?,'.',?4,?4)",rusqlite::params![id,ws,id,at])?;
    }
    for (id, ws) in [("b1", "default"), ("b2", "default"), ("bx", "private")] {
        c.execute(
            "INSERT INTO bots(id,workspace_id,name,harness,updated) VALUES (?,?,?,'fixture',?)",
            rusqlite::params![id, ws, id, at],
        )?;
        c.execute(
            "INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES (?,?,?)",
            rusqlite::params![id, ws, at],
        )?;
    }
    for (id, ws, project, bot, units) in [
        ("a", "default", "p1", "b1", 40),
        ("b", "default", "p1", "b2", 80),
        ("c", "default", "p2", "b1", 120),
        ("x", "private", "px", "bx", 400000),
    ] {
        c.execute("INSERT INTO sessions(id,workspace_id,bot_id,harness,cwd,status,created,updated) VALUES (?,?,?,'fixture','.','idle',?4,?4)",rusqlite::params![id,ws,bot,at-2_000])?;
        c.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?,'fixture','{}',?1,'r','ready',?)",rusqlite::params![id,at])?;
        c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES (?,?1,'finished',?2,?2)",rusqlite::params![id,at-2_000])?;
        c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES (?,?1,0,'fixture',?1,'{}','{}','d','finished',?2,?3)",rusqlite::params![id,at-2_000,at-1_000])?;
        c.execute("INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,project_id,bot_id,node_id,started,ended) VALUES (?,?1,?,?,?,'fixture',?,?)",rusqlite::params![id,ws,project,bot,at-2_000,at-1_000])?;
        c.execute("INSERT INTO realtime_output_samples(node_id,stream,epoch,seq,event_id,attempt_id,at,source_at,received_at,kind,units) VALUES ('fixture',?,'e',1,?1,?1,?2,?2,?2,'text',?3)",rusqlite::params![id,at-1_000,units])?;
    }
    Ok(())
}

pub fn seed_usage_summary(path: &std::path::Path, today: &str, at: i64) -> Result<()> {
    super::api::writable(path)?.execute_batch(&format!("INSERT INTO stat_daily (workspace_id, day, source, purpose, harness, model, input, output, cache_read, cache_write, hit_base, calls, turns, turn_ms, tools, tool_kinds)
             VALUES ('default', '{today}', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 1000000, 0, 3000000, 0, 4000000, 1, 2, 60000, 3, '{{\"execute\":2,\"mcp:steve_deliver\":1,\"tool:Bash\":2}}'),
                    ('default', '{today}', 'gateway', 'route', 'claude-cli', 'gpt-9', 100, 10, 0, 0, 0, 1, 0, 0, 0, '{{}}');
             INSERT INTO usage_records (workspace_id,ts,day,source,purpose,harness,model,input,output,cache_read,cache_write)
             VALUES ('default', {at}, '{today}', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 1000000, 0, 3000000, 0),
                    ('default', {at}, '{today}', 'gateway', 'route', 'claude-cli', 'gpt-9', 100, 10, 0, 0);
             INSERT INTO workspaces (id, name, created) VALUES ('elsewhere', '别处', 1);
             INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES ('prj_other', 'elsewhere', '别处', '/nonexistent', 1, 1);"))?;
    Ok(())
}

pub fn price_history(path: &std::path::Path) -> Result<(i64, Vec<i64>)> {
    let db = super::inspect_database(path)?;
    let audits = db.query_row(
        "SELECT COUNT(*) FROM audit WHERE action='stats.prices'",
        [],
        |r| r.get(0),
    )?;
    let mut q =
        db.prepare("SELECT version FROM revisions WHERE object_type='stats_prices' ORDER BY id")?;
    let versions = q
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((audits, versions))
}

pub fn seed_catalog_usage(path: &std::path::Path, today: &str, yesterday: &str) -> Result<()> {
    super::api::writable(path)?.execute_batch(&format!("INSERT INTO stat_daily (workspace_id, day, source, purpose, harness, model, input, output, cache_read, cache_write, hit_base, calls, turns, turn_ms, tools, tool_kinds)
             VALUES ('default', '{yesterday}', 'agent', 'turn', 'codex', 'gpt-5', 1000000, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}'),
                    ('default', '{today}', 'agent', 'turn', 'codex', 'gpt-5', 1000000, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}'),
                    ('default', '{today}', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 1000000, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}'),
                    ('default', '{today}', 'agent', 'turn', 'claude-code', 'mystery-1', 500, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}');"))?;
    Ok(())
}

pub fn turn_trace(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q =
        db.prepare("SELECT id, started, ended, stop_reason, node_attempt_id FROM agent_turns")?;
    Ok(q.query_map([], |r| {
        Ok(format!(
            "{:?}/{:?}/{:?}/{:?}/{:?}",
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?
        ))
    })?
    .collect::<rusqlite::Result<_>>()?)
}

pub fn event_trace(path: &Path) -> Result<Vec<String>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT kind, substr(data,1,200) FROM events WHERE kind<>'agent_message_chunk' ORDER BY seq")?;
    Ok(q.query_map([], |r| {
        Ok(format!(
            "{}:{}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?
        ))
    })?
    .collect::<rusqlite::Result<_>>()?)
}

pub fn acp_turns(
    path: &Path,
    session: &str,
) -> Result<Vec<(Option<String>, Option<i64>, i64, String, i64, String)>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT item_id, user_seq, attempt, stop_reason, tools, tool_kinds FROM agent_turns WHERE session_id = ? ORDER BY started, rowid")?;
    Ok(q.query_map([session], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })?
    .collect::<rusqlite::Result<_>>()?)
}

pub fn acp_usage(
    path: &Path,
    session: &str,
) -> Result<
    Vec<(
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<f64>,
        String,
    )>,
> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT model, input, output, cache_read, cache_write, cost_reported, detail FROM usage_records WHERE session_id = ? ORDER BY id")?;
    Ok(q.query_map([session], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })?
    .collect::<rusqlite::Result<_>>()?)
}

pub fn acp_cube(path: &Path) -> Result<(i64, i64, i64)> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT sum(input), sum(cache_read), sum(turns) FROM stat_daily WHERE source = 'agent'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

pub fn seed_streaming_bot(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute_batch("INSERT INTO bots(id,workspace_id,name,harness,enabled,updated) VALUES ('realtime-bot','default','stream','claude-code',1,1);
        INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('realtime-bot','default',1);")?;
    Ok(())
}

pub fn latest_realtime_end(path: &Path) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT MAX(at) FROM realtime_output_samples WHERE kind='end'",
        [],
        |r| r.get(0),
    )?)
}

pub fn realtime_units(path: &Path, from: i64, to: i64) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row(
        "SELECT COALESCE(SUM(units),0) FROM realtime_output_samples WHERE at>? AND at<=?",
        [from, to],
        |r| r.get(0),
    )?)
}

pub fn seed_session_token_windows(store: &Store) -> Result<()> {
    store.with_conn(|tx| {
        tx.execute_batch(
            "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES
               ('a','default','fixture','.','ended',0,0), ('b','default','fixture','.','ended',0,0);
             INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES
               ('a','local','{}','a','r','idle',0), ('b','remote','{}','b','r','idle',0);
             INSERT INTO usage_records(workspace_id,ts,day,source,purpose,harness,session_id,input,output,cache_read,cache_write,reasoning) VALUES
               ('default',2000,'2026-11-01','agent','turn','fixture','a',10,6,3,1,4),
               ('default',61000,'2026-11-02','agent','turn','fixture','a',10,5,NULL,NULL,3),
               ('default',61000,'2026-11-02','agent','turn','fixture','b',50,5,NULL,NULL,2),
               ('default',61000,'2026-11-02','gateway','think','fixture',NULL,100,50,NULL,NULL,0),
               ('other',61000,'2026-11-02','agent','turn','fixture','a',500,50,NULL,NULL,0);"
        )?;
        Ok(())
    })
}

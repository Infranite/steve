//! Isolated Node journal fixtures for Hub integration tests.

use std::path::Path;

use crate::Result;

pub fn operation_count(path: &Path, method: &str) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(
        db.query_row("SELECT COUNT(*) FROM ops WHERE method=?", [method], |r| {
            r.get(0)
        })?,
    )
}

pub fn stream_heads(path: &Path) -> Result<Vec<(String, String, i64, i64)>> {
    let db = super::inspect_database(path)?;
    let mut q = db.prepare("SELECT stream, epoch, produced, acknowledged FROM streams")?;
    Ok(
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?,
    )
}

#[derive(Clone, Copy)]
pub enum Failure {
    Start,
    Process,
    Text,
    Prepare,
}

pub fn set_failure(path: &Path, failure: Failure, enabled: bool) -> Result<()> {
    let sql = match (failure, enabled) {
        (Failure::Start, true) => {
            "CREATE TRIGGER fail_accept BEFORE INSERT ON ops WHEN NEW.method='start_turn' BEGIN SELECT RAISE(ABORT,'injected disk failure'); END"
        }
        (Failure::Start, false) => "DROP TRIGGER fail_accept",
        (Failure::Process, true) => {
            "CREATE TRIGGER fail_proc BEFORE INSERT ON procs BEGIN SELECT RAISE(ABORT,'registration failure'); END"
        }
        (Failure::Process, false) => "DROP TRIGGER fail_proc",
        (Failure::Text, true) => {
            "CREATE TRIGGER fail_ev BEFORE INSERT ON events WHEN json_extract(NEW.event,'$.body.type')='text_delta' BEGIN SELECT RAISE(ABORT,'injected disk failure'); END"
        }
        (Failure::Text, false) => "DROP TRIGGER fail_ev",
        (Failure::Prepare, true) => {
            "CREATE TRIGGER fail_prepare BEFORE UPDATE ON ops WHEN NEW.method='prepare_workspace' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'injected crash before the final receipt'); END"
        }
        (Failure::Prepare, false) => "DROP TRIGGER fail_prepare",
    };
    super::api::writable(path)?.execute_batch(sql)?;
    Ok(())
}

pub fn set_hub_inbox_failure(path: &Path, enabled: bool) -> Result<()> {
    let db = super::api::writable(path)?;
    db.execute_batch(if enabled { "CREATE TRIGGER fail_in BEFORE UPDATE OF applied ON node_streams WHEN NEW.session_id IS NOT NULL BEGIN SELECT RAISE(ABORT,'injected hub failure'); END" } else { "DROP TRIGGER fail_in" })?;
    Ok(())
}

pub fn retained_events(path: &Path) -> Result<i64> {
    let db = super::inspect_database(path)?;
    Ok(db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)
}

pub fn tamper_text(path: &Path) -> Result<usize> {
    Ok(super::api::writable(path)?.execute("UPDATE events SET event=json_set(event,'$.body.text','tampered') WHERE json_extract(event,'$.body.type')='text_delta'", [])?)
}

pub fn seed_historical_streams(path: &Path, count: usize) -> Result<()> {
    let mut db = super::api::writable(path)?;
    let tx = db.transaction()?;
    for i in 0..count {
        tx.execute(
            "INSERT INTO streams(stream,epoch,produced,acknowledged) VALUES (?1,'epoch01',1,1)",
            [format!("stream_{i:04}_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn process_group(path: &Path, session: &str) -> Result<i32> {
    let db = super::inspect_database(path)?;
    Ok(
        db.query_row("SELECT pgid FROM procs WHERE session=?", [session], |r| {
            r.get(0)
        })?,
    )
}

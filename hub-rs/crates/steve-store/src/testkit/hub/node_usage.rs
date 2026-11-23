//! Usage projections for gateway and research tests.

use serde_json::{Value, json};

use crate::{Result, Store, StoreError};

pub fn gateway_rows(store: &Store) -> Result<Vec<Value>> {
    store
        .sqlite_read(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id,source,purpose,harness,model,input,output,cache_read,cache_write,reasoning,cost_reported,ok,attempts,detail FROM usage_records ORDER BY id",
            )?;
            let v = st
                .query_map([], |r| {
                    let mut o = vec![];
                    for i in 0..14 {
                        o.push(match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(n) => json!(n),
                            rusqlite::types::ValueRef::Real(f) => json!(f),
                            rusqlite::types::ValueRef::Text(t) => {
                                json!(String::from_utf8_lossy(t))
                            }
                            rusqlite::types::ValueRef::Blob(_) => Value::Null,
                        });
                    }
                    Ok(Value::Array(o))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, StoreError>(v)
        })
}

pub fn clear(store: &Store) -> Result<()> {
    store.write::<_, StoreError>(|tx| {
        tx.conn.execute("DELETE FROM usage_records", [])?;
        Ok(())
    })
}

pub fn research_rows(store: &Store) -> Result<Vec<Value>> {
    store
        .sqlite_read(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id,purpose,harness,model,input,output,cache_read,cache_write,reasoning,cost_reported,ok,attempts,detail FROM usage_records WHERE source='gateway' ORDER BY id",
            )?;
            let v = st
                .query_map([], |r| {
                    let mut o = vec![];
                    for i in 0..13 {
                        o.push(match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(n) => json!(n),
                            rusqlite::types::ValueRef::Real(f) => json!(f),
                            rusqlite::types::ValueRef::Text(t) => {
                                json!(String::from_utf8_lossy(t))
                            }
                            rusqlite::types::ValueRef::Blob(_) => Value::Null,
                        });
                    }
                    Ok(Value::Array(o))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, StoreError>(v)
        })
}

pub fn refuse_command_apply(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch("CREATE TRIGGER fail_commit BEFORE UPDATE OF applied ON node_streams BEGIN SELECT RAISE(ABORT,'injected applied rollback'); END")?;
        Ok(())
    })
}

pub fn fail_usage_projection(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON usage_records BEGIN SELECT RAISE(ABORT,'local projection fault'); END")?;
        Ok(())
    })
}

pub fn token_report_totals(store: &Store) -> Result<(i64, i64)> {
    store.sqlite_read(|db| Ok((
        db.query_row("SELECT COUNT(*) FROM gateway_usage_reports WHERE json_extract(payload,'$.tokens.input') IS NOT NULL", [], |r| r.get(0))?,
        db.query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))?,
    )))
}

pub fn has_cancelled_job(store: &Store, attempt: &str) -> Result<bool> {
    store.sqlite_read(|db| Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM node_commands WHERE target_job_attempt=? AND method='cancel' AND state='finished')",
        [attempt], |r| r.get(0),
    )?))
}

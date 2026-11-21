//! Claim/plan failures are terminal for their row, not infrastructure failures of the source.
use rusqlite::OptionalExtension;
use steve_store::{Store, Tx};

use crate::{FlowError, FlowResult};

/// These transient SQLite locators are never persisted. Business fields are loaded inside the
/// savepoint, so a malformed field cannot abort decoding the rest of the batch.
pub(crate) struct Key {
    table: &'static str,
    subject: &'static str,
    rowid: i64,
}
impl Key {
    pub fn new(table: &'static str, subject: &'static str, rowid: i64) -> Self {
        Self {
            table,
            subject,
            rowid,
        }
    }
}

/// Local claim/plan code has no external effects to retry. Roll it back with a real Store
/// savepoint, then use the domain's existing terminal representation. Store savepoints also
/// discard staged events and diagnostics, unlike hand-written SQL-only savepoints.
/// Failure to persist that terminal state is a storage failure and reaches the source's backoff.
pub(crate) fn row<T>(
    store: &Store,
    key: &Key,
    now: i64,
    attempt: impl FnOnce(&Tx) -> FlowResult<Option<T>>,
    terminal: impl FnOnce(&Tx, &FlowError) -> FlowResult<Option<T>>,
) -> FlowResult<Option<T>> {
    store.transaction(|tx| {
        match store.transaction(attempt) {
            Ok(value)=>Ok(value),
            Err(error)=>{
        let scope=tx.query_row(&format!("SELECT workspace_id,{} FROM {} WHERE rowid=?",key.subject,key.table),[key.rowid],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional();

                let value=store.transaction(|tx|terminal(tx,&error))?;
                tracing::error!(table=key.table,rowid=key.rowid,error=%error,"flow row claim failed; terminal");
                if let Ok(Some((ws,subject)))=scope {
                    // An unavailable audit sink must not resurrect the already isolated row.
                    if let Err(audit)=store.transaction(|tx| {
                        steve_store::base::audit(tx,&ws,"steve","flow.claim_failed",&subject,&serde_json::json!({"table":key.table,"rowid":key.rowid,"error":error.to_string(),"at":now}))?;
                        Ok::<_,FlowError>(())
                    }) {tracing::error!(%audit,"recording terminal flow claim diagnostic failed");}
                }
                Ok(value)
            }
        }
    })
}

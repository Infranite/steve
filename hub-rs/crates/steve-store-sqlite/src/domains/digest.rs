//! Verification implementation of the digest operation; production tables are registered separately.
use super::*;
use rusqlite::params;
use steve_storage_api::ops::comms::digest::{EntryPoint, Source, SyncPendingMembership};
pub fn apply(c: &Connection, request: &SyncPendingMembership, rev: Rev) -> Result<(), CommitError> {
    if request.entry == EntryPoint::DequeueOnReceipt {
        let (conv, id) = request.receipt_identity().map_err(CommitError::Fatal)?;
        let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM verify_digest_receipt WHERE workspace_id=? AND conv=? AND message_id=?)",params![request.workspace_id.as_str(),conv,id],|r|r.get(0)).map_err(sql_error)?;
        if !exists {
            return Err(invalid("receipt must exist in the work unit"));
        }
        c.execute(
            "DELETE FROM verify_digest_pending WHERE workspace_id=? AND conv=? AND message_id=?",
            params![request.workspace_id.as_str(), conv, id],
        )
        .map_err(sql_error)?;
    } else if request.entry == EntryPoint::EnqueueDigestible {
        let (source, prefix) = match request.source {
            Source::ChanMessage => ("chan_message", "space:"),
            Source::MailMessage => ("mail_message", "mail_thread:"),
            _ => return Err(invalid("receipt is not a message")),
        };
        let desired:Option<(String,String,i64)>=c.query_row("SELECT workspace_id,conv,at FROM verify_digest_source s WHERE source=? AND id=? AND workspace_id=? AND substr(conv,1,length(?))=? AND status IN ('received','sent') AND NOT EXISTS(SELECT 1 FROM verify_digest_receipt r WHERE r.workspace_id=s.workspace_id AND r.conv=s.conv AND r.message_id=s.id)",params![source,request.id,request.workspace_id.as_str(),prefix,prefix],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql_error)?;
        let (ws, conv, at) = desired.clone().unwrap_or_default();
        c.execute("DELETE FROM verify_digest_pending WHERE message_id=? AND substr(conv,1,length(?))=? AND NOT(workspace_id=? AND conv=? AND at=?)",params![request.id,prefix,prefix,ws,conv,at]).map_err(sql_error)?;
        if desired.is_some() {
            let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM verify_digest_pending WHERE workspace_id=? AND conv=? AND message_id=?)",params![ws,conv,request.id],|r|r.get(0)).map_err(sql_error)?;
            if !exists {
                let next = next_sequence(c, Domain::Comms, "seq:verify_digest_pending")?;
                c.execute("INSERT INTO verify_digest_pending(workspace_id,conv,message_id,at,position,rev) VALUES(?,?,?,?,?,?)",params![ws,conv,request.id,at,next,rev.0 as i64]).map_err(sql_error)?;
            }
        }
    }
    c.execute("DELETE FROM verify_digest_convs", [])
        .map_err(sql_error)?;
    c.execute("INSERT INTO verify_digest_convs(workspace_id,conv,messages,oldest,newest,rev) SELECT workspace_id,conv,count(*),min(at),max(at),? FROM verify_digest_pending GROUP BY workspace_id,conv",[rev.0 as i64]).map_err(sql_error)?;
    Ok(())
}

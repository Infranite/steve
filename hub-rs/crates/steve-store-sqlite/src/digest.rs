//! Explicit equivalents of migration 112's inbox triggers. Every mutation runs inside
//! the source writer's transaction; no model, runtime, facade or second connection is needed.
use rusqlite::{Connection, OptionalExtension, Result, params};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    ChanMessage,
    MailMessage,
}
impl Source {
    fn shape(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::ChanMessage => ("chan_messages", "space_id", "created", "space:"),
            Self::MailMessage => ("mail_messages", "thread_id", "date", "mail_thread:"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scope {
    pub workspace: String,
    pub conv: String,
}
fn transaction(c: &Connection) -> Result<()> {
    if c.is_autocommit() {
        Err(rusqlite::Error::InvalidQuery)
    } else {
        Ok(())
    }
}
pub fn message_scope(c: &Connection, source: Source, id: &str) -> Result<Option<Scope>> {
    let (table, conv, _, prefix) = source.shape();
    c.query_row(
        &format!("SELECT workspace_id,?1 || {conv} FROM {table} WHERE id=?2"),
        params![prefix, id],
        |r| {
            Ok(Scope {
                workspace: r.get(0)?,
                conv: r.get(1)?,
            })
        },
    )
    .optional()
}
/// Recompute only the conversation projection. This never re-enqueues acknowledged messages.
pub fn sync_pending_conversation(c: &Connection, workspace: &str, conv: &str) -> Result<()> {
    transaction(c)?;
    c.execute(
        "INSERT INTO digest_pending_convs(workspace_id,conv,messages,oldest,newest)
         SELECT ?1,?2,count(*),min(at),max(at) FROM digest_pending
         WHERE workspace_id=?1 AND conv=?2 HAVING count(*)>0
         ON CONFLICT(workspace_id,conv) DO UPDATE SET
           messages=excluded.messages,oldest=excluded.oldest,newest=excluded.newest",
        params![workspace, conv],
    )?;
    c.execute(
        "DELETE FROM digest_pending_convs WHERE workspace_id=?1 AND conv=?2 AND NOT EXISTS
        (SELECT 1 FROM digest_pending WHERE workspace_id=?1 AND conv=?2)",
        params![workspace, conv],
    )?;
    Ok(())
}
/// INSERT OR IGNORE preserves an existing FIFO position, including when intake is repeated.
pub fn enqueue_digestible(c: &Connection, source: Source, id: &str) -> Result<()> {
    transaction(c)?;
    let (table, conv, at, prefix) = source.shape();
    let added = c.execute(
        &format!(
            "INSERT OR IGNORE INTO digest_pending(workspace_id,conv,message_id,at)
             SELECT m.workspace_id,?1 || m.{conv},m.id,m.{at} FROM {table} m
             WHERE m.id=?2 AND m.status IN ('received','sent')
               AND NOT EXISTS(SELECT 1 FROM digest_receipts r
                 WHERE r.workspace_id=m.workspace_id AND r.conv=?1 || m.{conv}
                   AND r.message_id=m.id)"
        ),
        params![prefix, id],
    )?;
    if added != 0 {
        let scope =
            message_scope(c, source, id)?.expect("inserted source exists in the same transaction");
        sync_pending_conversation(c, &scope.workspace, &scope.conv)?;
    }
    Ok(())
}
pub fn dequeue_on_receipt(
    c: &Connection,
    workspace: &str,
    conv: &str,
    message_id: &str,
) -> Result<()> {
    transaction(c)?;
    c.execute(
        "DELETE FROM digest_pending WHERE workspace_id=? AND conv=? AND message_id=?",
        params![workspace, conv, message_id],
    )?;
    sync_pending_conversation(c, workspace, conv)
}
/// Can run after DELETE: pending rows retain their old scope. Source kind separates channel
/// and mail IDs, even when both happen to use the same message ID.
pub fn remove_message(c: &Connection, source: Source, id: &str) -> Result<()> {
    transaction(c)?;
    let prefix = source.shape().3;
    let scopes = c
        .prepare_cached(
            "SELECT workspace_id,conv FROM digest_pending
             WHERE message_id=?1 AND substr(conv,1,length(?2))=?2",
        )?
        .query_map(params![id, prefix], |r| {
            Ok(Scope {
                workspace: r.get(0)?,
                conv: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    for scope in scopes {
        dequeue_on_receipt(c, &scope.workspace, &scope.conv, id)?;
    }
    Ok(())
}
/// Call only when a source UPDATE actually matched. Like 112's UPDATE OF trigger, this
/// removes the old membership before reinsertion, even for a same-value qualifying UPDATE.
/// Pass the captured scope for moves; None recovers it from the pending row after the write.
pub fn sync_message(
    c: &Connection,
    source: Source,
    id: &str,
    old_scope: Option<&Scope>,
) -> Result<()> {
    transaction(c)?;
    if let Some(scope) = old_scope {
        dequeue_on_receipt(c, &scope.workspace, &scope.conv, id)?;
    } else {
        remove_message(c, source, id)?;
    }
    enqueue_digestible(c, source, id)
}
/// Initial digest lookback intentionally removes old work without creating receipts.
pub fn remove_before(c: &Connection, workspace: &str, conv: &str, floor: i64) -> Result<()> {
    transaction(c)?;
    c.execute(
        "DELETE FROM digest_pending WHERE workspace_id=? AND conv=? AND at<?",
        params![workspace, conv, floor],
    )?;
    sync_pending_conversation(c, workspace, conv)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub kind: &'static str,
    pub conv: String,
    pub message_id: Option<String>,
}
type Key = (String, String);
type Stats = (i64, Option<i64>, Option<i64>);
/// Read-only audit. Migration 112's initial `since` exclusion is honored only for missing
/// historical rows; queued later backfill remains eligible regardless of provider time.
pub fn verify_pending(c: &Connection, workspace: &str) -> Result<Vec<Mismatch>> {
    let actual: BTreeMap<Key, i64> = c
        .prepare_cached("SELECT conv,message_id,at FROM digest_pending WHERE workspace_id=?")?
        .query_map([workspace], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<Result<_>>()?;
    let expected: BTreeMap<Key, i64> = c
        .prepare_cached(
            "WITH source AS (
               SELECT 'space:' || space_id AS conv,id AS message_id,created AS at
               FROM chan_messages WHERE workspace_id=?1 AND status IN ('received','sent')
               UNION ALL SELECT 'mail_thread:' || thread_id,id,date
               FROM mail_messages WHERE workspace_id=?1 AND status IN ('received','sent'))
             SELECT s.conv,s.message_id,s.at FROM source s
             WHERE NOT EXISTS(SELECT 1 FROM digest_receipts r
               WHERE r.workspace_id=?1 AND r.conv=s.conv AND r.message_id=s.message_id)
             AND (EXISTS(SELECT 1 FROM digest_pending p
                    WHERE p.workspace_id=?1 AND p.conv=s.conv AND p.message_id=s.message_id)
               OR NOT EXISTS(SELECT 1 FROM digest_cursors k
                    WHERE k.workspace_id=?1 AND k.conv=s.conv AND s.at<k.since))",
        )?
        .query_map([workspace], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<Result<_>>()?;
    let mut mismatches = Vec::new();
    for key in actual
        .keys()
        .chain(expected.keys())
        .collect::<BTreeSet<_>>()
    {
        let kind = match (actual.get(key), expected.get(key)) {
            (None, Some(_)) => Some("missing"),
            (Some(_), None) => Some("unexpected"),
            (Some(a), Some(b)) if a != b => Some("timestamp"),
            _ => None,
        };
        if let Some(kind) = kind {
            mismatches.push(Mismatch {
                kind,
                conv: key.0.clone(),
                message_id: Some(key.1.clone()),
            });
        }
    }
    let mut stats: BTreeMap<String, Stats> = BTreeMap::new();
    for ((conv, _), at) in &actual {
        let entry = stats
            .entry(conv.clone())
            .or_insert((0, Some(*at), Some(*at)));
        entry.0 += 1;
        entry.1 = Some(entry.1.unwrap().min(*at));
        entry.2 = Some(entry.2.unwrap().max(*at));
    }
    let stored: BTreeMap<String, Stats> = c
        .prepare_cached(
            "SELECT conv,messages,oldest,newest FROM digest_pending_convs WHERE workspace_id=?",
        )?
        .query_map([workspace], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })?
        .collect::<Result<_>>()?;
    for conv in stats.keys().chain(stored.keys()).collect::<BTreeSet<_>>() {
        if stats.get(conv) != stored.get(conv) {
            mismatches.push(Mismatch {
                kind: "conversation",
                conv: conv.clone(),
                message_id: None,
            });
        }
    }
    Ok(mismatches)
}

/// Preserve the SQL row count and no-match behavior at status-only UPDATE call sites.
/// Scope-changing UPDATEs must capture their old Scope and call sync_message directly.
pub fn status_changed(c: &Connection, source: Source, id: &str, matched: usize) -> Result<usize> {
    if matched != 0 {
        let scope = message_scope(c, source, id)?;
        sync_message(c, source, id, scope.as_ref())?;
    }
    Ok(matched)
}

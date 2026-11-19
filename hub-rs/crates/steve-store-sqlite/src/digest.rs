//! Transactional digest membership maintenance. Callers own the source transaction.
use rusqlite::{Connection, OptionalExtension, Result, params};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    ChanMessage,
    MailMessage,
}
/// Closed query shapes: values are bound parameters, never interpolated SQL.
#[derive(Clone, Copy, Debug)]
pub enum MessageFilter<'a> {
    All,
    Id(&'a str),
    IdsJson(&'a str),
    Connection(&'a str),
}
struct MessageShape {
    table: &'static str,
    conv_column: &'static str,
    time_column: &'static str,
    prefix: &'static str,
}
impl Source {
    /// Read source rows using the shared message shape and a fixed query predicate.
    pub fn messages(self, c: &Connection, filter: MessageFilter<'_>) -> Result<Vec<Message>> {
        let (predicate, value) = match filter {
            MessageFilter::All => ("1", None),
            MessageFilter::Id(id) => ("id=?1", Some(id)),
            MessageFilter::IdsJson(ids) => ("id IN (SELECT value FROM json_each(?1))", Some(ids)),
            MessageFilter::Connection(id) => ("connection_id=?1", Some(id)),
        };
        c.prepare_cached(&format!("{} WHERE {predicate}", self.select_sql()))?
            .query_map(rusqlite::params_from_iter(value), |r| {
                Ok(Message {
                    scope: scope_row(r)?,
                    id: r.get(2)?,
                    at: r.get(3)?,
                    status: r.get(4)?,
                })
            })?
            .collect()
    }
    pub fn scope(self, c: &Connection, id: &str) -> Result<Option<Scope>> {
        Ok(self
            .messages(c, MessageFilter::Id(id))?
            .into_iter()
            .next()
            .map(|m| m.scope))
    }
    fn select_sql(self) -> String {
        let MessageShape {
            table,
            conv_column,
            time_column,
            prefix,
        } = self.shape();
        format!(
            "SELECT workspace_id,'{prefix}' || {conv_column} AS conv,id AS message_id,{time_column} AS at,status FROM {table}"
        )
    }
    fn shape(self) -> MessageShape {
        match self {
            Self::ChanMessage => MessageShape {
                table: "chan_messages",
                conv_column: "space_id",
                time_column: "created",
                prefix: "space:",
            },
            Self::MailMessage => MessageShape {
                table: "mail_messages",
                conv_column: "thread_id",
                time_column: "date",
                prefix: "mail_thread:",
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Scope {
    pub workspace: String,
    pub conv: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub scope: Scope,
    pub status: String,
    pub at: i64,
}
fn transaction(c: &Connection) -> Result<()> {
    if c.is_autocommit() {
        Err(rusqlite::Error::InvalidQuery)
    } else {
        Ok(())
    }
}
fn scope_row(r: &rusqlite::Row<'_>) -> Result<Scope> {
    Ok(Scope {
        workspace: r.get(0)?,
        conv: r.get(1)?,
    })
}
// Source rows are normalized to these columns. Both intake and audits use this one predicate.
const ELIGIBLE: &str = "m.status IN ('received','sent') AND NOT EXISTS(
    SELECT 1 FROM digest_receipts r WHERE r.workspace_id=m.workspace_id
    AND r.conv=m.conv AND r.message_id=m.message_id)";
const PROJECTION_SELECT: &str = "SELECT workspace_id,conv,count(*) AS messages,min(at) AS oldest,max(at) AS newest FROM digest_pending";
fn sync_pending_conversation(c: &Connection, scope: &Scope) -> Result<()> {
    c.execute(
        &format!(
            "INSERT INTO digest_pending_convs(workspace_id,conv,messages,oldest,newest)
        {PROJECTION_SELECT} WHERE workspace_id=?1 AND conv=?2 GROUP BY workspace_id,conv
        ON CONFLICT(workspace_id,conv) DO UPDATE SET
        messages=excluded.messages,oldest=excluded.oldest,newest=excluded.newest"
        ),
        params![scope.workspace, scope.conv],
    )?;
    c.execute(
        "DELETE FROM digest_pending_convs WHERE workspace_id=?1 AND conv=?2 AND NOT EXISTS
        (SELECT 1 FROM digest_pending WHERE workspace_id=?1 AND conv=?2)",
        params![scope.workspace, scope.conv],
    )?;
    Ok(())
}
fn delete_membership(c: &Connection, scope: &Scope, id: &str) -> Result<()> {
    c.execute(
        "DELETE FROM digest_pending WHERE workspace_id=?1 AND conv=?2 AND message_id=?3",
        params![scope.workspace, scope.conv, id],
    )?;
    Ok(())
}
/// Call after INSERT or a matched relevant UPDATE, passing the scope captured before UPDATE.
/// None means an insertion. Eligible rows retain FIFO positions on repeated/same-value writes.
/// Recount after all membership changes so surviving conversations keep additive columns.
pub fn sync_message(
    c: &Connection,
    source: Source,
    id: &str,
    old_scope: Option<&Scope>,
) -> Result<()> {
    sync_message_scopes(c, source, id, old_scope.into_iter().cloned())
}
/// Contract adapters recover pre-change scopes from pending, even after source deletion.
pub(crate) fn pending_scopes(c: &Connection, source: Source, id: &str) -> Result<Vec<Scope>> {
    let prefix = source.shape().prefix;
    c.prepare_cached(
        "SELECT DISTINCT workspace_id,conv FROM digest_pending
        WHERE message_id=?1 AND substr(conv,1,length(?2))=?2 ORDER BY workspace_id,conv",
    )?
    .query_map(params![id, prefix], scope_row)?
    .collect()
}
/// Same implementation as legacy writes, accepting all recovered old scopes without
/// deleting/reinserting a surviving membership or resetting conversation extension fields.
pub(crate) fn sync_message_scopes(
    c: &Connection,
    source: Source,
    id: &str,
    old_scopes: impl IntoIterator<Item = Scope>,
) -> Result<()> {
    transaction(c)?;
    let source_sql = source.select_sql();
    let added = c
        .query_row(
            &format!(
                "WITH source AS ({source_sql} WHERE id=?1)
        INSERT INTO digest_pending(workspace_id,conv,message_id,at)
        SELECT m.workspace_id,m.conv,m.message_id,m.at FROM source m WHERE {ELIGIBLE}
        ON CONFLICT(workspace_id,conv,message_id) DO UPDATE SET at=excluded.at
        RETURNING workspace_id,conv"
            ),
            [id],
            scope_row,
        )
        .optional()?;
    let mut affected = BTreeSet::new();
    // No intermediate projection cleanup: a same-scope update of the only queued message
    // must preserve retry/dead-letter state added by later migrations.
    for scope in old_scopes {
        if added.as_ref() != Some(&scope) {
            delete_membership(c, &scope, id)?;
        }
        affected.insert(scope);
    }
    if let Some(scope) = added {
        affected.insert(scope);
    }
    for scope in affected {
        sync_pending_conversation(c, &scope)?;
    }
    Ok(())
}
pub(crate) fn receipt_exists(c: &Connection, scope: &Scope, id: &str) -> Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM digest_receipts
        WHERE workspace_id=?1 AND conv=?2 AND message_id=?3)",
        params![scope.workspace, scope.conv, id],
        |r| r.get(0),
    )
}
/// Reconcile only identified conversations, preserving additive fields on surviving rows.
pub(crate) fn reconcile_conversations(
    c: &Connection,
    scopes: impl IntoIterator<Item = Scope>,
) -> Result<()> {
    transaction(c)?;
    for scope in scopes.into_iter().collect::<BTreeSet<_>>() {
        sync_pending_conversation(c, &scope)?;
    }
    Ok(())
}
/// Exact acknowledgement; never re-enqueues the source message.
pub fn dequeue_on_receipt(
    c: &Connection,
    workspace: &str,
    conv: &str,
    message_ids: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<()> {
    let scope = Scope {
        workspace: workspace.into(),
        conv: conv.into(),
    };
    remove_message(
        c,
        message_ids
            .into_iter()
            .map(|id| (scope.clone(), id.as_ref().to_owned())),
    )
}
/// Capture source scopes before DELETE, then remove in one batch. Each affected conversation
/// is aggregated once, independent of the number of deleted messages.
pub fn remove_message(
    c: &Connection,
    messages: impl IntoIterator<Item = (Scope, String)>,
) -> Result<()> {
    transaction(c)?;
    let mut affected = BTreeSet::new();
    for (scope, id) in messages {
        delete_membership(c, &scope, &id)?;
        affected.insert(scope);
    }
    for scope in affected {
        sync_pending_conversation(c, &scope)?;
    }
    Ok(())
}
/// Initial lookback removes historical work without creating receipts; aggregate once.
pub fn remove_before(c: &Connection, workspace: &str, conv: &str, floor: i64) -> Result<()> {
    transaction(c)?;
    c.execute(
        "DELETE FROM digest_pending WHERE workspace_id=?1 AND conv=?2 AND at<?3",
        params![workspace, conv, floor],
    )?;
    sync_pending_conversation(
        c,
        &Scope {
            workspace: workspace.into(),
            conv: conv.into(),
        },
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MismatchKind {
    Missing,
    Unexpected,
    Timestamp,
    Conversation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub kind: MismatchKind,
    pub conv: String,
    pub message_id: Option<String>,
}
type Key = (String, String);
fn membership_mismatches(c: &Connection, workspace: &str) -> Result<Vec<Mismatch>> {
    let actual: BTreeMap<Key, i64> = c
        .prepare_cached("SELECT conv,message_id,at FROM digest_pending WHERE workspace_id=?")?
        .query_map([workspace], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<Result<_>>()?;
    let expected: BTreeMap<Key, i64> = c
        .prepare_cached(&format!(
            "WITH source AS (
           {} WHERE workspace_id=?1 UNION ALL {} WHERE workspace_id=?1)
         SELECT m.conv,m.message_id,m.at FROM source m WHERE {ELIGIBLE}
           AND (EXISTS(SELECT 1 FROM digest_pending p
                  WHERE p.workspace_id=?1 AND p.conv=m.conv AND p.message_id=m.message_id)
             OR NOT EXISTS(SELECT 1 FROM digest_cursors k
                  WHERE k.workspace_id=?1 AND k.conv=m.conv AND m.at<k.since))",
            Source::ChanMessage.select_sql(),
            Source::MailMessage.select_sql()
        ))?
        .query_map([workspace], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<Result<_>>()?;
    let mut mismatches = Vec::new();
    for key in actual
        .keys()
        .chain(expected.keys())
        .collect::<BTreeSet<_>>()
    {
        let kind = match (actual.get(key), expected.get(key)) {
            (None, Some(_)) => Some(MismatchKind::Missing),
            (Some(_), None) => Some(MismatchKind::Unexpected),
            (Some(a), Some(b)) if a != b => Some(MismatchKind::Timestamp),
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
    Ok(mismatches)
}
#[derive(Debug, PartialEq, Eq)]
struct Projection {
    messages: i64,
    oldest: Option<i64>,
    newest: Option<i64>,
}
fn projections(c: &Connection, sql: &str, workspace: &str) -> Result<BTreeMap<String, Projection>> {
    c.prepare_cached(sql)?
        .query_map([workspace], |r| {
            Ok((
                r.get(0)?,
                Projection {
                    messages: r.get(1)?,
                    oldest: r.get(2)?,
                    newest: r.get(3)?,
                },
            ))
        })?
        .collect()
}
fn projection_mismatches(c: &Connection, workspace: &str) -> Result<Vec<Mismatch>> {
    let expected = projections(
        c,
        &format!(
            "SELECT conv,messages,oldest,newest FROM ({PROJECTION_SELECT} WHERE workspace_id=?1 GROUP BY workspace_id,conv)"
        ),
        workspace,
    )?;
    let stored = projections(
        c,
        "SELECT conv,messages,oldest,newest FROM digest_pending_convs WHERE workspace_id=?1",
        workspace,
    )?;
    Ok(expected
        .keys()
        .chain(stored.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|conv| expected.get(*conv) != stored.get(*conv))
        .map(|conv| Mismatch {
            kind: MismatchKind::Conversation,
            conv: conv.clone(),
            message_id: None,
        })
        .collect())
}
/// Read-only audit. Missing historical rows below initial since cannot be distinguished from
/// excluded history; existing queued backfill is eligible regardless of provider time.
pub fn verify_pending(c: &Connection, workspace: &str) -> Result<Vec<Mismatch>> {
    let mut mismatches = membership_mismatches(c, workspace)?;
    mismatches.extend(projection_mismatches(c, workspace)?);
    Ok(mismatches)
}

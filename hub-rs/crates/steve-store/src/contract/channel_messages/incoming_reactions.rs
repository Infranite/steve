//! Storage contract for incoming channel reactions (including removal tombstones) and durable
//! reply-to-source linkages for channels without platform quote ids.

use sea_query::{Expr, ExprTrait, OnConflict, Query, Values};

use crate::{Read, Result, W};

/// One active visible incoming reaction on a channel message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReaction {
    pub actor_account_id: String,
    pub actor_party_id: Option<String>,
    pub actor_name: String,
    pub emoji: String,
    pub origin: String,
    pub at: i64,
    pub time_source: String,
    pub revision: Option<i64>,
}

/// Parameters for storing an incoming reaction or removal tombstone.
pub struct UpsertReaction<'a> {
    pub ws: &'a str,
    pub conn: &'a str,
    pub space_ext_id: &'a str,
    pub message_ext_id: &'a str,
    pub message_source_id: &'a str,
    pub message_id: Option<&'a str>,
    pub actor_account_id: &'a str,
    pub emoji: &'a str,
    pub origin: &'a str,
    pub removed: bool,
    pub at: i64,
    pub time_source: &'a str,
    pub revision: Option<i64>,
}

/// Fold producer revisions within each receiving connection, then keep the latest visible state per
/// `(actor_account_id, emoji)` across connections. Unversioned changes use timestamps, with removal
/// winning ties. Visibility (`mask`, `-1` for full internal view) is applied before folding so a
/// private identity cannot disclose events to another member through a shared message.
pub fn list_visible(r: &mut impl Read, message: &str, mask: i64) -> Result<Vec<StoredReaction>> {
    let mut rows = Vec::new();
    r.db().query(
        "WITH scoped AS (
           SELECT r.*, row_number() OVER (PARTITION BY r.connection_id, r.actor_account_id, r.emoji
              ORDER BY (r.source_revision IS NOT NULL) DESC, r.source_revision DESC,
                       r.action_at DESC, r.removed DESC, (r.origin <> 'manual') DESC) AS connection_rank
             FROM chan_message_reactions r
             JOIN chan_messages m ON m.id = r.message_id
             JOIN space_identities i ON i.space_id = m.space_id AND i.connection_id = r.connection_id
            WHERE r.message_id = ?1
              AND (?2 = -1 OR (i.ordinal IS NOT NULL AND ((1 << i.ordinal) & ?2) <> 0))),
         ranked AS (
           SELECT r.*, row_number() OVER (PARTITION BY r.actor_account_id, r.emoji
              ORDER BY r.action_at DESC, r.removed DESC, (r.origin <> 'manual') DESC) AS rank
             FROM scoped r WHERE r.connection_rank = 1)
         SELECT r.actor_account_id, a.party_id,
                coalesce(nullif(a.display_name, ''), p.name, ''),
                r.emoji, r.origin, r.action_at, r.time_source, r.source_revision
           FROM ranked r
           JOIN accounts a ON a.id = r.actor_account_id
           LEFT JOIN parties p ON p.id = a.party_id
          WHERE r.rank = 1 AND r.removed = 0
          ORDER BY r.action_at, r.actor_account_id, r.emoji",
        &Values(vec![message.into(), mask.into()]),
        &mut |row| {
            rows.push(StoredReaction {
                actor_account_id: row.get(0)?,
                actor_party_id: row.get(1)?,
                actor_name: row.get(2)?,
                emoji: row.get(3)?,
                origin: row.get(4)?,
                at: row.get(5)?,
                time_source: row.get(6)?,
                revision: row.get(7)?,
            });
            Ok(())
        },
    )?;
    Ok(rows)
}

/// Resolve a reaction's target message among messages seen by `conn`. Returns `Some(id)` only when
/// the target identifies a unique stored message.
pub fn resolve_target(
    r: &mut impl Read,
    ws: &str,
    conn: &str,
    space_ext_id: &str,
    message_ext_id: &str,
    message_source_id: &str,
) -> Result<Option<String>> {
    let mut ids = Vec::new();
    r.db().query(
        "SELECT DISTINCT m.id
           FROM chan_messages m
           JOIN spaces s ON s.id = m.space_id
           JOIN space_identities i ON i.space_id = s.id AND i.connection_id = ?2
          WHERE m.workspace_id = ?1
            AND (?3 = '' OR s.external_id = ?3)
            AND (?4 = '' OR m.external_id = ?4)
            AND (?5 = '' OR EXISTS (
                SELECT 1 FROM chan_message_seen x
                 WHERE x.message_id = m.id AND x.connection_id = ?2 AND x.source_id = ?5))
          LIMIT 2",
        &Values(vec![
            ws.into(),
            conn.into(),
            space_ext_id.into(),
            message_ext_id.into(),
            message_source_id.into(),
        ]),
        &mut |row| {
            ids.push(row.get::<String>(0)?);
            Ok(())
        },
    )?;
    Ok((ids.len() == 1).then(|| ids.remove(0)))
}

/// Upsert one reaction event or removal tombstone, preserving monotonic producer ordering:
/// versioned updates beat unversioned ones and advance only on strictly newer revisions;
/// unversioned updates advance by `action_at`, with removal winning timestamp ties.
pub fn upsert(w: &mut W<'_>, ev: &UpsertReaction<'_>) -> Result<()> {
    w.db.exec(
        "INSERT INTO chan_message_reactions (
            workspace_id, connection_id, space_external_id, message_external_id,
            message_source_id, message_id, actor_account_id, emoji, origin,
            removed, action_at, time_source, source_revision
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (
            workspace_id, connection_id, space_external_id, message_external_id,
            message_source_id, actor_account_id, emoji
         )
         DO UPDATE SET
            message_id = coalesce(excluded.message_id, chan_message_reactions.message_id),
            origin = excluded.origin,
            removed = excluded.removed,
            action_at = excluded.action_at,
            time_source = excluded.time_source,
            source_revision = excluded.source_revision
         WHERE (
                excluded.source_revision IS NOT NULL
                AND (
                    chan_message_reactions.source_revision IS NULL
                    OR excluded.source_revision > chan_message_reactions.source_revision
                )
            )
            OR (
                excluded.source_revision IS NULL
                AND chan_message_reactions.source_revision IS NULL
                AND (
                    excluded.action_at > chan_message_reactions.action_at
                    OR (
                        excluded.action_at = chan_message_reactions.action_at
                        AND (
                            excluded.removed > chan_message_reactions.removed
                            OR (
                                excluded.removed = chan_message_reactions.removed
                                AND excluded.origin <> 'manual'
                            )
                        )
                    )
                )
            )",
        &Values(vec![
            ev.ws.into(),
            ev.conn.into(),
            ev.space_ext_id.into(),
            ev.message_ext_id.into(),
            ev.message_source_id.into(),
            ev.message_id.into(),
            ev.actor_account_id.into(),
            ev.emoji.into(),
            ev.origin.into(),
            i64::from(ev.removed).into(),
            ev.at.into(),
            ev.time_source.into(),
            ev.revision.into(),
        ]),
    )?;
    Ok(())
}

/// Attach pending out-of-order reactions that arrived before `message_id` was stored.
pub fn attach_for_message(w: &mut W<'_>, ws: &str, conn: &str, message_id: &str) -> Result<u64> {
    w.db.exec(
        "UPDATE chan_message_reactions AS r
            SET message_id = ?3
          WHERE r.workspace_id = ?1
            AND r.connection_id = ?2
            AND r.message_id IS NULL
            AND EXISTS (
                SELECT 1
                  FROM chan_messages m
                  JOIN spaces s ON s.id = m.space_id
                 WHERE m.id = ?3
                   AND (r.space_external_id = '' OR r.space_external_id = s.external_id)
                   AND (r.message_external_id = '' OR r.message_external_id = m.external_id)
                   AND (r.message_source_id = '' OR EXISTS (
                        SELECT 1 FROM chan_message_seen x
                         WHERE x.message_id = m.id
                           AND x.connection_id = ?2
                           AND x.source_id = r.message_source_id))
            )",
        &Values(vec![ws.into(), conn.into(), message_id.into()]),
    )
}

/// Distinct inbound message ids with stored reactions that could cover `target` (exact message in
/// groups; `target` or same-space messages in `[from, to]` for DM burst scope).
pub fn reacted_candidates(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    target: &str,
    dm: bool,
    from: i64,
    to: i64,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    r.db().query(
        "SELECT DISTINCT r.message_id
           FROM chan_message_reactions r
           JOIN chan_messages m ON m.id = r.message_id
          WHERE m.workspace_id = ?1
            AND m.space_id = ?2
            AND m.direction = 'in'
            AND (m.id = ?3 OR (?4 <> 0 AND m.created >= ?5 AND m.created <= ?6))",
        &Values(vec![
            ws.into(),
            space.into(),
            target.into(),
            i64::from(dm).into(),
            from.into(),
            to.into(),
        ]),
        &mut |row| {
            ids.push(row.get::<String>(0)?);
            Ok(())
        },
    )?;
    Ok(ids)
}

/// Whether `emoji` on `message_id` matches Steve's own outgoing user-account lifecycle reaction.
pub fn is_our_outgoing_reaction(r: &mut impl Read, message_id: &str, emoji: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("chan_reactions")
            .and_where(Expr::col("message_id").eq(message_id))
            .and_where(Expr::col("as_user").eq(1_i64))
            .and_where(Expr::col("emoji").eq(emoji))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

/// Persist the internal source message an outbound reply answers.
pub fn record_reply_source(
    w: &mut W<'_>,
    out_message_id: &str,
    source_message_id: &str,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("chan_reply_sources")
            .columns(["out_message_id", "source_message_id"])
            .values_panic([out_message_id.into(), source_message_id.into()])
            .on_conflict(
                OnConflict::column("out_message_id")
                    .update_column("source_message_id")
                    .to_owned(),
            ),
    )?;
    Ok(())
}

/// Copy a durable reply-source binding when a recovery draft is forked with a fresh id.
pub fn copy_reply_source(w: &mut W<'_>, from_out_id: &str, to_out_id: &str) -> Result<()> {
    if let Some(src) = reply_source_of(w, from_out_id)? {
        record_reply_source(w, to_out_id, &src)?;
    }
    Ok(())
}

/// Look up the internal inbound message id an outbound message answers.
pub fn reply_source_of(r: &mut impl Read, out_message_id: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("source_message_id")
            .from("chan_reply_sources")
            .and_where(Expr::col("out_message_id").eq(out_message_id)),
        |row| row.get(0),
    )
}

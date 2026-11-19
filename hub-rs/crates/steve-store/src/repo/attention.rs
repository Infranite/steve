//! Personal read metadata and live projections, not another notification/event queue.
//! Page capabilities pin the full projection digest: a changing source explicitly invalidates a
//! traversal. Clients consume every page before replacing their baseline, and restart on conflict.
use crate::{Result, StoreError, Tx, feed, sha, uid};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const KINDS: [&str; 4] = ["replies", "pending", "feed", "channels"];
const TTL: i64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub route: String,
    pub entity_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub event_id: String,
    pub created: i64,
    pub target: Target,
    pub source_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
    /// Safe server default for a banner; a native explicit personal conversation opt-in may
    /// promote a generic channel indicator. Never inferred from Steve's role/quiet setting.
    pub notify: bool,
}
impl Entry {
    pub fn new(id: String, created: i64, route: &str, entity: String) -> Self {
        Self {
            event_id: id.clone(),
            source_id: id.clone(),
            id,
            created,
            target: Target {
                route: route.into(),
                entity_id: entity,
            },
            group_key: None,
            order: None,
            notify: true,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Category {
    pub kind: String,
    pub count: usize,
    pub unit: &'static str,
    pub available: bool,
    pub entries: Vec<Entry>,
    pub next_cursor: Option<String>,
    /// Only a completed traversal can issue an unchanged/read capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    pub revision: String,
    pub unchanged: bool,
}
pub fn epoch(c: &Connection) -> Result<String> {
    Ok(
        c.query_row("SELECT epoch FROM attention_meta WHERE id=1", [], |r| {
            r.get(0)
        })?,
    )
}

/// Only committed terminal final receipts for this caller's own main-conversation run.
/// Sequence is assigned at the run's final commit, not messages.rowid or a wall clock.
pub fn replies(c: &Connection, ws: &str, user: &str) -> Result<(Vec<Entry>, i64)> {
    feed::member(c, ws, user)?;
    let upper = c.query_row(
        "SELECT coalesce(max(seq),0) FROM thread_reply_commits WHERE workspace_id=? AND user_id=?",
        params![ws, user],
        |r| r.get(0),
    )?;
    let mut st = c.prepare_cached("SELECT q.message_id,q.seq,m.created FROM thread_reply_commits q
      JOIN messages m ON m.id=q.message_id JOIN thread_runs r ON r.id=q.run_id
      WHERE q.workspace_id=?1 AND q.user_id=?2 AND r.workspace_id=?1 AND r.user_id=?2
        AND r.status IN ('done','stopped','budget','violated','failed') AND m.workspace_id=?1 AND m.space='steve' AND m.author='steve'
        AND q.seq>coalesce((SELECT through_seq FROM thread_reply_reads WHERE workspace_id=?1 AND user_id=?2 AND space='steve'),0)
      ORDER BY q.seq DESC")?;
    let entries = st
        .query_map(params![ws, user], |r| {
            let id: String = r.get(0)?;
            let mut e = Entry::new(id.clone(), r.get(2)?, "", id);
            e.order = Some(r.get(1)?);
            Ok(e)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((entries, upper))
}

/// Resolve a retained reply navigation identity even after it was read. This is not a general
/// message lookup: unknown, foreign, channel and uncommitted/draft IDs all fail closed.
pub fn reply(c: &Connection, ws: &str, user: &str, id: &str) -> Result<(String, i64)> {
    feed::member(c, ws, user)?;
    c.query_row(
        "SELECT m.id,m.created FROM thread_reply_commits q
      JOIN messages m ON m.id=q.message_id JOIN thread_runs r ON r.id=q.run_id
      WHERE q.message_id=?1 AND q.workspace_id=?2 AND q.user_id=?3
        AND r.workspace_id=?2 AND r.user_id=?3 AND r.status IN ('done','stopped','budget','violated','failed')
        AND m.workspace_id=?2 AND m.space='steve' AND m.author='steve'",
        params![id, ws, user],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or(StoreError::NotFound("attention reply"))
}

/// Exact pendingCards semantics: every open wait on an unsnoozed/unarchived item, plus
/// orphan waits, excluding only confirm/mem. Status is immaterial once an item has a wait.
/// This follows web/src/store.ts pending()/allCards()/pendingCards(), not openDecisions count.
pub fn pending(c: &Connection, ws: &str, user: &str, now: i64) -> Result<Vec<Entry>> {
    feed::member(c, ws, user)?;
    let mut st = c.prepare_cached("SELECT d.id,d.created FROM decisions d
       LEFT JOIN items i ON i.id=d.item_id AND i.workspace_id=d.workspace_id
       WHERE d.workspace_id=?1 AND d.status='open' AND NOT(d.kind='confirm' AND d.subject_type='mem')
         AND (i.id IS NULL OR (i.archived=0 AND NOT(
           json_valid(i.snooze) AND coalesce(json_extract(i.snooze,'$.until'),0)>?2
           AND coalesce(json_extract(i.snooze,'$.label'),'')<>'')))
       ORDER BY d.created DESC,d.id DESC")?;
    Ok(st
        .query_map(params![ws, now], |r| {
            let id: String = r.get(0)?;
            Ok(Entry::new(id.clone(), r.get(1)?, "inbox", id))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Indexed unread metadata maintained by Feed/read/membership triggers. No content is copied.
/// Scope revision/count are O(1); each keyset page loads at most limit candidates, irrespective
/// of historical/read row count. The surrounding writer transaction performs bounded work only.
#[allow(clippy::too_many_arguments)]
pub fn feed_page(
    tx: &Tx,
    ws: &str,
    user: &str,
    cursor: Option<&str>,
    if_snapshot: Option<&str>,
    limit: usize,
    now: i64,
) -> Result<Category> {
    feed::member(tx, ws, user)?;
    if !(1..=100).contains(&limit) || (cursor.is_some() && if_snapshot.is_some()) {
        return Err(StoreError::Conflict("Invalid Feed attention page".into()));
    }
    let (revision, count): (String, i64) = tx.query_row(
        "SELECT revision,unread_count FROM attention_feed_scopes WHERE workspace_id=? AND user_id=?",
        params![ws,user], |r| Ok((r.get(0)?,r.get(1)?)),
    )?;
    let before = if let Some(token) = cursor {
        let w = resolve(tx, ws, user, "feed", token, now)?;
        if w.position <= 0 || w.revision != revision {
            return Err(StoreError::Conflict(
                "Attention snapshot changed; restart this category".into(),
            ));
        }
        Some(w.position)
    } else {
        None
    };
    let unchanged = if let Some(token) = if_snapshot {
        let w = resolve(tx, ws, user, "feed", token, now)?;
        if w.position != 0 {
            return Err(StoreError::Conflict("Expected completed snapshot".into()));
        }
        w.revision == revision
    } else {
        false
    };
    let mut entries = vec![];
    if !unchanged {
        // Two query shapes avoid an optional-bound OR that defeats SQLite's keyset range seek.
        let bound = if before.is_some() {
            " AND u.seq<?3"
        } else {
            ""
        };
        let sql=format!("SELECT e.id,e.seq,e.published_at,e.source_id,CASE WHEN e.source_type='notification' THEN json_extract(e.source_key,'$[1]') END
            FROM attention_feed_unread u JOIN feed_entries e ON e.id=u.entry_id
            WHERE u.workspace_id=?1 AND u.user_id=?2{bound} ORDER BY u.seq DESC LIMIT ?4");
        let mut st = tx.prepare_cached(&sql)?;
        entries = st
            .query_map(params![ws, user, before, limit as i64], |r| {
                let id: String = r.get(0)?;
                let mut e = Entry::new(id.clone(), r.get(2)?, "feed", id);
                e.order = Some(r.get(1)?);
                e.source_id = r.get(3)?;
                e.group_key = r.get(4)?;
                Ok(e)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }
    // Probe the indexed next key, not count/offset over earlier pages.
    let more = if let Some(last) = entries.last() {
        tx.query_row("SELECT EXISTS(SELECT 1 FROM attention_feed_unread WHERE workspace_id=? AND user_id=? AND seq<?)",
            params![ws,user,last.order],|r|r.get::<_,bool>(0))?
    } else {
        false
    };
    let next_cursor = if more {
        Some(window(
            tx,
            ws,
            user,
            "feed",
            &revision,
            0,
            entries.last().unwrap().order.unwrap(),
            now,
        )?)
    } else {
        None
    };
    let snapshot = if more {
        None
    } else {
        Some(window(tx, ws, user, "feed", &revision, 0, 0, now)?)
    };
    Ok(Category {
        kind: "feed".into(),
        count: count as usize,
        unit: "entries",
        available: true,
        entries,
        next_cursor,
        snapshot,
        revision,
        unchanged,
    })
}

struct Window {
    revision: String,
    upper: i64,
    position: i64,
}
fn resolve(
    c: &Connection,
    ws: &str,
    user: &str,
    kind: &str,
    token: &str,
    now: i64,
) -> Result<Window> {
    c.query_row(
        "SELECT revision,upper_seq,position FROM attention_windows
        WHERE token=? AND workspace_id=? AND user_id=? AND kind=? AND expires>?",
        params![token, ws, user, kind, now],
        |r| {
            Ok(Window {
                revision: r.get(0)?,
                upper: r.get(1)?,
                position: r.get(2)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| {
        StoreError::Conflict(
            "Invalid or expired attention capability for this scope/category".into(),
        )
    })
}
#[allow(clippy::too_many_arguments)]
fn window(
    tx: &Tx,
    ws: &str,
    user: &str,
    kind: &str,
    revision: &str,
    upper: i64,
    position: i64,
    now: i64,
) -> Result<String> {
    if let Some(t) = tx.query_row("SELECT token FROM attention_windows WHERE workspace_id=? AND user_id=? AND kind=? AND revision=? AND upper_seq=? AND position=? AND expires>? LIMIT 1",params![ws,user,kind,revision,upper,position,now+3_600_000], |r| r.get::<_,String>(0)).optional()? { return Ok(t); }
    let token = format!("{}{}", uid("aw"), uid(""));
    tx.execute(
        "INSERT INTO attention_windows VALUES(?,?,?,?,?,?,?,?)",
        params![token, ws, user, kind, revision, upper, position, now + TTL],
    )?;
    Ok(token)
}
#[allow(clippy::too_many_arguments)]
pub fn page(
    tx: &Tx,
    ws: &str,
    user: &str,
    kind: &str,
    entries: Vec<Entry>,
    upper: i64,
    cursor: Option<&str>,
    if_snapshot: Option<&str>,
    limit: usize,
    now: i64,
) -> Result<Category> {
    feed::member(tx, ws, user)?;
    if !KINDS.contains(&kind)
        || !(1..=100).contains(&limit)
        || (cursor.is_some() && if_snapshot.is_some())
    {
        return Err(StoreError::Conflict(
            "Invalid attention category, limit, or cursor combination".into(),
        ));
    }
    let revision = sha(&serde_json::to_string(&(ws, user, kind, upper, &entries))?);
    let position = if let Some(token) = cursor {
        let w = resolve(tx, ws, user, kind, token, now)?;
        if w.position == 0 || w.revision != revision || w.upper != upper {
            return Err(StoreError::Conflict(
                "Attention snapshot changed; restart this category".into(),
            ));
        }
        usize::try_from(w.position)
            .map_err(|_| StoreError::Conflict("Invalid page position".into()))?
    } else {
        0
    };
    let unchanged = if let Some(token) = if_snapshot {
        let w = resolve(tx, ws, user, kind, token, now)?;
        if w.position != 0 {
            return Err(StoreError::Conflict(
                "Expected an attention snapshot, not page cursor".into(),
            ));
        }
        w.revision == revision && w.upper == upper
    } else {
        false
    };
    let count = entries.len();
    let end = position.saturating_add(limit).min(count);
    if position > count {
        return Err(StoreError::Conflict(
            "Invalid attention page position".into(),
        ));
    }
    let snapshot = if unchanged || end == count {
        Some(window(tx, ws, user, kind, &revision, upper, 0, now)?)
    } else {
        None
    };
    let next_cursor = if !unchanged && end < count {
        Some(window(
            tx, ws, user, kind, &revision, upper, end as i64, now,
        )?)
    } else {
        None
    };
    Ok(Category {
        kind: kind.into(),
        count,
        unit: match kind {
            "replies" => "messages",
            "pending" => "cards",
            "feed" => "entries",
            _ => "conversations",
        },
        available: true,
        entries: if unchanged {
            vec![]
        } else {
            entries[position..end].to_vec()
        },
        next_cursor,
        snapshot,
        revision,
        unchanged,
    })
}

/// The client must supply the last reply actually shown, not merely the current head while a
/// conversation is focused. This acknowledges the visible prefix only; it never approves a card.
pub fn read_replies(
    tx: &Tx,
    ws: &str,
    user: &str,
    snapshot: &str,
    through_id: &str,
    now: i64,
) -> Result<bool> {
    feed::member(tx, ws, user)?;
    let w = resolve(tx, ws, user, "replies", snapshot, now)?;
    if w.position != 0 {
        return Err(StoreError::Conflict("Expected replies snapshot".into()));
    }
    let seq: Option<i64> = tx.query_row("SELECT q.seq FROM thread_reply_commits q
        JOIN thread_runs r ON r.id=q.run_id JOIN messages m ON m.id=q.message_id
        WHERE q.message_id=?1 AND q.workspace_id=?2 AND q.user_id=?3 AND q.seq<=?4
          AND r.workspace_id=?2 AND r.user_id=?3 AND r.status IN ('done','stopped','budget','violated','failed') AND m.space='steve' AND m.author='steve'",
        params![through_id,ws,user,w.upper], |r| r.get(0)).optional()?;
    let seq =
        seq.ok_or_else(|| StoreError::Conflict("Reply is not in this caller's snapshot".into()))?;
    let changed = tx.execute("INSERT INTO thread_reply_reads(workspace_id,user_id,space,through_seq) VALUES(?,?,'steve',?)
        ON CONFLICT(workspace_id,user_id,space) DO UPDATE SET through_seq=excluded.through_seq
        WHERE excluded.through_seq>thread_reply_reads.through_seq",params![ws,user,seq])?;
    Ok(changed > 0)
}

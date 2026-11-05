//! The SQL of the mail store (tables of migration 9, `steve-store/migrations/009.sql`), plus the scope
//! list in `source_settings` (migration 6). Parameterized queries only; functions take a connection or
//! a transaction.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_modules::accounts::connections::WAITING;
use steve_store::Tx;

use crate::adapter::{FolderInfo, FolderState, ScopeMode};
use crate::error::MailResult;

fn json_vec(s: Option<String>) -> Vec<String> {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn json_val(s: Option<String>) -> Value {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Array(Vec::new()))
}

// ---------------------------------------------------------------- folders

pub fn folders(c: &Connection, conn: &str) -> MailResult<Vec<FolderState>> {
    let mut st = c.prepare_cached(
        "SELECT ext_id, name, role, uidvalidity, last_uid, modseq, cursor, synced
           FROM mail_folders WHERE connection_id = ? ORDER BY rowid",
    )?;
    let rows = st.query_map([conn], |r| {
        Ok(FolderState {
            ext_id: r.get(0)?,
            name: r.get(1)?,
            role: r.get(2)?,
            mode: ScopeMode::Ignore,
            uidvalidity: r.get(3)?,
            last_uid: r.get(4)?,
            modseq: r.get(5)?,
            cursor: r.get(6)?,
            synced: r.get(7)?,
            flags_checked: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Adds a folder the provider listed, or updates its name and role.
pub fn upsert_folder(tx: &Tx, ws: &str, conn: &str, f: &FolderInfo) -> MailResult<()> {
    tx.prepare_cached(
        "INSERT INTO mail_folders (connection_id, workspace_id, ext_id, name, role) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(connection_id, ext_id) DO UPDATE SET name = excluded.name, role = excluded.role",
    )?
    .execute(params![conn, ws, f.ext_id, f.name, f.role])?;
    Ok(())
}

pub fn save_folder(tx: &Tx, conn: &str, f: &FolderState) -> MailResult<()> {
    tx.prepare_cached(
        "UPDATE mail_folders SET uidvalidity = ?, last_uid = ?, modseq = ?, cursor = ?, synced = ?
          WHERE connection_id = ? AND ext_id = ?",
    )?
    .execute(params![
        f.uidvalidity,
        f.last_uid,
        f.modseq,
        f.cursor,
        f.synced,
        conn,
        f.ext_id
    ])?;
    Ok(())
}

// ---------------------------------------------------------------- scopes (source_settings)

/// One entry of `source_settings.scopes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeEntry {
    pub ext_id: String,
    pub name: String,
    pub mode: ScopeMode,
}

/// `(paused, scopes, cursor)` of a connection's source settings.
pub type SourceRow = (bool, Vec<ScopeEntry>, Option<String>);

/// The source settings of a connection, if the row exists.
pub fn source(c: &Connection, conn: &str) -> MailResult<Option<SourceRow>> {
    Ok(c.prepare_cached(
        "SELECT paused, scopes, cursor FROM source_settings WHERE connection_id = ?",
    )?
    .query_row([conn], |r| {
        let scopes: Option<String> = r.get(1)?;
        Ok((
            r.get::<_, i64>(0)? != 0,
            scopes
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            r.get(2)?,
        ))
    })
    .optional()?)
}

/// Writes the scope list (creating the source settings row with its defaults when missing).
pub fn set_scopes(
    tx: &Tx,
    ws: &str,
    conn: &str,
    scopes: &[ScopeEntry],
    now: i64,
) -> MailResult<()> {
    let s = serde_json::to_string(scopes).unwrap_or_else(|_| "[]".into());
    tx.prepare_cached(
        "INSERT INTO source_settings (connection_id, workspace_id, scopes, updated) VALUES (?, ?, ?, ?)
         ON CONFLICT(connection_id) DO UPDATE SET scopes = excluded.scopes, updated = excluded.updated",
    )?
    .execute(params![conn, ws, s, now])?;
    Ok(())
}

pub fn set_cursor(tx: &Tx, conn: &str, cursor: Option<&str>) -> MailResult<()> {
    tx.prepare_cached("UPDATE source_settings SET cursor = ? WHERE connection_id = ?")?
        .execute(params![cursor, conn])?;
    Ok(())
}

// ---------------------------------------------------------------- settings

/// `mail_account_settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountSettings {
    pub signature: String,
    pub duty: String,
    pub send_delay_s: i64,
    pub archive_on_done: bool,
    pub sync_days: i64,
    pub append_sent: String,
    pub revision: i64,
    pub updated: i64,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self {
            signature: String::new(),
            duty: String::new(),
            send_delay_s: 10,
            archive_on_done: false,
            sync_days: 30,
            append_sent: "auto".into(),
            revision: 1,
            updated: 0,
        }
    }
}

pub fn settings(c: &Connection, conn: &str) -> MailResult<Option<AccountSettings>> {
    Ok(c.prepare_cached(
        "SELECT signature, duty, send_delay_s, archive_on_done, sync_days, append_sent, revision, updated
           FROM mail_account_settings WHERE connection_id = ?",
    )?
    .query_row([conn], |r| {
        Ok(AccountSettings {
            signature: r.get(0)?,
            duty: r.get(1)?,
            send_delay_s: r.get(2)?,
            archive_on_done: r.get::<_, i64>(3)? != 0,
            sync_days: r.get(4)?,
            append_sent: r.get(5)?,
            revision: r.get(6)?,
            updated: r.get(7)?,
        })
    })
    .optional()?)
}

/// Creates the settings row with defaults if missing.
pub fn ensure_settings(tx: &Tx, ws: &str, conn: &str, now: i64) -> MailResult<()> {
    tx.prepare_cached(
        "INSERT OR IGNORE INTO mail_account_settings (connection_id, workspace_id, updated) VALUES (?, ?, ?)",
    )?
    .execute(params![conn, ws, now])?;
    Ok(())
}

/// Writes every field and bumps the revision.
pub fn update_settings(tx: &Tx, conn: &str, s: &AccountSettings, now: i64) -> MailResult<()> {
    tx.prepare_cached(
        "UPDATE mail_account_settings SET signature = ?, duty = ?, send_delay_s = ?, archive_on_done = ?,
                sync_days = ?, append_sent = ?, revision = revision + 1, updated = ?
          WHERE connection_id = ?",
    )?
    .execute(params![
        s.signature,
        s.duty,
        s.send_delay_s,
        s.archive_on_done as i64,
        s.sync_days,
        s.append_sent,
        now,
        conn
    ])?;
    Ok(())
}

// ---------------------------------------------------------------- threads

/// `mail_threads`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRow {
    pub id: String,
    pub workspace_id: String,
    pub connection_id: String,
    pub thread_key: String,
    pub subject: String,
    pub last_at: i64,
    pub count: i64,
    pub unread: i64,
    pub participants: Vec<String>,
    pub state: String,
    pub visibility: String,
    pub created_by: String,
}

pub(crate) const THREAD_COLS: &str = "id, workspace_id, connection_id, thread_key, subject, last_at, count, unread, participants, state, visibility, created_by";

pub(crate) fn thread_row(r: &Row<'_>) -> rusqlite::Result<ThreadRow> {
    Ok(ThreadRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        connection_id: r.get(2)?,
        thread_key: r.get(3)?,
        subject: r.get(4)?,
        last_at: r.get(5)?,
        count: r.get(6)?,
        unread: r.get(7)?,
        participants: json_vec(r.get(8)?),
        state: r.get(9)?,
        visibility: r.get(10)?,
        created_by: r.get(11)?,
    })
}

pub fn thread(c: &Connection, id: &str) -> MailResult<Option<ThreadRow>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {THREAD_COLS} FROM mail_threads WHERE id = ?"
    ))?
    .query_row([id], thread_row)
    .optional()?)
}

pub fn thread_by_key(c: &Connection, conn: &str, key: &str) -> MailResult<Option<ThreadRow>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {THREAD_COLS} FROM mail_threads WHERE connection_id = ? AND thread_key = ?"
    ))?
    .query_row([conn, key], thread_row)
    .optional()?)
}

pub fn threads_of(c: &Connection, conn: &str) -> MailResult<Vec<ThreadRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {THREAD_COLS} FROM mail_threads WHERE connection_id = ? ORDER BY last_at DESC"
    ))?;
    Ok(st
        .query_map([conn], thread_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn insert_thread(tx: &Tx, t: &ThreadRow, now: i64) -> MailResult<()> {
    tx.prepare_cached(
        "INSERT INTO mail_threads (id, workspace_id, connection_id, thread_key, subject, last_at, count, unread,
                                   participants, state, visibility, created_by, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        t.id,
        t.workspace_id,
        t.connection_id,
        t.thread_key,
        t.subject,
        t.last_at,
        t.state,
        t.visibility,
        t.created_by,
        now,
        now
    ])?;
    Ok(())
}

/// Recomputes a thread's counters and participants from its messages.
pub fn refresh_thread(tx: &Tx, id: &str, now: i64) -> MailResult<()> {
    let (count, last_at): (i64, Option<i64>) = tx
        .prepare_cached("SELECT COUNT(*), MAX(date) FROM mail_messages WHERE thread_id = ?")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut unread = 0i64;
    let mut parts: Vec<String> = Vec::new();
    {
        let mut st = tx.prepare_cached(
            "SELECT direction, flags, from_account_id, to_addrs, cc_addrs FROM mail_messages WHERE thread_id = ? ORDER BY date",
        )?;
        let rows = st.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                json_vec(r.get(1)?),
                r.get::<_, Option<String>>(2)?,
                json_val(r.get(3)?),
                json_val(r.get(4)?),
            ))
        })?;
        for row in rows {
            let (dir, flags, from, to, cc) = row?;
            if dir == "in" && !flags.iter().any(|f| f == "seen") {
                unread += 1;
            }
            let mut add = |a: Option<String>| {
                if let Some(a) = a
                    && !parts.contains(&a)
                {
                    parts.push(a);
                }
            };
            add(from);
            for v in [to, cc] {
                for x in v.as_array().into_iter().flatten() {
                    add(x
                        .get("account_id")
                        .and_then(Value::as_str)
                        .map(String::from));
                }
            }
        }
    }
    tx.prepare_cached(
        "UPDATE mail_threads SET count = ?, unread = ?, last_at = MAX(last_at, ?), participants = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![
        count,
        unread,
        last_at.unwrap_or(0),
        serde_json::to_string(&parts).unwrap_or_else(|_| "[]".into()),
        now,
        id
    ])?;
    Ok(())
}

pub fn set_thread_state(tx: &Tx, id: &str, state: &str, now: i64) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_threads SET state = ?, updated = ? WHERE id = ?")?
        .execute(params![state, now, id])?;
    Ok(())
}

// ---------------------------------------------------------------- messages

/// `mail_messages` (the columns the store and the jobs need).
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRow {
    pub id: String,
    pub workspace_id: String,
    pub connection_id: String,
    pub thread_id: String,
    pub ext_id: String,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub refs: Vec<String>,
    pub folders: Vec<String>,
    pub flags: Vec<String>,
    pub direction: String,
    pub from_addr: String,
    pub from_name: String,
    pub from_account_id: Option<String>,
    pub from_party_id: Option<String>,
    pub reply_to: Option<String>,
    /// `[{"addr","name","account_id","party_id"}]`
    pub to_addrs: Value,
    pub cc_addrs: Value,
    pub subject: String,
    pub snippet: String,
    pub date: i64,
    pub size: i64,
    pub has_attachments: bool,
    pub bulk: bool,
    pub body_text: Option<String>,
    pub body_new: Option<String>,
    pub has_html: bool,
    pub raw_path: Option<String>,
    pub purged: Option<i64>,
    pub event_id: Option<String>,
    pub status: String,
    pub created: i64,
}

const MSG_COLS: &str = "id, workspace_id, connection_id, thread_id, ext_id, message_id, in_reply_to, refs, folders, flags,
  direction, from_addr, from_name, from_account_id, from_party_id, reply_to, to_addrs, cc_addrs, subject, snippet,
  date, size, has_attachments, bulk, body_text, body_new, has_html, raw_path, purged, event_id, status, created";

fn msg_row(r: &Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        connection_id: r.get(2)?,
        thread_id: r.get(3)?,
        ext_id: r.get(4)?,
        message_id: r.get(5)?,
        in_reply_to: r.get(6)?,
        refs: json_vec(r.get(7)?),
        folders: json_vec(r.get(8)?),
        flags: json_vec(r.get(9)?),
        direction: r.get(10)?,
        from_addr: r.get(11)?,
        from_name: r.get(12)?,
        from_account_id: r.get(13)?,
        from_party_id: r.get(14)?,
        reply_to: r.get(15)?,
        to_addrs: json_val(r.get(16)?),
        cc_addrs: json_val(r.get(17)?),
        subject: r.get(18)?,
        snippet: r.get(19)?,
        date: r.get(20)?,
        size: r.get(21)?,
        has_attachments: r.get::<_, i64>(22)? != 0,
        bulk: r.get::<_, i64>(23)? != 0,
        body_text: r.get(24)?,
        body_new: r.get(25)?,
        has_html: r.get::<_, i64>(26)? != 0,
        raw_path: r.get(27)?,
        purged: r.get(28)?,
        event_id: r.get(29)?,
        status: r.get(30)?,
        created: r.get(31)?,
    })
}

pub fn message(c: &Connection, id: &str) -> MailResult<Option<MessageRow>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {MSG_COLS} FROM mail_messages WHERE id = ?"
    ))?
    .query_row([id], msg_row)
    .optional()?)
}

pub fn message_by_ext(c: &Connection, conn: &str, ext: &str) -> MailResult<Option<MessageRow>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {MSG_COLS} FROM mail_messages WHERE connection_id = ? AND ext_id = ?"
    ))?
    .query_row([conn, ext], msg_row)
    .optional()?)
}

/// The messages of a connection with this Message-ID.
pub fn messages_by_mid(
    c: &Connection,
    ws: &str,
    conn: &str,
    mid: &str,
) -> MailResult<Vec<MessageRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {MSG_COLS} FROM mail_messages WHERE workspace_id = ? AND message_id = ? AND connection_id = ?"
    ))?;
    Ok(st
        .query_map([ws, mid, conn], msg_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// A message without Message-ID with the same sender, date and subject (dedupe on rescans).
pub fn message_by_shape(
    c: &Connection,
    conn: &str,
    from: &str,
    date: i64,
    subject: &str,
) -> MailResult<Option<MessageRow>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {MSG_COLS} FROM mail_messages WHERE connection_id = ? AND message_id IS NULL
            AND from_addr = ? AND date = ? AND subject = ? LIMIT 1"
    ))?
    .query_row(params![conn, from, date, subject], msg_row)
    .optional()?)
}

/// The columns of a message that a thread's list card shows.
#[derive(Debug, Clone)]
pub struct BriefMsg {
    pub direction: String,
    pub from_name: String,
    pub from_addr: String,
    pub from_party_id: Option<String>,
    pub snippet: String,
    pub has_attachments: bool,
    pub event_id: Option<String>,
}

/// [`thread_messages`] narrowed to [`BriefMsg`]: no bodies, references or recipients.
pub fn thread_brief_rows(c: &Connection, thread: &str) -> MailResult<Vec<BriefMsg>> {
    let mut st = c.prepare_cached(
        "SELECT direction, from_name, from_addr, from_party_id, snippet, has_attachments, event_id
           FROM mail_messages WHERE thread_id = ? ORDER BY date, rowid",
    )?;
    Ok(st
        .query_map([thread], |r| {
            Ok(BriefMsg {
                direction: r.get(0)?,
                from_name: r.get(1)?,
                from_addr: r.get(2)?,
                from_party_id: r.get(3)?,
                snippet: r.get(4)?,
                has_attachments: r.get::<_, i64>(5)? != 0,
                event_id: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn thread_messages(c: &Connection, thread: &str) -> MailResult<Vec<MessageRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {MSG_COLS} FROM mail_messages WHERE thread_id = ? ORDER BY date, rowid"
    ))?;
    Ok(st
        .query_map([thread], msg_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn insert_message(tx: &Tx, m: &MessageRow) -> MailResult<()> {
    let j = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
    tx.prepare_cached(&format!(
        "INSERT INTO mail_messages ({MSG_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))?
    .execute(params![
        m.id,
        m.workspace_id,
        m.connection_id,
        m.thread_id,
        m.ext_id,
        m.message_id,
        m.in_reply_to,
        j(&m.refs),
        j(&m.folders),
        j(&m.flags),
        m.direction,
        m.from_addr,
        m.from_name,
        m.from_account_id,
        m.from_party_id,
        m.reply_to,
        m.to_addrs.to_string(),
        m.cc_addrs.to_string(),
        m.subject,
        m.snippet,
        m.date,
        m.size,
        m.has_attachments as i64,
        m.bulk as i64,
        m.body_text,
        m.body_new,
        m.has_html as i64,
        m.raw_path,
        m.purged,
        m.event_id,
        m.status,
        m.created
    ])?;
    Ok(())
}

/// Another copy of a known message: its current id, folders and flags.
pub fn update_copy(
    tx: &Tx,
    id: &str,
    ext: &str,
    folders: &[String],
    flags: &[String],
) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_messages SET ext_id = ?, folders = ?, flags = ? WHERE id = ?")?
        .execute(params![
            ext,
            serde_json::to_string(folders).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(flags).unwrap_or_else(|_| "[]".into()),
            id
        ])?;
    Ok(())
}

pub fn set_flags(tx: &Tx, id: &str, flags: &[String]) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_messages SET flags = ? WHERE id = ?")?
        .execute(params![
            serde_json::to_string(flags).unwrap_or_else(|_| "[]".into()),
            id
        ])?;
    Ok(())
}

pub fn set_folders(tx: &Tx, id: &str, folders: &[String]) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_messages SET folders = ? WHERE id = ?")?
        .execute(params![
            serde_json::to_string(folders).unwrap_or_else(|_| "[]".into()),
            id
        ])?;
    Ok(())
}

pub fn set_event(tx: &Tx, id: &str, event: &str) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_messages SET event_id = ? WHERE id = ?")?
        .execute(params![event, id])?;
    Ok(())
}

/// Whether one of these Message-IDs is a message I sent from this connection.
pub fn is_mine(c: &Connection, conn: &str, mid: &str) -> MailResult<bool> {
    Ok(c.prepare_cached(
        "SELECT 1 FROM mail_messages WHERE connection_id = ? AND message_id = ? AND direction = 'out' LIMIT 1",
    )?
    .query_row([conn, mid], |_| Ok(()))
    .optional()?
    .is_some())
}

/// The thread of the first message of this connection with one of the ids.
pub fn thread_of_ids(
    c: &Connection,
    ws: &str,
    conn: &str,
    ids: &[String],
) -> MailResult<Option<String>> {
    let mut st = c.prepare_cached(
        "SELECT thread_id FROM mail_messages WHERE workspace_id = ? AND message_id = ? AND connection_id = ? LIMIT 1",
    )?;
    for id in ids {
        if let Some(t) = st
            .query_row([ws, id.as_str(), conn], |r| r.get::<_, String>(0))
            .optional()?
        {
            return Ok(Some(t));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------- attachments

pub fn insert_attachment(
    tx: &Tx,
    ws: &str,
    message: &str,
    a: &crate::adapter::PartMeta,
    now: i64,
) -> MailResult<String> {
    let id = steve_store::uid("mla");
    tx.prepare_cached(
        "INSERT INTO mail_attachments (id, workspace_id, message_id, part, name, mime, size, content_id, inline, created)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        ws,
        message,
        a.part,
        a.name,
        a.mime,
        a.size,
        a.content_id,
        a.inline as i64,
        now
    ])?;
    Ok(id)
}

/// `(id, name, mime, size, inline)`.
pub type AttachmentRow = (String, String, String, i64, bool);

/// A message's attachments.
pub fn attachments(c: &Connection, message: &str) -> MailResult<Vec<AttachmentRow>> {
    let mut st = c.prepare_cached(
        "SELECT id, name, mime, size, inline FROM mail_attachments WHERE message_id = ? ORDER BY rowid",
    )?;
    Ok(st
        .query_map([message], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, i64>(4)? != 0,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------- retention, disconnect

/// `(id, raw_path)` of messages older than `before` whose bodies are still kept.
/// One page of expired, not yet purged messages as `(id, raw_path, date)`, in `(date, id)` order and
/// strictly after `after`, so a row that cannot be purged does not come back on the next page.
pub fn expired(
    c: &Connection,
    ws: &str,
    before: i64,
    after: Option<(i64, &str)>,
    limit: i64,
) -> MailResult<Vec<(String, Option<String>, i64)>> {
    let (ad, ai) = after.map_or((None, None), |(d, i)| (Some(d), Some(i)));
    let mut st = c.prepare_cached(
        "SELECT id, raw_path, date FROM mail_messages WHERE workspace_id = ?1 AND date < ?2 AND purged IS NULL
            AND (?3 IS NULL OR (date, id) > (?3, ?4))
          ORDER BY date, id LIMIT ?5",
    )?;
    Ok(st
        .query_map(params![ws, before, ad, ai, limit], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn purge(tx: &Tx, id: &str, now: i64) -> MailResult<()> {
    tx.prepare_cached(
        "UPDATE mail_messages SET raw_path = NULL, body_text = NULL, body_new = NULL, purged = ? WHERE id = ?",
    )?
    .execute(params![now, id])?;
    Ok(())
}

/// The workspaces that have mail.
pub fn mail_workspaces(c: &Connection) -> MailResult<Vec<String>> {
    let mut st = c.prepare_cached("SELECT DISTINCT workspace_id FROM mail_messages")?;
    Ok(st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The raw files of a connection's messages.
pub fn raw_paths(c: &Connection, conn: &str) -> MailResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT raw_path FROM mail_messages WHERE connection_id = ? AND raw_path IS NOT NULL",
    )?;
    Ok(st
        .query_map([conn], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Deletes everything of a connection (disconnect), children first.
pub fn delete_connection(tx: &Tx, conn: &str) -> MailResult<()> {
    for sql in [
        "DELETE FROM mail_drafts WHERE connection_id = ?1",
        "DELETE FROM mail_attachments WHERE message_id IN (SELECT id FROM mail_messages WHERE connection_id = ?1)",
        "DELETE FROM mail_messages WHERE connection_id = ?1",
        "DELETE FROM mail_threads WHERE connection_id = ?1",
        "DELETE FROM mail_folders WHERE connection_id = ?1",
        "DELETE FROM mail_account_settings WHERE connection_id = ?1",
    ] {
        tx.prepare_cached(sql)?.execute([conn])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- drafts (E3)

/// One recipient of a draft (`to_addrs` / `cc_addrs` / `bcc_addrs` of `mail_drafts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rcpt {
    pub addr: String,
    #[serde(default)]
    pub name: String,
}

/// `mail_drafts`.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftRow {
    pub id: String,
    pub workspace_id: String,
    pub connection_id: String,
    pub thread_id: Option<String>,
    pub reply_to_message: Option<String>,
    /// `reply|reply_all|forward|new`
    pub mode: String,
    pub to: Vec<Rcpt>,
    pub cc: Vec<Rcpt>,
    pub bcc: Vec<Rcpt>,
    pub subject: String,
    pub body: String,
    /// `FileRef[]`.
    pub files: Vec<steve_store::FileRef>,
    /// `me|steve|<bot id>`
    pub author: String,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub decision_id: Option<String>,
    pub basis: Option<Value>,
    /// The version an approval decision was opened for.
    pub content_hash: String,
    /// `draft|pending|approved|sending|sent|failed|unknown|discarded`
    pub status: String,
    pub send_at: Option<i64>,
    pub sent_message: Option<String>,
    pub error: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub sent: Option<i64>,
}

const DRAFT_COLS: &str = "id, workspace_id, connection_id, thread_id, reply_to_message, mode, to_addrs, cc_addrs, bcc_addrs,
  subject, body, files, author, item_id, session_id, run_id, decision_id, basis, content_hash, status, send_at,
  sent_message, error, created, updated, sent";

fn rcpts(s: Option<String>) -> Vec<Rcpt> {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn draft_row(r: &Row<'_>) -> rusqlite::Result<DraftRow> {
    Ok(DraftRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        connection_id: r.get(2)?,
        thread_id: r.get(3)?,
        reply_to_message: r.get(4)?,
        mode: r.get(5)?,
        to: rcpts(r.get(6)?),
        cc: rcpts(r.get(7)?),
        bcc: rcpts(r.get(8)?),
        subject: r.get(9)?,
        body: r.get(10)?,
        files: r
            .get::<_, Option<String>>(11)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        author: r.get(12)?,
        item_id: r.get(13)?,
        session_id: r.get(14)?,
        run_id: r.get(15)?,
        decision_id: r.get(16)?,
        basis: r
            .get::<_, Option<String>>(17)?
            .and_then(|s| serde_json::from_str(&s).ok()),
        content_hash: r.get(18)?,
        status: r.get(19)?,
        send_at: r.get(20)?,
        sent_message: r.get(21)?,
        error: r.get(22)?,
        created: r.get(23)?,
        updated: r.get(24)?,
        sent: r.get(25)?,
    })
}

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

pub fn draft(c: &Connection, id: &str) -> MailResult<Option<DraftRow>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {DRAFT_COLS} FROM mail_drafts WHERE id = ?"
    ))?
    .query_row([id], draft_row)
    .optional()?)
}

/// The drafts of a thread that are not discarded, oldest first.
pub fn thread_drafts(c: &Connection, thread: &str) -> MailResult<Vec<DraftRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {DRAFT_COLS} FROM mail_drafts WHERE thread_id = ? AND status != 'discarded' ORDER BY created, rowid"
    ))?;
    Ok(st
        .query_map([thread], draft_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Approved drafts whose delay is over (the send queue), oldest first; those of a connection that
/// waits ([`WAITING`]: paused, to be authorized again) stay approved and aren't listed.
/// Approved drafts whose time has come, as `(id, connection_id)`, oldest first.
pub fn due_drafts(c: &Connection, now: i64, limit: i64) -> MailResult<Vec<(String, String)>> {
    let waiting = WAITING.map(|s| format!("'{s}'")).join(", ");
    let mut st = c.prepare_cached(&format!(
        "SELECT d.id, d.connection_id FROM mail_drafts d
          WHERE d.status = 'approved' AND d.send_at <= ?
            AND NOT EXISTS (SELECT 1 FROM connections c WHERE c.id = d.connection_id AND c.status IN ({waiting}))
          ORDER BY d.send_at LIMIT ?"
    ))?;
    Ok(st
        .query_map(params![now, limit], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn insert_draft(tx: &Tx, d: &DraftRow) -> MailResult<()> {
    tx.prepare_cached(&format!(
        "INSERT INTO mail_drafts ({DRAFT_COLS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))?
    .execute(params![
        d.id,
        d.workspace_id,
        d.connection_id,
        d.thread_id,
        d.reply_to_message,
        d.mode,
        to_json(&d.to),
        to_json(&d.cc),
        to_json(&d.bcc),
        d.subject,
        d.body,
        to_json(&d.files),
        d.author,
        d.item_id,
        d.session_id,
        d.run_id,
        d.decision_id,
        d.basis.as_ref().map(Value::to_string),
        d.content_hash,
        d.status,
        d.send_at,
        d.sent_message,
        d.error,
        d.created,
        d.updated,
        d.sent
    ])?;
    Ok(())
}

/// Writes the editable content (recipients, subject, body, files, thread links).
pub fn update_draft_content(tx: &Tx, d: &DraftRow, now: i64) -> MailResult<()> {
    tx.prepare_cached(
        "UPDATE mail_drafts SET mode = ?, thread_id = ?, reply_to_message = ?, to_addrs = ?, cc_addrs = ?, bcc_addrs = ?,
                subject = ?, body = ?, files = ?, content_hash = ?, basis = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![
        d.mode,
        d.thread_id,
        d.reply_to_message,
        to_json(&d.to),
        to_json(&d.cc),
        to_json(&d.bcc),
        d.subject,
        d.body,
        to_json(&d.files),
        d.content_hash,
        d.basis.as_ref().map(Value::to_string),
        now,
        d.id
    ])?;
    Ok(())
}

/// Moves a draft from one status to another; `false` if it was not in `from` (someone else moved it).
pub fn draft_transition(tx: &Tx, id: &str, from: &[&str], to: &str, now: i64) -> MailResult<bool> {
    let marks = vec!["?"; from.len()].join(", ");
    let sql = format!(
        "UPDATE mail_drafts SET status = ?, updated = ? WHERE id = ? AND status IN ({marks})"
    );
    let mut ps: Vec<rusqlite::types::Value> =
        vec![to.to_string().into(), now.into(), id.to_string().into()];
    ps.extend(
        from.iter()
            .map(|s| rusqlite::types::Value::Text((*s).into())),
    );
    Ok(tx.execute(&sql, rusqlite::params_from_iter(ps))? > 0)
}

/// Sets the send time (approval) or clears it (undo).
pub fn set_draft_send_at(tx: &Tx, id: &str, send_at: Option<i64>) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_drafts SET send_at = ? WHERE id = ?")?
        .execute(params![send_at, id])?;
    Ok(())
}

pub fn set_draft_decision(tx: &Tx, id: &str, decision: Option<&str>, hash: &str) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_drafts SET decision_id = ?, content_hash = ? WHERE id = ?")?
        .execute(params![decision, hash, id])?;
    Ok(())
}

pub fn set_draft_error(tx: &Tx, id: &str, error: Option<&str>) -> MailResult<()> {
    tx.prepare_cached("UPDATE mail_drafts SET error = ? WHERE id = ?")?
        .execute(params![error, id])?;
    Ok(())
}

pub fn set_draft_sent(tx: &Tx, id: &str, message: &str, now: i64) -> MailResult<()> {
    tx.prepare_cached(
        "UPDATE mail_drafts SET status = 'sent', sent_message = ?, sent = ?, error = NULL, updated = ? WHERE id = ?",
    )?
    .execute(params![message, now, now, id])?;
    Ok(())
}

/// The latest resolved decision about a draft whose answer was `send` (the approval `execute` needs).
pub fn send_approval(
    c: &Connection,
    ws: &str,
    draft: &str,
) -> MailResult<Option<steve_store::Decision>> {
    let id: Option<String> = c
        .prepare_cached(
            "SELECT id FROM decisions WHERE workspace_id = ? AND subject_type = 'mail_draft' AND subject_id = ?
                AND kind = 'approve' AND status = 'resolved' AND json_extract(result, '$.choice') = 'send'
              ORDER BY resolved DESC, created DESC LIMIT 1",
        )?
        .query_row([ws, draft], |r| r.get(0))
        .optional()?;
    match id {
        Some(id) => Ok(steve_store::base::decisions::get(c, &id)?),
        None => Ok(None),
    }
}

/// Every open decision about a draft (approval or failure).
pub fn open_draft_decisions(c: &Connection, ws: &str, draft: &str) -> MailResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT id FROM decisions WHERE workspace_id = ? AND subject_type = 'mail_draft' AND subject_id = ? AND status = 'open'",
    )?;
    Ok(st
        .query_map([ws, draft], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Back to open (cancel within the delay = `undoDecision` of the approval), like the store's `reopen`.
pub fn reopen_decision(tx: &Tx, id: &str) -> MailResult<()> {
    tx.prepare_cached(
        "UPDATE decisions SET status = 'open', result = NULL, resolved = NULL, undo = NULL WHERE id = ?",
    )?
    .execute([id])?;
    let item_id: Option<String> = tx
        .prepare_cached("SELECT item_id FROM decisions WHERE id = ?")?
        .query_row([id], |r| r.get(0))
        .optional()?
        .flatten();
    tx.publish(steve_store::ChangeEvent::Decision {
        id: id.into(),
        item_id: Some(item_id),
    });
    Ok(())
}

/// Unread mail of a mailbox.
pub fn unread_of(c: &Connection, conn: &str) -> MailResult<i64> {
    Ok(c.prepare_cached(
        "SELECT COALESCE(SUM(unread), 0) FROM mail_threads WHERE connection_id = ?",
    )?
    .query_row([conn], |r| r.get(0))?)
}

/// The workspace's bot `id` (`bot_workspaces`): `(name, role, rules)`.
pub fn bot(c: &Connection, ws: &str, id: &str) -> MailResult<Option<(String, String, String)>> {
    Ok(c.prepare_cached(
        "SELECT b.name, b.role, b.rules FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id
          WHERE w.workspace_id = ? AND b.id = ? AND b.deleted = 0",
    )?
    .query_row([ws, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .optional()?)
}

/// The main bot (the default drafter when there is no mail bot).
pub fn main_bot(c: &Connection, ws: &str) -> MailResult<Option<String>> {
    Ok(c.prepare_cached(
        "SELECT b.id FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id
          WHERE w.workspace_id = ? AND b.main = 1 AND b.deleted = 0 ORDER BY b.updated LIMIT 1",
    )?
    .query_row([ws], |r| r.get(0))
    .optional()?)
}

/// `(id, title, status)` of open items among `ids`.
pub fn open_items(c: &Connection, ids: &[String]) -> MailResult<Vec<(String, String, String)>> {
    let mut out = Vec::new();
    let mut st = c.prepare_cached(
        "SELECT id, title, status FROM items WHERE id = ? AND archived = 0 AND status NOT IN ('done', 'drop')",
    )?;
    for id in ids {
        if let Some(r) = st
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?
        {
            out.push(r);
        }
    }
    Ok(out)
}

/// A `files` row: `(workspace_id, sha256, name, mime, size)`.
pub type FileInfo = (String, String, String, String, i64);

pub fn file_row(c: &Connection, id: &str) -> MailResult<Option<FileInfo>> {
    Ok(
        c.prepare_cached("SELECT workspace_id, sha256, name, mime, size FROM files WHERE id = ?")?
            .query_row([id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()?,
    )
}

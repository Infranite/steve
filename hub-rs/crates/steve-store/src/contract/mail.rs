//! Mail folders, messages and drafts.

use sea_query::{Expr, ExprTrait, Func, Iden, LikeExpr, OnConflict, Order, Query};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::{Q, Row};
use crate::{Read, Result, W};

use super::sources::ScopeMode;

/// A folder as the provider lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderInfo {
    /// IMAP: the decoded name (modified UTF-7 → UTF-8); Gmail: the label id; Graph: the folder id.
    pub ext_id: String,
    pub name: String,
    /// `inbox|sent|drafts|archive|trash|junk|important|all|other`.
    pub role: String,
}

/// One folder's sync state (`mail_folders`) plus its scope mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderState {
    pub ext_id: String,
    pub name: String,
    pub role: String,
    pub mode: ScopeMode,
    pub uidvalidity: Option<i64>,
    pub last_uid: Option<i64>,
    pub modseq: Option<i64>,
    /// Graph delta link; IMAP's unresolved `(uid, attempts)` JSON tuple, if any.
    pub cursor: Option<String>,
    /// Unix ms of the last completed sync of this folder; `None` = never (the next sync backfills).
    pub synced: Option<i64>,
    /// Unix ms of the last flag re-check (servers without CONDSTORE), kept in memory by the job.
    pub flags_checked: Option<i64>,
}

/// An attachment as seen in a `BODYSTRUCTURE` (messages too big to fetch whole).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartMeta {
    /// MIME part path, `"1.2"`.
    pub part: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub inline: bool,
}

fn json_vec(s: Option<String>) -> Vec<String> {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn json_val(s: Option<String>) -> Value {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Array(Vec::new()))
}

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

const THREAD_COLS: [&str; 12] = [
    "id",
    "workspace_id",
    "connection_id",
    "thread_key",
    "subject",
    "last_at",
    "count",
    "unread",
    "participants",
    "state",
    "visibility",
    "created_by",
];

fn thread_row(r: &Row<'_>) -> Result<ThreadRow> {
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

const MSG_COLS: [&str; 32] = [
    "id",
    "workspace_id",
    "connection_id",
    "thread_id",
    "ext_id",
    "message_id",
    "in_reply_to",
    "refs",
    "folders",
    "flags",
    "direction",
    "from_addr",
    "from_name",
    "from_account_id",
    "from_party_id",
    "reply_to",
    "to_addrs",
    "cc_addrs",
    "subject",
    "snippet",
    "date",
    "size",
    "has_attachments",
    "bulk",
    "body_text",
    "body_new",
    "has_html",
    "raw_path",
    "purged",
    "event_id",
    "status",
    "created",
];

fn msg_row(r: &Row<'_>) -> Result<MessageRow> {
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
        has_attachments: r.get::<i64>(22)? != 0,
        bulk: r.get::<i64>(23)? != 0,
        body_text: r.get(24)?,
        body_new: r.get(25)?,
        has_html: r.get::<i64>(26)? != 0,
        raw_path: r.get(27)?,
        purged: r.get(28)?,
        event_id: r.get(29)?,
        status: r.get(30)?,
        created: r.get(31)?,
    })
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

/// `(id, name, mime, size, inline)`.
pub type AttachmentRow = (String, String, String, i64, bool);

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
    pub files: Vec<crate::FileRef>,
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

const DRAFT_COLS: [&str; 26] = [
    "id",
    "workspace_id",
    "connection_id",
    "thread_id",
    "reply_to_message",
    "mode",
    "to_addrs",
    "cc_addrs",
    "bcc_addrs",
    "subject",
    "body",
    "files",
    "author",
    "item_id",
    "session_id",
    "run_id",
    "decision_id",
    "basis",
    "content_hash",
    "status",
    "send_at",
    "sent_message",
    "error",
    "created",
    "updated",
    "sent",
];

fn rcpts(s: Option<String>) -> Vec<Rcpt> {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn draft_row(r: &Row<'_>) -> Result<DraftRow> {
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
            .get::<Option<String>>(11)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        author: r.get(12)?,
        item_id: r.get(13)?,
        session_id: r.get(14)?,
        run_id: r.get(15)?,
        decision_id: r.get(16)?,
        basis: r
            .get::<Option<String>>(17)?
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

#[derive(Iden, Clone, Copy)]
enum MailFolders {
    Table,
    ConnectionId,
    WorkspaceId,
    ExtId,
    Name,
    Role,
    Uidvalidity,
    LastUid,
    Modseq,
    Cursor,
    Synced,
}

pub fn folders(c: &mut impl Read, conn: &str) -> Result<Vec<FolderState>> {
    use MailFolders::*;
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns([
                ExtId,
                Name,
                Role,
                Uidvalidity,
                LastUid,
                Modseq,
                Cursor,
                Synced,
            ])
            .from(Table)
            .and_where(Expr::col(ConnectionId).eq(conn))
            .order_by_expr(dialect.delivery_order("mail_folders"), Order::Asc),
        |r| {
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
        },
    )
}

/// Adds a folder the provider listed, or updates its name and role.
pub fn upsert_folder(tx: &mut W<'_>, ws: &str, conn: &str, f: &FolderInfo) -> Result<()> {
    use MailFolders::*;
    tx.exec(
        Query::insert()
            .into_table(Table)
            .columns([ConnectionId, WorkspaceId, ExtId, Name, Role])
            .values_panic([
                Expr::val(conn),
                Expr::val(ws),
                Expr::val(f.ext_id.as_str()),
                Expr::val(f.name.as_str()),
                Expr::val(f.role.as_str()),
            ])
            .on_conflict(
                OnConflict::columns([ConnectionId, ExtId])
                    .update_columns([Name, Role])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn save_folder(tx: &mut W<'_>, conn: &str, f: &FolderState) -> Result<()> {
    use MailFolders::*;
    tx.exec(
        Query::update()
            .table(Table)
            .value(Uidvalidity, f.uidvalidity)
            .value(LastUid, f.last_uid)
            .value(Modseq, f.modseq)
            .value(Cursor, f.cursor.as_deref())
            .value(Synced, f.synced)
            .and_where(Expr::col(ConnectionId).eq(conn))
            .and_where(Expr::col(ExtId).eq(f.ext_id.as_str())),
    )?;
    Ok(())
}

pub fn settings(c: &mut impl Read, conn: &str) -> Result<Option<AccountSettings>> {
    c.one(
        Query::select()
            .columns([
                "signature",
                "duty",
                "send_delay_s",
                "archive_on_done",
                "sync_days",
                "append_sent",
                "revision",
                "updated",
            ])
            .from("mail_account_settings")
            .and_where(Expr::col("connection_id").eq(conn)),
        |r| {
            Ok(AccountSettings {
                signature: r.get(0)?,
                duty: r.get(1)?,
                send_delay_s: r.get(2)?,
                archive_on_done: r.get::<i64>(3)? != 0,
                sync_days: r.get(4)?,
                append_sent: r.get(5)?,
                revision: r.get(6)?,
                updated: r.get(7)?,
            })
        },
    )
}

/// Creates the settings row with defaults if missing.
pub fn ensure_settings(tx: &mut W<'_>, ws: &str, conn: &str, now: i64) -> Result<()> {
    tx.exec(
        Query::insert()
            .into_table("mail_account_settings")
            .columns(["connection_id", "workspace_id", "updated"])
            .values_panic([Expr::val(conn), Expr::val(ws), Expr::val(now)])
            .on_conflict(
                OnConflict::columns(["connection_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )?;
    Ok(())
}

/// Writes every field and bumps the revision.
pub fn update_settings(tx: &mut W<'_>, conn: &str, s: &AccountSettings, now: i64) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_account_settings")
            .value("signature", s.signature.as_str())
            .value("duty", s.duty.as_str())
            .value("send_delay_s", s.send_delay_s)
            .value("archive_on_done", s.archive_on_done as i64)
            .value("sync_days", s.sync_days)
            .value("append_sent", s.append_sent.as_str())
            .value("revision", Expr::col("revision").add(1_i64))
            .value("updated", now)
            .and_where(Expr::col("connection_id").eq(conn)),
    )?;
    Ok(())
}

pub fn thread(c: &mut impl Read, id: &str) -> Result<Option<ThreadRow>> {
    c.one(
        Query::select()
            .columns(THREAD_COLS)
            .from("mail_threads")
            .and_where(Expr::col("id").eq(id)),
        thread_row,
    )
}

pub fn thread_by_key(c: &mut impl Read, conn: &str, key: &str) -> Result<Option<ThreadRow>> {
    c.one(
        Query::select()
            .columns(THREAD_COLS)
            .from("mail_threads")
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("thread_key").eq(key)),
        thread_row,
    )
}

pub fn insert_thread(tx: &mut W<'_>, t: &ThreadRow, now: i64) -> Result<()> {
    tx.exec(
        Query::insert()
            .into_table("mail_threads")
            .columns([
                "id",
                "workspace_id",
                "connection_id",
                "thread_key",
                "subject",
                "last_at",
                "count",
                "unread",
                "participants",
                "state",
                "visibility",
                "created_by",
                "created",
                "updated",
            ])
            .values_panic([
                Expr::val(t.id.as_str()),
                Expr::val(t.workspace_id.as_str()),
                Expr::val(t.connection_id.as_str()),
                Expr::val(t.thread_key.as_str()),
                Expr::val(t.subject.as_str()),
                Expr::val(t.last_at),
                Expr::val(0_i64),
                Expr::val(0_i64),
                Expr::val("[]"),
                Expr::val(t.state.as_str()),
                Expr::val(t.visibility.as_str()),
                Expr::val(t.created_by.as_str()),
                Expr::val(now),
                Expr::val(now),
            ]),
    )?;
    Ok(())
}

pub fn set_thread_state(tx: &mut W<'_>, id: &str, state: &str, now: i64) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_threads")
            .value("state", state)
            .value("updated", now)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn message(c: &mut impl Read, id: &str) -> Result<Option<MessageRow>> {
    c.one(
        Query::select()
            .columns(MSG_COLS)
            .from("mail_messages")
            .and_where(Expr::col("id").eq(id)),
        msg_row,
    )
}

pub fn message_by_ext(c: &mut impl Read, conn: &str, ext: &str) -> Result<Option<MessageRow>> {
    c.one(
        Query::select()
            .columns(MSG_COLS)
            .from("mail_messages")
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("ext_id").eq(ext)),
        msg_row,
    )
}

/// The messages of a connection with this Message-ID.
pub fn messages_by_mid(
    c: &mut impl Read,
    ws: &str,
    conn: &str,
    mid: &str,
) -> Result<Vec<MessageRow>> {
    c.all(
        Query::select()
            .columns(MSG_COLS)
            .from("mail_messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("message_id").eq(mid))
            .and_where(Expr::col("connection_id").eq(conn)),
        msg_row,
    )
}

/// A message without Message-ID with the same sender, date and subject (dedupe on rescans).
pub fn message_by_shape(
    c: &mut impl Read,
    conn: &str,
    from: &str,
    date: i64,
    subject: &str,
) -> Result<Option<MessageRow>> {
    c.one(
        Query::select()
            .columns(MSG_COLS)
            .from("mail_messages")
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("message_id").is_null())
            .and_where(Expr::col("from_addr").eq(from))
            .and_where(Expr::col("date").eq(date))
            .and_where(Expr::col("subject").eq(subject))
            .limit(1),
        msg_row,
    )
}

/// [`thread_messages`] narrowed to [`BriefMsg`]: no bodies, references or recipients.
pub fn thread_brief_rows(c: &mut impl Read, thread: &str) -> Result<Vec<BriefMsg>> {
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns([
                "direction",
                "from_name",
                "from_addr",
                "from_party_id",
                "snippet",
                "has_attachments",
                "event_id",
            ])
            .from("mail_messages")
            .and_where(Expr::col("thread_id").eq(thread))
            .order_by("date", Order::Asc)
            .order_by_expr(dialect.delivery_order("mail_messages"), Order::Asc),
        |r| {
            Ok(BriefMsg {
                direction: r.get(0)?,
                from_name: r.get(1)?,
                from_addr: r.get(2)?,
                from_party_id: r.get(3)?,
                snippet: r.get(4)?,
                has_attachments: r.get::<i64>(5)? != 0,
                event_id: r.get(6)?,
            })
        },
    )
}

pub fn thread_messages(c: &mut impl Read, thread: &str) -> Result<Vec<MessageRow>> {
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns(MSG_COLS)
            .from("mail_messages")
            .and_where(Expr::col("thread_id").eq(thread))
            .order_by("date", Order::Asc)
            .order_by_expr(dialect.delivery_order("mail_messages"), Order::Asc),
        msg_row,
    )
}

pub fn insert_message(tx: &mut W<'_>, m: &MessageRow) -> Result<()> {
    let j = |v: &Vec<String>| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
    tx.exec(
        Query::insert()
            .into_table("mail_messages")
            .columns(MSG_COLS)
            .values_panic([
                Expr::val(m.id.as_str()),
                Expr::val(m.workspace_id.as_str()),
                Expr::val(m.connection_id.as_str()),
                Expr::val(m.thread_id.as_str()),
                Expr::val(m.ext_id.as_str()),
                Expr::val(m.message_id.as_deref()),
                Expr::val(m.in_reply_to.as_deref()),
                Expr::val(j(&m.refs)),
                Expr::val(j(&m.folders)),
                Expr::val(j(&m.flags)),
                Expr::val(m.direction.as_str()),
                Expr::val(m.from_addr.as_str()),
                Expr::val(m.from_name.as_str()),
                Expr::val(m.from_account_id.as_deref()),
                Expr::val(m.from_party_id.as_deref()),
                Expr::val(m.reply_to.as_deref()),
                Expr::val(m.to_addrs.to_string()),
                Expr::val(m.cc_addrs.to_string()),
                Expr::val(m.subject.as_str()),
                Expr::val(m.snippet.as_str()),
                Expr::val(m.date),
                Expr::val(m.size),
                Expr::val(m.has_attachments as i64),
                Expr::val(m.bulk as i64),
                Expr::val(m.body_text.as_deref()),
                Expr::val(m.body_new.as_deref()),
                Expr::val(m.has_html as i64),
                Expr::val(m.raw_path.as_deref()),
                Expr::val(m.purged),
                Expr::val(m.event_id.as_deref()),
                Expr::val(m.status.as_str()),
                Expr::val(m.created),
            ]),
    )?;
    steve_store_sqlite::digest::sync_message(
        &tx.legacy(),
        steve_store_sqlite::digest::Source::MailMessage,
        &m.id,
        None,
    )?;
    Ok(())
}

/// Another copy of a known message: its current id, folders and flags.
pub fn update_copy(
    tx: &mut W<'_>,
    id: &str,
    ext: &str,
    folders: &[String],
    flags: &[String],
) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_messages")
            .value("ext_id", ext)
            .value(
                "folders",
                serde_json::to_string(folders).unwrap_or_else(|_| "[]".into()),
            )
            .value(
                "flags",
                serde_json::to_string(flags).unwrap_or_else(|_| "[]".into()),
            )
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_flags(tx: &mut W<'_>, id: &str, flags: &[String]) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_messages")
            .value(
                "flags",
                serde_json::to_string(flags).unwrap_or_else(|_| "[]".into()),
            )
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_folders(tx: &mut W<'_>, id: &str, folders: &[String]) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_messages")
            .value(
                "folders",
                serde_json::to_string(folders).unwrap_or_else(|_| "[]".into()),
            )
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_event(tx: &mut W<'_>, id: &str, event: &str) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_messages")
            .value("event_id", event)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

/// Whether one of these Message-IDs is a message I sent from this connection.
pub fn is_mine(c: &mut impl Read, conn: &str, mid: &str) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("mail_messages")
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("message_id").eq(mid))
            .and_where(Expr::col("direction").eq("out"))
            .limit(1),
        |_| Ok(()),
    )
    .map(|o| o.is_some())
}

pub fn insert_attachment(
    tx: &mut W<'_>,
    ws: &str,
    message: &str,
    a: &PartMeta,
    now: i64,
) -> Result<String> {
    let id = crate::uid("mla");
    tx.exec(
        Query::insert()
            .into_table("mail_attachments")
            .columns([
                "id",
                "workspace_id",
                "message_id",
                "part",
                "name",
                "mime",
                "size",
                "content_id",
                "inline",
                "created",
            ])
            .values_panic([
                Expr::val(id.as_str()),
                Expr::val(ws),
                Expr::val(message),
                Expr::val(a.part.as_str()),
                Expr::val(a.name.as_str()),
                Expr::val(a.mime.as_str()),
                Expr::val(a.size),
                Expr::val(a.content_id.as_deref()),
                Expr::val(a.inline as i64),
                Expr::val(now),
            ]),
    )?;
    Ok(id)
}

/// A message's attachments.
pub fn attachments(c: &mut impl Read, message: &str) -> Result<Vec<AttachmentRow>> {
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns(["id", "name", "mime", "size", "inline"])
            .from("mail_attachments")
            .and_where(Expr::col("message_id").eq(message))
            .order_by_expr(dialect.delivery_order("mail_attachments"), Order::Asc),
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<i64>(4)? != 0,
            ))
        },
    )
}

pub fn purge(tx: &mut W<'_>, id: &str, now: i64) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_messages")
            .value("raw_path", None::<String>)
            .value("body_text", None::<String>)
            .value("body_new", None::<String>)
            .value("purged", now)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

/// The workspaces that have mail.
pub fn mail_workspaces(c: &mut impl Read) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .columns(["workspace_id"])
            .distinct()
            .from("mail_messages"),
        |r| r.get(0),
    )
}

/// The raw files of a connection's messages.
pub fn raw_paths(c: &mut impl Read, conn: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .columns(["raw_path"])
            .from("mail_messages")
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("raw_path").is_not_null()),
        |r| r.get(0),
    )
}

pub fn draft(c: &mut impl Read, id: &str) -> Result<Option<DraftRow>> {
    c.one(
        Query::select()
            .columns(DRAFT_COLS)
            .from("mail_drafts")
            .and_where(Expr::col("id").eq(id)),
        draft_row,
    )
}

/// The drafts of a thread that are not discarded, oldest first.
pub fn thread_drafts(c: &mut impl Read, thread: &str) -> Result<Vec<DraftRow>> {
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns(DRAFT_COLS)
            .from("mail_drafts")
            .and_where(Expr::col("thread_id").eq(thread))
            .and_where(Expr::col("status").ne("discarded"))
            .order_by("created", Order::Asc)
            .order_by_expr(dialect.delivery_order("mail_drafts"), Order::Asc),
        draft_row,
    )
}

pub fn insert_draft(tx: &mut W<'_>, d: &DraftRow) -> Result<()> {
    tx.exec(
        Query::insert()
            .into_table("mail_drafts")
            .columns(DRAFT_COLS)
            .values_panic([
                Expr::val(d.id.as_str()),
                Expr::val(d.workspace_id.as_str()),
                Expr::val(d.connection_id.as_str()),
                Expr::val(d.thread_id.as_deref()),
                Expr::val(d.reply_to_message.as_deref()),
                Expr::val(d.mode.as_str()),
                Expr::val(to_json(&d.to)),
                Expr::val(to_json(&d.cc)),
                Expr::val(to_json(&d.bcc)),
                Expr::val(d.subject.as_str()),
                Expr::val(d.body.as_str()),
                Expr::val(to_json(&d.files)),
                Expr::val(d.author.as_str()),
                Expr::val(d.item_id.as_deref()),
                Expr::val(d.session_id.as_deref()),
                Expr::val(d.run_id.as_deref()),
                Expr::val(d.decision_id.as_deref()),
                Expr::val(d.basis.as_ref().map(Value::to_string)),
                Expr::val(d.content_hash.as_str()),
                Expr::val(d.status.as_str()),
                Expr::val(d.send_at),
                Expr::val(d.sent_message.as_deref()),
                Expr::val(d.error.as_deref()),
                Expr::val(d.created),
                Expr::val(d.updated),
                Expr::val(d.sent),
            ]),
    )?;
    Ok(())
}

/// Writes the editable content (recipients, subject, body, files, thread links).
pub fn update_draft_content(tx: &mut W<'_>, d: &DraftRow, now: i64) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_drafts")
            .value("mode", d.mode.as_str())
            .value("thread_id", d.thread_id.as_deref())
            .value("reply_to_message", d.reply_to_message.as_deref())
            .value("to_addrs", to_json(&d.to))
            .value("cc_addrs", to_json(&d.cc))
            .value("bcc_addrs", to_json(&d.bcc))
            .value("subject", d.subject.as_str())
            .value("body", d.body.as_str())
            .value("files", to_json(&d.files))
            .value("content_hash", d.content_hash.as_str())
            .value("basis", d.basis.as_ref().map(Value::to_string))
            .value("updated", now)
            .and_where(Expr::col("id").eq(d.id.as_str())),
    )?;
    Ok(())
}

/// Sets the send time (approval) or clears it (undo).
pub fn set_draft_send_at(tx: &mut W<'_>, id: &str, send_at: Option<i64>) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_drafts")
            .value("send_at", send_at)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_draft_decision(
    tx: &mut W<'_>,
    id: &str,
    decision: Option<&str>,
    hash: &str,
) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_drafts")
            .value("decision_id", decision)
            .value("content_hash", hash)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_draft_error(tx: &mut W<'_>, id: &str, error: Option<&str>) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_drafts")
            .value("error", error)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_draft_sent(tx: &mut W<'_>, id: &str, message: &str, now: i64) -> Result<()> {
    tx.exec(
        Query::update()
            .table("mail_drafts")
            .value("status", "sent")
            .value("sent_message", message)
            .value("sent", now)
            .value("error", None::<String>)
            .value("updated", now)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}
/// Recomputes the thread's counters and participants from its messages.
pub fn refresh_thread(w: &mut W<'_>, id: &str, now: i64) -> Result<()> {
    let rows = w.all(
        Query::select()
            .columns([
                "direction",
                "flags",
                "from_account_id",
                "to_addrs",
                "cc_addrs",
                "date",
            ])
            .from("mail_messages")
            .and_where(Expr::col("thread_id").eq(id))
            .order_by("date", Order::Asc),
        |r| {
            Ok((
                r.get::<String>(0)?,
                json_vec(r.get(1)?),
                r.get::<Option<String>>(2)?,
                json_val(r.get(3)?),
                json_val(r.get(4)?),
                r.get::<i64>(5)?,
            ))
        },
    )?;
    let count = rows.len() as i64;
    let last_at = rows.last().map_or(0, |r| r.5);
    let mut unread = 0_i64;
    let mut participants = Vec::new();
    for (dir, flags, from, to, cc, _) in rows {
        if dir == "in" && !flags.iter().any(|f| f == "seen") {
            unread += 1;
        }
        let recipients = [to, cc].into_iter().flat_map(|v| {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| {
                    x.get("account_id")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .collect::<Vec<_>>()
        });
        for who in from.into_iter().chain(recipients) {
            if !participants.contains(&who) {
                participants.push(who);
            }
        }
    }
    let last_at = w
        .dialect()
        .greatest(Expr::col("last_at"), Expr::val(last_at));
    w.exec(
        Query::update()
            .table("mail_threads")
            .value("count", count)
            .value("unread", unread)
            .value("last_at", last_at)
            .value("participants", to_json(&participants))
            .value("updated", now)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn thread_of_ids(
    c: &mut impl Read,
    ws: &str,
    conn: &str,
    ids: &[String],
) -> Result<Option<String>> {
    for id in ids {
        if let Some(thread) = c.one(
            Query::select()
                .column("thread_id")
                .from("mail_messages")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("message_id").eq(id.as_str()))
                .and_where(Expr::col("connection_id").eq(conn))
                .limit(1),
            |r| r.get(0),
        )? {
            return Ok(Some(thread));
        }
    }
    Ok(None)
}

pub fn expired(
    c: &mut impl Read,
    ws: &str,
    before: i64,
    after: Option<(i64, &str)>,
    limit: i64,
) -> Result<Vec<(String, Option<String>, i64)>> {
    let mut query = Query::select();
    query
        .columns(["id", "raw_path", "date"])
        .from("mail_messages")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("date").lt(before))
        .and_where(Expr::col("purged").is_null())
        .order_by_columns([("date", Order::Asc), ("id", Order::Asc)])
        .limit(limit as u64);
    if let Some((date, id)) = after {
        query.and_where(
            Expr::col("date")
                .gt(date)
                .or(Expr::col("date").eq(date).and(Expr::col("id").gt(id))),
        );
    }
    c.all(&query, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
}

pub fn delete_connection(w: &mut W<'_>, conn: &str) -> Result<()> {
    let digest_messages = steve_store_sqlite::digest::Source::MailMessage
        .messages(
            &w.legacy(),
            steve_store_sqlite::digest::MessageFilter::Connection(conn),
        )?
        .into_iter()
        .map(|m| (m.scope, m.id))
        .collect::<Vec<_>>();
    let messages = Query::select()
        .column("id")
        .from("mail_messages")
        .and_where(Expr::col("connection_id").eq(conn))
        .take();
    w.exec(
        Query::delete()
            .from_table("mail_attachments")
            .and_where(Expr::col("message_id").in_subquery(messages)),
    )?;
    for name in [
        "mail_drafts",
        "mail_messages",
        "mail_threads",
        "mail_folders",
        "mail_account_settings",
    ] {
        w.exec(
            Query::delete()
                .from_table(name)
                .and_where(Expr::col("connection_id").eq(conn)),
        )?;
    }
    steve_store_sqlite::digest::remove_message(&w.legacy(), digest_messages)?;
    Ok(())
}

pub fn due_drafts(c: &mut impl Read, now: i64, limit: i64) -> Result<Vec<(String, String)>> {
    let waiting = super::connections::waiting_ids(c)?;
    let waiting = c.db().dialect().text_array(&waiting)?;
    c.all(
        Query::select()
            .columns(["id", "connection_id"])
            .from("mail_drafts")
            .and_where(Expr::col("status").eq("approved"))
            .and_where(Expr::col("send_at").lte(now))
            .and_where(Expr::col("connection_id").not_in_subquery(waiting))
            .order_by("send_at", Order::Asc)
            .limit(limit as u64),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

pub fn draft_transition(
    w: &mut W<'_>,
    id: &str,
    from: &[&str],
    to: &str,
    now: i64,
) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("mail_drafts")
            .value("status", to)
            .value("updated", now)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").is_in(from.iter().copied())),
    )? > 0)
}

pub fn unread_of(c: &mut impl Read, conn: &str) -> Result<i64> {
    Ok(c.one(
        Query::select()
            .expr(Func::sum(Expr::col("unread")))
            .from("mail_threads")
            .and_where(Expr::col("connection_id").eq(conn)),
        |r| r.get::<Option<i64>>(0),
    )?
    .flatten()
    .unwrap_or(0))
}
/// A folder of the mail UI, applied before cursor and limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    Inbox,
    NeedsReply,
    Waiting,
    Sent,
    Drafts,
    All,
}

pub fn list_threads(
    c: &mut impl Read,
    viewer: &crate::base::Viewer,
    view: View,
    account: Option<&str>,
    text: Option<&str>,
    before: Option<i64>,
    limit: u64,
) -> Result<Vec<ThreadRow>> {
    let dialect = c.db().dialect();
    let mut q = Query::select();
    q.columns(THREAD_COLS.map(|column| ("t", column)))
        .from_as("mail_threads", "t")
        .and_where(Expr::col(("t", "workspace_id")).eq(viewer.ws.as_str()))
        .order_by_columns([(("t", "last_at"), Order::Desc), (("t", "id"), Order::Desc)])
        .limit(limit);
    if viewer.is_service() {
        q.and_where(Expr::col(("t", "visibility")).eq("workspace"));
    }
    if let Some(account) = account {
        q.and_where(Expr::col(("t", "connection_id")).eq(account));
    }
    if let Some(before) = before {
        q.and_where(Expr::col(("t", "last_at")).lt(before));
    }

    let mut messages = Query::select();
    messages
        .expr(Expr::cust("1"))
        .from_as("mail_messages", "m")
        .and_where(Expr::col(("m", "thread_id")).eq(Expr::col(("t", "id"))));
    let state = Expr::col(("t", "state"));
    match view {
        View::Inbox => {
            let folders = dialect
                .json_rows(Expr::col(("m", "folders")), "j")
                .and_where(Expr::col(("j", "value")).eq(Expr::col(("f", "ext_id"))))
                .take();
            q.and_where(state.ne("muted")).and_where(Expr::exists(
                messages
                    .clone()
                    .join_as(
                        sea_query::JoinType::InnerJoin,
                        "mail_folders",
                        "f",
                        Expr::col(("f", "connection_id")).eq(Expr::col(("m", "connection_id"))),
                    )
                    .and_where(Expr::col(("f", "role")).eq("inbox"))
                    .and_where(Expr::exists(folders))
                    .take(),
            ));
        }
        View::NeedsReply => {
            q.and_where(state.eq("needs_reply"));
        }
        View::Waiting => {
            q.and_where(state.eq("waiting"));
        }
        View::Sent => {
            q.and_where(Expr::exists(
                messages
                    .clone()
                    .and_where(Expr::col(("m", "direction")).eq("out"))
                    .take(),
            ));
        }
        View::Drafts => {
            q.and_where(Expr::exists(
                Query::select()
                    .expr(Expr::cust("1"))
                    .from_as("mail_drafts", "d")
                    .and_where(Expr::col(("d", "thread_id")).eq(Expr::col(("t", "id"))))
                    .and_where(Expr::col(("d", "status")).is_in([
                        "draft", "pending", "approved", "sending", "failed", "unknown",
                    ]))
                    .take(),
            ));
        }
        View::All => {}
    }
    if let Some(text) = text {
        let mut pattern = String::from("%");
        for ch in text.chars() {
            if matches!(ch, '%' | '_' | '\\') {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        pattern.push('%');
        let pattern = LikeExpr::new(pattern).escape('\\');
        let like = |table, col| Expr::col((table, col)).like(pattern.clone());
        q.and_where(
            like("t", "subject").or(Expr::exists(
                messages
                    .and_where(
                        like("m", "from_name")
                            .or(like("m", "from_addr"))
                            .or(like("m", "snippet")),
                    )
                    .take(),
            )),
        );
    }
    c.all(&q, thread_row)
}

pub fn latest_draft(c: &mut impl Read, thread: &str) -> Result<Option<(String, String)>> {
    let order = c.db().dialect().delivery_order("mail_drafts");
    c.one(
        Query::select()
            .columns(["id", "status"])
            .from("mail_drafts")
            .and_where(Expr::col("thread_id").eq(thread))
            .and_where(Expr::col("status").is_not_in(["sent", "discarded"]))
            .order_by("updated", Order::Desc)
            .order_by_expr(order, Order::Desc)
            .limit(1),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

pub fn draft_of_run(c: &mut impl Read, ws: &str, run: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("id")
            .from("mail_drafts")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("run_id").eq(run))
            .order_by("created", Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

pub fn item_draft(c: &mut impl Read, item: &str, thread: &str) -> Result<Option<DraftRow>> {
    let order = c.db().dialect().delivery_order("mail_drafts");
    c.one(
        Query::select()
            .columns(DRAFT_COLS)
            .from("mail_drafts")
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("thread_id").eq(thread))
            .and_where(Expr::col("status").is_in([
                "draft", "pending", "approved", "sending", "failed", "unknown",
            ]))
            .order_by("created", Order::Desc)
            .order_by_expr(order, Order::Desc)
            .limit(1),
        draft_row,
    )
}

pub fn set_author(w: &mut W<'_>, id: &str, author: &str, session: Option<&str>) -> Result<()> {
    w.exec(
        Query::update()
            .table("mail_drafts")
            .value("author", author)
            .value("session_id", session)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_draft_basis(w: &mut W<'_>, id: &str, basis: &Value) -> Result<()> {
    set_optional_draft_basis(w, id, Some(basis))
}

pub fn set_optional_draft_basis(w: &mut W<'_>, id: &str, basis: Option<&Value>) -> Result<()> {
    w.exec(
        Query::update()
            .table("mail_drafts")
            .value("basis", basis.map(Value::to_string))
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn set_checked_basis(w: &mut W<'_>, d: &DraftRow, basis: &Value, now: i64) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("mail_drafts")
            .value("basis", basis.to_string())
            .value("updated", std::cmp::max(now, d.updated + 1))
            .and_where(Expr::col("id").eq(d.id.as_str()))
            .and_where(Expr::col("updated").eq(d.updated))
            .and_where(Expr::col("status").is_in(["draft", "pending"])),
    )? == 1)
}

pub fn purge_sent_bodies(w: &mut W<'_>, before: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("mail_drafts")
            .value("body", "")
            .and_where(Expr::col("status").eq("sent"))
            .and_where(Expr::col("sent").lt(before))
            .and_where(Expr::col("body").ne("")),
    )?;
    Ok(())
}

pub fn set_draft_thread(w: &mut W<'_>, id: &str, thread: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("mail_drafts")
            .value("thread_id", thread)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn sending_drafts(c: &mut impl Read) -> Result<Vec<DraftRow>> {
    c.all(
        Query::select()
            .columns(DRAFT_COLS)
            .from("mail_drafts")
            .and_where(Expr::col("status").eq("sending")),
        draft_row,
    )
}

pub fn unknown_drafts_without_open_decision(c: &mut impl Read) -> Result<Vec<String>> {
    let opened = super::decisions::open_mail_draft_ids(c)?;
    let excluded = c.db().dialect().text_array(&opened)?;
    c.all(
        Query::select()
            .column("id")
            .from("mail_drafts")
            .and_where(Expr::col("status").eq("unknown"))
            .and_where(Expr::col("id").not_in_subquery(excluded))
            .limit(100),
        |r| r.get(0),
    )
}

#[derive(Debug, Clone)]
pub struct PartRow {
    pub id: String,
    pub workspace_id: String,
    pub message_id: String,
    pub part: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub inline: bool,
    pub file_id: Option<String>,
}

const PART_COLS: [&str; 10] = [
    "id",
    "workspace_id",
    "message_id",
    "part",
    "name",
    "mime",
    "size",
    "content_id",
    "inline",
    "file_id",
];
fn part_row(r: &Row<'_>) -> Result<PartRow> {
    Ok(PartRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        message_id: r.get(2)?,
        part: r.get(3)?,
        name: r.get(4)?,
        mime: r.get(5)?,
        size: r.get(6)?,
        content_id: r.get(7)?,
        inline: r.get::<i64>(8)? != 0,
        file_id: r.get(9)?,
    })
}

pub fn attachment(c: &mut impl Read, id: &str) -> Result<Option<PartRow>> {
    c.one(
        Query::select()
            .columns(PART_COLS)
            .from("mail_attachments")
            .and_where(Expr::col("id").eq(id)),
        part_row,
    )
}

pub fn parts_of(c: &mut impl Read, message: &str) -> Result<Vec<PartRow>> {
    let order = c.db().dialect().delivery_order("mail_attachments");
    c.all(
        Query::select()
            .columns(PART_COLS)
            .from("mail_attachments")
            .and_where(Expr::col("message_id").eq(message))
            .order_by_expr(order, Order::Asc),
        part_row,
    )
}

pub fn set_file_id(w: &mut W<'_>, id: &str, file: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("mail_attachments")
            .value("file_id", file)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn party_timeline(
    c: &mut impl Read,
    viewer: &crate::base::Viewer,
    party: &str,
    before: Option<i64>,
    limit: u64,
) -> Result<Vec<(String, i64, String, String)>> {
    let ids = super::parties::merged_ids(c, party)?;
    let dialect = c.db().dialect();
    let ids = dialect.text_array(&ids)?;
    let has_party = |column| {
        Expr::exists(
            dialect
                .json_rows(Expr::col(("m", column)), "j")
                .and_where(
                    dialect
                        .json_field(Expr::col(("j", "value")), "party_id")
                        .in_subquery(ids.clone()),
                )
                .take(),
        )
    };
    let mut q = Query::select();
    q.columns([
        ("m", "thread_id"),
        ("m", "date"),
        ("m", "direction"),
        ("m", "subject"),
    ])
    .from_as("mail_messages", "m")
    .join_as(
        sea_query::JoinType::InnerJoin,
        "mail_threads",
        "t",
        Expr::col(("t", "id")).eq(Expr::col(("m", "thread_id"))),
    )
    .and_where(Expr::col(("m", "workspace_id")).eq(viewer.ws.as_str()))
    .and_where(
        Expr::col(("m", "from_party_id"))
            .in_subquery(ids.clone())
            .or(has_party("to_addrs"))
            .or(has_party("cc_addrs")),
    )
    .order_by(("m", "date"), Order::Desc)
    .limit(limit);
    if let Some(before) = before {
        q.and_where(Expr::col(("m", "date")).lt(before));
    }
    if viewer.is_service() {
        q.and_where(Expr::col(("t", "visibility")).eq("workspace"));
    }
    c.all(&q, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
}

#[cfg(feature = "testkit")]
pub mod fixtures;

pub fn unknown_draft_count(r: &mut impl Read) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(sea_query::Func::count(Expr::col("id")))
            .from("mail_drafts")
            .and_where(Expr::col("status").eq("unknown")),
        |row| row.get(0),
    )?
    .unwrap_or_default())
}

//! What a mail adapter implements and what it exchanges with the sync job.
//!
//! An adapter never touches the database: the job loads a [`SyncState`] (folders, cursors, scope
//! modes), the adapter advances it while handing every message to a [`MailSink`], and the job writes
//! the state back. The same MIME path ([`crate::parse`]) handles every provider.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use steve_modules::accounts::connections::{ConnCx, ConnectionProvider};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{AdapterError, MailError};

/// How a folder's mail is received (`source_settings.scopes[].mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeMode {
    /// Stored and turned into events.
    Receive,
    /// Stored, no events.
    Record,
    /// Not synced.
    Ignore,
}

impl ScopeMode {
    /// The default for a new folder: the inbox `receive`, sent mail `record`, the rest `ignore`.
    pub fn default_for(role: &str) -> Self {
        match role {
            "inbox" => Self::Receive,
            "sent" => Self::Record,
            _ => Self::Ignore,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Receive => "receive",
            Self::Record => "record",
            Self::Ignore => "ignore",
        }
    }
}

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
    /// Graph delta link.
    pub cursor: Option<String>,
    /// Unix ms of the last completed sync of this folder; `None` = never (the next sync backfills).
    pub synced: Option<i64>,
    /// Unix ms of the last flag re-check (servers without CONDSTORE), kept in memory by the job.
    pub flags_checked: Option<i64>,
}

/// Everything an adapter needs to continue where it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    /// The folders to sync (scope not `ignore`).
    pub folders: Vec<FolderState>,
    /// Account-wide cursor (`source_settings.cursor`, Gmail historyId).
    pub cursor: Option<String>,
    /// How many days the first sync looks back.
    pub sync_days: i64,
    /// Unix ms; the job's clock.
    pub now: i64,
}

/// What a sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Messages handed to the sink.
    pub fetched: usize,
    pub flag_updates: usize,
    pub removed: usize,
    /// Folders whose UIDVALIDITY changed (rescanned).
    pub reset: Vec<String>,
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

/// One message from the provider.
#[derive(Debug, Clone)]
pub struct FetchedMessage {
    /// IMAP `"<folder>:<uidvalidity>:<uid>"`, Gmail/Graph message id.
    pub ext_id: String,
    /// The folder's `ext_id`.
    pub folder: String,
    pub mode: ScopeMode,
    /// The whole MIME message, or only its header when `partial`.
    pub raw: Vec<u8>,
    /// Too big (>25 MB): `raw` is the header, `parts` the attachments from the structure.
    pub partial: bool,
    pub parts: Vec<PartMeta>,
    /// Normalized: `seen`, `flagged`, `answered`.
    pub flags: Vec<String>,
    /// Unix ms (IMAP INTERNALDATE).
    pub internal_date: Option<i64>,
    pub size: i64,
    /// The provider's thread id (Gmail `X-GM-THRID`, threadId, conversationId).
    pub provider_thread: Option<String>,
    /// Fetched by the first look-back of a folder: stored, never triggers flows.
    pub backfill: bool,
}

/// Where an adapter delivers what it fetched, a fetched chunk at a time. The store parses, writes
/// files and SQLite off the runtime.
#[async_trait]
pub trait MailSink: Send {
    /// Stores the messages in order. On an error some may be stored already: the cursor must not
    /// pass any of them (they come again and are deduped).
    async fn put_batch(&mut self, ms: Vec<FetchedMessage>) -> Result<(), MailError>;
    /// New flags of messages already delivered, `(ext_id, flags)` (normalized like
    /// [`FetchedMessage::flags`]).
    async fn flags_batch(&mut self, updates: Vec<(String, Vec<String>)>) -> Result<(), MailError>;
    /// The message left the folder.
    async fn removed(&mut self, ext_id: &str) -> Result<(), MailError>;
}

/// The second half of [`MailAdapter::open_sync`]: syncs once, or is closed unused (paused source).
#[async_trait]
pub trait SyncSession: Send {
    async fn sync(
        self: Box<Self>,
        state: &mut SyncState,
        sink: &mut dyn MailSink,
    ) -> Result<SyncReport, AdapterError>;
    async fn close(self: Box<Self>) {}
}

/// The default [`SyncSession`]: a plain [`MailAdapter::sync`] later.
struct Deferred<'a, A: ?Sized> {
    adapter: &'a A,
    cx: &'a ConnCx,
}

#[async_trait]
impl<A: MailAdapter + ?Sized> SyncSession for Deferred<'_, A> {
    async fn sync(
        self: Box<Self>,
        state: &mut SyncState,
        sink: &mut dyn MailSink,
    ) -> Result<SyncReport, AdapterError> {
        self.adapter.sync(self.cx, state, sink).await
    }
}

/// A change of a message's state at the provider (`POST /mail/threads/:id/archive|read`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlagChange {
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
    /// Move out of the inbox (IMAP: to the archive folder; Gmail: remove `INBOX`).
    pub archive: bool,
}

/// Envelope of an outgoing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendMeta {
    pub from: String,
    /// Every recipient (To, Cc, Bcc).
    pub rcpt: Vec<String>,
    /// Provider thread to file it under (Gmail threadId).
    pub thread: Option<String>,
    /// IMAP: write a copy to the sent folder (`mail_account_settings.append_sent`); `None` = auto
    /// (not on Gmail, which files sent mail itself).
    pub append_sent: Option<bool>,
}

/// What sending produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SentRef {
    /// The provider's id of the sent copy, when known (IMAP: the APPENDed copy is picked up by sync).
    pub ext_id: Option<String>,
}

/// A mail provider. The connection side (`id`, `verify`, credential form) comes from
/// [`ConnectionProvider`]; register the same `Arc` with `Connections` and with [`crate::Mail`].
#[async_trait]
pub trait MailAdapter: ConnectionProvider {
    async fn folders(&self, cx: &ConnCx) -> Result<Vec<FolderInfo>, AdapterError>;
    /// Incremental sync: pull new mail and flag changes by the cursors in `state`, hand every
    /// message to `sink`, advance `state`.
    async fn sync(
        &self,
        cx: &ConnCx,
        state: &mut SyncState,
        sink: &mut dyn MailSink,
    ) -> Result<SyncReport, AdapterError>;
    /// Push wait (IMAP IDLE on the inbox): `wake.send(())` on new mail; returns when `stop` fires or
    /// the connection breaks. Adapters without push return [`AdapterError::Unsupported`] and are polled.
    async fn watch(
        &self,
        cx: &ConnCx,
        stop: CancellationToken,
        wake: mpsc::Sender<()>,
    ) -> Result<(), AdapterError>;
    /// The original message again (`refetch`); `message_id` is the fallback when the id changed.
    async fn fetch_raw(
        &self,
        cx: &ConnCx,
        ext_id: &str,
        message_id: Option<&str>,
    ) -> Result<Vec<u8>, AdapterError>;
    async fn set_flags(
        &self,
        cx: &ConnCx,
        ext_id: &str,
        ch: &FlagChange,
    ) -> Result<(), AdapterError>;
    /// [`MailAdapter::set_flags`] for several messages, one result per message in order. IMAP does
    /// them over one session instead of logging in per message; the default loops `set_flags`.
    async fn set_flags_many(
        &self,
        cx: &ConnCx,
        ext_ids: &[&str],
        ch: &FlagChange,
    ) -> Vec<Result<(), AdapterError>> {
        let mut out = Vec::with_capacity(ext_ids.len());
        for ext in ext_ids {
            out.push(self.set_flags(cx, ext, ch).await);
        }
        out
    }
    /// The folders, and a [`SyncSession`] that syncs after the job has prepared its state from
    /// them. IMAP lists and syncs over one login; the default calls `folders`, then `sync`.
    async fn open_sync<'a>(
        &'a self,
        cx: &'a ConnCx,
    ) -> Result<(Vec<FolderInfo>, Box<dyn SyncSession + 'a>), AdapterError> {
        let folders = self.folders(cx).await?;
        Ok((folders, Box::new(Deferred { adapter: self, cx })))
    }
    /// Sends a finished MIME message. Only `send::execute` (E3) may call this: it re-checks the
    /// approval first. Auth/Rejected/Config/Gone/Unsupported must mean submission was
    /// definitely not accepted. A lost acknowledgement or any uncertain post-submission error
    /// must be Transient/Store; optional sent-folder copy failures after acceptance should return
    /// success. The sender never automatically retries an uncertain result.
    async fn send(
        &self,
        cx: &ConnCx,
        mime: &[u8],
        meta: &SendMeta,
    ) -> Result<SentRef, AdapterError>;

    /// Optional sent-folder copy, invoked only after confirmed submission is durably recorded.
    /// This must never submit the message again. The caller bounds the entire operation, including
    /// authentication and logout, and a failure cannot turn a confirmed send into unknown.
    async fn copy_sent(
        &self,
        _cx: &ConnCx,
        _mime: &[u8],
        _meta: &SendMeta,
    ) -> Result<(), AdapterError> {
        Ok(())
    }
}

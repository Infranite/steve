//! Fake mail providers for tests (email.md §12): IMAP, SMTP submission, the
//! Gmail API and Microsoft Graph mail, plus real-world-shaped fixture emails.
//!
//! Every server is started on a random port of 127.0.0.1 and returns a cheap,
//! cloneable handle. The handle is both the control interface (`inject`,
//! `set_flags`, `bump_uidvalidity`, `drop_connections`, `fail_next`, `reset`)
//! and the inspection interface (`messages`, `sent`, `commands`). Dropping
//! the last clone stops the server and closes its connections.
//!
//! Nothing here talks to a real account. TLS uses a throwaway root from
//! [`TestCa`] that the test hands to its client.

pub mod control;
pub mod fixtures;
pub mod gmail;
pub mod graph;
mod http;
pub mod imap;
mod mime;
pub mod smtp;
pub mod tls;

pub use control::{ControlServer, Fakes};
pub use gmail::FakeGmail;
pub use graph::FakeGraph;
pub use http::{INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN, TokenRequest};
pub use imap::{FakeImap, ImapConfig};
pub use mime::MailMeta;
pub use smtp::{FakeSmtp, SentMail, SmtpConfig};
pub use tls::TestCa;

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Errors from starting or controlling a fake.
#[derive(Debug, thiserror::Error)]
pub enum FakeError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("no such account: {0}")]
    NoSuchAccount(String),
    #[error("no such folder: {0}")]
    NoSuchFolder(String),
    #[error("no such message: {0}")]
    NoSuchMessage(String),
}

/// How a TCP fake (IMAP, SMTP) does transport security.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsMode {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    #[default]
    Implicit,
    /// Plain text until `STARTTLS`; authentication is refused before it
    /// (IMAP `LOGINDISABLED`, SMTP does not advertise `AUTH`).
    StartTls,
    /// Never TLS. Only for tests of the client's "refuse plain text" path.
    Plain,
}

/// Any byte stream a fake connection runs over (plain TCP or TLS).
pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub(crate) type BoxIo = Box<dyn Io>;

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Poison-tolerant lock: a panicking test thread must not wedge the fake for
/// the other connections.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Scripted failures: the next time `command` arrives, answer `response`
/// instead of running it. Shared shape for IMAP and SMTP.
#[derive(Debug, Default)]
pub(crate) struct FailRules(Mutex<Vec<(String, String)>>);

impl FailRules {
    pub(crate) fn push(&self, command: &str, response: &str) {
        lock(&self.0).push((command.to_ascii_uppercase(), response.to_string()));
    }
    pub(crate) fn take(&self, command: &str) -> Option<String> {
        let mut rules = lock(&self.0);
        let i = rules.iter().position(|(c, _)| c == command)?;
        Some(rules.remove(i).1)
    }
    pub(crate) fn clear(&self) {
        lock(&self.0).clear();
    }
}

/// Lets the handle cut every open connection (`drop_connections`) and stop
/// the listener when the last handle goes away.
#[derive(Debug)]
pub(crate) struct Lifeline {
    kill: watch::Sender<u64>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Lifeline {
    pub(crate) fn new() -> Self {
        Self {
            kill: watch::channel(0).0,
            task: Mutex::new(None),
        }
    }
    pub(crate) fn set_task(&self, task: JoinHandle<()>) {
        *lock(&self.task) = Some(task);
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.kill.subscribe()
    }
    pub(crate) fn kill_connections(&self) {
        self.kill.send_modify(|g| *g += 1);
    }
}

impl Drop for Lifeline {
    fn drop(&mut self) {
        self.kill_connections();
        if let Some(t) = lock(&self.task).take() {
            t.abort();
        }
    }
}

pub(crate) type SharedLifeline = Arc<Lifeline>;

/// Strips the angle brackets of a Message-ID-like value.
pub(crate) fn bare_id(id: &str) -> String {
    id.trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

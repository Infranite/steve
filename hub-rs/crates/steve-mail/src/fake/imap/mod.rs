//! Fake IMAP server (email.md §12).
//!
//! Supported: `CAPABILITY`, `ID`, `STARTTLS`, `LOGIN`, `AUTHENTICATE PLAIN`
//! (with or without SASL-IR), `ENABLE`, `NAMESPACE`, `LIST`/`LSUB` (with
//! SPECIAL-USE flags, modified UTF-7 names), `STATUS`, `CREATE`, `DELETE`,
//! `SUBSCRIBE`, `SELECT`/`EXAMINE` (`HIGHESTMODSEQ` with CONDSTORE),
//! `[UID] SEARCH`, `[UID] FETCH` (`UID FLAGS INTERNALDATE RFC822.SIZE
//! ENVELOPE BODY BODYSTRUCTURE BODY[...]`/`BODY.PEEK[...]` with sections and
//! partials, `RFC822*`, `MODSEQ`, `X-GM-THRID`, `X-GM-MSGID`, `CHANGEDSINCE`),
//! `[UID] STORE`, `[UID] COPY`, `[UID] MOVE`, `EXPUNGE`, `UID EXPUNGE`,
//! `APPEND` (sync literals and LITERAL+, `APPENDUID`), `IDLE`/`DONE`,
//! `CHECK`, `CLOSE`, `UNSELECT`, `NOOP`, `LOGOUT`.
//!
//! Mailbox changes made through the handle (or by other connections) reach a
//! selected session as untagged `EXISTS`/`EXPUNGE`/`FETCH (FLAGS)` — after
//! the next command, or immediately while it is in `IDLE`.

mod parse;
mod render;
mod session;

pub(crate) use parse::days_from_civil;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;
use tokio::sync::watch;

use super::mime::MailMeta;
use super::{FailRules, FakeError, Lifeline, SharedLifeline, TestCa, TlsMode, lock, now_secs};

/// How to start a [`FakeImap`].
#[derive(Debug, Clone)]
pub struct ImapConfig {
    pub tls: TlsMode,
    /// Root to sign the server certificate with; generated when `None`.
    /// Pass the same [`TestCa`] to the SMTP fake so one root covers both.
    pub ca: Option<TestCa>,
    /// `(login, password)`; each gets the default folders.
    pub accounts: Vec<(String, String)>,
    /// Advertise and implement CONDSTORE (`HIGHESTMODSEQ`, `MODSEQ`, `CHANGEDSINCE`).
    pub condstore: bool,
    /// Behave like Gmail: `X-GM-EXT-1`, `X-GM-THRID`/`X-GM-MSGID`, and the
    /// `[Gmail]/…` folder layout including `\All`.
    pub gmail: bool,
    /// Behave like NetEase 163: `SELECT`/`EXAMINE` are refused until the
    /// client sent `ID` (email.md §1).
    pub require_id: bool,
}

impl Default for ImapConfig {
    fn default() -> Self {
        Self {
            tls: TlsMode::Implicit,
            ca: None,
            accounts: vec![("me@example.com".into(), "app-password".into())],
            condstore: false,
            gmail: false,
            require_id: false,
        }
    }
}

/// Options for [`FakeImap::inject_with`].
#[derive(Debug, Clone, Default)]
pub struct Inject {
    /// e.g. `["\\Seen"]`.
    pub flags: Vec<String>,
    /// Unix seconds for INTERNALDATE; now when `None`.
    pub internal_date: Option<i64>,
}

/// A message as the fake stores it — what tests inspect.
#[derive(Debug, Clone, PartialEq)]
pub struct ImapMessage {
    pub uid: u32,
    pub flags: Vec<String>,
    pub internal_date: i64,
    pub modseq: u64,
    /// Gmail's `X-GM-THRID` (assigned by `References`/`In-Reply-To` even
    /// when the Gmail extension is off).
    pub thread_id: u64,
    pub raw: Vec<u8>,
}

/// A folder's identity and counters.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderInfo {
    pub name: String,
    pub special_use: Option<String>,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highest_modseq: u64,
    pub exists: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Msg {
    pub(crate) uid: u32,
    pub(crate) flags: BTreeSet<String>,
    pub(crate) internal_date: i64,
    pub(crate) raw: Arc<Vec<u8>>,
    pub(crate) meta: Arc<MailMeta>,
    pub(crate) modseq: u64,
    pub(crate) gm_msgid: u64,
    pub(crate) gm_thrid: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Folder {
    pub(crate) name: String,
    pub(crate) special_use: Option<String>,
    pub(crate) noselect: bool,
    pub(crate) uidvalidity: u32,
    pub(crate) uidnext: u32,
    pub(crate) messages: Vec<Msg>,
}

#[derive(Debug, Clone)]
pub(crate) struct Account {
    pub(crate) password: String,
    pub(crate) folders: Vec<Folder>,
}

impl Account {
    pub(crate) fn folder(&self, name: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| same_name(&f.name, name))
    }
    pub(crate) fn folder_mut(&mut self, name: &str) -> Option<&mut Folder> {
        self.folders.iter_mut().find(|f| same_name(&f.name, name))
    }
}

/// `INBOX` is case-insensitive, every other name is exact.
fn same_name(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case("INBOX") {
        b.eq_ignore_ascii_case("INBOX")
    } else {
        a == b
    }
}

#[derive(Debug)]
pub(crate) struct State {
    pub(crate) accounts: BTreeMap<String, Account>,
    /// One modseq counter for the whole server keeps HIGHESTMODSEQ monotonic
    /// across folders, which is all CONDSTORE needs.
    pub(crate) modseq: u64,
    next_gm_id: u64,
    next_uidvalidity: u32,
}

impl State {
    pub(crate) fn bump_modseq(&mut self) -> u64 {
        self.modseq += 1;
        self.modseq
    }

    fn new_uidvalidity(&mut self) -> u32 {
        self.next_uidvalidity += 1;
        self.next_uidvalidity
    }

    fn default_folders(&mut self, gmail: bool) -> Vec<Folder> {
        let layout: &[(&str, Option<&str>, bool)] = if gmail {
            &[
                ("INBOX", None, false),
                ("[Gmail]", None, true),
                ("[Gmail]/All Mail", Some("\\All"), false),
                ("[Gmail]/Drafts", Some("\\Drafts"), false),
                ("[Gmail]/Sent Mail", Some("\\Sent"), false),
                ("[Gmail]/Spam", Some("\\Junk"), false),
                ("[Gmail]/Trash", Some("\\Trash"), false),
            ]
        } else {
            &[
                ("INBOX", None, false),
                ("Sent", Some("\\Sent"), false),
                ("Drafts", Some("\\Drafts"), false),
                ("Archive", Some("\\Archive"), false),
                ("Trash", Some("\\Trash"), false),
                ("Junk", Some("\\Junk"), false),
            ]
        };
        layout
            .iter()
            .map(|(name, special, noselect)| Folder {
                name: name.to_string(),
                special_use: special.map(str::to_string),
                noselect: *noselect,
                uidvalidity: self.new_uidvalidity(),
                uidnext: 1,
                messages: Vec::new(),
            })
            .collect()
    }

    /// Adds a message to a folder, assigning UID, modseq and Gmail ids.
    /// `thread` keeps the Gmail thread of a copied message.
    pub(crate) fn add_message(
        &mut self,
        user: &str,
        folder: &str,
        raw: Arc<Vec<u8>>,
        flags: BTreeSet<String>,
        internal_date: i64,
        keep_ids: Option<(u64, u64)>,
    ) -> Result<(u32, u32), FakeError> {
        let meta = Arc::new(MailMeta::parse(&raw));
        let (gm_msgid, gm_thrid) = match keep_ids {
            Some(ids) => ids,
            None => {
                let account = self
                    .accounts
                    .get(user)
                    .ok_or_else(|| FakeError::NoSuchAccount(user.into()))?;
                let parent = meta.parent_ids().find_map(|id| {
                    account
                        .folders
                        .iter()
                        .flat_map(|f| f.messages.iter())
                        .find(|m| m.meta.message_id.as_deref() == Some(id.as_str()))
                        .map(|m| m.gm_thrid)
                });
                self.next_gm_id += 1;
                let id = self.next_gm_id;
                (id, parent.unwrap_or(id))
            }
        };
        let modseq = self.bump_modseq();
        let account = self
            .accounts
            .get_mut(user)
            .ok_or_else(|| FakeError::NoSuchAccount(user.into()))?;
        let f = account
            .folder_mut(folder)
            .filter(|f| !f.noselect)
            .ok_or_else(|| FakeError::NoSuchFolder(folder.into()))?;
        let uid = f.uidnext;
        f.uidnext += 1;
        f.messages.push(Msg {
            uid,
            flags,
            internal_date,
            raw,
            meta,
            modseq,
            gm_msgid,
            gm_thrid,
        });
        Ok((f.uidvalidity, uid))
    }
}

type CommandPause = (
    String,
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) cfg: ImapConfig,
    pub(crate) ca: TestCa,
    pub(crate) state: Mutex<State>,
    /// Bumped on every mailbox change; IDLE sessions wait on it.
    pub(crate) changes: watch::Sender<u64>,
    pub(crate) fail: FailRules,
    /// One-shot protocol barriers for deterministic concurrency tests, never elapsed sleeps.
    pub(crate) pauses: Mutex<Vec<CommandPause>>,
    pub(crate) log: Mutex<Vec<String>>,
    pub(crate) logins: Mutex<usize>,
}

impl Shared {
    pub(crate) fn changed(&self) {
        self.changes.send_modify(|g| *g += 1);
    }
    pub(crate) fn record(&self, line: String) {
        lock(&self.log).push(line);
    }
}

/// Handle to a running fake IMAP server. Cloning shares the server.
#[derive(Debug, Clone)]
pub struct FakeImap {
    shared: Arc<Shared>,
    life: SharedLifeline,
    addr: SocketAddr,
}

impl FakeImap {
    /// Binds 127.0.0.1 on a random port and starts serving.
    pub async fn start(cfg: ImapConfig) -> Result<Self, FakeError> {
        let ca = match &cfg.ca {
            Some(ca) => ca.clone(),
            None => TestCa::generate()?,
        };
        let mut state = State {
            accounts: BTreeMap::new(),
            modseq: 1,
            next_gm_id: 1_800_000_000_000_000_000,
            next_uidvalidity: 1_761_000_000,
        };
        for (user, pass) in &cfg.accounts {
            let folders = state.default_folders(cfg.gmail);
            state.accounts.insert(
                user.clone(),
                Account {
                    password: pass.clone(),
                    folders,
                },
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            cfg,
            ca,
            state: Mutex::new(state),
            changes: watch::channel(0).0,
            fail: FailRules::default(),
            pauses: Mutex::new(Vec::new()),
            log: Mutex::new(Vec::new()),
            logins: Mutex::new(0),
        });
        let life = Arc::new(Lifeline::new());
        let task = {
            let shared = shared.clone();
            let kill = life.subscribe();
            tokio::spawn(async move {
                loop {
                    let Ok((sock, _)) = listener.accept().await else {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        continue;
                    };
                    let shared = shared.clone();
                    let kill = kill.clone();
                    tokio::spawn(async move {
                        if let Err(e) = session::serve(shared, sock, kill).await {
                            tracing::debug!("fake imap connection ended: {e}");
                        }
                    });
                }
            })
        };
        life.set_task(task);
        Ok(Self { shared, life, addr })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    /// Always `localhost` — the name on the server certificate.
    pub fn host(&self) -> &'static str {
        "localhost"
    }
    pub fn tls_mode(&self) -> TlsMode {
        self.shared.cfg.tls
    }
    /// The root that signed the server certificate.
    pub fn ca(&self) -> &TestCa {
        &self.shared.ca
    }

    fn mutate<R>(
        &self,
        f: impl FnOnce(&mut State) -> Result<R, FakeError>,
    ) -> Result<R, FakeError> {
        let r = f(&mut lock(&self.shared.state))?;
        self.shared.changed();
        Ok(r)
    }

    /// Adds an account with the default folders (replacing one of the same login).
    pub fn add_account(&self, user: &str, password: &str) {
        let gmail = self.shared.cfg.gmail;
        let _ = self.mutate(|s| {
            let folders = s.default_folders(gmail);
            s.accounts.insert(
                user.into(),
                Account {
                    password: password.into(),
                    folders,
                },
            );
            Ok(())
        });
    }

    /// Adds a folder, e.g. `("已发送", Some("\\Sent"))` for a QQ-style layout.
    pub fn add_folder(
        &self,
        user: &str,
        name: &str,
        special_use: Option<&str>,
    ) -> Result<(), FakeError> {
        self.mutate(|s| {
            let uidvalidity = s.new_uidvalidity();
            let a = s
                .accounts
                .get_mut(user)
                .ok_or_else(|| FakeError::NoSuchAccount(user.into()))?;
            if a.folder(name).is_none() {
                a.folders.push(Folder {
                    name: name.into(),
                    special_use: special_use.map(str::to_string),
                    noselect: false,
                    uidvalidity,
                    uidnext: 1,
                    messages: Vec::new(),
                });
            }
            Ok(())
        })
    }

    /// Delivers a message now, unseen. Returns its UID.
    pub fn inject(&self, user: &str, folder: &str, eml: &[u8]) -> Result<u32, FakeError> {
        self.inject_with(user, folder, eml, Inject::default())
    }

    /// Delivers a message with explicit flags and INTERNALDATE. Returns its UID.
    pub fn inject_with(
        &self,
        user: &str,
        folder: &str,
        eml: &[u8],
        opts: Inject,
    ) -> Result<u32, FakeError> {
        self.mutate(|s| {
            s.add_message(
                user,
                folder,
                Arc::new(eml.to_vec()),
                opts.flags.into_iter().collect(),
                opts.internal_date.unwrap_or_else(now_secs),
                None,
            )
            .map(|(_, uid)| uid)
        })
    }

    /// Replaces a message's flags (as another client would), bumping its modseq.
    pub fn set_flags(
        &self,
        user: &str,
        folder: &str,
        uid: u32,
        flags: &[&str],
    ) -> Result<(), FakeError> {
        self.mutate(|s| {
            let modseq = s.bump_modseq();
            let m = find_msg(s, user, folder, uid)?;
            m.flags = flags.iter().map(|f| f.to_string()).collect();
            m.modseq = modseq;
            Ok(())
        })
    }

    /// Removes a message (as if deleted by another client).
    pub fn remove(&self, user: &str, folder: &str, uid: u32) -> Result<(), FakeError> {
        self.mutate(|s| {
            let f = find_folder(s, user, folder)?;
            let before = f.messages.len();
            f.messages.retain(|m| m.uid != uid);
            if f.messages.len() == before {
                return Err(FakeError::NoSuchMessage(uid.to_string()));
            }
            Ok(())
        })
    }

    /// Gives the folder a new UIDVALIDITY and renumbers its messages from 1,
    /// as a server does after rebuilding its index. Sessions that have the
    /// folder selected get `* BYE` and are disconnected. Returns the new value.
    pub fn bump_uidvalidity(&self, user: &str, folder: &str) -> Result<u32, FakeError> {
        self.mutate(|s| {
            let v = s.new_uidvalidity();
            let f = find_folder(s, user, folder)?;
            f.uidvalidity = v;
            for (i, m) in f.messages.iter_mut().enumerate() {
                m.uid = i as u32 + 1;
            }
            f.uidnext = f.messages.len() as u32 + 1;
            Ok(v)
        })
    }

    /// Cuts every open connection without a `BYE`; the listener keeps
    /// accepting so clients can reconnect.
    pub fn drop_connections(&self) {
        self.life.kill_connections();
    }

    /// The next time `command` arrives (`"SELECT"`, `"UID FETCH"`,
    /// `"LOGIN"`…), answer `<tag> <response>` instead, e.g.
    /// `fail_next("UID FETCH", "NO [UNAVAILABLE] try later")`.
    /// Response `"HOLD"` withholds the reply until the client abandons the command.
    pub fn fail_next(&self, command: &str, response: &str) {
        self.shared.fail.push(command, response);
    }

    /// Pause the next matching command before it runs. The receiver signals arrival; sending or
    /// dropping the returned sender releases it. Other connections continue normally. This lets
    /// tests order initial sync and IDLE without relying on command logs as completion signals.
    pub fn pause_next(
        &self,
        command: &str,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, seen) = tokio::sync::oneshot::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        lock(&self.shared.pauses).push((command.to_ascii_uppercase(), entered, wait));
        (seen, release)
    }

    /// Messages of a folder in UID order.
    pub fn messages(&self, user: &str, folder: &str) -> Result<Vec<ImapMessage>, FakeError> {
        let s = lock(&self.shared.state);
        let f = s
            .accounts
            .get(user)
            .ok_or_else(|| FakeError::NoSuchAccount(user.into()))?
            .folder(folder)
            .ok_or_else(|| FakeError::NoSuchFolder(folder.into()))?;
        Ok(f.messages
            .iter()
            .map(|m| ImapMessage {
                uid: m.uid,
                flags: m.flags.iter().cloned().collect(),
                internal_date: m.internal_date,
                modseq: m.modseq,
                thread_id: m.gm_thrid,
                raw: m.raw.as_ref().clone(),
            })
            .collect())
    }

    /// Folders of an account in LIST order.
    pub fn folders(&self, user: &str) -> Result<Vec<FolderInfo>, FakeError> {
        let s = lock(&self.shared.state);
        let a = s
            .accounts
            .get(user)
            .ok_or_else(|| FakeError::NoSuchAccount(user.into()))?;
        Ok(a.folders
            .iter()
            .map(|f| FolderInfo {
                name: f.name.clone(),
                special_use: f.special_use.clone(),
                uidvalidity: f.uidvalidity,
                uidnext: f.uidnext,
                highest_modseq: f.messages.iter().map(|m| m.modseq).max().unwrap_or(1),
                exists: f.messages.len(),
            })
            .collect())
    }

    /// Every command received, in order, as `"<tag> <COMMAND> <args>"`;
    /// `LOGIN`/`AUTHENTICATE` arguments are masked and literal bodies omitted.
    pub fn commands(&self) -> Vec<String> {
        lock(&self.shared.log).clone()
    }

    /// Successful logins so far (to assert reconnects or one IDLE connection).
    pub fn logins(&self) -> usize {
        *lock(&self.shared.logins)
    }

    /// Empties every folder back to the default layout, forgets scripted
    /// failures and the command log, and cuts connections. Accounts stay.
    pub fn reset(&self) {
        let gmail = self.shared.cfg.gmail;
        let _ = self.mutate(|s| {
            let users: Vec<String> = s.accounts.keys().cloned().collect();
            for u in users {
                let folders = s.default_folders(gmail);
                if let Some(a) = s.accounts.get_mut(&u) {
                    a.folders = folders;
                }
            }
            Ok(())
        });
        self.shared.fail.clear();
        lock(&self.shared.pauses).clear();
        lock(&self.shared.log).clear();
        *lock(&self.shared.logins) = 0;
        self.life.kill_connections();
    }
}

fn find_folder<'a>(
    s: &'a mut State,
    user: &str,
    folder: &str,
) -> Result<&'a mut Folder, FakeError> {
    s.accounts
        .get_mut(user)
        .ok_or_else(|| FakeError::NoSuchAccount(user.into()))?
        .folder_mut(folder)
        .ok_or_else(|| FakeError::NoSuchFolder(folder.into()))
}

fn find_msg<'a>(
    s: &'a mut State,
    user: &str,
    folder: &str,
    uid: u32,
) -> Result<&'a mut Msg, FakeError> {
    find_folder(s, user, folder)?
        .messages
        .iter_mut()
        .find(|m| m.uid == uid)
        .ok_or_else(|| FakeError::NoSuchMessage(uid.to_string()))
}

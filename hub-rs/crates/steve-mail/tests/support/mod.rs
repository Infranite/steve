//! Harness for the E1 tests: a migrated store in a temp dir (dev user, two workspaces), the vault on
//! a file key store, the connections framework with the IMAP adapter, the fake IMAP and SMTP servers
//! on one throwaway root, and a [`Mail`] with recording events. Never touches `~/.steve` or a real
//! mail server.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use steve_mail::events::{FnNotify, RecordingEvents};
use steve_mail::fake::imap::Inject;
use steve_mail::fake::{FakeImap, FakeSmtp, ImapConfig, SmtpConfig, TestCa, TlsMode};
use steve_mail::repo::{self, MessageRow, ThreadRow};
use steve_mail::usage::MailUsage;
use steve_mail::{ImapAdapter, Mail, MailConfig};
use steve_modules::Caller;
use steve_modules::accounts::connections::{Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use zeroize::Zeroizing;

pub const ADDR: &str = "me@example.com";
pub const PASS: &str = "app-password";
pub const DAY_MS: i64 = 86_400_000;

pub struct H {
    pub env: TestEnv,
    pub link: steve_mail::DraftLink,
    pub _keys: TempDir,
    pub _ca_dir: TempDir,
    pub vault: Vault,
    pub conns: Connections,
    pub mail: Mail,
    pub cfg: MailConfig,
    pub events: Arc<RecordingEvents>,
    pub dispatcher: Arc<steve_store::outbox::Dispatcher>,
    pub notified: Arc<Mutex<Vec<String>>>,
    pub imap: FakeImap,
    pub smtp: FakeSmtp,
}

#[derive(Clone)]
pub struct Opts {
    pub imap: ImapConfig,
    pub smtp_tls: TlsMode,
    pub secrets: Option<Arc<dyn steve_mail::SecretScan>>,
    pub hooks: Option<Arc<dyn steve_mail::DraftHooks>>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            imap: ImapConfig::default(),
            smtp_tls: TlsMode::Implicit,
            secrets: None,
            hooks: None,
        }
    }
}

pub async fn setup() -> H {
    setup_with(Opts::default()).await
}

pub async fn setup_with(o: Opts) -> H {
    let link = steve_mail::DraftLink::new();
    let l2 = link.clone();
    let env = TestEnv::new(move |r| {
        steve_modules::accounts::connections::register(r);
        r.decision(steve_mail::drafts::SUBJECT_TYPE, l2.handler());
        steve_mail::config::targets::register(r);
    });
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");

    let ca = TestCa::generate().expect("ca");
    let imap = FakeImap::start(ImapConfig {
        ca: Some(ca.clone()),
        ..o.imap
    })
    .await
    .expect("imap");
    let smtp = FakeSmtp::start(SmtpConfig {
        tls: o.smtp_tls,
        ca: Some(ca.clone()),
        deliver_to: None,
        ..Default::default()
    })
    .await
    .expect("smtp");

    // the test root goes in only through STEVE_MAIL_TEST_CA, behind the ~/.steve guard
    let ca_dir = tempfile::tempdir().expect("tmp");
    let pem = ca_dir.path().join("ca.pem");
    std::fs::write(&pem, ca.ca_pem()).expect("pem");
    let home = env.dir.path().to_string_lossy().to_string();
    let user_home = ca_dir.path().to_string_lossy().to_string();
    let pem_s = pem.to_string_lossy().to_string();
    let cfg = MailConfig::from_env(move |k| match k {
        "STEVE_HOME" => Some(home.clone()),
        "HOME" => Some(user_home.clone()),
        "STEVE_MAIL_TEST_CA" => Some(pem_s.clone()),
        _ => None,
    })
    .expect("mail config");

    let adapter = Arc::new(ImapAdapter::new(cfg.clone()));
    let conns = Connections::builder(
        env.modules.clone(),
        vault.clone(),
        ConnectionsConfig::new(vec!["http://127.0.0.1:8441".into()], true, |_| None),
    )
    .provider(adapter.clone())
    .usage(Arc::new(MailUsage::new(cfg.clone())))
    .build();
    let events = Arc::new(RecordingEvents::new());
    let notified: Arc<Mutex<Vec<String>>> = Arc::default();
    let n = notified.clone();
    let mut b = Mail::builder(env.modules.clone(), conns.clone(), cfg.clone()).drafts(link.clone());
    if let Some(s) = o.secrets.clone() {
        b = b.secrets(s);
    }
    if let Some(h) = o.hooks.clone() {
        b = b.hooks(h);
    }
    let mail = b
        .adapter(adapter)
        .events(events.clone())
        .notify(Arc::new(FnNotify(move |_ws: &str, id: &str| {
            n.lock().unwrap().push(id.to_string())
        })))
        .build();
    let dispatcher = Arc::new(steve_store::outbox::Dispatcher::new(vec![
        steve_mail::outbox::MailEventsOut::new(mail.clone()),
    ]));
    dispatcher.register(env.store()).expect("register");
    H {
        dispatcher,
        env,
        link,
        _keys: keys,
        _ca_dir: ca_dir,
        vault,
        conns,
        mail,
        cfg,
        events,
        notified,
        imap,
        smtp,
    }
}

pub fn me() -> Caller {
    caller(WS, ME)
}

impl H {
    /// The credential form of the IMAP dialog for the fakes.
    pub fn fields(&self, password: &str) -> (BTreeMap<String, Zeroizing<String>>, Value) {
        let mut f = BTreeMap::new();
        let mut put = |k: &str, v: String| {
            f.insert(k.to_string(), Zeroizing::new(v));
        };
        put("address", ADDR.into());
        put("password", password.into());
        put("imap_host", self.imap.host().into());
        put("imap_port", self.imap.port().to_string());
        put(
            "imap_security",
            match self.imap.tls_mode() {
                TlsMode::StartTls => "starttls",
                _ => "tls",
            }
            .into(),
        );
        put("smtp_host", self.smtp.host().into());
        put("smtp_port", self.smtp.port().to_string());
        put(
            "smtp_security",
            match self.smtp.tls_mode() {
                TlsMode::StartTls => "starttls",
                _ => "tls",
            }
            .into(),
        );
        (f, json!({}))
    }

    /// Connects the fake mailbox; returns the connection id.
    pub async fn connect(&self) -> String {
        let (f, config) = self.fields(PASS);
        self.conns
            .create(&me(), "imap", &f, config)
            .await
            .expect("connect")
            .id
    }

    /// Syncs, then delivers what the sync put in the outbox (the Hub's dispatcher does it there).
    pub async fn sync(&self, conn: &str) -> steve_mail::SyncReport {
        let r = self.mail.sync_now(WS, conn).await.expect("sync");
        self.deliver().await;
        r
    }

    /// One pass of an outbox dispatcher with the mail's consumer, on a blocking thread as in the Hub.
    pub async fn deliver(&self) -> steve_store::outbox::Pumped {
        let (d, store) = (self.dispatcher.clone(), self.env.store().clone());
        tokio::task::spawn_blocking(move || d.pump(&store, steve_store::now()).expect("pump"))
            .await
            .expect("pump task")
    }

    pub fn inject(&self, folder: &str, eml: &[u8]) -> u32 {
        self.imap.inject(ADDR, folder, eml).expect("inject")
    }

    pub fn inject_at(&self, folder: &str, eml: &[u8], unix_s: i64) -> u32 {
        self.imap
            .inject_with(
                ADDR,
                folder,
                eml,
                Inject {
                    internal_date: Some(unix_s),
                    ..Default::default()
                },
            )
            .expect("inject")
    }

    pub fn messages(&self, conn: &str) -> Vec<MessageRow> {
        let ids: Vec<String> = self
            .env
            .store()
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT id FROM mail_messages WHERE connection_id = ? ORDER BY date, rowid",
                )?;
                let v = st
                    .query_map([conn], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, steve_store::StoreError>(v)
            })
            .expect("ids");
        ids.iter()
            .map(|id| {
                self.env
                    .store()
                    .reader(|c| repo::message(c, id))
                    .expect("message")
                    .expect("row")
            })
            .collect()
    }

    pub fn by_subject(&self, conn: &str, subject: &str) -> MessageRow {
        self.messages(conn)
            .into_iter()
            .find(|m| m.subject == subject)
            .unwrap_or_else(|| panic!("no message {subject}"))
    }

    pub fn thread(&self, id: &str) -> ThreadRow {
        self.env
            .store()
            .reader(|c| repo::thread(c, id))
            .expect("thread")
            .expect("row")
    }

    pub fn count(&self, sql: &str, p: &str) -> i64 {
        self.env
            .store()
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(sql, [p], |r| r.get(0))?))
            .expect("count")
    }

    /// Waits until `f` holds (up to `secs`).
    /// Polls `f` for up to `secs`; between polls the outbox is delivered, as the Hub's dispatcher
    /// would (the background runners only put the events there).
    pub async fn until(&self, secs: u64, mut f: impl FnMut(&H) -> bool) -> bool {
        let end = tokio::time::Instant::now() + Duration::from_secs(secs);
        while tokio::time::Instant::now() < end {
            self.deliver().await;
            if f(self) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.deliver().await;
        f(self)
    }
}

/// Unix seconds, `days` ago.
pub fn days_ago(days: i64) -> i64 {
    steve_store::now() / 1000 - days * 86_400
}

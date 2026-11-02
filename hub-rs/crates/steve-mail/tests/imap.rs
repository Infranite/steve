//! Drives the fake IMAP server with `async-imap` (and a raw client for the
//! few things async-imap does not expose).

use std::time::Duration;

use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::{MailboxDatum, Response};
use async_imap::types::{Flag, NameAttribute, UnsolicitedResponse};
use futures::TryStreamExt;
use rustls::pki_types::ServerName;
use steve_mail::fake::imap::Inject;
use steve_mail::fake::{FakeImap, ImapConfig, TlsMode, fixtures};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

const ME: &str = "me@example.com";
const PASS: &str = "app-password";

type Session = async_imap::Session<TlsStream<TcpStream>>;

async fn tls(imap: &FakeImap) -> TlsStream<TcpStream> {
    let tcp = TcpStream::connect(imap.addr()).await.unwrap();
    imap.ca()
        .connector()
        .unwrap()
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap()
}

async fn client(imap: &FakeImap) -> async_imap::Client<TlsStream<TcpStream>> {
    let mut c = async_imap::Client::new(tls(imap).await);
    let greeting = c.read_response().await.unwrap().unwrap();
    assert!(matches!(greeting.parsed(), Response::Data { .. }));
    c
}

async fn login(imap: &FakeImap) -> Session {
    client(imap)
        .await
        .login(ME, PASS)
        .await
        .map_err(|(e, _)| e)
        .unwrap()
}

async fn start(cfg: ImapConfig) -> FakeImap {
    FakeImap::start(cfg).await.unwrap()
}

/// Minimal line-based client over TLS for raw protocol assertions.
struct Raw {
    io: BufReader<TlsStream<TcpStream>>,
}

impl Raw {
    async fn connect(imap: &FakeImap) -> Self {
        let mut r = Self {
            io: BufReader::new(tls(imap).await),
        };
        r.line().await; // greeting
        r
    }
    async fn line(&mut self) -> String {
        let mut s = String::new();
        self.io.read_line(&mut s).await.unwrap();
        s
    }
    async fn send(&mut self, bytes: &[u8]) {
        self.io.get_mut().write_all(bytes).await.unwrap();
        self.io.get_mut().flush().await.unwrap();
    }
    /// Sends `cmd` with tag `tag`, returns everything up to the tagged line.
    async fn cmd(&mut self, tag: &str, cmd: &str) -> Vec<String> {
        self.send(format!("{tag} {cmd}\r\n").as_bytes()).await;
        self.until(tag).await
    }
    async fn until(&mut self, tag: &str) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let l = self.line().await;
            assert!(!l.is_empty(), "connection closed; got {out:?}");
            // Skip literal payloads so their lines are not mistaken for tags.
            if let Some(n) = l
                .trim_end()
                .strip_suffix('}')
                .and_then(|x| x.rsplit_once('{'))
                .and_then(|(_, n)| n.parse::<usize>().ok())
            {
                let mut buf = vec![0; n];
                self.io.read_exact(&mut buf).await.unwrap();
                out.push(l);
                continue;
            }
            let done = l.starts_with(&format!("{tag} "));
            out.push(l);
            if done {
                return out;
            }
        }
    }
}

#[tokio::test]
async fn login_list_select_and_fetch() {
    let imap = start(ImapConfig::default()).await;
    let mut uids = Vec::new();
    for (name, raw) in fixtures::ALL {
        let uid = imap
            .inject_with(
                ME,
                "INBOX",
                raw,
                Inject {
                    internal_date: Some(1_761_500_000), // 26-Oct-2025
                    ..Default::default()
                },
            )
            .unwrap();
        uids.push((uid, *name, *raw));
    }

    let mut s = login(&imap).await;
    let caps = s.capabilities().await.unwrap();
    for c in ["IDLE", "UIDPLUS", "LITERAL+", "ID", "MOVE", "AUTH=PLAIN"] {
        assert!(caps.has_str(c), "missing {c}");
    }
    assert!(!caps.has_str("CONDSTORE"));

    let names: Vec<_> = s
        .list(Some(""), Some("*"))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let sent = names.iter().find(|n| n.name() == "Sent").unwrap();
    assert!(
        sent.attributes()
            .iter()
            .any(|a| matches!(a, NameAttribute::Sent))
    );
    assert!(names.iter().any(|n| n.name() == "INBOX"));
    assert_eq!(names.len(), 6);

    let mb = s.select("INBOX").await.unwrap();
    assert_eq!(mb.exists as usize, fixtures::ALL.len());
    let info = &imap.folders(ME).unwrap()[0];
    assert_eq!(mb.uid_validity, Some(info.uidvalidity));
    assert_eq!(mb.uid_next, Some(info.uidnext));

    let since = s.uid_search("SINCE 1-Oct-2025").await.unwrap();
    assert_eq!(since.len(), fixtures::ALL.len());
    assert!(s.uid_search("SINCE 1-Nov-2025").await.unwrap().is_empty());
    let alice = s.uid_search("FROM alice").await.unwrap();
    assert_eq!(alice.len(), 2, "thread-1 and thread-3");

    let fetches: Vec<_> = s
        .uid_fetch("1:*", "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[])")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(fetches.len(), uids.len());
    for f in &fetches {
        let uid = f.uid.unwrap();
        let (_, name, raw) = uids.iter().find(|(u, _, _)| *u == uid).unwrap();
        assert_eq!(f.body().unwrap(), *raw, "{name}");
        assert_eq!(f.size, Some(raw.len() as u32));
        assert_eq!(f.internal_date().unwrap().timestamp(), 1_761_500_000);
        assert!(!f.flags().any(|fl| fl == Flag::Seen), "PEEK keeps unseen");
    }

    // Non-PEEK fetch sets \Seen.
    let _: Vec<_> = s
        .uid_fetch("1", "BODY[HEADER]")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert!(
        imap.messages(ME, "INBOX").unwrap()[0]
            .flags
            .contains(&"\\Seen".to_string())
    );
    assert_eq!(s.uid_search("UNSEEN").await.unwrap().len(), uids.len() - 1);
    s.logout().await.unwrap();
}

#[tokio::test]
async fn incremental_fetch_follows_rfc_star() {
    let imap = start(ImapConfig::default()).await;
    let [t1, t2, t3] = fixtures::thread();
    imap.inject(ME, "INBOX", t1).unwrap();
    let mut s = login(&imap).await;
    s.select("INBOX").await.unwrap();

    // Nothing new: "2:*" still returns the last message (RFC 3501 n:* rule).
    let f: Vec<_> = s
        .uid_fetch("2:*", "(UID)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(
        f.iter().map(|f| f.uid.unwrap()).collect::<Vec<_>>(),
        vec![1]
    );

    imap.inject(ME, "INBOX", t2).unwrap();
    imap.inject(ME, "INBOX", t3).unwrap();
    let f: Vec<_> = s
        .uid_fetch("2:*", "(UID ENVELOPE)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(
        f.iter().map(|f| f.uid.unwrap()).collect::<Vec<_>>(),
        vec![2, 3]
    );
    let env = f[1].envelope().unwrap();
    assert_eq!(
        env.message_id.as_deref(),
        Some(&b"<thread-3.alice@acme.example>"[..])
    );
    assert_eq!(
        env.in_reply_to.as_deref(),
        Some(&b"<thread-2.me@example.com>"[..])
    );
    // The new messages were announced as EXISTS before the FETCH answer.
    let mut exists = None;
    while let Ok(u) = s.unsolicited_responses.try_recv() {
        if let UnsolicitedResponse::Exists(n) = u {
            exists = Some(n);
        }
    }
    assert_eq!(exists, Some(3));
}

#[tokio::test]
async fn idle_reports_new_mail() {
    let imap = start(ImapConfig::default()).await;
    let mut s = login(&imap).await;
    s.select("INBOX").await.unwrap();
    let mut idle = s.idle();
    idle.init().await.unwrap();
    let injector = imap.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        injector
            .inject(ME, "INBOX", fixtures::get("newsletter.eml").unwrap())
            .unwrap();
    });
    let (wait, _stop) = idle.wait_with_timeout(Duration::from_secs(5));
    match wait.await.unwrap() {
        IdleResponse::NewData(d) => {
            assert!(
                matches!(d.parsed(), Response::MailboxData(MailboxDatum::Exists(1))),
                "{:?}",
                d.parsed()
            );
        }
        other => panic!("expected EXISTS, got {other:?}"),
    }
    let mut s = idle.done().await.unwrap();
    let f: Vec<_> = s
        .uid_fetch("1", "(UID)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(f.len(), 1);
}

#[tokio::test]
async fn append_store_move_expunge() {
    let imap = start(ImapConfig::default()).await;
    let mut s = login(&imap).await;
    let reply = fixtures::get("thread-2.eml").unwrap();
    s.append("Sent", Some("(\\Seen)"), None, reply)
        .await
        .unwrap();
    let sent = imap.messages(ME, "Sent").unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].raw, reply);
    assert_eq!(sent[0].flags, vec!["\\Seen".to_string()]);

    imap.inject(ME, "INBOX", fixtures::get("gbk.eml").unwrap())
        .unwrap();
    imap.inject(ME, "INBOX", fixtures::get("big5.eml").unwrap())
        .unwrap();
    s.select("INBOX").await.unwrap();
    let f: Vec<_> = s
        .uid_store("1", "+FLAGS (\\Seen \\Flagged)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert!(f[0].flags().any(|x| x == Flag::Flagged));
    let inbox = imap.messages(ME, "INBOX").unwrap();
    assert_eq!(
        inbox[0].flags,
        vec!["\\Flagged".to_string(), "\\Seen".to_string()]
    );

    s.uid_mv("1", "Archive").await.unwrap();
    assert_eq!(imap.messages(ME, "INBOX").unwrap().len(), 1);
    let archived = imap.messages(ME, "Archive").unwrap();
    assert_eq!(archived.len(), 1);
    assert!(
        archived[0].flags.contains(&"\\Flagged".to_string()),
        "flags survive MOVE"
    );

    s.uid_store("2", "+FLAGS.SILENT (\\Deleted)")
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let gone: Vec<_> = s.expunge().await.unwrap().try_collect().await.unwrap();
    assert_eq!(gone, vec![1]);
    assert!(imap.messages(ME, "INBOX").unwrap().is_empty());
}

#[tokio::test]
async fn uidvalidity_change_disconnects_and_renumbers() {
    let imap = start(ImapConfig::default()).await;
    for raw in fixtures::thread() {
        imap.inject(ME, "INBOX", raw).unwrap();
    }
    imap.remove(ME, "INBOX", 1).unwrap();
    let mut s = login(&imap).await;
    let before = s.select("INBOX").await.unwrap().uid_validity.unwrap();
    let after = imap.bump_uidvalidity(ME, "INBOX").unwrap();
    assert_ne!(before, after);
    assert!(s.noop().await.is_err(), "BYE after UIDVALIDITY change");

    let mut s = login(&imap).await;
    let mb = s.select("INBOX").await.unwrap();
    assert_eq!(mb.uid_validity, Some(after));
    let uids = s.uid_search("ALL").await.unwrap();
    assert_eq!(uids, [1, 2].into_iter().collect(), "renumbered from 1");
}

#[tokio::test]
async fn authenticate_plain_and_bad_password() {
    struct Plain;
    impl async_imap::Authenticator for Plain {
        type Response = String;
        fn process(&mut self, _: &[u8]) -> String {
            format!("\0{ME}\0{PASS}")
        }
    }
    let imap = start(ImapConfig::default()).await;
    let mut s = client(&imap)
        .await
        .authenticate("PLAIN", Plain)
        .await
        .map_err(|(e, _)| e)
        .unwrap();
    s.select("INBOX").await.unwrap();

    let err = client(&imap).await.login(ME, "wrong").await;
    assert!(err.is_err());
    assert_eq!(imap.logins(), 1);
    let log = imap.commands();
    assert!(
        log.iter()
            .all(|l| !l.contains(PASS) && !l.contains("wrong")),
        "{log:?}"
    );
    assert!(log.iter().any(|l| l.ends_with("LOGIN ***")));
}

#[tokio::test]
async fn starttls_required_before_login() {
    let imap = start(ImapConfig {
        tls: TlsMode::StartTls,
        ..Default::default()
    })
    .await;
    let mut io = BufReader::new(TcpStream::connect(imap.addr()).await.unwrap());
    let mut line = String::new();
    io.read_line(&mut line).await.unwrap();
    assert!(
        line.contains("STARTTLS") && line.contains("LOGINDISABLED"),
        "{line}"
    );
    let mut ask = async |cmd: &str| {
        io.get_mut().write_all(cmd.as_bytes()).await.unwrap();
        let mut l = String::new();
        io.read_line(&mut l).await.unwrap();
        l
    };
    // Plain-text LOGIN is refused.
    let r = ask("a LOGIN me@example.com app-password\r\n").await;
    assert!(r.starts_with("a NO [PRIVACYREQUIRED]"), "{r}");
    let r = ask("b STARTTLS\r\n").await;
    assert!(r.starts_with("b OK"), "{r}");
    let tcp = io.into_inner();
    let tls = imap
        .ca()
        .connector()
        .unwrap()
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    let c = async_imap::Client::new(tls);
    let mut s = c.login(ME, PASS).await.map_err(|(e, _)| e).unwrap();
    s.select("INBOX").await.unwrap();
}

#[tokio::test]
async fn drop_connections_then_reconnect() {
    let imap = start(ImapConfig::default()).await;
    let mut s = login(&imap).await;
    s.select("INBOX").await.unwrap();
    imap.drop_connections();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(s.noop().await.is_err());
    let mut s = login(&imap).await;
    s.select("INBOX").await.unwrap();
    assert_eq!(imap.logins(), 2);
}

#[tokio::test]
async fn fail_next_scripts_one_failure() {
    let imap = start(ImapConfig::default()).await;
    let mut s = login(&imap).await;
    imap.fail_next("SELECT", "NO [UNAVAILABLE] try again later");
    assert!(s.select("INBOX").await.is_err());
    s.select("INBOX").await.unwrap();
    imap.fail_next("APPEND", "NO [OVERQUOTA] mailbox full");
    let msg = fixtures::get("thread-2.eml").unwrap();
    assert!(s.append("Sent", None, None, msg).await.is_err());
    s.append("Sent", None, None, msg).await.unwrap();
    imap.inject(ME, "INBOX", fixtures::get("gbk.eml").unwrap())
        .unwrap();
    // Caution for the real client: async-imap's FETCH/SEARCH streams ignore a
    // tagged NO and just end empty — the sync must not read that as "no new mail".
    imap.fail_next("UID FETCH", "NO [UNAVAILABLE] boom");
    let f: Vec<_> = s
        .uid_fetch("1:*", "(UID)")
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert!(f.is_empty());
    assert!(
        imap.commands()
            .iter()
            .any(|c| c.contains("UID FETCH 1:* (UID)"))
    );
}

#[tokio::test]
async fn bodystructure_and_sections() {
    let imap = start(ImapConfig::default()).await;
    let raw = fixtures::get("invoice-pdf.eml").unwrap();
    imap.inject(ME, "INBOX", raw).unwrap();
    let mut s = login(&imap).await;
    s.examine("INBOX").await.unwrap();
    let f: Vec<_> = s
        .uid_fetch(
            "1",
            "(BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (SUBJECT)] BODY.PEEK[2]<0.8>)",
        )
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let bs = f[0].bodystructure().unwrap();
    match bs {
        async_imap::imap_proto::BodyStructure::Multipart { common, bodies, .. } => {
            assert_eq!(common.ty.subtype.to_ascii_lowercase(), "mixed");
            assert_eq!(bodies.len(), 2);
        }
        other => panic!("not multipart: {other:?}"),
    }
    let header = f[0].header().unwrap();
    assert!(header.starts_with(b"Subject: =?UTF-8?B?"));
    // EXAMINE is read-only: no \Seen even for the non-PEEK path.
    assert!(imap.messages(ME, "INBOX").unwrap()[0].flags.is_empty());
}

#[tokio::test]
async fn condstore_gmail_and_id() {
    let imap = start(ImapConfig {
        condstore: true,
        gmail: true,
        require_id: true,
        ..Default::default()
    })
    .await;
    for raw in fixtures::thread() {
        imap.inject(ME, "INBOX", raw).unwrap();
    }
    imap.inject(ME, "INBOX", fixtures::get("newsletter.eml").unwrap())
        .unwrap();
    let msgs = imap.messages(ME, "INBOX").unwrap();
    assert_eq!(msgs[0].thread_id, msgs[1].thread_id);
    assert_eq!(msgs[1].thread_id, msgs[2].thread_id);
    assert_ne!(msgs[0].thread_id, msgs[3].thread_id);

    let mut s = login(&imap).await;
    assert!(s.select("INBOX").await.is_err(), "163-style: ID first");
    s.id([("name", Some("steve"))]).await.unwrap();
    let mb = s.select_condstore("INBOX").await.unwrap();
    let hms = mb.highest_modseq.unwrap();
    let names: Vec<_> = s
        .list(Some(""), Some("*"))
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let all = names
        .iter()
        .find(|n| n.name() == "[Gmail]/All Mail")
        .unwrap();
    assert!(
        all.attributes()
            .iter()
            .any(|a| matches!(a, NameAttribute::All))
    );

    imap.set_flags(ME, "INBOX", 2, &["\\Seen"]).unwrap();
    let f: Vec<_> = s
        .uid_fetch(
            "1:*",
            "(UID FLAGS) (CHANGEDSINCE ".to_string() + &hms.to_string() + ")",
        )
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    let changed: Vec<_> = f.iter().filter_map(|f| f.uid).collect();
    assert_eq!(changed, vec![2]);
    assert!(f[0].modseq.unwrap() > hms);
    drop(s);

    // X-GM-THRID through a raw client (async-imap does not surface it).
    let mut r = Raw::connect(&imap).await;
    r.cmd("a", "ID NIL").await;
    r.cmd("b", &format!("LOGIN {ME} {PASS}")).await;
    r.cmd("c", "SELECT INBOX").await;
    let lines = r.cmd("d", "UID FETCH 1:3 (X-GM-THRID)").await;
    let ids: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split("X-GM-THRID ").nth(1))
        .map(|x| x.trim_end().trim_end_matches(')'))
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.iter().all(|i| *i == msgs[0].thread_id.to_string()));
}

#[tokio::test]
async fn literal_plus_append_and_utf7_folders() {
    let imap = start(ImapConfig::default()).await;
    imap.add_folder(ME, "已发送", Some("\\Sent")).unwrap();
    let mut r = Raw::connect(&imap).await;
    r.cmd("a", &format!("LOGIN {ME} {PASS}")).await;
    let list = r.cmd("b", "LIST \"\" \"*\"").await;
    assert!(
        list.iter()
            .any(|l| l.contains("\\Sent") && l.contains("\"&XfJT0ZAB-\"")),
        "{list:?}"
    );
    let msg = fixtures::get("thread-2.eml").unwrap();
    // LITERAL+: no continuation request expected.
    r.send(format!("c APPEND \"&XfJT0ZAB-\" (\\Seen) {{{}+}}\r\n", msg.len()).as_bytes())
        .await;
    r.send(msg).await;
    r.send(b"\r\n").await;
    let resp = r.until("c").await;
    assert!(resp.last().unwrap().contains("[APPENDUID"), "{resp:?}");
    assert_eq!(imap.messages(ME, "已发送").unwrap()[0].raw, msg);

    let idle = r.cmd("d", "SELECT \"&XfJT0ZAB-\"").await;
    assert!(idle.iter().any(|l| l.starts_with("* 1 EXISTS")));
    // IDLE through the raw client: inject into the selected folder.
    r.send(b"e IDLE\r\n").await;
    assert!(r.line().await.starts_with("+ "));
    imap.inject(ME, "已发送", fixtures::get("thread-1.eml").unwrap())
        .unwrap();
    assert_eq!(r.line().await, "* 2 EXISTS\r\n");
    r.send(b"DONE\r\n").await;
    assert!(r.until("e").await.last().unwrap().starts_with("e OK"));
}

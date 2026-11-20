//! SG23: exercise draft preparation through its public tool, real Store and fake transports.
mod support;

use rusqlite::params;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use steve_mail::attach::{FileStore, Refused};
use steve_mail::drafts::{MAX_BODY_CHARS, MAX_SUBJECT_CHARS};
use steve_mail::mcp::{AgentDraft, MAX_ATTACHMENTS, NOT_ABOUT};
use steve_mail::{ImapAdapter, Mail, repo};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, WS, WS2};
use steve_store::{FileRef, NewFile};
use support::*;

struct Files {
    store: steve_store::Store,
    calls: Mutex<Vec<(String, String, Vec<u8>)>>,
}
impl FileStore for Files {
    fn store(&self, ws: &str, bytes: &[u8], name: &str, by: &str) -> Result<FileRef, Refused> {
        self.calls
            .lock()
            .unwrap()
            .push((name.into(), by.into(), bytes.into()));
        let f = self
            .store
            .files()
            .create(&NewFile {
                id: steve_store::uid("fil"),
                workspace_id: ws.into(),
                sha256: hex::encode(Sha256::digest(bytes)),
                name: name.into(),
                mime: "text/plain".into(),
                size: bytes.len() as i64,
                created_by: by.into(),
            })
            .map_err(|e| Refused::new(500, e.to_string()))?;
        Ok(FileRef {
            id: f.id,
            name: f.name,
            mime: f.mime,
            size: f.size,
        })
    }
}

struct Fixture {
    h: H,
    mail: Mail,
    files: Arc<Files>,
    work: tempfile::TempDir,
    cx: McpCx,
    conn: String,
    thread: String,
}
fn message(subject: &str, id: &str) -> Vec<u8> {
    format!("From: Alice <alice@external.example>\r\nTo: Owner <{ADDR}>\r\nCc: Bob <bob@external.example>\r\nReply-To: desk@external.example\r\nMessage-ID: <{id}@example.com>\r\nSubject: {subject}\r\nDate: Fri, 20 Nov 2026 10:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nQuestion\r\n").into_bytes()
}
async fn fixture() -> Fixture {
    fixture_with_message(&message("Original subject", "sg23")).await
}

async fn fixture_with_message(message: &[u8]) -> Fixture {
    let h = setup().await;
    h.inject("INBOX", message);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let thread = h.by_subject(&conn, "Original subject").thread_id;
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    h.env.store().transaction(|tx|->steve_store::Result<()>{
        tx.execute_batch("INSERT INTO bots (id,workspace_id,name,harness,updated) VALUES ('mail','default','Mail','claude-code',1); INSERT INTO bot_workspaces (bot_id,workspace_id,created) VALUES ('mail','default',1);")?;
        tx.execute("INSERT INTO items (id,workspace_id,no,kind,title,status,stage,created,updated) VALUES ('ST-23',?1,23,'task','reply','run','',1,1)",[WS])?;
        steve_store::base::audit(tx,WS,ME,"item.create","ST-23",&serde_json::json!({}))?;
        steve_store::base::links::add(tx,WS,&steve_store::base::links::NewLink::new(steve_store::base::ObjRef::new("item","ST-23"),"about",steve_store::base::ObjRef::new("mail_thread",&thread),"mail"))?;
        tx.execute("INSERT INTO sessions (id,workspace_id,item_id,bot_id,harness,cwd,status,created,updated) VALUES ('sg23-session',?1,'ST-23','mail','claude-code',?2,'running',1,1)",params![WS,cwd.to_str().unwrap()])?;
        Ok(())
    }).unwrap();
    let files = Arc::new(Files {
        store: h.env.store().clone(),
        calls: Mutex::new(Vec::new()),
    });
    let mail = Mail::builder(h.env.modules.clone(), h.conns.clone(), h.cfg.clone())
        .drafts(h.link.clone())
        .adapter(Arc::new(ImapAdapter::new(h.cfg.clone())))
        .files(files.clone())
        .build();
    let cx = McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "mail".into(),
        item_id: Some("ST-23".into()),
        session_id: Some("sg23-session".into()),
    };
    Fixture {
        h,
        mail,
        files,
        work,
        cx,
        conn,
        thread,
    }
}
impl Fixture {
    fn drafts(&self) -> Vec<repo::DraftRow> {
        self.h
            .env
            .store()
            .reader(|c| -> steve_store::Result<_> {
                let mut s =
                    c.prepare("SELECT id FROM mail_drafts WHERE item_id='ST-23' ORDER BY rowid")?;
                let ids = s
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(ids)
            })
            .unwrap()
            .iter()
            .map(|id| {
                self.h
                    .env
                    .store()
                    .reader(|c| repo::draft(c, id))
                    .unwrap()
                    .unwrap()
            })
            .collect()
    }
    async fn error(&self, cx: &McpCx, draft: AgentDraft) -> String {
        self.mail.tool_draft(cx, draft).await.unwrap_err()
    }
}
fn draft() -> AgentDraft {
    AgentDraft {
        body: "answer".into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sg23_scope_and_validation_order_table() {
    let f = fixture().await;
    for (name, kind, want) in [
        ("no item", 0, NOT_ABOUT),
        ("foreign workspace", 1, NOT_ABOUT),
        (
            "blank before attachment count",
            2,
            "Write the reply in body.",
        ),
        (
            "attachment count before subject",
            3,
            "At most 10 attachments.",
        ),
        ("subject before cwd", 4, "The subject must be one line."),
        ("body length before cwd", 5, "The body is too long."),
    ] {
        let mut cx = f.cx.clone();
        let mut a = draft();
        match kind {
            0 => {
                cx.item_id = None;
                a.body.clear();
            }
            1 => cx.ws = WS2.into(),
            2 => {
                a.body = " \n\t ".into();
                a.attachments = vec!["missing".into(); MAX_ATTACHMENTS + 1];
            }
            3 => {
                a.attachments = vec!["missing".into(); MAX_ATTACHMENTS + 1];
                a.subject = Some("bad\nsubject".into());
            }
            4 => {
                cx.session_id = None;
                a.subject = Some("bad\rsubject".into());
                a.attachments = vec!["missing".into()];
            }
            5 => {
                cx.session_id = None;
                a.body = "x".repeat(MAX_BODY_CHARS + 1);
                a.attachments = vec!["missing".into()];
            }
            _ => unreachable!(),
        }
        assert_eq!(f.error(&cx, a).await, want, "{name}");
        assert!(f.drafts().is_empty());
        assert!(f.files.calls.lock().unwrap().is_empty());
    }
    assert!(f.h.smtp.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn sg23_existing_state_gates_precede_subject_and_file_checks() {
    let f = fixture().await;
    f.mail.tool_draft(&f.cx, draft()).await.unwrap();
    let original = f.drafts().remove(0);
    for (status, want) in [
        (
            "unknown",
            "The previous send outcome is unknown. Ask the user to reconcile it; do not replace or resend this draft.",
        ),
        (
            "approved",
            "The user already approved the current draft and it is being sent; it can no longer change.",
        ),
        (
            "sending",
            "The user already approved the current draft and it is being sent; it can no longer change.",
        ),
    ] {
        f.h.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "UPDATE mail_drafts SET status=?1 WHERE id=?2",
                    params![status, original.id],
                )?;
                Ok(())
            })
            .unwrap();
        let mut a = draft();
        a.subject = Some("bad\nsubject".into());
        a.attachments = vec!["missing".into()];
        assert_eq!(f.error(&f.cx, a).await, want);
        let rows = f.drafts();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].body, original.body);
        assert_eq!(rows[0].subject, original.subject);
        assert_eq!(rows[0].status, status);
    }
    assert!(f.h.smtp.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn sg23_context_signature_subject_reply_mode_and_author() {
    let f = fixture().await;
    f.h.env
        .store()
        .transaction(|tx| -> steve_mail::error::MailResult<()> {
            let mut s = repo::settings(tx, &f.conn)?.unwrap_or_default();
            s.signature = "  Regards  ".into();
            repo::update_settings(tx, &f.conn, &s, 1)
        })
        .unwrap();
    let mut a = draft();
    a.body = "  answer \n".into();
    a.reply_all = Some(true);
    f.mail.tool_draft(&f.cx, a).await.unwrap();
    let d = f.drafts().remove(0);
    assert_eq!(d.subject, "Re: Original subject");
    assert_eq!(d.body, "answer\n\nRegards");
    assert_eq!(d.author, "mail");
    assert_eq!(d.mode, "reply_all");
    assert_eq!(
        d.to.iter().map(|r| r.addr.as_str()).collect::<Vec<_>>(),
        ["desk@external.example"]
    );
    assert_eq!(
        d.cc.iter().map(|r| r.addr.as_str()).collect::<Vec<_>>(),
        ["bob@external.example"]
    );
    assert_eq!(d.thread_id.as_deref(), Some(f.thread.as_str()));
    assert_eq!(d.session_id.as_deref(), Some("sg23-session"));
    f.mail.tool_draft(&f.cx, draft()).await.unwrap();
    assert_eq!(
        f.drafts()[0].mode,
        "reply_all",
        "None inherits the existing mode"
    );
    let mut a = draft();
    a.reply_all = Some(false);
    a.subject = Some("  Custom  ".into());
    let mut cx = f.cx.clone();
    cx.bot_id.clear();
    f.mail.tool_draft(&cx, a).await.unwrap();
    let d = f.drafts().remove(0);
    assert_eq!(d.subject, "Custom");
    assert_eq!(d.author, "steve");
    assert_eq!(d.mode, "reply");
    assert!(d.cc.is_empty());
    let mut a = draft();
    a.subject = Some("界".repeat(MAX_SUBJECT_CHARS + 1));
    a.body = "x".repeat(MAX_BODY_CHARS - 9); // signature adds 9 scalar values
    f.mail.tool_draft(&f.cx, a).await.unwrap();
    let d = f.drafts().remove(0);
    assert_eq!(d.subject, "界".repeat(MAX_SUBJECT_CHARS));
    assert_eq!(d.body.chars().count(), MAX_BODY_CHARS);
}

#[tokio::test(flavor = "multi_thread")]
async fn sg23_attachment_cwd_path_boundary_and_deduplication() {
    let f = fixture().await;
    let mut a = draft();
    a.attachments = vec!["part.txt".into()];
    for session in [None, Some("missing")] {
        let mut cx = f.cx.clone();
        cx.session_id = session.map(str::to_owned);
        assert_eq!(
            f.error(&cx, a.clone()).await,
            "This tool needs a session with a working directory."
        );
    }
    let cwd = f.work.path().join("cwd");
    std::fs::write(cwd.join("part.txt"), b"content").unwrap();
    std::fs::write(f.work.path().join("outside.txt"), b"outside").unwrap();
    for path in ["../outside.txt", "/absolute.txt"] {
        let mut a = draft();
        a.attachments = vec![path.into()];
        assert_eq!(
            f.error(&f.cx, a).await,
            format!("{path}: give a path relative to your working directory, without \"..\".")
        );
    }
    let mut a = draft();
    a.attachments = vec!["missing".into()];
    assert_eq!(
        f.error(&f.cx, a).await,
        "missing: no such file in your working directory."
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f.work.path().join("outside.txt"), cwd.join("link")).unwrap();
        let mut a = draft();
        a.attachments = vec!["link".into()];
        assert_eq!(
            f.error(&f.cx, a).await,
            "link: the file is outside your working directory."
        );
    }
    assert!(f.drafts().is_empty());
    assert!(f.files.calls.lock().unwrap().is_empty());
    let mut a = draft();
    a.attachments = vec!["part.txt".into(), "./part.txt".into()];
    f.mail.tool_draft(&f.cx, a).await.unwrap();
    let d = f.drafts().remove(0);
    assert_eq!(d.files.len(), 1);
    assert_eq!(d.files[0].name, "part.txt");
    assert_eq!(
        *f.files.calls.lock().unwrap(),
        [("part.txt".into(), "mail".into(), b"content".to_vec())]
    );
    assert!(f.h.smtp.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn sg23_recipient_and_missing_context_errors() {
    let self_reply = String::from_utf8(message("Original subject", "sg23"))
        .unwrap()
        .replace(
            "Reply-To: desk@external.example",
            &format!("Reply-To: {ADDR}"),
        );
    let f = fixture_with_message(self_reply.as_bytes()).await;
    assert_eq!(
        f.error(&f.cx, draft()).await,
        "There is nobody to reply to in this thread."
    );
    f.h.env.store().transaction(|tx| -> steve_store::Result<()> {
        tx.execute("INSERT INTO mail_threads (id,workspace_id,connection_id,thread_key,subject,last_at,visibility,created_by,created,updated) SELECT 'sg23-empty',workspace_id,connection_id,'sg23-empty','Empty',last_at+1,visibility,created_by,created,updated FROM mail_threads WHERE id=?",[&f.thread])?;
        steve_store::base::links::add(tx,WS,&steve_store::base::links::NewLink::new(steve_store::base::ObjRef::new("item","ST-23"),"about",steve_store::base::ObjRef::new("mail_thread","sg23-empty"),"mail"))?;
        Ok(())
    }).unwrap();
    assert_eq!(
        f.error(&f.cx, draft()).await,
        "This thread has no messages yet."
    );
    f.h.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE connections SET workspace_id=?", [WS2])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        f.error(&f.cx, draft()).await,
        "The mailbox of this thread is not available."
    );
    assert!(f.drafts().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn sg23_only_the_latest_linked_thread_is_selected() {
    let f = fixture().await;
    f.h.inject("INBOX", &message("Second subject", "sg23-second"));
    f.h.inject("INBOX", &message("Unlinked subject", "sg23-third"));
    f.h.sync(&f.conn).await;
    let second = f.h.by_subject(&f.conn, "Second subject").thread_id;
    let unlinked = f.h.by_subject(&f.conn, "Unlinked subject").thread_id;
    f.h.env
        .store()
        .transaction(|tx| -> steve_store::Result<()> {
            steve_store::base::links::add(
                tx,
                WS,
                &steve_store::base::links::NewLink::new(
                    steve_store::base::ObjRef::new("item", "ST-23"),
                    "about",
                    steve_store::base::ObjRef::new("mail_thread", &second),
                    "mail",
                ),
            )?;
            for (id, at) in [(&f.thread, 1), (&second, 2), (&unlinked, 3)] {
                tx.execute(
                    "UPDATE mail_threads SET last_at=?1 WHERE id=?2",
                    params![at, id],
                )?;
            }
            Ok(())
        })
        .unwrap();
    f.mail.tool_draft(&f.cx, draft()).await.unwrap();
    let d = f.drafts().remove(0);
    assert_eq!(d.thread_id.as_deref(), Some(second.as_str()));
    assert_eq!(d.subject, "Re: Second subject");
    assert!(f.h.smtp.sent().is_empty());
}

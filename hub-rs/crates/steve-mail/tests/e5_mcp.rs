//! E5 acceptance: a session only reads the threads its own item is about;
//! `steve_mail_draft` overwrites the old draft and reopens the decision; attachments are copied to
//! `.steve/attachments/` in the session's working directory (paths checked); after `no` the session
//! gets the user's feedback. Plus `to-item`, the item panel, and: no MCP tool sends mail.
//! Only the in-process fake IMAP/SMTP and temp dirs.

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_mail::attach::{FileStore, Refused};
use steve_mail::fake::fixtures;
use steve_mail::items::{ItemRequest, MailItems, MailPanel};
use steve_mail::mcp::{self, END, NOT_ABOUT, SAVED};
use steve_mail::{DraftHooks, ImapAdapter, Mail, repo};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::decisions::dispatch;
use steve_modules::panels::ItemPanel;
use steve_modules::testkit::{ME, OTHER, WS, WS2, call, caller};
use steve_store::FileRef;
use steve_store::NewFile;
use support::*;

#[derive(Default)]
struct Hooks(Mutex<Vec<String>>);
impl DraftHooks for Hooks {
    fn rejected(&self, _: &str, session: &str, _: Option<&str>, message: &str) {
        self.0.lock().unwrap().push(format!("{session}|{message}"));
    }
    fn awaiting_approval(&self, _: &str, session: &str, item: Option<&str>) {
        self.0
            .lock()
            .unwrap()
            .push(format!("awaiting {session}|{}", item.unwrap_or("")));
    }
}

/// A small stand-in for steve-hub's store: the same layout, a crude type check.
struct Files {
    store: steve_store::Store,
    home: std::path::PathBuf,
}
impl FileStore for Files {
    fn store(&self, ws: &str, b: &[u8], name: &str, by: &str) -> Result<FileRef, Refused> {
        if b.is_empty() {
            return Err(Refused::new(400, "文件是空的"));
        }
        if b.starts_with(b"MZ") {
            return Err(Refused::new(415, "不支持这种文件"));
        }
        let sha = hex::encode(Sha256::digest(b));
        let id = steve_store::uid("fil");
        let p = self
            .home
            .join("data/files")
            .join(ws)
            .join(&sha[..2])
            .join(&id);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b).unwrap();
        let mime = if b.starts_with(b"%PDF-") {
            "application/pdf"
        } else {
            "text/plain; charset=utf-8"
        };
        let r = self
            .store
            .files()
            .create(&NewFile {
                id: id.clone(),
                workspace_id: ws.into(),
                sha256: sha,
                name: name.into(),
                mime: mime.into(),
                size: b.len() as i64,
                created_by: by.into(),
            })
            .map_err(|e| Refused::new(500, e.to_string()))?;
        Ok(FileRef {
            id: r.id,
            name: r.name,
            mime: r.mime,
            size: r.size,
        })
    }
}

/// steve-core's `createItem`: an item row and its `item.create` audit (whose user it is).
struct Items {
    store: steve_store::Store,
    made: Mutex<Vec<ItemRequest>>,
}
#[async_trait]
impl MailItems for Items {
    async fn create_item(&self, req: ItemRequest) -> Result<String, String> {
        let n = self.made.lock().unwrap().len() + 100;
        let id = format!("ST-{n}");
        new_item(&self.store, &req.ws, &id, &req.by);
        self.made.lock().unwrap().push(req);
        Ok(id)
    }
}

fn new_item(store: &steve_store::Store, ws: &str, id: &str, by: &str) {
    store
        .transaction(|tx| -> Result<(), steve_store::StoreError> {
            let no: i64 = id.trim_start_matches("ST-").parse().unwrap_or(1);
            tx.execute(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES (?, ?, ?, 'task', 't', 'open', '', 1, 1)",
                rusqlite::params![id, ws, no],
            )?;
            steve_store::base::audit(tx, ws, by, "item.create", id, &json!({}))?;
            Ok(())
        })
        .unwrap();
}

fn new_session(store: &steve_store::Store, ws: &str, id: &str, item: &str, cwd: &Path) {
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO sessions (id, workspace_id, item_id, no, bot_id, harness, cwd, status, created, updated) VALUES (?, ?, ?, 1, 'mail', 'claude', ?, 'running', 1, 1)",
                rusqlite::params![id, ws, item, cwd.to_string_lossy()],
            )?;
            Ok(())
        })
        .unwrap();
}

struct E5 {
    h: H,
    mail: Mail,
    hooks: Arc<Hooks>,
    items: Arc<Items>,
    /// the reply chain Alice ↔ me
    chain: String,
    /// the invoice with a PDF
    invoice: String,
    work: tempfile::TempDir,
}

async fn e5() -> E5 {
    let hooks = Arc::new(Hooks::default());
    let h = setup_with(Opts {
        hooks: Some(hooks.clone()),
        ..Default::default()
    })
    .await;
    let [t1, t2, t3] = fixtures::thread();
    h.inject("INBOX", t1);
    h.inject("Sent", t2);
    h.inject("INBOX", t3);
    h.inject("INBOX", fixtures::get("invoice-pdf.eml").unwrap());
    let conn = h.connect().await;
    h.sync(&conn).await;
    h.env
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('mail', 'default', '邮件', 'claude-code', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('mail', 'default', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let chain = h.by_subject(&conn, "预算审批流程").thread_id;
    let invoice = h
        .messages(&conn)
        .into_iter()
        .find(|m| m.has_attachments)
        .unwrap()
        .thread_id;
    let items = Arc::new(Items {
        store: h.env.store().clone(),
        made: Mutex::default(),
    });
    // the module with E5's ports (the hooks were bound by the first build of the shared link)
    let mail = Mail::builder(h.env.modules.clone(), h.conns.clone(), h.cfg.clone())
        .drafts(h.link.clone())
        .adapter(Arc::new(ImapAdapter::new(h.cfg.clone())))
        .files(Arc::new(Files {
            store: h.env.store().clone(),
            home: h.env.dir.path().to_path_buf(),
        }))
        .items(items.clone())
        .build();
    E5 {
        h,
        mail,
        hooks,
        items,
        chain,
        invoice,
        work: tempfile::tempdir().unwrap(),
    }
}

impl E5 {
    /// An item about `thread` (made through `to-item`) with a running session in a fresh work dir.
    async fn item_session(
        &self,
        thread: &str,
        session: &str,
    ) -> (String, McpCx, std::path::PathBuf) {
        let item = self
            .mail
            .to_item(&me(), thread, Default::default())
            .await
            .expect("to-item");
        let cwd = self.work.path().join(session);
        std::fs::create_dir_all(&cwd).unwrap();
        new_session(self.h.env.store(), WS, session, &item, &cwd);
        (item.clone(), cx(Some(&item), Some(session)), cwd)
    }

    async fn tool(&self, cx: &McpCx, name: &str, args: Value) -> Result<String, String> {
        mcp::call(&self.mail, cx, name, &args).await
    }

    fn drafts_of(&self, item: &str) -> Vec<repo::DraftRow> {
        let ids: Vec<String> = self
            .h
            .env
            .store()
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT id FROM mail_drafts WHERE item_id = ? ORDER BY created, rowid",
                )?;
                let v = st
                    .query_map([item], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, steve_store::StoreError>(v)
            })
            .unwrap();
        ids.iter()
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

    fn decision(&self, id: &str) -> steve_store::Decision {
        self.h
            .env
            .store()
            .reader(|c| steve_store::base::decisions::get(c, id))
            .unwrap()
            .unwrap()
    }
}

fn cx(item: Option<&str>, session: Option<&str>) -> McpCx {
    McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "mail".into(),
        item_id: item.map(String::from),
        session_id: session.map(String::from),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_reads_only_its_items_threads() {
    let e = e5().await;
    let (item, scx, _) = e.item_session(&e.chain, "ses_a").await;

    let text = e.tool(&scx, mcp::THREAD, json!({})).await.unwrap();
    assert!(
        text.starts_with(&format!("Email thread {}", e.chain)),
        "{text}"
    );
    assert!(text.contains("3 messages"), "{text}");
    assert!(text.contains("From: Alice"), "{text}");
    assert!(text.contains("--- 1/3"), "{text}");
    let first = text.find("--- 1/3").unwrap();
    let last = text.find("--- 3/3").unwrap();
    assert!(first < last, "oldest first");

    // another thread of the same mailbox, not linked to this item
    assert_eq!(
        e.tool(&scx, mcp::THREAD, json!({"thread": e.invoice}))
            .await,
        Err(NOT_ABOUT.to_string())
    );
    // an attachment of that thread is not reachable either
    let mla: String = e
        .h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT a.id FROM mail_attachments a JOIN mail_messages m ON m.id = a.message_id WHERE m.thread_id = ?",
                [&e.invoice],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let err = e
        .tool(&scx, mcp::ATTACHMENT, json!({"id": mla}))
        .await
        .unwrap_err();
    assert!(err.contains("no attachment"), "{err}");

    // an item about no thread, no item at all, and an item of another user
    new_item(e.h.env.store(), WS, "ST-7", ME);
    for c in [cx(Some("ST-7"), None), cx(None, Some("ses_a"))] {
        for t in [mcp::THREAD, mcp::DRAFT] {
            assert_eq!(
                e.tool(&c, t, json!({"body": "x"})).await,
                Err(NOT_ABOUT.to_string()),
                "{t}"
            );
        }
    }
    new_item(e.h.env.store(), WS, "ST-8", OTHER);
    e.h.env
        .store()
        .transaction(|tx| {
            steve_store::base::links::add(
                tx,
                WS,
                &steve_store::base::links::NewLink::new(
                    steve_store::base::ObjRef::new("item", "ST-8"),
                    "about",
                    steve_store::base::ObjRef::new("mail_thread", &e.chain),
                    "mail",
                ),
            )
        })
        .unwrap();
    let other = McpCx {
        user: OTHER.into(),
        ..cx(Some("ST-8"), None)
    };
    assert_eq!(
        e.tool(&other, mcp::THREAD, json!({})).await,
        Err(NOT_ABOUT.to_string()),
        "the thread is private to the mailbox owner"
    );
    // another workspace with the same item id
    let foreign = McpCx {
        ws: WS2.into(),
        ..cx(Some(&item), None)
    };
    assert_eq!(
        e.tool(&foreign, mcp::THREAD, json!({})).await,
        Err(NOT_ABOUT.to_string())
    );
    // unknown tool, bad arguments
    assert!(
        e.tool(&scx, "steve_mail_send", json!({}))
            .await
            .unwrap_err()
            .contains("not found")
    );
    assert!(
        e.tool(&scx, mcp::DRAFT, json!({"body": 1}))
            .await
            .unwrap_err()
            .contains("expected string, received number at body")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn draft_overwrites_reopens_and_hears_no() {
    let e = e5().await;
    let (item, scx, _) = e.item_session(&e.chain, "ses_d").await;

    let r = e
        .tool(&scx, mcp::DRAFT, json!({"body": "三步，我周五前提交。"}))
        .await
        .unwrap();
    assert_eq!(r, format!("{SAVED}{END}"));
    let d = e.drafts_of(&item);
    assert_eq!(d.len(), 1);
    let first = d[0].clone();
    assert_eq!(first.status, "pending");
    assert_eq!(first.author, "mail");
    assert_eq!(first.session_id.as_deref(), Some("ses_d"));
    assert_eq!(first.mode, "reply");
    assert_eq!(first.subject, "Re: 预算审批流程");
    // the server's recipients: Alice, never me
    assert!(!first.to.is_empty());
    assert!(first.to.iter().all(|r| !r.addr.eq_ignore_ascii_case(ADDR)));
    let dec1 = first.decision_id.clone().unwrap();
    let d1 = e.decision(&dec1);
    assert_eq!(d1.status, "open");
    assert_eq!(d1.item_id.as_deref(), Some(item.as_str()));
    assert_eq!(d1.session_id.as_deref(), Some("ses_d"));
    assert_eq!(d1.options, vec!["send".to_string(), "no".to_string()]);

    // the agent writes again: the same draft, the old card expired, a new one for this version
    e.tool(
        &scx,
        mcp::DRAFT,
        json!({"body": "改一下：周四前提交。", "subject": "Re: 预算审批（更新）", "reply_all": true}),
    )
    .await
    .unwrap();
    let d = e.drafts_of(&item);
    assert_eq!(d.len(), 1, "one unsent draft per item and thread");
    let second = d[0].clone();
    assert_eq!(second.id, first.id);
    assert_eq!(second.body, "改一下：周四前提交。");
    assert_eq!(second.subject, "Re: 预算审批（更新）");
    assert_eq!(second.mode, "reply_all");
    assert_ne!(second.content_hash, first.content_hash);
    let dec2 = second.decision_id.clone().unwrap();
    assert_ne!(dec2, dec1);
    assert_eq!(e.decision(&dec1).status, "expired");
    assert_eq!(e.decision(&dec2).status, "open");
    assert!(e.decision(&dec2).draft.contains("周四前提交"));
    // the stale card cannot approve anything
    assert!(
        dispatch(&e.h.env.modules, &me(), &dec1, "send", "")
            .unwrap()
            .is_err()
    );
    assert_eq!(
        e.hooks
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("awaiting ses_d"))
            .count(),
        2
    );

    // no: the session gets the feedback, the draft is discarded
    dispatch(&e.h.env.modules, &me(), &dec2, "no", "语气再正式一点")
        .unwrap()
        .unwrap();
    assert_eq!(e.drafts_of(&item)[0].status, "discarded");
    let heard = e.hooks.0.lock().unwrap().clone();
    assert!(
        heard.contains(&"ses_d|用户没有批准发送这封邮件：语气再正式一点。按意见修改后再用 steve_mail_draft，或者用 steve_ask 问用户。".to_string()),
        "{heard:?}"
    );

    // the revision is a fresh draft with its own decision; the user approves it
    e.tool(
        &scx,
        mcp::DRAFT,
        json!({"body": "您好，我们会在周四前提交。"}),
    )
    .await
    .unwrap();
    let d = e.drafts_of(&item);
    assert_eq!(d.len(), 2);
    let third = d[1].clone();
    assert_eq!(third.status, "pending");
    dispatch(
        &e.h.env.modules,
        &me(),
        third.decision_id.as_ref().unwrap(),
        "send",
        "",
    )
    .unwrap()
    .unwrap();
    assert_eq!(e.drafts_of(&item)[1].status, "approved");
    // approved and waiting out its delay: the agent cannot change it any more
    let err = e
        .tool(&scx, mcp::DRAFT, json!({"body": "再改"}))
        .await
        .unwrap_err();
    assert!(err.contains("already approved"), "{err}");
    assert_eq!(e.drafts_of(&item)[1].body, "您好，我们会在周四前提交。");

    // none of the tools sent anything
    assert!(e.h.smtp.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_land_in_the_workdir() {
    let e = e5().await;
    let (item, scx, cwd) = e.item_session(&e.invoice, "ses_f").await;

    let text = e.tool(&scx, mcp::THREAD, json!({})).await.unwrap();
    let line = text
        .lines()
        .find(|l| l.starts_with("- mla_"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.contains("application/pdf"), "{line}");
    let mla = line[2..].split(' ').next().unwrap().to_string();

    let r = e
        .tool(&scx, mcp::ATTACHMENT, json!({"id": mla}))
        .await
        .unwrap();
    assert!(r.starts_with("Saved to .steve/attachments/"), "{r}");
    let rel = r["Saved to ".len()..].split(' ').next().unwrap();
    assert!(rel.ends_with("-发票-2026-10.pdf"), "{rel}");
    let bytes = std::fs::read(cwd.join(rel)).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    assert_eq!(
        std::fs::read_to_string(cwd.join(".steve/attachments/.gitignore")).unwrap(),
        "*\n"
    );
    let file_id: Option<String> =
        e.h.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT file_id FROM mail_attachments WHERE id = ?",
                    [&mla],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
    let file_id = file_id.expect("landed");
    assert!(rel.contains(file_id.trim_start_matches("fil_")));
    // again: the same file, not a second one
    assert_eq!(
        e.tool(&scx, mcp::ATTACHMENT, json!({"id": mla}))
            .await
            .unwrap(),
        r
    );
    assert_eq!(
        e.h.count("SELECT count(*) FROM files WHERE workspace_id = ?", WS),
        1
    );

    // a planted symlink cannot carry the copy outside
    #[cfg(unix)]
    {
        let (_, cx2, cwd2) = {
            let cwd2 = e.work.path().join("ses_g");
            std::fs::create_dir_all(&cwd2).unwrap();
            new_session(e.h.env.store(), WS, "ses_g", &item, &cwd2);
            (item.clone(), cx(Some(&item), Some("ses_g")), cwd2)
        };
        let outside = e.work.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, cwd2.join(".steve")).unwrap();
        assert!(
            e.tool(&cx2, mcp::ATTACHMENT, json!({"id": mla}))
                .await
                .unwrap_err()
                .contains("outside")
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    // the agent attaches files from its working directory, only from there
    std::fs::write(cwd.join("summary.txt"), "核对结果：金额一致").unwrap();
    std::fs::write(e.work.path().join("secret.txt"), "s").unwrap();
    for bad in ["../secret.txt", "/etc/hosts", "missing.txt"] {
        let err = e
            .tool(&scx, mcp::DRAFT, json!({"body": "x", "attachments": [bad]}))
            .await
            .unwrap_err();
        assert!(err.starts_with(bad), "{bad}: {err}");
    }
    std::fs::write(cwd.join("tool.exe"), b"MZ\x90\x00").unwrap();
    assert!(
        e.tool(
            &scx,
            mcp::DRAFT,
            json!({"body": "x", "attachments": ["tool.exe"]})
        )
        .await
        .unwrap_err()
        .contains("不支持")
    );
    assert!(e.drafts_of(&item).is_empty(), "nothing written on errors");
    e.tool(
        &scx,
        mcp::DRAFT,
        json!({"body": "已核对，见附件。", "attachments": ["summary.txt", "./summary.txt"]}),
    )
    .await
    .unwrap();
    let d = e.drafts_of(&item);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].files.len(), 1);
    assert_eq!(d[0].files[0].name, "summary.txt");
    assert!(
        e.decision(d[0].decision_id.as_ref().unwrap())
            .draft
            .contains("附件：summary.txt")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn to_item_and_the_panel() {
    let e = e5().await;
    let r = steve_mail::api::routes(e.mail.clone()).with_state(e.h.env.modules.clone());
    let path = format!("/mail/threads/{}/to-item", e.chain);
    let (s, v) = call(
        &r,
        Some(&me()),
        "POST",
        &path,
        Some(&json!({"mode": "propose"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let item = v["item_id"].as_str().unwrap().to_string();
    {
        let made = e.items.made.lock().unwrap();
        assert_eq!(made.len(), 1);
        assert!(made[0].propose);
        assert_eq!(made[0].by, ME);
        assert_eq!(made[0].title, "预算审批流程");
        assert!(made[0].goal.contains("Alice"), "{}", made[0].goal);
        assert_eq!(made[0].source, ("邮件".to_string(), "mail".to_string()));
    }
    // once per thread while the item is open
    let (s, v) = call(&r, Some(&me()), "POST", &path, Some(&json!({}))).await;
    assert_eq!((s, v["item_id"].as_str()), (200, Some(item.as_str())));
    assert_eq!(e.items.made.lock().unwrap().len(), 1);
    // links: about the thread, from the message, raised by the sender
    let preds: Vec<(String, String)> =
        e.h.env
            .store()
            .reader(|c| {
                steve_store::base::links::from(
                    c,
                    WS,
                    &steve_store::base::ObjRef::new("item", &item),
                    None,
                )
            })
            .unwrap()
            .into_iter()
            .map(|l| (l.predicate, l.object_type))
            .collect();
    for want in [
        ("about", "mail_thread"),
        ("from", "mail_message"),
        ("raised_by", "party"),
    ] {
        assert!(
            preds.contains(&(want.0.to_string(), want.1.to_string())),
            "{preds:?}"
        );
    }
    // not visible / bad input
    let (s, _) = call(&r, Some(&caller(WS2, ME)), "POST", &path, Some(&json!({}))).await;
    assert_eq!(s, 404);
    let (s, _) = call(
        &r,
        Some(&caller(WS, OTHER)),
        "POST",
        &path,
        Some(&json!({})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(
        &r,
        Some(&me()),
        "POST",
        &path,
        Some(&json!({"mode": "later"})),
    )
    .await;
    assert_eq!(s, 400);

    // the panel: subject, the latest message, linked to the thread page
    let panels =
        e.h.env
            .store()
            .reader(|c| MailPanel.panels(c, WS, ME, &item))
            .unwrap();
    assert_eq!(panels.len(), 1);
    let p = &panels[0];
    assert_eq!(p.id, format!("mail.thread:{}", e.chain));
    assert_eq!(p.title, "往来邮件");
    let head = p.head.as_ref().unwrap();
    assert_eq!(head.text, "预算审批流程");
    assert_eq!(
        head.href.as_deref(),
        Some(format!("#/mail/t/{}", e.chain).as_str())
    );
    assert_eq!(p.rows.len(), 1);
    assert!(p.rows[0].text.contains("Alice"), "{:?}", p.rows);
    assert!(p.rows[0].sub.is_some());
    assert_eq!(p.foot.as_deref(), Some("共 3 封"));

    // a draft shows on the panel
    let cwd = e.work.path().join("ses_p");
    std::fs::create_dir_all(&cwd).unwrap();
    new_session(e.h.env.store(), WS, "ses_p", &item, &cwd);
    e.tool(
        &cx(Some(&item), Some("ses_p")),
        mcp::DRAFT,
        json!({"body": "好的"}),
    )
    .await
    .unwrap();
    let panels =
        e.h.env
            .store()
            .reader(|c| MailPanel.panels(c, WS, ME, &item))
            .unwrap();
    let badges = &panels[0].head.as_ref().unwrap().badges;
    assert!(badges.contains(&"草稿待批准".to_string()), "{badges:?}");
    assert!(badges.contains(&"待回复".to_string()), "{badges:?}");
    // another user sees nothing of a private mailbox
    assert!(
        e.h.env
            .store()
            .reader(|c| MailPanel.panels(c, WS, OTHER, &item))
            .unwrap()
            .is_empty()
    );

    // landing over REST
    let mla: String = e
        .h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT a.id FROM mail_attachments a JOIN mail_messages m ON m.id = a.message_id WHERE m.thread_id = ?",
                [&e.invoice],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let (s, v) = call(
        &r,
        Some(&me()),
        "POST",
        &format!("/mail/attachments/{mla}/file"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["mime"], "application/pdf");
    assert!(v["id"].as_str().unwrap().starts_with("fil_"));
    let (s, _) = call(
        &r,
        Some(&caller(WS, OTHER)),
        "POST",
        &format!("/mail/attachments/{mla}/file"),
        None,
    )
    .await;
    assert_eq!(s, 404);
}

//! E6 acceptance: a thread added to the knowledge base is searchable at once,
//! private, at level `reasoning`; a thread becoming `done` submits candidate memories (what you sent
//! and the last reply) once; the §10 example yields the right change lines, applies and undoes. Plus
//! the thread routes `archive`, `read` and `to-knowledge`. Fake IMAP/SMTP, a scripted gateway, temp dirs.

mod support;

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use steve_knowledge::extract::PROMPT_HEAD;
use steve_knowledge::{Knowledge, Purpose, Query, retrieve};
use steve_mail::config::targets::{KIND, OP_PATCH};
use steve_mail::knowledge::ThreadLoader;
use steve_mail::{ImapAdapter, Mail, nodes, repo};
use steve_modules::changes::{self, Origin};
use steve_modules::testkit::{ME, OTHER, WS, WS2, call, caller};
use steve_store::base::{Level, Viewer};
use support::*;

struct E6 {
    h: H,
    mail: Mail,
    kb_events: Arc<Mutex<Vec<String>>>,
    conn: String,
    thread: String,
}

async fn e6() -> E6 {
    let h = setup().await;
    let [t1, t2, t3] = fixtures();
    h.inject("INBOX", t1);
    h.inject("Sent", t2);
    h.inject("INBOX", t3);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let thread = h.by_subject(&conn, "预算审批流程").thread_id;
    let kb_events: Arc<Mutex<Vec<String>>> = Arc::default();
    let ev = kb_events.clone();
    let kb = Knowledge::with_loaders(
        Arc::new(steve_knowledge::NoGuard),
        move |e: steve_knowledge::KbEvent| ev.lock().unwrap().push(e.id),
        vec![Arc::new(ThreadLoader)],
    );
    let mail = Mail::builder(h.env.modules.clone(), h.conns.clone(), h.cfg.clone())
        .drafts(h.link.clone())
        .adapter(Arc::new(ImapAdapter::new(h.cfg.clone())))
        .knowledge(kb)
        .build();
    E6 {
        h,
        mail,
        kb_events,
        conn,
        thread,
    }
}

fn fixtures() -> [&'static [u8]; 3] {
    steve_mail::fake::fixtures::thread()
}

fn search(h: &H, user: &str, text: &str) -> Vec<steve_knowledge::Hit> {
    h.env
        .store()
        .reader(|c| {
            let viewer = Viewer::load(c, WS, user)?;
            let q = Query::new(
                &steve_knowledge::port::SqliteStore,
                &viewer,
                text,
                Purpose::Search,
            );
            retrieve(c, &q)
        })
        .expect("search")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_thread_added_to_the_knowledge_base_is_searchable_for_reasoning_only() {
    let e = e6().await;
    assert!(
        search(&e.h, ME, "部门负责人").is_empty(),
        "not indexed by default"
    );

    let doc = e.mail.to_knowledge(&me(), &e.thread).expect("add");
    let hits = search(&e.h, ME, "部门负责人");
    assert!(!hits.is_empty());
    assert!(
        hits.iter()
            .all(|h| h.obj_id == doc && h.level == Level::Reasoning)
    );
    let hit = &hits[0];
    assert!(hit.text.contains("部门负责人"), "{hit:#?}");
    assert_eq!(hit.obj_id, doc);
    assert_eq!(hit.level, Level::Reasoning);
    assert_eq!(hit.source_kind, "files");
    assert!(hit.title.contains("预算审批流程"), "{}", hit.title);
    assert!(hit.heading.contains("me@example.com"), "{}", hit.heading);
    assert!(
        !hit.text.contains("> "),
        "quotes are not indexed: {}",
        hit.text
    );
    assert_eq!(*e.kb_events.lock().unwrap(), vec![doc.clone()]);
    // private: another member of the workspace finds nothing
    assert!(search(&e.h, OTHER, "部门负责人").is_empty());

    // adding again keeps one document
    assert_eq!(e.mail.to_knowledge(&me(), &e.thread).unwrap(), doc);
    let n: i64 =
        e.h.env
            .store()
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM kb_docs WHERE ref = ?",
                    [&e.thread],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
    assert_eq!(n, 1);

    // the index queue re-reads it through ThreadLoader, not the attachment loader
    e.h.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE kb_docs SET sha256 = '' WHERE id = ?", [&doc])?;
            Ok(())
        })
        .unwrap();
    let kb = Knowledge::with_loaders(
        Arc::new(steve_knowledge::NoGuard),
        |_| {},
        vec![
            Arc::new(ThreadLoader),
            Arc::new(steve_knowledge::FilesLoader::new(e.h.env.dir.path())),
        ],
    );
    steve_knowledge::index::run_batch(&e.h.env.modules, &kb, std::slice::from_ref(&doc))
        .await
        .unwrap();
    let hits = search(&e.h, ME, "部门负责人");
    assert!(hits[0].text.contains("部门负责人"));
    assert_eq!(hits[0].level, Level::Reasoning);

    // someone who cannot see the thread
    assert!(e.mail.to_knowledge(&caller(WS, OTHER), &e.thread).is_err());
    assert!(e.mail.to_knowledge(&caller(WS2, ME), &e.thread).is_err());
}

const CANDIDATES: &str = r#"{"items":[{"kind":"fact","text":"预算审批分三步：部门负责人、财务、CFO","about":[],"level":"public","evidence":"三步：部门负责人、财务、CFO","dup_of":null,"conflicts":null}]}"#;

fn candidates(h: &H) -> Vec<(String, String, String, String, String)> {
    h.env
        .store()
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT m.text, m.level, m.status, mc.origin, mc.origin_id FROM mems m JOIN mem_candidates mc ON mc.mem_id = m.id ORDER BY m.rowid",
            )?;
            let v = st
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_done_thread_submits_candidates_once() {
    let e = e6().await;
    e.h.env.gateway.script(PROMPT_HEAD, CANDIDATES);
    let msg = e.h.by_subject(&e.conn, "预算审批流程").id;
    e.h.env
        .store()
        .transaction(|tx| -> Result<(), steve_mail::MailError> {
            tx.execute(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES ('ST-7', ?, 7, 'task', '回复预算审批', 'open', '', 1, 1)",
                [WS],
            )?;
            nodes::link_item(tx, WS, "ST-7", &msg, &e.thread)
        })
        .unwrap();

    e.mail.on_item_finished(WS, "ST-7").await.unwrap();
    let t =
        e.h.env
            .store()
            .reader(|c| repo::thread(c, &e.thread))
            .unwrap()
            .unwrap();
    assert_eq!(t.state, "done");
    let got = candidates(&e.h);
    assert_eq!(got.len(), 1, "{got:?}");
    let (text, level, status, origin, origin_id) = &got[0];
    assert_eq!(text, "预算审批分三步：部门负责人、财务、CFO");
    assert_eq!(
        level, "reasoning",
        "the stricter of public and the mail default"
    );
    assert_eq!(status, "candidate");
    assert_eq!(origin, "mail");
    assert_eq!(origin_id, &e.thread);

    // the model saw what you sent and the last reply, not the first question
    let p = e.h.env.gateway.last_prompt().unwrap();
    assert!(p.contains("三步：部门负责人、财务、CFO"), "{p}");
    assert!(p.contains("收到，谢谢"), "{p}");
    assert!(!p.contains("下周之前要提交"), "{p}");
    assert!(p.contains("邮件往来「"), "{p}");

    // finishing again (already done) does not ask again; neither does a direct second submit
    let calls = e.h.env.gateway.prompts.lock().unwrap().len();
    e.mail.on_item_finished(WS, "ST-7").await.unwrap();
    assert_eq!(e.mail.submit_done(WS, &e.thread).await.unwrap(), 0);
    assert_eq!(e.h.env.gateway.prompts.lock().unwrap().len(), calls);
    assert_eq!(candidates(&e.h).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_archive_on_done_still_submits_candidates() {
    let e = e6().await;
    e.h.env.gateway.script(PROMPT_HEAD, CANDIDATES);
    let msg = e.h.by_subject(&e.conn, "预算审批流程").id;
    e.h.env
        .store()
        .transaction(|tx| -> Result<(), steve_mail::MailError> {
            tx.execute(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES ('ST-7', ?, 7, 'task', '回复预算审批', 'open', '', 1, 1)",
                [WS],
            )?;
            tx.execute(
                "UPDATE mail_account_settings SET archive_on_done = 1 WHERE connection_id = ?",
                [&e.conn],
            )?;
            nodes::link_item(tx, WS, "ST-7", &msg, &e.thread)
        })
        .unwrap();
    // the mailbox refuses the archive
    e.h.imap.fail_next("LOGIN", "NO [UNAVAILABLE] try later");

    let archived = e.mail.on_item_finished(WS, "ST-7").await.unwrap();
    assert!(archived.is_empty(), "{archived:?}");
    assert_eq!(e.h.imap.messages(ADDR, "INBOX").unwrap().len(), 2);
    // the thread is done and its memories were asked for anyway
    assert_eq!(candidates(&e.h).len(), 1);
}

const EXAMPLE: &str = "邮件签名改成 — Peng，回客户的信先给结论";
const EXAMPLE_OPS: &str = r#"好的：{"ops":[{"op":"set","key":"signature","value":"— Peng"},{"op":"set","key":"duty","value":"回客户的信先给结论"}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn the_section_10_example_drafts_applies_and_undoes() {
    let e = e6().await;
    let m = &e.h.env.modules;
    // an existing duty, so the append is visible
    e.mail
        .patch_settings(
            &me(),
            &e.conn,
            &serde_json::from_value(json!({"duty": "用中文回信"})).unwrap(),
        )
        .unwrap();

    // the global candidates list the mailbox
    let cands = changes::candidates(m, &me()).unwrap();
    let line = format!("{KIND}:{} — 邮箱 {ADDR}", e.conn);
    assert!(
        cands.iter().any(|c| c.line() == line),
        "{:?}",
        cands.iter().map(|c| c.line()).collect::<Vec<_>>()
    );
    assert!(
        changes::candidates(m, &caller(WS, OTHER))
            .unwrap()
            .iter()
            .all(|c| !c.code.starts_with("mail_account:")),
        "someone else's mailbox is not offered"
    );

    e.h.env
        .gateway
        .script("You configure the mailbox", EXAMPLE_OPS);
    let p = changes::draft(m, &me(), KIND, Some(&e.conn), EXAMPLE, Origin::default())
        .await
        .unwrap();
    let prompt = e.h.env.gateway.last_prompt().unwrap();
    assert!(prompt.contains("signature · 签名 · text"), "{prompt}");
    assert!(prompt.contains("send_delay_s"), "{prompt}");
    assert!(prompt.contains("append it to \"duty\""), "{prompt}");
    let rows: Vec<(String, String, String)> = p
        .changes
        .iter()
        .map(|c| (c.label.clone(), c.from.clone(), c.to.clone()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("签名".into(), "（空）".into(), "— Peng".into()),
            (
                "职责（追加）".into(),
                "用中文回信".into(),
                "回客户的信先给结论".into()
            ),
        ]
    );
    assert_eq!(p.target, json!({"kind": KIND, "id": e.conn}));
    let ops: Vec<Value> =
        e.h.env
            .store()
            .reader(|c| steve_store::base::changesets::get(c, &p.id))
            .unwrap()
            .unwrap()
            .ops;
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["op"], OP_PATCH);

    // apply
    let r = changes::apply(m, &me(), &p.id).unwrap().unwrap();
    assert_eq!(r, json!({"ok": true, "undo": p.id}));
    let s = e.mail.settings(&me(), &e.conn).unwrap();
    assert_eq!(s.signature, "— Peng");
    assert_eq!(s.duty, "用中文回信\n回客户的信先给结论");

    // undo
    assert_eq!(
        changes::undo(m, &me(), &p.id).unwrap().unwrap(),
        json!({"ok": true})
    );
    let s = e.mail.settings(&me(), &e.conn).unwrap();
    assert_eq!(s.signature, "");
    assert_eq!(s.duty, "用中文回信");

    // a draft goes stale when the settings change underneath it
    let p2 = changes::draft(m, &me(), KIND, Some(&e.conn), EXAMPLE, Origin::default())
        .await
        .unwrap();
    e.mail
        .patch_settings(
            &me(),
            &e.conn,
            &serde_json::from_value(json!({"send_delay_s": 30})).unwrap(),
        )
        .unwrap();
    let err = changes::apply(m, &me(), &p2.id).unwrap().unwrap_err();
    assert!(format!("{err:?}").contains(changes::STALE), "{err:?}");

    // somebody else's view of the mailbox: 404
    assert!(
        changes::draft(
            m,
            &caller(WS, OTHER),
            KIND,
            Some(&e.conn),
            EXAMPLE,
            Origin::default()
        )
        .await
        .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn other_fields_and_unrecognized_values() {
    let e = e6().await;
    let m = &e.h.env.modules;
    e.h.env.gateway.script(
        "You configure the mailbox",
        r#"{"ops":[{"op":"set","key":"send_delay_s","value":"30 秒"},{"op":"set","key":"archive_on_done","value":"是"},{"op":"set","key":"sync_days","value":"45"},{"op":"set","key":"colour","value":"red"}]}"#,
    );
    let p = changes::draft(m, &me(), KIND, Some(&e.conn), "x", Origin::default())
        .await
        .unwrap();
    let rows: Vec<(String, String, String)> = p
        .changes
        .iter()
        .map(|c| (c.label.clone(), c.from.clone(), c.to.clone()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("发送延迟".into(), "10 秒".into(), "30 秒".into()),
            ("完成后归档".into(), "否".into(), "是".into()),
            ("回溯天数".into(), "—".into(), "（没认出：45）".into()),
            ("（没认出）".into(), "—".into(), "colour: red".into()),
        ]
    );
    changes::apply(m, &me(), &p.id).unwrap().unwrap();
    let s = e.mail.settings(&me(), &e.conn).unwrap();
    assert_eq!(
        (s.send_delay_s, s.archive_on_done, s.sync_days),
        (30, true, 30)
    );
    changes::undo(m, &me(), &p.id).unwrap().unwrap();
    let s = e.mail.settings(&me(), &e.conn).unwrap();
    assert_eq!((s.send_delay_s, s.archive_on_done), (10, false));
}

#[tokio::test(flavor = "multi_thread")]
async fn thread_routes_archive_read_and_to_knowledge() {
    let e = e6().await;
    let r = steve_mail::api::routes(e.mail.clone()).with_state(e.h.env.modules.clone());
    let base = format!("/mail/threads/{}", e.thread);

    let (s, v) = call(&r, Some(&me()), "POST", &format!("{base}/read"), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v, json!({"ok": true, "changed": 3}));
    let (_, v) = call(&r, Some(&me()), "POST", &format!("{base}/read"), None).await;
    assert_eq!(v["changed"], 0, "already read");
    assert!(
        e.h.imap
            .messages(ADDR, "INBOX")
            .unwrap()
            .iter()
            .all(|m| m.flags.contains(&"\\Seen".to_string()))
    );

    let (s, v) = call(&r, Some(&me()), "POST", &format!("{base}/archive"), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v, json!({"ok": true, "archived": 2}));
    assert!(e.h.imap.messages(ADDR, "INBOX").unwrap().is_empty());
    assert_eq!(e.h.imap.messages(ADDR, "Archive").unwrap().len(), 2);

    let (s, v) = call(
        &r,
        Some(&me()),
        "POST",
        &format!("{base}/to-knowledge"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["doc_id"].as_str().unwrap().starts_with("kd_"));

    // not theirs / another workspace / no such thread: 404, nothing touched at the provider
    for (who, path) in [
        (caller(WS, OTHER), format!("{base}/archive")),
        (caller(WS2, ME), format!("{base}/read")),
        (me(), "/mail/threads/mt_nope/archive".to_string()),
        (caller(WS, OTHER), format!("{base}/to-knowledge")),
    ] {
        let (s, v) = call(&r, Some(&who), "POST", &path, None).await;
        assert_eq!(s, 404, "{path}: {v}");
        assert_eq!(v["error"], "没有这个往来");
    }
}

//! E4 acceptance (email.md §4, §14): with the “邮件分诊” template installed, notification mail goes
//! to the digest without a model call; mail that needs a reply gets a draft and an approval decision
//! (event outcome `need`), and nothing is sent; later mail of the same thread supplements the item made
//! from the first one; dry runs do not draft; `mail.archive` / `mail.mark_read` run without approval;
//! `archive_on_done` archives the thread when its item finishes. The fake IMAP/SMTP, the flow engine on
//! the same store, a scripted fake gateway; no real mail server, no `~/.steve`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use steve_flow::testkit::{CoreCall, FakeCore};
use steve_flow::{FlowCx, FlowSetup, GraphEngine};
use steve_mail::compose::SAFETY;
use steve_mail::events::FnNotify;
use steve_mail::fake::fixtures;
use steve_mail::fake::{FakeImap, FakeSmtp, ImapConfig, SmtpConfig, TestCa, TlsMode};
use steve_mail::flow_events::FlowEvents;
use steve_mail::nodes::{self, MailHooks};
use steve_mail::repo::{self, MessageRow};
use steve_mail::template;
use steve_mail::usage::MailUsage;
use steve_mail::{ImapAdapter, Mail, MailConfig};
use steve_modules::Caller;
use steve_modules::accounts::connections::{Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::StoreError;
use tempfile::TempDir;
use zeroize::Zeroizing;

const ADDR: &str = "me@example.com";

const BOT_REPLY: &str = r#"{"branch":"需要回复","reason":"Alice 在问预算审批流程","fields":{}}"#;
const BOT_MONEY: &str = r#"{"branch":"有截止或涉及钱","reason":"涉及预算","fields":{}}"#;
const DRAFT: &str = r#"{"subject":"Re: x","body":"预算审批分三步：填表、主管签字、财务复核。","reply_all":false,"needs_attention":false,"why":"回答流程问题","cites":[]}"#;

struct H {
    env: TestEnv,
    _keys: TempDir,
    _ca_dir: TempDir,
    conns: Connections,
    mail: Mail,
    flow: Arc<FlowCx>,
    core: Arc<FakeCore>,
    hooks: MailHooks,
    imap: FakeImap,
    smtp: FakeSmtp,
}

async fn setup() -> H {
    let link = steve_mail::DraftLink::new();
    let core = Arc::new(FakeCore::default());
    let fsetup = FlowSetup::new(core.clone());
    let hooks = MailHooks::new();
    let (l2, h2) = (link.clone(), hooks.clone());
    let env = TestEnv::new(|r| {
        steve_modules::accounts::connections::register(r);
        r.decision(steve_mail::drafts::SUBJECT_TYPE, l2.handler());
        fsetup.register(r);
        r.item_hooks(h2.boxed());
    });
    *core.store.lock().unwrap() = Some(env.store().clone());
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, role, main, harness, enabled, updated) VALUES ('steve', 'default', 'Steve', '', 1, 'claude-code', 1, 1), ('mail', 'default', '邮件', '处理邮件', 0, 'claude-code', 1, 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1), ('mail', 'default', 1);",
            )?;
            Ok(())
        })
        .expect("bots");

    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let ca = TestCa::generate().expect("ca");
    let imap = FakeImap::start(ImapConfig {
        ca: Some(ca.clone()),
        ..Default::default()
    })
    .await
    .expect("imap");
    let smtp = FakeSmtp::start(SmtpConfig {
        tls: TlsMode::Implicit,
        ca: Some(ca.clone()),
        deliver_to: None,
        ..Default::default()
    })
    .await
    .expect("smtp");
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
        vault,
        ConnectionsConfig::new(vec!["http://127.0.0.1:8441".into()], true, |_| None),
    )
    .provider(adapter.clone())
    .usage(Arc::new(MailUsage::new(cfg.clone())))
    .build();

    let events = FlowEvents::new();
    let notified: Arc<Mutex<Vec<String>>> = Arc::default();
    let n = notified.clone();
    let mail = Mail::builder(env.modules.clone(), conns.clone(), cfg)
        .drafts(link)
        .adapter(adapter)
        .events(Arc::new(events.clone()))
        .notify(Arc::new(FnNotify(move |_ws: &str, id: &str| {
            n.lock().unwrap().push(id.to_string())
        })))
        .build();
    let flow = nodes::register(fsetup, &mail).build(env.modules.clone(), Arc::new(GraphEngine));
    events.bind(&flow);
    hooks.bind(&mail);
    H {
        env,
        _keys: keys,
        _ca_dir: ca_dir,
        conns,
        mail,
        flow,
        core,
        hooks,
        imap,
        smtp,
    }
}

fn me() -> Caller {
    caller(WS, ME)
}

impl H {
    /// Connects the fake mailbox, syncs once (the look-back: no events) and installs the template.
    async fn connect(&self) -> String {
        let mut f = std::collections::BTreeMap::new();
        let mut put = |k: &str, v: String| {
            f.insert(k.to_string(), Zeroizing::new(v));
        };
        put("address", ADDR.into());
        put("password", "app-password".into());
        put("imap_host", self.imap.host().into());
        put("imap_port", self.imap.port().to_string());
        put("imap_security", "tls".into());
        put("smtp_host", self.smtp.host().into());
        put("smtp_port", self.smtp.port().to_string());
        put("smtp_security", "tls".into());
        let conn = self
            .conns
            .create(&me(), "imap", &f, json!({}))
            .await
            .expect("connect")
            .id;
        self.mail.sync_now(WS, &conn).await.expect("first sync");
        let id = template::install(&self.flow, WS, &conn, ME).expect("install");
        assert!(id.is_some(), "installed on the first mailbox");
        conn
    }

    /// Delivers `eml` as new mail and waits until its event is settled; returns it.
    async fn receive(&self, conn: &str, eml: &[u8]) -> Value {
        self.imap.inject(ADDR, "INBOX", eml).expect("inject");
        let before = self.event_ids().len();
        self.mail.sync_now(WS, conn).await.expect("sync");
        let mut ev = Value::Null;
        for _ in 0..200 {
            let ids = self.event_ids();
            if ids.len() > before {
                ev = self.event(&ids[ids.len() - 1]);
                let open = self.count(
                    "SELECT COUNT(*) FROM flow_runs WHERE event_id = ? AND status IN ('running', 'starting')",
                    ev["id"].as_str().unwrap(),
                );
                if open == 0 && ev["outcome"] != "pending" {
                    return ev;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("event not settled: {ev}");
    }

    fn event_ids(&self) -> Vec<String> {
        self.env
            .store()
            .reader(|c| {
                let mut st = c.prepare("SELECT id FROM ext_events ORDER BY received, rowid")?;
                let v = st
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap()
    }

    fn event(&self, id: &str) -> Value {
        self.env
            .store()
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT id, type, outcome, outcome_label, decision_id, work_item_id, thread_key FROM ext_events WHERE id = ?",
                    [id],
                    |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?, "type": r.get::<_, String>(1)?,
                            "outcome": r.get::<_, String>(2)?, "label": r.get::<_, String>(3)?,
                            "decision_id": r.get::<_, Option<String>>(4)?,
                            "item_id": r.get::<_, Option<String>>(5)?,
                            "thread_key": r.get::<_, Option<String>>(6)?,
                        }))
                    },
                )?)
            })
            .unwrap()
    }

    fn count(&self, sql: &str, p: &str) -> i64 {
        self.env
            .store()
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [p], |r| r.get(0))?))
            .unwrap()
    }

    fn message(&self, conn: &str, subject: &str) -> MessageRow {
        let id: String = self
            .env
            .store()
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT id FROM mail_messages WHERE connection_id = ?1 AND subject LIKE ?2 || '%' ORDER BY date LIMIT 1",
                    [conn, subject],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        self.env
            .store()
            .reader(|c| repo::message(c, &id))
            .unwrap()
            .unwrap()
    }

    fn gateway_calls(&self) -> usize {
        self.env.gateway.prompts.lock().unwrap().len()
    }

    fn template_flow(&self) -> String {
        self.env
            .store()
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT id FROM flows WHERE origin = ?",
                    [template::ORIGIN],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }
}

/// A notification without bulk headers, from a noreply address.
fn noreply_eml() -> Vec<u8> {
    b"Date: Sun, 01 Nov 2026 08:00:00 +0000\r\nFrom: GitHub <noreply@github.example>\r\nTo: me@example.com\r\nSubject: [steve] CI passed\r\nMessage-ID: <ci-1@github.example>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nAll checks have passed.\r\n".to_vec()
}

#[tokio::test(flavor = "multi_thread")]
async fn notifications_go_to_the_digest_without_the_model() {
    let h = setup().await;
    let conn = h.connect().await;
    let flow = h.template_flow();
    assert_eq!(
        h.count("SELECT enabled FROM flows WHERE id = ?", &flow),
        1,
        "the template is installed enabled"
    );
    // the mailbox's triage bot defaults to the mail bot
    let triage: Option<String> = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT triage_bot FROM source_settings WHERE connection_id = ?",
                [&conn],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(triage.as_deref(), Some("mail"));

    let ev = h
        .receive(&conn, fixtures::get("newsletter.eml").unwrap())
        .await;
    assert_eq!(ev["outcome"], "digest", "{ev}");
    let ev = h.receive(&conn, &noreply_eml()).await;
    assert_eq!(ev["outcome"], "digest", "{ev}");
    assert_eq!(h.gateway_calls(), 0, "no model call for notifications");
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM digest_entries WHERE workspace_id = ?",
            WS
        ),
        2
    );

    // installing again (a second mailbox, a restart) does nothing; neither after you deleted it
    assert_eq!(template::install(&h.flow, WS, &conn, ME).unwrap(), None);
    h.env
        .store()
        .with_conn(|c| {
            c.execute("UPDATE flows SET deleted = 1 WHERE id = ?", [&flow])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(template::install(&h.flow, WS, &conn, ME).unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn needing_a_reply_drafts_and_asks_but_never_sends() {
    let h = setup().await;
    let conn = h.connect().await;
    h.env
        .gateway
        .script(steve_flow::nodes::bot::PROMPT_START, BOT_REPLY);
    h.env.gateway.script(SAFETY, DRAFT);

    let [t1, _, _] = fixtures::thread();
    let ev = h.receive(&conn, t1).await;
    assert_eq!(ev["outcome"], "need", "{ev}");

    let m = h.message(&conn, "预算审批流程");
    let drafts = h
        .env
        .store()
        .reader(|c| repo::thread_drafts(c, &m.thread_id))
        .unwrap();
    assert_eq!(drafts.len(), 1);
    let d = &drafts[0];
    assert_eq!(d.status, "pending");
    assert_eq!(d.author, "mail", "drafted by the mail bot");
    assert_eq!(d.reply_to_message.as_deref(), Some(m.id.as_str()));
    assert_eq!(d.to[0].addr, "alice@acme.example");
    assert!(d.run_id.is_some());
    let dec = d.decision_id.clone().expect("approval decision");
    let st: (String, String, String) = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT kind, subject_type, status FROM decisions WHERE id = ?",
                [&dec],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(st.0, "approve");
    assert_eq!(st.1, "mail_draft");
    assert_eq!(st.2, "open");
    let thread = h
        .env
        .store()
        .reader(|c| repo::thread(c, &m.thread_id))
        .unwrap()
        .unwrap();
    assert_eq!(thread.state, "needs_reply");
    // the bot judged, the draft was written: two model calls
    assert_eq!(h.gateway_calls(), 2);
    // nothing leaves the mailbox without the decision
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert!(h.smtp.sent().is_empty());

    // exactly one card: the send approval (prototype r1, no notify after the draft)
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM decisions WHERE status = 'open' AND workspace_id = ?",
            WS
        ),
        1
    );

    // the run went bot → draft and ended there; the draft step was not gated by a second approval
    let steps: Vec<String> = h
        .env
        .store()
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT s.node_id FROM flow_run_steps s JOIN flow_runs r ON r.id = s.run_id WHERE r.event_id = ? ORDER BY s.seq",
            )?;
            let v = st
                .query_map([ev["id"].as_str().unwrap()], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok::<_, StoreError>(v)
        })
        .unwrap();
    assert_eq!(steps, ["t", "f", "l", "k", "b", "r"]);
    let g = template::graph();
    let after_draft: Vec<&Value> = g["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["from"] == "r")
        .collect();
    assert!(
        after_draft.is_empty(),
        "nothing after draft_reply: {after_draft:?}"
    );
    assert!(
        !g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["config"]["action"] == "notify"),
        "no notify node in the template"
    );

    // a run replayed at the draft step (a restart mid-step) reuses its draft
    let run_id = d.run_id.clone().unwrap();
    let mut run = h
        .env
        .store()
        .reader(|c| steve_flow::runs::get(c, &run_id))
        .unwrap()
        .unwrap();
    run.status = "running".into();
    run.cursor = Some("r".into());
    run.ended = None;
    h.env
        .store()
        .transaction(|tx| steve_flow::runs::save(tx, &run))
        .unwrap();
    steve_flow::engine::drive(&h.flow, &run_id).await.unwrap();
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM mail_drafts WHERE workspace_id = ?",
            WS
        ),
        1
    );
    assert_eq!(h.gateway_calls(), 2, "no second draft call");
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM decisions WHERE subject_type = 'flow_step' AND workspace_id = ?",
            WS
        ),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn later_mail_of_the_thread_supplements_the_item() {
    let h = setup().await;
    let conn = h.connect().await;
    h.env
        .gateway
        .script(steve_flow::nodes::bot::PROMPT_START, BOT_MONEY);
    let [t1, _, t3] = fixtures::thread();
    let ev = h.receive(&conn, t1).await;
    assert_eq!(ev["outcome"], "item", "{ev}");
    let item = ev["item_id"].as_str().expect("item").to_string();
    let m1 = h.message(&conn, "预算审批流程");
    // the item is linked to the mail and its thread
    for (pred, ty, id) in [
        ("from", "mail_message", m1.id.as_str()),
        ("about", "mail_thread", m1.thread_id.as_str()),
    ] {
        let n: i64 = h
            .env
            .store()
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM links WHERE subject_type = 'item' AND subject_id = ?1 AND predicate = ?2 AND object_type = ?3 AND object_id = ?4",
                    [item.as_str(), pred, ty, id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(n, 1, "{pred} {ty}");
    }
    assert_eq!(
        h.env
            .store()
            .reader(|c| nodes::item_of_thread(c, WS, &m1.thread_id))
            .unwrap()
            .as_deref(),
        Some(item.as_str())
    );
    assert_eq!(h.gateway_calls(), 1);

    let ev3 = h.receive(&conn, t3).await;
    assert_eq!(ev3["thread_key"], json!(m1.thread_id), "same thread");
    assert_eq!(ev3["outcome"], "linked", "{ev3}");
    assert_eq!(ev3["item_id"], json!(item));
    assert!(
        h.core
            .calls()
            .iter()
            .any(|c| matches!(c, CoreCall::Supplement { item: i, .. } if *i == item)),
        "supplemented"
    );
    assert_eq!(
        h.gateway_calls(),
        1,
        "no second judgement for the same thread"
    );
    assert_eq!(h.core.created().len(), 1, "no second item");
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_runs_do_not_draft() {
    let h = setup().await;
    let conn = h.connect().await;
    // a stored mail event to try the flow on
    h.env
        .gateway
        .script(steve_flow::nodes::bot::PROMPT_START, BOT_MONEY);
    let [t1, _, _] = fixtures::thread();
    let ev = h.receive(&conn, t1).await;
    let event = h
        .env
        .store()
        .reader(|c| steve_flow::ingest::event_json(c, ev["id"].as_str().unwrap()))
        .unwrap();
    let f = h
        .env
        .store()
        .reader(|c| steve_flow::flows::get(c, &h.template_flow()))
        .unwrap()
        .unwrap();
    let before = h.gateway_calls();
    let run = steve_flow::engine::dry_run(&h.flow, &f, event, None, vec!["r".into()], None)
        .await
        .unwrap();
    let out: String = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT output FROM flow_run_steps WHERE run_id = ? AND node_id = 'r'",
                [&run],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let out: Value = serde_json::from_str(&out).unwrap();
    assert!(out["skipped"].is_string(), "{out}");
    assert_eq!(out["action"], "mail.draft_reply");
    assert_eq!(h.gateway_calls(), before, "no model call");
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM mail_drafts WHERE workspace_id = ?",
            WS
        ),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn archive_and_mark_read_run_without_approval() {
    let h = setup().await;
    let conn = h.connect().await;
    // your own flow: every mail.new → mark read → archive
    let g = json!({
        "nodes": [
            {"id": "t", "kind": "trigger", "config": {"source": "mail", "types": ["mail.new"]}},
            {"id": "m", "kind": "action", "config": {"action": "mail.mark_read"}},
            {"id": "a", "kind": "action", "config": {"action": "mail.archive"}}
        ],
        "edges": [{"from": "t", "to": "m"}, {"from": "m", "to": "a"}]
    });
    let reg = &h.flow.nodes;
    assert!(
        steve_flow::graph::validate(
            &json!({"nodes": [{"id": "t", "kind": "trigger", "config": {"source": "mail"}},
                              {"id": "a", "kind": "action", "config": {"action": "mail.archive", "x": 1}}],
                    "edges": [{"from": "t", "to": "a"}]}),
            reg
        )
        .is_err()
    );
    let graph = steve_flow::graph::validate(&g, reg).unwrap();
    assert!(!reg.effectful(graph.node("m").unwrap()));
    assert!(!reg.effectful(graph.node("a").unwrap()));
    h.env
        .store()
        .transaction(|tx| {
            // the template off, so only this flow runs
            tx.execute("UPDATE flows SET enabled = 0 WHERE origin = ?", [template::ORIGIN])?;
            tx.execute("DELETE FROM flow_triggers WHERE flow_id IN (SELECT id FROM flows WHERE origin = ?)", [template::ORIGIN])?;
            steve_flow::flows::create(
                tx,
                WS,
                ME,
                &steve_flow::flows::NewFlow {
                    kind: "flow",
                    name: "读完归档",
                    description: "",
                    graph: &graph,
                    origin: "me",
                    origin_label: "你画的",
                    enabled: true,
                },
                steve_store::now(),
            )
        })
        .unwrap();
    let ev = h
        .receive(&conn, fixtures::get("multipart-alternative.eml").unwrap())
        .await;
    assert_ne!(ev["outcome"], "need", "no approval asked: {ev}");
    assert!(
        h.imap.messages(ADDR, "INBOX").unwrap().is_empty(),
        "archived"
    );
    let archived = h.imap.messages(ADDR, "Archive").unwrap();
    assert_eq!(archived.len(), 1);
    assert!(archived[0].flags.contains(&"\\Seen".to_string()), "read");
    let m = h.message(&conn, "关于下周三的会议");
    assert!(m.flags.contains(&"seen".to_string()));
    assert!(!m.folders.contains(&"INBOX".to_string()));
    let t = h
        .env
        .store()
        .reader(|c| repo::thread(c, &m.thread_id))
        .unwrap()
        .unwrap();
    assert_eq!(t.unread, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_item_archives_its_thread_when_asked() {
    let h = setup().await;
    let conn = h.connect().await;
    h.env
        .gateway
        .script(steve_flow::nodes::bot::PROMPT_START, BOT_MONEY);
    let [t1, _, _] = fixtures::thread();
    let ev = h.receive(&conn, t1).await;
    let item_id = ev["item_id"].as_str().unwrap().to_string();
    let m = h.message(&conn, "预算审批流程");
    let item = h.env.store().items().get(&item_id).unwrap().unwrap();

    // off (the default): the thread is done, the mail stays in the inbox
    assert!(
        h.mail
            .on_item_finished(WS, &item_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.imap.messages(ADDR, "INBOX").unwrap().len(), 1);
    let t = h
        .env
        .store()
        .reader(|c| repo::thread(c, &m.thread_id))
        .unwrap()
        .unwrap();
    assert_eq!(t.state, "done");

    // on: through the B1 hook
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE mail_account_settings SET archive_on_done = 1 WHERE connection_id = ?",
                [&conn],
            )?;
            Ok(())
        })
        .unwrap();
    h.env.modules.registry.hooks().finished(&item).await;
    let mut done = false;
    for _ in 0..200 {
        if h.imap.messages(ADDR, "INBOX").unwrap().is_empty() {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(done, "archived after the item finished");
    assert_eq!(h.imap.messages(ADDR, "Archive").unwrap().len(), 1);
    let _ = &h.hooks;
}

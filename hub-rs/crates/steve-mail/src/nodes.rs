//! Triage nodes: the flow actions `mail.draft_reply`, `mail.archive` and
//! `mail.mark_read`, the mail links of items made from mail, the `thread_key → item` lookup behind
//! `item.link {by:"thread"}`, and `archive_on_done` (B1 `on_finished`).
//!
//! - `mail.draft_reply` drafts with the quick-draft path and opens the approval decision; it
//!   never sends — the only way out is a `send` choice on that decision. The draft *is* the
//!   approval request, so S2 does not put a second approval gate in front of it: the action is not
//!   `effectful` for the engine. What the spec asks of an outward node still holds: dry runs do not
//!   run it (the `action` node only describes non-built-in actions in a dry run), and a run replayed
//!   after a restart does not draft twice (the draft of the run is reused).
//! - `mail.archive` / `mail.mark_read` change only your own mailbox, reversibly: no approval.
//!   Dry runs skip them too.
//!
//! Wiring (integrator), after [`Mail`] and before `FlowSetup::build`:
//! ```ignore
//! let setup = steve_mail::nodes::register(setup, &mail);   // actions + `item.create`/`item.link` with mail links + thread finder
//! // registry, before Modules: r.item_hooks(mail_hooks.boxed());  later: mail_hooks.bind(&mail);
//! ```

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_flow::nodes::action::{ItemCreate, ItemLink};
use steve_flow::{Action, FlowResult, FlowSetup, ItemFinder, NodeOutcome, RunCx};
use steve_modules::Caller;
use steve_modules::hooks::ItemHooks;
use steve_store::Item;
use steve_store::base::ObjRef;
use steve_store::base::links::{self, NewLink};

use crate::compose::QuickDraft;
use crate::error::{MailError, MailResult};
use crate::repo;
use crate::store::Mailbox;
use crate::{FlagChange, Inner, Mail};

pub const DRAFT_REPLY: &str = "mail.draft_reply";
pub const ARCHIVE: &str = "mail.archive";
pub const MARK_READ: &str = "mail.mark_read";

/// Registers the mail actions, the mail-aware `item.create` / `item.link` and the thread lookup.
pub fn register(setup: FlowSetup, mail: &Mail) -> FlowSetup {
    setup
        .action(Arc::new(DraftReply { mail: mail.clone() }))
        .action(Arc::new(Archive { mail: mail.clone() }))
        .action(Arc::new(MarkRead { mail: mail.clone() }))
        .action(Arc::new(WithMailLinks(ItemCreate)))
        .action(Arc::new(WithMailLinks(ItemLink)))
        .item_finder("thread", Arc::new(ThreadItems))
}

// ---------------------------------------------------------------- config helpers

fn opt_str<'v>(cfg: &'v Value, key: &str) -> Result<Option<&'v str>, String> {
    match cfg.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key} 只能是文字")),
    }
}

fn no_config(cfg: &Value) -> Result<(), String> {
    match cfg.as_object() {
        Some(m) if m.keys().all(|k| k == "action") => Ok(()),
        Some(_) => Err("这个动作没有可以设置的项".into()),
        None => Err("参数不对".into()),
    }
}

/// The mail an event is about: `(connection, message)`.
fn trigger_mail(ev: &Value) -> Result<(String, String), String> {
    let conn = ev["connection_id"]
        .as_str()
        .filter(|_| ev["source"] == "mail");
    let msg = ev["payload"]["mail_message_id"]
        .as_str()
        .or_else(|| ev["external_id"].as_str());
    match (conn, msg) {
        (Some(c), Some(m)) => Ok((c.to_string(), m.to_string())),
        _ => Err("这个流程不是由邮件触发的".into()),
    }
}

impl Mail {
    /// The trigger message of `ws`, with the mailbox it is in.
    fn trigger_message(&self, ws: &str, ev: &Value) -> Result<(Mailbox, repo::MessageRow), String> {
        let (conn, mid) = trigger_mail(ev)?;
        let found = self.0.m.store.reader(|c| -> MailResult<_> {
            let mb = Mailbox::load(c, ws, &conn)?;
            let m =
                repo::message(c, &mid)?.filter(|m| m.workspace_id == ws && m.connection_id == conn);
            Ok((mb, m))
        });
        match found {
            Ok((mb, Some(m))) => Ok((mb, m)),
            Ok((_, None)) => Err("触发的邮件已经不在了".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------- mail.draft_reply

/// `mail.draft_reply {bot?, instruction?, reply_all?}`: a quick draft of a reply to the trigger
/// mail plus its approval decision. Event outcome `need`.
pub struct DraftReply {
    mail: Mail,
}

#[async_trait]
impl Action for DraftReply {
    fn name(&self) -> &'static str {
        DRAFT_REPLY
    }

    fn validate(&self, cfg: &Value) -> Result<(), String> {
        opt_str(cfg, "bot")?;
        if let Some(i) = opt_str(cfg, "instruction")? {
            if i.chars().count() > 2000 {
                return Err("起草要求太长了（最多 2000 字）".into());
            }
            steve_flow::expr::check_template(i)?;
        }
        match cfg.get("reply_all") {
            None | Some(Value::Null) | Some(Value::Bool(_)) => Ok(()),
            Some(_) => Err("reply_all 只能是 true 或 false".into()),
        }
    }

    fn outcome(&self) -> Option<&'static str> {
        Some("need")
    }

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let ws = cx.ws().to_string();
        let (mb, msg) = match self.mail.trigger_message(&ws, cx.event()) {
            Ok(x) => x,
            Err(e) => return NodeOutcome::Fail(e),
        };
        let run_id = cx.run.id.clone();
        // a run replayed after a restart keeps the draft it already made
        let existing = self
            .mail
            .0
            .m
            .store
            .reader(|c| draft_of_run(c, &ws, &run_id));
        let draft = match existing {
            Ok(Some(id)) => self
                .mail
                .0
                .m
                .store
                .reader(|c| repo::draft(c, &id))
                .ok()
                .flatten()
                .map(|d| (d.id, d.subject, d.author, d.decision_id)),
            Ok(None) => None,
            Err(e) => return NodeOutcome::Fail(e.to_string()),
        };
        let (draft_id, subject, author, decision_id) = match draft {
            Some(d) => d,
            None => {
                let render = |k: &str| {
                    cfg[k]
                        .as_str()
                        .map(|t| cx.render(t, input).trim().to_string())
                        .filter(|t| !t.is_empty())
                };
                let who = Caller {
                    ws: ws.clone(),
                    me: mb.owner.clone(),
                };
                let q = QuickDraft {
                    thread_id: msg.thread_id.clone(),
                    message_id: Some(msg.id.clone()),
                    instruction: render("instruction"),
                    bot: render("bot"),
                    reply_all: cfg["reply_all"].as_bool(),
                    item_id: None,
                    session_id: None,
                    run_id: Some(run_id),
                };
                match self.mail.quick_draft(&who, q).await {
                    Ok(d) => (d.id, d.subject, d.author, d.decision_id),
                    Err(e) => return NodeOutcome::Fail(format!("起草回复失败：{e}")),
                }
            }
        };
        cx.effects.outcome = Some("need".into());
        cx.effects.label = Some(cut(&format!("起草了回复，等你确认：{subject}"), 60));
        cx.effects.decision_id = decision_id.clone();
        cx.effects.handled_by = Some(author);
        NodeOutcome::next(json!({
            "draft_id": draft_id,
            "decision_id": decision_id,
            "subject": subject,
        }))
    }
}

fn cut(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

fn draft_of_run(c: &Connection, ws: &str, run: &str) -> MailResult<Option<String>> {
    Ok(c.query_row(
        "SELECT id FROM mail_drafts WHERE workspace_id = ? AND run_id = ? ORDER BY created LIMIT 1",
        params![ws, run],
        |r| r.get(0),
    )
    .optional()?)
}

// ---------------------------------------------------------------- mail.archive / mail.mark_read

/// `mail.archive {}`: archives the thread of the trigger mail. Does not change the outcome.
pub struct Archive {
    mail: Mail,
}

#[async_trait]
impl Action for Archive {
    fn name(&self) -> &'static str {
        ARCHIVE
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        no_config(cfg)
    }
    async fn run(&self, cx: &mut RunCx<'_>, _cfg: &Value, _input: &Value) -> NodeOutcome {
        let ws = cx.ws().to_string();
        let (_, msg) = match self.mail.trigger_message(&ws, cx.event()) {
            Ok(x) => x,
            Err(e) => return NodeOutcome::Fail(e),
        };
        match self.mail.archive_thread(&ws, &msg.thread_id).await {
            Ok(n) => NodeOutcome::next(json!({ "archived": n, "thread_id": msg.thread_id })),
            Err(e) => NodeOutcome::Fail(format!("归档失败：{e}")),
        }
    }
}

/// `mail.mark_read {}`: marks the trigger mail read. Does not change the outcome.
pub struct MarkRead {
    mail: Mail,
}

#[async_trait]
impl Action for MarkRead {
    fn name(&self) -> &'static str {
        MARK_READ
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        no_config(cfg)
    }
    async fn run(&self, cx: &mut RunCx<'_>, _cfg: &Value, _input: &Value) -> NodeOutcome {
        let ws = cx.ws().to_string();
        let (_, msg) = match self.mail.trigger_message(&ws, cx.event()) {
            Ok(x) => x,
            Err(e) => return NodeOutcome::Fail(e),
        };
        match self.mail.mark_read(&ws, &msg.id).await {
            Ok(changed) => NodeOutcome::next(json!({ "read": true, "changed": changed })),
            Err(e) => NodeOutcome::Fail(format!("标已读失败：{e}")),
        }
    }
}

impl Mail {
    /// Archives a thread at the provider: every received message still in an inbox-role folder is
    /// moved out (IMAP: to the archive folder; Gmail: `INBOX` removed). The local copies follow.
    /// Returns how many messages moved.
    pub async fn archive_thread(&self, ws: &str, thread_id: &str) -> MailResult<usize> {
        let (conn, msgs, folders) = self.0.m.store.reader(|c| -> MailResult<_> {
            let t = repo::thread(c, thread_id)?
                .filter(|t| t.workspace_id == ws)
                .ok_or_else(|| MailError::Api(steve_modules::ApiError::not_found("往来")))?;
            let msgs = repo::thread_messages(c, &t.id)?;
            let folders = repo::folders(c, &t.connection_id)?;
            Ok((t.connection_id, msgs, folders))
        })?;
        let inbox: Vec<&str> = folders
            .iter()
            .filter(|f| f.role == "inbox")
            .map(|f| f.ext_id.as_str())
            .collect();
        let archive = folders
            .iter()
            .find(|f| f.role == "archive")
            .map(|f| f.ext_id.clone());
        let todo: Vec<_> = msgs
            .into_iter()
            .filter(|m| {
                m.direction == "in" && m.folders.iter().any(|f| inbox.contains(&f.as_str()))
            })
            .collect();
        if todo.is_empty() {
            return Ok(0);
        }
        let (adapter, cx) = self.adapter_for_write(ws, &conn)?;
        let ch = FlagChange {
            archive: true,
            ..Default::default()
        };
        let exts: Vec<&str> = todo.iter().map(|m| m.ext_id.as_str()).collect();
        let results = adapter.set_flags_many(&cx, &exts, &ch).await;
        let now = self.now();
        // the local copies follow the messages that moved
        let mut moved = 0;
        let mut first_err = None;
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            for (m, r) in todo.iter().zip(results) {
                if let Err(e) = r {
                    first_err.get_or_insert(e);
                    continue;
                }
                let mut f: Vec<String> = m
                    .folders
                    .iter()
                    .filter(|f| !inbox.contains(&f.as_str()))
                    .cloned()
                    .collect();
                if let Some(a) = &archive
                    && !f.contains(a)
                {
                    f.push(a.clone());
                }
                repo::set_folders(tx, &m.id, &f)?;
                moved += 1;
            }
            if moved > 0 {
                repo::refresh_thread(tx, thread_id, now)?;
            }
            Ok(())
        })?;
        if moved > 0 {
            self.0.notify.thread(ws, thread_id);
        }
        match first_err {
            Some(e) => Err(e.into()),
            None => Ok(moved),
        }
    }

    /// Marks one message read at the provider and locally. `false` when it already was.
    pub async fn mark_read(&self, ws: &str, message_id: &str) -> MailResult<bool> {
        let m = self
            .0
            .m
            .store
            .reader(|c| repo::message(c, message_id))?
            .filter(|m| m.workspace_id == ws)
            .ok_or_else(|| MailError::Api(steve_modules::ApiError::not_found("邮件")))?;
        if m.flags.iter().any(|f| f == "seen") {
            return Ok(false);
        }
        let (adapter, cx) = self.adapter_for_write(ws, &m.connection_id)?;
        adapter
            .set_flags(
                &cx,
                &m.ext_id,
                &FlagChange {
                    seen: Some(true),
                    ..Default::default()
                },
            )
            .await?;
        let mut flags = m.flags.clone();
        flags.push("seen".into());
        let now = self.now();
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            repo::set_flags(tx, &m.id, &flags)?;
            repo::refresh_thread(tx, &m.thread_id, now)
        })?;
        self.0.notify.thread(ws, &m.thread_id);
        Ok(true)
    }
}

// ---------------------------------------------------------------- items made from mail

/// `item.create` / `item.link` as built in, plus the mail links of §4 when the event is a mail:
/// `links(item,'from','mail_message',ml)` and `links(item,'about','mail_thread',mt)`. Other events
/// go through unchanged.
pub struct WithMailLinks<A>(pub A);

#[async_trait]
impl<A: Action> Action for WithMailLinks<A> {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn effectful(&self, cfg: &Value) -> bool {
        self.0.effectful(cfg)
    }
    fn grant(&self, cfg: &Value) -> Option<steve_flow::nodes::Grant> {
        self.0.grant(cfg)
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        self.0.validate(cfg)
    }
    fn outcome(&self) -> Option<&'static str> {
        self.0.outcome()
    }
    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        let out = self.0.run(cx, cfg, input).await;
        if let (NodeOutcome::Next { .. }, Some(item), Ok((_, mid))) =
            (&out, cx.effects.item_id.clone(), trigger_mail(cx.event()))
        {
            let ws = cx.ws().to_string();
            let r = cx.flow.store().transaction(|tx| -> MailResult<()> {
                let Some(m) = repo::message(tx, &mid)?.filter(|m| m.workspace_id == ws) else {
                    return Ok(());
                };
                link_item(tx, &ws, &item, &m.id, &m.thread_id)
            });
            if let Err(e) = r {
                tracing::warn!(item = %item, "mail links of an item: {e}");
            }
        }
        out
    }
}

/// Links an item to the mail it came from and to its thread.
pub fn link_item(
    tx: &steve_store::Tx,
    ws: &str,
    item: &str,
    message: &str,
    thread: &str,
) -> MailResult<()> {
    let it = || ObjRef::new("item", item);
    links::add(
        tx,
        ws,
        &NewLink::new(it(), "from", ObjRef::new("mail_message", message), "sig"),
    )?;
    links::add(
        tx,
        ws,
        &NewLink::new(it(), "about", ObjRef::new("mail_thread", thread), "sig"),
    )?;
    Ok(())
}

/// The item a mail thread belongs to (`thread_key → item`): the latest item linked `about` it, open
/// ones first. `thread` is the `mail_threads` id, which is the `thread_key` of mail events.
pub fn item_of_thread(c: &Connection, ws: &str, thread: &str) -> MailResult<Option<String>> {
    Ok(c.query_row(
        "SELECT l.subject_id FROM links l JOIN items i ON i.id = l.subject_id AND i.workspace_id = l.workspace_id
          WHERE l.workspace_id = ?1 AND l.subject_type = 'item' AND l.predicate = 'about'
            AND l.object_type = 'mail_thread' AND l.object_id = ?2
          ORDER BY (i.archived != 0 OR i.status IN ('done', 'drop')), l.created DESC, l.rowid DESC LIMIT 1",
        params![ws, thread],
        |r| r.get(0),
    )
    .optional()?)
}

/// The threads an item is about.
pub fn threads_of_item(c: &Connection, ws: &str, item: &str) -> MailResult<Vec<String>> {
    Ok(
        links::from(c, ws, &ObjRef::new("item", item), Some("about"))?
            .into_iter()
            .filter(|l| l.object_type == "mail_thread")
            .map(|l| l.object_id)
            .collect(),
    )
}

/// `item.link {by:"thread"}` for mail events: [`item_of_thread`]. Asked before S2's own lookup
/// (items made from an earlier event of the same `thread_key`).
pub struct ThreadItems;

impl ItemFinder for ThreadItems {
    fn find(&self, c: &Connection, ws: &str, event: &Value) -> FlowResult<Option<String>> {
        if event["source"] != "mail" {
            return Ok(None);
        }
        let Some(t) = event["thread_key"].as_str().filter(|t| !t.is_empty()) else {
            return Ok(None);
        };
        item_of_thread(c, ws, t).map_err(|e| steve_modules::ApiError::Internal(e.to_string()))
    }
}

// ---------------------------------------------------------------- archive_on_done

/// The B1 item hooks of mail: when an item about mail threads finishes, the threads become `done`,
/// and those of mailboxes with `archive_on_done` are archived (in a spawned task: it talks to the
/// provider). Registered before [`Mail`] exists, so it is bound later.
#[derive(Clone, Default)]
pub struct MailHooks(Arc<OnceLock<Weak<Inner>>>);

impl MailHooks {
    pub fn new() -> Self {
        Self::default()
    }
    /// For `Registry::item_hooks`.
    pub fn boxed(&self) -> Box<dyn ItemHooks> {
        Box::new(self.clone())
    }
    pub fn bind(&self, mail: &Mail) {
        let _ = self.0.set(Arc::downgrade(&mail.0));
    }
    fn mail(&self) -> Option<Mail> {
        self.0.get().and_then(Weak::upgrade).map(Mail)
    }
}

#[async_trait]
impl ItemHooks for MailHooks {
    fn name(&self) -> &'static str {
        "mail"
    }
    async fn on_finished(&self, item: &Item) {
        let Some(mail) = self.mail() else {
            return;
        };
        let (ws, id) = (item.workspace_id.clone(), item.id.clone());
        tokio::spawn(async move {
            if let Err(e) = mail.on_item_finished(&ws, &id).await {
                tracing::warn!(item = %id, "mail: finishing the threads of an item: {e}");
            }
        });
    }
}

impl Mail {
    /// An item finished: its threads become `done`; threads of mailboxes with `archive_on_done` are
    /// archived, and threads that were not `done` before submit candidate memories (E6,
    /// [`Mail::submit_done`]; failures are logged). Returns the archived thread ids.
    pub async fn on_item_finished(&self, ws: &str, item: &str) -> MailResult<Vec<String>> {
        let now = self.now();
        let threads =
            self.0
                .m
                .store
                .transaction(|tx| -> MailResult<Vec<(String, bool, bool)>> {
                    let mut out = Vec::new();
                    for t in threads_of_item(tx, ws, item)? {
                        let Some(row) = repo::thread(tx, &t)?.filter(|r| r.workspace_id == ws)
                        else {
                            continue;
                        };
                        let newly = row.state != "done";
                        repo::set_thread_state(tx, &row.id, "done", now)?;
                        let archive = repo::settings(tx, &row.connection_id)?
                            .is_some_and(|s| s.archive_on_done);
                        out.push((row.id, archive, newly));
                    }
                    Ok(out)
                })?;
        let mut archived = Vec::new();
        for (t, archive, _) in &threads {
            self.0.notify.thread(ws, t);
            // one failing thread (mailbox down, auth paused) neither stops the others nor the
            // candidate memories below: the threads are already `done`, so there is no retry
            if *archive {
                match self.archive_thread(ws, t).await {
                    Ok(_) => archived.push(t.clone()),
                    Err(e) => tracing::warn!(thread = %t, "mail: archive on done: {e}"),
                }
            }
        }
        for (t, _, newly) in &threads {
            if *newly && let Err(e) = self.submit_done(ws, t).await {
                tracing::warn!(thread = %t, "mail: candidate memories of a done thread: {e}");
            }
        }
        Ok(archived)
    }
}

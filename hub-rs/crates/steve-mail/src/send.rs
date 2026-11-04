//! The only way mail leaves Steve (email.md §5.4): an approval decision answered `send`, a delay you can
//! undo, then [`Mail::execute`] — which checks the approval again, builds the MIME, refuses registered
//! vault secrets, and calls the adapter. `MailAdapter::send(` appears nowhere else in this crate.
//!
//! - [`MailDraftHandler`]: the `mail_draft` decision handler (`send`/`no`, `retry`/`discard`, undo);
//! - [`DraftLink`]: what the handler shares with [`Mail`] (the clock, the hooks, approvals that hit an
//!   edited draft and must be reopened);
//! - the queue: [`Mail::send_due`] every 2 seconds ([`Mail::spawn_send_queue`]).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::{WriteRefusal, may_write_in};
use steve_modules::accounts::service::current_party;
use steve_modules::contacts::people_repo;
use steve_modules::decisions::{DecisionCx, DecisionHandler, choice_result};
use steve_modules::outbound::{YOLO_ACTOR, decided_by_yolo, strip_ai_prefix, with_ai_prefix};
use steve_modules::{ApiError, ApiResult, Clock, SystemClock};
use steve_store::base;
use steve_store::{Decision, NewDecision, Tx, uid};
use tokio::task::JoinHandle;

use crate::adapter::SendMeta;
use crate::drafts::{self, FAIL_OPTIONS, MSG_ALREADY_SENT, MSG_EDITED, SUBJECT_TYPE, content_hash};
use crate::error::{AdapterError, MailError, MailResult};
use crate::repo::{self, DraftRow, MessageRow, Rcpt, ThreadRow};
use crate::store::{self, Mailbox};
use crate::{Mail, lock, parse, settings};

/// Attachments of one mail, together.
pub const MAX_ATTACHMENTS_BYTES: i64 = 20 * 1024 * 1024;
/// How often the queue looks for approved mail whose delay is over.
pub const QUEUE_EVERY: Duration = Duration::from_secs(2);
/// Bound provider waits; expiration means unknown, never permission to resend.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(120);
/// Optional provider-side copy must not extend the submission's uncertainty window.
pub const COPY_TIMEOUT: Duration = Duration::from_secs(5);
pub const ERR_UNKNOWN: &str =
    "发送结果不明，可能已经发出。请核实已发送邮件或询问收件人，确认未发送后才重试";
pub const UNKNOWN_OPTIONS: [&str; 2] = ["confirmed_not_sent", "discard"];

/// The capability a connection needs to send mail.
pub const SEND_CAP: &str = "mail.send";
pub const ERR_NO_APPROVAL: &str = "没有有效的批准";
pub const ERR_SECRET: &str = "正文里有保险箱里的机密，已拦下";

/// The vault's redactor as the sender needs it (accounts-vault.md §4.4; V4 implements it). Answers the
/// field labels of registered secret values found in `text`; any hit blocks the mail.
pub trait SecretScan: Send + Sync {
    fn find(&self, ws: &str, text: &str) -> Vec<String>;
}

/// No redactor installed: finds nothing.
pub struct NoSecrets;

impl SecretScan for NoSecrets {
    fn find(&self, _: &str, _: &str) -> Vec<String> {
        Vec::new()
    }
}

/// What steve-core does for drafts (the integrator implements it; calls come after the commit or from
/// inside the decision's transaction, so implementations should only queue work).
pub trait DraftHooks: Send + Sync {
    /// `no` on a draft written in a session: tell the agent (steve-core's `send`, list §6.5).
    fn rejected(&self, ws: &str, session_id: &str, item_id: Option<&str>, message: &str) {
        let _ = (ws, session_id, item_id, message);
    }
    /// The mail of an item went out: `finish(item, "邮件已发出")` when its acceptance is “草稿即交付”.
    fn sent(&self, ws: &str, item_id: &str, subject: &str) {
        let _ = (ws, item_id, subject);
    }
    /// `steve_mail_draft` opened an approval for a session's draft: the session now waits for the
    /// user like after `steve_request_approval` (pending = approval, item “在等你批准”).
    fn awaiting_approval(&self, ws: &str, session_id: &str, item_id: Option<&str>) {
        let _ = (ws, session_id, item_id);
    }
}

pub struct NoHooks;
impl DraftHooks for NoHooks {}

struct LinkInner {
    clock: OnceLock<Arc<dyn Clock>>,
    hooks: OnceLock<Arc<dyn DraftHooks>>,
    /// Drafts whose approval answered 409 because the content changed: reopened with the new version.
    stale: Mutex<BTreeSet<String>>,
}

/// Shared between the decision handler (registered before `Modules` exists) and [`Mail`]. Cheap to
/// clone.
#[derive(Clone)]
pub struct DraftLink(Arc<LinkInner>);

impl Default for DraftLink {
    fn default() -> Self {
        Self::new()
    }
}

impl DraftLink {
    pub fn new() -> Self {
        Self(Arc::new(LinkInner {
            clock: OnceLock::new(),
            hooks: OnceLock::new(),
            stale: Mutex::new(BTreeSet::new()),
        }))
    }

    /// The handler to register: `registry.decision(drafts::SUBJECT_TYPE, link.handler())`.
    pub fn handler(&self) -> Box<dyn DecisionHandler> {
        Box::new(MailDraftHandler(self.clone()))
    }

    pub(crate) fn bind(&self, clock: Arc<dyn Clock>, hooks: Arc<dyn DraftHooks>) {
        let _ = self.0.clock.set(clock);
        let _ = self.0.hooks.set(hooks);
    }

    fn now(&self) -> i64 {
        match self.0.clock.get() {
            Some(c) => c.now(),
            None => SystemClock.now(),
        }
    }

    pub(crate) fn hooks(&self) -> Arc<dyn DraftHooks> {
        self.0
            .hooks
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(NoHooks))
    }

    fn mark_stale(&self, id: &str) {
        lock(&self.0.stale).insert(id.to_string());
    }

    fn take_stale(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.0.stale))
            .into_iter()
            .collect()
    }
}

/// The `mail_draft` decision handler.
pub struct MailDraftHandler(DraftLink);

fn own(tx: &Tx, ws: &str, id: &str) -> ApiResult<DraftRow> {
    repo::draft(tx, id)
        .map_err(api)?
        .filter(|d| d.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("草稿"))
}

fn api(e: MailError) -> ApiError {
    match e {
        MailError::Api(a) => a,
        e => ApiError::Internal(e.to_string()),
    }
}

impl MailDraftHandler {
    fn approve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> MailResult<Value> {
        let now = self.0.now();
        let mut draft = own(tx, cx.ws, &d.subject_id)?;
        if draft.status != "pending" || draft.decision_id.as_deref() != Some(d.id.as_str()) {
            return Err(ApiError::conflict("这封邮件已经处理过了").into());
        }
        let mut result = choice_result(cx);
        match cx.choice {
            "send" => {
                if !cx.text.trim().is_empty() {
                    // your edit on the card: this version is what you approve
                    if cx.text.chars().count() > drafts::MAX_BODY_CHARS {
                        return Err(ApiError::invalid("text").into());
                    }
                    draft.body = cx.text.to_string();
                    draft.content_hash = content_hash(&draft);
                    repo::update_draft_content(tx, &draft, now)?;
                } else if content_hash(&draft) != draft.content_hash {
                    self.0.mark_stale(&draft.id);
                    return Err(ApiError::conflict(MSG_EDITED).into());
                }
                let delay = repo::settings(tx, &draft.connection_id)?
                    .unwrap_or_default()
                    .send_delay_s
                    .clamp(0, 60);
                let send_at = now + delay * 1000;
                repo::draft_transition(tx, &draft.id, &["pending"], "approved", now)?;
                repo::set_draft_send_at(tx, &draft.id, Some(send_at))?;
                repo::set_draft_error(tx, &draft.id, None)?;
                let yolo = cx.actor == YOLO_ACTOR;
                if let Some(item) = &draft.item_id {
                    let (who, line) = if yolo {
                        ("steve", format!("YOLO：批准发送邮件「{}」", draft.subject))
                    } else {
                        ("me", format!("批准发送邮件「{}」", draft.subject))
                    };
                    people_repo::timeline_add(tx, item, who, "sys", &line)?;
                }
                base::audit(
                    tx,
                    cx.ws,
                    cx.actor,
                    "mail_draft.approve",
                    &draft.id,
                    &json!({"decision": d.id, "edited": !cx.text.trim().is_empty()}),
                )?;
                result["content_hash"] = json!(draft.content_hash);
                result["send_at"] = json!(send_at);
                if yolo {
                    result["decided_by"] = json!(YOLO_ACTOR);
                }
            }
            "no" => {
                repo::draft_transition(tx, &draft.id, &["pending"], "discarded", now)?;
                if let Some(t) = &draft.thread_id {
                    repo::set_thread_state(tx, t, "open", now)?;
                }
                base::audit(
                    tx,
                    cx.ws,
                    cx.actor,
                    "mail_draft.reject",
                    &draft.id,
                    &json!({"decision": d.id}),
                )?;
                if let Some(s) = &draft.session_id {
                    let note = cx.text.trim();
                    let msg = if note.is_empty() {
                        "用户没有批准发送这封邮件。按意见修改后再用 steve_mail_draft，或者用 steve_ask 问用户。".to_string()
                    } else {
                        format!(
                            "用户没有批准发送这封邮件：{note}。按意见修改后再用 steve_mail_draft，或者用 steve_ask 问用户。"
                        )
                    };
                    self.0
                        .hooks()
                        .rejected(cx.ws, s, draft.item_id.as_deref(), &msg);
                }
            }
            _ => return Err(ApiError::invalid("choice").into()),
        }
        Ok(result)
    }

    fn failed(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> MailResult<Value> {
        let now = self.0.now();
        let draft = own(tx, cx.ws, &d.subject_id)?;
        let uncertain = draft.status == "unknown";
        if draft.status != "failed" && !uncertain {
            return Err(ApiError::conflict("这封邮件已经处理过了").into());
        }
        if uncertain {
            // Normal REST callers are authenticated users; autonomous decisions use synthetic actors.
            // Check membership as well as known actor names so arbitrary bot ids cannot opt in.
            let member: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM users u JOIN workspace_members wm ON wm.user_id = u.id WHERE u.id = ?1 AND wm.workspace_id = ?2)",
                [cx.actor, cx.ws], |r| r.get(0),
            )?;
            if cx.actor == YOLO_ACTOR || cx.actor == "steve" || !member {
                return Err(ApiError::rule("发送结果不明，只能由登录用户核实后处理").into());
            }
        }
        match cx.choice {
            "retry" if !uncertain => {
                repo::draft_transition(tx, &draft.id, &["failed"], "approved", now)?;
                repo::set_draft_send_at(tx, &draft.id, Some(now))?;
            }
            "confirmed_not_sent" if uncertain => {
                repo::draft_transition(tx, &draft.id, &["unknown"], "approved", now)?;
                repo::set_draft_send_at(tx, &draft.id, Some(now))?;
            }
            "discard" => {
                repo::draft_transition(tx, &draft.id, &["failed", "unknown"], "discarded", now)?;
            }
            _ => return Err(ApiError::invalid("choice").into()),
        }
        base::audit(
            tx,
            cx.ws,
            cx.actor,
            &format!("mail_draft.{}", cx.choice),
            &draft.id,
            &json!({"decision": d.id}),
        )?;
        let mut result = choice_result(cx);
        if cx.actor == YOLO_ACTOR && cx.choice == "retry" {
            result["decided_by"] = json!(YOLO_ACTOR);
        }
        Ok(result)
    }
}

impl DecisionHandler for MailDraftHandler {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        match d.kind.as_str() {
            "approve" => self.approve(tx, cx, d),
            "fail" => self.failed(tx, cx, d),
            _ => Err(ApiError::rule("不认识这种决定").into()),
        }
        .map_err(api)
    }

    /// Undo before the delay is over puts the draft back to `pending`; afterwards `409 邮件已经发出，不能撤回`.
    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        let now = self.0.now();
        let Some(draft) = repo::draft(tx, &d.subject_id).map_err(api)? else {
            return Ok(());
        };
        let choice = d.result.get("choice").and_then(Value::as_str).unwrap_or("");
        match (d.kind.as_str(), choice) {
            ("approve", "send") => undo_approval(tx, &draft, now).map_err(api),
            ("approve", "no") if draft.status == "discarded" => {
                repo::draft_transition(tx, &draft.id, &["discarded"], "pending", now)
                    .map_err(api)?;
                Ok(())
            }
            ("fail", _) if ["approved", "discarded"].contains(&draft.status.as_str()) => {
                let previous = if d.options.iter().any(|o| o == "confirmed_not_sent") {
                    "unknown"
                } else {
                    "failed"
                };
                repo::draft_transition(tx, &draft.id, &["approved", "discarded"], previous, now)
                    .map_err(api)?;
                repo::set_draft_send_at(tx, &draft.id, None).map_err(api)?;
                Ok(())
            }
            _ if ["sending", "sent"].contains(&draft.status.as_str()) => {
                Err(ApiError::conflict(MSG_ALREADY_SENT))
            }
            _ => Ok(()),
        }
    }
}

/// Approved and still inside the delay → back to `pending`; otherwise 409.
pub(crate) fn undo_approval(tx: &Tx, d: &DraftRow, now: i64) -> MailResult<()> {
    match d.status.as_str() {
        "approved" if d.send_at.is_some_and(|t| now < t) => {
            if !repo::draft_transition(tx, &d.id, &["approved"], "pending", now)? {
                return Err(ApiError::conflict(MSG_ALREADY_SENT).into());
            }
            repo::set_draft_send_at(tx, &d.id, None)?;
            if let Some(t) = &d.thread_id {
                repo::set_thread_state(tx, t, "needs_reply", now)?;
            }
            Ok(())
        }
        "approved" | "sending" | "sent" => Err(ApiError::conflict(MSG_ALREADY_SENT).into()),
        _ => Err(ApiError::conflict("这封邮件没有在等待发出").into()),
    }
}

/// What [`Mail::execute`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum SendOutcome {
    /// Sent; the outgoing `mail_messages` row.
    Sent { message: String },
    /// Refused before anything left (no valid approval, a vault secret): audit `mail.send.blocked`.
    Blocked { reason: String },
    /// The provider failed; a `fail` decision is open.
    Failed { error: String },
    /// The provider may have accepted it. Requires human reconciliation, never automatic retry.
    Unknown { error: String },
    /// Nothing to do (not due, already sending or sent, discarded, gone).
    Skipped,
}

/// Stable for one draft/content intent, including a user-authorized retry. This helps reconciliation;
/// SMTP servers are not required to deduplicate it.
fn message_id(d: &DraftRow) -> String {
    format!("{}.{}@steve.local", d.id, d.content_hash)
}

struct SendGuard {
    mail: Mail,
    draft: DraftRow,
    completed: bool,
}
impl Drop for SendGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        // Conditional transition: an error after a committed outcome cannot overwrite it.
        if let Err(e) = self.mail.unknown(&self.draft, "发送任务中断") {
            tracing::error!(draft = %self.draft.id, error = %e, "mail send recovery failed; claim remains for startup recovery");
        }
    }
}

/// The MIME message and its envelope.
struct Built {
    mime: Vec<u8>,
    message_id: String,
    in_reply_to: Option<String>,
    refs: Vec<String>,
    meta: SendMeta,
    from_name: String,
    mailbox: Mailbox,
    thread_key: Option<String>,
}

/// `<p>`-free minimal HTML from plain text: escaped, line breaks kept.
pub fn text_to_html(text: &str) -> String {
    let mut out = String::from("<div style=\"white-space:pre-wrap\">");
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>\n"),
            c => out.push(c),
        }
    }
    out.push_str("</div>");
    out
}

fn addr_list(l: &[Rcpt]) -> Address<'static> {
    Address::new_list(
        l.iter()
            .map(|r| {
                Address::new_address((!r.name.is_empty()).then(|| r.name.clone()), r.addr.clone())
            })
            .collect(),
    )
}

pub(crate) fn files_dir(
    home: &std::path::Path,
    ws: &str,
    sha: &str,
    id: &str,
) -> Option<std::path::PathBuf> {
    let safe = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    (safe(ws) && safe(id) && sha.len() >= 2 && safe(sha))
        .then(|| home.join("data/files").join(ws).join(&sha[..2]).join(id))
}

impl Mail {
    fn blocked(&self, tx: &Tx, d: &DraftRow, reason: &str, detail: Value) -> MailResult<()> {
        let mut detail = detail;
        detail["draft"] = json!(d.id);
        detail["reason"] = json!(reason);
        base::audit(
            tx,
            &d.workspace_id,
            "steve",
            "mail.send.blocked",
            &d.id,
            &detail,
        )?;
        Ok(())
    }

    /// Step 1 of §5.4: `approved → sending` (conditional, so a mail goes out once) and the approval
    /// checked again — a resolved `send` decision about this draft for exactly this content.
    fn claim(&self, id: &str, now: i64) -> MailResult<Result<(DraftRow, String), SendOutcome>> {
        self.0.m.store.transaction(|tx| {
            let Some(d) = repo::draft(tx, id)? else {
                return Ok(Err(SendOutcome::Skipped));
            };
            match d.status.as_str() {
                "approved" => {}
                "sending" | "sent" | "discarded" | "unknown" => return Ok(Err(SendOutcome::Skipped)),
                st => {
                    self.blocked(tx, &d, "not_approved", json!({"status": st}))?;
                    return Ok(Err(SendOutcome::Blocked {
                        reason: ERR_NO_APPROVAL.into(),
                    }));
                }
            }
            if d.send_at.is_some_and(|t| t > now) {
                return Ok(Err(SendOutcome::Skipped));
            }
            // the connection as it is now, not as it was when you approved
            match may_write_in(tx, &d.workspace_id, &d.connection_id, Some(SEND_CAP))? {
                Ok(()) => {}
                Err(WriteRefusal::Later(_)) => return Ok(Err(SendOutcome::Skipped)),
                Err(WriteRefusal::Never(why)) => {
                    repo::draft_transition(tx, id, &["approved"], "failed", now)?;
                    repo::set_draft_error(tx, id, Some(&why))?;
                    self.blocked(tx, &d, "connection", json!({"why": why}))?;
                    return Ok(Err(SendOutcome::Blocked { reason: why }));
                }
            }
            if !repo::draft_transition(tx, id, &["approved"], "sending", now)? {
                return Ok(Err(SendOutcome::Skipped));
            }
            let approval = repo::send_approval(tx, &d.workspace_id, id)?;
            let current = content_hash(&d);
            let ok = approval.as_ref().is_some_and(|a| {
                a.result.get("content_hash").and_then(Value::as_str)
                    == Some(d.content_hash.as_str())
            }) && current == d.content_hash;
            let Some(a) = approval.filter(|_| ok) else {
                repo::draft_transition(tx, id, &["sending"], "failed", now)?;
                repo::set_draft_error(tx, id, Some(ERR_NO_APPROVAL))?;
                self.blocked(tx, &d, "no_approval", json!({}))?;
                return Ok(Err(SendOutcome::Blocked {
                    reason: ERR_NO_APPROVAL.into(),
                }));
            };
            base::audit(tx, &d.workspace_id, "steve", "mail.send.attempt", &d.id,
                &json!({"message_id": message_id(&d), "content_hash": d.content_hash, "decision": a.id}))?;
            // `[AI] ` on the first line of the body only for YOLO's approval; a mark the text brought
            // along is removed otherwise. The subject stays as written.
            let mut d = d;
            d.body = if decided_by_yolo(&a.result) {
                with_ai_prefix(&d.body)
            } else {
                strip_ai_prefix(&d.body).to_string()
            };
            Ok(Ok((d, a.id)))
        })
    }

    /// Step 2: the MIME message (`mail-builder`), the same for every adapter.
    fn build(&self, c: &Connection, d: &DraftRow, now: i64) -> MailResult<Built> {
        let mailbox = Mailbox::load(c, &d.workspace_id, &d.connection_id)?;
        let conn = steve_modules::accounts::connections::repo::get(c, &d.connection_id)?
            .ok_or_else(|| MailError::Api(ApiError::not_found("连接")))?;
        let from = conn
            .config
            .get("address")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .or_else(|| mailbox.my_addrs.first().cloned())
            .ok_or_else(|| MailError::Other("这个邮箱没有发件地址".into()))?;
        let party: Option<String> = {
            use rusqlite::OptionalExtension;
            c.prepare_cached("SELECT party_id FROM accounts WHERE id = ?")?
                .query_row([&mailbox.account_id], |r| r.get(0))
                .optional()?
        };
        let from_name = match party {
            Some(p) => current_party(c, &p)
                .map_err(MailError::Api)?
                .map(|p| p.name)
                .unwrap_or_default(),
            None => String::new(),
        };
        let (in_reply_to, refs) = match &d.reply_to_message {
            Some(m) if d.mode != "forward" && d.mode != "new" => match repo::message(c, m)? {
                Some(orig) => drafts::reply_headers(&orig),
                None => (None, Vec::new()),
            },
            _ => (None, Vec::new()),
        };
        let thread_key = match &d.thread_id {
            Some(t) => repo::thread(c, t)?.map(|t| t.thread_key),
            None => None,
        };
        let message_id = message_id(d);

        let mut b = MessageBuilder::new()
            .from(Address::new_address(
                (!from_name.is_empty()).then(|| from_name.clone()),
                from.clone(),
            ))
            .subject(d.subject.clone())
            .message_id(message_id.clone())
            .date(now / 1000)
            .text_body(d.body.clone())
            .html_body(text_to_html(&d.body));
        if !d.to.is_empty() {
            b = b.to(addr_list(&d.to));
        }
        if !d.cc.is_empty() {
            b = b.cc(addr_list(&d.cc));
        }
        // Bcc only in the envelope: a Bcc header would show them to everyone
        if let Some(irt) = &in_reply_to {
            b = b.in_reply_to(irt.clone());
            b = b.references(refs.clone());
        }
        let mut total = 0i64;
        for f in &d.files {
            let (ws, sha, name, mime, size) = repo::file_row(c, &f.id)?
                .filter(|r| r.0 == d.workspace_id)
                .ok_or_else(|| MailError::Other(format!("附件「{}」不在了", f.name)))?;
            total += size;
            if total > MAX_ATTACHMENTS_BYTES {
                return Err(MailError::Other("附件合计不能超过 20 MB".into()));
            }
            let path = files_dir(self.0.cfg.home(), &ws, &sha, &f.id)
                .ok_or_else(|| MailError::Other(format!("附件「{name}」不在了")))?;
            let bytes = std::fs::read(&path)
                .map_err(|_| MailError::Other(format!("附件「{name}」不在了")))?;
            b = b.attachment(mime, name, bytes);
        }
        let mime = b
            .write_to_vec()
            .map_err(|e| MailError::Other(format!("生成邮件失败：{e}")))?;
        let rcpt: Vec<String> =
            d.to.iter()
                .chain(&d.cc)
                .chain(&d.bcc)
                .map(|r| r.addr.clone())
                .collect();
        let st = repo::settings(c, &d.connection_id)?.unwrap_or_default();
        Ok(Built {
            mime,
            message_id,
            in_reply_to,
            refs,
            meta: SendMeta {
                from,
                rcpt,
                thread: thread_key.clone(),
                append_sent: settings::append_sent(&st),
            },
            from_name,
            mailbox,
            thread_key,
        })
    }

    /// Sends one approved draft whose delay is over (§5.4). Never sends without a resolved `send`
    /// decision for exactly this content; refusals write `mail.send.blocked`.
    pub async fn execute(&self, draft_id: &str) -> MailResult<SendOutcome> {
        let Some(_activity) = self.0.m.store.try_activity("mail_send") else {
            return Ok(SendOutcome::Skipped);
        };
        self.recover_sending()?;
        let now = self.now();
        let (d, decision) = match self.claim(draft_id, now)? {
            Ok(x) => x,
            Err(o) => return Ok(o),
        };
        // A cancelled task must not leave a live-process claim stuck until a restart.
        let mut attempt = SendGuard {
            mail: self.clone(),
            draft: d.clone(),
            completed: false,
        };
        let built = match self.0.m.store.reader(|c| self.build(c, &d, now)) {
            Ok(b) => b,
            Err(e) => return self.fail(&d, &e.to_string()),
        };
        // step 3: registered vault secrets never leave
        let found = self
            .0
            .secrets
            .find(&d.workspace_id, &format!("{}\n{}", d.subject, d.body));
        if !found.is_empty() {
            self.0.m.store.transaction(|tx| -> MailResult<()> {
                repo::draft_transition(tx, &d.id, &["sending"], "failed", now)?;
                repo::set_draft_error(tx, &d.id, Some(ERR_SECRET))?;
                self.blocked(tx, &d, "secret", json!({"fields": found}))
            })?;
            self.notify_thread(&d);
            return Ok(SendOutcome::Blocked {
                reason: ERR_SECRET.into(),
            });
        }
        // Adapter lookup happens before any submission, so a local setup error is definite.
        let (adapter, cx) = match self.adapter_of(&d.workspace_id, &d.connection_id) {
            Ok(v) => v,
            Err(e) => return self.fail(&d, &e.to_string()),
        };
        let sent =
            tokio::time::timeout(SEND_TIMEOUT, adapter.send(&cx, &built.mime, &built.meta)).await;
        let result = match sent {
            Ok(Ok(r)) => {
                let msg = match self.record_sent(&d, &decision, &built, r.ext_id, now) {
                    Ok(msg) => msg,
                    Err(_) => return self.unknown(&d, "服务器已接受邮件，但本地发送记录保存失败"),
                };
                // From here on the send is confirmed and recorded, even if the optional copy
                // hangs, fails, or this task is cancelled. It is outside SEND_TIMEOUT.
                attempt.completed = true;
                if let Some(item) = &d.item_id {
                    self.0.link.hooks().sent(&d.workspace_id, item, &d.subject);
                }
                match tokio::time::timeout(
                    COPY_TIMEOUT,
                    adapter.copy_sent(&cx, &built.mime, &built.meta),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    _ => {
                        tracing::warn!(draft = %d.id, "mail sent; optional sent-folder copy did not finish")
                    }
                }
                Ok(SendOutcome::Sent { message: msg })
            }
            Ok(Err(AdapterError::Transient(_) | AdapterError::Store(_))) | Err(_) => {
                self.unknown(&d, "等待服务器确认时超时、断连或记录失败")
            }
            Ok(Err(e)) => self.fail(&d, &e.to_string()),
        };
        attempt.completed = result.is_ok();
        result
    }

    /// Recover only once, before this Mail singleton can claim any draft. Never age out an active
    /// send: elapsed time cannot tell whether a provider accepted it. One Hub owns this database.
    pub fn recover_sending(&self) -> MailResult<usize> {
        let mut recovered = lock(&self.0.send_recovered);
        if *recovered {
            return Ok(0);
        }
        let drafts = self
            .0
            .m
            .store
            .transaction(|tx| -> MailResult<Vec<DraftRow>> {
                let ids = {
                    let mut st =
                        tx.prepare("SELECT id FROM mail_drafts WHERE status = 'sending'")?;
                    st.query_map([], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                let mut drafts = Vec::new();
                for id in ids {
                    if let Some(d) = repo::draft(tx, &id)? {
                        self.unknown_tx(tx, &d, "发送期间程序中断，未收到可靠的完成记录")?;
                        drafts.push(d);
                    }
                }
                Ok(drafts)
            })?;
        *recovered = true;
        drop(recovered);
        for d in &drafts {
            self.notify_thread(d);
        }
        Ok(drafts.len())
    }

    fn unknown_tx(&self, tx: &Tx, d: &DraftRow, reason: &str) -> MailResult<bool> {
        if !repo::draft_transition(tx, &d.id, &["sending"], "unknown", self.now())? {
            return Ok(false);
        }
        let error = format!("{reason}。{ERR_UNKNOWN}");
        repo::set_draft_error(tx, &d.id, Some(&error))?;
        repo::set_draft_send_at(tx, &d.id, None)?;
        drafts::expire_open(tx, &d.workspace_id, &d.id)?;
        self.open_unknown_decision(tx, d)?;
        base::audit(
            tx,
            &d.workspace_id,
            "steve",
            "mail.send.unknown",
            &d.id,
            &json!({"message_id": message_id(d), "content_hash": d.content_hash, "reason": reason}),
        )?;
        Ok(true)
    }

    fn open_unknown_decision(&self, tx: &Tx, d: &DraftRow) -> MailResult<()> {
        let decision = base::decisions::open(
            tx,
            &NewDecision {
                workspace_id: d.workspace_id.clone(),
                kind: "fail".into(),
                subject_type: SUBJECT_TYPE.into(),
                subject_id: d.id.clone(),
                item_id: d.item_id.clone(),
                session_id: d.session_id.clone(),
                question: format!(
                    "邮件「{}」发送结果不明，请核实。重试可能重复发送；放弃待办不会撤回邮件。",
                    d.subject
                ),
                draft: Some(drafts::decision_text(d)),
                options: Some(UNKNOWN_OPTIONS.map(String::from).to_vec()),
                requested_by: "steve".into(),
            },
        )?;
        repo::set_draft_decision(tx, &d.id, Some(&decision.id), &d.content_hash)?;
        Ok(())
    }

    /// Generic decision dismissal can close a card without settling its mail. Recreate the
    /// reconciliation entry, including orphan drafts with no item/thread, until the user explicitly
    /// chooses confirmed_not_sent/discard. The read-only empty pass never takes the write lock.
    pub fn repair_unknown_decisions(&self) -> MailResult<usize> {
        const MISSING: &str = "SELECT md.id FROM mail_drafts md WHERE md.status = 'unknown' AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.subject_type = 'mail_draft' AND d.subject_id = md.id AND d.workspace_id = md.workspace_id AND d.status = 'open') LIMIT 100";
        let ids = self.0.m.store.reader(|c| -> MailResult<Vec<String>> {
            let mut st = c.prepare(MISSING)?;
            Ok(st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })?;
        if ids.is_empty() {
            return Ok(0);
        }
        let restored = self
            .0
            .m
            .store
            .transaction(|tx| -> MailResult<Vec<DraftRow>> {
                let mut restored = Vec::new();
                for id in ids {
                    let Some(d) = repo::draft(tx, &id)?.filter(|d| d.status == "unknown") else {
                        continue;
                    };
                    if !repo::open_draft_decisions(tx, &d.workspace_id, &d.id)?.is_empty() {
                        continue;
                    }
                    self.open_unknown_decision(tx, &d)?;
                    base::audit(
                        tx,
                        &d.workspace_id,
                        "steve",
                        "mail.send.reconciliation_reopened",
                        &d.id,
                        &json!({}),
                    )?;
                    restored.push(d);
                }
                Ok(restored)
            })?;
        for d in &restored {
            self.notify_thread(d);
        }
        Ok(restored.len())
    }

    fn unknown(&self, d: &DraftRow, reason: &str) -> MailResult<SendOutcome> {
        let changed = self
            .0
            .m
            .store
            .transaction(|tx| self.unknown_tx(tx, d, reason))?;
        if changed {
            self.notify_thread(d);
        }
        Ok(SendOutcome::Unknown {
            error: format!("{reason}。{ERR_UNKNOWN}"),
        })
    }

    fn notify_thread(&self, d: &DraftRow) {
        if let Some(t) = &d.thread_id {
            self.0.notify.thread(&d.workspace_id, t);
        }
    }

    /// The provider (or building the message) failed: `failed` and a `fail` decision (retry / discard).
    fn fail(&self, d: &DraftRow, error: &str) -> MailResult<SendOutcome> {
        let now = self.now();
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            repo::draft_transition(tx, &d.id, &["sending"], "failed", now)?;
            repo::set_draft_error(tx, &d.id, Some(error))?;
            drafts::expire_open(tx, &d.workspace_id, &d.id)?;
            base::decisions::open(
                tx,
                &NewDecision {
                    workspace_id: d.workspace_id.clone(),
                    kind: "fail".into(),
                    subject_type: SUBJECT_TYPE.into(),
                    subject_id: d.id.clone(),
                    item_id: d.item_id.clone(),
                    session_id: d.session_id.clone(),
                    question: format!("邮件「{}」没有发出去：{error}", d.subject),
                    draft: Some(drafts::decision_text(d)),
                    options: Some(FAIL_OPTIONS.map(String::from).to_vec()),
                    requested_by: "steve".into(),
                },
            )?;
            base::audit(
                tx,
                &d.workspace_id,
                "steve",
                "mail.send.failed",
                &d.id,
                &json!({"draft": d.id}),
            )?;
            Ok(())
        })?;
        tracing::info!(draft = %d.id, error, "mail not sent");
        self.notify_thread(d);
        Ok(SendOutcome::Failed {
            error: error.to_string(),
        })
    }

    /// Success: the outgoing message is in the library right away (the provider's copy is merged by
    /// Message-ID when it syncs), the thread waits for the other side.
    fn record_sent(
        &self,
        d: &DraftRow,
        decision: &str,
        b: &Built,
        ext_id: Option<String>,
        now: i64,
    ) -> MailResult<String> {
        let ws = d.workspace_id.as_str();
        let id = uid("ml");
        let raw_path = store::raw_rel_path(ws, &id);
        let raw_written = store::write_raw(self.0.cfg.home(), &raw_path, &b.mime).is_ok();
        let thread_id = self.0.m.store.transaction(|tx| -> MailResult<String> {
            let thread_id = match &d.thread_id {
                Some(t) => t.clone(),
                None => {
                    let t = ThreadRow {
                        id: uid("mt"),
                        workspace_id: ws.into(),
                        connection_id: d.connection_id.clone(),
                        thread_key: b.thread_key.clone().unwrap_or_else(|| b.message_id.clone()),
                        subject: d.subject.clone(),
                        last_at: now,
                        count: 0,
                        unread: 0,
                        participants: Vec::new(),
                        state: "waiting".into(),
                        visibility: "private".into(),
                        created_by: b.mailbox.owner.clone(),
                    };
                    repo::insert_thread(tx, &t, now)?;
                    t.id
                }
            };
            let who = |l: &[Rcpt]| -> MailResult<Value> {
                let mut out = Vec::new();
                for r in l {
                    let (party, _) = drafts::audience_of(tx, ws, &r.addr)?;
                    out.push(json!({"addr": r.addr, "name": r.name, "account_id": null, "party_id": party}));
                }
                Ok(Value::Array(out))
            };
            let from_party: Option<String> = {
                use rusqlite::OptionalExtension;
                tx.prepare_cached("SELECT party_id FROM accounts WHERE id = ?")?
                    .query_row([&b.mailbox.account_id], |r| r.get(0))
                    .optional()?
            };
            let m = MessageRow {
                id: id.clone(),
                workspace_id: ws.into(),
                connection_id: d.connection_id.clone(),
                thread_id: thread_id.clone(),
                ext_id: ext_id.clone().unwrap_or_else(|| format!("steve:{}", d.id)),
                message_id: Some(b.message_id.clone()),
                in_reply_to: b.in_reply_to.clone(),
                refs: b.refs.clone(),
                folders: Vec::new(),
                flags: vec!["seen".into()],
                direction: "out".into(),
                from_addr: b.meta.from.clone(),
                from_name: b.from_name.clone(),
                from_account_id: Some(b.mailbox.account_id.clone()),
                from_party_id: from_party,
                reply_to: None,
                to_addrs: who(&d.to)?,
                cc_addrs: who(&d.cc)?,
                subject: d.subject.clone(),
                snippet: parse::snippet(&d.body),
                date: now,
                size: b.mime.len() as i64,
                has_attachments: !d.files.is_empty(),
                bulk: false,
                body_text: Some(d.body.clone()),
                body_new: Some(d.body.clone()),
                has_html: true,
                raw_path: raw_written.then(|| raw_path.clone()),
                purged: None,
                event_id: None,
                status: "sent".into(),
                created: now,
            };
            repo::insert_message(tx, &m)?;
            repo::set_draft_sent(tx, &d.id, &id, now)?;
            repo::refresh_thread(tx, &thread_id, now)?;
            repo::set_thread_state(tx, &thread_id, "waiting", now)?;
            if d.thread_id.is_none() {
                tx.prepare_cached("UPDATE mail_drafts SET thread_id = ? WHERE id = ?")?
                    .execute([thread_id.as_str(), d.id.as_str()])?;
            }
            if let Some(item) = &d.item_id {
                people_repo::timeline_add(tx, item, "steve", "sys", &format!("邮件已发出：{}", d.subject))?;
            }
            base::audit(
                tx,
                ws,
                "steve",
                "mail.send",
                &d.id,
                &json!({"draft": d.id, "to_count": d.to.len() + d.cc.len() + d.bcc.len(), "decision": decision}),
            )?;
            Ok(thread_id)
        })?;
        self.0.notify.thread(ws, &thread_id);
        Ok(id)
    }

    /// Approvals that met an edited draft get a fresh decision for the current version (“决定重新打开”).
    pub fn refresh_stale(&self) -> MailResult<usize> {
        let mut n = 0;
        for id in self.0.link.take_stale() {
            let now = self.now();
            let reopened = self
                .0
                .m
                .store
                .transaction(|tx| -> MailResult<Option<DraftRow>> {
                    let Some(mut d) = repo::draft(tx, &id)? else {
                        return Ok(None);
                    };
                    if d.status != "pending" || content_hash(&d) == d.content_hash {
                        return Ok(None);
                    }
                    let author = d.author.clone();
                    drafts::open_approval(tx, &mut d, &author, now)?;
                    Ok(Some(d))
                })?;
            if let Some(d) = reopened {
                self.notify_thread(&d);
                n += 1;
            }
        }
        Ok(n)
    }

    /// One pass of the queue (`mail.send_queue`): reopen stale approvals, then send what is due.
    pub async fn send_due(&self) -> MailResult<Vec<(String, SendOutcome)>> {
        self.recover_sending()?;
        self.repair_unknown_decisions()?;
        if let Err(e) = self.refresh_stale() {
            tracing::warn!(error = %e, "mail drafts: reopening approvals");
        }
        let due = self
            .0
            .m
            .store
            .reader(|c| repo::due_drafts(c, self.now(), 20))?;
        let mut out = Vec::new();
        for (id, _) in due {
            let o = self.execute(&id).await?;
            out.push((id, o));
        }
        Ok(out)
    }

    /// The send queue: every 2 seconds.
    pub fn spawn_send_queue(&self) -> JoinHandle<()> {
        let m = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = m.send_due().await {
                    tracing::warn!(error = %e, "mail send queue");
                }
                tokio::time::sleep(QUEUE_EVERY).await;
            }
        })
    }
}

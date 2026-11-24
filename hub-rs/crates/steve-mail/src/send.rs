//! The only way mail leaves Steve: an approval decision answered `send`, a delay you can
//! undo, then [`Mail::execute`] — which checks the approval again, builds the MIME, checks final
//! disclosure and sending authority, then calls the adapter. `MailAdapter::send(` appears nowhere
//! else in this crate.
//!
//! - [`MailDraftHandler`]: the `mail_draft` decision handler (`send`/`no`, `retry`/`discard`, undo);
//! - [`DraftLink`]: what the handler shares with [`Mail`] (the clock, the hooks, approvals that hit an
//!   edited draft and must be reopened);
//! - the queue: [`Mail::send_due`] every 2 seconds ([`Mail::spawn_send_queue`]).

use steve_store::W;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use serde::Serialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::{WriteRefusal, may_write_in};
use steve_modules::contacts::people_repo;
use steve_modules::decisions::{DecisionCx, DecisionHandler, DecisionOutcome, choice_result};
use steve_modules::outbound::Verdict;
use steve_modules::outbound::{legacy_automatic_decision, strip_ai_prefix};
use steve_modules::{ApiError, ApiResult, Clock, SystemClock};
use steve_store::{Decision, NewDecision, uid};

use crate::adapter::SendMeta;
use crate::drafts::{self, FAIL_OPTIONS, SUBJECT_TYPE, content_hash};
use crate::error::{AdapterError, MailError, MailResult};
use crate::repo::{self, DraftRow, MessageRow, Rcpt, ThreadRow};
use crate::store::{self, Mailbox};
use crate::{Mail, lock, parse, settings};

mod submission;
mod validation;

/// Attachments of one mail, together.
pub const MAX_ATTACHMENTS_BYTES: i64 = 20 * 1024 * 1024;
/// How often the queue looks for approved mail whose delay is over.
pub const QUEUE_EVERY: Duration = Duration::from_secs(2);
/// Bound provider waits; expiration means unknown, never permission to resend.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(120);
/// Optional provider-side copy must not extend the submission's uncertainty window.
pub const COPY_TIMEOUT: Duration = Duration::from_secs(5);
/// How many mailboxes the queue sends from at once (each mailbox sends one draft at a time).
const SEND_MAILBOXES: usize = 4;
pub const ERR_UNKNOWN: &str =
    "发送结果不明，可能已经发出。请核实已发送邮件或询问收件人，确认未发送后才重试";
pub const UNKNOWN_OPTIONS: [&str; 2] = ["confirmed_not_sent", "discard"];

/// The capability a connection needs to send mail.
pub const SEND_CAP: &str = "mail.send";
pub const ERR_NO_APPROVAL: &str = "没有有效的批准";

/// What steve-core does for drafts (the integrator implements it; calls come after the commit or from
/// inside the decision's transaction, so implementations should only queue work).
pub trait DraftHooks: Send + Sync {
    /// `no` on a draft written in a session: tell the agent (steve-core's `send`).
    fn rejected(&self, ws: &str, session_id: &str, item_id: Option<&str>, message: &str) {
        let _ = (ws, session_id, item_id, message);
    }
    /// The provider accepted the approved draft for this item version.
    fn sent(
        &self,
        ws: &str,
        item_id: &str,
        subject: &str,
        version: Option<steve_types::DeliveryVersion>,
    ) {
        let _ = (ws, item_id, subject, version);
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

fn own(tx: &mut steve_store::W<'_>, ws: &str, id: &str) -> ApiResult<DraftRow> {
    repo::draft(tx, id)?
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
    fn approve(
        &self,
        tx: &mut steve_store::W<'_>,
        cx: &DecisionCx,
        d: &Decision,
    ) -> MailResult<Value> {
        let now = self.0.now();
        let mut draft = own(tx, cx.ws, &d.subject_id)?;
        if draft.status != "pending" || draft.decision_id.as_deref() != Some(d.id.as_str()) {
            return Err(ApiError::conflict(steve_i18n::t!("mail.draft.already_handled")).into());
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
                    return Err(ApiError::conflict(steve_i18n::t!("mail.draft.edited")).into());
                }
                let delay = repo::settings(tx, &draft.connection_id)?
                    .unwrap_or_default()
                    .send_delay_s
                    .clamp(0, 60);
                let send_at = now + delay * 1000;
                repo::draft_transition(tx, &draft.id, &["pending"], "approved", now)?;
                repo::set_draft_send_at(tx, &draft.id, Some(send_at))?;
                repo::set_draft_error(tx, &draft.id, None)?;
                if let Some(item) = &draft.item_id {
                    let (who, line) = ("me", format!("批准发送邮件「{}」", draft.subject));
                    people_repo::timeline_add(tx, item, who, "sys", &line)?;
                }
                tx.audit(
                    cx.ws,
                    cx.actor,
                    "mail_draft.approve",
                    &draft.id,
                    &json!({"decision": d.id, "edited": !cx.text.trim().is_empty()}),
                )?;
                result["content_hash"] = json!(draft.content_hash);
                result["send_at"] = json!(send_at);
            }
            "no" => {
                repo::draft_transition(tx, &draft.id, &["pending"], "discarded", now)?;
                if let Some(t) = &draft.thread_id {
                    repo::set_thread_state(tx, t, "open", now)?;
                }
                tx.audit(
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

    fn failed(
        &self,
        tx: &mut steve_store::W<'_>,
        cx: &DecisionCx,
        d: &Decision,
    ) -> MailResult<Value> {
        let now = self.0.now();
        let draft = own(tx, cx.ws, &d.subject_id)?;
        let uncertain = draft.status == "unknown";
        if draft.status != "failed" && !uncertain {
            return Err(ApiError::conflict(steve_i18n::t!("mail.draft.already_handled")).into());
        }
        if uncertain {
            // Normal REST callers are authenticated users; autonomous decisions use synthetic actors.
            // Check membership as well as known actor names so arbitrary bot ids cannot opt in.
            let member = steve_store::settings::user_is_member(tx, cx.ws, cx.actor)?;
            if !member {
                return Err(
                    ApiError::rule(steve_i18n::t!("mail.decision.unknown_needs_user")).into(),
                );
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
        tx.audit(
            cx.ws,
            cx.actor,
            &format!("mail_draft.{}", cx.choice),
            &draft.id,
            &json!({"decision": d.id}),
        )?;
        Ok(choice_result(cx))
    }
}

impl DecisionHandler for MailDraftHandler {
    fn resolve(&self, w: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let result = match d.kind.as_str() {
            "approve" => self.approve(w, cx, d),
            "fail" => self.failed(w, cx, d),
            _ => Err(ApiError::rule(steve_i18n::t!("mail.decision.unknown_kind")).into()),
        }
        .map_err(api)?;
        if let Some(draft) = repo::draft(w, &d.subject_id)? {
            drafts::emit_draft(w, &draft);
        }
        Ok(result.into())
    }

    /// Undo before the delay is over puts the draft back to `pending`; afterwards `409 邮件已经发出，不能撤回`.
    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        let now = self.0.now();
        let Some(draft) = repo::draft(tx, &d.subject_id)? else {
            return Ok(());
        };
        let choice = d.result.get("choice").and_then(Value::as_str).unwrap_or("");
        match (d.kind.as_str(), choice) {
            ("approve", "send") => undo_approval(tx, &draft, now).map_err(api),
            ("approve", "no") if draft.status == "discarded" => {
                repo::draft_transition(tx, &draft.id, &["discarded"], "pending", now)?;
                Ok(())
            }
            ("fail", _) if ["approved", "discarded"].contains(&draft.status.as_str()) => {
                let previous = if d.options.iter().any(|o| o == "confirmed_not_sent") {
                    "unknown"
                } else {
                    "failed"
                };
                repo::draft_transition(tx, &draft.id, &["approved", "discarded"], previous, now)?;
                repo::set_draft_send_at(tx, &draft.id, None)?;
                Ok(())
            }
            _ if ["sending", "sent"].contains(&draft.status.as_str()) => Err(ApiError::conflict(
                steve_i18n::t!("mail.draft.already_sent"),
            )),
            _ => Ok(()),
        }
    }
}

/// Approved and still inside the delay → back to `pending`; otherwise 409.
pub(crate) fn undo_approval(tx: &mut steve_store::W<'_>, d: &DraftRow, now: i64) -> MailResult<()> {
    match d.status.as_str() {
        "approved" if d.send_at.is_some_and(|t| now < t) => {
            if !repo::draft_transition(tx, &d.id, &["approved"], "pending", now)? {
                return Err(ApiError::conflict(steve_i18n::t!("mail.draft.already_sent")).into());
            }
            repo::set_draft_send_at(tx, &d.id, None)?;
            if let Some(t) = &d.thread_id {
                repo::set_thread_state(tx, t, "needs_reply", now)?;
            }
            Ok(())
        }
        "approved" | "sending" | "sent" => {
            Err(ApiError::conflict(steve_i18n::t!("mail.draft.already_sent")).into())
        }
        _ => Err(ApiError::conflict(steve_i18n::t!("mail.draft.not_waiting")).into()),
    }
}

/// What [`Mail::execute`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum SendOutcome {
    /// Sent; the outgoing `mail_messages` row.
    Sent { message: String },
    /// Refused before anything left (approval, disclosure or authority): audit `mail.send.blocked`.
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
    submitted: bool,
    completed: bool,
}
impl Drop for SendGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        // Before provider I/O the outcome is definite. Conditional transitions keep a committed
        // failure/success from being overwritten if a later local operation fails.
        let result = if self.submitted {
            self.mail.unknown(&self.draft, "发送任务中断")
        } else {
            self.mail
                .fail(&self.draft, &steve_i18n::t!("mail.disclosure.interrupted"))
                .or_else(|e| {
                    // Keep the existing reconciliation fallback if even a definite local failure
                    // cannot be recorded. A failed `failed` transition must not strand a live claim.
                    tracing::warn!(draft = %self.draft.id, error = %e, "mail preparation failure could not be recorded");
                    self.mail.unknown(&self.draft, "发送准备中断，失败记录未能保存")
                })
        };
        if let Err(e) = result {
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
    fn blocked(
        &self,
        tx: &mut steve_store::W<'_>,
        d: &DraftRow,
        reason: &str,
        detail: Value,
    ) -> MailResult<()> {
        let mut detail = detail;
        detail["draft"] = json!(d.id);
        detail["reason"] = json!(reason);
        tx.audit(
            &d.workspace_id,
            "steve",
            "mail.send.blocked",
            &d.id,
            &detail,
        )?;
        Ok(())
    }

    /// Step 1: `approved → sending` (conditional, so a mail goes out once) and the approval
    /// checked again — a resolved `send` decision about this draft for exactly this content.
    fn claim(&self, id: &str, now: i64) -> MailResult<Result<(DraftRow, Decision), SendOutcome>> {
        self.0.m.store.write(|tx| {
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
            let approval = steve_store::decision_repo::send_approval(tx, &d.workspace_id, id)?;
            let current = content_hash(&d);
            let ok = approval.as_ref().is_some_and(|a| {
                !legacy_automatic_decision(&a.result) && a.result.get("content_hash").and_then(Value::as_str)
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
            tx.audit( &d.workspace_id, "steve", "mail.send.attempt", &d.id,
                &json!({"message_id": message_id(&d), "content_hash": d.content_hash, "decision": a.id}))?;
            let mut d = d;
            d.status = "sending".into();
            d.updated = now;
            Ok(Ok((d, a)))
        })
    }

    /// Step 2: the MIME message (`mail-builder`), the same for every adapter.
    fn build(&self, c: &mut impl steve_store::Read, d: &DraftRow, now: i64) -> MailResult<Built> {
        let mailbox = Mailbox::load(c, &d.workspace_id, &d.connection_id)?;
        let conn = steve_store::connections::config(c, &d.connection_id)?
            .ok_or_else(|| MailError::Api(ApiError::not_found("连接")))?;
        let from = conn
            .get("address")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .or_else(|| mailbox.my_addrs.first().cloned())
            .ok_or_else(|| MailError::Other("这个邮箱没有发件地址".into()))?;
        let party = steve_store::accounts::Accounts::new(c).party(&mailbox.account_id)?;
        let from_name = match party {
            Some(p) => steve_store::parties::current_audience(c, &p)?
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
            let (ws, sha, name, mime, size) = steve_store::file_repo::file_row(c, &f.id)?
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

    /// Sends one approved draft whose delay is over. Every path, retries included, needs a resolved
    /// `send` for this content and a fresh full disclosure check of its final audience and wire body.
    /// A manual approval cannot bypass the check. Refusals write `mail.send.blocked`.
    pub async fn execute(&self, draft_id: &str) -> MailResult<SendOutcome> {
        let Some(_activity) = self.0.m.store.try_activity("mail_send") else {
            return Ok(SendOutcome::Skipped);
        };
        self.recover_sending()?;
        let now = self.now();
        let (stored, approval) = match self.claim(draft_id, now)? {
            Ok(x) => x,
            Err(o) => return Ok(o),
        };
        // A cancelled task must not leave a live-process claim stuck until a restart.
        let mut attempt = SendGuard {
            mail: self.clone(),
            draft: stored.clone(),
            submitted: false,
            completed: false,
        };
        // Check exactly what the MIME builder sends, after the outbound identity mark is applied.
        let mut d = stored.clone();
        d.body = strip_ai_prefix(&d.body).to_string();
        let built = match self.0.m.store.read(|c| self.build(c, &d, now)) {
            Ok(b) => b,
            Err(e) => return self.fail(&d, &e.to_string()),
        };
        // Adapter lookup happens before any submission, so a local setup error is definite.
        let (adapter, cx) = match self.adapter_of(&d.workspace_id, &d.connection_id) {
            Ok(v) => v,
            Err(e) => return self.fail(&d, &e.to_string()),
        };
        let (check, inputs) = match self.full_check(&d).await {
            Ok(c) => c,
            Err(e) => return self.fail(&d, &e.to_string()),
        };
        let refusal = self.revalidate_send(&d, &stored, &approval, &check, &inputs)?;
        if let Some(outcome) = refusal {
            if let SendOutcome::Blocked { reason } = &outcome {
                self.fail(&d, reason)?;
            }
            attempt.completed = true;
            return Ok(outcome);
        }
        self.submit(
            &d,
            &mut attempt,
            submission::Submission {
                adapter: adapter.as_ref(),
                cx: &cx,
                built: &built,
                approval: &approval,
                now,
            },
        )
        .await
    }

    /// Recover only once, before this Mail singleton can claim any draft. Never age out an active
    /// send: elapsed time cannot tell whether a provider accepted it. One Hub owns this database.
    pub fn recover_sending(&self) -> MailResult<usize> {
        let mut recovered = lock(&self.0.send_recovered);
        if *recovered {
            return Ok(0);
        }
        let drafts = self.0.m.store.write(|tx| -> MailResult<Vec<DraftRow>> {
            let drafts = repo::sending_drafts(tx)?;
            for d in &drafts {
                self.unknown_tx(tx, d, "发送期间程序中断，未收到可靠的完成记录")?;
            }
            Ok(drafts)
        })?;
        *recovered = true;
        drop(recovered);
        Ok(drafts.len())
    }

    fn unknown_tx(
        &self,
        tx: &mut steve_store::W<'_>,
        d: &DraftRow,
        reason: &str,
    ) -> MailResult<bool> {
        if !repo::draft_transition(tx, &d.id, &["sending"], "unknown", self.now())? {
            return Ok(false);
        }
        let error = format!("{reason}。{ERR_UNKNOWN}");
        repo::set_draft_error(tx, &d.id, Some(&error))?;
        repo::set_draft_send_at(tx, &d.id, None)?;
        drafts::expire_open(tx, &d.workspace_id, &d.id)?;
        self.open_unknown_decision(tx, d)?;
        tx.audit(
            &d.workspace_id,
            "steve",
            "mail.send.unknown",
            &d.id,
            &json!({"message_id": message_id(d), "content_hash": d.content_hash, "reason": reason}),
        )?;
        drafts::emit_draft(tx, d);
        Ok(true)
    }

    fn open_unknown_decision(&self, tx: &mut steve_store::W<'_>, d: &DraftRow) -> MailResult<()> {
        let settled = steve_store::decision_repo::settled_count(
            tx,
            &d.workspace_id,
            SUBJECT_TYPE,
            &d.id,
            "fail",
        )?;
        let decision = steve_store::DecisionRows(tx).open(&NewDecision {
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
            question_template: None,
            source: Some(steve_store::DecisionSource::MailDraft {
                previous: d.decision_id.clone(),
                content_hash: format!("unknown:{}:{settled}", d.content_hash),
            }),
        })?;
        repo::set_draft_decision(tx, &d.id, Some(&decision.id), &d.content_hash)?;
        Ok(())
    }

    /// Generic decision dismissal can close a card without settling its mail. Recreate the
    /// reconciliation entry, including orphan drafts with no item/thread, until the user explicitly
    /// chooses confirmed_not_sent/discard. The read-only empty pass never takes the write lock.
    pub fn repair_unknown_decisions(&self) -> MailResult<usize> {
        let ids = self
            .0
            .m
            .store
            .read(|r| repo::unknown_drafts_without_open_decision(r))?;
        if ids.is_empty() {
            return Ok(0);
        }
        let restored = self.0.m.store.write(|tx| -> MailResult<Vec<DraftRow>> {
            let mut restored = Vec::new();
            for id in ids {
                let Some(d) = repo::draft(tx, &id)?.filter(|d| d.status == "unknown") else {
                    continue;
                };
                if !steve_store::decision_repo::open_draft_decisions(tx, &d.workspace_id, &d.id)?
                    .is_empty()
                {
                    continue;
                }
                self.open_unknown_decision(tx, &d)?;
                tx.audit(
                    &d.workspace_id,
                    "steve",
                    "mail.send.reconciliation_reopened",
                    &d.id,
                    &json!({}),
                )?;
                drafts::emit_draft(tx, &d);
                restored.push(d);
            }
            Ok(restored)
        })?;
        Ok(restored.len())
    }

    fn unknown(&self, d: &DraftRow, reason: &str) -> MailResult<SendOutcome> {
        self.0.m.store.write(|tx| self.unknown_tx(tx, d, reason))?;
        Ok(SendOutcome::Unknown {
            error: format!("{reason}。{ERR_UNKNOWN}"),
        })
    }

    /// The provider (or building the message) failed: `failed` and a `fail` decision (retry / discard).
    fn fail(&self, d: &DraftRow, error: &str) -> MailResult<SendOutcome> {
        let now = self.now();
        self.0.m.store.write(|tx| -> MailResult<()> {
            if !repo::draft_transition(tx, &d.id, &["sending"], "failed", now)? {
                return Ok(());
            }
            repo::set_draft_error(tx, &d.id, Some(error))?;
            drafts::expire_open(tx, &d.workspace_id, &d.id)?;
            let settled = steve_store::decision_repo::settled_count(
                tx,
                &d.workspace_id,
                SUBJECT_TYPE,
                &d.id,
                "fail",
            )?;
            steve_store::DecisionRows(tx).open(&NewDecision {
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
                question_template: None,
                source: Some(steve_store::DecisionSource::MailDraft {
                    previous: d.decision_id.clone(),
                    content_hash: format!("fail:{}:{settled}", d.content_hash),
                }),
            })?;
            tx.audit(
                &d.workspace_id,
                "steve",
                "mail.send.failed",
                &d.id,
                &json!({"draft": d.id}),
            )?;
            drafts::emit_draft(tx, d);
            Ok(())
        })?;
        tracing::info!(draft = %d.id, error, "mail not sent");
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
        self.0.m.store.write(|tx| -> MailResult<()> {
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
            let from_party = steve_store::accounts::Accounts::new(tx).party(&b.mailbox.account_id)?;
            let mut who = |l: &[Rcpt]| -> MailResult<Value> {
                let mut out = Vec::new();
                for r in l {
                    let (party, _) = drafts::audience_of(tx, ws, &r.addr)?;
                    out.push(json!({"addr": r.addr, "name": r.name, "account_id": null, "party_id": party}));
                }
                Ok(Value::Array(out))
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
                repo::set_draft_thread(tx, &d.id, &thread_id)?;
            }
            if let Some(item) = &d.item_id {
                people_repo::timeline_add(tx, item, "steve", "sys", &format!("邮件已发出：{}", d.subject))?;
            }
            tx.audit(
                ws,
                "steve",
                "mail.send",
                &d.id,
                &json!({"draft": d.id, "to_count": d.to.len() + d.cc.len() + d.bcc.len(), "decision": decision}),
            )?;
            crate::events::emit_thread(tx, ws, &thread_id);
            Ok(())
        })?;
        Ok(id)
    }

    /// Approvals that met an edited draft get a fresh decision for the current version (“决定重新打开”).
    pub fn refresh_stale(&self) -> MailResult<usize> {
        let mut n = 0;
        for id in self.0.link.take_stale() {
            let now = self.now();
            let reopened = self.0.m.store.write(|tx| -> MailResult<Option<DraftRow>> {
                let Some(mut d) = repo::draft(tx, &id)? else {
                    return Ok(None);
                };
                if d.status != "pending" || content_hash(&d) == d.content_hash {
                    return Ok(None);
                }
                let author = d.author.clone();
                drafts::open_approval(tx, &mut d, &author, now)?;
                drafts::emit_draft(tx, &d);
                Ok(Some(d))
            })?;
            if reopened.is_some() {
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
            .read(|c| repo::due_drafts(c, self.now(), 20))?;
        // One stuck SMTP host must not hold up the other mailboxes: each mailbox sends its drafts in
        // order, a few mailboxes at a time.
        let mut groups: Vec<(String, Vec<(usize, String)>)> = Vec::new();
        for (i, (id, conn)) in due.into_iter().enumerate() {
            match groups.iter_mut().find(|(c, _)| *c == conn) {
                Some((_, g)) => g.push((i, id)),
                None => groups.push((conn, vec![(i, id)])),
            }
        }
        let mut out: Vec<(usize, String, SendOutcome)> = stream::iter(groups)
            .map(|(_, drafts)| async move {
                let mut done = Vec::with_capacity(drafts.len());
                for (i, id) in drafts {
                    match self.execute(&id).await {
                        Ok(o) => done.push((i, id, o)),
                        Err(e) => tracing::warn!(draft = %id, error = %e, "mail send queue: draft"),
                    }
                }
                done
            })
            .buffer_unordered(SEND_MAILBOXES)
            .flat_map(stream::iter)
            .collect()
            .await;
        out.sort_by_key(|(i, _, _)| *i);
        Ok(out.into_iter().map(|(_, id, o)| (id, o)).collect())
    }

    /// The send queue: every 2 seconds.
    pub async fn run_send_queue(self) {
        let m = self.clone();

        loop {
            if let Err(e) = m.send_due().await {
                tracing::warn!(error = %e, "mail send queue");
            }
            tokio::time::sleep(QUEUE_EVERY).await;
        }
    }
}

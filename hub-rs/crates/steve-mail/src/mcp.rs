//! MCP tools of the mail module for agents in a session:
//! `steve_mail_thread`, `steve_mail_attachment` and `steve_mail_draft`. They only work for a session
//! whose item is about an email thread (a `from`/`about` link) the item's user may see; everything
//! else answers `This item is not about an email thread.`.
//!
//! None of them sends mail: `steve_mail_draft` writes a draft and opens the `mail_draft` approval,
//! and only the user's `send` on that decision leads to [`Mail::execute`]. Recipients are the
//! server's; the agent cannot add any.
//!
//! Serving them is steve-mcp's (integrator), like the contacts and knowledge tools: list [`tools`]
//! after the Steve tools and route names for which [`handles`] is true to [`call`] with the session's
//! [`McpCx`]. `steve_mail_draft` ends the turn (its answer ends with steve-mcp's `END`) and calls
//! [`crate::DraftHooks::awaiting_approval`].

use std::path::PathBuf;
use std::sync::LazyLock;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::outbound::{self, Audience, CheckCx, CheckResult};
use steve_modules::{ApiError, Caller};
use steve_store::base::{self, Viewer};
use steve_store::{FileRef, uid};

use crate::Mail;
use crate::attach::size_label;
use crate::drafts::{self, audience_of, reply_recipients, reply_subject, with_signature};
use crate::error::{MailError, MailResult};
use crate::items::visible_item_threads;
use crate::parse::cut_chars;
use crate::repo::{self, DraftRow, MessageRow, Rcpt, ThreadRow};

pub const THREAD: &str = "steve_mail_thread";
pub const ATTACHMENT: &str = "steve_mail_attachment";
pub const DRAFT: &str = "steve_mail_draft";

/// The answer outside an email item (verbatim).
pub const NOT_ABOUT: &str = "This item is not about an email thread.";
/// steve-mcp's `END` (tools.rs), copied verbatim so the draft tool ends the turn like `steve_ask`.
pub const END: &str = "End your turn now. The user's answer will arrive as your next message.";
/// The answer of `steve_mail_draft` (verbatim) before `END`.
pub const SAVED: &str = "Draft saved; the user will review it. ";
/// Characters of message text in one `steve_mail_thread` answer.
pub const THREAD_MAX: usize = 20_000;
/// Files one draft may carry.
pub const MAX_ATTACHMENTS: usize = drafts::MAX_FILES;

/// Read-only tools: they only read the item's own thread or copy one of its attachments into the
/// session's working directory (safe for the runner's auto-allow). `steve_mail_draft` writes a draft
/// and opens a decision, which is also only a report to the user (it never sends).
pub const READ_ONLY: [&str; 2] = [THREAD, ATTACHMENT];

static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![
        json!({
            "name": THREAD,
            "description": "Read the email thread this item is about: every message with sender, recipients, date, text and attachments.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "thread": { "type": "string", "description": "thread id (mt_…); default: the item's thread" }
                }
            }
        }),
        json!({
            "name": ATTACHMENT,
            "description": "Save an attachment from the email thread into your working directory so you can read it.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "attachment id (mla_…)" }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": DRAFT,
            "description": "Write or update the reply for this item's email thread. The user reviews it and decides whether to send; you cannot send email yourself. Then end your turn.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "body": { "type": "string", "description": "plain text, without the signature" },
                    "subject": { "type": "string" },
                    "reply_all": { "type": "boolean" },
                    "attachments": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "paths relative to your working directory"
                    }
                },
                "required": ["body"]
            }
        }),
    ]
});

/// The tool definitions, `[{name, description, inputSchema}]`, in listing order.
pub fn tools() -> &'static [Value] {
    &TOOLS
}

/// Is `name` one of this module's tools?
pub fn handles(name: &str) -> bool {
    [THREAD, ATTACHMENT, DRAFT].contains(&name)
}

/// Runs one tool. `Ok` is the result text, `Err` the `isError` text (steve-mcp's convention).
pub async fn call(mail: &Mail, cx: &McpCx, name: &str, args: &Value) -> Result<String, String> {
    let r = match name {
        THREAD => match validate(name, args, &[("thread", false, "string")]) {
            Ok(()) => {
                let (m, cx, t) = (mail.clone(), cx.clone(), str_arg(args, "thread"));
                blocking(move || m.tool_thread(&cx, t.as_deref())).await
            }
            Err(e) => Err(e),
        },
        ATTACHMENT => match validate(name, args, &[("id", true, "string")]) {
            Ok(()) => {
                let (m, cx, id) = (mail.clone(), cx.clone(), str_arg(args, "id"));
                blocking(move || m.tool_attachment(&cx, id.as_deref().unwrap_or(""))).await
            }
            Err(e) => Err(e),
        },
        DRAFT => match validate(
            name,
            args,
            &[
                ("body", true, "string"),
                ("subject", false, "string"),
                ("reply_all", false, "boolean"),
                ("attachments", false, "string[]"),
            ],
        ) {
            Ok(()) => {
                let a = AgentDraft {
                    body: str_arg(args, "body").unwrap_or_default(),
                    subject: str_arg(args, "subject"),
                    reply_all: args.get("reply_all").and_then(Value::as_bool),
                    attachments: args
                        .get("attachments")
                        .and_then(Value::as_array)
                        .map(|l| {
                            l.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                mail.tool_draft(cx, a).await
            }
            Err(e) => Err(e),
        },
        _ => return Err(format!("MCP error -32602: Tool {name} not found")),
    };
    if let Err(e) = &r {
        tracing::warn!(tool = name, error = %e, "mail tool failed");
    }
    r
}

async fn blocking(
    f: impl FnOnce() -> Result<String, String> + Send + 'static,
) -> Result<String, String> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(format!("task failed: {e}")))
}

fn str_arg(args: &Value, k: &str) -> Option<String> {
    args.get(k).and_then(Value::as_str).map(String::from)
}

fn received(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// The argument check in the wording steve-mcp (zod v4) uses, so agents can fix their call.
fn validate(tool: &str, args: &Value, fields: &[(&str, bool, &str)]) -> Result<(), String> {
    let mut issues = Vec::new();
    for (k, required, ty) in fields {
        let v = args.get(*k);
        match (v, *ty) {
            (None, _) if !required => {}
            (Some(Value::String(_)), "string") | (Some(Value::Bool(_)), "boolean") => {}
            (Some(Value::Array(l)), "string[]") => {
                for (i, x) in l.iter().enumerate() {
                    if !x.is_string() {
                        issues.push(format!(
                            "Invalid input: expected string, received {} at {k}.{i}",
                            received(Some(x))
                        ));
                    }
                }
            }
            _ => {
                let want = if *ty == "string[]" { "array" } else { ty };
                issues.push(format!(
                    "Invalid input: expected {want}, received {} at {k}",
                    received(v)
                ));
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {tool}: {}",
            issues.join("\n")
        ))
    }
}

fn internal(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// `(viewer, the item's visible threads, most recent first)`; [`NOT_ABOUT`] without any.
fn scope(mail: &Mail, cx: &McpCx) -> Result<(Viewer, Vec<ThreadRow>), String> {
    let Some(item) = cx.item_id.as_deref() else {
        return Err(NOT_ABOUT.into());
    };
    let m = mail.modules();
    let viewer = m
        .store
        .reader(|c| -> MailResult<Viewer> {
            let v = steve_knowledge::context::item_viewer(c, &cx.ws, item)?;
            if v.user.is_empty() && !cx.user.is_empty() {
                return Ok(Viewer::load(c, &cx.ws, &cx.user)?);
            }
            Ok(v)
        })
        .map_err(internal)?;
    let threads = m
        .store
        .reader(|c| visible_item_threads(c, &viewer, item))
        .map_err(internal)?;
    if threads.is_empty() {
        return Err(NOT_ABOUT.into());
    }
    Ok((viewer, threads))
}

/// The thread asked for, which must be one of the item's; default the most recent.
fn pick<'a>(threads: &'a [ThreadRow], id: Option<&str>) -> Result<&'a ThreadRow, String> {
    match id.map(str::trim).filter(|s| !s.is_empty()) {
        None => threads.first().ok_or_else(|| NOT_ABOUT.to_string()),
        Some(id) => threads
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| NOT_ABOUT.to_string()),
    }
}

fn session_cwd(c: &Connection, ws: &str, session: &str) -> MailResult<Option<String>> {
    Ok(
        c.prepare_cached("SELECT cwd FROM sessions WHERE id = ? AND workspace_id = ?")?
            .query_row([session, ws], |r| r.get(0))
            .optional()?,
    )
}

fn cwd_of(mail: &Mail, cx: &McpCx) -> Result<PathBuf, String> {
    let s = cx
        .session_id
        .as_deref()
        .ok_or("This tool needs a session with a working directory.")?;
    mail.modules()
        .store
        .reader(|c| session_cwd(c, &cx.ws, s))
        .map_err(internal)?
        .map(PathBuf::from)
        .ok_or_else(|| "This tool needs a session with a working directory.".into())
}

fn person(name: &str, addr: &str) -> String {
    if name.trim().is_empty() {
        addr.to_string()
    } else {
        format!("{} <{addr}>", name.trim())
    }
}

fn addr_line(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let a = r.get("addr")?.as_str()?;
            Some(person(
                r.get("name").and_then(Value::as_str).unwrap_or(""),
                a,
            ))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn date_line(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc2822())
        .unwrap_or_default()
}

/// The unsent draft of this item for this thread (one per item and thread).
fn item_draft(c: &Connection, item: &str, thread: &str) -> MailResult<Option<DraftRow>> {
    let id: Option<String> = c
        .prepare_cached(
            "SELECT id FROM mail_drafts WHERE item_id = ? AND thread_id = ?
               AND status IN ('draft', 'pending', 'approved', 'sending', 'failed', 'unknown')
             ORDER BY created DESC, rowid DESC LIMIT 1",
        )?
        .query_row([item, thread], |r| r.get(0))
        .optional()?;
    match id {
        Some(id) => repo::draft(c, &id),
        None => Ok(None),
    }
}

fn set_author(c: &Connection, id: &str, author: &str, session: Option<&str>) -> MailResult<()> {
    c.prepare_cached("UPDATE mail_drafts SET author = ?, session_id = ? WHERE id = ?")?
        .execute(params![author, session, id])?;
    Ok(())
}

/// What the agent passed to `steve_mail_draft`.
#[derive(Debug, Clone, Default)]
pub struct AgentDraft {
    pub body: String,
    pub subject: Option<String>,
    pub reply_all: Option<bool>,
    pub attachments: Vec<String>,
}

/// Everything `steve_mail_draft` gathers before the disclosure check.
struct Prepared {
    who: Caller,
    thread: ThreadRow,
    orig: MessageRow,
    existing: Option<DraftRow>,
    reply_all: bool,
    to: Vec<Rcpt>,
    cc: Vec<Rcpt>,
    subject: String,
    body: String,
    files: Vec<FileRef>,
    audience: Audience,
}

impl Mail {
    /// `steve_mail_thread`: oldest first, headers, the new text of each message (together at most
    /// 20000 characters, newest kept first, through the guard), attachments.
    pub fn tool_thread(&self, cx: &McpCx, thread: Option<&str>) -> Result<String, String> {
        let (_, threads) = scope(self, cx)?;
        let t = pick(&threads, thread)?;
        let guard = self.0.guard.clone();
        let item = cx.item_id.clone().unwrap_or_default();
        self.0
            .m
            .store
            .reader(|c| -> MailResult<String> {
                let msgs = repo::thread_messages(c, &t.id)?;
                // the budget goes to the newest messages first
                let mut texts: Vec<String> = vec![String::new(); msgs.len()];
                let mut left = THREAD_MAX;
                for (i, m) in msgs.iter().enumerate().rev() {
                    texts[i] = match &m.body_new {
                        None => format!(
                            "(text deleted after the retention period; snippet: {})",
                            m.snippet
                        ),
                        Some(_) if left == 0 => "(earlier text left out; it is too long)".into(),
                        Some(b) => {
                            let b = steve_knowledge::state::redact(guard.as_ref(), b.trim());
                            let b = cut_chars(&b, left);
                            left = left.saturating_sub(b.chars().count());
                            b
                        }
                    };
                }
                let mut out = vec![format!(
                    "Email thread {} · {} · {} message{}",
                    t.id,
                    t.subject,
                    msgs.len(),
                    if msgs.len() == 1 { "" } else { "s" }
                )];
                let others: Vec<String> = threads
                    .iter()
                    .filter(|o| o.id != t.id)
                    .map(|o| format!("{} ({})", o.id, o.subject))
                    .collect();
                if !others.is_empty() {
                    out.push(format!(
                        "This item is also about: {} (pass thread to read one).",
                        others.join(", ")
                    ));
                }
                for (i, m) in msgs.iter().enumerate() {
                    let mut b = vec![format!("--- {}/{} · {} ---", i + 1, msgs.len(), m.id)];
                    b.push(format!("From: {}", person(&m.from_name, &m.from_addr)));
                    b.push(format!("To: {}", addr_line(&m.to_addrs)));
                    let cc = addr_line(&m.cc_addrs);
                    if !cc.is_empty() {
                        b.push(format!("Cc: {cc}"));
                    }
                    b.push(format!("Date: {}", date_line(m.date)));
                    b.push(format!("Subject: {}", m.subject));
                    b.push(String::new());
                    b.push(texts[i].clone());
                    let atts = repo::attachments(c, &m.id)?;
                    if !atts.is_empty() {
                        b.push(String::new());
                        b.push("Attachments:".into());
                        for (id, name, mime, size, inline) in atts {
                            b.push(format!(
                                "- {id} · {name} · {mime} · {}{}",
                                size_label(size),
                                if inline { " · inline image" } else { "" }
                            ));
                        }
                    }
                    out.push(b.join("\n"));
                }
                if let Some(d) = item_draft(c, &item, &t.id)? {
                    out.push(format!(
                        "Your current draft for this thread: {} ({}).",
                        d.id,
                        match d.status.as_str() {
                            "pending" => "waiting for the user's decision",
                            "approved" | "sending" => "approved by the user, about to be sent",
                            "failed" => "sending failed",
                            "unknown" => "sending outcome unknown; only a human may reconcile before retrying",
                            _ => "not submitted",
                        }
                    ));
                }
                Ok(out.join("\n\n"))
            })
            .map_err(internal)
    }

    /// `steve_mail_attachment`: lands the attachment and copies it to `<cwd>/.steve/attachments/`.
    pub fn tool_attachment(&self, cx: &McpCx, id: &str) -> Result<String, String> {
        let (viewer, threads) = scope(self, cx)?;
        let in_item = self
            .0
            .m
            .store
            .reader(|c| self.own_attachment(c, &viewer, id.trim()))
            .ok()
            .is_some_and(|(_, m)| threads.iter().any(|t| t.id == m.thread_id));
        if !in_item {
            return Err(format!(
                "There is no attachment {id} in this item's email thread."
            ));
        }
        let cwd = cwd_of(self, cx)?;
        let f = self
            .land_attachment(&viewer, id.trim())
            .map_err(internal)?
            .map_err(|r| format!("{} (the user can open it in the mailbox)", r.message))?;
        let rel = self.copy_to_workdir(&cx.ws, &cwd, &f)?;
        Ok(format!(
            "Saved to {rel} ({}, {}) in your working directory.",
            f.mime,
            size_label(f.size)
        ))
    }

    fn draft_prepare(&self, cx: &McpCx, a: &AgentDraft) -> Result<Prepared, String> {
        let (viewer, threads) = scope(self, cx)?;
        let thread = pick(&threads, None)?.clone();
        let item = cx.item_id.clone().unwrap_or_default();
        let body = a.body.trim();
        if body.is_empty() {
            return Err("Write the reply in body.".into());
        }
        if a.attachments.len() > MAX_ATTACHMENTS {
            return Err(format!("At most {MAX_ATTACHMENTS} attachments."));
        }
        let who = Caller {
            ws: cx.ws.clone(),
            me: viewer.user.clone(),
        };
        let mailbox = self
            .own_mailbox(&who, &thread.connection_id)
            .map_err(|_| "The mailbox of this thread is not available.".to_string())?;
        let (orig, existing, signature) = self
            .0
            .m
            .store
            .reader(|c| -> MailResult<_> {
                let orig = drafts::reply_target(c, &thread.id)?;
                let existing = item_draft(c, &item, &thread.id)?;
                let sig = repo::settings(c, &thread.connection_id)?
                    .unwrap_or_default()
                    .signature;
                Ok((orig, existing, sig))
            })
            .map_err(internal)?;
        let orig = orig.ok_or("This thread has no messages yet.")?;
        if existing.as_ref().is_some_and(|d| d.status == "unknown") {
            return Err("The previous send outcome is unknown. Ask the user to reconcile it; do not replace or resend this draft.".into());
        }
        if let Some(d) = &existing
            && ["approved", "sending"].contains(&d.status.as_str())
        {
            return Err(
                "The user already approved the current draft and it is being sent; it can no longer change."
                    .into(),
            );
        }
        let reply_all = a
            .reply_all
            .unwrap_or_else(|| existing.as_ref().is_some_and(|d| d.mode == "reply_all"));
        let (to, cc) = reply_recipients(&orig, reply_all, &mailbox.my_addrs);
        if to.is_empty() && cc.is_empty() {
            return Err("There is nobody to reply to in this thread.".into());
        }
        let subject = match a
            .subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(s) => {
                if s.contains(['\r', '\n']) {
                    return Err("The subject must be one line.".into());
                }
                s.to_string()
            }
            None => reply_subject(if orig.subject.trim().is_empty() {
                &thread.subject
            } else {
                &orig.subject
            }),
        };
        let subject = cut_chars(&subject, drafts::MAX_SUBJECT_CHARS);
        let body = with_signature(body, &signature);
        if body.chars().count() > drafts::MAX_BODY_CHARS {
            return Err("The body is too long.".into());
        }
        let files = if a.attachments.is_empty() {
            Vec::new()
        } else {
            let cwd = cwd_of(self, cx)?;
            self.files_from_workdir(&cx.ws, &cx.bot_id, &cwd, &a.attachments)?
        };
        // §5.3: who reads it
        let audience = self
            .0
            .m
            .store
            .reader(|c| -> MailResult<Audience> {
                let mut au = Audience::default();
                let mut names = Vec::new();
                for r in to.iter().chain(cc.iter()) {
                    let (party, internal) = audience_of(c, &cx.ws, &r.addr)?;
                    au.external |= !internal;
                    if let Some(p) = party {
                        au.parties.push(p);
                    }
                    names.push(if r.name.is_empty() {
                        r.addr.clone()
                    } else {
                        r.name.clone()
                    });
                }
                au.description = format!(
                    "邮件收件人：{}，{}",
                    names.join("、"),
                    if au.external { "外部" } else { "内部" }
                );
                Ok(au)
            })
            .map_err(internal)?;
        Ok(Prepared {
            who,
            thread,
            orig,
            existing,
            reply_all,
            to,
            cc,
            subject,
            body,
            files,
            audience,
        })
    }

    /// `steve_mail_draft`: one unsent draft per item and thread — written, or overwritten with its
    /// approval reopened for the new version. Never sends.
    pub async fn tool_draft(&self, cx: &McpCx, a: AgentDraft) -> Result<String, String> {
        let (m, c2, a2) = (self.clone(), cx.clone(), a.clone());
        let p = tokio::task::spawn_blocking(move || m.draft_prepare(&c2, &a2))
            .await
            .map_err(|e| format!("task failed: {e}"))??;
        let check = outbound::check(
            &self.0.m,
            &CheckCx {
                ws: cx.ws.clone(),
                actor: cx.bot_id.clone(),
                draft_model: None,
                audience_said: None,
            },
            &p.audience,
            &format!("{}\n\n{}", p.subject, p.body),
            &[],
        )
        .await;
        let (m, c2) = (self.clone(), cx.clone());
        let d = tokio::task::spawn_blocking(move || m.draft_write(&c2, p, check))
            .await
            .map_err(|e| format!("task failed: {e}"))?
            .map_err(|e| match e {
                MailError::Api(a) => a.to_string(),
                e => e.to_string(),
            })?;
        if let Some(s) = &cx.session_id {
            self.0
                .link
                .hooks()
                .awaiting_approval(&cx.ws, s, cx.item_id.as_deref());
        }
        tracing::info!(draft = %d, "steve_mail_draft");
        Ok(format!("{SAVED}{END}"))
    }

    fn draft_write(&self, cx: &McpCx, p: Prepared, check: CheckResult) -> MailResult<String> {
        let now = self.now();
        let basis = serde_json::to_value(&check).unwrap_or(Value::Null);
        let mode = if p.reply_all { "reply_all" } else { "reply" };
        let author = if cx.bot_id.is_empty() {
            "steve".to_string()
        } else {
            cx.bot_id.clone()
        };
        let thread_id = p.thread.id.clone();
        let Some(existing) = p.existing else {
            let d = DraftRow {
                id: uid("md"),
                workspace_id: p.who.ws.clone(),
                connection_id: p.thread.connection_id.clone(),
                thread_id: Some(thread_id),
                reply_to_message: Some(p.orig.id.clone()),
                mode: mode.into(),
                to: p.to,
                cc: p.cc,
                bcc: Vec::new(),
                subject: p.subject,
                body: p.body,
                files: p.files,
                author,
                item_id: cx.item_id.clone(),
                session_id: cx.session_id.clone(),
                run_id: None,
                decision_id: None,
                basis: Some(basis.clone()),
                content_hash: String::new(),
                status: "draft".into(),
                send_at: None,
                sent_message: None,
                error: None,
                created: now,
                updated: now,
                sent: None,
            };
            return Ok(self.insert_pending(d, &check)?.id);
        };
        let d = self.0.m.store.transaction(|tx| -> MailResult<DraftRow> {
            let mut d = repo::draft(tx, &existing.id)?
                .ok_or_else(|| MailError::Api(ApiError::not_found("草稿")))?;
            if !["draft", "pending", "failed"].contains(&d.status.as_str()) {
                return Err(ApiError::conflict(
                    "The user already approved the current draft and it is being sent; it can no longer change.",
                )
                .into());
            }
            d.mode = mode.into();
            d.reply_to_message = Some(p.orig.id.clone());
            d.to = p.to;
            d.cc = p.cc;
            d.bcc = Vec::new();
            d.subject = p.subject;
            d.body = p.body;
            d.files = p.files;
            d.basis = Some(basis);
            d.author = author.clone();
            d.session_id = cx.session_id.clone();
            d.content_hash = drafts::content_hash(&d);
            repo::update_draft_content(tx, &d, now)?;
            set_author(tx, &d.id, &author, d.session_id.as_deref())?;
            repo::set_draft_error(tx, &d.id, None)?;
            // the old card expires, a new one opens for this version
            drafts::open_approval(tx, &mut d, &author, now)?;
            base::audit(
                tx,
                &d.workspace_id,
                &author,
                "mail_draft.update",
                &d.id,
                &json!({"mode": d.mode, "by": "steve_mail_draft"}),
            )?;
            Ok(d)
        })?;
        self.0.notify.thread(&d.workspace_id, &thread_id);
        Ok(d.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_checks() {
        assert!(validate(DRAFT, &json!({"body": "x"}), &[("body", true, "string")]).is_ok());
        let e = validate(
            DRAFT,
            &json!({"reply_all": "yes", "attachments": ["a", 3]}),
            &[
                ("body", true, "string"),
                ("reply_all", false, "boolean"),
                ("attachments", false, "string[]"),
            ],
        )
        .unwrap_err();
        assert!(
            e.contains("expected string, received undefined at body"),
            "{e}"
        );
        assert!(
            e.contains("expected boolean, received string at reply_all"),
            "{e}"
        );
        assert!(
            e.contains("expected string, received number at attachments.1"),
            "{e}"
        );
        assert!(handles(DRAFT) && handles(THREAD) && handles(ATTACHMENT));
        assert!(!handles("steve_mail_send"));
        assert_eq!(tools().len(), 3);
        // no tool can send
        assert!(
            tools()
                .iter()
                .all(|t| !t["name"].as_str().unwrap().contains("send"))
        );
    }
}

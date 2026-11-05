//! Drafts: what you write yourself, what a bot drafts, the recipient rules, the
//! content hash an approval is tied to, and the `mail_draft` approval decision. Sending is in
//! [`crate::send`]; nothing here sends.
//!
//! Recipients of a reply are computed here from the original message ([`reply_recipients`]); the model
//! and the agents never pick them. You can change them on the card (that is your edit).

use std::collections::BTreeSet;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_modules::accounts::service::current_party;
use steve_modules::outbound::CheckResult;
use steve_modules::{ApiError, Caller, decisions};
use steve_store::base::{self, Viewer, Visibility};
use steve_store::{FileRef, NewDecision, Tx, uid};

use crate::Mail;
use crate::error::{MailError, MailResult};
use crate::repo::{self, DraftRow, MessageRow, Rcpt, ThreadRow};
use crate::store::Mailbox;

/// `decisions.subject_type` of drafts.
pub const SUBJECT_TYPE: &str = "mail_draft";
/// Options of the approval decision.
pub const APPROVE_OPTIONS: [&str; 2] = ["send", "no"];
/// Options of the decision opened when sending failed.
pub const FAIL_OPTIONS: [&str; 2] = ["retry", "discard"];
/// At most this many recipients (To + Cc + Bcc).
pub const MAX_RCPTS: usize = 50;
/// At most this many attachments.
pub const MAX_FILES: usize = 10;
pub const MAX_SUBJECT_CHARS: usize = 998;
pub const MAX_BODY_CHARS: usize = 100_000;

pub const MSG_EDITED: &str = "草稿在你批准前被改过，请再看一遍";
pub const MSG_ALREADY_SENT: &str = "邮件已经发出，不能撤回";

/// `sha256(recipients, subject, body, attachment ids)`: the version a decision approves.
pub fn content_hash(d: &DraftRow) -> String {
    let lower = |l: &[Rcpt]| -> Vec<String> { l.iter().map(|r| r.addr.to_lowercase()).collect() };
    let files: Vec<&str> = d.files.iter().map(|f| f.id.as_str()).collect();
    let v = json!([
        lower(&d.to),
        lower(&d.cc),
        lower(&d.bcc),
        d.subject,
        d.body,
        files
    ]);
    hex::encode(Sha256::digest(v.to_string().as_bytes()))
}

/// A plausible single address: `local@domain.tld`, nothing that could smuggle headers or a second
/// recipient.
pub fn valid_addr(a: &str) -> bool {
    let a = a.trim();
    if a.len() > 254
        || a.chars()
            .any(|c| c.is_whitespace() || "<>,;:\"()[]\\".contains(c))
    {
        return false;
    }
    let Some((local, domain)) = a.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && !local.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
}

/// `Re: ` in front unless the subject already starts with it (any case, also `RE：`/`回复：`).
pub fn reply_subject(s: &str) -> String {
    let t = s.trim();
    let l = t.to_lowercase();
    if l.starts_with("re:")
        || l.starts_with("re：")
        || t.starts_with("回复：")
        || t.starts_with("回复:")
    {
        t.to_string()
    } else {
        format!("Re: {t}")
    }
}

/// `Fwd: ` in front unless already there.
pub fn forward_subject(s: &str) -> String {
    let t = s.trim();
    let l = t.to_lowercase();
    if l.starts_with("fwd:") || l.starts_with("fw:") {
        t.to_string()
    } else {
        format!("Fwd: {t}")
    }
}

/// The body with the mailbox signature at the end (once).
pub fn with_signature(body: &str, signature: &str) -> String {
    let sig = signature.trim();
    let b = body.trim_end();
    if sig.is_empty() || b.ends_with(sig) {
        return b.to_string();
    }
    format!("{b}\n\n{sig}")
}

fn addrs_of(v: &Value) -> Vec<Rcpt> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| {
            Some(Rcpt {
                addr: x.get("addr")?.as_str()?.trim().to_string(),
                name: x
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

/// `reply` → the original's `Reply-To` ?? `From`; `reply_all` → also its To and Cc. Every address
/// of mine is dropped, duplicates removed. Returns `(to, cc)`.
pub fn reply_recipients(orig: &MessageRow, all: bool, mine: &[String]) -> (Vec<Rcpt>, Vec<Rcpt>) {
    let is_mine = |a: &str| mine.iter().any(|m| m.eq_ignore_ascii_case(a.trim()));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut take = |r: Rcpt, out: &mut Vec<Rcpt>| {
        let k = r.addr.trim().to_lowercase();
        if k.is_empty() || is_mine(&k) || !seen.insert(k) {
            return;
        }
        out.push(r);
    };
    let mut to = Vec::new();
    let mut cc = Vec::new();
    if orig.direction == "out" {
        // replying to my own last message: write to its recipients again
        for r in addrs_of(&orig.to_addrs) {
            take(r, &mut to);
        }
    } else {
        let first = match orig.reply_to.as_deref().filter(|r| !r.trim().is_empty()) {
            Some(r) => Rcpt {
                addr: r.trim().to_string(),
                name: if r.trim().eq_ignore_ascii_case(&orig.from_addr) {
                    orig.from_name.clone()
                } else {
                    String::new()
                },
            },
            None => Rcpt {
                addr: orig.from_addr.trim().to_string(),
                name: orig.from_name.clone(),
            },
        };
        take(first, &mut to);
    }
    if all {
        for r in addrs_of(&orig.to_addrs) {
            take(r, &mut to);
        }
        for r in addrs_of(&orig.cc_addrs) {
            take(r, &mut cc);
        }
    }
    (to, cc)
}

/// `In-Reply-To` and `References` of a reply to `orig` (RFC 5322 §3.6.4): the parent's References (or,
/// without them, its In-Reply-To) followed by its Message-ID. Ids without angle brackets.
pub fn reply_headers(orig: &MessageRow) -> (Option<String>, Vec<String>) {
    let Some(mid) = orig.message_id.clone() else {
        return (None, Vec::new());
    };
    let mut refs: Vec<String> = if orig.refs.is_empty() {
        orig.in_reply_to.iter().cloned().collect()
    } else {
        orig.refs.clone()
    };
    if !refs.contains(&mid) {
        refs.push(mid.clone());
    }
    (Some(mid), refs)
}

/// The message a reply in this thread answers: the latest received one, else the latest.
pub fn reply_target(c: &Connection, thread: &str) -> MailResult<Option<MessageRow>> {
    let msgs = repo::thread_messages(c, thread)?;
    Ok(msgs
        .iter()
        .rev()
        .find(|m| m.direction == "in")
        .or(msgs.last())
        .cloned())
}

// ---------------------------------------------------------------- output

/// One recipient as the card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RcptOut {
    pub addr: String,
    pub name: String,
    pub party_id: Option<String>,
    /// Not me, not the own organization or a colleague (the audience is outside).
    pub external: bool,
}

/// `Draft`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DraftOut {
    pub id: String,
    pub connection_id: String,
    pub thread_id: Option<String>,
    pub reply_to: Option<String>,
    pub mode: String,
    pub to: Vec<RcptOut>,
    pub cc: Vec<RcptOut>,
    pub bcc: Vec<RcptOut>,
    pub subject: String,
    pub body: String,
    pub files: Vec<FileRef>,
    pub author: String,
    pub status: String,
    pub decision_id: Option<String>,
    pub basis: Option<Value>,
    pub send_at: Option<i64>,
    pub error: Option<String>,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub sent_message: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub sent: Option<i64>,
}

/// `(party_id, internal)` of an address: its account's party; internal = has a user (me, a colleague),
/// is the own organization, or the domain belongs to the own organization.
pub fn audience_of(c: &Connection, ws: &str, addr: &str) -> MailResult<(Option<String>, bool)> {
    use rusqlite::OptionalExtension;
    let a = addr.trim().to_lowercase();
    let party: Option<String> = c
        .prepare_cached(
            "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id
              WHERE h.workspace_id = ? AND h.family = 'email' AND h.handle = ? LIMIT 1",
        )?
        .query_row([ws, a.as_str()], |r| r.get(0))
        .optional()?;
    let p = match &party {
        Some(id) => current_party(c, id).map_err(MailError::Api)?,
        None => None,
    };
    let mut internal = p
        .as_ref()
        .is_some_and(|p| p.user_id.is_some() || p.org_kind.as_deref() == Some("self"));
    if !internal && let Some((_, domain)) = a.rsplit_once('@') {
        internal = c
            .prepare_cached(
                "SELECT 1 FROM org_domains d JOIN parties o ON o.id = d.org_id
                  WHERE d.workspace_id = ? AND d.domain = ? AND o.org_kind = 'self' LIMIT 1",
            )?
            .query_row([ws, domain], |_| Ok(()))
            .optional()?
            .is_some();
    }
    Ok((p.map(|p| p.id), internal))
}

fn rcpt_out(c: &Connection, ws: &str, l: &[Rcpt]) -> MailResult<Vec<RcptOut>> {
    l.iter()
        .map(|r| {
            let (party_id, internal) = audience_of(c, ws, &r.addr)?;
            Ok(RcptOut {
                addr: r.addr.clone(),
                name: r.name.clone(),
                party_id,
                external: !internal,
            })
        })
        .collect()
}

pub fn draft_out(c: &Connection, d: &DraftRow) -> MailResult<DraftOut> {
    let ws = d.workspace_id.as_str();
    Ok(DraftOut {
        id: d.id.clone(),
        connection_id: d.connection_id.clone(),
        thread_id: d.thread_id.clone(),
        reply_to: d.reply_to_message.clone(),
        mode: d.mode.clone(),
        to: rcpt_out(c, ws, &d.to)?,
        cc: rcpt_out(c, ws, &d.cc)?,
        bcc: rcpt_out(c, ws, &d.bcc)?,
        subject: d.subject.clone(),
        body: d.body.clone(),
        files: d.files.clone(),
        author: d.author.clone(),
        status: d.status.clone(),
        decision_id: d.decision_id.clone(),
        basis: d.basis.clone(),
        send_at: d.send_at,
        error: d.error.clone(),
        item_id: d.item_id.clone(),
        session_id: d.session_id.clone(),
        sent_message: d.sent_message.clone(),
        created: d.created,
        updated: d.updated,
        sent: d.sent,
    })
}

// ---------------------------------------------------------------- input

/// A recipient as the web sends it: `"a@b.c"` or `{"addr","name"}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RcptIn {
    Addr(String),
    Full {
        addr: String,
        #[serde(default)]
        name: String,
    },
}

impl RcptIn {
    fn into_rcpt(self) -> Rcpt {
        match self {
            Self::Addr(a) => Rcpt {
                addr: a.trim().to_string(),
                name: String::new(),
            },
            Self::Full { addr, name } => Rcpt {
                addr: addr.trim().to_string(),
                name: name.trim().to_string(),
            },
        }
    }
}

/// `POST /mail/drafts`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDraft {
    pub connection_id: String,
    pub mode: String,
    pub thread_id: Option<String>,
    pub reply_to: Option<String>,
    pub to: Vec<RcptIn>,
    pub cc: Option<Vec<RcptIn>>,
    pub bcc: Option<Vec<RcptIn>>,
    pub subject: String,
    pub body: String,
    pub files: Option<Vec<FileRef>>,
}

/// `PUT /mail/drafts/:id` (everything optional).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftPatch {
    pub connection_id: Option<String>,
    pub mode: Option<String>,
    pub thread_id: Option<String>,
    pub reply_to: Option<String>,
    pub to: Option<Vec<RcptIn>>,
    pub cc: Option<Vec<RcptIn>>,
    pub bcc: Option<Vec<RcptIn>>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub files: Option<Vec<FileRef>>,
}

fn conv(l: Option<Vec<RcptIn>>) -> Option<Vec<Rcpt>> {
    l.map(|l| l.into_iter().map(RcptIn::into_rcpt).collect())
}

/// Field checks of a draft's content; collects every failing field.
fn validate(d: &DraftRow) -> Result<(), ApiError> {
    let mut bad = Vec::new();
    if !["reply", "reply_all", "forward", "new"].contains(&d.mode.as_str()) {
        bad.push("mode".to_string());
    }
    for (k, l) in [("to", &d.to), ("cc", &d.cc), ("bcc", &d.bcc)] {
        for (i, r) in l.iter().enumerate() {
            if !valid_addr(&r.addr) {
                bad.push(format!("{k}.{i}"));
            }
            if r.name.chars().count() > 200 || r.name.contains(['\r', '\n']) {
                bad.push(format!("{k}.{i}.name"));
            }
        }
    }
    if d.to.len() + d.cc.len() + d.bcc.len() > MAX_RCPTS {
        bad.push("to".into());
    }
    if d.subject.chars().count() > MAX_SUBJECT_CHARS || d.subject.contains(['\r', '\n']) {
        bad.push("subject".into());
    }
    if d.body.chars().count() > MAX_BODY_CHARS {
        bad.push("body".into());
    }
    if d.files.len() > MAX_FILES {
        bad.push("files".into());
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Invalid(bad))
    }
}

/// The text of the approval card: `收件人：…\n抄送：…\n主题：…\n\n<正文>\n\n附件：…`.
pub fn decision_text(d: &DraftRow) -> String {
    let list = |l: &[Rcpt]| {
        l.iter()
            .map(|r| {
                if r.name.is_empty() {
                    r.addr.clone()
                } else {
                    format!("{} <{}>", r.name, r.addr)
                }
            })
            .collect::<Vec<_>>()
            .join("、")
    };
    let mut s = format!("收件人：{}\n", list(&d.to));
    if !d.cc.is_empty() {
        s.push_str(&format!("抄送：{}\n", list(&d.cc)));
    }
    if !d.bcc.is_empty() {
        s.push_str(&format!("密送：{}\n", list(&d.bcc)));
    }
    s.push_str(&format!("主题：{}\n\n{}", d.subject, d.body));
    if !d.files.is_empty() {
        let names: Vec<&str> = d.files.iter().map(|f| f.name.as_str()).collect();
        s.push_str(&format!("\n\n附件：{}", names.join("、")));
    }
    s
}

/// `发邮件给 <收件人名字>：<主题>`.
pub fn decision_question(d: &DraftRow) -> String {
    let names: Vec<&str> =
        d.to.iter()
            .map(|r| {
                if r.name.is_empty() {
                    r.addr.as_str()
                } else {
                    r.name.as_str()
                }
            })
            .collect();
    format!("发邮件给 {}：{}", names.join("、"), d.subject)
}

/// Opens the approval decision for the draft's current content: `status=pending`, the hash of
/// this version recorded. Open decisions about the draft are expired first (one card per draft).
pub fn open_approval(
    tx: &Tx,
    d: &mut DraftRow,
    requested_by: &str,
    now: i64,
) -> MailResult<String> {
    expire_open(tx, &d.workspace_id, &d.id)?;
    d.content_hash = content_hash(d);
    let dec = base::decisions::open(
        tx,
        &NewDecision {
            workspace_id: d.workspace_id.clone(),
            kind: "approve".into(),
            subject_type: SUBJECT_TYPE.into(),
            subject_id: d.id.clone(),
            item_id: d.item_id.clone(),
            session_id: d.session_id.clone(),
            question: decision_question(d),
            draft: Some(decision_text(d)),
            options: Some(APPROVE_OPTIONS.map(String::from).to_vec()),
            requested_by: requested_by.into(),
        },
    )?;
    repo::set_draft_decision(tx, &d.id, Some(&dec.id), &d.content_hash)?;
    repo::draft_transition(tx, &d.id, &["draft", "pending", "failed"], "pending", now)?;
    d.decision_id = Some(dec.id.clone());
    d.status = "pending".into();
    if let Some(t) = &d.thread_id {
        repo::set_thread_state(tx, t, "needs_reply", now)?;
    }
    Ok(dec.id)
}

/// Expires every open decision about a draft.
pub fn expire_open(tx: &Tx, ws: &str, draft: &str) -> MailResult<()> {
    for id in repo::open_draft_decisions(tx, ws, draft)? {
        base::decisions::settle(tx, &id, &json!({"expired": true}), "expired", &Value::Null)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- the service

impl Mail {
    pub(crate) fn viewer_of(&self, who: &Caller) -> MailResult<Viewer> {
        Ok(self.0.m.viewer(who)?)
    }

    /// A thread the caller may see (`404 没有这个往来`).
    pub(crate) fn own_thread(
        &self,
        c: &Connection,
        viewer: &Viewer,
        id: &str,
    ) -> MailResult<ThreadRow> {
        repo::thread(c, id)?
            .filter(|t| {
                t.workspace_id == viewer.ws
                    && viewer.sees(&Visibility::from_db(&t.visibility), &t.created_by)
            })
            .ok_or_else(|| ApiError::not_found("往来").into())
    }

    /// The mailbox behind a connection the caller may use (`404 没有这个连接`).
    pub(crate) fn own_mailbox(&self, who: &Caller, connection_id: &str) -> MailResult<Mailbox> {
        self.0
            .conns
            .get(who, connection_id)
            .map_err(MailError::from)?;
        self.0
            .m
            .store
            .reader(|c| Mailbox::load(c, &who.ws, connection_id))
    }

    /// A draft the caller may see: its mailbox is theirs (`404 没有这个草稿`).
    pub(crate) fn own_draft(&self, who: &Caller, id: &str) -> MailResult<DraftRow> {
        let d = self
            .0
            .m
            .store
            .reader(|c| repo::draft(c, id))?
            .filter(|d| d.workspace_id == who.ws)
            .ok_or_else(|| MailError::Api(ApiError::not_found("草稿")))?;
        if self.0.conns.get(who, &d.connection_id).is_err() {
            return Err(ApiError::not_found("草稿").into());
        }
        Ok(d)
    }

    fn out(&self, d: &DraftRow) -> MailResult<DraftOut> {
        self.0.m.store.reader(|c| draft_out(c, d))
    }

    fn notify_draft(&self, d: &DraftRow) {
        if let Some(t) = &d.thread_id {
            self.0.notify.thread(&d.workspace_id, t);
        }
    }

    /// Checks the thread / message references and the files of a draft for the caller.
    fn check_refs(&self, who: &Caller, d: &DraftRow) -> MailResult<()> {
        let viewer = self.viewer_of(who)?;
        let adapter = self
            .0
            .m
            .store
            .reader(|c| steve_modules::accounts::connections::repo::get(c, &d.connection_id))?
            .map(|r| r.adapter)
            .unwrap_or_default();
        self.0.m.store.reader(|c| -> MailResult<()> {
            if let Some(t) = &d.thread_id {
                let t = self.own_thread(c, &viewer, t)?;
                if t.connection_id != d.connection_id {
                    return Err(ApiError::not_found("往来").into());
                }
            }
            if let Some(m) = &d.reply_to_message {
                let ok = repo::message(c, m)?.is_some_and(|m| {
                    m.workspace_id == who.ws && Some(&m.thread_id) == d.thread_id.as_ref()
                });
                if !ok {
                    return Err(ApiError::not_found("邮件").into());
                }
            }
            let mut total = 0i64;
            for f in &d.files {
                let row = repo::file_row(c, &f.id)?
                    .filter(|r| r.0 == who.ws)
                    .ok_or_else(|| MailError::Api(ApiError::not_found("附件")))?;
                total += row.4;
            }
            if adapter == "outlook" && total > 3 * 1024 * 1024 {
                return Err(ApiError::rule("Outlook 账号一次最多发 3 MB 附件").into());
            }
            if total > crate::send::MAX_ATTACHMENTS_BYTES {
                return Err(ApiError::rule("附件合计不能超过 20 MB").into());
            }
            Ok(())
        })
    }

    /// `POST /mail/drafts`: you write one yourself (`author=me, status=draft`).
    pub fn create_draft(&self, who: &Caller, p: NewDraft) -> MailResult<DraftOut> {
        self.own_mailbox(who, &p.connection_id)?;
        let now = self.now();
        let mut d = DraftRow {
            id: uid("md"),
            workspace_id: who.ws.clone(),
            connection_id: p.connection_id,
            thread_id: p.thread_id,
            reply_to_message: p.reply_to,
            mode: p.mode,
            to: conv(Some(p.to)).unwrap_or_default(),
            cc: conv(p.cc).unwrap_or_default(),
            bcc: conv(p.bcc).unwrap_or_default(),
            subject: p.subject.trim().to_string(),
            body: p.body,
            files: p.files.unwrap_or_default(),
            author: "me".into(),
            item_id: None,
            session_id: None,
            run_id: None,
            decision_id: None,
            basis: None,
            content_hash: String::new(),
            status: "draft".into(),
            send_at: None,
            sent_message: None,
            error: None,
            created: now,
            updated: now,
            sent: None,
        };
        validate(&d)?;
        if d.reply_to_message.is_some() && d.thread_id.is_none() {
            return Err(ApiError::invalid("thread_id").into());
        }
        self.check_refs(who, &d)?;
        d.content_hash = content_hash(&d);
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            repo::insert_draft(tx, &d)?;
            base::audit(
                tx,
                &who.ws,
                &who.me,
                "mail_draft.create",
                &d.id,
                &json!({"mode": d.mode}),
            )?;
            Ok(())
        })?;
        self.notify_draft(&d);
        self.out(&d)
    }

    /// `PUT /mail/drafts/:id`: only `draft`/`pending` can change. A pending draft keeps the hash its
    /// decision was opened with, so approving the old card answers 409.
    pub fn update_draft(&self, who: &Caller, id: &str, p: DraftPatch) -> MailResult<DraftOut> {
        let mut d = self.own_draft(who, id)?;
        if !["draft", "pending"].contains(&d.status.as_str()) {
            return Err(ApiError::conflict("这封邮件已经不能改了").into());
        }
        if let Some(c) = p.connection_id.filter(|c| *c != d.connection_id) {
            self.own_mailbox(who, &c)?;
            d.connection_id = c;
        }
        if let Some(v) = p.mode {
            d.mode = v;
        }
        if let Some(v) = p.thread_id {
            d.thread_id = Some(v);
        }
        if let Some(v) = p.reply_to {
            d.reply_to_message = Some(v);
        }
        if let Some(v) = conv(p.to) {
            d.to = v;
        }
        if let Some(v) = conv(p.cc) {
            d.cc = v;
        }
        if let Some(v) = conv(p.bcc) {
            d.bcc = v;
        }
        if let Some(v) = p.subject {
            d.subject = v.trim().to_string();
        }
        if let Some(v) = p.body {
            d.body = v;
        }
        if let Some(v) = p.files {
            d.files = v;
        }
        validate(&d)?;
        self.check_refs(who, &d)?;
        if d.status == "draft" {
            d.content_hash = content_hash(&d);
        }
        let now = self.now();
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            let cur =
                repo::draft(tx, id)?.ok_or_else(|| MailError::Api(ApiError::not_found("草稿")))?;
            if cur.status != d.status {
                return Err(ApiError::conflict("这封邮件已经不能改了").into());
            }
            repo::update_draft_content(tx, &d, now)?;
            base::audit(tx, &who.ws, &who.me, "mail_draft.update", id, &json!({}))?;
            Ok(())
        })?;
        d.updated = now;
        self.notify_draft(&d);
        self.out(&d)
    }

    /// `GET /mail/drafts/:id`.
    pub fn get_draft(&self, who: &Caller, id: &str) -> MailResult<DraftOut> {
        let d = self.own_draft(who, id)?;
        self.out(&d)
    }

    /// The drafts of a thread (not discarded).
    pub fn thread_drafts(&self, who: &Caller, thread: &str) -> MailResult<Vec<DraftOut>> {
        let viewer = self.viewer_of(who)?;
        self.0.m.store.reader(|c| {
            self.own_thread(c, &viewer, thread)?;
            repo::thread_drafts(c, thread)?
                .iter()
                .map(|d| draft_out(c, d))
                .collect()
        })
    }

    /// `DELETE /mail/drafts/:id`: `discarded`; an open decision about it expires. Not after sending
    /// started.
    pub fn delete_draft(&self, who: &Caller, id: &str) -> MailResult<()> {
        let d = self.own_draft(who, id)?;
        let now = self.now();
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            if !repo::draft_transition(
                tx,
                id,
                &["draft", "pending", "approved", "failed"],
                "discarded",
                now,
            )? {
                let st = repo::draft(tx, id)?.map(|d| d.status).unwrap_or_default();
                return Err(ApiError::conflict(if st == "discarded" {
                    "这封邮件已经丢弃了"
                } else {
                    MSG_ALREADY_SENT
                })
                .into());
            }
            repo::set_draft_send_at(tx, id, None)?;
            expire_open(tx, &who.ws, id)?;
            base::audit(tx, &who.ws, &who.me, "mail_draft.discard", id, &json!({}))?;
            Ok(())
        })?;
        self.notify_draft(&d);
        Ok(())
    }

    /// `POST /mail/drafts/:id/send`: your click is your approval (last paragraph): a decision for
    /// the current content is opened and resolved `send` by you right away, through the same handler
    /// as every other approval. The mail then waits out the send delay in the queue.
    pub fn send_draft(&self, who: &Caller, id: &str) -> MailResult<DraftOut> {
        let d = self.own_draft(who, id)?;
        if d.to.is_empty() && d.cc.is_empty() && d.bcc.is_empty() {
            return Err(ApiError::rule("还没有收件人").into());
        }
        let now = self.now();
        let dec = self.0.m.store.transaction(|tx| -> MailResult<String> {
            let mut d =
                repo::draft(tx, id)?.ok_or_else(|| MailError::Api(ApiError::not_found("草稿")))?;
            if !["draft", "pending", "failed"].contains(&d.status.as_str()) {
                return Err(ApiError::conflict("这封邮件已经处理过了").into());
            }
            open_approval(tx, &mut d, &who.me, now)
        })?;
        match decisions::dispatch(&self.0.m, who, &dec, "send", "") {
            Some(r) => {
                r?;
            }
            None => {
                return Err(ApiError::Internal(
                    "mail_draft decision handler is not registered".into(),
                )
                .into());
            }
        }
        self.notify_draft(&d);
        self.get_draft(who, id)
    }

    /// `POST /mail/drafts/:id/cancel`: an approved mail still inside its delay goes back to `pending`
    /// and its approval reopens (the same as undoing the decision); otherwise 409.
    pub fn cancel_draft(&self, who: &Caller, id: &str) -> MailResult<DraftOut> {
        let d = self.own_draft(who, id)?;
        let now = self.now();
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            let cur =
                repo::draft(tx, id)?.ok_or_else(|| MailError::Api(ApiError::not_found("草稿")))?;
            crate::send::undo_approval(tx, &cur, now)?;
            if let Some(dec) = repo::send_approval(tx, &who.ws, id)? {
                repo::reopen_decision(tx, &dec.id)?;
                base::audit(tx, &who.ws, &who.me, "decision.undo", &dec.id, &json!({}))?;
            }
            base::audit(tx, &who.ws, &who.me, "mail_draft.cancel", id, &json!({}))?;
            Ok(())
        })?;
        self.notify_draft(&d);
        self.get_draft(who, id)
    }

    /// Opens the approval for a draft written by a bot or Steve (quick drafts, flows, `steve_mail_draft`):
    /// `requested_by` = the author. Returns the decision id.
    pub fn request_approval(&self, draft_id: &str) -> MailResult<String> {
        let now = self.now();
        let (dec, d) = self
            .0
            .m
            .store
            .transaction(|tx| -> MailResult<(String, DraftRow)> {
                let mut d = repo::draft(tx, draft_id)?
                    .ok_or_else(|| MailError::Api(ApiError::not_found("草稿")))?;
                if !["draft", "pending", "failed"].contains(&d.status.as_str()) {
                    return Err(ApiError::conflict("这封邮件已经处理过了").into());
                }
                let author = d.author.clone();
                let id = open_approval(tx, &mut d, &author, now)?;
                Ok((id, d))
            })?;
        self.notify_draft(&d);
        Ok(dec)
    }

    /// Writes a new draft (bot/Steve author) and opens its approval in one transaction.
    pub(crate) fn insert_pending(
        &self,
        mut d: DraftRow,
        basis: &CheckResult,
    ) -> MailResult<DraftRow> {
        let now = self.now();
        d.basis = Some(serde_json::to_value(basis).unwrap_or(Value::Null));
        let d = self.0.m.store.transaction(|tx| -> MailResult<DraftRow> {
            d.content_hash = content_hash(&d);
            repo::insert_draft(tx, &d)?;
            let author = d.author.clone();
            open_approval(tx, &mut d, &author, now)?;
            base::audit(
                tx,
                &d.workspace_id,
                &author,
                "mail_draft.create",
                &d.id,
                &json!({"mode": d.mode}),
            )?;
            Ok(d)
        })?;
        self.notify_draft(&d);
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert!(valid_addr("a@b.co"));
        assert!(valid_addr(" sarah.chen+x@acme.com "));
        for bad in [
            "",
            "a",
            "a@b",
            "a@@b.c",
            "a b@c.d",
            "a@b.c,d@e.f",
            "<a@b.c>",
            "a@.b",
            "a@b..c",
            "a@b.c\r\nBcc: x@y.z",
        ] {
            assert!(!valid_addr(bad), "{bad}");
        }
    }

    #[test]
    fn subjects() {
        assert_eq!(reply_subject("Friday review"), "Re: Friday review");
        assert_eq!(reply_subject("RE: Friday review"), "RE: Friday review");
        assert_eq!(reply_subject("回复：周五"), "回复：周五");
        assert_eq!(forward_subject("Fw: x"), "Fw: x");
        assert_eq!(with_signature("Hi\n\n", "— Peng"), "Hi\n\n— Peng");
        assert_eq!(with_signature("Hi\n\n— Peng", "— Peng"), "Hi\n\n— Peng");
        assert_eq!(with_signature("Hi", ""), "Hi");
    }
}

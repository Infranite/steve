//! Reading the mail library: the thread list with its five views, one thread with its
//! messages, drafts and items, the thread state (`PATCH`), and a message's original as HTML, inline
//! `cid:` image or `.eml`. Threads are private to the mailbox's owner: every entry point checks the
//! caller sees the thread (`404 没有这个往来` / `没有这封邮件`).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::accounts::connections::repo as conn_repo;
use steve_modules::{ApiError, Caller};
use steve_store::base::{self, Viewer, Visibility};

use crate::attach::Refused;
use crate::drafts::{DraftOut, draft_out};
use crate::error::{MailError, MailResult};
use crate::repo::{self, MessageRow, ThreadRow};
use crate::{Mail, nodes, parse, store};

/// `limit` of `GET /mail/threads`: default and maximum.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 100;
/// Inline images put into the HTML as `data:` URLs: per image and in total.
pub const MAX_INLINE: usize = 5 * 1024 * 1024;
pub const MAX_INLINE_TOTAL: usize = 15 * 1024 * 1024;
/// The original is past the local retention.
pub const PURGED: &str = "原件已经过了本地保留期，请先重新取原件";

/// The views of the list (需要处理 · 收件箱 · 等对方 · 已发送 · 草稿, and all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    /// Mail still in an inbox folder, not muted.
    #[default]
    Inbox,
    /// `state = needs_reply`.
    NeedsReply,
    /// `state = waiting` (you replied last).
    Waiting,
    /// Threads with a message you sent.
    Sent,
    /// Threads with an unsent draft.
    Drafts,
    All,
}

impl View {
    fn sql(self) -> &'static str {
        match self {
            View::Inbox => {
                "t.state <> 'muted' AND EXISTS (SELECT 1 FROM mail_messages m JOIN mail_folders f
                   ON f.connection_id = m.connection_id AND f.role = 'inbox'
                   WHERE m.thread_id = t.id AND EXISTS (SELECT 1 FROM json_each(m.folders) j WHERE j.value = f.ext_id))"
            }
            View::NeedsReply => "t.state = 'needs_reply'",
            View::Waiting => "t.state = 'waiting'",
            View::Sent => {
                "EXISTS (SELECT 1 FROM mail_messages m WHERE m.thread_id = t.id AND m.direction = 'out')"
            }
            View::Drafts => {
                "EXISTS (SELECT 1 FROM mail_drafts d WHERE d.thread_id = t.id AND d.status IN ('draft', 'pending', 'approved', 'sending', 'failed', 'unknown'))"
            }
            View::All => "1",
        }
    }
}

/// `GET /mail/threads?account=&view=&q=&before=&limit=`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    /// A connection id.
    pub account: Option<String>,
    #[serde(default)]
    pub view: View,
    /// Matches subject, sender name and address, snippet.
    pub q: Option<String>,
    /// Paging: threads with `last_at` before this (the last one of the previous page).
    pub before: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConnRef {
    pub id: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FromOut {
    pub name: String,
    pub addr: String,
    pub party_id: Option<String>,
    pub org: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignalOut {
    pub event_id: String,
    pub outcome_label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DraftBrief {
    pub id: String,
    pub status: String,
}

/// `ThreadBrief`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadBrief {
    pub id: String,
    pub connection: ConnRef,
    pub subject: String,
    pub last_at: i64,
    pub count: i64,
    pub unread: i64,
    pub state: String,
    pub from: Option<FromOut>,
    pub snippet: String,
    pub has_attachments: bool,
    pub signal: Option<SignalOut>,
    pub item_id: Option<String>,
    pub draft: Option<DraftBrief>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AddrOut {
    pub addr: String,
    pub name: String,
    pub party_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttachmentOut {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub file_id: Option<String>,
    pub inline: bool,
}

/// `Message`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessageOut {
    pub id: String,
    pub direction: String,
    pub from: AddrOut,
    pub to: Vec<AddrOut>,
    pub cc: Vec<AddrOut>,
    pub subject: String,
    pub date: i64,
    pub text: Option<String>,
    pub text_new: Option<String>,
    pub has_html: bool,
    pub purged: bool,
    pub attachments: Vec<AttachmentOut>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemRef {
    pub id: String,
    pub title: String,
    pub status: String,
}

/// `GET /mail/threads/:id`: the brief plus messages (oldest first), drafts and linked items.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadDetail {
    #[serde(flatten)]
    pub brief: ThreadBrief,
    pub messages: Vec<MessageOut>,
    pub drafts: Vec<DraftOut>,
    pub items: Vec<ItemRef>,
}

/// `PATCH /mail/threads/:id`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadPatch {
    /// `open` | `done` | `muted`.
    pub state: Option<String>,
}

/// A message's HTML as served.
#[derive(Debug, Clone, PartialEq)]
pub struct Html {
    pub body: String,
    pub csp: String,
}

/// An inline part.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub bytes: Vec<u8>,
    pub mime: String,
}

fn addr_list(v: &Value) -> Vec<AddrOut> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(AddrOut {
                addr: r.get("addr")?.as_str()?.to_string(),
                name: r
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                party_id: r.get("party_id").and_then(Value::as_str).map(String::from),
            })
        })
        .collect()
}

/// The `LIKE` pattern of a search text (`%`, `_`, `\` escaped).
fn like(q: &str) -> String {
    let mut s = String::from("%");
    for ch in q.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(ch);
    }
    s.push('%');
    s
}

fn current_org(c: &Connection, viewer: &Viewer, party: &str) -> Option<String> {
    steve_modules::contacts::service::jobs(c, viewer, party)
        .ok()?
        .into_iter()
        .find(|j| j.to.is_none())
        .map(|j| j.org_name)
}

/// The brief of a thread the viewer sees.
pub fn brief(c: &Connection, viewer: &Viewer, t: &ThreadRow) -> MailResult<ThreadBrief> {
    brief_with(c, viewer, t, &mut HashMap::new())
}

/// [`brief`] with the mailbox addresses looked up so far (a list shares a few mailboxes).
fn brief_with(
    c: &Connection,
    viewer: &Viewer,
    t: &ThreadRow,
    addresses: &mut HashMap<String, String>,
) -> MailResult<ThreadBrief> {
    let msgs = repo::thread_brief_rows(c, &t.id)?;
    let address = match addresses.get(&t.connection_id) {
        Some(a) => a.clone(),
        None => {
            let a = conn_repo::get(c, &t.connection_id)?
                .and_then(|r| {
                    r.config
                        .get("address")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .unwrap_or_default();
            addresses.insert(t.connection_id.clone(), a.clone());
            a
        }
    };
    let shown = msgs
        .iter()
        .rev()
        .find(|m| m.direction == "in")
        .or(msgs.last());
    let from = shown.map(|m| FromOut {
        name: m.from_name.clone(),
        addr: m.from_addr.clone(),
        party_id: m.from_party_id.clone(),
        org: m
            .from_party_id
            .as_deref()
            .and_then(|p| current_org(c, viewer, p)),
    });
    let signal = match msgs.iter().rev().find_map(|m| m.event_id.clone()) {
        Some(ev) => c
            .prepare_cached(
                "SELECT outcome_label FROM ext_events WHERE id = ? AND workspace_id = ?",
            )?
            .query_row([&ev, &t.workspace_id], |r| r.get::<_, String>(0))
            .optional()?
            .map(|label| SignalOut {
                event_id: ev,
                outcome_label: label,
            }),
        None => None,
    };
    let draft = c
        .prepare_cached(
            "SELECT id, status FROM mail_drafts WHERE thread_id = ? AND status NOT IN ('sent', 'discarded') ORDER BY updated DESC, rowid DESC LIMIT 1",
        )?
        .query_row([&t.id], |r| {
            Ok(DraftBrief {
                id: r.get(0)?,
                status: r.get(1)?,
            })
        })
        .optional()?;
    Ok(ThreadBrief {
        id: t.id.clone(),
        connection: ConnRef {
            id: t.connection_id.clone(),
            address,
        },
        subject: t.subject.clone(),
        last_at: t.last_at,
        count: t.count,
        unread: t.unread,
        state: t.state.clone(),
        from,
        snippet: msgs.last().map(|m| m.snippet.clone()).unwrap_or_default(),
        has_attachments: msgs.iter().any(|m| m.has_attachments),
        signal,
        item_id: nodes::item_of_thread(c, &t.workspace_id, &t.id)?,
        draft,
    })
}

fn message_out(c: &Connection, m: &MessageRow) -> MailResult<MessageOut> {
    let attachments = parts_of(c, &m.id)?
        .into_iter()
        .map(|a| AttachmentOut {
            id: a.id,
            name: a.name,
            mime: a.mime,
            size: a.size,
            file_id: a.file_id,
            inline: a.inline,
        })
        .collect();
    Ok(MessageOut {
        id: m.id.clone(),
        direction: m.direction.clone(),
        from: AddrOut {
            addr: m.from_addr.clone(),
            name: m.from_name.clone(),
            party_id: m.from_party_id.clone(),
        },
        to: addr_list(&m.to_addrs),
        cc: addr_list(&m.cc_addrs),
        subject: m.subject.clone(),
        date: m.date,
        text: m.body_text.clone(),
        text_new: m.body_new.clone(),
        has_html: m.has_html,
        purged: m.purged.is_some(),
        attachments,
    })
}

/// A `mail_attachments` row with what serving inline parts needs.
struct PartRow {
    id: String,
    part: String,
    name: String,
    mime: String,
    size: i64,
    content_id: Option<String>,
    inline: bool,
    file_id: Option<String>,
}

fn parts_of(c: &Connection, message: &str) -> MailResult<Vec<PartRow>> {
    let mut st = c.prepare_cached(
        "SELECT id, part, name, mime, size, content_id, inline, file_id FROM mail_attachments WHERE message_id = ? ORDER BY rowid",
    )?;
    Ok(st
        .query_map([message], |r| {
            Ok(PartRow {
                id: r.get(0)?,
                part: r.get(1)?,
                name: r.get(2)?,
                mime: r.get(3)?,
                size: r.get(4)?,
                content_id: r.get(5)?,
                inline: r.get::<_, i64>(6)? != 0,
                file_id: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn linked_items(c: &Connection, ws: &str, thread: &str) -> MailResult<Vec<ItemRef>> {
    let mut st = c.prepare_cached(
        "SELECT i.id, i.title, i.status FROM links l JOIN items i ON i.id = l.subject_id AND i.workspace_id = l.workspace_id
          WHERE l.workspace_id = ? AND l.subject_type = 'item' AND l.predicate = 'about'
            AND l.object_type = 'mail_thread' AND l.object_id = ?
          ORDER BY l.created, l.rowid",
    )?;
    let rows = st.query_map([ws, thread], |r| {
        Ok(ItemRef {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
        })
    })?;
    let mut out: Vec<ItemRef> = Vec::new();
    for r in rows {
        let r = r?;
        if !out.iter().any(|x| x.id == r.id) {
            out.push(r);
        }
    }
    Ok(out)
}

/// The Content-Security-Policy of a message's HTML: nothing loads, no scripts, inline styles
/// only; remote images only with `images`.
pub fn csp(images: bool) -> String {
    let img = if images {
        "data: cid: https:"
    } else {
        "data: cid:"
    };
    format!("default-src 'none'; style-src 'unsafe-inline'; img-src {img}; sandbox")
}

/// Cleans mail HTML: no scripts, event handlers, forms, frames, `javascript:` links or `<base>`;
/// layout attributes and inline styles stay (the CSP keeps them from loading anything).
pub fn sanitize(html: &str) -> String {
    let mut b = ammonia::Builder::default();
    b.add_generic_attributes([
        "style", "class", "align", "valign", "bgcolor", "color", "width", "height", "border",
        "dir", "face", "size",
    ])
    .add_tag_attributes("table", ["cellpadding", "cellspacing", "background"])
    .add_tag_attributes("td", ["colspan", "rowspan", "nowrap"])
    .add_tag_attributes("th", ["colspan", "rowspan", "nowrap"])
    .add_tags(["center", "font"])
    .add_url_schemes(["cid"])
    .link_rel(Some("noopener noreferrer"));
    b.clean(html).to_string()
}

/// Replaces `src="cid:<id>"` with the inline part as a `data:` URL (when known and small enough).
fn inline_cids(html: &str, parts: &[(String, Part)]) -> String {
    use base64::Engine;
    let mut out = html.to_string();
    let mut total = 0usize;
    for (cid, p) in parts {
        if p.bytes.len() > MAX_INLINE || total + p.bytes.len() > MAX_INLINE_TOTAL {
            continue;
        }
        let needle = format!("src=\"cid:{cid}\"");
        if !out.contains(&needle) {
            continue;
        }
        total += p.bytes.len();
        let data = base64::engine::general_purpose::STANDARD.encode(&p.bytes);
        out = out.replace(&needle, &format!("src=\"data:{};base64,{data}\"", p.mime));
    }
    out
}

impl Mail {
    fn nf_thread() -> MailError {
        ApiError::not_found("往来").into()
    }

    /// `GET /mail/threads`.
    pub fn list_threads(&self, who: &Caller, q: &ListQuery) -> MailResult<Vec<ThreadBrief>> {
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
        let text = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let mut bad = Vec::new();
        if !(1..=MAX_LIMIT).contains(&limit) {
            bad.push("limit".to_string());
        }
        if text.is_some_and(|s| s.chars().count() > 200) {
            bad.push("q".to_string());
        }
        if !bad.is_empty() {
            return Err(ApiError::Invalid(bad).into());
        }
        if let Some(a) = &q.account {
            self.0.conns.get(who, a).map_err(MailError::from)?;
        }
        let viewer = self.viewer_of(who)?;
        self.0.m.store.reader(|c| {
            let scope = base::visible_sql("t", &viewer);
            let mut sql = format!(
                "SELECT {} FROM mail_threads t WHERE t.workspace_id = ? AND {} AND {}",
                repo::THREAD_COLS,
                scope.sql,
                q.view.sql()
            );
            let mut args: Vec<rusqlite::types::Value> = vec![who.ws.clone().into()];
            args.extend(scope.params);
            if let Some(a) = &q.account {
                sql.push_str(" AND t.connection_id = ?");
                args.push(a.clone().into());
            }
            if let Some(s) = text {
                sql.push_str(
                    " AND (t.subject LIKE ?1x ESCAPE '\\' OR EXISTS (SELECT 1 FROM mail_messages m WHERE m.thread_id = t.id AND (m.from_name LIKE ?1x ESCAPE '\\' OR m.from_addr LIKE ?1x ESCAPE '\\' OR m.snippet LIKE ?1x ESCAPE '\\')))",
                );
                let p = like(s);
                // positional parameters: one per placeholder
                sql = sql.replace("?1x", "?");
                for _ in 0..4 {
                    args.push(p.clone().into());
                }
            }
            if let Some(b) = q.before {
                sql.push_str(" AND t.last_at < ?");
                args.push(b.into());
            }
            sql.push_str(" ORDER BY t.last_at DESC, t.id DESC LIMIT ?");
            args.push((limit as i64).into());
            let threads: Vec<ThreadRow> = c
                .prepare(&sql)?
                .query_map(params_from_iter(args), repo::thread_row)?
                .collect::<rusqlite::Result<_>>()?;
            let mut addresses = HashMap::new();
            threads
                .iter()
                .map(|t| brief_with(c, &viewer, t, &mut addresses))
                .collect()
        })
    }

    /// `GET /mail/threads/:id`.
    pub fn thread_detail(&self, who: &Caller, id: &str) -> MailResult<ThreadDetail> {
        let viewer = self.viewer_of(who)?;
        self.0.m.store.reader(|c| {
            let t = self.own_thread(c, &viewer, id)?;
            let messages = repo::thread_messages(c, &t.id)?
                .iter()
                .map(|m| message_out(c, m))
                .collect::<MailResult<Vec<_>>>()?;
            let drafts = repo::thread_drafts(c, &t.id)?
                .iter()
                .map(|d| draft_out(c, d))
                .collect::<MailResult<Vec<_>>>()?;
            Ok(ThreadDetail {
                brief: brief(c, &viewer, &t)?,
                messages,
                drafts,
                items: linked_items(c, &t.workspace_id, &t.id)?,
            })
        })
    }

    /// `PATCH /mail/threads/:id {state}`: `muted` makes later mail of the thread `ignored`
    /// (“这串往来已静音”); `done` submits candidate memories in the background.
    pub fn patch_thread(&self, who: &Caller, id: &str, p: &ThreadPatch) -> MailResult<ThreadBrief> {
        let state = p.state.as_deref();
        if state.is_some_and(|s| !["open", "done", "muted"].contains(&s)) {
            return Err(ApiError::invalid("state").into());
        }
        let viewer = self.viewer_of(who)?;
        let now = self.now();
        let (b, done) = self.0.m.store.transaction(|tx| -> MailResult<_> {
            let t = self.own_thread(tx, &viewer, id)?;
            let mut done = false;
            if let Some(s) = state
                && s != t.state
            {
                repo::set_thread_state(tx, &t.id, s, now)?;
                base::audit(
                    tx,
                    &who.ws,
                    &who.me,
                    "mail_thread.patch",
                    &t.id,
                    &json!({ "state": { "from": t.state, "to": s } }),
                )?;
                done = s == "done";
            }
            let t = repo::thread(tx, &t.id)?.ok_or_else(Self::nf_thread)?;
            Ok((brief(tx, &viewer, &t)?, done))
        })?;
        self.0.notify.thread(&who.ws, &b.id);
        if done && tokio::runtime::Handle::try_current().is_ok() {
            let (m, ws, t) = (self.clone(), who.ws.clone(), b.id.clone());
            tokio::spawn(async move {
                if let Err(e) = m.submit_done(&ws, &t).await {
                    tracing::warn!(thread = %t, "mail: candidate memories of a done thread: {e}");
                }
            });
        }
        Ok(b)
    }

    /// A message of a thread the caller sees, and its original's bytes (`410` when purged).
    fn own_message(&self, who: &Caller, id: &str) -> MailResult<(MessageRow, Option<Vec<u8>>)> {
        let viewer = self.viewer_of(who)?;
        let m = self.0.m.store.reader(|c| -> MailResult<MessageRow> {
            let m = repo::message(c, id)?
                .filter(|m| m.workspace_id == who.ws)
                .ok_or_else(|| MailError::from(ApiError::not_found("邮件")))?;
            repo::thread(c, &m.thread_id)?
                .filter(|t| viewer.sees(&Visibility::from_db(&t.visibility), &t.created_by))
                .ok_or_else(|| MailError::from(ApiError::not_found("邮件")))?;
            Ok(m)
        })?;
        let raw = match m
            .raw_path
            .as_deref()
            .and_then(|r| store::raw_abs_path(self.0.cfg.home(), r))
        {
            Some(p) => match std::fs::read(p) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
            None => None,
        };
        Ok((m, raw))
    }

    /// `GET /mail/messages/:id/raw`: the original (`message/rfc822`); `410` once purged.
    pub fn message_raw(&self, who: &Caller, id: &str) -> MailResult<Result<Vec<u8>, Refused>> {
        let (_, raw) = self.own_message(who, id)?;
        Ok(raw.ok_or_else(|| Refused::new(410, PURGED)))
    }

    /// `GET /mail/messages/:id/html`: the sanitized HTML body with inline images as `data:` URLs,
    /// and the CSP to serve it with. `404` without an HTML body, `410` once purged.
    pub fn message_html(
        &self,
        who: &Caller,
        id: &str,
        images: bool,
    ) -> MailResult<Result<Html, Refused>> {
        let (m, raw) = self.own_message(who, id)?;
        if !m.has_html {
            return Err(ApiError::not_found("HTML 正文").into());
        }
        let Some(raw) = raw else {
            return Ok(Err(Refused::new(410, PURGED)));
        };
        let html = parse::html_body(&raw).ok_or_else(|| ApiError::not_found("HTML 正文"))?;
        let atts = self.0.m.store.reader(|c| parts_of(c, &m.id))?;
        let parts: Vec<(String, Part)> = atts
            .iter()
            .filter(|a| a.mime.starts_with("image/"))
            .filter_map(|a| {
                let cid = a.content_id.clone()?;
                let bytes = parse::attachment_bytes(&raw, &a.part)?;
                Some((
                    cid,
                    Part {
                        bytes,
                        mime: a.mime.clone(),
                    },
                ))
            })
            .collect();
        Ok(Ok(Html {
            body: inline_cids(&sanitize(&html), &parts),
            csp: csp(images),
        }))
    }

    /// `GET /mail/messages/:id/cid/:cid`: an inline part by its `Content-ID`. Only images are served
    /// as such; anything else comes as `application/octet-stream`.
    pub fn message_cid(
        &self,
        who: &Caller,
        id: &str,
        cid: &str,
    ) -> MailResult<Result<Part, Refused>> {
        let (m, raw) = self.own_message(who, id)?;
        let cid = cid.trim().trim_start_matches('<').trim_end_matches('>');
        let a = self
            .0
            .m
            .store
            .reader(|c| parts_of(c, &m.id))?
            .into_iter()
            .find(|a| a.content_id.as_deref() == Some(cid))
            .ok_or_else(|| MailError::from(ApiError::not_found("图片")))?;
        let Some(raw) = raw else {
            return Ok(Err(Refused::new(410, PURGED)));
        };
        let bytes = parse::attachment_bytes(&raw, &a.part)
            .ok_or_else(|| MailError::from(ApiError::not_found("图片")))?;
        let mime = if a.mime.starts_with("image/") && !a.mime.contains("svg") {
            a.mime
        } else {
            "application/octet-stream".into()
        };
        Ok(Ok(Part { bytes, mime }))
    }

    /// The bus side of the module (integrator: call for every `ChangeEvent`): a connection that
    /// appears, recovers or goes away starts or stops its runner now (the first sync of a new mailbox
    /// runs at once instead of at the next minute's reconcile).
    pub fn on_event(&self, e: &steve_store::ChangeEvent) {
        if let steve_store::ChangeEvent::Connection { .. } = e
            && tokio::runtime::Handle::try_current().is_ok()
            && let Err(err) = self.reconcile()
        {
            tracing::warn!(error = %err, "mail runners");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_and_inlines() {
        let html = r#"<html><head><base href="https://evil/"><script>alert(1)</script></head>
<body onload="x()"><p style="color:red" onclick="y()">Hi <a href="javascript:z()">x</a> <a href="https://ok.example/">ok</a></p>
<img src="cid:logo@x"><img src="https://track.example/p.gif"><form action="/x"><input name="a"></form><iframe src="https://x"></iframe></body></html>"#;
        let s = sanitize(html);
        for bad in [
            "<script",
            "onload",
            "onclick",
            "javascript:",
            "<base",
            "<form",
            "<iframe",
            "<input",
        ] {
            assert!(!s.contains(bad), "{bad} in {s}");
        }
        assert!(s.contains(r#"style="color:red""#), "{s}");
        assert!(s.contains(r#"href="https://ok.example/""#), "{s}");
        assert!(s.contains(r#"src="cid:logo@x""#), "{s}");
        let parts = vec![(
            "logo@x".to_string(),
            Part {
                bytes: b"\x89PNG".to_vec(),
                mime: "image/png".into(),
            },
        )];
        let out = inline_cids(&s, &parts);
        assert!(
            out.contains(r#"src="data:image/png;base64,iVBORw==""#),
            "{out}"
        );
        assert!(
            out.contains("https://track.example/p.gif"),
            "the CSP blocks it"
        );
        assert_eq!(
            csp(false),
            "default-src 'none'; style-src 'unsafe-inline'; img-src data: cid:; sandbox"
        );
        assert!(csp(true).contains("img-src data: cid: https:;"));
        assert_eq!(like("50%_a\\"), "%50\\%\\_a\\\\%");
    }
}

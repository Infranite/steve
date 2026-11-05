//! Mail and items:
//! - [`item_threads`]: the threads an item is about (`links(item, from|about, mail_thread|mail_message)`);
//! - [`Mail::to_item`]: `POST /mail/threads/:id/to-item`, like signals' “转成事项”;
//! - [`MailPanel`]: the B1 item panel (`registry.item_panel(Box::new(MailPanel))`).

use async_trait::async_trait;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;
use steve_modules::panels::{
    ActionOutcome, ItemPanel, Panel, PanelAction, PanelHead, PanelRow, RowState,
};
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::base::links::{self, NewLink};
use steve_store::base::{self, ObjRef, Viewer, Visibility};

use crate::error::{MailError, MailResult};
use crate::parse::cut_chars;
use crate::repo::{self, ThreadRow};
use crate::{Mail, drafts};

/// `links.object_type` of a thread / a message.
pub const THREAD: &str = "mail_thread";
pub const MESSAGE: &str = "mail_message";
/// The source written on the links this module makes.
pub const LINK_SOURCE: &str = "mail";

/// An item made from a thread (steve-core `createItem`).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRequest {
    pub ws: String,
    /// Whose item it is (the caller of `to-item`).
    pub by: String,
    pub title: String,
    pub goal: String,
    /// An existing bot of the workspace.
    pub bot: String,
    /// `mode: "propose"`: start with a 方案 decision instead of a session.
    pub propose: bool,
    /// `["邮件", "mail"]`.
    pub source: (String, String),
}

/// steve-core as `to-item` needs it (the integrator implements it, e.g. over steve-flow's `Core`).
/// Never called while a store transaction is open.
#[async_trait]
pub trait MailItems: Send + Sync {
    /// Returns the item id (`ST-12`).
    async fn create_item(&self, req: ItemRequest) -> Result<String, String>;
}

/// Not wired: refuses.
pub struct NoItems;

#[async_trait]
impl MailItems for NoItems {
    async fn create_item(&self, _: ItemRequest) -> Result<String, String> {
        Err("建事项还没有接上".into())
    }
}

/// The threads an item is about, in link order: `from`/`about` links to a thread, or to a message of
/// one. Not filtered for visibility.
pub fn item_threads(c: &Connection, ws: &str, item: &str) -> MailResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for l in links::from(c, ws, &ObjRef::new("item", item), None)? {
        if !matches!(l.predicate.as_str(), "from" | "about") {
            continue;
        }
        let t = match l.object_type.as_str() {
            THREAD => Some(l.object_id.clone()),
            MESSAGE => repo::message(c, &l.object_id)?
                .filter(|m| m.workspace_id == ws)
                .map(|m| m.thread_id),
            _ => None,
        };
        if let Some(t) = t
            && !out.contains(&t)
        {
            out.push(t);
        }
    }
    Ok(out)
}

/// The visible threads of an item for `viewer`, most recent first.
pub fn visible_item_threads(
    c: &Connection,
    viewer: &Viewer,
    item: &str,
) -> MailResult<Vec<ThreadRow>> {
    let mut out = Vec::new();
    for id in item_threads(c, &viewer.ws, item)? {
        if let Some(t) = repo::thread(c, &id)?.filter(|t| {
            t.workspace_id == viewer.ws
                && viewer.sees(&Visibility::from_db(&t.visibility), &t.created_by)
        }) {
            out.push(t);
        }
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.last_at));
    Ok(out)
}

/// The open items (not done, dropped or archived) that are about a thread.
pub fn thread_items(c: &Connection, ws: &str, thread: &str) -> MailResult<Vec<String>> {
    let mut ids: Vec<String> = Vec::new();
    for l in links::to(c, ws, &ObjRef::new(THREAD, thread), None)? {
        if l.subject_type == "item"
            && matches!(l.predicate.as_str(), "from" | "about")
            && !ids.contains(&l.subject_id)
        {
            ids.push(l.subject_id);
        }
    }
    Ok(repo::open_items(c, &ids)?
        .into_iter()
        .map(|(id, _, _)| id)
        .collect())
}

/// `POST /mail/threads/:id/to-item`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToItem {
    pub bot: Option<String>,
    pub mode: Option<String>,
}

impl Mail {
    /// “转成事项” for a thread: an item from the subject and the latest received message, linked
    /// `from` that message, `about` the thread, `raised_by` its sender (and `from` its event, whose
    /// outcome becomes `item`). A thread that already has an open item answers that item.
    pub async fn to_item(&self, who: &Caller, thread: &str, p: ToItem) -> MailResult<String> {
        let propose = match p.mode.as_deref() {
            None | Some("start") => false,
            Some("propose") => true,
            Some(_) => return Err(ApiError::invalid("mode").into()),
        };
        let (mm, w, t, bot) = (self.clone(), who.clone(), thread.to_string(), p.bot.clone());
        let prep =
            tokio::task::spawn_blocking(steve_i18n::carry(move || mm.to_item_prepare(&w, &t, bot)))
                .await
                .map_err(|e| MailError::Other(format!("task failed: {e}")))??;
        let (req, orig) = match prep {
            Ok(id) => return Ok(id),
            Err(x) => x,
        };
        let req = ItemRequest { propose, ..req };
        let item =
            self.0.items.create_item(req).await.map_err(|e| {
                ApiError::rule(steve_i18n::t!("mail.item.create_failed", error = e))
            })?;
        let (mm, w, t, it) = (self.clone(), who.clone(), thread.to_string(), item.clone());
        tokio::task::spawn_blocking(steve_i18n::carry(move || {
            mm.to_item_link(&w, &t, &it, orig.as_ref())
        }))
        .await
        .map_err(|e| MailError::Other(format!("task failed: {e}")))??;
        self.0.notify.thread(&who.ws, thread);
        Ok(item)
    }

    /// `Ok(item)` when the thread already has an open item; otherwise the request to make.
    #[allow(clippy::type_complexity)]
    fn to_item_prepare(
        &self,
        who: &Caller,
        thread: &str,
        bot: Option<String>,
    ) -> MailResult<Result<String, (ItemRequest, Option<repo::MessageRow>)>> {
        let viewer = self.viewer_of(who)?;
        self.0.m.store.reader(|c| {
            let t = self.own_thread(c, &viewer, thread)?;
            if let Some(id) = thread_items(c, &who.ws, &t.id)?.into_iter().next() {
                return Ok(Ok(id));
            }
            let bot = match bot {
                Some(b) => {
                    repo::bot(c, &who.ws, &b)?
                        .ok_or_else(|| MailError::Api(ApiError::not_found("Bot")))?;
                    b
                }
                None => match repo::bot(c, &who.ws, "mail")? {
                    Some(_) => "mail".to_string(),
                    None => repo::main_bot(c, &who.ws)?.ok_or_else(|| {
                        MailError::Api(ApiError::rule(steve_i18n::t!("mail.item.no_main_bot")))
                    })?,
                },
            };
            let orig = drafts::reply_target(c, &t.id)?;
            let subject = if t.subject.trim().is_empty() {
                "邮件往来".to_string()
            } else {
                t.subject.trim().to_string()
            };
            let goal = match &orig {
                Some(m) => {
                    let who = if m.from_name.is_empty() {
                        m.from_addr.clone()
                    } else {
                        format!("{} <{}>", m.from_name, m.from_addr)
                    };
                    let text = m.body_new.clone().unwrap_or_else(|| m.snippet.clone());
                    format!(
                        "邮件往来「{subject}」\n\n{who}：\n{}",
                        cut_chars(text.trim(), 4000)
                    )
                }
                None => format!("邮件往来「{subject}」"),
            };
            Ok(Err((
                ItemRequest {
                    ws: who.ws.clone(),
                    by: who.me.clone(),
                    title: cut_chars(&subject, 80),
                    goal,
                    bot,
                    propose: false,
                    source: ("邮件".into(), "mail".into()),
                },
                orig,
            )))
        })
    }

    fn to_item_link(
        &self,
        who: &Caller,
        thread: &str,
        item: &str,
        orig: Option<&repo::MessageRow>,
    ) -> MailResult<()> {
        let ws = who.ws.as_str();
        let it = || ObjRef::new("item", item);
        self.0.m.store.transaction(|tx| -> MailResult<()> {
            links::add(
                tx,
                ws,
                &NewLink::new(it(), "about", ObjRef::new(THREAD, thread), LINK_SOURCE),
            )?;
            if let Some(m) = orig {
                links::add(
                    tx,
                    ws,
                    &NewLink::new(it(), "from", ObjRef::new(MESSAGE, &m.id), LINK_SOURCE),
                )?;
                if let Some(p) = m.from_party_id.as_ref().filter(|_| m.direction == "in") {
                    links::add(
                        tx,
                        ws,
                        &NewLink::new(it(), "raised_by", ObjRef::new("party", p), LINK_SOURCE),
                    )?;
                }
                if let Some(ev) = &m.event_id {
                    links::add(
                        tx,
                        ws,
                        &NewLink::new(it(), "from", ObjRef::new("event", ev), LINK_SOURCE),
                    )?;
                    tx.prepare_cached(
                        "UPDATE ext_events SET outcome = 'item', outcome_label = ?, work_item_id = ? WHERE id = ? AND workspace_id = ?",
                    )?
                    .execute(rusqlite::params![format!("转成了事项 {item}"), item, ev, ws])?;
                }
            }
            base::audit(
                tx,
                ws,
                &who.me,
                "mail_thread.to_item",
                thread,
                &json!({"item": item}),
            )?;
            Ok(())
        })
    }
}

// ---------------------------------------------------------------- the item panel

/// The B1 panel “往来邮件”: per thread the item is about (and the user may see), the latest message's
/// sender, time and snippet, linked to the thread page.
pub struct MailPanel;

/// Panel id prefix.
pub const PANEL: &str = "mail.thread";

fn state_badge(state: &str) -> Option<String> {
    Some(match state {
        "needs_reply" => steve_i18n::t!("mail.badge.needs_reply"),
        "waiting" => steve_i18n::t!("mail.badge.waiting"),
        "done" => steve_i18n::t!("mail.badge.done"),
        "muted" => steve_i18n::t!("mail.badge.muted"),
        _ => return None,
    })
}

fn draft_badge(status: &str) -> Option<String> {
    Some(match status {
        "pending" => steve_i18n::t!("mail.badge.draft_pending"),
        "approved" | "sending" => steve_i18n::t!("mail.badge.draft_sending"),
        "unknown" => steve_i18n::t!("mail.badge.draft_unknown"),
        "failed" => steve_i18n::t!("mail.badge.draft_failed"),
        _ => return None,
    })
}

/// The panels of an item for `viewer`.
pub fn item_panels(c: &Connection, viewer: &Viewer, item: &str) -> MailResult<Vec<Panel>> {
    let mut out = Vec::new();
    for t in visible_item_threads(c, viewer, item)? {
        let href = format!("#/mail/t/{}", t.id);
        let msgs = repo::thread_messages(c, &t.id)?;
        let mut badges: Vec<String> = state_badge(&t.state).into_iter().collect();
        if let Some(b) = repo::thread_drafts(c, &t.id)?
            .iter()
            .rev()
            .find_map(|d| draft_badge(&d.status))
        {
            badges.push(b);
        }
        let rows = msgs
            .last()
            .map(|m| {
                let who = if m.from_name.trim().is_empty() {
                    m.from_addr.clone()
                } else {
                    m.from_name.clone()
                };
                vec![PanelRow {
                    state: if m.direction == "out" {
                        RowState::Ok
                    } else {
                        RowState::Pending
                    },
                    text: if m.direction == "out" {
                        steve_i18n::t!("mail.panel.me_to", names = rcpt_names(&m.to_addrs))
                    } else {
                        who
                    },
                    sub: (!m.snippet.trim().is_empty()).then(|| m.snippet.clone()),
                    href: Some(href.clone()),
                }]
            })
            .unwrap_or_default();
        out.push(Panel {
            id: format!("{PANEL}:{}", t.id),
            plugin: "mail".into(),
            title: steve_i18n::t!("mail.panel.title"),
            head: Some(PanelHead {
                text: if t.subject.trim().is_empty() {
                    steve_i18n::t!("mail.subject.none")
                } else {
                    t.subject.clone()
                },
                href: Some(href.clone()),
                badges,
            }),
            rows,
            foot: Some(steve_i18n::t!(
                "mail.panel.count",
                n = t.count.max(msgs.len() as i64)
            )),
            actions: vec![PanelAction {
                id: "open".into(),
                label: steve_i18n::t!("mail.panel.open"),
                href: Some(href),
            }],
            updated: msgs.last().map_or(t.last_at, |m| m.date),
        });
    }
    Ok(out)
}

fn rcpt_names(v: &serde_json::Value) -> String {
    let names: Vec<String> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let n = r.get("name").and_then(|n| n.as_str()).unwrap_or("").trim();
            let a = r.get("addr").and_then(|a| a.as_str())?;
            Some(if n.is_empty() {
                a.to_string()
            } else {
                n.to_string()
            })
        })
        .collect();
    names.join(&steve_i18n::t!("mail.list_sep"))
}

fn api(e: MailError) -> ApiError {
    match e {
        MailError::Api(a) => a,
        e => ApiError::Internal(e.to_string()),
    }
}

#[async_trait]
impl ItemPanel for MailPanel {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn panels(&self, c: &Connection, ws: &str, me: &str, item_id: &str) -> ApiResult<Vec<Panel>> {
        let viewer = Viewer::load(c, ws, me)?;
        item_panels(c, &viewer, item_id).map_err(api)
    }

    /// The only button is a link the web opens itself.
    async fn action(
        &self,
        _: &Modules,
        _: &Caller,
        _: &str,
        _: &str,
        _: &str,
    ) -> ApiResult<ActionOutcome> {
        Err(ApiError::not_found("操作"))
    }
}

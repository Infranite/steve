//! A conversation, whatever it comes from, and its messages as the digest reads them.
//!
//! A conversation is named `<type>:<id>`: the same `(type, id)` a memory's `mem_about` row, a link or a
//! subject points at. Today's types:
//! - `space`: a channel space (`spaces`) — a group, DM, topic group or thread on Feishu or an external
//!   channel (ext_ws); its messages are `chan_messages` (received and sent ones; drafts and blocked
//!   replies are not part of the conversation);
//! - `mail_thread`: a mail thread (`mail_threads`); its messages are `mail_messages`.
//!
//! The connector protocol (phase C) adds types (a PR, an issue …) by adding a reader here: everything after
//! [`read_after`] only sees [`Msg`].

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_store::{
    StoreError,
    base::{Viewer, Visibility},
};

type Result<T> = std::result::Result<T, StoreError>;

/// The conversation types this build reads.
pub const TYPES: [&str; 2] = ["space", "mail_thread"];

/// A conversation: `space:sp_…`, `mail_thread:mt_…`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConvRef {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

impl ConvRef {
    pub fn new(kind: &str, id: &str) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }

    /// Parses `<type>:<id>` of a known type.
    pub fn parse(key: &str) -> Option<Self> {
        let (k, id) = key.split_once(':')?;
        (TYPES.contains(&k) && !id.is_empty() && id.len() <= 128).then(|| Self::new(k, id))
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }

    pub fn obj(&self) -> steve_store::base::ObjRef {
        steve_store::base::ObjRef::new(&self.kind, &self.id)
    }

    /// Where the conversation's own page is.
    pub fn href(&self) -> String {
        match self.kind.as_str() {
            "space" => format!("#/channels/space/{}", self.id),
            "mail_thread" => format!("#/mail/thread/{}", self.id),
            _ => String::new(),
        }
    }
}

/// What the digest needs to know about a conversation besides its messages.
#[derive(Debug, Clone, Serialize)]
pub struct ConvInfo {
    pub conv: ConvRef,
    pub name: String,
    /// The source's own kind: `group|dm|channel|thread|meeting` for a space, `mail_thread`.
    pub source_kind: String,
    /// The adapter it came through (`feishu_app`, `ext_ws`, `imap` …), when known.
    pub adapter: String,
    pub members: i64,
    pub has_external: bool,
    /// The space's duty (its role, `space_settings.duty`); empty for mail.
    pub role: String,
    /// 只记录 (`space_settings.muted`).
    pub muted: bool,
    /// `realtime|quiet|blocked` (`space_prefs`); blocked conversations are not digested.
    pub attention: String,
    pub active: bool,
}

/// One participant as a message names them. `party` is the contact Steve knows them as.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub party: Option<String>,
    pub account: Option<String>,
    pub name: String,
    /// An app or bot account (alerts, notifications), or Steve itself.
    pub bot: bool,
    /// You (the workspace owner), on any of your accounts.
    pub mine: bool,
}

/// One message of a conversation, source-neutral.
#[derive(Debug, Clone, Serialize)]
pub struct Msg {
    /// Steve's id of the message (`cm_…`, `ml_…`): what every packet, segment and reference names.
    pub id: String,
    /// The provider's id, when it gave one.
    pub ext_id: Option<String>,
    pub at: i64,
    pub sender: Person,
    pub mentions: Vec<Person>,
    /// Steve's id of the message this one answers, in the same conversation.
    pub reply_to: Option<String>,
    /// The reply chain or topic: a thread key, a mail thread's reference root.
    pub thread: Option<String>,
    pub text: String,
    pub attachments: Vec<Value>,
    pub subject: Option<String>,
}

/// The party of the workspace owner (you), if Steve has one.
pub fn my_party(c: &Connection, ws: &str) -> Result<Option<String>> {
    Ok(c.prepare_cached(
        "SELECT p.id FROM parties p JOIN workspace_members m ON m.user_id = p.user_id
              WHERE p.workspace_id = ?1 AND m.workspace_id = ?1 AND m.role = 'owner'
              ORDER BY m.created LIMIT 1",
    )?
    .query_row([ws], |r| r.get(0))
    .optional()?)
}

/// The workspace owner's user id.
pub fn owner(c: &Connection, ws: &str) -> Result<Option<String>> {
    Ok(c
        .prepare_cached(
            "SELECT user_id FROM workspace_members WHERE workspace_id = ? AND role = 'owner' ORDER BY created LIMIT 1",
        )?
        .query_row([ws], |r| r.get(0))
        .optional()?)
}

/// The owner is not entitled to other members' personal channels or private mail.
/// Reuse the source's access contract; connector adapters must supply the same boundary.
pub fn owner_viewer(c: &Connection, ws: &str) -> Result<Option<Viewer>> {
    let Some(user) = owner(c, ws)? else {
        return Ok(None);
    };
    let viewer = Viewer::load(c, ws, &user)?;
    let scopes = steve_channels::spaces::visible_ids(c, &viewer)
        .map_err(|e| StoreError::Hook(e.to_string()))?
        .into_iter()
        .map(|id| format!("space:{id}"));
    Ok(Some(viewer.with_scopes(scopes)))
}

pub fn owner_sees(c: &Connection, ws: &str, conv: &ConvRef) -> Result<bool> {
    let Some(user) = owner(c, ws)? else {
        return Ok(false);
    };
    match conv.kind.as_str() {
        "space" => steve_channels::spaces::visible(c, &Viewer::bare(ws, &user), &conv.id)
            .map_err(|e| StoreError::Hook(e.to_string())),
        "mail_thread" => {
            let row: Option<(String, String)> = c.query_row(
                "SELECT visibility, created_by FROM mail_threads WHERE workspace_id = ? AND id = ?",
                params![ws, conv.id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let Some((visibility, by)) = row else {
                return Ok(false);
            };
            Ok(
                owner_viewer(c, ws)?
                    .is_some_and(|v| v.sees(&Visibility::from_db(&visibility), &by)),
            )
        }
        _ => Ok(false),
    }
}

/// A conversation's facts, `None` when absent or outside the digest owner's source access.
pub fn info(c: &Connection, ws: &str, conv: &ConvRef) -> Result<Option<ConvInfo>> {
    if !owner_sees(c, ws, conv)? {
        return Ok(None);
    }
    match conv.kind.as_str() {
        "space" => Ok(c
            .prepare_cached(
                "SELECT s.name, s.kind, coalesce(cn.adapter, ''), s.member_count, s.has_external,
                        coalesce(ss.duty, ''), coalesce(ss.muted, 0), coalesce(sp.attention, 'realtime'), s.status
                   FROM spaces s LEFT JOIN connections cn ON cn.id = s.connection_id
                   LEFT JOIN space_settings ss ON ss.space_id = s.id
                   LEFT JOIN space_prefs sp ON sp.space_id = s.id
                  WHERE s.workspace_id = ? AND s.id = ?",
            )?
            .query_row(params![ws, conv.id], |r| {
                Ok(ConvInfo {
                    conv: conv.clone(),
                    name: r.get(0)?,
                    source_kind: r.get(1)?,
                    adapter: r.get(2)?,
                    members: r.get(3)?,
                    has_external: r.get::<_, i64>(4)? != 0,
                    role: r.get(5)?,
                    muted: r.get::<_, i64>(6)? != 0,
                    attention: r.get(7)?,
                    active: r.get::<_, String>(8)? == "active",
                })
            })
            .optional()?),
        "mail_thread" => Ok(c
            .prepare_cached(
                "SELECT t.subject, coalesce(cn.adapter, ''), json_array_length(t.participants), t.state
                   FROM mail_threads t LEFT JOIN connections cn ON cn.id = t.connection_id
                  WHERE t.workspace_id = ? AND t.id = ?",
            )?
            .query_row(params![ws, conv.id], |r| {
                let state: String = r.get(3)?;
                Ok(ConvInfo {
                    conv: conv.clone(),
                    name: r.get(0)?,
                    source_kind: "mail_thread".into(),
                    adapter: r.get(1)?,
                    members: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    has_external: true,
                    role: String::new(),
                    muted: state == "muted",
                    attention: if state == "muted" { "quiet" } else { "realtime" }.into(),
                    active: true,
                })
            })
            .optional()?),
        _ => Ok(None),
    }
}

/// Conversations of `ws` with unreceipted messages (including late arrivals behind the cursor):
/// `(conv, newest message time)`.
pub fn with_news(c: &Connection, ws: &str, since: i64) -> Result<Vec<(ConvRef, i64)>> {
    let mut out = Vec::new();
    let mut st = c.prepare_cached(
        "SELECT s.id, x.last FROM spaces s
           JOIN (SELECT space_id, max(created) AS last FROM chan_messages
                  WHERE workspace_id = ?1 AND status IN ('received', 'sent') GROUP BY space_id) x ON x.space_id = s.id
           LEFT JOIN digest_cursors d ON d.workspace_id = ?1 AND d.conv = 'space:' || s.id
          WHERE s.workspace_id = ?1 AND EXISTS (SELECT 1 FROM chan_messages m WHERE m.space_id = s.id AND m.workspace_id = ?1
            AND m.status IN ('received', 'sent') AND m.created >= coalesce(d.since, ?2) AND NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = ?1 AND r.conv = 'space:' || s.id AND r.message_id = m.id))",
    )?;
    for r in st.query_map(params![ws, since], |r| {
        Ok((ConvRef::new("space", &r.get::<_, String>(0)?), r.get(1)?))
    })? {
        out.push(r?);
    }
    let mut st = c.prepare_cached(
        "SELECT t.id, x.last FROM mail_threads t
           JOIN (SELECT thread_id, max(date) AS last FROM mail_messages WHERE workspace_id = ?1 GROUP BY thread_id) x
             ON x.thread_id = t.id
           LEFT JOIN digest_cursors d ON d.workspace_id = ?1 AND d.conv = 'mail_thread:' || t.id
          WHERE t.workspace_id = ?1 AND EXISTS (SELECT 1 FROM mail_messages m WHERE m.thread_id = t.id AND m.workspace_id = ?1
            AND m.status IN ('received', 'sent') AND m.date >= coalesce(d.since, ?2) AND NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = ?1 AND r.conv = 'mail_thread:' || t.id AND r.message_id = m.id))",
    )?;
    for r in st.query_map(params![ws, since], |r| {
        Ok((
            ConvRef::new("mail_thread", &r.get::<_, String>(0)?),
            r.get(1)?,
        ))
    })? {
        out.push(r?);
    }
    let mut visible = Vec::new();
    for (conv, at) in out {
        if owner_sees(c, ws, &conv)? {
            visible.push((conv, at));
        }
    }
    Ok(visible)
}

/// Who an account is: `(party, display name, bot)`.
fn account(c: &Connection, id: &str) -> Result<Option<(String, String, bool)>> {
    Ok(c
        .prepare_cached(
            "SELECT a.party_id, coalesce(nullif(a.display_name, ''), nullif(p.name, ''), a.handle_display), a.kind IN ('app_bot', 'bot')
               FROM accounts a LEFT JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        )?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?)
}

/// Caches account lookups for one read.
struct People<'c> {
    c: &'c Connection,
    me: Option<String>,
    accounts: HashMap<String, Option<(String, String, bool)>>,
}

impl<'c> People<'c> {
    fn new(c: &'c Connection, ws: &str) -> Result<Self> {
        Ok(Self {
            c,
            me: my_party(c, ws)?,
            accounts: HashMap::new(),
        })
    }

    fn of_account(&mut self, id: &str) -> Result<Option<(String, String, bool)>> {
        if let Some(x) = self.accounts.get(id) {
            return Ok(x.clone());
        }
        let x = account(self.c, id)?;
        self.accounts.insert(id.to_string(), x.clone());
        Ok(x)
    }

    fn person(
        &mut self,
        account_id: Option<&str>,
        party: Option<String>,
        name: &str,
    ) -> Result<Person> {
        let a = match account_id {
            Some(id) => self.of_account(id)?,
            None => None,
        };
        let party = party.or_else(|| a.as_ref().map(|a| a.0.clone()));
        let mine = party.is_some() && party == self.me;
        Ok(Person {
            name: if name.trim().is_empty() {
                a.as_ref().map(|a| a.1.clone()).unwrap_or_default()
            } else {
                name.trim().to_string()
            },
            bot: a.as_ref().is_some_and(|a| a.2) && !mine,
            account: account_id.map(str::to_string),
            party,
            mine,
        })
    }
}

/// Up to `limit` messages of `conv` after `(at, id)`, oldest first.
pub fn read_after(
    c: &Connection,
    ws: &str,
    conv: &ConvRef,
    at: i64,
    id: &str,
    limit: usize,
) -> Result<Vec<Msg>> {
    match conv.kind.as_str() {
        "space" => space_messages(c, ws, &conv.id, Some((at, id)), None, limit, false),
        "mail_thread" => mail_messages(c, ws, &conv.id, Some((at, id)), None, limit, false),
        _ => Ok(Vec::new()),
    }
}

/// The last `limit` messages of `conv` up to and including `(at, id)`, oldest first (the context a
/// window carries from the one before).
pub fn read_before(
    c: &Connection,
    ws: &str,
    conv: &ConvRef,
    at: i64,
    id: &str,
    limit: usize,
) -> Result<Vec<Msg>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut v = match conv.kind.as_str() {
        "space" => space_messages(c, ws, &conv.id, None, Some((at, id)), limit, false)?,
        "mail_thread" => mail_messages(c, ws, &conv.id, None, Some((at, id)), limit, false)?,
        _ => Vec::new(),
    };
    v.reverse();
    Ok(v)
}

/// Messages by Steve id (quotes outside the window, tool reads).
pub fn by_ids(c: &Connection, ws: &str, conv: &ConvRef, ids: &[String]) -> Result<Vec<Msg>> {
    let mut out = Vec::new();
    for id in ids {
        let at: Option<i64> = match conv.kind.as_str() {
            "space" => c
                .prepare_cached("SELECT created FROM chan_messages WHERE id = ? AND space_id = ? AND workspace_id = ?")?
                .query_row(params![id, conv.id, ws], |r| r.get(0))
                .optional()?,
            "mail_thread" => c
                .prepare_cached("SELECT date FROM mail_messages WHERE id = ? AND thread_id = ? AND workspace_id = ?")?
                .query_row(params![id, conv.id, ws], |r| r.get(0))
                .optional()?,
            _ => None,
        };
        if let Some(at) = at {
            out.extend(
                read_before(c, ws, conv, at, id, 1)?
                    .into_iter()
                    .filter(|m| &m.id == id),
            );
        }
    }
    Ok(out)
}

fn json_list(s: Option<String>) -> Vec<Value> {
    s.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn space_messages(
    c: &Connection,
    ws: &str,
    space: &str,
    after: Option<(i64, &str)>,
    upto: Option<(i64, &str)>,
    limit: usize,
    unread: bool,
) -> Result<Vec<Msg>> {
    let (cond, order, (at, id)) = match (after, upto) {
        (Some(a), _) => (
            "(m.created > ?3 OR (m.created = ?3 AND m.id > ?4))",
            "ASC",
            a,
        ),
        (None, Some(u)) => (
            "(m.created < ?3 OR (m.created = ?3 AND m.id <= ?4))",
            "DESC",
            u,
        ),
        (None, None) => ("1", "ASC", (0, "")),
    };
    let receipt = if unread {
        "AND NOT EXISTS (SELECT 1 FROM digest_receipts d WHERE d.workspace_id = m.workspace_id AND d.conv = 'space:' || m.space_id AND d.message_id = m.id)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT m.id, m.external_id, m.created, m.sender_party_id, m.sender_account_id, m.direction, coalesce(m.text, ''),
                m.attachments, m.mentions, m.thread_key, m.on_behalf,
                (SELECT q.id FROM chan_messages q WHERE q.space_id = m.space_id AND q.external_id = m.reply_to AND m.reply_to IS NOT NULL LIMIT 1)
           FROM chan_messages m
          WHERE m.workspace_id = ?1 AND m.space_id = ?2 AND m.status IN ('received', 'sent') AND {cond} {receipt}
          ORDER BY m.created {order}, m.id {order} LIMIT {limit}"
    );
    struct Row {
        id: String,
        ext: Option<String>,
        at: i64,
        party: Option<String>,
        account: Option<String>,
        out: bool,
        text: String,
        atts: Option<String>,
        mentions: Option<String>,
        thread: Option<String>,
        behalf: String,
        reply: Option<String>,
    }
    let rows: Vec<Row> = c
        .prepare_cached(&sql)?
        .query_map(params![ws, space, at, id], |r| {
            Ok(Row {
                id: r.get(0)?,
                ext: r.get(1)?,
                at: r.get(2)?,
                party: r.get(3)?,
                account: r.get(4)?,
                out: r.get::<_, String>(5)? == "out",
                text: r.get(6)?,
                atts: r.get(7)?,
                mentions: r.get(8)?,
                thread: r.get(9)?,
                behalf: r.get(10)?,
                reply: r.get(11)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut people = People::new(c, ws)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let mut sender = people.person(r.account.as_deref(), r.party.clone(), "")?;
        // Steve's replies (a persona on a bot account) are Steve's, not yours
        if r.out && !sender.mine {
            sender.bot = true;
            if sender.name.is_empty() {
                sender.name = "Steve".into();
            }
        }
        if r.out && r.behalf == "proxy" {
            sender.mine = true;
            sender.bot = false;
        }
        let mut mentions = Vec::new();
        for m in json_list(r.mentions) {
            let acc = m.get("account_id").and_then(Value::as_str);
            let name = m.get("name").and_then(Value::as_str).unwrap_or_default();
            mentions.push(people.person(acc, None, name)?);
        }
        // a thread key equal to the message's own id is no topic: a message on its own
        let thread = r.thread.filter(|t| Some(t) != r.ext.as_ref());
        out.push(Msg {
            id: r.id,
            ext_id: r.ext,
            at: r.at,
            sender,
            mentions,
            reply_to: r.reply,
            thread,
            text: r.text,
            attachments: json_list(r.atts),
            subject: None,
        });
    }
    Ok(out)
}

fn mail_messages(
    c: &Connection,
    ws: &str,
    thread: &str,
    after: Option<(i64, &str)>,
    upto: Option<(i64, &str)>,
    limit: usize,
    unread: bool,
) -> Result<Vec<Msg>> {
    let (cond, order, (at, id)) = match (after, upto) {
        (Some(a), _) => ("(m.date > ?3 OR (m.date = ?3 AND m.id > ?4))", "ASC", a),
        (None, Some(u)) => ("(m.date < ?3 OR (m.date = ?3 AND m.id <= ?4))", "DESC", u),
        (None, None) => ("1", "ASC", (0, "")),
    };
    let receipt = if unread {
        "AND NOT EXISTS (SELECT 1 FROM digest_receipts d WHERE d.workspace_id = m.workspace_id AND d.conv = 'mail_thread:' || m.thread_id AND d.message_id = m.id)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT m.id, m.message_id, m.date, m.from_party_id, m.from_account_id, coalesce(nullif(m.body_new, ''), nullif(m.body_text, ''), m.snippet),
                m.to_addrs, m.cc_addrs, m.from_name, m.from_addr, m.subject, m.bulk, m.refs,
                (SELECT q.id FROM mail_messages q WHERE q.thread_id = m.thread_id AND q.message_id = m.in_reply_to AND m.in_reply_to IS NOT NULL LIMIT 1)
           FROM mail_messages m
          WHERE m.workspace_id = ?1 AND m.thread_id = ?2 AND m.status IN ('received', 'sent') AND {cond} {receipt}
          ORDER BY m.date {order}, m.id {order} LIMIT {limit}"
    );
    struct Row {
        id: String,
        mid: Option<String>,
        at: i64,
        party: Option<String>,
        account: Option<String>,
        text: String,
        to: Option<String>,
        cc: Option<String>,
        name: String,
        addr: String,
        subject: String,
        bulk: bool,
        refs: Option<String>,
        reply: Option<String>,
    }
    let rows: Vec<Row> = c
        .prepare_cached(&sql)?
        .query_map(params![ws, thread, at, id], |r| {
            Ok(Row {
                id: r.get(0)?,
                mid: r.get(1)?,
                at: r.get(2)?,
                party: r.get(3)?,
                account: r.get(4)?,
                text: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                to: r.get(6)?,
                cc: r.get(7)?,
                name: r.get(8)?,
                addr: r.get(9)?,
                subject: r.get(10)?,
                bulk: r.get::<_, i64>(11)? != 0,
                refs: r.get(12)?,
                reply: r.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut people = People::new(c, ws)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let name = if r.name.trim().is_empty() {
            r.addr.clone()
        } else {
            r.name.clone()
        };
        let mut sender = people.person(r.account.as_deref(), r.party.clone(), &name)?;
        sender.bot |= r.bulk;
        let mut mentions = Vec::new();
        for a in json_list(r.to).into_iter().chain(json_list(r.cc)) {
            let acc = a.get("account_id").and_then(Value::as_str);
            let n = a
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.trim().is_empty())
                .or_else(|| a.get("addr").and_then(Value::as_str))
                .unwrap_or_default();
            mentions.push(people.person(acc, None, n)?);
        }
        let mut atts = Vec::new();
        let mut st = c.prepare_cached(
            "SELECT id, name, mime, size, inline FROM mail_attachments WHERE message_id = ? ORDER BY part",
        )?;
        for a in st.query_map([&r.id], |x| {
            Ok(serde_json::json!({
                "type": if x.get::<_, String>(2)?.starts_with("image/") { "image" } else { "file" },
                "key": x.get::<_, String>(0)?, "name": x.get::<_, String>(1)?,
                "mime": x.get::<_, String>(2)?, "size": x.get::<_, i64>(3)?, "inline": x.get::<_, i64>(4)? != 0,
            }))
        })? {
            let a = a?;
            if a["inline"] != true {
                atts.push(a);
            }
        }
        // the root of the reference chain: the thread's topic within a mail thread
        let root = json_list(r.refs)
            .first()
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push(Msg {
            id: r.id,
            ext_id: r.mid,
            at: r.at,
            sender,
            mentions,
            reply_to: r.reply,
            thread: root,
            text: r.text,
            attachments: atts,
            subject: Some(r.subject).filter(|s| !s.trim().is_empty()),
        });
    }
    Ok(out)
}

/// The number of messages `conv` has after `(at, id)`, and the oldest and newest time among them.
pub fn pending(
    c: &Connection,
    ws: &str,
    conv: &ConvRef,
    at: i64,
    id: &str,
) -> Result<(i64, i64, i64)> {
    let sql = match conv.kind.as_str() {
        "space" => {
            "SELECT count(*), coalesce(min(created), 0), coalesce(max(created), 0) FROM chan_messages
              WHERE workspace_id = ?1 AND space_id = ?2 AND status IN ('received', 'sent')
                AND (created > ?3 OR (created = ?3 AND id > ?4))"
        }
        "mail_thread" => {
            "SELECT count(*), coalesce(min(date), 0), coalesce(max(date), 0) FROM mail_messages
              WHERE workspace_id = ?1 AND thread_id = ?2 AND status IN ('received', 'sent') AND (date > ?3 OR (date = ?3 AND id > ?4))"
        }
        _ => return Ok((0, 0, 0)),
    };
    Ok(c.prepare_cached(sql)?
        .query_row(params![ws, conv.id, at, id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?)
}

/// The share of bot messages among the last `n` messages of `conv` (`None` with fewer than 10).
pub fn bot_share(c: &Connection, ws: &str, conv: &ConvRef, n: usize) -> Result<Option<f64>> {
    let msgs = read_before(c, ws, conv, i64::MAX, "\u{10ffff}", n)?;
    if msgs.len() < 10 {
        return Ok(None);
    }
    let bots = msgs.iter().filter(|m| m.sender.bot).count();
    Ok(Some(bots as f64 / msgs.len() as f64))
}

/// Stored messages not yet cut, including late arrivals behind the timestamp cursor.
pub fn unread(
    c: &Connection,
    ws: &str,
    conv: &ConvRef,
    since: i64,
    limit: usize,
) -> Result<Vec<Msg>> {
    match conv.kind.as_str() {
        "space" => space_messages(c, ws, &conv.id, Some((since, "")), None, limit, true),
        "mail_thread" => mail_messages(c, ws, &conv.id, Some((since, "")), None, limit, true),
        _ => Ok(Vec::new()),
    }
}

pub fn unread_stats(
    c: &Connection,
    ws: &str,
    conv: &ConvRef,
    since: i64,
) -> Result<(i64, i64, i64)> {
    let (table, parent, at) = match conv.kind.as_str() {
        "space" => ("chan_messages", "space_id", "created"),
        "mail_thread" => ("mail_messages", "thread_id", "date"),
        _ => return Ok((0, 0, 0)),
    };
    let sql = format!("SELECT count(*), coalesce(min({at}), 0), coalesce(max({at}), 0) FROM {table} m
        WHERE m.workspace_id = ?1 AND m.{parent} = ?2 AND m.{at} >= ?3 AND m.status IN ('received', 'sent')
        AND NOT EXISTS (SELECT 1 FROM digest_receipts r WHERE r.workspace_id = ?1 AND r.conv = ?4 AND r.message_id = m.id)");
    Ok(
        c.query_row(&sql, params![ws, conv.id, since, conv.key()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?,
    )
}

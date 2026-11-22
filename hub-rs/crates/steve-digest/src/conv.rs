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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_store::{
    AccountIdentity, Attachment, ChannelMessage, ChannelSources, Connections, Contacts,
    MailMessage, MailSources, Members, MessageSelection, Read, Store, StoreError, base::Viewer,
    digest::Windows,
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

/// The party of the workspace owner, if Steve has one.
pub fn my_party(c: &mut impl Read, ws: &str) -> Result<Option<String>> {
    for user in Members::new(c).owners(ws)? {
        if let Some(party) = Contacts::new(c).user_party(ws, &user)? {
            return Ok(Some(party));
        }
    }
    Ok(None)
}

pub fn owner(c: &mut impl Read, ws: &str) -> Result<Option<String>> {
    Members::new(c).owner(ws)
}

pub fn owner_viewer(c: &mut impl Read, ws: &str) -> Result<Option<Viewer>> {
    Ok(owner(c, ws)?.map(|user| Viewer::bare(ws, &user)))
}

pub fn owner_sees(c: &mut impl Read, ws: &str, conv: &ConvRef) -> Result<bool> {
    let Some(viewer) = owner_viewer(c, ws)? else {
        return Ok(false);
    };
    visible(c, &viewer, conv)
}

fn visible(c: &mut impl Read, viewer: &Viewer, conv: &ConvRef) -> Result<bool> {
    match conv.kind.as_str() {
        "space" => Ok(ChannelSources(c).space(&viewer.ws, &conv.id)?.is_some()),
        "mail_thread" => Ok(MailSources(c)
            .thread(&viewer.ws, &conv.id)?
            .is_some_and(|t| t.visible_to(viewer))),
        _ => Ok(false),
    }
}

/// A conversation's facts, checked against the owner's source access in the same snapshot.
pub fn info(c: &mut impl Read, ws: &str, conv: &ConvRef) -> Result<Option<ConvInfo>> {
    let Some(viewer) = owner_viewer(c, ws)? else {
        return Ok(None);
    };
    Ok(Some(match conv.kind.as_str() {
        "space" => {
            let Some(s) = ChannelSources(c).space(ws, &conv.id)? else {
                return Ok(None);
            };
            ConvInfo {
                conv: conv.clone(),
                name: s.name,
                source_kind: s.kind,
                adapter: Connections(c).adapter(s.connection.as_deref())?,
                members: s.members,
                has_external: s.external,
                role: s.role,
                muted: s.muted,
                attention: s.attention,
                active: s.active,
            }
        }
        "mail_thread" => {
            let Some(t) = MailSources(c)
                .thread(ws, &conv.id)?
                .filter(|t| t.visible_to(&viewer))
            else {
                return Ok(None);
            };
            ConvInfo {
                conv: conv.clone(),
                name: t.subject,
                source_kind: "mail_thread".into(),
                adapter: Connections(c).adapter(t.connection.as_deref())?,
                members: t.members,
                has_external: true,
                role: String::new(),
                muted: t.muted,
                attention: if t.muted { "quiet" } else { "realtime" }.into(),
                active: true,
            }
        }
        _ => return Ok(None),
    }))
}

/// Conversations with pending messages. Acknowledged history is never scanned.
pub fn with_news(store: &Store, ws: &str) -> Result<Vec<(ConvRef, i64)>> {
    store.read(|r| {
        let mut out = Vec::new();
        let Some(viewer) = owner_viewer(r, ws)? else {
            return Ok(out);
        };
        for (key, at) in r.digest_windows().pending_conversations(ws)? {
            if let Some(conv) = ConvRef::parse(&key)
                && visible(r, &viewer, &conv)?
            {
                out.push((conv, at));
            }
        }
        Ok(out)
    })
}

struct People {
    me: Option<String>,
    accounts: HashMap<String, AccountIdentity>,
}

impl People {
    fn person(&self, account: Option<&str>, party: Option<String>, name: &str) -> Person {
        let identity = account.and_then(|id| self.accounts.get(id));
        let party = party.or_else(|| identity.map(|a| a.party.clone()));
        let mine = party.is_some() && party == self.me;
        let name = if name.trim().is_empty() {
            identity.map(|a| a.name.clone()).unwrap_or_default()
        } else {
            name.trim().to_owned()
        };
        Person {
            party,
            account: account.map(str::to_owned),
            name,
            bot: identity.is_some_and(|a| a.bot) && !mine,
            mine,
        }
    }
}

fn account_ids(people: &[Value]) -> impl Iterator<Item = &str> {
    people.iter().filter_map(|p| p["account_id"].as_str())
}

fn channel_message(r: ChannelMessage, people: &People) -> Msg {
    let mut sender = people.person(r.account.as_deref(), r.party, "");
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
    let mentions = r
        .mentions
        .iter()
        .map(|p| {
            people.person(
                p["account_id"].as_str(),
                None,
                p["name"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    Msg {
        id: r.id,
        thread: r.thread.filter(|t| Some(t) != r.ext.as_ref()),
        ext_id: r.ext,
        at: r.at,
        sender,
        mentions,
        reply_to: r.reply,
        text: r.text,
        attachments: r.atts,
        subject: None,
    }
}

fn mail_message(r: MailMessage, people: &People, attachments: Vec<Attachment>) -> Msg {
    let name = if r.name.trim().is_empty() {
        &r.addr
    } else {
        &r.name
    };
    let mut sender = people.person(r.account.as_deref(), r.party, name);
    sender.bot |= r.bulk;
    let mentions =
        r.to.iter()
            .chain(&r.cc)
            .map(|p| {
                let name = p["name"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                    .or_else(|| p["addr"].as_str())
                    .unwrap_or_default();
                people.person(p["account_id"].as_str(), None, name)
            })
            .collect();
    let attachments = attachments
        .into_iter()
        .map(|a| {
            serde_json::json!({"type": if a.mime.starts_with("image/") { "image" } else { "file" },
            "key":a.id,"name":a.name,"mime":a.mime,"size":a.size,"inline":false})
        })
        .collect();
    Msg {
        id: r.id,
        ext_id: r.mid,
        at: r.at,
        sender,
        mentions,
        reply_to: r.reply,
        thread: r.refs.into_iter().next(),
        text: r.text,
        attachments,
        subject: Some(r.subject).filter(|s| !s.trim().is_empty()),
    }
}

fn messages(
    c: &mut impl Read,
    ws: &str,
    conv: &ConvRef,
    selection: MessageSelection<'_>,
) -> Result<Vec<Msg>> {
    match conv.kind.as_str() {
        "space" => {
            let rows = ChannelSources(c).messages(ws, &conv.id, selection)?;
            let ids = rows.iter().flat_map(|r| {
                r.account
                    .as_deref()
                    .into_iter()
                    .chain(account_ids(&r.mentions))
            });
            let people = People {
                me: my_party(c, ws)?,
                accounts: Contacts::new(c).account_identities(ids)?,
            };
            Ok(rows
                .into_iter()
                .map(|r| channel_message(r, &people))
                .collect())
        }
        "mail_thread" => {
            let rows = MailSources(c).messages(ws, &conv.id, selection)?;
            let ids = rows.iter().flat_map(|r| {
                r.account
                    .as_deref()
                    .into_iter()
                    .chain(account_ids(&r.to))
                    .chain(account_ids(&r.cc))
            });
            let people = People {
                me: my_party(c, ws)?,
                accounts: Contacts::new(c).account_identities(ids)?,
            };
            let ids: Vec<_> = rows.iter().map(|r| r.id.clone()).collect();
            let mut attachments = MailSources(c).attachments(&ids)?;
            Ok(rows
                .into_iter()
                .map(|r| {
                    let atts = attachments.remove(&r.id).unwrap_or_default();
                    mail_message(r, &people, atts)
                })
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

pub fn read_after(
    c: &mut impl Read,
    ws: &str,
    conv: &ConvRef,
    at: i64,
    id: &str,
    limit: usize,
) -> Result<Vec<Msg>> {
    messages(c, ws, conv, MessageSelection::After(at, id, limit))
}

pub fn read_before(
    c: &mut impl Read,
    ws: &str,
    conv: &ConvRef,
    at: i64,
    id: &str,
    limit: usize,
) -> Result<Vec<Msg>> {
    let mut rows = messages(c, ws, conv, MessageSelection::Through(at, id, limit))?;
    rows.reverse();
    Ok(rows)
}

/// Messages in caller order. A range with one endpoint repeated still returns both endpoints.
pub fn by_ids(c: &mut impl Read, ws: &str, conv: &ConvRef, ids: &[String]) -> Result<Vec<Msg>> {
    let mut out = Vec::new();
    for ids in ids.chunks(500) {
        let found: HashMap<_, _> = messages(c, ws, conv, MessageSelection::Ids(ids))?
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        out.extend(ids.iter().filter_map(|id| found.get(id).cloned()));
    }
    Ok(out)
}

pub fn guess_kind(c: &mut impl Read, ws: &str, info: &ConvInfo) -> Result<&'static str> {
    Ok(match info.source_kind.as_str() {
        "dm" => "dm",
        "mail_thread" => "mail",
        _ => {
            let last = read_before(c, ws, &info.conv, i64::MAX, "\u{10ffff}", 200)?;
            if last.len() >= 10
                && last.iter().filter(|msg| msg.sender.bot).count() * 5 >= last.len() * 3
            {
                "feed"
            } else {
                "team"
            }
        }
    })
}

pub fn unread(c: &mut impl Read, ws: &str, conv: &ConvRef, limit: usize) -> Result<Vec<Msg>> {
    let ids = Windows::new(c).pending_ids(ws, &conv.key(), limit)?;
    by_ids(c, ws, conv, &ids)
}

//! A starred contact's message to you reaches you on its own, on your own account (`Mode::Own`).
//!
//! **Who is starred**: a contact you starred in Steve (`parties.important`, the star on the contact
//! page; through the contact it was merged into, [`crate::decide::starred`]). The star is set in
//! Steve only. It is not read from Feishu/Lark: the Open API has no starred contacts (星标联系人) for a
//! user, and the user-session gateway the own-account connection uses does not offer them either.
//!
//! **What reaches you**: a message from a starred person that is meant for you — a private chat, an
//! @ of you, or a reply to you in a group — becomes a personal notification ([`crate::notifications`],
//! kind `starred`, urgency `high`) through your own notification policy and its route. It does so
//! whatever the space's replies are set to (the rules, 只记录 `muted`, `reply_as_you = record`, no
//! voice there, no grant to send as you): those decide what is drafted or asked for you, this only
//! tells you. Under `digest` it goes out now only to the private chat you chose for it, else it stays
//! in your inbox, as under `log`; quiet hours hold it unless you allowed starred contacts through
//! (`starred_in_quiet`).
//!
//! **Decided for the chat, not the receiving connection**: in a group where your own account and a
//! Steve bot are both members, one message comes in twice and is stored once, from whichever copy
//! came first (the other is a duplicate: in the chat's one space, `space_identities`, or through
//! [`crate::messages::seen_elsewhere`]). Whichever it is, each
//! own-account connection that sees the chat is asked whether the message is meant for its user:
//! from what is stored (an @ of their account or contact, a reply to one of their messages in any
//! space of the chat) when a bot's copy came first, and again from your own account's copy when it
//! comes second ([`OwnCopy`]). One notification per message and user, whichever asks first.
//!
//! **What does not**: anything you are there for yourself — you answered it or are chatting there,
//! in any space of the chat ([`crate::presence::owner_engaged_chat`]), asked when the message comes
//! in and again before it is sent, so nothing goes out once you replied; a chat you set aside in any
//! of its spaces (不提醒, 屏蔽, `reply_as_you = off`, which win over the star); the same sender's next
//! messages while you have not spoken there since the last notification (one per stretch of talk).
//! The star never makes a draft: what is drafted or asked is still the rules' and the composer's
//! ([`crate::decide`]: a starred contact's card). Purging or deleting the message purges every copy
//! of it the notification made (migration 101's triggers).

use steve_modules::Caller;
use steve_store::channel_message_repo::starred::{self as repo, Message as Stored};

use crate::inbound::{Inbound, Next};
use crate::spaces::{self, Attention, Mode, ReplyAsYou};
use crate::{ChannelResult, Channels};

/// The notification's head.
pub const HEAD: &str = "⭐ 星标联系人";
/// At most this many characters of the message in the notification (one look on a phone).
pub const TEXT_MAX: usize = 200;

/// What the star did with a message, on its event after what the rules did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alert {
    /// Notified (`sends`: sent to you now; else kept in your inbox, `log`).
    Notified { id: String, sends: bool },
    /// Notified about the sender's earlier message already; you have not spoken there since.
    Again,
    /// You have no notification policy turned on.
    PolicyOff,
    /// You are there yourself.
    YouThere,
}

impl Alert {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Notified { sends: true, .. } => LABEL_SENT,
            Self::Notified { sends: false, .. } => LABEL_INBOX,
            Self::Again => LABEL_AGAIN,
            Self::PolicyOff => LABEL_POLICY_OFF,
            Self::YouThere => LABEL_YOU_THERE,
        }
    }
}

pub const LABEL_SENT: &str = "⭐ 星标联系人，已单独提醒你";
pub const LABEL_INBOX: &str = "⭐ 星标联系人，已放进你的通知收件箱";
pub const LABEL_AGAIN: &str = "⭐ 星标联系人，刚提醒过你，不重复提醒";
pub const LABEL_POLICY_OFF: &str = "⭐ 星标联系人，你的个人通知没打开，没有单独提醒";
pub const LABEL_YOU_THERE: &str = "⭐ 星标联系人，你已在这里，不单独提醒";

/// The space is set aside for the star: 不提醒 or 屏蔽, or `reply_as_you = off` (Steve leaves it
/// alone on your account).
pub fn set_aside(r: &mut impl steve_store::Read, space: &str) -> ChannelResult<bool> {
    let set = spaces::settings(r, space)?;
    Ok(set.attention >= Attention::Quiet || set.reply_as_you == ReplyAsYou::Off)
}

/// How a message reached you, in the notification.
fn how(dm: bool, mentioned: bool, space: &str) -> String {
    if dm {
        "私聊你".into()
    } else if mentioned {
        format!("在「{space}」@ 了你")
    } else {
        format!("在「{space}」回复了你")
    }
}

/// The notification's text: who and how on the first line (the inbox's title), then what they said.
pub fn body(sender: &str, dm: bool, mentioned: bool, space: &str, text: &str) -> String {
    let who = if sender.trim().is_empty() {
        "有人"
    } else {
        sender.trim()
    };
    let said = crate::decide::flat(text, TEXT_MAX);
    let head = format!("{HEAD} {who} {}", how(dm, mentioned, space));
    if said.is_empty() {
        head
    } else {
        format!("{head}\n{said}")
    }
}

/// How the owner's own account received a message, from the copy at hand: a private chat with the
/// owner, an @ of the owner's account (`is_connection` on that account's copy), and the provider ids
/// it replies to (its `reply_to` and thread root). What is stored about the message is looked at
/// besides ([`Channels::starred_alert`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnCopy {
    pub dm: bool,
    pub mentioned: bool,
    /// A reply to the owner, as the receiving connection's rules saw it ([`crate::rules::Facts`]).
    pub reply_to_me: bool,
    pub replies_to: Vec<String>,
}

impl OwnCopy {
    /// The owner's account's copy of `msg` (a chat of kind `kind` as that account sees it).
    pub fn of(msg: &crate::InboundMessage, kind: crate::SpaceKind) -> Self {
        Self {
            dm: kind == crate::SpaceKind::Dm,
            mentioned: msg.mentions_connection(),
            reply_to_me: false,
            replies_to: [msg.reply_to.as_deref(), msg.root_id.as_deref()]
                .into_iter()
                .flatten()
                .filter(|e| *e != msg.external_id)
                .map(String::from)
                .collect(),
        }
    }
}

/// One owner a message may be meant for: the user of an own-account connection (`Mode::Own`) that
/// sees the message's chat, that connection and its account.
struct Owner {
    user: String,
    conn: String,
    account: String,
    /// That account's own copy of the message, when it is the one at hand.
    copy: OwnCopy,
}

impl Channels {
    /// The star's part for a stored inbound message `inbound` received on connection `conn`, before
    /// the rules run (they may take minutes). The star is decided for the chat, not for the
    /// connection that happened to receive it: whichever copy came first (a bot's or your own
    /// account's), each own-account connection that sees the chat is asked whether the message is
    /// meant for its user ([`meant`]). `None`: not a starred contact's message to you, or its chat
    /// is set aside. A failure only logs: the message goes on.
    pub(crate) fn starred_alert(&self, ws: &str, conn: &str, inbound: &Inbound) -> Option<Alert> {
        if inbound.next != Next::Handle || inbound.from_owner || inbound.duplicate {
            return None;
        }
        let copy = inbound.facts.as_ref().map(|f| OwnCopy {
            dm: f.dm,
            mentioned: f.mentioned,
            reply_to_me: f.reply_to_me,
            replies_to: Vec::new(),
        });
        self.starred_logged(ws, conn, inbound, copy)
    }

    /// The star's part when your own account's copy of a message arrives after another
    /// connection's (a bot's) was stored: the message is a duplicate there and nothing else looks
    /// at it, but only this copy says whether it @-ed or answered you. Notified: the stored
    /// message's event says so too.
    pub(crate) fn starred_elsewhere(
        &self,
        ws: &str,
        conn: &str,
        inbound: &Inbound,
        copy: OwnCopy,
    ) -> Option<Alert> {
        let a = self.starred_logged(ws, conn, inbound, Some(copy))?;
        if matches!(a, Alert::Notified { .. }) {
            let (mid, label) = (inbound.message_id.clone(), a.label());
            if let Err(e) = self
                .store()
                .write(|tx| crate::respond::append_label(tx, &mid, label))
            {
                tracing::warn!(
                    space = inbound.space_id,
                    "starred contact's notification label: {e}"
                );
            }
        }
        Some(a)
    }

    fn starred_logged(
        &self,
        ws: &str,
        conn: &str,
        inbound: &Inbound,
        copy: Option<OwnCopy>,
    ) -> Option<Alert> {
        if inbound.blocked || inbound.history.is_some() || inbound.message_id.is_empty() {
            return None;
        }
        match self.starred_inner(ws, conn, &inbound.message_id, copy) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(
                    space = inbound.space_id,
                    "starred contact's notification: {e}"
                );
                None
            }
        }
    }

    fn starred_inner(
        &self,
        ws: &str,
        conn: &str,
        message: &str,
        copy: Option<OwnCopy>,
    ) -> ChannelResult<Option<Alert>> {
        let now = self.now();
        self.store().write(|w| -> ChannelResult<Option<Alert>> {
            let Some(m) = repo::message(w, ws, message)? else {
                return Ok(None);
            };
            if !crate::decide::starred(w, ws, m.party.as_deref())?
                || set_aside_chat(w, ws, &m.space)?
            {
                return Ok(None);
            }
            let mut best: Option<Alert> = None;
            for o in owners(w, ws, conn, &m.space, copy)? {
                let Some((dm, mentioned)) = meant(w, ws, &m, &o)? else {
                    continue;
                };
                let a = alert(w, ws, &m, &o, dm, mentioned, now)?;
                if best.as_ref().is_none_or(|b| rank(&a) > rank(b)) {
                    best = Some(a);
                }
            }
            Ok(best)
        })
    }
}

/// Which outcome speaks for the message when several owners see its chat.
fn rank(a: &Alert) -> u8 {
    match a {
        Alert::Notified { .. } => 4,
        Alert::YouThere => 3,
        Alert::Again => 2,
        Alert::PolicyOff => 1,
    }
}

/// The own-account connections that see the chat of `space`: the receiving connection `conn` when
/// it is one (with `copy`, its own copy of the message), and every other identity of the space or
/// connection with a space for the same chat (by what is stored only).
fn owners(
    r: &mut impl steve_store::Read,
    ws: &str,
    conn: &str,
    space: &str,
    copy: Option<OwnCopy>,
) -> ChannelResult<Vec<Owner>> {
    let others = repo::other_connections(r, ws, space, conn)?;
    let mut out = Vec::new();
    for (k, cp) in std::iter::once((conn.to_string(), copy.unwrap_or_default()))
        .chain(others.into_iter().map(|o| (o, OwnCopy::default())))
    {
        if let Some((user, account)) = own_user(r, ws, &k)?
            && !out.iter().any(|o: &Owner| o.user == user)
        {
            out.push(Owner {
                user,
                conn: k,
                account,
                copy: cp,
            });
        }
    }
    Ok(out)
}

/// Whether stored message `m` is meant for owner `o`, and how: `(private chat, @ of you)`, else a
/// reply to you. `None`: not meant for them, or it is their own.
fn meant(
    r: &mut impl steve_store::Read,
    ws: &str,
    m: &Stored,
    owner: &Owner,
) -> ChannelResult<Option<(bool, bool)>> {
    if m.sender_account.as_deref() == Some(owner.account.as_str())
        || match &m.party {
            Some(party) => repo::by_user(r, ws, &owner.user, party)?,
            None => false,
        }
    {
        return Ok(None);
    }
    let mentioned =
        owner.copy.mentioned || repo::mentions_user(r, ws, &owner.user, &m.id, &owner.account)?;
    if owner.copy.dm || mentioned {
        return Ok(Some((owner.copy.dm, mentioned)));
    }
    let references: Vec<_> = m
        .replies_to
        .iter()
        .chain(&owner.copy.replies_to)
        .map(String::as_str)
        .collect();
    let replied = owner.copy.reply_to_me
        || !references.is_empty()
            && repo::replied_to_user(
                r,
                &repo::ReplyAudience {
                    ws,
                    user: &owner.user,
                    chat: &m.chat,
                    account: &owner.account,
                    connection: &owner.conn,
                    references: &references,
                },
            )?;
    Ok(replied.then_some((false, false)))
}

/// Notifies owner `o` about `m` (meant for them: `dm`, `mentioned`, else a reply), unless they are
/// there for it themselves.
fn alert(
    w: &mut steve_store::W<'_>,
    ws: &str,
    m: &Stored,
    o: &Owner,
    dm: bool,
    mentioned: bool,
    now: i64,
) -> ChannelResult<Alert> {
    if crate::presence::owner_engaged_chat(w, ws, &m.id, now)?.is_some() {
        return Ok(Alert::YouThere);
    }
    let repo::Display {
        sender,
        text,
        attachments,
        space,
    } = repo::display(w, &m.id)?.ok_or_else(|| steve_modules::ApiError::not_found("消息"))?;
    let text = crate::media::with_unread_note(&text, &attachments);
    let space = if space.trim().is_empty() {
        "群聊".to_string()
    } else {
        space
    };
    let body = body(&sender, dm, mentioned, &space, &text);
    let who = Caller {
        ws: ws.into(),
        me: o.user.clone(),
    };
    Ok(
        match crate::notifications::queue_starred(w, &who, &m.id, &body, now)? {
            crate::notifications::Queued::Yes { id, sends } => Alert::Notified { id, sends },
            crate::notifications::Queued::Again => Alert::Again,
            crate::notifications::Queued::Off => Alert::PolicyOff,
        },
    )
}

/// The chat of stored space `space` is set aside for the star in any connection's space for it
/// ([`set_aside`]): what you said about the chat holds whichever identity received the message.
pub fn set_aside_chat(
    r: &mut impl steve_store::Read,
    ws: &str,
    space: &str,
) -> ChannelResult<bool> {
    use steve_store::channel_spaces::rows;
    if set_aside(r, space)? {
        return Ok(true);
    }
    let Some(row) = rows::get_in_workspace(r, ws, space)? else {
        return Ok(false);
    };
    for other in rows::by_chat(r, ws, &row.external_id)? {
        if other.id != space && set_aside(r, &other.id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The user whose own account connection `conn` is (`Mode::Own`: its account's contact is a
/// user's), with that account; `None` for a bot's connection.
fn own_user(
    r: &mut impl steve_store::Read,
    ws: &str,
    conn: &str,
) -> ChannelResult<Option<(String, String)>> {
    let (account, mode) = crate::bindings::connection_account(r, ws, conn)?;
    if mode != Mode::Own {
        return Ok(None);
    }
    Ok(
        steve_store::connections::lifecycle::personal_user(r, ws, conn)?
            .map(|user| (user, account)),
    )
}

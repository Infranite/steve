//! The inbound pipeline `on_inbound(ws, conn, msg)`, steps 1–3 of channels-personas.md §2.3:
//!
//! 1. locate or create the space (a new one gets `space_settings(configured=0)`, its members, and —
//!    for a group a bot account just found itself in — the “新群” decision); `identify` the sender
//!    (`is_ours` = one of our own bots);
//! 2. store `chan_messages(direction=in)` and hand the event to S1 (`source=im`,
//!    `type=im.mention|im.dm|im.message|im.reply`, with `space_id` and `message_id`) so the signals
//!    page has it;
//! 3. loop guard: a message from the connection's own account is not processed (a self account's
//!    trigger words are C6's to check), one from another bot is only recorded unless the space has
//!    `collab`; more than 20 messages sent by Steve in a space within 10 minutes mute it and open a
//!    `fail` decision.
//!
//! What comes back ([`Inbound::next`]) tells steps 4–7 whether to evaluate the rules;
//! [`Channels::on_message`] runs them (C3):
//!
//! 4. your own account's trigger words (§2.7) come first (handed to the responder, C6);
//! 5. a space with a live `flow_id` belongs to that flow: its `message` trigger took the event in
//!    step 2, so nothing more happens here;
//! 6. otherwise the space's rules (or the account's) decide ([`crate::rules`]); muted, everything but
//!    `handoff` becomes `record`;
//! 7. the action is carried out without sending anything ([`crate::respond`]).
//!
//! A chat is one space per installation whichever identities see it (`space_identities`), so the
//! same message coming in through two connections — your self poller and a bot's long connection —
//! is stored once: the second copy only fills in what the first lacked and triggers nothing
//! (`duplicate`). Steve's own sends read back are echoes, not messages ([`messages::our_echo`]).

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_flow::{ActorRef, Ingested, RawEvent};
use steve_modules::ApiError;
use steve_modules::accounts::{ExtraHandle, Hints, Identified, identify};
use steve_store::base::{self, DecisionKind};
use steve_store::{NewDecision, Tx};

use crate::channel::{HandleRef, InboundMessage, MemberInfo, SpaceInfo, SpaceKind};
use crate::respond::{self, Plan, Response};
use crate::rules::{self, ContextLine, Facts, GatewayJudge};
use crate::spaces::{self, Attention, Mode, NewSpace};
use crate::{ChannelResult, Channels, messages};

/// `ext_events.source` of channel messages.
pub const SOURCE: &str = "im";
/// `outcome_label` of a message from the connection's own account.
pub const LABEL_OWN: &str = "自己发出的消息";
/// `outcome_label` of a message from another bot (no `collab`).
pub const LABEL_BOT: &str = "来自机器人，只记录";
/// `outcome_label` of a message stored before through another connection.
pub const LABEL_ELSEWHERE: &str = "已从另一个连接收到";
/// `outcome_label` of Steve's own outbound message read back in.
pub const LABEL_ECHO: &str = "Steve 发出的消息";
/// `outcome_label` of a message in a space set to 不提醒.
pub const LABEL_QUIET: &str = "不提醒，只记录";
/// What happened to a message in a space set to 屏蔽 (not stored).
pub const LABEL_BLOCKED: &str = "已屏蔽，不处理";
/// How long an outbound message without its provider id yet can come back as an echo.
pub const ECHO_WINDOW_MS: i64 = 10 * 60_000;
/// The loop guard: more than this many messages sent by Steve in one space …
pub const RATE_MAX: i64 = 20;
/// … within this window mutes the space.
pub const RATE_WINDOW_MS: i64 = 10 * 60_000;

/// What steps 4–7 do with the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Evaluate trigger words, flow and rules (C3).
    Handle,
    /// From the connection's own account: not processed, except a self account's trigger words
    /// (C6).
    Own,
    /// Only recorded, with this label (another bot, a duplicate, a dropped event).
    Record(&'static str),
}

/// What [`Channels::on_inbound`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound {
    /// `cm_…`
    pub message_id: String,
    /// `spc_…`
    pub space_id: String,
    /// The message was already there (redelivery, or seen through the other path, or Steve's own
    /// outbound message read back), or its space is blocked: nothing else happened.
    pub duplicate: bool,
    /// The space is set to 屏蔽: the message was not stored (`message_id` is empty).
    pub blocked: bool,
    /// The space was created by this message.
    pub space_created: bool,
    /// The “新群” decision opened for it.
    pub decision_id: Option<String>,
    pub sender_account_id: Option<String>,
    pub sender_party_id: Option<String>,
    /// The sender is the own organization's (one of our bots).
    pub sender_is_ours: bool,
    /// `im.dm | im.mention | im.reply | im.message`
    pub type_key: String,
    /// S1's answer (`None` for a duplicate or when S1 failed: the message is stored regardless).
    pub event: Option<Ingested>,
    pub next: Next,
    /// The loop guard muted the space just now.
    pub muted_now: bool,
    /// What the conditions of the rules look at (not for duplicates).
    pub facts: Option<Facts>,
    /// Steps 4–7 ([`Channels::on_message`]; `None` from [`Channels::on_inbound`] and for
    /// duplicates).
    pub response: Option<Response>,
}

/// The connection facts the pipeline needs.
#[derive(Debug, Clone)]
pub(crate) struct ConnFacts {
    pub adapter: String,
    pub account_id: String,
    pub created_by: String,
    pub account_name: String,
    /// The user whose “我” owns the account (a personal, `mode=self` connection).
    pub owner_user: Option<String>,
}

pub(crate) fn conn_facts(
    c: &rusqlite::Connection,
    ws: &str,
    conn: &str,
) -> ChannelResult<ConnFacts> {
    c.query_row(
        "SELECT c.adapter, c.account_id, c.created_by, coalesce(nullif(a.display_name, ''), a.handle_display), p.user_id
           FROM connections c JOIN accounts a ON a.id = c.account_id LEFT JOIN parties p ON p.id = a.party_id
          WHERE c.id = ? AND c.workspace_id = ?",
        params![conn, ws],
        |r| {
            Ok(ConnFacts {
                adapter: r.get(0)?,
                account_id: r.get(1)?,
                created_by: r.get(2)?,
                account_name: r.get(3)?,
                owner_user: r.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::not_found("连接"))
}

/// Identifies a handle in a savepoint: a failure leaves no half-created contact and does not stop
/// the message.
pub(crate) fn identify_handle(
    tx: &Tx,
    ws: &str,
    conn: &str,
    actor: &str,
    h: &HandleRef,
    name: Option<&str>,
    label: &str,
) -> Option<Identified> {
    identify_handle_as(tx, ws, conn, actor, h, name, label, false)
}

/// [`identify_handle`] for a sender or member that may be a bot: a new bot becomes a bot party with
/// a `bot` account, not an unconfirmed person (ownership.md §3).
#[allow(clippy::too_many_arguments)]
pub(crate) fn identify_handle_as(
    tx: &Tx,
    ws: &str,
    conn: &str,
    actor: &str,
    h: &HandleRef,
    name: Option<&str>,
    label: &str,
    bot: bool,
) -> Option<Identified> {
    if h.handle.trim().is_empty() {
        return None;
    }
    if tx.execute_batch("SAVEPOINT chan_identify").is_err() {
        return None;
    }
    let hints = Hints {
        display_name: name.map(String::from),
        extra: h
            .extra
            .iter()
            .map(|x| ExtraHandle {
                handle: x.clone(),
                connection_id: Some(conn.into()),
            })
            .collect(),
        source: Some("channel".into()),
        source_label: Some(label.chars().take(60).collect()),
        kind: bot.then(|| "bot".to_string()),
        ..Hints::default()
    };
    match identify(tx, ws, actor, &h.system, &h.handle, &hints) {
        Ok(id) => {
            let _ = tx.execute_batch("RELEASE chan_identify");
            Some(id)
        }
        Err(e) => {
            tracing::warn!(
                ws,
                system = h.system,
                "channel inbound: not identified: {e}"
            );
            let _ = tx.execute_batch("ROLLBACK TO chan_identify; RELEASE chan_identify");
            None
        }
    }
}

/// Is any of these handles known already (so the sender's name need not be looked up)?
fn handle_known(c: &rusqlite::Connection, ws: &str, h: &HandleRef) -> ChannelResult<bool> {
    for x in std::iter::once(&h.handle).chain(h.extra.iter()) {
        let hit: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM account_handles WHERE workspace_id = ? AND handle = ?)",
            params![ws, x.trim()],
            |r| r.get(0),
        )?;
        if hit {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The fallback when the adapter cannot describe the space.
fn info_from(msg: &InboundMessage) -> SpaceInfo {
    let kind = msg.space_kind.unwrap_or(SpaceKind::Group);
    let name = msg
        .space_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_default();
    SpaceInfo {
        name,
        kind,
        member_count: 0,
        has_external: msg.sender.external,
    }
}

fn summary(sender: &str, msg: &InboundMessage) -> String {
    let body = if msg.text.trim().is_empty() {
        match msg.attachments.first().and_then(|a| a["type"].as_str()) {
            Some("image") => "[图片]".to_string(),
            Some(_) => "[文件]".to_string(),
            None => "[消息]".to_string(),
        }
    } else {
        msg.text.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    if sender.is_empty() {
        body
    } else {
        format!("{sender}：{body}")
    }
}

/// The transaction's result.
struct Written {
    inbound: Inbound,
    sender_name: String,
    space_ext: String,
}

impl Channels {
    /// Steps 1–3 for one message received through connection `conn` of `ws`.
    pub async fn on_inbound(
        &self,
        ws: &str,
        conn: &str,
        msg: InboundMessage,
    ) -> ChannelResult<Inbound> {
        let mut c = steve_modules::Check::new();
        c.len("space_ext_id", &msg.space_ext_id, 1, 500).len(
            "external_id",
            &msg.external_id,
            1,
            500,
        );
        c.done()?;
        let store = self.store().clone();
        let facts = store.reader(|c| conn_facts(c, ws, conn))?;
        let (existing, known) = store.reader(|c| -> ChannelResult<_> {
            Ok((
                spaces::find(c, ws, conn, &msg.space_ext_id)?,
                handle_known(c, ws, &msg.sender.handle)?,
            ))
        })?;

        // 屏蔽: nothing is processed or stored, not even the sender
        if let Some(sp) = &existing
            && store
                .reader(|c| spaces::settings(c, &sp.id))
                .is_ok_and(|s| s.attention == Attention::Blocked)
        {
            return Ok(Inbound {
                message_id: String::new(),
                space_id: sp.id.clone(),
                duplicate: true,
                blocked: true,
                space_created: false,
                decision_id: None,
                sender_account_id: None,
                sender_party_id: None,
                sender_is_ours: false,
                type_key: String::new(),
                event: None,
                next: Next::Record(LABEL_BLOCKED),
                muted_now: false,
                facts: None,
                response: None,
            });
        }

        // What the adapter can tell about a new space and its members (best effort: a failure
        // falls back to what the payload says).
        let adapter = self.adapter(&facts.adapter);
        let cx = adapter
            .as_ref()
            .and_then(|_| match self.connections().cx(ws, conn) {
                Ok(cx) => Some(cx),
                Err(e) => {
                    tracing::warn!(conn, "channel inbound: no connection context: {e}");
                    None
                }
            });
        let mut info: Option<SpaceInfo> = None;
        let mut members: Vec<MemberInfo> = Vec::new();
        let need_members = existing.is_none() || (msg.sender.display_name.is_none() && !known);
        if let (Some(a), Some(cx)) = (&adapter, &cx) {
            if existing.is_none() {
                match a.space_info(cx, &msg.space_ext_id).await {
                    Ok(i) => info = Some(i),
                    Err(e) => tracing::warn!(conn, "channel inbound: space_info failed: {e}"),
                }
            }
            if need_members && a.features().members {
                match a.members(cx, &msg.space_ext_id).await {
                    Ok(m) => members = m,
                    Err(e) => tracing::warn!(conn, "channel inbound: members failed: {e}"),
                }
            }
        }
        let mut info = info.unwrap_or_else(|| info_from(&msg));
        info.has_external |= msg.sender.external;
        let sender_name = msg.sender.display_name.clone().or_else(|| {
            members
                .iter()
                .find(|m| {
                    m.handle.system == msg.sender.handle.system
                        && (m.handle.is(&msg.sender.handle.handle)
                            || msg.sender.handle.extra.iter().any(|x| m.handle.is(x)))
                })
                .map(|m| m.name.clone())
        });
        if info.kind == SpaceKind::Dm && info.name.trim().is_empty() {
            info.name = sender_name.clone().unwrap_or_else(|| "私聊".into());
        }
        if info.member_count == 0 && !members.is_empty() {
            info.member_count = members.len() as i64;
        }

        let now = self.now();
        let written = store.transaction(|tx| {
            self.write(
                tx,
                ws,
                conn,
                &facts,
                &msg,
                &info,
                &members,
                sender_name.as_deref(),
                now,
            )
        })?;
        let mut inbound = written.inbound;
        if inbound.duplicate {
            return Ok(inbound);
        }
        // a sender known only by an id: the name job looks them up soon (crate::names)
        if steve_modules::accounts::looks_like_handle(&written.sender_name) && !msg.sender.is_bot {
            self.kick_names(ws, false);
        }

        // What the rules look at; the event carries it too, for flows converted from rules.
        let rf = store.reader(|c| rule_facts(c, ws, &facts, &inbound, &msg))?;

        // Step 2, second half: the signal.
        let mut ev = RawEvent::new(
            SOURCE,
            inbound.type_key.clone(),
            msg.external_id.clone(),
            msg.created,
            summary(&written.sender_name, &msg),
        );
        ev.also = also(&inbound.type_key, &msg);
        ev.thread_key = msg.thread_key.clone();
        ev.group_key = Some(inbound.space_id.clone());
        ev.scope_ext_id = Some(written.space_ext);
        ev.space_id = Some(inbound.space_id.clone());
        ev.message_id = Some(inbound.message_id.clone());
        ev.actor = match (&inbound.sender_account_id, &inbound.sender_party_id) {
            (Some(a), Some(p)) => Some(ActorRef::Known {
                account_id: a.clone(),
                party_id: p.clone(),
            }),
            _ => None,
        };
        ev.payload = json!({
            "text": msg.text,
            "attachments": msg.attachments,
            "via": msg.via,
            "provider_event_id": msg.event_id,
            "raw": msg.raw,
            "facts": rf.to_value(),
        });
        inbound.facts = Some(rf);
        ev.record_only = match &inbound.next {
            Next::Handle => None,
            Next::Own => Some(LABEL_OWN.into()),
            Next::Record(l) => Some((*l).into()),
        };
        match self.sink().ingest(ws, Some(conn), ev).await {
            Ok(r) => {
                if let Some(id) = r.id() {
                    let (mid, id) = (inbound.message_id.clone(), id.to_string());
                    store.transaction(|tx| messages::set_event(tx, &mid, &id))?;
                }
                if matches!(r, Ingested::Dropped(_)) && inbound.next == Next::Handle {
                    // the source is paused or the type is off: keep the message, do not act on it
                    inbound.next = Next::Record("事件源已暂停或类型已关闭");
                }
                inbound.event = Some(r);
            }
            Err(e) => tracing::warn!(conn, "channel inbound: event not ingested: {e}"),
        }
        // 不提醒: one line in the next report while there is news here (never 待处理)
        if inbound.next == Next::Record(LABEL_QUIET) {
            let event = inbound
                .event
                .as_ref()
                .and_then(|e| e.id().map(String::from));
            let (sid, now) = (inbound.space_id.clone(), self.now());
            let r = store.transaction(|tx| -> ChannelResult<()> {
                let label = spaces::get(tx, &sid)?
                    .map(|s| respond::space_label(&s))
                    .unwrap_or_default();
                let text = format!("「{label}」有新消息（不提醒，只记录）");
                let open: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM digest_entries WHERE workspace_id = ? AND taken IS NULL AND text = ?)",
                    params![ws, text],
                    |r| r.get(0),
                )?;
                if !open {
                    steve_flow::digest::add(tx, ws, &text, event.as_deref(), None, now)
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                }
                Ok(())
            });
            if let Err(e) = r {
                tracing::warn!(conn, "channel inbound: digest not noted: {e}");
            }
        }
        Ok(inbound)
    }

    #[allow(clippy::too_many_arguments)]
    fn write(
        &self,
        tx: &Tx,
        ws: &str,
        conn: &str,
        facts: &ConnFacts,
        msg: &InboundMessage,
        info: &SpaceInfo,
        members: &[MemberInfo],
        sender_name: Option<&str>,
        now: i64,
    ) -> ChannelResult<Written> {
        let mode = match spaces::channel_settings(tx, ws, conn)? {
            Some(s) => s.mode,
            None if facts.owner_user.is_some() => Mode::Own,
            None => Mode::Bot,
        };
        spaces::ensure_channel_settings(tx, ws, conn, mode, now)?;
        let system = facts_system(tx, &facts.account_id)?;

        // 1. the space
        let (space, created) = spaces::locate_or_create(
            tx,
            &NewSpace {
                ws,
                system_id: &system,
                connection_id: conn,
                kind: info.kind,
                external_id: &msg.space_ext_id,
                name: &info.name,
                member_count: info.member_count,
                has_external: info.has_external,
                now,
            },
        )?;
        let label = if space.name.is_empty() {
            "渠道消息".to_string()
        } else {
            space.name.clone()
        };
        let actor = facts.owner_user.as_deref().unwrap_or(&facts.created_by);
        let mut decision_id = None;
        if created {
            spaces::add_member(tx, ws, &space.id, &facts.account_id, "bot", now)?;
            for m in members {
                if let Some(id) = identify_handle_as(
                    tx,
                    ws,
                    conn,
                    actor,
                    &m.handle,
                    Some(&m.name),
                    &label,
                    m.role == "bot",
                ) && id.account_id != facts.account_id
                {
                    spaces::add_member(tx, ws, &space.id, &id.account_id, &m.role, now)?;
                }
            }
            if mode == Mode::Bot && info.kind.is_group() {
                decision_id = Some(spaces::open_new_space_decision(
                    tx,
                    ws,
                    &space,
                    &facts.account_name,
                )?);
            }
        }

        // 1. the sender
        let sender = identify_handle_as(
            tx,
            ws,
            conn,
            actor,
            &msg.sender.handle,
            sender_name,
            &label,
            msg.sender.is_bot,
        );
        if let Some(s) = &sender
            && !spaces::is_member(tx, &space.id, &s.account_id)?
        {
            let role = if msg.sender.is_bot { "bot" } else { "member" };
            if spaces::add_member(tx, ws, &space.id, &s.account_id, role, now)? {
                spaces::event(tx, ws, &space.id);
            }
        }
        let (sender_account, sender_party) = match &sender {
            Some(s) => (Some(s.account_id.clone()), Some(s.party_id.clone())),
            None => (None, None),
        };
        let sender_display: String = match &sender_account {
            Some(a) => tx
                .query_row(
                    "SELECT coalesce(nullif(display_name, ''), handle_display) FROM accounts WHERE id = ?",
                    [a],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or_default(),
            None => sender_name.unwrap_or("").to_string(),
        };

        // a DM stored before anyone could be named for it is named after the other party
        let from_other = sender_account.as_deref() != Some(facts.account_id.as_str());
        if space.kind == SpaceKind::Dm.code()
            && (space.name.trim().is_empty() || space.name == spaces::DM_FALLBACK)
            && from_other
            && !sender_display.trim().is_empty()
        {
            tx.execute(
                "UPDATE spaces SET name = ? WHERE id = ?",
                params![sender_display.trim(), space.id],
            )?;
            spaces::event(tx, ws, &space.id);
        }

        // mentions → account ids (the connection's own account for “@机器人” / “@你”)
        let mut mentions = Vec::new();
        for m in &msg.mentions {
            let account = if m.is_connection {
                Some(facts.account_id.clone())
            } else {
                m.handle.as_ref().and_then(|h| {
                    identify_handle(tx, ws, conn, actor, h, Some(&m.name), &label)
                        .map(|i| i.account_id)
                })
            };
            mentions.push(json!({ "account_id": account, "name": m.name }));
        }

        // 2. the message (a redelivery stops here, and so does one stored through the other
        // connection to the same chat: it only fills in fields)
        let first = match messages::by_external(tx, &space.id, &msg.external_id)? {
            Some(id) => {
                messages::complete(tx, &id, msg.thread_key.as_deref(), msg.reply_to.as_deref())?;
                Some((id, space.id.clone(), "重复的消息"))
            }
            None => match messages::seen_elsewhere(
                tx,
                ws,
                conn,
                mode == Mode::Own,
                &msg.space_ext_id,
                &msg.external_id,
            )? {
                Some((id, sp)) => {
                    messages::complete_from(
                        tx,
                        &id,
                        msg.thread_key.as_deref(),
                        msg.reply_to.as_deref(),
                        sender_account.as_deref(),
                        sender_party.as_deref(),
                    )?;
                    Some((id, sp, LABEL_ELSEWHERE))
                }
                None => None,
            },
        };
        // Steve's own message read back (sent as you, or by our bot): not a new message. Only
        // what one of Steve's identities could have sent is compared.
        let first = match first {
            Some(f) => Some(f),
            None if !from_other
                || sender.as_ref().is_some_and(|s| s.is_ours)
                || crate::arbiter::from_you(
                    tx,
                    ws,
                    sender_account.as_deref(),
                    sender_party.as_deref(),
                )? =>
            {
                match messages::our_echo(
                    tx,
                    ws,
                    &msg.space_ext_id,
                    &msg.external_id,
                    &msg.text,
                    now - ECHO_WINDOW_MS,
                )? {
                    Some((id, sp)) => {
                        if sp == space.id {
                            tx.execute(
                                "UPDATE chan_messages SET external_id = coalesce(external_id, ?) WHERE id = ?",
                                params![msg.external_id, id],
                            )?;
                        }
                        Some((id, sp, LABEL_ECHO))
                    }
                    None => None,
                }
            }
            None => None,
        };
        if let Some((id, space_id, label)) = first {
            return Ok(Written {
                inbound: Inbound {
                    message_id: id,
                    space_id,
                    duplicate: true,
                    blocked: false,
                    space_created: created,
                    decision_id,
                    sender_account_id: sender_account,
                    sender_party_id: sender_party,
                    sender_is_ours: sender.as_ref().is_some_and(|s| s.is_ours),
                    type_key: String::new(),
                    event: None,
                    next: Next::Record(label),
                    muted_now: false,
                    facts: None,
                    response: None,
                },
                sender_name: sender_display,
                space_ext: space.external_id,
            });
        }
        let mentions = Value::Array(mentions);
        let attachments = Value::Array(msg.attachments.clone());
        let Some(message_id) = messages::insert_inbound(
            tx,
            &messages::NewInbound {
                ws,
                space_id: &space.id,
                external_id: &msg.external_id,
                thread_key: msg.thread_key.as_deref(),
                reply_to: msg.reply_to.as_deref(),
                sender_account_id: sender_account.as_deref(),
                sender_party_id: sender_party.as_deref(),
                text: &msg.text,
                attachments: &attachments,
                mentions: &mentions,
                created: msg.created,
            },
        )?
        else {
            return Err(ApiError::conflict("这条消息已经记下了"));
        };
        spaces::touch(tx, &space.id, msg.created.max(now), msg.sender.external)?;

        let kind = SpaceKind::parse(&space.kind).unwrap_or(info.kind);
        let reply_exts: Vec<&str> = [msg.reply_to.as_deref(), msg.root_id.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let type_key = if kind == SpaceKind::Dm {
            "im.dm"
        } else if msg.mentions_connection() {
            "im.mention"
        } else if !reply_exts.is_empty() && messages::is_ours_out(tx, &space.id, &reply_exts)? {
            "im.reply"
        } else {
            "im.message"
        };

        // 3. the loop guard
        let set = spaces::settings(tx, &space.id)?;
        let sender_is_ours = sender.as_ref().is_some_and(|s| s.is_ours);
        let own = sender_account.as_deref() == Some(facts.account_id.as_str());
        let next = if own {
            // your own words still count in 不提醒 (trigger words are your explicit command)
            Next::Own
        } else if set.attention >= Attention::Quiet {
            Next::Record(LABEL_QUIET)
        } else if (msg.sender.is_bot || sender_is_ours) && !set.collab {
            Next::Record(LABEL_BOT)
        } else {
            Next::Handle
        };
        // another assistant's automatic reply on your account: recorded, never an instruction
        let next = match next {
            Next::Handle | Next::Own
                if crate::arbiter::looks_automated(&msg.text)
                    && (own
                        || sender_account
                            .as_deref()
                            .map(|a| crate::arbiter::is_yours(tx, ws, a))
                            .transpose()?
                            .unwrap_or(false)) =>
            {
                Next::Record(crate::arbiter::LABEL_AUTOMATED)
            }
            n => n,
        };
        let mut muted_now = false;
        if !set.muted && messages::sent_since(tx, &space.id, now - RATE_WINDOW_MS)? > RATE_MAX {
            muted_now = spaces::mute(tx, ws, &space.id, now)?;
            if muted_now {
                base::decisions::open(
                    tx,
                    &NewDecision {
                        workspace_id: ws.into(),
                        kind: DecisionKind::Fail.code().into(),
                        subject_type: spaces::SUBJECT_SPACE.into(),
                        subject_id: space.id.clone(),
                        item_id: None,
                        session_id: None,
                        question: format!("{label} 里回复太多，已静默"),
                        draft: None,
                        options: None,
                        requested_by: "steve".into(),
                    },
                )?;
            }
        }
        Ok(Written {
            inbound: Inbound {
                message_id,
                space_id: space.id,
                duplicate: false,
                blocked: false,
                space_created: created,
                decision_id,
                sender_account_id: sender_account,
                sender_party_id: sender_party,
                sender_is_ours,
                type_key: type_key.into(),
                event: None,
                next,
                muted_now,
                facts: None,
                response: None,
            },
            sender_name: sender_display,
            space_ext: space.external_id,
        })
    }
}

/// `systems.id` of the connection's account (the space's `system_id`).
pub(crate) fn facts_system(tx: &Tx, account: &str) -> ChannelResult<String> {
    Ok(tx.query_row(
        "SELECT system_id FROM accounts WHERE id = ?",
        [account],
        |r| r.get(0),
    )?)
}

/// The other types that hold besides the main one (a trigger on `im.mention` also takes a mention
/// that replies to Steve).
fn also(main: &str, msg: &InboundMessage) -> Vec<String> {
    let mut out = Vec::new();
    if main != "im.mention" && msg.mentions_connection() {
        out.push("im.mention".to_string());
    }
    if main != "im.message" && main != "im.dm" {
        out.push("im.message".to_string());
    }
    out
}

/// The facts the rules' conditions look at (after step 1–2's writes).
fn rule_facts(
    c: &rusqlite::Connection,
    ws: &str,
    conn: &ConnFacts,
    inbound: &Inbound,
    msg: &InboundMessage,
) -> ChannelResult<Facts> {
    let space = spaces::get(c, &inbound.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
    let kind = SpaceKind::parse(&space.kind).unwrap_or(SpaceKind::Group);
    let exts: Vec<&str> = [msg.reply_to.as_deref(), msg.root_id.as_deref()]
        .into_iter()
        .flatten()
        .filter(|e| *e != msg.external_id)
        .collect();
    let reply_to_me = !exts.is_empty()
        && (messages::is_ours_out(c, &space.id, &exts)?
            || messages::is_by(c, &space.id, &exts, &conn.account_id)?);
    let (confirmed, teams) = match &inbound.sender_party_id {
        Some(p) => {
            let kind: Option<String> = c
                .query_row("SELECT contact_kind FROM parties WHERE id = ?", [p], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            let mut st = c.prepare_cached(
                "SELECT m.team_id FROM team_members m JOIN parties t ON t.id = m.team_id AND t.status = 'active'
                  WHERE m.workspace_id = ? AND m.contact_id = ? ORDER BY m.team_id",
            )?;
            let teams = st
                .query_map(params![ws, p], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            (kind.as_deref() != Some("unconfirmed"), teams)
        }
        None => (false, Vec::new()),
    };
    let mut f = Facts {
        dm: kind == SpaceKind::Dm,
        group: kind.is_group(),
        mentioned: msg.mentions_connection(),
        reply_to_me,
        text: msg.text.clone(),
        sender_party: inbound.sender_party_id.clone(),
        sender_confirmed: confirmed,
        sender_teams: teams,
        sender_external: msg.sender.external,
        weekday: 0,
        minute: 0,
    };
    let tz = steve_flow::schedules::workspace_tz(c, ws)?;
    f.set_time(msg.created, &tz);
    Ok(f)
}

/// The inbound message as steps 4–7 need it.
struct Stored {
    external_id: String,
    thread_key: Option<String>,
    text: String,
    sender_name: String,
    event_id: Option<String>,
    /// The names of the people mentioned (leading mentions are not part of trigger words).
    mention_names: Vec<String>,
    /// `chan_messages.attachments` as stored.
    attachments: Value,
}

fn stored(c: &rusqlite::Connection, id: &str) -> ChannelResult<Stored> {
    c.query_row(
        "SELECT m.external_id, m.thread_key, coalesce(m.text, ''), coalesce(nullif(a.display_name, ''), a.handle_display, p.name, ''), m.event_id, m.mentions, m.attachments
           FROM chan_messages m LEFT JOIN accounts a ON a.id = m.sender_account_id LEFT JOIN parties p ON p.id = m.sender_party_id
          WHERE m.id = ?",
        [id],
        |r| {
            Ok(Stored {
                external_id: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                thread_key: r.get(1)?,
                text: r.get(2)?,
                sender_name: r.get(3)?,
                event_id: r.get(4)?,
                mention_names: r
                    .get::<_, Option<String>>(5)?
                    .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect(),
                attachments: r
                    .get::<_, Option<String>>(6)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Null),
            })
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::not_found("消息"))
}

/// Is the space's flow there to take its messages (not deleted, enabled)?
fn live_flow(c: &rusqlite::Connection, ws: &str, id: &str) -> ChannelResult<bool> {
    Ok(steve_flow::flows::get(c, id)?
        .is_some_and(|f| f.workspace_id == ws && !f.deleted && f.enabled))
}

impl Channels {
    /// The whole inbound pipeline (§2.3): steps 1–3 ([`Channels::on_inbound`]), then 4–7. A
    /// duplicate (also one seen through the other connection) stops after step 2.
    pub async fn on_message(
        &self,
        ws: &str,
        conn: &str,
        msg: InboundMessage,
    ) -> ChannelResult<Inbound> {
        let mut inbound = self.on_inbound(ws, conn, msg).await?;
        if inbound.duplicate {
            return Ok(inbound);
        }
        let r = self.respond(ws, conn, &inbound).await?;
        inbound.response = Some(r);
        Ok(inbound)
    }

    /// Steps 4–7 for a message steps 1–3 stored. Sends nothing. The space's voice acts
    /// (domain-model.md §1.4, [`crate::arbiter`]): whichever identity received the message, only the
    /// voice's mode and rules count and the voice answers; your `/bot` goes to the voice, or, with no
    /// voice, to your web conversation. Precedence 屏蔽 (stopped in step 1) > `/bot` > 不提醒 > role
    /// rules.
    pub async fn respond(
        &self,
        ws: &str,
        conn: &str,
        inbound: &Inbound,
    ) -> ChannelResult<Response> {
        let label = match &inbound.next {
            Next::Handle | Next::Own => None,
            Next::Record(l) => Some(*l),
        };
        // only 不提醒 still listens, for your `/bot`
        if let Some(l) = label
            && l != LABEL_QUIET
        {
            return Ok(Response::skipped(l));
        }
        let store = self.store().clone();
        let (cs, space, set, m, voice, cmd, voice_account) =
            store.reader(|c| -> ChannelResult<_> {
                let space = spaces::get(c, &inbound.space_id)?
                    .ok_or_else(|| ApiError::not_found("空间"))?;
                let voice = spaces::voice(c, &space.id)?;
                let acting = voice.as_deref().unwrap_or(conn);
                let (account, mode) = crate::bindings::connection_account(c, ws, acting)?;
                let cs = spaces::channel_settings(c, ws, acting)?;
                let set = spaces::settings(c, &space.id)?;
                let m = stored(c, &inbound.message_id)?;
                let cmd = crate::arbiter::command(
                    c,
                    ws,
                    &space,
                    inbound.sender_account_id.as_deref(),
                    inbound.sender_party_id.as_deref(),
                    &m.text,
                    &m.mention_names,
                )?;
                Ok((
                    cs.map(|s| (s.mode, s.rules)).unwrap_or((mode, json!([]))),
                    space,
                    set,
                    m,
                    voice,
                    cmd,
                    account,
                ))
            })?;
        let (mode, account_rules) = cs;
        let mut facts = inbound.facts.clone().unwrap_or_default();
        let mut plan = Plan {
            ws: ws.into(),
            connection_id: voice.clone().unwrap_or_else(|| conn.to_string()),
            mode,
            space_id: space.id.clone(),
            space_name: respond::space_label(&space),
            message_id: inbound.message_id.clone(),
            external_id: m.external_id.clone(),
            thread_key: m.thread_key.clone(),
            // what Steve cannot read of it is said, never left out (`[图片：未读取内容]`)
            text: crate::media::with_unread_note(&m.text, &m.attachments),
            sender_account_id: inbound.sender_account_id.clone(),
            sender_party_id: inbound.sender_party_id.clone(),
            sender_name: m.sender_name.clone(),
            act: rules::Act::Record,
            rule: None,
            muted: set.muted,
            trigger: None,
        };

        // 4. your `/bot`, wherever it came in: the voice answers, else your web conversation
        if let Some(instruction) = cmd {
            if voice.is_none() {
                let mut r = Response::skipped("你的指令，没有发言身份，转到网页对话");
                r.by = respond::By::Trigger {
                    instruction: instruction.clone(),
                };
                if let Err(e) = self.converse_web(&plan, &instruction).await {
                    tracing::warn!(space = space.id, "command to the web conversation: {e}");
                }
                return Ok(r);
            }
            plan.act = rules::Act::Reply;
            plan.trigger = Some(instruction);
            return self.execute(&plan).await;
        }
        if let Some(l) = label {
            return Ok(Response::skipped(l));
        }
        if inbound.next == Next::Own {
            return Ok(Response::skipped(LABEL_OWN));
        }
        // nobody speaks here: recorded only
        let Some(v) = voice else {
            return Ok(Response::skipped(crate::arbiter::LABEL_OBSERVE));
        };
        if v != conn {
            // the conditions as the voice sees the message (another identity received it)
            facts.mentioned =
                store.reader(|c| mentions_account(c, &inbound.message_id, &voice_account))?;
        }

        // 5. the space's flow
        if let Some(flow_id) = &set.flow_id
            && store.reader(|c| live_flow(c, ws, flow_id))?
        {
            let mut r = Response::skipped("由流程处理");
            r.by = respond::By::Flow {
                flow_id: flow_id.clone(),
            };
            return Ok(r);
        }

        // 6. the rules: the space's, else the voice's account's
        let rules = rules::load(set.rules.as_ref().unwrap_or(&account_rules), mode);
        let context: Vec<ContextLine> = store
            .reader(|c| {
                messages::context_before(c, &space.id, &inbound.message_id, rules::INTENT_CONTEXT)
            })?
            .into_iter()
            .map(|(who, text)| ContextLine { who, text })
            .collect();
        let judge = GatewayJudge {
            gateway: self.modules().gateway.as_ref(),
            guard: self.guard(),
            message: &m.text,
            sender: if m.sender_name.is_empty() {
                "某人"
            } else {
                &m.sender_name
            },
            space: &plan.space_name,
            context: &context,
        };
        let verdict = rules::evaluate(&rules, &facts, set.muted, &judge).await;
        plan.act = verdict.act;
        plan.rule = verdict.rule;

        // 7. carry it out (never sends: C6 does, after approval)
        let mut r = self.execute(&plan).await?;
        r.muted = verdict.muted;
        if let Some(e) = &m.event_id {
            store.transaction(|tx| respond::mark_event(tx, e, &r))?;
        }
        tracing::debug!(space = space.id, act = r.act.code(), rule = ?r.rule, "channel rules");
        Ok(r)
    }
}

/// Does inbound message `id` @-mention `account`?
fn mentions_account(c: &rusqlite::Connection, id: &str, account: &str) -> ChannelResult<bool> {
    let raw: String = c.query_row(
        "SELECT mentions FROM chan_messages WHERE id = ?",
        [id],
        |r| r.get(0),
    )?;
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    Ok(v.as_array()
        .is_some_and(|a| a.iter().any(|m| m["account_id"].as_str() == Some(account))))
}

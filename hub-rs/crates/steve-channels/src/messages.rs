//! Channel messages (`chan_messages`): storing inbound ones, the
//! `ChanMessage` shape, paging, the `channels.retention` job (text → NULL after `keepRaw` days) and
//! the contact page's “往来” ([`MessageTimeline`]).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use steve_modules::contacts::{TimelineEntry, TimelineSource};
use steve_modules::{ApiResult, Modules};
use steve_store::base::Viewer;
use steve_store::channel_message_repo::projection::{self, Before};
use steve_store::channel_message_repo::retention as stored_retention;
use steve_store::{Read, Store};

use crate::ChannelResult;

/// `GET /spaces/:id` shows this many recent messages.
pub const RECENT: usize = 30;
/// `GET /spaces/:id/messages`: default and maximum page size.
pub const PAGE: usize = 50;
pub const PAGE_MAX: usize = 100;
/// The timeline line shows this many characters of a message.
const TIMELINE_CUT: usize = 60;

/// Steve's own outbound message coming back in (the self poller reads what Steve sent as you or as
/// the bot; a long connection sees what Steve sent as you): an outbound message of the same chat with
/// this provider id, or, when the send has not written its provider id back yet, a pending/sent one
/// from the last `window_ms` with the same text (compared without Steve's `[AI]` / proxy marks and
/// a trailing signature, [`crate::arbiter::same_text`]). `(message, space)`.
pub(crate) fn our_echo(
    r: &mut impl Read,
    ws: &str,
    space_ext: &str,
    ext: &str,
    text: &str,
    since: i64,
) -> ChannelResult<Option<(String, String)>> {
    use steve_store::channel_message_repo::ingress;

    let by_id = ingress::sent_with_id(r, ws, space_ext, ext)?;
    if by_id.is_some() || crate::arbiter::echo_key(text).is_empty() {
        return Ok(by_id);
    }
    Ok(ingress::possible_echoes(r, ws, space_ext, since)?
        .into_iter()
        .find(|candidate| crate::arbiter::same_text(&candidate.text, text))
        .map(|candidate| (candidate.id, candidate.space)))
}

/// The `n` messages before `id` in its space, oldest first: `(sender name, text)` (purged ones
/// left out). The `intent` judgement's context.
pub fn context_before(
    r: &mut impl Read,
    space: &str,
    id: &str,
    n: usize,
) -> ChannelResult<Vec<(String, String)>> {
    let lines = steve_store::channel_message_repo::transcript::before(r, space, id, n as u64)?;
    named_lines(r, lines)
}

/// The text of the message this composer turn is answering.
pub fn replied_to(
    r: &mut impl Read,
    space: &str,
    id: &str,
) -> ChannelResult<Option<(String, String)>> {
    let Some(line) = steve_store::channel_message_repo::transcript::quoted(r, space, id)? else {
        return Ok(None);
    };
    Ok(named_lines(r, vec![line])?.pop())
}

fn named_lines(
    r: &mut impl Read,
    lines: Vec<steve_store::channel_message_repo::transcript::Line>,
) -> ChannelResult<Vec<(String, String)>> {
    let accounts: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.account.as_deref())
        .collect();
    let parties: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.party.as_deref())
        .collect();
    let accounts: HashMap<_, _> = steve_store::accounts::names_and_handles(r, &accounts)?
        .into_iter()
        .collect();
    let parties: HashMap<_, _> = steve_store::parties::names(r, &parties)?
        .into_iter()
        .collect();
    Ok(lines
        .into_iter()
        .map(|line| {
            let name = line
                .account
                .as_deref()
                .and_then(|id| accounts.get(id))
                .or_else(|| line.party.as_deref().and_then(|id| parties.get(id)))
                .cloned()
                .unwrap_or_default();
            (name, line.text)
        })
        .collect())
}

// ---------------------------------------------------------------- shape

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SenderOut {
    pub name: String,
    pub party_id: Option<String>,
    pub account_id: Option<String>,
}

/// The `ChanMessage` shape. `basis` is `null` for inbound messages and messages without one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChanMessage {
    pub id: String,
    pub space_id: String,
    pub direction: String,
    pub sender: SenderOut,
    pub persona_id: Option<String>,
    pub on_behalf: String,
    /// `null` once the retention period is over (“原文已过保留期限”).
    pub text: Option<String>,
    pub attachments: Value,
    pub mentions: Value,
    pub status: String,
    pub thread_key: Option<String>,
    pub reply_to: Option<String>,
    pub event_id: Option<String>,
    pub work_item_id: Option<String>,
    pub decision_id: Option<String>,
    pub created: i64,
    pub sent: Option<i64>,
    pub purged: Option<i64>,
    pub basis: Value,
    /// An inbound message only recorded as history: `before` (written before the connection was on)
    /// or `late` (arrived too late); `null` for everything else.
    pub history: Option<String>,
    /// The link that opens it (or, where its platform has no link to one message, its chat) in its
    /// platform's own client ([`crate::open_link`]); `null` when there is none.
    pub open_url: Option<String>,
}

fn projected(r: &mut impl Read, row: projection::Message) -> ChannelResult<ChanMessage> {
    let mut m = ChanMessage {
        id: row.id,
        space_id: row.space_id,
        direction: row.direction,
        sender: SenderOut {
            name: row.sender_name,
            party_id: row.sender_party_id,
            account_id: row.sender_account_id,
        },
        persona_id: row.persona_id,
        on_behalf: row.on_behalf,
        text: row.text,
        attachments: row.attachments,
        mentions: row.mentions,
        status: row.status,
        thread_key: row.thread_key,
        reply_to: crate::fallback::platform(row.reply_to),
        event_id: row.event_id,
        work_item_id: row.work_item_id,
        decision_id: row.decision_id,
        created: row.created,
        sent: row.sent,
        purged: row.purged,
        basis: Value::Null,
        history: row.history,
        open_url: None,
    };
    with_basis(r, &mut m)?;
    Ok(m)
}

pub fn get(store: &Store, id: &str) -> ChannelResult<Option<ChanMessage>> {
    store.read(|r| {
        projection::get(r, id)?
            .map(|row| projected(r, row))
            .transpose()
    })
}

/// Stored message `id` as an adapter needs it to act on it again (recall, reactions), for
/// connection `conn`: the delivery id is the one that connection knows it by.
pub(crate) fn message_ref(
    r: &mut impl Read,
    id: &str,
    conn: &str,
) -> ChannelResult<Option<crate::channel::MessageRef>> {
    use steve_store::{accounts, channel_message_repo::provider, channel_spaces};

    let Some(m) = provider::message(r, id)? else {
        return Ok(None);
    };
    let Some(chat) = channel_spaces::platform_chat(r, &m.space)? else {
        return Ok(None);
    };
    let sender = match m.sender_account {
        Some(account) => accounts::system_handle(r, &account)?
            .map(|(system, handle)| crate::channel::HandleRef::new(system, handle)),
        None => None,
    };
    Ok(Some(crate::channel::MessageRef {
        space_ext_id: chat.external_id,
        ext_id: crate::fallback::platform(m.external_id),
        created: m.created,
        created_exact: m.created_exact,
        sender,
        text: m.text,
        thread_key: m.thread_key,
        source_id: provider::source_id(r, id, conn)?,
    }))
}

/// The outbound message's basis (C5), for the interfaces only you see; attachments in their public
/// form (no provider keys; `url` or `unavailable`) and an old card / post read again from its raw
/// event ([`crate::media::present`]); its link into the platform's client ([`ChanMessage::open_url`]).
fn with_basis(r: &mut impl Read, m: &mut ChanMessage) -> ChannelResult<()> {
    if let Some(space) = steve_store::channel_spaces::rows::get(r, &m.space_id)? {
        let external = steve_store::channel_message_repo::external_id(r, &m.id)?;
        m.open_url = crate::open_link::message_url(r, &space, external.as_deref())?;
    }
    if m.direction == "out" {
        m.basis = crate::basis::summary_of(r, &m.id)?;
    }
    if let Some((text, atts)) = crate::media::present(r, &m.id) {
        m.text = text;
        m.attachments = atts;
    }
    Ok(())
}

/// Messages of a space older than `before` (all when `None`), newest first.
/// Single-user reads include every receiving identity; the caller checks the workspace.
pub fn page(
    store: &Store,
    space: &str,
    _me: &str,
    before: Option<i64>,
    limit: usize,
) -> ChannelResult<Vec<ChanMessage>> {
    project_page(store, space, before.map(Before::Time), limit)
}

/// Like [`page`], with an exact cursor: the messages strictly before `(created, id)` (all when
/// `None`), newest first, so messages of the same millisecond are never skipped between pages.
pub fn page_before(
    store: &Store,
    space: &str,
    _me: &str,
    before: Option<(i64, &str)>,
    limit: usize,
) -> ChannelResult<Vec<ChanMessage>> {
    project_page(
        store,
        space,
        before.map(|(time, id)| Before::Message(time, id)),
        limit,
    )
}

fn project_page(
    store: &Store,
    space: &str,
    before: Option<Before<'_>>,
    limit: usize,
) -> ChannelResult<Vec<ChanMessage>> {
    store.read(|r| {
        projection::page(r, space, before, limit as u64)?
            .into_iter()
            .map(|row| projected(r, row))
            .collect()
    })
}

// ---------------------------------------------------------------- retention

/// How often `channels.retention` runs.
pub const RETENTION_EVERY: Duration = Duration::from_secs(3600);

/// Messages purged per write transaction, so a large purge doesn't hold the write lock long.
const PURGE_BATCH: usize = 1000;

/// One pass at `now`: in every workspace, message text and attachments (card blocks, file names,
/// resource keys) older than `keepRaw` days are cleared and `purged = now`. Returns the messages
/// purged. Blocking: runs in batches of [`PURGE_BATCH`], each in its own transaction.
pub fn sweep(m: &Modules, now: i64) -> ApiResult<usize> {
    let workspaces = m.store.read(|r| steve_store::Workspaces(r).ids())?;
    m.store.write(stored_retention::clear_purged_attachments)?;
    let mut n = 0;
    for ws in workspaces {
        loop {
            let done = m.store.write(|w| {
                let days = steve_flow::retention::keep_raw_days(w, &ws)?;
                let cutoff = now - days * 24 * 3_600_000;
                Ok::<_, steve_store::StoreError>(stored_retention::purge_before(
                    w,
                    &ws,
                    cutoff,
                    now,
                    PURGE_BATCH as u64,
                )? as usize)
            })?;
            n += done;
            if done < PURGE_BATCH {
                break;
            }
        }
    }
    Ok(n)
}

/// The hourly job; each sweep runs off the async runtime.
/// The returned task must be held until shutdown.
pub fn retention(m: Modules) -> steve_runtime::Task {
    steve_runtime::every_after(
        "channels.retention",
        m.clock.clone(),
        RETENTION_EVERY,
        steve_runtime::FirstRun::Immediate,
        move || {
            let m = m.clone();
            async move {
                match m.blocking(|m| sweep(m, m.clock.now())).await {
                    Ok(n) if n > 0 => tracing::info!(purged = n, "channels.retention"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("channels.retention: {e}"),
                }
            }
        },
    )
}

// ---------------------------------------------------------------- timeline

/// The contact page's “往来”, kind `message`: what a party said in spaces the viewer sees, newest
/// first. Messages recorded under a contact later merged into this one count too.
pub struct MessageTimeline;

impl MessageTimeline {
    pub fn arc() -> Arc<dyn TimelineSource> {
        Arc::new(Self)
    }
}

fn cut(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

impl TimelineSource for MessageTimeline {
    fn entries(
        &self,
        store: &steve_store::Store,
        viewer: &Viewer,
        party: &str,
        before: Option<i64>,
        limit: usize,
    ) -> ApiResult<Vec<TimelineEntry>> {
        store.read(|r| {
            let rows = steve_store::channel_message_repo::history::by_party(
                r,
                &viewer.ws,
                party,
                before,
                limit as u64,
            )?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let name = if row.name.is_empty() {
                        "私聊"
                    } else {
                        &row.name
                    };
                    let text = match row
                        .text
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                    {
                        Some(text) => format!("在「{name}」说：{}", cut(text, TIMELINE_CUT)),
                        None if row.text.is_none() => {
                            format!("在「{name}」发了一条消息（原文已过保留期限）")
                        }
                        None => format!("在「{name}」发了一条消息"),
                    };
                    TimelineEntry {
                        ts: row.created,
                        kind: "message".into(),
                        text,
                        href: format!("#/channels/space/{}", row.space),
                    }
                })
                .collect())
        })
    }
}

/// The open item a message's thread came from, when its sender may add to it: whoever raised the item
/// (the sender of the message it came from) or you (`from_owner`). The item came from a message of the
/// same thread in the same space, by a rule (`work_item_id`) or by the conversation (its origin
/// message's `chan.message`). Anyone else's reply, a closed item or no thread: `None`, and the message
/// takes its usual way (decided by the user on 2026-11-09).
pub(crate) fn thread_item(
    r: &mut impl Read,
    ws: &str,
    message_id: &str,
    from_owner: bool,
) -> ChannelResult<Option<String>> {
    let item = steve_store::channel_message_repo::history::open_thread_item(r, ws, message_id)?;
    Ok(item
        .filter(|row| from_owner || (row.sender.is_some() && row.raiser == row.sender))
        .map(|row| row.item))
}

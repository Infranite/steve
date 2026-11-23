//! The reaction lifecycle on an inbound message Steve answers: an
//! “acknowledged / working on it” reaction the moment Steve decides to respond, swapped for “done”
//! once the answer is out (or the item it became has ended), or for “failed” when it could not be
//! answered or sent. An answer that waits for your approval takes the acknowledgement off (nothing is
//! being worked on meanwhile); once approved it still ends “done” or “failed”, and a draft you
//! discard, or whose approval expires, stays without one.
//!
//! Reactions are best-effort: every provider call has a short timeout, a failure is logged and kept
//! in `chan_reactions.error`, and nothing here ever stops or delays a reply beyond that timeout.
//!
//! Who reacts: the space's voice, the identity that answers there. A bot account reacts as the bot (its channel
//! adapter, [`crate::ChannelAdapter::add_reaction`]); your own account (`mode=self`, voice where no bot
//! is) reacts as you through the outbox's send-as-you path ([`Outbox`](crate::Outbox) is a
//! [`Reactor`]), because the reply itself goes out as you and the bot may not be in that chat — and
//! there only on your own messages (your `/bot`), never on anybody else's.
//!
//! State (`chan_reactions`, migration 020): `ack → done | failed | cleared`; `failed → done` when a
//! retry gets the answer out; `cleared → done | failed` when a discarded draft is taken back (undo)
//! and then goes out or fails. `answer_id` is the outbound row whose sending completes it.

mod finish;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::channel::{MessageRef, ReactionKind};
use crate::{ChannelResult, Channels};
use steve_store::channel_message_repo::{self, reactions as repo};

/// How long one provider call may take before it is given up on.
pub const REACTION_TIMEOUT: Duration = Duration::from_secs(4);

/// A reaction the provider accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    pub reaction_id: String,
    pub emoji: String,
    /// Added with your user token, not the bot's.
    pub as_user: bool,
}

/// Puts reactions on and takes them off, as the account of connection `conn`.
#[async_trait]
pub trait Reactor: Send + Sync {
    async fn add(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        msg: &MessageRef,
        kind: ReactionKind,
    ) -> Result<Added, String>;

    /// `as_user`: it was added as you ([`Added::as_user`]).
    async fn remove(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        msg: &MessageRef,
        reaction_id: &str,
        as_user: bool,
    ) -> Result<(), String>;
}

/// The default reactor: the connection's channel adapter (bot accounts only).
#[derive(Debug, Default, Clone, Copy)]
pub struct AdapterReactor;

/// `connections.adapter` of a connection.
pub(crate) fn adapter_of(ch: &Channels, ws: &str, conn: &str) -> Result<String, String> {
    ch.store()
        .read(|r| -> ChannelResult<String> {
            steve_store::connections::connection_kind(r, conn)?
                .filter(|(workspace, _)| workspace == ws)
                .map(|(_, adapter)| adapter)
                .ok_or_else(|| steve_modules::ApiError::not_found("连接"))
        })
        .map_err(|e| e.to_string())
}

#[async_trait]
impl Reactor for AdapterReactor {
    async fn add(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        msg: &MessageRef,
        kind: ReactionKind,
    ) -> Result<Added, String> {
        let adapter = adapter_of(ch, ws, conn)?;
        let ext_msg_id = msg.id().ok_or_else(|| "这条消息没有消息 id".to_string())?;
        let a = ch
            .adapter(&adapter)
            .ok_or_else(|| "这个渠道不支持表情回复".to_string())?;
        if !a.features().reactions {
            return Err("这个渠道不支持表情回复".into());
        }
        let cx = ch
            .connections()
            .cx_for(ws, conn, None)
            .map_err(|e| e.to_string())?;
        let id = a
            .add_reaction(&cx, ext_msg_id, kind)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Added {
            reaction_id: id,
            emoji: a.reaction_emoji(kind).into(),
            as_user: false,
        })
    }

    async fn remove(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        msg: &MessageRef,
        reaction_id: &str,
        _as_user: bool,
    ) -> Result<(), String> {
        let adapter = adapter_of(ch, ws, conn)?;
        let ext_msg_id = msg.id().ok_or_else(|| "这条消息没有消息 id".to_string())?;
        let a = ch
            .adapter(&adapter)
            .ok_or_else(|| "这个渠道不支持表情回复".to_string())?;
        let cx = ch
            .connections()
            .cx_for(ws, conn, None)
            .map_err(|e| e.to_string())?;
        a.remove_reaction(&cx, ext_msg_id, reaction_id)
            .await
            .map_err(|e| e.to_string())
    }
}

/// How a reaction ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Done,
    Failed,
    /// Take the acknowledgement off (a draft you discarded, an item you dropped).
    Clear,
}

impl Finish {
    fn state(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Clear => "cleared",
        }
    }
}

/// Where a message is, for the provider calls.
struct Target {
    ws: String,
    space: String,
    account: Option<String>,
    party: Option<String>,
    /// The voice's connection and the message as its adapter acts on it; `None` once no identity
    /// of the space is connected any more (its connections removed: `spaces.connection_id` is
    /// NULL, migration 012). The state then changes here only, without a provider call.
    via: Option<(String, MessageRef)>,
}

/// The message reacts as the space's voice (the identity that answers there,
/// [`crate::spaces::voice`]), whichever identity received it.
fn target(r: &mut impl steve_store::Read, message_id: &str) -> ChannelResult<Option<Target>> {
    let Some(target) = repo::inbound_target(r, message_id)? else {
        return Ok(None);
    };
    let space = steve_store::channel_spaces::rows::get(r, &target.space)?;
    let via = match space.map(|s| s.connection_id).filter(|id| !id.is_empty()) {
        Some(conn) => {
            let Some(message) = crate::messages::message_ref(r, message_id, &conn)? else {
                return Ok(None);
            };
            Some((conn, message))
        }
        None => None,
    };
    Ok(Some(Target {
        ws: target.workspace,
        space: target.space,
        account: target.account,
        party: target.party,
        via,
    }))
}

/// A personal account only reacts to its owner's messages.
fn unasked_as_you(r: &mut impl steve_store::Read, target: &Target) -> ChannelResult<bool> {
    let Some((conn, _)) = &target.via else {
        return Ok(false);
    };
    if crate::arbiter::mode_of(r, &target.ws, conn)? != crate::spaces::Mode::Own {
        return Ok(false);
    }
    let (mine, _) = crate::bindings::connection_account(r, &target.ws, conn)?;
    let own = target.account.as_deref() == Some(mine.as_str())
        || crate::conversations::sender_is_owner(r, &target.ws, target.party.as_deref())?;
    Ok(!own)
}

/// One lock per message, so an acknowledgement and its ending never interleave.
#[derive(Default)]
pub(crate) struct Locks(Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>);

impl Locks {
    fn of(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 1000 {
            m.retain(|_, l| Arc::strong_count(l) > 1);
        }
        m.entry(id.to_string()).or_default().clone()
    }
}

async fn timed<T>(f: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    match tokio::time::timeout(REACTION_TIMEOUT, f).await {
        Ok(r) => r,
        Err(_) => Err("表情回复超时".into()),
    }
}

impl Channels {
    /// “收到”: Steve decided to respond to inbound `message_id`. Once per message; a failure of the
    /// provider is recorded (state `ack` without a reaction id) and does not stop anything. Returns as
    /// soon as the message's reaction lock is held: the provider call runs in the background holding it,
    /// so the answer is never kept waiting on it, and a finish of the same message waits for the
    /// acknowledgement to be on before it takes it off.
    pub async fn react_ack(&self, message_id: &str) {
        let guard = self.reaction_locks().of(message_id).lock_owned().await;
        // the record first, now: the answer may be bound to it (`expect_answer`) before the provider replies
        let t = match self.ack_record(message_id) {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(message = message_id, "ack reaction: {e}");
                return;
            }
        };
        let (ch, id) = (self.clone(), message_id.to_string());
        tokio::spawn(async move {
            let _g = guard;
            if let Err(e) = ch.ack_provider(&id, &t).await {
                tracing::warn!(message = id, "ack reaction: {e}");
            }
        });
    }

    /// Waits for any in-flight reaction call on `message_id` (such as [`Self::react_ack`]'s
    /// background provider call, whose lock is acquired before `react_ack` returns) to settle.
    pub async fn react_settled(&self, message_id: &str) {
        let lock = self.reaction_locks().of(message_id);
        let _g = lock.lock().await;
    }

    /// Records state `ack` for `message_id` (no reaction id yet); `None` when there is nothing to
    /// acknowledge (no target, or acknowledged already). The caller holds the message's reaction lock.
    fn ack_record(&self, message_id: &str) -> ChannelResult<Option<Target>> {
        let (t, row) = self.store().read(|r| {
            Ok::<_, crate::ChannelError>((target(r, message_id)?, repo::get(r, message_id)?))
        })?;
        // no connected identity: nobody to react as, nothing to record
        let Some(t) = t.filter(|t| t.via.is_some()) else {
            return Ok(None);
        };
        if self.store().read(|r| unasked_as_you(r, &t))? {
            return Ok(None);
        }
        if row.is_some_and(|r| r.state != "cleared") {
            return Ok(None);
        }
        let now = self.now();
        self.store().write(|w| -> ChannelResult<()> {
            repo::acknowledge(w, &t.ws, message_id, now)?;
            channel_message_repo::emit(w, &t.ws, &t.space, message_id);
            Ok(())
        })?;
        Ok(Some(t))
    }

    /// Puts the acknowledgement on at the provider and records its id (or the failure). The caller holds
    /// the message's reaction lock, so nothing takes it off before it is on.
    async fn ack_provider(&self, message_id: &str, t: &Target) -> ChannelResult<()> {
        let Some((conn, msg)) = &t.via else {
            return Ok(());
        };
        let added = timed(
            self.reactor()
                .add(self, &t.ws, conn, msg, ReactionKind::Ack),
        )
        .await;
        if let Err(e) = &added {
            tracing::info!(message = message_id, "ack reaction not added: {e}");
        }
        let now = self.now();
        self.store().write(|w| {
            let (id, emoji, as_user, error) = match &added {
                Ok(a) => (
                    Some(a.reaction_id.as_str()),
                    Some(a.emoji.as_str()),
                    a.as_user,
                    None,
                ),
                Err(e) => (None, None, false, Some(e.as_str())),
            };
            repo::save_result(
                w,
                message_id,
                repo::Change {
                    id,
                    emoji,
                    as_user,
                    error,
                },
                None,
                now,
            )?;
            channel_message_repo::emit(w, &t.ws, &t.space, message_id);
            Ok(())
        })
    }

    /// Ends the reaction of `message_id`: the acknowledgement comes off, “done” or “failed” goes on
    /// (nothing goes on for [`Finish::Clear`]). No-op for a message never acknowledged, or one
    /// already in that state; `done` is final (a later failure of something else does not undo it).
    pub async fn react_finish(&self, message_id: &str, how: Finish) {
        if let Err(e) = self.react_finish_inner(message_id, how).await {
            tracing::warn!(message = message_id, "reaction {}: {e}", how.state());
        }
    }

    async fn react_finish_inner(&self, message_id: &str, how: Finish) -> ChannelResult<()> {
        let lock = self.reaction_locks().of(message_id);
        let _g = lock.lock().await;
        let (t, row) = self.store().read(|r| {
            Ok::<_, crate::ChannelError>((target(r, message_id)?, repo::get(r, message_id)?))
        })?;
        let (Some(t), Some(row)) = (t, row) else {
            return Ok(());
        };
        if !finish::allowed(&row.state, how) {
            return Ok(());
        }
        let finished = self.finish_provider(message_id, how, &t, &row).await;
        let now = self.now();
        self.store().write(|w| {
            let change = repo::Change {
                id: finished.id.as_deref(),
                emoji: finished.emoji.as_deref(),
                as_user: finished.as_user,
                error: finished.error.as_deref(),
            };
            repo::save_result(w, message_id, change, Some(how.state()), now)?;
            channel_message_repo::emit(w, &t.ws, &t.space, message_id);
            Ok(())
        })
    }

    /// The outbound row `out_id` went out (`true`) or could not (`false`): the inbound message it
    /// answers turns done or failed.
    pub async fn react_answered(&self, out_id: &str, sent: bool) {
        let src = self
            .store()
            .read(|r| repo::discarded_answer_source(r, out_id));
        match src {
            Ok(Some(m)) => {
                self.react_finish(&m, if sent { Finish::Done } else { Finish::Failed })
                    .await
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(out = out_id, "reaction of the answer: {e}"),
        }
    }

    /// A draft answer you discarded: its message's acknowledgement comes off.
    pub async fn react_discarded(&self, out_id: &str) {
        if let Ok(Some(m)) = self
            .store()
            .read(|r| repo::discarded_answer_source(r, out_id))
        {
            self.react_finish(&m, Finish::Clear).await;
        }
    }

    /// The item made from a message ended: done when it finished, the acknowledgement comes off when
    /// it was dropped. Every inbound message the item came from (`links … from chan_message`).
    pub async fn react_item_ended(&self, ws: &str, item_id: &str, finished: bool) {
        let ids = self.store().read(|r| {
            let sources = steve_store::link_repo::channel_sources(r, ws, &[item_id])?;
            let messages: Vec<_> = sources.into_iter().map(|(_, id)| id).collect();
            repo::for_messages(r, &messages)
        });
        for m in ids.unwrap_or_default() {
            self.react_finish(
                &m,
                if finished {
                    Finish::Done
                } else {
                    Finish::Clear
                },
            )
            .await;
        }
    }
}

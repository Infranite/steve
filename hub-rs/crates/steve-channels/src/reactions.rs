//! The reaction lifecycle on an inbound message Steve answers (channels-personas.md §2.11): an
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
//! [`Reactor`]), because the reply itself goes out as you and the bot may not be in that chat.
//!
//! State (`chan_reactions`, migration 020): `ack → done | failed | cleared`; `failed → done` when a
//! retry gets the answer out; `cleared → done | failed` when a discarded draft is taken back (undo)
//! and then goes out or fails. `answer_id` is the outbound row whose sending completes it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::channel::ReactionKind;
use crate::{ChannelResult, Channels, messages};

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
        ext_msg_id: &str,
        kind: ReactionKind,
    ) -> Result<Added, String>;

    /// `as_user`: it was added as you ([`Added::as_user`]).
    async fn remove(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        ext_msg_id: &str,
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
        .reader(|c| -> ChannelResult<String> {
            Ok(c.query_row(
                "SELECT adapter FROM connections WHERE id = ? AND workspace_id = ?",
                [conn, ws],
                |r| r.get(0),
            )?)
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
        ext_msg_id: &str,
        kind: ReactionKind,
    ) -> Result<Added, String> {
        let adapter = adapter_of(ch, ws, conn)?;
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
        ext_msg_id: &str,
        reaction_id: &str,
        _as_user: bool,
    ) -> Result<(), String> {
        let adapter = adapter_of(ch, ws, conn)?;
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

/// One `chan_reactions` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReactionRow {
    pub message_id: String,
    /// `ack | done | failed | cleared`
    pub state: String,
    pub reaction_id: Option<String>,
    pub emoji: Option<String>,
    pub as_user: bool,
    pub answer_id: Option<String>,
    pub error: Option<String>,
    pub updated: i64,
}

pub fn get(c: &Connection, message_id: &str) -> ChannelResult<Option<ReactionRow>> {
    Ok(c.query_row(
        "SELECT message_id, state, reaction_id, emoji, as_user, answer_id, error, updated FROM chan_reactions WHERE message_id = ?",
        [message_id],
        |r| {
            Ok(ReactionRow {
                message_id: r.get(0)?,
                state: r.get(1)?,
                reaction_id: r.get(2)?,
                emoji: r.get(3)?,
                as_user: r.get::<_, i64>(4)? != 0,
                answer_id: r.get(5)?,
                error: r.get(6)?,
                updated: r.get(7)?,
            })
        },
    )
    .optional()?)
}

/// The inbound message whose reaction the sending of outbound `out_id` completes.
pub fn answered_by(c: &Connection, out_id: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT message_id FROM chan_reactions WHERE answer_id = ?",
        [out_id],
        |r| r.get(0),
    )
    .optional()?)
}

/// Remembers that sending `out_id` answers `message_id` (only while it is acknowledged or failed).
pub fn expect_answer(c: &Connection, message_id: &str, out_id: &str) -> ChannelResult<bool> {
    Ok(c.execute(
        "UPDATE chan_reactions SET answer_id = ? WHERE message_id = ? AND state IN ('ack', 'failed')",
        params![out_id, message_id],
    )? == 1)
}

/// Where a message is, for the provider calls.
struct Target {
    ws: String,
    conn: String,
    space: String,
    ext: String,
}

/// The message reacts as the space's voice (the identity that answers there,
/// [`crate::spaces::voice`]), whichever identity received it.
fn target(c: &Connection, message_id: &str) -> ChannelResult<Option<Target>> {
    let t = c.query_row(
        "SELECT m.workspace_id, s.connection_id, m.space_id, coalesce(m.external_id, '')
           FROM chan_messages m JOIN spaces s ON s.id = m.space_id WHERE m.id = ? AND m.direction = 'in'",
        [message_id],
        |r| {
            Ok(Target {
                ws: r.get(0)?,
                conn: r.get(1)?,
                space: r.get(2)?,
                ext: r.get(3)?,
            })
        },
    )
    .optional()?
    .filter(|t: &Target| !t.ext.is_empty());
    let Some(mut t) = t else { return Ok(None) };
    if let Some(v) = crate::spaces::voice(c, &t.space)? {
        t.conn = v;
    }
    Ok(Some(t))
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

    /// Records state `ack` for `message_id` (no reaction id yet); `None` when there is nothing to
    /// acknowledge (no target, or acknowledged already). The caller holds the message's reaction lock.
    fn ack_record(&self, message_id: &str) -> ChannelResult<Option<Target>> {
        let (t, row) = self.store().reader(|c| {
            Ok::<_, crate::ChannelError>((target(c, message_id)?, get(c, message_id)?))
        })?;
        let Some(t) = t else { return Ok(None) };
        if row.is_some_and(|r| r.state != "cleared") {
            return Ok(None);
        }
        let now = self.now();
        self.store().transaction(|tx| -> ChannelResult<()> {
            tx.execute(
                "INSERT INTO chan_reactions (message_id, workspace_id, state, reaction_id, emoji, as_user, error, updated)
                 VALUES (?, ?, 'ack', NULL, NULL, 0, NULL, ?)
                 ON CONFLICT (message_id) DO UPDATE SET state = 'ack', reaction_id = NULL, emoji = NULL, as_user = 0,
                   answer_id = NULL, error = NULL, updated = excluded.updated",
                params![message_id, t.ws, now],
            )?;
            messages::event(tx, &t.ws, &t.space, message_id);
            Ok(())
        })?;
        Ok(Some(t))
    }

    /// Puts the acknowledgement on at the provider and records its id (or the failure). The caller holds
    /// the message's reaction lock, so nothing takes it off before it is on.
    async fn ack_provider(&self, message_id: &str, t: &Target) -> ChannelResult<()> {
        let added = timed(
            self.reactor()
                .add(self, &t.ws, &t.conn, &t.ext, ReactionKind::Ack),
        )
        .await;
        if let Err(e) = &added {
            tracing::info!(message = message_id, "ack reaction not added: {e}");
        }
        let now = self.now();
        self.store().transaction(|tx| -> ChannelResult<()> {
            let (id, emoji, as_user, err) = match &added {
                Ok(a) => (Some(a.reaction_id.clone()), Some(a.emoji.clone()), a.as_user, None),
                Err(e) => (None, None, false, Some(e.clone())),
            };
            tx.execute(
                "UPDATE chan_reactions SET reaction_id = ?, emoji = ?, as_user = ?, error = ?, updated = ?
                  WHERE message_id = ? AND state = 'ack'",
                params![id, emoji, as_user as i64, err, now, message_id],
            )?;
            messages::event(tx, &t.ws, &t.space, message_id);
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
        let (t, row) = self.store().reader(|c| {
            Ok::<_, crate::ChannelError>((target(c, message_id)?, get(c, message_id)?))
        })?;
        let (Some(t), Some(row)) = (t, row) else {
            return Ok(());
        };
        let allowed = match (row.state.as_str(), how) {
            (s, h) if s == h.state() => false,
            ("ack", _) => true,
            ("failed", Finish::Done) => true,
            // a discarded draft taken back and sent after all (or failing then): the asker still
            // learns how it ended
            ("cleared", Finish::Done | Finish::Failed) => true,
            _ => false,
        };
        if !allowed {
            return Ok(());
        }
        let mut err: Option<String> = None;
        if let Some(rid) = &row.reaction_id
            && let Err(e) =
                timed(
                    self.reactor()
                        .remove(self, &t.ws, &t.conn, &t.ext, rid, row.as_user),
                )
                .await
        {
            tracing::info!(message = message_id, "reaction not removed: {e}");
            err = Some(e);
        }
        let kind = match how {
            Finish::Done => Some(ReactionKind::Done),
            Finish::Failed => Some(ReactionKind::Failed),
            Finish::Clear => None,
        };
        let added = match kind {
            Some(k) => Some(timed(self.reactor().add(self, &t.ws, &t.conn, &t.ext, k)).await),
            None => None,
        };
        let (id, emoji, as_user) = match &added {
            Some(Ok(a)) => (
                Some(a.reaction_id.clone()),
                Some(a.emoji.clone()),
                a.as_user,
            ),
            Some(Err(e)) => {
                tracing::info!(
                    message = message_id,
                    "{} reaction not added: {e}",
                    how.state()
                );
                err = Some(e.clone());
                (None, None, row.as_user)
            }
            None => (None, None, row.as_user),
        };
        let now = self.now();
        self.store().transaction(|tx| -> ChannelResult<()> {
            tx.execute(
                "UPDATE chan_reactions SET state = ?, reaction_id = ?, emoji = ?, as_user = ?, error = ?, updated = ?
                  WHERE message_id = ?",
                params![how.state(), id, emoji, as_user as i64, err, now, message_id],
            )?;
            messages::event(tx, &t.ws, &t.space, message_id);
            Ok(())
        })
    }

    /// The outbound row `out_id` went out (`true`) or could not (`false`): the inbound message it
    /// answers turns done or failed.
    pub async fn react_answered(&self, out_id: &str, sent: bool) {
        let src = self.store().reader(|c| answered_by(c, out_id));
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
        if let Ok(Some(m)) = self.store().reader(|c| answered_by(c, out_id)) {
            self.react_finish(&m, Finish::Clear).await;
        }
    }

    /// The item made from a message ended: done when it finished, the acknowledgement comes off when
    /// it was dropped. Every inbound message the item came from (`links … from chan_message`).
    pub async fn react_item_ended(&self, ws: &str, item_id: &str, finished: bool) {
        let ids = self.store().reader(|c| -> ChannelResult<Vec<String>> {
            let mut st = c.prepare_cached(
                "SELECT l.object_id FROM links l JOIN chan_reactions r ON r.message_id = l.object_id
                  WHERE l.workspace_id = ? AND l.subject_type = 'item' AND l.subject_id = ?
                    AND l.predicate = 'from' AND l.object_type = 'chan_message'",
            )?;
            let v = st
                .query_map(params![ws, item_id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(v)
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

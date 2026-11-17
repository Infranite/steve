//! Writes a channel refused for good, remembered (`chan_write_blocks`, ext_ws protocol §3.6).
//!
//! A [`ProviderError::Blocked`] send says no retry will help until a permission changes at the
//! provider: the connection is read-only ([`BlockScope::Connection`], every chat of it) or one chat
//! refuses writes ([`BlockScope::Target`]). It is remembered for [`MEMORY_MS`]; meanwhile:
//!
//! - automatic replies (a rule's reply or draft, the acknowledgements) are not drafted there
//!   ([`active`]), so no model runs and no card appears for a message that cannot go out;
//! - only the first refused message opens a card, the one notice saying why and how to allow
//!   sending ([`Refusal::notice`]); later ones are kept as not sent, without a card, at most one
//!   notice per connection or chat every [`MEMORY_MS`], even across a lift;
//! - YOLO never retries it (the card offers only `discard`).
//!
//! It is lifted early when the provider reports the connection writable again ([`lift_connection`],
//! the channel service's session status) or a write there goes through ([`lift_after_send`]).
//!
//! [`ProviderError::Blocked`]: steve_modules::accounts::connections::ProviderError::Blocked

use rusqlite::{Connection, OptionalExtension, params};
use steve_modules::accounts::connections::BlockScope;

use crate::ChannelResult;

/// How long a refusal is remembered: no automatic drafts there, no second card about it.
pub const MEMORY_MS: i64 = 24 * 60 * 60 * 1000;

/// `space_id` of a block of the whole connection.
const EVERY_CHAT: &str = "";

/// What [`record`] made of a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    /// No card explained this block within [`MEMORY_MS`]: the refused message opens the one notice.
    pub notice: bool,
}

fn key(scope: BlockScope, space_id: &str) -> &str {
    match scope {
        BlockScope::Connection => EVERY_CHAT,
        BlockScope::Target => space_id,
    }
}

/// A refused write on `connection_id` (in `space_id`) at `now`: remembered until `now +`
/// [`MEMORY_MS`], and whether this refusal is the one to open a notice (it then counts as noticed).
pub fn record(
    c: &Connection,
    ws: &str,
    connection_id: &str,
    space_id: &str,
    scope: BlockScope,
    reason: &str,
    now: i64,
) -> ChannelResult<Refusal> {
    let space = key(scope, space_id);
    let noticed: Option<Option<i64>> = c
        .query_row(
            "SELECT noticed_at FROM chan_write_blocks WHERE connection_id = ? AND space_id = ?",
            params![connection_id, space],
            |r| r.get(0),
        )
        .optional()?;
    let notice = noticed.flatten().is_none_or(|at| at <= now - MEMORY_MS);
    c.execute(
        "INSERT INTO chan_write_blocks (connection_id, space_id, workspace_id, reason, blocked_at, until, noticed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, CASE WHEN ?7 THEN ?5 END)
         ON CONFLICT (connection_id, space_id) DO UPDATE SET reason = excluded.reason,
           blocked_at = excluded.blocked_at, until = excluded.until,
           noticed_at = CASE WHEN ?7 THEN excluded.blocked_at ELSE noticed_at END",
        params![connection_id, space, ws, reason, now, now + MEMORY_MS, notice],
    )?;
    Ok(Refusal { notice })
}

/// The reason writes to `space_id` of `connection_id` are refused at `now`, if a refusal of the
/// connection or of that chat is remembered.
pub fn active(
    c: &Connection,
    connection_id: &str,
    space_id: &str,
    now: i64,
) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT reason FROM chan_write_blocks WHERE connection_id = ? AND space_id IN (?, ?)
                AND until > ? ORDER BY space_id = ? DESC LIMIT 1",
        params![connection_id, EVERY_CHAT, space_id, now, EVERY_CHAT],
        |r| r.get(0),
    )
    .optional()?)
}

/// The provider says `connection_id` may write again: its own block is lifted, and with
/// `every_chat` (nothing is refused any more) its chats' too. The notice times stay, so a refusal
/// soon after opens no second card. Returns whether anything was lifted.
pub fn lift_connection(
    c: &Connection,
    connection_id: &str,
    every_chat: bool,
) -> ChannelResult<bool> {
    let n = c.execute(
        "UPDATE chan_write_blocks SET until = 0
          WHERE connection_id = ?1 AND until > 0 AND (space_id = ?2 OR ?3)",
        params![connection_id, EVERY_CHAT, every_chat],
    )?;
    Ok(n > 0)
}

/// A write to `space_id` of `connection_id` went through: neither the connection nor that chat
/// refuses writes.
pub fn lift_after_send(c: &Connection, connection_id: &str, space_id: &str) -> ChannelResult<()> {
    c.execute(
        "UPDATE chan_write_blocks SET until = 0
          WHERE connection_id = ? AND space_id IN (?, ?) AND until > 0",
        params![connection_id, EVERY_CHAT, space_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chan_write_blocks (connection_id TEXT NOT NULL, space_id TEXT NOT NULL DEFAULT '',
               workspace_id TEXT NOT NULL, reason TEXT NOT NULL, blocked_at INTEGER NOT NULL,
               until INTEGER NOT NULL, noticed_at INTEGER, PRIMARY KEY (connection_id, space_id));",
        )
        .unwrap();
        c
    }

    #[test]
    fn a_read_only_connection_gives_one_notice_a_day() {
        let c = db();
        let r = record(&c, "w", "con", "s1", BlockScope::Connection, "只读", 1_000).unwrap();
        assert!(r.notice);
        // every chat of the connection, not just the one refused
        assert_eq!(
            active(&c, "con", "s2", 2_000).unwrap().as_deref(),
            Some("只读")
        );
        let again = record(&c, "w", "con", "s2", BlockScope::Connection, "只读", 2_000).unwrap();
        assert!(!again.notice);
        // forgotten after a day, and noticed again
        assert_eq!(active(&c, "con", "s1", 2_000 + MEMORY_MS).unwrap(), None);
        let later = record(
            &c,
            "w",
            "con",
            "s1",
            BlockScope::Connection,
            "只读",
            1_000 + MEMORY_MS,
        )
        .unwrap();
        assert!(later.notice);
    }

    #[test]
    fn a_refusing_chat_blocks_only_itself() {
        let c = db();
        assert!(
            record(&c, "w", "con", "s1", BlockScope::Target, "不许", 1)
                .unwrap()
                .notice
        );
        assert!(active(&c, "con", "s1", 2).unwrap().is_some());
        assert_eq!(active(&c, "con", "s2", 2).unwrap(), None);
        // another chat refusing is its own notice
        assert!(
            record(&c, "w", "con", "s2", BlockScope::Target, "不许", 3)
                .unwrap()
                .notice
        );
    }

    #[test]
    fn a_lift_keeps_the_notice_time() {
        let c = db();
        record(&c, "w", "con", "s1", BlockScope::Connection, "只读", 1).unwrap();
        record(&c, "w", "con", "s1", BlockScope::Target, "不许", 1).unwrap();
        assert!(lift_connection(&c, "con", false).unwrap());
        // the chat's own refusal stays until nothing is refused any more
        assert!(active(&c, "con", "s1", 2).unwrap().is_some());
        assert_eq!(active(&c, "con", "s2", 2).unwrap(), None);
        assert!(lift_connection(&c, "con", true).unwrap());
        assert!(!lift_connection(&c, "con", true).unwrap());
        assert_eq!(active(&c, "con", "s1", 2).unwrap(), None);
        // read-only again soon after the lift: remembered again, but no second card
        let r = record(&c, "w", "con", "s1", BlockScope::Connection, "只读", 3).unwrap();
        assert!(!r.notice);
        assert!(active(&c, "con", "s1", 4).unwrap().is_some());
    }

    #[test]
    fn a_sent_message_lifts_its_chat_and_connection() {
        let c = db();
        record(&c, "w", "con", "s1", BlockScope::Target, "不许", 1).unwrap();
        record(&c, "w", "con", "s2", BlockScope::Target, "不许", 1).unwrap();
        lift_after_send(&c, "con", "s1").unwrap();
        assert_eq!(active(&c, "con", "s1", 2).unwrap(), None);
        assert!(active(&c, "con", "s2", 2).unwrap().is_some());
    }
}

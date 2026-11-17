//! An account that cannot send what Steve says for your trigger words (a read-only connection, a
//! channel service that forbids writes, a missing permission): the reply stays in Steve's
//! conversation of the chat, the send's own fail card says what became of that one message, and you
//! are told once, in the notifications and your conversation, that this account does not send and
//! where that is fixed. Not again for every message: the account is remembered in `settings`
//! (`trigger.unsendable.<connection>`) until a send from it goes out, after which a new failure is
//! told again.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

use crate::deliver::Delivered;
use crate::{ChannelResult, Channels};

/// The notices, for showing a stored one in the reader's language ([`shown`]).
const KEYS: &[&str] = &["channels.notice.trigger_unsendable"];

/// The reason a notice gives when the provider gave none.
const NO_REASON: &str = "渠道没有说明原因";

/// `notice.kind` of the notice.
pub const NOTICE_KIND: &str = "channel";

fn key(conn: &str) -> String {
    format!("trigger.unsendable.{conn}")
}

/// A stored notice of this module in the current request's language; `None`: not one of ours.
pub fn shown(stored: &str) -> Option<String> {
    steve_i18n::reword!(stored, KEYS)
}

/// The account of connection `conn` as you know it: its provider and its name there.
fn account(c: &Connection, conn: &str) -> ChannelResult<(String, String)> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT a.system_id, coalesce(nullif(a.display_name, ''), a.handle_display)
               FROM connections c JOIN accounts a ON a.id = c.account_id WHERE c.id = ?",
            [conn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((system, name)) => (crate::conversations::provider_name(c, &system), name),
        None => (String::new(), conn.to_string()),
    })
}

/// The provider's word on message `id`'s failed send (its audit row).
fn last_error(c: &Connection, id: &str) -> ChannelResult<String> {
    Ok(c.query_row(
        "SELECT coalesce(json_extract(detail, '$.error'), '') FROM audit
          WHERE target = ? AND action = 'chan_message.send.failed' ORDER BY id DESC LIMIT 1",
        [id],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .unwrap_or_default())
}

/// The notice for account `conn` that cannot send: `error`, its provider's word.
pub(crate) fn notice_text(provider: &str, account: &str, error: &str) -> String {
    steve_i18n::stored(|| {
        steve_i18n::t!(
            "channels.notice.trigger_unsendable",
            provider = provider,
            account = account,
            error = error
        )
    })
}

impl Channels {
    /// What became of a trigger word's message sent as you from connection `conn`: a failure
    /// tells you once that the account does not send; a send that went out forgets it. Best-effort.
    pub(crate) fn after_trigger_send(&self, ws: &str, conn: &str, d: &Delivered) {
        if let Err(e) = self.after_trigger_send_inner(ws, conn, d) {
            tracing::warn!(connection = conn, "trigger send outcome not recorded: {e}");
        }
    }

    fn after_trigger_send_inner(&self, ws: &str, conn: &str, d: &Delivered) -> ChannelResult<()> {
        match d {
            Delivered::Sent { .. } => {
                self.store().transaction(|tx| -> ChannelResult<()> {
                    tx.execute(
                        "DELETE FROM settings WHERE workspace_id = ? AND key = ?",
                        params![ws, key(conn)],
                    )?;
                    Ok(())
                })?;
            }
            Delivered::Failed { message_id, .. } => {
                let now = self.now();
                self.store().transaction(|tx| -> ChannelResult<()> {
                    let told: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM settings WHERE workspace_id = ? AND key = ?)",
                        params![ws, key(conn)],
                        |r| r.get(0),
                    )?;
                    if told {
                        return Ok(());
                    }
                    let error = last_error(tx, message_id)?;
                    let (provider, name) = account(tx, conn)?;
                    let why = error.trim().trim_end_matches('。');
                    let text = notice_text(&provider, &name, if why.is_empty() { NO_REASON } else { why });
                    tx.execute(
                        "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?1, ?2, ?3, ?4)
                           ON CONFLICT (workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
                        params![ws, key(conn), json!({"error": error, "at": now}).to_string(), now],
                    )?;
                    // like the Hub's own failure notices: one in the notifications, a line in your
                    // conversation pointing back at it
                    let id = steve_store::base::message_add(
                        tx,
                        ws,
                        "steve",
                        &text,
                        None,
                        &json!({"notice": {"kind": NOTICE_KIND, "level": "fail"}}),
                        "notify",
                    )?;
                    steve_store::base::message_add(
                        tx,
                        ws,
                        "steve",
                        &text,
                        None,
                        &json!({"notice": {"kind": NOTICE_KIND, "level": "fail", "ref": id}}),
                        "steve",
                    )?;
                    Ok(())
                })?;
            }
            Delivered::Drafted { .. } | Delivered::Queued { .. } | Delivered::Skipped => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_i18n::{EN, with};

    #[test]
    fn the_notice_is_stored_in_zh_cn_and_shown_in_the_readers_language() {
        let stored = with(EN, || notice_text("飞书", "李雷", "连接只读"));
        assert!(stored.contains("飞书") && stored.contains("连接只读") && stored.contains("接入"));
        let en = with(EN, || shown(&stored)).expect("one of ours");
        assert!(
            en.contains("Connections") && en.contains("连接只读"),
            "{en}"
        );
        assert!(shown("别的通知").is_none());
    }
}

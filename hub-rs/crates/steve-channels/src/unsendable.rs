//! An account that cannot send what Steve says for your trigger words (a read-only connection, a
//! channel service that forbids writes, a missing permission): the reply stays in Steve's
//! conversation of the chat, the send's own fail card says what became of that one message, and you
//! are told once, in the notifications and your conversation, that this account does not send and
//! where that is fixed. Not again for every message: the account is remembered in `settings`
//! (`trigger.unsendable.<connection>`) until a send from it goes out, after which a new failure is
//! told again.
//!
//! A write the channel refused for good ([`crate::write_blocks`]) has its own one notice already
//! (the card of the first refused message): no second one is given here for it.

use steve_store::connections::Connections;
use steve_store::{Accounts, Read, Systems, audit_repo, channel_message_repo, settings, thread};

use crate::deliver::Delivered;
use crate::{ChannelResult, Channels};

/// The notices, for showing a stored one in the reader's language ([`shown`]).
const KEYS: &[&str] = &["channels.notice.trigger_unsendable"];

/// The reason a notice gives when the provider gave none.
const NO_REASON: &str = "渠道没有说明原因";

/// `notice.kind` of the notice.
pub const NOTICE_KIND: &str = "channel";

/// A stored notice of this module in the current request's language; `None`: not one of ours.
pub fn shown(stored: &str) -> Option<String> {
    steve_i18n::reword!(stored, KEYS)
}

/// The account of connection `conn` as you know it: its provider and its name there.

fn account(r: &mut impl Read, ws: &str, conn: &str) -> ChannelResult<(String, String)> {
    let account = match Connections::new(r).source(ws, conn)? {
        Some(connection) => Accounts(r).get(&connection.account_id)?,
        None => None,
    };
    let Some(account) = account else {
        return Ok((String::new(), conn.to_owned()));
    };
    let provider = Systems(r).provider_label(ws, &account.system_id)?;
    let name = if account.display_name.is_empty() {
        account.handle_display
    } else {
        account.display_name
    };
    Ok((provider, name))
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
                self.store()
                    .write(|w| settings::clear_unsendable(w, ws, conn))?;
            }
            Delivered::Failed { message_id, .. } => {
                let now = self.now();
                self.store().write(|w| -> ChannelResult<()> {
                    if settings::unsendable_reported(w, ws, conn)? {
                        return Ok(());
                    }
                    let space = channel_message_repo::space_of(w, ws, message_id)?;
                    if crate::write_blocks::active(w, conn, space.as_deref().unwrap_or(""), now)?
                        .is_some()
                    {
                        return Ok(());
                    }
                    let error =
                        audit_repo::channel_send_error(w, ws, message_id)?.unwrap_or_default();
                    let (provider, name) = account(w, ws, conn)?;
                    let why = error.trim().trim_end_matches('。');
                    let text = notice_text(
                        &provider,
                        &name,
                        if why.is_empty() { NO_REASON } else { why },
                    );
                    settings::report_unsendable(w, ws, conn, &error, now)?;
                    thread::post_notice(w, ws, NOTICE_KIND, "fail", &text, None, true, now)?;
                    Ok(())
                })?;
            }
            // refused for good and told once already (the write block's card)
            Delivered::Blocked { .. } => {}
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

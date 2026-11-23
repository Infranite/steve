//! One provider message, one response.
//!
//! A space is one row per (installation, external chat id) whichever identities see it
//! (`space_identities`), so a provider message is stored once (the unique `(space, external_id)`
//! index): the copy through the other identity or a redelivery is a duplicate. On top of that:
//!
//! - **only the voice answers**: the space's speaking identity ([`crate::spaces::voice`]) is the
//!   only one whose rules run; the other identity observes;
//! - **`/bot` from you** ([`command`]): your command wherever it is first seen (on your own account,
//!   or received by a bot), answered by the voice, or, with no voice, in your web conversation.
//!   Precedence 屏蔽 > `/bot` > 不提醒 > role rules;
//! - **echoes are not messages** ([`crate::messages::our_echo`], compared by [`echo_key`]); a message
//!   on your account that looks like another assistant's automatic reply is never an instruction
//!   ([`looks_automated`]).

use rusqlite::{Connection, OptionalExtension};

use crate::ChannelResult;
use crate::spaces::{self, Mode, SpaceRow};

/// … of a message no identity can answer (the space has no voice).
pub const LABEL_OBSERVE: &str = "由这个群的发言身份处理，这里只记录";
/// … of an automatic reply by another assistant on your account.
pub const LABEL_AUTOMATED: &str = "看起来是其他助手的自动回复，只记录";

/// The text without what Steve adds on the way out (the `[AI]` mark, the proxy mark), whitespace
/// collapsed: what an echo is compared by.
pub fn echo_key(text: &str) -> String {
    let t = steve_modules::outbound::strip_ai_prefix(text.trim());
    let t = t.trim_end().trim_end_matches(crate::outbound::PROXY_MARK);
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Do two texts count as the same message for echo detection ([`echo_key`]s equal, or one a prefix
/// of the other: a signature line added on the way out)?
pub fn same_text(a: &str, b: &str) -> bool {
    let (a, b) = (echo_key(a), echo_key(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.starts_with(&b) || b.starts_with(&a))
}

/// Text another assistant posts on your account by itself (“🤖 AI 代答，仅供参考”, an `[AI]` mark,
/// Steve's own proxy mark): recorded, never read as a command or a question to answer.
pub fn looks_automated(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("[AI]")
        || t.contains(crate::outbound::PROXY_MARK)
        || t.contains("AI 代答")
        || t.contains("AI代答")
        || (t.contains('🤖') && t.contains("仅供参考"))
}

/// Is `account` yours (the account of one of your own connections, or the same person)?
pub fn is_yours(r: &mut impl steve_store::Read, ws: &str, account: &str) -> ChannelResult<bool> {
    use steve_store::accounts::owned_party;
    let own = steve_store::channel_spaces::account_settings::personal_accounts(r, ws)?;
    if own.iter().any(|a| a == account) {
        return Ok(true);
    }
    let Some(party) = owned_party(r, ws, account)? else {
        return Ok(false);
    };
    for other in own {
        if owned_party(r, ws, &other)?.as_deref() == Some(&party) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Is the sender the user or one of the accounts they use in this workspace?
pub fn from_you(
    r: &mut impl steve_store::Read,
    ws: &str,
    sender_account: Option<&str>,
    sender_party: Option<&str>,
) -> ChannelResult<bool> {
    if let Some(id) = sender_party
        && steve_store::Contacts::new(r)
            .party(id)?
            .is_some_and(|p| p.is_me())
    {
        return Ok(true);
    }
    match sender_account {
        Some(account) => is_yours(r, ws, account),
        None => Ok(false),
    }
}

/// How many merges the sender's identity follows before looking at handles.
const MERGE_HOPS: usize = 16;

/// Is the sender one of the connection or workspace owners, by party, merge,
/// account handle, alias or email?
pub fn from_owner(
    r: &mut impl steve_store::Read,
    ws: &str,
    users: &[String],
    sender_party: Option<&str>,
    sender: &crate::channel::Sender,
) -> ChannelResult<bool> {
    use steve_store::{Accounts, Contacts, UserRows};
    let mut owners = Vec::new();
    for user in users {
        if let Some(id) = Contacts::new(r).user_party(ws, user)? {
            owners.push((id, user));
        }
    }
    if owners.is_empty() {
        return Ok(false);
    }
    let mut current = sender_party.map(str::to_owned);
    for _ in 0..MERGE_HOPS {
        let Some(party) = current else { break };
        if owners.iter().any(|(id, _)| id == &party) {
            return Ok(true);
        }
        current = Contacts::new(r).party(&party)?.and_then(|p| p.merged_into);
    }
    let seen: std::collections::HashSet<_> = std::iter::once(&sender.handle)
        .chain(sender.aliases.iter())
        .flat_map(|h| std::iter::once(&h.handle).chain(h.extra.iter()))
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if seen.is_empty() {
        return Ok(false);
    }
    for (owner, user) in owners {
        let parties = Contacts::new(r).merged_party_ids(&owner)?;
        let handles = Accounts(r).party_handles(ws, &parties)?;
        let aliases = Contacts::new(r)
            .party(&owner)?
            .map(|p| p.aliases)
            .unwrap_or_default();
        let email = UserRows(r).email(user)?;
        if handles
            .into_iter()
            .chain(aliases)
            .chain(email)
            .any(|handle| seen.contains(&handle.trim().to_lowercase()))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// An instruction admitted using the live receiving connection's verified author.
pub struct Command {
    pub instruction: String,
    pub sender_account: Option<String>,
    pub sender_party: Option<String>,
    pub from_self: bool,
}

/// Built-in automation markers and the receiving account's extra prefixes. Mentions do not hide
/// a prefix; this check also applies to follow-ups without a trigger word and at send time.
pub fn automated(text: &str, mentions: &[String], trigger: &crate::rules::Trigger) -> bool {
    let raw = text.trim_start();
    let text = crate::rules::strip_leading_mentions(raw, mentions).trim_start();
    looks_automated(raw)
        || looks_automated(text)
        || trigger.ignore_prefixes.iter().any(|prefix| {
            let prefix = prefix.trim();
            !prefix.is_empty() && (raw.starts_with(prefix) || text.starts_with(prefix))
        })
}

/// The command in a stored message, or a follow-up to one, admitted on the actual receiving
/// personal connection. Sending reuses this exact check, including the persisted receipt, current
/// connection settings, author, automation filter, scope and blocked-space boundary. Bot copies,
/// bot authors and historical messages cannot create a personal-send authorization.
pub fn command(
    c: &Connection,
    r: &mut impl steve_store::Read,
    ws: &str,
    space: &SpaceRow,
    conn: &str,
    msg: &str,
) -> ChannelResult<Option<Command>> {
    let Some(cs) = spaces::channel_settings(r, ws, conn)? else {
        return Ok(None);
    };
    if cs.mode != Mode::Own
        || spaces::settings(r, &space.id)?.attention == spaces::Attention::Blocked
    {
        return Ok(None);
    }
    let receipt: Option<(Option<String>, Option<String>, bool)> = c
        .query_row(
            "SELECT r.sender_account_id,r.sender_party_id,r.from_self FROM chan_trigger_receipts r
           JOIN chan_messages m ON m.id=r.message_id JOIN connections cn ON cn.id=r.connection_id
           JOIN space_identities si ON si.space_id=m.space_id AND si.connection_id=cn.id
          WHERE r.message_id=?1 AND r.connection_id=?2 AND m.workspace_id=?3 AND m.space_id=?4
            AND cn.workspace_id=?3 AND cn.status IN ('ok','error') AND si.left_at IS NULL
            AND m.direction='in' AND m.history IS NULL
            AND (r.from_self=0 OR r.sender_account_id=cn.account_id)",
            rusqlite::params![msg, conn, ws, space.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((sender_account, sender_party, from_self)) = receipt else {
        return Ok(None);
    };
    let t = crate::rules::Trigger::from_value(&cs.trigger);
    let m = crate::inbound::stored(r, msg)?;
    if automated(&m.text, &m.mention_names, &t) || !t.allows(from_self, sender_party.as_deref()) {
        return Ok(None);
    }
    let dm = space.kind == crate::channel::SpaceKind::Dm.code();
    let instruction = match t.instruction(&space.id, dm, &m.text, &m.mention_names) {
        Some(i) => Some(i),
        None if t.on && t.applies(&space.id, dm) && continues(c, &space.id, msg, conn)? => {
            let said = crate::rules::strip_leading_mentions(&m.text, &m.mention_names).trim();
            (!said.is_empty()).then(|| said.to_string())
        }
        None => None,
    };
    Ok(instruction.map(|instruction| Command {
        instruction,
        sender_account,
        sender_party,
        from_self,
    }))
}

/// Does stored message `msg` go on with a trigger word answered in `space`: in the thread of one
/// (its root or a message in it), or replying to one or to Steve's answer to one?
pub fn continues(c: &Connection, space: &str, msg: &str, conn: &str) -> ChannelResult<bool> {
    // a message whose trigger word was taken (its conversation turn says so)
    const ANSWERED: &str = "EXISTS (SELECT 1 FROM messages turn WHERE turn.workspace_id = t.workspace_id
        AND json_extract(turn.meta, '$.chan.message') = t.id AND json_extract(turn.meta, '$.chan.command') = 1
        AND json_extract(turn.meta, '$.chan.connection') = ?3)";
    Ok(c.query_row(
        &format!(
            "SELECT EXISTS (
               SELECT 1 FROM chan_messages m, chan_messages t
                WHERE m.id = ?1 AND t.space_id = ?2 AND t.id <> m.id AND t.created <= m.created
                  AND (
                    -- the same thread: its root, or a message in it
                    (m.thread_key IS NOT NULL AND (t.external_id = m.thread_key OR t.thread_key = m.thread_key) AND {ANSWERED})
                    -- replying to the trigger word, or to Steve's answer said as you
                    OR (m.reply_to IS NOT NULL AND t.external_id = m.reply_to
                        AND ({ANSWERED} OR (t.direction = 'out' AND t.on_behalf = 'proxy'
                            AND t.sender_account_id=(SELECT account_id FROM connections WHERE id=?3))))
                  ))"
        ),
        rusqlite::params![msg, space, conn],
        |r| r.get(0),
    )?)
}

/// The mode of a connection (its channel settings; bot when it has none).
pub fn mode_of(r: &mut impl steve_store::Read, ws: &str, conn: &str) -> ChannelResult<Mode> {
    Ok(spaces::channel_settings(r, ws, conn)?
        .map(|s| s.mode)
        .unwrap_or(Mode::Bot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_keys_drop_steves_marks() {
        assert!(same_text("[AI] 好的", "好的\n—— Steve"));
        assert!(!same_text("", ""));
        assert_eq!(echo_key("[AI] 好的，明天十点"), "好的，明天十点");
        assert_eq!(
            echo_key("好的，明天十点（由 Steve 代发）"),
            "好的，明天十点"
        );
        assert_eq!(echo_key("  好的，\n明天十点 "), "好的， 明天十点");
    }

    #[test]
    fn literal_prefixes_work_before_or_after_mentions() {
        let t = crate::rules::Trigger::from_value(
            &serde_json::json!({"ignore_prefixes":["AUTO:","@worker"]}),
        );
        assert!(automated(" @worker automatic follow-up", &[], &t));
        assert!(automated(
            " @Person AUTO: follow-up",
            &["Person".into()],
            &t
        ));
        assert!(automated("@Person [AI] follow-up", &["Person".into()], &t));
        assert!(!automated(
            "a person discussing AUTO: in a sentence",
            &[],
            &t
        ));
        assert!(!automated("auto: a different literal prefix", &[], &t));
    }

    #[test]
    fn automatic_replies_are_not_commands() {
        assert!(looks_automated("没能生成回答…🤖 AI 代答，仅供参考"));
        assert!(looks_automated("[AI] 收到"));
        assert!(looks_automated("好的（由 Steve 代发）"));
        assert!(!looks_automated("/bot 帮我回一下"));
        assert!(!looks_automated("明天开会吗？"));
    }
}

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

/// Your account in this workspace that speaks for you (a `mode=self` connection): `(connection,
/// account)` pairs.
fn self_connections(c: &Connection, ws: &str) -> ChannelResult<Vec<(String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT cs.connection_id, c.account_id FROM channel_settings cs JOIN connections c ON c.id = cs.connection_id
          WHERE cs.workspace_id = ? AND cs.mode = 'self' AND c.status NOT IN ('disconnected')
          ORDER BY cs.connection_id",
    )?;
    let v = st
        .query_map([ws], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
    Ok(v)
}

/// The account's party.
fn party_of(c: &Connection, account: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT party_id FROM accounts WHERE id = ?",
        [account],
        |r| r.get(0),
    )
    .optional()?
    .flatten())
}

fn same_person(c: &Connection, a: &str, b: &str) -> ChannelResult<bool> {
    if a == b {
        return Ok(true);
    }
    let pa = party_of(c, a)?;
    Ok(pa.is_some() && pa == party_of(c, b)?)
}

/// Is `account` yours (the account of one of your own connections, or the same person)?
pub fn is_yours(c: &Connection, ws: &str, account: &str) -> ChannelResult<bool> {
    for (_, a) in self_connections(c, ws)? {
        if same_person(c, &a, account)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Is the sender you: a person whose party is a user's “我”, or the account of one of your own
/// (`mode=self`) connections, or the same person?
pub fn from_you(
    c: &Connection,
    ws: &str,
    sender_account: Option<&str>,
    sender_party: Option<&str>,
) -> ChannelResult<bool> {
    if let Some(p) = sender_party
        && c.query_row(
            "SELECT user_id IS NOT NULL FROM parties WHERE id = ?",
            [p],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false)
    {
        return Ok(true);
    }
    match sender_account {
        Some(a) => is_yours(c, ws, a),
        None => Ok(false),
    }
}

/// How many merges [`from_owner`] follows from a sender's contact (a chain is never this long).
const MERGE_HOPS: usize = 16;

/// Is the sender one of `users` (the owner of the receiving connection, the workspace's owner)
/// however the adapter named them: the sender's contact is a user's “我” or was merged into one,
/// or one of the sender's handles (primary, aliases, their per-connection ids) is a handle of an
/// account of that “我” (or of a contact merged into it), one of its aliases, or the user's email.
/// The core's backstop for adapters that hand your own messages over under a handle Steve did not
/// know as yours.
pub fn from_owner(
    c: &Connection,
    ws: &str,
    users: &[String],
    sender_party: Option<&str>,
    sender: &crate::channel::Sender,
) -> ChannelResult<bool> {
    let mut mes: Vec<(String, String)> = Vec::new();
    for u in users {
        if let Some(id) = c
            .query_row(
                "SELECT id FROM parties WHERE workspace_id = ? AND user_id = ?",
                rusqlite::params![ws, u],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            mes.push((id, u.clone()));
        }
    }
    if mes.is_empty() {
        return Ok(false);
    }
    // the sender's contact, following merges
    let mut cur = sender_party.map(String::from);
    for _ in 0..MERGE_HOPS {
        let Some(p) = cur else { break };
        if mes.iter().any(|(m, _)| *m == p) {
            return Ok(true);
        }
        cur = c
            .query_row("SELECT merged_into FROM parties WHERE id = ?", [&p], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
    }
    // the sender's handles
    let seen: Vec<String> = std::iter::once(&sender.handle)
        .chain(sender.aliases.iter())
        .flat_map(|h| std::iter::once(&h.handle).chain(h.extra.iter()))
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if seen.is_empty() {
        return Ok(false);
    }
    for (me, user) in &mes {
        let mut known: Vec<String> = Vec::new();
        let mut st = c.prepare_cached(
            "WITH RECURSIVE ids(id) AS (SELECT ?2 UNION SELECT p.id FROM parties p JOIN ids ON p.merged_into = ids.id)
             SELECT h.handle FROM account_handles h JOIN accounts a ON a.id = h.account_id
              WHERE h.workspace_id = ?1 AND a.party_id IN (SELECT id FROM ids)
             UNION SELECT a.handle FROM accounts a WHERE a.workspace_id = ?1 AND a.party_id IN (SELECT id FROM ids)",
        )?;
        for h in st.query_map(rusqlite::params![ws, me], |r| r.get::<_, String>(0))? {
            known.push(h?.trim().to_lowercase());
        }
        let aliases: Option<String> = c
            .query_row("SELECT aliases FROM parties WHERE id = ?", [me], |r| {
                r.get(0)
            })
            .optional()?;
        known.extend(
            aliases
                .and_then(|a| serde_json::from_str::<Vec<String>>(&a).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.trim().to_lowercase()),
        );
        if let Some(email) = c
            .query_row("SELECT email FROM users WHERE id = ?", [user], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten()
        {
            known.push(email.trim().to_lowercase());
        }
        if seen
            .iter()
            .any(|h| known.iter().any(|k| !k.is_empty() && k == h))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Your `/bot` command in `space`: the instruction after the trigger word, when the sender is you
/// ([`from_you`]) — on your own account or received by a bot — the trigger is on and its scope (`*`,
/// `dm:*`, space ids) covers this space. Leading mentions are skipped (`@Steve /bot …`); what Steve
/// itself sent as you (the proxy mark) and other assistants' automatic replies never count.
pub fn command(
    c: &Connection,
    ws: &str,
    space: &SpaceRow,
    sender_account: Option<&str>,
    sender_party: Option<&str>,
    text: &str,
    mention_names: &[String],
) -> ChannelResult<Option<String>> {
    if looks_automated(text) || !from_you(c, ws, sender_account, sender_party)? {
        return Ok(None);
    }
    let Some(t) = spaces::your_trigger(c, ws)? else {
        return Ok(None);
    };
    let dm = space.kind == crate::channel::SpaceKind::Dm.code();
    Ok(t.instruction(&space.id, dm, text, mention_names))
}

/// The mode of a connection (its channel settings; bot when it has none).
pub fn mode_of(c: &Connection, ws: &str, conn: &str) -> ChannelResult<Mode> {
    Ok(spaces::channel_settings(c, ws, conn)?
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
    fn automatic_replies_are_not_commands() {
        assert!(looks_automated("没能生成回答…🤖 AI 代答，仅供参考"));
        assert!(looks_automated("[AI] 收到"));
        assert!(looks_automated("好的（由 Steve 代发）"));
        assert!(!looks_automated("/bot 帮我回一下"));
        assert!(!looks_automated("明天开会吗？"));
    }
}

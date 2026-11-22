//! The facts the tiers of 待处理 need about a conversation (steve-store `views::tier`): whether the
//! message a decision is about is from a starred contact, and whether you spoke in its conversation
//! yourself since. Both are read from what is stored, never from the clock, so a decision's tier
//! changes only with a message, a contact or an account (the Hub's projection follows those).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use steve_store::views::ChatFacts;

use crate::ChannelResult;

/// The inbound message a decision about chat message `id` is about: the message itself, or the
/// one a drafted reply answers; with its space, sender and time.
fn target(c: &Connection, id: &str) -> ChannelResult<Option<(String, Option<String>, i64)>> {
    Ok(c.query_row(
        "SELECT t.space_id, t.sender_party_id, t.created FROM chan_messages m
           JOIN chan_messages t ON t.direction = 'in'
            AND (t.id = m.id OR (m.direction = 'out' AND t.space_id = m.space_id AND t.external_id = m.reply_to))
          WHERE m.id = ? ORDER BY t.created DESC LIMIT 1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?)
}

/// The facts of decisions `(decision, chat message)` of `ws`, by decision. A decision whose message
/// answers nothing (a new message in a space) has none.
pub fn facts(
    c: &Connection,
    ws: &str,
    about: &[(String, String)],
) -> ChannelResult<HashMap<String, ChatFacts>> {
    let mut out = HashMap::new();
    for (decision, message) in about {
        let Some((space, sender, at)) = target(c, message)? else {
            continue;
        };
        let starred = crate::decide::starred(c, ws, sender.as_deref())?;
        // every space of the chat: your own account and a bot keep one chat as two spaces
        let mut st = c.prepare_cached(
            "SELECT o.id FROM spaces s JOIN spaces o ON o.workspace_id = s.workspace_id
               AND (o.id = s.id OR o.external_id = s.external_id) WHERE s.id = ?1 AND s.workspace_id = ?2",
        )?;
        let spaces = st
            .query_map(params![space, ws], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut replied = false;
        for sp in spaces {
            if crate::presence::you_spoke(c, ws, &sp, at + 1, i64::MAX)? {
                replied = true;
                break;
            }
        }
        out.insert(decision.clone(), ChatFacts { starred, replied });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_store::views::tier;
    use steve_types::{DecisionTier, FoldReason};

    /// A contact's message in a chat, a reply drafted to it, and what the tiers make of both as
    /// the contact is starred and as you speak there yourself.
    #[test]
    fn starred_contacts_ask_you_and_your_own_reply_folds_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            steve_store::Store::open(dir.path().join("s.sqlite3"), steve_store::NoPublisher)
                .unwrap();
        store.ensure_workspace().unwrap();
        let me = store.users().create("dev:me", "Me", None, "owner").unwrap();
        let ws = steve_store::WS;
        store
            .with_conn(|c| {
                c.execute_batch(&format!(
                    "INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated)
                       VALUES ('p_me', '{ws}', 'person', 'Me', '{me}', 'x', 1, 1),
                              ('p_a', '{ws}', 'person', 'A', NULL, 'x', 1, 1);
                     INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
                       VALUES ('sp1', '{ws}', 'fake', 'dm', 'chat-1', 'A', 1, 1);
                     INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id, text, created)
                       VALUES ('cm_in', '{ws}', 'sp1', 'ext-1', 'in', 'p_a', 'hi?', 10);
                     INSERT INTO chan_messages (id, workspace_id, space_id, reply_to, direction, text, status, created)
                       VALUES ('cm_draft', '{ws}', 'sp1', 'ext-1', 'out', 'hello', 'draft', 11);"
                ))?;
                Ok(())
            })
            .unwrap();
        let about = [
            ("d_ask".to_string(), "cm_in".to_string()),
            ("d_draft".to_string(), "cm_draft".to_string()),
        ];
        let tiers = |store: &steve_store::Store| {
            let f = store.reader(|c| facts(c, ws, &about)).unwrap();
            (
                tier("ask", "chan_message", f["d_ask"]),
                tier("approve", "chan_message", f["d_draft"]),
            )
        };
        assert_eq!(
            tiers(&store),
            ((DecisionTier::Decide, None), (DecisionTier::Draft, None))
        );

        // you answered in the chat yourself: both fold
        store
            .with_conn(|c| {
                c.execute_batch(&format!(
                    "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_party_id, text, created)
                       VALUES ('cm_mine', '{ws}', 'sp1', 'ext-2', 'in', 'p_me', 'on it', 12)"
                ))?;
                Ok(())
            })
            .unwrap();
        let folded = (DecisionTier::Folded, Some(FoldReason::Replied));
        assert_eq!(tiers(&store), (folded, folded));

        // a starred contact always asks you, even after you spoke and even as a draft
        store
            .with_conn(|c| {
                c.execute("UPDATE parties SET important = 1 WHERE id = 'p_a'", [])?;
                Ok(())
            })
            .unwrap();
        let decide = (DecisionTier::Decide, None);
        assert_eq!(tiers(&store), (decide, decide));
        // and an approved send that failed always does
        assert_eq!(
            tier(
                "fail",
                "chan_message",
                ChatFacts {
                    starred: false,
                    replied: true
                }
            ),
            decide
        );
    }
}

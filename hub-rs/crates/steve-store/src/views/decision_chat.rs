//! Stored channel facts used by decision views. All reads run on the decision's read snapshot.

use std::collections::HashSet;

use sea_query::{Alias, Expr, ExprTrait, JoinType, Query, SelectStatement};

use super::CHUNK;
use crate::{Read, Result};

pub const PROXY_MARK: &str = "（由 Steve 代发）";

/// Automated replies on the owner's account do not count as their own words.
pub fn looks_automated(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("[AI]")
        || text.contains(PROXY_MARK)
        || text.contains("AI 代答")
        || text.contains("AI代答")
        || (text.contains('🤖') && text.contains("仅供参考"))
}

/// The inbound message the card is about, including the source of a drafted reply.
pub(super) fn source() -> Expr {
    Expr::cust(
        "(SELECT t.id FROM chan_messages t
           WHERE t.workspace_id = decisions.workspace_id AND t.direction = 'in'
             AND (t.id = m.id OR (m.direction = 'out' AND t.space_id = m.space_id AND
               (t.external_id = m.reply_to OR t.id IN (SELECT message_id FROM chan_reactions WHERE answer_id = m.id)
                OR t.id IN (SELECT source_message_id FROM output_deliveries WHERE message_id = m.id))))
           ORDER BY t.created DESC, t.id LIMIT 1)",
    )
}

pub(super) fn starred() -> Expr {
    Expr::cust(
        "EXISTS (
           WITH RECURSIVE up(id, depth) AS (
             SELECT COALESCE(src.sender_party_id,
                    (SELECT a.party_id FROM accounts a WHERE a.id = src.sender_account_id)), 0
             UNION ALL SELECT p.merged_into, up.depth + 1 FROM parties p JOIN up ON p.id = up.id
               WHERE p.merged_into IS NOT NULL AND up.depth < 5
           )
           SELECT 1 FROM parties p JOIN up ON p.id = up.id
             WHERE p.workspace_id = decisions.workspace_id AND p.important = 1
               AND p.status = 'active' AND coalesce(p.contact_kind, '') <> 'bot' AND p.type <> 'bot'
         )",
    )
}

/// One stable representative space for chat copies seen by different accounts on a provider.
pub(super) fn conversation() -> Expr {
    Expr::cust(
        "(SELECT other.id FROM spaces other
          WHERE other.workspace_id = s.workspace_id
            AND (other.id = s.id OR (other.system_id = s.system_id AND other.external_id = s.external_id))
          ORDER BY other.created, other.id LIMIT 1)",
    )
}

pub(super) fn join_source(q: &mut SelectStatement) {
    q.join_as(
        JoinType::LeftJoin,
        Alias::new("chan_messages"),
        Alias::new("src"),
        Expr::col((Alias::new("src"), Alias::new("id"))).eq(source()),
    );
}

/// A user can speak in any provider space of the chat. Match the workspace owner, their
/// merged contacts and the personal account of that space.
const OWNER_WORDS: &str = "EXISTS (
    WITH RECURSIVE mine(id) AS (
      SELECT p.id FROM parties p JOIN workspace_members wm ON wm.user_id = p.user_id
        WHERE p.workspace_id = decisions.workspace_id AND wm.workspace_id = decisions.workspace_id
          AND wm.role = 'owner'
      UNION SELECT a.party_id FROM connections c JOIN accounts a ON a.id = c.account_id
        JOIN channel_settings cs ON cs.connection_id = c.id AND cs.mode = 'self'
        WHERE c.id = other.connection_id
      UNION SELECT p.id FROM parties p JOIN mine ON p.merged_into = mine.id
    )
    SELECT 1 FROM mine
      WHERE id = reply.sender_party_id
         OR id = (SELECT party_id FROM accounts WHERE id = reply.sender_account_id)
)";

pub(super) fn replied(c: &mut impl Read, ids: &[&str]) -> Result<HashSet<String>> {
    let mut replied = HashSet::new();
    for ids in ids.chunks(CHUNK) {
        let mut q = Query::select();
        q.expr(Expr::cust("decisions.id"))
            .expr(Expr::cust("coalesce(reply.text, '')"))
            .from(Alias::new("decisions"))
            .join_as(
                JoinType::InnerJoin,
                Alias::new("chan_messages"),
                Alias::new("m"),
                Expr::cust(
                    "m.id = decisions.subject_id AND m.workspace_id = decisions.workspace_id",
                ),
            );
        join_source(&mut q);
        q.join_as(
            JoinType::InnerJoin,
            Alias::new("spaces"),
            Alias::new("s"),
            Expr::cust("s.id = src.space_id AND s.workspace_id = decisions.workspace_id"),
        )
        .join_as(
            JoinType::InnerJoin,
            Alias::new("spaces"),
            Alias::new("other"),
            Expr::cust(
                "other.workspace_id = s.workspace_id AND
                  (other.id = s.id OR (other.system_id = s.system_id AND other.external_id = s.external_id))",
            ),
        )
        .join_as(
            JoinType::InnerJoin,
            Alias::new("chan_messages"),
            Alias::new("reply"),
            Expr::cust(
                "reply.space_id = other.id AND reply.direction = 'in' AND reply.created > src.created
                    AND reply.purged IS NULL
                    AND (s.kind = 'dm' OR reply.reply_to = src.external_id
                         OR (src.thread_key IS NOT NULL AND reply.thread_key = src.thread_key))",
            ),
        )
        .and_where(Expr::cust("decisions.id").is_in(ids.iter().copied()))
        .and_where(Expr::cust(OWNER_WORDS));
        c.all(&q, |r| {
            let text: String = r.get(1)?;
            if !looks_automated(&text) {
                replied.insert(r.get(0)?);
            }
            Ok(())
        })?;
    }
    Ok(replied)
}

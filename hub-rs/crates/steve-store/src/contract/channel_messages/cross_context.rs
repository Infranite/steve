//! Cross-conversation and active-item queries for owner-voice reply context (#275).
//!
//! All results returned here are background reasoning context only; the caller tags them as
//! `Level::Reasoning` so outbound disclosure checks prevent leaking other chats or internal items.

use sea_query::Values;

use crate::{Read, Result};

/// A matching snippet from another visible IM space or the main Steve conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherConversationSnippet {
    /// `"chan"` for another channel space, `"main"` for Steve's primary conversation.
    pub source: String,
    pub space_id: String,
    pub space_name: String,
    pub sender_name: String,
    pub text: String,
    pub at: i64,
}

/// An active/open item relevant to the current conversation's topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelevantItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub current: String,
    pub updated: i64,
}

fn escape_like(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    out.push('%');
    for c in term.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('%');
    out
}

/// Searches recent messages in OTHER non-blocked channel spaces visible to `viewer_user`
/// (excluding `exclude_space`) matching any of `terms`.
pub fn other_space_snippets(
    r: &mut impl Read,
    ws: &str,
    viewer_user: &str,
    exclude_space: &str,
    terms: &[String],
    since_ms: i64,
    limit: usize,
) -> Result<Vec<OtherConversationSnippet>> {
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let mut vals: Vec<sea_query::Value> = vec![
        ws.into(),
        exclude_space.into(),
        since_ms.into(),
        viewer_user.into(),
    ];
    let mut like_clauses = Vec::with_capacity(terms.len());
    for term in terms {
        let idx = vals.len() + 1;
        vals.push(escape_like(term).into());
        like_clauses.push(format!("m.text LIKE ?{idx} ESCAPE '\\'"));
    }
    let lim_idx = vals.len() + 1;
    vals.push((limit as i64).into());
    let sql = format!(
        "SELECT m.space_id,
                coalesce(nullif(s.name, ''), s.kind),
                coalesce(nullif(p.name, ''), nullif(a.display_name, ''), ''),
                m.text,
                coalesce(m.ext_created, m.created) AS at
           FROM chan_messages m
           JOIN spaces s ON s.id = m.space_id
           LEFT JOIN space_prefs sp ON sp.space_id = s.id
           LEFT JOIN accounts a ON a.id = m.sender_account_id
           LEFT JOIN parties p ON p.id = m.sender_party_id
          WHERE m.workspace_id = ?1
            AND m.space_id <> ?2
            AND m.status IN ('received', 'sent')
            AND m.purged IS NULL
            AND coalesce(m.ext_created, m.created) >= ?3
            AND coalesce(sp.attention, 'realtime') <> 'blocked'
            AND m.text IS NOT NULL AND m.text <> ''
            AND (
                NOT EXISTS (
                    SELECT 1 FROM connections c
                    JOIN accounts ca ON ca.id = c.account_id
                    JOIN parties cp ON cp.id = ca.party_id
                    WHERE c.id = s.connection_id AND cp.user_id IS NOT NULL AND cp.user_id <> ?4
                )
            )
            AND ({})
          ORDER BY at DESC, m.id DESC
          LIMIT ?{lim_idx}",
        like_clauses.join(" OR ")
    );
    let mut out = Vec::new();
    r.db().query(&sql, &Values(vals), &mut |row| {
        out.push(OtherConversationSnippet {
            source: "other_chat".into(),
            space_id: row.get(0)?,
            space_name: row.get::<Option<String>>(1)?.unwrap_or_default(),
            sender_name: row.get::<Option<String>>(2)?.unwrap_or_default(),
            text: row.get::<Option<String>>(3)?.unwrap_or_default(),
            at: row.get(4)?,
        });
        Ok(())
    })?;
    Ok(out)
}

/// Searches recent turns in the workspace's main Steve conversation (`messages.space = 'steve'`)
/// matching any of `terms`.
pub fn main_thread_snippets(
    r: &mut impl Read,
    ws: &str,
    terms: &[String],
    since_ms: i64,
    limit: usize,
) -> Result<Vec<OtherConversationSnippet>> {
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let mut vals: Vec<sea_query::Value> = vec![ws.into(), since_ms.into()];
    let mut like_clauses = Vec::with_capacity(terms.len());
    for term in terms {
        let idx = vals.len() + 1;
        vals.push(escape_like(term).into());
        like_clauses.push(format!("text LIKE ?{idx} ESCAPE '\\'"));
    }
    let lim_idx = vals.len() + 1;
    vals.push((limit as i64).into());
    let sql = format!(
        "SELECT author, text, created
           FROM messages
          WHERE workspace_id = ?1
            AND space = 'steve'
            AND created >= ?2
            AND text <> ''
            AND ({})
          ORDER BY created DESC, id DESC
          LIMIT ?{lim_idx}",
        like_clauses.join(" OR ")
    );
    let mut out = Vec::new();
    r.db().query(&sql, &Values(vals), &mut |row| {
        let author: String = row.get(0)?;
        out.push(OtherConversationSnippet {
            source: "main_thread".into(),
            space_id: "steve".into(),
            space_name: "Steve".into(),
            sender_name: author,
            text: row.get(1)?,
            at: row.get(2)?,
        });
        Ok(())
    })?;
    Ok(out)
}

/// Active/open items in `ws` whose title, latest progress, or latest goal matches any of `terms`.
pub fn relevant_open_items(
    r: &mut impl Read,
    ws: &str,
    terms: &[String],
    limit: usize,
) -> Result<Vec<RelevantItem>> {
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let mut vals: Vec<sea_query::Value> = vec![ws.into()];
    let mut like_clauses = Vec::with_capacity(terms.len());
    for term in terms {
        let idx = vals.len() + 1;
        vals.push(escape_like(term).into());
        like_clauses.push(format!(
            "(i.title LIKE ?{idx} ESCAPE '\\' OR coalesce(g.text, '') LIKE ?{idx} ESCAPE '\\' OR coalesce(p.text, '') LIKE ?{idx} ESCAPE '\\')"
        ));
    }
    let lim_idx = vals.len() + 1;
    vals.push((limit as i64).into());
    let sql = format!(
        "SELECT i.id, i.title, i.phase, coalesce(p.text, ''), i.updated
           FROM items i
           LEFT JOIN item_goals g
             ON g.item_id = i.id
            AND g.version = (SELECT max(version) FROM item_goals WHERE item_id = i.id)
           LEFT JOIN timeline p
             ON p.id = (SELECT max(id) FROM timeline WHERE item_id = i.id AND kind = 'progress')
          WHERE i.workspace_id = ?1
            AND i.archived = 0
            AND i.phase <> 'closed'
            AND ({})
          ORDER BY i.updated DESC, i.id DESC
          LIMIT ?{lim_idx}",
        like_clauses.join(" OR ")
    );
    let mut out = Vec::new();
    r.db().query(&sql, &Values(vals), &mut |row| {
        out.push(RelevantItem {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
            current: row.get::<Option<String>>(3)?.unwrap_or_default(),
            updated: row.get(4)?,
        });
        Ok(())
    })?;
    Ok(out)
}

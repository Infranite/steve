//! Where contacts were seen in the channels ([`steve_modules::contacts::SpacePort::seen`]), so 需要你
//! 判断 can tell an unknown person apart without showing an id: the spaces they were in (as a
//! member, a sender or someone @-mentioned), their last
//! message, and whether every one of those spaces is one you muted (不提醒 / 屏蔽 — then they don't
//! need your judgement at all).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use steve_modules::contacts::{Seen, SeenMessage, SeenSpace};
use steve_store::base::Viewer;

use crate::ChannelResult;
use crate::spaces::visible_sql;

/// Spaces listed per contact at most.
const MAX_SPACES: usize = 5;
/// Characters of the last message shown.
const SNIPPET: usize = 80;

fn snippet(t: &str) -> String {
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > SNIPPET {
        let mut s: String = one.chars().take(SNIPPET).collect();
        s.push('…');
        s
    } else {
        one
    }
}

/// A space someone was seen in: `(id, name, kind, when)`.
type SpaceRow = (String, String, String, i64);

/// For each party of `parties` that was seen somewhere the viewer can see.
pub fn seen(
    c: &Connection,
    viewer: &Viewer,
    parties: &[String],
) -> ChannelResult<Vec<(String, Seen)>> {
    let vis = visible_sql("?3");
    let mut spaces_q = c.prepare(&format!(
        "SELECT s.id, s.name, s.kind, MAX(x.at) AS at FROM (
             SELECT space_id, created AS at FROM chan_messages
              WHERE workspace_id = ?1 AND direction = 'in' AND sender_party_id = ?2
             UNION ALL
             SELECT sm.space_id, sm.joined FROM space_members sm JOIN accounts a ON a.id = sm.account_id
              WHERE sm.workspace_id = ?1 AND a.party_id = ?2) x
           JOIN spaces s ON s.id = x.space_id
          WHERE s.workspace_id = ?1 AND {vis}
          GROUP BY s.id"
    ))?;
    // Where they were @-mentioned, for all of `parties` in one pass over the mentions.
    let mut mentioned: HashMap<String, Vec<SpaceRow>> = HashMap::new();
    if !parties.is_empty() {
        let vis2 = visible_sql("?2");
        let mut st = c.prepare(&format!(
            "SELECT a.party_id, s.id, s.name, s.kind, MAX(m.created) FROM chan_messages m,
                    json_each(CASE WHEN json_valid(m.mentions) THEN m.mentions ELSE '[]' END) j
               JOIN accounts a ON a.id = json_extract(j.value, '$.account_id')
               JOIN spaces s ON s.id = m.space_id
              WHERE m.workspace_id = ?1 AND a.party_id IN (SELECT value FROM json_each(?3))
                AND s.workspace_id = ?1 AND {vis2}
              GROUP BY a.party_id, s.id"
        ))?;
        let ids = serde_json::to_string(parties).unwrap_or_else(|_| "[]".into());
        let rows = st.query_map(params![viewer.ws, viewer.user, ids], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
            ))
        })?;
        for row in rows {
            let (party, sp) = row?;
            mentioned.entry(party).or_default().push(sp);
        }
    }
    let mut last_q = c.prepare(&format!(
        "SELECT m.text, m.created, s.id, s.name FROM chan_messages m JOIN spaces s ON s.id = m.space_id
          WHERE m.workspace_id = ?1 AND m.direction = 'in' AND m.sender_party_id = ?2
            AND m.text IS NOT NULL AND trim(m.text) <> '' AND {vis}
          ORDER BY m.created DESC LIMIT 1"
    ))?;
    let mut count_q = c.prepare(
        "SELECT count(*) FROM chan_messages WHERE workspace_id = ?1 AND direction = 'in' AND sender_party_id = ?2",
    )?;
    let mut attention_of: HashMap<String, String> = HashMap::new();
    let mut out = Vec::new();
    for p in parties {
        let mut rows: Vec<SpaceRow> = spaces_q
            .query_map(params![viewer.ws, p, viewer.user], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // each space once, at the latest time they were seen there, latest first
        for m in mentioned.remove(p).unwrap_or_default() {
            match rows.iter_mut().find(|r| r.0 == m.0) {
                Some(r) => r.3 = r.3.max(m.3),
                None => rows.push(m),
            }
        }
        rows.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
        if rows.is_empty() {
            continue;
        }
        let mut spaces = Vec::new();
        for (id, name, kind, _) in &rows {
            let a = match attention_of.get(id) {
                Some(a) => a.clone(),
                None => {
                    let a = crate::spaces::attention(c, id)?.code().to_string();
                    attention_of.insert(id.clone(), a.clone());
                    a
                }
            };
            spaces.push(SeenSpace {
                id: id.clone(),
                name: name.clone(),
                kind: kind.clone(),
                attention: a,
            });
        }
        let quiet = spaces.iter().all(|s| s.attention != "realtime");
        let last = last_q
            .query_row(params![viewer.ws, p, viewer.user], |r| {
                Ok(SeenMessage {
                    text: snippet(&r.get::<_, String>(0)?),
                    created: r.get(1)?,
                    space_id: r.get(2)?,
                    space_name: r.get(3)?,
                })
            })
            .optional()?;
        let messages: i64 = count_q.query_row(params![viewer.ws, p], |r| r.get(0))?;
        spaces.truncate(MAX_SPACES);
        out.push((
            p.clone(),
            Seen {
                last_at: rows.first().map(|r| r.3),
                spaces,
                last,
                messages,
                quiet,
            },
        ));
    }
    Ok(out)
}

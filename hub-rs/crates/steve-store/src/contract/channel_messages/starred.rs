//! Persisted audience and text for starred-contact notifications.

use crate::contract::Row;
use crate::{Read, Result};
use sea_query::{Value, Values};

fn query<T>(
    r: &mut impl Read,
    sql: &str,
    args: Vec<Value>,
    mut decode: impl FnMut(&Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let mut rows = Vec::new();
    r.db().query(sql, &Values(args), &mut |row| {
        rows.push(decode(row)?);
        Ok(())
    })?;
    Ok(rows)
}

pub struct Message {
    pub id: String,
    pub space: String,
    pub chat: String,
    pub party: Option<String>,
    pub sender_account: Option<String>,
    pub replies_to: Vec<String>,
}

pub fn message(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<Message>> {
    Ok(query(r, "SELECT m.space_id, s.external_id, m.sender_party_id, m.sender_account_id, m.reply_to, m.thread_key
           FROM chan_messages m JOIN spaces s ON s.id = m.space_id
          WHERE m.id = ? AND m.workspace_id = ? AND m.direction = 'in'", vec![id.into(), ws.into()], |row| {
        Ok(Message {
            id: id.into(), space: row.get(0)?, chat: row.get(1)?,
            party: row.get(2)?, sender_account: row.get(3)?,
            replies_to: [row.get::<Option<String>>(4)?, row.get::<Option<String>>(5)?].into_iter().flatten().collect(),
        })
    })?.into_iter().next())
}

pub fn other_connections(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    conn: &str,
) -> Result<Vec<String>> {
    query(r, "SELECT conn FROM (
           SELECT si.connection_id AS conn FROM space_identities si
            WHERE si.space_id = ?1 AND si.workspace_id = ?2 AND si.left_at IS NULL
           UNION SELECT o.connection_id FROM spaces s JOIN spaces o
               ON o.workspace_id = s.workspace_id AND o.external_id = s.external_id
            WHERE s.id = ?1 AND s.workspace_id = ?2)
          WHERE conn IS NOT NULL AND conn <> ?3 AND EXISTS (SELECT 1 FROM connections c WHERE c.id = conn)
          ORDER BY conn", vec![space.into(), ws.into(), conn.into()], |row| row.get(0))
}

const USER_PARTIES: &str = "SELECT p.id FROM parties p WHERE p.workspace_id = ?1 AND p.user_id = ?2
    UNION SELECT q.id FROM parties q JOIN parties p ON p.id = q.merged_into
     WHERE p.workspace_id = ?1 AND p.user_id = ?2";

pub fn by_user(r: &mut impl Read, ws: &str, user: &str, party: &str) -> Result<bool> {
    Ok(!query(
        r,
        &format!("SELECT id FROM ({USER_PARTIES}) WHERE id = ?3 LIMIT 1"),
        vec![ws.into(), user.into(), party.into()],
        |_| Ok(()),
    )?
    .is_empty())
}

pub fn mentions_user(
    r: &mut impl Read,
    ws: &str,
    user: &str,
    message: &str,
    account: &str,
) -> Result<bool> {
    Ok(!query(
        r,
        &format!(
            "SELECT m.id FROM chan_messages m,
           json_each(CASE WHEN json_valid(m.mentions) THEN m.mentions ELSE '[]' END) j
           JOIN accounts a ON a.id = json_extract(j.value, '$.account_id')
         WHERE m.id = ?3 AND (a.id = ?4 OR a.party_id IN ({USER_PARTIES})) LIMIT 1"
        ),
        vec![ws.into(), user.into(), message.into(), account.into()],
        |_| Ok(()),
    )?
    .is_empty())
}

pub struct ReplyAudience<'a> {
    pub ws: &'a str,
    pub user: &'a str,
    pub chat: &'a str,
    pub account: &'a str,
    pub connection: &'a str,
    pub references: &'a [&'a str],
}

pub fn replied_to_user(r: &mut impl Read, a: &ReplyAudience<'_>) -> Result<bool> {
    Ok(!query(r, &format!(
        "SELECT r.id FROM chan_messages r JOIN spaces s ON s.id = r.space_id
          WHERE s.workspace_id = ?1 AND s.external_id = ?3
            AND r.external_id IN (SELECT value FROM json_each(?4))
            AND ((r.direction = 'in' AND (r.sender_account_id = ?5 OR r.sender_party_id IN ({USER_PARTIES})))
              OR (r.direction = 'out' AND (s.connection_id = ?6 OR EXISTS (
                 SELECT 1 FROM space_identities si WHERE si.space_id = s.id AND si.connection_id = ?6
                    AND si.ordinal IS NOT NULL AND ((r.seen_by >> si.ordinal) & 1) = 1)))) LIMIT 1"
    ), vec![
        a.ws.into(), a.user.into(), a.chat.into(), serde_json::to_string(a.references)?.into(),
        a.account.into(), a.connection.into(),
    ], |_| Ok(()))?.is_empty())
}

pub struct Display {
    pub sender: String,
    pub text: String,
    pub attachments: serde_json::Value,
    pub space: String,
}

pub fn display(r: &mut impl Read, id: &str) -> Result<Option<Display>> {
    Ok(query(
        r,
        "SELECT coalesce(nullif(p.name, ''), nullif(a.display_name, ''), a.handle_display, ''),
                    coalesce(m.text, ''), m.attachments, coalesce(s.name, '')
               FROM chan_messages m JOIN spaces s ON s.id = m.space_id
               LEFT JOIN accounts a ON a.id = m.sender_account_id
               LEFT JOIN parties p ON p.id = m.sender_party_id
              WHERE m.id = ?",
        vec![id.into()],
        |row| {
            Ok(Display {
                sender: row.get(0)?,
                text: row.get(1)?,
                attachments: row
                    .get::<Option<String>>(2)?
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_default(),
                space: row.get(3)?,
            })
        },
    )?
    .into_iter()
    .next())
}

//! Stored evidence for resolving names learned through a channel.

use crate::contract::Row;
use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Query, Values};

fn workspace_rows<T>(
    r: &mut impl Read,
    ws: &str,
    sql: &str,
    mut decode: impl FnMut(&Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let mut rows = Vec::new();
    r.db().query(sql, &Values(vec![ws.into()]), &mut |row| {
        rows.push(decode(row)?);
        Ok(())
    })?;
    Ok(rows)
}

pub fn mentions(r: &mut impl Read, ws: &str) -> Result<Vec<(Option<String>, Option<String>)>> {
    workspace_rows(r, ws, "SELECT json_extract(j.value, '$.account_id'), json_extract(j.value, '$.name')
           FROM chan_messages m, json_each(CASE WHEN json_valid(m.mentions) THEN m.mentions ELSE '[]' END) j
          WHERE m.workspace_id = ? AND json_extract(j.value, '$.account_id') IS NOT NULL
          ORDER BY m.created DESC", |row| Ok((row.get(0)?, row.get(1)?)))
}

pub struct DirectName {
    pub name: String,
    pub account: String,
    pub senders: i64,
}

pub fn direct_names(r: &mut impl Read, ws: &str) -> Result<Vec<DirectName>> {
    workspace_rows(
        r,
        ws,
        "SELECT s.name, min(m.sender_account_id), count(DISTINCT m.sender_account_id)
           FROM spaces s JOIN chan_messages m ON m.space_id = s.id AND m.direction = 'in'
          WHERE s.workspace_id = ? AND s.kind = 'dm' AND m.sender_account_id IS NOT NULL
            AND m.sender_account_id NOT IN (SELECT cn.account_id FROM space_identities si
                  JOIN connections cn ON cn.id = si.connection_id WHERE si.space_id = s.id
                 UNION SELECT cn.account_id FROM connections cn WHERE cn.id = s.connection_id)
          GROUP BY s.id",
        |row| {
            Ok(DirectName {
                name: row.get(0)?,
                account: row.get(1)?,
                senders: row.get(2)?,
            })
        },
    )
}

pub struct TenantEvidence {
    pub account: String,
    pub tenant: Option<String>,
    pub space: String,
    pub party: String,
    pub actor: Option<String>,
}

pub fn sender_tenants(r: &mut impl Read, ws: &str) -> Result<Vec<TenantEvidence>> {
    let rows = workspace_rows(r, ws, "SELECT m.sender_account_id,
                coalesce(json_extract(e.payload, '$.raw.event.sender.tenant_key'), json_extract(e.payload, '$.raw.sender.tenant_key')),
                coalesce(json_extract(e.payload, '$.raw.event.sender.sender_type'), json_extract(e.payload, '$.raw.sender.sender_type')),
                s.name, p.id, p.created_by
           FROM chan_messages m JOIN ext_events e ON e.id = m.event_id JOIN spaces s ON s.id = m.space_id
           JOIN accounts a ON a.id = m.sender_account_id JOIN parties p ON p.id = a.party_id
          WHERE m.workspace_id = ?1 AND m.direction = 'in' AND e.payload IS NOT NULL
            AND a.kind NOT IN ('bot', 'app_bot') AND coalesce(p.contact_kind, '') <> 'bot'
            AND NOT EXISTS (SELECT 1 FROM links l WHERE l.subject_id = a.party_id
                             AND l.predicate = 'works_at' AND l.workspace_id = ?1)
          ORDER BY m.created DESC", |row| Ok((
        row.get::<Option<String>>(2)?,
        TenantEvidence {
            account: row.get(0)?, tenant: row.get(1)?, space: row.get(3)?,
            party: row.get(4)?, actor: row.get(5)?,
        }
    )))?;
    Ok(rows
        .into_iter()
        .filter(|(kind, _)| kind.as_deref() != Some("app"))
        .map(|(_, row)| row)
        .collect())
}

pub fn mentioned_tenants(r: &mut impl Read, ws: &str) -> Result<Vec<TenantEvidence>> {
    workspace_rows(r, ws, "SELECT h.account_id, json_extract(j.value, '$.tenant_key'), s.name, p.id, p.created_by
           FROM chan_messages m JOIN ext_events e ON e.id = m.event_id JOIN spaces s ON s.id = m.space_id,
                json_each(coalesce(json_extract(e.payload, '$.raw.event.message.mentions'), json_extract(e.payload, '$.raw.mentions'), '[]')) j
           JOIN account_handles h ON h.workspace_id = m.workspace_id
            AND h.handle = coalesce(json_extract(j.value, '$.id.open_id'), json_extract(j.value, '$.id'))
           JOIN accounts a ON a.id = h.account_id JOIN parties p ON p.id = a.party_id
          WHERE m.workspace_id = ?1 AND e.payload IS NOT NULL AND json_type(j.value, '$.id') IS NOT NULL
            AND a.kind NOT IN ('bot', 'app_bot') AND coalesce(p.contact_kind, '') <> 'bot'
            AND NOT EXISTS (SELECT 1 FROM links l WHERE l.subject_id = a.party_id
                             AND l.predicate = 'works_at' AND l.workspace_id = ?1)
          ORDER BY m.created DESC", |row| Ok(TenantEvidence {
        account: row.get(0)?, tenant: row.get(1)?, space: row.get(2)?,
        party: row.get(3)?, actor: row.get(4)?,
    }))
}

pub struct SeenSpace {
    pub account: String,
    pub external: String,
    pub name: String,
    pub connection: String,
}

pub fn seen_spaces(r: &mut impl Read, ws: &str) -> Result<Vec<SeenSpace>> {
    workspace_rows(r, ws, "SELECT x.account_id, s.external_id, s.name, coalesce(s.connection_id,
                  (SELECT si.connection_id FROM space_identities si WHERE si.space_id = s.id
                    ORDER BY si.left_at IS NOT NULL, si.joined, si.connection_id LIMIT 1), '')
           FROM (SELECT account_id, space_id FROM space_members WHERE workspace_id = ?1
                 UNION SELECT sender_account_id, space_id FROM chan_messages
                        WHERE workspace_id = ?1 AND direction = 'in' AND sender_account_id IS NOT NULL) x
           JOIN spaces s ON s.id = x.space_id
          WHERE s.status = 'active'
          ORDER BY s.last_message DESC", |row| Ok(SeenSpace {
        account: row.get(0)?, external: row.get(1)?, name: row.get(2)?, connection: row.get(3)?,
    }))
}

pub struct ProfileIdentity {
    pub id: String,
    pub adapter: String,
    pub config: serde_json::Value,
    pub personal: bool,
}

pub fn identities(r: &mut impl Read, ws: &str) -> Result<Vec<ProfileIdentity>> {
    workspace_rows(r, ws, "SELECT cn.id, cn.adapter, cn.config, p.user_id IS NOT NULL
           FROM connections cn JOIN accounts a ON a.id = cn.account_id LEFT JOIN parties p ON p.id = a.party_id
          WHERE cn.workspace_id = ? AND cn.status IN ('ok', 'error')
          ORDER BY cn.created, cn.id", |row| Ok(ProfileIdentity {
        id: row.get(0)?, adapter: row.get(1)?,
        config: row.get::<Option<String>>(2)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        personal: row.get::<i64>(3)? != 0,
    }))
}

pub fn has_job(r: &mut impl Read, ws: &str, account: &str) -> Result<bool> {
    let joined = Query::select()
        .column(("l", "id"))
        .from_as("links", "l")
        .inner_join(
            "accounts",
            Expr::col(("accounts", "party_id")).equals(("l", "subject_id")),
        )
        .and_where(Expr::col(("accounts", "id")).eq(account))
        .and_where(Expr::col(("l", "workspace_id")).eq(ws))
        .and_where(Expr::col(("l", "predicate")).eq("works_at"))
        .limit(1)
        .take();
    Ok(r.one(&joined, |_| Ok(()))?.is_some())
}

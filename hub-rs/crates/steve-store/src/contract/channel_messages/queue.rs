//! Durable candidate selection for the channel send queue and approval maintenance.

use sea_query::{Value, Values};

use crate::contract::Row;
use crate::{Read, Result};

const QUEUE_CANDIDATES: &str = "WITH candidates AS (
    SELECT m.id, d.id AS decision, m.created, NULL AS first_at,
      max(json_extract(d.result,'$.send_at'),coalesce(m.sent+?1+1,0),
        coalesce((SELECT max(n.next_attempt_at) FROM personal_notifications n
          JOIN output_deliveries od ON od.id=n.delivery_id WHERE od.message_id=m.id),0),
        coalesce((SELECT value FROM json_each(?4) WHERE key=m.id),0)) AS at
    FROM chan_messages m JOIN decisions d ON d.id=m.decision_id
    WHERE m.direction='out' AND m.status='pending' AND d.status='resolved'
      AND d.subject_type=?2 AND d.subject_id=m.id AND json_extract(d.result,'$.send_at') IS NOT NULL
    UNION ALL
    SELECT m.id, NULL, m.created, s.first_at,
      max(CASE WHEN m.sent IS NULL THEN m.created+?3+1 ELSE m.sent+?1+1 END,
        CASE WHEN s.first_at IS NOT NULL THEN 0 ELSE coalesce((
          SELECT max(n.next_attempt_at) FROM personal_notifications n JOIN output_deliveries od ON od.id=n.delivery_id
          JOIN notification_policies p ON p.workspace_id=n.workspace_id AND p.user_id=n.user_id
          WHERE od.message_id=m.id AND p.enabled=1),0) END,
        coalesce((SELECT value FROM json_each(?4) WHERE key=m.id),0))
    FROM chan_messages m JOIN chan_send_state s ON s.message_id=m.id
    WHERE m.direction='out' AND m.status='pending' AND m.decision_id IS NULL
      AND (s.first_at IS NOT NULL OR NOT EXISTS (
        SELECT 1 FROM personal_notifications n JOIN output_deliveries od ON od.id=n.delivery_id
        JOIN notification_policies p ON p.workspace_id=n.workspace_id AND p.user_id=n.user_id
        WHERE od.message_id=m.id AND p.enabled=1 AND (p.mode='log'
          OR (p.mode='digest' AND (n.topic<>'contact.starred' OR p.space_id IS NULL)))))
) ";

fn rows<T>(
    r: &mut impl Read,
    sql: &str,
    values: Vec<Value>,
    mut decode: impl FnMut(&Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let mut result = Vec::new();
    r.db().query(sql, &Values(values), &mut |row| {
        result.push(decode(row)?);
        Ok(())
    })?;
    Ok(result)
}

pub struct Scan<'a> {
    pub claim_stale_ms: i64,
    pub subject: &'a str,
    pub unclaimed_ms: i64,
    pub deferred: &'a str,
}

impl Scan<'_> {
    fn args(&self) -> Vec<Value> {
        vec![
            self.claim_stale_ms.into(),
            self.subject.into(),
            self.unclaimed_ms.into(),
            self.deferred.into(),
        ]
    }

    pub fn next_at(&self, r: &mut impl Read) -> Result<Option<i64>> {
        Ok(rows(
            r,
            &format!("{QUEUE_CANDIDATES} SELECT min(at) FROM candidates"),
            self.args(),
            |row| row.get::<Option<i64>>(0),
        )?
        .into_iter()
        .next()
        .flatten())
    }

    pub fn approved(&self, r: &mut impl Read, now: i64) -> Result<Vec<(String, String)>> {
        let mut args = self.args();
        args.push(now.into());
        rows(r, &format!(
            "{QUEUE_CANDIDATES} SELECT id, decision FROM candidates WHERE decision IS NOT NULL AND at<=?5
                ORDER BY at,created,id LIMIT 50"
        ), args, |row| Ok((row.get(0)?, row.get(1)?)))
    }

    pub fn direct(&self, r: &mut impl Read, now: i64) -> Result<Vec<Direct>> {
        let mut args = self.args();
        args.push(now.into());
        rows(r, &format!(
            "{QUEUE_CANDIDATES} SELECT id,created,first_at FROM candidates WHERE decision IS NULL AND at<=?5
                ORDER BY at,created,id LIMIT 50"
        ), args, |row| Ok(Direct {
            id: row.get(0)?, created: row.get(1)?, first_at: row.get(2)?,
        }))
    }
}

pub struct Direct {
    pub id: String,
    pub created: i64,
    pub first_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct StaleCard {
    pub id: String,
    pub ws: String,
    pub kind: String,
    pub subject: String,
    pub space: String,
    pub created: i64,
}

pub struct Expiry<'a> {
    pub now: i64,
    pub starred: &'a str,
    pub commitment: &'a str,
    pub commits: &'a str,
    pub after: Option<(i64, &'a str)>,
    pub limit: i64,
}

pub fn stale_cards(r: &mut impl Read, filter: &Expiry<'_>) -> Result<Vec<StaleCard>> {
    let (after, id) = filter.after.unwrap_or((i64::MIN, ""));
    rows(r, "SELECT d.id, d.workspace_id, d.kind, d.subject_id, m.space_id, d.created
           FROM decisions d INDEXED BY decisions_chan_open
           JOIN chan_messages m ON m.id = d.subject_id
           JOIN spaces s ON s.id = m.space_id
           JOIN channel_settings cs ON cs.connection_id = s.connection_id AND cs.workspace_id = d.workspace_id
          WHERE d.subject_type = 'chan_message' AND d.status = 'open'
            AND cs.mode = 'self' AND cs.card_ttl_h IS NOT NULL
            AND d.created < ?1 - cs.card_ttl_h * 3600000
            AND ((d.kind = 'ask' AND m.direction = 'in')
              OR (d.kind = 'approve' AND m.direction = 'out' AND m.status = 'draft' AND m.reply_to IS NOT NULL
                  AND m.on_behalf = 'none' AND m.persona_id IS NULL))
            AND substr(d.question, 1, length(?2)) <> ?2
            AND instr(d.question, ?3) = 0 AND instr(d.question, ?4) = 0
            AND (d.created > ?5 OR (d.created = ?5 AND d.id > ?6))
          ORDER BY d.created, d.id LIMIT ?7", vec![
        filter.now.into(), filter.starred.into(), filter.commitment.into(), filter.commits.into(),
        after.into(), id.into(), filter.limit.into(),
    ], |row| Ok(StaleCard {
        id: row.get(0)?, ws: row.get(1)?, kind: row.get(2)?,
        subject: row.get(3)?, space: row.get(4)?, created: row.get(5)?,
    }))
}

pub fn instructions(
    r: &mut impl Read,
    since: i64,
    choice: &str,
) -> Result<Vec<(String, String, String, String)>> {
    rows(r, "SELECT workspace_id, id, subject_id, coalesce(json_extract(result, '$.text'), '')
           FROM decisions INDEXED BY decisions_chan_answered
          WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'resolved' AND resolved >= ?
            AND json_extract(result, '$.choice') = ? AND json_extract(result, '$.composed') IS NULL
          ORDER BY resolved LIMIT 20", vec![since.into(), choice.into()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
}

pub struct Chosen {
    pub ws: String,
    pub decision: String,
    pub message: String,
    pub draft: String,
    pub by: String,
}

pub fn chosen_drafts(r: &mut impl Read, since: i64, choice: &str) -> Result<Vec<Chosen>> {
    rows(r, "SELECT workspace_id, id, subject_id, draft, coalesce(json_extract(result, '$.by'), 'steve')
           FROM decisions INDEXED BY decisions_chan_answered
          WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'resolved' AND resolved >= ?
            AND json_extract(result, '$.choice') = ? AND json_extract(result, '$.composed') IS NULL
          ORDER BY resolved LIMIT 20", vec![since.into(), choice.into()], |row| Ok(Chosen {
        ws: row.get(0)?, decision: row.get(1)?, message: row.get(2)?,
        draft: row.get(3)?, by: row.get(4)?,
    }))
}

pub fn rewrites(r: &mut impl Read, subject: &str, choice: &str) -> Result<Vec<(String, String)>> {
    rows(r, "SELECT m.id, d.id FROM chan_messages m JOIN decisions d ON d.id = m.decision_id
          WHERE m.direction = 'out' AND m.status = 'draft' AND d.status = 'resolved' AND d.subject_type = ?
            AND d.subject_id = m.id AND json_extract(d.result, '$.choice') = ?
          ORDER BY m.created LIMIT 20", vec![subject.into(), choice.into()], |row| Ok((row.get(0)?, row.get(1)?)))
}

pub struct Unknown {
    pub id: String,
    pub workspace: String,
    pub connection: String,
    pub adapter: String,
    pub first_at: Option<i64>,
}

pub fn unknown(r: &mut impl Read, subject: &str) -> Result<Vec<Unknown>> {
    rows(r, "SELECT m.id, m.workspace_id, c.id, c.adapter, s.first_at FROM chan_messages m
                   JOIN spaces sp ON sp.id = m.space_id JOIN connections c ON c.id = sp.connection_id
                   LEFT JOIN chan_send_state s ON s.message_id = m.id
                  WHERE m.direction = 'out' AND m.status = 'unknown'
                    AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.subject_type = ?1
                                      AND d.subject_id = m.id AND d.status = 'open')
                  ORDER BY coalesce((SELECT max(n.next_attempt_at) FROM personal_notifications n
                    JOIN output_deliveries od ON od.id=n.delivery_id WHERE od.message_id=m.id),0),m.created LIMIT 50", vec![subject.into()], |row| Ok(Unknown {
        id: row.get(0)?, workspace: row.get(1)?, connection: row.get(2)?,
        adapter: row.get(3)?, first_at: row.get(4)?,
    }))
}

pub fn item_conversions(r: &mut impl Read) -> Result<Vec<(String, String, String)>> {
    rows(
        r,
        "SELECT id, workspace_id, json_extract(result, '$.source_message') FROM decisions
                  WHERE subject_type = 'chan_message' AND status = 'resolved'
                    AND json_extract(result, '$.choice') = 'to_item'
                    AND json_extract(result, '$.source_message') IS NOT NULL
                    AND json_extract(result, '$.item') IS NULL ORDER BY resolved LIMIT 50",
        vec![],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
}

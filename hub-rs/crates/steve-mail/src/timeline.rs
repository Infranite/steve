//! The contact page's “往来”: mail from a party
//! or to it, newest first, only in threads the viewer may see. Mail recorded under a contact that was
//! later merged into this one counts too.

use rusqlite::Connection;
use rusqlite::types::Value as Sql;
use steve_modules::ApiResult;
use steve_modules::contacts::{TimelineEntry, TimelineSource};
use steve_store::base::{Viewer, visible_sql};

/// Kind `mail`.
pub struct MailTimeline;

impl TimelineSource for MailTimeline {
    fn entries(
        &self,
        c: &Connection,
        viewer: &Viewer,
        party: &str,
        before: Option<i64>,
        limit: usize,
    ) -> ApiResult<Vec<TimelineEntry>> {
        let vis = visible_sql("t", viewer);
        let sql = format!(
            "WITH RECURSIVE ids(id) AS (SELECT ?1 UNION SELECT p.id FROM parties p JOIN ids ON p.merged_into = ids.id)
             SELECT m.thread_id, m.date, m.direction, m.subject FROM mail_messages m
               JOIN mail_threads t ON t.id = m.thread_id
              WHERE m.workspace_id = ?2 AND (?3 IS NULL OR m.date < ?3)
                AND (m.from_party_id IN (SELECT id FROM ids)
                  OR EXISTS (SELECT 1 FROM json_each(m.to_addrs) j WHERE json_extract(j.value, '$.party_id') IN (SELECT id FROM ids))
                  OR EXISTS (SELECT 1 FROM json_each(m.cc_addrs) j WHERE json_extract(j.value, '$.party_id') IN (SELECT id FROM ids)))
                AND {}
              ORDER BY m.date DESC LIMIT ?4",
            renumber(&vis.sql, 5)
        );
        let mut ps = vec![
            Sql::Text(party.into()),
            Sql::Text(viewer.ws.clone()),
            before.map(Sql::Integer).unwrap_or(Sql::Null),
            Sql::Integer(limit as i64),
        ];
        ps.extend(vis.params);
        let mut st = c.prepare(&sql).map_err(steve_store::StoreError::from)?;
        let rows = st
            .query_map(steve_store::base::bind(ps), |r| {
                let (thread, ts, dir, subject): (String, i64, String, String) =
                    (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                let subject = if subject.is_empty() {
                    steve_i18n::t!("mail.subject.none")
                } else {
                    subject
                };
                Ok(TimelineEntry {
                    ts,
                    kind: "mail".into(),
                    text: if dir == "out" {
                        steve_i18n::t!("mail.timeline.sent", subject = subject)
                    } else {
                        steve_i18n::t!("mail.timeline.received", subject = subject)
                    },
                    href: format!("#/mail/t/{thread}"),
                })
            })
            .map_err(steve_store::StoreError::from)?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(steve_store::StoreError::from)?)
    }
}

/// Positional `?` → numbered `?n` from `start` (the scope uses bare `?`, the query numbered ones).
fn renumber(sql: &str, start: usize) -> String {
    let mut out = String::new();
    let mut n = start;
    for ch in sql.chars() {
        if ch == '?' {
            out.push_str(&format!("?{n}"));
            n += 1;
        } else {
            out.push(ch);
        }
    }
    out
}

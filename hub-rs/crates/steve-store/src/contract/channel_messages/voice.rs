//! The owner's channel messages and draft outcomes used to learn their voice.

use sea_query::{Expr, ExprTrait, JoinType, Order, Query};

use crate::{Read, Result};

use super::{dialogue::SAID_AT, presence::owner_view};

pub struct Sample {
    pub id: String,
    pub at: i64,
    pub space: String,
    pub dm: bool,
    pub text: String,
    pub media: bool,
    pub other_before: bool,
    pub mentioned: bool,
    pub reply_to_me: bool,
    pub mentions: Vec<String>,
}

pub fn samples(
    r: &mut impl Read,
    ws: &str,
    conn: &str,
    space: &str,
    before: i64,
    rank_dm: Option<bool>,
    limit: i64,
) -> Result<Vec<Sample>> {
    let order = match rank_dm {
        Some(_) => {
            "CASE WHEN m.space_id = ?2 THEN 0 WHEN (s.kind = 'dm') = ?6 THEN 1 ELSE 2 END, m.created DESC, m.id DESC"
        }
        None => "m.created DESC, m.id DESC",
    };
    let sql = format!(
        "SELECT m.id, {SAID_AT}, m.space_id, s.kind = 'dm', coalesce(m.text, ''),
                coalesce(json_array_length(m.attachments), 0) > 0,
                p.id IS NOT NULL AND NOT (coalesce(p.sender_party_id IN (SELECT id FROM mine), 0)
                                          OR coalesce(p.sender_account_id IN (SELECT id FROM my_accounts), 0)),
                coalesce(json_extract(e.payload, '$.facts.mentioned'), 0),
                coalesce(json_extract(e.payload, '$.facts.reply_to_me'), 0),
                coalesce(p.mentions, '[]')
         FROM chan_messages m JOIN spaces s ON s.id = m.space_id
         LEFT JOIN chan_messages p ON p.id = (
              SELECT q.id FROM chan_messages q WHERE q.space_id = m.space_id
                 AND (q.created < m.created OR (q.created = m.created AND q.id < m.id))
               ORDER BY q.created DESC, q.id DESC LIMIT 1)
         LEFT JOIN ext_events e ON e.id = p.event_id
         WHERE m.workspace_id = ?1 AND m.sender_party_id IN (SELECT id FROM mine)
           AND s.connection_id = ?3 AND m.direction = 'in' AND m.status IN ('received', 'sent')
           AND m.created < ?4 AND m.text IS NOT NULL AND m.text <> ''
         ORDER BY {order} LIMIT ?5"
    );
    let args = [conn.into(), before.into(), limit.into()]
        .into_iter()
        .chain(rank_dm.map(|dm| i64::from(dm).into()));
    owner_view(r, ws, space, &sql, args, |row| {
        let raw = row.get::<String>(9)?;
        let mentions = serde_json::from_str::<Vec<serde_json::Value>>(&raw)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m["account_id"].as_str().map(String::from))
            .collect();
        Ok(Sample {
            id: row.get(0)?,
            at: row.get(1)?,
            space: row.get(2)?,
            dm: row.get(3)?,
            text: row.get(4)?,
            media: row.get(5)?,
            other_before: row.get(6)?,
            mentioned: row.get::<i64>(7)? != 0,
            reply_to_me: row.get::<i64>(8)? != 0,
            mentions,
        })
    })
}

pub fn people_names(r: &mut impl Read, ws: &str, conn: &str) -> Result<Vec<String>> {
    let spaces = Query::select()
        .from_as("spaces", "s")
        .and_where(Expr::col(("s", "workspace_id")).eq(ws))
        .and_where(Expr::col(("s", "connection_id")).eq(conn))
        .take();
    let members = r.all(
        spaces
            .clone()
            .columns([("a", "display_name"), ("p", "name"), ("p", "aliases")])
            .join_as(
                JoinType::InnerJoin,
                "space_members",
                "sm",
                Expr::col(("sm", "space_id")).equals(("s", "id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).equals(("sm", "account_id")),
            )
            .join_as(
                JoinType::LeftJoin,
                "parties",
                "p",
                Expr::col(("p", "id")).equals(("a", "party_id")),
            ),
        |row| {
            let aliases = row
                .get::<Option<String>>(2)?
                .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
                .unwrap_or_default();
            let mut names: Vec<_> = aliases
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            names.extend(
                [row.get::<Option<String>>(0)?, row.get::<Option<String>>(1)?]
                    .into_iter()
                    .flatten(),
            );
            Ok(names)
        },
    )?;
    let mut result: Vec<_> = members.into_iter().flatten().collect();
    result.extend(
        r.all(
            spaces
                .clone()
                .column(("s", "name"))
                .and_where(Expr::col(("s", "kind")).eq("dm")),
            |row| row.get::<String>(0),
        )?,
    );
    Ok(result)
}

pub struct Example {
    pub draft: String,
    pub choice: String,
    pub text: String,
    pub message: String,
    pub space: String,
    pub created: i64,
    pub resolved: i64,
}

pub fn examples(
    r: &mut impl Read,
    ws: &str,
    conn: &str,
    space: &str,
    subject: &str,
    kind: &str,
    limit: u64,
) -> Result<Vec<Example>> {
    let dialect = r.db().dialect();
    let field = |key| dialect.json_field(Expr::col(("d", "result")), key);
    r.all(
        Query::select()
            .column(("d", "draft"))
            .expr(field("choice"))
            .expr(field("text"))
            .columns([
                ("m", "id"),
                ("m", "space_id"),
                ("d", "created"),
                ("d", "resolved"),
            ])
            .from_as("decisions", "d")
            .join_as(
                JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "id")).equals(("d", "subject_id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "spaces",
                "s",
                Expr::col(("s", "id")).equals(("m", "space_id")),
            )
            .and_where(Expr::col(("d", "workspace_id")).eq(ws))
            .and_where(Expr::col(("d", "subject_type")).eq(subject))
            .and_where(Expr::col(("d", "kind")).eq(kind))
            .and_where(Expr::col(("d", "status")).eq("resolved"))
            .and_where(field("decided_by").is_null())
            .and_where(Expr::col(("s", "connection_id")).eq(conn))
            .and_where(Expr::col(("m", "persona_id")).is_null())
            .and_where(Expr::col(("m", "direction")).eq("out"))
            .and_where(Expr::col(("m", "on_behalf")).eq("none"))
            .and_where(field("choice").is_in(["yes", "no"]))
            .order_by_expr(Expr::col(("m", "space_id")).eq(space), Order::Desc)
            .order_by(("d", "resolved"), Order::Desc)
            .limit(limit),
        |row| {
            let created = row.get(5)?;
            Ok(Example {
                draft: row.get(0)?,
                choice: row.get(1)?,
                text: row.get::<Option<String>>(2)?.unwrap_or_default(),
                message: row.get(3)?,
                space: row.get(4)?,
                created,
                resolved: row.get::<Option<i64>>(6)?.unwrap_or(created),
            })
        },
    )
}

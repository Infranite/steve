//! Storage facts for participation in a channel conversation.

use sea_query::{Expr, ExprTrait, Query, Value, Values};

use crate::contract::Row;
use crate::{Read, Result};

const MINE: &str = "WITH RECURSIVE mine(id) AS (
       SELECT p.id FROM parties p JOIN workspace_members wm
           ON wm.user_id = p.user_id AND wm.workspace_id = ?1 AND wm.role = 'owner'
        WHERE p.workspace_id = ?1
       UNION SELECT a.party_id FROM spaces s JOIN connections c ON c.id = s.connection_id
           JOIN accounts a ON a.id = c.account_id
           JOIN channel_settings cs ON cs.connection_id = c.id AND cs.mode = 'self'
        WHERE s.id = ?2
       UNION SELECT p.id FROM parties p JOIN mine ON p.merged_into = mine.id
     ), my_accounts(id) AS (SELECT a.id FROM accounts a WHERE a.party_id IN (SELECT id FROM mine))";

const IS_MINE: &str = "(m.sender_party_id IN (SELECT id FROM mine)
    OR m.sender_account_id IN (SELECT id FROM my_accounts))";

pub(super) fn owner_view<T>(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    tail: &str,
    args: impl IntoIterator<Item = Value>,
    mut decode: impl FnMut(&Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let values = Values([ws.into(), space.into()].into_iter().chain(args).collect());
    let mut rows = Vec::new();
    r.db()
        .query(&format!("{MINE} {tail}"), &values, &mut |row| {
            rows.push(decode(row)?);
            Ok(())
        })?;
    Ok(rows)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mine {
    pub parties: Vec<String>,
    pub accounts: Vec<String>,
}

pub fn mine(r: &mut impl Read, ws: &str, space: &str) -> Result<Mine> {
    Ok(Mine {
        parties: owner_view(r, ws, space, "SELECT id FROM mine", [], |row| row.get(0))?,
        accounts: accounts(r, ws, space)?,
    })
}

pub fn accounts(r: &mut impl Read, ws: &str, space: &str) -> Result<Vec<String>> {
    owner_view(r, ws, space, "SELECT id FROM my_accounts", [], |row| {
        row.get(0)
    })
}

pub struct Line {
    pub id: String,
    pub at: i64,
    pub owner: bool,
    pub sender: String,
    pub thread: Option<String>,
    pub external: Option<String>,
    pub reply_to: Option<String>,
    pub mentions: Vec<String>,
    pub sender_account: Option<String>,
    pub text: String,
    pub answered: bool,
}

fn line(row: &Row<'_>) -> Result<Line> {
    let mentions = row
        .get::<Option<String>>(7)?
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["account_id"].as_str().map(String::from))
        .collect();
    Ok(Line {
        id: row.get(0)?,
        at: row.get(1)?,
        owner: row.get(2)?,
        sender: row.get(3)?,
        thread: row.get(4)?,
        external: row.get(5)?,
        reply_to: row.get(6)?,
        mentions,
        sender_account: row.get(8)?,
        text: row.get(9)?,
        answered: row.get(10)?,
    })
}

// The shared send states where a reply already owns the source message.
const ANSWERED: [&str; 4] = ["pending", "sent", "unknown", "recalled"];

fn line_columns() -> String {
    format!("m.id, m.created, {IS_MINE},
       coalesce(m.sender_party_id, m.sender_account_id, ''), m.thread_key, m.external_id, m.reply_to, m.mentions,
       m.sender_account_id, coalesce(m.text, ''),
       EXISTS (SELECT 1 FROM chan_messages o WHERE o.space_id = m.space_id AND o.direction = 'out'
                  AND o.reply_to = m.external_id AND o.status IN ('{}', '{}', '{}', '{}')
                  AND o.on_behalf = 'none' AND o.persona_id IS NULL
                  AND o.sender_account_id IN (SELECT id FROM my_accounts))",
        ANSWERED[0], ANSWERED[1], ANSWERED[2], ANSWERED[3])
}

pub fn line_for(r: &mut impl Read, ws: &str, space: &str, id: &str) -> Result<Option<Line>> {
    let sql = format!(
        "SELECT {} FROM chan_messages m WHERE m.id = ?3",
        line_columns()
    );
    Ok(owner_view(r, ws, space, &sql, [id.into()], line)?.pop())
}

pub fn lines(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    from: i64,
    to: i64,
    limit: i64,
) -> Result<Vec<Line>> {
    let order = match r.db().dialect() {
        crate::contract::Dialect::Sqlite => "m.rowid",
        #[cfg(test)]
        crate::contract::Dialect::Postgres => unimplemented!("PostgreSQL presence ordering"),
    };
    let sql = format!(
        "SELECT {} FROM chan_messages m
          WHERE m.space_id = ?2 AND m.direction = 'in' AND m.created >= ?3 AND m.created <= ?4
          ORDER BY m.created, {order} LIMIT ?5",
        line_columns()
    );
    owner_view(
        r,
        ws,
        space,
        &sql,
        [from.into(), to.into(), limit.into()],
        line,
    )
}

pub fn own_words(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    from: i64,
    to: i64,
) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT coalesce(m.text, '') FROM chan_messages m
          WHERE m.space_id = ?2 AND m.direction = 'in' AND m.created >= ?3 AND m.created <= ?4
            AND {IS_MINE}"
    );
    owner_view(r, ws, space, &sql, [from.into(), to.into()], |row| {
        row.get(0)
    })
}

pub fn turn_of(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("turn_of")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("direction").eq("in")),
        |row| row.get(0),
    )?
    .flatten())
}

pub enum Supersede<'a> {
    Before(i64),
    Messages(&'a [String]),
    All,
}

pub struct PendingAnswer {
    pub message: String,
    pub status: String,
    pub source: String,
}

pub struct PendingHandoff {
    pub decision: String,
    pub source: String,
}

pub fn supersession(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    filter: Supersede<'_>,
    subject: &str,
) -> Result<(Vec<PendingAnswer>, Vec<PendingHandoff>)> {
    let (cond, arg): (&str, Value) = match filter {
        Supersede::Before(at) => ("src.created < ?3", at.into()),
        Supersede::Messages(ids) => (
            "src.id IN (SELECT value FROM json_each(?3))",
            serde_json::to_string(ids)?.into(),
        ),
        Supersede::All => ("?3 IS NOT NULL", 1_i64.into()),
    };
    let drafts = format!(
        "SELECT o.id, o.status, src.id FROM chan_messages o
           JOIN chan_messages src ON src.space_id = o.space_id AND src.direction = 'in'
                AND src.external_id = o.reply_to
           JOIN chan_messages m ON m.id = src.id
          WHERE o.space_id = ?2 AND o.direction = 'out' AND o.status IN ('draft', 'failed')
            AND o.on_behalf = 'none' AND o.persona_id IS NULL
            AND o.sender_account_id IN (SELECT id FROM my_accounts)
            AND NOT {IS_MINE} AND {cond}"
    );
    let asks = format!(
        "SELECT d.id, src.id FROM decisions d
           JOIN chan_messages src ON src.id = d.subject_id AND src.direction = 'in'
           JOIN chan_messages m ON m.id = src.id
          WHERE d.workspace_id = ?1 AND d.subject_type = ?4 AND d.kind = 'ask'
            AND d.status = 'open' AND src.space_id = ?2 AND NOT {IS_MINE} AND {cond}"
    );
    Ok((
        owner_view(r, ws, space, &drafts, [arg.clone()], |row| {
            Ok(PendingAnswer {
                message: row.get(0)?,
                status: row.get(1)?,
                source: row.get(2)?,
            })
        })?,
        owner_view(r, ws, space, &asks, [arg, subject.into()], |row| {
            Ok(PendingHandoff {
                decision: row.get(0)?,
                source: row.get(1)?,
            })
        })?,
    ))
}

pub fn another_senders_message(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    external: &str,
) -> Result<Option<String>> {
    let sql = format!(
        "SELECT m.id, {IS_MINE} FROM chan_messages m
          WHERE m.space_id = ?2 AND m.direction = 'in' AND m.external_id = ?3"
    );
    let src = owner_view(r, ws, space, &sql, [external.into()], |row| {
        Ok((row.get::<String>(0)?, row.get::<bool>(1)?))
    })?;
    Ok(src
        .into_iter()
        .next()
        .filter(|(_, mine)| !mine)
        .map(|(id, _)| id))
}

pub fn inbound_location(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, String)>> {
    r.one(
        Query::select()
            .columns([("m", "space_id"), ("s", "kind")])
            .from_as("chan_messages", "m")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "spaces",
                "s",
                Expr::col(("s", "id")).equals(("m", "space_id")),
            )
            .and_where(Expr::col(("m", "id")).eq(id))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("m", "direction")).eq("in")),
        |row| {
            Ok((
                row.get(0)?,
                row.get::<Option<String>>(1)?.unwrap_or_default(),
            ))
        },
    )
}

fn sender() -> Expr {
    sea_query::Func::coalesce([
        Expr::col(("m", "sender_party_id")),
        Expr::col(("m", "sender_account_id")),
        Expr::cust("''"),
    ])
    .into()
}

pub fn previous_words(
    r: &mut impl Read,
    space: &str,
    who: &str,
    from: i64,
    before: i64,
) -> Result<Vec<(String, i64)>> {
    r.all(
        Query::select()
            .columns([("m", "text"), ("m", "created")])
            .from_as("chan_messages", "m")
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("m", "direction")).eq("in"))
            .and_where(sender().eq(who))
            .and_where(Expr::col(("m", "created")).lt(before))
            .and_where(Expr::col(("m", "created")).gte(from))
            .order_by(("m", "created"), sea_query::Order::Desc)
            .limit(10),
        |row| {
            Ok((
                row.get::<Option<String>>(0)?.unwrap_or_default(),
                row.get(1)?,
            ))
        },
    )
}

pub fn previous_handoff(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    who: &str,
    before: i64,
    subject: &str,
) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column(("d", "id"))
            .from_as("decisions", "d")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "id")).equals(("d", "subject_id")),
            )
            .and_where(Expr::col(("m", "direction")).eq("in"))
            .and_where(Expr::col(("d", "workspace_id")).eq(ws))
            .and_where(Expr::col(("d", "subject_type")).eq(subject))
            .and_where(Expr::col(("d", "kind")).eq("ask"))
            .and_where(Expr::col(("d", "status")).eq("open"))
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(sender().eq(who))
            .and_where(Expr::col(("m", "created")).lt(before))
            .order_by(("d", "created"), sea_query::Order::Desc)
            .limit(1),
        |row| row.get(0),
    )
}

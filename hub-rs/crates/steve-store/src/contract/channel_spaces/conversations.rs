//! Conversation list projections and per-user read state.

use sea_query::{Expr, ExprTrait, Func, OnConflict, Order, Query, Values};

use crate::{Read, Result, W};

const LIST: &str =
        "WITH grouped AS (
          SELECT space, count(*) AS n, max(created) AS last FROM messages
          WHERE workspace_id = ?1 AND space LIKE 'chan:%' GROUP BY space
        ), base AS (
          SELECT s.id, s.name, s.system_id, g.space AS conv, g.n, g.last AS conv_last,
            (SELECT m.id FROM chan_messages m WHERE m.space_id = s.id
              ORDER BY m.created DESC, m.id DESC LIMIT 1) AS chan_id
          FROM spaces s LEFT JOIN grouped g ON g.space = 'chan:' || s.id
          WHERE s.workspace_id = ?1
            AND (?7 = '' OR s.connection_id = ?7 OR EXISTS (SELECT 1 FROM space_identities li
              WHERE li.space_id = s.id AND li.connection_id = ?7 AND li.left_at IS NULL))
            AND (?8 = 0 OR EXISTS (SELECT 1 FROM chan_messages m
              WHERE m.workspace_id = ?1 AND m.space_id = s.id AND m.direction = 'out'
                AND (m.status = 'sent' OR (m.status = 'draft' AND EXISTS (
                  SELECT 1 FROM decisions d WHERE d.id = m.decision_id AND d.workspace_id = ?1
                    AND d.kind = 'approve' AND d.status = 'open'
                    AND d.subject_type = 'chan_message' AND d.subject_id = m.id)))))
        ), joined AS (
          SELECT b.*, cm.created AS chan_last, coalesce(cm.text, '') AS chan_text, cm.direction AS chan_dir
          FROM base b LEFT JOIN chan_messages cm ON cm.id = b.chan_id
          WHERE b.conv IS NOT NULL OR b.chan_id IS NOT NULL
        ), visible AS (
          SELECT j.id, 'chan:' || j.id AS space, coalesce(j.n, 0) AS n,
            max(coalesce(j.conv_last, 0), coalesce(j.chan_last, 0)) AS last,
            coalesce(p.closed, 0) AS closed, p.read_up_to AS read_up_to, j.name, j.system_id,
            coalesce((SELECT nullif(name, '') FROM systems WHERE id = CASE WHEN j.system_id LIKE '%_app'
              THEN substr(j.system_id, 1, length(j.system_id)-4) ELSE j.system_id END),
              (SELECT nullif(name, '') FROM systems WHERE id = j.system_id), j.system_id) AS provider,
            CASE WHEN coalesce(j.chan_last, 0) > coalesce(j.conv_last, 0) THEN j.chan_text
              ELSE (SELECT text FROM messages WHERE workspace_id = ?1 AND space = j.conv
                ORDER BY created DESC, rowid DESC LIMIT 1) END AS last_text,
            CASE WHEN coalesce(j.chan_last, 0) > coalesce(j.conv_last, 0)
              THEN CASE j.chan_dir WHEN 'out' THEN 'steve' ELSE 'me' END
              ELSE (SELECT author FROM messages WHERE workspace_id = ?1 AND space = j.conv
                ORDER BY created DESC, rowid DESC LIMIT 1) END AS last_author
          FROM joined j
          LEFT JOIN conversation_preferences p ON p.workspace_id = ?1 AND p.user_id = ?2 AND p.space_id = j.id
        ), filtered AS (
          SELECT * FROM visible WHERE (?3 = -1 OR closed = ?3)
          AND (?4 = '' OR instr(lower(coalesce(name, '') || ' ' || system_id || ' ' || provider || ' ' || coalesce(last_text, '')), lower(?4)) > 0)
        )";

const ATTENTION: &str =
        "SELECT s.id,s.kind,p.read_up_to FROM spaces s
         LEFT JOIN conversation_preferences p ON p.workspace_id=s.workspace_id AND p.user_id=?2 AND p.space_id=s.id
         LEFT JOIN space_prefs pref ON pref.space_id=s.id AND pref.workspace_id=s.workspace_id
         WHERE s.workspace_id=?1 AND s.status='active' AND coalesce(p.closed,0)=0
           AND coalesce(pref.attention,'realtime')<>'blocked'
           AND (EXISTS (SELECT 1 FROM space_identities si JOIN connections cn ON cn.id=si.connection_id
             WHERE si.space_id=s.id AND si.left_at IS NULL AND cn.workspace_id=?1
               AND cn.status IN ('ok','error') AND EXISTS(SELECT 1 FROM json_each(cn.caps) cap WHERE cap.value='msg.receive')
               AND (si.owner_user IS NULL OR si.owner_user=?2))
             OR (NOT EXISTS (SELECT 1 FROM space_identities si WHERE si.space_id=s.id)
               AND EXISTS (SELECT 1 FROM connections cn WHERE cn.id=s.connection_id AND cn.workspace_id=?1
                 AND cn.status IN ('ok','error') AND EXISTS(SELECT 1 FROM json_each(cn.caps) cap WHERE cap.value='msg.receive'))))";

pub struct Filter<'a> {
    pub workspace: &'a str,
    pub user: &'a str,
    pub closed: Option<bool>,
    pub replied: bool,
    pub search: &'a str,
    pub connection: &'a str,
    pub limit: Option<i64>,
    pub offset: i64,
}

pub struct Conversation {
    pub id: String,
    pub count: i64,
    pub last: i64,
    pub closed: bool,
    pub text: String,
    pub author: String,
    pub read_up_to: Option<i64>,
}

pub struct Page {
    pub total: i64,
    pub rows: Vec<Conversation>,
}

pub fn list(r: &mut impl Read, filter: &Filter<'_>) -> Result<Page> {
    let args = [
        filter.workspace.into(),
        filter.user.into(),
        filter.closed.map_or(-1, i64::from).into(),
        filter.search.into(),
        filter.limit.unwrap_or(-1).into(),
        filter.offset.into(),
        filter.connection.into(),
        i64::from(filter.replied).into(),
    ];
    let mut total = 0;
    r.db().query(
        &format!("{LIST} SELECT count(*) FROM filtered"),
        &Values(args.to_vec()),
        &mut |row| {
            total = row.get(0)?;
            Ok(())
        },
    )?;
    let mut rows = Vec::new();
    r.db().query(
        &format!("{LIST} SELECT id,n,last,closed,coalesce(last_text,''),coalesce(last_author,'me'),read_up_to
                    FROM filtered ORDER BY last DESC,space ASC LIMIT ?5 OFFSET ?6"),
        &Values(args.to_vec()),
        &mut |row| {
            rows.push(Conversation {
                id: row.get(0)?,
                count: row.get(1)?,
                last: row.get(2)?,
                closed: row.get::<i64>(3)? != 0,
                text: row.get(4)?,
                author: row.get(5)?,
                read_up_to: row.get(6)?,
            });
            Ok(())
        },
    )?;
    Ok(Page { total, rows })
}

pub struct OpenSpace {
    pub id: String,
    pub kind: String,
    pub read_up_to: Option<i64>,
}

pub fn attention_spaces(r: &mut impl Read, ws: &str, user: &str) -> Result<Vec<OpenSpace>> {
    let mut rows = Vec::new();
    r.db().query(
        ATTENTION,
        &Values(vec![ws.into(), user.into()]),
        &mut |row| {
            rows.push(OpenSpace {
                id: row.get(0)?,
                kind: row.get(1)?,
                read_up_to: row.get(2)?,
            });
            Ok(())
        },
    )?;
    Ok(rows)
}

pub fn channel_spaces(
    r: &mut impl Read,
    ws: &str,
    user: &str,
    connection: &str,
) -> Result<Vec<(String, Option<i64>)>> {
    let mut rows = Vec::new();
    r.db().query(
        "SELECT s.id, p.read_up_to FROM spaces s
           LEFT JOIN conversation_preferences p ON p.workspace_id = s.workspace_id
               AND p.user_id = ?2 AND p.space_id = s.id
          WHERE s.workspace_id = ?1 AND coalesce(p.closed, 0) = 0
            AND (s.connection_id = ?3 OR EXISTS (SELECT 1 FROM space_identities li
              WHERE li.space_id = s.id AND li.connection_id = ?3 AND li.left_at IS NULL))
            AND (EXISTS (SELECT 1 FROM messages x WHERE x.workspace_id = ?1 AND x.space = 'chan:' || s.id)
              OR EXISTS (SELECT 1 FROM chan_messages m WHERE m.space_id = s.id))",
        &Values(vec![ws.into(), user.into(), connection.into()]),
        &mut |row| {
            rows.push((row.get(0)?, row.get(1)?));
            Ok(())
        },
    )?;
    Ok(rows)
}

pub fn has_history(r: &mut impl Read, ws: &str, space: &str, conversation: &str) -> Result<bool> {
    if r.one(
        Query::select()
            .column("id")
            .from("messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(conversation))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some()
    {
        return Ok(true);
    }
    Ok(r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space_id").eq(space))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn set_closed(w: &mut W<'_>, ws: &str, user: &str, space: &str, closed: bool) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("conversation_preferences")
            .columns(["workspace_id", "user_id", "space_id", "closed"])
            .values_panic([
                ws.into(),
                user.into(),
                space.into(),
                i64::from(closed).into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "user_id", "space_id"])
                    .update_column("closed")
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn advance_read(w: &mut W<'_>, ws: &str, user: &str, space: &str, at: i64) -> Result<()> {
    let current = Expr::col(("conversation_preferences", "read_up_to"));
    let existing: Expr = Func::coalesce([current, Expr::Constant(0_i64.into())]).into();
    w.exec(
        Query::insert()
            .into_table("conversation_preferences")
            .columns(["workspace_id", "user_id", "space_id", "read_up_to"])
            .values_panic([ws.into(), user.into(), space.into(), at.into()])
            .on_conflict(
                OnConflict::columns(["workspace_id", "user_id", "space_id"])
                    .value(
                        "read_up_to",
                        Expr::case(existing.clone().gte(at), existing).finally(at),
                    )
                    .to_owned(),
            ),
    )?;
    Ok(())
}

fn unseen(space: &str, user: &str, since: i64) -> sea_query::SelectStatement {
    let own = Query::select()
        .column("id")
        .from("parties")
        .and_where(Expr::col("id").equals(("m", "sender_party_id")))
        .and_where(Expr::col("user_id").eq(user))
        .take();
    Query::select()
        .from_as("chan_messages", "m")
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .and_where(Expr::col(("m", "direction")).eq("in"))
        .and_where(Expr::col(("m", "history")).is_null())
        .and_where(Expr::col(("m", "created")).gt(since))
        .and_where(Expr::exists(own).not())
        .take()
}

pub fn count_unseen(
    r: &mut impl Read,
    space: &str,
    user: &str,
    since: i64,
    cap: u64,
) -> Result<i64> {
    let limited = unseen(space, user, since)
        .column(("m", "id"))
        .limit(cap)
        .take();
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col(("limited", "id"))))
            .from_subquery(limited, "limited"),
        |row| row.get(0),
    )?
    .unwrap_or_default())
}

pub fn latest_unseen(
    r: &mut impl Read,
    space: &str,
    user: &str,
    since: i64,
) -> Result<Option<(String, i64)>> {
    r.one(
        unseen(space, user, since)
            .columns([("m", "id"), ("m", "created")])
            .order_by_columns([(("m", "created"), Order::Desc), (("m", "id"), Order::Desc)])
            .limit(1),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

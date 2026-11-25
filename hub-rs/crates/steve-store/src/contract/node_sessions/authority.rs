//! Principal and path-scope facts for Node sessions.

use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Order, Query};

use crate::{Read, Result, W};

pub fn members(r: &mut impl Read, ws: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column("user_id")
            .from("workspace_members")
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by("user_id", Order::Asc),
        |row| row.get(0),
    )
}

pub struct Item {
    pub workspace: String,
    pub requester: Option<String>,
    pub cwd: Option<String>,
    pub project_path: Option<String>,
    pub worktree: Option<bool>,
}

pub fn item(r: &mut impl Read, id: &str) -> Result<Option<Item>> {
    r.one(
        Query::select()
            .columns([
                ("i", "workspace_id"),
                ("i", "requested_by"),
                ("i", "cwd"),
                ("p", "path"),
                ("p", "worktree"),
            ])
            .from_as("items", "i")
            .join_as(
                JoinType::LeftJoin,
                "projects",
                "p",
                Expr::col(("p", "id")).eq(Expr::col(("i", "project_id"))),
            )
            .and_where(Expr::col(("i", "id")).eq(id)),
        |row| {
            Ok(Item {
                workspace: row.get(0)?,
                requester: row.get(1)?,
                cwd: row.get(2)?,
                project_path: row.get(3)?,
                worktree: row.get(4)?,
            })
        },
    )
}

pub struct Session {
    pub workspace: String,
    pub item: Option<String>,
    pub bot: Option<String>,
    pub cwd: String,
    pub principal: Option<String>,
    pub audience: Option<Vec<String>>,
}

pub fn session(r: &mut impl Read, id: &str) -> Result<Option<Session>> {
    r.one(
        Query::select()
            .columns([
                ("s", "workspace_id"),
                ("s", "item_id"),
                ("s", "bot_id"),
                ("s", "cwd"),
                ("r", "principal"),
                ("r", "audience"),
            ])
            .from_as("sessions", "s")
            .join_as(
                JoinType::LeftJoin,
                "runtime_sessions",
                "r",
                Expr::col(("r", "session_id")).eq(Expr::col(("s", "id"))),
            )
            .and_where(Expr::col(("s", "id")).eq(id)),
        |row| {
            Ok(Session {
                workspace: row.get(0)?,
                item: row.get(1)?,
                bot: row.get(2)?,
                cwd: row.get(3)?,
                principal: row.get(4)?,
                audience: row
                    .get::<Option<String>>(5)?
                    .and_then(|value| serde_json::from_str(&value).ok()),
            })
        },
    )
}

pub fn set_principal(
    w: &mut W<'_>,
    session: &str,
    principal: &str,
    audience: &[String],
) -> Result<()> {
    w.exec(
        Query::update()
            .table("runtime_sessions")
            .value("principal", principal)
            .value("audience", serde_json::to_string(audience)?)
            .and_where(Expr::col("session_id").eq(session)),
    )?;
    Ok(())
}

pub fn set_state_dir(w: &mut W<'_>, session: &str, dir: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("runtime_sessions")
            .value("state_dir", dir)
            .and_where(Expr::col("session_id").eq(session)),
    )?;
    Ok(())
}

pub struct KnowledgeFolder {
    pub path: String,
    pub visibility: String,
    pub creator: String,
}

pub fn knowledge_folders(r: &mut impl Read, ws: &str) -> Result<Vec<KnowledgeFolder>> {
    let d = r.db().dialect();
    let config = Expr::col("config");
    r.all(
        Query::select()
            .expr(d.json_field(config.clone(), "path"))
            .columns(["visibility", "created_by"])
            .from("kb_sources")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("kind").eq("folder"))
            .and_where(d.json_valid(config.clone()))
            .and_where(d.json_type(config, "$.path").eq("text"))
            .and_where(Expr::col("paused").eq(false)),
        |row| {
            Ok(KnowledgeFolder {
                path: row.get(0)?,
                visibility: row.get(1)?,
                creator: row.get(2)?,
            })
        },
    )
}

pub struct PathDifference<'a> {
    pub total: usize,
    pub sample: &'a [String],
}

pub struct ViewRecord<'a, V> {
    pub session: &'a str,
    pub node: &'a str,
    pub at: i64,
    pub digest: &'a str,
    pub view: &'a V,
    pub extra: Option<PathDifference<'a>>,
    pub missing: Option<PathDifference<'a>>,
}

pub fn record_view<V: serde::Serialize>(w: &mut W<'_>, record: ViewRecord<'_, V>) -> Result<()> {
    let (extra_n, extra) = difference(record.extra)?;
    let (missing_n, missing) = difference(record.missing)?;
    w.exec(
        Query::insert()
            .into_table("view_shadow")
            .columns([
                "session_id",
                "node_id",
                "kind",
                "at",
                "digest",
                "view",
                "extra_count",
                "extra",
                "missing_count",
                "missing",
            ])
            .values_panic([
                record.session.into(),
                record.node.into(),
                "session".into(),
                record.at.into(),
                record.digest.into(),
                serde_json::to_string(record.view)?.into(),
                extra_n.into(),
                extra.into(),
                missing_n.into(),
                missing.into(),
            ])
            .on_conflict(
                OnConflict::column("session_id")
                    .update_columns([
                        "node_id",
                        "kind",
                        "at",
                        "digest",
                        "view",
                        "extra_count",
                        "extra",
                        "missing_count",
                        "missing",
                    ])
                    .to_owned(),
            ),
    )?;
    w.exec(
        Query::update()
            .table("runtime_sessions")
            .value("view_digest", record.digest)
            .and_where(Expr::col("session_id").eq(record.session)),
    )?;
    Ok(())
}

fn difference(value: Option<PathDifference<'_>>) -> Result<(Option<i64>, Option<String>)> {
    match value {
        Some(value) => Ok((
            Some(value.total as i64),
            Some(serde_json::to_string(value.sample)?),
        )),
        None => Ok((None, None)),
    }
}

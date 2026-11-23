use std::collections::BTreeMap;

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Json;
use crate::{Members, Read, Result, Session, W};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Object {
    pub sha256: String,
    pub size: u64,
    pub mime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub item_id: Option<String>,
    pub title: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub created: i64,
    pub entrypoint: Option<String>,
    pub preview: Value,
    /// Static is the default; only then do the package's scripts never run.
    pub interactive: bool,
    #[serde(skip)]
    pub reply: bool,
    #[serde(skip)]
    pub workspace_id: String,
    /// The user who manages a delivery made outside an item; item deliveries have none.
    #[serde(skip)]
    pub owner_id: Option<String>,
    #[serde(skip)]
    pub registered_url: String,
    #[serde(skip)]
    pub sha256: String,
    #[serde(skip)]
    pub manifest: BTreeMap<String, Object>,
}

fn visible(c: &mut impl Read) -> SelectStatement {
    let d = |field: &'static str| Expr::col(("d", field));
    let dialect = c.db().dialect();
    Query::select()
        .from_as("deliveries", "d")
        .columns(
            [
                "id",
                "workspace_id",
                "item_id",
                "title",
                "name",
                "mime",
                "size",
                "created",
                "entrypoint",
                "sha256",
                "manifest",
                "preview",
                "url",
                "owner_id",
                "interactive",
                "reply",
            ]
            .map(|field| ("d", field)),
        )
        .join_as(
            JoinType::LeftJoin,
            "items",
            "i",
            Expr::col(("i", "id")).eq(d("item_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "sessions",
            "s",
            Expr::col(("s", "id")).eq(d("session_id")),
        )
        .and_where(
            d("reply").eq(false).or(Expr::col(("s", "workspace_id"))
                .eq(d("workspace_id"))
                .and(dialect.null_safe_eq(Expr::col(("s", "item_id")), d("item_id")))),
        )
        .and_where(
            d("item_id")
                .is_null()
                .or(Expr::col(("i", "workspace_id")).eq(d("workspace_id"))),
        )
        .take()
}

fn decode(r: &super::Row<'_>) -> Result<Delivery> {
    Ok(Delivery {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        item_id: r.get(2)?,
        title: r.get(3)?,
        name: r.get(4)?,
        mime: r.get(5)?,
        size: r.get::<i64>(6)? as u64,
        created: r.get(7)?,
        entrypoint: r.get(8)?,
        sha256: r.get(9)?,
        manifest: r.get::<Json<_>>(10)?.0,
        preview: r.get::<Json<_>>(11)?.0,
        registered_url: r.get(12)?,
        owner_id: r.get(13)?,
        interactive: r.get(14)?,
        reply: r.get(15)?,
    })
}

pub fn get(c: &mut impl Read, id: &str, ws: Option<&str>) -> Result<Option<Delivery>> {
    let mut q = visible(c);
    q.and_where(Expr::col(("d", "id")).eq(id));
    if let Some(ws) = ws {
        q.and_where(Expr::col(("d", "workspace_id")).eq(ws));
    }
    c.one(&q, decode)
}

pub fn owned(c: &mut impl Read, ws: &str, user: &str) -> Result<Vec<Delivery>> {
    let dialect = c.db().dialect();
    let ids = Query::select()
        .column("id")
        .from("deliveries")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("owner_id").eq(user))
        .order_by("created", Order::Desc)
        .order_by_expr(dialect.insertion_order(None), Order::Desc)
        .limit(500)
        .take();
    let q = visible(c)
        .and_where(Expr::col(("d", "id")).in_subquery(ids))
        .order_by(("d", "created"), Order::Desc)
        .order_by_expr(dialect.insertion_order(Some("d")), Order::Desc)
        .take();
    Ok(c.all(&q, |r| Ok(decode(r)))?
        .into_iter()
        .filter_map(Result::ok)
        .collect())
}

pub fn member(c: &mut impl Read, ws: &str, user: &str) -> Result<bool> {
    Ok(Members::new(c).role(ws, user)?.is_some())
}

pub fn item_manager(c: &mut impl Read, ws: &str, item: Option<&str>, user: &str) -> Result<bool> {
    if let Some(item) = item
        && c.one(
            Query::select()
                .column("id")
                .from("items")
                .and_where(Expr::col("id").eq(item))
                .and_where(Expr::col("requested_by").eq(user)),
            |_| Ok(()),
        )?
        .is_some()
    {
        return Ok(true);
    }
    Ok(Members::new(c).role(ws, user)?.as_deref() == Some("owner"))
}

pub fn session_in_scope(c: &mut impl Read, session: &Session) -> Result<bool> {
    let mut q = Query::select();
    q.column(("s", "id"))
        .from_as("sessions", "s")
        .and_where(Expr::col(("s", "id")).eq(session.id.as_str()))
        .and_where(Expr::col(("s", "workspace_id")).eq(session.workspace_id.as_str()));
    if let Some(item) = &session.item_id {
        q.join_as(
            JoinType::InnerJoin,
            "items",
            "i",
            Expr::col(("i", "id"))
                .eq(Expr::col(("s", "item_id")))
                .and(Expr::col(("i", "workspace_id")).eq(Expr::col(("s", "workspace_id")))),
        )
        .and_where(Expr::col(("i", "id")).eq(item.as_str()));
    } else {
        q.and_where(Expr::col(("s", "item_id")).is_null())
            .and_where(Expr::col(("s", "status")).ne("ended"));
    }
    Ok(c.one(&q, |_| Ok(()))?.is_some())
}

pub fn session_owner(c: &mut impl Read, session: &Session) -> Result<Option<String>> {
    let ws = session.workspace_id.as_str();
    let principal = c.one(
        Query::select()
            .column(("m", "user_id"))
            .from_as("runtime_sessions", "r")
            .join_as(
                JoinType::InnerJoin,
                "workspace_members",
                "m",
                Expr::col(("m", "user_id")).eq(Expr::col(("r", "principal"))),
            )
            .and_where(Expr::col(("r", "session_id")).eq(session.id.as_str()))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws)),
        |r| r.get(0),
    )?;
    match principal {
        Some(_) => Ok(principal),
        None => Members::new(c).owner(ws),
    }
}

pub fn active_reply(
    c: &mut impl Read,
    session: &str,
    binding: &str,
    turn: &str,
    attempt: &str,
) -> Result<bool> {
    let r = |field: &'static str| Expr::col(("r", field));
    let a = |field: &'static str| Expr::col(("a", field));
    let t = |field: &'static str| Expr::col(("t", field));
    Ok(c.one(
        Query::select()
            .column(("a", "attempt_id"))
            .from_as("runtime_sessions", "r")
            .join_as(
                JoinType::InnerJoin,
                "runtime_turns",
                "t",
                t("session_id").eq(r("session_id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "a",
                a("turn_id")
                    .eq(t("turn_id"))
                    .and(a("attempt_id").eq(t("active_attempt"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "sessions",
                "s",
                Expr::col(("s", "id"))
                    .eq(r("session_id"))
                    .and(Expr::col(("s", "status")).ne("ended"))
                    .and(Expr::col(("s", "readonly")).eq(false)),
            )
            .and_where(r("session_id").eq(session))
            .and_where(r("workspace_ref").eq(binding))
            .and_where(t("turn_id").eq(turn))
            .and_where(a("attempt_id").eq(attempt))
            .and_where(r("active_attempt").eq(a("attempt_id")))
            .and_where(r("node_id").eq(a("node_id")))
            .and_where(a("cancel_requested").eq(false))
            .and_where(a("state").is_not_in(["finished", "rejected", "unknown"])),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn insert(w: &mut W<'_>, d: &Delivery, session: Option<&str>) -> Result<()> {
    let mut q = Query::insert();
    q.into_table("deliveries")
        .columns([
            "id",
            "url",
            "workspace_id",
            "item_id",
            "session_id",
            "owner_id",
            "title",
            "name",
            "mime",
            "size",
            "created",
            "entrypoint",
            "sha256",
            "manifest",
            "preview",
            "interactive",
            "reply",
        ])
        .values_panic([
            d.id.as_str().into(),
            d.registered_url.as_str().into(),
            d.workspace_id.as_str().into(),
            d.item_id.as_deref().into(),
            session.into(),
            d.owner_id.as_deref().into(),
            d.title.as_str().into(),
            d.name.as_str().into(),
            d.mime.as_str().into(),
            (d.size as i64).into(),
            d.created.into(),
            d.entrypoint.as_deref().into(),
            d.sha256.as_str().into(),
            serde_json::to_string(&d.manifest)?.into(),
            serde_json::to_string(&d.preview)?.into(),
            d.interactive.into(),
            d.reply.into(),
        ]);
    if d.reply {
        q.on_conflict(OnConflict::column("id").do_nothing().to_owned());
    }
    w.exec(&q)?;
    Ok(())
}

pub fn set_interactive(w: &mut W<'_>, id: &str, on: bool) -> Result<()> {
    w.exec(
        Query::update()
            .table("deliveries")
            .value("interactive", on)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub struct ShareRow {
    pub token: String,
    pub expires_at: i64,
    pub created: i64,
    pub revoked_at: Option<i64>,
}

pub fn create_share(w: &mut W<'_>, delivery: &str, user: &str, row: &ShareRow) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("delivery_shares")
            .columns([
                "token",
                "delivery_id",
                "created_by",
                "created",
                "expires_at",
            ])
            .values_panic([
                row.token.as_str().into(),
                delivery.into(),
                user.into(),
                row.created.into(),
                row.expires_at.into(),
            ]),
    )?;
    Ok(())
}

pub fn shares(c: &mut impl Read, delivery: &str) -> Result<Vec<ShareRow>> {
    c.all(
        Query::select()
            .columns(["token", "expires_at", "created", "revoked_at"])
            .from("delivery_shares")
            .and_where(Expr::col("delivery_id").eq(delivery))
            .order_by_columns([("created", Order::Asc), ("token", Order::Asc)]),
        |r| {
            Ok(ShareRow {
                token: r.get(0)?,
                expires_at: r.get(1)?,
                created: r.get(2)?,
                revoked_at: r.get(3)?,
            })
        },
    )
}

pub fn revoke(w: &mut W<'_>, delivery: &str, token: &str, at: i64) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("delivery_shares")
            .value(
                "revoked_at",
                Func::coalesce([Expr::col("revoked_at"), Expr::val(at)]),
            )
            .and_where(Expr::col("delivery_id").eq(delivery))
            .and_where(Expr::col("token").eq(token)),
    )? == 1)
}

pub fn shared(c: &mut impl Read, token: &str, at: i64) -> Result<Option<Delivery>> {
    let id: Option<String> = c.one(
        Query::select()
            .column("delivery_id")
            .from("delivery_shares")
            .and_where(Expr::col("token").eq(token))
            .and_where(Expr::col("revoked_at").is_null())
            .and_where(Expr::col("expires_at").gt(at)),
        |r| r.get(0),
    )?;
    match id {
        Some(id) => Ok(get(c, &id, None)?.filter(|d| !d.reply)),
        None => Ok(None),
    }
}

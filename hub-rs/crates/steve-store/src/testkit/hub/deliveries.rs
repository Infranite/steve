use sea_query::{Expr, ExprTrait, Func, Query};

use crate::contract::Q;
use crate::{R, Result, Store, W};

pub fn item(w: &mut W<'_>, workspace: &str, id: &str, number: i64) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("items")
            .columns([
                "id",
                "workspace_id",
                "no",
                "title",
                "phase",
                "created",
                "updated",
            ])
            .values_panic([
                id.into(),
                workspace.into(),
                number.into(),
                "delivery".into(),
                "active".into(),
                1_i64.into(),
                1_i64.into(),
            ]),
    )?;
    Ok(())
}

pub fn bind(
    store: &Store,
    session: &str,
    node: &str,
    workspace: &str,
    principal: Option<&str>,
) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::insert()
                .into_table("runtime_sessions")
                .columns([
                    "session_id",
                    "node_id",
                    "workspace_ref",
                    "node_session_id",
                    "config_revision",
                    "phase",
                    "updated",
                    "principal",
                ])
                .values_panic([
                    session.into(),
                    node.into(),
                    workspace.into(),
                    session.into(),
                    "r".into(),
                    "active".into(),
                    0_i64.into(),
                    principal.into(),
                ]),
        )?;
        Ok(())
    })
}

pub fn count(r: &mut R<'_>) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col("id")))
            .from("deliveries"),
        |r| r.get(0),
    )?
    .unwrap_or(0))
}

pub fn remove_memberships(store: &Store, user: &str) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::delete()
                .from_table("workspace_members")
                .and_where(Expr::col("user_id").eq(user)),
        )?;
        Ok(())
    })
}

pub fn set_requester(store: &Store, user: &str) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::update()
                .table("items")
                .value("requested_by", user)
                .and_where(Expr::col("id").eq("ST-1")),
        )?;
        Ok(())
    })
}

pub fn change_url(w: &mut W<'_>, id: &str, url: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("deliveries")
            .value("url", url)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn move_item(store: &Store, id: &str, workspace: &str, number: i64) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::update()
                .table("items")
                .value("workspace_id", workspace)
                .value("no", number)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    })
}

pub fn expire_share(store: &Store, token: &str, at: i64) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::update()
                .table("delivery_shares")
                .value("expires_at", at)
                .and_where(Expr::col("token").eq(token)),
        )?;
        Ok(())
    })
}

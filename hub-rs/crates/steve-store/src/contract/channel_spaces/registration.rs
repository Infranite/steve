//! Creation of channel spaces and the identities that receive their messages.

use sea_query::{Expr, ExprTrait, OnConflict, Order, Query};
use serde_json::json;

use crate::contract::Q;
use crate::{Read, Result, W, connections};

/// Identity ordinals are stored as bits in a signed BIGINT.
pub const MAX_IDENTITIES: i64 = 62;

pub struct Receiver {
    pub connection: String,
    pub user: Option<String>,
    pub platform: Option<String>,
}

pub fn receivers(r: &mut impl Read, space: &str) -> Result<Vec<Receiver>> {
    r.all(
        Query::select()
            .columns(["connection_id", "owner_user", "platform"])
            .from("space_identities")
            .and_where(Expr::col("space_id").eq(space))
            .order_by("connection_id", Order::Asc),
        |row| {
            Ok(Receiver {
                connection: row.get(0)?,
                user: row.get(1)?,
                platform: row.get(2)?,
            })
        },
    )
}

pub fn learned_platform(r: &mut impl Read, conn: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("platform")
            .from("space_identities")
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("platform").is_not_null())
            .limit(1),
        |row| row.get(0),
    )
}

pub struct Space<'a> {
    pub ws: &'a str,
    pub system: &'a str,
    pub connection: &'a str,
    pub kind: &'a str,
    pub external: &'a str,
    pub name: &'a str,
    pub member_count: i64,
    pub has_external: bool,
    pub disclosure: &'a str,
    pub muted: bool,
    pub platform: Option<&'a str>,
    pub now: i64,
}

pub fn create(w: &mut W<'_>, n: &Space<'_>) -> Result<bool> {
    let id = crate::uid("spc");
    let inserted = w.exec(
        Query::insert()
            .into_table("spaces")
            .columns([
                "id",
                "workspace_id",
                "system_id",
                "connection_id",
                "kind",
                "external_id",
                "name",
                "member_count",
                "has_external",
                "created",
                "updated",
            ])
            .values_panic([
                id.as_str().into(),
                n.ws.into(),
                n.system.into(),
                n.connection.into(),
                n.kind.into(),
                n.external.into(),
                n.name.into(),
                n.member_count.into(),
                i64::from(n.has_external).into(),
                n.now.into(),
                n.now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "connection_id", "external_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    see(w, n.ws, &id, n.connection, n.platform, n.now)?;
    w.exec(
        Query::insert()
            .into_table("space_settings")
            .columns([
                "space_id",
                "workspace_id",
                "disclosure",
                "muted",
                "item_defaults",
                "updated",
            ])
            .values_panic([
                id.as_str().into(),
                n.ws.into(),
                n.disclosure.into(),
                i64::from(n.muted).into(),
                json!({"tags": [], "project_id": null, "bot_id": null})
                    .to_string()
                    .into(),
                n.now.into(),
            ]),
    )?;
    w.emit(steve_types::ChannelsEvent::Space {
        id: id.into(),
        ws: n.ws.into(),
    });
    Ok(true)
}

pub fn see(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    connection: &str,
    platform: Option<&str>,
    now: i64,
) -> Result<bool> {
    let owner = connections::lifecycle::personal_user(w, ws, connection)?;
    let existing = w.one(
        Query::select()
            .columns(["left_at", "owner_user", "platform"])
            .from("space_identities")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("connection_id").eq(connection)),
        |row| {
            Ok((
                row.get::<Option<i64>>(0)?,
                row.get::<Option<String>>(1)?,
                row.get::<Option<String>>(2)?,
            ))
        },
    )?;
    if let Some((left_at, old_owner, old_platform)) = existing {
        let platform = platform.or(old_platform.as_deref());
        if left_at.is_none() && old_owner == owner && platform == old_platform.as_deref() {
            return Ok(false);
        }
        w.exec(
            Query::update()
                .table("space_identities")
                .value("left_at", Option::<i64>::None)
                .value("owner_user", owner)
                .value("platform", platform)
                .and_where(Expr::col("space_id").eq(space))
                .and_where(Expr::col("connection_id").eq(connection)),
        )?;
        return Ok(true);
    }
    let (count, last): (i64, Option<i64>) = w
        .one(
            Query::select()
                .expr(Expr::cust("count(*)"))
                .expr(Expr::col("ordinal").max())
                .from("space_identities")
                .and_where(Expr::col("space_id").eq(space)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .unwrap_or_default();
    let ordinal = (count < MAX_IDENTITIES).then(|| last.map_or(0, |last| last + 1));
    w.exec(
        Query::insert()
            .into_table("space_identities")
            .columns([
                "space_id",
                "connection_id",
                "workspace_id",
                "joined",
                "ordinal",
                "owner_user",
                "platform",
            ])
            .values_panic([
                space.into(),
                connection.into(),
                ws.into(),
                now.into(),
                ordinal.into(),
                owner.into(),
                platform.into(),
            ]),
    )?;
    Ok(true)
}

pub fn describe(
    w: &mut W<'_>,
    space: &str,
    name: &str,
    members: i64,
    has_external: bool,
    now: i64,
) -> Result<()> {
    let mut update = Query::update();
    update
        .table("spaces")
        .value("has_external", i64::from(has_external))
        .value("updated", now)
        .and_where(Expr::col("id").eq(space));
    if !name.is_empty() {
        update.value("name", name);
    }
    if members > 0 {
        update.value("member_count", members);
    }
    w.exec(&update)?;
    Ok(())
}

pub enum MembershipChange {
    Add(i64),
    Remove(i64),
}

pub fn count_members(
    w: &mut W<'_>,
    space: &str,
    delta: MembershipChange,
    has_external: bool,
    now: i64,
) -> Result<()> {
    let members = Expr::col("member_count");
    let next = match delta {
        MembershipChange::Add(count) => members.add(count),
        MembershipChange::Remove(count) => w
            .db()
            .dialect()
            .greatest(members.sub(count), Expr::cust("0")),
    };
    w.exec(
        Query::update()
            .table("spaces")
            .value("member_count", next)
            .value("has_external", i64::from(has_external))
            .value("updated", now)
            .and_where(Expr::col("id").eq(space)),
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
pub enum State {
    Active,
    Left,
    Archived,
}

pub fn set_state(w: &mut W<'_>, space: &str, state: State, now: i64) -> Result<bool> {
    let status = match state {
        State::Active => "active",
        State::Left => "left",
        State::Archived => "archived",
    };
    let mut update = Query::update();
    update
        .table("spaces")
        .value("status", status)
        .value("updated", now)
        .and_where(Expr::col("id").eq(space));
    if !matches!(state, State::Active) {
        update.and_where(Expr::col("status").ne(status));
    }
    Ok(w.exec(&update)? != 0)
}

/// The first description with a member count fills the information left absent on discovery.
pub fn fill_member_count(
    w: &mut W<'_>,
    space: &str,
    count: i64,
    external: bool,
    now: i64,
) -> Result<()> {
    let external = w
        .db()
        .dialect()
        .greatest(Expr::col("has_external"), Expr::val(i64::from(external)));
    w.exec(
        Query::update()
            .table("spaces")
            .values([
                ("member_count", Expr::val(count)),
                ("has_external", external),
                ("updated", Expr::val(now)),
            ])
            .and_where(Expr::col("id").eq(space))
            .and_where(Expr::col("member_count").eq(0i64)),
    )?;
    Ok(())
}

pub fn name_direct_message(w: &mut W<'_>, ws: &str, space: &str, name: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("spaces")
            .value("name", name)
            .and_where(Expr::col("id").eq(space))
            .and_where(Expr::col("workspace_id").eq(ws)),
    )?;
    emit(w, ws, space);
    Ok(())
}

pub fn emit(w: &mut W<'_>, ws: &str, space: &str) {
    w.emit(steve_types::ChannelsEvent::Space {
        id: space.into(),
        ws: ws.into(),
    });
}

pub fn routed_identity(r: &mut impl Read, ws: &str, space: &str, conn: &str) -> Result<bool> {
    let has_identity = r
        .one(
            Query::select()
                .column("space_id")
                .from("space_identities")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("space_id").eq(space))
                .and_where(Expr::col("connection_id").eq(conn))
                .and_where(Expr::col("left_at").is_null()),
            |_| Ok(()),
        )?
        .is_some();
    if !has_identity {
        return Ok(false);
    }
    Ok(connections::Connections::new(r)
        .source(ws, conn)?
        .is_some_and(|row| matches!(row.status.as_str(), "ok" | "error" | "paused" | "reauth")))
}

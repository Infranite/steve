//! The persisted state of a Feishu app registration. Credential contents live in the vault.

use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement, UpdateStatement};

use crate::contract::Row;
use crate::{Read, Result, W};

#[cfg(feature = "testkit")]
pub mod fixtures;

pub mod state {
    pub const PENDING: &str = "pending";
    pub const SAVED: &str = "credentials_saved";
    pub const VERIFYING: &str = "verifying";
    pub const SUCCEEDED: &str = "succeeded";
    pub const DENIED: &str = "denied";
    pub const EXPIRED: &str = "expired";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
    pub const ACTIVE: [&str; 3] = [PENDING, SAVED, VERIFYING];
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub id: String,
    pub ws: String,
    pub initiator: String,
    pub brand: String,
    pub switched: bool,
    pub device_item: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri_complete: Option<String>,
    pub interval_s: i64,
    pub expires_at: i64,
    pub state: String,
    pub error: Option<String>,
    pub error_kind: Option<String>,
    pub app_may_exist: bool,
    pub client_id: Option<String>,
    pub vault_item: Option<String>,
    pub connection_id: Option<String>,
    pub account_id: Option<String>,
}

fn query() -> SelectStatement {
    Query::select()
        .columns([
            "id",
            "workspace_id",
            "initiator",
            "brand",
            "switched",
            "device_item",
            "user_code",
            "verification_uri_complete",
            "interval_s",
            "expires_at",
            "state",
            "error",
            "error_kind",
            "app_may_exist",
            "client_id",
            "vault_item",
            "connection_id",
            "account_id",
        ])
        .from("chan_app_registrations")
        .take()
}

fn decode(row: &Row<'_>) -> Result<Registration> {
    Ok(Registration {
        id: row.get(0)?,
        ws: row.get(1)?,
        initiator: row.get(2)?,
        brand: row.get(3)?,
        switched: row.get::<i64>(4)? != 0,
        device_item: row.get(5)?,
        user_code: row.get(6)?,
        verification_uri_complete: row.get(7)?,
        interval_s: row.get(8)?,
        expires_at: row.get(9)?,
        state: row.get(10)?,
        error: row.get(11)?,
        error_kind: row.get(12)?,
        app_may_exist: row.get::<i64>(13)? != 0,
        client_id: row.get(14)?,
        vault_item: row.get(15)?,
        connection_id: row.get(16)?,
        account_id: row.get(17)?,
    })
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<Registration>> {
    r.one(query().and_where(Expr::col("id").eq(id)), decode)
}

pub fn for_initiator(r: &mut impl Read, ws: &str, user: &str) -> Result<Option<Registration>> {
    r.one(
        query()
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("initiator").eq(user))
            .and_where(Expr::col("state").is_in(state::ACTIVE))
            .order_by("updated", Order::Desc)
            .limit(1),
        decode,
    )
}

pub fn active(r: &mut impl Read) -> Result<Vec<Registration>> {
    r.all(
        query().and_where(Expr::col("state").is_in(state::ACTIVE)),
        decode,
    )
}

pub struct NewRegistration<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub initiator: &'a str,
    pub requested_brand: &'a str,
    pub brand: &'a str,
    pub device_item: &'a str,
    pub user_code: &'a str,
    pub page: &'a str,
    pub interval_s: i64,
    pub expires_at: i64,
    pub now: i64,
}

pub fn create(w: &mut W<'_>, reg: NewRegistration<'_>) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("chan_app_registrations")
            .columns([
                "id",
                "workspace_id",
                "initiator",
                "brand_requested",
                "brand",
                "switched",
                "device_item",
                "user_code",
                "verification_uri_complete",
                "interval_s",
                "expires_at",
                "state",
                "created",
                "updated",
            ])
            .values_panic([
                reg.id.into(),
                reg.ws.into(),
                reg.initiator.into(),
                reg.requested_brand.into(),
                reg.brand.into(),
                0_i64.into(),
                reg.device_item.into(),
                reg.user_code.into(),
                reg.page.into(),
                reg.interval_s.into(),
                reg.expires_at.into(),
                state::PENDING.into(),
                reg.now.into(),
                reg.now.into(),
            ]),
    )?;
    Ok(())
}

fn update(id: &str) -> UpdateStatement {
    Query::update()
        .table("chan_app_registrations")
        .and_where(Expr::col("id").eq(id))
        .take()
}

fn from_state(id: &str, expected: &str) -> UpdateStatement {
    update(id).and_where(Expr::col("state").eq(expected)).take()
}

pub fn clear_ticket(w: &mut W<'_>, id: &str) -> Result<()> {
    w.exec(update(id).value("device_item", None::<String>))?;
    Ok(())
}

pub fn cancel(w: &mut W<'_>, id: &str, now: i64) -> Result<()> {
    w.exec(update(id).values([
        ("state", state::CANCELLED.into()),
        ("device_item", None::<String>.into()),
        ("vault_item", None::<String>.into()),
        ("app_may_exist", 1_i64.into()),
        ("error", None::<String>.into()),
        ("error_kind", None::<String>.into()),
        ("updated", now.into()),
    ]))?;
    Ok(())
}

pub fn finish_pending(
    w: &mut W<'_>,
    id: &str,
    next: &str,
    reason: (&str, &str),
    app_may_exist: bool,
    now: i64,
) -> Result<bool> {
    let may_exist = w.dialect().greatest(
        Expr::col("app_may_exist"),
        Expr::val(i64::from(app_may_exist)),
    );
    Ok(w.exec(from_state(id, state::PENDING).values([
        ("state", next.into()),
        ("error", reason.0.into()),
        ("error_kind", reason.1.into()),
        ("app_may_exist", may_exist),
        ("updated", now.into()),
    ]))? == 1)
}

pub fn retry_interrupted_check(w: &mut W<'_>, id: &str, now: i64) -> Result<()> {
    w.exec(
        from_state(id, state::VERIFYING)
            .values([("state", state::SAVED.into()), ("updated", now.into())]),
    )?;
    Ok(())
}

pub fn set_interval(w: &mut W<'_>, id: &str, interval: i64, now: i64) -> Result<()> {
    w.exec(
        from_state(id, state::PENDING)
            .values([("interval_s", interval.into()), ("updated", now.into())]),
    )?;
    Ok(())
}

pub fn switch_brand(w: &mut W<'_>, id: &str, brand: &str, now: i64) -> Result<()> {
    w.exec(from_state(id, state::PENDING).values([
        ("brand", brand.into()),
        ("switched", 1_i64.into()),
        ("updated", now.into()),
    ]))?;
    Ok(())
}

/// A credential received after its poll deadline is still accepted when that deadline ended the
/// attempt. Cancellation and other terminal outcomes are final.
pub fn can_save(r: &mut impl Read, id: &str, deadline_reasons: [&str; 2]) -> Result<bool> {
    let eligible = Expr::col("state").eq(state::PENDING).or(Expr::col("state")
        .eq(state::EXPIRED)
        .and(Expr::col("error_kind").is_in(deadline_reasons)));
    Ok(r.one(
        Query::select()
            .column("id")
            .from("chan_app_registrations")
            .and_where(Expr::col("id").eq(id))
            .and_where(eligible),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn mark_app_may_exist(w: &mut W<'_>, id: &str, now: i64) -> Result<()> {
    w.exec(update(id).values([("app_may_exist", 1_i64.into()), ("updated", now.into())]))?;
    Ok(())
}

pub fn save(
    w: &mut W<'_>,
    id: &str,
    vault_item: &str,
    client_id: &str,
    error: Option<(&str, &str)>,
    now: i64,
) -> Result<()> {
    w.exec(
        update(id)
            .and_where(Expr::col("state").is_in([state::PENDING, state::EXPIRED]))
            .values([
                ("state", state::SAVED.into()),
                ("vault_item", vault_item.into()),
                ("client_id", client_id.into()),
                ("user_code", None::<String>.into()),
                ("verification_uri_complete", None::<String>.into()),
                ("error", error.map(|e| e.0).into()),
                ("error_kind", error.map(|e| e.1).into()),
                ("updated", now.into()),
            ]),
    )?;
    Ok(())
}

pub fn claim_verification(w: &mut W<'_>, id: &str, now: i64) -> Result<bool> {
    Ok(w.exec(
        from_state(id, state::SAVED)
            .and_where(Expr::col("vault_item").is_not_null())
            .values([
                ("state", state::VERIFYING.into()),
                ("error", None::<String>.into()),
                ("error_kind", None::<String>.into()),
                ("updated", now.into()),
            ]),
    )? == 1)
}

pub fn bound(w: &mut W<'_>, id: &str, connection: &str, account: &str, now: i64) -> Result<bool> {
    Ok(w.exec(from_state(id, state::VERIFYING).values([
        ("state", state::SUCCEEDED.into()),
        ("vault_item", None::<String>.into()),
        ("connection_id", connection.into()),
        ("account_id", account.into()),
        ("error", None::<String>.into()),
        ("error_kind", None::<String>.into()),
        ("updated", now.into()),
    ]))? == 1)
}

pub fn verification_failed(w: &mut W<'_>, id: &str, reason: (&str, &str), now: i64) -> Result<()> {
    w.exec(from_state(id, state::VERIFYING).values([
        ("state", state::SAVED.into()),
        ("error", reason.0.into()),
        ("error_kind", reason.1.into()),
        ("updated", now.into()),
    ]))?;
    Ok(())
}

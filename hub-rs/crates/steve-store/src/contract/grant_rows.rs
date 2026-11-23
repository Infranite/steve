use sea_query::{Expr, ExprTrait, Query};

use crate::{Read, Result, W};

pub struct GrantRows<'a, C>(pub &'a mut C);

pub struct FlowGrant<'a> {
    pub ws: &'a str,
    pub flow: &'a str,
    pub version: i64,
    pub capability: &'a str,
    pub resource_type: &'a str,
    pub resource_id: &'a str,
    pub at: i64,
}

impl<C: Read> GrantRows<'_, C> {
    pub fn flow_granted(&mut self, g: &FlowGrant<'_>) -> Result<bool> {
        let version = self
            .0
            .db()
            .dialect()
            .json_field(Expr::col("scope"), "flow_version");
        self.0
            .one(
                Query::select()
                    .column("subject_id")
                    .from("grants")
                    .and_where(Expr::col("workspace_id").eq(g.ws))
                    .and_where(Expr::col("subject_type").eq("flow"))
                    .and_where(Expr::col("subject_id").eq(g.flow))
                    .and_where(Expr::col("capability").eq(g.capability))
                    .and_where(Expr::col("resource_type").eq(g.resource_type))
                    .and_where(Expr::col("resource_id").is_in([g.resource_id, "*"]))
                    .and_where(Expr::col("revoked").is_null())
                    .and_where(Expr::col("until").gt(g.at))
                    .and_where(version.eq(g.version))
                    .limit(1),
                |_| Ok(()),
            )
            .map(|grant| grant.is_some())
    }
}

impl GrantRows<'_, W<'_>> {
    pub fn grant_flow(
        &mut self,
        g: &FlowGrant<'_>,
        until: i64,
        actor: &str,
        decision: &str,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("grants")
                .columns([
                    "id",
                    "workspace_id",
                    "subject_type",
                    "subject_id",
                    "capability",
                    "resource_type",
                    "resource_id",
                    "scope",
                    "until",
                    "created_by",
                    "decision_id",
                    "created",
                ])
                .values_panic([
                    crate::uid("gr").into(),
                    g.ws.into(),
                    "flow".into(),
                    g.flow.into(),
                    g.capability.into(),
                    g.resource_type.into(),
                    g.resource_id.into(),
                    serde_json::json!({ "flow_version": g.version })
                        .to_string()
                        .into(),
                    until.into(),
                    actor.into(),
                    decision.into(),
                    g.at.into(),
                ]),
        )?;
        Ok(())
    }
}

pub struct VaultGrant {
    pub id: String,
    pub subject_id: String,
    pub scope: serde_json::Value,
    pub until: i64,
}

impl<C: Read> GrantRows<'_, C> {
    pub fn active_for_vault(&mut self, ws: &str, item: &str, now: i64) -> Result<Vec<VaultGrant>> {
        self.0.all(
            Query::select()
                .from("grants")
                .columns(["id", "subject_id", "scope", "until"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("capability").eq("secret.use"))
                .and_where(Expr::col("resource_type").eq("vault_item"))
                .and_where(Expr::col("resource_id").eq(item))
                .and_where(Expr::col("subject_type").eq("bot"))
                .and_where(Expr::col("revoked").is_null())
                .and_where(Expr::col("until").gt(now))
                .order_by("created", sea_query::Order::Asc)
                .order_by("id", sea_query::Order::Asc),
            |row| {
                Ok(VaultGrant {
                    id: row.get(0)?,
                    subject_id: row.get(1)?,
                    scope: serde_json::from_str(&row.get::<String>(2)?)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    until: row.get(3)?,
                })
            },
        )
    }
}

impl GrantRows<'_, W<'_>> {
    pub fn revoke_for_vault(&mut self, ws: &str, item: &str, now: i64) -> Result<usize> {
        Ok(self.0.exec(
            Query::update()
                .table("grants")
                .value("revoked", now)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("resource_type").eq("vault_item"))
                .and_where(Expr::col("resource_id").eq(item))
                .and_where(Expr::col("revoked").is_null())
                .and_where(Expr::col("until").gt(now)),
        )? as usize)
    }
}

pub struct VaultGrantRecord {
    pub workspace_id: String,
    pub subject_id: String,
    pub resource_id: String,
}

pub struct NewVaultGrant<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub bot: &'a str,
    pub item: &'a str,
    pub scope: &'a serde_json::Value,
    pub until: i64,
    pub actor: &'a str,
    pub decision: Option<&'a str>,
    pub at: i64,
}

fn secret_grants() -> sea_query::SelectStatement {
    Query::select()
        .from("grants")
        .and_where(Expr::col("capability").eq("secret.use"))
        .and_where(Expr::col("resource_type").eq("vault_item"))
        .take()
}

impl<C: Read> GrantRows<'_, C> {
    pub fn vault_grant(&mut self, id: &str) -> Result<Option<VaultGrantRecord>> {
        self.0.one(
            secret_grants()
                .columns(["workspace_id", "subject_id", "resource_id"])
                .and_where(Expr::col("id").eq(id)),
            |row| {
                Ok(VaultGrantRecord {
                    workspace_id: row.get(0)?,
                    subject_id: row.get(1)?,
                    resource_id: row.get(2)?,
                })
            },
        )
    }

    pub fn vault_covering(
        &mut self,
        ws: &str,
        item: &str,
        bot: &str,
        scope: &serde_json::Value,
        now: i64,
    ) -> Result<Option<String>> {
        let grants = self.0.all(
            secret_grants()
                .columns(["id", "scope"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("resource_id").eq(item))
                .and_where(Expr::col("subject_type").eq("bot"))
                .and_where(Expr::col("subject_id").eq(bot))
                .and_where(Expr::col("revoked").is_null())
                .and_where(Expr::col("until").gt(now))
                .order_by("until", sea_query::Order::Desc)
                .order_by("id", sea_query::Order::Asc),
            |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
        )?;
        Ok(grants.into_iter().find_map(|(id, value)| {
            (serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})) == *scope)
                .then_some(id)
        }))
    }

    pub fn vault_flow_grant(
        &mut self,
        ws: &str,
        flow: &str,
        version: i64,
        item: &str,
        now: i64,
    ) -> Result<Option<String>> {
        let scope = self
            .0
            .db()
            .dialect()
            .json_field(Expr::col("scope"), "flow_version");
        self.0.one(
            secret_grants()
                .column("id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject_type").eq("flow"))
                .and_where(Expr::col("subject_id").eq(flow))
                .and_where(Expr::col("resource_id").eq(item))
                .and_where(Expr::col("revoked").is_null())
                .and_where(Expr::col("until").gt(now))
                .and_where(scope.eq(version))
                .order_by("until", sea_query::Order::Desc)
                .order_by("id", sea_query::Order::Asc)
                .limit(1),
            |row| row.get(0),
        )
    }
}

impl GrantRows<'_, W<'_>> {
    pub fn insert_vault_grant(&mut self, row: &NewVaultGrant<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("grants")
                .columns([
                    "id",
                    "workspace_id",
                    "subject_type",
                    "subject_id",
                    "capability",
                    "resource_type",
                    "resource_id",
                    "scope",
                    "until",
                    "created_by",
                    "decision_id",
                    "created",
                ])
                .values_panic([
                    row.id.into(),
                    row.ws.into(),
                    "bot".into(),
                    row.bot.into(),
                    "secret.use".into(),
                    "vault_item".into(),
                    row.item.into(),
                    row.scope.to_string().into(),
                    row.until.into(),
                    row.actor.into(),
                    row.decision.into(),
                    row.at.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn revoke_vault_grant(&mut self, id: &str, now: i64) -> Result<bool> {
        Ok(self.0.exec(
            Query::update()
                .table("grants")
                .value("revoked", now)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("revoked").is_null()),
        )? != 0)
    }
}

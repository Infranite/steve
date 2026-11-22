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

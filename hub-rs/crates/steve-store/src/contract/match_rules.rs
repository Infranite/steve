use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};

use crate::{Read, Result, W, now, uid};

pub struct MatchRules<'a, C>(pub &'a mut C);

/// `match_rules.config` of `email_prefix`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefixConfig {
    pub same_org_only: bool,
    pub exclude: Vec<String>,
    pub min_len: i64,
}

impl Default for PrefixConfig {
    fn default() -> Self {
        Self {
            same_org_only: true,
            exclude: ["admin", "info", "noreply", "support", "hello"]
                .map(String::from)
                .to_vec(),
            min_len: 3,
        }
    }
}

/// A `match_rules` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatchRule {
    pub id: String,
    #[serde(skip)]
    pub workspace_id: String,
    pub family: String,
    pub kind: String,
    pub config: PrefixConfig,
    pub enabled: bool,
    pub updated: i64,
}

fn rule(row: &super::Row<'_>) -> Result<MatchRule> {
    let config: Option<String> = row.get(4)?;
    Ok(MatchRule {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        family: row.get(2)?,
        kind: row.get(3)?,
        config: config
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default(),
        enabled: row.get(5)?,
        updated: row.get(6)?,
    })
}

fn rules() -> SelectStatement {
    Query::select()
        .from("match_rules")
        .columns([
            "id",
            "workspace_id",
            "family",
            "kind",
            "config",
            "enabled",
            "updated",
        ])
        .take()
}

impl<C: Read> MatchRules<'_, C> {
    pub fn list(&mut self, ws: &str) -> Result<Vec<MatchRule>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            rules()
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("created", Order::Asc)
                .order_by_expr(order, Order::Asc),
            rule,
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<MatchRule>> {
        self.0.one(rules().and_where(Expr::col("id").eq(id)), rule)
    }

    pub fn prefix(&mut self, ws: &str) -> Result<Option<MatchRule>> {
        self.0.one(
            rules()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("family").eq("email"))
                .and_where(Expr::col("kind").eq("email_prefix"))
                .order_by("created", Order::Asc)
                .limit(1),
            rule,
        )
    }
}

impl MatchRules<'_, W<'_>> {
    pub fn seed_prefix(&mut self, ws: &str) -> Result<()> {
        let at = now();
        self.0.exec(
            Query::insert()
                .into_table("match_rules")
                .columns([
                    "id",
                    "workspace_id",
                    "family",
                    "kind",
                    "config",
                    "enabled",
                    "created",
                    "updated",
                ])
                .values_panic([
                    uid("mr").into(),
                    ws.into(),
                    "email".into(),
                    "email_prefix".into(),
                    serde_json::to_string(&PrefixConfig::default())?.into(),
                    1_i64.into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn update(&mut self, id: &str, enabled: bool, config: &PrefixConfig) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("match_rules")
                .value("enabled", i64::from(enabled))
                .value("config", serde_json::to_string(config)?)
                .value("updated", now())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}

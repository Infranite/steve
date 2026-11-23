use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Connections, Read, Result, W};

/// `connections.health`. The shown part is `last_ok`, `last_error`, `error_at`; the rest drives the
/// jobs: `fails` in a row, `next_at` (backoff: no check or refresh before it), and the token expiry
/// mirrored from the credential so the refresh job needn't decrypt to know what is due.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthState {
    pub last_ok: i64,
    pub last_error: String,
    pub error_at: i64,
    pub fails: i64,
    pub next_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnRow {
    pub id: String,
    pub workspace_id: String,
    pub account_id: String,
    pub adapter: String,
    pub auth: String,
    pub granted_scopes: Vec<String>,
    pub caps: Vec<String>,
    pub credential_item: Option<String>,
    /// `ok|reauth|error|paused|revoked`.
    pub status: String,
    pub health: HealthState,
    pub config: Value,
    pub last_sync: Option<i64>,
    pub created_by: String,
    pub revision: i64,
    pub created: i64,
    pub updated: i64,
}

const COLS: [&str; 16] = [
    "id",
    "workspace_id",
    "account_id",
    "adapter",
    "auth",
    "granted_scopes",
    "caps",
    "credential_item",
    "status",
    "health",
    "config",
    "last_sync",
    "created_by",
    "revision",
    "created",
    "updated",
];

fn base() -> SelectStatement {
    Query::select().from("connections").columns(COLS).take()
}

fn ordered() -> SelectStatement {
    base()
        .order_by("created", Order::Asc)
        .order_by("id", Order::Asc)
        .take()
}

fn decode(row: &super::Row<'_>) -> Result<ConnRow> {
    Ok(ConnRow {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        account_id: row.get(2)?,
        adapter: row.get(3)?,
        auth: row.get(4)?,
        granted_scopes: serde_json::from_str(&row.get::<String>(5)?).unwrap_or_default(),
        caps: serde_json::from_str(&row.get::<String>(6)?).unwrap_or_default(),
        credential_item: row.get(7)?,
        status: row.get(8)?,
        health: serde_json::from_str(&row.get::<String>(9)?).unwrap_or_default(),
        config: serde_json::from_str(&row.get::<String>(10)?)
            .unwrap_or(Value::Object(Default::default())),
        last_sync: row.get(11)?,
        created_by: row.get(12)?,
        revision: row.get(13)?,
        created: row.get(14)?,
        updated: row.get(15)?,
    })
}

pub struct DirectoryConnection {
    pub id: String,
    pub workspace_id: String,
    pub adapter: String,
    pub created_by: String,
}

fn directory_decode(row: &super::Row<'_>) -> Result<DirectoryConnection> {
    Ok(DirectoryConnection {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        adapter: row.get(2)?,
        created_by: row.get(3)?,
    })
}

fn directories(dialect: super::Dialect) -> SelectStatement {
    let capable = Query::select()
        .column(("cap", "value"))
        .from_function(dialect.json_array_elements(Expr::col("caps")), "cap")
        .and_where(Expr::col(("cap", "value")).eq("directory.read"))
        .take();
    Query::select()
        .from("connections")
        .columns(["id", "workspace_id", "adapter", "created_by"])
        .and_where(Expr::col("status").eq("ok"))
        .and_where(Expr::exists(capable))
        .take()
}

impl<C: Read> Connections<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<ConnRow>> {
        self.0.one(base().and_where(Expr::col("id").eq(id)), decode)
    }

    pub fn of_account(&mut self, account: &str) -> Result<Option<ConnRow>> {
        self.0.one(
            base().and_where(Expr::col("account_id").eq(account)),
            decode,
        )
    }

    pub fn list(&mut self, ws: &str) -> Result<Vec<ConnRow>> {
        self.0.all(
            ordered().and_where(Expr::col("workspace_id").eq(ws)),
            decode,
        )
    }

    pub fn live(&mut self) -> Result<Vec<ConnRow>> {
        self.0.all(
            ordered().and_where(Expr::col("status").is_in(["ok", "error"])),
            decode,
        )
    }

    pub fn authorized(&mut self) -> Result<Vec<ConnRow>> {
        self.0.all(
            ordered().and_where(Expr::col("status").is_in(["ok", "error", "paused"])),
            decode,
        )
    }

    pub fn brief(&mut self, account: &str) -> Result<Option<ConnectionBrief>> {
        self.0.one(
            Query::select()
                .from("connections")
                .columns(["id", "status", "caps", "last_sync"])
                .and_where(Expr::col("account_id").eq(account)),
            |row| {
                let caps: String = row.get(2)?;
                Ok(ConnectionBrief {
                    id: row.get(0)?,
                    status: row.get(1)?,
                    caps: serde_json::from_str(&caps).unwrap_or_default(),
                    last_sync: row.get(3)?,
                })
            },
        )
    }

    pub fn directory(&mut self, ws: &str, id: &str) -> Result<Option<DirectoryConnection>> {
        let dialect = self.0.db().dialect();
        self.0.one(
            directories(dialect)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            directory_decode,
        )
    }

    pub fn directories(&mut self, ws: Option<&str>) -> Result<Vec<DirectoryConnection>> {
        let dialect = self.0.db().dialect();
        let mut query = directories(dialect);
        if let Some(ws) = ws {
            query.and_where(Expr::col("workspace_id").eq(ws));
        }
        self.0.all(
            query
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            directory_decode,
        )
    }

    pub fn usable_adapter(&mut self, ws: &str, adapter: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("connections")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("adapter").eq(adapter))
                    .and_where(Expr::col("status").is_in(["ok", "error"]))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn contains(&mut self, ws: &str, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("connections")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").eq(id))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn permission_missing(&mut self, ws: &str, scope: &str) -> Result<bool> {
        let dialect = self.0.db().dialect();
        let config = Expr::col(("c", "config"));
        let permissions = Expr::case(
            dialect.json_valid(config.clone()),
            dialect.json_field(config, "permissions"),
        )
        .finally(Expr::value("[]"));
        let value = Expr::col(("permission", "value"));
        let missing = Query::select()
            .column(("permission", "value"))
            .from_function(
                dialect.json_array_elements(permissions.into()),
                "permission",
            )
            .and_where(dialect.json_field(value.clone(), "scope").eq(scope))
            .and_where(dialect.json_field(value, "status").eq("missing"))
            .take();
        Ok(self
            .0
            .one(
                Query::select()
                    .from_as("connections", "c")
                    .column(("c", "id"))
                    .and_where(Expr::col(("c", "workspace_id")).eq(ws))
                    .and_where(Expr::exists(missing))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn has_live_party(&mut self, party: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column(("connections", "id"))
                    .from("connections")
                    .inner_join(
                        "accounts",
                        Expr::col(("connections", "account_id")).equals(("accounts", "id")),
                    )
                    .and_where(Expr::col(("accounts", "party_id")).eq(party))
                    .and_where(Expr::col(("connections", "status")).ne("revoked"))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn never_synced(&mut self, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("connections")
                    .column("id")
                    .and_where(Expr::col("id").eq(id))
                    .and_where(Expr::col("last_sync").is_null()),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn change_keys(&mut self) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .from("connections")
                .columns(["workspace_id", "id"])
                .order_by("id", Order::Asc),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn directory_states(&mut self, ws: &str) -> Result<Vec<(String, String)>> {
        let directory = self
            .0
            .db()
            .dialect()
            .json_field(Expr::col("config"), "directory");
        self.0.all(
            Query::select()
                .from("connections")
                .column("id")
                .expr(directory.clone())
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(directory.is_not_null())
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn change_signature(&mut self, id: &str) -> Result<Option<String>> {
        let changed = self
            .0
            .db()
            .dialect()
            .json_field(Expr::col("config"), "permissions_checked_at")
            .cast_as("TEXT");
        self.0.one(
            Query::select()
                .from("connections")
                .columns([
                    "status",
                    "granted_scopes",
                    "caps",
                    "credential_item",
                    "revision",
                ])
                .expr(changed)
                .and_where(Expr::col("id").eq(id)),
            |row| {
                let status: String = row.get(0)?;
                let scopes: String = row.get(1)?;
                let caps: String = row.get(2)?;
                let credential = row.get::<Option<String>>(3)?.unwrap_or_default();
                let revision: i64 = row.get(4)?;
                let checked = row
                    .get::<Option<i64>>(5)?
                    .map(|at| at.to_string())
                    .unwrap_or_default();
                Ok(format!(
                    "{status}|{scopes}|{caps}|{credential}|{checked}|{revision}"
                ))
            },
        )
    }
}

fn change(id: &str) -> sea_query::UpdateStatement {
    Query::update()
        .table("connections")
        .and_where(Expr::col("id").eq(id))
        .take()
}

impl Connections<'_, W<'_>> {
    pub fn insert(&mut self, row: &ConnRow) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("connections")
                .columns(COLS)
                .values_panic([
                    row.id.as_str().into(),
                    row.workspace_id.as_str().into(),
                    row.account_id.as_str().into(),
                    row.adapter.as_str().into(),
                    row.auth.as_str().into(),
                    serde_json::to_string(&row.granted_scopes)?.into(),
                    serde_json::to_string(&row.caps)?.into(),
                    row.credential_item.as_deref().into(),
                    row.status.as_str().into(),
                    serde_json::to_string(&row.health)?.into(),
                    row.config.to_string().into(),
                    row.last_sync.into(),
                    row.created_by.as_str().into(),
                    1_i64.into(),
                    row.created.into(),
                    row.updated.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn update(&mut self, r: &ConnRow, at: i64) -> Result<()> {
        self.0.exec(
            change(&r.id)
                .value("granted_scopes", serde_json::to_string(&r.granted_scopes)?)
                .value("caps", serde_json::to_string(&r.caps)?)
                .value("credential_item", r.credential_item.as_deref())
                .value("status", r.status.as_str())
                .value("health", serde_json::to_string(&r.health)?)
                .value("config", r.config.to_string())
                .value("last_sync", r.last_sync)
                .value("revision", Expr::col("revision").add(1_i64))
                .value("updated", at),
        )?;
        Ok(())
    }

    pub fn set_enabled_at(&mut self, id: &str, at: i64) -> Result<()> {
        self.0.exec(change(id).value("enabled_at", at))?;
        Ok(())
    }

    pub fn set_config(&mut self, id: &str, config: &Value, at: i64) -> Result<()> {
        self.0.exec(
            change(id)
                .value("config", config.to_string())
                .value("updated", at),
        )?;
        Ok(())
    }

    pub fn set_health(
        &mut self,
        id: &str,
        status: &str,
        health: &HealthState,
        at: i64,
    ) -> Result<()> {
        self.0.exec(
            change(id)
                .value("status", status)
                .value("health", serde_json::to_string(health)?)
                .value("updated", at),
        )?;
        Ok(())
    }

    pub fn set_last_sync(&mut self, id: &str, at: i64) -> Result<()> {
        self.0.exec(change(id).value("last_sync", at))?;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("connections")
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn replace_config(&mut self, id: &str, config: &Value) -> Result<()> {
        self.0
            .exec(change(id).value("config", config.to_string()))?;
        Ok(())
    }
}

/// The part of a connection shown on the account page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConnectionBrief {
    pub id: String,
    pub status: String,
    pub caps: Vec<String>,
    pub last_sync: Option<i64>,
}

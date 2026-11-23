use sea_query::{Expr, ExprTrait, Order, Query};

use crate::{Read, Result, W};

pub struct VaultKeys<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultKey {
    pub version: i64,
    pub keychain_account: String,
    pub fingerprint: String,
    pub status: String,
    pub created: i64,
    pub retired: Option<i64>,
}

pub const ACTIVE: &str = "active";
pub const RETIRING: &str = "retiring";
pub const RETIRED: &str = "retired";

pub struct WrappedItemKey {
    pub item_id: String,
    pub dek_wrapped: Vec<u8>,
    pub kek_version: i64,
}

impl<C: Read> VaultKeys<'_, C> {
    pub fn keys(&mut self, ws: &str) -> Result<Vec<VaultKey>> {
        self.0.all(
            Query::select()
                .from("vault_keys")
                .columns([
                    "version",
                    "keychain_account",
                    "fingerprint",
                    "status",
                    "created",
                    "retired",
                ])
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("version", Order::Asc),
            |row| {
                Ok(VaultKey {
                    version: row.get(0)?,
                    keychain_account: row.get(1)?,
                    fingerprint: row.get(2)?,
                    status: row.get(3)?,
                    created: row.get(4)?,
                    retired: row.get(5)?,
                })
            },
        )
    }

    pub fn active_version(&mut self, ws: &str) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .from("vault_keys")
                .column("version")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq(ACTIVE))
                .order_by("version", Order::Desc)
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn rotating_workspaces(&mut self) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .distinct()
                .from("vault_keys")
                .column("workspace_id")
                .and_where(Expr::col("status").eq(RETIRING))
                .order_by("workspace_id", Order::Asc),
            |row| row.get(0),
        )
    }

    pub fn items_to_rewrap(&mut self, ws: &str, target: i64) -> Result<Vec<WrappedItemKey>> {
        self.0.all(
            Query::select()
                .from("vault_items")
                .columns(["id", "dek_wrapped", "kek_version"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("dek_wrapped").is_not_null())
                .and_where(Expr::col("kek_version").ne(target))
                .order_by("id", Order::Asc),
            |row| {
                Ok(WrappedItemKey {
                    item_id: row.get(0)?,
                    dek_wrapped: row.get(1)?,
                    kek_version: row.get(2)?,
                })
            },
        )
    }
}

impl VaultKeys<'_, W<'_>> {
    pub fn insert(
        &mut self,
        ws: &str,
        version: i64,
        account: &str,
        fingerprint: &str,
        status: &str,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("vault_keys")
                .columns([
                    "workspace_id",
                    "version",
                    "keychain_account",
                    "fingerprint",
                    "status",
                    "created",
                ])
                .values_panic([
                    ws.into(),
                    version.into(),
                    account.into(),
                    fingerprint.into(),
                    status.into(),
                    now.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn set_status(
        &mut self,
        ws: &str,
        version: i64,
        status: &str,
        retired: Option<i64>,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("vault_keys")
                .values([("status", status.into()), ("retired", retired.into())])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("version").eq(version)),
        )?;
        Ok(())
    }

    pub fn set_item_dek(&mut self, item: &str, wrapped: &[u8], version: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("vault_items")
                .values([
                    ("dek_wrapped", wrapped.to_vec().into()),
                    ("kek_version", version.into()),
                ])
                .and_where(Expr::col("id").eq(item)),
        )?;
        Ok(())
    }
}

use sea_query::{Cond, Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use crate::{Read, Result, W};

pub struct VaultItems<'a, C>(pub &'a mut C);

/// One row of `vaults`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRow {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    /// `personal` | `shared` | `system`
    pub scope: String,
    pub owner_user: Option<String>,
    pub backend: String,
    pub created: i64,
}

impl VaultRow {
    /// Personal only for its owner, shared for every member, system never (Hub-internal).
    pub fn visible_to(&self, ws: &str, user: &str) -> bool {
        self.workspace_id == ws
            && match self.scope.as_str() {
                "shared" => true,
                "personal" => self.owner_user.as_deref() == Some(user),
                _ => false,
            }
    }
}

fn vault_row(r: &super::Row<'_>) -> Result<VaultRow> {
    Ok(VaultRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        scope: r.get(3)?,
        owner_user: r.get(4)?,
        backend: r.get(5)?,
        created: r.get(6)?,
    })
}

/// One row of `vault_items`. `Debug` leaves out the wrapped key.
#[derive(Clone)]
pub struct ItemRow {
    pub id: String,
    pub workspace_id: String,
    pub vault_id: String,
    pub template: String,
    pub title: String,
    pub site: String,
    pub account_id: Option<String>,
    pub sensitivity: String,
    pub dek_wrapped: Option<Vec<u8>>,
    pub kek_version: Option<i64>,
    pub external_ref: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

impl std::fmt::Debug for ItemRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemRow")
            .field("id", &self.id)
            .field("vault_id", &self.vault_id)
            .field("template", &self.template)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Filters of `GET /vault/items`.
#[derive(Debug, Clone, Default)]
pub struct ItemFilter {
    pub vault: Option<String>,
    pub template: Option<String>,
    pub account: Option<String>,
    /// Matched against title, site, field labels, non-secret field values and the account's handles.
    /// Never against secret fields (their values are only ciphertext anyway).
    pub q: Option<String>,
}

/// One row of `vault_fields`. `Debug` leaves out the values.
#[derive(Clone)]
pub struct FieldRow {
    pub id: String,
    pub item_id: String,
    pub label: String,
    pub kind: String,
    pub secret: bool,
    pub value_plain: Option<String>,
    pub value_ct: Option<Vec<u8>>,
    pub hint: String,
    pub sort: i64,
    pub updated: i64,
}

impl std::fmt::Debug for FieldRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldRow")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

/// A field of a template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct FieldDef {
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub secret: bool,
}

/// One row of `vault_templates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub fields: Vec<FieldDef>,
    pub default_sensitivity: String,
    pub source: String,
}

impl Template {
    /// The template's own field with this label, if any.
    pub fn field(&self, label: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.label == label)
    }
}

const ITEM_COLS: &[&str] = &[
    "id",
    "workspace_id",
    "vault_id",
    "template",
    "title",
    "site",
    "account_id",
    "sensitivity",
    "dek_wrapped",
    "kek_version",
    "external_ref",
    "status",
    "created_by",
    "created",
    "updated",
];
const VAULT_COLS: [&str; 7] = [
    "id",
    "workspace_id",
    "name",
    "scope",
    "owner_user",
    "backend",
    "created",
];
const FIELD_COLS: [&str; 10] = [
    "id",
    "item_id",
    "label",
    "type",
    "secret",
    "value_plain",
    "value_ct",
    "hint",
    "sort",
    "updated",
];
const TEMPLATE_COLS: [&str; 5] = ["id", "name", "fields", "default_sensitivity", "source"];

fn items() -> SelectStatement {
    Query::select()
        .from_as("vault_items", "i")
        .columns(ITEM_COLS.iter().map(|col| ("i", *col)))
        .take()
}

fn template_row(row: &super::Row<'_>) -> Result<Template> {
    Ok(Template {
        id: row.get(0)?,
        name: row.get(1)?,
        fields: serde_json::from_str(&row.get::<String>(2)?).unwrap_or_default(),
        default_sensitivity: row.get(3)?,
        source: row.get(4)?,
    })
}

fn item_row(r: &super::Row<'_>) -> Result<ItemRow> {
    Ok(ItemRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        vault_id: r.get(2)?,
        template: r.get(3)?,
        title: r.get(4)?,
        site: r.get(5)?,
        account_id: r.get(6)?,
        sensitivity: r.get(7)?,
        dek_wrapped: r.get(8)?,
        kek_version: r.get(9)?,
        external_ref: r.get(10)?,
        status: r.get(11)?,
        created_by: r.get(12)?,
        created: r.get(13)?,
        updated: r.get(14)?,
    })
}

fn field_row(r: &super::Row<'_>) -> Result<FieldRow> {
    Ok(FieldRow {
        id: r.get(0)?,
        item_id: r.get(1)?,
        label: r.get(2)?,
        kind: r.get(3)?,
        secret: r.get(4)?,
        value_plain: r.get(5)?,
        value_ct: r.get(6)?,
        hint: r.get(7)?,
        sort: r.get(8)?,
        updated: r.get(9)?,
    })
}

/// Active vault item attached to an account and visible to the viewer.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ItemRef {
    pub id: String,
    pub title: String,
}

impl<C: Read> VaultItems<'_, C> {
    pub fn account_items(&mut self, account: &str, user: &str) -> Result<Vec<ItemRef>> {
        let order = self.0.db().dialect().insertion_order(Some("i"));
        self.0.all(
            Query::select()
                .from_as("vault_items", "i")
                .columns([("i", "id"), ("i", "title")])
                .join_as(
                    sea_query::JoinType::InnerJoin,
                    "vaults",
                    "v",
                    Expr::col(("v", "id"))
                        .equals(("i", "vault_id"))
                        .and(Expr::col(("v", "workspace_id")).equals(("i", "workspace_id"))),
                )
                .and_where(Expr::col(("i", "account_id")).eq(account))
                .and_where(Expr::col(("i", "status")).eq("active"))
                .and_where(
                    Expr::col(("v", "scope"))
                        .eq("shared")
                        .or(Expr::col(("v", "scope"))
                            .eq("personal")
                            .and(Expr::col(("v", "owner_user")).eq(user))),
                )
                .order_by(("i", "created"), Order::Asc)
                .order_by_expr(order, Order::Asc),
            |row| {
                Ok(ItemRef {
                    id: row.get(0)?,
                    title: row.get(1)?,
                })
            },
        )
    }

    pub fn vaults(&mut self, ws: &str) -> Result<Vec<VaultRow>> {
        self.0.all(
            Query::select()
                .from("vaults")
                .columns(VAULT_COLS)
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            vault_row,
        )
    }

    pub fn vault(&mut self, id: &str) -> Result<Option<VaultRow>> {
        self.0.one(
            Query::select()
                .from("vaults")
                .columns(VAULT_COLS)
                .and_where(Expr::col("id").eq(id)),
            vault_row,
        )
    }

    pub fn item_counts(&mut self, ws: &str) -> Result<Vec<(String, i64)>> {
        self.0.all(
            Query::select()
                .from("vault_items")
                .column("vault_id")
                .expr(Func::count(Expr::col("id")))
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq("active"))
                .group_by_col("vault_id"),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn templates(&mut self, ws: &str) -> Result<Vec<Template>> {
        let rows = self.0.all(
            Query::select()
                .from("vault_templates")
                .columns(TEMPLATE_COLS)
                .and_where(Expr::col("workspace_id").is_in(["*", ws]))
                .order_by_expr(
                    Expr::col("workspace_id").eq(Expr::Constant("*".into())),
                    Order::Desc,
                )
                .order_by("id", Order::Asc),
            template_row,
        )?;
        let mut out: Vec<Template> = Vec::new();
        for row in rows {
            match out.iter_mut().find(|entry| entry.id == row.id) {
                Some(entry) => *entry = row,
                None => out.push(row),
            }
        }
        Ok(out)
    }

    pub fn template(&mut self, ws: &str, id: &str) -> Result<Option<Template>> {
        self.0.one(
            Query::select()
                .from("vault_templates")
                .columns(TEMPLATE_COLS)
                .and_where(Expr::col("workspace_id").is_in(["*", ws]))
                .and_where(Expr::col("id").eq(id))
                .order_by_expr(
                    Expr::col("workspace_id").eq(Expr::Constant("*".into())),
                    Order::Asc,
                )
                .limit(1),
            template_row,
        )
    }

    pub fn item(&mut self, id: &str) -> Result<Option<ItemRow>> {
        self.0
            .one(items().and_where(Expr::col(("i", "id")).eq(id)), item_row)
    }

    pub fn fields(&mut self, item: &str) -> Result<Vec<FieldRow>> {
        self.0.all(
            Query::select()
                .from("vault_fields")
                .columns(FIELD_COLS)
                .and_where(Expr::col("item_id").eq(item))
                .order_by("sort", Order::Asc)
                .order_by("id", Order::Asc),
            field_row,
        )
    }

    pub fn list(&mut self, ws: &str, user: &str, filter: &ItemFilter) -> Result<Vec<ItemRow>> {
        let mut query = items();
        query
            .join_as(
                JoinType::InnerJoin,
                "vaults",
                "v",
                Expr::col(("v", "id")).equals(("i", "vault_id")),
            )
            .and_where(Expr::col(("i", "workspace_id")).eq(ws))
            .and_where(Expr::col(("i", "status")).eq("active"))
            .cond_where(
                Cond::any().add(Expr::col(("v", "scope")).eq("shared")).add(
                    Cond::all()
                        .add(Expr::col(("v", "scope")).eq("personal"))
                        .add(Expr::col(("v", "owner_user")).eq(user)),
                ),
            );
        for (column, value) in [
            ("vault_id", &filter.vault),
            ("template", &filter.template),
            ("account_id", &filter.account),
        ] {
            if let Some(value) = value {
                query.and_where(Expr::col(("i", column)).eq(value.as_str()));
            }
        }
        if let Some(text) = filter.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let dialect = self.0.db().dialect();
            let contains = |value: Expr| {
                dialect.contains(
                    Func::lower(value).into(),
                    Func::lower(Expr::value(text)).into(),
                )
            };
            let field = Query::select()
                .from_as("vault_fields", "f")
                .column(("f", "id"))
                .and_where(Expr::col(("f", "item_id")).equals(("i", "id")))
                .cond_where(
                    Cond::any().add(contains(Expr::col(("f", "label")))).add(
                        Cond::all()
                            .add(Expr::col(("f", "secret")).eq(0_i64))
                            .add(contains(Expr::col(("f", "value_plain")))),
                    ),
                )
                .take();
            let accounts = super::Accounts(self.0).display_matches(ws, text)?;
            let mut clause = Cond::any()
                .add(contains(Expr::col(("i", "title"))))
                .add(contains(Expr::col(("i", "site"))))
                .add(Expr::exists(field));
            if !accounts.is_empty() {
                clause = clause.add(Expr::col(("i", "account_id")).is_in(accounts));
            }
            query.cond_where(clause);
        }
        self.0.all(
            query
                .order_by(("i", "updated"), Order::Desc)
                .order_by(("i", "id"), Order::Asc),
            item_row,
        )
    }
}

fn item_scope(id: &str) -> sea_query::UpdateStatement {
    Query::update()
        .table("vault_items")
        .and_where(Expr::col("id").eq(id))
        .take()
}

fn field_scope(column: &'static str, value: &str) -> sea_query::UpdateStatement {
    Query::update()
        .table("vault_fields")
        .and_where(Expr::col(column).eq(value))
        .take()
}

impl VaultItems<'_, W<'_>> {
    pub fn link_account(&mut self, item: &str, account: &str) -> Result<()> {
        self.0.exec(item_scope(item).value("account_id", account))?;
        Ok(())
    }

    pub fn unlink_account(&mut self, account: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("vault_items")
                .value("account_id", Option::<String>::None)
                .and_where(Expr::col("account_id").eq(account)),
        )?;
        Ok(())
    }

    pub fn insert_vault(&mut self, row: &VaultRow) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("vaults")
                .columns(VAULT_COLS)
                .values_panic([
                    row.id.as_str().into(),
                    row.workspace_id.as_str().into(),
                    row.name.as_str().into(),
                    row.scope.as_str().into(),
                    row.owner_user.as_deref().into(),
                    row.backend.as_str().into(),
                    row.created.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn seed_templates(&mut self, templates: &[Template]) -> Result<()> {
        for row in templates {
            self.0.exec(
                Query::insert()
                    .into_table("vault_templates")
                    .columns([
                        "id",
                        "workspace_id",
                        "name",
                        "fields",
                        "default_sensitivity",
                        "source",
                    ])
                    .values_panic([
                        row.id.as_str().into(),
                        "*".into(),
                        row.name.as_str().into(),
                        serde_json::to_string(&row.fields)?.into(),
                        row.default_sensitivity.as_str().into(),
                        row.source.as_str().into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["workspace_id", "id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
        }
        Ok(())
    }

    pub fn insert_item(&mut self, row: &ItemRow) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("vault_items")
                .columns(ITEM_COLS.iter().copied())
                .values_panic([
                    row.id.as_str().into(),
                    row.workspace_id.as_str().into(),
                    row.vault_id.as_str().into(),
                    row.template.as_str().into(),
                    row.title.as_str().into(),
                    row.site.as_str().into(),
                    row.account_id.as_deref().into(),
                    row.sensitivity.as_str().into(),
                    row.dek_wrapped.clone().into(),
                    row.kek_version.into(),
                    row.external_ref.as_deref().into(),
                    row.status.as_str().into(),
                    row.created_by.as_str().into(),
                    row.created.into(),
                    row.updated.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn update_item_meta(&mut self, row: &ItemRow, at: i64) -> Result<()> {
        self.0.exec(item_scope(&row.id).values([
            ("title", row.title.as_str().into()),
            ("site", row.site.as_str().into()),
            ("account_id", row.account_id.as_deref().into()),
            ("sensitivity", row.sensitivity.as_str().into()),
            ("updated", at.into()),
        ]))?;
        Ok(())
    }

    pub fn touch_item(&mut self, id: &str, at: i64) -> Result<()> {
        self.0.exec(item_scope(id).value("updated", at))?;
        Ok(())
    }

    pub fn destroy_item(&mut self, id: &str, at: i64) -> Result<()> {
        let erasure = self.0.db.dialect().erased_blob("dek_wrapped");
        self.0.exec(
            item_scope(id)
                .value("dek_wrapped", erasure)
                .and_where(Expr::col("dek_wrapped").is_not_null()),
        )?;
        self.erase_fields("item_id", id)?;
        self.0.exec(field_scope("item_id", id).values([
            ("value_ct", Option::<Vec<u8>>::None.into()),
            ("value_plain", Option::<String>::None.into()),
            ("hint", "".into()),
            ("updated", at.into()),
        ]))?;
        self.0.exec(item_scope(id).values([
            ("dek_wrapped", Option::<Vec<u8>>::None.into()),
            ("kek_version", Option::<i64>::None.into()),
            ("external_ref", Option::<String>::None.into()),
            ("status", "deleted".into()),
            ("updated", at.into()),
        ]))?;
        Ok(())
    }

    pub fn insert_field(&mut self, ws: &str, row: &FieldRow) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("vault_fields")
                .columns(["workspace_id"].into_iter().chain(FIELD_COLS))
                .values_panic([
                    ws.into(),
                    row.id.as_str().into(),
                    row.item_id.as_str().into(),
                    row.label.as_str().into(),
                    row.kind.as_str().into(),
                    i64::from(row.secret).into(),
                    row.value_plain.as_deref().into(),
                    row.value_ct.clone().into(),
                    row.hint.as_str().into(),
                    row.sort.into(),
                    row.updated.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn set_field_value(
        &mut self,
        id: &str,
        plain: Option<&str>,
        encrypted: Option<&[u8]>,
        hint: &str,
        at: i64,
    ) -> Result<()> {
        self.erase_fields("id", id)?;
        self.0.exec(field_scope("id", id).values([
            ("value_plain", plain.into()),
            ("value_ct", encrypted.map(<[u8]>::to_vec).into()),
            ("hint", hint.into()),
            ("updated", at.into()),
        ]))?;
        Ok(())
    }

    fn delete_field_rows(&mut self, column: &'static str, id: &str) -> Result<()> {
        self.erase_fields(column, id)?;
        self.0.exec(
            Query::delete()
                .from_table("vault_fields")
                .and_where(Expr::col(column).eq(id)),
        )?;
        Ok(())
    }

    pub fn delete_field(&mut self, id: &str) -> Result<()> {
        self.delete_field_rows("id", id)
    }

    pub fn delete_fields(&mut self, item: &str) -> Result<()> {
        self.delete_field_rows("item_id", item)
    }

    fn erase_fields(&mut self, column: &'static str, id: &str) -> Result<()> {
        let erasure = self.0.db.dialect().erased_blob("value_ct");
        self.0.exec(
            field_scope(column, id)
                .value("value_ct", erasure)
                .and_where(Expr::col("value_ct").is_not_null()),
        )?;
        Ok(())
    }
}

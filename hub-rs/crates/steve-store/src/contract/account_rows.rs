use sea_query::{
    Cond, Expr, ExprTrait, Func, JoinType, LikeExpr, OnConflict, Order, Query, SelectStatement,
};

use crate::{Contacts, Read, Result, W, base::Viewer, now, uid, vault_items::VaultItems};

pub struct Accounts<'a, C>(pub &'a mut C);

/// An `accounts` row. Never holds secrets: a card is only its last four digits.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: String,
    pub workspace_id: String,
    pub system_id: String,
    pub family: String,
    pub party_id: String,
    pub kind: String,
    pub handle: String,
    pub handle_display: String,
    pub display_name: String,
    pub verified: bool,
    pub confidence: f64,
    pub source: String,
    pub source_label: String,
    pub status: String,
    pub revision: i64,
    pub created: i64,
    pub updated: i64,
}

/// An account to insert, with its normalized primary handle.
#[derive(Debug, Clone)]
pub struct NewAccount {
    pub system_id: String,
    pub family: String,
    pub party_id: String,
    pub kind: String,
    pub handle: String,
    pub handle_display: String,
    pub display_name: String,
    pub verified: bool,
    pub confidence: f64,
    pub source: String,
    pub source_label: String,
}

/// Columns an account edit may set; `None` = unchanged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountPatch {
    pub display_name: Option<String>,
    pub kind: Option<String>,
    pub party_id: Option<String>,
    pub verified: Option<bool>,
    pub confidence: Option<f64>,
}

/// Filters of `GET /accounts` once the scope is resolved to party conditions.
#[derive(Debug, Clone, Default)]
pub struct AccountQuery {
    /// Only this party.
    pub party: Option<String>,
    /// Exclude these parties (`scope=others`).
    pub not_parties: Vec<String>,
    pub system: Option<String>,
}

/// `object_type` of accounts in `revisions`.
pub const ACCOUNT: &str = "account";

const COLS: [&str; 17] = [
    "id",
    "workspace_id",
    "system_id",
    "family",
    "party_id",
    "kind",
    "handle",
    "handle_display",
    "display_name",
    "verified",
    "confidence",
    "source",
    "source_label",
    "status",
    "revision",
    "created",
    "updated",
];

fn accounts() -> SelectStatement {
    Query::select()
        .from_as("accounts", "a")
        .columns(COLS.map(|col| ("a", col)))
        .take()
}

fn account(row: &super::Row<'_>) -> Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        system_id: row.get(2)?,
        family: row.get(3)?,
        party_id: row.get(4)?,
        kind: row.get(5)?,
        handle: row.get(6)?,
        handle_display: row.get(7)?,
        display_name: row.get(8)?,
        verified: row.get(9)?,
        confidence: row.get(10)?,
        source: row.get(11)?,
        source_label: row.get(12)?,
        status: row.get(13)?,
        revision: row.get(14)?,
        created: row.get(15)?,
        updated: row.get(16)?,
    })
}

fn handles() -> SelectStatement {
    Query::select().from("account_handles").take()
}

impl<C: Read> Accounts<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<Account>> {
        self.0
            .one(accounts().and_where(Expr::col(("a", "id")).eq(id)), account)
    }

    pub fn identification_candidates(&mut self, ws: &str) -> Result<Vec<Account>> {
        self.0.all(
            accounts()
                .and_where(Expr::col(("a", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "status")).eq("active"))
                .and_where(Expr::col(("a", "kind")).is_not_in(["bot", "app_bot"]))
                .order_by(("a", "created"), Order::Asc)
                .order_by(("a", "id"), Order::Asc),
            account,
        )
    }

    pub fn of_party(&mut self, ws: &str, party: &str) -> Result<Vec<Account>> {
        let order = self.0.db().dialect().insertion_order(Some("a"));
        self.0.all(
            accounts()
                .and_where(Expr::col(("a", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "party_id")).eq(party))
                .order_by(("a", "created"), Order::Asc)
                .order_by_expr(order, Order::Asc),
            account,
        )
    }

    pub fn count_of(&mut self, ws: &str, party: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("accounts")
                    .expr(Func::count(Expr::col("id")))
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("party_id").eq(party))
                    .and_where(Expr::col("status").eq("active")),
                |row| row.get(0),
            )?
            .unwrap_or(0))
    }

    pub fn list(&mut self, viewer: &Viewer, filter: &AccountQuery) -> Result<Vec<Account>> {
        let order = self.0.db().dialect().insertion_order(Some("a"));
        let mut q = accounts();
        q.and_where(Expr::col(("a", "workspace_id")).eq(viewer.ws.as_str()))
            .order_by(("a", "created"), Order::Asc)
            .order_by_expr(order, Order::Asc);
        if let Some(party) = &filter.party {
            q.and_where(Expr::col(("a", "party_id")).eq(party.as_str()));
        }
        if !filter.not_parties.is_empty() {
            q.and_where(
                Expr::col(("a", "party_id"))
                    .is_not_in(filter.not_parties.iter().map(String::as_str)),
            );
        }
        if let Some(system) = &filter.system {
            q.and_where(Expr::col(("a", "system_id")).eq(system.as_str()));
        }
        let mut rows = self.0.all(&q, account)?;
        let ids = rows
            .iter()
            .map(|row| row.party_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let visible = Contacts::new(self.0).visible_parties(viewer, ids)?;
        rows.retain(|row| visible.contains(&row.party_id));
        Ok(rows)
    }

    pub fn parties_by_handle(&mut self, ws: &str, text: &str) -> Result<Vec<String>> {
        let dialect = self.0.db().dialect();
        let text = text.to_lowercase();
        let matching = |column| {
            dialect.contains(
                Func::lower(Expr::col(column)).into(),
                Expr::value(text.as_str()),
            )
        };
        let other = Query::select()
            .from_as("account_handles", "h")
            .column(("h", "account_id"))
            .and_where(Expr::col(("h", "account_id")).equals(("a", "id")))
            .and_where(matching(("h", "handle")))
            .take();
        self.0.all(
            Query::select()
                .distinct()
                .from_as("accounts", "a")
                .column(("a", "party_id"))
                .and_where(Expr::col(("a", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "status")).eq("active"))
                .cond_where(
                    Cond::any()
                        .add(matching(("a", "handle_display")))
                        .add(Expr::exists(other)),
                )
                .order_by(("a", "party_id"), Order::Asc),
            |row| row.get(0),
        )
    }

    pub fn by_handle(&mut self, ws: &str, family: &str, handle: &str) -> Result<Option<String>> {
        self.0.one(
            handles()
                .column("account_id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("family").eq(family))
                .and_where(Expr::col("handle").eq(handle)),
            |row| row.get(0),
        )
    }

    pub fn preferred_handle(
        &mut self,
        ws: &str,
        party: &str,
        family: &str,
        system: &str,
        connection: Option<&str>,
    ) -> Result<Option<String>> {
        let scope = Expr::col(("h", "connection_id"));
        let allowed = match connection {
            Some(id) => scope.clone().is_null().or(scope.clone().eq(id)),
            None => scope.clone().is_null(),
        };
        let order = self.0.db().dialect().insertion_order(Some("h"));
        self.0.one(
            Query::select()
                .from_as("account_handles", "h")
                .column(("h", "handle"))
                .join_as(
                    JoinType::InnerJoin,
                    "accounts",
                    "a",
                    Expr::col(("a", "id")).equals(("h", "account_id")),
                )
                .and_where(Expr::col(("a", "workspace_id")).eq(ws))
                .and_where(Expr::col(("h", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "party_id")).eq(party))
                .and_where(Expr::col(("h", "family")).eq(family))
                .and_where(allowed)
                .order_by_expr(scope.is_null(), Order::Asc)
                .order_by_expr(Expr::col(("a", "system_id")).ne(system), Order::Asc)
                .order_by_expr(
                    Expr::col(("h", "handle")).ne(Expr::col(("a", "handle"))),
                    Order::Asc,
                )
                .order_by(("h", "created"), Order::Asc)
                .order_by_expr(order, Order::Asc)
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn owners_of_accounts(
        &mut self,
        ws: &str,
        parties: &[String],
    ) -> Result<Vec<(String, String)>> {
        let ids = self.0.db().dialect().text_array(parties)?;
        self.0.all(
            Query::select()
                .columns(["id", "party_id"])
                .from("accounts")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("party_id").in_subquery(ids)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// The party's published and alternate handles, paired with each account's system.
    pub fn matching_handles(
        &mut self,
        ws: &str,
        party: &str,
        limit: u64,
    ) -> Result<Vec<(String, String)>> {
        let aliases = Query::select()
            .columns([("a", "system_id"), ("h", "handle")])
            .from_as("account_handles", "h")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).eq(Expr::col(("h", "account_id"))),
            )
            .and_where(Expr::col(("a", "workspace_id")).eq(ws))
            .and_where(Expr::col(("h", "workspace_id")).eq(ws))
            .and_where(Expr::col(("a", "party_id")).eq(party))
            .take();
        self.0.all(
            Query::select()
                .columns(["system_id", "handle"])
                .from("accounts")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("party_id").eq(party))
                .union(sea_query::UnionType::Distinct, aliases)
                .limit(limit),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn handles_of(&mut self, id: &str) -> Result<Vec<(String, Option<String>)>> {
        let order = self.0.db().dialect().insertion_order(Some("h"));
        self.0.all(
            Query::select()
                .from_as("account_handles", "h")
                .columns([("h", "handle"), ("h", "connection_id")])
                .join_as(
                    JoinType::InnerJoin,
                    "accounts",
                    "a",
                    Expr::col(("a", "id")).equals(("h", "account_id")),
                )
                .and_where(Expr::col(("h", "account_id")).eq(id))
                .order_by_expr(
                    Expr::col(("h", "handle")).ne(Expr::col(("a", "handle"))),
                    Order::Asc,
                )
                .order_by(("h", "created"), Order::Asc)
                .order_by_expr(order, Order::Asc),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn email_handles(&mut self, ws: &str, party: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .from_as("account_handles", "h")
                .column(("h", "handle"))
                .join_as(
                    JoinType::InnerJoin,
                    "accounts",
                    "a",
                    Expr::col(("a", "id")).equals(("h", "account_id")),
                )
                .and_where(Expr::col(("h", "workspace_id")).eq(ws))
                .and_where(Expr::col(("h", "family")).eq("email"))
                .and_where(Expr::col(("a", "party_id")).eq(party))
                .order_by(("h", "handle"), Order::Asc),
            |row| row.get(0),
        )
    }

    /// Primary and additional handles owned by these parties in the workspace.

    pub(in crate::contract) fn ids_by_parties(
        &mut self,
        parties: &[String],
    ) -> Result<Vec<String>> {
        if parties.is_empty() {
            return Ok(vec![]);
        }
        let parties = self.0.db().dialect().text_array(parties)?;
        self.0.all(
            Query::select()
                .column("id")
                .from("accounts")
                .and_where(Expr::col("party_id").in_subquery(parties)),
            |row| row.get(0),
        )
    }

    pub fn party_handles(&mut self, ws: &str, parties: &[String]) -> Result<Vec<String>> {
        if parties.is_empty() {
            return Ok(vec![]);
        }
        let party_ids = self.0.db().dialect().text_array(parties)?;
        let mut handles = self.0.all(
            Query::select()
                .column(("a", "handle"))
                .from_as("accounts", "a")
                .and_where(Expr::col(("a", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "party_id")).in_subquery(party_ids.clone())),
            |row| row.get(0),
        )?;
        handles.extend(
            self.0.all(
                Query::select()
                    .column(("h", "handle"))
                    .from_as("account_handles", "h")
                    .join_as(
                        JoinType::InnerJoin,
                        "accounts",
                        "a",
                        Expr::col(("a", "id")).equals(("h", "account_id")),
                    )
                    .and_where(Expr::col(("h", "workspace_id")).eq(ws))
                    .and_where(Expr::col(("a", "party_id")).in_subquery(party_ids)),
                |row| row.get::<String>(0),
            )?,
        );
        Ok(handles)
    }

    pub fn primary_emails(&mut self, ws: &str, parties: &[String]) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for ids in parties.chunks(500) {
            out.extend(
                self.0.all(
                    Query::select()
                        .from("accounts")
                        .column("handle")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("family").eq("email"))
                        .and_where(Expr::col("party_id").is_in(ids.iter().map(String::as_str))),
                    |row| row.get::<String>(0),
                )?,
            );
        }
        Ok(out)
    }

    pub fn emails_with_local(&mut self, ws: &str, local: &str) -> Result<Vec<(String, String)>> {
        let mut pattern = String::with_capacity(local.len() + 2);
        for ch in local.chars() {
            if matches!(ch, '%' | '_' | '\\') {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        pattern.push_str("@%");
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            handles()
                .columns(["handle", "account_id"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("family").eq("email"))
                .and_where(Expr::col("handle").like(LikeExpr::new(pattern).escape('\\')))
                .order_by("created", Order::Asc)
                .order_by_expr(order, Order::Asc),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }
}

fn update(id: &str) -> sea_query::UpdateStatement {
    Query::update()
        .table("accounts")
        .and_where(Expr::col("id").eq(id))
        .take()
}

impl Accounts<'_, W<'_>> {
    pub fn insert(&mut self, ws: &str, row: &NewAccount) -> Result<Account> {
        let id = uid("acc");
        let at = now();
        self.0.exec(
            Query::insert()
                .into_table("accounts")
                .columns([
                    "id",
                    "workspace_id",
                    "system_id",
                    "family",
                    "party_id",
                    "kind",
                    "handle",
                    "handle_display",
                    "display_name",
                    "verified",
                    "confidence",
                    "source",
                    "source_label",
                    "created",
                    "updated",
                ])
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    row.system_id.as_str().into(),
                    row.family.as_str().into(),
                    row.party_id.as_str().into(),
                    row.kind.as_str().into(),
                    row.handle.as_str().into(),
                    row.handle_display.as_str().into(),
                    row.display_name.as_str().into(),
                    i64::from(row.verified).into(),
                    row.confidence.into(),
                    row.source.as_str().into(),
                    row.source_label.as_str().into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        self.add_handle(ws, &row.family, &row.handle, &id, None)?;
        crate::repo::must(self.get(&id)?)
    }

    pub fn update(&mut self, id: &str, patch: &AccountPatch, revision: Option<i64>) -> Result<()> {
        let values = [
            (
                "display_name",
                patch.display_name.as_deref().map(sea_query::Value::from),
            ),
            ("kind", patch.kind.as_deref().map(sea_query::Value::from)),
            (
                "party_id",
                patch.party_id.as_deref().map(sea_query::Value::from),
            ),
            (
                "verified",
                patch.verified.map(|flag| i64::from(flag).into()),
            ),
            ("confidence", patch.confidence.map(Into::into)),
            ("revision", revision.map(Into::into)),
        ];
        if values.iter().any(|(_, value)| value.is_some()) {
            self.0.exec(
                update(id)
                    .values(
                        values
                            .into_iter()
                            .filter_map(|(column, value)| value.map(|v| (column, Expr::value(v)))),
                    )
                    .value("updated", now()),
            )?;
        }
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        VaultItems(self.0).unlink_account(id)?;
        self.0.exec(
            Query::delete()
                .from_table("account_handles")
                .and_where(Expr::col("account_id").eq(id)),
        )?;
        self.0.exec(
            Query::delete()
                .from_table("accounts")
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn move_party(
        &mut self,
        ws: &str,
        ids: &[String],
        from: &str,
        to: &str,
    ) -> Result<Vec<String>> {
        let at = now();
        let mut moved = Vec::new();
        for id in ids {
            let writes = self.0.exec(
                update(id)
                    .value("party_id", to)
                    .value("updated", at)
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("party_id").eq(from)),
            )?;
            if writes != 0 {
                moved.push(id.clone());
            }
        }
        Ok(moved)
    }

    pub fn add_handle(
        &mut self,
        ws: &str,
        family: &str,
        handle: &str,
        account: &str,
        connection: Option<&str>,
    ) -> Result<bool> {
        Ok(self.0.exec(
            Query::insert()
                .into_table("account_handles")
                .columns([
                    "workspace_id",
                    "family",
                    "handle",
                    "account_id",
                    "connection_id",
                    "created",
                ])
                .values_panic([
                    ws.into(),
                    family.into(),
                    handle.into(),
                    account.into(),
                    connection.into(),
                    now().into(),
                ])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "family", "handle"])
                        .do_nothing()
                        .to_owned(),
                ),
        )? != 0)
    }

    pub fn clear_connection_handles(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("account_handles")
                .and_where(Expr::col("connection_id").eq(id)),
        )?;
        Ok(())
    }
}

impl<C: Read> Accounts<'_, C> {
    pub fn display(&mut self, ws: &str, id: &str) -> Result<Option<(String, String)>> {
        self.0.one(
            Query::select()
                .from("accounts")
                .columns(["id", "display_name", "handle_display"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            |row| {
                let display_name: String = row.get(1)?;
                let name = if display_name.is_empty() {
                    row.get(2)?
                } else {
                    display_name
                };
                Ok((row.get(0)?, name))
            },
        )
    }

    pub fn ownership_matches(&mut self, ws: &str, text: &str) -> Result<Vec<(String, bool)>> {
        let dialect = self.0.db().dialect();
        let matches =
            |column| dialect.contains(Func::lower(Expr::col(column)).into(), Expr::value(text));
        let pattern = sea_query::Cond::any()
            .add(matches(("a", "display_name")))
            .add(matches(("a", "handle_display")))
            .add(matches(("h", "handle")));
        let rows = self.0.all(
            Query::select()
                .from_as("accounts", "a")
                .join_as(
                    JoinType::LeftJoin,
                    "account_handles",
                    "h",
                    Expr::col(("a", "id")).equals(("h", "account_id")),
                )
                .columns([
                    ("a", "id"),
                    ("a", "display_name"),
                    ("a", "handle_display"),
                    ("h", "handle"),
                ])
                .and_where(Expr::col(("a", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "status")).ne("deleted"))
                .cond_where(pattern)
                .order_by(("a", "id"), Order::Asc),
            |row| {
                Ok((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<String>(2)?,
                    row.get::<Option<String>>(3)?,
                ))
            },
        )?;
        let mut out: Vec<(String, bool)> = Vec::new();
        for (id, display, primary, handle) in rows {
            let exact = [Some(display), Some(primary), handle]
                .into_iter()
                .flatten()
                .any(|name| name.to_ascii_lowercase() == text);
            if let Some((_, previous)) = out.last_mut().filter(|(last, _)| last == &id) {
                *previous |= exact;
            } else if out.len() < 20 {
                out.push((id, exact));
            } else {
                break;
            }
        }
        Ok(out)
    }

    pub fn service_identities(&mut self, ws: &str, kinds: &[&str]) -> Result<Vec<Account>> {
        self.0.all(
            accounts()
                .and_where(Expr::col(("a", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "status")).ne("deleted"))
                .and_where(Expr::col(("a", "kind")).is_in(kinds.iter().copied()))
                .order_by(("a", "created"), Order::Asc),
            account,
        )
    }

    pub fn display_matches(&mut self, ws: &str, text: &str) -> Result<Vec<String>> {
        let dialect = self.0.db().dialect();
        let contains = |column: &'static str| {
            dialect.contains(
                Func::lower(Expr::col(column)).into(),
                Func::lower(Expr::value(text)).into(),
            )
        };
        self.0.all(
            Query::select()
                .from("accounts")
                .column("id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .cond_where(
                    Cond::any()
                        .add(contains("display_name"))
                        .add(contains("handle_display")),
                ),
            |row| row.get(0),
        )
    }
}

use sea_query::{Cond, Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use crate::{BotRows, Read, Result, UserRows, W};

pub struct SecretUses<'a, C>(pub &'a mut C);

/// One `secret_uses` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseRecord {
    pub id: String,
    pub workspace_id: String,
    pub item_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub session_id: Option<String>,
    pub work_item_id: Option<String>,
    pub purpose: String,
    pub mode: String,
    pub command: Option<String>,
    pub fields: Vec<String>,
    pub decision_id: Option<String>,
    pub grant_id: Option<String>,
    pub device_id: String,
    pub status: String,
    pub exit_code: Option<i64>,
    pub created: i64,
    pub finished: Option<i64>,
    pub run_id: Option<String>,
}

fn use_row(r: &super::Row<'_>) -> Result<UseRecord> {
    let fields: String = r.get(10)?;
    Ok(UseRecord {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        item_id: r.get(2)?,
        actor_type: r.get(3)?,
        actor_id: r.get(4)?,
        session_id: r.get(5)?,
        work_item_id: r.get(6)?,
        purpose: r.get(7)?,
        mode: r.get(8)?,
        command: r.get(9)?,
        fields: serde_json::from_str(&fields).unwrap_or_default(),
        decision_id: r.get(11)?,
        grant_id: r.get(12)?,
        device_id: r.get(13)?,
        status: r.get(14)?,
        exit_code: r.get(15)?,
        created: r.get(16)?,
        finished: r.get(17)?,
        run_id: r.get(18)?,
    })
}

/// A row of `GET /vault/uses`: the use, the item title, the actor's display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseListRow {
    pub u: UseRecord,
    pub item_title: String,
    pub actor_name: Option<String>,
}

const COLS: [&str; 19] = [
    "id",
    "workspace_id",
    "item_id",
    "actor_type",
    "actor_id",
    "session_id",
    "work_item_id",
    "purpose",
    "mode",
    "command",
    "fields",
    "decision_id",
    "grant_id",
    "device_id",
    "status",
    "exit_code",
    "created",
    "finished",
    "run_id",
];

fn uses() -> SelectStatement {
    Query::select()
        .from_as("secret_uses", "u")
        .columns(COLS.map(|col| ("u", col)))
        .take()
}

impl<C: Read> SecretUses<'_, C> {
    pub fn first_pending_at(&mut self) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .column("created")
                .from("secret_uses")
                .and_where(Expr::col("status").eq("pending"))
                .order_by("created", Order::Asc)
                .limit(1),
            |r| r.get(0),
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<UseRecord>> {
        self.0
            .one(uses().and_where(Expr::col(("u", "id")).eq(id)), use_row)
    }

    pub fn in_status(&mut self, status: &str, before: i64, limit: u64) -> Result<Vec<UseRecord>> {
        self.0.all(
            uses()
                .and_where(Expr::col(("u", "status")).eq(status))
                .and_where(Expr::col(("u", "created")).lte(before))
                .order_by(("u", "created"), Order::Asc)
                .order_by(("u", "id"), Order::Asc)
                .limit(limit),
            use_row,
        )
    }

    pub fn list(
        &mut self,
        ws: &str,
        user: &str,
        item: Option<&str>,
        bot: Option<&str>,
        limit: i64,
    ) -> Result<Vec<UseListRow>> {
        let mut query = uses();
        query
            .column(("i", "title"))
            .join_as(
                JoinType::InnerJoin,
                "vault_items",
                "i",
                Expr::col(("i", "id")).equals(("u", "item_id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "vaults",
                "v",
                Expr::col(("v", "id")).equals(("i", "vault_id")),
            )
            .and_where(Expr::col(("u", "workspace_id")).eq(ws))
            .and_where(Expr::col(("i", "workspace_id")).eq(ws))
            .cond_where(
                Cond::any().add(Expr::col(("v", "scope")).eq("shared")).add(
                    Cond::all()
                        .add(Expr::col(("v", "scope")).eq("personal"))
                        .add(Expr::col(("v", "owner_user")).eq(user)),
                ),
            )
            .order_by(("u", "created"), Order::Desc)
            .order_by(("u", "id"), Order::Desc);
        if let Ok(limit) = u64::try_from(limit) {
            query.limit(limit);
        }
        if let Some(item) = item {
            query.and_where(Expr::col(("u", "item_id")).eq(item));
        }
        if let Some(bot) = bot {
            query
                .and_where(Expr::col(("u", "actor_type")).eq("bot"))
                .and_where(Expr::col(("u", "actor_id")).eq(bot));
        }
        let mut rows = self.0.all(&query, |row| {
            Ok(UseListRow {
                u: use_row(row)?,
                item_title: row.get(19)?,
                actor_name: None,
            })
        })?;
        for row in &mut rows {
            row.actor_name = match row.u.actor_type.as_str() {
                "bot" => BotRows(self.0).name(&row.u.actor_id)?,
                "me" => UserRows(self.0).display_name(&row.u.workspace_id, &row.u.actor_id)?,
                _ => None,
            };
        }
        Ok(rows)
    }
}

impl SecretUses<'_, W<'_>> {
    pub fn insert(&mut self, row: &UseRecord) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("secret_uses")
                .columns(COLS)
                .values_panic([
                    row.id.as_str().into(),
                    row.workspace_id.as_str().into(),
                    row.item_id.as_str().into(),
                    row.actor_type.as_str().into(),
                    row.actor_id.as_str().into(),
                    row.session_id.as_deref().into(),
                    row.work_item_id.as_deref().into(),
                    row.purpose.as_str().into(),
                    row.mode.as_str().into(),
                    row.command.as_deref().into(),
                    serde_json::to_string(&row.fields)?.into(),
                    row.decision_id.as_deref().into(),
                    row.grant_id.as_deref().into(),
                    row.device_id.as_str().into(),
                    row.status.as_str().into(),
                    row.exit_code.into(),
                    row.created.into(),
                    row.finished.into(),
                    row.run_id.as_deref().into(),
                ]),
        )?;
        Ok(())
    }

    pub fn set_decision(&mut self, id: &str, decision: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("secret_uses")
                .value("decision_id", decision)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn transition(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        grant: Option<&str>,
        exit: Option<i64>,
        finished: Option<i64>,
    ) -> Result<bool> {
        Ok(self.0.exec(
            Query::update()
                .table("secret_uses")
                .value("status", to)
                .value(
                    "grant_id",
                    Func::coalesce([Expr::value(grant), Expr::col("grant_id")]),
                )
                .value(
                    "exit_code",
                    Func::coalesce([Expr::value(exit), Expr::col("exit_code")]),
                )
                .value(
                    "finished",
                    Func::coalesce([Expr::value(finished), Expr::col("finished")]),
                )
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("status").eq(from)),
        )? != 0)
    }

    pub fn delete_before(&mut self, at: i64) -> Result<usize> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("secret_uses")
                .and_where(Expr::col("created").lt(at)),
        )? as usize)
    }
}

pub struct NewUseLog<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub item: &'a str,
    pub actor_type: &'a str,
    pub actor: &'a str,
    pub purpose: &'a str,
    pub mode: &'a str,
    pub fields: &'a [String],
    pub status: &'a str,
    pub at: i64,
}

fn internal(ws: &str, item: &str, actor: &str) -> SelectStatement {
    Query::select()
        .from("secret_uses")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("item_id").eq(item))
        .and_where(Expr::col("mode").eq("internal"))
        .and_where(Expr::col("actor_id").eq(actor))
        .take()
}

impl<C: Read> SecretUses<'_, C> {
    pub fn has_internal(&mut self, ws: &str, item: &str, actor: &str, since: i64) -> Result<bool> {
        Ok(self
            .0
            .one(
                internal(ws, item, actor)
                    .column("id")
                    .and_where(Expr::col("status").eq("ok"))
                    .and_where(Expr::col("created").gte(since))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn last_internal(
        &mut self,
        ws: &str,
        item: &str,
        actor: &str,
    ) -> Result<Option<(String, String, i64)>> {
        let insertion_order = self.0.db().dialect().insertion_order(None);
        self.0.one(
            internal(ws, item, actor)
                .columns(["id", "status", "finished", "created"])
                .order_by("created", Order::Desc)
                .order_by_expr(insertion_order, Order::Desc)
                .limit(1),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<Option<i64>>(2)?.unwrap_or(row.get(3)?),
                ))
            },
        )
    }
}

impl SecretUses<'_, W<'_>> {
    pub fn log(&mut self, row: &NewUseLog<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("secret_uses")
                .columns([
                    "id",
                    "workspace_id",
                    "item_id",
                    "actor_type",
                    "actor_id",
                    "purpose",
                    "mode",
                    "fields",
                    "status",
                    "created",
                    "finished",
                ])
                .values_panic([
                    row.id.into(),
                    row.ws.into(),
                    row.item.into(),
                    row.actor_type.into(),
                    row.actor.into(),
                    row.purpose.into(),
                    row.mode.into(),
                    serde_json::to_string(row.fields)?.into(),
                    row.status.into(),
                    row.at.into(),
                    row.at.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn touch_finished(&mut self, id: &str, at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("secret_uses")
                .value("finished", at)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}

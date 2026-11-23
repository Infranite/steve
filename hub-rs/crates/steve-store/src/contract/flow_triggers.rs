use sea_query::{Cond, Expr, ExprTrait, JoinType, Order, Query, SelectStatement};

use crate::{Read, Result, W};

pub struct FlowTriggers<'a, C>(pub &'a mut C);

pub struct DueTrigger {
    pub id: String,
    pub flow_id: String,
    pub node_id: String,
    pub cron: Option<String>,
    pub tz: Option<String>,
    pub next_at: i64,
    pub missed: Option<String>,
}

fn enabled_time_triggers() -> SelectStatement {
    Query::select()
        .from_as("flow_triggers", "t")
        .join_as(
            JoinType::InnerJoin,
            "flows",
            "f",
            Expr::col(("f", "id")).equals(("t", "flow_id")),
        )
        .and_where(Expr::col(("t", "kind")).eq("time"))
        .and_where(Expr::col(("t", "enabled")).eq(1_i64))
        .and_where(Expr::col(("f", "enabled")).eq(1_i64))
        .and_where(Expr::col(("f", "deleted")).eq(0_i64))
        .and_where(Expr::col(("t", "next_at")).is_not_null())
        .take()
}

fn due(dialect: super::Dialect, now: i64) -> SelectStatement {
    let at = Expr::col(("t", "next_at"));
    enabled_time_triggers()
        .cond_where(
            Cond::any()
                .add(dialect.invalid_integer(at.clone()))
                .add(at.lte(now)),
        )
        .take()
}

impl<C: Read> FlowTriggers<'_, C> {
    pub fn time_node(&mut self, flow: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("node_id")
                .from("flow_triggers")
                .and_where(Expr::col("flow_id").eq(flow))
                .and_where(Expr::col("kind").eq("time"))
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn next_deadline(&mut self, now: i64) -> Result<Option<i64>> {
        super::next_deadline(
            self.0,
            &mut enabled_time_triggers(),
            Expr::col(("t", "next_at")),
            now,
        )
    }

    /// The locator stays readable when a trigger's business columns are malformed.
    pub fn due(&mut self, now: i64, limit: u64) -> Result<Vec<i64>> {
        let dialect = self.0.db().dialect();
        self.0.all(
            due(dialect, now)
                .expr(dialect.insertion_order(Some("t")))
                .order_by_columns([(("t", "next_at"), Order::Asc), (("t", "id"), Order::Asc)])
                .limit(limit),
            |row| row.get(0),
        )
    }

    pub fn due_at(&mut self, locator: i64, now: i64) -> Result<Option<DueTrigger>> {
        let dialect = self.0.db().dialect();
        self.0.one(
            due(dialect, now)
                .columns([
                    ("t", "id"),
                    ("t", "flow_id"),
                    ("t", "node_id"),
                    ("t", "cron"),
                    ("t", "tz"),
                    ("t", "next_at"),
                    ("t", "missed"),
                ])
                .and_where(dialect.insertion_order(Some("t")).eq(locator)),
            |row| {
                Ok(DueTrigger {
                    id: row.get(0)?,
                    flow_id: row.get(1)?,
                    node_id: row.get(2)?,
                    cron: row.get(3)?,
                    tz: row.get(4)?,
                    next_at: row.get(5)?,
                    missed: row.get(6)?,
                })
            },
        )
    }

    pub fn scope_at(&mut self, locator: i64) -> Result<Option<(String, String)>> {
        let key = self.0.db().dialect().insertion_order(None);
        self.0.one(
            Query::select()
                .columns(["workspace_id", "id"])
                .from("flow_triggers")
                .and_where(key.eq(locator)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn has_item_listener(&mut self, ws: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column(("t", "flow_id"))
                    .from_as("flow_triggers", "t")
                    .join_as(
                        JoinType::InnerJoin,
                        "flows",
                        "f",
                        Expr::col(("f", "id")).equals(("t", "flow_id")),
                    )
                    .and_where(Expr::col(("t", "workspace_id")).eq(ws))
                    .and_where(Expr::col(("t", "kind")).eq("item"))
                    .and_where(Expr::col(("t", "enabled")).eq(1_i64))
                    .and_where(Expr::col(("f", "enabled")).eq(1_i64))
                    .and_where(Expr::col(("f", "deleted")).eq(0_i64))
                    .limit(1),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }
}

pub struct EventTrigger {
    pub flow: String,
    pub version: i64,
    pub node: String,
    pub kind: String,
    pub types: Vec<String>,
    pub connections: Vec<String>,
    pub scopes: Vec<String>,
    pub spaces: Vec<String>,
}

impl<C: Read> FlowTriggers<'_, C> {
    pub fn for_source(&mut self, ws: &str, source: &str) -> Result<Vec<EventTrigger>> {
        self.0.all(
            Query::select()
                .columns([
                    ("t", "flow_id"),
                    ("f", "version"),
                    ("t", "node_id"),
                    ("t", "kind"),
                    ("t", "types"),
                    ("t", "connections"),
                    ("t", "scopes"),
                    ("t", "spaces"),
                ])
                .from_as("flow_triggers", "t")
                .join_as(
                    JoinType::InnerJoin,
                    "flows",
                    "f",
                    Expr::col(("f", "id")).equals(("t", "flow_id")),
                )
                .and_where(Expr::col(("t", "workspace_id")).eq(ws))
                .and_where(Expr::col(("t", "kind")).is_in(["event", "message", "item"]))
                .and_where(Expr::col(("t", "enabled")).eq(1_i64))
                .and_where(Expr::col(("f", "enabled")).eq(1_i64))
                .and_where(Expr::col(("f", "deleted")).eq(0_i64))
                .cond_where(
                    Cond::any()
                        .add(Expr::col(("t", "source")).is_null())
                        .add(Expr::col(("t", "source")).is_in(["", source])),
                )
                .order_by_columns([
                    (("f", "created"), Order::Asc),
                    (("t", "flow_id"), Order::Asc),
                ]),
            |row| {
                Ok(EventTrigger {
                    flow: row.get(0)?,
                    version: row.get(1)?,
                    node: row.get(2)?,
                    kind: row.get(3)?,
                    types: row.get::<super::Json<_>>(4)?.0,
                    connections: row.get::<super::Json<_>>(5)?.0,
                    scopes: row.get::<super::Json<_>>(6)?.0,
                    spaces: row.get::<super::Json<_>>(7)?.0,
                })
            },
        )
    }
}

impl FlowTriggers<'_, W<'_>> {
    pub fn disable_at(&mut self, locator: i64) -> Result<()> {
        let key = self.0.dialect().insertion_order(None);
        self.0.exec(
            Query::update()
                .table("flow_triggers")
                .value("enabled", 0_i64)
                .and_where(key.eq(locator)),
        )?;
        Ok(())
    }

    pub fn disable(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flow_triggers")
                .value("enabled", 0_i64)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn advance(&mut self, id: &str, expected: i64, next: Option<i64>) -> Result<bool> {
        Ok(self.0.exec(
            Query::update()
                .table("flow_triggers")
                .value("next_at", next)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("next_at").eq(expected)),
        )? == 1)
    }
}

use sea_query::{
    Expr, ExprTrait, Func, OnConflict, Order, Query, SelectStatement, UpdateStatement, Value,
};
use serde::{Deserialize, Serialize};

use crate::contract::Row;
use crate::{Read, Result, W};

pub struct Todos<'a, C>(pub &'a mut C);

#[derive(Serialize)]
pub struct MatterTodo {
    pub id: String,
    pub text: String,
    pub status: String,
}

impl<C: Read> Todos<'_, C> {
    pub fn for_matter(&mut self, ws: &str, matter: &str, owner: &str) -> Result<Vec<MatterTodo>> {
        self.0.all(
            Query::select()
                .columns(["id", "text", "state"])
                .from("todos")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("matter_id").eq(matter))
                .and_where(Expr::col("owner").eq(owner))
                .order_by("created", Order::Desc)
                .limit(20),
            |r| {
                Ok(MatterTodo {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    status: r.get(2)?,
                })
            },
        )
    }
}

impl Todos<'_, W<'_>> {
    pub fn move_matter(&mut self, ws: &str, from: &str, into: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("todos")
                .value("matter_id", into)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("matter_id").eq(from)),
        )?;
        Ok(())
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TodoFields {
    pub text: String,
    pub kind: String,
    pub parent: Option<String>,
    pub matter: Option<String>,
    pub target_date: Option<String>,
    pub due: Option<i64>,
    pub due_date: Option<String>,
    /// not set by hand: a hand-set deadline has none
    pub due_quote: Option<String>,
    pub remind_at: Option<i64>,
    pub snooze_until: Option<i64>,
    pub pri: i64,
    pub star: bool,
    pub recur: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TodoFilter {
    /// all when empty
    pub states: Vec<String>,
    pub kind: Option<String>,
    /// only the children of this goal
    pub parent: Option<String>,
    pub limit: usize,
}

#[derive(Serialize)]
pub struct Todo {
    pub id: String,
    pub state: String,
    pub item_id: Option<String>,
    pub source: String,
    pub writer: String,
    pub created: i64,
    pub updated: i64,
    pub closed: Option<i64>,
    pub due_kind: Option<String>,
    pub block_event: Option<String>,
    #[serde(skip)]
    pub owner: String,
    #[serde(flatten)]
    pub fields: TodoFields,
}

pub struct TodoInput<'a> {
    pub ws: &'a str,
    pub owner: &'a str,
    pub fields: &'a TodoFields,
    pub source: &'a str,
    pub writer: &'a str,
    pub packet: Option<&'a str>,
    pub now: i64,
}

const EDITABLE: [&str; 14] = [
    "text",
    "kind",
    "parent_id",
    "matter_id",
    "target_date",
    "due",
    "due_date",
    "due_quote",
    "remind_at",
    "snooze_until",
    "pri",
    "star",
    "recur",
    "due_kind",
];

fn read_fields(r: &Row<'_>) -> Result<TodoFields> {
    Ok(TodoFields {
        text: r.get(0)?,
        kind: r.get(1)?,
        parent: r.get(2)?,
        matter: r.get(3)?,
        target_date: r.get(4)?,
        due: r.get(5)?,
        due_date: r.get(6)?,
        due_quote: r.get(7)?,
        remind_at: r.get(8)?,
        snooze_until: r.get(9)?,
        pri: r.get(10)?,
        star: r.get(11)?,
        recur: r.get(12)?,
    })
}

fn field_values(f: &TodoFields) -> [Value; 14] {
    let due_kind = if f.due.is_some() {
        Some("time")
    } else {
        f.due_date.as_ref().map(|_| "date")
    };
    [
        f.text.as_str().into(),
        f.kind.as_str().into(),
        f.parent.as_deref().into(),
        f.matter.as_deref().into(),
        f.target_date.as_deref().into(),
        f.due.into(),
        f.due_date.as_deref().into(),
        f.due_quote.as_deref().into(),
        f.remind_at.into(),
        f.snooze_until.into(),
        f.pri.into(),
        i64::from(f.star).into(),
        f.recur.as_deref().into(),
        due_kind.into(),
    ]
}

fn todo_row(r: &Row<'_>) -> Result<Todo> {
    Ok(Todo {
        id: r.get(14)?,
        state: r.get(15)?,
        item_id: r.get(16)?,
        source: r.get(17)?,
        writer: r.get(18)?,
        created: r.get(19)?,
        updated: r.get(20)?,
        closed: r.get(21)?,
        due_kind: r.get(13)?,
        block_event: r.get(22)?,
        owner: r.get(23)?,
        fields: read_fields(r)?,
    })
}

fn in_workspace(ws: &str) -> SelectStatement {
    Query::select()
        .from("todos")
        .and_where(Expr::col("workspace_id").eq(ws))
        .take()
}

fn full(ws: &str) -> SelectStatement {
    in_workspace(ws)
        .columns(EDITABLE)
        .columns([
            "id",
            "state",
            "item_id",
            "source",
            "writer",
            "created",
            "updated",
            "closed",
            "block_event",
            "owner",
        ])
        .take()
}

fn owned(ws: &str, user: &str) -> SelectStatement {
    in_workspace(ws)
        .and_where(Expr::col("owner").eq(user))
        .take()
}

fn key(ws: &str, id: &str) -> Expr {
    Expr::col("workspace_id").eq(ws).and(Expr::col("id").eq(id))
}

fn edit(ws: &str, id: &str, now: i64) -> UpdateStatement {
    Query::update()
        .table("todos")
        .and_where(key(ws, id))
        .value("updated", now)
        .take()
}

impl<C: Read> Todos<'_, C> {
    pub fn get(&mut self, ws: &str, id: &str) -> Result<Option<Todo>> {
        self.0
            .one(full(ws).and_where(Expr::col("id").eq(id)), todo_row)
    }

    pub fn owner(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            in_workspace(ws)
                .column("owner")
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn has_children(&mut self, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("id")
                    .from("todos")
                    .and_where(Expr::col("parent_id").eq(id))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn children(&mut self, ws: &str, id: &str) -> Result<Vec<(String, Option<String>)>> {
        self.0.all(
            in_workspace(ws)
                .columns(["state", "item_id"])
                .and_where(Expr::col("parent_id").eq(id))
                .order_by("created", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn list(&mut self, ws: &str, owner: &str, f: &TodoFilter) -> Result<Vec<Todo>> {
        let mut q = full(ws);
        q.and_where(Expr::col("owner").eq(owner));
        if !f.states.is_empty() {
            q.and_where(Expr::col("state").is_in(f.states.iter().map(String::as_str)));
        }
        if let Some(kind) = &f.kind {
            q.and_where(Expr::col("kind").eq(kind.as_str()));
        }
        if let Some(parent) = &f.parent {
            q.and_where(Expr::col("parent_id").eq(parent.as_str()));
        }
        let date = self.0.db().dialect().local_date(Expr::col("due"));
        self.0.all(
            q.order_by_expr(Expr::col("state").ne("open"), Order::Asc)
                .order_by_expr(
                    Expr::col("due")
                        .is_null()
                        .and(Expr::col("due_date").is_null()),
                    Order::Asc,
                )
                .order_by_expr(
                    Func::coalesce([Expr::col("due_date"), date]).into(),
                    Order::Asc,
                )
                .order_by_expr(Expr::col("due").is_null(), Order::Asc)
                .order_by("due", Order::Asc)
                .order_by("created", Order::Desc)
                .limit(f.limit.clamp(1, 500) as u64),
            todo_row,
        )
    }

    pub fn sources(&mut self, id: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns(["conv", "message_id"])
                .from("todo_refs")
                .and_where(Expr::col("todo_id").eq(id))
                .order_by("rowid", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn parties(&mut self, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("party_id")
                .from("todo_parties")
                .and_where(Expr::col("todo_id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn agenda(&mut self, ws: &str, user: &str) -> Result<Vec<Todo>> {
        self.0.all(
            full(ws)
                .and_where(Expr::col("owner").eq(user))
                .and_where(Expr::col("state").eq("open"))
                .and_where(Expr::col("kind").eq("todo"))
                .order_by("star", Order::Desc)
                .order_by("pri", Order::Desc)
                .order_by_expr(
                    Func::coalesce([Expr::col("due_date"), Expr::value("")]).into(),
                    Order::Asc,
                )
                .order_by_expr(
                    Func::coalesce([Expr::col("due"), Expr::value(0_i64)]).into(),
                    Order::Asc,
                )
                .order_by("created", Order::Asc),
            todo_row,
        )
    }

    pub fn goals(&mut self, ws: &str, user: &str) -> Result<Vec<(String, String, String)>> {
        self.0.all(
            owned(ws, user)
                .columns(["id", "text", "target_date"])
                .and_where(Expr::col("state").eq("open"))
                .and_where(Expr::col("kind").eq("goal"))
                .and_where(Expr::col("target_date").is_not_null())
                .order_by("target_date", Order::Asc)
                .order_by("created", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }
}

impl Todos<'_, W<'_>> {
    pub fn insert(&mut self, t: TodoInput<'_>) -> Result<String> {
        let id = crate::uid("td");
        self.0.exec(
            Query::insert()
                .into_table("todos")
                .columns(EDITABLE.into_iter().chain([
                    "id",
                    "workspace_id",
                    "owner",
                    "source",
                    "writer",
                    "packet_id",
                    "created",
                    "updated",
                ]))
                .values_panic(field_values(t.fields).into_iter().map(Expr::value).chain([
                    id.as_str().into(),
                    t.ws.into(),
                    t.owner.into(),
                    t.source.into(),
                    t.writer.into(),
                    t.packet.into(),
                    t.now.into(),
                    t.now.into(),
                ])),
        )?;
        Ok(id)
    }

    pub fn change_fields(&mut self, ws: &str, id: &str, f: &TodoFields, now: i64) -> Result<()> {
        self.0.exec(
            edit(ws, id, now).values(
                EDITABLE
                    .into_iter()
                    .zip(field_values(f))
                    .map(|(field, value)| (field, Expr::value(value))),
            ),
        )?;
        Ok(())
    }

    pub fn set_state(
        &mut self,
        ws: &str,
        id: &str,
        state: &str,
        item: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        Ok(self.0.exec(
            edit(ws, id, now)
                .value("state", state)
                .value("item_id", item)
                .value("closed", (state != "open").then_some(now)),
        )? != 0)
    }

    pub fn add_source(&mut self, id: &str, conv: &str, message: &str) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("todo_refs")
                .columns(["todo_id", "conv", "message_id"])
                .values_panic([id.into(), conv.into(), message.into()])
                .on_conflict(
                    OnConflict::columns(["todo_id", "conv", "message_id"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn add_party(&mut self, id: &str, party: &str) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("todo_parties")
                .columns(["todo_id", "party_id"])
                .values_panic([id.into(), party.into()])
                .on_conflict(
                    OnConflict::columns(["todo_id", "party_id"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn remove(&mut self, ws: &str, id: &str) -> Result<bool> {
        Ok(self
            .0
            .exec(Query::delete().from_table("todos").and_where(key(ws, id)))?
            != 0)
    }
}

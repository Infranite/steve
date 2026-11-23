//! The workspace's skills (migration 65, `skills`): Markdown instructions the harness of a Bot
//! assigned one reads as `<name>/SKILL.md`, written by the session's Node where the harness adapter
//! declares it reads skills. Only the ones switched on for the Bot are given ([`super`]).

use crate::contract::Row;
use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};

use super::Assignments;
use crate::{Read, Result, W};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub created: i64,
    pub updated: i64,
}

pub struct Skills<'a, C>(pub &'a mut C);

const COLS: [&str; 7] = [
    "id",
    "workspace_id",
    "name",
    "description",
    "content",
    "created",
    "updated",
];

fn row(r: &Row<'_>) -> Result<Skill> {
    Ok(Skill {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        content: r.get(4)?,
        created: r.get(5)?,
        updated: r.get(6)?,
    })
}

/// A workspace's skills, by name.

fn rows() -> SelectStatement {
    Query::select().from("skills").columns(COLS).take()
}

impl<C: Read> Skills<'_, C> {
    pub fn list(&mut self, ws: &str) -> Result<Vec<Skill>> {
        self.0.all(
            rows()
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("name", Order::Asc),
            row,
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<Skill>> {
        self.0.one(rows().and_where(Expr::col("id").eq(id)), row)
    }

    pub fn name_taken(&mut self, ws: &str, name: &str, except: Option<&str>) -> Result<bool> {
        let by_name = Expr::col("name").eq(name);
        Ok(self
            .0
            .one(
                Query::select()
                    .from("skills")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(by_name)
                    .and_where(Expr::col("id").ne(except.unwrap_or_default()))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn of_bot(&mut self, bot: &str) -> Result<Vec<(Skill, bool)>> {
        super::assigned(
            self.0,
            bot,
            "skill",
            |r, id| Skills(r).get(id),
            |row| row.name.clone(),
        )
    }

    pub fn enabled_for_session(&mut self, session: &str) -> Result<Vec<Skill>> {
        let Some(bot) = Assignments(self.0).bot_of_session(session)? else {
            return Ok(Vec::new());
        };
        Ok(self
            .of_bot(&bot)?
            .into_iter()
            .filter_map(|(row, on)| on.then_some(row))
            .collect())
    }
}

impl Skills<'_, W<'_>> {
    pub fn insert(&mut self, s: &Skill) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("skills")
                .columns(COLS)
                .values_panic([
                    s.id.as_str().into(),
                    s.workspace_id.as_str().into(),
                    s.name.as_str().into(),
                    s.description.as_str().into(),
                    s.content.as_str().into(),
                    s.created.into(),
                    s.updated.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn update(&mut self, s: &Skill) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("skills")
                .and_where(Expr::col("id").eq(s.id.as_str()))
                .value("name", s.name.as_str())
                .value("description", s.description.as_str())
                .value("content", s.content.as_str())
                .value("updated", s.updated),
        )?;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<bool> {
        Assignments(self.0).remove_entry("skill", id)?;
        Ok(self.0.exec(
            Query::delete()
                .from_table("skills")
                .and_where(Expr::col("id").eq(id)),
        )? != 0)
    }
}

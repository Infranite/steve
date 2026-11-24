use sea_query::{Expr, ExprTrait, Func, Order, Query};

use crate::{Read, Result};

pub struct RecordSearch<'a, C>(pub &'a mut C);

/// One work-item record hit from direct search over `items` and `item_goals`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRecord {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub summary: String,
    pub updated: i64,
}

/// One main-thread conversation message hit from direct search over `messages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRecord {
    pub id: String,
    pub author: String,
    pub text: String,
    pub created: i64,
}

impl<C: Read> RecordSearch<'_, C> {
    /// Work items in `ws` matching every whitespace-separated term in `terms` across id, title,
    /// goal or acceptance summary, newest first.
    pub fn items(&mut self, ws: &str, terms: &[String], limit: usize) -> Result<Vec<ItemRecord>> {
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let rows = self.0.all(
            Query::select()
                .columns(["id", "title", "updated"])
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("updated", Order::Desc)
                .order_by("id", Order::Desc)
                .limit(500),
            |r| Ok((r.get::<String>(0)?, r.get::<String>(1)?, r.get::<i64>(2)?)),
        )?;
        let mut out = Vec::new();
        for (id, title, updated) in rows {
            let goal = self
                .0
                .one(
                    Query::select()
                        .column("text")
                        .from("item_goals")
                        .and_where(Expr::col("item_id").eq(id.as_str()))
                        .order_by("version", Order::Desc)
                        .limit(1),
                    |r| r.get::<String>(0),
                )?
                .unwrap_or_default();
            let summary = self
                .0
                .one(
                    Query::select()
                        .column("draft")
                        .from("decisions")
                        .and_where(Expr::col("item_id").eq(id.as_str()))
                        .and_where(Expr::col("kind").eq("accept"))
                        .order_by("created", Order::Desc)
                        .limit(1),
                    |r| r.get::<String>(0),
                )?
                .unwrap_or_default();
            let hay = format!("{id} {title} {goal} {summary}").to_lowercase();
            if terms.iter().all(|t| hay.contains(t.as_str())) {
                out.push(ItemRecord {
                    id,
                    title,
                    goal,
                    summary,
                    updated,
                });
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Main-thread chat messages in `ws` matching every term in `terms`, newest first.
    pub fn chat(
        &mut self,
        ws: &str,
        space: &str,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<ChatRecord>> {
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let dialect = self.0.db().dialect();
        let mut q = Query::select();
        q.columns(["id", "author", "text", "created"])
            .from("messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space));
        for term in terms {
            let contains = dialect.contains(
                Func::lower(Expr::col("text")).into(),
                Func::lower(Expr::value(term.as_str())).into(),
            );
            q.and_where(contains);
        }
        q.order_by("created", Order::Desc)
            .order_by("id", Order::Desc)
            .limit(limit as u64);
        self.0.all(&q, |r| {
            Ok(ChatRecord {
                id: r.get(0)?,
                author: r.get(1)?,
                text: r.get(2)?,
                created: r.get(3)?,
            })
        })
    }
}

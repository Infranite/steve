//! The main conversation's tail, history and individual message views.
use sea_query::{Alias, Expr, ExprTrait, Order, Query, SelectStatement};
use serde_json::Value;
use steve_types::{MessageView, Millis, RawJson};

use super::{Page, column, page, read_ids};
use crate::contract::{Json, Row};
use crate::{Read, Result};

pub const TAIL: u32 = 200;
pub const FIRST_PAGE: usize = 50;

fn main_query() -> SelectStatement {
    Query::select()
        .from(Alias::new("messages"))
        .and_where(column("space").eq("steve"))
        .and_where(Expr::cust(
            "COALESCE(json_extract(meta, '$.withdrawn'), 0) != 1",
        ))
        .to_owned()
}

fn select() -> SelectStatement {
    main_query()
        .columns(["id", "workspace_id", "author", "text", "item_id", "created", "meta"].map(Alias::new))
        .expr(Expr::cust(
            "CASE WHEN json_extract(meta, '$.notice.ref') IS NOT NULL THEN EXISTS (
                SELECT 1 FROM thread_notices n
                 WHERE n.workspace_id = messages.workspace_id
                   AND (n.id = json_extract(messages.meta, '$.notice.ref') OR n.host_message_id = messages.id)
                   AND n.closed_at IS NOT NULL
            ) ELSE 0 END",
        ))
        .expr(Expr::cust(
            "CASE WHEN messages.author = 'me' THEN EXISTS (
                SELECT 1 FROM thread_reply_tasks t
                 WHERE t.workspace_id = messages.workspace_id
                   AND t.space = messages.space
                   AND t.origin_message_id = messages.id
                   AND t.reply_revision = 0
                   AND t.status = 'queued'
                   AND EXISTS (
                     SELECT 1 FROM thread_reply_tasks ahead
                      WHERE ahead.workspace_id = t.workspace_id
                        AND ahead.space = t.space
                        AND ahead.status IN ('queued', 'claimed')
                        AND (ahead.input_seq < t.input_seq OR (ahead.input_seq = t.input_seq AND ahead.id < t.id))
                   )
            ) ELSE 0 END",
        ))
        .to_owned()
}

fn row(r: &Row<'_>) -> Result<MessageView> {
    let mut meta = r
        .get::<Option<Json<Value>>>(6)?
        .map_or(Value::Null, |x| x.0);
    if r.get::<bool>(7)? {
        meta["notice"]["closed"] = Value::Bool(true);
    }
    if r.get::<bool>(8)? {
        if !meta.is_object() {
            meta = Value::Object(serde_json::Map::new());
        }
        meta["held"] = Value::Bool(true);
    }
    Ok(MessageView {
        id: r.get::<String>(0)?.into(),
        ws: r.get::<String>(1)?.into(),
        author: r.get(2)?,
        text: r.get(3)?,
        item_id: r.get::<Option<String>>(4)?.map(Into::into),
        created: Millis(r.get(5)?),
        meta: RawJson::from_str(&meta.to_string())?,
    })
}

pub struct MessageViews<'a, C>(pub(super) &'a mut C);

impl<C: Read> MessageViews<'_, C> {
    pub fn hot(&mut self) -> Result<Vec<MessageView>> {
        let workspaces = self.0.all(
            main_query().column(Alias::new("workspace_id")).distinct(),
            |r| r.get::<String>(0),
        )?;
        let mut views = Vec::new();
        for ws in workspaces {
            views.extend(
                self.history(
                    &ws,
                    &Page {
                        before: None,
                        limit: TAIL,
                    },
                )?
                .0,
            );
        }
        Ok(views)
    }

    pub fn tail_ids(&mut self, ws: &str) -> Result<Vec<String>> {
        self.0.all(
            main_query()
                .column(Alias::new("id"))
                .and_where(column("workspace_id").eq(ws))
                .order_by(Alias::new("created"), Order::Desc)
                .order_by(Alias::new("id"), Order::Desc)
                .limit(TAIL.into()),
            |r| r.get(0),
        )
    }

    pub fn notices(&mut self, ws: &str) -> Result<Vec<String>> {
        crate::thread::notice_host_message_ids(self.0, ws)
    }

    pub fn of(&mut self, ws: &str, ids: &[String]) -> Result<Vec<MessageView>> {
        read_ids(self.0, select(), ws, ids, row)
    }

    pub fn history(
        &mut self,
        ws: &str,
        options: &Page,
    ) -> Result<(Vec<MessageView>, Option<String>)> {
        page(self.0, &mut select(), ws, options, "created", row, |v| {
            (v.created.0, v.id.as_str())
        })
    }
}

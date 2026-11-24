//! Decisions (the single source of 待处理) and the main thread with Steve.

use super::Cached;
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::Value;

use super::{all, one, to_json};
use crate::{Decision, Message, Result, Store, now, uid};
use crate::{DecisionRows, truthy};

/// One row in `messages`; the caller publishes its event. Returns the id.
pub(crate) fn message_insert(
    c: &Connection,
    ws: &str,
    author: &str,
    text: &str,
    item_id: Option<&str>,
    meta: &Value,
    space: &str,
) -> Result<String> {
    let id = uid("msg");
    let mut db = crate::contract::Sqlite(c);
    let statement = message_statement(
        crate::contract::Dialect::Sqlite,
        &id,
        ws,
        author,
        text,
        item_id,
        meta,
        space,
    );
    crate::contract::exec_on(&mut db, &statement)?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn message_statement(
    dialect: crate::contract::Dialect,
    id: &str,
    ws: &str,
    author: &str,
    text: &str,
    item_id: Option<&str>,
    meta: &Value,
    space: &str,
) -> sea_query::InsertStatement {
    use sea_query::{Expr, ExprTrait, Func, Query};
    let last = Query::select()
        .expr(Expr::from(Func::max(Expr::col("created"))).add(Expr::cust("1")))
        .from("messages")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("space").eq(space))
        .take();
    // Created timestamps strictly increase in a conversation because they are the listing cursor.
    let next = dialect.greatest(
        Expr::val(now()),
        Func::coalesce([Expr::from(last), Expr::cust("0")]).into(),
    );
    Query::insert()
        .into_table("messages")
        .columns([
            "id",
            "workspace_id",
            "space",
            "author",
            "text",
            "item_id",
            "meta",
            "created",
        ])
        .values_panic([
            id.into(),
            ws.into(),
            space.into(),
            author.into(),
            text.into(),
            item_id.into(),
            truthy(meta).then(|| meta.to_string()).into(),
            next,
        ])
        .take()
}

/// Add a message and publish it with the rest of the write.
pub fn append_message(
    w: &mut crate::W<'_>,
    ws: &str,
    author: &str,
    text: &str,
    item_id: Option<&str>,
    meta: &Value,
    space: &str,
) -> Result<String> {
    let id = uid("msg");
    let stmt = message_statement(w.dialect(), &id, ws, author, text, item_id, meta, space);
    w.exec(&stmt)?;
    w.emit(steve_types::DomainEvent::from(
        steve_types::WorkEvent::Message {
            space: space.into(),
            ws: ws.into(),
            id: id.clone().into(),
        },
    ));
    Ok(id)
}

impl Store {
    pub fn decisions(&self) -> Decisions<'_> {
        Decisions(self)
    }
    pub fn messages(&self) -> Messages<'_> {
        Messages(self)
    }
}

/// The source revision of a replayable decision. The same request ID may be reused
/// in a different attempt, so its execution identity is part of its key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionSource {
    Permission { node: String, attempt: String },
    TurnStall { attempt: String },
    Record,
    Revision { revision: String },
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewDecision {
    pub workspace_id: String,
    /// `accept` | `ask` | `approve` | `fail` | `propose`
    pub kind: String,
    pub subject_type: String,
    pub subject_id: String,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub question: String,
    #[serde(default)]
    pub question_template: Option<steve_types::DecisionPrompt>,
    pub draft: Option<String>,
    pub options: Option<Vec<String>>,
    pub requested_by: String,
    #[serde(default)]
    pub source: Option<DecisionSource>,
}

pub struct Decisions<'a>(&'a Store);

const DECISION: &str = "SELECT * FROM decisions WHERE id = ?";

fn changed(w: &crate::WriteConn, id: &str) -> Result<Option<Decision>> {
    let d = one(w.c, DECISION, [id], Decision::from_row)?;
    if let Some(row) = &d {
        w.publish(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Decision {
                ws: row.workspace_id.clone().into(),
                id: id.into(),
                item_id: Some(row.item_id.clone().map(Into::into)),
            },
        ));
    }
    Ok(d)
}

impl Decisions<'_> {
    /// Open decisions, oldest first.
    pub fn open(&self, ws: &str) -> Result<Vec<Decision>> {
        self.0.read_conn(|c| all(c, "SELECT * FROM decisions WHERE workspace_id = ? AND status = 'open' ORDER BY created", [ws], Decision::from_row))
    }

    /// Settled decisions resolved after `since`, newest first, at most 100.
    pub fn recent(&self, ws: &str, since: i64) -> Result<Vec<Decision>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT * FROM decisions WHERE workspace_id = ? AND status != 'open' AND resolved > ? ORDER BY resolved DESC LIMIT 100",
                params![ws, since],
                Decision::from_row,
            )
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Decision>> {
        self.0
            .read_conn(|c| one(c, DECISION, [id], Decision::from_row))
    }

    pub fn of_item(&self, item_id: &str) -> Result<Vec<Decision>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT * FROM decisions WHERE item_id = ? ORDER BY created",
                [item_id],
                Decision::from_row,
            )
        })
    }

    pub fn create(&self, p: &NewDecision) -> Result<Option<Decision>> {
        self.0.write(|w| DecisionRows(w).open(p).map(Some))
    }

    pub fn resolve(
        &self,
        id: &str,
        result: &Value,
        status: &str,
        undo: &Value,
    ) -> Result<Option<Decision>> {
        self.0.write(|w| {
            let mut rows = DecisionRows(w);
            rows.settle(id, result, status, undo)?;
            rows.get(id)
        })
    }

    /// Replaces the result of a decision already settled, e.g. what became of an approved release after it was
    /// answered. Status, resolved time and undo stay; publishes like any change.
    pub fn set_result(&self, id: &str, result: &Value) -> Result<Option<Decision>> {
        self.0.write_conn(|w| {
            w.c.run(
                "UPDATE decisions SET result = ? WHERE id = ? AND status != 'open'",
                params![to_json(result)?, id],
            )?;
            changed(w, id)
        })
    }

    /// Merge annotations into the current settled result under the writer lock. An answer snapshot
    /// may predate a background result (for example a release failure); never replace that result.
    pub fn merge_result(&self, id: &str, annotations: &Value) -> Result<Option<Decision>> {
        self.0.write_conn(|w| {
            w.c.run(
                "UPDATE decisions SET result = json_patch(CASE WHEN json_type(result) = 'object' THEN result ELSE '{}' END, ?) WHERE id = ? AND status != 'open'",
                params![to_json(annotations)?, id],
            )?;
            changed(w, id)
        })
    }

    pub fn reopen(&self, id: &str) -> Result<Option<Decision>> {
        self.0.write(|w| {
            crate::decision_repo::reopen(w, id)?;
            DecisionRows(w).get(id)
        })
    }
}

/// One thread per workspace and `space` (only `steve` today).
pub struct Messages<'a>(&'a Store);

impl Messages<'_> {
    /// The last `limit` messages, oldest first (TS defaults: space `steve`, limit 200).
    pub fn list(&self, ws: &str, space: &str, limit: i64) -> Result<Vec<Message>> {
        self.0.read_conn(|c| {
            let mut v = all(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = ? ORDER BY created DESC LIMIT ?",
                params![ws, space, limit],
                Message::from_row,
            )?;
            v.reverse();
            Ok(v)
        })
    }

    /// The newest `limit` messages of a conversation up to `until` (created, inclusive), oldest first:
    /// what was said around a message however much was said after it.
    pub fn list_until(
        &self,
        ws: &str,
        space: &str,
        until: i64,
        limit: i64,
    ) -> Result<Vec<Message>> {
        self.0.read_conn(|c| {
            let mut v = all(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = ? AND created <= ? ORDER BY created DESC LIMIT ?",
                params![ws, space, until, limit],
                Message::from_row,
            )?;
            v.reverse();
            Ok(v)
        })
    }

    /// Messages whose text contains `q` (ASCII case-insensitive) or that belong to item `q`, newest first.
    pub fn search(&self, ws: &str, space: &str, q: &str, limit: i64) -> Result<Vec<Message>> {
        let like = format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = ? AND (text LIKE ? ESCAPE '\\' OR upper(coalesce(item_id, '')) = upper(?)) ORDER BY created DESC LIMIT ?",
                params![ws, space, like, q, limit],
                Message::from_row,
            )
        })
    }

    /// The answer an item was taken from (its offer, marked `promoted` with the item), if any.
    pub fn promoted_to(&self, ws: &str, item: &str) -> Result<Option<Message>> {
        self.0.read_conn(|c| {
            one(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND json_extract(meta, '$.promoted') = ? ORDER BY created LIMIT 1",
                params![ws, item],
                Message::from_row,
            )
        })
    }

    /// Returns the id. `meta` is stored only if truthy (TS defaults: no item, meta null, space `steve`).
    pub fn add(
        &self,
        ws: &str,
        author: &str,
        text: &str,
        item_id: Option<&str>,
        meta: &Value,
        space: &str,
    ) -> Result<String> {
        self.0.write_conn(|w| {
            let id = message_insert(w.c, ws, author, text, item_id, meta, space)?;
            w.publish(steve_types::DomainEvent::from(
                steve_types::WorkEvent::Message {
                    space: space.into(),
                    ws: ws.into(),
                    id: id.clone().into(),
                },
            ));
            Ok(id)
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Message>> {
        self.0.read_conn(|c| {
            one(
                c,
                "SELECT * FROM messages WHERE id = ?",
                [id],
                Message::from_row,
            )
        })
    }

    pub fn patch_meta(&self, id: &str, meta: &Value) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run(
                "UPDATE messages SET meta = ? WHERE id = ?",
                params![to_json(meta)?, id],
            )?;
            if let Some((ws, space)) = crate::repo::one(
                w.c,
                "SELECT workspace_id, space FROM messages WHERE id = ?",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Message {
                        space: space.into(),
                        ws: ws.into(),
                        id: id.into(),
                    },
                ));
            }
            Ok(())
        })
    }

    /// Rewrites a message's text and meta (thread flow-back merging a reply into the one before).
    pub fn patch_text_meta(&self, id: &str, text: &str, meta: &Value) -> Result<()> {
        self.0.write_conn(|w| {
            w.c.run(
                "UPDATE messages SET text = ?, meta = ? WHERE id = ?",
                params![text, to_json(meta)?, id],
            )?;
            if let Some((ws, space)) = crate::repo::one(
                w.c,
                "SELECT workspace_id, space FROM messages WHERE id = ?",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )? {
                w.publish(steve_types::DomainEvent::from(
                    steve_types::WorkEvent::Message {
                        space: space.into(),
                        ws: ws.into(),
                        id: id.into(),
                    },
                ));
            }
            Ok(())
        })
    }

    /// The newest message by `author` in the `steve` space. (TS returns `meta` unparsed here; nothing
    /// serializes this one, so it is parsed like everywhere else.)
    pub fn last_of(&self, ws: &str, author: &str) -> Result<Option<Message>> {
        self.0.read_conn(|c| {
            one(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = 'steve' AND author = ? ORDER BY created DESC LIMIT 1",
                [ws, author],
                Message::from_row,
            )
        })
    }
}

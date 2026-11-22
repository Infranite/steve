//! Persisted conversational-config drafts (`changesets`): an ordered list of operations
//! `{"op":"<module>.<verb>", …}`, the preview lines, and after apply the `before` of every operation so
//! it can be undone (24 hours). The engine that runs the operations is in steve-modules.

use crate::repo::Cached;
use rusqlite::{Connection, Row, params};
use serde::Serialize;
use serde_json::Value;

use super::Change;
use crate::repo::to_json;
use crate::types::json_or;
use crate::{Result, Tx, now, uid};

/// How long an applied changeset can be undone.
pub const UNDO_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Changeset {
    pub id: String,
    pub workspace_id: String,
    pub target_type: String,
    pub target_id: Option<String>,
    /// The target's `revision` when drafted; apply refuses if it moved.
    pub target_revision: Option<i64>,
    pub title: String,
    pub href: String,
    pub changes: Vec<Change>,
    pub ops: Vec<Value>,
    /// One entry per operation once applied.
    pub before: Option<Vec<Value>>,
    /// `draft` | `applied` | `dropped` | `undone` | `stale`
    pub status: String,
    /// `me` | `steve` | `plugin:<id>` | `signal`
    pub origin: String,
    pub request: String,
    pub basis: String,
    pub created_by: String,
    pub created: i64,
    pub applied: Option<i64>,
    pub undone: Option<i64>,
}

impl Changeset {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            target_type: r.get("target_type")?,
            target_id: r.get("target_id")?,
            target_revision: r.get("target_revision")?,
            title: r.get("title")?,
            href: r.get("href")?,
            changes: json_or(r.get("changes")?, Vec::new()),
            ops: json_or(r.get("ops")?, Vec::new()),
            before: json_or(r.get("before")?, None),
            status: r.get("status")?,
            origin: r.get("origin")?,
            request: r.get("request")?,
            basis: r.get("basis")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
            applied: r.get("applied")?,
            undone: r.get("undone")?,
        })
    }

    /// Can it still be undone at `at`?
    pub fn undoable(&self, at: i64) -> bool {
        self.status == "applied" && self.applied.is_some_and(|t| at - t <= UNDO_WINDOW_MS)
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewChangeset {
    pub target_type: String,
    pub target_id: Option<String>,
    pub target_revision: Option<i64>,
    pub title: String,
    pub href: String,
    pub changes: Vec<Change>,
    pub ops: Vec<Value>,
    pub origin: String,
    pub request: String,
    pub basis: String,
    pub created_by: String,
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Changeset>> {
    let r = c
        .prepare_cached("SELECT * FROM changesets WHERE id = ?")?
        .query_row([id], Changeset::from_row);
    match r {
        Ok(x) => Ok(Some(x)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Newest first, optionally of one status.
pub fn recent(
    c: &Connection,
    ws: &str,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<Changeset>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM changesets WHERE workspace_id = ? AND (?2 IS NULL OR status = ?2) ORDER BY created DESC, id LIMIT ?3",
    )?;
    let rows = st.query_map(params![ws, status, limit], Changeset::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Stores a draft (`chs_` id). Publishes `{type:"changeset"}`.
pub fn create(tx: &Tx, ws: &str, p: &NewChangeset) -> Result<Changeset> {
    let id = uid("chs");
    tx.prepare_cached(
        "INSERT INTO changesets (id, workspace_id, target_type, target_id, target_revision, title, href, changes, ops, status, origin, request, basis, created_by, created)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'draft', ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        ws,
        p.target_type,
        p.target_id,
        p.target_revision,
        p.title,
        p.href,
        to_json(&p.changes)?,
        to_json(&p.ops)?,
        p.origin,
        p.request,
        p.basis,
        p.created_by,
        now()
    ])?;
    publish(tx, &id, ws);
    crate::repo::must(get(tx, &id)?)
}

fn publish(tx: &Tx, id: &str, ws: &str) {
    tx.publish(steve_types::DomainEvent::from(
        steve_types::ConfigEvent::Changeset {
            id: id.into(),
            ws: ws.into(),
        },
    ));
}

/// `draft → applied` with the operations' `before`; `false` if it was not a draft any more (a second
/// apply racing the first).
pub fn mark_applied(tx: &Tx, cs: &Changeset, before: &[Value]) -> Result<bool> {
    let n = tx.run(
        "UPDATE changesets SET status = 'applied', before = ?, applied = ? WHERE id = ? AND status = 'draft'",
        params![to_json(&before)?, now(), cs.id],
    )?;
    if n > 0 {
        publish(tx, &cs.id, &cs.workspace_id);
    }
    Ok(n > 0)
}

/// `applied → undone`; `false` if it was not applied.
pub fn mark_undone(tx: &Tx, cs: &Changeset) -> Result<bool> {
    let n = tx.run(
        "UPDATE changesets SET status = 'undone', undone = ? WHERE id = ? AND status = 'applied'",
        params![now(), cs.id],
    )?;
    if n > 0 {
        publish(tx, &cs.id, &cs.workspace_id);
    }
    Ok(n > 0)
}

/// `draft → dropped | stale`; `false` if it was not a draft.
pub fn close_draft(tx: &Tx, cs: &Changeset, status: &str) -> Result<bool> {
    let n = tx.run(
        "UPDATE changesets SET status = ? WHERE id = ? AND status = 'draft'",
        params![status, cs.id],
    )?;
    if n > 0 {
        publish(tx, &cs.id, &cs.workspace_id);
    }
    Ok(n > 0)
}

use crate::contract::{Json, Row as ContractRow};
use crate::{Read, W};
use sea_query::{Expr, ExprTrait, Query};

pub struct ChangeSets<'a, C>(pub &'a mut C);

const COLUMNS: [&str; 18] = [
    "id",
    "workspace_id",
    "target_type",
    "target_id",
    "target_revision",
    "title",
    "href",
    "changes",
    "ops",
    "before",
    "status",
    "origin",
    "request",
    "basis",
    "created_by",
    "created",
    "applied",
    "undone",
];

fn read_changeset(row: &ContractRow<'_>) -> Result<Changeset> {
    Ok(Changeset {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        target_type: row.get(2)?,
        target_id: row.get(3)?,
        target_revision: row.get(4)?,
        title: row.get(5)?,
        href: row.get(6)?,
        changes: row.get::<Json<_>>(7)?.0,
        ops: row.get::<Json<_>>(8)?.0,
        before: row.get::<Option<Json<_>>>(9)?.map(|v| v.0),
        status: row.get(10)?,
        origin: row.get(11)?,
        request: row.get(12)?,
        basis: row.get(13)?,
        created_by: row.get(14)?,
        created: row.get(15)?,
        applied: row.get(16)?,
        undone: row.get(17)?,
    })
}

impl<C: Read> ChangeSets<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<Changeset>> {
        self.0.one(
            Query::select()
                .columns(COLUMNS)
                .from("changesets")
                .and_where(Expr::col("id").eq(id)),
            read_changeset,
        )
    }
}

impl ChangeSets<'_, W<'_>> {
    pub fn create(&mut self, ws: &str, draft: &NewChangeset) -> Result<Changeset> {
        let id = uid("chs");
        self.0.exec(
            Query::insert()
                .into_table("changesets")
                .columns([
                    "id",
                    "workspace_id",
                    "target_type",
                    "target_id",
                    "target_revision",
                    "title",
                    "href",
                    "changes",
                    "ops",
                    "status",
                    "origin",
                    "request",
                    "basis",
                    "created_by",
                    "created",
                ])
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    draft.target_type.as_str().into(),
                    draft.target_id.as_deref().into(),
                    draft.target_revision.into(),
                    draft.title.as_str().into(),
                    draft.href.as_str().into(),
                    to_json(&draft.changes)?.into(),
                    to_json(&draft.ops)?.into(),
                    "draft".into(),
                    draft.origin.as_str().into(),
                    draft.request.as_str().into(),
                    draft.basis.as_str().into(),
                    draft.created_by.as_str().into(),
                    now().into(),
                ]),
        )?;
        self.emit(&id, ws);
        crate::repo::must(self.get(&id)?)
    }

    fn emit(&mut self, id: &str, ws: &str) {
        self.0.emit(steve_types::DomainEvent::from(
            steve_types::ConfigEvent::Changeset {
                id: id.into(),
                ws: ws.into(),
            },
        ));
    }

    fn transition(
        &mut self,
        cs: &Changeset,
        expected: &str,
        next: &str,
        update: impl FnOnce(&mut sea_query::UpdateStatement),
    ) -> Result<bool> {
        let mut query = Query::update();
        query
            .table("changesets")
            .value("status", next)
            .and_where(Expr::col("id").eq(cs.id.as_str()))
            .and_where(Expr::col("status").eq(expected));
        update(&mut query);
        let changed = self.0.exec(&query)? > 0;
        if changed {
            self.emit(&cs.id, &cs.workspace_id);
        }
        Ok(changed)
    }

    pub fn mark_applied(&mut self, cs: &Changeset, before: &[Value]) -> Result<bool> {
        let before = to_json(&before)?;
        self.transition(cs, "draft", "applied", move |query| {
            query.value("before", before).value("applied", now());
        })
    }

    pub fn mark_undone(&mut self, cs: &Changeset) -> Result<bool> {
        self.transition(cs, "applied", "undone", |query| {
            query.value("undone", now());
        })
    }

    pub fn close_draft(&mut self, cs: &Changeset, status: &str) -> Result<bool> {
        self.transition(cs, "draft", status, |_| {})
    }
}

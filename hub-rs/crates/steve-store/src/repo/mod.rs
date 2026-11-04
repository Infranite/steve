//! Repositories (hub/src/repo.ts). Lists are scoped to one workspace; single objects are fetched by global id.
//! Each `store.x()` returns a borrowed view named like the TS object (`items`, `bots`, …).

mod annotations;
mod bots;
mod catalog;
mod core;
mod decisions;
mod files;
mod items;
pub mod kinds;
pub mod lanes;
mod sessions;
mod stats;
mod thread;
pub mod yolo;

pub use self::annotations::*;
pub use self::bots::*;
pub use self::catalog::*;
pub use self::core::*;
pub use self::decisions::*;
pub use self::files::*;
pub use self::items::*;
pub use self::sessions::*;
pub use self::stats::*;
pub use self::thread::*;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Params, Row};
use serde::Serialize;

use crate::{Result, now};

/// Every statement goes through the connection's statement cache (TS re-prepares on each call; with the
/// capacity raised in `db::pragmas` the hot statements stay compiled).
pub trait Cached {
    fn run<P: Params>(&self, sql: &str, p: P) -> rusqlite::Result<usize>;
    fn row<T, P: Params, F: FnOnce(&Row<'_>) -> rusqlite::Result<T>>(
        &self,
        sql: &str,
        p: P,
        f: F,
    ) -> rusqlite::Result<T>;
}

impl Cached for Connection {
    fn run<P: Params>(&self, sql: &str, p: P) -> rusqlite::Result<usize> {
        self.prepare_cached(sql)?.execute(p)
    }
    fn row<T, P: Params, F: FnOnce(&Row<'_>) -> rusqlite::Result<T>>(
        &self,
        sql: &str,
        p: P,
        f: F,
    ) -> rusqlite::Result<T> {
        self.prepare_cached(sql)?.query_row(p, f)
    }
}

pub(crate) fn one<T>(
    c: &Connection,
    sql: &str,
    p: impl Params,
    f: fn(&Row) -> rusqlite::Result<T>,
) -> Result<Option<T>> {
    Ok(c.prepare_cached(sql)?.query_row(p, f).optional()?)
}

pub(crate) fn all<T>(
    c: &Connection,
    sql: &str,
    p: impl Params,
    f: fn(&Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut st = c.prepare_cached(sql)?;
    let rows = st.query_map(p, f)?;
    Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
}

/// For rows the same write just inserted (TS `get(id)!`).
pub(crate) fn must<T>(v: Option<T>) -> Result<T> {
    v.ok_or(crate::StoreError::Sqlite(
        rusqlite::Error::QueryReturnedNoRows,
    ))
}

pub(crate) fn exists(c: &Connection, sql: &str, p: impl Params) -> Result<bool> {
    Ok(c.prepare_cached(sql)?.exists(p)?)
}

/// `setClause`: only the keys that were given, booleans as 0/1, explicit nulls kept.
#[derive(Default)]
pub(crate) struct Sets {
    cols: Vec<&'static str>,
    vals: Vec<Sql>,
}

impl Sets {
    pub fn val(&mut self, col: &'static str, v: Option<impl Into<Sql>>) -> &mut Self {
        if let Some(v) = v {
            self.cols.push(col);
            self.vals.push(v.into());
        }
        self
    }

    /// A nullable column: outer `None` = not given, `Some(None)` = set NULL.
    pub fn null(&mut self, col: &'static str, v: Option<Option<impl Into<Sql>>>) -> &mut Self {
        if let Some(v) = v {
            self.cols.push(col);
            self.vals.push(v.map_or(Sql::Null, Into::into));
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    pub fn sql(&self) -> String {
        self.cols
            .iter()
            .map(|k| format!("{k} = ?"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The SET values followed by `tail` (the WHERE parameters).
    pub fn params(&self, tail: impl IntoIterator<Item = Sql>) -> Vec<Sql> {
        self.vals.iter().cloned().chain(tail).collect()
    }
}

pub(crate) fn to_json(v: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

/// `files.length ? JSON.stringify(files) : null` for the `timeline.files` / `queued.files` columns.
pub(crate) fn file_refs(files: &[crate::FileRef]) -> Result<Option<String>> {
    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(to_json(&files)?))
    }
}

/// Writes one row to `revisions` (repo.ts `revision`).
pub(crate) fn revision(
    c: &Connection,
    ws: &str,
    kind: &str,
    id: &str,
    version: i64,
    patch: &impl Serialize,
    author: &str,
) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO revisions (workspace_id, object_type, object_id, version, patch, author, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(rusqlite::params![ws, kind, id, version, to_json(patch)?, author, now()])?;
    Ok(())
}

/// Counters are global per name (`workspace_id = '*'`), so item numbers with one prefix never collide.
pub(crate) fn next_counter(c: &Connection, name: &str) -> Result<i64> {
    Ok(c.prepare_cached(
        "INSERT INTO counters (workspace_id, name, value) VALUES ('*', ?, 1) ON CONFLICT(workspace_id, name) DO UPDATE SET value = value + 1 RETURNING value",
    )?
    .query_row([name], |r| r.get(0))?)
}

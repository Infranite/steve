//! What a database must do for the store. Everything above this module (transactions, scopes,
//! events, repositories, SQL) is written once; a backend only runs what the core rendered.

use sea_query::{Expr, Query, QueryStatementWriter, SelectStatement, SqliteQueryBuilder, Values};

use crate::{Result, StoreError};

/// How statements are rendered for a backend (placeholders, quoting, upsert, `RETURNING`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
}

impl Dialect {
    /// The statement as this database writes it, with its values to bind.
    pub(crate) fn render<S: QueryStatementWriter>(self, s: &S) -> (String, Values) {
        match self {
            Dialect::Sqlite => s.build(SqliteQueryBuilder),
        }
    }

    pub(crate) fn ascii_nocase(self, expr: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Expr::cust_with_expr("(? COLLATE NOCASE)", expr),
        }
    }

    /// A bound text array as rows, so large lists use one parameter.
    pub(crate) fn text_array(
        self,
        items: &[impl AsRef<str> + serde::Serialize],
    ) -> Result<SelectStatement> {
        let json = serde_json::to_string(items).map_err(|e| StoreError::Rejected(e.to_string()))?;
        Ok(match self {
            Dialect::Sqlite => Query::select()
                .column("value")
                .from_function(sea_query::Func::cust("json_each").arg(json), "items")
                .take(),
        })
    }

    /// `array` (a JSON array column) with `item` appended.
    pub(crate) fn json_push(self, array: Expr, item: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Expr::cust_with_exprs("json_insert(?, '$[#]', ?)", [array, item]),
        }
    }
}

/// One connection of one database. Transactions and savepoints are plain SQL through [`Self::batch`];
/// only how a transaction begins differs per backend.
///
/// The built-in backend is SQLite ([`super::sqlite`]). Any other database is a driver plugin from
/// the plugin store, attached as one more implementation of this trait that forwards over the
/// plugin's socket: `begin_write`/`begin_read` → `begin`, `batch` → `exec` without values (the core
/// sends only `COMMIT`, `ROLLBACK`, `SAVEPOINT n`, `RELEASE SAVEPOINT n`, `ROLLBACK TO SAVEPOINT n`
/// and, for migrations, DDL), `exec` → `exec`, `query` → `query` (rows come back as [`Col`] values
/// and are read through a [`Row`] over them). The plugin never sees a repository: SQL, dialect,
/// transaction and scope semantics, migrations and events stay in the core, and the plugin maps its
/// client library's errors onto [`StoreError::Storage`] / [`StoreError::Rejected`].
pub trait Backend {
    fn dialect(&self) -> Dialect;
    /// One Hub per database. SQLite: the instance file lock the Hub takes before opening the
    /// store; a server database would take an advisory lock on the write connection here.
    fn lock_instance(&mut self) -> Result<()>;
    /// Opens the write transaction (SQLite `BEGIN IMMEDIATE`: the write lock at the start).
    fn begin_write(&mut self) -> Result<()>;
    /// Opens a read-only snapshot (SQLite `BEGIN`: deferred, the snapshot starts at the first read).
    fn begin_read(&mut self) -> Result<()>;
    /// A statement without values or rows: transaction control, savepoints, DDL.
    fn batch(&mut self, sql: &str) -> Result<()>;
    /// A statement with values; the number of rows it changed.
    fn exec(&mut self, sql: &str, values: &Values) -> Result<u64>;
    /// A statement returning rows (`SELECT`, or a write with `RETURNING`), each handed to `each`.
    fn query(
        &mut self,
        sql: &str,
        values: &Values,
        each: &mut dyn FnMut(&Row<'_>) -> Result<()>,
    ) -> Result<()>;
}

/// One result row, read by position.
pub struct Row<'a>(pub(crate) RowImpl<'a>);

pub(crate) enum RowImpl<'a> {
    Sqlite(&'a rusqlite::Row<'a>),
}

/// A column value as a backend hands it over.
pub enum Col<'a> {
    Null,
    Int(i64),
    Real(f64),
    Text(&'a str),
}

impl Row<'_> {
    pub(crate) fn get<T: FromCol>(&self, i: usize) -> Result<T> {
        T::from_col(self.col(i)?)
    }

    fn col(&self, i: usize) -> Result<Col<'_>> {
        match &self.0 {
            RowImpl::Sqlite(r) => super::sqlite::col(r, i),
        }
    }
}

/// Decoding one column. A value of the wrong type is a defect of this build, not a passing
/// failure of the database: [`StoreError::Rejected`].
pub trait FromCol: Sized {
    fn from_col(c: Col<'_>) -> Result<Self>;
}

fn wrong(want: &str) -> StoreError {
    StoreError::Rejected(format!("column is not {want}"))
}

impl FromCol for i64 {
    fn from_col(c: Col<'_>) -> Result<Self> {
        match c {
            Col::Int(v) => Ok(v),
            _ => Err(wrong("an integer")),
        }
    }
}

impl FromCol for f64 {
    fn from_col(c: Col<'_>) -> Result<Self> {
        match c {
            Col::Real(v) => Ok(v),
            Col::Int(v) => Ok(v as f64),
            _ => Err(wrong("a number")),
        }
    }
}

/// SQLite has no boolean: 0/1.
impl FromCol for bool {
    fn from_col(c: Col<'_>) -> Result<Self> {
        match c {
            Col::Int(v) => Ok(v != 0),
            _ => Err(wrong("a boolean")),
        }
    }
}

impl FromCol for String {
    fn from_col(c: Col<'_>) -> Result<Self> {
        match c {
            Col::Text(v) => Ok(v.to_owned()),
            _ => Err(wrong("text")),
        }
    }
}

impl<T: FromCol> FromCol for Option<T> {
    fn from_col(c: Col<'_>) -> Result<Self> {
        match c {
            Col::Null => Ok(None),
            c => T::from_col(c).map(Some),
        }
    }
}

/// A JSON column (TEXT on SQLite), decoded straight into `T`.
pub(crate) struct Json<T>(pub T);

impl<T: serde::de::DeserializeOwned> FromCol for Json<T> {
    fn from_col(c: Col<'_>) -> Result<Self> {
        match c {
            Col::Text(v) => serde_json::from_str(v)
                .map(Json)
                .map_err(|e| StoreError::Rejected(e.to_string())),
            _ => Err(wrong("JSON")),
        }
    }
}

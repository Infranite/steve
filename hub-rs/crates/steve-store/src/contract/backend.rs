//! What a database must do for the store. Everything above this module (transactions, scopes,
//! events, repositories, SQL) is written once; a backend only runs what the core rendered.

use sea_query::{
    Expr, ExprTrait, Func, FunctionCall, Query, QueryStatementWriter, SelectStatement,
    SqliteQueryBuilder, Value, Values,
};

use crate::{Result, StoreError};

/// How statements are rendered for a backend (placeholders, quoting, upsert, `RETURNING`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
}

impl Dialect {
    /// The statement as this database writes it, with its values to bind.
    pub(crate) fn render<S: QueryStatementWriter>(self, s: &S) -> Result<(String, Values)> {
        let (sql, mut values) = match self {
            Dialect::Sqlite => s.build(SqliteQueryBuilder),
        };
        // sea-query binds LIMIT and OFFSET as unsigned values. Drivers only receive BIGINTs.
        for value in &mut values.0 {
            if let Value::BigUnsigned(n) = value {
                let n = n
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| StoreError::Rejected("SQL parameter exceeds BIGINT".into()))?;
                *value = Value::BigInt(n);
            }
        }
        Ok((sql, values))
    }

    pub(crate) fn fulltext_match(self, table: &'static str, text: &str) -> Expr {
        match self {
            Dialect::Sqlite => {
                Expr::cust_with_exprs("? MATCH ?", [Expr::col(table), Expr::value(text)])
            }
        }
    }

    pub(crate) fn fulltext_score(self, table: &'static str) -> FunctionCall {
        match self {
            Dialect::Sqlite => Func::cust("bm25").arg(Expr::col(table)),
        }
    }

    pub(crate) fn contains(self, text: Expr, needle: Expr) -> Expr {
        match self {
            Self::Sqlite => Func::cust("instr")
                .args([text, needle])
                .gt(Expr::Constant(0_i64.into())),
        }
    }

    pub(crate) fn knowledge_score(self) -> Expr {
        match self {
            Self::Sqlite => Expr::cust("bm25(kb_fts, 1.0, 2.0)"),
        }
    }

    pub(crate) fn local_date(self, ms: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Expr::cust_with_expr("date(? / 1000, 'unixepoch', 'localtime')", ms),
        }
    }

    pub(crate) fn erased_blob(self, column: &'static str) -> Expr {
        match self {
            Self::Sqlite => Func::cust("zeroblob")
                .arg(Func::cust("length").arg(Expr::col(column)))
                .into(),
        }
    }

    pub(crate) fn json_valid(self, json: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Func::cust("json_valid").arg(json).into(),
        }
    }

    pub(crate) fn json_field(self, json: Expr, field: &'static str) -> Expr {
        self.json_path(json, format!("$.{field}"))
    }

    pub(crate) fn json_path(self, json: Expr, path: impl Into<String>) -> Expr {
        match self {
            Dialect::Sqlite => Func::cust("json_extract")
                .arg(json)
                .arg(Expr::Constant(path.into().into()))
                .into(),
        }
    }

    pub(crate) fn json_type(self, json: Expr, path: &'static str) -> Expr {
        match self {
            Self::Sqlite => Func::cust("json_type")
                .arg(json)
                .arg(Expr::Constant(path.into()))
                .into(),
        }
    }

    pub(crate) fn greatest(self, a: Expr, b: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Func::cust("max").args([a, b]).into(),
        }
    }

    pub(crate) fn least(self, a: Expr, b: Expr) -> Expr {
        match self {
            Self::Sqlite => Func::cust("min").args([a, b]).into(),
        }
    }

    pub(crate) fn insertion_order(self, alias: Option<&'static str>) -> Expr {
        match (self, alias) {
            (Self::Sqlite, Some(alias)) => Expr::col((alias, "rowid")),
            (Self::Sqlite, None) => Expr::col("rowid"),
        }
    }

    pub(crate) fn invalid_integer_columns(
        self,
        required: &'static str,
        optional: &[&'static str],
    ) -> sea_query::Condition {
        match self {
            Self::Sqlite => {
                let invalid = |column| self.invalid_integer(Expr::col(column));
                optional.iter().fold(
                    sea_query::Cond::any().add(invalid(required)),
                    |cond, &column| {
                        cond.add(
                            sea_query::Cond::all()
                                .add(Expr::col(column).is_not_null())
                                .add(invalid(column)),
                        )
                    },
                )
            }
        }
    }

    pub(crate) fn invalid_integer(self, value: Expr) -> Expr {
        match self {
            Self::Sqlite => self
                .storage_type(value)
                .ne(Expr::Constant("integer".into())),
        }
    }

    pub(crate) fn storage_type(self, value: Expr) -> Expr {
        match self {
            Self::Sqlite => Func::cust("typeof").arg(value).into(),
        }
    }

    pub(crate) fn null_safe_eq(self, a: Expr, b: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Expr::cust_with_exprs("? IS ?", [a, b]),
        }
    }

    /// Table function whose `value` column contains each element.
    pub(crate) fn json_array_elements(self, array: Expr) -> FunctionCall {
        match self {
            Dialect::Sqlite => Func::cust("json_each").arg(array),
        }
    }

    pub(crate) fn ascii_nocase(self, expr: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Expr::cust_with_expr("(? COLLATE NOCASE)", expr),
        }
    }

    pub(crate) fn json_integer(self, column: Expr, key: &'static str) -> Expr {
        match self {
            Dialect::Sqlite => {
                Expr::cust_with_expr("CAST(? AS BIGINT)", self.json_field(column, key))
            }
        }
    }

    pub(crate) fn json_merge_patch(self, column: Expr, patch: Expr) -> Expr {
        match self {
            Dialect::Sqlite => Expr::cust_with_exprs("json_patch(?, ?)", [column, patch]),
        }
    }

    pub(crate) fn json_set_field(self, column: Expr, field: &'static str, value: Expr) -> Expr {
        match self {
            Self::Sqlite => Func::cust("json_set")
                .args([
                    column,
                    Expr::Constant(format!("$.{field}").into()),
                    Func::cust("json").arg(value).into(),
                ])
                .into(),
        }
    }

    pub(crate) fn json_rows(self, column: Expr, alias: &'static str) -> SelectStatement {
        match self {
            Dialect::Sqlite => Query::select()
                .column((alias, "value"))
                .from_function(sea_query::Func::cust("json_each").arg(column), alias)
                .take(),
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
    Blob(&'a [u8]),
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

impl FromCol for Vec<u8> {
    fn from_col(column: Col<'_>) -> Result<Self> {
        match column {
            Col::Blob(bytes) => Ok(bytes.to_vec()),
            _ => Err(wrong("a blob")),
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

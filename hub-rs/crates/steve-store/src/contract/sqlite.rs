//! The built-in backend: SQLite, on the store's write connection or one of its pooled readers.

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, ToSql};
use sea_query::{Value, Values};

use super::backend::{Backend, Col, Dialect, Row, RowImpl};
use crate::{Result, StoreError};

/// One SQLite connection the store already holds (the write connection under its lock, or a pooled
/// read-only connection).
pub(crate) struct Sqlite<'c>(pub(crate) &'c Connection);

/// The database failing (retry later) versus this write breaking a rule.
pub(crate) fn err(e: rusqlite::Error) -> StoreError {
    match e.sqlite_error_code() {
        Some(code) if crate::storage_failure(code) => StoreError::Storage(e.to_string()),
        _ => StoreError::Rejected(e.to_string()),
    }
}

/// Binds a sea-query value without copying text. Only the types the store's schema uses are
/// accepted (integers are 64-bit, JSON is TEXT); anything else is refused rather than coerced.
struct V<'a>(&'a Value);

impl ToSql for V<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        const NULL: ToSqlOutput<'static> = ToSqlOutput::Borrowed(ValueRef::Null);
        Ok(match self.0 {
            Value::Bool(v) => v.map_or(NULL, |b| (b as i64).into()),
            Value::BigInt(v) => v.map_or(NULL, Into::into),
            // sea-query uses u64 for LIMIT and OFFSET. SQLite binds signed BIGINT.
            Value::BigUnsigned(v) => v
                .map(i64::try_from)
                .transpose()
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                .map_or(NULL, Into::into),
            Value::Double(v) => v.map_or(NULL, Into::into),
            Value::String(v) => v.as_deref().map_or(NULL, |s| {
                ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes()))
            }),
            Value::Json(v) => v.as_deref().map_or(NULL, |j| j.to_string().into()),
            other => {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    format!("the store binds no {other:?}").into(),
                ));
            }
        })
    }
}

fn bind(values: &Values) -> impl Iterator<Item = V<'_>> {
    values.0.iter().map(V)
}

pub(crate) fn col<'a>(r: &'a rusqlite::Row<'a>, i: usize) -> Result<Col<'a>> {
    Ok(match r.get_ref(i).map_err(err)? {
        ValueRef::Null => Col::Null,
        ValueRef::Integer(v) => Col::Int(v),
        ValueRef::Real(v) => Col::Real(v),
        ValueRef::Text(t) => {
            Col::Text(std::str::from_utf8(t).map_err(|e| StoreError::Rejected(e.to_string()))?)
        }
        ValueRef::Blob(_) => return Err(StoreError::Rejected("column is a blob".into())),
    })
}

impl Backend for Sqlite<'_> {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    fn lock_instance(&mut self) -> Result<()> {
        // the Hub holds its instance file lock before it opens the store; SQLite needs no other
        Ok(())
    }

    fn begin_write(&mut self) -> Result<()> {
        // IMMEDIATE: another process (the upgrade supervisor, the next release during a switch)
        // can open the same file; a DEFERRED write that read first fails at once with
        // SQLITE_BUSY_SNAPSHOT once that process committed, IMMEDIATE waits out the busy timeout
        self.batch("BEGIN IMMEDIATE")
    }

    fn begin_read(&mut self) -> Result<()> {
        self.batch("BEGIN")
    }

    fn batch(&mut self, sql: &str) -> Result<()> {
        self.0.execute_batch(sql).map_err(err)
    }

    fn exec(&mut self, sql: &str, values: &Values) -> Result<u64> {
        let mut st = self.0.prepare_cached(sql).map_err(err)?;
        Ok(st
            .execute(rusqlite::params_from_iter(bind(values)))
            .map_err(err)? as u64)
    }

    fn query(
        &mut self,
        sql: &str,
        values: &Values,
        each: &mut dyn FnMut(&Row<'_>) -> Result<()>,
    ) -> Result<()> {
        let mut st = self.0.prepare_cached(sql).map_err(err)?;
        let mut rows = st
            .query(rusqlite::params_from_iter(bind(values)))
            .map_err(err)?;
        while let Some(r) = rows.next().map_err(err)? {
            each(&Row(RowImpl::Sqlite(r)))?;
        }
        Ok(())
    }
}

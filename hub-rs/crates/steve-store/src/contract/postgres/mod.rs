//! A PostgreSQL connection used by the storage contract tests.
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use postgres::{Client, NoTls};
use sea_query::{Expr, ExprTrait, Func, Value, Values};

use super::backend::{Backend, Col, Dialect, Row, RowImpl};
use crate::{Result, StoreError};

pub(super) struct Pg(Client);

impl Pg {
    pub(super) fn connect(url: &str) -> Result<Self> {
        Client::connect(url, NoTls).map(Self).map_err(error)
    }
}

fn error(e: postgres::Error) -> StoreError {
    if let Some(db) = e.as_db_error() {
        let code = db.code().code();
        if code == "55P03" || code.starts_with("40") {
            return StoreError::Busy(db.to_string());
        }
        if !(code.starts_with("08")
            || code.starts_with("53")
            || code.starts_with("57")
            || code.starts_with("58"))
        {
            return StoreError::Rejected(db.to_string());
        }
    } else if !e.is_closed()
        && e.source().is_some_and(|source| {
            source
                .downcast_ref::<postgres::types::WrongType>()
                .is_some()
        })
    {
        return StoreError::Rejected(e.to_string());
    }
    StoreError::Storage(e.to_string())
}

use std::error::Error as _;

#[derive(Debug)]
enum Param<'a> {
    Integer(Option<i64>),
    Float(Option<f64>),
    Flag(Option<bool>),
    Text(Option<std::borrow::Cow<'a, str>>),
    Blob(Option<&'a [u8]>),
}

impl<'a> TryFrom<&'a Value> for Param<'a> {
    type Error = StoreError;

    fn try_from(value: &'a Value) -> Result<Self> {
        Ok(match value {
            Value::BigInt(value) => Self::Integer(*value),
            Value::Double(value) => Self::Float(*value),
            Value::Bool(value) => Self::Flag(*value),
            Value::String(value) => Self::Text(value.as_deref().map(Into::into)),
            Value::Bytes(value) => Self::Blob(value.as_deref()),
            Value::Json(value) => Self::Text(value.as_ref().map(|json| json.to_string().into())),
            other => {
                return Err(StoreError::Rejected(format!(
                    "PostgreSQL has no binding for {other:?}"
                )));
            }
        })
    }
}

impl ToSql for Param<'_> {
    fn accepts(_: &Type) -> bool {
        true
    }

    fn to_sql(
        &self,
        ty: &Type,
        out: &mut bytes::BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        match self {
            Self::Integer(value) => value.to_sql_checked(ty, out),
            Self::Float(value) => value.to_sql_checked(ty, out),
            Self::Text(value) => value.as_deref().to_sql_checked(ty, out),
            Self::Blob(value) => value.to_sql_checked(ty, out),
            Self::Flag(value) if *ty == Type::BOOL => value.to_sql_checked(ty, out),
            Self::Flag(value) => value.map(i64::from).to_sql_checked(ty, out),
        }
    }
    to_sql_checked!();
}

pub(super) fn col(r: &postgres::Row, i: usize) -> Result<Col<'_>> {
    macro_rules! value {
        ($rust:ty, $map:expr) => {
            r.try_get::<_, Option<$rust>>(i)
                .map(|value| value.map_or(Col::Null, $map))
                .map_err(|e| StoreError::Rejected(e.to_string()))
        };
    }
    let column = r
        .columns()
        .get(i)
        .ok_or_else(|| StoreError::Rejected(format!("missing PostgreSQL column {i}")))?;
    match *column.type_() {
        Type::INT8 => value!(i64, Col::Int),
        Type::BOOL => value!(bool, |value| Col::Int(i64::from(value))),
        Type::FLOAT8 => value!(f64, Col::Real),
        Type::TEXT => value!(&str, Col::Text),
        Type::BYTEA => value!(&[u8], Col::Blob),
        ref ty => Err(StoreError::Rejected(format!(
            "PostgreSQL column has type {ty}"
        ))),
    }
}

impl Backend for Pg {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }
    fn lock_instance(&mut self) -> Result<()> {
        // Contract tests each use a schema of their own.
        Ok(())
    }
    fn begin_write(&mut self) -> Result<()> {
        self.batch("BEGIN")
    }
    fn begin_read(&mut self) -> Result<()> {
        self.batch("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
    }
    fn batch(&mut self, sql: &str) -> Result<()> {
        self.0.batch_execute(sql).map_err(error)
    }
    fn exec(&mut self, sql: &str, values: &Values) -> Result<u64> {
        let params: Vec<_> = values
            .0
            .iter()
            .map(Param::try_from)
            .collect::<Result<_>>()?;
        let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
        self.0.execute(sql, &params).map_err(error)
    }
    fn query(
        &mut self,
        sql: &str,
        values: &Values,
        each: &mut dyn FnMut(&Row<'_>) -> Result<()>,
    ) -> Result<()> {
        let params: Vec<_> = values
            .0
            .iter()
            .map(Param::try_from)
            .collect::<Result<_>>()?;
        let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
        for row in self.0.query(sql, &params).map_err(error)? {
            each(&Row(RowImpl::Postgres(&row)))?;
        }
        Ok(())
    }
}

fn path_call(function: &'static str, json: Expr, path: &str) -> Expr {
    let mut args = vec![Expr::cust_with_expr("CAST($1 AS JSONB)", json)];
    // Contract JSON paths here are literals of the form $.parent.child.
    args.extend(
        path.trim_start_matches('$')
            .split('.')
            .filter(|key| !key.is_empty())
            .map(|key| Expr::Constant(key.into())),
    );
    Func::cust(function).args(args).into()
}

pub(super) fn json_value(json: Expr, path: &str) -> Expr {
    path_call("jsonb_extract_path", json, path)
}

pub(super) fn json_path(json: Expr, path: String) -> Expr {
    path_call("jsonb_extract_path_text", json, &path)
}

pub(super) fn json_type(json: Expr, path: &str) -> Expr {
    let kind: Expr = Func::cust("jsonb_typeof")
        .arg(json_value(json, path))
        .into();
    Expr::case(
        kind.clone().eq(Expr::Constant("string".into())),
        Expr::Constant("text".into()),
    )
    .finally(kind)
    .into()
}

mod tests;

//! Reads of the open and configure commands that establish a session's effective policy.

use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use steve_node_api::{Command, ConfigureSession, OpenSession, PermissionMode};

use crate::{Read, Result, StoreError};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Open,
    Configure,
    Release,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Self::Open => "open_session",
            Self::Configure => "configure_session",
            Self::Release => "release_session",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Open | Self::Configure => "$.body.session",
            Self::Release => "$.body",
        }
    }
}

pub struct Observation {
    pub operation: String,
    pub method: Method,
    pub state: String,
    request: String,
    pub refusal: Option<String>,
}

impl Observation {
    pub fn open(&self) -> Result<OpenSession> {
        Ok(serde_json::from_str::<Command<OpenSession>>(&self.request)?.body)
    }

    pub fn configure(&self) -> Result<ConfigureSession> {
        Ok(serde_json::from_str::<Command<ConfigureSession>>(&self.request)?.body)
    }

    pub fn permissions(&self) -> Result<PermissionMode> {
        match self.method {
            Method::Open => self.open().map(|o| o.policy.permissions),
            Method::Configure => self.configure().map(|o| o.policy.permissions),
            Method::Release => Err(StoreError::Rejected("release has no session policy".into())),
        }
    }
}

fn by_session(c: &mut impl Read, session: &str, method: Option<Method>) -> SelectStatement {
    let path = method.map_or("$.body.session", Method::path);
    let mut q = Query::select();
    q.from("node_commands").and_where(
        c.db()
            .dialect()
            .json_path(Expr::col("request"), path)
            .eq(session),
    );
    if let Some(method) = method {
        q.and_where(Expr::col("method").eq(method.name()));
    } else {
        q.and_where(Expr::col("method").is_in([Method::Open.name(), Method::Configure.name()]));
    }
    q
}

pub fn latest(
    c: &mut impl Read,
    session: &str,
    method: Option<Method>,
    successful: bool,
) -> Result<Option<Observation>> {
    let mut q = by_session(c, session, method);
    let dialect = c.db().dialect();
    q.columns(["operation_id", "method", "state", "request"])
        .expr(Func::coalesce([
            dialect.json_path(Expr::col("receipt"), "$.payload.safe_detail"),
            dialect.json_path(Expr::col("receipt"), "$.payload.state.error.safe_detail"),
        ]))
        .order_by("created", Order::Desc)
        .order_by_expr(dialect.insertion_order(None), Order::Desc)
        .limit(1);
    if successful {
        q.and_where(Expr::col("state").eq("finished"))
            .and_where(dialect.json_field(Expr::col("receipt"), "success").eq(true));
    }
    c.one(&q, |r| {
        let method: String = r.get(1)?;
        let method = match method.as_str() {
            "open_session" => Method::Open,
            "configure_session" => Method::Configure,
            "release_session" => Method::Release,
            _ => return Err(StoreError::Rejected("unexpected session command".into())),
        };
        Ok(Observation {
            operation: r.get(0)?,
            method,
            state: r.get(2)?,
            request: r.get(3)?,
            refusal: r.get(4)?,
        })
    })
}

pub fn count(c: &mut impl Read, session: &str, method: Method) -> Result<i64> {
    let mut q = by_session(c, session, Some(method));
    Ok(
        c.one(q.expr(Func::count(Expr::col("operation_id"))), |r| r.get(0))?
            .unwrap_or(0),
    )
}

pub fn body<T: serde::de::DeserializeOwned>(c: &mut impl Read, operation: &str) -> Result<T> {
    c.one(
        Query::select()
            .column("request")
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(operation)),
        |r| Ok(r.get::<crate::contract::Json<Command<T>>>(0)?.0.body),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing command {operation}")))
}

pub fn configure_session(c: &mut impl Read, operation: &str) -> Result<String> {
    let session = c
        .db()
        .dialect()
        .json_path(Expr::col("request"), "$.body.session");
    c.one(
        Query::select()
            .expr(session)
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(operation)),
        |r| r.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing command {operation}")))
}

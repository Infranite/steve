//! Durable command identity and reconciliation reads for the Hub-side Node ledger.

use sea_query::{Expr, ExprTrait, Query};
use serde::Deserialize;
use serde_json::Value;
use steve_node_api::OperationId;

use crate::{Read, Result};

use super::Json;

pub mod config;

/// The stored receipt wraps the Node payload with the Hub's explicit success observation.
#[derive(Debug, Deserialize)]
pub struct StoredReceipt {
    #[serde(default)]
    pub payload: Option<Value>,
}

/// The immutable request and the latest observation of this operation.
#[derive(Debug)]
pub struct CommandRecord {
    pub operation: String,
    pub node: String,
    pub method: String,
    pub request: Value,
    pub subject: Option<String>,
    pub digest: String,
    pub state: String,
    pub receipt: Option<StoredReceipt>,
    pub revision: i64,
}

pub fn record(
    c: &mut impl Read,
    operation: &str,
    on_node: Option<&str>,
) -> Result<Option<CommandRecord>> {
    let mut q = Query::select();
    q.columns([
        "node_id", "method", "request", "digest", "state", "receipt", "revision", "subject",
    ])
    .from("node_commands")
    .and_where(Expr::col("operation_id").eq(operation));
    if let Some(node) = on_node {
        q.and_where(Expr::col("node_id").eq(node));
    }
    c.one(&q, |r| {
        Ok(CommandRecord {
            operation: operation.into(),
            node: r.get(0)?,
            method: r.get(1)?,
            request: r.get::<Json<Value>>(2)?.0,
            digest: r.get(3)?,
            state: r.get(4)?,
            receipt: r.get::<Option<Json<StoredReceipt>>>(5)?.map(|v| v.0),
            revision: r.get(6)?,
            subject: r.get(7)?,
        })
    })
}

pub fn state(c: &mut impl Read, node: &str, operation: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("state")
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(operation))
            .and_where(Expr::col("node_id").eq(node)),
        |r| r.get(0),
    )
}

pub fn operation_is_foreign(
    c: &mut impl Read,
    node: &str,
    operation: &OperationId,
) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("operation_id")
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(operation.as_str()))
            .and_where(Expr::col("node_id").ne(node)),
        |_| Ok(()),
    )?
    .is_some())
}

pub mod dispatch;

pub mod recovery;

pub mod reconcile;

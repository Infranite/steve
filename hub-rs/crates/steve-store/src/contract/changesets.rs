//! Stored configuration changesets.

use sea_query::{Expr, ExprTrait, Query};
use serde::Deserialize;

use crate::contract::Json;
use crate::{Read, Result};

#[derive(Deserialize)]
struct Operation {
    op: String,
}

pub fn operation_names(r: &mut impl Read, id: &str) -> Result<Option<Vec<String>>> {
    let ops = r.one(
        Query::select()
            .column("ops")
            .from("changesets")
            .and_where(Expr::col("id").eq(id)),
        |row| Ok(row.get::<Json<Vec<Operation>>>(0)?.0),
    )?;
    Ok(ops.map(|ops| ops.into_iter().map(|op| op.op).collect()))
}

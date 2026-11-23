use sea_query::{Expr, ExprTrait, Query};

use crate::{Read, Result};

pub(crate) fn personal_trigger_sources(
    r: &mut impl Read,
    ws: &str,
    sources: &[&str],
) -> Result<std::collections::HashSet<String>> {
    if sources.is_empty() {
        return Ok(Default::default());
    }
    let dialect = r.db().dialect();
    let source = dialect.json_field(Expr::col("meta"), "chan.message");
    let ids = dialect.text_array(sources)?;
    let rows = r.all(
        Query::select()
            .distinct()
            .expr(source.clone())
            .from("messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(source.in_subquery(ids))
            .and_where(
                dialect
                    .json_field(Expr::col("meta"), "chan.command")
                    .eq(1_i64),
            )
            .and_where(
                dialect
                    .json_field(Expr::col("meta"), "chan.mode")
                    .eq("self"),
            ),
        |row| row.get(0),
    )?;
    Ok(rows.into_iter().collect())
}

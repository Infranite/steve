use sea_query::{Asterisk, Expr, ExprTrait, Func, Query};

use crate::{Read, Result};

/// How many items and other entities carry this tag.
pub fn use_count(c: &mut impl Read, tag: &str) -> Result<i64> {
    let mut count = 0;
    for table in ["item_tags", "entity_tags"] {
        count += c
            .one(
                Query::select()
                    .expr(Func::count(Expr::col(Asterisk)))
                    .from(table)
                    .and_where(Expr::col("tag_id").eq(tag)),
                |r| r.get::<i64>(0),
            )?
            .unwrap_or_default();
    }
    Ok(count)
}

use sea_query::{Expr, ExprTrait, JoinType, Query};

use crate::{Read, Result};

/// The item's template and sensitivity label, with no access to the secret itself.
pub fn use_policy(c: &mut impl Read, id: &str) -> Result<Option<(String, String)>> {
    c.one(
        Query::select()
            .columns([("i", "template"), ("i", "sensitivity")])
            .from_as("secret_uses", "u")
            .join_as(
                JoinType::InnerJoin,
                "vault_items",
                "i",
                Expr::col(("i", "id")).eq(Expr::col(("u", "item_id"))),
            )
            .and_where(Expr::col(("u", "id")).eq(id)),
        |r| {
            Ok((
                r.get::<Option<String>>(0)?.unwrap_or_default(),
                r.get::<Option<String>>(1)?.unwrap_or_default(),
            ))
        },
    )
}

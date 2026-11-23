use sea_query::{Expr, ExprTrait, Query};

use crate::{Result, Store};

pub fn set_user_role(store: &Store, user: &str, role: &str) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::update()
                .table("users")
                .value("role", role)
                .and_where(Expr::col("id").eq(user)),
        )?;
        Ok(())
    })
}

pub fn refuse_metadata_updates(store: &Store) -> Result<()> {
    store.write(|w| {
        use crate::contract::Q;
        w.db().batch(
            "CREATE TRIGGER refuse_metadata BEFORE UPDATE ON settings
             WHEN NEW.key='hub.metadata.v1' BEGIN SELECT RAISE(ABORT,'test failure'); END;",
        )
    })
}

pub mod realtime;

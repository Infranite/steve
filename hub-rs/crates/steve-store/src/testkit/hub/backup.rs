use sea_query::{Expr, ExprTrait, Func, Query};

use crate::contract::Q;
use crate::{R, Result, Store};

pub fn seed(store: &Store) -> Result<()> {
    store.write(|w| {
        w.db()
            .batch("CREATE TABLE backup_fixture(value); INSERT INTO backup_fixture VALUES ('kept')")
    })
}

pub fn fill(store: &Store) -> Result<()> {
    store.write(|w| {
        w.db().batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<4000)
         INSERT INTO backup_fixture SELECT zeroblob(4000) FROM n",
        )
    })
}

pub fn append(store: &Store) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::insert()
                .into_table("backup_fixture")
                .columns(["value"])
                .values_panic(["more".into()]),
        )?;
        Ok(())
    })
}

pub fn kept(r: &mut R<'_>) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col("value")))
            .from("backup_fixture")
            .and_where(Expr::col("value").eq("kept")),
        |r| r.get(0),
    )?
    .unwrap_or(0))
}

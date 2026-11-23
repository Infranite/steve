use super::{execute, scalar};
use crate::{Result, Store, W};
use sea_query::{Expr, ExprTrait, Query};

pub fn seed_contact(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('ct_s', 'default', 'contact', 'Sarah', 'usr_dev', 1, 1)",
    )
}

pub fn state(store: &Store) -> Result<(String, i64, i64, i64)> {
    store.sqlite_read(|c| {
        let (name, revision) = c.query_row(
            "SELECT name, revision FROM parties WHERE id = 'ct_s'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let orgs = c.query_row(
            "SELECT count(*) FROM parties WHERE type = 'org'",
            [],
            |row| row.get(0),
        )?;
        let jobs = c.query_row(
            "SELECT count(*) FROM links WHERE predicate = 'works_at'",
            [],
            |row| row.get(0),
        )?;
        Ok((name, revision, orgs, jobs))
    })
}

pub fn audit_count(store: &Store, id: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT count(*) FROM audit WHERE action IN ('config.apply', 'config.undo') AND target = ?",
        [id],
    )
}

pub fn edit_contact(store: &Store) -> Result<()> {
    execute(
        store,
        "UPDATE parties SET note = 'edited', revision = revision + 1 WHERE id = 'ct_s'",
    )
}

pub fn delete_party(w: &mut W<'_>, id: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("parties")
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

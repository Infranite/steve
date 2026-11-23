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

/// A bare database standing in for the previous release in a supervisor fixture.
pub fn seed_evidence(path: &std::path::Path, evidence: &str) -> Result<()> {
    let c = rusqlite::Connection::open(path)?;
    c.execute_batch("CREATE TABLE upgrade_evidence(value TEXT);")?;
    c.execute("INSERT INTO upgrade_evidence VALUES (?)", [evidence])?;
    Ok(())
}

pub fn evidence(path: &std::path::Path) -> Result<String> {
    let c =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(c.query_row("SELECT value FROM upgrade_evidence", [], |r| r.get(0))?)
}

pub fn seed_admin_header(path: &std::path::Path) -> Result<()> {
    let c = rusqlite::Connection::open(path)?;
    crate::db::migrate(&c)?;
    let version = crate::db::SCHEMA_VERSION;
    c.execute_batch(&format!(
        "DELETE FROM schema_version; INSERT INTO schema_version(rowid,v) VALUES(17,{version});
         DELETE FROM schema_compat; INSERT INTO schema_compat(rowid,min_reader) VALUES(29,{});",
        version - 1,
    ))?;
    Ok(())
}

pub fn seed_old_admin_header(path: &std::path::Path) -> Result<()> {
    let c = rusqlite::Connection::open(path)?;
    c.execute_batch(
        "CREATE TABLE schema_version(v); INSERT INTO schema_version(rowid,v) VALUES(17,50);
         CREATE TABLE schema_compat(min_reader); INSERT INTO schema_compat(rowid,min_reader) VALUES(29,49);",
    )?;
    Ok(())
}

pub fn seed_old_backup(path: &std::path::Path) -> Result<()> {
    let c = rusqlite::Connection::open(path)?;
    c.execute_batch(
        "CREATE TABLE schema_version(v INT); INSERT INTO schema_version VALUES(50);
         CREATE TABLE old_record(v); INSERT INTO old_record VALUES('old');",
    )?;
    Ok(())
}

pub fn admin_header(path: &std::path::Path) -> Result<Vec<(i64, i64)>> {
    let db = super::inspect_database(path)?;
    let mut s = db.prepare(
        "SELECT rowid,v FROM schema_version UNION ALL
         SELECT rowid,min_reader FROM schema_compat ORDER BY 1",
    )?;
    Ok(s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn token_audit_actions(store: &Store) -> Result<Vec<String>> {
    store.reader(|c| {
        let mut s =
            c.prepare("SELECT action FROM audit WHERE action LIKE 'token.%' ORDER BY action")?;
        Ok(s.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

// The last writable connection removes WAL sidecars before retention reads the backup.
pub fn quick_check(path: &std::path::Path) -> Result<String> {
    Ok(super::api::writable(path)?.query_row("PRAGMA quick_check", [], |r| r.get(0))?)
}

pub fn seed_channel_cleanup(store: &Store) -> Result<()> {
    let seed_sql = "INSERT INTO parties(id,workspace_id,type,name,source,created_by,created,updated)
             VALUES('backup-owner','default','contact','Backup owner','manual','usr_dev',1,1);
             INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated)
             VALUES('backup-account','default','test','test','backup-owner','personal','backup','Backup','connect',1,1);
             INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,status,created,updated,account_id)
             VALUES('backup-space','default','test','group','backup-space','Backup','left',1,1,'backup-account');
             INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,text,created)
             VALUES('backup-message','default','backup-space','in','received','before cleanup',1);";
    store.transaction(|tx| -> Result<(), crate::StoreError> {
        tx.execute_batch(seed_sql)?;
        steve_store_sqlite::digest::sync_message(
            tx,
            steve_store_sqlite::digest::Source::ChanMessage,
            "backup-message",
            None,
        )?;
        Ok(())
    })
}

fn message(db: &rusqlite::Connection) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT text FROM chan_messages WHERE id='backup-message'",
        [],
        |r| r.get(0),
    )?)
}

pub fn snapshot_message(path: &std::path::Path) -> Result<Option<String>> {
    message(&super::api::writable(path)?)
}

pub fn channel_after_cleanup(store: &Store) -> Result<(Option<String>, bool)> {
    store.reader(|db| {
        Ok((
            message(db)?,
            steve_store_sqlite::digest::verify_pending(db, "default")?.is_empty(),
        ))
    })
}

pub fn checkpoint(store: &Store) -> Result<()> {
    store.with_conn(|db| {
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    })
}

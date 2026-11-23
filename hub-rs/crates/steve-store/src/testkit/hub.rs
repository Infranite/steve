use sea_query::{Expr, ExprTrait, Query};

use crate::{Result, Store};

pub mod api;
pub mod authority;
pub mod backup;
pub mod deliveries;
pub mod digest;
pub mod hub_fixtures;
pub mod mutations;
pub mod node_checks;
pub mod node_files;
pub mod node_fixtures;
pub mod node_journal;
pub mod node_planning;
pub mod node_process;
pub mod node_statistics;
pub mod node_usage;
pub mod outbox_checks;
pub mod realtime;
pub mod state;
pub mod wiring;
pub mod workflow_cases;
pub mod workspace_cases;
pub mod yolo;

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

/// The schema-91 snapshot that stored workflow invalidations only as timeline lines.
pub fn snapshot_before_invalidations(path: &std::path::Path, at: i64) -> Result<()> {
    use crate::db;
    let c = rusqlite::Connection::open(path)?;
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(91);",
    )?;
    for migration in &db::MIGRATIONS[..91] {
        c.execute_batch(migration)?;
    }
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES ('w','w',1);
         INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
         VALUES ('i1','w',1,'t','run','执行',1,1);",
    )?;
    c.execute(
        "INSERT INTO timeline(item_id,ts,actor,kind,text)
         VALUES ('i1',?,'steve','sys','收尾作废：计划里又有没做完的步骤')",
        [at],
    )?;
    Ok(())
}

/// Number of row changes observed by the single writer, including changes from triggers.
pub fn writer_changes(store: &Store) -> Result<i64> {
    store.write(|w| {
        use crate::contract::Q;
        Ok(w.one(
            Query::select().expr(sea_query::Func::cust("total_changes")),
            |r| r.get(0),
        )?
        .unwrap_or_default())
    })
}

pub fn node_audit_count(store: &Store, node: &str, actor: &str, action: &str) -> Result<i64> {
    store.read(|r| {
        use crate::contract::Q;
        Ok(r.one(
            Query::select()
                .expr(sea_query::Func::count(Expr::col("what")))
                .from("node_audit")
                .and_where(Expr::col("node_id").eq(node))
                .and_where(Expr::col("actor").eq(actor))
                .and_where(Expr::col("what").eq(action)),
            |r| r.get(0),
        )?
        .unwrap_or_default())
    })
}

pub fn node_env_fixture(store: &Store) -> Result<()> {
    use crate::contract::Q;
    store.write(|w| {
        w.db().batch(
            "INSERT INTO workspaces(id,name,created) VALUES ('team','Team',1),('solo1','One',1),('solo2','Two',1);
             INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES
              ('u1','team','u1','One','owner',1),('u2','team','u2','Two','member',2);
             INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES
              ('team','u1','owner',1),('team','u2','member',2),('solo1','u1','owner',1),('solo2','u2','owner',1);
             INSERT INTO items(id,workspace_id,no,title,status,stage,requested_by,created,updated) VALUES
              ('i1','team',1,'One','run','执行','u2',1,1),('i2','team',2,'Two','run','执行',NULL,1,1),
              ('i3','solo1',1,'Three','run','执行','u1',1,1),('i4','solo2',1,'Four','run','执行','u2',1,1),
              ('i5','team',3,'Five','run','执行','u1',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES
              ('s1','team','i1','custom','.','running',1,1),('s2','team','i2','custom','.','running',1,1),
              ('s3','solo1','i3','custom','.','running',1,1),('s4','solo2','i4','custom','.','running',1,1),
              ('s5','team','i5','custom','.','running',1,1);
             INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES
              ('n_u1','u1','One',zeroblob(32),'f1','active',1),('n_u2','u2','Two',zeroblob(32),'f2','active',1),
              ('n_old','u2','Old',zeroblob(32),'f3','revoked',1);",
        )
    })
}

pub fn remove_other_node_env_user(store: &Store) -> Result<()> {
    store.write(|w| {
        w.exec(
            Query::delete()
                .from_table("workspace_members")
                .and_where(Expr::col("user_id").eq("u2")),
        )?;
        w.exec(
            Query::delete()
                .from_table("users")
                .and_where(Expr::col("id").eq("u2")),
        )?;
        Ok(())
    })
}

fn inspect_database(path: &std::path::Path) -> Result<rusqlite::Connection> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(db)
}

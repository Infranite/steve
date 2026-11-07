//! 050/051/052 follow the published v0.2.66 schema (049): old data stays intact, collaboration and
//! prerequisite waits are added once, and the completion gate applies to old items too.

mod common;

use common::count;
use rusqlite::Connection;
use steve_store::{NoPublisher, Store, db};

#[test]
fn published_v49_upgrades_and_reopens_with_existing_data_and_new_gates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL);
         INSERT INTO schema_version VALUES(49);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);
         INSERT INTO schema_compat VALUES(49);",
    )
    .unwrap();
    for migration in &db::MIGRATIONS[..49] {
        c.execute_batch(migration).unwrap();
    }
    // The released runner adds these conditional columns alongside its numbered migrations.
    c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
    c.execute_batch(
        "ALTER TABLE bots ADD COLUMN reasoning_effort TEXT;
         ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;
         INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
         INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
           VALUES('parent','default',1,'Existing work','run','Work',1,1),
                 ('prerequisite','default',2,'Existing prerequisite','run','Work',1,1);
         INSERT INTO item_deps(item_id,dep_id,created) VALUES('parent','prerequisite',1);
         INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated)
           VALUES('session','default','parent','fixture','/tmp','idle',1,1);
         INSERT INTO agent_turns(id,workspace_id,session_id,harness,started,model)
           VALUES('turn','default','session','fixture',1,'existing-model');",
    )
    .unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, 49);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('workflows','item_dependency_waits')"
        ),
        0
    );
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .with_conn(|c| {
            let compat = db::compat(c)?;
            assert_eq!(
                (compat.schema, compat.min_reader),
                (db::SCHEMA_VERSION, db::SCHEMA_VERSION)
            );
            assert_eq!(
                c.query_row("SELECT title FROM items WHERE id='parent'", [], |r| {
                    r.get::<_, String>(0)
                })?,
                "Existing work"
            );
            assert_eq!(
                c.query_row("SELECT model FROM agent_turns WHERE id='turn'", [], |r| {
                    r.get::<_, String>(0)
                })?,
                "existing-model"
            );
            assert_eq!(count(c, "SELECT count(*) FROM item_deps"), 1);
            assert_eq!(
                count(c, "SELECT count(*) FROM pragma_foreign_key_check"),
                0
            );
            // Items created by the old build receive the completion gate too.
            assert!(
                c.execute(
                    "UPDATE items SET status='done',phase='closed',outcome='completed' WHERE id='parent'",
                    [],
                )
                .is_err()
            );
            c.execute_batch(
                "INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
                   VALUES('parent',1,2,3,2,2);
                 INSERT INTO workflow_units(workflow_id,id,position,spec,status)
                   VALUES('parent','unit',0,'{}','pending');
                 INSERT INTO item_dependency_waits(item_id,cycle,session_id,resume,reason,state,created,updated)
                   VALUES('parent',1,'session','automatic','Pending prerequisite','waiting',2,2);",
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);

    // The migrations contain one-shot CREATEs. Opening twice must preserve their rows and
    // resume state, rather than replaying them or resetting the published schema.
    let reopened = Store::open(&path, NoPublisher).unwrap();
    reopened
        .with_conn(|c| {
            assert_eq!(count(c, "SELECT count(*) FROM workflows"), 1);
            assert_eq!(count(c, "SELECT count(*) FROM workflow_units"), 1);
            assert_eq!(
                count(c, "SELECT count(*) FROM item_dependency_waits WHERE state='waiting'"),
                1
            );
            c.execute(
                "UPDATE items SET status='done',phase='closed',outcome='completed' WHERE id='prerequisite'",
                [],
            )?;
            assert!(
                c.execute(
                    "UPDATE items SET status='done',phase='closed',outcome='completed' WHERE id='parent'",
                    [],
                )
                .is_err(),
                "the outstanding wait still gates completion after the prerequisite finishes"
            );
            c.execute(
                "UPDATE item_dependency_waits SET state='started' WHERE item_id='parent'",
                [],
            )?;
            assert_eq!(
                c.execute(
                    "UPDATE items SET status='done',phase='closed',outcome='completed' WHERE id='parent'",
                    [],
                )?,
                1
            );
            assert_eq!(
                c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                "ok"
            );
            assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();
}

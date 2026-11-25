//! 138: clears cached harness update state persisted before typed update reasons (#558),
//! while leaving unrelated workspace settings intact.

use rusqlite::Connection;
use steve_store::testkit::SqliteFixtures;
use steve_store::{NoPublisher, Store, db};

mod common;
use common::count;

#[test]
fn clears_cached_harness_update_settings_and_preserves_other_settings() {
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("harness.updates.%"))
        .expect("harness update cache cleanup migration registered") as i64;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);",
    )
    .unwrap();
    for (i, migration) in db::MIGRATIONS[..before as usize].iter().enumerate() {
        if i == 119 {
            for table in ["bots", "sessions"] {
                let present: bool = c
                    .query_row(
                        &format!(
                            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = 'reasoning_effort')"
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                if !present {
                    c.execute_batch(&format!(
                        "ALTER TABLE {table} ADD COLUMN reasoning_effort TEXT"
                    ))
                    .unwrap();
                }
            }
        }
        c.execute_batch(migration).unwrap();
        if i + 1 == 122 {
            c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
        }
    }
    c.execute_batch(&format!(
        "INSERT INTO schema_version VALUES({before});
         INSERT INTO schema_compat VALUES({before});
         INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
         INSERT INTO settings(workspace_id,key,value,updated)
           VALUES('default','harness.updates.local','{{\"report\":{{\"harnesses\":[{{\"blocked_reason\":\"legacy string\"}}]}},\"at\":1,\"runs\":{{}}}}',1),
                 ('default','harness.updates.n_remote','{{\"report\":null,\"at\":2,\"runs\":{{}}}}',2),
                 ('default','node.hub_id','hub_keep_me',3);"
    ))
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .fixture_write(|c| {
            let compat = db::compat(c)?;
            assert_eq!(
                (compat.schema, compat.min_reader),
                (db::SCHEMA_VERSION, db::SCHEMA_VERSION)
            );
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM settings WHERE key LIKE 'harness.updates.%'"
                ),
                0
            );
            let kept: String = c.query_row(
                "SELECT value FROM settings WHERE workspace_id='default' AND key='node.hub_id'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(kept, "hub_keep_me");
            assert_eq!(
                c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                "ok"
            );
            assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();
}

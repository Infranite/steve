//! 125: removes the retired dynamic work-graph tables, clears steve_workflow tool pins,
//! and strips stale workflow_revision keys from items.closing while keeping historical
//! closed child items and their artifacts intact.

use rusqlite::Connection;
use steve_store::testkit::SqliteFixtures;
use steve_store::{NoPublisher, Store, db};

mod common;
use common::count;

#[test]
fn drops_old_workflow_tables_and_pins_while_preserving_closed_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL);
         INSERT INTO schema_version VALUES(124);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);
         INSERT INTO schema_compat VALUES(124);",
    )
    .unwrap();
    for migration in &db::MIGRATIONS[..122] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
    for migration in &db::MIGRATIONS[122..124] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
         INSERT INTO bots(id,workspace_id,name,harness,updated)
           VALUES('dev','default','Dev','fixture',1);
         INSERT INTO items(id,workspace_id,no,title,phase,outcome,source,closing,created,updated)
           VALUES('ST-1','default',1,'Parent','closing',NULL,'[\"对话\",\"chat\"]',
                  '{\"kind\":\"complete\",\"reason\":\"done\",\"by\":\"dev\",\"at\":1,\"frozen_revision\":1,\"scope_revision\":0,\"workflow_revision\":3,\"accepted\":false}',1,1),
                 ('ST-2','default',2,'Closed child','closed','completed','[\"协作工作\",\"workflow\"]',NULL,1,1);
         INSERT INTO artifacts(id,item_id,kind,title,location,created)
           VALUES('art_1','ST-2','report','Final report','/tmp/report.md',1);
         INSERT INTO workflows(item_id,revision,max_parallel,max_attempts,created,updated)
           VALUES('ST-1',3,2,3,1,1);
         INSERT INTO workflow_units(workflow_id,id,position,spec,status,item_id,attempt)
           VALUES('ST-1','u1',0,'{}','succeeded','ST-2',1);
         INSERT INTO workflow_attempts(item_id,attempt,workflow_id,unit_id,snapshot,created)
           VALUES('ST-2',1,'ST-1','u1','{}',1);
         INSERT INTO workflow_history(workflow_id,revision,action,reason,unit_id,detail,created)
           VALUES('ST-1',1,'plan','init',NULL,'{}',1);
         INSERT INTO workflow_review_findings(workflow_id,fingerprint,scope,findings,applied_revision)
           VALUES('ST-1','fp1','{}','[]',NULL);
         INSERT INTO bot_tool_pins(bot_id,kind,target,created)
           VALUES('dev','tool','steve_workflow',1),
                 ('dev','tool','steve_item_read',2);",
    )
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
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN (
                        'workflows','workflow_units','workflow_history','workflow_attempts','workflow_review_findings'
                    )"
                ),
                0
            );
            let pins: Vec<String> = c
                .prepare("SELECT target FROM bot_tool_pins ORDER BY target")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(pins, ["steve_item_read"]);
            let has_wf_rev: bool = c.query_row(
                "SELECT json_type(closing, '$.workflow_revision') IS NOT NULL FROM items WHERE id='ST-1'",
                [],
                |r| r.get(0),
            )?;
            assert!(!has_wf_rev);
            let close_kind: String = c.query_row(
                "SELECT json_extract(closing, '$.kind') FROM items WHERE id='ST-1'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(close_kind, "complete");
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM items WHERE id='ST-2' AND phase='closed' AND outcome='completed'"
                ),
                1
            );
            assert_eq!(
                count(c, "SELECT count(*) FROM artifacts WHERE id='art_1' AND item_id='ST-2'"),
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

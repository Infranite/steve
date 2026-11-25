//! 140: one-time retirement of non-terminal child items left behind by the retired dynamic
//! work-graph engine (`source[1] = "workflow"`), replacing the per-startup scan while preserving
//! closed history, ordinary items, and script-workflow (`source[1] = "script"`) items.

use rusqlite::Connection;
use steve_store::testkit::SqliteFixtures;
use steve_store::{NoPublisher, Store, db};

mod common;
use common::count;

#[test]
fn retires_open_legacy_workflow_children_and_preserves_closed_and_non_legacy_items() {
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("legacy_workflow_retired"))
        .expect("legacy workflow child retirement migration registered") as i64;
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
        r#"INSERT INTO schema_version VALUES({before});
           INSERT INTO schema_compat VALUES({before});
           INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
           INSERT INTO items(id,workspace_id,no,kind,title,phase,outcome,closed_reason,closed_at,closing,source,cycle,revision,created,updated) VALUES
             ('wf_active','default',1,'task','Legacy active','active',NULL,NULL,NULL,NULL,'["协作工作","workflow"]',1,2,10,100),
             ('wf_paused','default',2,'task','Legacy paused','paused',NULL,NULL,NULL,NULL,'["协作工作","workflow"]',1,1,10,101),
             ('wf_closing','default',3,'task','Legacy closing','closing',NULL,NULL,NULL,'{{"kind":"complete","reason":"old"}}','["协作工作","workflow"]',1,3,10,102),
             ('wf_done','default',4,'task','Legacy done','closed','completed','done_before',90,NULL,'["协作工作","workflow"]',1,4,10,90),
             ('script_child','default',5,'task','Script workflow child','active',NULL,NULL,NULL,NULL,'["workflow","script"]',1,1,10,103),
             ('ordinary','default',6,'task','Ordinary item','active',NULL,NULL,NULL,NULL,'["对话","chat"]',1,1,10,104);
           INSERT INTO timeline(item_id,ts,actor,kind,text) VALUES
             ('wf_done',90,'dev','deliver','kept artifact');
           INSERT INTO sessions(id,workspace_id,item_id,no,bot_id,harness,cwd,status,created,updated) VALUES
             ('s_active','default','wf_active',1,'dev','claude','/tmp','running',10,100),
             ('s_script','default','script_child',1,'dev','claude','/tmp','running',10,103);
           INSERT INTO queued(id,session_id,text,via,created,files) VALUES
             ('q_user','s_active','follow up','me',50,'[]'),
             ('q_steve','s_active','ping','steve',51,'[]'),
             ('q_keep','s_script','still running','me',52,'[]');
           INSERT INTO session_resume(session_id,created) VALUES
             ('s_active',55),
             ('s_script',56);
           INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,item_id,session_id,question,status,requested_by,created) VALUES
             ('d_legacy','default','accept','item','wf_closing','wf_closing',NULL,'Approve?','open','steve',60),
             ('d_script','default','ask','item','script_child','script_child','s_script','Keep?','open','steve',61);
           INSERT INTO item_dependency_waits(item_id,cycle,session_id,resume,reason,state,created,updated,wait_owner,wait_started_at,wait_deadline,wait_on_timeout) VALUES
             ('wf_active',1,'s_active','automatic','dep','waiting',60,60,'system:dependencies',60,300,'escalate_to_owner'),
             ('ordinary',1,NULL,'automatic','dep','waiting',61,61,'system:dependencies',61,301,'escalate_to_owner');
           INSERT INTO item_placement_waits(item_id,workspace_id,blocked,created) VALUES
             ('wf_paused','default','{{"reason":"no_candidates","rejected":[]}}',60),
             ('script_child','default','{{"reason":"no_candidates","rejected":[]}}',61);
           INSERT INTO item_operations(id,item_id,cycle,revision,kind,required,status,label,evidence,created,updated) VALUES
             ('op_legacy','wf_active',1,2,'release',1,'pending','release','{{}}',60,60),
             ('op_ordinary','ordinary',1,1,'release',1,'pending','release','{{}}',61,61);"#
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

            // All three open legacy workflow children are retired to closed/cancelled.
            for (id, expected_ts) in [("wf_active", 100_i64), ("wf_paused", 101), ("wf_closing", 102)] {
                let row: (String, Option<String>, Option<String>, Option<i64>, Option<String>) = c
                    .query_row(
                        "SELECT phase, outcome, closed_reason, closed_at, closing FROM items WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )?;
                assert_eq!(
                    row,
                    (
                        "closed".into(),
                        Some("cancelled".into()),
                        Some("legacy_workflow_retired".into()),
                        Some(expected_ts),
                        None,
                    ),
                    "{id}"
                );
                let cycle_row: (String, Option<String>, String) = c.query_row(
                    "SELECT outcome, reason, source FROM item_cycles WHERE item_id=?1 AND cycle=1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                assert_eq!(
                    cycle_row,
                    (
                        "cancelled".into(),
                        Some("legacy_workflow_retired".into()),
                        "migrated".into(),
                    ),
                    "{id} cycle"
                );
            }

            // Already-closed legacy child keeps its original history untouched.
            let done: (String, Option<String>, Option<String>, Option<i64>) = c.query_row(
                "SELECT phase, outcome, closed_reason, closed_at FROM items WHERE id='wf_done'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            assert_eq!(
                done,
                (
                    "closed".into(),
                    Some("completed".into()),
                    Some("done_before".into()),
                    Some(90),
                )
            );
            assert_eq!(
                count(c, "SELECT count(*) FROM timeline WHERE item_id='wf_done' AND text='kept artifact'"),
                1
            );

            // Script-workflow child (`source[1] = "script"`) and ordinary item remain active.
            for id in ["script_child", "ordinary"] {
                let phase: String = c.query_row("SELECT phase FROM items WHERE id=?1", [id], |r| r.get(0))?;
                assert_eq!(phase, "active", "{id}");
            }

            // Sessions, queued dispatches, resumes, decisions, waits, and operations cleaned for legacy only.
            let s_active: String = c.query_row("SELECT status FROM sessions WHERE id='s_active'", [], |r| r.get(0))?;
            let s_script: String = c.query_row("SELECT status FROM sessions WHERE id='s_script'", [], |r| r.get(0))?;
            assert_eq!(s_active, "ended");
            assert_eq!(s_script, "running");

            assert_eq!(count(c, "SELECT count(*) FROM queued WHERE session_id='s_active'"), 0);
            assert_eq!(count(c, "SELECT count(*) FROM queued WHERE session_id='s_script'"), 1);
            assert_eq!(
                count(c, "SELECT count(*) FROM session_dispatch WHERE id='q_user' AND state='cancelled'"),
                1
            );
            assert_eq!(count(c, "SELECT count(*) FROM session_resume WHERE session_id='s_active'"), 0);
            assert_eq!(count(c, "SELECT count(*) FROM session_resume WHERE session_id='s_script'"), 1);

            let d_legacy: String = c.query_row("SELECT status FROM decisions WHERE id='d_legacy'", [], |r| r.get(0))?;
            let d_script: String = c.query_row("SELECT status FROM decisions WHERE id='d_script'", [], |r| r.get(0))?;
            assert_eq!(d_legacy, "expired");
            assert_eq!(d_script, "open");

            assert_eq!(count(c, "SELECT count(*) FROM item_dependency_waits WHERE item_id='wf_active'"), 0);
            assert_eq!(count(c, "SELECT count(*) FROM item_dependency_waits WHERE item_id='ordinary'"), 1);
            assert_eq!(count(c, "SELECT count(*) FROM item_placement_waits WHERE item_id='wf_paused'"), 0);
            assert_eq!(count(c, "SELECT count(*) FROM item_placement_waits WHERE item_id='script_child'"), 1);

            let op_legacy: (i64, String) = c.query_row(
                "SELECT required, status FROM item_operations WHERE id='op_legacy'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let op_ordinary: (i64, String) = c.query_row(
                "SELECT required, status FROM item_operations WHERE id='op_ordinary'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(op_legacy, (0, "waived".into()));
            assert_eq!(op_ordinary, (1, "pending".into()));

            assert_eq!(
                c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                "ok"
            );
            assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();
}

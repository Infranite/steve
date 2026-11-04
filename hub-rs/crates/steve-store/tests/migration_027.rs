//! 027: the item lifecycle. Existing items become what they already were — done → closed / completed,
//! drop → closed / cancelled (acceptance unknown, a `migrated` cycle row), work with a session → active,
//! work never taken up → ready — without emitting any event, and running the migrations again changes
//! nothing.

mod common;

use common::*;
use rusqlite::Connection;
use steve_store::db;

const SEED: &str = "
INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1);
INSERT INTO items (id, workspace_id, no, title, status, stage, now, created, updated) VALUES
  ('it_done', 'default', 1, 'a', 'done', '收尾', '已验收', 1, 10),
  ('it_drop', 'default', 2, 'b', 'drop', '执行', '你放弃了', 1, 20),
  ('it_run', 'default', 3, 'c', 'run', '执行', '', 1, 30),
  ('it_wait', 'default', 4, 'd', 'wait', '交付与验收', '', 1, 40),
  ('it_propose', 'default', 5, 'e', 'propose', '规划', '', 1, 50),
  ('it_pause', 'default', 6, 'f', 'pause', '执行', '', 1, 60);
INSERT INTO sessions (id, workspace_id, item_id, harness, cwd, status, created, updated) VALUES
  ('s1', 'default', 'it_run', 'claude-code', '/tmp', 'idle', 1, 1),
  ('s2', 'default', 'it_wait', 'claude-code', '/tmp', 'idle', 1, 1),
  ('s3', 'default', 'it_pause', 'claude-code', '/tmp', 'ended', 1, 1),
  ('s4', 'default', 'it_done', 'claude-code', '/tmp', 'ended', 1, 1);
";

fn v26() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let c = Connection::open(dir.path().join("steve.sqlite3")).unwrap();
    c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL)")
        .unwrap();
    for m in &db::MIGRATIONS[..26] {
        c.execute_batch(m).unwrap();
    }
    c.execute("INSERT INTO schema_version (v) VALUES (26)", [])
        .unwrap();
    c.execute_batch(SEED).unwrap();
    (dir, c)
}

type Row = (
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    String,
);

fn items(c: &Connection) -> Vec<Row> {
    let mut st = c
        .prepare(
            "SELECT id, phase, cycle, revision, outcome, acceptance, closed_at, status FROM items ORDER BY no",
        )
        .unwrap();
    st.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn s(x: &str) -> String {
    x.into()
}

#[test]
fn old_items_become_what_they_were_without_any_event() {
    let (_dir, c) = v26();
    let events = count(&c, "SELECT count(*) FROM outbox_events");
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
    assert_eq!(
        items(&c),
        [
            (
                s("it_done"),
                s("closed"),
                1,
                0,
                Some(s("completed")),
                None,
                Some(10),
                s("done")
            ),
            (
                s("it_drop"),
                s("closed"),
                1,
                0,
                Some(s("cancelled")),
                None,
                Some(20),
                s("drop")
            ),
            (s("it_run"), s("active"), 1, 0, None, None, None, s("run")),
            (s("it_wait"), s("active"), 1, 0, None, None, None, s("wait")),
            (
                s("it_propose"),
                s("ready"),
                1,
                0,
                None,
                None,
                None,
                s("propose")
            ),
            (
                s("it_pause"),
                s("active"),
                1,
                0,
                None,
                None,
                None,
                s("pause")
            ),
        ]
    );
    // the closed ones have their cycle on record, marked migrated, acceptance not invented
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM item_cycles WHERE source = 'migrated' AND acceptance IS NULL AND cycle = 1"
        ),
        2
    );
    assert_eq!(
        count(
            &c,
            "SELECT closed FROM item_cycles WHERE item_id = 'it_drop'"
        ),
        20
    );
    // no fact is emitted for history, no operation and no closing intent is invented
    assert_eq!(count(&c, "SELECT count(*) FROM outbox_events"), events);
    assert_eq!(count(&c, "SELECT count(*) FROM item_operations"), 0);
    assert_eq!(
        count(&c, "SELECT count(*) FROM items WHERE closing IS NOT NULL"),
        0
    );

    // again: nothing changes
    let before = items(&c);
    db::migrate(&c).unwrap();
    assert_eq!(items(&c), before);
    assert_eq!(count(&c, "SELECT count(*) FROM item_cycles"), 2);
    assert_eq!(count(&c, "SELECT count(*) FROM outbox_events"), events);
}

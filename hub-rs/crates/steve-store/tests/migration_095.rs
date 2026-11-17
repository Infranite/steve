//! 095: the personal harness configuration default moves from each workspace's `ownerContext`
//! setting to the Hub's `node.owner_context`: off when any workspace had turned it off, else unset
//! (on); the workspace setting goes.

use rusqlite::Connection;
use steve_store::db;

fn at_092(settings: &str) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(94);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..94] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(settings).unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
    c
}

fn hub_default(c: &Connection) -> Option<(String, i64)> {
    c.query_row(
        "SELECT value, updated FROM settings WHERE workspace_id='default' AND key='node.owner_context'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

fn left(c: &Connection) -> i64 {
    c.query_row(
        "SELECT count(*) FROM settings WHERE key='ownerContext'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_workspace_that_turned_it_off_turns_the_hub_default_off() {
    let c = at_092(
        r#"INSERT INTO settings(workspace_id,key,value,updated) VALUES
             ('default','ownerContext','"on"',5), ('w2','ownerContext','"off"',7), ('w3','ownerContext','"off"',9);"#,
    );
    assert_eq!(hub_default(&c), Some(("off".into(), 9)));
    assert!(!steve_store::nodes::owner_context_default(&c).unwrap());
    assert_eq!(left(&c), 0, "the workspace setting goes");
}

#[test]
fn on_everywhere_or_never_set_leaves_the_default_on() {
    for seed in [
        r#"INSERT INTO settings(workspace_id,key,value,updated) VALUES ('default','ownerContext','"on"',5);"#,
        "",
    ] {
        let c = at_092(seed);
        assert_eq!(hub_default(&c), None);
        assert!(steve_store::nodes::owner_context_default(&c).unwrap());
        assert_eq!(left(&c), 0);
    }
}

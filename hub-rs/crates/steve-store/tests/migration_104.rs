//! 104: the shadow facts of an item's work (#327). It takes the number after main's 100 (calendar),
//! 101 (starred contacts), 102 (alarms) and 103 (morning brief): the file, the `db.rs` include and
//! the LOCK line move together. On a database at 103 with turns already run, the upgrade adds the
//! fact tables empty and indexes the existing turns, which the newest-turn read then uses.

use rusqlite::Connection;
use steve_store::db;

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn the_file_is_at_its_position() {
    assert_eq!(db::MIGRATIONS[103], include_str!("../migrations/104.sql"));
    assert!(db::MIGRATIONS[103].contains("CREATE TABLE item_delivery_facts"));
    assert!(db::MIGRATIONS[102].contains("CREATE TABLE briefings"));
    assert!(db::MIGRATIONS[99].contains("CREATE TABLE cal_calendars"));
}

#[test]
fn applies_on_a_103_database_with_turns_run() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(103);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..103] {
        c.execute_batch(m).unwrap();
    }
    // turns of two sessions before 104 (only the turns matter here: their sessions are not set up)
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES
           ('t1','s1','finished',10,11),('t2','s1','finished',20,21),('t3','s2','running',15,15);",
    )
    .unwrap();
    c.execute_batch(db::MIGRATIONS[103]).unwrap();
    for t in [
        "item_delivery_facts",
        "item_waits",
        "item_judgements",
        "work_policy",
    ] {
        let n: i64 = one(&c, &format!("SELECT COUNT(*) FROM {t}"));
        assert_eq!(n, 0, "{t} is empty at migration");
    }
    let n: i64 = one(&c, "SELECT COUNT(*) FROM runtime_turns");
    assert_eq!(n, 3, "the turns are kept");
    let newest: String = one(
        &c,
        "SELECT turn_id FROM runtime_turns WHERE session_id='s1' ORDER BY created DESC,rowid DESC LIMIT 1",
    );
    assert_eq!(newest, "t2");
    let plan: Vec<String> = c
        .prepare(
            "EXPLAIN QUERY PLAN SELECT turn_id FROM runtime_turns WHERE session_id='s1' ORDER BY created DESC,rowid DESC LIMIT 1",
        )
        .unwrap()
        .query_map([], |r| r.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let plan = plan.join(" | ");
    assert!(plan.contains("runtime_turns_session"), "{plan}");
}

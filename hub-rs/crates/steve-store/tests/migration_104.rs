//! Current work-fact tables and the newest-turn index.
#[path = "common/current.rs"]
mod current;

use rusqlite::Connection;

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn facts_start_empty_and_newest_turn_reads_use_the_session_index() {
    let mut c = Connection::open_in_memory().unwrap();
    current::migrate(&mut c).unwrap();
    // Only turns matter here; their sessions are not set up.
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES
           ('t1','s1','finished',10,11),('t2','s1','finished',20,21),('t3','s2','running',15,15);",
    )
    .unwrap();
    for t in [
        "item_delivery_facts",
        "item_waits",
        "item_judgements",
        "work_policy",
    ] {
        let n: i64 = one(&c, &format!("SELECT COUNT(*) FROM {t}"));
        assert_eq!(n, 0, "{t} starts empty");
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

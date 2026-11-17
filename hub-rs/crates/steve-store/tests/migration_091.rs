//! 091: the model access setting for service sessions (`model_access.v1`) goes; every other
//! setting stays.

use rusqlite::Connection;
use steve_store::db;

#[test]
fn removes_the_model_access_setting_and_nothing_else() {
    let c = Connection::open_in_memory().unwrap();
    for m in &db::MIGRATIONS[..90] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO settings(workspace_id,key,value,updated) VALUES
           ('default','model_access.v1','{\"harnesses\":{}}',1),
           ('default','other.v1','{}',1);",
    )
    .unwrap();
    c.execute_batch(db::MIGRATIONS[90]).unwrap();
    let keys: Vec<String> = c
        .prepare("SELECT key FROM settings ORDER BY key")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(keys, ["other.v1"]);
}

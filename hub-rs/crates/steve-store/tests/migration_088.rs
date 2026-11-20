//! Identity ordinals are unique within each space.
use rusqlite::Connection;
use steve_store::db;

#[test]
fn identity_ordinals_are_unique_within_a_space() {
    let c = Connection::open_in_memory().unwrap();
    db::migrate(&c).unwrap();
    c.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    c.pragma_update(None, "foreign_keys", "OFF").unwrap();
    c.execute("INSERT INTO space_identities(space_id,connection_id,workspace_id,joined,ordinal) VALUES ('s1','con_a','w',1,1)", []).unwrap();
    assert!(
        c.execute("INSERT INTO space_identities(space_id,connection_id,workspace_id,joined,ordinal) VALUES ('s1','con_z','w',1,1)", [])
            .is_err()
    );
}

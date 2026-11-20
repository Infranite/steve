//! A Node environment entry stores either a value or a vault reference.
use rusqlite::Connection;
#[path = "common/current.rs"]
mod current;

#[test]
fn node_environment_values_and_vault_references_are_mutually_exclusive() {
    let mut c = Connection::open_in_memory().unwrap();
    current::migrate(&mut c).unwrap();
    c.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    c.pragma_update(None, "foreign_keys", "OFF").unwrap();
    // one user's variable on one Node is a value or a vault reference, never both
    assert!(
        c.execute(
            "INSERT INTO node_env(node_id,principal,key,value,vault_ref,updated_by,updated) VALUES ('n1','u1','K','v','ref','u1',1)",
            [],
        )
        .is_err()
    );
    c.execute(
        "INSERT INTO node_env(node_id,principal,key,value,updated_by,updated) VALUES ('n1','u1','K','v','u1',1)",
        [],
    )
    .unwrap();
}

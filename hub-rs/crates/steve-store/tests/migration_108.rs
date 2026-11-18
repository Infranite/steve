use rusqlite::{Connection, params};
use steve_store::{bot_dispatch, db};

#[test]
fn upgrades_a_107_database_without_granting_new_nodes_legacy_dispatch() {
    assert_eq!(db::MIGRATIONS[107], include_str!("../migrations/108.sql"));
    let c = Connection::open_in_memory().unwrap();
    for migration in &db::MIGRATIONS[..107] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL);
         INSERT INTO schema_version VALUES (107);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);
         INSERT INTO schema_compat VALUES (107);
         INSERT INTO workspaces(id,name,created) VALUES ('default','Fixture',1);",
    )
    .unwrap();
    let insert = |id: &str, state: &str, local: bool| {
        c.execute(
            "INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,local,created)
             VALUES (?1,'owner',?1,zeroblob(32),?1,?2,?3,1)",
            params![id, state, local],
        )
        .unwrap();
    };
    insert("existing", "active", false);
    insert("paused", "paused", false);
    insert("pending", "pending", false);
    insert("revoked", "revoked", false);
    insert("local", "active", true);

    db::migrate(&c).unwrap();
    assert_eq!(
        db::compat(&c).unwrap(),
        db::Compat {
            schema: db::SCHEMA_VERSION,
            min_reader: db::SCHEMA_VERSION,
        }
    );
    insert("new", "active", false);
    c.execute(
        "UPDATE nodes SET state='active' WHERE node_id='pending'",
        [],
    )
    .unwrap();
    // Reopening must not backfill newly paired or newly confirmed Nodes.
    db::migrate(&c).unwrap();
    for (id, was_eligible) in [
        ("existing", true),
        ("paused", true),
        ("pending", false),
        ("revoked", false),
        ("local", false),
        ("new", false),
    ] {
        assert_eq!(
            bot_dispatch::legacy_spread(&c, id).unwrap(),
            was_eligible,
            "{id}"
        );
    }
    assert!(
        bot_dispatch::selected(&c, "default", None)
            .unwrap()
            .is_none()
    );
    assert!(
        bot_dispatch::bindings(&c, "default", "worker")
            .unwrap()
            .is_none()
    );
}

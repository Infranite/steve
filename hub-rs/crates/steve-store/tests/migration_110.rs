use rusqlite::{Connection, params};
use steve_store::{bot_dispatch, db};

#[test]
fn upgrades_a_109_database_without_granting_new_nodes_legacy_dispatch() {
    assert_eq!(db::MIGRATIONS[109], include_str!("../migrations/110.sql"));
    let c = Connection::open_in_memory().unwrap();
    for migration in &db::MIGRATIONS[..109] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL);
         INSERT INTO schema_version VALUES (109);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);
         INSERT INTO schema_compat VALUES (109);
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
    c.execute(
        "INSERT INTO node_removals
         (operation_id,node_id,requested_by,grace_secs,phase,created,updated)
         VALUES ('removal','existing','owner',30,'requested',1,1)",
        [],
    )
    .unwrap();

    c.execute(
        "INSERT INTO deliveries
         (id,url,workspace_id,session_id,title,name,mime,size,created,sha256,manifest,preview,reply)
         VALUES ('reply','/reply','default','session','Reply','file.txt','text/plain',1,1,'hash','[]','{}',1)",
        [],
    )
    .unwrap();

    db::migrate(&c).unwrap();
    let reply: (String, bool) = c
        .query_row(
            "SELECT session_id,reply FROM deliveries WHERE id='reply'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(reply, ("session".into(), true));
    let removal: (String, String) = c
        .query_row(
            "SELECT node_id,phase FROM node_removals WHERE operation_id='removal'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(removal, ("existing".into(), "requested".into()));
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
    let defaults: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM bot_team_defaults WHERE workspace_id='default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(defaults, 0);
    assert!(
        bot_dispatch::bindings(&c, "default", "worker")
            .unwrap()
            .is_none()
    );
}

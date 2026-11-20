#[path = "common/current.rs"]
mod current;
use rusqlite::{Connection, params};
use steve_store::bot_dispatch;

#[test]
fn new_nodes_and_state_changes_do_not_grant_legacy_dispatch() {
    let mut c = Connection::open_in_memory().unwrap();
    current::migrate(&mut c).unwrap();
    c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES ('default','Fixture',1);")
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

    current::migrate(&mut c).unwrap();
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
    insert("new", "active", false);
    c.execute(
        "UPDATE nodes SET state='active' WHERE node_id='pending'",
        [],
    )
    .unwrap();
    // Reopening must not grant newly paired or newly confirmed Nodes legacy dispatch.
    current::migrate(&mut c).unwrap();
    for (id, was_eligible) in [
        ("existing", false),
        ("paused", false),
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

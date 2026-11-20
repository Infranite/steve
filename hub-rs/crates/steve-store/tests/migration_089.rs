//! Current channel settings and connection lifetime constraints.

use rusqlite::Connection;
use steve_store::db;

#[test]
fn write_blocks_default_to_the_connection_and_follow_its_deletion() {
    let c = Connection::open_in_memory().unwrap();
    db::migrate(&c).unwrap();
    c.execute_batch(
        "PRAGMA foreign_keys=ON;
         INSERT INTO workspaces(id,name,created) VALUES ('w','w',1);
         INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u1','w','s1','One','owner',1);
         INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated) VALUES
           ('p_bot','w','bot','Bot',NULL,'u1',1,1);
         INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated) VALUES
           ('a_bot','w','slack','slack','p_bot','app_bot','bot','bot','connect',1,1);
         INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated,config) VALUES
           ('con','w','a_bot','ext_ws.bot','none','u1',1,1,'{}');
         INSERT INTO chan_write_blocks(connection_id,workspace_id,reason,blocked_at,until,noticed_at) VALUES
           ('con','w','只读',1,2,1);",
    )
    .unwrap();
    let space: String = c
        .query_row("SELECT space_id FROM chan_write_blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(space, "", "a block of the whole connection");
    c.execute("DELETE FROM connections WHERE id = 'con'", [])
        .unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM chan_write_blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

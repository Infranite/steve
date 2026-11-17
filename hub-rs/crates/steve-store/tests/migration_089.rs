//! 089: the refused writes a channel's connection or chat is remembered by. On an 088 database it
//! adds an empty table; a block goes with its connection.

use rusqlite::Connection;
use steve_store::db;

#[test]
fn applies_on_an_088_database_and_goes_with_the_connection() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(88);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..88] {
        c.execute_batch(m).unwrap();
    }
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
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

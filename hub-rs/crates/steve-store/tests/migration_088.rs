//! 088: identities numbered per space, whose personal account each is, and what each message was
//! seen by. On an 087 database every existing message takes every identity its space had, so
//! nobody sees more or less than before; a personal identity whose connection is gone keeps its
//! owner from the space's remembered account.

use rusqlite::Connection;
use steve_store::db;

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn applies_on_an_087_database_and_keeps_what_everyone_sees() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(87);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..87] {
        c.execute_batch(m).unwrap();
    }
    // s1: a bot (con_bot) and you through it (con_me); s2: only your account (con_me);
    // s3: a personal account whose connection is gone, remembered by the space
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO workspaces(id,name,created) VALUES ('w','w',1);
         INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES
           ('u1','w','s1','One','owner',1),('u2','w','s2','Two','member',1);
         INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated) VALUES
           ('p_me','w','person','我','u1','u1',1,1),('p_bot','w','bot','Bot',NULL,'u1',1,1),('p_x','w','person','X','u2','u1',1,1);
         INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated) VALUES
           ('a_me','w','slack','slack','p_me','personal','me','me','connect',1,1),
           ('a_bot','w','slack','slack','p_bot','app_bot','bot','bot','connect',1,1),
           ('a_x','w','slack','slack','p_x','personal','x','x','connect',1,1);
         INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated,config) VALUES
           ('con_bot','w','a_bot','fake','none','u1',1,1,'{}'),('con_me','w','a_me','ext_ws','none','u1',1,1,'{\"platform\":\"slack\"}');
         INSERT INTO spaces(id,workspace_id,system_id,connection_id,kind,external_id,name,created,updated,account_id) VALUES
           ('s1','w','slack','con_bot','group','G1','g1',1,1,NULL),('s2','w','slack','con_me','group','G2','g2',1,1,NULL),
           ('s3','w','slack',NULL,'group','G3','g3',1,1,'a_x'),('s_none','w','slack',NULL,'group','G4','g4',1,1,NULL);
         INSERT INTO space_identities(space_id,connection_id,workspace_id,joined) VALUES
           ('s1','con_bot','w',10),('s1','con_me','w',20),('s2','con_me','w',5),('s3','con_gone','w',1);
         INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,created) VALUES
           ('m1','w','s1','in','received',1),('m2','w','s2','in','received',1),('m3','w','s3','in','received',1),
           ('m4','w','s_none','in','received',1);",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
    let ord = |s: &str, conn: &str| -> (Option<i64>, Option<String>) {
        c.query_row(
            "SELECT ordinal, owner_user FROM space_identities WHERE space_id=? AND connection_id=?",
            [s, conn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    // numbered by when they joined; the bot is nobody's, your account is yours
    assert_eq!(ord("s1", "con_bot"), (Some(0), None));
    assert_eq!(ord("s1", "con_me"), (Some(1), Some("u1".into())));
    assert_eq!(ord("s2", "con_me"), (Some(0), Some("u1".into())));
    // the connection is gone: the account the space remembers says whose it was
    assert_eq!(ord("s3", "con_gone"), (Some(0), Some("u2".into())));
    // a channel service's platform comes from its connection; a built-in one's is learned later
    let platform = |conn: &str| -> Option<String> {
        c.query_row(
            "SELECT platform FROM space_identities WHERE space_id='s1' AND connection_id=?",
            [conn],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        (platform("con_me").as_deref(), platform("con_bot")),
        (Some("slack"), None)
    );
    let seen = |m: &str| -> i64 {
        one(
            &c,
            &format!("SELECT seen_by FROM chan_messages WHERE id='{m}'"),
        )
    };
    assert_eq!((seen("m1"), seen("m2"), seen("m3")), (0b11, 0b1, 0b1));
    // a message of a space without identities: left to the space
    assert_eq!(seen("m4"), 0);
    // numbers are unique within a space
    assert!(
        c.execute("INSERT INTO space_identities(space_id,connection_id,workspace_id,joined,ordinal) VALUES ('s1','con_z','w',1,1)", [])
            .is_err()
    );
}

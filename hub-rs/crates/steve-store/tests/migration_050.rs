//! 050: who a run works for. On a 049 database it fills an item's creator from the audit trail
//! (members only), a bot's owner (not the main bot's), each Node session's principal and audience,
//! and shares every confirmed paired Node with the workspaces its owner owns.

use rusqlite::Connection;
use steve_store::db;

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn applies_on_a_049_database_and_fills_principals() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(49);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..49] {
        c.execute_batch(m).unwrap();
    }
    // w1 is u1's alone; w2 is u1's with u2 as a member; u3 left w2
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO workspaces(id,name,created) VALUES ('w1','one',1),('w2','two',1);
         INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES
           ('u1','w1','s1','One','owner',1),('u2','w2','s2','Two','member',1);
         INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES
           ('w1','u1','owner',1),('w2','u1','owner',1),('w2','u2','member',2);
         INSERT INTO bots(id,workspace_id,name,main,harness,updated) VALUES
           ('steve','w1','Steve',1,'claude-code',1),('b2','w2','Dev',0,'codex',1);
         INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES
           ('i1','w1',1,'a','run','定义',1,1),('i2','w2',1,'b','run','定义',1,1),
           ('i3','w2',2,'c','run','定义',1,1),('i4','w2',3,'d','run','定义',1,1);
         INSERT INTO audit(workspace_id,ts,actor,action,target) VALUES
           ('w1',1,'u1','item.create','i1'),('w2',1,'u2','item.create','i2'),
           ('w2',1,'steve','item.create','i3'),('w2',1,'u3','item.create','i4');
         INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES
           ('s1','w1','i1','claude-code','','idle',1,1),('s2','w2','i2','codex','','idle',1,1),
           ('s3','w2','i3','codex','','idle',1,1),('s4','w2','i4','codex','','idle',1,1),
           ('s5','w1',NULL,'codex','','idle',1,1);
         INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
           SELECT id,'local','{}',id,'r','ready',1 FROM sessions;
         INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES
           ('n1','u1','mac',zeroblob(32),'f1','active',1),('n2','u2','pc',zeroblob(32),'f2','active',1),
           ('n3','u1','old',zeroblob(32),'f3','revoked',1),('n4','u1','new',zeroblob(32),'f4','pending',1);
         INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,local,created) VALUES
           ('nl','u1','hub',zeroblob(32),'fl','active',1,1);",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);

    let by = |i: &str| -> Option<String> {
        c.query_row("SELECT requested_by FROM items WHERE id=?", [i], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(by("i1").as_deref(), Some("u1"));
    assert_eq!(by("i2").as_deref(), Some("u2"));
    assert_eq!(by("i3"), None, "Steve asked: nobody");
    assert_eq!(by("i4"), None, "no longer a member");

    let owner = |b: &str| -> Option<String> {
        c.query_row("SELECT owner FROM bots WHERE id=?", [b], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(owner("steve"), None, "the main bot is the instance admin's");
    assert_eq!(owner("b2").as_deref(), Some("u1"));

    let run = |s: &str| -> (String, String) {
        c.query_row(
            "SELECT principal, audience FROM runtime_sessions WHERE session_id=?",
            [s],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(run("s1"), ("u1".into(), r#"["user:u1"]"#.into()));
    assert_eq!(run("s2"), ("u2".into(), r#"["workspace:w2"]"#.into()));
    assert_eq!(run("s3"), ("svc:w2".into(), r#"["workspace:w2"]"#.into()));
    assert_eq!(run("s4"), ("svc:w2".into(), r#"["workspace:w2"]"#.into()));
    // no item, nobody asked: the service principal, even in a workspace of one
    assert_eq!(run("s5"), ("svc:w1".into(), r#"["workspace:w1"]"#.into()));

    let grants: Vec<String> = c
        .prepare("SELECT node_id||'>'||grantee_kind||':'||grantee FROM node_grants WHERE accepted IS NOT NULL ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // not the revoked or the unconfirmed one; the local Node with every workspace
    assert_eq!(
        grants,
        [
            "n1>workspace:w1",
            "n1>workspace:w2",
            "nl>workspace:w1",
            "nl>workspace:w2"
        ]
    );
    let none: i64 = one(&c, "SELECT COUNT(*) FROM node_env");
    assert_eq!(none, 0);

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

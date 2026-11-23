use rusqlite::Connection;
use steve_store::{db, runtime};

#[test]
fn migrates_command_keys_and_permission_modes() {
    let c = Connection::open_in_memory().unwrap();
    for migration in &db::MIGRATIONS[..117] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(
        r#"
        INSERT INTO workspaces(id,name,created) VALUES('w','W',1);
        INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES
          ('s','w','h','.','idle',1,1),('other','w','h','.','idle',1,1);
        INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated,desired_config)
          VALUES ('s','n','{}','s','r','released',1,'{"policy":{"permissions":"plan"}}'),
                 ('other','n','{}','other','r','opening',1,NULL);
        INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,receipt,created,updated) VALUES
          ('open','n','open_session','{"body":{"session":"s","policy":{"permissions":"ask"}}}','d','finished','{"success":true}',1,1),
          ('cfg','n','configure_session','{"body":{"session":"s","policy":{"permissions":"plan"}}}','d','finished',
             '{"success":false,"payload":{"state":{"error":{"code":"denied","safe_detail":"Refused"}}}}',1,2),
          ('unknown','n','open_session','{"body":{"session":"other","policy":{"permissions":"auto"}}}','d','unknown',NULL,2,2),
          ('prep','n','prepare_workspace','{"body":{"resource":"item:a"}}','d','pending',NULL,1,1),
          ('tidy','n','tidy_workspace','{"subject":"tidy:a","body":{"workspace":{"id":"ws"}}}','d','pending',NULL,1,1),
          ('recover','n','recover','{"body":{"target":"open"}}','d','pending',NULL,1,1),
          ('abandon','n','abandon','{"body":{"reason":"stopped"}}','d','pending',NULL,1,1),
          ('exec','n','exec','{"body":{"limits":{"deadline":123}}}','d','pending',NULL,1,1),
          ('release','n','release_session','{"body":"s"}','d','pending',NULL,1,1);
        "#,
    )
    .unwrap();
    c.execute_batch(db::MIGRATIONS[117]).unwrap();

    let s = runtime::session_modes(&c, "n", "s").unwrap().unwrap();
    assert_eq!(s.applied.as_deref(), Some("ask"));
    assert_eq!(s.desired.as_deref(), Some("plan"));
    assert_eq!(s.refusal_detail.as_deref(), Some("Refused"));
    assert!(s.refused && s.released && !s.sending && !s.unknown);
    let code: String = c
        .query_row(
            "SELECT mode_refusal_code FROM runtime_sessions WHERE session_id='s'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(code, "denied");
    let other = runtime::session_modes(&c, "n", "other").unwrap().unwrap();
    assert!(other.unknown && !other.sending && other.applied.is_none());

    assert_eq!(
        c.query_row(
            "SELECT group_concat(key,'|') FROM (
                SELECT coalesce(workspace_resource,workspace_id,recovery_target,abandon_reason,exec_deadline,session_id) AS key
                FROM node_commands WHERE method NOT IN ('open_session','configure_session') ORDER BY rowid
            )",
            [],
            |r| r.get::<_, String>(0),
        ).unwrap(),
        "item:a|ws|open|stopped|123|s",
    );
    assert_eq!(
        c.query_row(
            "SELECT subject FROM node_commands WHERE operation_id='tidy'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "tidy:a",
    );
}

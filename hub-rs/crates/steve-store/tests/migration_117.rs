use rusqlite::Connection;
use steve_store::db;

fn one(c: &Connection, sql: &str) -> String {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn old_shapes_are_rewritten_or_removed_once() {
    assert_eq!(db::MIGRATIONS[116], include_str!("../migrations/117.sql"));
    let c = Connection::open_in_memory().unwrap();
    for migration in &db::MIGRATIONS[..116] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch(
        r#"PRAGMA foreign_keys = OFF;
        INSERT INTO workspaces(id,name,created) VALUES ('w','W',1);
        INSERT INTO messages(id,workspace_id,space,author,text,created,meta) VALUES
         ('unconfirmed','w','chan:s','steve','hi',1,'{"chan_out":{"status":"sent","message":null}}'),
         ('confirmed','w','chan:s','steve','hi',1,'{"chan_out":{"status":"sent","message":"cm1"}}'),
         ('failed_build','w','main','steve','x',1,'{"card":{"item":"ST-1","released":{"version":"build failed: x","at":5}}}'),
         ('released','w','main','steve','x',1,'{"card":{"item":"ST-1","released":{"version":"1.2.3","at":5}}}');
        INSERT INTO chan_messages(id,workspace_id,space_id,direction,created) VALUES ('cm1','w','s','out',1);
        INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated)
         VALUES ('ended','w','h','.','ended',1,1),('idle','w','h','.','idle',1,1);
        INSERT INTO session_stops(session_id,stop_asked,asked_seq,updated)
         VALUES ('ended','a',NULL,1),('idle','b',3,1);
        INSERT INTO queued(id,session_id,text,via,created)
         VALUES ('q_ended','ended','t','me',1),('q_idle','idle','t','me',1);
        INSERT INTO settings VALUES ('w','models.route','{"models":["a"]}',1),
         ('w','models.title','{"harness":"codex","models":["b"]}',1),
         ('w','models.v2','2',3),('w','models.v3','1',4);
        INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,question,options,status,requested_by,created) VALUES
         ('handoff','w','ask','chan_message','x','q','["draft_reply","to_item","done"]','open','steve',1),
         ('group','w','ask','space','x','q','["open","mute"]','open','steve',1),
         ('answered','w','ask','space','x','q','["open","mute"]','resolved','steve',1);
        INSERT INTO bot_tool_pins VALUES ('b','tool','steve_whois',2),('b','tool','steve_contacts_find',1),('b','tool','steve_debug',1);
        INSERT INTO tool_usage(bot_id,tool,day,called) VALUES ('b','steve_whois',1,3),('b','steve_debug',1,1);"#,
    )
    .unwrap();
    c.execute_batch(db::MIGRATIONS[116]).unwrap();

    let meta = |id: &str| one(&c, &format!("SELECT meta FROM messages WHERE id='{id}'"));
    let unconfirmed: serde_json::Value = serde_json::from_str(&meta("unconfirmed")).unwrap();
    assert_eq!(unconfirmed["chan_out"]["status"], "unknown");
    assert!(unconfirmed["chan_out"]["reason"].is_string());
    assert!(meta("confirmed").contains(r#""status":"sent""#));
    let card: serde_json::Value = serde_json::from_str(&meta("failed_build")).unwrap();
    assert_eq!(
        card["card"]["build"],
        serde_json::json!({"text": "build failed: x", "at": 5})
    );
    assert!(card["card"]["released"].is_null());
    assert!(meta("released").contains(r#""version":"1.2.3""#));

    assert_eq!(
        one(
            &c,
            "SELECT group_concat(session_id) FROM session_stops WHERE stop_asked IS NOT NULL"
        ),
        "idle"
    );
    assert_eq!(one(&c, "SELECT group_concat(id) FROM queued"), "q_idle");

    let setting = |k: &str| one(&c, &format!("SELECT value FROM settings WHERE key='{k}'"));
    assert_eq!(
        setting("models.route"),
        r#"{"models":["a"],"harness":"claude-code"}"#
    );
    assert_eq!(
        setting("models.title"),
        r#"{"harness":"codex","models":["b"]}"#
    );
    assert_eq!(setting("models.seeded"), "true");
    assert_eq!(
        one(
            &c,
            "SELECT CAST(count(*) AS TEXT) FROM settings WHERE key IN ('models.v2','models.v3')"
        ),
        "0"
    );

    let options = |id: &str| {
        one(
            &c,
            &format!("SELECT options FROM decisions WHERE id='{id}'"),
        )
    };
    assert_eq!(
        options("handoff"),
        r#"["instruct","verbatim","to_item","done"]"#
    );
    assert_eq!(options("group"), r#"["open","mute","quiet","block"]"#);
    assert_eq!(options("answered"), r#"["open","mute"]"#);

    assert_eq!(
        one(
            &c,
            "SELECT group_concat(target || ':' || created, ',') FROM (SELECT * FROM bot_tool_pins ORDER BY target)"
        ),
        "steve_contacts:1,steve_debug:1"
    );
    assert_eq!(
        one(&c, "SELECT group_concat(tool) FROM tool_usage"),
        "steve_debug"
    );
}

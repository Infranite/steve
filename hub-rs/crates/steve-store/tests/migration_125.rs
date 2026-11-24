use rusqlite::Connection;
use steve_store::db;

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn text(c: &Connection, sql: &str) -> Option<String> {
    c.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn links_members_to_parties_and_backfills_actors() {
    assert_eq!(db::MIGRATIONS[124], include_str!("../migrations/125.sql"));
    let c = Connection::open_in_memory().unwrap();
    for sql in &db::MIGRATIONS[..122] {
        c.execute_batch(sql).unwrap();
    }
    c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
    for sql in &db::MIGRATIONS[122..124] {
        c.execute_batch(sql).unwrap();
    }
    c.execute_batch(
        r#"CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(124);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL); INSERT INTO schema_compat VALUES(124);

         INSERT OR IGNORE INTO workspaces(id,name,created) VALUES
           ('ws_one','Solo',1),
           ('ws_many','Team',1);

         INSERT INTO users(id,workspace_id,subject,name,email,role,created) VALUES
           ('usr_solo','ws_one','sub_solo','solo.login','solo@example.test','owner',1),
           ('usr_linked','ws_many','sub_linked','linked.login','linked@example.test','owner',1),
           ('usr_two','ws_many','sub_two','two.login','two@example.test','member',1);

         INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES
           ('ws_one','usr_solo','owner',1),
           ('ws_many','usr_linked','owner',1),
           ('ws_many','usr_two','member',1);

         -- usr_linked already has an active contact party with a person display name
         INSERT INTO parties(id,workspace_id,type,name,rels,contact_kind,user_id,status,created_by,created,updated)
         VALUES ('ct_existing','ws_many','contact','Lin Zhi','["me"]','real','usr_linked','active','usr_linked',1,1);

         INSERT INTO items(id,workspace_id,no,title,phase,created,updated) VALUES
           ('ST-1','ws_one',1,'Solo item','active',1,1),
           ('ST-2','ws_many',1,'Team item','active',1,1);

         INSERT INTO timeline(item_id,kind,text,actor,ts) VALUES
           ('ST-1','me','Solo comment','me',10),
           ('ST-1','sys','Bot line','steve',11),
           ('ST-2','me','Ambiguous legacy comment','me',12);

         INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,item_id,question,options,status,result,requested_by,created,resolved)
         VALUES
           ('dec_human','ws_one','approve','permission','s1','ST-1','Approve?','["yes"]','resolved','{"choice":"yes"}','code',10,20),
           ('dec_yolo','ws_one','approve','permission','s1','ST-1','Auto?','["yes"]','resolved','{"choice":"yes","decided_by":"yolo"}','code',11,21),
           ('dec_open','ws_one','approve','permission','s1','ST-1','Still open?','["yes"]','open','null','code',12,NULL),
           ('dec_exp','ws_one','approve','permission','s1','ST-1','Expired?','["yes"]','expired','null','code',13,22),
           ('dec_multi','ws_many','approve','permission','s2','ST-2','Team approve?','["yes"]','resolved','{"choice":"yes"}','code',14,23);"#,
    )
    .unwrap();

    db::migrate(&c).unwrap();
    // Idempotent when run twice.
    db::migrate(&c).unwrap();

    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );

    // Every workspace member now has exactly one active linked contact party.
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM parties WHERE workspace_id='ws_one' AND user_id='usr_solo' AND status='active'"
        ),
        1
    );
    assert_eq!(
        text(
            &c,
            "SELECT name FROM parties WHERE workspace_id='ws_one' AND user_id='usr_solo' AND status='active'"
        )
        .as_deref(),
        Some("solo.login")
    );
    // Existing linked contact was preserved without duplicating.
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM parties WHERE workspace_id='ws_many' AND user_id='usr_linked' AND status='active'"
        ),
        1
    );
    assert_eq!(
        text(
            &c,
            "SELECT name FROM parties WHERE workspace_id='ws_many' AND user_id='usr_linked' AND status='active'"
        )
        .as_deref(),
        Some("Lin Zhi")
    );
    // Second member in ws_many also got a linked contact.
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM parties WHERE workspace_id='ws_many' AND user_id='usr_two' AND status='active'"
        ),
        1
    );

    // Timeline backfill: single-member workspace maps 'me' -> usr_solo; multi-member stays 'me'; bot untouched.
    assert_eq!(
        text(
            &c,
            "SELECT actor FROM timeline WHERE item_id='ST-1' AND kind='me'"
        )
        .as_deref(),
        Some("usr_solo")
    );
    assert_eq!(
        text(
            &c,
            "SELECT actor FROM timeline WHERE item_id='ST-1' AND kind='sys'"
        )
        .as_deref(),
        Some("steve")
    );
    assert_eq!(
        text(
            &c,
            "SELECT actor FROM timeline WHERE item_id='ST-2' AND kind='me'"
        )
        .as_deref(),
        Some("me")
    );

    // Decisions resolved_by backfill.
    assert_eq!(
        text(&c, "SELECT resolved_by FROM decisions WHERE id='dec_human'").as_deref(),
        Some("usr_solo")
    );
    assert_eq!(
        text(&c, "SELECT resolved_by FROM decisions WHERE id='dec_yolo'").as_deref(),
        Some("yolo")
    );
    assert_eq!(
        text(&c, "SELECT resolved_by FROM decisions WHERE id='dec_open'"),
        None
    );
    assert_eq!(
        text(&c, "SELECT resolved_by FROM decisions WHERE id='dec_exp'"),
        None
    );
    assert_eq!(
        text(&c, "SELECT resolved_by FROM decisions WHERE id='dec_multi'"),
        None
    );

    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
        0
    );
}

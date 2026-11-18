use rusqlite::Connection;
use steve_store::db;

#[test]
fn upgrade_keeps_outgoing_reactions_separate_and_cascades_removed_message_data() {
    let c = Connection::open_in_memory().unwrap();
    for migration in &db::MIGRATIONS[..110] {
        c.execute_batch(migration).unwrap();
    }
    c.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL);
        INSERT INTO schema_version VALUES(110);
        CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);
        INSERT INTO schema_compat VALUES(110);
        INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
        INSERT INTO parties(id,workspace_id,type,name,created_by,created,updated)
          VALUES('owner','default','person','Owner','owner',1,1);
        INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated)
          VALUES('account','default','slack','slack_user','owner','personal','u_owner','Owner','connect',1,1);
        INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated)
          VALUES('space','default','slack','dm','chat','Peer',1,1);
        INSERT INTO chan_messages(id,workspace_id,space_id,external_id,direction,text,created)
          VALUES('message','default','space','platform-id','in','Question',1);
        INSERT INTO chan_reactions(message_id,workspace_id,state,emoji,updated)
          VALUES('message','default','done','DONE',2);").unwrap();
    db::migrate(&c).unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;
        INSERT INTO chan_message_reactions(workspace_id,connection_id,message_external_id,message_id,actor_account_id,emoji,origin,removed,action_at)
          VALUES('default','connection','platform-id','message','account','👍','manual',1,3);
        INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,created)
          VALUES('reply','default','space','out','Answer',3);
        INSERT INTO chan_reply_sources(out_message_id,source_message_id) VALUES('reply','message');").unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(c.query_row("SELECT time_source, source_revision FROM chan_message_reactions WHERE message_id='message'", [],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))).unwrap(), ("provider".into(), None));
    assert_eq!(
        c.query_row(
            "SELECT state FROM chan_reactions WHERE message_id='message'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "done"
    );
    assert_eq!(
        c.query_row(
            "SELECT removed FROM chan_message_reactions WHERE message_id='message'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    // Removing an account is not blocked by new reaction references; outgoing state is unchanged.
    c.execute("DELETE FROM accounts WHERE id='account'", [])
        .unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM chan_message_reactions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    c.execute("DELETE FROM chan_reactions WHERE message_id='message'", [])
        .unwrap();
    c.execute("DELETE FROM chan_messages WHERE id='message'", [])
        .unwrap();
    assert_eq!(
        c.query_row("SELECT count(*) FROM chan_reply_sources", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        c.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}

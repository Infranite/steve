//! Personal channel indicators respect visibility/read state, not Steve's participation mode.
use rusqlite::params;
use steve_channels::conversations;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2};
use steve_store::base::Viewer;

fn setup() -> TestEnv {
    let e = TestEnv::new(|_| {});
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO parties(id,workspace_id,type,name,created_by,created,updated) VALUES('org','default','organization','Org','usr_dev',1,1);
        INSERT INTO parties(id,workspace_id,type,name,user_id,created_by,created,updated) VALUES('mine','default','person','Me','usr_dev','usr_dev',1,1);
        INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated) VALUES
          ('shared','default','feishu','feishu_app','org','app_bot','shared','shared','connect',1,1),
          ('private','default','feishu','feishu_user','mine','personal','me','me','connect',1,1);
        INSERT INTO connections(id,workspace_id,account_id,adapter,auth,caps,created_by,created,updated) VALUES
          ('bot','default','shared','feishu_app','none','[\"msg.receive\"]','usr_dev',1,1),
          ('own','default','private','feishu_user','none','[\"msg.receive\"]','usr_dev',1,1);
        INSERT INTO channel_settings(connection_id,workspace_id,mode,rules,updated) VALUES('bot','default','bot','{}',1),('own','default','self','{}',1);")?;
        Ok(())
    }).unwrap();
    e
}
fn add(e: &TestEnv, id: &str, kind: &str, connection: &str) {
    e.store().with_conn(|c| {
        c.execute("INSERT INTO spaces(id,workspace_id,system_id,connection_id,kind,external_id,name,created,updated) VALUES(?1,'default','feishu',?2,?3,?1,?1,1,1)",params![id,connection,kind])?;
        c.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,created) VALUES(?1,'default',?2,'in','Private body',100)",params![format!("message_{id}"),id])?;
        Ok(())
    }).unwrap();
}
fn list(e: &TestEnv, user: &str) -> Vec<steve_store::attention::Entry> {
    e.store()
        .reader(|c| conversations::attention(c, WS, &Viewer::bare(WS, user), 200))
        .unwrap()
}
#[test]
fn channel_attention_is_personal_visible_enabled_and_quiet_is_not_mute() {
    let e = setup();
    add(&e, "dm", "dm", "bot");
    add(&e, "group", "group", "bot");
    add(&e, "private", "dm", "own");
    add(&e, "closed", "dm", "bot");
    add(&e, "blocked", "group", "bot");
    add(&e, "left", "group", "bot");
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO space_prefs(space_id,workspace_id,attention,updated) VALUES('dm','default','quiet',1),('blocked','default','blocked',1);
          INSERT INTO space_settings(space_id,workspace_id,muted,disclosure,updated) VALUES('dm','default',1,'{}',1);
          INSERT INTO conversation_preferences(workspace_id,user_id,space_id,closed) VALUES('default','usr_dev','closed',1);
          UPDATE spaces SET status='left' WHERE id='left';")?;
        Ok(())
    }).unwrap();
    let mine = list(&e, ME);
    assert_eq!(
        mine.len(),
        3,
        "quiet and role muted still surface unread; closed/blocked/left do not: {mine:?}"
    );
    assert!(mine.iter().any(|x| x.id == "chan:dm" && x.notify));
    assert!(mine.iter().any(|x| x.id == "chan:group" && !x.notify));
    assert!(mine.iter().all(|x| x.group_key.is_none()));
    let other = list(&e, OTHER);
    assert_eq!(
        other.len(),
        3,
        "other sees closed conversation but not private DM"
    );
    assert!(!other.iter().any(|x| x.id == "chan:private"));
    assert!(
        e.store()
            .reader(|c| conversations::attention(c, WS2, &Viewer::bare(WS2, OTHER), 200))
            .is_err()
    );
    e.store()
        .transaction(|tx| {
            steve_channels::conv_record::mark_read(
                tx,
                WS,
                &Viewer::bare(WS, ME),
                "chan:dm",
                100,
                200,
            )
        })
        .unwrap();
    assert!(!list(&e, ME).iter().any(|x| x.id == "chan:dm"));
    assert!(list(&e, OTHER).iter().any(|x| x.id == "chan:dm"));
    e.store()
        .with_conn(|c| {
            c.execute("UPDATE connections SET status='paused' WHERE id='bot'", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        list(&e, ME).len(),
        1,
        "disabled bot channels disappear, personal DM remains"
    );
    e.store()
        .with_conn(|c| {
            c.execute("UPDATE connections SET status='revoked' WHERE id='own'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(list(&e, ME).is_empty());
}

#[test]
fn channels_use_actual_live_incoming_visible_ids_not_unread_count_or_rowid() {
    let e = setup();
    add(&e, "dm", "dm", "bot");
    e.store().with_conn(|c| {
        c.execute_batch("INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,history,sender_party_id,created) VALUES
          ('out','default','dm','out','sent',NULL,NULL,101),
          ('history','default','dm','in','old','sync',NULL,102),
          ('self','default','dm','in','own',NULL,'mine',103);")?;
        Ok(())
    }).unwrap();
    assert_eq!(list(&e, ME)[0].event_id, "message_dm");
    e.store().with_conn(|c| {
        c.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,created) VALUES('new-live','default','dm','in','New',104)",[])?;Ok(())
    }).unwrap();
    let a = list(&e, ME);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].event_id, "new-live");
    assert!(
        e.store()
            .reader(|c| conversations::attention(
                c,
                WS,
                &Viewer::bare(WS, ME),
                steve_channels::conv_record::UNREAD_HORIZON_MS + 200
            ))
            .unwrap()
            .is_empty()
    );
}

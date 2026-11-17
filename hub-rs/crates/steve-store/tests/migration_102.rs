//! 102: alarms, a personal notification's topic and the feed kind `reminder`. The feed table is rebuilt:
//! its rows, read marks and sequence are kept, and so are 098's unread-index triggers on it; the reminders
//! and snoozes written before are planned.

use rusqlite::Connection;
use steve_store::db;

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn the_feed_keeps_its_rows_and_numbers_and_old_reminders_are_planned() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(101);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..101] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
         INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('u1','default','s1','One','owner',1);
         INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('default','u1','owner',1);
         INSERT INTO outbox_events(id,workspace_id,topic,version,payload,created)
           VALUES('evt_1','default','work.item.done',1,'{\"item_id\":\"ST-1\"}',1),('evt_2','default','work.item.done',1,'{}',2);
         INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created)
           VALUES('ntf_1','default','u1','evt_1','notification:evt_1:u1','log','Work item completed: one','held',1),
                 ('ntf_2','default','u1','evt_2','notification:evt_2:u1','log','Work item completed: two','held',2);
         INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,kind,message_id,created)
           VALUES('ntf_s','default','u1','cm_1','notification:cm_1:u1','log','A 私聊你','held','starred','cm_1',2);
         INSERT INTO feed_reads(workspace_id,user_id,entry_id,read_at) VALUES('default','u1','feed_ntf_1',5);
         DELETE FROM feed_entries WHERE id='feed_ntf_2';
         INSERT INTO todos(id,workspace_id,owner,text,state,source,writer,created,updated,remind_at)
           VALUES('td_open','default','u1','open one','open','you','u1',1,1,100),
                 ('td_done','default','u1','done one','done','you','u1',1,1,100),
                 ('td_none','default','u1','no reminder','open','you','u1',1,1,NULL);",
    )
    .unwrap();
    let seq = count(
        &c,
        "SELECT seq FROM sqlite_sequence WHERE name='feed_entries'",
    );
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);

    // kept: the entry, its kind and item, its read mark; the removed newest number is not given again
    let (kind, item): (String, String) = c
        .query_row(
            "SELECT kind, item_id FROM feed_entries WHERE id='feed_ntf_1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), item.as_str()), ("completion", "ST-1"));
    assert_eq!(count(&c, "SELECT count(*) FROM feed_reads"), 1);
    assert_eq!(
        count(
            &c,
            "SELECT seq FROM sqlite_sequence WHERE name='feed_entries'"
        ),
        seq
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM personal_notifications WHERE topic='work.item.done'"
        ),
        2
    );

    // 101's `kind` is folded into the one topic: a starred contact's notification is `contact.starred`, and its
    // feed entry stays `starred`
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM personal_notifications WHERE id='ntf_s' AND topic='contact.starred'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM feed_entries WHERE id='feed_ntf_s' AND kind='starred'"
        ),
        1
    );

    // a reminder's notification is a `reminder` entry, numbered after the old ones
    c.execute_batch(
        "INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic)
           VALUES('ntf_3','default','u1','evt_3','notification:evt_3:u1','log','待办提醒：x','held',3,'todo.due');",
    )
    .unwrap();
    let (kind, n): (String, i64) = c
        .query_row(
            "SELECT kind, seq FROM feed_entries WHERE id='feed_ntf_3'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "reminder");
    assert!(n > seq);
    // 098's unread index still follows the rebuilt table: a new entry is unread, a read mark clears it
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='feed_entries'
               AND name IN ('attention_feed_entry_add','attention_feed_entry_before_update','attention_feed_entry_update')"
        ),
        3
    );
    let unread =
        "SELECT count(*) FROM attention_feed_unread WHERE user_id='u1' AND entry_id='feed_ntf_3'";
    assert_eq!(count(&c, unread), 1);
    c.execute_batch(
        "INSERT INTO feed_reads(workspace_id,user_id,entry_id,read_at) VALUES('default','u1','feed_ntf_3',6);",
    )
    .unwrap();
    assert_eq!(count(&c, unread), 0);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM attention_feed_unread WHERE entry_id='feed_ntf_1'"
        ),
        0,
        "a read mark from before stays read"
    );
    assert!(
        c.execute(
            "INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic)
               VALUES('ntf_4','default','u1','evt_4','notification:evt_4:u1','log','x','held',4,'elsewhere')",
            [],
        )
        .is_err(),
        "an unknown topic is refused"
    );

    // the open todo with a reminder is to be planned; the done one and the one without are not
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM time_alarm_dirty WHERE subject='todo:td_open'"
        ),
        1
    );
    assert_eq!(count(&c, "SELECT count(*) FROM time_alarm_dirty"), 1);
    // and from now on the triggers mark changes
    c.execute_batch(
        "DELETE FROM time_alarm_dirty;
         UPDATE todos SET text='renamed' WHERE id='td_none';
         UPDATE todos SET snooze_until=200 WHERE id='td_none';",
    )
    .unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM time_alarm_dirty WHERE subject='todo:td_none'"
        ),
        1,
        "a snooze marks it, a new text does not"
    );
    c.execute_batch(
        "DELETE FROM time_alarm_dirty; UPDATE todos SET text='again' WHERE id='td_none';",
    )
    .unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM time_alarm_dirty"), 0);
    assert_eq!(
        count(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
}

/// Every topic the column allows is mapped to its feed kind by the insert trigger, by name; nothing falls back.
#[test]
fn every_topic_has_its_feed_kind() {
    let c = Connection::open_in_memory().unwrap();
    db::migrate(&c).unwrap();
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
         INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('u1','default','s1','One','owner',1);
         INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('default','u1','owner',1);",
    )
    .unwrap();
    for (n, (topic, kind)) in [
        ("work.item.done", "completion"),
        ("todo.due", "reminder"),
        ("snooze.wake", "reminder"),
        ("contact.starred", "starred"),
        // 103
        ("briefing.ready", "briefing"),
    ]
    .into_iter()
    .enumerate()
    {
        c.execute(
            "INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic)
               VALUES(?1,'default','u1',?1,'notification:'||?1,'log','x','held',1,?2)",
            rusqlite::params![format!("ntf_{n}"), topic],
        )
        .unwrap();
        let got: String = c
            .query_row(
                "SELECT kind FROM feed_entries WHERE notification_id=?",
                [format!("ntf_{n}")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(got, kind, "{topic}");
    }
    // the column's CHECK lists exactly these (103 rebuilt the table and added `briefing.ready`)
    let ddl: String = c
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='personal_notifications'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        ddl.contains(
            "CHECK(topic IN ('work.item.done','todo.due','snooze.wake','contact.starred','briefing.ready'))"
        )
    );
}

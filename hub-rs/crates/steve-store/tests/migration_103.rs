//! 103: the morning brief. The notification and feed tables are rebuilt for the topic `briefing.ready` and
//! the feed kind `briefing`: their rows and every column (101's `kind` and `message_id`, 102's `topic`),
//! numbers, read marks and the triggers on them are kept, and 101's starred purge still works on the new
//! table; a brief schedule, its owner's time zone and the workspace's mark it to be planned again.

use rusqlite::Connection;
use steve_store::db;

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

const BRIEF: &str = r#"{"requester":"u1","when":"工作日 08:30","cron":"30 8 * * 1-5","tz":"Asia/Taipei","what":"","bot":"","mode":"brief"}"#;
const ITEM: &str = r#"{"when":"每天 09:00","cron":"0 9 * * *","tz":"Asia/Taipei","what":"x","bot":"b","mode":"item"}"#;

#[test]
fn notifications_and_the_feed_keep_everything_and_briefs_are_marked_for_planning() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(102);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..102] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('default','Fixture',1);
         INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('u1','default','s1','One','owner',1),('u2','default','s2','Two','member',1);
         INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('default','u1','owner',1),('default','u2','member',1);
         INSERT INTO outbox_events(id,workspace_id,topic,version,payload,created)
           VALUES('evt_1','default','work.item.done',1,'{\"item_id\":\"ST-1\"}',1),('evt_2','default','todo.due',1,'{}',2);
         INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic)
           VALUES('ntf_1','default','u1','evt_1','notification:evt_1:u1','log','Work item completed: one','held',1,'work.item.done'),
                 ('ntf_2','default','u1','evt_2','notification:evt_2:u1','now','待办提醒：x','waiting',2,'todo.due');
         INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,created,updated)
           VALUES('sp_dm','default','feishu','dm','dm','A',1,1),('sp_me','default','feishu','dm','me','Me',1,1);
         INSERT INTO chan_messages(id,workspace_id,space_id,direction,text,status,created)
           VALUES('cm_in','default','sp_dm','in','A: 在吗','received',3),('cm_out','default','sp_me','out','A 私聊你：在吗','pending',3),
                 ('cm_in2','default','sp_dm','in','A: 还在吗','received',3),('cm_out2','default','sp_me','out','A 私聊你：还在吗','pending',3);
         INSERT INTO output_deliveries(id,workspace_id,requester,idempotency_key,fingerprint,intent,connection_id,space_id,message_id,created)
           VALUES('od_1','default','u1','k1','f1','{\"content\":{\"markdown\":\"x\"}}','con','sp_me','cm_out',3),
                 ('od_2','default','u1','k2','f2','{\"content\":{\"markdown\":\"y\"}}','con','sp_me','cm_out2',3);
         INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic,message_id,delivery_id)
           VALUES('ntf_s','default','u1','cm_in','notification:cm_in:u1','now','A 私聊你：在吗','submitted',3,'contact.starred','cm_in','od_1'),
                 ('ntf_s2','default','u1','cm_in2','notification:cm_in2:u1','now','A 私聊你：还在吗','submitted',3,'contact.starred','cm_in2','od_2');
         UPDATE personal_notifications SET kind='starred' WHERE topic='contact.starred';
         INSERT INTO feed_reads(workspace_id,user_id,entry_id,read_at) VALUES('default','u1','feed_ntf_1',5);",
    )
    .unwrap();
    let seq = count(
        &c,
        "SELECT seq FROM sqlite_sequence WHERE name='feed_entries'",
    );
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);

    // kept: every notification with its topic, 101's kind and message, state and entry; the read mark;
    // the numbering
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM personal_notifications WHERE (id='ntf_1' AND topic='work.item.done' AND kind='completion' AND state='held')
               OR (id='ntf_2' AND topic='todo.due' AND state='waiting' AND mode='now')
               OR (id='ntf_s' AND topic='contact.starred' AND kind='starred' AND message_id='cm_in' AND delivery_id='od_1')"
        ),
        3
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM feed_entries WHERE (id='feed_ntf_1' AND kind='completion' AND item_id='ST-1')
               OR (id='feed_ntf_2' AND kind='reminder') OR (id='feed_ntf_s' AND kind='starred')"
        ),
        3
    );
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
            "SELECT count(*) FROM attention_feed_unread WHERE user_id='u1' AND entry_id='feed_ntf_2'"
        ),
        1,
        "the unread one is still unread"
    );
    for (table, n) in [("feed_entries", 3), ("personal_notifications", 1)] {
        assert_eq!(
            count(
                &c,
                &format!(
                    "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='{table}'"
                )
            ),
            n,
            "{table}'s triggers"
        );
    }
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('personal_notifications_due','personal_notifications_delivery','feed_scope','feed_recipient')"
        ),
        4
    );

    // every topic is a feed kind of its own name: a brief's notification is a `briefing` entry, unread,
    // numbered after the old ones
    c.execute_batch(
        "INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic)
           VALUES('ntf_3','default','u1','evt_3','notification:evt_3:u1','log','晨报 · 02-03 周三','held',3,'briefing.ready'),
                 ('ntf_4','default','u1','evt_4','notification:evt_4:u1','log','x','held',4,'work.item.done'),
                 ('ntf_5','default','u1','evt_5','notification:evt_5:u1','log','x','held',5,'todo.due'),
                 ('ntf_6','default','u1','evt_6','notification:evt_6:u1','log','x','held',6,'snooze.wake'),
                 ('ntf_7','default','u1','evt_7','notification:evt_7:u1','log','x','held',7,'contact.starred');",
    )
    .unwrap();
    let (kind, n): (String, i64) = c
        .query_row(
            "SELECT kind, seq FROM feed_entries WHERE id='feed_ntf_3'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "briefing");
    assert!(n > seq);
    for (id, kind) in [
        ("feed_ntf_4", "completion"),
        ("feed_ntf_5", "reminder"),
        ("feed_ntf_6", "reminder"),
        ("feed_ntf_7", "starred"),
    ] {
        assert_eq!(
            count(
                &c,
                &format!("SELECT count(*) FROM feed_entries WHERE id='{id}' AND kind='{kind}'")
            ),
            1,
            "{id}"
        );
    }

    // 101's purge of a starred contact's copies still runs against the rebuilt table: purging the
    // message's text, then deleting another
    c.execute_batch("UPDATE chan_messages SET text=NULL WHERE id='cm_in';")
        .unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM personal_notifications WHERE id='ntf_s' AND body IS NULL AND state='purged'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM chan_messages WHERE id='cm_out' AND text IS NULL AND status='discarded'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM output_deliveries WHERE id='od_1' AND json_extract(intent,'$.content')='{}'"
        ),
        1
    );
    c.execute_batch("DELETE FROM chan_messages WHERE id='cm_in2';")
        .unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM personal_notifications WHERE id='ntf_s2' AND body IS NULL AND state='purged'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM chan_messages WHERE id='cm_out2' AND text IS NULL AND status='discarded'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM attention_feed_unread WHERE user_id='u1' AND entry_id='feed_ntf_3'"
        ),
        1
    );
    // deleting a notification still clears its entry's reference, not the entry
    c.execute_batch("PRAGMA foreign_keys=ON; DELETE FROM personal_notifications WHERE id='ntf_1';")
        .unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM feed_entries WHERE id='feed_ntf_1' AND notification_id IS NULL"
        ),
        1
    );

    // a brief schedule is marked when made, changed, moved to another clock; an item schedule is not
    let dirty = |c: &Connection| {
        count(
            c,
            "SELECT count(*) FROM time_alarm_dirty WHERE subject='brief:flw_b'",
        )
    };
    c.execute(
        "INSERT INTO flows(id,workspace_id,kind,name,enabled,graph,origin,schedule,created_by,created,updated)
           VALUES('flw_b','default','schedule','晨报',1,'{}','steve',?1,'u1',1,1),
                 ('flw_i','default','schedule','x',1,'{}','me',?2,'u1',1,1),
                 ('flw_g','default','flow','g',1,'{}','me',NULL,'u1',1,1)",
        [BRIEF, ITEM],
    )
    .unwrap();
    assert_eq!(dirty(&c), 1);
    assert_eq!(count(&c, "SELECT count(*) FROM time_alarm_dirty"), 1);
    for change in [
        "UPDATE flows SET enabled=0 WHERE id='flw_b'",
        "INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated) VALUES('default','u1',0,'log','UTC',1,0,1)",
        "UPDATE notification_policies SET timezone='Asia/Tokyo' WHERE user_id='u1'",
        "INSERT INTO settings(workspace_id,key,value,updated) VALUES('default','tz','\"UTC\"',1)",
        "UPDATE settings SET value='\"Asia/Tokyo\"' WHERE key='tz'",
        "DELETE FROM workspace_members WHERE user_id='u1'",
    ] {
        c.execute_batch("DELETE FROM time_alarm_dirty;").unwrap();
        c.execute_batch(change).unwrap();
        assert_eq!(dirty(&c), 1, "{change}");
    }
    // changes that do not move a brief
    for change in [
        "UPDATE flows SET name='renamed' WHERE id='flw_b'",
        "UPDATE flows SET enabled=0 WHERE id IN ('flw_i','flw_g')",
        "INSERT INTO notification_policies(workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated) VALUES('default','u2',0,'log','UTC',1,0,1)",
        "UPDATE settings SET updated=2 WHERE key='tz'",
    ] {
        c.execute_batch("DELETE FROM time_alarm_dirty;").unwrap();
        c.execute_batch(change).unwrap();
        assert_eq!(
            count(&c, "SELECT count(*) FROM time_alarm_dirty"),
            0,
            "{change}"
        );
    }
    assert!(
        c.execute(
            "INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created,topic)
               VALUES('ntf_9','default','u2','evt_9','notification:evt_9:u2','log','x','held',9,'elsewhere')",
            [],
        )
        .is_err(),
        "an unknown topic is still refused"
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
}

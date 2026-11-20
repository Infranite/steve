//! Every allowed notification topic maps to its current feed kind.
#[path = "common/current.rs"]
mod current;

use rusqlite::Connection;

/// Every topic the column allows is mapped to its feed kind by the insert trigger, by name; nothing falls back.
#[test]
fn every_topic_has_its_feed_kind() {
    let mut c = Connection::open_in_memory().unwrap();
    current::migrate(&mut c).unwrap();
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

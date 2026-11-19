#[path = "common/history.rs"]
mod history;
use rusqlite::Connection;
use steve_store_sqlite::{
    convert::{self, Outcome},
    migrate, schema,
};

fn columns(c: &Connection, table: &str) -> Vec<String> {
    c.prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
fn physical_tables(c: &Connection) -> Vec<(String, i64, String)> {
    c.prepare("SELECT name,rootpage,sql FROM sqlite_schema WHERE name IN ('items','sessions') ORDER BY name").unwrap()
        .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}
#[test]
fn historical_order_converts_without_rebuild_and_item_session_repositories_read_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let mut c = Connection::open(&path).unwrap();
    history::after_050(&c);
    let items = columns(&c, "items");
    let sessions = columns(&c, "sessions");
    assert_eq!(
        &items[25..33],
        [
            "phase",
            "cycle",
            "revision",
            "outcome",
            "acceptance",
            "closing",
            "closed_at",
            "closed_reason"
        ]
    );
    assert_eq!(items[33], "design_gate");
    assert_eq!(&sessions[21..], ["reasoning_effort", "first_tool_at"]);
    schema::verify_baseline(&c).unwrap();
    c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0); INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('owner','w','fixture','fixture','owner',0);").unwrap();
    // Open the old facade BEFORE conversion: reopening it afterward would replay the
    // old runner against deleted version tables. Reads below use its actual from_row.
    let store = steve_store::Store::open(&path, steve_store::NoPublisher).unwrap();
    let before = physical_tables(&c);
    assert!(matches!(
        convert::convert(&mut c).unwrap(),
        Outcome::Converted {
            deleted_tokens: 0,
            deleted_clients: 0,
            deleted_sessions: 0,
            ..
        }
    ));
    migrate::check_versions(&c, true).unwrap();
    assert_eq!(physical_tables(&c), before);
    assert_eq!(columns(&c, "items"), items);
    assert_eq!(columns(&c, "sessions"), sessions);
    c.execute_batch("INSERT INTO items(id,workspace_id,no,kind,title,status,stage,plan,now,priority,archived,created,updated,star,design_gate,phase,cycle,revision,outcome,acceptance,closing,closed_at,closed_reason)
        VALUES('i','w',23,'task','事项😀','done','closed','[]','read back','高',1,101,202,1,1,'closed',7,9,'completed','human','{\"fixture\":true}',303,'fixture reason');
        INSERT INTO sessions(id,workspace_id,item_id,no,harness,model,cwd,status,used,size,cost,title,created,updated,device_id,source,readonly,import_key,reasoning_effort,first_tool_at)
        VALUES('s','w','i',29,'fixture','fixture-model','/fixture','idle',31,41,1.25,'会话😀',404,505,'fixture-device','fixture-source',1,'fixture-import','high',606);").unwrap();
    let item = store.items().get("i").unwrap().unwrap();
    assert_eq!(
        (item.id.as_str(), item.workspace_id.as_str(), item.no),
        ("i", "w", 23)
    );
    assert_eq!(
        (
            item.title.as_str(),
            item.status.as_str(),
            item.stage.as_str()
        ),
        ("事项😀", "done", "closed")
    );
    assert_eq!(
        (
            item.now.as_str(),
            item.priority.as_str(),
            item.archived,
            item.star
        ),
        ("read back", "高", true, true)
    );
    assert_eq!((item.created, item.updated), (101, 202));
    assert!(item.design_gate);
    assert_eq!(
        (item.phase.as_str(), item.cycle, item.revision),
        ("closed", 7, 9)
    );
    assert_eq!(item.outcome.as_deref(), Some("completed"));
    assert_eq!(item.acceptance.as_deref(), Some("human"));
    assert_eq!(item.closing, Some(serde_json::json!({"fixture":true})));
    assert_eq!(item.closed_at, Some(303));
    assert_eq!(item.closed_reason.as_deref(), Some("fixture reason"));
    let session = store.sessions().get("s").unwrap().unwrap();
    assert_eq!(
        (
            session.id.as_str(),
            session.workspace_id.as_str(),
            session.no
        ),
        ("s", "w", 29)
    );
    assert_eq!(session.item_id.as_deref(), Some("i"));
    assert_eq!(
        (
            session.harness.as_str(),
            session.cwd.as_str(),
            session.status.as_str()
        ),
        ("fixture", "/fixture", "idle")
    );
    assert_eq!(session.model.as_deref(), Some("fixture-model"));
    assert_eq!(
        (session.used, session.size, session.cost),
        (Some(31), Some(41), Some(1.25))
    );
    assert_eq!(
        (session.title.as_str(), session.created, session.updated),
        ("会话😀", 404, 505)
    );
    assert_eq!(
        (
            session.device_id.as_str(),
            session.source.as_str(),
            session.readonly
        ),
        ("fixture-device", "fixture-source", 1)
    );
    assert_eq!(session.import_key.as_deref(), Some("fixture-import"));
    assert_eq!(session.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        c.query_row("SELECT first_tool_at FROM sessions WHERE id='s'", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        606
    );
    assert_eq!(convert::convert(&mut c).unwrap(), Outcome::AlreadyCurrent);
}

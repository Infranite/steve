//! 092: what the workflow statistics measure. On a 091 database the closing invalidations and pull
//! request merges the timeline recorded become `invalidate` and `merge` marks once (flagged
//! `backfill`), each invalidation with its reason code; a session's first tool call sets
//! `sessions.first_tool_at`, and later ones leave it.

mod common;

use common::fresh;
use rusqlite::Connection;
use serde_json::json;
use steve_store::{NewSession, WS, db};

#[test]
fn applies_on_a_091_database_and_turns_timeline_lines_into_marks() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(91);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..91] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "PRAGMA foreign_keys=OFF;
         INSERT INTO workspaces(id,name,created) VALUES ('w','w',1);
         INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','w',1,'t','run','执行',1,1);
         INSERT INTO timeline(item_id,ts,actor,kind,text) VALUES
           ('i1',10,'steve','sys','收尾作废：计划里又有没做完的步骤'),
           ('i1',11,'steve','sys','收尾作废：有新的交付，上一次交付作废'),
           ('i1',12,'steve','sys','收尾作废：冻结之后结果又变了：HEAD 变了，需要重新交付'),
           ('i1',13,'steve','sys','收尾作废：没见过的原因'),
           ('i1',14,'steve','sys','PR #7 已合并：https://example.com/o/r/pull/7'),
           ('i1',15,'me','me','收尾作废：计划里又有没做完的步骤');",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
    let marks: Vec<(i64, String, String, Option<String>, i64)> = c
        .prepare("SELECT ts, kind, to_v, ref, backfill FROM item_marks ORDER BY ts")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let inv = |ts: i64, code: &str| (ts, "invalidate".to_string(), code.to_string(), None, 1);
    assert_eq!(
        marks,
        [
            inv(10, "plan"),
            inv(11, "redeliver"),
            inv(12, "changed"),
            inv(13, "other"),
            (
                14,
                "merge".into(),
                "merged".into(),
                Some("https://example.com/o/r/pull/7".into()),
                1
            ),
        ]
    );
    let why: String = c
        .query_row(
            "SELECT json_extract(detail,'$.why') FROM item_marks WHERE ts = 10",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(why, "计划里又有没做完的步骤");
}

#[test]
fn the_first_tool_call_of_a_session_is_kept() {
    let t = fresh();
    let s = &t.store;
    let id = s
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            harness: "fake".into(),
            cwd: "/tmp".into(),
            title: "test".into(),
            token: steve_store::uid("token"),
            ..Default::default()
        })
        .unwrap()
        .id;
    let first = || -> Option<i64> {
        s.with_conn(|c| {
            Ok(c.query_row(
                "SELECT first_tool_at FROM sessions WHERE id = ?",
                [&id],
                |r| r.get(0),
            )?)
        })
        .unwrap()
    };
    s.events().add(&id, "user", json!({"text": "hi"})).unwrap();
    assert_eq!(first(), None, "only a tool call counts");
    s.events()
        .add(
            &id,
            "tool_call",
            json!({"toolCallId": "t1", "status": "pending"}),
        )
        .unwrap();
    let at = first().expect("set by the first tool call");
    let ts: i64 = s
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT ts FROM events WHERE session_id = ? AND kind = 'tool_call'",
                [&id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(at, ts, "the time of the event that recorded it");
    std::thread::sleep(std::time::Duration::from_millis(5));
    s.events()
        .add(
            &id,
            "tool_call",
            json!({"toolCallId": "t2", "status": "pending"}),
        )
        .unwrap();
    assert_eq!(first(), Some(at), "later calls leave it");
}

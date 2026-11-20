//! A session keeps the timestamp of its first tool call.

mod common;

use common::fresh;
use serde_json::json;
use steve_store::{NewSession, WS};

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

//! A Node acts only on its own work: events, tool calls and files about another Node's sessions are
//! refused at the guard, whatever the payload says.

mod node_harness;

use std::sync::Arc;
use std::time::Duration;

use node_harness::*;
use serde_json::json;
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;
use steve_node_wire::{RemoteFiles, RemoteSink, RemoteTools};

fn attempt_of(h: &Hub, session: &str) -> ExecutionRef {
    let s: String = h.q(&format!(
        "SELECT a.execution FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id='{session}'"
    ));
    serde_json::from_str(&s).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_cannot_speak_for_another() {
    let h = Hub::start().await;
    let (ida, a) = h.paired("u1").await;
    let (idb, b) = h.paired("u1").await;
    let fa = FakeNode::new(a.as_str());
    let fb = FakeNode::new(b.as_str());
    let la = dial(&h, &ida, &a, fa.clone(), 1).await.unwrap();
    let lb = dial(&h, &idb, &b, fb.clone(), 1).await.unwrap();
    fa.connect(Arc::new(RemoteSink(la.link.clone())));
    fb.connect(Arc::new(RemoteSink(lb.link.clone())));
    // A's session runs a turn on A
    h.session("sa");
    let rt = h.bind(&a, "sa");
    for _ in 0..250 {
        if h.q::<String>("SELECT phase FROM runtime_sessions WHERE session_id='sa'") == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rt.prompt("sa", "hello", "user", &[]).unwrap();
    until("A's attempt", 5, || {
        fa.log().iter().any(|(m, _)| *m == "start_turn")
    })
    .await;
    let exec = attempt_of(&h, "sa");
    let before: i64 = h.q("SELECT COUNT(*) FROM events");

    // B reports text for A's attempt: refused whole, nothing committed, audited
    let e = RemoteSink(lb.link.clone())
        .commit(EventBatch {
            events: vec![NodeEvent {
                schema: 1,
                id: EventId::new("forged_1").unwrap(),
                at: Timestamp(1),
                cursor: Watermark {
                    stream: StreamId::new("st_forged").unwrap(),
                    epoch: Epoch::new("e1").unwrap(),
                    seq: 1,
                },
                subject: Subject::Execution(exec.clone()),
                body: EventBody::TextDelta {
                    message: "m".into(),
                    block: "b".into(),
                    utf8_offset: 0,
                    text: "I am A".into(),
                },
            }],
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert_eq!(
        h.q::<i64>(&format!(
            "SELECT COUNT(*) FROM node_event_inbox WHERE node_id='{b}'"
        )),
        0
    );
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM events"), before);
    assert_eq!(
        h.q::<i64>(&format!(
            "SELECT COUNT(*) FROM node_audit WHERE node_id='{b}' AND what='foreign_event'"
        )),
        1
    );

    // B calls Steve's tools as A's attempt: refused, the service never sees it
    let r = RemoteTools(lb.link.clone())
        .call(ToolCall {
            id: CallId::new("c1").unwrap(),
            execution: exec.clone(),
            catalog: Revision::new("c1").unwrap(),
            name: "steve_update".into(),
            args: json!({"now": "x"}),
            deadline: Timestamp(i64::MAX / 2),
        })
        .await;
    assert!(
        matches!(&r, Err(e) if e.code == ErrorCode::Denied)
            || matches!(&r, Ok(ToolResult::Failed { error }) if error.code == ErrorCode::Denied),
        "{r:?}"
    );
    assert!(h.service.calls.lock().unwrap().is_empty());

    // B fetches a file as A's attempt: refused
    let e = RemoteFiles(lb.link.clone())
        .fetch(
            FileAccess::Execution(exec.clone()),
            ArtifactId::new("fil_x").unwrap(),
            0,
            100,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);

    // the Hub asks B about A's session: B knows nothing of it
    let e = h
        .fleet
        .remote(&b)
        .session(SessionId::new("sa").unwrap())
        .await
        .unwrap_err();
    assert_ne!(e.code, ErrorCode::Offline);
    // and A's own events still commit
    fa.emit(
        Subject::Execution(exec.clone()),
        EventBody::TextDelta {
            message: "m".into(),
            block: "b".into(),
            utf8_offset: 0,
            text: "real".into(),
        },
    )
    .await
    .unwrap();
    until("A's text", 5, || {
        h.q::<i64>(&format!(
            "SELECT COUNT(*) FROM node_event_inbox WHERE node_id='{a}'"
        )) > 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_cannot_dial_as_another() {
    let h = Hub::start().await;
    let (_ida, a) = h.paired("u1").await;
    let (idb, _b) = h.paired("u1").await;
    // B's key, A's id
    let fake = FakeNode::new(a.as_str());
    let e = dial(&h, &idb, &a, fake, 1).await.err().unwrap();
    assert_eq!(e.code, "denied");
}

/// Two coordinators on one store each lease only their own Node's commands.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_coordinators_never_cross() {
    let h = Hub::start().await;
    let (ida, a) = h.paired("u1").await;
    let (idb, b) = h.paired("u1").await;
    let fa = FakeNode::new(a.as_str());
    let fb = FakeNode::new(b.as_str());
    let la = dial(&h, &ida, &a, fa.clone(), 1).await.unwrap();
    let lb = dial(&h, &idb, &b, fb.clone(), 1).await.unwrap();
    fa.connect(Arc::new(RemoteSink(la.link.clone())));
    fb.connect(Arc::new(RemoteSink(lb.link.clone())));
    for i in 0..10 {
        h.session(&format!("sa{i}"));
        h.session(&format!("sb{i}"));
        h.bind(&a, &format!("sa{i}"));
        h.bind(&b, &format!("sb{i}"));
    }
    until("all prepared", 10, || {
        fa.log()
            .iter()
            .filter(|(m, _)| *m == "open_session")
            .count()
            == 10
            && fb
                .log()
                .iter()
                .filter(|(m, _)| *m == "open_session")
                .count()
                == 10
    })
    .await;
    for (_, op) in fa.log() {
        assert!(!op.as_str().contains("_sb"), "{op} went to A");
    }
}

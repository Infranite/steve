//! A Node acts only on its own work: events, tool calls and files about another Node's sessions are
//! refused at the guard, whatever the payload says.

mod node_harness;

use std::sync::Arc;

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
    // Welcome arrives before the Hub finishes installing its side of the link. Starting
    // the coordinator sooner can hit Offline and its five-second reconcile backoff.
    until("A's link installed", 15, || {
        h.fleet.remote(&a).generation() == Some(la.generation)
    })
    .await;
    h.session("sa");
    let rt = h.bind(&a, "sa");
    until("A's session ready", 15, || {
        h.q::<String>("SELECT phase FROM runtime_sessions WHERE session_id='sa'") == "ready"
    })
    .await;
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

    // L2: A itself, naming its own turn's attempt as a job's: no such job attempt, refused
    let job = ExecutionRef::Job {
        job: JobId::new("j_forged").unwrap(),
        attempt: exec.attempt().clone(),
    };
    let e = RemoteSink(la.link.clone())
        .commit(EventBatch {
            events: vec![NodeEvent {
                schema: 1,
                id: EventId::new("forged_job_1").unwrap(),
                at: Timestamp(1),
                cursor: Watermark {
                    stream: StreamId::new("st_forged_job").unwrap(),
                    epoch: Epoch::new("e1").unwrap(),
                    seq: 1,
                },
                subject: Subject::Execution(job),
                body: EventBody::TextDelta {
                    message: "m".into(),
                    block: "b".into(),
                    utf8_offset: 0,
                    text: "a job".into(),
                },
            }],
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);

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

/// A command the Hub sends a Node directly is in no ledger (a project directory's workspace for a
/// flow command, the merge queue or a push). The Node journals its receipt like any other: the
/// batch is taken, so the session events behind it are not held back, and nothing is audited.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receipt_of_a_command_outside_the_ledger_is_taken() {
    let h = Hub::start().await;
    let (ida, a) = h.paired("u1").await;
    let fa = FakeNode::new(a.as_str());
    let la = dial(&h, &ida, &a, fa.clone(), 1).await.unwrap();
    fa.connect(Arc::new(RemoteSink(la.link.clone())));
    until("A's link installed", 15, || {
        h.fleet.remote(&a).generation() == Some(la.generation)
    })
    .await;
    let op = OperationId::new("op_fprep_direct").unwrap();
    assert_eq!(
        h.q::<i64>("SELECT COUNT(*) FROM node_commands WHERE operation_id='op_fprep_direct'"),
        0
    );
    fa.emit(
        Subject::Operation {
            operation: op.clone(),
        },
        EventBody::OperationChanged {
            receipt: Receipt {
                operation: op,
                state: Observed::Finished {
                    outcome: Outcome::Succeeded,
                    result: None,
                    error: None,
                },
            },
        },
    )
    .await
    .unwrap();
    // a session opened after it comes up: its events are behind that receipt in A's journal
    h.session("sa");
    h.bind(&a, "sa");
    until("A's session ready", 15, || {
        h.q::<String>("SELECT phase FROM runtime_sessions WHERE session_id='sa'") == "ready"
    })
    .await;
    until("everything A journaled is taken", 15, || fa.unacked() == 0).await;
    assert_eq!(
        h.q::<i64>(&format!(
            "SELECT COUNT(*) FROM node_audit WHERE node_id='{a}' AND what='foreign_event'"
        )),
        0
    );
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

/// What a remote Node replays when the Hub pulls it (reconcile) passes the same guard as what it
/// pushes: B's journal holds a receipt of A's operation, B's coordinator reconciles and replays
/// it; the batch is refused and audited, nothing reaches the inbox or A's receipt, and the
/// reconcile still completes for B.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_cannot_replay_events_about_another() {
    let h = Hub::start().await;
    let (ida, a) = h.paired("u1").await;
    let (idb, b) = h.paired("u1").await;
    let fa = FakeNode::new(a.as_str());
    let la = dial(&h, &ida, &a, fa.clone(), 1).await.unwrap();
    fa.connect(Arc::new(RemoteSink(la.link.clone())));
    // Welcome arrives before the Hub finishes installing its side of the link. Starting
    // the coordinator sooner can hit Offline and its five-second reconcile backoff.
    until("A's link installed", 15, || {
        h.fleet.remote(&a).generation() == Some(la.generation)
    })
    .await;
    h.session("sa");
    let rt = h.bind(&a, "sa");
    until("A's session ready", 15, || {
        h.q::<String>("SELECT phase FROM runtime_sessions WHERE session_id='sa'") == "ready"
    })
    .await;
    rt.prompt("sa", "hello", "user", &[]).unwrap();
    until("A's attempt", 5, || {
        fa.log().iter().any(|(m, _)| *m == "start_turn")
    })
    .await;
    let a_op: String = h.q(&format!(
        "SELECT operation_id FROM node_commands WHERE node_id='{a}' AND method='start_turn'"
    ));
    // FakeNode's call log precedes the Hub committing the legitimate response. Freeze
    // that response first, so its normal arrival cannot look like B changed A's receipt.
    until("A's receipt committed", 15, || {
        h.q::<Option<String>>(&format!(
            "SELECT receipt FROM node_commands WHERE operation_id='{a_op}'"
        ))
        .is_some()
    })
    .await;
    let a_receipt: Option<String> = h.q(&format!(
        "SELECT receipt FROM node_commands WHERE operation_id='{a_op}'"
    ));

    // B's journal holds an event about A's operation, never pushed: only replay brings it
    let fb = FakeNode::new(b.as_str());
    let op = OperationId::new(a_op.clone()).unwrap();
    fb.emit(
        Subject::Operation {
            operation: op.clone(),
        },
        EventBody::OperationChanged {
            receipt: Receipt {
                operation: op,
                state: Observed::Unknown {
                    reason: "forged".into(),
                },
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(fb.unacked(), 1);
    let _lb = dial(&h, &idb, &b, fb.clone(), 1).await.unwrap();
    h.session("sb");
    let rb = h.bind(&b, "sb");
    until("B reconciled", 10, || rb.is_reconciled()).await;

    assert!(
        h.q::<i64>(&format!(
            "SELECT COUNT(*) FROM node_audit WHERE node_id='{b}' AND what='foreign_event' AND safe_detail LIKE 'replayed%'"
        )) >= 1,
        "audited"
    );
    assert_eq!(
        h.q::<i64>(&format!(
            "SELECT COUNT(*) FROM node_event_inbox WHERE node_id='{b}' AND payload LIKE '%forged%'"
        )),
        0,
        "nothing of it taken in"
    );
    assert_eq!(
        h.q::<Option<String>>(&format!(
            "SELECT receipt FROM node_commands WHERE operation_id='{a_op}'"
        )),
        a_receipt,
        "A's receipt untouched"
    );
}

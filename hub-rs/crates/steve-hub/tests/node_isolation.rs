//! A Node acts only on its own work: events, tool calls and files about another Node's sessions are
//! refused at the guard, whatever the payload says.

mod node_harness;

use std::sync::Arc;
use steve_runtime::testkit::eventually_sync;
use steve_store::testkit::hub::node_checks;

use node_harness::*;
use serde_json::json;
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;
use steve_node_wire::{RemoteFiles, RemoteSink, RemoteTools};

fn attempt_of(h: &Hub, session: &str) -> ExecutionRef {
    let s: String = node_checks::attempt_a_execution_by_session(&h.store, &session).unwrap();
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
    eventually_sync("A's link installed", || {
        h.fleet.remote(&a).generation() == Some(la.generation)
    })
    .await;
    h.session("sa");
    let rt = h.bind(&a, "sa");
    eventually_sync("A's session ready", || {
        node_checks::binding_phase_by_session(&h.store, "sa").unwrap() == "ready"
    })
    .await;
    rt.prompt("sa", "hello", "user", &[]).unwrap();
    eventually_sync("A's attempt", || {
        fa.log().iter().any(|(m, _)| *m == "start_turn")
    })
    .await;
    let exec = attempt_of(&h, "sa");
    let before: i64 = node_checks::event_count(&h.store).unwrap();

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
    assert_eq!(e.code, Code::Forbidden);
    assert_eq!(
        node_checks::stream_count_by_node_id(&h.store, b.as_str()).unwrap(),
        0
    );
    assert_eq!(node_checks::event_count(&h.store).unwrap(), before);
    assert_eq!(
        node_checks::node_audit_count_by_node_id_foreign_event(&h.store, b.as_str()).unwrap(),
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
    assert_eq!(e.code, Code::Forbidden);

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
        matches!(&r, Err(e) if e.code == Code::Forbidden)
            || matches!(&r, Ok(ToolResult::Failed { error }) if error.code == Code::Forbidden),
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
    assert_eq!(e.code, Code::Forbidden);

    // the Hub asks B about A's session: B knows nothing of it
    let e = h
        .fleet
        .remote(&b)
        .session(SessionId::new("sa").unwrap())
        .await
        .unwrap_err();
    assert_ne!(e.code, Code::Unavailable);
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
    eventually_sync("A's text", || {
        node_checks::event_count_by_source_node(&h.store, a.as_str()).unwrap() > 0
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
    eventually_sync("A's link installed", || {
        h.fleet.remote(&a).generation() == Some(la.generation)
    })
    .await;
    let op = OperationId::new("op_fprep_direct").unwrap();
    assert_eq!(
        node_checks::command_count_by_operation(&h.store, "op_fprep_direct").unwrap(),
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
    eventually_sync("A's session ready", || {
        node_checks::binding_phase_by_session(&h.store, "sa").unwrap() == "ready"
    })
    .await;
    eventually_sync("everything A journaled is taken", || fa.unacked() == 0).await;
    assert_eq!(
        node_checks::node_audit_count_by_node_id_foreign_event(&h.store, a.as_str()).unwrap(),
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
    eventually_sync("all prepared", || {
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
    eventually_sync("A's link installed", || {
        h.fleet.remote(&a).generation() == Some(la.generation)
    })
    .await;
    h.session("sa");
    let rt = h.bind(&a, "sa");
    eventually_sync("A's session ready", || {
        node_checks::binding_phase_by_session(&h.store, "sa").unwrap() == "ready"
    })
    .await;
    rt.prompt("sa", "hello", "user", &[]).unwrap();
    eventually_sync("A's attempt", || {
        fa.log().iter().any(|(m, _)| *m == "start_turn")
    })
    .await;
    let a_op: String =
        node_checks::command_operation_id_by_node_id_start_turn(&h.store, a.as_str()).unwrap();
    // FakeNode's call log precedes the Hub committing the legitimate response. Freeze
    // that response first, so its normal arrival cannot look like B changed A's receipt.
    eventually_sync("A's receipt committed", || {
        node_checks::command_receipt_by_operation(&h.store, &a_op)
            .unwrap()
            .is_some()
    })
    .await;
    let a_receipt: Option<String> =
        node_checks::command_receipt_by_operation(&h.store, &a_op).unwrap();

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
    eventually_sync("B reconciled", || rb.is_reconciled()).await;

    assert!(
        node_checks::foreign_event_audit_count(&h.store, b.as_str()).unwrap() >= 1,
        "audited"
    );
    assert_eq!(
        node_checks::event_count_by_source_node_matching_text(&h.store, b.as_str()).unwrap(),
        0,
        "nothing of it taken in"
    );
    assert_eq!(
        node_checks::command_receipt_by_operation(&h.store, &a_op).unwrap(),
        a_receipt,
        "A's receipt untouched"
    );
}

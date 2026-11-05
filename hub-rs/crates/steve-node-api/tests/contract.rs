//! The contract's rules, checked against the fake Node, and the wire shape of its types.

use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use steve_node_api::testkit::{FakeNode, RecordingSink, build::*};
use steve_node_api::*;

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
    let s = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&s).unwrap();
    assert_eq!(&back, v, "{s}");
}

fn op(s: &str) -> OperationId {
    OperationId::new(s).unwrap()
}

#[test]
fn types_survive_the_wire() {
    roundtrip(&cmd("op_1", turn("s_1", "t_1", "a_1", "你好")));
    roundtrip(&cmd("op_2", open("s_1", "hub")));
    let req = permission("p_1", exec_turn("s_1", "t_1", "a_1"), "git push");
    roundtrip(&req);
    roundtrip(&answer(&req, PermissionDecision::AllowOnce));
    roundtrip(&Receipt {
        operation: op("op_1"),
        state: Observed::Finished {
            outcome: Outcome::Succeeded,
            result: Some(OperationResult::Execution(done())),
            error: None,
        },
    });
    roundtrip(&Receipt {
        operation: op("op_1"),
        state: Observed::Waiting {
            on: vec![WaitRef::Permission(req.id.clone())],
        },
    });
    roundtrip(&EventBody::Usage {
        report: UsageReport {
            report_id: "u1".into(),
            model: Some("sonnet".into()),
            meter_epoch: Epoch::new("m1").unwrap(),
            scope: UsageScope::Background,
            measurement: Measurement::Delta,
            tokens: Tokens {
                input: Some(10),
                output: None,
                ..Default::default()
            },
            context_used: None,
            context_limit: None,
            cost: Some(Money {
                decimal: "0.012".into(),
                currency: "USD".into(),
            }),
        },
    });
    roundtrip(&ReadChunk {
        offset: 0,
        bytes: vec![0, 1, 254, 255],
        eof: true,
    });
}

#[test]
fn wire_names_are_steves() {
    let v = serde_json::to_value(EventBody::TextDelta {
        message: "m".into(),
        block: "b".into(),
        utf8_offset: 0,
        text: "x".into(),
    })
    .unwrap();
    assert_eq!(v["type"], "text_delta");
    let v = serde_json::to_value(Subject::Execution(exec_turn("s_1", "t_1", "a_1"))).unwrap();
    assert_eq!(v["of"], "execution");
    assert_eq!(v["type"], "turn");
    // unknown enum values never decode (so they can never mean "allow")
    assert!(serde_json::from_str::<PermissionDecision>("\"allow_always\"").is_err());
}

#[tokio::test]
async fn a_command_runs_once() {
    let n = FakeNode::new("hub");
    n.open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap();
    let c = cmd("op_t1", turn("s_1", "t_1", "a_1", "hi"));
    let first = n.start_turn(c.clone()).await.unwrap();
    assert_eq!(first.state, Observed::Running);
    // the same command again: the same receipt, nothing new runs
    assert_eq!(n.start_turn(c.clone()).await.unwrap(), first);
    assert_eq!(
        n.log().iter().filter(|(m, _)| *m == "start_turn").count(),
        1
    );
    // same id, other content: conflict
    let mut other = c.clone();
    other.body.run.prompt = vec![Part::Text {
        text: "else".into(),
    }];
    assert_eq!(
        n.start_turn(other).await.unwrap_err().code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn an_attempt_never_runs_twice() {
    let n = FakeNode::new("hub");
    n.open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap();
    n.start_turn(cmd("op_a", turn("s_1", "t_1", "a_1", "hi")))
        .await
        .unwrap();
    n.finish(&exec_turn("s_1", "t_1", "a_1"), done())
        .await
        .unwrap();
    // the same attempt under a new operation id is not a new run
    let e = n
        .start_turn(cmd("op_b", turn("s_1", "t_1", "a_1", "hi")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    // a new attempt (the Hub decided to retry) is
    n.start_turn(cmd("op_c", turn("s_1", "t_1", "a_2", "hi")))
        .await
        .unwrap();
}

#[tokio::test]
async fn one_attempt_per_session_at_a_time() {
    let n = FakeNode::new("hub");
    n.open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap();
    n.start_turn(cmd("op_a", turn("s_1", "t_1", "a_1", "one")))
        .await
        .unwrap();
    let e = n
        .start_turn(cmd("op_b", turn("s_1", "t_2", "a_2", "two")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    assert_eq!(e.retry, RetryClass::SameOperation);
    // a busy refusal is not remembered: the same command goes through once the session is free
    n.finish(&exec_turn("s_1", "t_1", "a_1"), done())
        .await
        .unwrap();
    n.start_turn(cmd("op_b", turn("s_1", "t_2", "a_2", "two")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_late_command_is_stale() {
    let n = FakeNode::new("hub");
    n.set_now(120_000);
    let e = n
        .open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
}

#[tokio::test]
async fn permission_answers_bind_to_the_request() {
    let n = FakeNode::new("hub");
    let sink = Arc::new(RecordingSink::default());
    n.connect(sink.clone());
    n.open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap();
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "push it")))
        .await
        .unwrap();
    let exec = exec_turn("s_1", "t_1", "a_1");
    let req = permission("p_1", exec.clone(), "git push");
    n.ask_permission(req.clone()).await.unwrap();
    assert!(matches!(
        n.receipt(&op("op_t")).unwrap().state,
        Observed::Waiting { .. }
    ));

    // an answer for another action does not apply
    let mut wrong = answer(&req, PermissionDecision::AllowOnce);
    wrong.action_digest = Digest::sha256(b"something else");
    let e = n.resolve_permission(cmd("op_w", wrong)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);

    n.resolve_permission(cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
        .await
        .unwrap();
    assert_eq!(n.receipt(&op("op_t")).unwrap().state, Observed::Running);
    // answering again under a new id: nobody waits anymore
    let e = n
        .resolve_permission(cmd("op_r2", answer(&req, PermissionDecision::Reject)))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    // the same answer resent is fine (same receipt)
    n.resolve_permission(cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_finished_attempt_expires_its_waits() {
    let n = FakeNode::new("hub");
    let sink = Arc::new(RecordingSink::default());
    n.connect(sink.clone());
    n.open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap();
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    let exec = exec_turn("s_1", "t_1", "a_1");
    n.ask_permission(permission("p_1", exec.clone(), "rm -rf build"))
        .await
        .unwrap();
    n.finish(&exec, done()).await.unwrap();
    let kinds: Vec<String> = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|e| {
            serde_json::to_value(&e.body).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "attempt_started",
            "permission_requested",
            "permission_expired",
            "attempt_ended"
        ]
    );
}

#[tokio::test]
async fn events_are_numbered_and_acknowledged() {
    let n = FakeNode::new("hub");
    n.open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap();
    // no Hub connected: events stay on the Node
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    let exec = exec_turn("s_1", "t_1", "a_1");
    for i in 0..3 {
        n.emit(
            Subject::Execution(exec.clone()),
            EventBody::TextDelta {
                message: "m1".into(),
                block: "b1".into(),
                utf8_offset: i,
                text: "a".into(),
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(n.unacked(), 4);
    let from = Watermark {
        stream: StreamId::new("session:s_1").unwrap(),
        epoch: Epoch::new("e1").unwrap(),
        seq: 0,
    };
    let batch = n.replay(from.clone(), 1 << 20).await.unwrap();
    let seqs: Vec<u64> = batch.events.iter().map(|e| e.cursor.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    // the Hub commits them (twice: the second time changes nothing) and acknowledges
    let sink = RecordingSink::default();
    let ack = sink.commit(batch.clone()).await.unwrap();
    assert_eq!(sink.commit(batch).await.unwrap(), ack);
    assert_eq!(sink.events.lock().unwrap().len(), 4);
    n.acknowledge(ack).await.unwrap();
    assert_eq!(n.unacked(), 0);
    // another epoch is a gap, not silently the same stream
    let mut other = from;
    other.epoch = Epoch::new("e2").unwrap();
    assert_eq!(
        n.replay(other, 1 << 20).await.unwrap_err().code,
        ErrorCode::Gap
    );
}

#[tokio::test]
async fn unknown_is_not_an_end() {
    let n = FakeNode::new("hub");
    n.open_session(cmd("op_open", open("s_1", "hub")))
        .await
        .unwrap();
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    n.lose_track(&exec_turn("s_1", "t_1", "a_1"));
    let r = n.operation(op("op_t")).await.unwrap();
    assert_eq!(r.state.tag(), StateTag::Unknown);
    assert!(!r.state.is_finished());
    // the session is still taken: nothing may start a second attempt on an unknown one
    let e = n
        .start_turn(cmd("op_t2", turn("s_1", "t_1", "a_2", "x")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
}

#[tokio::test]
async fn a_port_is_bound_to_its_node() {
    let n = FakeNode::new("mbp");
    let r = n
        .prepare_workspace(cmd(
            "op_w",
            Prepare {
                key: "ST-1".into(),
                kind: PrepareKind::Scratch,
            },
        ))
        .await
        .unwrap();
    let Observed::Finished {
        result: Some(OperationResult::Workspace(v)),
        ..
    } = r.state
    else {
        panic!("{r:?}")
    };
    assert_eq!(v.workspace.node.as_str(), "mbp");
    // preparing again with the same key gives the same workspace
    let again = n
        .prepare_workspace(cmd(
            "op_w2",
            Prepare {
                key: "ST-1".into(),
                kind: PrepareKind::Scratch,
            },
        ))
        .await
        .unwrap();
    assert!(
        matches!(again.state, Observed::Finished { result: Some(OperationResult::Workspace(ref w)), .. } if w.workspace == v.workspace)
    );
    let e = n.workspace(workspace("hub", "w_ST-1")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
}

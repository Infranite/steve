//! The contract's rules, checked against the fake Node, and the wire shape of its types.

use std::sync::{Arc, Barrier};

use serde::{Serialize, de::DeserializeOwned};
use steve_node_api::testkit::{FakeNode, Point, RecordingSink, build::*};
use steve_node_api::*;

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
    let s = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&s).unwrap();
    assert_eq!(&back, v, "{s}");
}

fn op(s: &str) -> OperationId {
    OperationId::new(s).unwrap()
}

fn connected(node: &str) -> (Arc<FakeNode>, Arc<RecordingSink>) {
    let n = FakeNode::new(node);
    let sink = Arc::new(RecordingSink::default());
    n.connect(sink.clone());
    (n, sink)
}

// ── wire shape ──

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
    roundtrip(&OperationResult::Exec(ExecResult {
        exit_code: Some(2),
        stdout: "x".into(),
        stderr: String::new(),
        truncated: false,
    }));
    roundtrip(&EventBody::Usage {
        report: UsageReport {
            report_id: "u1".into(),
            model: Some("sonnet".into()),
            meter_epoch: Epoch::new("m1").unwrap(),
            scope: UsageScope::Background,
            measurement: Measurement::Delta,
            tokens: Tokens {
                input: Some(u64::MAX),
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
    roundtrip(&FileRead {
        path: FileAddress::Root(RootPath {
            root: RootId::new("docs").unwrap(),
            rel: RelPath::new("a/b.md").unwrap(),
        }),
        expected: None,
        offset: 0,
        max: 10,
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
    // a malformed digest never decodes
    assert!(serde_json::from_str::<Digest>("\"x\"").is_err());
}

#[test]
fn digests_are_canonical_and_per_method() {
    let a: serde_json::Value =
        serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":[1,{"q":1,"p":2}]}}"#).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(r#"{"a":{"x":[1,{"p":2,"q":1}],"y":2},"b":1}"#).unwrap();
    assert_eq!(digest_of(&a).unwrap(), digest_of(&b).unwrap());
    let c = cmd("op_1", open("s_1", "hub"));
    assert_ne!(
        c.digest("open_session").unwrap(),
        c.digest("release_session").unwrap()
    );
}

// ── commands ──

#[tokio::test]
async fn a_command_runs_once() {
    let (n, sink) = connected("hub");
    ready(&n, "s_1").await;
    let c = cmd("op_t1", turn("s_1", "t_1", "a_1", "hi"));
    let first = n.start_turn(c.clone()).await.unwrap();
    assert_eq!(first.state, Observed::Running);
    // the same command again: the same receipt, nothing new runs, no second event
    assert_eq!(n.start_turn(c.clone()).await.unwrap(), first);
    assert_eq!(
        n.log().iter().filter(|(m, _)| *m == "start_turn").count(),
        1
    );
    assert_eq!(
        sink.kinds()
            .iter()
            .filter(|k| *k == "attempt_started")
            .count(),
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
    // same id for another method: conflict
    let e = n
        .release_session(cmd("op_t1", SessionId::new("s_1").unwrap()))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_resends_start_once() {
    let (n, sink) = connected("hub");
    ready(&n, "s_1").await;
    // both resends pass the door before either is recorded
    let gate = Arc::new(Barrier::new(2));
    let g = gate.clone();
    n.set_hook(move |p| {
        if p == Point::BeforeAccept("start_turn") {
            g.wait();
        }
    });
    let c = cmd("op_t1", turn("s_1", "t_1", "a_1", "hi"));
    let (a, b) = tokio::join!(
        tokio::spawn({
            let n = n.clone();
            let c = c.clone();
            async move { n.start_turn(c).await }
        }),
        tokio::spawn({
            let n = n.clone();
            let c = c.clone();
            async move { n.start_turn(c).await }
        })
    );
    assert_eq!(a.unwrap().unwrap(), b.unwrap().unwrap());
    assert_eq!(
        sink.kinds()
            .iter()
            .filter(|k| *k == "attempt_started")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_late_command_is_stale_but_a_known_one_reads_back() {
    let n = FakeNode::new("hub");
    let prep = cmd("op_p", prepare("item:x"));
    let r = n.prepare_workspace(prep.clone()).await.unwrap();
    n.set_now(120_000);
    assert_eq!(n.prepare_workspace(prep).await.unwrap(), r);
    let e = n
        .prepare_workspace(cmd("op_p2", prepare("item:y")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    // a window that ends before it begins is invalid
    let mut bad = cmd("op_p3", prepare("item:z"));
    bad.accept_before = Timestamp(10);
    assert_eq!(
        n.prepare_workspace(bad).await.unwrap_err().code,
        ErrorCode::Invalid
    );
}

// ── attempts and sessions ──

#[tokio::test]
async fn an_attempt_never_runs_twice() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_a", turn("s_1", "t_1", "a_1", "hi")))
        .await
        .unwrap();
    n.finish(&exec_turn("s_1", "t_1", "a_1"), done())
        .await
        .unwrap();
    let e = n
        .start_turn(cmd("op_b", turn("s_1", "t_1", "a_1", "hi")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    n.start_turn(cmd("op_c", turn("s_1", "t_1", "a_2", "hi")))
        .await
        .unwrap();
}

#[tokio::test]
async fn one_attempt_per_session_at_a_time() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_a", turn("s_1", "t_1", "a_1", "one")))
        .await
        .unwrap();
    let e = n
        .start_turn(cmd("op_b", turn("s_1", "t_2", "a_2", "two")))
        .await
        .unwrap_err();
    assert_eq!(
        (e.code, e.retry),
        (ErrorCode::Busy, RetryClass::SameOperation)
    );
    // a refusal is not remembered: the same command goes through once the session is free
    n.finish(&exec_turn("s_1", "t_1", "a_1"), done())
        .await
        .unwrap();
    n.start_turn(cmd("op_b", turn("s_1", "t_2", "a_2", "two")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_late_end_of_an_old_attempt_does_not_free_the_new_one() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    let a1 = exec_turn("s_1", "t_1", "a_1");
    n.start_turn(cmd("op_1", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    n.finish(&a1, done()).await.unwrap();
    n.start_turn(cmd("op_2", turn("s_1", "t_2", "a_2", "y")))
        .await
        .unwrap();
    // the old end again: a no-op; another end of it: conflict; neither frees a_2's session
    n.finish(&a1, done()).await.unwrap();
    let mut other = done();
    other.outcome = Outcome::Failed;
    assert_eq!(
        n.finish(&a1, other).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    let e = n
        .start_turn(cmd("op_3", turn("s_1", "t_3", "a_3", "z")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
}

#[tokio::test]
async fn facts_must_name_the_whole_execution() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    ready(&n, "s_2").await;
    n.start_turn(cmd("op_1", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    // the right attempt with the wrong session or turn is refused
    for wrong in [
        exec_turn("s_2", "t_1", "a_1"),
        exec_turn("s_1", "t_9", "a_1"),
    ] {
        assert_eq!(
            n.finish(&wrong, done()).await.unwrap_err().code,
            ErrorCode::Invalid
        );
        assert!(n.lose_track(&wrong).is_err());
        assert!(
            n.ask_permission(permission("p_x", wrong.clone(), "ls"))
                .await
                .is_err()
        );
    }
    // and nothing is said for an attempt that is over
    let a1 = exec_turn("s_1", "t_1", "a_1");
    n.finish(&a1, done()).await.unwrap();
    let e = n
        .ask_permission(permission("p_late", a1, "ls"))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
}

#[tokio::test]
async fn a_turn_runs_in_its_sessions_context() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    let mut scratch = turn("s_1", "t_1", "a_1", "x");
    scratch.run.directory = RunDirectory::Scratch;
    assert_eq!(
        n.start_turn(cmd("op_1", scratch)).await.unwrap_err().code,
        ErrorCode::Invalid
    );
    let mut harness = turn("s_1", "t_1", "a_1", "x");
    harness.run.choice.harness = "codex".into();
    assert!(n.start_turn(cmd("op_2", harness)).await.is_err());
    // released: open it again first
    n.release_session(cmd("op_rel", SessionId::new("s_1").unwrap()))
        .await
        .unwrap();
    let e = n
        .start_turn(cmd("op_3", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    n.open_session(cmd("op_reopen", open("s_1", "hub")))
        .await
        .unwrap();
    n.start_turn(cmd("op_3", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    // open while running: busy
    let e = n
        .open_session(cmd("op_again", open("s_1", "hub")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
}

#[tokio::test]
async fn configure_checks_the_revision() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    let v = n.session(SessionId::new("s_1").unwrap()).await.unwrap();
    let mut c = ConfigureSession {
        session: v.id.clone(),
        expected: Revision::new("stale").unwrap(),
        choice: choice(),
        policy: policy(),
    };
    assert_eq!(
        n.configure_session(cmd("op_c1", c.clone()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    c.expected = v.revision.clone();
    c.choice.model = Some("opus".into());
    n.configure_session(cmd("op_c2", c)).await.unwrap();
    let after = n.session(v.id.clone()).await.unwrap();
    assert_ne!(after.revision, v.revision);
    assert_eq!(after.effective.model.as_deref(), Some("opus"));
}

#[tokio::test]
async fn capacity_and_capabilities_are_enforced() {
    let (n, _) = connected("hub");
    n.set_capacity(Capacity {
        sessions: 1,
        jobs: 0,
        execs: 0,
    });
    ready(&n, "s_1").await;
    ready(&n, "s_2").await;
    n.start_turn(cmd("op_1", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    let e = n
        .start_turn(cmd("op_2", turn("s_2", "t_1", "a_2", "x")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Exhausted);
    let e = n
        .start_job(cmd("op_j", job("j_1", "a_j", "hi")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Exhausted);
    let mut sandboxed = job("j_2", "a_j2", "hi");
    sandboxed.run.policy.sandbox = SandboxRequirement::Required {
        guarantees: vec!["fs-readonly".into()],
    };
    n.set_capacity(Capacity {
        sessions: 9,
        jobs: 9,
        execs: 0,
    });
    assert_eq!(
        n.start_job(cmd("op_j2", sandboxed)).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    let mut late = job("j_3", "a_j3", "hi");
    late.run.limits.deadline = Timestamp(5);
    assert_eq!(
        n.start_job(cmd("op_j3", late)).await.unwrap_err().code,
        ErrorCode::Expired
    );
}

// ── permissions ──

#[tokio::test]
async fn permission_answers_bind_to_the_request() {
    let (n, sink) = connected("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "push it")))
        .await
        .unwrap();
    let exec = exec_turn("s_1", "t_1", "a_1");
    let req = permission("p_1", exec.clone(), "git push");
    n.ask_permission(req.clone()).await.unwrap();
    n.ask_permission(req.clone()).await.unwrap(); // the same request again: nothing new
    let mut other = req.clone();
    other.title = "another".into();
    assert_eq!(
        n.ask_permission(other).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    assert!(
        matches!(n.receipt(&op("op_t")).unwrap().state, Observed::Waiting { ref on } if on.len() == 1)
    );

    let mut wrong = answer(&req, PermissionDecision::AllowOnce);
    wrong.action_digest = Digest::sha256(b"something else");
    assert_eq!(
        n.resolve_permission(cmd("op_w", wrong))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Stale
    );

    n.resolve_permission(cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
        .await
        .unwrap();
    assert_eq!(n.receipt(&op("op_t")).unwrap().state, Observed::Running);
    // resent: same receipt, no second event
    n.resolve_permission(cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
        .await
        .unwrap();
    assert_eq!(
        sink.kinds()
            .iter()
            .filter(|k| *k == "permission_resolved")
            .count(),
        1
    );
    // another answer later: nobody waits anymore
    let e = n
        .resolve_permission(cmd("op_r2", answer(&req, PermissionDecision::Reject)))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
}

#[tokio::test]
async fn permission_answers_respect_expiry_and_choices() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    let exec = exec_turn("s_1", "t_1", "a_1");
    let mut only_reject = permission("p_1", exec.clone(), "rm -rf /tmp/x");
    only_reject.choices = vec![PermissionDecision::Reject];
    n.ask_permission(only_reject.clone()).await.unwrap();
    let e = n
        .resolve_permission(cmd(
            "op_a",
            answer(&only_reject, PermissionDecision::AllowOnce),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    let mut soon = permission("p_2", exec.clone(), "git push");
    soon.expires_at = Timestamp(2_000);
    n.ask_permission(soon.clone()).await.unwrap();
    n.set_now(3_000);
    let e = n
        .resolve_permission(cmd("op_b", answer(&soon, PermissionDecision::AllowOnce)))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Expired);
    // a request whose digest does not match its action is refused
    let mut forged = permission("p_3", exec, "ls");
    forged.preconditions = vec![Precondition::Head {
        workspace: workspace("hub", "item:s_1"),
        commit: "abc".into(),
    }];
    assert_eq!(
        n.ask_permission(forged).await.unwrap_err().code,
        ErrorCode::Invalid
    );
}

#[tokio::test]
async fn a_finished_attempt_expires_its_waits() {
    let (n, sink) = connected("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    let exec = exec_turn("s_1", "t_1", "a_1");
    n.ask_permission(permission("p_1", exec.clone(), "rm -rf build"))
        .await
        .unwrap();
    n.finish(&exec, done()).await.unwrap();
    let kinds: Vec<String> = sink
        .kinds()
        .into_iter()
        .filter(|k| k != "session_changed")
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

// ── events ──

#[tokio::test]
async fn events_are_numbered_and_acknowledged() {
    let n = FakeNode::new("hub");
    ready(&n, "s_1").await;
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
    // session_changed (open), attempt_started, three deltas
    let heads = n.streams().await.unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!((heads[0].produced, heads[0].acknowledged), (5, 0));
    let from = Watermark {
        stream: heads[0].stream.clone(),
        epoch: heads[0].epoch.clone(),
        seq: 0,
    };
    let batch = n.replay(from.clone(), 1 << 20).await.unwrap();
    let seqs: Vec<u64> = batch.events.iter().map(|e| e.cursor.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    // too small a budget for even one event is an error, not an empty answer
    assert_eq!(
        n.replay(from.clone(), 10).await.unwrap_err().code,
        ErrorCode::Exhausted
    );
    let sink = RecordingSink::default();
    let ack = sink.commit(batch.clone()).await.unwrap();
    assert_eq!(sink.commit(batch).await.unwrap(), ack);
    n.acknowledge(ack).await.unwrap();
    assert_eq!(n.unacked(), 0);
    // what was acknowledged is gone: asking for it is a gap, not an empty batch
    assert_eq!(
        n.replay(from.clone(), 1 << 20).await.unwrap_err().code,
        ErrorCode::Gap
    );
    let mut other = from.clone();
    other.epoch = Epoch::new("e2").unwrap();
    assert_eq!(
        n.replay(other, 1 << 20).await.unwrap_err().code,
        ErrorCode::Gap
    );
    // an ack ahead of what was produced is refused
    let mut ahead = from;
    ahead.seq = 99;
    assert!(
        n.acknowledge(EventAck {
            durable: vec![ahead]
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn the_sink_acknowledges_only_a_contiguous_prefix() {
    let n = FakeNode::new("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    let head = n.streams().await.unwrap().remove(0);
    let all = n
        .replay(
            Watermark {
                stream: head.stream.clone(),
                epoch: head.epoch.clone(),
                seq: 0,
            },
            1 << 20,
        )
        .await
        .unwrap()
        .events;
    let sink = RecordingSink::default();
    // seq 2 alone: nothing contiguous yet
    let ack = sink
        .commit(EventBatch {
            events: vec![all[1].clone()],
        })
        .await
        .unwrap();
    assert!(ack.durable.is_empty());
    // then seq 1: both
    let ack = sink
        .commit(EventBatch {
            events: vec![all[0].clone()],
        })
        .await
        .unwrap();
    assert_eq!(ack.durable[0].seq, 2);
    // another event at a taken position is a conflict
    let mut forged = all[0].clone();
    forged.id = EventId::new("ev_forged").unwrap();
    assert_eq!(
        sink.commit(EventBatch {
            events: vec![forged]
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn a_hub_that_cannot_commit_gets_the_events_later() {
    let (n, sink) = connected("hub");
    ready(&n, "s_1").await;
    sink.set_failing(true);
    // accepted even though the events could not be delivered
    let r = n
        .start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    assert_eq!(r.state, Observed::Running);
    assert_eq!(n.delivery_failures(), 1);
    sink.set_failing(false);
    let head = n.streams().await.unwrap().remove(0);
    let missed = n
        .replay(
            Watermark {
                stream: head.stream,
                epoch: head.epoch,
                seq: head.acknowledged,
            },
            1 << 20,
        )
        .await
        .unwrap();
    let ack = sink.commit(missed).await.unwrap();
    n.acknowledge(ack).await.unwrap();
    assert_eq!(n.unacked(), 0);
}

// ── unknown, cancel, workspaces ──

#[tokio::test]
async fn unknown_is_not_an_end() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    n.lose_track(&exec_turn("s_1", "t_1", "a_1")).unwrap();
    let r = n.operation(op("op_t")).await.unwrap();
    assert_eq!(r.state.tag(), StateTag::Unknown);
    assert!(!r.state.is_finished());
    let e = n
        .start_turn(cmd("op_t2", turn("s_1", "t_1", "a_2", "x")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    // nor can its workspace be tidied
    let e = n
        .tidy_workspace(cmd(
            "op_tidy",
            Tidy {
                workspace: workspace("hub", "item:s_1"),
                expected: Revision::new("r1").unwrap(),
                keep_branch: false,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
}

#[tokio::test]
async fn cancel_is_an_intent() {
    let (n, _) = connected("hub");
    ready(&n, "s_1").await;
    n.start_turn(cmd("op_t", turn("s_1", "t_1", "a_1", "x")))
        .await
        .unwrap();
    let r = n
        .cancel(cmd(
            "op_c",
            Cancel {
                target: op("op_t"),
                reason: "paused".into(),
            },
        ))
        .await
        .unwrap();
    assert!(r.state.is_finished());
    // the target goes on until it really ends (here: normally, before the cancel took effect)
    assert!(n.cancel_requested(&op("op_t")));
    assert_eq!(n.receipt(&op("op_t")).unwrap().state, Observed::Running);
    n.finish(&exec_turn("s_1", "t_1", "a_1"), done())
        .await
        .unwrap();
    let snap = n
        .snapshot(PageRequest {
            cursor: None,
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(snap.items.len(), 2);
    let rest = n
        .snapshot(PageRequest {
            cursor: snap.next.clone(),
            limit: 10,
        })
        .await
        .unwrap();
    assert!(rest.next.is_none());
    assert_eq!(snap.items.len() + rest.items.len(), n.log().len());
}

#[tokio::test]
async fn workspaces_are_bound_to_their_node_and_spec() {
    let n = FakeNode::new("mbp");
    let r = n
        .prepare_workspace(cmd("op_w", prepare("item:ST-1")))
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
    // the same resource and spec: the same workspace
    let again = n
        .prepare_workspace(cmd("op_w2", prepare("item:ST-1")))
        .await
        .unwrap();
    assert!(
        matches!(again.state, Observed::Finished { result: Some(OperationResult::Workspace(ref w)), .. } if w.workspace == v.workspace)
    );
    // the same resource otherwise: conflict
    let wt = Prepare {
        resource: "item:ST-1".into(),
        kind: PrepareKind::Worktree {
            repository: RootPath {
                root: RootId::new("code").unwrap(),
                rel: RelPath::new("steve").unwrap(),
            },
            branch: "steve/st-1".into(),
            base: None,
        },
    };
    assert_eq!(
        n.prepare_workspace(cmd("op_w3", wt))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    // any resource text works, none panics
    n.prepare_workspace(cmd("op_w4", prepare(&"中文 / x".repeat(40))))
        .await
        .unwrap();
    // another node's ids are refused everywhere
    let foreign = workspace("hub", "item:ST-1");
    assert_eq!(
        n.workspace(foreign.clone()).await.unwrap_err().code,
        ErrorCode::Denied
    );
    let e = n
        .tidy_workspace(cmd(
            "op_t",
            Tidy {
                workspace: foreign,
                expected: Revision::new("r1").unwrap(),
                keep_branch: false,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert_eq!(
        n.open_session(cmd("op_o", open("s_1", "hub")))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Denied
    );
}

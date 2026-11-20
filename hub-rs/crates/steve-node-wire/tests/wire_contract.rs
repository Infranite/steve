//! The same contract, directly against the fake Node and through the wire: receipts, errors and
//! events must come out identical.

mod common;

use std::sync::Arc;

use common::*;
use serde_json::json;
use steve_node_api::testkit::{FakeNode, RecordingSink, build::*};
use steve_node_api::*;
use steve_node_wire::RemoteSink;
use steve_node_wire::methods::Lane;

fn op(s: &str) -> OperationId {
    OperationId::new(s).unwrap()
}

fn show<T: serde::Serialize>(r: &ApiResult<T>) -> String {
    match r {
        Ok(v) => format!("ok {}", serde_json::to_string(v).unwrap()),
        Err(e) => format!("err {}", serde_json::to_string(e).unwrap()),
    }
}

/// Runs one scenario through `n` (the fake itself, or a RemoteNode in front of it) and returns
/// everything it answered.
pub async fn contract(n: Arc<dyn Node>, fake: &FakeNode) -> Vec<String> {
    let mut out = vec![];
    out.push(show(
        &n.prepare_workspace(cmd("op_prep", prepare("item:s_1")))
            .await,
    ));
    out.push(show(
        &n.open_session(cmd("op_open", open("s_1", "n1"))).await,
    ));
    let t1 = cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi"));
    out.push(show(&n.start_turn(t1.clone()).await));
    // a resend: the same receipt, nothing new runs
    out.push(show(&n.start_turn(t1.clone()).await));
    // another attempt while one runs: busy
    out.push(show(
        &n.start_turn(cmd("op_t2", turn_on("n1", "s_1", "t_1", "a_2", "hi")))
            .await,
    ));
    // the same attempt under another operation: conflict
    out.push(show(
        &n.start_turn(cmd("op_t3", turn_on("n1", "s_1", "t_1", "a_1", "hi")))
            .await,
    ));
    // past its window: stale
    let mut late = cmd("op_late", turn_on("n1", "s_1", "t_9", "a_9", "x"));
    late.issued_at = Timestamp(10);
    late.accept_before = Timestamp(20);
    out.push(show(&n.start_turn(late).await));
    out.push(show(&n.session(SessionId::new("s_1").unwrap()).await));
    out.push(show(&n.session(SessionId::new("nope").unwrap()).await));
    // a permission the harness raised, answered wrong then right
    let exec = exec_turn("s_1", "t_1", "a_1");
    let req = permission("p_1", exec.clone(), "git push");
    fake.ask_permission(req.clone()).await.unwrap();
    let mut wrong = answer(&req, PermissionDecision::AllowOnce);
    wrong.action_digest = Digest::sha256(b"else");
    out.push(show(&n.resolve_permission(cmd("op_w", wrong)).await));
    out.push(show(
        &n.resolve_permission(cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
            .await,
    ));
    out.push(show(&n.operation(op("op_t1")).await));
    out.push(show(&n.operation(op("op_never")).await));
    out.push(show(
        &n.snapshot(PageRequest {
            cursor: None,
            limit: 10,
        })
        .await,
    ));
    let heads = n.streams().await.unwrap();
    out.push(serde_json::to_string(&heads).unwrap());
    let from = Watermark {
        stream: heads[0].stream.clone(),
        epoch: heads[0].epoch.clone(),
        seq: heads[0].acknowledged,
    };
    let batch = n.replay(from.clone(), 1 << 20).await;
    out.push(show(&batch));
    out.push(show(&n.replay(from.clone(), 10).await));
    let sink = RecordingSink::default();
    let ack = match batch {
        Ok(b) if !b.events.is_empty() => sink.commit(b).await.unwrap(),
        _ => EventAck::default(),
    };
    out.push(show(&n.acknowledge(ack).await));
    let mut zero = from;
    zero.seq = 0;
    out.push(show(&n.replay(zero, 1 << 20).await));
    out.push(show(
        &n.cancel(cmd(
            "op_c",
            Cancel {
                target: op("op_t1"),
                reason: "user".into(),
            },
        ))
        .await,
    ));
    out.push(show(
        &n.history(PageRequest {
            cursor: None,
            limit: 5,
        })
        .await,
    ));
    fake.finish(&exec, done()).await.unwrap();
    out.push(show(&n.operation(op("op_t1")).await));
    out.push(show(
        &n.release_session(cmd("op_rel", SessionId::new("s_1").unwrap()))
            .await,
    ));
    out
}

#[tokio::test]
async fn the_wire_answers_exactly_like_the_node() {
    let direct = FakeNode::new("n1");
    let direct_sink = Arc::new(RecordingSink::default());
    direct.connect(direct_sink.clone());
    let a = contract(direct.clone(), &direct).await;

    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    fake.connect(Arc::new(RemoteSink(w.link.clone())));
    let b = contract(w.remote.clone(), &fake).await;
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x, y, "step {i}");
    }
    // the events went Node → Hub over the same link, identical
    assert_eq!(direct_sink.kinds(), w.sink.kinds());
    assert!(
        w.sink.kinds().contains(&"permission_requested".to_string()),
        "{:?}",
        w.sink.kinds()
    );
    assert_eq!(direct.log(), fake.log());
    // the digest the Node keeps is the one the Hub computed: the payload went verbatim
    let t1 = cmd("op_t1", turn_on("n1", "s_1", "t_1", "a_1", "hi"));
    let snap = fake
        .snapshot(PageRequest {
            cursor: None,
            limit: 50,
        })
        .await
        .unwrap();
    let e = snap
        .entries
        .iter()
        .find(|e| e.receipt.operation == op("op_t1"))
        .unwrap();
    assert_eq!(e.digest, t1.digest(&e.method).unwrap());
}

#[tokio::test]
async fn unknown_methods_are_unsupported_and_the_link_stays() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    let e = w
        .hub_conn
        .call_with(
            "frobnicate",
            json!({}),
            Lane::Control,
            std::time::Duration::from_secs(5),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    // a Node→Hub method sent to the Node is unknown there too
    let e = w
        .hub_conn
        .call("events.commit", json!({"events": []}), None)
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert!(w.remote.probe(false).await.is_ok(), "still up");
}

#[tokio::test]
async fn old_nodes_refuse_the_update_handshake_before_a_remote_upgrade_is_sent() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    assert_eq!(
        w.remote.harness_updates(false).await.unwrap_err().code,
        Code::Unsupported
    );
    let c = Command {
        id: op("optional_upgrade"),
        issued_at: Timestamp(1),
        accept_before: Timestamp(i64::MAX),
        grant: GrantId::new("owner").unwrap(),
        body: UpgradeHarness {
            harness: "fixture".into(),
            versions: std::collections::BTreeMap::from([("adapter".into(), "2.0.0".into())]),
        },
    };
    let error = w.remote.upgrade_harness(c).await.unwrap_err();
    assert_eq!(error.code, Code::Unsupported);
    assert!(error.safe_detail.contains("harness-updates-v1"));
    assert_eq!(
        fake.operation(op("optional_upgrade"))
            .await
            .unwrap_err()
            .code,
        Code::NotFound
    );
}

#[tokio::test]
async fn every_method_is_served() {
    // each table entry reaches its port: nothing answers "unknown method"
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    for s in steve_node_wire::methods::ALL {
        let conn = match s.dir {
            steve_node_wire::methods::Dir::HubToNode => &w.hub_conn,
            steve_node_wire::methods::Dir::NodeToHub => &w.node_conn,
        };
        let r = conn.call(s.name, json!(null), None).await;
        if let Err(e) = r {
            assert!(
                !e.safe_detail.contains("unknown method"),
                "{} is not served: {e}",
                s.name
            );
        }
    }
}

#[tokio::test]
async fn remote_capacity_for_jobs_and_execs_is_zero() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    let c = w.remote.probe(false).await.unwrap();
    assert_eq!((c.capacity.jobs, c.capacity.execs), (0, 0));
}

#[tokio::test]
async fn live_capacity_round_trips_and_reports_the_executor_value() {
    let fake = FakeNode::new("n1");
    fake.follow_session_capacity();
    let w = wire(fake.clone(), 1);
    for limit in [32, 64, 1, 256] {
        w.remote.set_session_capacity(limit).await.unwrap();
        w.remote.set_session_capacity(limit).await.unwrap(); // idempotent retry
        assert_eq!(fake.probe(false).await.unwrap().capacity.sessions, limit);
        assert_eq!(
            w.remote.probe(false).await.unwrap().capacity.sessions,
            limit
        );
    }
    assert_eq!(
        w.remote.set_session_capacity(0).await.unwrap_err().code,
        Code::Invalid
    );
    assert_eq!(w.remote.probe(false).await.unwrap().capacity.sessions, 256);
}

#[tokio::test]
async fn controlled_install_requires_a_colocated_current_node_and_serves_the_new_face() {
    let fake = FakeNode::new("n1");
    let w = wire(fake, 1);
    let command = cmd(
        "install_pi",
        InstallHarness {
            harness: "pi".into(),
        },
    );
    assert_eq!(
        w.remote
            .install_harness(command.clone())
            .await
            .unwrap_err()
            .code,
        Code::Forbidden
    );
    w.remote.set_colocated(true);
    // An up-to-date transport reaches the fake Node's default Unsupported face, rather than
    // sending arbitrary npm arguments or falling back to filesystem operations on the Hub.
    let error = w.remote.install_harness(command).await.unwrap_err();
    assert_eq!(error.code, Code::Unsupported);
    assert!(
        error
            .safe_detail
            .contains("controlled harness installation unsupported")
    );
}

#[tokio::test]
async fn component_integrity_and_other_installation_evidence_survive_the_wire() {
    let fake = FakeNode::new("n1");
    let mut harnesses = fake.probe(false).await.unwrap().harnesses;
    let component: HarnessComponent = serde_json::from_value(json!({
        "id":"adapter", "name":"ACP", "package":"example", "installed":false,
        "status":"unknown", "detected":{"path":"/opt/example/bin/agent","version":"1.2.3"}
    }))
    .unwrap();
    harnesses[0].setup = Some(HarnessSetup {
        state: SetupState::Ready,
        components: vec![component.clone()],
        can_install: false,
        reason: None,
        help: None,
        error: None,
    });
    fake.set_harnesses(harnesses);
    let w = wire(fake, 1);
    let result = w.remote.probe(false).await.unwrap();
    assert_eq!(
        result.harnesses[0].setup.as_ref().unwrap().components,
        vec![component]
    );
}

#[tokio::test]
async fn an_upgrade_is_staged_switched_and_asked_about_over_the_link() {
    let fake = FakeNode::new("n1");
    fake.set_build(Some(BuildIdentity {
        version: "1.0.0".into(),
        commit: "a".repeat(40),
        target: "aarch64-apple-darwin".into(),
    }));
    let w = wire(fake.clone(), 1);
    let op = OperationId::new("nup_1").unwrap();
    // a node that does not say it can is told so, nothing staged
    let ask = NodeUpgrade {
        operation: op.clone(),
        build: BuildIdentity {
            version: "2.0.0".into(),
            commit: "b".repeat(40),
            target: "aarch64-apple-darwin".into(),
        },
        artifact: ArtifactId::new("node-upgrade.nup_1").unwrap(),
        size: 10,
        sha256: "0".repeat(64),
        health_secs: 60,
    };
    assert_eq!(
        w.remote.upgrade_stage(ask.clone()).await.unwrap_err().code,
        Code::Unsupported
    );
    fake.set_upgradable(true);
    assert_eq!(
        w.remote.upgrade_stage(ask.clone()).await.unwrap().phase,
        "staged"
    );
    // the same operation again answers its state
    assert_eq!(w.remote.upgrade_stage(ask).await.unwrap().phase, "staged");
    let s = w
        .remote
        .upgrade_switch(UpgradeSwitch {
            operation: op.clone(),
            abandon: false,
            force: false,
        })
        .await
        .unwrap();
    assert_eq!(s.phase, "succeeded");
    assert_eq!(
        w.remote.upgrade_status(op).await.unwrap().phase,
        "succeeded"
    );
    assert_eq!(
        w.remote.probe_build().await.unwrap().build.unwrap().version,
        "2.0.0"
    );
}

/// G2: what machine a Node is crosses the link as the Node says it (`node.host`).
#[tokio::test]
async fn host_crosses_the_link() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    let h = w.remote.host().await.unwrap();
    assert_eq!(h, fake.host().await.unwrap());
    assert_eq!(h.hostname.as_deref(), Some("Studio Mac"));
    assert_eq!((h.os.as_str(), h.arch.as_str()), ("macos", "aarch64"));
}

/// A removal is begun, confirmed and asked about over the link (`node.remove*`); a Node that does
/// not say it can is told so and nothing is begun.
#[tokio::test]
async fn a_removal_is_begun_confirmed_and_asked_about_over_the_link() {
    let fake = FakeNode::new("n1");
    let w = wire(fake.clone(), 1);
    let op = OperationId::new("nrm_1").unwrap();
    let ask = NodeRemove {
        operation: op.clone(),
        grace_secs: 60,
    };
    assert_eq!(
        w.remote.remove_begin(ask.clone()).await.unwrap_err().code,
        Code::Unsupported
    );
    assert!(fake.removal_calls().is_empty());
    fake.set_features(vec![FEATURE_NODE_REMOVE.into()]);
    assert_eq!(
        w.remote.remove_begin(ask.clone()).await.unwrap().phase,
        "ready"
    );
    // the same operation again answers its state
    assert_eq!(w.remote.remove_begin(ask).await.unwrap().phase, "ready");
    let s = w
        .remote
        .remove_commit(RemoveCommit {
            operation: op.clone(),
            abandon: false,
        })
        .await
        .unwrap();
    assert_eq!(s.phase, "removing");
    assert_eq!(
        w.remote.remove_status(op.clone()).await.unwrap().phase,
        "removing"
    );
    assert_eq!(
        fake.removal_calls(),
        vec![("begin", op.clone()), ("commit", op.clone())]
    );
}

#[tokio::test]
async fn an_actual_ordinary_error_frame_matches_the_protocol_11_sample() {
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use steve_node_wire::{Conn, ConnOptions, Envelope, Frame, Kind, NodeServer};

    assert_eq!(
        PROTOCOL, 11,
        "ordinary wire changes require a new versioned sample"
    );
    let (server, mut peer) = steve_node_wire::testkit::pair();
    let node = FakeNode::new("n1");
    let conn = Conn::spawn(
        server.sink,
        server.stream,
        7,
        NodeServer::new(node),
        ConnOptions {
            side: "golden",
            heartbeat: Duration::from_secs(600),
            dead_after: Duration::from_secs(600),
            ..Default::default()
        },
    );
    let request = Envelope {
        v: PROTOCOL,
        t: Kind::Req,
        id: "request_missing".into(),
        re: None,
        m: steve_node_wire::methods::OP_INSPECT.into(),
        generation: 7,
        trace: None,
        deadline_at: 0,
        p: json!("missing_operation"),
    };
    peer.sink
        .send(Frame::Text(request.encode().unwrap()))
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), peer.stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Frame::Text(text) = frame else {
        panic!("expected an ordinary response")
    };
    assert_eq!(
        text,
        include_str!("fixtures/ordinary/11-missing-operation.json").trim_end()
    );
    conn.close(None, 1000, "test complete");
}

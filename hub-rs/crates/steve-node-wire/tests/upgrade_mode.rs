mod common;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use steve_node_api::testkit::{FakeNode, RecordingSink};
use steve_node_api::upgrade_v1 as v1;
use steve_node_api::*;
use steve_node_wire::{
    Conn, ConnOptions, Envelope, Frame, Handler, HubLink, Kind, NodeServer, RemoteFiles,
    RemoteNode, ServeHub,
};

fn options(side: &'static str) -> ConnOptions {
    ConnOptions {
        side,
        mode: v1::Mode::Upgrade,
        heartbeat: Duration::from_secs(600),
        dead_after: Duration::from_secs(600),
    }
}
fn build(version: &str) -> BuildIdentity {
    BuildIdentity {
        version: version.into(),
        commit: "ab".repeat(20),
        target: "test-target".into(),
    }
}
fn request(version: &str) -> NodeUpgrade {
    NodeUpgrade {
        operation: OperationId::new("op_upgrade").unwrap(),
        build: build(version),
        artifact: ArtifactId::new("node-upgrade.op_upgrade").unwrap(),
        size: 7,
        sha256: "cd".repeat(32),
        health_secs: 1,
    }
}

#[derive(Default)]
struct Count {
    normal: AtomicUsize,
    controls: AtomicUsize,
}
#[async_trait]
impl Handler for Count {
    async fn handle(&self, _: &str, _: Value) -> ApiResult<Value> {
        self.normal.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }
    fn control(&self, _: &str, _: &Value) {
        self.controls.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn upgrade_rejects_every_ordinary_method_before_decode_or_execution_and_keeps_the_link() {
    let (hub, mut node) = steve_node_wire::testkit::pair();
    let count = Arc::new(Count::default());
    let connection = Conn::spawn(hub.sink, hub.stream, 7, count.clone(), options("h"));
    let mut names: Vec<_> = steve_node_wire::methods::ALL
        .iter()
        .map(|m| m.name)
        .filter(|m| !v1::is_method(m))
        .collect();
    names.push("a.future.method");
    for version in [v1::UPGRADE_PROTOCOL, PROTOCOL, PROTOCOL + 1] {
        for method in &names {
            let request = Envelope {
                v: version,
                t: Kind::Req,
                id: "raw".into(),
                re: None,
                m: (*method).into(),
                generation: 7,
                trace: None,
                deadline_at: 0,
                p: json!("must not decode this as a Command or NodeEvent"),
            };
            node.sink
                .send(Frame::Text(request.encode().unwrap()))
                .await
                .unwrap();
            let Some(Ok(Frame::Text(text))) = node.stream.next().await else {
                panic!("{method}: connection closed")
            };
            let response = Envelope::decode(&text).unwrap();
            assert_eq!(response.v, v1::UPGRADE_PROTOCOL);
            let v1::Outcome::Err(error) = serde_json::from_value(response.p).unwrap() else {
                panic!("{method} was executed")
            };
            assert_eq!(
                (error.code, error.retry),
                (v1::Code::Incompatible, v1::Retry::Never),
                "{method}"
            );
        }
    }
    let paused = Envelope::ctl("paused", "paused", 7, json!({"paused":false}));
    node.sink
        .send(Frame::Text(paused.encode().unwrap()))
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(count.normal.load(Ordering::SeqCst), 0);
    assert_eq!(count.controls.load(Ordering::SeqCst), 0);
    assert!(!connection.is_closed());
}

struct Archive {
    reads: AtomicUsize,
}
#[async_trait]
impl HubFiles for Archive {
    async fn fetch(
        &self,
        access: FileAccess,
        id: ArtifactId,
        offset: u64,
        max: u32,
    ) -> ApiResult<ReadChunk> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if !matches!(access, FileAccess::Operation { operation } if operation.as_str() == "op_upgrade")
            || id.as_str() != "node-upgrade.op_upgrade"
        {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "not granted to this operation",
            ));
        }
        assert!(max <= v1::CHUNK_BYTES);
        Ok(ReadChunk {
            offset,
            bytes: b"archive".to_vec(),
            eof: true,
        })
    }
    async fn begin_upload(&self, _: BeginUpload) -> ApiResult<Transfer> {
        panic!("upgrade must not upload")
    }
    async fn upload(&self, _: BlobChunk) -> ApiResult<Transfer> {
        panic!("upgrade must not upload")
    }
    async fn upload_status(&self, _: TransferId) -> ApiResult<TransferState> {
        panic!("upgrade must not upload")
    }
    async fn finish_upload(&self, _: TransferId) -> ApiResult<ArtifactRef> {
        panic!("upgrade must not upload")
    }
    async fn abort_upload(&self, _: TransferId) -> ApiResult<TransferState> {
        panic!("upgrade must not upload")
    }
}

fn wired(
    node: Arc<FakeNode>,
) -> (
    Arc<RemoteNode>,
    Arc<Conn>,
    Arc<Conn>,
    RemoteFiles,
    Arc<Archive>,
) {
    let (hub, peer) = steve_node_wire::testkit::pair();
    let archive = Arc::new(Archive {
        reads: AtomicUsize::new(0),
    });
    let hub_connection = Conn::spawn(
        hub.sink,
        hub.stream,
        7,
        Arc::new(ServeHub {
            sink: Arc::new(RecordingSink::default()),
            tools: Arc::new(common::StubTools::default()),
            files: Some(archive.clone()),
            secrets: None,
        }),
        options("h"),
    );
    let node_connection = Conn::spawn(
        peer.sink,
        peer.stream,
        7,
        NodeServer::new(node.clone()),
        options("n"),
    );
    let remote = RemoteNode::new(node.id().clone());
    remote.attach(hub_connection.clone(), false);
    let link = HubLink::new();
    link.set(Some(node_connection.clone()));
    (
        remote,
        hub_connection,
        node_connection,
        RemoteFiles(link),
        archive,
    )
}

#[tokio::test]
async fn all_five_methods_work_without_ordinary_protocol_payloads() {
    let node = FakeNode::new("n1");
    node.set_upgradable(true);
    node.set_build(Some(build("1.9.0")));
    let (remote, hub, peer, files, archive) = wired(node);
    assert!(remote.is_upgrade());
    assert!(remote.ready_generation().is_none());
    assert_eq!(
        remote.probe(false).await.unwrap_err().retry,
        RetryClass::SameOperation
    );
    let probe = remote.probe_build().await.unwrap();
    assert_eq!(probe.build.unwrap().version, "1.9.0");
    assert!(probe.upgradable);
    assert_eq!(
        remote.upgrade_stage(request("2.0.0")).await.unwrap().phase,
        "staged"
    );
    let access = FileAccess::Operation {
        operation: OperationId::new("op_upgrade").unwrap(),
    };
    let data = files
        .fetch(
            access,
            ArtifactId::new("node-upgrade.op_upgrade").unwrap(),
            0,
            1024,
        )
        .await
        .unwrap();
    assert_eq!(data.bytes, b"archive");
    for payload in [
        json!({"access":{"for":"execution","session":"s1"},"id":"node-upgrade.op_upgrade","offset":0,"max":1024}),
        json!({"access":{"for":"operation","operation":"op_upgrade"},"id":"ordinary-file","offset":0,"max":1024}),
        json!({"access":{"for":"operation","operation":"other"},"id":"node-upgrade.op_upgrade","offset":0,"max":1024}),
    ] {
        assert!(
            peer.call_upgrade(v1::BLOB_FETCH, payload, None)
                .await
                .is_err()
        );
    }
    assert_eq!(
        archive.reads.load(Ordering::SeqCst),
        1,
        "invalid scopes never reach the file port"
    );
    let operation = OperationId::new("op_upgrade").unwrap();
    assert_eq!(
        remote
            .upgrade_status(operation.clone())
            .await
            .unwrap()
            .phase,
        "staged"
    );
    assert_eq!(
        remote
            .upgrade_switch(UpgradeSwitch {
                operation: operation.clone(),
                abandon: false,
                force: false
            })
            .await
            .unwrap()
            .phase,
        "succeeded"
    );
    assert_eq!(
        remote.upgrade_status(operation).await.unwrap().phase,
        "succeeded"
    );
    assert!(!hub.is_closed() && !peer.is_closed());
}

#[tokio::test]
async fn downgrade_returns_the_permanent_error_and_never_switches_even_when_forced() {
    let node = FakeNode::new("n1");
    node.set_upgradable(true);
    node.set_build(Some(build("1.10.0")));
    let (remote, hub, _peer, _files, _archive) = wired(node.clone());
    remote.upgrade_stage(request("1.9.0")).await.unwrap();
    for force in [false, true] {
        let error = hub
            .call_upgrade(
                v1::UPGRADE_SWITCH,
                json!({"operation":"op_upgrade","abandon":false,"force":force}),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            (error.code, error.retry),
            (v1::Code::Downgrade, v1::Retry::Never)
        );
        assert_eq!(
            remote
                .upgrade_status(OperationId::new("op_upgrade").unwrap())
                .await
                .unwrap()
                .phase,
            "staged"
        );
        assert_eq!(
            node.probe_build().await.unwrap().build.unwrap().version,
            "1.10.0"
        );
    }
}

#[tokio::test]
async fn ordinary_calls_remain_retryable_in_both_directions_without_writing_frames() {
    use steve_node_wire::{
        RemoteTools,
        methods::{self, Lane},
    };
    let node = FakeNode::new("n1");
    let (remote, hub, peer, files, _archive) = wired(node.clone());
    let op = OperationId::new("op_pending").unwrap();
    let before = (hub.frames_written(), peer.frames_written());
    for connection in [&hub, &peer] {
        for method in methods::ALL {
            for error in [
                connection
                    .call(method.name, json!({}), Some(&op))
                    .await
                    .unwrap_err(),
                connection
                    .call_with(
                        method.name,
                        json!({}),
                        Lane::Control,
                        Duration::from_secs(1),
                        Some(&op),
                    )
                    .await
                    .unwrap_err(),
            ] {
                assert_eq!(
                    (error.code, error.retry),
                    (ErrorCode::Offline, RetryClass::SameOperation)
                );
            }
        }
    }
    // Both the new-work and control/read RemoteNode paths must use the ordinary gate.
    for error in [
        remote
            .start_turn(steve_node_api::testkit::build::cmd(
                "op_pending",
                steve_node_api::testkit::build::turn_on("n1", "s1", "t1", "a1", "queued"),
            ))
            .await
            .unwrap_err(),
        remote
            .cancel(steve_node_api::testkit::build::cmd(
                "op_cancel",
                Cancel {
                    target: op.clone(),
                    reason: "stop".into(),
                },
            ))
            .await
            .unwrap_err(),
        remote
            .release_session(steve_node_api::testkit::build::cmd(
                "op_release",
                SessionId::new("s1").unwrap(),
            ))
            .await
            .unwrap_err(),
        remote.probe(false).await.unwrap_err(),
    ] {
        assert_eq!(
            (error.code, error.retry),
            (ErrorCode::Offline, RetryClass::SameOperation)
        );
    }
    // A running turn's Steve tool port must also refuse before using the upgrade socket.
    let link = files.0.clone();
    let execution = ExecutionRef::Turn {
        session: SessionId::new("s1").unwrap(),
        turn: TurnId::new("t1").unwrap(),
        attempt: AttemptId::new("a1").unwrap(),
    };
    let tools = RemoteTools(link);
    let error = tools.catalog(execution.clone()).await.unwrap_err();
    assert_eq!(
        (error.code, error.retry),
        (ErrorCode::Offline, RetryClass::SameOperation)
    );
    let error = tools
        .call(ToolCall {
            id: CallId::new("call_running").unwrap(),
            execution,
            catalog: Revision::new("catalog_1").unwrap(),
            name: "steve_note".into(),
            args: json!({}),
            deadline: Timestamp(0),
        })
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.retry),
        (ErrorCode::Offline, RetryClass::SameOperation)
    );
    assert_eq!((hub.frames_written(), peer.frames_written()), before);
    assert!(node.log().is_empty());
}

#[tokio::test]
async fn ordinary_blob_fetch_cannot_reach_the_upgrade_archive_port_even_on_a_normal_link() {
    let (h, n) = steve_node_wire::testkit::pair();
    let archive = Arc::new(Archive {
        reads: AtomicUsize::new(0),
    });
    let hub = Conn::spawn(
        h.sink,
        h.stream,
        1,
        Arc::new(ServeHub {
            sink: Arc::new(RecordingSink::default()),
            tools: Arc::new(common::StubTools::default()),
            files: Some(archive.clone()),
            secrets: None,
        }),
        ConnOptions::default(),
    );
    let peer = Conn::spawn(
        n.sink,
        n.stream,
        1,
        Arc::new(Count::default()),
        ConnOptions::default(),
    );
    let payload = json!({"access":{"for":"operation","operation":"op_upgrade"},
        "id":"node-upgrade.op_upgrade","offset":0,"max":1024});
    let error = peer
        .call("blob.fetch", payload.clone(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
    assert_eq!(archive.reads.load(Ordering::SeqCst), 0);
    peer.call_upgrade(v1::BLOB_FETCH, payload, None)
        .await
        .unwrap();
    assert_eq!(archive.reads.load(Ordering::SeqCst), 1);
    assert!(!hub.is_closed());
}

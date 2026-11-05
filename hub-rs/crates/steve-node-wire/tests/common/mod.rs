#![allow(dead_code)]
//! A Hub and a Node joined by an in-memory link: `RemoteNode → link → NodeServer(node)` one way,
//! `RemoteSink/Tools → link → ServeHub(sink, tools)` the other.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use steve_node_api::testkit::RecordingSink;
use steve_node_api::*;
use steve_node_wire::testkit::{End, Faults, pair};
use steve_node_wire::*;

pub struct Wired {
    pub remote: Arc<RemoteNode>,
    pub hub_conn: Arc<Conn>,
    pub node_conn: Arc<Conn>,
    pub link: Arc<HubLink>,
    pub sink: Arc<RecordingSink>,
    pub tools: Arc<StubTools>,
    /// Faults of what the Hub writes / what the Node writes.
    pub hub_faults: Arc<Faults>,
    pub node_faults: Arc<Faults>,
}

pub fn opts(side: &'static str) -> ConnOptions {
    ConnOptions {
        side,
        ..Default::default()
    }
}

/// Joins `node` to a fresh Hub side at `generation`.
pub fn wire(node: Arc<dyn Node>, generation: u64) -> Wired {
    let (h, n): (End, End) = pair();
    let sink = Arc::new(RecordingSink::default());
    let tools = Arc::new(StubTools::default());
    let hub_conn = Conn::spawn(
        h.sink,
        h.stream,
        generation,
        Arc::new(ServeHub {
            sink: sink.clone(),
            tools: tools.clone(),
            files: None,
        }),
        opts("h"),
    );
    let node_conn = Conn::spawn(
        n.sink,
        n.stream,
        generation,
        NodeServer::new(node.clone()),
        opts("n"),
    );
    let remote = RemoteNode::new(node.id().clone());
    remote.attach(hub_conn.clone(), false);
    let link = HubLink::new();
    link.set(Some(node_conn.clone()));
    Wired {
        remote,
        hub_conn,
        node_conn,
        link,
        sink,
        tools,
        hub_faults: h.faults,
        node_faults: n.faults,
    }
}

/// Answers every call `completed`, and counts them.
#[derive(Default)]
pub struct StubTools {
    pub calls: Mutex<Vec<ToolCall>>,
    /// Hold every call this long before answering.
    pub delay_ms: Mutex<u64>,
}

#[async_trait]
impl HubTools for StubTools {
    async fn catalog(&self, _: ExecutionRef) -> ApiResult<Catalog> {
        Ok(Catalog {
            revision: Revision::new("c1").unwrap(),
            tools: vec![],
        })
    }
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        self.calls.lock().unwrap().push(q.clone());
        let d = *self.delay_ms.lock().unwrap();
        if d > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(d)).await;
        }
        Ok(ToolResult::Completed {
            parts: vec![Part::Text {
                text: format!("ran {}", q.name),
            }],
            structured: None,
            end_turn: false,
        })
    }
    async fn call_status(&self, key: ToolKey) -> ApiResult<ToolResult> {
        let found = self
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.key() == key)
            .cloned();
        match found {
            Some(q) => Ok(ToolResult::Completed {
                parts: vec![Part::Text {
                    text: format!("ran {}", q.name),
                }],
                structured: None,
                end_turn: false,
            }),
            None => Err(ApiError::new(ErrorCode::Invalid, "no such call")),
        }
    }
    async fn cancel_call(&self, _: ToolKey) -> ApiResult<ToolResult> {
        Err(ApiError::new(ErrorCode::Unsupported, "no"))
    }
}

pub async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

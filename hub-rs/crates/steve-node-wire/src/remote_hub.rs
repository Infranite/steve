//! The Node's handles on the Hub (events, Steve's tools, blobs), over whatever link the Node has
//! right now. The Node connects them once ([`steve_node_api`] ports); the dial loop swaps the link
//! underneath on every reconnect. Without a link a call is `offline`: the Node's journal keeps the
//! events and delivers them again, deduplicated by the Hub per (node, stream, epoch, seq).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use steve_node_api::*;

use crate::conn::{Conn, from_value, to_value};
use crate::envelope::BULK_CAP;
use crate::methods::{self, *};

#[derive(Default)]
pub struct HubLink {
    slot: Mutex<Option<Arc<Conn>>>,
}

impl HubLink {
    pub fn new() -> Arc<HubLink> {
        Arc::new(HubLink::default())
    }

    pub fn set(&self, conn: Option<Arc<Conn>>) {
        let old = std::mem::replace(&mut *self.slot.lock().unwrap(), conn);
        drop(old);
    }

    pub fn current(&self) -> Option<Arc<Conn>> {
        self.slot.lock().unwrap().clone()
    }

    fn conn(&self) -> ApiResult<Arc<Conn>> {
        match self.current() {
            Some(c) if !c.is_closed() => Ok(c),
            _ => Err(ApiError::new(ErrorCode::Offline, "hub not connected")),
        }
    }

    async fn go<A: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        args: &A,
        op: Option<&OperationId>,
    ) -> ApiResult<R> {
        let v = self.conn()?.call(method, to_value(args)?, op).await?;
        from_value(v)
    }
}

pub struct RemoteSink(pub Arc<HubLink>);
pub struct RemoteTools(pub Arc<HubLink>);
pub struct RemoteFiles(pub Arc<HubLink>);

/// Cuts a batch into contiguous runs whose encoded events stay within [`BULK_CAP`]. A single event
/// over the cap goes alone (and is refused by the frame limit if it really is too big).
pub fn split(batch: EventBatch) -> Vec<EventBatch> {
    let mut out = vec![];
    let mut cur = EventBatch::default();
    let mut size = 0usize;
    for e in batch.events {
        let n = serde_json::to_string(&e).map(|s| s.len()).unwrap_or(0) + 1;
        if !cur.events.is_empty() && size + n > BULK_CAP {
            out.push(std::mem::take(&mut cur));
            size = 0;
        }
        size += n;
        cur.events.push(e);
    }
    if !cur.events.is_empty() {
        out.push(cur);
    }
    out
}

#[async_trait]
impl EventSink for RemoteSink {
    /// Answered once the Hub committed. A batch over the cap goes as several, in order; the acks
    /// are merged (the last watermark of each stream wins, the runs being contiguous).
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        let mut last: HashMap<(StreamId, Epoch), Watermark> = HashMap::new();
        let mut order = vec![];
        for part in split(batch) {
            let ack: EventAck = self.0.go(methods::EVENTS_COMMIT, &part, None).await?;
            for w in ack.durable {
                let k = (w.stream.clone(), w.epoch.clone());
                if !last.contains_key(&k) {
                    order.push(k.clone());
                }
                last.insert(k, w);
            }
        }
        Ok(EventAck {
            durable: order.into_iter().filter_map(|k| last.remove(&k)).collect(),
        })
    }
}

/// The operation a lost tool call is asked about under.
fn tool_op(id: &CallId) -> ApiResult<OperationId> {
    OperationId::new(derived_id("tool", id.as_str()))
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("{e:?}")))
}

#[async_trait]
impl HubTools for RemoteTools {
    async fn catalog(&self, execution: ExecutionRef) -> ApiResult<Catalog> {
        self.0.go(TOOL_CATALOG, &execution, None).await
    }
    /// Waits as long as the call's own deadline (plus 5 s). Lost after it was sent, it answers
    /// [`ToolResult::Unknown`]: the agent is told the effect may have happened, nothing is resent.
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        let op = tool_op(&q.id)?;
        let left = (q.deadline.0 - crate::now_ms()).max(0) as u64;
        let deadline = Duration::from_millis(left) + Duration::from_secs(5);
        let conn = self.0.conn()?;
        match conn
            .call_with(TOOL_CALL, to_value(&q)?, Lane::Control, deadline, Some(&op))
            .await
        {
            Ok(v) => from_value(v),
            Err(e) if e.code == ErrorCode::OutcomeUnknown => {
                Ok(ToolResult::Unknown { operation: op })
            }
            Err(e) => Err(e),
        }
    }
    async fn call_status(&self, key: ToolKey) -> ApiResult<ToolResult> {
        self.0.go(TOOL_INSPECT, &key, None).await
    }
    async fn cancel_call(&self, key: ToolKey) -> ApiResult<ToolResult> {
        let op = tool_op(&key.id)?;
        self.0.go(TOOL_CANCEL, &key, Some(&op)).await
    }
}

#[async_trait]
impl HubFiles for RemoteFiles {
    async fn fetch(
        &self,
        access: FileAccess,
        id: ArtifactId,
        offset: u64,
        max: u32,
    ) -> ApiResult<ReadChunk> {
        // hex doubles the bytes: keep a chunk's frame within the cap
        let max = max.min((BULK_CAP / 2) as u32 - 1024);
        let a = FetchArgs {
            access,
            id,
            offset,
            max,
        };
        self.0.go(BLOB_FETCH, &a, None).await
    }
    async fn begin_upload(&self, b: BeginUpload) -> ApiResult<Transfer> {
        let op = b.key.clone();
        self.0.go(BLOB_BEGIN, &b, Some(&op)).await
    }
    async fn upload(&self, chunk: BlobChunk) -> ApiResult<Transfer> {
        self.0.go(BLOB_CHUNK, &chunk, None).await
    }
    async fn upload_status(&self, transfer: TransferId) -> ApiResult<TransferState> {
        self.0.go(BLOB_STATUS, &transfer, None).await
    }
    async fn finish_upload(&self, transfer: TransferId) -> ApiResult<ArtifactRef> {
        self.0.go(BLOB_FINISH, &transfer, None).await
    }
    async fn abort_upload(&self, transfer: TransferId) -> ApiResult<TransferState> {
        self.0.go(BLOB_ABORT, &transfer, None).await
    }
}

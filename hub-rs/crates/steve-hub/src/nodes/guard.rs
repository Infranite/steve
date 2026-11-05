//! The only door from a Node's link into the Hub. Everything is keyed on the node id the link
//! authenticated as, never on what a payload says, and on the link's generation:
//!
//! - every frame of a link that is no longer the registry's current one is refused `stale`;
//! - before anything with an effect (events committed, a tool called, an upload kept) the Node's
//!   row is read again: same generation, active or paused, or `stale`;
//! - an event batch must be wholly about this Node's own sessions, attempts and operations, and
//!   the ids in each event's body must be its subject's (`Sink::commit` itself does not check it);
//!   otherwise nothing commits and an audit row says so.
//!
//! Revocation cannot stop processes on a hostile Node; it only stops the Hub from taking its
//! facts, serving its tools or giving it files.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use steve_node_api::*;
use steve_store::{Store, StoreError};

use super::fleet::NodeFleet;
use super::registry::NodeRegistry;

#[derive(Clone)]
pub struct LinkCtx {
    pub node: NodeId,
    pub generation: u64,
    pub store: Store,
    pub registry: Arc<NodeRegistry>,
    pub fleet: Arc<NodeFleet>,
}

fn stale() -> ApiError {
    ApiError::new(
        ErrorCode::Stale,
        "this connection no longer speaks for the node",
    )
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!("node guard: {e}");
    ApiError::new(ErrorCode::Internal, "node guard storage unavailable")
}

impl LinkCtx {
    /// The registry still has this link as the Node's current one.
    pub fn live(&self) -> ApiResult<()> {
        if self.registry.current(self.node.as_str()) == Some(self.generation) {
            Ok(())
        } else {
            Err(stale())
        }
    }

    /// [`Self::live`], and the store agrees: same generation, not revoked, not pending.
    pub async fn fresh(&self) -> ApiResult<()> {
        self.live()?;
        let (store, node) = (self.store.clone(), self.node.to_string());
        let row = tokio::task::spawn_blocking(move || {
            store.reader(|c| steve_store::nodes::link_state(c, &node))
        })
        .await
        .map_err(internal)?
        .map_err(internal)?;
        match row {
            Some((g, state))
                if g as u64 == self.generation && matches!(state.as_str(), "active" | "paused") =>
            {
                Ok(())
            }
            _ => Err(stale()),
        }
    }

    async fn audit(&self, what: &'static str, detail: String) {
        let (store, node) = (self.store.clone(), self.node.to_string());
        let r = tokio::task::spawn_blocking(move || {
            store.transaction::<_, StoreError>(|tx| {
                steve_store::nodes::audit(
                    tx,
                    Some(&node),
                    None,
                    what,
                    Some(&detail),
                    crate::nodes::now(),
                )
            })
        })
        .await;
        if !matches!(r, Ok(Ok(()))) {
            tracing::error!("node audit failed");
        }
    }
}

/// Whether every event of the batch is about `node`'s own work; why the first one is not, if any.
///
/// The subject must be this Node's: its session (the Hub's session id and the Node's are the same),
/// an attempt of that very turn of that very session, or one of its operations. The ids the
/// coordinator takes from the body must be the subject's: a permission request or resolution of
/// another execution, a session view of another session or a receipt of another operation would
/// otherwise reach work that is not this Node's.
fn foreign(
    c: &rusqlite::Connection,
    node: &str,
    batch: &EventBatch,
) -> Result<Option<String>, StoreError> {
    let own_session = |s: &SessionId| -> Result<bool, StoreError> {
        Ok(c.query_row(
            "SELECT 1 FROM runtime_sessions WHERE session_id=?1 AND node_session_id=?1 AND node_id=?2",
            params![s.as_str(), node],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    };
    let own_execution = |x: &ExecutionRef| -> Result<bool, StoreError> {
        let found = match x {
            ExecutionRef::Turn {
                session,
                turn,
                attempt,
            } => c
                .query_row(
                    "SELECT 1 FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id JOIN runtime_sessions s ON s.session_id=t.session_id WHERE a.attempt_id=?1 AND a.turn_id=?2 AND t.session_id=?3 AND s.node_session_id=?3 AND a.node_id=?4 AND s.node_id=?4",
                    params![attempt.as_str(), turn.as_str(), session.as_str(), node],
                    |_| Ok(()),
                )
                .optional()?,
            ExecutionRef::Job { attempt, .. } => c
                .query_row(
                    "SELECT 1 FROM runtime_attempts WHERE attempt_id=? AND node_id=?",
                    params![attempt.as_str(), node],
                    |_| Ok(()),
                )
                .optional()?,
        };
        Ok(found.is_some())
    };
    for e in &batch.events {
        let own = match &e.subject {
            Subject::Session { session } => own_session(session)?,
            Subject::Execution(x) => own_execution(x)?,
            Subject::Operation { operation } => c
                .query_row(
                    "SELECT 1 FROM node_commands WHERE operation_id=? AND node_id=?",
                    params![operation.as_str(), node],
                    |_| Ok(()),
                )
                .optional()?
                .is_some(),
        };
        if !own {
            return Ok(Some(format!(
                "event {} names a subject of another node",
                e.id
            )));
        }
        let execution = match &e.subject {
            Subject::Execution(x) => Some(x),
            _ => None,
        };
        let session = match &e.subject {
            Subject::Session { session } => Some(session),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => Some(session),
            _ => None,
        };
        let body_matches = match &e.body {
            EventBody::PermissionRequested { request } => execution == Some(&request.execution),
            EventBody::PermissionResolved { resolution } => {
                execution == Some(&resolution.execution)
            }
            EventBody::SessionChanged { view } => session == Some(&view.id),
            EventBody::OperationChanged { receipt } => matches!(
                &e.subject,
                Subject::Operation { operation } if *operation == receipt.operation
            ),
            _ => true,
        };
        if !body_matches {
            return Ok(Some(format!(
                "event {} names in its body other work than its subject",
                e.id
            )));
        }
    }
    Ok(None)
}

/// The guard on what a remote Node replays when the Hub pulls it (reconcile): the same check as
/// [`GuardedSink`]'s, keyed on the Node the coordinator drives. A batch about other work is refused
/// whole (`denied`) and audited.
pub(crate) fn replay_check(store: Store, node: NodeId) -> crate::wire::node_runtime::ReplayCheck {
    Arc::new(move |batch: &EventBatch| {
        let bad = store
            .reader(|c| foreign(c, node.as_str(), batch))
            .map_err(internal)?;
        let Some(why) = bad else { return Ok(()) };
        let audited = store.transaction::<_, StoreError>(|tx| {
            steve_store::nodes::audit(
                tx,
                Some(node.as_str()),
                None,
                "foreign_event",
                Some(&format!("replayed: {why}")),
                crate::nodes::now(),
            )
        });
        if audited.is_err() {
            tracing::error!("node audit failed");
        }
        Err(ApiError::new(
            ErrorCode::Denied,
            "replayed events about another node's work",
        ))
    })
}

pub struct GuardedSink(pub LinkCtx);

#[async_trait]
impl EventSink for GuardedSink {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        let l = &self.0;
        l.fresh().await?;
        let (store, node) = (l.store.clone(), l.node.to_string());
        let (batch, bad) = tokio::task::spawn_blocking(move || {
            let bad = store.reader(|c| foreign(c, &node, &batch));
            (batch, bad)
        })
        .await
        .map_err(internal)?;
        if let Some(why) = bad.map_err(internal)? {
            l.audit("foreign_event", why).await;
            return Err(ApiError::new(
                ErrorCode::Denied,
                "events about another node's work",
            ));
        }
        l.fleet.sink_for(&l.node).commit(batch).await
    }
}

pub struct GuardedTools(pub LinkCtx);

#[async_trait]
impl HubTools for GuardedTools {
    async fn catalog(&self, execution: ExecutionRef) -> ApiResult<Catalog> {
        self.0.live()?;
        self.0
            .fleet
            .tools_for(&self.0.node)
            .catalog(execution)
            .await
    }
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        self.0.fresh().await?;
        self.0.fleet.tools_for(&self.0.node).call(q).await
    }
    async fn call_status(&self, key: ToolKey) -> ApiResult<ToolResult> {
        self.0.live()?;
        self.0.fleet.tools_for(&self.0.node).call_status(key).await
    }
    async fn cancel_call(&self, key: ToolKey) -> ApiResult<ToolResult> {
        self.0.fresh().await?;
        self.0.fleet.tools_for(&self.0.node).cancel_call(key).await
    }
}

pub struct GuardedFiles(pub LinkCtx);

#[async_trait]
impl HubFiles for GuardedFiles {
    async fn fetch(
        &self,
        access: FileAccess,
        id: ArtifactId,
        offset: u64,
        max: u32,
    ) -> ApiResult<ReadChunk> {
        self.0.fresh().await?;
        self.0
            .fleet
            .files_for(&self.0.node)
            .fetch(access, id, offset, max)
            .await
    }
    async fn begin_upload(&self, b: BeginUpload) -> ApiResult<Transfer> {
        self.0.fresh().await?;
        self.0.fleet.files_for(&self.0.node).begin_upload(b).await
    }
    async fn upload(&self, chunk: BlobChunk) -> ApiResult<Transfer> {
        self.0.fresh().await?;
        self.0.fleet.files_for(&self.0.node).upload(chunk).await
    }
    async fn upload_status(&self, transfer: TransferId) -> ApiResult<TransferState> {
        self.0.live()?;
        self.0
            .fleet
            .files_for(&self.0.node)
            .upload_status(transfer)
            .await
    }
    async fn finish_upload(&self, transfer: TransferId) -> ApiResult<ArtifactRef> {
        self.0.fresh().await?;
        self.0
            .fleet
            .files_for(&self.0.node)
            .finish_upload(transfer)
            .await
    }
    async fn abort_upload(&self, transfer: TransferId) -> ApiResult<TransferState> {
        self.0.fresh().await?;
        self.0
            .fleet
            .files_for(&self.0.node)
            .abort_upload(transfer)
            .await
    }
}

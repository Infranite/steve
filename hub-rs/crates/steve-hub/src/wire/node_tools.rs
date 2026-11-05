//! Authenticated local Node -> ToolService. Execution identity replaces the legacy MCP bearer:
//! the session and workspace come from the Hub ledger, never from agent-supplied tool arguments.
//! A separate host journal fences ToolKey before calling the existing service. Service effects
//! cannot share this transaction: a crash leaves Unknown, never an automatic second execution.
use super::tool_service::ToolService;
use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use steve_node_api::*;
use steve_store::{Session, Store, StoreError};

pub(crate) fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!("node host: {e}");
    ApiError::new(ErrorCode::Internal, "Node host storage unavailable")
}
fn denied() -> ApiError {
    ApiError::new(
        ErrorCode::Denied,
        "execution is not authorized on this Node",
    )
}

/// Exact execution and authenticated Node binding. Historical lookup is only for a known call;
/// fresh side effects require the current, uncancelled attempt. Jobs are not yet hosted here.
pub(crate) fn authorize(
    store: &Store,
    node: &NodeId,
    exec: &ExecutionRef,
    active: bool,
) -> ApiResult<(Session, TurnSpec)> {
    let ExecutionRef::Turn {
        session,
        turn,
        attempt,
    } = exec
    else {
        return Err(denied());
    };
    let row = store.reader(|c| {
        Ok::<_, StoreError>(c.query_row(
            "SELECT a.execution,a.spec,a.state,a.cancel_requested,s.active_attempt,t.active_attempt FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id JOIN runtime_sessions s ON s.session_id=t.session_id WHERE a.attempt_id=?1 AND a.turn_id=?2 AND t.session_id=?3 AND a.node_id=?4 AND s.node_id=?4 AND s.node_session_id=?3",
            params![attempt.as_str(), turn.as_str(), session.as_str(), node.as_str()],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,bool>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?)),
        ).optional()?)
    }).map_err(internal)?.ok_or_else(denied)?;
    let saved: ExecutionRef = serde_json::from_str(&row.0).map_err(internal)?;
    let spec: TurnSpec = serde_json::from_str(&row.1).map_err(internal)?;
    if saved != *exec
        || spec.session != *session
        || spec.turn != *turn
        || spec.run.attempt != *attempt
        || spec.run.policy.tools.as_str() != "hub"
    {
        return Err(denied());
    }
    if active
        && (row.3
            || matches!(row.2.as_str(), "finished" | "unknown" | "rejected")
            || row.4.as_deref() != Some(attempt.as_str())
            || row.5.as_deref() != Some(attempt.as_str()))
    {
        return Err(denied());
    }
    let s = store
        .sessions()
        .get(session.as_str())
        .map_err(internal)?
        .ok_or_else(denied)?;
    // Same ended-session revocation as sessions.by_token(), plus no imported/read-only sessions.
    if s.status == "ended" || s.readonly != 0 || s.device_id != "hub" || s.source != "Steve" {
        return Err(denied());
    }
    if let Some(item) = &s.item_id {
        let item = store
            .items()
            .get(item)
            .map_err(internal)?
            .ok_or_else(denied)?;
        if item.workspace_id != s.workspace_id {
            return Err(denied());
        }
    }
    if let Some(bot) = &s.bot_id {
        let bot = store
            .bots()
            .get(bot)
            .map_err(internal)?
            .ok_or_else(denied)?;
        if bot.workspace_id != s.workspace_id || !bot.enabled {
            return Err(denied());
        }
    }
    Ok((s, spec))
}

/// Injectable only at the bridge boundary: production always uses the existing ToolService.
#[async_trait]
pub trait Service: Send + Sync {
    fn catalog(&self) -> Vec<ToolDescriptor>;
    async fn call(
        &self,
        session: Session,
        name: &str,
        args: serde_json::Value,
    ) -> Result<String, String>;
}
#[async_trait]
impl Service for ToolService {
    fn catalog(&self) -> Vec<ToolDescriptor> {
        self.catalog()
    }
    async fn call(
        &self,
        session: Session,
        name: &str,
        args: serde_json::Value,
    ) -> Result<String, String> {
        self.call(session, name, args).await
    }
}
struct Ledger {
    db: Mutex<Connection>,
    // Exclusive process owner, so opening a second bridge cannot mark live calls Unknown.
    _lock: std::fs::File,
}
pub struct NodeTools {
    store: Store,
    node: NodeId,
    ledger: Arc<Ledger>,
    service: OnceLock<Arc<dyn Service>>,
    /// ToolService invocations in flight. A call past the bound is refused before its ledger
    /// row exists, so it is plainly not executed (never uncertain).
    permits: Arc<tokio::sync::Semaphore>,
}

/// How many Steve tool calls one Node may have running in the Hub at once.
pub(crate) const MAX_CALLS: usize = 16;

/// SQLite (the Hub store and the fsyncing ledger) off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(internal)?
}
impl NodeTools {
    pub fn open(store: Store, node: NodeId, home: &Path) -> ApiResult<Arc<Self>> {
        use std::os::fd::AsRawFd;
        std::fs::create_dir_all(home).map_err(internal)?;
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.join("host-tools.lock"))
            .map_err(internal)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(ApiError::new(
                ErrorCode::Busy,
                "another tool host owns this ledger",
            ));
        }
        let mut db = Connection::open(home.join("host-tools.sqlite3")).map_err(internal)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(internal)?;
        let tx = db.transaction().map_err(internal)?;
        let version: i64 = tx
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(internal)?;
        if version > 1 {
            return Err(ApiError::new(ErrorCode::Unsupported, "newer tool journal"));
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS calls (key TEXT PRIMARY KEY, digest TEXT NOT NULL, operation TEXT NOT NULL, node TEXT NOT NULL, session TEXT NOT NULL, workspace TEXT NOT NULL, name TEXT NOT NULL, catalog TEXT NOT NULL, deadline INTEGER NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL, state TEXT NOT NULL, result TEXT, cancel_requested INTEGER NOT NULL DEFAULT 0); UPDATE calls SET state='unknown' WHERE state='running'; PRAGMA user_version=1;").map_err(internal)?;
        tx.commit().map_err(internal)?;
        Ok(Arc::new(Self {
            store,
            node,
            ledger: Arc::new(Ledger {
                db: Mutex::new(db),
                _lock: lock,
            }),
            service: OnceLock::new(),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CALLS)),
        }))
    }
    /// Install after core/modules are built, before starting the coordinator. No tools are served
    /// while the Core -> Runtime -> Node -> ToolService initialization cycle is incomplete.
    pub fn install(&self, service: Arc<dyn Service>) -> ApiResult<()> {
        self.service
            .set(service)
            .map_err(|_| ApiError::new(ErrorCode::Conflict, "tool service already installed"))
    }
    fn service(&self) -> ApiResult<Arc<dyn Service>> {
        self.service
            .get()
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Offline, "tool service not ready"))
    }
    fn catalog_of(service: &dyn Service) -> ApiResult<Catalog> {
        let tools = service.catalog();
        let digest = digest_of(&tools)?;
        Ok(Catalog {
            revision: Revision::new(derived_id("catalog", digest.as_str())).map_err(internal)?,
            tools,
        })
    }
    fn key(key: &ToolKey) -> ApiResult<String> {
        serde_json::to_string(key).map_err(internal)
    }
    /// Authorizes `exec` (historically, or as the running attempt) on a blocking thread.
    async fn authorized(&self, exec: &ExecutionRef, active: bool) -> ApiResult<Session> {
        let (store, node, exec) = (self.store.clone(), self.node.clone(), exec.clone());
        blocking(move || authorize(&store, &node, &exec, active).map(|(s, _)| s)).await
    }
    /// What the ledger holds for `key`, for an execution authorized historically.
    async fn saved(&self, key: &ToolKey, digest: Option<Digest>) -> ApiResult<Option<ToolResult>> {
        let (store, node, ledger) = (self.store.clone(), self.node.clone(), self.ledger.clone());
        let (exec, encoded) = (key.execution.clone(), Self::key(key)?);
        blocking(move || {
            authorize(&store, &node, &exec, false)?;
            saved(&ledger.db.lock().unwrap(), &encoded, digest.as_ref())
        })
        .await
    }
}
fn saved(db: &Connection, key: &str, digest: Option<&Digest>) -> ApiResult<Option<ToolResult>> {
    let row = db
        .query_row(
            "SELECT digest,operation,state,result FROM calls WHERE key=?",
            [key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(internal)?;
    let Some((d, op, state, result)) = row else {
        return Ok(None);
    };
    if digest.is_some_and(|digest| digest.as_str() != d) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "ToolKey reused with different name/arguments",
        ));
    }
    let operation = OperationId::new(op).map_err(internal)?;
    Ok(Some(match result {
        Some(r) => serde_json::from_str(&r).map_err(internal)?,
        None if state == "running" => ToolResult::Running { operation },
        None => ToolResult::Unknown { operation },
    }))
}
#[async_trait]
impl HubTools for NodeTools {
    async fn catalog(&self, execution: ExecutionRef) -> ApiResult<Catalog> {
        self.authorized(&execution, true).await?;
        Self::catalog_of(&*self.service()?)
    }
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        let key = q.key();
        let digest = q.args_digest()?;
        if let Some(r) = self.saved(&key, Some(digest.clone())).await? {
            return Ok(r);
        }
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                ErrorCode::Exhausted,
                "too many Steve tool calls running for this Node",
            )
        })?;
        let session = self.authorized(&q.execution, true).await?;
        let service = self.service()?;
        let catalog = Self::catalog_of(&*service)?;
        if catalog.revision != q.catalog {
            return Err(ApiError::new(ErrorCode::Stale, "tool catalog changed"));
        }
        if !catalog.tools.iter().any(|t| t.name == q.name) {
            return Err(ApiError::new(ErrorCode::Denied, "tool outside catalog"));
        }
        if !q.args.is_object() {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "tool arguments must be an object",
            ));
        }
        if q.deadline.0 <= steve_store::now() {
            return Err(ApiError::new(
                ErrorCode::Expired,
                "tool deadline elapsed before execution",
            ));
        }
        let encoded = Self::key(&key)?;
        let operation = OperationId::new(derived_id("tool", &encoded)).map_err(internal)?;
        let status_key = encoded.clone();
        let fenced = {
            let (ledger, encoded, operation) =
                (self.ledger.clone(), encoded.clone(), operation.clone());
            let (node, session, name, catalog, deadline) = (
                self.node.clone(),
                session.clone(),
                q.name.clone(),
                q.catalog.clone(),
                q.deadline.0,
            );
            blocking(move || {
                let db = ledger.db.lock().unwrap();
                if let Some(r) = saved(&db, &encoded, Some(&digest))? {
                    return Ok(Some(r));
                }
                db.execute("INSERT INTO calls(key,digest,operation,node,session,workspace,name,catalog,deadline,created,updated,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10,'running')",params![encoded,digest.as_str(),operation.as_str(),node.as_str(),session.id,session.workspace_id,name,catalog.as_str(),deadline,steve_store::now()]).map_err(internal)?;
                Ok(None)
            })
            .await?
        };
        if let Some(r) = fenced {
            return Ok(r);
        }
        // The task owns execution even if the caller disconnects. Timeout/cancel does not abort
        // ToolService: it exposes no cancellation proof and may already have an outside effect.
        let ledger = self.ledger.clone();
        let store = self.store.clone();
        let node = self.node.clone();
        let wait_ms = (q.deadline.0 - steve_store::now()).clamp(0, 60_000) as u64;
        let op = operation.clone();
        let task = tokio::spawn(async move {
            let _permit = permit;
            // A cancel that landed between the fence and here wins: nothing has run yet, so the
            // outcome is a certain refusal rather than Unknown.
            let (check, key) = (ledger.clone(), encoded.clone());
            let cancelled = blocking(move || {
                check
                    .db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT cancel_requested FROM calls WHERE key=?1",
                        [&key],
                        |r| r.get::<_, bool>(0),
                    )
                    .map_err(internal)
            })
            .await?;
            let exec = q.execution.clone();
            let authorized = if cancelled {
                Ok(())
            } else {
                blocking(move || authorize(&store, &node, &exec, true).map(|_| ())).await
            };
            let result = match authorized {
                _ if cancelled => ToolResult::Failed {
                    error: ApiError::new(
                        ErrorCode::Denied,
                        "tool call cancelled before invocation",
                    ),
                },
                Err(error) => ToolResult::Failed { error },
                Ok(_) if steve_store::now() >= q.deadline.0 => ToolResult::Failed {
                    error: ApiError::new(ErrorCode::Expired, "tool expired before invocation"),
                },
                Ok(_) => match service.call(session, &q.name, q.args).await {
                    Ok(text) => ToolResult::Completed {
                        end_turn: text.ends_with(steve_mcp::tools::END),
                        parts: vec![Part::Text { text }],
                        structured: None,
                    },
                    Err(text) => ToolResult::Failed {
                        error: ApiError::new(
                            ErrorCode::Invalid,
                            steve_vault::redact::scrub_any(&text),
                        ),
                    },
                },
            };
            let encoded_result = serde_json::to_string(&result).map_err(internal)?;
            blocking(move || {
                ledger
                    .db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE calls SET state='done',result=?2,updated=?3 WHERE key=?1",
                        params![encoded, encoded_result, steve_store::now()],
                    )
                    .map_err(internal)
            })
            .await?;
            Ok::<_, ApiError>(result)
        });
        match tokio::time::timeout(std::time::Duration::from_millis(wait_ms), task).await {
            Ok(Ok(Ok(result))) => Ok(result),
            _ => {
                let ledger = self.ledger.clone();
                blocking(move || {
                    ledger.db.lock().unwrap().execute("UPDATE calls SET state='unknown',updated=?2 WHERE key=?1 AND state='running'",params![status_key,steve_store::now()]).map_err(internal)
                })
                .await?;
                Ok(ToolResult::Unknown { operation: op })
            }
        }
    }
    async fn call_status(&self, key: ToolKey) -> ApiResult<ToolResult> {
        self.saved(&key, None)
            .await?
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "unknown ToolKey"))
    }
    async fn cancel_call(&self, key: ToolKey) -> ApiResult<ToolResult> {
        let (store, node, ledger) = (self.store.clone(), self.node.clone(), self.ledger.clone());
        let encoded = Self::key(&key)?;
        let result = blocking(move || {
            authorize(&store, &node, &key.execution, false)?;
            let db = ledger.db.lock().unwrap();
            let result = saved(&db, &encoded, None)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "unknown ToolKey"))?;
            db.execute("UPDATE calls SET cancel_requested=1,state=CASE WHEN state='running' THEN 'unknown' ELSE state END,updated=?2 WHERE key=?1",params![encoded,steve_store::now()]).map_err(internal)?;
            Ok(result)
        })
        .await?;
        match result {
            ToolResult::Running { operation } => Ok(ToolResult::Unknown { operation }),
            r => Ok(r),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::node_runtime::{Binding, NodeDomain, NodeRuntime, Sink};
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_node_api::testkit::{FakeNode, build};
    pub(crate) struct Ends;
    impl NodeDomain for Ends {
        fn turn_ended(&self, _: &str, _: &str, _: Option<String>) {}
    }
    pub(crate) struct Host {
        pub dir: tempfile::TempDir,
        pub store: Store,
        pub exec: ExecutionRef,
        pub _rt: Arc<NodeRuntime>,
        pub _node: Arc<FakeNode>,
    }
    pub(crate) async fn fixture(files: Vec<steve_store::FileRef>) -> Host {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path().join("hub.sqlite3"),
            steve_store::FnPublisher(|_| {}),
        )
        .unwrap();
        fixture_on(dir, store, files).await
    }
    /// An item `i1` with session `s1` bound to a fake Node, its turn running, on `store`.
    pub(crate) async fn fixture_on(
        dir: tempfile::TempDir,
        store: Store,
        files: Vec<steve_store::FileRef>,
    ) -> Host {
        store.ensure_workspace().unwrap();
        store.transaction::<_,StoreError>(|tx| {
            tx.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)",[])?;
            tx.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','','active',1,1)",[])?;
            Ok(())
        }).unwrap();
        let node = FakeNode::new("local");
        let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Ends));
        node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
        rt.start();
        let mut policy = build::policy();
        policy.tools = GrantId::new("hub").unwrap();
        rt.bind(&Binding {
            session: "s1".into(),
            resource: "item:i1".into(),
            prepare: PrepareKind::Scratch,
            choice: build::choice(),
            policy,
        })
        .unwrap();
        for _ in 0..200 {
            let phase = store
                .reader(|c| {
                    Ok::<_, StoreError>(c.query_row(
                        "SELECT phase FROM runtime_sessions WHERE session_id='s1'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?)
                })
                .unwrap();
            if phase == "ready" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        rt.prompt("s1", "hello", "user", &files).unwrap();
        let exec = store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT execution FROM runtime_attempts LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap();
        Host {
            dir,
            store,
            exec: serde_json::from_str(&exec).unwrap(),
            _rt: rt,
            _node: node,
        }
    }
    struct Mock {
        calls: AtomicUsize,
        gate: Option<Arc<tokio::sync::Notify>>,
    }
    #[async_trait]
    impl Service for Mock {
        fn catalog(&self) -> Vec<ToolDescriptor> {
            vec![ToolDescriptor {
                name: "test".into(),
                description: "test".into(),
                input_schema: json!({"type":"object"}),
                read_only: false,
            }]
        }
        async fn call(&self, _: Session, _: &str, _: serde_json::Value) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            Ok(format!("ok {}", steve_mcp::tools::END))
        }
    }
    async fn request(tools: &NodeTools, exec: ExecutionRef) -> ToolCall {
        ToolCall {
            id: CallId::new("call1").unwrap(),
            catalog: tools.catalog(exec.clone()).await.unwrap().revision,
            execution: exec,
            name: "test".into(),
            args: json!({"a":1,"b":2}),
            deadline: Timestamp(steve_store::now() + 60_000),
        }
    }
    #[tokio::test]
    async fn once_conflict_revocation_and_durable_result() {
        let h = fixture(vec![]).await;
        let tools =
            NodeTools::open(h.store.clone(), NodeId::new("local").unwrap(), h.dir.path()).unwrap();
        let service = Arc::new(Mock {
            calls: AtomicUsize::new(0),
            gate: None,
        });
        tools.install(service.clone()).unwrap();
        let q = request(&tools, h.exec.clone()).await;
        // either call may take the fence first (the ledger runs on blocking threads): one runs,
        // the other sees it running or done
        let (a, b) = tokio::join!(tools.call(q.clone()), tools.call(q.clone()));
        let (a, b) = (a.unwrap(), b.unwrap());
        let (won, other) = if matches!(a, ToolResult::Completed { .. }) {
            (a, b)
        } else {
            (b, a)
        };
        assert!(matches!(won, ToolResult::Completed { end_turn: true, .. }));
        assert!(matches!(
            other,
            ToolResult::Running { .. } | ToolResult::Completed { .. }
        ));
        assert_eq!(service.calls.load(Ordering::SeqCst), 1);
        let mut same = q.clone();
        same.args = serde_json::from_str("{\"b\":2,\"a\":1}").unwrap();
        assert!(matches!(
            tools.call(same).await.unwrap(),
            ToolResult::Completed { .. }
        ));
        let mut changed = q.clone();
        changed.args = json!({"a":3});
        assert_eq!(
            tools.call(changed).await.unwrap_err().code,
            ErrorCode::Conflict
        );
        drop(tools);
        let tools =
            NodeTools::open(h.store.clone(), NodeId::new("local").unwrap(), h.dir.path()).unwrap();
        assert!(matches!(
            tools.call(q.clone()).await.unwrap(),
            ToolResult::Completed { .. }
        ));
        assert!(matches!(
            tools.cancel_call(q.key()).await.unwrap(),
            ToolResult::Completed { .. }
        ));
        let other = NodeTools::open(
            h.store.clone(),
            NodeId::new("other").unwrap(),
            &h.dir.path().join("other"),
        )
        .unwrap();
        assert_eq!(
            other.call_status(q.key()).await.unwrap_err().code,
            ErrorCode::Denied
        );
        h.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("UPDATE sessions SET status='ended' WHERE id='s1'", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            tools.call_status(q.key()).await.unwrap_err().code,
            ErrorCode::Denied
        );
    }
    #[tokio::test]
    async fn timeout_and_cancel_are_uncertain_never_reinvoked() {
        let h = fixture(vec![]).await;
        let tools =
            NodeTools::open(h.store.clone(), NodeId::new("local").unwrap(), h.dir.path()).unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        let service = Arc::new(Mock {
            calls: AtomicUsize::new(0),
            gate: Some(gate.clone()),
        });
        tools.install(service.clone()).unwrap();
        let mut q = request(&tools, h.exec.clone()).await;
        q.deadline = Timestamp(steve_store::now() + 50);
        assert!(matches!(
            tools.call(q.clone()).await.unwrap(),
            ToolResult::Unknown { .. }
        ));
        assert!(matches!(
            tools.cancel_call(q.key()).await.unwrap(),
            ToolResult::Unknown { .. }
        ));
        assert!(matches!(
            tools.call(q.clone()).await.unwrap(),
            ToolResult::Unknown { .. }
        ));
        gate.notify_one();
        for _ in 0..100 {
            if matches!(
                tools.call_status(q.key()).await.unwrap(),
                ToolResult::Completed { .. }
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            tools.call_status(q.key()).await.unwrap(),
            ToolResult::Completed { .. }
        ));
        assert_eq!(service.calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn interrupted_call_is_unknown_on_reopen() {
        let h = fixture(vec![]).await;
        let tools =
            NodeTools::open(h.store.clone(), NodeId::new("local").unwrap(), h.dir.path()).unwrap();
        tools
            .install(Arc::new(Mock {
                calls: AtomicUsize::new(0),
                gate: None,
            }))
            .unwrap();
        let q = request(&tools, h.exec.clone()).await;
        // Crash fixture: accepted intent was durable, its execution/final commit was interrupted.
        {
            let db = tools.ledger.db.lock().unwrap();
            db.execute(
                "INSERT INTO calls VALUES (?1,?2,'tool_op','local','s1','default','test','rev',999,1,1,'running',NULL,0)",
                params![NodeTools::key(&q.key()).unwrap(), q.args_digest().unwrap().as_str()],
            )
            .unwrap();
        }
        drop(tools);
        let tools =
            NodeTools::open(h.store.clone(), NodeId::new("local").unwrap(), h.dir.path()).unwrap();
        assert!(matches!(
            tools.call(q).await.unwrap(),
            ToolResult::Unknown { .. }
        ));
    }

    /// Past MAX_CALLS running calls, one more is refused before its ledger row exists (plainly
    /// not executed), and goes through once a slot is free.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn calls_past_the_bound_are_refused_before_the_ledger() {
        let h = fixture(vec![]).await;
        let tools =
            NodeTools::open(h.store.clone(), NodeId::new("local").unwrap(), h.dir.path()).unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        let service = Arc::new(Mock {
            calls: AtomicUsize::new(0),
            gate: Some(gate.clone()),
        });
        tools.install(service.clone()).unwrap();
        let q = request(&tools, h.exec.clone()).await;
        let mut running = vec![];
        for i in 0..MAX_CALLS {
            let mut c = q.clone();
            c.id = CallId::new(format!("busy{i}")).unwrap();
            let t = tools.clone();
            running.push(tokio::spawn(async move { t.call(c).await }));
        }
        for _ in 0..500 {
            if service.calls.load(Ordering::SeqCst) == MAX_CALLS {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(service.calls.load(Ordering::SeqCst), MAX_CALLS);
        let mut over = q.clone();
        over.id = CallId::new("over").unwrap();
        assert_eq!(
            tools.call(over.clone()).await.unwrap_err().code,
            ErrorCode::Exhausted
        );
        let rows: i64 = tools
            .ledger
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM calls WHERE key=?",
                [NodeTools::key(&over.key()).unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0, "a refused call leaves nothing uncertain");
        for _ in 0..500 {
            if running.iter().all(|t| t.is_finished()) {
                break;
            }
            gate.notify_waiters();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        for t in running {
            assert!(matches!(
                t.await.unwrap().unwrap(),
                ToolResult::Completed { .. }
            ));
        }
        let open = tools.clone();
        let late = tokio::spawn(async move { open.call(over).await });
        for _ in 0..500 {
            if late.is_finished() {
                break;
            }
            gate.notify_waiters();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(matches!(
            late.await.unwrap().unwrap(),
            ToolResult::Completed { .. }
        ));
    }

    /// The bridge on the Hub's real ToolService (not a stand-in): the catalog is the MCP one, a
    /// Steve tool runs for the bound session and changes its item, and an ended session is refused
    /// before anything runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_real_tool_service_runs_for_the_bound_session_only() {
        steve_modules::net::forbid_in_this_process();
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().to_path_buf();
        let vars: std::collections::HashMap<&str, String> = [
            ("HOME", h.display().to_string()),
            ("STEVE_HOME", h.join("steve").display().to_string()),
            ("STEVE_HOST", "127.0.0.1".into()),
            ("STEVE_PUBLIC_URL", "http://127.0.0.1:9".into()),
            ("STEVE_DEV_LOGIN", "1".into()),
            ("STEVE_REPO", "/nonexistent".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
            ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
            ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
        ]
        .into_iter()
        .collect();
        let get = move |k: &str| vars.get(k).cloned();
        let cfg = Arc::new(crate::config::Config::from_vars(&get).unwrap());
        for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        // STEVE_NODE is unset: the Hub's ToolService as it would be installed for a Node
        let w = super::super::wire_with(&cfg, &crate::bus::Bus::new(), Arc::new(get)).unwrap();
        let store = Store::open(cfg.db_path(), steve_store::FnPublisher(|_| {})).unwrap();
        let home = cfg.data_dir.join("node");
        let host = fixture_on(dir, store.clone(), vec![]).await;
        let tools = NodeTools::open(store.clone(), NodeId::new("local").unwrap(), &home).unwrap();
        tools.install(w.tool_service.clone()).unwrap();

        let catalog = tools.catalog(host.exec.clone()).await.unwrap();
        let names: Vec<&str> = catalog.tools.iter().map(|t| t.name.as_str()).collect();
        let mcp: Vec<String> = w
            .tool_service
            .catalog()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, mcp, "the same tools as the MCP endpoint");
        assert!(names.contains(&steve_mcp::tools::UPDATE));

        let call = |id: &str| ToolCall {
            id: CallId::new(id).unwrap(),
            catalog: catalog.revision.clone(),
            execution: host.exec.clone(),
            name: steve_mcp::tools::UPDATE.into(),
            args: json!({"now": "bridging tools", "plan": [{"step": "bridge", "done": false}]}),
            deadline: Timestamp(steve_store::now() + 60_000),
        };
        let r = tools.call(call("u1")).await.unwrap();
        assert!(matches!(r, ToolResult::Completed { .. }), "{r:?}");
        let now: String = store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT now FROM items WHERE id='i1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(now, "bridging tools", "the tool ran for the bound item");

        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("UPDATE sessions SET status='ended' WHERE id='s1'", [])?;
                Ok(())
            })
            .unwrap();
        let mut late = call("u2");
        late.args = json!({"now": "too late", "plan": []});
        let e = tools.call(late).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
        let now: String = store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT now FROM items WHERE id='i1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(now, "bridging tools", "nothing ran for an ended session");
    }
}

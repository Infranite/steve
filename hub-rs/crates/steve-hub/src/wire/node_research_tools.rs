//! Research jobs use the existing read-only MCP allowlist, bound to their authenticated Node and
//! durable thread run. Session turns still use NodeTools unchanged. No bearer token leaves the Hub.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use steve_node_api::*;
use steve_store::{Store, StoreError, ThreadRun};

use super::mcp_extra::Extra;
use super::node_tools::{MAX_CALLS, NodeTools, internal};
use super::research_mcp;

fn denied() -> ApiError {
    ApiError::new(
        ErrorCode::Denied,
        "research execution is not authorized on this Node",
    )
}

fn authorize(store: &Store, node: &NodeId, exec: &ExecutionRef) -> ApiResult<ThreadRun> {
    let ExecutionRef::Job { job, attempt } = exec else {
        return Err(denied());
    };
    let row = store.reader(|c| {
        Ok::<_, StoreError>(c.query_row(
            "SELECT a.execution,a.spec,j.thread_run_id
             FROM runtime_jobs j JOIN runtime_job_attempts a ON a.job_id=j.job_id
             JOIN thread_runs r ON r.id=j.thread_run_id
             JOIN messages m ON m.id=r.message_id
             WHERE j.job_id=?1 AND a.attempt_id=?2 AND j.node_id=?3 AND a.node_id=?3
             AND j.kind='research' AND j.state='open' AND a.state IN ('planned','accepted','running')
             AND a.cancel_requested=0 AND r.status='running'
             AND r.workspace_id=j.workspace_id AND m.workspace_id=j.workspace_id",
            params![job.as_str(), attempt.as_str(), node.as_str()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        ).optional()?)
    }).map_err(internal)?.ok_or_else(denied)?;
    let saved: ExecutionRef = serde_json::from_str(&row.0).map_err(internal)?;
    let spec: JobSpec = serde_json::from_str(&row.1).map_err(internal)?;
    if saved != *exec
        || spec.job != *job
        || spec.run.attempt != *attempt
        || spec.run.policy.tools.as_str() != "hub"
        || !matches!(
            spec.work,
            JobWork::Research(ResearchJob {
                steve_tools: true,
                ..
            })
        )
        || spec.run.limits.deadline.0 <= steve_store::now()
    {
        return Err(denied());
    }
    store
        .thread()
        .run(&row.2)
        .map_err(internal)?
        .filter(|run| run.status == "running")
        .ok_or_else(denied)
}

#[async_trait]
trait ReadService: Send + Sync {
    async fn call(
        &self,
        run: ThreadRun,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<String, String>;
}

#[async_trait]
impl ReadService for Extra {
    async fn call(
        &self,
        run: ThreadRun,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let (store, ws) = (self.store.clone(), run.workspace_id.clone());
        let owner = tokio::task::spawn_blocking(move || {
            store.reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT user_id FROM workspace_members WHERE workspace_id=?1 AND role='owner' ORDER BY created LIMIT 1",
                    [ws], |r| r.get::<_, String>(0),
                ).optional()?.unwrap_or_default())
            })
        }).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        research_mcp::call(self, &run, name, args, &owner).await
    }
}

struct ReadCall {
    digest: Digest,
    result: Arc<Mutex<ToolResult>>,
}

pub struct ResearchTools {
    store: Store,
    node: NodeId,
    inner: Arc<NodeTools>,
    service: OnceLock<Arc<dyn ReadService>>,
    // Reads have no outside effect: replay in-process, and safely read again after a restart.
    calls: Mutex<HashMap<ToolKey, ReadCall>>,
    permits: Arc<tokio::sync::Semaphore>,
}

impl ResearchTools {
    pub fn new(store: Store, node: NodeId, inner: Arc<NodeTools>) -> Arc<Self> {
        Arc::new(Self {
            store,
            node,
            inner,
            service: OnceLock::new(),
            calls: Mutex::new(HashMap::new()),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CALLS)),
        })
    }

    pub fn install(&self, extra: Extra) -> ApiResult<()> {
        self.service.set(Arc::new(extra)).map_err(|_| {
            ApiError::new(
                ErrorCode::Conflict,
                "research tool service already installed",
            )
        })
    }

    async fn authorized(&self, exec: &ExecutionRef) -> ApiResult<ThreadRun> {
        let (store, node, exec) = (self.store.clone(), self.node.clone(), exec.clone());
        tokio::task::spawn_blocking(move || authorize(&store, &node, &exec))
            .await
            .map_err(internal)?
    }

    fn catalog_of() -> ApiResult<Catalog> {
        let tools: Vec<_> = research_mcp::tools()
            .into_iter()
            .map(|t| ToolDescriptor {
                name: t["name"].as_str().unwrap_or_default().into(),
                description: t["description"].as_str().unwrap_or_default().into(),
                input_schema: t["inputSchema"].clone(),
                read_only: true,
            })
            .collect();
        Ok(Catalog {
            revision: Revision::new(derived_id("research-tools", digest_of(&tools)?.as_str()))
                .map_err(internal)?,
            tools,
        })
    }

    fn saved(&self, key: &ToolKey, digest: Option<&Digest>) -> ApiResult<Option<ToolResult>> {
        let calls = self.calls.lock().unwrap();
        let Some(call) = calls.get(key) else {
            return Ok(None);
        };
        if digest.is_some_and(|d| *d != call.digest) {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "ToolKey reused with different name/arguments",
            ));
        }
        Ok(Some(call.result.lock().unwrap().clone()))
    }

    async fn prune_finished(&self) -> ApiResult<()> {
        let observed: std::collections::HashSet<_> =
            self.calls.lock().unwrap().keys().cloned().collect();
        if observed.is_empty() {
            return Ok(());
        }
        let (store, node) = (self.store.clone(), self.node.clone());
        let active = tokio::task::spawn_blocking(move || {
            store.reader(|c| {
                let mut stmt = c.prepare_cached(
                    "SELECT a.attempt_id FROM runtime_job_attempts a
                     JOIN runtime_jobs j ON j.job_id=a.job_id JOIN thread_runs r ON r.id=j.thread_run_id
                     WHERE a.node_id=?1 AND j.node_id=?1 AND j.state='open' AND r.status='running'
                     AND a.state IN ('planned','accepted','running') AND a.cancel_requested=0",
                )?;
                let rows = stmt.query_map([node.as_str()], |r| r.get::<_, String>(0))?;
                Ok::<_, StoreError>(rows.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)
            })
        }).await.map_err(internal)?.map_err(internal)?;
        self.calls.lock().unwrap().retain(|key, _| !observed.contains(key) || matches!(&key.execution, ExecutionRef::Job { attempt, .. } if active.contains(attempt.as_str())));
        Ok(())
    }
}

#[async_trait]
impl HubTools for ResearchTools {
    async fn catalog(&self, execution: ExecutionRef) -> ApiResult<Catalog> {
        if matches!(execution, ExecutionRef::Turn { .. }) {
            return self.inner.catalog(execution).await;
        }
        self.authorized(&execution).await?;
        if self.service.get().is_none() {
            return Err(ApiError::new(
                ErrorCode::Offline,
                "research tool service not ready",
            ));
        }
        Self::catalog_of()
    }

    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        if matches!(q.execution, ExecutionRef::Turn { .. }) {
            return self.inner.call(q).await;
        }
        let run = self.authorized(&q.execution).await?;
        let key = q.key();
        let digest = q.args_digest()?;
        if let Some(r) = self.saved(&key, Some(&digest))? {
            return Ok(r);
        }
        // Old run results are already inaccessible; do not retain their potentially large text.
        self.prune_finished().await?;
        let catalog = Self::catalog_of()?;
        if q.catalog != catalog.revision {
            return Err(ApiError::new(
                ErrorCode::Stale,
                "research tool catalog changed",
            ));
        }
        if !catalog.tools.iter().any(|t| t.name == q.name) {
            return Err(denied());
        }
        if !q.args.is_object() {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "tool arguments must be an object",
            ));
        }
        let remaining = q.deadline.0 - steve_store::now();
        if remaining <= 0 {
            return Err(ApiError::new(ErrorCode::Expired, "tool deadline elapsed"));
        }
        let service =
            self.service.get().cloned().ok_or_else(|| {
                ApiError::new(ErrorCode::Offline, "research tool service not ready")
            })?;
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::new(ErrorCode::Exhausted, "too many research tool calls"))?;
        let operation = OperationId::new(derived_id(
            "research-tool",
            &serde_json::to_string(&key).map_err(internal)?,
        ))
        .map_err(internal)?;
        let result = {
            let mut calls = self.calls.lock().unwrap();
            if let Some(call) = calls.get(&key) {
                if call.digest != digest {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "ToolKey reused with different name/arguments",
                    ));
                }
                return Ok(call.result.lock().unwrap().clone());
            }
            let result = Arc::new(Mutex::new(ToolResult::Running { operation }));
            calls.insert(
                key,
                ReadCall {
                    digest,
                    result: result.clone(),
                },
            );
            result
        };
        let saved = result.clone();
        let (store, node, exec) = (self.store.clone(), self.node.clone(), q.execution.clone());
        // The task owns the read if the requesting socket disappears; resends see its one result.
        let task = tokio::spawn(async move {
            let _permit = permit;
            let (check_store, check_node, check_exec) = (store.clone(), node.clone(), exec.clone());
            let allowed = tokio::task::spawn_blocking(move || {
                authorize(&check_store, &check_node, &check_exec)
            })
            .await
            .map_err(internal)
            .and_then(|r| r);
            let cancelled = !matches!(*saved.lock().unwrap(), ToolResult::Running { .. });
            let out = match allowed {
                Err(e) => Err(e.safe_detail),
                Ok(_) if cancelled => Err("research tool call cancelled".into()),
                Ok(_) => match tokio::time::timeout(
                    std::time::Duration::from_millis(
                        (q.deadline.0 - steve_store::now()).max(0) as u64
                    ),
                    service.call(run, &q.name, &q.args),
                )
                .await
                {
                    Ok(out) => out,
                    Err(_) => Err("research tool deadline elapsed".into()),
                },
            };
            let active = tokio::task::spawn_blocking(move || authorize(&store, &node, &exec))
                .await
                .map_err(internal)
                .and_then(|r| r);
            let done = match active {
                Err(error) => ToolResult::Failed { error },
                Ok(_) => match out {
                    Ok(text) => ToolResult::Completed {
                        parts: vec![Part::Text { text }],
                        structured: None,
                        end_turn: false,
                    },
                    Err(text) => ToolResult::Failed {
                        error: ApiError::new(
                            ErrorCode::Invalid,
                            steve_vault::redact::scrub_any(&text),
                        ),
                    },
                },
            };
            let mut r = saved.lock().unwrap();
            if matches!(*r, ToolResult::Running { .. }) {
                *r = done
            }
        });
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(remaining as u64), task).await;
        self.authorized(&q.execution).await?;
        Ok(result.lock().unwrap().clone())
    }

    async fn call_status(&self, key: ToolKey) -> ApiResult<ToolResult> {
        if matches!(key.execution, ExecutionRef::Turn { .. }) {
            return self.inner.call_status(key).await;
        }
        self.authorized(&key.execution).await?;
        self.saved(&key, None)?
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "unknown ToolKey"))
    }

    async fn cancel_call(&self, key: ToolKey) -> ApiResult<ToolResult> {
        if matches!(key.execution, ExecutionRef::Turn { .. }) {
            return self.inner.cancel_call(key).await;
        }
        self.authorized(&key.execution).await?;
        let calls = self.calls.lock().unwrap();
        let call = calls
            .get(&key)
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "unknown ToolKey"))?;
        let mut result = call.result.lock().unwrap();
        if matches!(*result, ToolResult::Running { .. }) {
            *result = ToolResult::Failed {
                error: ApiError::new(ErrorCode::Denied, "research tool call cancelled"),
            };
        }
        Ok(result.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_node_api::testkit::build;

    struct Reader {
        count: AtomicUsize,
        hold: Option<Arc<tokio::sync::Notify>>,
    }
    #[async_trait]
    impl ReadService for Reader {
        async fn call(
            &self,
            run: ThreadRun,
            _: &str,
            _: &serde_json::Value,
        ) -> Result<String, String> {
            self.count.fetch_add(1, Ordering::SeqCst);
            if let Some(hold) = &self.hold {
                hold.notified().await
            }
            Ok(format!("{}:{}", run.workspace_id, run.message_id))
        }
    }
    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        host: Arc<ResearchTools>,
        exec: ExecutionRef,
        reader: Arc<Reader>,
    }
    fn fixture(hold: Option<Arc<tokio::sync::Notify>>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path().join("store.sqlite3"),
            steve_store::FnPublisher(|_| {}),
        )
        .unwrap();
        store.ensure_workspace().unwrap();
        let exec = build::exec_job("research-job", "attempt");
        let mut spec = build::research("research-job", "attempt", "question");
        spec.run.policy.tools = GrantId::new("hub").unwrap();
        spec.run.limits.deadline = Timestamp(steve_store::now() + 60_000);
        store.transaction::<_, StoreError>(|tx| {
            tx.execute("INSERT INTO messages(id,workspace_id,space,author,text,created) VALUES ('question','default','steve','me','question',1)", [])?;
            tx.execute("INSERT INTO thread_runs(id,workspace_id,message_id,status,created) VALUES ('run','default','question','running',1)", [])?;
            tx.execute("INSERT INTO runtime_jobs(job_id,kind,purpose,workspace_id,node_id,thread_run_id,created,updated) VALUES ('research-job','research','research','default','local','run',1,1)", [])?;
            tx.execute("INSERT INTO runtime_job_attempts(attempt_id,job_id,ordinal,model,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('attempt','research-job',0,'model','local','op',?1,?2,'digest','running',1,1)", params![serde_json::to_string(&exec).unwrap(), serde_json::to_string(&spec).unwrap()])?;
            Ok(())
        }).unwrap();
        let node = NodeId::new("local").unwrap();
        let inner =
            NodeTools::open(store.clone(), node.clone(), &dir.path().join("tools")).unwrap();
        let host = ResearchTools::new(store.clone(), node, inner);
        let reader = Arc::new(Reader {
            count: AtomicUsize::new(0),
            hold,
        });
        assert!(host.service.set(reader.clone()).is_ok());
        Fixture {
            _dir: dir,
            store,
            host,
            exec,
            reader,
        }
    }
    async fn query(f: &Fixture) -> ToolCall {
        ToolCall {
            id: CallId::new("call").unwrap(),
            execution: f.exec.clone(),
            catalog: f.host.catalog(f.exec.clone()).await.unwrap().revision,
            name: research_mcp::ITEMS_SEARCH.into(),
            args: json!({"query": "task", "workspace": "foreign"}),
            deadline: Timestamp(steve_store::now() + 30_000),
        }
    }

    #[tokio::test]
    async fn only_the_live_research_attempt_on_its_node_has_read_only_tools() {
        let f = fixture(None);
        let catalog = f.host.catalog(f.exec.clone()).await.unwrap();
        assert_eq!(catalog.tools.len(), 5);
        assert!(catalog.tools.iter().all(|t| t.read_only));
        assert_eq!(
            authorize(&f.store, &NodeId::new("foreign").unwrap(), &f.exec)
                .unwrap_err()
                .code,
            ErrorCode::Denied
        );
        let mut q = query(&f).await;
        q.name = "steve_deliver".into();
        assert_eq!(f.host.call(q).await.unwrap_err().code, ErrorCode::Denied);
        assert_eq!(f.reader.count.load(Ordering::SeqCst), 0);
        f.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("UPDATE runtime_jobs SET workspace_id='foreign'", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            f.host.catalog(f.exec).await.unwrap_err().code,
            ErrorCode::Denied
        );
    }

    #[tokio::test]
    async fn research_reads_replay_once_and_argument_changes_conflict() {
        let f = fixture(None);
        let mut q = query(&f).await;
        let first = f.host.call(q.clone()).await.unwrap();
        assert!(
            matches!(&first, ToolResult::Completed { parts, .. } if parts == &vec![Part::Text { text: "default:question".into() }])
        );
        assert_eq!(f.host.call(q.clone()).await.unwrap(), first);
        assert_eq!(f.host.call_status(q.key()).await.unwrap(), first);
        assert_eq!(f.reader.count.load(Ordering::SeqCst), 1);
        q.args = json!({"query": "different"});
        assert_eq!(f.host.call(q).await.unwrap_err().code, ErrorCode::Conflict);
    }

    #[tokio::test]
    async fn cancelled_or_finished_runs_cannot_read_even_cached_results() {
        let f = fixture(None);
        let q = query(&f).await;
        f.host.call(q.clone()).await.unwrap();
        f.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("UPDATE runtime_job_attempts SET cancel_requested=1", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            f.host.call(q.clone()).await.unwrap_err().code,
            ErrorCode::Denied
        );
        assert_eq!(
            f.host.call_status(q.key()).await.unwrap_err().code,
            ErrorCode::Denied
        );
        f.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("UPDATE runtime_job_attempts SET cancel_requested=0", [])?;
                tx.execute("UPDATE thread_runs SET status='done'", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            f.host.catalog(f.exec).await.unwrap_err().code,
            ErrorCode::Denied
        );
        f.host.prune_finished().await.unwrap();
        assert!(f.host.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn concurrent_resends_and_cancel_share_one_read() {
        let hold = Arc::new(tokio::sync::Notify::new());
        let f = fixture(Some(hold.clone()));
        let q = query(&f).await;
        let (host, start) = (f.host.clone(), q.clone());
        let running = tokio::spawn(async move { host.call(start).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while f.reader.count.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            f.host.call(q.clone()).await.unwrap(),
            ToolResult::Running { .. }
        ));
        let cancelled = f.host.cancel_call(q.key()).await.unwrap();
        assert!(matches!(cancelled, ToolResult::Failed { .. }));
        hold.notify_one();
        assert_eq!(running.await.unwrap().unwrap(), cancelled);
        assert_eq!(f.reader.count.load(Ordering::SeqCst), 1);
    }
}

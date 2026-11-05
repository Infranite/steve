//! Fault injection across the Hub and a real Node: the coordinator (`NodeRuntime` on a temp Store)
//! drives a real `LocalNode` on the fake ACP agent (`steve-acp/tests/fake-acp.js`, which logs every
//! message it gets). Each case breaks one step of the way a prompt travels and checks what holds
//! today, whatever the fault:
//!
//! - the agent got each prompt at most once (once if it was accepted at all);
//! - each event position (stream, epoch, seq) is stored once on the Hub, in the inbox and in
//!   `events`, and everything the Node journaled reached the Hub;
//! - an attempt is never run again on its own;
//! - a Steve tool call runs once.
//!
//! Points: before and after the Node accepts (a journal `ops` abort, a lost reply), before and
//! after the harness starts (a `procs` abort, the Node closed and opened again mid-turn), before
//! and after an event is written (a journal `events` abort, a Hub inbox abort), and the Hub
//! committed but the Node never heard the ack (acks dropped, or the Node restarted first).
//! Gaps that later steps close (pruned redelivery, journal identity, quarantine…) are not asserted
//! here; their steps add their cases.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use steve_hub::wire::node_runtime::{Binding, NodeDomain, NodeRuntime, Sink};
use steve_hub::wire::node_tools::{NodeTools, Service};
use steve_node::{LocalNode, NodeConfig, Root};
use steve_node_api::testkit::{FakeNode, Point, build};
use steve_node_api::*;
use steve_store::{FnPublisher, Store, StoreError};

const S: &str = "s1";
const NODE: &str = "local";

// ── the Hub's ends ──

#[derive(Default)]
struct Ends(Mutex<Vec<(String, String, Option<String>)>>);

impl NodeDomain for Ends {
    fn turn_ended(&self, session: &str, stop: &str, error: Option<String>) {
        self.0
            .lock()
            .unwrap()
            .push((session.into(), stop.into(), error));
    }
}

// ── the Node, behind a switch: a reply can be lost, the process closed and opened again ──

/// The real Node as the Hub sees it. Between [`Rig::close_node`] and [`Rig::open_node`] it is
/// offline; [`Faulty::lose_reply`] makes the next answer of a method get lost after the Node
/// decided (the Hub hears `outcome_unknown`).
struct Faulty {
    id: NodeId,
    node: Mutex<Option<LocalNode>>,
    lose: Mutex<Vec<&'static str>>,
}

impl Faulty {
    fn n(&self) -> ApiResult<LocalNode> {
        self.node
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| ApiError::new(ErrorCode::Offline, "the node is down"))
    }

    fn lose_reply(&self, method: &'static str) {
        self.lose.lock().unwrap().push(method);
    }

    fn reply(
        &self,
        method: &'static str,
        op: &OperationId,
        r: ApiResult<Receipt>,
    ) -> ApiResult<Receipt> {
        let mut lose = self.lose.lock().unwrap();
        match lose.iter().position(|m| *m == method) {
            Some(i) if r.is_ok() => {
                lose.remove(i);
                Err(ApiError::uncertain(op, "the reply was lost"))
            }
            _ => r,
        }
    }
}

/// One face of the Node, delegated: commands (their reply possibly lost), then queries.
macro_rules! face {
    ($face:ident { $(cmd $c:ident: $body:ty;)* $(query $q:ident($($a:ident: $t:ty),*) -> $r:ty;)* }) => {
        #[async_trait]
        impl $face for Faulty {
            $(async fn $c(&self, c: Command<$body>) -> ApiResult<Receipt> {
                let op = c.id.clone();
                let r = self.n()?.$c(c).await;
                self.reply(stringify!($c), &op, r)
            })*
            $(async fn $q(&self, $($a: $t),*) -> ApiResult<$r> {
                self.n()?.$q($($a),*).await
            })*
        }
    };
}

face!(SessionExecutor {
    cmd open_session: OpenSession;
    cmd configure_session: ConfigureSession;
    cmd start_turn: TurnSpec;
    cmd release_session: SessionId;
    query session(id: SessionId) -> SessionView;
});
face!(JobExecutor {
    cmd start_job: JobSpec;
});
face!(ExecutionControl {
    cmd cancel: Cancel;
    cmd abandon: Abandon;
    cmd recover: Recover;
    cmd resolve_permission: PermissionResolution;
    query operation(id: OperationId) -> Receipt;
    query snapshot(page: PageRequest) -> Snapshot;
    query streams() -> Vec<StreamHead>;
    query replay(from: Watermark, max_bytes: u32) -> EventBatch;
    query acknowledge(ack: EventAck) -> ();
});
face!(WorkspaceExecutor {
    cmd prepare_workspace: Prepare;
    cmd tidy_workspace: Tidy;
    cmd git_apply: GitApply;
    cmd exec: ExecSpec;
    cmd scan: ScanSpec;
    query workspace(w: WorkspaceRef) -> WorkspaceView;
    query git_facts(q: GitQuery) -> GitFacts;
    query list_dir(path: PathRef, page: PageRequest) -> Page<FileInfo>;
    query list_root(at: RootPath, page: PageRequest) -> Page<FileInfo>;
    query read_file(q: FileRead) -> ReadChunk;
    query scan_page(scan: OperationId, page: PageRequest) -> ScanPage;
});
face!(Discovery {
    cmd import_history: HistoryImport;
    query probe(fresh: bool) -> Capabilities;
    query history(page: PageRequest) -> Page<HistoryEntry>;
    query import_page(import: OperationId, page: PageRequest) -> ImportedPage;
    query locate(source: HistoryRef) -> HistoryLocation;
});

impl Node for Faulty {
    fn id(&self) -> &NodeId {
        &self.id
    }
}

// ── the way back: the Hub commits, the ack may be lost ──

/// Between the Node and the Hub's sink. While `lose_acks` is above zero, a batch the Hub committed
/// comes back with an empty ack (the Node keeps it and sends it again).
struct Gate {
    to: Mutex<Option<std::sync::Weak<NodeRuntime>>>,
    lose_acks: Mutex<usize>,
    lost: AtomicUsize,
    commits: AtomicUsize,
}

#[async_trait]
impl EventSink for Gate {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        let to = self.to.lock().unwrap().clone();
        let Some(to) = to else {
            return Err(ApiError::new(ErrorCode::Offline, "no hub"));
        };
        let ack = Sink(to).commit(batch).await?;
        let lose = {
            let mut n = self.lose_acks.lock().unwrap();
            let lose = *n > 0 && !ack.durable.is_empty();
            if lose {
                *n -= 1;
            }
            lose
        };
        if lose {
            self.lost.fetch_add(1, Ordering::SeqCst);
            return Ok(EventAck { durable: vec![] });
        }
        Ok(ack)
    }
}

// ── Steve's tools, counted ──

fn update_catalog() -> Vec<ToolDescriptor> {
    vec![ToolDescriptor {
        name: "steve_update".into(),
        description: "Update the item.".into(),
        input_schema: serde_json::json!({"type": "object"}),
        read_only: false,
    }]
}

/// Answers every call; counts how many times it ran.
#[derive(Default)]
struct Counted(AtomicUsize);

#[async_trait]
impl Service for Counted {
    fn catalog(&self) -> Vec<ToolDescriptor> {
        update_catalog()
    }
    async fn call(
        &self,
        _: steve_store::Session,
        _: &str,
        _: serde_json::Value,
    ) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("Steve updated the item.".into())
    }
}

/// Starts every call (counted) and never finishes it: a call in flight when the Hub goes away.
#[derive(Default)]
struct Stuck(AtomicUsize);

#[async_trait]
impl Service for Stuck {
    fn catalog(&self) -> Vec<ToolDescriptor> {
        update_catalog()
    }
    async fn call(
        &self,
        _: steve_store::Session,
        _: &str,
        _: serde_json::Value,
    ) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

/// The Hub's tool host as the Node reaches it. The ledger ([`NodeTools`]) runs on a runtime of its
/// own, so [`ToolHost::crash`] ends it mid-call the way the Hub process dies and opens it again on
/// the same file; every [`ToolCall`] the Node sends is kept, so a case can send the very same one
/// (same ToolKey) again.
struct ToolHost {
    store: Store,
    home: PathBuf,
    service: Arc<dyn Service>,
    hub: Mutex<Option<(Arc<NodeTools>, tokio::runtime::Runtime)>>,
    sent: Mutex<Vec<ToolCall>>,
}

impl ToolHost {
    async fn open(store: Store, home: PathBuf, service: Arc<dyn Service>) -> Arc<ToolHost> {
        let host = Arc::new(ToolHost {
            store,
            home,
            service,
            hub: Mutex::new(None),
            sent: Mutex::new(vec![]),
        });
        host.start().await;
        host
    }

    /// Opens the ledger (marking calls left running as unknown) once the last owner let it go.
    async fn start(&self) {
        let t0 = Instant::now();
        let tools = loop {
            match NodeTools::open(self.store.clone(), NodeId::new(NODE).unwrap(), &self.home) {
                Ok(t) => break t,
                Err(e) if e.code == ErrorCode::Busy && t0.elapsed() < Duration::from_secs(20) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(e) => panic!("open the tool ledger: {e:?}"),
            }
        };
        tools.install(self.service.clone()).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        *self.hub.lock().unwrap() = Some((tools, rt));
    }

    /// The Hub process dies (its calls in flight with it) and starts again on the same ledger.
    async fn crash(&self) {
        let (tools, rt) = self.hub.lock().unwrap().take().unwrap();
        let old = Arc::downgrade(&tools);
        drop(tools);
        rt.shutdown_background();
        until("the old tool host gone", || old.upgrade().is_none()).await;
        self.start().await;
    }

    /// The `ToolCall`s the Node sent, in order.
    fn sent(&self) -> Vec<ToolCall> {
        self.sent.lock().unwrap().clone()
    }

    async fn on_hub<T: Send + 'static, F>(
        &self,
        f: impl FnOnce(Arc<NodeTools>) -> F,
    ) -> ApiResult<T>
    where
        F: std::future::Future<Output = ApiResult<T>> + Send + 'static,
    {
        let (tools, handle) = match &*self.hub.lock().unwrap() {
            Some((t, rt)) => (t.clone(), rt.handle().clone()),
            None => return Err(ApiError::new(ErrorCode::Offline, "the Hub is down")),
        };
        handle
            .spawn(f(tools))
            .await
            .unwrap_or_else(|_| Err(ApiError::new(ErrorCode::Offline, "the Hub went down")))
    }
}

impl Drop for ToolHost {
    fn drop(&mut self) {
        // the test's runtime is async: let the Hub's go without blocking on it
        if let Some((_, rt)) = self.hub.lock().unwrap().take() {
            rt.shutdown_background();
        }
    }
}

#[async_trait]
impl HubTools for ToolHost {
    async fn catalog(&self, execution: ExecutionRef) -> ApiResult<Catalog> {
        self.on_hub(|t| async move { t.catalog(execution).await })
            .await
    }
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        self.sent.lock().unwrap().push(q.clone());
        self.on_hub(|t| async move { t.call(q).await }).await
    }
    async fn call_status(&self, key: ToolKey) -> ApiResult<ToolResult> {
        self.on_hub(|t| async move { t.call_status(key).await })
            .await
    }
    async fn cancel_call(&self, key: ToolKey) -> ApiResult<ToolResult> {
        self.on_hub(|t| async move { t.cancel_call(key).await })
            .await
    }
}

// ── the rig ──

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    node: Arc<Faulty>,
    gate: Arc<Gate>,
    rt: Arc<NodeRuntime>,
    ends: Arc<Ends>,
    tools: Mutex<Option<Arc<ToolHost>>>,
}

fn adapter(root: &Path) -> PathBuf {
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
    let path = root.join("adapter.sh");
    let script = format!(
        "#!/bin/sh\nexport FAKE_ACP_LOG='{}'\nexec node '{}' \"$@\"\n",
        root.join("acp.log").display(),
        fake.display()
    );
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

impl Rig {
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        adapter(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        let store = Store::open(root.join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
        store.ensure_workspace().unwrap();
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", [])?;
                tx.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES (?,'default','i1','claude-code','','active',1,1)", [S])?;
                Ok(())
            })
            .unwrap();
        let node = Arc::new(Faulty {
            id: NodeId::new(NODE).unwrap(),
            node: Mutex::new(None),
            lose: Mutex::new(vec![]),
        });
        let ends = Arc::new(Ends::default());
        let rt = NodeRuntime::new(store.clone(), node.clone(), ends.clone());
        let gate = Arc::new(Gate {
            to: Mutex::new(Some(Arc::downgrade(&rt))),
            lose_acks: Mutex::new(0),
            lost: AtomicUsize::new(0),
            commits: AtomicUsize::new(0),
        });
        let r = Rig {
            _dir: dir,
            root,
            store,
            node,
            gate,
            rt,
            ends,
            tools: Mutex::new(None),
        };
        r.open_node().await;
        r.rt.start();
        r
    }

    fn home(&self) -> PathBuf {
        self.root.join("node")
    }

    fn config(&self) -> NodeConfig {
        let root = self.root.clone();
        NodeConfig {
            node: NodeId::new(NODE).unwrap(),
            home: self.home(),
            acp: steve_acp::AcpConfig {
                adapters: HashMap::from([("claude-code".to_string(), root.join("adapter.sh"))]),
                node: "node".into(),
                claude_executable: "claude".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.clone(),
                version: "test".into(),
                idle_release: Duration::from_secs(600),
                retry_delays: vec![],
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: root.join("work"),
                display: "~/work".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 4,
                jobs: 0,
                execs: 0,
            },
        }
    }

    /// The Node process starts (again) on its home: a new incarnation of the same journal.
    async fn open_node(&self) {
        let n = LocalNode::open(self.config()).unwrap();
        n.connect(self.gate.clone(), None);
        let tools = self.tools.lock().unwrap().clone();
        if let Some(t) = tools {
            n.connect_tools(t).await.unwrap();
        }
        *self.node.node.lock().unwrap() = Some(n);
    }

    /// The Node process stops (its harnesses with it); it is offline until opened again.
    async fn close_node(&self) {
        let n = self.node.node.lock().unwrap().take();
        if let Some(n) = n {
            n.shutdown().await;
        }
    }

    /// The Hub process restarts on the same database: a new coordinator that reconciles with the
    /// Node before it sends anything.
    async fn restart_hub(&mut self) {
        let old = Arc::downgrade(&self.rt);
        self.rt = NodeRuntime::new(self.store.clone(), self.node.clone(), self.ends.clone());
        *self.gate.to.lock().unwrap() = Some(Arc::downgrade(&self.rt));
        until("the old coordinator gone", || old.upgrade().is_none()).await;
        self.rt.start();
        until("reconciled", || self.rt.is_reconciled()).await;
    }

    /// Steve's tools for the Node's harnesses, answered by `service`.
    async fn with_tools(&mut self, service: Arc<dyn Service>) -> Arc<ToolHost> {
        let host = ToolHost::open(self.store.clone(), self.home(), service).await;
        *self.tools.lock().unwrap() = Some(host.clone());
        self.node
            .n()
            .unwrap()
            .connect_tools(host.clone())
            .await
            .unwrap();
        host
    }

    /// `start_turn` commands the Node's journal accepted.
    fn journal_starts(&self) -> i64 {
        self.journal()
            .query_row(
                "SELECT COUNT(*) FROM ops WHERE method='start_turn'",
                [],
                |x| x.get(0),
            )
            .unwrap()
    }

    /// The Node's own journal, for triggers and counts.
    fn journal(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.home().join("node.sqlite3")).unwrap()
    }

    /// The Hub's database, for triggers.
    fn hub_db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.join("steve.sqlite3")).unwrap()
    }

    fn bind(&self) {
        self.bind_with(build::policy());
    }

    fn bind_with(&self, policy: ExecutionPolicy) {
        self.rt
            .bind(&Binding {
                session: S.into(),
                resource: "item:i1".into(),
                prepare: PrepareKind::Scratch,
                choice: build::choice(),
                policy,
            })
            .unwrap();
    }

    fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    fn phase(&self) -> String {
        self.q(&format!(
            "SELECT phase FROM runtime_sessions WHERE session_id='{S}'"
        ))
    }

    fn ends(&self) -> Vec<(String, String, Option<String>)> {
        self.ends.0.lock().unwrap().clone()
    }

    fn acp(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.root.join("acp.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Prompts the agent got that carry `token`.
    fn prompts(&self, token: &str) -> usize {
        self.acp()
            .iter()
            .filter(|m| m["method"] == "session/prompt" && m.to_string().contains(token))
            .count()
    }

    /// Adapter processes started.
    fn spawns(&self) -> usize {
        self.acp()
            .iter()
            .filter(|m| m.get("spawn").is_some())
            .count()
    }

    fn starts(&self) -> i64 {
        self.q("SELECT COUNT(*) FROM node_commands WHERE method='start_turn'")
    }

    fn attempts(&self) -> Vec<String> {
        self.store
            .reader(|c| {
                let mut st =
                    c.prepare("SELECT state FROM runtime_attempts ORDER BY created, rowid")?;
                let v = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap()
    }

    /// The Hub's events from the Node, counted by kind.
    fn kinds(&self) -> String {
        self.q("SELECT COALESCE(group_concat(kind||':'||n), '') FROM (SELECT kind, COUNT(*) n FROM events WHERE source_node IS NOT NULL GROUP BY kind ORDER BY kind)")
    }

    async fn ready(&self) {
        self.bind();
        until("ready", || self.phase() == "ready").await;
    }

    /// Each position stored once on the Hub; once the Node is quiet, everything it journaled is
    /// durable on the Hub and acknowledged on the Node.
    async fn stored_once(&self) {
        let dup_inbox: i64 = self.q("SELECT COUNT(*) FROM (SELECT 1 FROM node_event_inbox GROUP BY node_id,stream,epoch,seq HAVING COUNT(*)>1)");
        assert_eq!(dup_inbox, 0, "an inbox position twice");
        let dup_ids: i64 = self.q("SELECT COUNT(*) FROM (SELECT 1 FROM node_event_inbox GROUP BY node_id,event_id HAVING COUNT(*)>1)");
        assert_eq!(dup_ids, 0, "an event id twice");
        let dup_events: i64 = self.q("SELECT COUNT(*) FROM (SELECT 1 FROM events WHERE source_node IS NOT NULL GROUP BY source_node,source_stream,source_epoch,source_seq HAVING COUNT(*)>1)");
        assert_eq!(dup_events, 0, "an event position twice in events");
        until("the Node's journal acknowledged", || {
            let j = self.journal();
            let mut st = j
                .prepare("SELECT stream, epoch, produced, acknowledged FROM streams")
                .unwrap();
            let heads: Vec<(String, String, i64, i64)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            !heads.is_empty()
                && heads.iter().all(|(stream, epoch, produced, acked)| {
                    let durable: i64 = self.q(&format!(
                        "SELECT COALESCE(MAX(durable),0) FROM node_streams WHERE node_id='{NODE}' AND stream='{stream}' AND epoch='{epoch}'"
                    ));
                    produced == acked && durable == *produced
                })
        })
        .await;
    }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── no fault: the baseline every case is measured against ──

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clean_turn_runs_once_and_every_event_is_stored_once() {
    let r = Rig::new().await;
    r.ready().await;
    r.rt.prompt(S, "@say clean", "user", &[]).unwrap();
    until("the end", || !r.ends().is_empty()).await;
    assert_eq!(r.ends()[0].1, "end_turn");
    assert_eq!(r.prompts("clean"), 1);
    assert_eq!(r.attempts(), ["finished"]);
    r.stored_once().await;
    let texts: i64 =
        r.q("SELECT COUNT(*) FROM events WHERE kind='text' AND source_node IS NOT NULL");
    assert_eq!(texts, 1);
}

// ── before and after the Node accepts ──

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_the_journal_cannot_record_never_reaches_the_agent() {
    let r = Rig::new().await;
    r.ready().await;
    let j = r.journal();
    j.execute_batch("CREATE TRIGGER fail_accept BEFORE INSERT ON ops WHEN NEW.method='start_turn' BEGIN SELECT RAISE(ABORT,'injected disk failure'); END").unwrap();
    r.rt.prompt(S, "@say refused", "user", &[]).unwrap();
    until("the end", || !r.ends().is_empty()).await;
    assert_eq!(r.prompts("refused"), 0, "nothing was accepted, nothing ran");
    assert_eq!(r.attempts().len(), 1, "no new attempt on its own");
    assert_eq!(r.starts(), 1, "sent once, not again");
    // the journal works again: the next message runs, the refused one stays gone
    j.execute_batch("DROP TRIGGER fail_accept").unwrap();
    until("ready", || r.phase() == "ready").await;
    r.rt.prompt(S, "@say after", "user", &[]).unwrap();
    until("the second end", || r.ends().len() == 2).await;
    assert_eq!((r.prompts("refused"), r.prompts("after")), (0, 1));
    r.stored_once().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_reply_after_accept_is_the_same_start_again_never_a_rerun() {
    let r = Rig::new().await;
    r.ready().await;
    r.node.lose_reply("start_turn");
    r.rt.prompt(S, "@say once", "user", &[]).unwrap();
    until("the end", || !r.ends().is_empty()).await;
    assert_eq!(r.ends()[0].1, "end_turn");
    assert_eq!(r.prompts("once"), 1);
    assert_eq!(r.attempts(), ["finished"]);
    assert_eq!(r.starts(), 1);
    assert_eq!(r.journal_starts(), 1);
    r.stored_once().await;
}

// ── the Hub committed, the Node did not hear it ──

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_acks_bring_the_same_events_again_and_they_are_stored_once() {
    let r = Rig::new().await;
    r.ready().await;
    *r.gate.lose_acks.lock().unwrap() = 3;
    const PROMPT: &str = "@say one @tool execute Bash @say two";
    r.rt.prompt(S, PROMPT, "user", &[]).unwrap();
    until("the end", || !r.ends().is_empty()).await;
    until("acks lost", || r.gate.lost.load(Ordering::SeqCst) == 3).await;
    r.stored_once().await;
    assert_eq!(r.prompts("@say one"), 1);
    assert_eq!(r.attempts(), ["finished"]);
    // what a run with every ack heard stores
    let clean = Rig::new().await;
    clean.ready().await;
    clean.rt.prompt(S, PROMPT, "user", &[]).unwrap();
    until("the clean end", || !clean.ends().is_empty()).await;
    assert_eq!(r.kinds(), clean.kinds());
}

// ── before and after the harness starts ──

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_harness_whose_process_cannot_be_registered_never_starts() {
    let r = Rig::new().await;
    let j = r.journal();
    j.execute_batch("CREATE TRIGGER fail_proc BEFORE INSERT ON procs BEGIN SELECT RAISE(ABORT,'registration failure'); END").unwrap();
    r.bind();
    r.rt.prompt(S, "@say first", "user", &[]).unwrap();
    until("the open fails", || !r.ends().is_empty()).await;
    assert_eq!(r.ends()[0].1, "error");
    assert_eq!(r.spawns(), 0, "no adapter before its process is durable");
    assert_eq!(r.prompts("first"), 0);
    assert!(r.attempts().is_empty(), "nothing was started");
    // registration works again: the next message opens the session, and what waited goes with it
    j.execute_batch("DROP TRIGGER fail_proc").unwrap();
    r.rt.prompt(S, "@say second", "user", &[]).unwrap();
    until("the turn ends", || r.ends().len() == 2).await;
    assert_eq!(r.ends()[1].1, "end_turn");
    assert_eq!(r.spawns(), 1);
    assert_eq!((r.prompts("first"), r.prompts("second")), (1, 1));
    assert_eq!(r.attempts(), ["finished"], "one turn, both messages");
    r.stored_once().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_closed_mid_turn_leaves_the_attempt_unknown_never_rerun() {
    let mut r = Rig::new().await;
    r.ready().await;
    r.rt.prompt(S, "@sleep 60000 @say mid", "user", &[])
        .unwrap();
    until("the prompt", || r.prompts("mid") == 1).await;
    r.close_node().await;
    r.open_node().await;
    // the Hub restarts too and reconciles with the Node
    r.restart_hub().await;
    until("unknown", || r.attempts() == ["unknown"]).await;
    assert_eq!(r.prompts("mid"), 1, "never prompted again");
    assert_eq!(r.journal_starts(), 1, "never started again");
    // Stop gives it up; the next message is a new turn
    assert!(r.rt.cancel(S).unwrap());
    until("abandoned", || r.attempts() == ["finished"]).await;
    // the restarted Node let the session go: the next message opens it again
    r.rt.prompt(S, "@say next", "user", &[]).unwrap();
    until("the next turn", || r.attempts() == ["finished", "finished"]).await;
    assert_eq!((r.prompts("mid"), r.prompts("next")), (1, 1));
    r.stored_once().await;
}

// ── before and after an event is written ──

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_event_the_journal_cannot_write_never_reaches_the_hub_half_done() {
    let mut r = Rig::new().await;
    r.ready().await;
    let j = r.journal();
    j.execute_batch("CREATE TRIGGER fail_ev BEFORE INSERT ON events WHEN json_extract(NEW.event,'$.body.type')='text_delta' BEGIN SELECT RAISE(ABORT,'injected disk failure'); END").unwrap();
    r.rt.prompt(S, "@say unwritten", "user", &[]).unwrap();
    until("the prompt", || r.prompts("unwritten") == 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let texts: i64 = r.q("SELECT COUNT(*) FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='text_delta'");
    assert_eq!(texts, 0, "nothing the journal did not keep");
    // the Node faulted; it and the Hub come back
    j.execute_batch("DROP TRIGGER fail_ev").unwrap();
    r.close_node().await;
    r.open_node().await;
    r.restart_hub().await;
    until("unknown", || r.attempts() == ["unknown"]).await;
    assert_eq!(r.prompts("unwritten"), 1, "never prompted again");
    assert_eq!(r.journal_starts(), 1);
    r.stored_once().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_batch_the_hub_cannot_take_comes_again_and_is_stored_once() {
    let r = Rig::new().await;
    r.ready().await;
    let hub = r.hub_db();
    hub.execute_batch("CREATE TRIGGER fail_in BEFORE INSERT ON node_event_inbox WHEN json_extract(NEW.payload,'$.body.type')='text_delta' BEGIN SELECT RAISE(ABORT,'injected hub failure'); END").unwrap();
    r.rt.prompt(S, "@say refused @say again", "user", &[])
        .unwrap();
    until("the prompt", || r.prompts("refused") == 1).await;
    let before = r.gate.commits.load(Ordering::SeqCst);
    until("sent again", || {
        r.gate.commits.load(Ordering::SeqCst) >= before + 2
    })
    .await;
    assert!(
        r.ends().is_empty(),
        "the end waits behind the refused events"
    );
    let inbox: i64 = r.q("SELECT COUNT(*) FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='text_delta'");
    assert_eq!(inbox, 0, "the whole batch was refused");
    hub.execute_batch("DROP TRIGGER fail_in").unwrap();
    until("the end", || !r.ends().is_empty()).await;
    assert_eq!(r.ends()[0].1, "end_turn");
    assert_eq!(r.prompts("refused"), 1);
    assert_eq!(r.attempts(), ["finished"]);
    r.stored_once().await;
    let text: String = r.q("SELECT group_concat(json_extract(payload,'$.body.text'),'') FROM (SELECT payload FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='text_delta' ORDER BY seq)");
    assert_eq!(text, "refusedagain");
}

// ── the Hub committed, the Node restarted before it heard the ack ──

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_restarted_before_its_ack_sends_again_and_nothing_is_stored_twice() {
    let r = Rig::new().await;
    r.ready().await;
    *r.gate.lose_acks.lock().unwrap() = usize::MAX;
    r.rt.prompt(S, "@say unheard", "user", &[]).unwrap();
    until("the end", || !r.ends().is_empty()).await;
    let kinds = r.kinds();
    let inbox: i64 = r.q("SELECT COUNT(*) FROM node_event_inbox");
    r.close_node().await;
    *r.gate.lose_acks.lock().unwrap() = 0;
    r.open_node().await;
    r.stored_once().await;
    assert_eq!(r.kinds(), kinds, "the Hub's events are the same");
    let again: i64 = r.q("SELECT COUNT(*) FROM node_event_inbox");
    assert!(again >= inbox, "nothing lost");
    assert_eq!(r.prompts("unheard"), 1);
    assert_eq!(r.attempts(), ["finished"]);
}

// ── Steve's tools ──

/// A turn that may use Steve's tools, its agent waiting; the tool endpoint and token it was given.
async fn tool_turn(r: &Rig) -> (String, String) {
    let mut policy = build::policy();
    policy.tools = GrantId::new("hub").unwrap();
    r.bind_with(policy);
    until("ready", || r.phase() == "ready").await;
    r.rt.prompt(S, "@sleep 60000 @say tool", "user", &[])
        .unwrap();
    until("the prompt", || r.prompts("tool") == 1).await;
    let new = r
        .acp()
        .into_iter()
        .find(|m| m["method"] == "session/new")
        .unwrap();
    let server = &new["params"]["mcpServers"][0];
    let url = server["url"].as_str().unwrap().to_string();
    let token = server["headers"][0]["value"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .to_string();
    (url, token)
}

fn update_call() -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "steve_update", "arguments": {"now": "x"}}})
}

/// The Node, then the Hub, restart under a turn; the attempt is unknown and never run again.
async fn restart_both(r: &mut Rig, host: &ToolHost) {
    r.close_node().await;
    host.crash().await;
    r.open_node().await;
    r.restart_hub().await;
    until("unknown", || r.attempts() == ["unknown"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_call_runs_once_through_a_node_restart() {
    let mut r = Rig::new().await;
    let service = Arc::new(Counted::default());
    let host = r.with_tools(service.clone()).await;
    let (url, token) = tool_turn(&r).await;
    let (_, called) = mcp(&url, &token, update_call()).await;
    assert_eq!(
        called["result"]["content"][0]["text"], "Steve updated the item.",
        "{called}"
    );
    assert_eq!(service.0.load(Ordering::SeqCst), 1);
    let sent = host.sent();
    assert_eq!(sent.len(), 1, "the Node sent the call once");
    let q = sent[0].clone();
    let done = host.call(q.clone()).await.unwrap();
    assert!(
        matches!(&done, ToolResult::Completed { parts, .. }
            if parts == &[Part::Text { text: "Steve updated the item.".into() }]),
        "the same ToolKey again is the saved result: {done:?}"
    );
    assert_eq!(service.0.load(Ordering::SeqCst), 1, "not run again");

    restart_both(&mut r, &host).await;
    // the same ToolKey after both restarts (a reopened ledger): still the saved result
    assert_eq!(host.call(q.clone()).await.unwrap(), done);
    assert_eq!(host.call_status(q.key()).await.unwrap(), done);
    // the harness sends its request again: the session that issued the token is gone with the
    // attempt, so nothing runs (the Node mints a new CallId per request, never the JSON-RPC id)
    let before = host.sent().len();
    let (status, again) = mcp(&url, &token, update_call()).await;
    assert_eq!(status, 401, "{again}");
    assert_eq!(
        host.sent().len(),
        before,
        "the harness resend never reached the Hub"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(service.0.load(Ordering::SeqCst), 1);
    assert_eq!(r.prompts("tool"), 1);
    r.stored_once().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_call_in_flight_when_both_restart_is_unknown_never_rerun() {
    let mut r = Rig::new().await;
    let service = Arc::new(Stuck::default());
    let host = r.with_tools(service.clone()).await;
    let (url, token) = tool_turn(&r).await;
    let waiting = tokio::spawn({
        let (url, token) = (url.clone(), token.clone());
        async move { mcp(&url, &token, update_call()).await }
    });
    until("the call running", || service.0.load(Ordering::SeqCst) == 1).await;
    let q = host.sent()[0].clone();
    assert!(matches!(
        host.call(q.clone()).await.unwrap(),
        ToolResult::Running { .. }
    ));

    restart_both(&mut r, &host).await;
    let (_, lost) = waiting.await.unwrap();
    assert_eq!(
        lost["result"]["isError"], true,
        "the harness hears an error: {lost}"
    );
    let unknown = host.call(q.clone()).await.unwrap();
    assert!(
        matches!(unknown, ToolResult::Unknown { .. }),
        "running when the Hub died: unknown, {unknown:?}"
    );
    assert!(matches!(
        host.call_status(q.key()).await.unwrap(),
        ToolResult::Unknown { .. }
    ));
    let before = host.sent().len();
    let (status, again) = mcp(&url, &token, update_call()).await;
    assert_eq!(status, 401, "{again}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(service.0.load(Ordering::SeqCst), 1, "never invoked again");
    assert_eq!(
        host.sent().len(),
        before,
        "the harness resend never reached the Hub"
    );
    assert_eq!(r.prompts("tool"), 1);
    r.stored_once().await;
}

/// A JSON-RPC POST to an MCP endpoint over plain HTTP/1.1.
async fn mcp(url: &str, token: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
    let b = body.to_string();
    let req = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMcp-Protocol-Version: 2025-06-18\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status: u16 = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("");
    let v = body
        .find('{')
        .and_then(|i| body.rfind('}').map(|j| &body[i..=j]))
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, v)
}

// ── the contract's fake: the Hub holds a batch before the Node lets it go ──

#[tokio::test]
async fn the_hub_has_a_batch_before_the_node_drops_its_copy() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
    store.ensure_workspace().unwrap();
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)", [S])?;
            Ok(())
        })
        .unwrap();
    let count = |store: &Store, sql: &str| -> i64 {
        store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    };
    let node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Ends::default()));
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    rt.bind(&Binding {
        session: S.into(),
        resource: format!("item:{S}"),
        prepare: PrepareKind::Scratch,
        choice: build::choice(),
        policy: build::policy(),
    })
    .unwrap();
    rt.prompt(S, "hello", "user", &[]).unwrap();
    until("started", || {
        count(
            &store,
            "SELECT COUNT(*) FROM runtime_attempts WHERE state='running'",
        ) == 1
            && node.unacked() == 0
    })
    .await;
    let exec: ExecutionRef = serde_json::from_str(
        &store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT execution FROM runtime_attempts",
                    [],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .unwrap(),
    )
    .unwrap();
    // at each point: the Hub's text rows, and what the Node still keeps
    let seen: Arc<Mutex<Vec<(Point, i64, usize)>>> = Arc::default();
    let (st, weak, log) = (store.clone(), Arc::downgrade(&node), seen.clone());
    node.set_hook(move |p| {
        if matches!(p, Point::BeforeCommit | Point::AfterCommit | Point::BeforeAck) {
            let held = count(&st, "SELECT COUNT(*) FROM node_event_inbox WHERE json_extract(payload,'$.body.type')='text_delta'");
            let kept = weak.upgrade().map(|n| n.unacked()).unwrap_or(0);
            log.lock().unwrap().push((p, held, kept));
        }
    });
    node.emit(
        Subject::Execution(exec),
        EventBody::TextDelta {
            message: "m1".into(),
            block: "b1".into(),
            utf8_offset: 0,
            text: "hi".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (Point::BeforeCommit, 0, 1),
            (Point::AfterCommit, 1, 1),
            (Point::BeforeAck, 1, 1),
        ],
        "committed on the Hub while the Node still keeps it"
    );
    assert_eq!(node.unacked(), 0, "let go once acknowledged");
}

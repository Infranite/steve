//! An in-memory Node for tests of the Hub side. It keeps the contract's rules, so a Hub that breaks
//! them fails here and not on a real machine: commands are deduplicated by operation id and content,
//! late commands are stale, an attempt never runs twice, a session runs one attempt at a time, a
//! permission answer applies only to the request still waiting. Tests drive what the harness would do
//! ([`FakeNode::emit`], [`FakeNode::finish`], [`FakeNode::ask_permission`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::blob::*;
use crate::common::*;
use crate::discovery::*;
use crate::events::*;
use crate::ids::*;
use crate::ports::*;
use crate::run::*;
use crate::workspace::*;

fn id<T: TryFrom<String>>(s: impl Into<String>) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(s.into()).expect("valid test id")
}

#[derive(Default)]
struct State {
    now: i64,
    /// operation id → (content digest, current receipt)
    inbox: HashMap<OperationId, (Digest, Receipt)>,
    /// attempt → the operation that runs it
    attempts: HashMap<AttemptId, OperationId>,
    /// session → its running attempt
    active: HashMap<SessionId, ExecutionRef>,
    sessions: HashMap<SessionId, SessionView>,
    /// waiting permission requests
    waiting: HashMap<PermissionId, PermissionRequest>,
    workspaces: HashMap<WorkspaceId, WorkspaceView>,
    /// stream → (epoch, next seq, unacknowledged events)
    streams: BTreeMap<StreamId, (Epoch, u64, Vec<NodeEvent>)>,
    /// every accepted command, in order: (method, operation id)
    log: Vec<(&'static str, OperationId)>,
    next_event: u64,
}

pub struct FakeNode {
    node: NodeId,
    st: Mutex<State>,
    sink: Mutex<Option<Arc<dyn EventSink>>>,
}

impl FakeNode {
    pub fn new(node: &str) -> Arc<Self> {
        Arc::new(FakeNode {
            node: id(node),
            st: Mutex::new(State {
                now: 1_000,
                ..Default::default()
            }),
            sink: Mutex::new(None),
        })
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where events go (the Hub under test). Without one they are only kept for `replay`.
    pub fn connect(&self, sink: Arc<dyn EventSink>) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    /// The Node's clock (Unix ms) that `accept_before` is checked against.
    pub fn set_now(&self, ms: i64) {
        self.st().now = ms;
    }

    /// Accepted commands in order, by method.
    pub fn log(&self) -> Vec<(&'static str, OperationId)> {
        self.st().log.clone()
    }

    pub fn receipt(&self, op: &OperationId) -> Option<Receipt> {
        self.st().inbox.get(op).map(|(_, r)| r.clone())
    }

    /// The command inbox: same id and content → the receipt it already has; same id, other content →
    /// conflict; past its window → stale. `start` runs only for a new command.
    fn accept<T: serde::Serialize>(
        &self,
        method: &'static str,
        c: &Command<T>,
        start: impl FnOnce(&mut State) -> ApiResult<Observed>,
    ) -> ApiResult<Receipt> {
        let digest = c.digest();
        let mut st = self.st();
        if let Some((d, r)) = st.inbox.get(&c.id) {
            return if *d == digest {
                Ok(r.clone())
            } else {
                Err(ApiError::new(ErrorCode::Conflict, "same operation, other content").on(&c.id))
            };
        }
        if st.now > c.accept_before.0 {
            return Err(ApiError::new(ErrorCode::Stale, "past accept_before").on(&c.id));
        }
        let state = start(&mut st)?;
        let r = Receipt {
            operation: c.id.clone(),
            state,
        };
        st.inbox.insert(c.id.clone(), (digest, r.clone()));
        st.log.push((method, c.id.clone()));
        Ok(r)
    }

    fn stream_of(subject: &Subject) -> StreamId {
        match subject {
            Subject::Session { session } => id(format!("session:{session}")),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => {
                id(format!("session:{session}"))
            }
            Subject::Execution(ExecutionRef::Job { job, .. }) => id(format!("job:{job}")),
            Subject::Operation { operation } => id(format!("op:{operation}")),
        }
    }

    /// Number an event in its stream and keep it until acknowledged.
    fn record(st: &mut State, subject: Subject, body: EventBody) -> NodeEvent {
        let stream = Self::stream_of(&subject);
        st.next_event += 1;
        let n = st.next_event;
        let at = Timestamp(st.now);
        let entry = st
            .streams
            .entry(stream.clone())
            .or_insert_with(|| (id("e1"), 1, vec![]));
        let ev = NodeEvent {
            schema: crate::EVENT_SCHEMA,
            id: id(format!("ev_{n}")),
            at,
            cursor: Watermark {
                stream,
                epoch: entry.0.clone(),
                seq: entry.1,
            },
            subject,
            body,
        };
        entry.1 += 1;
        entry.2.push(ev.clone());
        ev
    }

    /// Send events to the sink and drop what it acknowledged.
    async fn deliver(&self, events: Vec<NodeEvent>) -> ApiResult<()> {
        let sink = self.sink.lock().unwrap().clone();
        let Some(sink) = sink else { return Ok(()) };
        let ack = sink.commit(EventBatch { events }).await?;
        self.drop_acked(&ack);
        Ok(())
    }

    fn drop_acked(&self, ack: &EventAck) {
        let mut st = self.st();
        for w in &ack.durable {
            if let Some((epoch, _, kept)) = st.streams.get_mut(&w.stream)
                && *epoch == w.epoch
            {
                kept.retain(|e| e.cursor.seq > w.seq);
            }
        }
    }

    /// Events not yet acknowledged, per stream.
    pub fn unacked(&self) -> usize {
        self.st().streams.values().map(|(_, _, k)| k.len()).sum()
    }

    /// What the harness reports while it runs (text, tools, usage…).
    pub async fn emit(&self, subject: Subject, body: EventBody) -> ApiResult<()> {
        let ev = Self::record(&mut self.st(), subject, body);
        self.deliver(vec![ev]).await
    }

    /// The harness asks before a tool runs: the attempt waits until the Hub answers.
    pub async fn ask_permission(&self, req: PermissionRequest) -> ApiResult<()> {
        let ev = {
            let mut st = self.st();
            let op = st.attempts.get(req.execution.attempt()).cloned();
            if let Some(op) = op
                && let Some((_, r)) = st.inbox.get_mut(&op)
            {
                let mut on = match &r.state {
                    Observed::Waiting { on } => on.clone(),
                    _ => vec![],
                };
                on.push(WaitRef::Permission(req.id.clone()));
                r.state = Observed::Waiting { on };
            }
            st.waiting.insert(req.id.clone(), req.clone());
            Self::record(
                &mut st,
                Subject::Execution(req.execution.clone()),
                EventBody::PermissionRequested { request: req },
            )
        };
        self.deliver(vec![ev]).await
    }

    /// The attempt ends: its receipt is final, the session is free again.
    pub async fn finish(&self, exec: &ExecutionRef, completion: Completion) -> ApiResult<()> {
        let ev = {
            let mut st = self.st();
            let op = st.attempts.get(exec.attempt()).cloned();
            if let Some(op) = op
                && let Some((_, r)) = st.inbox.get_mut(&op)
            {
                r.state = Observed::Finished {
                    outcome: completion.outcome,
                    result: Some(OperationResult::Execution(completion.clone())),
                    error: completion.error.clone(),
                };
            }
            if let ExecutionRef::Turn { session, .. } = exec {
                st.active.remove(session);
                if let Some(v) = st.sessions.get_mut(session) {
                    v.phase = SessionPhase::Ready;
                    v.active = None;
                }
            }
            let expired: Vec<PermissionId> = st
                .waiting
                .iter()
                .filter(|(_, r)| r.execution == *exec)
                .map(|(k, _)| k.clone())
                .collect();
            for p in &expired {
                st.waiting.remove(p);
            }
            let mut evs: Vec<NodeEvent> = expired
                .into_iter()
                .map(|p| {
                    Self::record(
                        &mut st,
                        Subject::Execution(exec.clone()),
                        EventBody::PermissionExpired { id: p },
                    )
                })
                .collect();
            evs.push(Self::record(
                &mut st,
                Subject::Execution(exec.clone()),
                EventBody::AttemptEnded { completion },
            ));
            evs
        };
        self.deliver(ev).await
    }

    /// After a crash the Node cannot tell what an attempt did.
    pub fn lose_track(&self, exec: &ExecutionRef) {
        let mut st = self.st();
        if let Some(op) = st.attempts.get(exec.attempt()).cloned()
            && let Some((_, r)) = st.inbox.get_mut(&op)
        {
            r.state = Observed::Unknown {
                reason: "node restarted".into(),
            };
        }
    }

    fn start_attempt(
        st: &mut State,
        op: &OperationId,
        exec: ExecutionRef,
        session: Option<&SessionId>,
    ) -> ApiResult<Observed> {
        let attempt = exec.attempt().clone();
        if let Some(other) = st.attempts.get(&attempt)
            && other != op
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "this attempt already runs under another operation",
            )
            .on(op));
        }
        if let Some(s) = session {
            match st.sessions.get(s) {
                None => return Err(ApiError::new(ErrorCode::Invalid, "session not open")),
                Some(_) if st.active.contains_key(s) => {
                    return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
                }
                Some(_) => {}
            }
            st.active.insert(s.clone(), exec.clone());
            if let Some(v) = st.sessions.get_mut(s) {
                v.phase = SessionPhase::Active;
                v.active = Some(exec.clone());
            }
        }
        st.attempts.insert(attempt, op.clone());
        Ok(Observed::Running)
    }
}

fn unsupported<T>(what: &str) -> ApiResult<T> {
    Err(ApiError::new(
        ErrorCode::Unsupported,
        format!("fake node: {what}"),
    ))
}

#[async_trait]
impl SessionExecutor for FakeNode {
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt> {
        self.accept("open_session", &c, |st| {
            let view = SessionView {
                id: c.body.session.clone(),
                revision: id("r1"),
                phase: SessionPhase::Ready,
                active: None,
                effective: c.body.choice.clone(),
                resume: Some(id(format!("resume_{}", c.body.session))),
            };
            st.sessions.insert(c.body.session.clone(), view.clone());
            Ok(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Session(view)),
                error: None,
            })
        })
    }
    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt> {
        self.accept("configure_session", &c, |st| {
            if st.active.contains_key(&c.body.session) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            let Some(v) = st.sessions.get_mut(&c.body.session) else {
                return Err(ApiError::new(ErrorCode::Invalid, "session not open"));
            };
            v.effective = c.body.choice.clone();
            Ok(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Session(v.clone())),
                error: None,
            })
        })
    }
    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt> {
        let exec = ExecutionRef::Turn {
            session: c.body.session.clone(),
            turn: c.body.turn.clone(),
            attempt: c.body.run.attempt.clone(),
        };
        let fresh = !self.st().inbox.contains_key(&c.id);
        let r = self.accept("start_turn", &c, |st| {
            Self::start_attempt(st, &c.id, exec.clone(), Some(&c.body.session))
        })?;
        if fresh {
            self.emit(Subject::Execution(exec), EventBody::AttemptStarted)
                .await?;
        }
        Ok(r)
    }
    async fn session(&self, s: SessionId) -> ApiResult<SessionView> {
        match self.st().sessions.get(&s) {
            Some(v) => Ok(v.clone()),
            None => Err(ApiError::new(ErrorCode::Invalid, "no such session")),
        }
    }
    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt> {
        self.accept("release_session", &c, |st| {
            if st.active.contains_key(&c.body) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            if let Some(v) = st.sessions.get_mut(&c.body) {
                v.phase = SessionPhase::Released;
            }
            Ok(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Unit),
                error: None,
            })
        })
    }
}

#[async_trait]
impl JobExecutor for FakeNode {
    async fn start_job(&self, c: Command<JobSpec>) -> ApiResult<Receipt> {
        let exec = ExecutionRef::Job {
            job: c.body.job.clone(),
            attempt: c.body.run.attempt.clone(),
        };
        let fresh = !self.st().inbox.contains_key(&c.id);
        let r = self.accept("start_job", &c, |st| {
            Self::start_attempt(st, &c.id, exec.clone(), None)
        })?;
        if fresh {
            self.emit(Subject::Execution(exec), EventBody::AttemptStarted)
                .await?;
        }
        Ok(r)
    }
}

#[async_trait]
impl ExecutionControl for FakeNode {
    async fn operation(&self, op: OperationId) -> ApiResult<Receipt> {
        self.receipt(&op)
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation").on(&op))
    }
    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        self.accept("cancel", &c, |st| match st.inbox.get(&target) {
            Some(_) => Ok(Observed::Accepted),
            None => Err(ApiError::new(ErrorCode::Invalid, "no such operation")),
        })
    }
    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt> {
        let res = c.body.clone();
        let r = self.accept("resolve_permission", &c, |st| {
            let Some(req) = st.waiting.get(&res.id) else {
                return Err(ApiError::new(
                    ErrorCode::Stale,
                    "nobody waits for this answer",
                ));
            };
            if req.execution != res.execution || req.digest != res.action_digest {
                return Err(ApiError::new(
                    ErrorCode::Stale,
                    "the answer is for another attempt or action",
                ));
            }
            let exec = req.execution.clone();
            st.waiting.remove(&res.id);
            if let Some(op) = st.attempts.get(exec.attempt()).cloned()
                && let Some((_, r)) = st.inbox.get_mut(&op)
                && let Observed::Waiting { on } = &r.state
            {
                let on: Vec<WaitRef> = on
                    .iter()
                    .filter(|w| **w != WaitRef::Permission(res.id.clone()))
                    .cloned()
                    .collect();
                r.state = if on.is_empty() {
                    Observed::Running
                } else {
                    Observed::Waiting { on }
                };
            }
            Ok(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Unit),
                error: None,
            })
        })?;
        let ev = Self::record(
            &mut self.st(),
            Subject::Execution(res.execution.clone()),
            EventBody::PermissionResolved { resolution: res },
        );
        self.deliver(vec![ev]).await?;
        Ok(r)
    }
    async fn snapshot(&self, _page: PageRequest) -> ApiResult<Page<Receipt>> {
        let st = self.st();
        let mut items: Vec<Receipt> = st.inbox.values().map(|(_, r)| r.clone()).collect();
        items.sort_by(|a, b| a.operation.cmp(&b.operation));
        Ok(Page { items, next: None })
    }
    async fn replay(&self, from: Watermark, _max_bytes: u32) -> ApiResult<EventBatch> {
        let st = self.st();
        let Some((epoch, _, kept)) = st.streams.get(&from.stream) else {
            return Ok(EventBatch::default());
        };
        if *epoch != from.epoch {
            return Err(ApiError::new(ErrorCode::Gap, "another stream epoch"));
        }
        if kept.first().is_some_and(|e| e.cursor.seq > from.seq + 1) {
            return Err(ApiError::new(ErrorCode::Gap, "past retention"));
        }
        Ok(EventBatch {
            events: kept
                .iter()
                .filter(|e| e.cursor.seq > from.seq)
                .cloned()
                .collect(),
        })
    }
    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()> {
        self.drop_acked(&ack);
        Ok(())
    }
}

#[async_trait]
impl WorkspaceExecutor for FakeNode {
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        let node = self.node.clone();
        self.accept("prepare_workspace", &c, |st| {
            let wid: WorkspaceId = id(format!("w_{}", c.body.key));
            let branch = match &c.body.kind {
                PrepareKind::Worktree { branch, .. } => Some(branch.clone()),
                _ => None,
            };
            let view = st
                .workspaces
                .entry(wid.clone())
                .or_insert_with(|| WorkspaceView {
                    workspace: WorkspaceRef {
                        node: node.clone(),
                        id: wid.clone(),
                    },
                    revision: id("r1"),
                    usable: true,
                    branch,
                    display: format!("fake/{}", c.body.key),
                })
                .clone();
            Ok(Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Workspace(view)),
                error: None,
            })
        })
    }
    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        if w.node != self.node {
            return Err(ApiError::new(ErrorCode::Denied, "another node's workspace"));
        }
        self.st()
            .workspaces
            .get(&w.id)
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))
    }
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.accept("tidy_workspace", &c, |st| {
            match st.workspaces.get(&c.body.workspace.id) {
                Some(v) if v.revision != c.body.expected => {
                    Err(ApiError::new(ErrorCode::Conflict, "changed since"))
                }
                Some(_) => {
                    st.workspaces.remove(&c.body.workspace.id);
                    Ok(Observed::Finished {
                        outcome: Outcome::Succeeded,
                        result: Some(OperationResult::Unit),
                        error: None,
                    })
                }
                None => Err(ApiError::new(ErrorCode::Invalid, "no such workspace")),
            }
        })
    }
    async fn git_facts(&self, _q: GitQuery) -> ApiResult<GitFacts> {
        unsupported("git")
    }
    async fn git_apply(&self, _c: Command<GitApply>) -> ApiResult<Receipt> {
        unsupported("git")
    }
    async fn exec(&self, _c: Command<ExecSpec>) -> ApiResult<Receipt> {
        unsupported("exec")
    }
    async fn list_dir(&self, _p: PathRef, _page: PageRequest) -> ApiResult<Page<FileInfo>> {
        unsupported("files")
    }
    async fn list_root(&self, _at: RootPath, _page: PageRequest) -> ApiResult<Page<FileInfo>> {
        unsupported("files")
    }
    async fn read_file(&self, _q: FileRead) -> ApiResult<ReadChunk> {
        unsupported("files")
    }
    async fn scan(&self, _c: Command<ScanSpec>) -> ApiResult<Receipt> {
        unsupported("scan")
    }
    async fn scan_page(&self, _s: OperationId, _p: PageRequest) -> ApiResult<ScanPage> {
        unsupported("scan")
    }
}

#[async_trait]
impl Discovery for FakeNode {
    async fn probe(&self, _fresh: bool) -> ApiResult<Capabilities> {
        Ok(Capabilities {
            node: self.node.clone(),
            os: "fake".into(),
            version: "0".into(),
            harnesses: vec![HarnessCapability {
                name: "claude-code".into(),
                version: "0".into(),
                sessions: true,
                jobs: true,
                models: vec![],
                resume: true,
                cancel: true,
                permissions: true,
                images: true,
                enforcement: vec![],
            }],
            roots: vec![],
            capacity: Capacity {
                sessions: 4,
                jobs: 4,
                execs: 4,
            },
        })
    }
    async fn history(&self, _p: PageRequest) -> ApiResult<Page<HistoryEntry>> {
        Ok(Page {
            items: vec![],
            next: None,
        })
    }
    async fn import_history(&self, _c: Command<HistoryImport>) -> ApiResult<Receipt> {
        unsupported("history")
    }
    async fn import_page(&self, _i: OperationId, _p: PageRequest) -> ApiResult<ImportedPage> {
        unsupported("history")
    }
}

impl Node for FakeNode {
    fn id(&self) -> &NodeId {
        &self.node
    }
}

/// An [`EventSink`] that keeps what it got and acknowledges everything (deduplicating like the Hub
/// must: by stream, epoch and seq).
#[derive(Default)]
pub struct RecordingSink {
    pub events: Mutex<Vec<NodeEvent>>,
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        let mut kept = self.events.lock().unwrap();
        let mut top: BTreeMap<(StreamId, Epoch), u64> = BTreeMap::new();
        for e in kept.iter() {
            let k = (e.cursor.stream.clone(), e.cursor.epoch.clone());
            let s = top.entry(k).or_default();
            *s = (*s).max(e.cursor.seq);
        }
        for e in batch.events {
            let k = (e.cursor.stream.clone(), e.cursor.epoch.clone());
            let s = top.entry(k).or_default();
            if e.cursor.seq > *s {
                *s = e.cursor.seq;
                kept.push(e);
            }
        }
        Ok(EventAck {
            durable: top
                .into_iter()
                .map(|((stream, epoch), seq)| Watermark { stream, epoch, seq })
                .collect(),
        })
    }
}

/// Builders for tests.
pub mod build {
    use super::*;

    pub fn cmd<T>(op: &str, body: T) -> Command<T> {
        Command {
            id: id(op),
            issued_at: Timestamp(1_000),
            accept_before: Timestamp(60_000),
            grant: id("g_test"),
            body,
        }
    }

    pub fn choice() -> HarnessChoice {
        HarnessChoice {
            harness: "claude-code".into(),
            model: Some("sonnet".into()),
            effort: None,
        }
    }

    pub fn policy() -> ExecutionPolicy {
        ExecutionPolicy {
            revision: id("p1"),
            permissions: PermissionMode::Ask,
            sandbox: SandboxRequirement::None,
            readable: vec![],
            network: NetworkScope::Unrestricted,
            tools: id("g_tools"),
        }
    }

    pub fn workspace(node: &str, w: &str) -> WorkspaceRef {
        WorkspaceRef {
            node: id(node),
            id: id(w),
        }
    }

    pub fn open(session: &str, node: &str) -> OpenSession {
        OpenSession {
            session: id(session),
            workspace: workspace(node, "w_1"),
            choice: choice(),
            policy: policy(),
            resume: None,
        }
    }

    pub fn turn(session: &str, turn: &str, attempt: &str, text: &str) -> TurnSpec {
        TurnSpec {
            session: id(session),
            turn: id(turn),
            run: RunSpec {
                attempt: id(attempt),
                choice: choice(),
                directory: RunDirectory::Scratch,
                prompt: vec![Part::Text { text: text.into() }],
                policy: policy(),
                limits: Limits {
                    deadline: Timestamp(10_000_000),
                    offline_until: Timestamp(10_000_000),
                    max_output_bytes: 1 << 20,
                    max_tokens: None,
                    max_cost: None,
                },
                continuation: None,
            },
        }
    }

    pub fn exec_turn(session: &str, turn: &str, attempt: &str) -> ExecutionRef {
        ExecutionRef::Turn {
            session: id(session),
            turn: id(turn),
            attempt: id(attempt),
        }
    }

    pub fn done() -> Completion {
        Completion {
            outcome: Outcome::Succeeded,
            stop_reason: "end_turn".into(),
            error: None,
            result: vec![],
            continuation: None,
        }
    }

    pub fn permission(pid: &str, exec: ExecutionRef, command: &str) -> PermissionRequest {
        let action = Action::Exec {
            command: command.into(),
            cwd: None,
            network: NetworkScope::Unrestricted,
        };
        PermissionRequest {
            id: id(pid),
            execution: exec,
            invocation: id(format!("call_{pid}")),
            title: command.into(),
            digest: action.digest(),
            action,
            choices: vec![PermissionDecision::AllowOnce, PermissionDecision::Reject],
            expires_at: Timestamp(10_000_000),
        }
    }

    pub fn answer(req: &PermissionRequest, d: PermissionDecision) -> PermissionResolution {
        PermissionResolution {
            id: req.id.clone(),
            execution: req.execution.clone(),
            action_digest: req.digest.clone(),
            decision_revision: id("d1"),
            decision: d,
        }
    }

    /// Unused in the builders but handy for tests that move blobs.
    pub fn manifest(bytes: &[u8]) -> BlobManifest {
        BlobManifest {
            sha256: Digest::sha256(bytes),
            size: bytes.len() as u64,
            mime: "application/octet-stream".into(),
            name: "blob".into(),
        }
    }
}

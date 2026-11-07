//! An in-memory Node for tests of the Hub side. It keeps the contract's rules strictly, so a Hub that
//! breaks them fails here and not on a real machine:
//!
//! - commands are deduplicated by operation id, method and content; a known command can always be
//!   read back, a new one past its window is stale;
//! - an attempt never runs twice; a session runs one attempt at a time; a final state never changes;
//! - lifecycle facts must name the whole execution (session, turn and attempt), not just the attempt;
//! - a permission answer applies only to the live request it was given for, before it expires, with
//!   one of the offered choices;
//! - a workspace in use (running, waiting or unknown) is never tidied; another Node's ids are refused;
//! - events are numbered per stream, kept until acknowledged, replayable, never renumbered;
//! - a readonly root is listed, read and scanned, never prepared, run in or changed by git;
//! - a scan is complete only on its last page and only with no issue; its pages share a manifest;
//! - a secret is taken once from the Hub, under the operation that uses it, and masked in output.
//!
//! What it does not simulate it refuses (`unsupported`) instead of pretending. Tests drive what the
//! harness would do ([`FakeNode::emit`], [`FakeNode::finish`], [`FakeNode::ask_permission`]) and can
//! stop it at fixed points ([`FakeNode::set_hook`]) to test interleavings without sleeping.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

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

/// Points where a test can hold the fake (a hook that blocks, a barrier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    /// A command arrived, nothing decided yet.
    BeforeAccept(&'static str),
    /// Accepted and recorded, the answer not yet returned.
    AfterAccept(&'static str),
    /// Events are about to go to the Hub.
    BeforeCommit,
    /// The Hub answered the batch (committed or refused), the Node has not looked at it yet.
    AfterCommit,
    /// The Hub committed the batch, its ack not applied yet (events still kept).
    BeforeAck,
    /// The last page of a snapshot was read (its heads are those of that moment).
    AfterSnapshot,
}

type Hook = Arc<dyn Fn(Point) + Send + Sync>;

struct Accepted {
    method: &'static str,
    digest: Digest,
    receipt: Receipt,
    execution: Option<ExecutionRef>,
    workspace: Option<WorkspaceRef>,
    /// The session a session control (open, configure, release) is for.
    session: Option<SessionId>,
    cancel_requested: bool,
}

struct Stream {
    epoch: Epoch,
    produced: u64,
    acknowledged: u64,
    kept: Vec<NodeEvent>,
}

struct Session {
    open: OpenSession,
    view: SessionView,
    /// The incarnation of its workspace it was opened in: tidied and prepared again is another one.
    incarnation: u64,
}

struct Workspace {
    resource: String,
    spec: Digest,
    view: WorkspaceView,
    incarnation: u64,
    /// The allowed root it lives under (none for a scratch directory).
    root: Option<RootId>,
}

struct FakeFile {
    bytes: Vec<u8>,
    version: Revision,
}

struct FakeScan {
    manifest: Revision,
    entries: Vec<FileInfo>,
    issues: Vec<ScanIssue>,
}

#[derive(Default)]
struct State {
    now: i64,
    inbox: BTreeMap<OperationId, Accepted>,
    /// attempt → (its whole execution, the operation that runs it)
    attempts: HashMap<AttemptId, (ExecutionRef, OperationId)>,
    /// session → its running (or unknown) attempt
    active: HashMap<SessionId, ExecutionRef>,
    /// running jobs
    jobs: BTreeSet<AttemptId>,
    /// every job started, by attempt (kept after it ended), and its draft so far
    job_specs: HashMap<AttemptId, (JobSpec, String)>,
    sessions: HashMap<SessionId, Session>,
    waiting: HashMap<PermissionId, PermissionRequest>,
    /// answered permissions: id → the resolution that was applied
    resolved: HashMap<PermissionId, PermissionResolution>,
    workspaces: HashMap<WorkspaceId, Workspace>,
    /// Revisions and incarnations come from one counter: never reused.
    counter: u64,
    /// Snapshots being paged: token → (entries, heads)
    snapshots: HashMap<u64, (Vec<SnapshotEntry>, Vec<StreamHead>)>,
    streams: BTreeMap<StreamId, Stream>,
    log: Vec<(&'static str, OperationId)>,
    next_event: u64,
    /// Which journal this is (0: the first). A new journal numbers its streams in its own epoch
    /// and its events with its own ids, as LocalNode's journal identity does.
    journal: u64,
    capacity: Option<Capacity>,
    delivery_failures: u64,
    /// Refusals the test asked for, by method, used up in order.
    refusals: Vec<(&'static str, ApiError)>,
    /// Methods refused with this code until told otherwise.
    refused: HashMap<&'static str, ErrorCode>,
    /// What the next commands of a method do instead of the normal answer, in order.
    scripted: HashMap<&'static str, std::collections::VecDeque<Scripted>>,
    /// The most sessions open (not released) in one workspace at once, by workspace.
    crowded: HashMap<WorkspaceId, usize>,
    /// The allowed roots. With none set, preparing takes any root (tests that do not care);
    /// files and scans always need one.
    roots: Vec<RootInfo>,
    features: Vec<String>,
    /// The files under the roots, as on disk.
    files: BTreeMap<(RootId, RelPath), FakeFile>,
    /// Issues every scan of a root reports (an unreadable directory, a held link).
    scan_issues: HashMap<RootId, Vec<ScanIssue>>,
    scans: HashMap<OperationId, FakeScan>,
    /// What git says in a workspace, as the test scripted it.
    git: HashMap<WorkspaceId, GitFacts>,
    /// What the next execs print, in order (then: exit 0, no output).
    exec_answers: std::collections::VecDeque<ExecResult>,
    history: Vec<(HistoryEntry, HistoryLocation)>,
}

/// What the inbox makes of a command before it is decided.
#[allow(clippy::large_enum_variant)]
enum Admitted {
    /// Seen before: its receipt answers.
    Resend(Receipt),
    New {
        drop_reply: bool,
        unknown: bool,
    },
}

/// What [`FakeNode::script_once`] makes one command do.
#[derive(Debug, Clone)]
pub enum Scripted {
    /// Refused before anything is recorded.
    Refuse(ApiError),
    /// Recorded and decided as normal, the answer lost on the way back.
    DropReply,
    /// Recorded, then the Node cannot tell what it did: its receipt is unknown, nothing else happens.
    Unknown,
}

pub struct FakeNode {
    node: NodeId,
    st: Mutex<State>,
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    secrets: Mutex<Option<Arc<dyn HubSecrets>>>,
    hook: Mutex<Option<Hook>>,
}

/// What a new command's handler decided: its state, its execution and workspace, events to emit.
struct Decided {
    state: Observed,
    execution: Option<ExecutionRef>,
    workspace: Option<WorkspaceRef>,
    session: Option<SessionId>,
    events: Vec<(Subject, EventBody)>,
}

impl Decided {
    fn done(result: OperationResult) -> Self {
        Decided {
            state: Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(result),
                error: None,
            },
            execution: None,
            workspace: None,
            session: None,
            events: vec![],
        }
    }
}

const HARNESS: &str = "claude-code";

impl FakeNode {
    pub fn new(node: &str) -> Arc<Self> {
        Arc::new(FakeNode {
            node: id(node),
            st: Mutex::new(State {
                now: 1_000,
                ..Default::default()
            }),
            sink: Mutex::new(None),
            secrets: Mutex::new(None),
            hook: Mutex::new(None),
        })
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn at(&self, p: Point) {
        let hook = self.hook.lock().unwrap().clone();
        if let Some(h) = hook {
            h(p);
        }
    }

    /// Where events go (the Hub under test). Without one they are only kept for `replay`.
    pub fn connect(&self, sink: Arc<dyn EventSink>) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    /// Where secrets come from (the Hub under test). Without one a command that needs a secret is
    /// `unsupported`.
    pub fn connect_secrets(&self, secrets: Arc<dyn HubSecrets>) {
        *self.secrets.lock().unwrap() = Some(secrets);
    }

    /// The allowed roots (what `probe` reports and what files, scans and prepares check).
    pub fn set_roots(&self, roots: Vec<RootInfo>) {
        self.st().roots = roots;
    }

    pub fn set_features(&self, features: Vec<String>) {
        self.st().features = features;
    }

    /// Writes a file under a root (a new version every time).
    pub fn put_file(&self, root: &str, rel: &str, bytes: &[u8]) {
        let mut st = self.st();
        let version = Self::revision(&mut st);
        st.files.insert(
            (id(root), RelPath::new(rel).expect("valid test path")),
            FakeFile {
                bytes: bytes.to_vec(),
                version,
            },
        );
    }

    pub fn remove_file(&self, root: &str, rel: &str) {
        let rel = RelPath::new(rel).expect("valid test path");
        self.st().files.remove(&(id(root), rel));
    }

    /// Every later scan of `root` reports `issue`.
    pub fn scan_issue(&self, root: &str, issue: ScanIssue) {
        self.st()
            .scan_issues
            .entry(id(root))
            .or_default()
            .push(issue);
    }

    /// What git says in `w` from now on (`git_facts` answers it, `git_apply` checks against it).
    /// Reads are answered from `facts.reads` by their request; any other read is `unsupported`.
    pub fn script_git(&self, w: &WorkspaceRef, facts: GitFacts) {
        self.st().git.insert(w.id.clone(), facts);
    }

    /// What the next exec prints and exits with (before secrets are masked).
    pub fn answer_exec(&self, r: ExecResult) {
        self.st().exec_answers.push_back(r);
    }

    /// A harness history entry the Node lists, and where it is.
    pub fn put_history(&self, entry: HistoryEntry, location: HistoryLocation) {
        self.st().history.push((entry, location));
    }

    pub fn set_hook(&self, hook: impl Fn(Point) + Send + Sync + 'static) {
        *self.hook.lock().unwrap() = Some(Arc::new(hook));
    }

    /// The Node's clock (Unix ms).
    pub fn set_now(&self, ms: i64) {
        self.st().now = ms;
    }

    /// The next command of `method` is refused with `e` before anything is decided (nothing runs).
    pub fn refuse_next(&self, method: &'static str, e: ApiError) {
        self.st().refusals.push((method, e));
    }

    /// As a Node process that restarts (LocalNode's journal when it opens again): every operation
    /// not finished becomes unknown, nobody waits for an answer any more and every session is let
    /// go, all silently (no event).
    pub fn restart(&self) {
        let mut st = self.st();
        for a in st.inbox.values_mut() {
            if !a.receipt.state.is_finished()
                && !matches!(a.receipt.state, Observed::Unknown { .. })
            {
                a.receipt.state = Observed::Unknown {
                    reason: "the node restarted while it ran".into(),
                };
            }
        }
        st.waiting.clear();
        for s in st.sessions.values_mut() {
            s.view.phase = SessionPhase::Released;
        }
    }

    /// As a Node whose home was deleted or made again: a new, empty journal. Nothing it accepted,
    /// ran or kept is known any more (no event says so); streams start again in a new epoch and
    /// event ids are new. The log and what the test scripted stay.
    pub fn reset_journal(&self) {
        let mut st = self.st();
        let old = std::mem::take(&mut *st);
        *st = State {
            now: old.now,
            journal: old.journal + 1,
            log: old.log,
            capacity: old.capacity,
            delivery_failures: old.delivery_failures,
            refusals: old.refusals,
            refused: old.refused,
            scripted: old.scripted,
            // the machine's own: roots, files and harness history outlive the journal
            roots: old.roots,
            features: old.features,
            files: old.files,
            scan_issues: old.scan_issues,
            history: old.history,
            ..Default::default()
        };
    }

    /// As a journal restored from an older copy that kept its epoch: the last event of the session
    /// `session`'s stream is forgotten, so the next one there takes its position (the Hub already
    /// holds another event at it) under a new id.
    pub fn rewind(&self, session: &str) {
        let stream = Self::stream_of(&Subject::Session {
            session: id(session),
        });
        let mut st = self.st();
        if let Some(s) = st.streams.get_mut(&stream) {
            s.produced = s.produced.saturating_sub(1);
            s.acknowledged = s.acknowledged.min(s.produced);
            let last = s.produced;
            s.kept.retain(|e| e.cursor.seq <= last);
        }
    }

    /// Limit concurrent sessions and jobs (unlimited by default).
    pub fn set_capacity(&self, c: Capacity) {
        self.st().capacity = Some(c);
    }

    /// Refuse every `method` with `code` before recording it (`None`: stop refusing).
    pub fn refuse(&self, method: &'static str, code: Option<ErrorCode>) {
        match code {
            Some(c) => self.st().refused.insert(method, c),
            None => self.st().refused.remove(method),
        };
    }

    /// The next `method` command (after those scripted before it) does `s`.
    pub fn script_once(&self, method: &'static str, s: Scripted) {
        self.st().scripted.entry(method).or_default().push_back(s);
    }

    /// The most sessions that were ever open at once in one workspace (two harnesses in one
    /// worktree when above 1).
    pub fn most_open_in_a_workspace(&self) -> usize {
        self.st().crowded.values().copied().max().unwrap_or(0)
    }

    /// Accepted commands in order, by method.
    pub fn log(&self) -> Vec<(&'static str, OperationId)> {
        self.st().log.clone()
    }

    pub fn receipt(&self, op: &OperationId) -> Option<Receipt> {
        self.st().inbox.get(op).map(|a| a.receipt.clone())
    }

    pub fn cancel_requested(&self, op: &OperationId) -> bool {
        self.st().inbox.get(op).is_some_and(|a| a.cancel_requested)
    }

    /// Batches the Hub failed to take (they stay for replay).
    pub fn delivery_failures(&self) -> u64 {
        self.st().delivery_failures
    }

    /// Events not yet acknowledged.
    pub fn unacked(&self) -> usize {
        self.st().streams.values().map(|s| s.kept.len()).sum()
    }

    /// The command inbox. Known id: same method and content → its receipt (even past the window);
    /// otherwise conflict. New id: past the window → stale; then `decide` runs, under the same lock
    /// as the recording and the numbering of its events, so a resend can never emit them twice.
    async fn accept<T: serde::Serialize>(
        &self,
        method: &'static str,
        c: &Command<T>,
        decide: impl FnOnce(&mut State) -> ApiResult<Decided>,
    ) -> ApiResult<Receipt> {
        self.at(Point::BeforeAccept(method));
        c.check_window()?;
        let digest = c.digest(method)?;
        let (receipt, events, drop_reply) = {
            let mut st = self.st();
            let (drop_reply, unknown) = match Self::admit(&mut st, method, c, &digest)? {
                Admitted::Resend(r) => return Ok(r),
                Admitted::New {
                    drop_reply,
                    unknown,
                } => (drop_reply, unknown),
            };
            let d = decide(&mut st)?;
            let (receipt, events) = Self::record_new(&mut st, method, c, digest, d, unknown);
            (receipt, events, drop_reply)
        };
        self.finish_accept(method, c, receipt, events, drop_reply)
            .await
    }

    /// [`Self::accept`] for a command carrying secrets. Every refusal runs first (the inbox, the
    /// scripted refusals, the window, then `check`); only a command known to be new and accepted
    /// takes its secrets, so a refused one leaves them with the Hub for its resend. `check` runs
    /// again with `decide` once the secrets are in, under the lock that records the command.
    async fn accept_taking<T: serde::Serialize>(
        &self,
        method: &'static str,
        c: &Command<T>,
        refs: &[SecretRef],
        check: impl Fn(&State) -> ApiResult<()>,
        decide: impl FnOnce(&mut State, &[SecretBytes]) -> ApiResult<Decided>,
    ) -> ApiResult<Receipt> {
        self.at(Point::BeforeAccept(method));
        c.check_window()?;
        let digest = c.digest(method)?;
        let (drop_reply, unknown) = {
            let mut st = self.st();
            let new = match Self::admit(&mut st, method, c, &digest)? {
                Admitted::Resend(r) => return Ok(r),
                Admitted::New {
                    drop_reply,
                    unknown,
                } => (drop_reply, unknown),
            };
            check(&st)?;
            new
        };
        let secrets = self.take_secrets(&c.id, refs).await?;
        let (receipt, events) = {
            let mut st = self.st();
            // the same command sent twice at once: the first one recorded answers both
            if let Some(a) = st.inbox.get(&c.id) {
                return if a.digest == digest && a.method == method {
                    Ok(a.receipt.clone())
                } else {
                    Err(
                        ApiError::new(ErrorCode::Conflict, "same operation, other content")
                            .on(&c.id),
                    )
                };
            }
            check(&st)?;
            let d = decide(&mut st, &secrets)?;
            Self::record_new(&mut st, method, c, digest, d, unknown)
        };
        self.finish_accept(method, c, receipt, events, drop_reply)
            .await
    }

    /// The checks before a command is decided: the test's refusals, a known id (its receipt or a
    /// conflict), the window.
    fn admit<T>(
        st: &mut State,
        method: &'static str,
        c: &Command<T>,
        digest: &Digest,
    ) -> ApiResult<Admitted> {
        if let Some(code) = st.refused.get(method) {
            return Err(ApiError::new(*code, "refused by the test").on(&c.id));
        }
        let script = if st.inbox.contains_key(&c.id) {
            None
        } else {
            st.scripted.get_mut(method).and_then(|q| q.pop_front())
        };
        let (mut drop_reply, mut unknown) = (false, false);
        match script {
            Some(Scripted::Refuse(e)) => return Err(e),
            Some(Scripted::DropReply) => drop_reply = true,
            Some(Scripted::Unknown) => unknown = true,
            None => {}
        }
        if let Some(a) = st.inbox.get(&c.id) {
            return if a.digest == *digest && a.method == method {
                Ok(Admitted::Resend(a.receipt.clone()))
            } else {
                Err(ApiError::new(ErrorCode::Conflict, "same operation, other content").on(&c.id))
            };
        }
        if st.now > c.accept_before.0 {
            return Err(ApiError::new(ErrorCode::Stale, "past accept_before").on(&c.id));
        }
        if let Some(i) = st.refusals.iter().position(|(m, _)| *m == method) {
            return Err(st.refusals.remove(i).1.on(&c.id));
        }
        Ok(Admitted::New {
            drop_reply,
            unknown,
        })
    }

    /// Records a new command as decided and numbers its events.
    fn record_new<T>(
        st: &mut State,
        method: &'static str,
        c: &Command<T>,
        digest: Digest,
        mut d: Decided,
        unknown: bool,
    ) -> (Receipt, Vec<NodeEvent>) {
        if unknown {
            d.state = Observed::Unknown {
                reason: "scripted by the test".into(),
            };
            d.events = vec![];
        }
        let receipt = Receipt {
            operation: c.id.clone(),
            state: d.state,
        };
        st.inbox.insert(
            c.id.clone(),
            Accepted {
                method,
                digest,
                receipt: receipt.clone(),
                execution: d.execution,
                workspace: d.workspace,
                session: d.session,
                cancel_requested: false,
            },
        );
        st.log.push((method, c.id.clone()));
        let events: Vec<NodeEvent> = d
            .events
            .into_iter()
            .map(|(s, b)| Self::record(st, s, b))
            .collect();
        (receipt, events)
    }

    async fn finish_accept<T>(
        &self,
        method: &'static str,
        c: &Command<T>,
        receipt: Receipt,
        events: Vec<NodeEvent>,
        drop_reply: bool,
    ) -> ApiResult<Receipt> {
        self.at(Point::AfterAccept(method));
        // a Hub that cannot take the events now gets them on replay: the command stays accepted
        self.deliver(events).await;
        if drop_reply {
            return Err(ApiError::uncertain(&c.id, "the answer was lost (scripted)"));
        }
        Ok(receipt)
    }

    fn stream_of(subject: &Subject) -> StreamId {
        let s = match subject {
            Subject::Session { session } => derived_id("session", session.as_str()),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => {
                derived_id("session", session.as_str())
            }
            Subject::Execution(ExecutionRef::Job { job, .. }) => derived_id("job", job.as_str()),
            Subject::Operation { operation } => derived_id("op", operation.as_str()),
        };
        id(s)
    }

    fn record(st: &mut State, subject: Subject, body: EventBody) -> NodeEvent {
        let stream = Self::stream_of(&subject);
        st.next_event += 1;
        let n = st.next_event;
        let at = Timestamp(st.now);
        let journal = st.journal;
        let event_id = match journal {
            0 => format!("ev_{n}"),
            j => format!("ev_j{j}_{n}"),
        };
        let s = st.streams.entry(stream.clone()).or_insert_with(|| Stream {
            epoch: id(match journal {
                0 => "e1".to_string(),
                j => format!("j{j}"),
            }),
            produced: 0,
            acknowledged: 0,
            kept: vec![],
        });
        s.produced += 1;
        let ev = NodeEvent {
            schema: crate::EVENT_SCHEMA,
            id: id(event_id),
            at,
            cursor: Watermark {
                stream,
                epoch: s.epoch.clone(),
                seq: s.produced,
            },
            subject,
            body,
        };
        s.kept.push(ev.clone());
        ev
    }

    async fn deliver(&self, events: Vec<NodeEvent>) {
        if events.is_empty() {
            return;
        }
        let sink = self.sink.lock().unwrap().clone();
        let Some(sink) = sink else { return };
        self.at(Point::BeforeCommit);
        let committed = sink.commit(EventBatch { events }).await;
        self.at(Point::AfterCommit);
        match committed {
            Ok(ack) => {
                self.at(Point::BeforeAck);
                let _ = self.apply_ack(&ack);
            }
            Err(_) => self.st().delivery_failures += 1,
        }
    }

    fn apply_ack(&self, ack: &EventAck) -> ApiResult<()> {
        let mut st = self.st();
        for w in &ack.durable {
            let Some(s) = st.streams.get_mut(&w.stream) else {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "ack of an unknown stream",
                ));
            };
            if s.epoch != w.epoch {
                return Err(ApiError::new(ErrorCode::Stale, "ack of another epoch"));
            }
            if w.seq > s.produced {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "ack ahead of what was produced",
                ));
            }
            if w.seq > s.acknowledged {
                s.acknowledged = w.seq;
                s.kept.retain(|e| e.cursor.seq > w.seq);
            }
        }
        Ok(())
    }

    /// The execution `exec` names exactly, live (not finished).
    fn live_op(st: &State, exec: &ExecutionRef) -> ApiResult<OperationId> {
        match st.attempts.get(exec.attempt()) {
            Some((e, op)) if e == exec => match st.inbox.get(op) {
                Some(a) if !a.receipt.state.is_finished() => Ok(op.clone()),
                _ => Err(ApiError::new(ErrorCode::Stale, "that attempt is over")),
            },
            Some(_) => Err(ApiError::new(
                ErrorCode::Invalid,
                "the attempt belongs to another execution",
            )),
            None => Err(ApiError::new(ErrorCode::Invalid, "no such attempt")),
        }
    }

    /// What the harness reports while it runs (text, tools, usage…), for a live execution or a session.
    pub async fn emit(&self, subject: Subject, body: EventBody) -> ApiResult<()> {
        let ev = {
            let mut st = self.st();
            match &subject {
                Subject::Execution(e) => {
                    Self::live_op(&st, e)?;
                }
                Subject::Session { session } if !st.sessions.contains_key(session) => {
                    return Err(ApiError::new(ErrorCode::Invalid, "no such session"));
                }
                _ => {}
            }
            Self::record(&mut st, subject, body)
        };
        self.deliver(vec![ev]).await;
        Ok(())
    }

    /// The harness asks before a tool runs: the attempt waits until the Hub answers. The same request
    /// again changes nothing; another one under the same id is a conflict.
    pub async fn ask_permission(&self, req: PermissionRequest) -> ApiResult<()> {
        let ev = {
            let mut st = self.st();
            if let Some(old) = st.waiting.get(&req.id) {
                return if *old == req {
                    Ok(())
                } else {
                    Err(ApiError::new(
                        ErrorCode::Conflict,
                        "another request with this id",
                    ))
                };
            }
            if st.resolved.contains_key(&req.id) {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "this request was answered",
                ));
            }
            if permission_digest(&req.action, &req.preconditions)? != req.digest {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "digest does not match the action",
                ));
            }
            let op = Self::live_op(&st, &req.execution)?;
            self.check_preconditions(&st, &req.preconditions)?;
            if let Some(a) = st.inbox.get_mut(&op) {
                let mut on = match &a.receipt.state {
                    Observed::Waiting { on } => on.clone(),
                    _ => vec![],
                };
                let w = WaitRef::Permission(req.id.clone());
                if !on.contains(&w) {
                    on.push(w);
                }
                a.receipt.state = Observed::Waiting { on };
            }
            st.waiting.insert(req.id.clone(), req.clone());
            Self::record(
                &mut st,
                Subject::Execution(req.execution.clone()),
                EventBody::PermissionRequested { request: req },
            )
        };
        self.deliver(vec![ev]).await;
        Ok(())
    }

    /// The attempt ends. Its receipt becomes final and stays so: the same end again is a no-op, a
    /// different one a conflict. Only this attempt frees its session.
    pub async fn finish(&self, exec: &ExecutionRef, completion: Completion) -> ApiResult<()> {
        if let Some(r) = &completion.job
            && !r.end.fits(completion.outcome)
        {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "the job's end contradicts its outcome",
            ));
        }
        let evs = {
            let mut st = self.st();
            let op = match st.attempts.get(exec.attempt()) {
                Some((e, op)) if e == exec => op.clone(),
                Some(_) => {
                    return Err(ApiError::new(
                        ErrorCode::Invalid,
                        "the attempt belongs to another execution",
                    ));
                }
                None => return Err(ApiError::new(ErrorCode::Invalid, "no such attempt")),
            };
            let final_state = Observed::Finished {
                outcome: completion.outcome,
                result: Some(OperationResult::Execution(completion.clone())),
                error: completion.error.clone(),
            };
            let Some(a) = st.inbox.get_mut(&op) else {
                return Err(ApiError::new(
                    ErrorCode::Internal,
                    "attempt without operation",
                ));
            };
            if a.receipt.state.is_finished() {
                return if a.receipt.state == final_state {
                    Ok(())
                } else {
                    Err(ApiError::new(
                        ErrorCode::Conflict,
                        "already ended otherwise",
                    ))
                };
            }
            a.receipt.state = final_state;
            match exec {
                ExecutionRef::Turn { session, .. } => {
                    if st.active.get(session) == Some(exec) {
                        st.active.remove(session);
                        if let Some(s) = st.sessions.get_mut(session) {
                            s.view.phase = SessionPhase::Ready;
                            s.view.active = None;
                        }
                    }
                }
                ExecutionRef::Job { attempt, .. } => {
                    st.jobs.remove(attempt);
                }
            }
            let expired: Vec<PermissionId> = st
                .waiting
                .iter()
                .filter(|(_, r)| r.execution == *exec)
                .map(|(k, _)| k.clone())
                .collect();
            let mut evs = vec![];
            for p in expired {
                st.waiting.remove(&p);
                evs.push(Self::record(
                    &mut st,
                    Subject::Execution(exec.clone()),
                    EventBody::PermissionExpired { id: p },
                ));
            }
            evs.push(Self::record(
                &mut st,
                Subject::Execution(exec.clone()),
                EventBody::AttemptEnded { completion },
            ));
            evs
        };
        self.deliver(evs).await;
        Ok(())
    }

    /// As a Node whose journal lost an operation: it holds no record of `op` any more.
    pub fn forget(&self, op: &OperationId) {
        self.st().inbox.remove(op);
    }

    /// After a crash the Node cannot tell what a live attempt did. It keeps holding its session.
    pub fn lose_track(&self, exec: &ExecutionRef) -> ApiResult<()> {
        let mut st = self.st();
        let op = Self::live_op(&st, exec)?;
        if let Some(a) = st.inbox.get_mut(&op) {
            a.receipt.state = Observed::Unknown {
                reason: "node restarted".into(),
            };
        }
        Ok(())
    }

    /// The job an attempt was started with (kept after it ended).
    pub fn job_spec(&self, attempt: &AttemptId) -> Option<JobSpec> {
        self.st().job_specs.get(attempt).map(|(s, _)| s.clone())
    }

    /// A job's harness starts a new message (it replaces the draft so far) and writes it in `deltas`.
    pub async fn draft(
        &self,
        exec: &ExecutionRef,
        message: &str,
        deltas: &[&str],
    ) -> ApiResult<()> {
        let subject = Subject::Execution(exec.clone());
        let started = EventBody::MessageStarted {
            message: message.into(),
            block: "b0".into(),
            role: Role::Assistant,
        };
        self.emit(subject.clone(), started).await?;
        let mut offset = 0;
        for d in deltas {
            let delta = EventBody::TextDelta {
                message: message.into(),
                block: "b0".into(),
                utf8_offset: offset,
                text: (*d).into(),
            };
            self.emit(subject.clone(), delta).await?;
            offset += d.len() as u64;
        }
        if let Some((_, text)) = self.st().job_specs.get_mut(exec.attempt()) {
            *text = deltas.concat();
        }
        Ok(())
    }

    /// Ends the running jobs a Node would end by itself now, keeping their draft as the answer: a
    /// requested cancel as cancelled (research: stopped), one past its deadline as timed out
    /// (research: the time budget). Unknown ones are left alone. Returns those it ended.
    pub async fn settle_jobs(&self) -> ApiResult<Vec<ExecutionRef>> {
        let due: Vec<(ExecutionRef, Completion)> = {
            let st = self.st();
            st.jobs
                .iter()
                .filter_map(|attempt| {
                    let (exec, op) = st.attempts.get(attempt)?;
                    let a = st.inbox.get(op)?;
                    let (spec, text) = st.job_specs.get(attempt)?;
                    if matches!(a.receipt.state, Observed::Unknown { .. }) {
                        return None;
                    }
                    let (outcome, stop, error, end) = if a.cancel_requested {
                        (Outcome::Cancelled, "cancelled", None, JobEnd::Stopped)
                    } else if spec.run.limits.deadline.0 <= st.now {
                        let e = ApiError::new(ErrorCode::Expired, "deadline passed");
                        let end = JobEnd::Budget {
                            hit: BudgetHit::Time,
                        };
                        (Outcome::TimedOut, "timeout", Some(e), end)
                    } else {
                        return None;
                    };
                    let report = matches!(spec.work, JobWork::Research(_)).then_some(JobReport {
                        end,
                        turns: 0,
                        files_read: vec![],
                        sandboxed: false,
                    });
                    let completion = Completion {
                        outcome,
                        stop_reason: stop.into(),
                        error,
                        result: build::text(text),
                        continuation: None,
                        transient: false,
                        acted: false,
                        job: report,
                    };
                    Some((exec.clone(), completion))
                })
                .collect()
        };
        for (exec, completion) in &due {
            self.finish(exec, completion.clone()).await?;
        }
        Ok(due.into_iter().map(|(e, _)| e).collect())
    }

    fn next(st: &mut State) -> u64 {
        st.counter += 1;
        st.counter
    }

    /// A revision of this fake journal: a new journal never repeats one of an older journal.
    fn revision(st: &mut State) -> Revision {
        let n = Self::next(st);
        match st.journal {
            0 => id(format!("r{n}")),
            j => id(format!("r{j}_{n}")),
        }
    }

    /// What this Node can enforce: no sandbox guarantee at all.
    fn check_policy(p: &ExecutionPolicy) -> ApiResult<()> {
        if let SandboxRequirement::Required { guarantees } = &p.sandbox
            && !guarantees.is_empty()
        {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "the fake enforces no sandbox",
            ));
        }
        Ok(())
    }

    /// The preconditions still hold. The fake knows workspace revisions; file versions and HEAD it does
    /// not simulate, so it refuses them rather than pretending they hold.
    fn check_preconditions(&self, st: &State, pre: &[Precondition]) -> ApiResult<()> {
        for p in pre {
            match p {
                Precondition::Workspace {
                    workspace,
                    revision,
                } => {
                    self.mine(workspace)?;
                    match st.workspaces.get(&workspace.id) {
                        Some(w) if w.view.revision == *revision => {}
                        _ => {
                            return Err(ApiError::new(
                                ErrorCode::Stale,
                                "the workspace changed since",
                            ));
                        }
                    }
                }
                Precondition::File { .. } | Precondition::Head { .. } => {
                    return unsupported("file and HEAD preconditions");
                }
            }
        }
        Ok(())
    }

    fn check_run(st: &State, run: &RunSpec) -> ApiResult<()> {
        if run.choice.harness != HARNESS {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "no such harness here",
            ));
        }
        Self::check_policy(&run.policy)?;
        if run.limits.deadline.0 <= st.now {
            return Err(ApiError::new(ErrorCode::Expired, "deadline already passed"));
        }
        Ok(())
    }

    fn take_attempt(st: &mut State, op: &OperationId, exec: &ExecutionRef) -> ApiResult<()> {
        if let Some((_, other)) = st.attempts.get(exec.attempt())
            && other != op
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "this attempt already ran under another operation",
            )
            .on(op));
        }
        st.attempts
            .insert(exec.attempt().clone(), (exec.clone(), op.clone()));
        Ok(())
    }

    fn mine(&self, w: &WorkspaceRef) -> ApiResult<()> {
        if w.node != self.node {
            return Err(ApiError::new(ErrorCode::Denied, "another node's workspace"));
        }
        Ok(())
    }

    fn root_access(st: &State, root: &RootId) -> ApiResult<RootAccess> {
        st.roots
            .iter()
            .find(|r| r.id == *root)
            .map(|r| r.access)
            .ok_or_else(|| ApiError::new(ErrorCode::Denied, "not an allowed root"))
    }

    /// A workspace commands and git may change: it exists and its root (as it is now) is writable.
    fn writable(st: &State, w: &WorkspaceRef) -> ApiResult<()> {
        let Some(ws) = st.workspaces.get(&w.id) else {
            return Err(ApiError::new(ErrorCode::Invalid, "no such workspace"));
        };
        if let Some(r) = &ws.root
            && Self::root_access(st, r)? == RootAccess::ReadOnly
        {
            return Err(ApiError::new(ErrorCode::Denied, "a readonly root"));
        }
        Ok(())
    }

    /// Takes the secrets of a command not seen before from the Hub (a resend takes nothing: its
    /// receipt answers). `File` needs the `secret-file` feature.
    async fn take_secrets(
        &self,
        op: &OperationId,
        refs: &[SecretRef],
    ) -> ApiResult<Vec<SecretBytes>> {
        if refs.is_empty() || self.receipt(op).is_some() {
            return Ok(vec![]);
        }
        if refs
            .iter()
            .any(|r| matches!(r.how, SecretDelivery::File { .. }))
            && !self.st().features.iter().any(|f| f == FEATURE_SECRET_FILE)
        {
            return unsupported("secret files");
        }
        let Some(secrets) = self.secrets.lock().unwrap().clone() else {
            return unsupported("no secrets channel");
        };
        let mut out = vec![];
        for r in refs {
            out.push(
                secrets
                    .take(op.clone(), r.id.clone())
                    .await
                    .map_err(|e| e.on(op))?,
            );
        }
        Ok(out)
    }

    fn heads(st: &State) -> Vec<StreamHead> {
        st.streams
            .iter()
            .map(|(k, s)| StreamHead {
                stream: k.clone(),
                epoch: s.epoch.clone(),
                produced: s.produced,
                acknowledged: s.acknowledged,
            })
            .collect()
    }

    fn running(st: &State) -> (u32, u32) {
        (st.active.len() as u32, st.jobs.len() as u32)
    }
}

fn unsupported<T>(what: &str) -> ApiResult<T> {
    Err(ApiError::new(
        ErrorCode::Unsupported,
        format!("fake node: {what}"),
    ))
}

/// The remainder of `p` below `base` (`""` for `base` itself), or `None` outside it.
fn below<'a>(base: &RelPath, p: &'a RelPath) -> Option<&'a str> {
    let (b, p) = (base.as_str(), p.as_str());
    if b.is_empty() {
        return Some(p);
    }
    if p == b {
        return Some("");
    }
    p.strip_prefix(b).and_then(|r| r.strip_prefix('/'))
}

/// One page of `items` (cursors are offsets, `o<n>`).
fn page_of<T: Clone>(items: &[T], page: &PageRequest) -> ApiResult<(Vec<T>, Option<Cursor>)> {
    let from = match &page.cursor {
        None => 0,
        Some(c) => c
            .as_str()
            .strip_prefix('o')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n <= items.len())
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "bad cursor"))?,
    };
    let to = (from + page.limit.max(1) as usize).min(items.len());
    let next = (to < items.len()).then(|| id(format!("o{to}")));
    Ok((items[from..to].to_vec(), next))
}

/// Replaces every secret's text in `s` (output is masked before it leaves the Node).
fn mask(s: &str, secrets: &[SecretBytes]) -> String {
    let mut out = s.to_string();
    for x in secrets {
        let t = String::from_utf8_lossy(x.expose());
        if !t.is_empty() {
            out = out.replace(t.as_ref(), "***");
        }
    }
    out
}

/// A Hub's secrets for tests: each given to one operation, taken once.
#[derive(Default)]
pub struct FakeSecrets {
    held: Mutex<HashMap<String, (OperationId, Vec<u8>)>>,
    taken: Mutex<Vec<(OperationId, String)>>,
}

impl FakeSecrets {
    /// `id` may be taken once, for `op`.
    pub fn give(&self, op: &str, secret: &str, bytes: &[u8]) {
        self.held
            .lock()
            .unwrap()
            .insert(secret.into(), (id(op), bytes.to_vec()));
    }
    /// What was taken, in order.
    pub fn taken(&self) -> Vec<(OperationId, String)> {
        self.taken.lock().unwrap().clone()
    }
}

#[async_trait]
impl HubSecrets for FakeSecrets {
    async fn take(&self, operation: OperationId, secret: String) -> ApiResult<SecretBytes> {
        let mut held = self.held.lock().unwrap();
        match held.get(&secret) {
            Some((op, _)) if *op != operation => Err(ApiError::new(
                ErrorCode::Denied,
                "a secret of another operation",
            )),
            Some(_) => {
                let (_, bytes) = held.remove(&secret).unwrap();
                self.taken.lock().unwrap().push((operation, secret));
                Ok(SecretBytes::new(bytes))
            }
            None if self.taken.lock().unwrap().iter().any(|(_, s)| *s == secret) => {
                Err(ApiError::new(ErrorCode::Stale, "already taken"))
            }
            None => Err(ApiError::new(ErrorCode::Denied, "no such secret")),
        }
    }
}

#[async_trait]
impl SessionExecutor for FakeNode {
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        self.accept("open_session", &c, |st| {
            let o = &c.body;
            if o.choice.harness != HARNESS {
                return Err(ApiError::new(
                    ErrorCode::Unsupported,
                    "no such harness here",
                ));
            }
            Self::check_policy(&o.policy)?;
            let Some(incarnation) = st.workspaces.get(&o.workspace.id).map(|w| w.incarnation)
            else {
                return Err(ApiError::new(ErrorCode::Invalid, "no such workspace"));
            };
            if let Some(s) = st.sessions.get(&o.session) {
                if st.active.contains_key(&o.session) {
                    return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
                }
                if s.view.phase != SessionPhase::Released {
                    return Err(ApiError::new(ErrorCode::Conflict, "already open"));
                }
                if s.open.workspace != o.workspace || s.open.choice.harness != o.choice.harness {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "reopened in another workspace or harness",
                    ));
                }
            }
            let view = SessionView {
                id: o.session.clone(),
                revision: Self::revision(st),
                phase: SessionPhase::Ready,
                active: None,
                effective: o.choice.clone(),
                effective_permissions: o.policy.permissions,
                guarantees: vec![],
                resume: Some(id(derived_id("resume", o.session.as_str()))),
            };
            st.sessions.insert(
                o.session.clone(),
                Session {
                    open: o.clone(),
                    view: view.clone(),
                    incarnation,
                },
            );
            let open = st
                .sessions
                .values()
                .filter(|s| {
                    s.open.workspace == o.workspace && s.view.phase != SessionPhase::Released
                })
                .count();
            let most = st.crowded.entry(o.workspace.id.clone()).or_default();
            *most = (*most).max(open);
            let mut d = Decided::done(OperationResult::Session(view.clone()));
            d.workspace = Some(o.workspace.clone());
            d.session = Some(o.session.clone());
            d.events = vec![(
                Subject::Session {
                    session: o.session.clone(),
                },
                EventBody::SessionChanged { view },
            )];
            Ok(d)
        })
        .await
    }

    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt> {
        self.accept("configure_session", &c, |st| {
            let b = &c.body;
            if st.active.contains_key(&b.session) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            let Some(s) = st.sessions.get_mut(&b.session) else {
                return Err(ApiError::new(ErrorCode::Invalid, "session not open"));
            };
            if s.view.revision != b.expected {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "the session changed since",
                ));
            }
            if b.choice.harness != s.open.choice.harness {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "another harness needs another session",
                ));
            }
            Self::check_policy(&b.policy)?;
            // everything checked: now change it
            let revision = Self::revision(st);
            let Some(s) = st.sessions.get_mut(&b.session) else {
                return Err(ApiError::new(ErrorCode::Internal, "session vanished"));
            };
            s.open.choice = b.choice.clone();
            s.open.policy = b.policy.clone();
            s.view.effective = b.choice.clone();
            s.view.effective_permissions = b.policy.permissions;
            s.view.revision = revision;
            let view = s.view.clone();
            let mut d = Decided::done(OperationResult::Session(view.clone()));
            d.session = Some(b.session.clone());
            d.events = vec![(
                Subject::Session {
                    session: b.session.clone(),
                },
                EventBody::SessionChanged { view },
            )];
            Ok(d)
        })
        .await
    }

    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt> {
        let exec = ExecutionRef::Turn {
            session: c.body.session.clone(),
            turn: c.body.turn.clone(),
            attempt: c.body.run.attempt.clone(),
        };
        self.accept("start_turn", &c, |st| {
            let t = &c.body;
            let Some(s) = st.sessions.get(&t.session) else {
                return Err(ApiError::new(ErrorCode::Invalid, "session not open"));
            };
            if s.view.phase == SessionPhase::Released {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "session released: open it again",
                ));
            }
            t.check_against(&s.open)?;
            let ws = s.open.workspace.clone();
            if st.workspaces.get(&ws.id).map(|w| w.incarnation) != Some(s.incarnation) {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "the session's workspace is gone: open it again",
                ));
            }
            Self::check_run(st, &t.run)?;
            if st.active.contains_key(&t.session) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            if let Some(cap) = &st.capacity
                && Self::running(st).0 >= cap.sessions
            {
                return Err(ApiError::new(ErrorCode::Exhausted, "no session capacity"));
            }
            Self::take_attempt(st, &c.id, &exec)?;
            st.active.insert(t.session.clone(), exec.clone());
            if let Some(s) = st.sessions.get_mut(&t.session) {
                s.view.phase = SessionPhase::Active;
                s.view.active = Some(exec.clone());
            }
            Ok(Decided {
                state: Observed::Running,
                execution: Some(exec.clone()),
                workspace: Some(ws),
                session: None,
                events: vec![(Subject::Execution(exec.clone()), EventBody::AttemptStarted)],
            })
        })
        .await
    }

    async fn session(&self, s: SessionId) -> ApiResult<SessionView> {
        match self.st().sessions.get(&s) {
            Some(x) => Ok(x.view.clone()),
            None => Err(ApiError::new(ErrorCode::Invalid, "no such session")),
        }
    }

    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt> {
        self.accept("release_session", &c, |st| {
            if st.active.contains_key(&c.body) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            let Some(s) = st.sessions.get_mut(&c.body) else {
                return Err(ApiError::new(ErrorCode::Invalid, "no such session"));
            };
            s.view.phase = SessionPhase::Released;
            let view = s.view.clone();
            let mut d = Decided::done(OperationResult::Unit);
            d.session = Some(c.body.clone());
            d.events = vec![(
                Subject::Session {
                    session: c.body.clone(),
                },
                EventBody::SessionChanged { view },
            )];
            Ok(d)
        })
        .await
    }
}

#[async_trait]
impl JobExecutor for FakeNode {
    async fn start_job(&self, c: Command<JobSpec>) -> ApiResult<Receipt> {
        let exec = ExecutionRef::Job {
            job: c.body.job.clone(),
            attempt: c.body.run.attempt.clone(),
        };
        self.accept("start_job", &c, |st| {
            c.body.check()?;
            Self::check_run(st, &c.body.run)?;
            if let Some(cap) = &st.capacity
                && Self::running(st).1 >= cap.jobs
            {
                return Err(ApiError::new(ErrorCode::Exhausted, "no job capacity"));
            }
            Self::take_attempt(st, &c.id, &exec)?;
            st.jobs.insert(exec.attempt().clone());
            st.job_specs
                .insert(exec.attempt().clone(), (c.body.clone(), String::new()));
            Ok(Decided {
                state: Observed::Running,
                execution: Some(exec.clone()),
                workspace: None,
                session: None,
                events: vec![(Subject::Execution(exec.clone()), EventBody::AttemptStarted)],
            })
        })
        .await
    }
}

#[async_trait]
impl ExecutionControl for FakeNode {
    async fn operation(&self, op: OperationId) -> ApiResult<Receipt> {
        self.receipt(&op)
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation").on(&op))
    }

    /// Records the intent (the cancel itself is done once recorded); the target ends when the test
    /// says so ([`FakeNode::finish`]), possibly normally.
    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        self.accept("cancel", &c, |st| {
            let Some(a) = st.inbox.get_mut(&target) else {
                return Err(ApiError::new(ErrorCode::Invalid, "no such operation"));
            };
            if !a.receipt.state.is_finished() {
                a.cancel_requested = true;
            }
            Ok(Decided::done(OperationResult::Unit))
        })
        .await
    }

    async fn abandon(&self, c: Command<Abandon>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        self.accept("abandon", &c, |st| {
            let Some(a) = st.inbox.get(&target) else {
                return Err(ApiError::new(ErrorCode::Invalid, "no such operation"));
            };
            if !matches!(a.receipt.state, Observed::Unknown { .. }) {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "only an unknown execution is abandoned (cancel a live one)",
                ));
            }
            let exec = a.execution.clone();
            // as the real Node ends it: interrupted, its waiting permissions expired
            let completion = Completion {
                outcome: Outcome::Interrupted,
                stop_reason: "abandoned".into(),
                error: Some(ApiError::new(ErrorCode::OutcomeUnknown, "abandoned")),
                result: vec![],
                continuation: None,
                transient: false,
                acted: true,
                job: None,
            };
            if let Some(a) = st.inbox.get_mut(&target) {
                a.receipt.state = Observed::Finished {
                    outcome: Outcome::Interrupted,
                    result: Some(OperationResult::Execution(completion.clone())),
                    error: completion.error.clone(),
                };
            }
            let mut events = vec![];
            if let Some(x) = &exec {
                let expired: Vec<PermissionId> = st
                    .waiting
                    .iter()
                    .filter(|(_, r)| r.execution == *x)
                    .map(|(k, _)| k.clone())
                    .collect();
                for p in expired {
                    st.waiting.remove(&p);
                    events.push((
                        Subject::Execution(x.clone()),
                        EventBody::PermissionExpired { id: p },
                    ));
                }
                events.push((
                    Subject::Execution(x.clone()),
                    EventBody::AttemptEnded { completion },
                ));
            }
            if let Some(ExecutionRef::Turn { session, .. }) = &exec
                && st.active.get(session) == exec.as_ref()
            {
                st.active.remove(session);
                if let Some(s) = st.sessions.get_mut(session) {
                    s.view.phase = SessionPhase::Released;
                    s.view.active = None;
                }
            }
            if let Some(ExecutionRef::Job { attempt, .. }) = &exec {
                st.jobs.remove(attempt);
            }
            let mut d = Decided::done(OperationResult::Unit);
            d.events = events;
            Ok(d)
        })
        .await
    }

    /// As LocalNode retires an unknown control: only the exact unknown target, never an execution
    /// (abandon it), refused while the session has live work. The target ends interrupted (its
    /// effects stay as they are, nothing is replayed); a session control lets its session go.
    async fn recover(&self, c: Command<Recover>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        self.accept("recover", &c, |st| {
            let Some(a) = st.inbox.get(&target) else {
                return Err(ApiError::new(ErrorCode::Invalid, "no such operation"));
            };
            if a.digest != c.body.expected || !matches!(a.receipt.state, Observed::Unknown { .. }) {
                return Err(ApiError::new(ErrorCode::Stale, "recovery target changed"));
            }
            if a.execution.is_some() {
                return unsupported("recover execution: use abandon");
            }
            let (method, session) = (a.method, a.session.clone());
            if let Some(s) = &session
                && let Some(x) = st.active.get(s)
                && st.attempts.get(x.attempt()).is_some_and(|(_, op)| {
                    st.inbox
                        .get(op)
                        .is_some_and(|o| !matches!(o.receipt.state, Observed::Unknown { .. }))
                })
            {
                return Err(ApiError::new(ErrorCode::Busy, "session has live work"));
            }
            let state = Observed::Finished {
                outcome: Outcome::Interrupted,
                result: None,
                error: Some(ApiError::new(
                    ErrorCode::OutcomeUnknown,
                    "control retired; effects preserved, not replayed",
                )),
            };
            if let Some(a) = st.inbox.get_mut(&target) {
                a.receipt.state = state.clone();
            }
            let mut events = vec![(
                Subject::Operation {
                    operation: target.clone(),
                },
                EventBody::OperationChanged {
                    receipt: Receipt {
                        operation: target.clone(),
                        state,
                    },
                },
            )];
            if matches!(
                method,
                "open_session" | "configure_session" | "release_session"
            ) && let Some(s) = &session
                && let Some(row) = st.sessions.get_mut(s)
            {
                row.view.phase = SessionPhase::Released;
                events.push((
                    Subject::Session { session: s.clone() },
                    EventBody::SessionChanged {
                        view: row.view.clone(),
                    },
                ));
            }
            let mut d = Decided::done(OperationResult::Unit);
            d.events = events;
            Ok(d)
        })
        .await
    }

    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt> {
        let res = c.body.clone();
        self.accept("resolve_permission", &c, |st| {
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
            if req.expires_at.0 <= st.now {
                return Err(ApiError::new(ErrorCode::Expired, "the request expired"));
            }
            if !req.choices.contains(&res.decision) {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "not one of the offered choices",
                ));
            }
            // checked again right before the approval is used
            if res.decision == PermissionDecision::AllowOnce {
                self.check_preconditions(st, &req.preconditions)?;
            }
            let exec = req.execution.clone();
            let op = Self::live_op(st, &exec)?;
            st.waiting.remove(&res.id);
            st.resolved.insert(res.id.clone(), res.clone());
            if let Some(a) = st.inbox.get_mut(&op)
                && let Observed::Waiting { on } = &a.receipt.state
            {
                let on: Vec<WaitRef> = on
                    .iter()
                    .filter(|w| **w != WaitRef::Permission(res.id.clone()))
                    .cloned()
                    .collect();
                a.receipt.state = if on.is_empty() {
                    Observed::Running
                } else {
                    Observed::Waiting { on }
                };
            }
            let mut d = Decided::done(OperationResult::Unit);
            d.events = vec![(
                Subject::Execution(exec),
                EventBody::PermissionResolved {
                    resolution: res.clone(),
                },
            )];
            Ok(d)
        })
        .await
    }

    async fn snapshot(&self, page: PageRequest) -> ApiResult<Snapshot> {
        let mut st = self.st();
        let limit = page.limit.max(1) as usize;
        let (token, offset) = match &page.cursor {
            None => {
                let entries: Vec<SnapshotEntry> = st
                    .inbox
                    .values()
                    .map(|a| SnapshotEntry {
                        receipt: a.receipt.clone(),
                        method: a.method.into(),
                        digest: a.digest.clone(),
                        execution: a.execution.clone(),
                        workspace: a.workspace.clone(),
                        pending: a
                            .execution
                            .as_ref()
                            .map(|e| {
                                st.waiting
                                    .values()
                                    .filter(|r| r.execution == *e)
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default(),
                        cancel_requested: a.cancel_requested,
                    })
                    .collect();
                let heads = Self::heads(&st);
                let token = Self::next(&mut st);
                st.snapshots.insert(token, (entries, heads));
                (token, 0usize)
            }
            Some(c) => {
                let bad = || ApiError::new(ErrorCode::Invalid, "not a snapshot cursor");
                let (t, o) = c
                    .as_str()
                    .strip_prefix("snap")
                    .and_then(|r| r.split_once('.'))
                    .ok_or_else(bad)?;
                (t.parse().map_err(|_| bad())?, o.parse().map_err(|_| bad())?)
            }
        };
        let Some((entries, heads)) = st.snapshots.get(&token) else {
            return Err(ApiError::new(ErrorCode::Expired, "this snapshot is gone"));
        };
        let page: Vec<SnapshotEntry> = entries.iter().skip(offset).take(limit).cloned().collect();
        let heads = heads.clone();
        let more = offset + page.len() < entries.len();
        let next = more.then(|| id(format!("snap{token}.{}", offset + page.len())));
        if !more {
            st.snapshots.remove(&token);
        }
        drop(st);
        if !more {
            self.at(Point::AfterSnapshot);
        }
        Ok(Snapshot {
            entries: page,
            heads,
            next,
        })
    }

    async fn streams(&self) -> ApiResult<Vec<StreamHead>> {
        Ok(Self::heads(&self.st()))
    }

    async fn replay(&self, from: Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        let st = self.st();
        let Some(s) = st.streams.get(&from.stream) else {
            return Err(ApiError::new(ErrorCode::Invalid, "no such stream"));
        };
        if s.epoch != from.epoch {
            return Err(ApiError::new(ErrorCode::Gap, "another stream epoch"));
        }
        if from.seq < s.acknowledged {
            return Err(ApiError::new(
                ErrorCode::Gap,
                "already acknowledged and dropped",
            ));
        }
        if from.seq > s.produced {
            return Err(ApiError::new(ErrorCode::Invalid, "ahead of the stream"));
        }
        let mut events = vec![];
        let mut used = 0usize;
        for e in s.kept.iter().filter(|e| e.cursor.seq > from.seq) {
            let n = serde_json::to_vec(e).map(|v| v.len()).unwrap_or(usize::MAX);
            if used.saturating_add(n) > max_bytes as usize {
                if events.is_empty() {
                    return Err(ApiError::new(
                        ErrorCode::Exhausted,
                        "the next event does not fit in max_bytes",
                    ));
                }
                break;
            }
            used += n;
            events.push(e.clone());
        }
        Ok(EventBatch { events })
    }

    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()> {
        self.apply_ack(&ack)
    }
}

#[async_trait]
impl WorkspaceExecutor for FakeNode {
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        let node = self.node.clone();
        self.accept("prepare_workspace", &c, |st| {
            let root = match &c.body.kind {
                PrepareKind::Existing { dir } => Some(dir.root.clone()),
                PrepareKind::Worktree { repository, .. } => Some(repository.root.clone()),
                PrepareKind::Scratch => None,
            };
            if let Some(r) = &root
                && !st.roots.is_empty()
                && Self::root_access(st, r)? == RootAccess::ReadOnly
            {
                return Err(ApiError::new(ErrorCode::Denied, "a readonly root"));
            }
            if let PrepareKind::Worktree { at: Some(at), .. } = &c.body.kind
                && !st.roots.is_empty()
                && Self::root_access(st, &at.root)? == RootAccess::ReadOnly
            {
                return Err(ApiError::new(ErrorCode::Denied, "a readonly root"));
            }
            let spec = digest_of(&c.body.kind)?;
            let wid: WorkspaceId = id(derived_id("w", &c.body.resource));
            let view = match st.workspaces.get(&wid) {
                Some(w) if w.resource == c.body.resource && w.spec == spec => w.view.clone(),
                Some(_) => {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "this resource's workspace was prepared otherwise",
                    ));
                }
                None => {
                    let branch = match &c.body.kind {
                        PrepareKind::Worktree { branch, .. } => Some(branch.clone()),
                        _ => None,
                    };
                    let incarnation = Self::next(st);
                    let v = WorkspaceView {
                        workspace: WorkspaceRef {
                            node: node.clone(),
                            id: wid.clone(),
                        },
                        revision: Self::revision(st),
                        usable: true,
                        branch,
                        display: format!("fake/{wid}"),
                    };
                    st.workspaces.insert(
                        wid.clone(),
                        Workspace {
                            resource: c.body.resource.clone(),
                            spec,
                            view: v.clone(),
                            incarnation,
                            root,
                        },
                    );
                    v
                }
            };
            let mut d = Decided::done(OperationResult::Workspace(view.clone()));
            d.workspace = Some(view.workspace);
            Ok(d)
        })
        .await
    }

    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        self.mine(&w)?;
        self.st()
            .workspaces
            .get(&w.id)
            .map(|x| x.view.clone())
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))
    }

    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        self.accept("tidy_workspace", &c, |st| {
            let w = &c.body.workspace;
            let in_use = st.inbox.values().any(|a| {
                a.workspace.as_ref() == Some(w)
                    && a.execution.is_some()
                    && !a.receipt.state.is_finished()
            });
            if in_use {
                return Err(ApiError::new(ErrorCode::Busy, "the workspace is in use"));
            }
            match st.workspaces.get(&w.id) {
                Some(x) if x.view.revision != c.body.expected => {
                    Err(ApiError::new(ErrorCode::Conflict, "changed since"))
                }
                Some(_) => {
                    st.workspaces.remove(&w.id);
                    // sessions opened in it cannot run anymore (they are released; opening again
                    // needs a workspace)
                    for s in st.sessions.values_mut() {
                        if s.open.workspace == *w {
                            s.view.phase = SessionPhase::Released;
                        }
                    }
                    Ok(Decided::done(OperationResult::Unit))
                }
                None => Err(ApiError::new(ErrorCode::Invalid, "no such workspace")),
            }
        })
        .await
    }

    async fn git_facts(&self, q: GitQuery) -> ApiResult<GitFacts> {
        self.mine(&q.workspace)?;
        let st = self.st();
        if !st.workspaces.contains_key(&q.workspace.id) {
            return Err(ApiError::new(ErrorCode::Invalid, "no such workspace"));
        }
        let Some(facts) = st.git.get(&q.workspace.id) else {
            return unsupported("git facts nobody scripted");
        };
        let mut reads = vec![];
        for r in &q.reads {
            match facts.reads.iter().find(|f| f.read == *r) {
                Some(f) => reads.push(f.clone()),
                None => return unsupported("a git read nobody scripted"),
            }
        }
        Ok(GitFacts {
            reads,
            ..facts.clone()
        })
    }

    /// Checks the decision's revision and HEAD against the scripted facts, then moves them on: a
    /// new revision, and a new HEAD after a rebase or a merge.
    async fn git_apply(&self, c: Command<GitApply>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        let auth = match &c.body.action {
            GitAction::Push { auth: Some(a), .. } => {
                if !matches!(a.how, SecretDelivery::Header { .. }) {
                    return Err(ApiError::new(
                        ErrorCode::Invalid,
                        "a push credential goes as a header",
                    )
                    .on(&c.id));
                }
                vec![a.clone()]
            }
            _ => vec![],
        };
        let w = &c.body.workspace;
        // every refusal before the credential is taken
        let check = |st: &State| -> ApiResult<()> {
            Self::writable(st, w)?;
            let Some(facts) = st.git.get(&w.id) else {
                return unsupported("git facts nobody scripted");
            };
            if facts.revision != c.body.expected || facts.head != c.body.expected_head {
                return Err(ApiError::new(ErrorCode::Stale, "changed since"));
            }
            if let GitAction::Push {
                branch,
                expected_local,
                ..
            } = &c.body.action
            {
                let local = facts
                    .refs
                    .iter()
                    .find(|(r, _)| r == branch)
                    .and_then(|(_, sha)| sha.as_ref())
                    .unwrap_or(&facts.head);
                if local != expected_local {
                    return Err(ApiError::new(ErrorCode::Stale, "the branch moved"));
                }
            }
            Ok(())
        };
        self.accept_taking("git_apply", &c, &auth, check, |st, _secrets| {
            let mut next = st.git[&w.id].clone();
            if matches!(
                c.body.action,
                GitAction::Rebase { .. } | GitAction::Merge { .. }
            ) {
                next.head = format!("c{}", Self::next(st));
            }
            next.revision = Self::revision(st);
            next.reads = vec![];
            st.git.insert(w.id.clone(), next.clone());
            let mut d = Decided::done(OperationResult::Git(next));
            d.workspace = Some(w.clone());
            Ok(d)
        })
        .await
    }

    /// Answers what the test gave ([`FakeNode::answer_exec`]), with its secrets masked.
    async fn exec(&self, c: Command<ExecSpec>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        if c.body
            .secrets
            .iter()
            .any(|r| matches!(r.how, SecretDelivery::Header { .. }))
        {
            return Err(ApiError::new(ErrorCode::Invalid, "a header is for a push only").on(&c.id));
        }
        let check = |st: &State| -> ApiResult<()> {
            Self::writable(st, &c.body.workspace)?;
            Self::check_policy(&c.body.policy)?;
            if c.body.limits.deadline.0 <= st.now {
                return Err(ApiError::new(ErrorCode::Expired, "deadline already passed"));
            }
            Ok(())
        };
        self.accept_taking("exec", &c, &c.body.secrets, check, |st, secrets| {
            let r = st.exec_answers.pop_front().unwrap_or(ExecResult {
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                truncated: false,
            });
            let r = ExecResult {
                stdout: mask(&r.stdout, secrets),
                stderr: mask(&r.stderr, secrets),
                ..r
            };
            let mut d = Decided::done(OperationResult::Exec(r));
            d.workspace = Some(c.body.workspace.clone());
            Ok(d)
        })
        .await
    }
    async fn list_dir(&self, _p: PathRef, _page: PageRequest) -> ApiResult<Page<FileInfo>> {
        unsupported("files in a workspace")
    }
    /// The files and directories right below `at`.
    async fn list_root(&self, at: RootPath, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        let st = self.st();
        Self::root_access(&st, &at.root)?;
        let mut items: BTreeMap<String, FileInfo> = BTreeMap::new();
        for ((root, rel), f) in &st.files {
            if *root != at.root {
                continue;
            }
            let Some(rest) = below(&at.rel, rel).filter(|r| !r.is_empty()) else {
                continue;
            };
            let name = rest.split('/').next().unwrap_or(rest);
            let path = match at.rel.as_str() {
                "" => name.to_string(),
                base => format!("{base}/{name}"),
            };
            let info = if name == rest {
                FileInfo {
                    path: RelPath::new(path.clone()).expect("a stored path"),
                    is_dir: false,
                    size: f.bytes.len() as u64,
                    version: f.version.clone(),
                    too_big: false,
                    mtime: None,
                }
            } else {
                FileInfo {
                    path: RelPath::new(path.clone()).expect("a stored path"),
                    is_dir: true,
                    size: 0,
                    version: id("dir"),
                    too_big: false,
                    mtime: None,
                }
            };
            items.insert(path, info);
        }
        let items: Vec<FileInfo> = items.into_values().collect();
        let (items, next) = page_of(&items, &page)?;
        Ok(Page { items, next })
    }
    async fn read_file(&self, q: FileRead) -> ApiResult<crate::blob::ReadChunk> {
        let FileAddress::Root(at) = &q.path else {
            return unsupported("files in a workspace");
        };
        let st = self.st();
        Self::root_access(&st, &at.root)?;
        let Some(f) = st.files.get(&(at.root.clone(), at.rel.clone())) else {
            return Err(ApiError::new(ErrorCode::Invalid, "no such file"));
        };
        if q.expected.as_ref().is_some_and(|v| *v != f.version) {
            return Err(ApiError::new(ErrorCode::Conflict, "the file changed"));
        }
        let from = (q.offset as usize).min(f.bytes.len());
        let to = (from + q.max as usize).min(f.bytes.len());
        Ok(crate::blob::ReadChunk {
            offset: from as u64,
            bytes: f.bytes[from..to].to_vec(),
            eof: to == f.bytes.len(),
        })
    }
    /// Lists every wanted file below the root at once (a snapshot read page by page), with the
    /// include and exclude patterns of [`crate::glob`] (matched below the scanned directory).
    async fn scan(&self, c: Command<ScanSpec>) -> ApiResult<Receipt> {
        if c.body
            .include
            .iter()
            .chain(&c.body.exclude)
            .any(|p| !crate::glob::valid_pattern(p))
        {
            return Err(ApiError::new(ErrorCode::Invalid, "a scan pattern"));
        }
        self.accept("scan", &c, |st| {
            let spec = &c.body;
            Self::root_access(st, &spec.root.root)?;
            let rules = crate::glob::Rules::new(spec.include.clone(), spec.exclude.clone());
            // a file under a directory the scan does not enter is not seen
            let entered = |rel: &str| {
                let dirs: Vec<&str> = rel.split('/').collect();
                (1..dirs.len()).all(|n| {
                    !crate::glob::never_entered(dirs[n - 1])
                        && !rules.skips_dir(&dirs[..n].join("/"))
                })
            };
            let mut entries: Vec<FileInfo> = st
                .files
                .iter()
                .filter(|((root, _), _)| *root == spec.root.root)
                .filter_map(|((_, rel), f)| {
                    let rest = below(&spec.root.rel, rel).filter(|r| !r.is_empty())?;
                    (entered(rest) && rules.wants_file(rest)).then_some((rel, f))
                })
                .map(|(rel, f)| FileInfo {
                    path: rel.clone(),
                    is_dir: false,
                    size: f.bytes.len() as u64,
                    version: f.version.clone(),
                    too_big: f.bytes.len() as u64 > spec.limits.max_file_bytes,
                    mtime: None,
                })
                .collect();
            let mut issues = st
                .scan_issues
                .get(&spec.root.root)
                .cloned()
                .unwrap_or_default();
            let max = spec.limits.max_files as usize;
            if entries.len() > max {
                issues.push(ScanIssue::OverLimit {
                    count: (entries.len() - max) as u64,
                });
                entries.truncate(max);
            }
            // the same tree gives the same manifest
            let manifest = id(digest_of(&(&entries, &issues))?.to_string());
            st.scans.insert(
                c.id.clone(),
                FakeScan {
                    manifest,
                    entries,
                    issues,
                },
            );
            Ok(Decided::done(OperationResult::Pages {
                operation: c.id.clone(),
            }))
        })
        .await
    }
    async fn scan_page(&self, scan: OperationId, page: PageRequest) -> ApiResult<ScanPage> {
        let st = self.st();
        let Some(s) = st.scans.get(&scan) else {
            return Err(ApiError::new(ErrorCode::Invalid, "no such scan").on(&scan));
        };
        // cursors name their scan: one from another scan is refused
        let d = digest_of(&scan)?;
        let tag = &d.as_str()[7..15];
        let from = match &page.cursor {
            None => 0,
            Some(c) => c
                .as_str()
                .strip_prefix(&format!("s{tag}."))
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n <= s.entries.len())
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "not a cursor of this scan"))?,
        };
        let to = (from + page.limit.max(1) as usize).min(s.entries.len());
        let entries = s.entries[from..to].to_vec();
        let next = (to < s.entries.len()).then(|| id(format!("s{tag}.{to}")));
        let last = next.is_none();
        Ok(ScanPage {
            scan,
            manifest: s.manifest.clone(),
            entries,
            next,
            complete: last && !s.issues.iter().any(ScanIssue::hides_files),
            issues: if last { s.issues.clone() } else { vec![] },
        })
    }
}

#[async_trait]
impl Discovery for FakeNode {
    async fn probe(&self, _fresh: bool) -> ApiResult<Capabilities> {
        let st = self.st();
        let capacity = st.capacity.clone().unwrap_or(Capacity {
            sessions: u32::MAX,
            jobs: u32::MAX,
            execs: 0,
        });
        Ok(Capabilities {
            node: self.node.clone(),
            os: "fake".into(),
            version: "0".into(),
            harnesses: vec![HarnessCapability {
                name: HARNESS.into(),
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
            roots: st.roots.clone(),
            capacity,
            features: st.features.clone(),
        })
    }
    async fn history(&self, p: PageRequest) -> ApiResult<Page<HistoryEntry>> {
        let all: Vec<HistoryEntry> = self.st().history.iter().map(|(e, _)| e.clone()).collect();
        let (items, next) = page_of(&all, &p)?;
        Ok(Page { items, next })
    }
    async fn import_history(&self, _c: Command<HistoryImport>) -> ApiResult<Receipt> {
        unsupported("history")
    }
    async fn import_page(&self, _i: OperationId, _p: PageRequest) -> ApiResult<ImportedPage> {
        unsupported("history")
    }
    async fn locate(&self, source: HistoryRef) -> ApiResult<HistoryLocation> {
        self.st()
            .history
            .iter()
            .find(|(e, _)| e.source == source)
            .map(|(_, l)| l.clone())
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "not a history this node lists"))
    }
}

impl Node for FakeNode {
    fn id(&self) -> &NodeId {
        &self.node
    }
}

/// An [`EventSink`] that commits like the Hub must: each `(stream, epoch, seq)` once (the same key
/// with other content is a conflict), and acknowledges only the contiguous prefix it holds.
#[derive(Default)]
pub struct RecordingSink {
    held: Mutex<BTreeMap<(StreamId, Epoch, u64), NodeEvent>>,
    fail: Mutex<bool>,
}

impl RecordingSink {
    /// Committed events in stream order.
    pub fn events(&self) -> Vec<NodeEvent> {
        self.held.lock().unwrap().values().cloned().collect()
    }
    /// The body types of committed events, in stream order.
    pub fn kinds(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|e| {
                serde_json::to_value(&e.body).unwrap()["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
    /// Refuse commits (a Hub whose database is down).
    pub fn set_failing(&self, on: bool) {
        *self.fail.lock().unwrap() = on;
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        if *self.fail.lock().unwrap() {
            return Err(ApiError::new(ErrorCode::Offline, "hub store unavailable"));
        }
        let mut held = self.held.lock().unwrap();
        // the whole batch is checked (against itself and what is held) before anything is kept
        let mut seen: BTreeMap<(StreamId, Epoch, u64), &NodeEvent> = BTreeMap::new();
        for e in &batch.events {
            let k = (
                e.cursor.stream.clone(),
                e.cursor.epoch.clone(),
                e.cursor.seq,
            );
            let clash = held.get(&k).is_some_and(|old| old != e)
                || seen.get(&k).is_some_and(|old| *old != e);
            if clash {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "another event at this position",
                ));
            }
            seen.insert(k, e);
        }
        // the ack covers the streams of this batch only, as the Hub's: an epoch of an older
        // journal it still holds is none of this Node's business
        let touched: BTreeSet<(StreamId, Epoch)> = batch
            .events
            .iter()
            .map(|e| (e.cursor.stream.clone(), e.cursor.epoch.clone()))
            .collect();
        for e in batch.events {
            let k = (
                e.cursor.stream.clone(),
                e.cursor.epoch.clone(),
                e.cursor.seq,
            );
            held.entry(k).or_insert(e);
        }
        // contiguous frontier per (stream, epoch)
        let mut frontier: BTreeMap<(StreamId, Epoch), u64> = BTreeMap::new();
        for (s, ep, seq) in held.keys() {
            if !touched.contains(&(s.clone(), ep.clone())) {
                continue;
            }
            let f = frontier.entry((s.clone(), ep.clone())).or_insert(0);
            if *seq == *f + 1 {
                *f = *seq;
            }
        }
        Ok(EventAck {
            durable: frontier
                .into_iter()
                .filter(|(_, seq)| *seq > 0)
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
            harness: HARNESS.into(),
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

    /// The workspace [`prepare`] gives for `resource` on `node`.
    pub fn workspace(node: &str, resource: &str) -> WorkspaceRef {
        WorkspaceRef {
            node: id(node),
            id: id(derived_id("w", resource)),
        }
    }

    pub fn prepare(resource: &str) -> Prepare {
        Prepare {
            resource: resource.into(),
            kind: PrepareKind::Scratch,
        }
    }

    /// Prepare `item:<session>` and open the session in it.
    pub async fn ready(n: &FakeNode, session: &str) {
        let res = format!("item:{session}");
        n.prepare_workspace(cmd(&format!("op_prep_{session}"), prepare(&res)))
            .await
            .unwrap();
        n.open_session(cmd(
            &format!("op_open_{session}"),
            open(session, n.id().as_str()),
        ))
        .await
        .unwrap();
    }

    pub fn open(session: &str, node: &str) -> OpenSession {
        OpenSession {
            session: id(session),
            workspace: workspace(node, &format!("item:{session}")),
            choice: choice(),
            policy: policy(),
            resume: None,
        }
    }

    pub fn limits() -> Limits {
        Limits {
            deadline: Timestamp(10_000_000),
            offline_until: Timestamp(10_000_000),
            max_output_bytes: 1 << 20,
            max_tokens: None,
            max_cost: None,
        }
    }

    /// A turn in the session's own workspace on `hub`.
    pub fn turn(session: &str, turn: &str, attempt: &str, text: &str) -> TurnSpec {
        turn_on("hub", session, turn, attempt, text)
    }

    pub fn turn_on(node: &str, session: &str, turn: &str, attempt: &str, text: &str) -> TurnSpec {
        TurnSpec {
            session: id(session),
            turn: id(turn),
            run: RunSpec {
                attempt: id(attempt),
                choice: choice(),
                directory: RunDirectory::Workspace {
                    workspace: workspace(node, &format!("item:{session}")),
                },
                prompt: vec![Part::Text { text: text.into() }],
                policy: policy(),
                limits: limits(),
                continuation: None,
            },
        }
    }

    /// A think job without tools.
    pub fn job(job: &str, attempt: &str, text: &str) -> JobSpec {
        JobSpec {
            job: id(job),
            run: RunSpec {
                attempt: id(attempt),
                choice: choice(),
                directory: RunDirectory::Scratch,
                prompt: vec![Part::Text { text: text.into() }],
                policy: policy(),
                limits: limits(),
                continuation: None,
            },
            work: JobWork::Think(ThinkJob {
                instructions: None,
                tools: ThinkToolMode::None,
            }),
        }
    }

    /// A research job in its scratch directory, with Steve's tools and the default idle limit.
    pub fn research(job_id: &str, attempt: &str, text: &str) -> JobSpec {
        JobSpec {
            work: JobWork::Research(ResearchJob {
                cwd: None,
                readable: vec![],
                budget: ResearchBudget {
                    max_turns: None,
                    max_tools: None,
                    idle_ms: 600_000,
                },
                steve_tools: true,
                earlier: vec![],
            }),
            ..job(job_id, attempt, text)
        }
    }

    pub fn exec_job(job: &str, attempt: &str) -> ExecutionRef {
        ExecutionRef::Job {
            job: id(job),
            attempt: id(attempt),
        }
    }

    /// A job's answer: one text part, none when empty.
    pub fn text(s: &str) -> Vec<Part> {
        if s.is_empty() {
            vec![]
        } else {
            vec![Part::Text { text: s.into() }]
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
            transient: false,
            acted: false,
            job: None,
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
            digest: permission_digest(&action, &[]).unwrap(),
            action,
            preconditions: vec![],
            choices: vec![PermissionDecision::AllowOnce, PermissionDecision::Reject],
            expires_at: Timestamp(10_000_000),
            own_pids: vec![],
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
}

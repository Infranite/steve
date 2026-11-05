//! steve-core and steve-acp wired together:
//! - [`HubHost`]: steve-acp's [`Host`](steve_acp::Host) over steve-store; `turn_end` and `mcp_endpoint` go to the
//!   core. Lock order is core → acp: the runner calls the store-only methods under its own lock, never the core.
//! - [`AcpRuntime`]: steve-core's [`AgentRuntime`](steve_core::AgentRuntime) over [`Acp`].
//! - [`CoreDomain`]: the HTTP layer's [`Domain`] over [`Core`]. The core's mutating calls are synchronous (and
//!   may run git under its lock), so they run on the blocking pool.
//! - [`CoreMcp`]: steve-mcp's backend, the five tools → `core.tool_*`.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use steve_acp::{Acp, Host, Message, PermissionRequest, SessionInfo, StoredFile, Usage};
use steve_core::{Core, CoreError, ItemAction, SendNow, SupplementMode};
use steve_store::{SessionPatch, Store};

use crate::backend::{BResult, BackendError, CreateItem, Domain, NewTag, Obj, Route, Tier};

fn cerr(e: CoreError) -> BackendError {
    match e {
        // a module's refusal keeps its status (`409 邮件已经发出，不能撤回`); its internal errors stay generic
        CoreError::Module(e) => BackendError {
            status: e.status(),
            message: match e {
                steve_modules::ApiError::Internal(m) => {
                    tracing::error!("module: {m}");
                    steve_i18n::t!("hub.error.generic")
                }
                e => e.to_string(),
            },
        },
        CoreError::Status(status, message) => BackendError { status, message },
        // the store refused a write that contradicts the lifecycle: the user's conflict, not a crash
        CoreError::Store(steve_store::StoreError::Conflict(message)) => BackendError {
            status: 409,
            message,
        },
        e => BackendError::msg(e.to_string()),
    }
}

/// A thread action's result and whether it had been handled before.
fn handled(h: steve_core::thread::Handled) -> BResult<(Obj, bool)> {
    let already = h.already;
    Ok((to_obj(h)?, already))
}

fn to_obj<T: Serialize>(x: T) -> BResult<Obj> {
    match serde_json::to_value(x).map_err(|e| BackendError::msg(e.to_string()))? {
        Value::Object(o) => Ok(o),
        other => Err(BackendError::msg(format!(
            "expected an object, got {other}"
        ))),
    }
}

fn from_json<T: DeserializeOwned>(v: Value) -> BResult<T> {
    serde_json::from_value(v).map_err(|e| BackendError::msg(e.to_string()))
}

/// Runs a synchronous core call on the blocking pool, in the request's language.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> BResult<T> {
    tokio::task::spawn_blocking(steve_i18n::carry(f))
        .await
        .map_err(|e| BackendError::msg(format!("task failed: {e}")))?
        .map_err(cerr)
}

/// A synchronous call that may block (git, SQLite) from inside an async task, without stalling the runtime.
fn block_here<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

// ── steve-acp Host ──

/// What the ACP runner needs from the Hub: sessions, events and the queue in the store; the core for
/// turn ends and MCP tokens (set once the core exists, since the core needs the runner first).
pub struct HubHost {
    pub store: Store,
    pub data_dir: PathBuf,
    pub core: OnceLock<Core>,
    /// Schedules count session costs against their budget (`on_session_cost` after every cost update).
    pub flow: OnceLock<std::sync::Arc<steve_flow::FlowCx>>,
    /// The YOLO executor: gets the full input of each tool permission request, and answers it once
    /// the runner waits for it (set once the core exists).
    pub yolo: OnceLock<steve_core::yolo::Engine>,
    /// The local Node (`STEVE_NODE_MODE=local`), so YOLO can judge its requests against the bound
    /// workspace's real directory. Unset (a remote Node, or none): YOLO asks.
    pub local_node: OnceLock<std::sync::Arc<steve_node::LocalNode>>,
    /// The first `user` event of the turn being started, for `agent_turns.user_seq`.
    pub user_seqs: super::acp_stats::UserSeqs,
    /// Per session, what the runner's turn said so far (its tool updates carry only what changed),
    /// and when it last said something.
    pub transcripts: std::sync::Mutex<Readers>,
}

pub type Readers = std::collections::HashMap<String, (steve_transcript::LegacyReader, Instant)>;

/// How long a session's transcript reader is kept with nothing new from its runner.
const TRANSCRIPT_IDLE: Duration = Duration::from_secs(3600);

impl HubHost {
    /// The runner's row as transcript entries: its ACP updates are read (with what its turn said
    /// before), the Hub's own kinds pass. A tool update the reader never saw the beginning of (the
    /// Hub restarted in between) is dropped rather than stored as a fragment that would replace
    /// the tool's card.
    fn transcript_of(
        &self,
        session_id: &str,
        kind: &str,
        data: Value,
    ) -> Vec<steve_transcript::Entry> {
        let mut readers = self.transcripts.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        readers.retain(|_, (_, used)| now.duration_since(*used) < TRANSCRIPT_IDLE);
        if !steve_transcript::is_legacy(kind, &data) {
            if kind == "user" {
                // a new turn: tool updates never cross one, what the reader kept is done with
                readers.remove(session_id);
            }
            return vec![steve_transcript::Entry {
                kind: kind.into(),
                data,
            }];
        }
        if !readers.contains_key(session_id) {
            // the store is read without holding every session's readers
            drop(readers);
            let cwd = self
                .store
                .sessions()
                .get(session_id)
                .ok()
                .flatten()
                .map(|s| s.cwd)
                .unwrap_or_default();
            readers = self.transcripts.lock().unwrap_or_else(|e| e.into_inner());
            readers
                .entry(session_id.to_string())
                .or_insert_with(|| (steve_transcript::LegacyReader::new(&cwd), now));
        }
        let Some((reader, used)) = readers.get_mut(session_id) else {
            return vec![];
        };
        *used = now;
        if !reader.knows(kind, &data) {
            tracing::warn!(
                "event {session_id} {kind}: an update of a tool not seen this turn, dropped"
            );
            return vec![];
        }
        reader.read(kind, &data)
    }

    fn patch(&self, sid: &str, p: SessionPatch) {
        if let Err(e) = self.store.sessions().patch(sid, &p) {
            tracing::error!("session {sid} patch: {e}");
        }
    }
}

impl Host for HubHost {
    fn session(&self, session_id: &str) -> Option<SessionInfo> {
        let s = self.store.sessions().get(session_id).ok().flatten()?;
        let perm_mode = match &s.bot_id {
            Some(b) => self.store.bots().get(b).ok().flatten().map(|b| b.perm_mode),
            None => None,
        };
        // YOLO widens `default` to `acceptEdits` for an item with its own directory (never further)
        let perm_mode = match self.core.get() {
            Some(c) => c.session_perm_mode(&s, perm_mode),
            None => perm_mode,
        };
        Some(SessionInfo {
            id: s.id,
            harness: s.harness,
            cwd: s.cwd,
            acp_session_id: s.acp_session_id,
            model: s.model,
            reasoning_effort: s.reasoning_effort,
            perm_mode,
            status: s.status,
        })
    }
    fn set_status(&self, session_id: &str, status: &str) {
        self.patch(
            session_id,
            SessionPatch {
                status: Some(status.into()),
                ..Default::default()
            },
        );
        // a turn starts: a “它停下了” card of its item no longer applies. Off this call, which may run under
        // the runner's lock or the core's.
        if matches!(status, "running" | "starting")
            && let Some(core) = self.core.get().cloned()
            && let Ok(h) = tokio::runtime::Handle::try_current()
        {
            let sid = session_id.to_string();
            h.spawn_blocking(move || {
                if let Err(e) = core.expire_stall(&sid) {
                    tracing::error!("expire stall {sid}: {e}");
                }
            });
        }
    }
    fn set_acp_session_id(&self, session_id: &str, acp_session_id: &str) {
        self.patch(
            session_id,
            SessionPatch {
                acp_session_id: Some(Some(acp_session_id.into())),
                ..Default::default()
            },
        );
    }
    fn set_reasoning_effort(&self, session_id: &str, effort: &str) {
        self.patch(
            session_id,
            SessionPatch {
                reasoning_effort: Some(
                    (effort != "default" && !effort.is_empty()).then(|| effort.into()),
                ),
                ..Default::default()
            },
        );
    }
    fn set_model(&self, session_id: &str, model: &str) {
        self.patch(
            session_id,
            SessionPatch {
                model: Some(Some(model.into())),
                ..Default::default()
            },
        );
    }
    fn set_usage(&self, session_id: &str, u: Usage) {
        let cost = u.cost.is_some();
        self.patch(
            session_id,
            SessionPatch {
                used: Some(u.used),
                size: Some(u.size),
                cost: u.cost.map(Some),
                ..Default::default()
            },
        );
        if cost
            && let Some(flow) = self.flow.get().cloned()
            && let Ok(h) = tokio::runtime::Handle::try_current()
        {
            let sid = session_id.to_string();
            h.spawn(async move { steve_flow::on_session_cost(&flow, &sid).await });
        }
    }
    fn add_event(&self, session_id: &str, kind: &str, data: Value) {
        // the runner writes `permission` right after it registered the request: YOLO may answer now
        let armed = (kind == "permission")
            .then(|| data["decision"].as_str().map(String::from))
            .flatten();
        for x in self.transcript_of(session_id, kind, data) {
            match self.store.events().add(session_id, &x.kind, x.data) {
                Ok(seq) if kind == "user" => self.user_seqs.user_event(session_id, seq),
                Ok(_) => {}
                Err(e) => tracing::error!("event {session_id} {kind}: {e}"),
            }
        }
        if let (Some(id), Some(engine)) = (armed, self.yolo.get()) {
            engine.arm(&id);
            let engine = engine.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    h.spawn_blocking(move || engine.kick(&id));
                }
                Err(_) => {
                    std::thread::spawn(move || engine.kick(&id));
                }
            }
        }
    }
    fn queue_add(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: &[steve_acp::FileRef],
    ) -> Result<bool, String> {
        let files: Vec<steve_store::FileRef> = files
            .iter()
            .map(|f| steve_store::FileRef {
                id: f.id.clone(),
                name: f.name.clone(),
                mime: f.mime.clone(),
                size: f.size as i64,
            })
            .collect();
        match self
            .store
            .queued()
            .add_unless_waiting(session_id, text, via, &files)
        {
            Ok(added) => Ok(added),
            Err(e) => {
                tracing::error!("queue {session_id}: {e}");
                Err(e.to_string())
            }
        }
    }
    fn queue_take(&self, session_id: &str) -> Vec<Message> {
        match self.store.queued().claim(session_id) {
            Ok(rows) => rows
                .into_iter()
                .map(|q| {
                    // delivered before the stop (maybe acted on): context to check, not a new request
                    let (text, via) = steve_core::resumed_text(&q.via, &q.text);
                    Message {
                        text,
                        via,
                        files: q
                            .files
                            .into_iter()
                            .map(|f| steve_acp::FileRef {
                                id: f.id,
                                name: f.name,
                                mime: f.mime,
                                size: f.size.max(0) as u64,
                            })
                            .collect(),
                    }
                })
                .collect(),
            Err(e) => {
                tracing::error!("queue take {session_id}: {e}");
                vec![]
            }
        }
    }
    fn dispatch_submitted(&self, session_id: &str) -> bool {
        match self.store.queued().submitted(session_id) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("dispatch receipt {session_id}: {e}");
                false
            }
        }
    }
    fn maintenance(&self) -> bool {
        self.store.maintenance()
    }
    fn dispatch_deferred(&self, session_id: &str) {
        if let Err(e) = self.store.queued().defer_claimed(session_id) {
            tracing::error!(
                "dispatch defer {session_id}: {e}; durable claim retained for next boot"
            );
        }
    }
    fn dispatch_finished(&self, session_id: &str, state: &str) {
        if let Err(e) = self.store.queued().receipt(session_id, state) {
            tracing::error!("dispatch finish {session_id}: {e}");
        }
    }
    fn queue_len(&self, session_id: &str) -> usize {
        self.store
            .queued()
            .list(session_id)
            .map(|q| q.len())
            .unwrap_or(0)
    }
    fn file(&self, file_id: &str) -> Option<StoredFile> {
        let f = self.store.files().get(file_id).ok().flatten()?;
        let path = self
            .data_dir
            .join("files")
            .join(&f.workspace_id)
            .join(f.sha256.get(..2).unwrap_or(""))
            .join(&f.id);
        path.exists().then(|| StoredFile {
            id: f.id,
            name: f.name,
            mime: f.mime,
            size: f.size.max(0) as u64,
            path,
        })
    }
    fn mcp_endpoint(&self, session: &SessionInfo) -> steve_acp::McpEndpoint {
        let Some(core) = self.core.get() else {
            return steve_acp::McpEndpoint::default();
        };
        match block_here(|| core.mcp_endpoint(&session.id)) {
            Ok(e) => steve_acp::McpEndpoint {
                url: e.url,
                token: e.token,
            },
            Err(e) => {
                tracing::error!("mcp endpoint {}: {e}", session.id);
                steve_acp::McpEndpoint::default()
            }
        }
    }
    fn permission_decision(&self, session_id: &str, req: PermissionRequest) -> Option<String> {
        let s = self.store.sessions().get(session_id).ok().flatten()?;
        let d = steve_store::NewDecision {
            workspace_id: s.workspace_id,
            kind: "approve".into(),
            subject_type: "tool_call".into(),
            subject_id: req.subject_id,
            item_id: s.item_id,
            session_id: Some(s.id),
            question: req.question,
            draft: Some(req.draft),
            options: Some(req.options),
            requested_by: s.bot_id.unwrap_or_else(|| "agent".into()),
        };
        let input = steve_core::yolo::ToolInput {
            raw: req.raw,
            kind: req.kind,
            locations: req.locations,
            own_dir: None,
            own_pids: None,
        };
        match self.store.decisions().create(&d) {
            Ok(x) => x.map(|x| {
                if let Some(engine) = self.yolo.get() {
                    engine.stash(&x.id, input);
                }
                x.id
            }),
            Err(e) => {
                tracing::error!("permission decision {session_id}: {e}");
                None
            }
        }
    }
    fn permissions_expired(&self, session_id: &str, decision_ids: &[String]) {
        for id in decision_ids {
            match self.store.decisions().resolve(
                id,
                &serde_json::json!({"superseded": "这一轮已经结束，授权不再需要"}),
                "expired",
                &serde_json::Value::Null,
            ) {
                Ok(_) => {}
                Err(e) => tracing::warn!("expire permission {id} of {session_id}: {e}"),
            }
        }
    }
    fn turn_end(&self, t: steve_acp::TurnEnd) {
        let Some(core) = self.core.get() else { return };
        let t = steve_core::TurnEnd {
            session_id: t.session_id,
            stop_reason: t.stop_reason,
            error: t.error,
        };
        if let Err(e) = block_here(|| core.on_turn_end(t)) {
            tracing::error!("turn end: {e}");
        }
    }
    fn turn_begin(&self, b: steve_acp::TurnBegin) -> Option<String> {
        super::acp_stats::turn_begin(&self.store, &self.user_seqs, b)
    }
    fn turn_finish(&self, f: steve_acp::TurnFinish) {
        super::acp_stats::turn_finish(&self.store, f);
    }
    fn autonomous_usage(&self, u: steve_acp::AutonomousUsage) {
        super::acp_stats::autonomous(&self.store, u);
    }
}

// ── steve-core AgentRuntime ──

/// The core's view of the ACP runner.
pub struct AcpRuntime(pub Acp);

/// How a legacy turn token reads as a [`steve_core::Running::attempt`].
const LEGACY_TURN: &str = "legacy:";

impl steve_core::AgentRuntime for AcpRuntime {
    fn prompt(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: &[steve_core::repo::FileRef],
    ) -> steve_core::Result<steve_core::Prompted> {
        let files = files
            .iter()
            .map(|f| steve_acp::FileRef {
                id: f.id.clone(),
                name: f.name.clone(),
                mime: f.mime.clone(),
                size: f.size.max(0) as u64,
            })
            .collect();
        // prompt_files never awaits (it spawns the turn and returns), so it can be driven synchronously here
        match futures::executor::block_on(self.0.prompt_files(session_id, text, via, files)) {
            Ok(steve_acp::Prompted::Sent) => Ok(steve_core::Prompted::Sent),
            Ok(steve_acp::Prompted::Queued) => Ok(steve_core::Prompted::Queued),
            Err(e) => Err(CoreError::Msg(e.to_string())),
        }
    }
    /// The runner's in-memory turn token, as `legacy:<token>` (no legacy turn survives a restart).
    fn running(&self, session_id: &str) -> Option<steve_core::Running> {
        self.0.current(session_id).map(|t| steve_core::Running {
            session: session_id.to_string(),
            attempt: format!("{LEGACY_TURN}{t}"),
        })
    }
    fn stop(&self, turn: &steve_core::Running) -> steve_core::Result<steve_core::Stop> {
        let token = turn
            .attempt
            .strip_prefix(LEGACY_TURN)
            .and_then(|t| t.parse().ok());
        Ok(match token {
            Some(t) if self.0.cancel_if(&turn.session, t) => steve_core::Stop::Requested,
            _ => steve_core::Stop::NotRunning,
        })
    }
    fn is_busy(&self, session_id: &str) -> bool {
        self.0.is_busy(session_id)
    }
    fn release(&self, session_id: &str) {
        self.0.release(session_id)
    }
    fn answer_permission(&self, decision_id: &str, option_id: Option<&str>) -> bool {
        self.0.answer_permission(decision_id, option_id)
    }
    fn hold(&self, on: bool) {
        self.0.hold(on)
    }
    fn is_holding(&self) -> bool {
        self.0.is_holding()
    }
    fn running_turns(&self) -> usize {
        self.0.running_turns()
    }
    fn flush(&self, session_id: &str, lead: Option<(&str, &str)>) -> bool {
        self.0.flush(
            session_id,
            lead.map(|(text, via)| Message {
                text: text.into(),
                via: via.into(),
                files: vec![],
            }),
        )
    }
    fn set_mode(&self, session_id: &str, mode: &str) -> bool {
        self.0.set_mode(session_id, mode)
    }
    fn agent_pid(&self, session_id: &str) -> Option<u32> {
        self.0.adapter_pid(session_id).filter(|p| *p > 1)
    }
}

/// What YOLO's tool guard reads of a Node's permission request: the action as Steve states it,
/// its paths under the bound workspace's real directory on this machine (`dir`, from the local
/// Node), the Node-reported processes of the harness. A path into another workspace, or any path
/// while the directory is unknown (a remote or stale Node), becomes `other`: the guard asks rather
/// than judge words against a guessed base. `own_pids` is always the Node's (never this machine's
/// `ps`), and the directory counts as YOLO's own only if the Node made it.
pub(crate) fn yolo_input(
    bound: Option<&steve_node_api::WorkspaceRef>,
    dir: Option<&steve_node::LocalDir>,
    r: &steve_node_api::PermissionRequest,
) -> steve_core::yolo::ToolInput {
    use steve_node_api::Action;
    let root = dir.filter(|_| bound.is_some());
    let abs = |p: &steve_node_api::PathRef| -> Option<String> {
        let root = root?;
        (Some(&p.workspace) == bound).then(|| {
            root.path
                .join(p.rel.as_str())
                .to_string_lossy()
                .into_owned()
        })
    };
    let all =
        |ps: &[steve_node_api::PathRef]| -> Option<Vec<String>> { ps.iter().map(abs).collect() };
    let other = |what: String| ("other", serde_json::json!({ "summary": what }), vec![]);
    let unknown = |what: &str| other(format!("Node 工作区的本机目录未知：{what}"));
    let (kind, raw, locations): (&str, Value, Vec<String>) = match &r.action {
        Action::Exec {
            command, cwd: c, ..
        } => match (root, c.as_ref().map(abs)) {
            (None, _) | (_, Some(None)) => unknown(command),
            // the base of relative shell words is always the real directory, never the item's cwd
            (Some(root), c) => (
                "execute",
                serde_json::json!({
                    "command": command,
                    "cwd": c.flatten().unwrap_or_else(|| root.path.to_string_lossy().into_owned()),
                }),
                vec![],
            ),
        },
        Action::Write { paths, .. } => match all(paths) {
            Some(ls) if !ls.is_empty() => ("edit", serde_json::json!({ "file_path": ls[0] }), ls),
            _ => unknown(&r.title),
        },
        Action::Delete { paths } => match all(paths) {
            Some(ls) if !ls.is_empty() => ("delete", serde_json::json!({}), ls),
            _ => unknown(&r.title),
        },
        Action::Move { from, to } => match (abs(from), abs(to)) {
            (Some(f), Some(t)) => (
                "move",
                serde_json::json!({ "source": f, "destination": t }),
                vec![],
            ),
            _ => unknown(&r.title),
        },
        Action::Read { paths } => match all(paths) {
            Some(ls) if !ls.is_empty() => ("read", serde_json::json!({}), ls),
            _ => unknown(&r.title),
        },
        Action::Search { query } => ("search", serde_json::json!({ "query": query }), vec![]),
        Action::Fetch { url } => ("fetch", serde_json::json!({ "url": url }), vec![]),
        Action::Tool { args, .. } => ("other", args.clone(), vec![]),
        Action::Unknown { summary } => other(summary.clone()),
    };
    steve_core::yolo::ToolInput {
        raw,
        kind: Some(kind.into()),
        locations,
        own_dir: root.filter(|d| d.owned).map(|d| d.path.clone()),
        own_pids: Some(r.own_pids.clone()),
    }
}

/// Turn ends of Node sessions reach the core the same way the ACP runner's do.
impl super::node_runtime::NodeDomain for HubHost {
    fn attempt_started(
        &self,
        session: &str,
        attempt: &str,
        ordinal: i64,
        user_seq: Option<i64>,
        at: i64,
    ) {
        super::acp_stats::node_attempt_started(
            &self.store,
            session,
            attempt,
            ordinal,
            user_seq,
            at,
        );
        // the coordinator set the session running: a “它停下了” card of its item no longer
        // applies (as with the legacy set_status)
        if let Some(core) = self.core.get().cloned()
            && let Ok(h) = tokio::runtime::Handle::try_current()
        {
            let sid = session.to_string();
            h.spawn_blocking(move || {
                if let Err(e) = core.expire_stall(&sid) {
                    tracing::error!("expire stall {sid}: {e}");
                }
            });
        }
    }
    fn attempt_finished(
        &self,
        session: &str,
        attempt: &str,
        completion: &steve_node_api::Completion,
        facts: &super::node_runtime::AttemptFacts,
        at: i64,
    ) {
        super::acp_stats::node_attempt_finished(
            &self.store,
            session,
            attempt,
            completion,
            facts,
            at,
        );
    }
    fn cost_changed(&self, session: &str) {
        if let Some(flow) = self.flow.get().cloned()
            && let Ok(h) = tokio::runtime::Handle::try_current()
        {
            let sid = session.to_string();
            h.spawn(async move { steve_flow::on_session_cost(&flow, &sid).await });
        }
    }
    fn permission_arming(
        &self,
        session: &str,
        decision: &str,
        request: &steve_node_api::PermissionRequest,
    ) {
        let Some(engine) = self.yolo.get() else {
            return;
        };
        // the binding, not `sessions.cwd`: a fresh Node workspace lives where only the Node knows
        let bound = self
            .store
            .reader(|c| steve_store::runtime::workspace_ref_of(c, session))
            .ok()
            .flatten()
            .and_then(|w| serde_json::from_str::<steve_node_api::WorkspaceRef>(&w).ok());
        let dir = bound
            .as_ref()
            .and_then(|w| self.local_node.get()?.local_dir(w));
        engine.stash(decision, yolo_input(bound.as_ref(), dir.as_ref(), request));
        // the Node waits for the answer from the moment it asked
        engine.arm(decision);
    }
    fn permission_asked(
        &self,
        session: &str,
        decision: &str,
        request: &steve_node_api::PermissionRequest,
    ) {
        let _ = (session, request);
        // its tool input was armed before the commit (permission_arming)
        let Some(engine) = self.yolo.get().cloned() else {
            return;
        };
        let id = decision.to_string();
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(move || engine.kick(&id));
            }
            Err(_) => {
                std::thread::spawn(move || engine.kick(&id));
            }
        }
    }
    fn turn_ended(&self, session: &str, stop_reason: &str, error: Option<String>) {
        let Some(core) = self.core.get() else { return };
        let t = steve_core::TurnEnd {
            session_id: session.into(),
            stop_reason: stop_reason.into(),
            error,
        };
        if let Err(e) = block_here(|| core.on_turn_end(t)) {
            tracing::error!("turn end: {e}");
        }
    }
}

/// Each session runs on one backend, fixed when it is bound: the ones bound to a Node go to that
/// Node's coordinator (`runtime_sessions.node_id`: the local Node, or a paired remote Node through
/// the fleet), every other one to the ACP runner. Holds and counts cover them all.
pub struct RoutedRuntime {
    pub legacy: AcpRuntime,
    /// The local Node's coordinator (`STEVE_NODE_MODE=local`).
    pub node: Option<Arc<super::node_runtime::NodeRuntime>>,
    /// The remote Nodes' coordinators (`STEVE_NODE_LISTEN=1`).
    pub fleet: Option<Arc<crate::nodes::NodeFleet>>,
    /// The remote Node new sessions are bound to when they can be (`STEVE_NODE_BIND`); unset: none.
    pub bind_remote: Option<steve_node_api::NodeId>,
    /// Where bindings are read.
    pub store: steve_store::Store,
}

/// Whether boot recovery leaves `session` to a Node that is not enabled here: only when its
/// binding was read; on a read error it keeps the legacy recovery (intent kept, notice written).
fn parked_for_recovery(store: &steve_store::Store, session: &str) -> bool {
    super::node_runtime::bound_strict(store, session).unwrap_or_else(|e| {
        tracing::warn!(
            session,
            "binding unreadable at recovery: {e}; recovered as legacy"
        );
        false
    })
}

/// Which backend a session goes to.
enum Backend {
    Legacy,
    Node(Arc<super::node_runtime::NodeRuntime>),
    /// Bound to a Node that is not enabled here (STEVE_NODE_MODE or STEVE_NODE_LISTEN off): its
    /// messages wait in the queue and nothing else happens; it is never handed to the legacy runner.
    Parked,
}

/// The Node `session` is bound to. A read error counts as bound to an unknown Node: never hand
/// it to another backend.
fn node_of(store: &steve_store::Store, session: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    store
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(
                c.query_row(
                    "SELECT node_id FROM runtime_sessions WHERE session_id=?",
                    [session],
                    |r| r.get::<_, String>(0),
                )
                .optional()?,
            )
        })
        .unwrap_or_else(|e| {
            tracing::error!("binding of {session}: {e}");
            Some(String::new())
        })
}

impl RoutedRuntime {
    fn route(&self, session_id: &str) -> Backend {
        let Some(node) = node_of(&self.store, session_id) else {
            return Backend::Legacy;
        };
        if let Some(n) = &self.node
            && n.node_id().as_str() == node
        {
            return Backend::Node(n.clone());
        }
        if node == "local" {
            return Backend::Parked;
        }
        match (&self.fleet, steve_node_api::NodeId::new(node)) {
            (Some(f), Ok(id)) => f.start(&id).map_or(Backend::Parked, Backend::Node),
            _ => Backend::Parked,
        }
    }

    fn node_for(&self, session_id: &str) -> Option<Arc<super::node_runtime::NodeRuntime>> {
        match self.route(session_id) {
            Backend::Node(n) => Some(n),
            _ => None,
        }
    }

    fn parked(&self, session_id: &str) -> bool {
        matches!(self.route(session_id), Backend::Parked)
    }

    /// Binds a new session to the configured remote Node when it can run there. False: it was not
    /// (not configured, not possible for this session, or the bind failed), nothing changed.
    fn bind_remote(&self, session_id: &str) -> bool {
        let (Some(f), Some(target)) = (&self.fleet, &self.bind_remote) else {
            return false;
        };
        let Some(b) = super::node_binding::remote_binding_for(&self.store, session_id, target)
        else {
            tracing::info!("{session_id}: not bound to the remote Node {target}");
            return false;
        };
        let Some(rt) = f.start(target) else {
            return false;
        };
        match rt.bind(&b) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("bind {session_id} to the remote Node {target}: {e}");
                false
            }
        }
    }
}

impl steve_core::AgentRuntime for RoutedRuntime {
    /// A new session is bound to the remote Node named by `STEVE_NODE_BIND` when it can run there;
    /// otherwise, with the local Node enabled here, to it when its binding can be planned
    /// (otherwise, e.g. a permission mode the Node path cannot express, it stays on the legacy
    /// runner). Existing sessions are never bound; with neither configured nothing changes.
    fn created(&self, session_id: &str) {
        if self.bind_remote(session_id) {
            return;
        }
        let Some(n) = &self.node else { return };
        match super::node_binding::binding_for(&self.store, session_id) {
            Some(b) => {
                if let Err(e) = n.bind(&b) {
                    tracing::error!(
                        "bind {session_id} to the Node: {e}; it stays on the legacy runner"
                    );
                }
            }
            None => tracing::info!("{session_id}: no Node binding can be planned; legacy runner"),
        }
    }
    fn prompt(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: &[steve_core::repo::FileRef],
    ) -> steve_core::Result<steve_core::Prompted> {
        let n = match self.route(session_id) {
            Backend::Node(n) => Some(n),
            Backend::Parked => None,
            Backend::Legacy => return self.legacy.prompt(session_id, text, via, files),
        };
        let files: Vec<steve_store::FileRef> = files
            .iter()
            .map(|f| steve_store::FileRef {
                id: f.id.clone(),
                name: f.name.clone(),
                mime: f.mime.clone(),
                size: f.size,
            })
            .collect();
        let Some(n) = n else {
            return super::node_runtime::park(&self.store, session_id, text, via, &files)
                .map(|()| steve_core::Prompted::Queued)
                .map_err(|e| CoreError::Msg(e.to_string()));
        };
        match n.prompt(session_id, text, via, &files) {
            Ok(super::node_runtime::Planned::Created) => Ok(steve_core::Prompted::Sent),
            Ok(_) => Ok(steve_core::Prompted::Queued),
            Err(e) => Err(CoreError::Msg(e.to_string())),
        }
    }
    fn running(&self, session_id: &str) -> Option<steve_core::Running> {
        match self.route(session_id) {
            Backend::Node(n) => match n.active_attempt(session_id) {
                Ok(a) => a.map(|attempt| steve_core::Running {
                    session: session_id.to_string(),
                    attempt,
                }),
                Err(e) => {
                    tracing::error!("running turn of {session_id}: {e}");
                    None
                }
            },
            Backend::Parked => None,
            Backend::Legacy => self.legacy.running(session_id),
        }
    }
    /// A Node session's stop goes to its coordinator, only for the attempt it names; never to the
    /// legacy runner.
    fn stop(&self, turn: &steve_core::Running) -> steve_core::Result<steve_core::Stop> {
        match self.route(&turn.session) {
            Backend::Node(n) => Ok(if n.cancel_attempt(&turn.session, &turn.attempt)? {
                steve_core::Stop::Requested
            } else {
                steve_core::Stop::NotRunning
            }),
            Backend::Parked => Ok(steve_core::Stop::NotRunning),
            Backend::Legacy => self.legacy.stop(turn),
        }
    }
    fn is_busy(&self, session_id: &str) -> bool {
        match self.route(session_id) {
            Backend::Node(n) => n.is_busy(session_id),
            Backend::Parked => true,
            Backend::Legacy => self.legacy.is_busy(session_id),
        }
    }
    fn release(&self, session_id: &str) {
        match self.route(session_id) {
            // idle only; a busy one that the business side ended is let go when its turn is over
            Backend::Node(n) => match n.release(session_id) {
                Ok(true) => {}
                Ok(false) => tracing::debug!("release {session_id}: kept, busy or not open"),
                Err(e) => tracing::error!("release {session_id}: {e}"),
            },
            Backend::Parked => {}
            Backend::Legacy => self.legacy.release(session_id),
        }
    }
    fn answer_permission(&self, decision_id: &str, option_id: Option<&str>) -> bool {
        if let Some(sid) = super::node_runtime::decision_session(&self.store, decision_id) {
            match self.route(&sid) {
                Backend::Node(n) => {
                    return n
                        .answer_permission(decision_id, option_id)
                        .unwrap_or_else(|e| {
                            tracing::error!("answer {decision_id}: {e}");
                            false
                        });
                }
                Backend::Parked => return false,
                Backend::Legacy => {}
            }
        }
        self.legacy.answer_permission(decision_id, option_id)
    }
    fn hold(&self, on: bool) {
        self.legacy.hold(on);
        if let Some(n) = &self.node {
            n.hold(on);
        }
        if let Some(f) = &self.fleet {
            f.hold(on);
        }
    }
    fn is_holding(&self) -> bool {
        self.legacy.is_holding()
            || self.node.as_ref().is_some_and(|n| n.is_holding())
            || self.fleet.as_ref().is_some_and(|f| f.is_holding())
    }
    fn uncertain(&self, session_id: &str) -> bool {
        self.node_for(session_id)
            .is_some_and(|n| n.is_uncertain(session_id))
    }
    /// Not `route()`: its park-on-doubt suits messages (undone by a later call), but here a yes
    /// drops the session's resume intent for good, so an unreadable binding counts as legacy.
    fn recovers_itself(&self, session_id: &str) -> bool {
        match self.node.as_deref() {
            Some(n) => n.owns(session_id),
            None => parked_for_recovery(&self.store, session_id),
        }
    }
    fn running_turns(&self) -> usize {
        self.legacy.running_turns()
            + self.node.as_ref().map_or(0, |n| n.running_turns())
            + self.fleet.as_ref().map_or(0, |f| f.running_turns())
    }
    fn node_working(&self) -> usize {
        self.node.as_ref().map_or(0, |n| n.working())
    }
    fn flush(&self, session_id: &str, lead: Option<(&str, &str)>) -> bool {
        match self.node_for(session_id) {
            Some(n) => n
                .flush(session_id, lead)
                .inspect_err(|e| tracing::error!("flush {session_id}: {e}"))
                .is_ok_and(|p| p == super::node_runtime::Planned::Created),
            None if self.parked(session_id) => false,
            None => self.legacy.flush(session_id, lead),
        }
    }
    fn set_mode(&self, session_id: &str, mode: &str) -> bool {
        match self.node_for(session_id) {
            // through the Node's configure, at the session's next idle boundary
            Some(n) => match super::node_binding::permission_mode(mode) {
                Some(p) => n
                    .configure(
                        session_id,
                        &super::node_runtime::ConfigChange {
                            permissions: Some(p),
                            ..Default::default()
                        },
                    )
                    .inspect_err(|e| tracing::error!("set_mode {session_id}: {e}"))
                    .is_ok(),
                None => {
                    tracing::warn!(session = %session_id, mode, "a mode the Node path cannot express: unchanged");
                    false
                }
            },
            None if self.parked(session_id) => false,
            None => self.legacy.set_mode(session_id, mode),
        }
    }
    fn agent_pid(&self, session_id: &str) -> Option<u32> {
        match self.node_for(session_id) {
            // a Node's pids are in its own namespace: they reach YOLO as the Node reported them
            // (`PermissionRequest.own_pids`), never through a `ps` here
            Some(_) => None,
            None if self.parked(session_id) => None,
            None => self.legacy.agent_pid(session_id),
        }
    }
}

// ── the HTTP layer's Domain ──

pub struct CoreDomain {
    pub core: Core,
    pub acp: Acp,
    /// The opt-in local Node (`STEVE_NODE_MODE=local`): stopped with the Hub, its harnesses with it.
    pub node: Option<Arc<steve_node::LocalNode>>,
}

/// How long the Hub's shutdown waits for the local Node to stop its harnesses.
const NODE_SHUTDOWN: Duration = Duration::from_secs(5);

fn refs(files: Vec<Value>) -> BResult<Vec<steve_core::repo::FileRef>> {
    files.into_iter().map(from_json).collect()
}

#[async_trait]
impl Domain for CoreDomain {
    async fn seed(&self) -> BResult<()> {
        let c = self.core.clone();
        blocking(move || c.seed()).await
    }
    async fn recover(&self) -> BResult<()> {
        let c = self.core.clone();
        blocking(move || {
            // before anything resumes: turns left open were cut by the restart
            super::acp_stats::interrupt_open(c.store().store(), steve_store::now());
            c.recover()?;
            /* research runs cut off by the restart, and the one-time thread_items backfill */
            c.thread_recover()
        })
        .await
    }
    async fn upgrade_drain(&self, action: &str) -> BResult<Obj> {
        static CONTROL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _lock = CONTROL.lock().await;
        let store = self.core.store().store();
        if action == "start" {
            store
                .maintenance_set(true)
                .map_err(|e| BackendError::msg(e.to_string()))?;
            self.core
                .maintenance_hold(true)
                .map_err(|e| BackendError::msg(e.to_string()))?;
        } else if action == "cancel" {
            // If adapters were already quiesced, their groups must really be gone before reopen.
            if !self.acp.reopen() && (!self.acp.quiesce().await || !self.acp.reopen()) {
                return Err(BackendError::msg(steve_i18n::t!(
                    "hub.maintenance.agents_alive"
                )));
            }
            store
                .maintenance_set(false)
                .map_err(|e| BackendError::msg(e.to_string()))?;
            self.core
                .maintenance_hold(false)
                .map_err(|e| BackendError::msg(e.to_string()))?;
            // turns the drain cut while they waited for a permission: no restart came to resume them
            for sid in self.acp.take_cut() {
                if !self.acp.continue_cut(&sid) {
                    tracing::warn!(session = %sid, "a turn the drain cut could not be continued");
                }
            }
            self.ready().await?;
        }
        let active = store.maintenance();
        let running = store.activities();
        // a turn that only waits for your permission answer doesn't hold a drain: it is cut, the
        // restart resumes it (it asks again)
        let acp = self.acp.working_turns();
        // a Node turn has no cut-and-resume: it holds the drain, also while it waits for your
        // permission answer (until it is answered or expires), and while its outcome is unknown
        let node = self.core.node_working();
        let source_release_busy = self.core.source_release_busy();
        let quiet = active
            && running.values().sum::<usize>() == 0
            && acp == 0
            && node == 0
            && !source_release_busy;
        let adapters_stopped = if quiet {
            self.acp.quiesce().await
        } else {
            false
        };
        let unknown: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM mail_drafts WHERE status='unknown'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .map_err(|e| BackendError::msg(e.to_string()))?;
        let sealed = quiet
            && adapters_stopped
            && self.acp.working_turns() == 0
            && self.core.node_working() == 0
            && store.maintenance_seal();
        Ok(json!({"maintenance":active,"ready":sealed,"running":running,"acp":acp,"node":node,"adapters_stopped":adapters_stopped,"unknown_mail":unknown,"source_release_busy":source_release_busy}).as_object().unwrap().clone())
    }
    /// Never fails the Hub: what could not be resumed is logged and reported on `/healthz`
    /// (`recovery`), and its durable intent stays for the next boot.
    async fn ready(&self) -> BResult<()> {
        let c = self.core.clone();
        let r = blocking(move || {
            c.ready();
            c.thread_ready()
        })
        .await;
        crate::recovery::record(&r);
        Ok(())
    }
    async fn report_release(&self) -> BResult<()> {
        let c = self.core.clone();
        blocking(move || c.report_release()).await
    }
    async fn shutdown(&self) {
        let node = async {
            match &self.node {
                Some(n) => tokio::time::timeout(NODE_SHUTDOWN, n.shutdown())
                    .await
                    .is_ok(),
                None => true,
            }
        };
        let (research_stopped, (), node_stopped) = tokio::join!(
            self.core.shutdown_research(Duration::from_secs(5)),
            self.acp.shutdown(Duration::from_millis(500)),
            node,
        );
        if !research_stopped {
            tracing::error!("research process groups did not confirm shutdown before deadline");
        }
        if !node_stopped {
            tracing::error!("the local Node did not stop its harnesses before the deadline");
        }
    }
    fn spawn_background(&self) -> bool {
        tokio::spawn(self.core.clone().watch_releases());
        tokio::spawn(self.core.clone().run_housekeeping());
        tokio::spawn(self.core.clone().run_workspace_watch());
        tokio::spawn(self.core.clone().run_stall_watch());
        // ST-34: items made before the categories, or left without one, get theirs
        tokio::spawn(self.core.clone().backfill_kinds_all());
        true
    }

    async fn create_item(&self, p: CreateItem) -> BResult<Obj> {
        let c = self.core.clone();
        let [a, b] = p.source;
        let n = steve_core::CreateItem {
            ws: p.ws,
            goal: p.goal,
            title: p.title,
            bot: p.bot,
            project: p.project,
            source: Some((a, b)),
            parent: None,
            tag_ids: p.tag_ids,
            propose: p.mode == "propose",
            opening: p.opening,
            files: vec![],
            actor: Some(p.actor),
        };
        to_obj(blocking(move || c.create_item(n)).await?)
    }
    async fn kinds_backfill(&self, ws: &str, dry: bool) -> BResult<Value> {
        let r = self.core.backfill_kinds(ws, dry).await.map_err(cerr)?;
        serde_json::to_value(r).map_err(|e| BackendError::msg(e.to_string()))
    }
    async fn disk_report(&self) -> BResult<Value> {
        let c = self.core.clone();
        let r = blocking(move || c.disk_report()).await?;
        serde_json::to_value(r).map_err(|e| BackendError::msg(e.to_string()))
    }
    async fn reclaim_workspaces(&self) -> BResult<Value> {
        let c = self.core.clone();
        let r = blocking(move || c.reclaim_workspaces()).await?;
        serde_json::to_value(r).map_err(|e| BackendError::msg(e.to_string()))
    }
    async fn kinds_review(&self, ws: &str) -> BResult<Value> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        let r = blocking(move || c.kinds_review(&ws)).await?;
        serde_json::to_value(r).map_err(|e| BackendError::msg(e.to_string()))
    }
    async fn kinds_reviewed(&self, ws: &str, ids: Vec<String>) -> BResult<Value> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        let r = blocking(move || c.kinds_reviewed(&ws, &ids)).await?;
        serde_json::to_value(r).map_err(|e| BackendError::msg(e.to_string()))
    }
    async fn route(&self, ws: &str, text: &str) -> BResult<Route> {
        let r = self.core.route(ws, text, &[], false).await.map_err(cerr)?;
        Ok(Route {
            kind: r.kind,
            title: r.title,
            bot: r.bot,
            project: r.project,
            tag_ids: r.tag_ids,
            new_tags: r
                .new_tags
                .into_iter()
                .map(|t| NewTag {
                    name: t.name,
                    cat: t.cat,
                    description: t.description,
                })
                .collect(),
            reply: r.reply,
        })
    }
    async fn item_action(&self, id: &str, action: &str, actor: &str) -> BResult<Obj> {
        let a = ItemAction::parse(action)
            .ok_or_else(|| BackendError::msg(format!("unknown action {action}")))?;
        let (c, id, actor) = (self.core.clone(), id.to_string(), actor.to_string());
        to_obj(blocking(move || c.item_action(&id, a, &actor)).await?)
    }
    async fn lifecycle_command(
        &self,
        id: &str,
        command: &str,
        expected_cycle: i64,
        expected_revision: i64,
        reason: Option<String>,
        goal: Option<String>,
        actor: &str,
    ) -> BResult<Obj> {
        let (c, id, command, actor) = (
            self.core.clone(),
            id.to_string(),
            command.to_string(),
            actor.to_string(),
        );
        to_obj(
            blocking(move || {
                c.lifecycle_command(
                    &id,
                    &command,
                    expected_cycle,
                    expected_revision,
                    reason.as_deref(),
                    goal.as_deref(),
                    &actor,
                )
            })
            .await?,
        )
    }
    async fn lifecycle_view(&self, id: &str) -> BResult<Option<Value>> {
        let (c, id) = (self.core.clone(), id.to_string());
        blocking(move || c.lifecycle_view(&id).map(Some)).await
    }
    async fn lifecycle_views(&self, ws: &str) -> BResult<std::collections::HashMap<String, Value>> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        blocking(move || c.lifecycle_views_ws(&ws)).await
    }
    async fn lifecycle_tick(&self) -> BResult<usize> {
        let c = self.core.clone();
        blocking(move || c.lifecycle_tick()).await
    }
    async fn revise_goal(&self, id: &str, text: &str) -> BResult<()> {
        let (c, id, text) = (self.core.clone(), id.to_string(), text.to_string());
        blocking(move || c.revise_goal(&id, &text)).await
    }
    async fn supplement(&self, id: &str, text: &str, mode: &str, files: Vec<Value>) -> BResult<()> {
        let m = match mode {
            "steer" => SupplementMode::Steer,
            "new" => SupplementMode::New,
            _ => SupplementMode::Auto,
        };
        let files = refs(files)?;
        let (c, id, text) = (self.core.clone(), id.to_string(), text.to_string());
        blocking(move || c.supplement(&id, &text, m, &files)).await
    }
    async fn deliver_queued(&self, id: &str) -> BResult<()> {
        let (c, id) = (self.core.clone(), id.to_string());
        blocking(move || c.deliver_queued(&id)).await
    }
    async fn reassign(&self, id: &str, bot: &str, actor: &str) -> BResult<Obj> {
        let (c, id, bot, actor) = (
            self.core.clone(),
            id.to_string(),
            bot.to_string(),
            actor.to_string(),
        );
        to_obj(blocking(move || c.reassign(&id, &bot, &actor)).await?)
    }
    async fn add_dep(&self, id: &str, dep: &str) -> BResult<()> {
        let (c, id, dep) = (self.core.clone(), id.to_string(), dep.to_string());
        blocking(move || c.add_dep(&id, &dep)).await
    }
    async fn rm_dep(&self, id: &str, dep: &str) -> BResult<()> {
        let (c, id, dep) = (self.core.clone(), id.to_string(), dep.to_string());
        blocking(move || c.rm_dep(&id, &dep)).await
    }

    async fn session_message(
        &self,
        session: &str,
        text: &str,
        mode: &str,
        files: Vec<Value>,
    ) -> BResult<String> {
        let files = refs(files)?;
        let interrupt = mode == "插话";
        let (c, sid, text) = (self.core.clone(), session.to_string(), text.to_string());
        Ok(
            blocking(move || c.session_message(&sid, &text, interrupt, &files))
                .await?
                .as_str()
                .to_string(),
        )
    }
    async fn cancel(&self, session: &str) -> BResult<bool> {
        // routed: a Node session's stop goes to its coordinator, never to the legacy runner
        let (c, sid) = (self.core.clone(), session.to_string());
        tokio::task::spawn_blocking(move || c.cancel_turn(&sid))
            .await
            .map_err(|e| BackendError::msg(format!("task failed: {e}")))?
            .map_err(|e| BackendError::msg(e.to_string()))
    }
    async fn hold_status(&self) -> BResult<Value> {
        let c = self.core.clone();
        let h = tokio::task::spawn_blocking(move || c.hold_status())
            .await
            .map_err(|e| BackendError::msg(format!("task failed: {e}")))?;
        serde_json::to_value(h).map_err(|e| BackendError::msg(e.to_string()))
    }
    async fn queued_edit(&self, session: &str, id: &str, text: &str) -> BResult<Obj> {
        let (c, sid, id, text) = (
            self.core.clone(),
            session.to_string(),
            id.to_string(),
            text.to_string(),
        );
        to_obj(blocking(move || c.queued_edit(&sid, &id, &text)).await?)
    }
    async fn queued_remove(&self, session: &str, id: &str) -> BResult<Obj> {
        let (c, sid, id) = (self.core.clone(), session.to_string(), id.to_string());
        to_obj(blocking(move || c.queued_remove(&sid, &id)).await?)
    }
    async fn queued_send_now(&self, session: &str, id: &str) -> BResult<Obj> {
        let (c, sid, id) = (self.core.clone(), session.to_string(), id.to_string());
        to_obj(
            match blocking(move || c.queued_send_now(&sid, &id)).await? {
                SendNow::Interrupted => json!({ "result": "interrupted" }),
                SendNow::Sent => json!({ "result": "sent" }),
                SendNow::Waiting(reason) => json!({ "result": "waiting", "reason": reason }),
            },
        )
    }
    fn is_busy(&self, session: &str) -> bool {
        self.core.is_busy(session)
    }
    async fn end_session(&self, session: &str) -> BResult<()> {
        let (c, sid) = (self.core.clone(), session.to_string());
        blocking(move || c.end_session(&sid)).await
    }
    async fn fork(
        &self,
        ws: &str,
        session: &str,
        seq: f64,
        goal: &str,
        as_session: bool,
        actor: &str,
    ) -> BResult<Obj> {
        // `seq <= x` on integer seqs
        let seq = if seq.is_finite() {
            seq.floor() as i64
        } else if seq > 0.0 {
            i64::MAX
        } else {
            i64::MIN
        };
        let (c, ws, sid, goal, actor) = (
            self.core.clone(),
            ws.to_string(),
            session.to_string(),
            goal.to_string(),
            actor.to_string(),
        );
        to_obj(blocking(move || c.fork(&ws, &sid, seq, &goal, as_session, &actor)).await?)
    }

    async fn resolve(&self, id: &str, choice: &str, text: &str, actor: &str) -> BResult<Obj> {
        let (c, id, choice, text, actor) = (
            self.core.clone(),
            id.to_string(),
            choice.to_string(),
            text.to_string(),
            actor.to_string(),
        );
        to_obj(blocking(move || c.resolve(&id, &choice, &text, &actor)).await?)
    }
    async fn undo_decision(&self, id: &str, actor: &str) -> BResult<Obj> {
        let (c, id, actor) = (self.core.clone(), id.to_string(), actor.to_string());
        to_obj(blocking(move || c.undo_decision(&id, &actor)).await?)
    }
    async fn dismiss_decision(&self, id: &str, actor: &str) -> BResult<Obj> {
        let (c, id, actor) = (self.core.clone(), id.to_string(), actor.to_string());
        to_obj(blocking(move || c.dismiss_decision(&id, &actor)).await?)
    }
    async fn drain(&self, token: &str, start: bool) -> BResult<Option<Obj>> {
        let (c, token) = (self.core.clone(), token.to_string());
        match blocking(move || c.drain(&token, start)).await? {
            Some(d) => Ok(Some(to_obj(
                json!({ "idle": d.idle, "running": d.running }),
            )?)),
            None => Ok(None),
        }
    }
    async fn tidy_branch(&self, item: &str) -> BResult<()> {
        let (c, item) = (self.core.clone(), item.to_string());
        blocking(move || c.tidy_branch(&item)).await
    }

    fn bot_prompt(&self, bot: &Obj, ws: &str) -> BResult<String> {
        let id = bot.get("id").and_then(Value::as_str).unwrap_or_default();
        let mut b = self
            .core
            .store()
            .store()
            .bots()
            .get(id)
            .map_err(|e| BackendError::msg(e.to_string()))?
            .ok_or_else(|| BackendError::not_found(crate::api::Noun::Bot.missing()))?;
        b.prompt = bot
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.core.bot_prompt(&b, ws).map_err(cerr)
    }
    async fn delete_bot(&self, id: &str, to: &str, actor: &str) -> BResult<Obj> {
        let (c, id, to, actor) = (
            self.core.clone(),
            id.to_string(),
            to.to_string(),
            actor.to_string(),
        );
        let moved = blocking(move || c.delete_bot(&id, &to, &actor)).await?;
        to_obj(json!({ "moved": moved }))
    }

    async fn say(
        &self,
        ws: &str,
        user: &str,
        text: &str,
        files: Vec<Value>,
        mentions: Vec<steve_modules::contacts::item_people::Mention>,
    ) -> BResult<()> {
        let files = refs(files)?;
        let (c, ws, user, text) = (
            self.core.clone(),
            ws.to_string(),
            user.to_string(),
            text.to_string(),
        );
        blocking(move || {
            c.say_as(&ws, Some(&user), &text, &files, &mentions)
                .map(|_| ())
        })
        .await
    }
    async fn say_to(
        &self,
        ws: &str,
        user: &str,
        text: &str,
        files: Vec<Value>,
        mentions: Vec<steve_modules::contacts::item_people::Mention>,
        item: Option<String>,
        reply_to: Option<String>,
    ) -> BResult<()> {
        let files = refs(files)?;
        let (c, ws, user, text) = (
            self.core.clone(),
            ws.to_string(),
            user.to_string(),
            text.to_string(),
        );
        let to = steve_core::thread::SayTo {
            item: item.map(|i| i.trim().trim_start_matches('#').to_uppercase()),
            reply_to,
        };
        blocking(move || {
            c.say_to(&ws, Some(&user), &text, &files, &mentions, to)
                .map(|_| ())
        })
        .await
    }
    async fn say_guarded(&self, ws: &str, stored: &str) -> BResult<()> {
        let (c, ws, stored) = (self.core.clone(), ws.to_string(), stored.to_string());
        blocking(move || {
            c.say_guarded(
                &ws,
                &stored,
                steve_vault::guard::REPLY,
                steve_vault::guard::REPLY_HREF,
            )
            .map(|_| ())
        })
        .await
    }
    async fn stop(&self, ws: &str) -> BResult<bool> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        blocking(move || c.stop(&ws)).await
    }
    async fn thread_busy(&self, ws: &str) -> BResult<bool> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        blocking(move || c.main_busy(&ws)).await
    }
    async fn thread_queued_edit(&self, ws: &str, message: &str, text: &str) -> BResult<bool> {
        let (c, ws, m, t) = (
            self.core.clone(),
            ws.to_string(),
            message.to_string(),
            text.to_string(),
        );
        blocking(move || c.thread_queued_edit(&ws, &m, &t)).await
    }
    async fn thread_queued_withdraw(&self, ws: &str, message: &str) -> BResult<Option<Obj>> {
        let (c, ws, m) = (self.core.clone(), ws.to_string(), message.to_string());
        match blocking(move || c.thread_queued_withdraw(&ws, &m)).await? {
            Some((text, files)) => Ok(Some(to_obj(json!({"text": text, "files": files}))?)),
            None => Ok(None),
        }
    }
    async fn promote(&self, ws: &str, message: &str) -> BResult<Obj> {
        to_obj(self.core.promote(ws, message).await.map_err(cerr)?)
    }
    async fn reroute(&self, ws: &str, message: &str, to: &str) -> BResult<(Obj, bool)> {
        let to = steve_core::thread::RerouteTo::parse(to).ok_or_else(|| BackendError {
            status: 400,
            message: steve_i18n::t!("hub.error.invalid", fields = "to"),
        })?;
        handled(self.core.reroute(ws, message, to).await.map_err(cerr)?)
    }
    async fn offer(&self, ws: &str, message: &str, take: bool) -> BResult<(Obj, bool)> {
        let (c, ws, m) = (self.core.clone(), ws.to_string(), message.to_string());
        handled(blocking(move || c.offer(&ws, &m, take)).await?)
    }
    async fn suggest(&self, ws: &str, message: &str, choice: &str) -> BResult<(Obj, bool)> {
        let (c, ws, m, ch) = (
            self.core.clone(),
            ws.to_string(),
            message.to_string(),
            choice.to_string(),
        );
        handled(blocking(move || c.suggest(&ws, &m, &ch)).await?)
    }
    async fn a2ui_action(
        &self,
        ws: &str,
        user: &str,
        message: &str,
        action: steve_core::thread::A2uiAction,
    ) -> BResult<Obj> {
        let (c, ws, user, m) = (
            self.core.clone(),
            ws.to_string(),
            user.to_string(),
            message.to_string(),
        );
        let r = blocking(move || c.a2ui_action(&ws, Some(&user), &m, action)).await?;
        to_obj(json!(r))
    }
    async fn stop_run(&self, ws: &str, run: &str) -> BResult<bool> {
        let (c, ws, r) = (self.core.clone(), ws.to_string(), run.to_string());
        blocking(move || c.stop_run(&ws, &r)).await
    }
    async fn thread_follow(&self, ws: &str, item: &str, follow: bool) -> BResult<bool> {
        let (c, ws, i) = (self.core.clone(), ws.to_string(), item.to_string());
        blocking(move || {
            if c.store().item(&i)?.is_none_or(|x| x.workspace_id != ws) {
                return Ok(false);
            }
            Ok(c.store().store().thread().item_follow(&i, follow)?)
        })
        .await
    }

    async fn claude_print(
        &self,
        purpose: &'static str,
        ws: &str,
        prompt: &str,
        tier: Tier,
        timeout_ms: u64,
    ) -> BResult<String> {
        let t = match tier {
            Tier::Fast => steve_core::Tier::Fast,
            Tier::Smart => steve_core::Tier::Smart,
        };
        let p = steve_core::stats::tagged(
            purpose,
            ws,
            self.core.gateway().call(
                prompt,
                t,
                Duration::from_millis(timeout_ms),
                &steve_core::gateway::ThinkTools::None,
            ),
        )
        .await
        .map_err(cerr)?;
        Ok(p.text)
    }

    async fn models_view(&self, ws: &str) -> BResult<Value> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        blocking(move || Ok(c.models().view(&ws))).await
    }
    async fn models_used(&self, ws: &str, harness: &str) -> BResult<Value> {
        let (c, ws, h) = (self.core.clone(), ws.to_string(), harness.to_string());
        blocking(move || {
            let since = steve_store::now() - 7 * 86_400_000;
            Ok(json!(c.models().used(&ws, &h, since)))
        })
        .await
    }
    async fn models_write(&self, ws: &str, values: &Obj, actor: &str) -> BResult<()> {
        let (c, ws, values, actor) = (
            self.core.clone(),
            ws.to_string(),
            values.clone(),
            actor.to_string(),
        );
        blocking(move || Ok(c.models().write(&ws, &values, &actor, "settings.models")?)).await
    }
    async fn config_draft(&self, ws: &str, target: &Obj, text: &str) -> BResult<Obj> {
        let t = steve_core::changes::Target {
            kind: target
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            id: target.get("id").and_then(Value::as_str).map(str::to_string),
        };
        to_obj(self.core.draft(ws, t, text).await.map_err(cerr)?)
    }
    async fn config_apply(&self, id: &str, actor: &str, ws: &str) -> BResult<Obj> {
        let (c, id, actor, ws) = (
            self.core.clone(),
            id.to_string(),
            actor.to_string(),
            ws.to_string(),
        );
        to_obj(blocking(move || c.apply(&id, &actor, &ws)).await?)
    }
    async fn config_undo(&self, id: &str, actor: &str, ws: &str) -> BResult<Obj> {
        let (c, id, actor, ws) = (
            self.core.clone(),
            id.to_string(),
            actor.to_string(),
            ws.to_string(),
        );
        to_obj(blocking(move || c.undo(&id, &actor, &ws)).await?)
    }
    async fn config_drop(&self, id: &str, ws: &str) -> BResult<Obj> {
        let (c, id, ws) = (self.core.clone(), id.to_string(), ws.to_string());
        to_obj(blocking(move || c.drop_draft(&id, &ws)).await?)
    }
    async fn config_mark(&self, ws: &str, id: &str, state: Option<&str>) -> BResult<()> {
        let (c, id, ws, state) = (
            self.core.clone(),
            id.to_string(),
            ws.to_string(),
            state.map(str::to_string),
        );
        blocking(move || {
            c.mark_proposal(&ws, &id, state.as_deref());
            Ok(())
        })
        .await
    }
}

// ── steve-mcp backend ──

pub struct CoreMcp {
    pub store: Store,
    pub core: Core,
}

type ToolResult = Result<(), steve_mcp::ToolError>;

async fn tool(f: impl FnOnce() -> Result<(), CoreError> + Send + 'static) -> ToolResult {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(|e| e.to_string().into()),
        Err(e) => Err(e.to_string().into()),
    }
}

impl steve_mcp::Backend for CoreMcp {
    type Session = steve_store::Session;

    async fn session_by_token(&self, token: &str) -> Option<steve_store::Session> {
        let (store, token) = (self.store.clone(), token.to_string());
        tokio::task::spawn_blocking(move || store.sessions().by_token(&token).ok().flatten())
            .await
            .ok()
            .flatten()
    }
    async fn update(
        &self,
        s: &steve_store::Session,
        now: String,
        plan: Vec<steve_mcp::PlanStep>,
    ) -> ToolResult {
        let (c, s) = (self.core.clone(), s.clone());
        let plan: Vec<steve_core::runtime::PlanStep> = plan
            .into_iter()
            .map(|p| steve_core::runtime::PlanStep {
                step: p.step,
                done: p.done,
                left: false,
            })
            .collect();
        tool(move || c.tool_update(&s, &now, &plan)).await
    }
    async fn ask(
        &self,
        s: &steve_store::Session,
        question: String,
        options: Vec<String>,
        hint: steve_mcp::AskHint,
    ) -> ToolResult {
        let (c, s) = (self.core.clone(), s.clone());
        tool(move || {
            c.tool_ask_with(
                &s,
                &question,
                &options,
                hint.recommended.as_deref(),
                hint.why.as_deref(),
            )
        })
        .await
    }
    async fn request_approval(
        &self,
        s: &steve_store::Session,
        action: String,
        detail: String,
    ) -> ToolResult {
        let (c, s) = (self.core.clone(), s.clone());
        tool(move || c.tool_request_approval(&s, &action, &detail)).await
    }
    async fn deliver(
        &self,
        s: &steve_store::Session,
        summary: String,
        outputs: Vec<steve_mcp::Output>,
        checks: Vec<steve_mcp::Check>,
        unfinished: Option<Vec<String>>,
    ) -> ToolResult {
        let (c, s) = (self.core.clone(), s.clone());
        let outputs: Vec<steve_core::Output> = outputs
            .into_iter()
            .map(|o| steve_core::Output {
                kind: o.kind,
                title: o.title,
                location: o.location,
                role: o.role,
            })
            .collect();
        let checks: Vec<steve_core::Check> = checks
            .into_iter()
            .map(|x| steve_core::Check {
                name: x.name,
                passed: x.passed,
            })
            .collect();
        let unfinished = unfinished.unwrap_or_default();
        tool(move || c.tool_deliver(&s, &summary, &outputs, &checks, &unfinished)).await
    }
    async fn publish(
        &self,
        s: &steve_store::Session,
        branch: String,
        summary: String,
    ) -> ToolResult {
        let (c, s) = (self.core.clone(), s.clone());
        tool(move || c.tool_publish(&s, &branch, &summary)).await
    }
}

#[cfg(test)]
mod recovery_tests {
    use steve_store::{NoPublisher, Store};

    /// A binding that cannot be read parks messages (undoable) but never makes boot recovery
    /// drop a legacy session's resume intent.
    #[test]
    fn an_unreadable_binding_is_not_parked_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), NoPublisher).unwrap();
        assert!(!super::parked_for_recovery(&store, "s1"));
        store
            .with_conn(|c| Ok(c.execute_batch("DROP TABLE runtime_sessions")?))
            .unwrap();
        assert!(super::super::node_runtime::bound_strict(&store, "s1").is_err());
        assert!(
            super::super::node_runtime::bound(&store, "s1"),
            "messages still park on doubt"
        );
        assert!(!super::parked_for_recovery(&store, "s1"));
    }
}

#[cfg(test)]
mod yolo_input_tests {
    use super::yolo_input;
    use std::path::PathBuf;
    use steve_node::LocalDir;
    use steve_node_api::testkit::build;
    use steve_node_api::*;

    fn ws(id: &str) -> WorkspaceRef {
        WorkspaceRef {
            node: NodeId::new("local").unwrap(),
            id: WorkspaceId::new(id).unwrap(),
        }
    }
    fn at(w: &WorkspaceRef, rel: &str) -> PathRef {
        PathRef {
            workspace: w.clone(),
            rel: RelPath::new(rel).unwrap(),
        }
    }
    fn req(action: Action, own_pids: Vec<u32>) -> PermissionRequest {
        let exec = ExecutionRef::Turn {
            session: SessionId::new("s1").unwrap(),
            turn: TurnId::new("t1").unwrap(),
            attempt: AttemptId::new("a1").unwrap(),
        };
        let mut r = build::permission("p1", exec, "x");
        r.action = action;
        r.own_pids = own_pids;
        r
    }
    fn write(w: &WorkspaceRef, rel: &str) -> Action {
        Action::Write {
            paths: vec![at(w, rel)],
            change_summary: "edit".into(),
            change: None,
        }
    }

    /// The Node's directory, not the session's cwd (which the Hub never trusts for a Node session).
    #[test]
    fn paths_land_under_the_nodes_directory() {
        let w = ws("w1");
        let root = PathBuf::from("/node/worktrees/w1");
        let dir = LocalDir {
            path: root.clone(),
            owned: true,
        };
        let t = yolo_input(Some(&w), Some(&dir), &req(write(&w, "src/x.rs"), vec![7]));
        assert_eq!(t.kind.as_deref(), Some("edit"));
        assert_eq!(t.raw["file_path"], "/node/worktrees/w1/src/x.rs");
        assert_eq!(t.locations, vec!["/node/worktrees/w1/src/x.rs".to_string()]);
        assert_eq!(t.own_dir, Some(root.clone()));
        assert_eq!(t.own_pids, Some(vec![7]));

        let exec = |cwd: Option<PathRef>| Action::Exec {
            command: "cargo test".into(),
            cwd,
            network: NetworkScope::Unrestricted,
        };
        let t = yolo_input(Some(&w), Some(&dir), &req(exec(None), vec![]));
        assert_eq!(t.kind.as_deref(), Some("execute"));
        assert_eq!(t.raw["cwd"], "/node/worktrees/w1");
        assert_eq!(t.own_pids, Some(vec![]), "never the Hub's `ps`");
        let t = yolo_input(
            Some(&w),
            Some(&dir),
            &req(exec(Some(at(&w, "hub-rs"))), vec![]),
        );
        assert_eq!(t.raw["cwd"], "/node/worktrees/w1/hub-rs");

        let t = yolo_input(
            Some(&w),
            Some(&dir),
            &req(
                Action::Read {
                    paths: vec![at(&w, "a")],
                },
                vec![],
            ),
        );
        assert_eq!(t.kind.as_deref(), Some("read"));
        assert_eq!(t.locations, vec!["/node/worktrees/w1/a".to_string()]);
    }

    #[test]
    fn another_workspace_or_an_unknown_directory_is_other() {
        let w = ws("w1");
        let elsewhere = ws("w2");
        let dir = LocalDir {
            path: PathBuf::from("/node/worktrees/w1"),
            owned: true,
        };
        let t = yolo_input(Some(&w), Some(&dir), &req(write(&elsewhere, "a"), vec![]));
        assert_eq!(t.kind.as_deref(), Some("other"));
        assert!(t.locations.is_empty());
        // a remote or stale Node: no directory here
        for action in [
            write(&w, "a"),
            Action::Read {
                paths: vec![at(&w, "a")],
            },
            Action::Exec {
                command: "rm -rf build".into(),
                cwd: None,
                network: NetworkScope::Unrestricted,
            },
        ] {
            let t = yolo_input(Some(&w), None, &req(action, vec![]));
            assert_eq!(t.kind.as_deref(), Some("other"), "{:?}", t.raw);
            assert_eq!(t.own_dir, None);
        }
        // not bound: the directory is not used
        let t = yolo_input(None, Some(&dir), &req(write(&w, "a"), vec![]));
        assert_eq!(t.kind.as_deref(), Some("other"));
    }

    #[test]
    fn an_adopted_directory_is_not_yolos_own() {
        let w = ws("w1");
        let dir = LocalDir {
            path: PathBuf::from("/src/proj"),
            owned: false,
        };
        let t = yolo_input(Some(&w), Some(&dir), &req(write(&w, "a"), vec![]));
        assert_eq!(t.raw["file_path"], "/src/proj/a");
        assert_eq!(t.own_dir, None);
    }

    /// What the guard makes of these: unknown is asked, never judged against a guessed base.
    #[test]
    fn the_guard_asks_when_the_directory_is_unknown() {
        use steve_core::yolo::{guard, judge};
        let w = ws("w1");
        let t = yolo_input(Some(&w), None, &req(write(&w, "a"), vec![]));
        let tool = judge::Tool {
            kind: t.kind,
            title: "编辑 a".into(),
            raw: t.raw,
            truncated: false,
            locations: t.locations,
            options: vec![("once".into(), "allow_once".into())],
            cwd: t.own_dir,
            project_dir: None,
            branch: None,
            own_pids: t.own_pids.unwrap_or_default(),
        };
        let env = judge::Env {
            steve_home: PathBuf::from("/nonexistent/steve"),
            home: PathBuf::from("/nonexistent"),
            tmp: vec![],
        };
        assert!(
            !matches!(guard::check(&tool, &[], &env), guard::Guard::Allow { .. }),
            "asked"
        );
    }
}

//! steve-core and the Nodes wired together:
//! - [`HubHost`]: what the Nodes' coordinators report to ([`NodeDomain`](super::node_runtime::NodeDomain)):
//!   turn ends, workspace facts and permission requests go to the core.
//! - [`RoutedRuntime`]: steve-core's [`AgentRuntime`](steve_core::AgentRuntime) over the Nodes. There is no
//!   in-process runner any more (B7): a session no Node takes is refused, never run on the Hub.
//! - [`CoreDomain`]: the HTTP layer's [`Domain`] over [`Core`]. The core's mutating calls are synchronous (and
//!   may run git under its lock), so they run on the blocking pool.
//! - [`CoreMcp`]: steve-mcp's backend, the Steve tools → `core.tool_*` (and `core.lane_status`).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use steve_core::{Core, CoreError, ItemAction, SendNow, SupplementMode};
use steve_store::Store;

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

// ── the Hub side of the Nodes ──

/// What the Nodes' coordinators need from the Hub: the store, and the core for turn ends and
/// workspace facts (set once the core exists, since the core needs the runtime first).
pub struct HubHost {
    pub store: Store,
    pub data_dir: PathBuf,
    pub core: OnceLock<Core>,
    /// Schedules count session costs against their budget (`on_session_cost` after every cost update).
    pub flow: OnceLock<std::sync::Arc<steve_flow::FlowCx>>,
    /// The YOLO executor: gets the full input of each tool permission request, and answers it once
    /// the Node waits for it (set once the core exists).
    pub yolo: OnceLock<steve_core::yolo::Engine>,
    /// The local Node (`STEVE_NODE_MODE=local`): YOLO judges its requests against the directory
    /// its prepare reported ([`prepared_dir`]). Unset (a remote Node, or none): YOLO asks.
    pub local_node: OnceLock<std::sync::Arc<dyn super::host_node::HostNode>>,
}

impl HubHost {
    /// The harness mode `session_id` starts in: its bot's `perm_mode`, as YOLO widens it
    /// (`default` to `acceptEdits` for an item with its own directory, never further).
    pub fn perm_mode(&self, session_id: &str) -> Option<String> {
        let s = self.store.sessions().get(session_id).ok().flatten()?;
        let perm_mode = match &s.bot_id {
            Some(b) => self.store.bots().get(b).ok().flatten().map(|b| b.perm_mode),
            None => None,
        };
        match self.core.get() {
            Some(c) => c.session_perm_mode(&s, perm_mode),
            None => perm_mode,
        }
    }
}

/// The directory on this machine of a workspace the local Node (`local`) prepared, from the
/// [`steve_node_api::WorkspaceView`] its finished prepare returned to the Hub's ledger: on the
/// Hub's own machine its display is the path, `~` for the home. Owned unless it was an existing
/// directory the Node adopted. `None` for another Node's workspace (its display is no address
/// here), one never prepared, one that is no directory now, and one the Node no longer stands
/// behind there ([`steve_node::LocalNode::holds`]: tidied or forgotten, or an adopted directory
/// whose root was taken away). A store read and the local Node's own journal: never a command.
pub(crate) fn prepared_dir(
    store: &Store,
    local: Option<&dyn super::host_node::HostNode>,
    w: &steve_node_api::WorkspaceRef,
) -> Option<steve_node::LocalDir> {
    use steve_node_api::{Command, Observed, OperationResult, Prepare, PrepareKind, Receipt};
    let local = local.filter(|n| n.id() == &w.node)?;
    let rows: Vec<(String, String)> = store
        .reader(|c| {
            let mut st = c.prepare_cached(
                "SELECT request, receipt FROM node_commands
                  WHERE node_id=? AND method='prepare_workspace' AND state='finished'
                    AND receipt IS NOT NULL
                  ORDER BY updated DESC, rowid DESC",
            )?;
            let rows = st
                .query_map([w.node.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(rows)
        })
        .ok()?;
    let (kind, view) = rows.into_iter().find_map(|(request, receipt)| {
        let receipt: Value = serde_json::from_str(&receipt).ok()?;
        let r: Receipt = serde_json::from_value(receipt.get("payload")?.clone()).ok()?;
        let Observed::Finished {
            result: Some(OperationResult::Workspace(v)),
            ..
        } = r.state
        else {
            return None;
        };
        if v.workspace != *w {
            return None;
        }
        let c: Command<Prepare> = serde_json::from_str(&request).ok()?;
        Some((c.body.kind, v))
    })?;
    let home = std::env::var("HOME").ok();
    let path = std::fs::canonicalize(display_path(&view.display, home.as_deref())?).ok()?;
    let adopted = matches!(kind, PrepareKind::Existing { .. });
    (path.is_dir() && local.holds(w, &path, adopted)).then_some(steve_node::LocalDir {
        path,
        owned: !matches!(kind, PrepareKind::Existing { .. }),
    })
}

use super::host_node::display_path;

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
    fn workspace_changed(&self, fact: super::node_runtime::WorkspaceFact) {
        use super::node_runtime::WorkspaceFact;
        let Some(core) = self.core.get().cloned() else {
            return;
        };
        // where a worktree the local Node made is on this machine (a remote Node's never is)
        let dir = match &fact {
            WorkspaceFact::Prepared { view, .. } if view.branch.is_some() => {
                match self.local_node.get().and_then(|n| n.local_dir(view)) {
                    Some(d) => Some(d.path),
                    None => return,
                }
            }
            WorkspaceFact::Prepared { .. } => return,
            _ => None,
        };
        let run = move || {
            let r = match fact {
                WorkspaceFact::Prepared { session, view } => {
                    let path = dir.unwrap_or_default();
                    core.workspace_ready(&session, &path.to_string_lossy(), view.branch.as_deref())
                }
                WorkspaceFact::PrepareFailed { session, error } => core.workspace_failed(
                    &session,
                    error
                        .as_ref()
                        .is_some_and(|e| e.code == steve_node_api::ErrorCode::Exhausted),
                    error.as_ref().map_or("", |e| e.safe_detail.as_str()),
                ),
                WorkspaceFact::Tidied { item, removed, .. } => {
                    core.workspace_tidied(&item, removed)
                }
            };
            if let Err(e) = r {
                tracing::error!("workspace fact: {e}");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(run);
            }
            Err(_) => {
                std::thread::spawn(run);
            }
        }
    }
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
        // applies
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
        // where the Node said it prepared it (N0e-E11c): a directory here only on the local Node
        let local = self.local_node.get().map(|n| n.as_ref());
        let dir = bound
            .as_ref()
            .and_then(|w| prepared_dir(&self.store, local, w));
        engine.stash(decision, yolo_input(bound.as_ref(), dir.as_ref(), request));
        // the Node waits for the answer from the moment it asked
        engine.arm(decision);
    }
    fn permission_asked(
        &self,
        _session: &str,
        decision: &str,
        _request: &steve_node_api::PermissionRequest,
    ) {
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
    fn turn_ended(
        &self,
        session: &str,
        attempt: Option<&str>,
        stop_reason: &str,
        error: Option<String>,
    ) {
        let Some(core) = self.core.get() else { return };
        let t = steve_core::TurnEnd {
            session_id: session.into(),
            attempt: attempt.map(String::from),
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
/// the fleet). A session bound to no Node runs nowhere (the Hub never runs a harness itself): a
/// new one no Node takes is refused ([`steve_core::AgentRuntime::refusal`]). Holds and counts
/// cover every Node.
pub struct RoutedRuntime {
    /// The local Node's coordinator (`STEVE_NODE_MODE=local`).
    pub node: Option<Arc<super::node_runtime::NodeRuntime>>,
    /// The remote Nodes' coordinators (enabled by default).
    pub fleet: Option<Arc<crate::nodes::NodeFleet>>,
    /// The remote Node new sessions are bound to when they can be (`STEVE_NODE_BIND`); unset: none.
    pub bind_remote: Option<steve_node_api::NodeId>,
    /// `STEVE_NODE_PLACEMENT=spread` (B4): a new session without a fixed Node may go to a linked
    /// remote Node that is clearly less loaded than this machine.
    pub spread: bool,
    /// Where bindings are read.
    pub store: steve_store::Store,
    /// A binding starts in the mode the session is meant to have ([`HubHost::perm_mode`]), and in
    /// a directory the local Node takes.
    pub host: Arc<HubHost>,
    /// The local Node's roots, built again before a session is bound to it.
    pub workspaces: Arc<super::workspaces::Workspaces>,
    /// Steve's worktrees directory, where the local Node makes the items' worktrees (set with the
    /// local Node; unset, the core makes them).
    pub worktrees: Option<std::path::PathBuf>,
}

/// Whether boot recovery leaves `session` to the Node it is bound to (enabled here or not: a
/// Node's reconcile settles its attempts, a parked session waits for its Node): only when its
/// binding was read; on a read error the core's recovery says the cut turn is not continued.
fn bound_for_recovery(store: &steve_store::Store, session: &str) -> bool {
    super::node_runtime::bound_strict(store, session).unwrap_or_else(|e| {
        tracing::warn!(
            session,
            "binding unreadable at recovery: {e}; its cut turn is not continued"
        );
        false
    })
}

/// Which backend a session goes to.
enum Backend {
    Node(Arc<super::node_runtime::NodeRuntime>),
    /// Bound to a Node that is not enabled here (STEVE_NODE_MODE or STEVE_NODE_LISTEN off): its
    /// messages wait in the queue and nothing else happens.
    Parked,
    /// Bound to no Node: nothing runs it ([`RoutedRuntime::refused`]).
    Unbound,
}

/// The Node `session` is bound to. A read error counts as bound to an unknown Node: never hand
/// it to another backend.
fn node_of(store: &steve_store::Store, session: &str) -> Option<String> {
    node_of_strict(store, session).unwrap_or_else(|e| {
        tracing::error!("binding of {session}: {e}");
        Some(String::new())
    })
}

/// [`node_of`], with a read error as an error.
fn node_of_strict(
    store: &steve_store::Store,
    session: &str,
) -> Result<Option<String>, steve_store::StoreError> {
    use rusqlite::OptionalExtension;
    store.reader(|c| {
        Ok::<_, steve_store::StoreError>(
            c.query_row(
                "SELECT node_id FROM runtime_sessions WHERE session_id=?",
                [session],
                |r| r.get::<_, String>(0),
            )
            .optional()?,
        )
    })
}

/// How much less loaded (load per CPU) a remote Node must be for a new session to go there.
const SPREAD_MARGIN: f64 = 0.25;

/// The least loaded of `there` if it is clearly less loaded than `here`.
fn spread_pick<T>(here: f64, there: impl IntoIterator<Item = (T, f64)>) -> Option<T> {
    there
        .into_iter()
        .filter(|(_, l)| l.is_finite() && *l + SPREAD_MARGIN < here)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

impl RoutedRuntime {
    fn route(&self, session_id: &str) -> Backend {
        // A proven unbound session holds no process; cancellation can retire it. Adoption has
        // its own workflow gate. An unreadable binding still parks through node_of.
        self.route_to(node_of(&self.store, session_id))
    }

    /// [`Self::route`], with an unreadable binding as an error.
    fn route_strict(&self, session_id: &str) -> Result<Backend, steve_store::StoreError> {
        Ok(self.route_to(node_of_strict(&self.store, session_id)?))
    }

    /// An unbound work unit cannot be adopted before same-principal and Node admission pass.
    /// Read failures likewise cannot establish that a session is ordinary work.
    fn workflow_item(&self, session_id: &str) -> Result<Option<String>, steve_store::StoreError> {
        let Some(item) = self
            .store
            .sessions()
            .get(session_id)?
            .and_then(|s| s.item_id)
        else {
            return Ok(None);
        };
        // Reset/replan may detach an old attempt from the live graph. Its durable source marker
        // still prevents an old child/session from gaining ordinary session adoption.
        if self.store.workflows().by_child(&item)?.is_some()
            || self
                .store
                .items()
                .get(&item)?
                .is_some_and(|i| i.source.1 == "workflow")
        {
            Ok(Some(item))
        } else {
            Ok(None)
        }
    }

    fn check_workflow_session(&self, session_id: &str) -> steve_core::Result<()> {
        if let Some(why) = self
            .store
            .reader(|c| steve_store::workflow_gate::session_blocker(c, session_id))?
        {
            return Err(CoreError::Msg(why));
        }
        let Some(item) = self.workflow_item(session_id)? else {
            return Ok(());
        };
        steve_core::AgentRuntime::check_workflow_execution(self, &item)
    }

    fn route_to(&self, node: Option<String>) -> Backend {
        let Some(node) = node else {
            return Backend::Unbound;
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

    /// Whether the Node `session_id` is bound to can be told [`steve_node_api::PermissionMode::Auto`]:
    /// the local Node always (it is this build), a remote one once its link said
    /// [`steve_node_api::AUTO_MODE`]. An older Node cannot decode it and would refuse the configure.
    /// `None`: not known yet (a remote Node not linked since the Hub started), which may change.
    fn speaks_auto(&self, session_id: &str) -> Option<bool> {
        let Some(node) = node_of(&self.store, session_id) else {
            return Some(false);
        };
        if self
            .node
            .as_ref()
            .is_some_and(|n| n.node_id().as_str() == node)
        {
            return Some(true);
        }
        match (&self.fleet, steve_node_api::NodeId::new(node)) {
            (Some(f), Ok(id)) => f.speaks_known(&id, steve_node_api::AUTO_MODE),
            _ => Some(false),
        }
    }

    /// The harness mode `session_id` is meant to start in (YOLO's widening included).
    fn perm_mode(&self, session_id: &str) -> Option<String> {
        self.host.perm_mode(session_id)
    }

    /// Binds a new session to the configured remote Node when it can run there. False: it was not
    /// (not configured, not possible for this session, or the bind failed), nothing changed.
    fn bind_remote(&self, session_id: &str, after: Option<&str>) -> bool {
        let target = match &self.bind_remote {
            Some(t) => t.clone(),
            None if self.spread => match self.spread_target() {
                Some(t) => t,
                None => return false,
            },
            None => return false,
        };
        self.bind_remote_to(session_id, after, &target)
    }

    /// The tag the item of `session_id` asks of a Node (B7).
    fn required_tag(&self, session_id: &str) -> Option<String> {
        let item = self.store.sessions().get(session_id).ok()??.item_id?;
        self.store
            .reader(|c| steve_store::nodes::item_tag(c, &item))
            .ok()?
    }

    /// The linked remote Node clearly less loaded than this machine (load per CPU, from the
    /// vitals of the last few minutes, B3), if any: the least loaded of them.
    fn spread_target(&self) -> Option<steve_node_api::NodeId> {
        let f = self.fleet.as_ref()?;
        let since = crate::nodes::now() - 5 * 60 * 1000;
        let load = |id: &str| -> Option<f64> {
            let data: String = self
                .store
                .reader(|c| {
                    use rusqlite::OptionalExtension;
                    c.query_row(
                        "SELECT data FROM node_vitals WHERE node_id=? AND at>=? ORDER BY at DESC LIMIT 1",
                        rusqlite::params![id, since],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(steve_store::StoreError::from)
                })
                .ok()??;
            let v: serde_json::Value = serde_json::from_str(&data).ok()?;
            let cpus = v["cpus"].as_f64().filter(|c| *c > 0.0)?;
            Some(v["load"][0].as_f64()? / cpus)
        };
        // this machine unknown: nothing to compare with, the work stays
        let here = load("local")?;
        let there = f
            .nodes()
            .into_iter()
            .filter(|id| id != "local")
            .filter_map(|id| steve_node_api::NodeId::new(id).ok())
            .filter(|id| f.remote(id).generation().is_some())
            .filter_map(|id| load(id.as_str()).map(|l| (id, l)));
        spread_pick(here, there)
    }

    fn bind_remote_to(
        &self,
        session_id: &str,
        after: Option<&str>,
        target: &steve_node_api::NodeId,
    ) -> bool {
        let Some(f) = &self.fleet else {
            return false;
        };
        // H1: a remote Node runs only its owner's own work until runs there are sandboxed
        let refusal = self
            .store
            .sessions()
            .get(session_id)
            .ok()
            .flatten()
            .map(|s| {
                self.store.reader(|c| {
                    super::node_binding::remote_refusal(c, target.as_str(), &s.workspace_id)
                })
            });
        match refusal {
            Some(Ok(None)) => {}
            Some(Ok(Some(r))) => {
                let why = steve_i18n::t!(
                    "hub.node.remote.refused",
                    node = target.as_str(),
                    why = r.why()
                );
                tracing::info!("{session_id}: not bound to the remote Node: {why}");
                return false;
            }
            _ => return false,
        }
        let auto = f.speaks(target, steve_node_api::AUTO_MODE);
        let perm = self.perm_mode(session_id);
        let Some(b) = super::node_binding::remote_binding_for(
            &self.store,
            session_id,
            perm.as_deref(),
            target,
            auto,
        ) else {
            tracing::info!("{session_id}: not bound to the remote Node {target}");
            return false;
        };
        let Some(rt) = f.start(target) else {
            return false;
        };
        match rt.bind_after(&b, after) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("bind {session_id} to the remote Node {target}: {e}");
                false
            }
        }
    }
}

/// Why the local Node cannot take a session, as text (in the current language: call it inside
/// [`steve_i18n::stored`]). `None`: maybe only for now (a row unreadable, gone or ended).
fn unbound_why(u: super::node_binding::Unbound) -> Option<String> {
    use super::node_binding::Unbound;
    Some(match u {
        Unbound::Unreadable | Unbound::Missing | Unbound::Ended => return None,
        Unbound::ReadOnly => steve_i18n::t!("hub.node.unbound.read_only"),
        Unbound::NotHere => steve_i18n::t!("hub.node.unbound.not_here"),
        Unbound::NoItem => steve_i18n::t!("hub.node.unbound.no_item"),
        Unbound::Archived => steve_i18n::t!("hub.node.unbound.archived"),
        Unbound::Bot => steve_i18n::t!("hub.node.unbound.bot"),
        Unbound::Mode(m) => steve_i18n::t!("hub.node.unbound.mode", mode = m),
        Unbound::OutsideRoots => steve_i18n::t!("hub.node.unbound.outside_roots"),
        Unbound::Workflow(why) => why,
    })
}

impl RoutedRuntime {
    /// The binding `session_id` gets on the local Node, planned only (nothing bound or written).
    fn local_binding(&self, session_id: &str) -> Option<super::node_runtime::Binding> {
        self.local_plan(session_id).ok()
    }

    /// [`Self::local_binding`], saying why there is none.
    fn local_plan(
        &self,
        session_id: &str,
    ) -> Result<super::node_runtime::Binding, super::node_binding::Unbound> {
        // a project added a moment ago is a root before its first session is prepared
        if let Err(e) = self.workspaces.refresh_roots() {
            tracing::error!("local Node roots before binding {session_id}: {e}");
        }
        let perm = self.perm_mode(session_id);
        let local = self.host.local_node.get();
        let allows = |at: &steve_node_api::RootPath| local.is_some_and(|l| l.allows(at));
        super::node_binding::plan(
            &self.store,
            session_id,
            perm.as_deref(),
            &allows,
            self.worktrees.as_deref(),
        )
    }

    /// Why nothing runs `session_id` (bound to no Node), as stored text (zh-CN).
    fn refused(&self, session_id: &str) -> String {
        steve_i18n::stored(|| {
            let why = match &self.node {
                None => steve_i18n::t!("hub.node.unbound.off"),
                Some(n) if n.is_stopped() => steve_i18n::t!("hub.node.unbound.stopped"),
                // a held work unit was never planned
                Some(_) if self.check_workflow_session(session_id).is_err() => {
                    steve_i18n::t!("hub.node.unbound.unreadable")
                }
                Some(_) => match self.local_plan(session_id) {
                    Err(u) => unbound_why(u)
                        .unwrap_or_else(|| steve_i18n::t!("hub.node.unbound.unreadable")),
                    Ok(_) => steve_i18n::t!("hub.node.unbound.bind_failed"),
                },
            };
            steve_i18n::t!("hub.node.refused", why = why)
        })
    }
}

impl steve_core::AgentRuntime for RoutedRuntime {
    fn check_workflow_execution(&self, item_id: &str) -> steve_core::Result<()> {
        if let Some(why) = self
            .store
            .reader(|c| steve_store::workflow_gate::item_blocker(c, item_id))?
        {
            return Err(CoreError::Msg(why));
        }
        let item = self
            .store
            .items()
            .get(item_id)?
            .ok_or_else(|| CoreError::Msg("协作事项不存在。".into()))?;
        let graph = match self.store.workflows().by_child(item_id)? {
            Some((parent, _)) => self.store.workflows().get(&parent)?,
            None => self.store.workflows().get(item_id)?,
        }
        .ok_or_else(|| CoreError::Msg("协作工作图不存在，无法确认请求主体。".into()))?;
        // Use the ordinary principal contract after the strict durable gate has ruled out its
        // departed-member fallback. NULL requester is an explicit workspace service principal.
        let principal = self
            .store
            .reader(|c| super::principal::of_item(c, item_id))?
            .ok_or_else(|| CoreError::Msg("无法确认协作请求主体。".into()))?;
        let parent = self
            .store
            .reader(|c| super::principal::of_item(c, &graph.item_id))?;
        if parent.as_ref() != Some(&principal) {
            return Err(CoreError::Msg("工作单元与主事项的请求主体不一致。".into()));
        }
        if graph.item_id == item_id {
            for unit in &graph.units {
                if matches!(unit.status.as_str(), "dispatching" | "running")
                    && let Some(child) = unit.item_id.as_deref()
                {
                    // Each existing unit keeps its own binding and inherited routing. A usable
                    // parent Node cannot hide an offline or no-longer-authorized child Node.
                    self.check_workflow_execution(child)?;
                }
            }
        }
        if let Some(project) = item.project_id.as_deref() {
            let project = self
                .store
                .projects()
                .get(project)?
                .ok_or_else(|| CoreError::Msg("协作项目不存在。".into()))?;
            if project.device_id != super::workspaces::HUB_DEVICE {
                return Err(CoreError::Msg(
                    "当前 Node 路由仅支持本机项目的独立 Git worktree。".into(),
                ));
            }
        }
        // Before a child exists, prove that its inherited routing constraints have an eligible
        // Node. Actual sessions still go through ordinary binding, roots, policy and ceiling.
        let eligible = |node: &str| -> steve_core::Result<bool> {
            let tag = self
                .store
                .reader(|c| steve_store::nodes::item_tag(c, item_id))?;
            if let Some(tag) = tag {
                let nodes = self
                    .store
                    .reader(|c| steve_store::nodes::with_tag(c, &tag))?;
                if !nodes.iter().any(|n| n == node) {
                    return Ok(false);
                }
            }
            if node == "local" {
                return Ok(self.node.as_ref().is_some_and(|n| !n.is_stopped())
                    && (item.project_id.is_none() || self.worktrees.is_some()));
            }
            if item.project_id.is_some() {
                return Ok(false);
            }
            let Some(f) = &self.fleet else {
                return Ok(false);
            };
            let Ok(id) = steve_node_api::NodeId::new(node.to_string()) else {
                return Ok(false);
            };
            Ok(self
                .store
                .reader(|c| super::node_binding::node_takes(c, node, &item.workspace_id))?
                && f.remote(&id).generation().is_some())
        };
        // Recovery keeps the bound Node; availability on some other Node is no substitute.
        for session in self.store.sessions().of_item(item_id)? {
            if session.status != "ended"
                && let Some(node) = node_of_strict(&self.store, &session.id)?
                && !eligible(&node)?
            {
                return Err(CoreError::Msg(
                    "协作会话绑定的 Node 不可用或不再获授权。".into(),
                ));
            }
        }
        if eligible("local")? {
            return Ok(());
        }
        let mut targets = Vec::new();
        if let Some(target) = &self.bind_remote {
            targets.push(target.as_str().to_string());
        }
        if let Some(target) = self.spread_target().filter(|_| self.spread) {
            targets.push(target.as_str().to_string());
        }
        if let Some(tag) = self
            .store
            .reader(|c| steve_store::nodes::item_tag(c, item_id))?
        {
            targets.extend(
                self.store
                    .reader(|c| steve_store::nodes::with_tag(c, &tag))?,
            );
        }
        for node in targets {
            if eligible(&node)? {
                return Ok(());
            }
        }
        Err(CoreError::Msg(
            "协作工作没有可用且获授权的 Node；请检查设备连接、路由标签与项目 worktree 配置。"
                .into(),
        ))
    }
    fn makes_worktrees(&self) -> bool {
        self.node.is_some() && self.worktrees.is_some()
    }
    /// Only the local Node makes worktrees; a remote one takes no item of a project.
    fn would_make_workspace(&self, session_id: &str) -> bool {
        self.makes_worktrees()
            && self
                .local_binding(session_id)
                .is_some_and(|b| matches!(b.prepare, steve_node_api::PrepareKind::Worktree { .. }))
    }
    fn makes_workspace(&self, session_id: &str) -> bool {
        let Some(n) = &self.node else { return false };
        n.makes_workspace(session_id).unwrap_or_else(|e| {
            tracing::error!("workspace of {session_id}: {e}");
            false
        })
    }
    fn tidy(&self, session_id: &str, keep_branch: bool) -> steve_core::Result<bool> {
        let Some(n) = &self.node else {
            return Ok(false);
        };
        let Some(item) = self
            .store
            .sessions()
            .get(session_id)?
            .and_then(|s| s.item_id)
        else {
            return Ok(false);
        };
        Ok(n.tidy_workspace(&item, keep_branch)?)
    }
    /// A new session is bound to the remote Node named by `STEVE_NODE_BIND` when it can run there;
    /// otherwise, with the local Node enabled here, to it when its binding can be planned.
    /// Otherwise (the local Node off or stopped, a permission mode no Node can express, a
    /// directory outside its roots, a failed bind) it is bound nowhere, and [`Self::refusal`]
    /// says why. `after` (a predecessor's release on its way): it opens only once that is done.
    fn created(&self, session_id: &str, after: Option<&str>) {
        // Workflow coordinates ordinary Node sessions. Confirm its authority before any binding
        // or preparation; no Hub execution fallback is available.
        if let Err(e) = self.check_workflow_session(session_id) {
            tracing::warn!(session_id, "协作运行未就绪：{e}");
            return;
        }
        // the tag its item asks of a Node (B7): a Node with it, the Hub's own first
        let mut here = false;
        if let Some(tag) = self.required_tag(session_id) {
            let nodes = self
                .store
                .reader(|c| steve_store::nodes::with_tag(c, &tag))
                .unwrap_or_default();
            if self.node.is_some() && nodes.iter().any(|n| n == "local") {
                here = true;
            } else {
                for n in nodes.iter().filter(|n| n.as_str() != "local") {
                    if let Ok(id) = steve_node_api::NodeId::new(n.clone())
                        && self.bind_remote_to(session_id, after, &id)
                    {
                        return;
                    }
                }
                let text = steve_i18n::t!("hub.node.tag_unmet", tag = tag);
                if let Err(e) =
                    self.store
                        .events()
                        .add(session_id, "system", serde_json::json!({"text": text}))
                {
                    tracing::warn!("{session_id}: {e}");
                }
                // A workflow must not lose its required Node tag through the ordinary fallback.
                if !matches!(self.workflow_item(session_id), Ok(None)) {
                    return;
                }
            }
        }
        if !here && self.bind_remote(session_id, after) {
            return;
        }
        let Some(n) = &self.node else { return };
        match self.local_binding(session_id) {
            Some(b) => {
                if let Err(e) = n.bind_after(&b, after) {
                    n.fell_back();
                    tracing::error!("bind {session_id} to the Node: {e}; refused");
                }
            }
            None => {
                n.fell_back();
                tracing::info!("{session_id}: no Node binding can be planned; refused")
            }
        }
    }
    fn refusal(&self, session_id: &str) -> Option<String> {
        match node_of_strict(&self.store, session_id) {
            Ok(Some(_)) => None,
            Ok(None) => Some(self.refused(session_id)),
            // not known: its messages park rather than run anywhere ([`node_of`])
            Err(e) => {
                tracing::error!("binding of {session_id}: {e}");
                None
            }
        }
    }
    /// A session bound to no Node (nothing took it) is refused, said in it: the Hub never runs
    /// a harness itself.
    fn prompt(
        &self,
        session_id: &str,
        text: &str,
        via: &str,
        files: &[steve_core::repo::FileRef],
    ) -> steve_core::Result<steve_core::Prompted> {
        self.check_workflow_session(session_id)?;
        let n = match self.route(session_id) {
            Backend::Node(n) => Some(n),
            Backend::Parked => None,
            Backend::Unbound => {
                let why = self.refused(session_id);
                if let Err(e) = self.store.events().add(
                    session_id,
                    "system",
                    json!({ "text": why, "refused": true }),
                ) {
                    tracing::error!("refusal of {session_id}: {e}");
                }
                return Err(CoreError::Msg(why));
            }
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
            Backend::Parked | Backend::Unbound => None,
        }
    }
    /// A Node session's stop goes to its coordinator, only for the attempt it names.
    fn stop(&self, turn: &steve_core::Running) -> steve_core::Result<steve_core::Stop> {
        match self.route(&turn.session) {
            Backend::Node(n) => Ok(if n.cancel_attempt(&turn.session, &turn.attempt)? {
                steve_core::Stop::Requested
            } else {
                steve_core::Stop::NotRunning
            }),
            Backend::Parked | Backend::Unbound => Ok(steve_core::Stop::NotRunning),
        }
    }
    /// The user's Stop: a Node may also retire a control of the session whose outcome is unknown.
    fn user_stop(&self, session_id: &str) -> steve_core::Result<steve_core::Stop> {
        match self.route(session_id) {
            Backend::Node(n) => Ok(if n.user_stop(session_id)? {
                steve_core::Stop::Requested
            } else {
                steve_core::Stop::NotRunning
            }),
            Backend::Parked | Backend::Unbound => Ok(steve_core::Stop::NotRunning),
        }
    }
    fn is_busy(&self, session_id: &str) -> bool {
        match self.route(session_id) {
            Backend::Node(n) => n.is_busy(session_id),
            Backend::Parked => true,
            Backend::Unbound => false,
        }
    }
    /// A Node session's release is a queued command ([`steve_core::Released::Pending`]); one the
    /// business side ended during a turn is queued for when the turn is over. A parked one keeps
    /// whatever its Node holds ([`steve_core::Released::Busy`], as it counts as busy); nothing
    /// holds one bound to no Node.
    fn release(&self, session_id: &str) -> steve_core::Result<steve_core::Released> {
        use super::node_runtime::Release;
        use steve_core::Released;
        match self.route(session_id) {
            Backend::Node(n) => Ok(match n.release(session_id)? {
                Release::Gone => Released::Gone,
                Release::Pending(op) => Released::Pending(op),
                Release::Busy => Released::Busy,
            }),
            Backend::Parked => Ok(Released::Busy),
            Backend::Unbound => Ok(Released::Gone),
        }
    }
    /// A Node session's answer is a queued command ([`steve_core::Answered::Pending`]); a store
    /// error is returned, so the card stays open and the Node keeps waiting for a real answer.
    /// That holds for the lookups that pick the backend too.
    fn answer_permission(
        &self,
        decision_id: &str,
        option_id: Option<&str>,
    ) -> steve_core::Result<steve_core::Answered> {
        use steve_core::Answered;
        let Some(sid) = super::node_runtime::decision_session(&self.store, decision_id)? else {
            return Ok(Answered::Gone);
        };
        match self.route_strict(&sid)? {
            Backend::Node(n) => Ok(if n.answer_permission(decision_id, option_id)? {
                Answered::Pending
            } else {
                Answered::Gone
            }),
            // parked: its Node is not enabled here, nobody can take the answer now; unbound:
            // the harness that asked is gone
            Backend::Parked | Backend::Unbound => Ok(Answered::Gone),
        }
    }
    fn hold(&self, on: bool) {
        if let Some(n) = &self.node {
            n.hold(on);
        }
        if let Some(f) = &self.fleet {
            f.hold(on);
        }
    }
    fn is_holding(&self) -> bool {
        self.node.as_ref().is_some_and(|n| n.is_holding())
            || self.fleet.as_ref().is_some_and(|f| f.is_holding())
    }
    fn uncertain(&self, session_id: &str) -> bool {
        self.node_for(session_id)
            .is_some_and(|n| n.is_uncertain(session_id))
    }
    /// Not `route()`: its park-on-doubt suits messages (undone by a later call), but here a yes
    /// drops the session's resume intent for good, so an unreadable binding counts as unbound.
    fn recovers_itself(&self, session_id: &str) -> bool {
        bound_for_recovery(&self.store, session_id)
    }
    fn running_turns(&self) -> usize {
        self.node.as_ref().map_or(0, |n| n.running_turns())
            + self.fleet.as_ref().map_or(0, |f| f.running_turns())
    }
    /// What a release waits for: the Hub's own Node's work, unless that Node runs in a process
    /// of its own (B1), whose turns go on through the Hub's restart.
    fn node_working(&self) -> usize {
        if self.host.local_node.get().is_some_and(|n| !n.in_process()) {
            return 0;
        }
        self.node.as_ref().map_or(0, |n| n.working())
    }
    fn flush(&self, session_id: &str, lead: Option<(&str, &str)>) -> steve_core::Flushed {
        use super::node_runtime::Planned;
        if let Err(e) = self.check_workflow_session(session_id) {
            tracing::warn!(session_id, "协作队列等待主体或 Node 准入：{e}");
            return steve_core::Flushed::Held;
        }
        match self.route(session_id) {
            Backend::Node(n) => match n.flush_turn(session_id, lead) {
                Ok((Planned::Created, Some(attempt))) => {
                    steve_core::Flushed::Started(steve_core::Running {
                        session: session_id.to_string(),
                        attempt,
                    })
                }
                Ok((Planned::Created, None)) | Ok((Planned::Busy, _)) => steve_core::Flushed::Busy,
                Ok((Planned::Held, _)) => steve_core::Flushed::Held,
                Ok((Planned::Empty, _)) => steve_core::Flushed::Empty,
                Err(e) => {
                    tracing::error!("flush {session_id}: {e}");
                    steve_core::Flushed::Held
                }
            },
            // a parked session's queue waits for its Node; nothing takes an unbound one's
            Backend::Parked | Backend::Unbound => steve_core::Flushed::Held,
        }
    }
    /// Node attempts of one turn (a gateway retry) are the same turn; a store that cannot tell
    /// says they are not.
    fn same_turn(&self, a: &steve_core::Running, b: &steve_core::Running) -> bool {
        if a == b {
            return true;
        }
        match self.route(&a.session) {
            Backend::Node(n) if a.session == b.session => n
                .same_turn(&a.attempt, &b.attempt)
                .inspect_err(|e| tracing::error!("same turn {}: {e}", a.session))
                .unwrap_or(false),
            _ => false,
        }
    }
    fn set_mode(&self, session_id: &str, mode: &str) -> steve_core::Result<steve_core::Configured> {
        use super::node_runtime::Configured as Node;
        use steve_core::Configured;
        // the binding is read strictly: an unreadable one is an error (the caller keeps the
        // switch pending and retries it), not a parked session's NotRunning
        match self.route_strict(session_id)? {
            // through the Node's configure, at the session's next idle boundary
            Backend::Node(n) => match super::node_binding::permission_mode(mode) {
                // a remote Node not linked since the Hub started may well speak auto: not known
                // yet, so the switch stays pending (an error) and the next sync asks again
                Some(steve_node_api::PermissionMode::Auto)
                    if self.speaks_auto(session_id).is_none() =>
                {
                    Err(CoreError::Msg(format!(
                        "set mode {mode} on {session_id}: its Node has not linked yet"
                    )))
                }
                Some(steve_node_api::PermissionMode::Auto)
                    if self.speaks_auto(session_id) == Some(false) =>
                {
                    // its hello said an older protocol: trying again would not help
                    Ok(Configured::Unsupported)
                }
                Some(p) => Ok(
                    match n.configure(
                        session_id,
                        &super::node_runtime::ConfigChange {
                            permissions: Some(p),
                            ..Default::default()
                        },
                    )? {
                        Node::Unchanged => Configured::Applied,
                        Node::Sent | Node::Pending => Configured::Pending,
                        Node::OnReopen => Configured::NotRunning,
                    },
                ),
                // a mode no Node can express: permanent, trying again would not help
                None => Ok(Configured::Unsupported),
            },
            // its Node is not enabled here, or none runs it: nothing runs for it now
            Backend::Parked | Backend::Unbound => Ok(Configured::NotRunning),
        }
    }
}

// ── the HTTP layer's Domain ──

pub struct CoreDomain {
    pub core: Core,
    /// The harness adapters the Hub knows, kept fresh from the local Node.
    pub harnesses: Arc<crate::harnesses::HubHarnesses>,
    /// The runtime the core runs sessions on.
    pub routed: Arc<RoutedRuntime>,
    /// The local Node (`STEVE_NODE_MODE=local`): stopped with the Hub, its harnesses
    /// with it.
    pub node: Option<Arc<steve_node::LocalNode>>,
    /// The connections' runs (long connections, pollers): stopped with the Hub.
    pub lifecycle: Option<steve_channels::lifecycle::ConnectionRuntime>,
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
            /* research runs cut off by the restart */
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
            store
                .maintenance_set(false)
                .map_err(|e| BackendError::msg(e.to_string()))?;
            self.core
                .maintenance_hold(false)
                .map_err(|e| BackendError::msg(e.to_string()))?;
            self.ready().await?;
        }
        let active = store.maintenance();
        let running = store.activities();
        // a Node turn has no cut-and-resume: it holds the drain, also while it waits for your
        // permission answer (until it is answered or expires), and while its outcome is unknown
        let node = self.core.node_working();
        let source_release_busy = self.core.source_release_busy();
        let quiet =
            active && running.values().sum::<usize>() == 0 && node == 0 && !source_release_busy;
        let unknown: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM mail_drafts WHERE status='unknown'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .map_err(|e| BackendError::msg(e.to_string()))?;
        let sealed = quiet && self.core.node_working() == 0 && store.maintenance_seal();
        let mut blockers = self
            .routed
            .node
            .as_ref()
            .map_or_else(Vec::new, |n| n.working_details());
        for (kind, count) in &running {
            if *count > 0 {
                blockers.push(json!({"kind":"activity","state":kind,"count":count,"reason":"waiting_for_activity"}));
            }
        }
        if source_release_busy {
            blockers.push(json!({"kind":"source_release","state":"running","reason":"waiting_for_source_release"}));
        }
        // `acp` and `adapters_stopped` stay for older upgraders: the Hub runs no adapter itself
        Ok(json!({"maintenance":active,"ready":sealed,"running":running,"acp":0,"node":node,"adapters_stopped":true,"unknown_mail":unknown,"source_release_busy":source_release_busy,"blockers":blockers}).as_object().unwrap().clone())
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
        let lifecycle = async {
            if let Some(rt) = &self.lifecycle {
                rt.shutdown().await;
            }
        };
        let (research_stopped, node_stopped, ()) = tokio::join!(
            self.core.shutdown_research(Duration::from_secs(5)),
            node,
            lifecycle,
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
            requester: None,
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
    async fn diagnostic_snapshot(&self, id: &str) -> BResult<Value> {
        let (c, id) = (self.core.clone(), id.to_string());
        blocking(move || c.diagnostic_snapshot(&id)).await
    }
    async fn workflow_get(&self, id: &str) -> BResult<Value> {
        let (c, id) = (self.core.clone(), id.to_string());
        blocking(move || c.workflow_get(&id)).await
    }
    async fn dependency_get(&self, id: &str) -> BResult<Value> {
        let (c, id) = (self.core.clone(), id.to_string());
        blocking(move || c.dependency_get(&id)).await
    }
    async fn dependency_wait(
        &self,
        id: &str,
        expected_cycle: i64,
        resume: &str,
        reason: &str,
    ) -> BResult<Value> {
        let (c, id, resume, reason) = (
            self.core.clone(),
            id.to_string(),
            resume.to_string(),
            reason.to_string(),
        );
        blocking(move || c.dependency_wait(&id, expected_cycle, &resume, &reason)).await
    }
    async fn workflow_change(&self, id: &str, args: Value, actor: &str) -> BResult<Value> {
        let (c, id, actor) = (self.core.clone(), id.to_string(), actor.to_string());
        blocking(move || c.workflow_change(&id, args, &actor)).await
    }
    async fn lifecycle_views(&self, ws: &str) -> BResult<std::collections::HashMap<String, Value>> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        blocking(move || c.lifecycle_views_ws(&ws)).await
    }
    async fn lifecycle_tick(&self) -> BResult<usize> {
        let c = self.core.clone();
        blocking(move || c.lifecycle_tick()).await
    }
    async fn lane_view(&self, upstream: &str, branch: &str) -> BResult<Option<Value>> {
        let (c, u, b) = (self.core.clone(), upstream.to_string(), branch.to_string());
        blocking(move || c.lane_view(&u, &b)).await
    }
    async fn lane_policy(
        &self,
        upstream: &str,
        branch: &str,
        window_ms: Option<i64>,
        max_members: Option<i64>,
    ) -> BResult<Option<Value>> {
        let (c, u, b) = (self.core.clone(), upstream.to_string(), branch.to_string());
        blocking(move || c.lane_policy(&u, &b, window_ms, max_members)).await
    }
    async fn lane_now(&self, upstream: &str, branch: &str) -> BResult<bool> {
        let (c, u, b) = (self.core.clone(), upstream.to_string(), branch.to_string());
        blocking(move || c.lane_now(&u, &b)).await
    }
    async fn mq_view(&self, upstream: &str, branch: &str, ws: &str) -> BResult<Option<Value>> {
        let (c, u, b, w) = (
            self.core.clone(),
            upstream.to_string(),
            branch.to_string(),
            ws.to_string(),
        );
        blocking(move || c.mq_view(&u, &b, &w)).await
    }
    async fn mq_act(
        &self,
        upstream: &str,
        branch: &str,
        ws: &str,
        act: Value,
        actor: &str,
    ) -> BResult<Option<Value>> {
        let (c, u, b, w, a) = (
            self.core.clone(),
            upstream.to_string(),
            branch.to_string(),
            ws.to_string(),
            actor.to_string(),
        );
        blocking(move || c.mq_act(&u, &b, &w, &act, &a)).await
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
        // routed: a Node session's stop goes to its coordinator
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
    async fn continue_in_new_session(&self, session: &str, actor: &str) -> BResult<String> {
        let (c, sid, actor) = (self.core.clone(), session.to_string(), actor.to_string());
        blocking(move || c.continue_in_new_session(&sid, &actor)).await
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
    async fn say_guard_note(&self, ws: &str) -> BResult<()> {
        let (c, ws) = (self.core.clone(), ws.to_string());
        blocking(move || {
            c.say_note(
                &ws,
                steve_vault::guard::ROUTED,
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
    async fn promote(&self, ws: &str, message: &str, by: &str) -> BResult<Obj> {
        to_obj(
            self.core
                .promote(ws, message, Some(by))
                .await
                .map_err(cerr)?,
        )
    }
    async fn reroute(&self, ws: &str, message: &str, to: &str, by: &str) -> BResult<(Obj, bool)> {
        let to = steve_core::thread::RerouteTo::parse(to).ok_or_else(|| BackendError {
            status: 400,
            message: steve_i18n::t!("hub.error.invalid", fields = "to"),
        })?;
        handled(
            self.core
                .reroute(ws, message, to, Some(by))
                .await
                .map_err(cerr)?,
        )
    }
    async fn offer(&self, ws: &str, message: &str, take: bool, by: &str) -> BResult<(Obj, bool)> {
        let (c, ws, m, by) = (
            self.core.clone(),
            ws.to_string(),
            message.to_string(),
            by.to_string(),
        );
        handled(blocking(move || c.offer(&ws, &m, take, Some(&by))).await?)
    }
    async fn suggest(
        &self,
        ws: &str,
        message: &str,
        choice: &str,
        by: &str,
    ) -> BResult<(Obj, bool)> {
        let (c, ws, m, ch, by) = (
            self.core.clone(),
            ws.to_string(),
            message.to_string(),
            choice.to_string(),
            by.to_string(),
        );
        handled(blocking(move || c.suggest(&ws, &m, &ch, Some(&by))).await?)
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

    async fn harnesses(&self, fresh: bool) -> BResult<Vec<steve_core::harnesses::Harness>> {
        if fresh && let Some(n) = &self.node {
            self.harnesses.refresh(n.as_ref(), true).await;
        }
        Ok(self.core.models().harnesses())
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
    async fn onboard_start(&self, ws: &str, actor: &str) -> BResult<String> {
        let (c, ws, actor) = (self.core.clone(), ws.to_string(), actor.to_string());
        blocking(move || c.onboard_start(&ws, &actor)).await
    }
    async fn onboard_end(&self, ws: &str, actor: &str) -> BResult<()> {
        let (c, ws, actor) = (self.core.clone(), ws.to_string(), actor.to_string());
        blocking(move || c.onboard_end(&ws, &actor, steve_core::thread::onboard::SKIPPED)).await
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
        let observation = super::diagnostics::ToolObservation::new(&self.store, s, "steve_update");
        let (c, s) = (self.core.clone(), s.clone());
        let plan: Vec<steve_core::runtime::PlanStep> = plan
            .into_iter()
            .map(|p| steve_core::runtime::PlanStep {
                step: p.step,
                done: p.done,
                left: false,
            })
            .collect();
        observation
            .run(tool(move || c.tool_update(&s, &now, &plan)))
            .await
    }
    async fn ask(
        &self,
        s: &steve_store::Session,
        question: String,
        options: Vec<String>,
        hint: steve_mcp::AskHint,
    ) -> ToolResult {
        let observation = super::diagnostics::ToolObservation::new(&self.store, s, "steve_ask");
        let (c, s) = (self.core.clone(), s.clone());
        observation
            .run(tool(move || {
                c.tool_ask_with(
                    &s,
                    &question,
                    &options,
                    hint.recommended.as_deref(),
                    hint.why.as_deref(),
                )
            }))
            .await
    }
    async fn request_approval(
        &self,
        s: &steve_store::Session,
        action: String,
        detail: String,
    ) -> ToolResult {
        let observation =
            super::diagnostics::ToolObservation::new(&self.store, s, "steve_request_approval");
        let (c, s) = (self.core.clone(), s.clone());
        observation
            .run(tool(move || c.tool_request_approval(&s, &action, &detail)))
            .await
    }
    async fn deliver(
        &self,
        s: &steve_store::Session,
        summary: String,
        outputs: Vec<steve_mcp::Output>,
        checks: Vec<steve_mcp::Check>,
        unfinished: Option<Vec<String>>,
    ) -> ToolResult {
        let observation = super::diagnostics::ToolObservation::new(&self.store, s, "steve_deliver");
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
                operated: x.operated,
            })
            .collect();
        let unfinished = unfinished.unwrap_or_default();
        observation
            .run(tool(move || {
                c.tool_deliver(&s, &summary, &outputs, &checks, &unfinished)
            }))
            .await
    }
    async fn publish(
        &self,
        s: &steve_store::Session,
        branch: String,
        summary: String,
    ) -> ToolResult {
        let observation = super::diagnostics::ToolObservation::new(&self.store, s, "steve_publish");
        let (c, s) = (self.core.clone(), s.clone());
        observation
            .run(tool(move || c.tool_publish(&s, &branch, &summary)))
            .await
    }
    async fn lane_status(&self, s: &steve_store::Session) -> Result<String, steve_mcp::ToolError> {
        let observation =
            super::diagnostics::ToolObservation::new(&self.store, s, "steve_lane_status");
        let (c, s) = (self.core.clone(), s.clone());
        observation
            .run(async {
                match tokio::task::spawn_blocking(move || c.lane_status(&s).map(|st| st.render()))
                    .await
                {
                    Ok(r) => r.map_err(|e| e.to_string().into()),
                    Err(e) => Err(e.to_string().into()),
                }
            })
            .await
    }
}

#[cfg(test)]
mod recovery_tests {
    use steve_store::{NoPublisher, Store};

    /// A binding that cannot be read parks messages (undoable) but never makes boot recovery
    /// leave a cut turn to a backend unsaid.
    #[test]
    fn an_unreadable_binding_is_not_bound_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), NoPublisher).unwrap();
        assert!(!super::bound_for_recovery(&store, "s1"));
        store
            .with_conn(|c| Ok(c.execute_batch("DROP TABLE runtime_sessions")?))
            .unwrap();
        assert!(super::super::node_runtime::bound_strict(&store, "s1").is_err());
        assert!(
            super::super::node_runtime::bound(&store, "s1"),
            "messages still park on doubt"
        );
        assert!(!super::bound_for_recovery(&store, "s1"));
    }
}

#[cfg(test)]
mod routed_tests {
    use std::sync::{Arc, OnceLock};
    use steve_core::{AgentRuntime, Answered};
    use steve_store::{NoPublisher, Store};

    fn routed(dir: &std::path::Path) -> super::RoutedRuntime {
        let store = Store::open(dir.join("hub.sqlite3"), NoPublisher).unwrap();
        let host = Arc::new(super::HubHost {
            store: store.clone(),
            data_dir: dir.into(),
            core: OnceLock::new(),
            flow: OnceLock::new(),
            yolo: OnceLock::new(),
            local_node: OnceLock::new(),
        });
        super::RoutedRuntime {
            host,
            node: None,
            fleet: None,
            bind_remote: None,
            spread: false,
            workspaces: super::super::workspaces::Workspaces::new(
                store.clone(),
                None,
                None,
                vec![],
            ),
            store,
            worktrees: None,
        }
    }

    /// The lookups that pick the backend for an answer are strict: an unreadable card or binding
    /// is an error (the card stays open), never "gone" (expired).
    #[test]
    fn an_unreadable_card_or_binding_is_not_an_expired_answer() {
        let dir = tempfile::tempdir().unwrap();
        let rt = routed(dir.path());
        rt.store
            .with_conn(|c| {
                Ok(c.execute(
                    "INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,session_id,question,status,requested_by,created)
                     VALUES('d1','w','permission','session','s1','s1','?','open','agent',0)",
                    [],
                )?)
            })
            .unwrap();
        // readable, bound to no Node: the harness that asked is gone
        assert_eq!(
            rt.answer_permission("d1", Some("allow")).unwrap(),
            Answered::Gone
        );
        rt.store
            .with_conn(|c| Ok(c.execute_batch("DROP TABLE runtime_sessions")?))
            .unwrap();
        assert!(
            rt.answer_permission("d1", Some("allow")).is_err(),
            "binding unreadable"
        );
        // a mode switch reads the binding strictly too: an error keeps it pending for the next
        // sync, rather than a NotRunning that drops it
        assert!(
            rt.set_mode("s1", "default").is_err(),
            "binding unreadable: the switch is retried"
        );
        rt.store
            .with_conn(|c| Ok(c.execute_batch("DROP TABLE decisions")?))
            .unwrap();
        assert!(
            rt.answer_permission("d1", Some("allow")).is_err(),
            "card unreadable"
        );
    }

    /// A switch to auto for a session on a remote Node not linked since the Hub started is not
    /// known to be impossible: an error (the YOLO sync keeps it pending and asks again), never the
    /// permanent Unsupported, which only a hello older than AUTO_MODE gives.
    #[tokio::test]
    async fn auto_on_a_remote_node_not_linked_yet_stays_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = routed(dir.path());
        let fleet = crate::nodes::NodeFleet::new(
            rt.store.clone(),
            dir.path().to_path_buf(),
            Some(Arc::new(super::super::node_tools::tests::Ends)),
            None,
        );
        rt.fleet = Some(fleet.clone());
        rt.store
            .transaction::<_, steve_store::StoreError>(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s1','default','claude-code','.','active',1,1)",
                    [],
                )?;
                steve_store::runtime::bind_session_in(
                    tx,
                    &steve_store::runtime::SessionBinding {
                        session: "s1",
                        node: "n2",
                        workspace: &serde_json::json!({"node": "n2", "id": "w1"}),
                        node_session: "s1",
                        config_revision: "r1",
                    },
                    1,
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            rt.set_mode("s1", "auto").is_err(),
            "not linked yet: retried"
        );
        let n2 = steve_node_api::NodeId::new("n2").unwrap();
        fleet.linked(
            &n2,
            (steve_node_api::AUTO_MODE.0, steve_node_api::AUTO_MODE.1 - 1),
        );
        assert!(matches!(
            rt.set_mode("s1", "auto").unwrap(),
            steve_core::Configured::Unsupported
        ));
    }

    /// A RoutedRuntime with the local Node on (a fake one), item `i1` with an idle session `s1`
    /// bound to no Node, working in a scratch workspace (no directory of its own, so no root is
    /// needed).
    fn with_node(dir: &std::path::Path) -> super::RoutedRuntime {
        let mut rt = routed(dir);
        rt.store.ensure_workspace().unwrap();
        let node = steve_node_api::testkit::FakeNode::new("local");
        let n = super::super::node_runtime::NodeRuntime::new(
            rt.store.clone(),
            node.clone(),
            Arc::new(super::super::node_tools::tests::Ends),
        );
        node.connect(Arc::new(super::super::node_runtime::Sink(Arc::downgrade(
            &n,
        ))));
        n.start();
        rt.node = Some(n);
        exec(
            &rt.store,
            "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'t','run','执行',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,acp_session_id,created,updated) VALUES ('s1','default','i1','claude-code','','idle',NULL,1,1);",
        );
        rt
    }

    fn exec(store: &Store, sql: &str) {
        store.with_conn(|c| Ok(c.execute_batch(sql)?)).unwrap();
    }

    fn one<T: rusqlite::types::FromSql>(store: &Store, sql: &str) -> T {
        store
            .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }

    fn bound(rt: &super::RoutedRuntime) -> bool {
        one::<i64>(
            &rt.store,
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id='s1'",
        ) == 1
    }

    /// With the Node off, or stopped, a message to a session no Node took is refused, said in
    /// the session: it never runs on the Hub.
    #[tokio::test]
    async fn without_a_running_node_a_message_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = with_node(dir.path());
        rt.node.as_ref().unwrap().stop("tools did not connect");
        let e = rt
            .prompt("s1", "还在吗", "me", &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("本机 Node 没有在运行"), "{e}");
        rt.node = None;
        let e = rt
            .prompt("s1", "还在吗", "me", &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("STEVE_NODE_MODE=off"), "{e}");
        assert!(!bound(&rt));
        assert_eq!(
            one::<i64>(
                &rt.store,
                "SELECT COUNT(*) FROM events WHERE session_id='s1' AND kind='system' AND json_extract(data,'$.refused')=1"
            ),
            2,
            "said each time"
        );
        assert_eq!(
            one::<i64>(
                &rt.store,
                "SELECT COUNT(*) FROM queued WHERE session_id='s1'"
            ),
            0,
            "nothing waits for a runner that is not there"
        );
        assert!(!rt.is_busy("s1"));
        assert_eq!(rt.flush("s1", None), steve_core::Flushed::Held);
        assert_eq!(rt.release("s1").unwrap(), steve_core::Released::Gone);
    }

    /// A session the Node cannot take says why when it is messaged: a mode the Node cannot
    /// express, a bot gone, an archived project, an item moved away, an import.
    #[tokio::test]
    async fn a_session_no_node_can_take_says_why() {
        for (sql, why) in [
            (
                "INSERT INTO bots(id,workspace_id,name,harness,perm_mode,updated) VALUES ('b1','default','b','claude-code','dontAsk',1); INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('b1','default',1); UPDATE sessions SET bot_id='b1'",
                "dontAsk",
            ),
            ("UPDATE sessions SET bot_id='nobody'", "Bot"),
            (
                "INSERT INTO projects(id,workspace_id,name,path,kind,archived,created,updated) VALUES ('p1','default','p','/nonexistent-steve-b5','dir',1,1,1); UPDATE items SET project_id='p1'",
                "归档",
            ),
            ("UPDATE sessions SET item_id=NULL", "事项"),
            ("UPDATE sessions SET readonly=1", "只读"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let rt = with_node(dir.path());
            exec(&rt.store, sql);
            let r = rt.refusal("s1");
            assert!(r.as_deref().is_some_and(|r| r.contains(why)), "{sql}: {r:?}");
            if let Err(e) = rt.prompt("s1", "还在吗", "me", &[]) {
                assert!(e.to_string().contains(why), "{sql}: {e}");
            } else {
                panic!("{sql}: a message to it ran");
            }
            assert!(!bound(&rt), "{sql}");
        }
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

/// N0e-E11c: YOLO's directory of a local Node's workspace comes from the view its prepare
/// returned to the ledger, never from the Node's own bookkeeping or the session's cwd.
#[cfg(test)]
mod prepared_dir_tests {
    use super::{display_path, prepared_dir, yolo_input};

    #[test]
    fn spread_goes_only_where_it_is_clearly_lighter() {
        use super::spread_pick;
        assert_eq!(
            spread_pick(1.0, [("a", 0.9), ("b", 0.5), ("c", 0.6)]),
            Some("b")
        );
        assert_eq!(spread_pick(1.0, [("a", 0.9)]), None, "not clearly lighter");
        assert_eq!(
            spread_pick(0.1, [("a", 0.0)]),
            None,
            "this machine is idle enough"
        );
        assert_eq!(spread_pick(1.0, [("a", f64::NAN)]), None);
        assert_eq!(spread_pick::<&str>(1.0, []), None);
    }
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use steve_node_api::testkit::build;
    use steve_node_api::*;
    use steve_store::runtime::{self as rt, ReceiptState};
    use steve_store::{NoPublisher, Store, StoreError};

    struct T {
        dir: tempfile::TempDir,
        store: Store,
        node: steve_node::LocalNode,
    }

    fn t() -> T {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), NoPublisher).unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("p1")).unwrap();
        let node = steve_node::LocalNode::open(steve_node::NodeConfig {
            ceiling: steve_node::PolicyCeiling::unlimited(),
            node: NodeId::new("local").unwrap(),
            home: dir.path().join("node"),
            acp: steve_acp::AcpConfig {
                declared: Default::default(),
                adapters: Default::default(),
                node: "node".into(),
                claude_executable: "/usr/bin/false".into(),
                steve_home: dir.path().join("steve"),
                codex_home: dir.path().join("codex"),
                user_home: dir.path().to_path_buf(),
                version: "test".into(),
            },
            roots: vec![steve_node::Root {
                id: RootId::new("projects").unwrap(),
                path: std::fs::canonicalize(&projects).unwrap(),
                display: "projects".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 0,
                jobs: 0,
                execs: 0,
            },
            storage: Default::default(),
        })
        .unwrap();
        T { dir, store, node }
    }

    /// Prepares `kind` on the Node and books it in the ledger the way the coordinator does: the
    /// command, then its finished receipt.
    async fn prepare(t: &T, op: &str, kind: PrepareKind) -> WorkspaceView {
        let c = Command {
            id: OperationId::new(op).unwrap(),
            issued_at: Timestamp(steve_store::now()),
            accept_before: Timestamp(steve_store::now() + 60_000),
            grant: GrantId::new("hub").unwrap(),
            body: Prepare {
                resource: format!("item:{op}"),
                kind,
            },
        };
        let r = t.node.prepare_workspace(c.clone()).await.unwrap();
        let receipt = loop {
            let r = t.node.operation(r.operation.clone()).await.unwrap();
            if r.state.is_finished() {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let Observed::Finished {
            result: Some(OperationResult::Workspace(v)),
            ..
        } = receipt.state.clone()
        else {
            panic!("{:?}", receipt.state)
        };
        let request = serde_json::to_value(&c).unwrap();
        t.store
            .transaction::<_, StoreError>(|tx| {
                rt::insert_command_in(
                    tx,
                    &rt::NewCommand {
                        operation: op,
                        node: "local",
                        method: "prepare_workspace",
                        request: &request,
                        digest: "d",
                        target_attempt: None,
                        dependency: None,
                    },
                    1,
                )?;
                rt::merge_receipt_in(
                    tx,
                    op,
                    ReceiptState::Finished,
                    true,
                    &serde_json::to_value(&receipt).unwrap(),
                    2,
                )?;
                Ok(())
            })
            .unwrap();
        v
    }

    fn local() -> NodeId {
        NodeId::new("local").unwrap()
    }

    fn write(w: &WorkspaceRef, rel: &str) -> PermissionRequest {
        let exec = ExecutionRef::Turn {
            session: SessionId::new("s1").unwrap(),
            turn: TurnId::new("t1").unwrap(),
            attempt: AttemptId::new("a1").unwrap(),
        };
        let mut r = build::permission("p1", exec, "x");
        r.action = Action::Write {
            paths: vec![PathRef {
                workspace: w.clone(),
                rel: RelPath::new(rel).unwrap(),
            }],
            change_summary: "edit".into(),
            change: None,
        };
        r
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_scratch_workspace_is_owned_and_its_paths_are_not_the_sessions_cwd() {
        let t = t();
        let v = prepare(&t, "op_s", PrepareKind::Scratch).await;
        let dir = prepared_dir(&t.store, Some(&t.node), &v.workspace).expect("a directory");
        assert!(dir.owned);
        assert!(
            dir.path
                .starts_with(std::fs::canonicalize(t.dir.path()).unwrap())
        );
        // the session's cwd is somewhere else entirely: the guard sees the workspace's paths
        let cwd = t.dir.path().join("projects/p1");
        assert!(!dir.path.starts_with(&cwd));
        let input = yolo_input(
            Some(&v.workspace),
            Some(&dir),
            &write(&v.workspace, "src/x.rs"),
        );
        assert_eq!(input.own_dir.as_deref(), Some(dir.path.as_path()));
        assert_eq!(
            input.locations,
            vec![dir.path.join("src/x.rs").to_string_lossy().to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_adopted_directory_is_not_owned() {
        let t = t();
        let v = prepare(
            &t,
            "op_e",
            PrepareKind::Existing {
                dir: RootPath {
                    root: RootId::new("projects").unwrap(),
                    rel: RelPath::new("p1").unwrap(),
                },
            },
        )
        .await;
        let dir = prepared_dir(&t.store, Some(&t.node), &v.workspace).expect("a directory");
        assert!(!dir.owned);
        assert_eq!(
            dir.path,
            std::fs::canonicalize(t.dir.path().join("projects/p1")).unwrap()
        );
        let input = yolo_input(Some(&v.workspace), Some(&dir), &write(&v.workspace, "a"));
        assert_eq!(input.own_dir, None, "the item's cwd rule decides");
        // gone now: nothing to judge against
        std::fs::remove_dir_all(t.dir.path().join("projects/p1")).unwrap();
        assert!(prepared_dir(&t.store, Some(&t.node), &v.workspace).is_none());
    }

    fn adopt() -> PrepareKind {
        PrepareKind::Existing {
            dir: RootPath {
                root: RootId::new("projects").unwrap(),
                rel: RelPath::new("p1").unwrap(),
            },
        }
    }

    /// The adopted directory stays on disk once its root is taken away (D11): YOLO has no
    /// directory for it, and the guard asks instead of judging paths under it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_adopted_directory_whose_root_is_gone_has_no_directory_here() {
        let t = t();
        let v = prepare(&t, "op_e", adopt()).await;
        assert!(prepared_dir(&t.store, Some(&t.node), &v.workspace).is_some());
        t.node.set_roots(vec![]);
        assert!(t.dir.path().join("projects/p1").is_dir());
        let dir = prepared_dir(&t.store, Some(&t.node), &v.workspace);
        assert!(dir.is_none(), "{dir:?}");
        let input = yolo_input(
            Some(&v.workspace),
            dir.as_ref(),
            &write(&v.workspace, "src/x.rs"),
        );
        assert_eq!(input.kind.as_deref(), Some("other"));
        assert!(input.locations.is_empty());
    }

    /// A workspace the Node tidied (its journal forgot it) has no directory here, though the
    /// adopted directory is still there and the ledger still has its prepare.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tidied_workspace_has_no_directory_here() {
        let t = t();
        let v = prepare(&t, "op_e", adopt()).await;
        let c = Command {
            id: OperationId::new("op_t").unwrap(),
            issued_at: Timestamp(steve_store::now()),
            accept_before: Timestamp(steve_store::now() + 60_000),
            grant: GrantId::new("hub").unwrap(),
            body: Tidy {
                workspace: v.workspace.clone(),
                expected: v.revision.clone(),
                keep_branch: true,
            },
        };
        let r = t.node.tidy_workspace(c).await.unwrap();
        let done = loop {
            let r = t.node.operation(r.operation.clone()).await.unwrap();
            if r.state.is_finished() {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(
            matches!(done.state, Observed::Finished { error: None, .. }),
            "{:?}",
            done.state
        );
        assert!(t.dir.path().join("projects/p1").is_dir());
        assert!(prepared_dir(&t.store, Some(&t.node), &v.workspace).is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn another_nodes_or_an_unprepared_workspace_has_no_directory_here() {
        let t = t();
        let v = prepare(&t, "op_s", PrepareKind::Scratch).await;
        // the local Node is off here: even its own workspace is not judged by path
        assert!(prepared_dir(&t.store, None, &v.workspace).is_none());
        let remote = WorkspaceRef {
            node: NodeId::new("n_far").unwrap(),
            id: v.workspace.id.clone(),
        };
        assert!(prepared_dir(&t.store, Some(&t.node), &remote).is_none());
        let unknown = WorkspaceRef {
            node: local(),
            id: WorkspaceId::new("w_none").unwrap(),
        };
        assert!(prepared_dir(&t.store, Some(&t.node), &unknown).is_none());
    }

    #[test]
    fn a_display_stands_for_a_path_only_when_it_is_one() {
        assert_eq!(
            display_path("~/code/x", Some("/srv/u")),
            Some(PathBuf::from("/srv/u/code/x"))
        );
        assert_eq!(
            display_path("~", Some("/srv/u")),
            Some(PathBuf::from("/srv/u"))
        );
        assert_eq!(display_path("~other/x", Some("/srv/u")), None);
        assert_eq!(display_path("~/x", None), None);
        assert_eq!(display_path("code/x", Some("/srv/u")), None);
        assert_eq!(
            display_path("/srv/x", None).as_deref(),
            Some(Path::new("/srv/x"))
        );
    }
}

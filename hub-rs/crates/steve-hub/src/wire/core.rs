//! steve-core and steve-acp wired together:
//! - [`HubHost`]: steve-acp's [`Host`](steve_acp::Host) over steve-store; `turn_end` and `mcp_endpoint` go to the
//!   core. Lock order is core → acp: the runner calls the store-only methods under its own lock, never the core.
//! - [`AcpRuntime`]: steve-core's [`AgentRuntime`](steve_core::AgentRuntime) over [`Acp`].
//! - [`CoreDomain`]: the HTTP layer's [`Domain`] over [`Core`]. The core's mutating calls are synchronous (and
//!   may run git under its lock), so they run on the blocking pool.
//! - [`CoreMcp`]: steve-mcp's backend, the five tools → `core.tool_*`.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

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
                    "出了点问题，请稍后再试".into()
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

/// Runs a synchronous core call on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> BResult<T> {
    tokio::task::spawn_blocking(f)
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
    /// The first `user` event of the turn being started, for `agent_turns.user_seq`.
    pub user_seqs: super::acp_stats::UserSeqs,
}

impl HubHost {
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
        match self.store.events().add(session_id, kind, data) {
            Ok(seq) if kind == "user" => self.user_seqs.user_event(session_id, seq),
            Ok(_) => {}
            Err(e) => tracing::error!("event {session_id} {kind}: {e}"),
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
    fn cancel(&self, session_id: &str) -> bool {
        self.0.cancel(session_id)
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

// ── the HTTP layer's Domain ──

pub struct CoreDomain {
    pub core: Core,
    pub acp: Acp,
}

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
            self.acp.hold(true);
        } else if action == "cancel" {
            // If adapters were already quiesced, their groups must really be gone before reopen.
            if !self.acp.reopen() && (!self.acp.quiesce().await || !self.acp.reopen()) {
                return Err(BackendError::msg(
                    "智能体进程组尚未退出，维护状态保留，请重试解除",
                ));
            }
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
        // a turn that only waits for your permission answer doesn't hold a drain: it is cut, the
        // restart resumes it (it asks again)
        let acp = self.acp.working_turns();
        let source_release_busy = self.core.source_release_busy();
        let quiet =
            active && running.values().sum::<usize>() == 0 && acp == 0 && !source_release_busy;
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
        let sealed =
            quiet && adapters_stopped && self.acp.working_turns() == 0 && store.maintenance_seal();
        Ok(json!({"maintenance":active,"ready":sealed,"running":running,"acp":acp,"adapters_stopped":adapters_stopped,"unknown_mail":unknown,"source_release_busy":source_release_busy}).as_object().unwrap().clone())
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
        let (research_stopped, ()) = tokio::join!(
            self.core.shutdown_research(Duration::from_secs(5)),
            self.acp.shutdown(Duration::from_millis(500)),
        );
        if !research_stopped {
            tracing::error!("research process groups did not confirm shutdown before deadline");
        }
    }
    fn spawn_background(&self) -> bool {
        tokio::spawn(self.core.clone().watch_releases());
        tokio::spawn(self.core.clone().run_housekeeping());
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
        Ok(self.acp.cancel(session))
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
        self.acp.is_busy(session)
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
            .ok_or_else(|| BackendError::not_found("没有这个 Bot"))?;
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
            message: "参数不对：to".into(),
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
            self.core
                .gateway()
                .call(prompt, t, Duration::from_millis(timeout_ms), &[]),
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

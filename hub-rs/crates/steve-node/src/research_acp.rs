//! Research's ACP policy and budgets. Read-only enforcement relies on the OS sandbox,
//! `PermissionMode::Plan`, declarative allowlisting limited to Steve's read-only research MCP
//! tools, and unconditional refusal of every other permission request in one-shot ACP calls.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use tokio::time::Instant as IdleInstant;

use serde_json::{Value, json};
use steve_acp::StateDirRuntime;

use super::*;
use crate::acp_job;

pub(super) fn sandbox_mode(
    requirement: &SandboxRequirement,
    properties: &steve_harness_spec::AdapterProperties,
) -> ApiResult<r::SandboxMode> {
    let only_fs = |guarantees: &[String]| guarantees.iter().all(|g| g == FS_READONLY);
    if properties.self_read_only {
        match requirement {
            SandboxRequirement::None => return Ok(r::SandboxMode::Off),
            SandboxRequirement::Required { guarantees } if only_fs(guarantees) => {
                return Ok(r::SandboxMode::Off);
            }
            SandboxRequirement::AllowFallback {
                preferred,
                acceptable,
            } if only_fs(preferred) && acceptable.is_empty() => {
                return Ok(r::SandboxMode::Off);
            }
            _ => {}
        }
    }
    if !properties.nested_sandbox && *requirement != SandboxRequirement::None {
        return Err(Error::new(
            Code::Unsupported,
            "adapter cannot provide the requested research sandbox",
        ));
    }
    let unsupported = |why: &str| Err(Error::new(Code::Unsupported, why));
    match requirement {
        SandboxRequirement::None => Ok(r::SandboxMode::Off),
        SandboxRequirement::Required { guarantees } if guarantees.is_empty() => {
            Ok(r::SandboxMode::Off)
        }
        SandboxRequirement::Required { guarantees } if only_fs(guarantees) => {
            if steve_confine::available() {
                Ok(r::SandboxMode::Required)
            } else {
                unsupported("no read-only system sandbox on this node")
            }
        }
        SandboxRequirement::AllowFallback {
            preferred,
            acceptable,
        } if only_fs(preferred) && acceptable.is_empty() => Ok(r::SandboxMode::Auto),
        _ => unsupported("this node's research sandbox only gives fs-readonly"),
    }
}

#[derive(Default)]
struct State {
    message: Option<String>,
    text: String,
    previous: String,
    turns: u32,
    calls: HashSet<String>,
    files: Vec<String>,
    stop: Option<r::RunStatus>,
    output: u64,
    last_event: Option<IdleInstant>,
    session: Option<String>,
}

impl State {
    fn event(&mut self, event: &EventBody, budget: r::Budget) {
        match event {
            EventBody::MessageStarted {
                message,
                role: Role::Assistant,
                ..
            } => {
                self.message = Some(message.clone());
                if !self.text.trim().is_empty() {
                    self.previous = std::mem::take(&mut self.text);
                }
                self.text.clear();
                self.turns += 1;
                if budget.max_turns.is_some_and(|max| self.turns > max) {
                    self.stop = Some(r::RunStatus::Budget(r::BudgetHit::Turns));
                }
            }
            EventBody::TextDelta {
                message,
                utf8_offset,
                text,
                ..
            } if self.message.as_deref() == Some(message) => {
                let offset = *utf8_offset as usize;
                if offset <= self.text.len() && self.text.is_char_boundary(offset) {
                    self.text.truncate(offset);
                    self.text.push_str(text);
                }
            }
            EventBody::ToolChanged { tool } => {
                if self.stop.is_some() {
                    return;
                }
                self.calls.insert(tool.id.as_str().to_string());
                if budget
                    .max_tools
                    .is_some_and(|max| self.calls.len() > max as usize)
                {
                    self.stop = Some(r::RunStatus::Budget(r::BudgetHit::Tools));
                    return;
                }
                if tool.phase == ToolPhase::Succeeded
                    && tool.category == ToolCategory::Read
                    && let Some(target) = &tool.target
                    && !target.is_empty()
                    && !self.files.contains(target)
                {
                    self.files.push(target.clone());
                }
            }
            _ => {}
        }
    }

    fn raw(&self) -> String {
        if self.text.trim().is_empty() {
            self.previous.clone()
        } else {
            self.text.clone()
        }
    }
}

struct Prepared {
    cwd: PathBuf,
    scope: r::Scope,
    prompt: Vec<Value>,
    sandbox: Option<acp_job::ReadonlySandbox>,
}

async fn prepare(
    cfg: &r::ResearchConfig,
    req: &r::ResearchRequest,
    plan: &r::AttemptPlan,
    run_id: &str,
    adapter_state: &[PathBuf],
    confine: Option<&str>,
) -> std::io::Result<Prepared> {
    let dir = cfg.scratch.join(run_id);
    tokio::fs::create_dir_all(dir.join("tmp")).await?;
    let dir = r::guard::resolve(&dir);
    let files_dir = cfg.files_root.join(run_id);
    let files = r::attach::prepare(&files_dir, &req.attachments).await?;
    let cwd = req.cwd.clone().unwrap_or_else(|| dir.clone());
    let mut readable = req.add_dirs.clone();
    readable.push(dir.clone());
    readable.extend(files.dir);
    let scope = r::Scope::new(&cwd, &readable, &cfg.home);
    let sandboxed = steve_confine::decide(cfg.sandbox)
        .ok_or_else(|| std::io::Error::other("research requires an unavailable system sandbox"))?;
    let sandbox = if sandboxed {
        // The run's directory and the harness's own state. Project and attachment paths never
        // become writable simply because an adapter chose to run a command.
        let writable: Vec<PathBuf> = std::iter::once(dir.clone())
            .chain(adapter_state.iter().map(|p| r::guard::resolve(p)))
            .collect();
        if cfg!(target_os = "macos") {
            let sb = steve_confine::readonly_profile(&writable)
                .ok_or_else(|| std::io::Error::other("unusable research sandbox path"))?;
            // and the control plane out of reach, after the profile's own allows ([`crate::confine`])
            let sb = format!("{sb}{}", confine.unwrap_or_default());
            let path = dir.join("sandbox.sb");
            tokio::fs::write(&path, sb).await?;
            Some(acp_job::ReadonlySandbox::Seatbelt(path))
        } else {
            Some(acp_job::ReadonlySandbox::Bwrap {
                dir: dir.clone(),
                writable,
            })
        }
    } else {
        None
    };
    let mut prompt =
        vec![json!({"type": "text", "text": format!("{}{}", plan.prompt, files.note)})];
    for image in files.images {
        prompt.push(json!({
            "type": "image",
            "mimeType": image["source"]["media_type"],
            "data": image["source"]["data"],
        }));
    }
    Ok(Prepared {
        cwd,
        scope,
        prompt,
        sandbox,
    })
}

fn outcome(run_id: String, model: String, status: r::RunStatus) -> r::ResearchOutcome {
    r::ResearchOutcome {
        run_id,
        status,
        model: Some(model),
        session: None,
        files_read: vec![],
        turns: 0,
        ms: 0,
        first_text_ms: None,
        sandboxed: false,
        raw: String::new(),
    }
}

impl LocalNode {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn research_acp_attempt(
        &self,
        op: &OperationId,
        exec: &ExecutionRef,
        spec: &JobSpec,
        cfg: &r::ResearchConfig,
        req: &r::ResearchRequest,
        plan: &r::AttemptPlan,
        spawned: &(dyn Fn(u32) -> bool + Send + Sync),
        budget_epoch: u64,
    ) -> (r::ResearchOutcome, Option<Stop>, Vec<(Subject, EventBody)>) {
        // The idle budget and its polling interval must share the runtime's monotonic clock.
        // Real elapsed time below is only for reporting actual execution duration.
        let (started, idle_started) = (Instant::now(), IdleInstant::now());
        let run_id = workspace::dir_name(exec.attempt().as_str());
        let mut out = outcome(run_id.clone(), plan.model.clone(), r::RunStatus::Done);
        let prepared = match self.prepare_attempt(cfg, req, plan, &run_id).await {
            Ok(p) => p,
            Err(e) => {
                out.status = r::RunStatus::Failed(format!("research ACP setup: {e}"));
                let _ = tokio::fs::remove_dir_all(cfg.files_root.join(&run_id)).await;
                return (out, None, vec![]);
            }
        };
        out.sandboxed = prepared.is_sandboxed(self, req);
        let budget = cfg.budget;
        let state = Mutex::new(State {
            session: plan.resume.clone(),
            ..State::default()
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let properties = self.0.cfg.acp.adapter_properties(&req.harness);
        let wrap = self.research_wrap(properties, prepared.sandbox.as_ref(), cfg, &run_id);
        let mut output = prepared.output(self, out.first_text_ms);
        let cancel = tokio::sync::Notify::new();
        let mut run = Box::pin(
            ResearchCall {
                node: self,
                exec,
                spec,
                req,
                plan,
                budget,
                state: &state,
                cwd: &prepared.cwd,
                prompt: prepared.prompt,
                wrap: &wrap,
            }
            .run(spawned, &cancel, tx),
        );
        let mut tick = tokio::time::interval(POLL);
        let mut why = None;
        let mut failure = None;
        let result = loop {
            tokio::select! {
                biased;
                _ = tick.tick() => {
                    let state = state.lock().unwrap();
                    if let Some(stop) = self.stop_now(op, &spec.run.limits, state.output, budget_epoch) {
                        why = Some(stop);
                        break None;
                    }
                    if state.last_event.unwrap_or(idle_started).elapsed() >= budget.idle {
                        failure = Some(r::RunStatus::Budget(r::BudgetHit::Idle));
                        break None;
                    }
                }
                Some(body) = rx.recv() => {
                    if let Err(e) = output.receive(self, exec, &state, budget, started, body).await {
                        failure = Some(r::RunStatus::Failed(e.to_string()));
                        break None;
                    }
                    if state.lock().unwrap().stop.is_some() {
                        break None;
                    }
                    if let Some(stop) = self.stop_now(op, &spec.run.limits, state.lock().unwrap().output, budget_epoch) {
                        why = Some(stop);
                        break None;
                    }
                }
                result = &mut run => break Some(result),
            }
        };
        // Stopping execution must not drop the sampler. Signal once and await the same
        // attempt's bounded cleanup; this drives no new tool or model work after the signal.
        if result.is_none() {
            cancel.notify_one();
            let _ = (&mut run).await;
        }
        // Includes the Final produced during cancellation, before AttemptEnded is journaled.
        while let Ok(body) = rx.try_recv() {
            let _ = output
                .receive(self, exec, &state, budget, started, body)
                .await;
        }
        drop(run);
        if why.is_none() && state.lock().unwrap().output > spec.run.limits.max_output_bytes {
            why = Some(Stop::Output);
        }
        let state = state.into_inner().unwrap();
        apply_outcome(&mut out, state, failure, &mut why, result);
        out.first_text_ms = output.first_text_ms;
        out.ms = started.elapsed().as_millis() as u64;
        let tail = output.finish(self, exec, &mut out.raw).await;
        (out, why, tail)
    }

    /// Resolve declared state directories before preparing the scratch, attachments and sandbox.
    async fn prepare_attempt(
        &self,
        cfg: &r::ResearchConfig,
        req: &r::ResearchRequest,
        plan: &r::AttemptPlan,
        run_id: &str,
    ) -> std::io::Result<Prepared> {
        // the harness's own state, where it declares one (its configuration, logins, history)
        let state_dirs = self
            .0
            .cfg
            .acp
            .adapter_properties(&req.harness)
            .state
            .map(|st| st.ambient_dirs(&self.0.cfg.acp.user_home))
            .unwrap_or_default();
        prepare(
            cfg,
            req,
            plan,
            run_id,
            &state_dirs,
            self.0.confine.seatbelt_rules(),
        )
        .await
    }

    /// Retain the adapter's sandbox policy and scope TMPDIR to this execution.
    fn research_wrap<'a>(
        &'a self,
        properties: steve_harness_spec::AdapterProperties,
        sandbox: Option<&'a acp_job::ReadonlySandbox>,
        cfg: &r::ResearchConfig,
        run_id: &str,
    ) -> impl Fn(
        steve_acp::oneshot::Command,
        &mut steve_acp::oneshot::Env,
    ) -> steve_acp::oneshot::Command
    + Send
    + Sync
    + 'a {
        // Even FullAccess jobs get their own temporary directory. A tool honoring TMPDIR creates
        // images inside this execution's grant, rather than in a shared /tmp that cannot be scoped.
        let temp = r::guard::resolve(&cfg.scratch.join(run_id).join("tmp"));
        move |command, env: &mut steve_acp::oneshot::Env| {
            let base_wrap = acp_job::wrap(
                sandbox,
                properties.nested_sandbox.then_some(&self.0.confine),
                None,
                &self.0.cfg.acp.user_home,
                &properties.outer_sandbox_args,
            );

            let command = base_wrap(command, env);
            env.push(("TMPDIR".into(), temp.clone().into_os_string()));
            command
        }
    }
}

impl Prepared {
    /// A self-sandboxing adapter counts as sandboxed without a host wrapper.
    fn is_sandboxed(&self, node: &LocalNode, req: &r::ResearchRequest) -> bool {
        self.sandbox.is_some()
            || node
                .0
                .cfg
                .acp
                .adapter_properties(&req.harness)
                .self_read_only
    }
    /// Capture the prepared source grants once and initialize this attempt's streamed output.
    fn output(&self, node: &LocalNode, first_text_ms: Option<u64>) -> ResearchOutput {
        let image_scope = crate::inline_images::Scope::new(
            self.scope.cwd.clone(),
            self.scope.readable.clone(),
            crate::inline_images::protected(&node.0.cfg.acp.steve_home, &node.0.cfg.home),
        );
        ResearchOutput {
            images: crate::inline_images::Images::default(),
            scope: image_scope,
            first_text_ms,
            open_messages: vec![],
            assistant_messages: HashSet::new(),
            pending_closing: vec![],
        }
    }
}

/// Exact read-only Steve MCP tools exposed to a research job (`ResearchJob::steve_tools`).
/// Mutating tools (such as `steve_onboard_*` or session action tools) are excluded; the Hub
/// remains authoritative for workspace and conversation scoping on every call.
const RESEARCH_READ_ONLY_STEVE_TOOLS: &[&str] = &[
    "steve_knowledge_search",
    "steve_knowledge_read",
    "steve_debug",
    "steve_items_search",
    "steve_item_read",
    "steve_sessions_list",
    "steve_bots_list",
    "steve_nodes_list",
    "steve_im_thread",
    "steve_im_media",
];

/// Prepared inputs for one ACP call; no polling or stop policy lives in the transport.
struct ResearchCall<'a> {
    node: &'a LocalNode,
    exec: &'a ExecutionRef,
    spec: &'a JobSpec,
    req: &'a r::ResearchRequest,
    plan: &'a r::AttemptPlan,
    budget: r::Budget,
    state: &'a Mutex<State>,
    cwd: &'a Path,
    prompt: Vec<Value>,
    wrap: steve_acp::oneshot::Wrap<'a>,
}

impl ResearchCall<'_> {
    /// Wire policy callbacks and the prepared request to the shared cancellable transport.
    async fn run(
        self,
        spawned: &(dyn Fn(u32) -> bool + Send + Sync),
        cancel: &tokio::sync::Notify,
        tx: mpsc::UnboundedSender<EventBody>,
    ) -> Result<steve_acp::oneshot::Outcome, acp_job::OneShotError> {
        let Self {
            node,
            exec,
            spec,
            req,
            plan,
            budget,
            state,
            cwd,
            prompt,
            wrap,
        } = self;
        let event = |event: EventBody| {
            let _ = tx.send(event);
        };
        let session = |session: &str| {
            state.lock().unwrap().session = Some(session.into());
        };
        let activity = || {
            state.lock().unwrap().last_event = Some(IdleInstant::now());
        };
        let mcp = research_mcp(req);
        acp_job::run_cancellable(
            acp_job::Request {
                cfg: &node.0.cfg.acp,
                harness: &req.harness,
                cwd,
                model: (!plan.model.is_empty()).then_some(plan.model.as_str()),
                effort: req
                    .effort
                    .as_deref()
                    .and_then(steve_node_api::ReasoningEffort::parse)
                    .unwrap_or_default(),
                resume: plan.resume.as_deref(),
                head: None,
                tools_off: false,
                mode: spec.run.policy.permissions,
                prompt,
                mcp_servers: mcp,
                wrap: Some(wrap),
                timeout: budget.hard.unwrap_or(Duration::from_secs(86400)),
                prefix: exec.attempt().as_str(),
                on_session: Some(&session),
                on_activity: Some(&activity),
                max_output_bytes: spec.run.limits.max_output_bytes,
                emit_thoughts: true,
                allow_steve_mcp_tools: if req.mcp.is_some() {
                    RESEARCH_READ_ONLY_STEVE_TOOLS
                } else {
                    &[]
                },
            },
            spawned,
            &event,
            cancel.notified(),
        )
        .await
    }
}

/// Describe only the research job's own Steve MCP grant.
fn research_mcp(req: &r::ResearchRequest) -> Value {
    req.mcp.as_ref().map_or_else(
        || json!([]),
        |m| {
            json!([{ "type": "http", "name": "steve", "url": m.url,
            "headers": [{ "name": "Authorization", "value": format!("Bearer {}", m.token) }] }])
        },
    )
}

/// Convert the settled ACP call and policy facts without hiding any pending closing events.
fn apply_outcome(
    out: &mut r::ResearchOutcome,
    state: State,
    failure: Option<r::RunStatus>,
    why: &mut Option<Stop>,
    result: Option<Result<steve_acp::oneshot::Outcome, acp_job::OneShotError>>,
) {
    out.turns = state.turns;
    out.files_read = state.files.clone();
    out.raw = state.raw();
    out.session = state.session;
    out.status = if let Some(status) = state.stop.or(failure) {
        status
    } else if why.is_some() {
        r::RunStatus::Stopped
    } else {
        match result {
            Some(Ok(done)) => {
                out.raw = done.text;
                out.session = Some(done.session);
                match done.stop_reason.as_str() {
                    "cancelled" => r::RunStatus::Stopped,
                    "max_tokens" | "max_turn_requests" => r::RunStatus::Budget(r::BudgetHit::Turns),
                    _ => r::RunStatus::Done,
                }
            }
            Some(Err(acp_job::OneShotError::Timeout(_))) => {
                r::RunStatus::Budget(r::BudgetHit::Time)
            }
            Some(Err(acp_job::OneShotError::OutputLimit)) => {
                *why = Some(Stop::Output);
                r::RunStatus::Stopped
            }
            Some(Err(acp_job::OneShotError::ToolRefused)) => {
                r::RunStatus::Violated(r::Violation::Tool {
                    tool: "permission_refused".into(),
                })
            }
            Some(Err(e)) => r::RunStatus::Failed(e.to_string()),
            None => r::RunStatus::Stopped,
        }
    };
}

struct ResearchOutput {
    images: crate::inline_images::Images,
    scope: crate::inline_images::Scope,
    first_text_ms: Option<u64>,
    open_messages: Vec<String>,
    assistant_messages: HashSet<String>,
    pending_closing: Vec<(Subject, EventBody)>,
}

impl ResearchOutput {
    fn track_message(&mut self, body: &EventBody, elapsed_ms: u64) {
        match body {
            EventBody::MessageStarted { message, role, .. } => {
                if !self.open_messages.contains(message) {
                    self.open_messages.push(message.clone());
                }
                if *role == Role::Assistant {
                    self.assistant_messages.insert(message.clone());
                }
            }
            EventBody::MessageCompleted { message } => {
                self.open_messages.retain(|m| m != message);
            }
            EventBody::TextDelta { message, .. }
                if self.first_text_ms.is_none() && self.assistant_messages.contains(message) =>
            {
                self.first_text_ms = Some(elapsed_ms);
            }
            _ => {}
        }
    }

    fn take_unclosed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.open_messages)
    }

    /// Account an incoming event before capturing its images and journaling its closing facts.
    async fn receive(
        &mut self,
        node: &LocalNode,
        exec: &ExecutionRef,
        state: &Mutex<State>,
        budget: r::Budget,
        started: Instant,
        body: EventBody,
    ) -> ApiResult<()> {
        let original = vec![(Subject::Execution(exec.clone()), body.clone())];
        {
            let mut state = state.lock().unwrap();
            state.last_event = Some(IdleInstant::now());
            state.output += output_bytes(&original);
            state.event(&body, budget);
        }
        self.track_message(&body, started.elapsed().as_millis() as u64);
        let closing_fact = !crate::journal::needs_budget(&body);
        let evs = self
            .images
            .process(&self.scope, exec, node.hub_files(), vec![body])
            .await
            .into_iter()
            .map(|body| (Subject::Execution(exec.clone()), body))
            .collect::<Vec<_>>();
        let result = node.emit(evs.clone());
        if closing_fact && result.is_err() {
            self.pending_closing.extend(evs);
        }
        result
    }

    /// Capture the final block and return closing events before the caller removes source files.
    async fn finish(
        mut self,
        node: &LocalNode,
        exec: &ExecutionRef,
        raw: &mut String,
    ) -> Vec<(Subject, EventBody)> {
        // Capture the final open block before run_research removes scratch and attachment dirs.
        let captured = self
            .images
            .capture(&self.scope, exec, node.hub_files(), true)
            .await;
        *raw = self.images.rewrite(raw);
        let open_messages = self.take_unclosed();
        let mut tail = self.pending_closing;
        tail.extend(
            captured
                .into_iter()
                .map(|body| (Subject::Execution(exec.clone()), body)),
        );
        tail.extend(open_messages.into_iter().map(|message| {
            (
                Subject::Execution(exec.clone()),
                EventBody::MessageCompleted { message },
            )
        }));
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_event(
        id: &str,
        category: ToolCategory,
        phase: ToolPhase,
        target: Option<&str>,
    ) -> EventBody {
        EventBody::ToolChanged {
            tool: ToolView {
                id: CallId::new(id).unwrap(),
                parent: None,
                origin: ToolOrigin::Harness,
                name: "tool".into(),
                category,
                title: "tool".into(),
                description: None,
                target: target.map(str::to_string),
                phase,
                action: None,
                evidence: None,
                result: vec![],
                error: None,
                duration_ms: None,
            },
        }
    }

    #[test]
    fn tool_budget_and_read_file_tracking_run_on_tool_events() {
        let two = r::Budget {
            max_tools: Some(2),
            ..r::Budget::default()
        };
        let mut state = State::default();
        state.event(
            &tool_event(
                "a",
                ToolCategory::Read,
                ToolPhase::Succeeded,
                Some("src/lib.rs"),
            ),
            two,
        );
        state.event(
            &tool_event(
                "b",
                ToolCategory::Search,
                ToolPhase::Succeeded,
                Some("fn add"),
            ),
            two,
        );
        assert!(state.stop.is_none());
        assert_eq!(state.files, vec!["src/lib.rs"]);
        state.event(
            &tool_event(
                "c",
                ToolCategory::Read,
                ToolPhase::Pending,
                Some("src/main.rs"),
            ),
            two,
        );
        assert!(matches!(
            state.stop,
            Some(r::RunStatus::Budget(r::BudgetHit::Tools))
        ));
    }

    #[test]
    fn sandbox_policy_uses_adapter_capabilities_instead_of_names() {
        let own = steve_harness_spec::AdapterProperties {
            self_read_only: true,
            nested_sandbox: false,
            ..Default::default()
        };
        let required = SandboxRequirement::Required {
            guarantees: vec![FS_READONLY.into()],
        };
        let preferred = SandboxRequirement::AllowFallback {
            preferred: vec![FS_READONLY.into()],
            acceptable: vec![],
        };
        assert_eq!(sandbox_mode(&required, &own).unwrap(), r::SandboxMode::Off);
        assert_eq!(sandbox_mode(&preferred, &own).unwrap(), r::SandboxMode::Off);
        let plain = steve_harness_spec::AdapterProperties {
            self_read_only: false,
            nested_sandbox: true,
            ..Default::default()
        };
        assert_eq!(
            sandbox_mode(&preferred, &plain).unwrap(),
            r::SandboxMode::Auto
        );
        // Built-in Codex runs inside Steve's read-only OS sandbox (`nested_sandbox = true`,
        // `self_read_only = false`), following the nested path rather than `SandboxMode::Off`.
        let codex = steve_harness_spec::spec("codex", None).properties;
        assert!(codex.nested_sandbox && !codex.self_read_only);
        assert_eq!(
            sandbox_mode(&preferred, &codex).unwrap(),
            r::SandboxMode::Auto
        );
        if steve_confine::available() {
            assert_eq!(
                sandbox_mode(&required, &codex).unwrap(),
                r::SandboxMode::Required
            );
        } else {
            assert_eq!(
                sandbox_mode(&required, &codex).unwrap_err().code,
                Code::Unsupported
            );
        }
        assert_eq!(
            sandbox_mode(
                &SandboxRequirement::Required {
                    guarantees: vec!["no-network".into()]
                },
                &own
            )
            .unwrap_err()
            .code,
            Code::Unsupported,
        );
    }

    #[test]
    fn thoughts_do_not_trigger_first_text_or_turns_and_unclosed_messages_close() {
        let mut state = State::default();
        let budget = r::Budget {
            max_turns: Some(5),
            ..r::Budget::default()
        };
        let mut out = ResearchOutput {
            images: crate::inline_images::Images::default(),
            scope: crate::inline_images::Scope::new(PathBuf::from("/tmp"), vec![], vec![]),
            first_text_ms: None,
            open_messages: vec![],
            assistant_messages: HashSet::new(),
            pending_closing: vec![],
        };
        let thought_start = EventBody::MessageStarted {
            message: "thought-1".into(),
            block: "b0".into(),
            role: Role::Thought,
        };
        let thought_delta = EventBody::TextDelta {
            message: "thought-1".into(),
            block: "b0".into(),
            utf8_offset: 0,
            text: "internal reasoning".into(),
        };
        state.event(&thought_start, budget);
        state.event(&thought_delta, budget);
        out.track_message(&thought_start, 5);
        out.track_message(&thought_delta, 15);
        assert_eq!(state.turns, 0);
        assert!(state.raw().is_empty());
        assert_eq!(out.first_text_ms, None);

        let reply_start = EventBody::MessageStarted {
            message: "reply-1".into(),
            block: "b1".into(),
            role: Role::Assistant,
        };
        let reply_delta = EventBody::TextDelta {
            message: "reply-1".into(),
            block: "b1".into(),
            utf8_offset: 0,
            text: "final answer".into(),
        };
        state.event(&reply_start, budget);
        state.event(&reply_delta, budget);
        out.track_message(&reply_start, 20);
        out.track_message(&reply_delta, 42);
        assert_eq!(state.turns, 1);
        assert_eq!(state.raw(), "final answer");
        assert_eq!(out.first_text_ms, Some(42));
        assert_eq!(
            out.take_unclosed(),
            vec!["thought-1".to_string(), "reply-1".to_string()]
        );
    }
}

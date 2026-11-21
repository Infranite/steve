//! Research's ACP policy and budgets. The transport is shared with think jobs; only this module
//! decides which reported reads belong to the research scope.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use tokio::time::Instant as IdleInstant;

use serde_json::{Value, json};
use steve_acp::StateDirRuntime;
use steve_acp::normalize::{CommandFact, ToolFact};

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
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "adapter cannot provide the requested research sandbox",
        ));
    }
    let unsupported = |why: &str| Err(ApiError::new(ErrorCode::Unsupported, why));
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

/// Tools that load nothing but deferred tool schemas (Claude Code's `ToolSearch`).
const INERT_TOOLS: [&str; 1] = ["ToolSearch"];

/// The ACP kind describes what a tool does, independent of its adapter's spelling. Steve's MCP
/// tools still have an explicit allowlist because the server exposes actions as well as reads.
pub(crate) fn guard(
    fact: &ToolFact,
    scope: &r::Scope,
    steve_tools: bool,
    sandboxed_shell: bool,
) -> Result<(), r::Violation> {
    let tool = if fact.name.is_empty() {
        fact.title.clone()
    } else {
        fact.name.clone()
    };
    if fact.origin == ToolOrigin::Steve {
        let name = fact.name.strip_prefix("mcp__steve__").unwrap_or(&fact.name);
        return if steve_tools && r::guard::MCP_TOOLS.contains(&name) {
            Ok(())
        } else {
            Err(r::Violation::Tool { tool })
        };
    }
    if fact.origin == ToolOrigin::External {
        return Err(r::Violation::Tool { tool });
    }
    // a harness's own bookkeeping that touches nothing (loading deferred tool schemas)
    if INERT_TOOLS.contains(&fact.name.as_str()) {
        return Ok(());
    }
    let mut scope = scope.clone();
    if let Some(cwd) = &fact.cwd {
        r::guard::check_path(&tool, cwd, &scope)?;
        scope.cwd = r::guard::resolve(&scope.cwd.join(cwd));
    }
    // ACP adapters may describe a shell command by its purpose (Read/Search), while
    // retaining the full command input. It needs exactly the same sandbox and path
    // checks as Exec; its display title never grants a capability.
    let command_backed = fact.category == ToolCategory::Exec
        || (matches!(fact.category, ToolCategory::Read | ToolCategory::Search)
            && fact.command.is_some());
    if fact.origin == ToolOrigin::Harness && command_backed && sandboxed_shell {
        for path in &fact.paths {
            r::guard::check_path(&tool, path, &scope)?;
        }
        return match &fact.command {
            Some(CommandFact::Shell(command)) => r::guard::check_shell(command, &scope),
            Some(CommandFact::Argv(argv)) if !argv.is_empty() => {
                let program = Path::new(&argv[0]);
                if argv[0].contains('/')
                    && !program.starts_with("/usr/bin")
                    && !program.starts_with("/bin")
                {
                    r::guard::check_path(&tool, &argv[0], &scope)?;
                }
                // Preserve argument boundaries; this is inspection, never an argv-to-shell
                // conversion. Shell `-c` payloads get the same path guard as shell commands.
                for arg in argv.iter().skip(1) {
                    if !arg.starts_with('-')
                        && (arg.starts_with('/') || arg.starts_with("./") || arg.starts_with("../"))
                        && !arg.contains(char::is_whitespace)
                    {
                        r::guard::check_path(&tool, arg, &scope)?;
                    }
                    r::guard::check_shell(arg, &scope)?;
                }
                Ok(())
            }
            _ => Err(r::Violation::BadInput { tool }),
        };
    }
    if !matches!(
        fact.category,
        ToolCategory::Read | ToolCategory::Search | ToolCategory::Think
    ) || fact.command.is_some()
        || fact.url.is_some()
    {
        return Err(r::Violation::Tool { tool });
    }
    if fact.category == ToolCategory::Read && fact.paths.is_empty() {
        return Err(r::Violation::BadInput { tool });
    }
    for path in &fact.paths {
        r::guard::check_path(&tool, path, &scope)?;
    }
    if fact.category == ToolCategory::Search {
        // what the search reaches, even when an adapter supplies no locations
        if let Some(input) = fact.raw.get("rawInput") {
            r::guard::check_search(&tool, input, &scope)?;
        }
    }
    Ok(())
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
    fn tool(
        &mut self,
        fact: &ToolFact,
        scope: &r::Scope,
        budget: r::Budget,
        tools: bool,
        sandboxed_shell: bool,
        full: bool,
    ) -> bool {
        if self.stop.is_some() {
            return false;
        }
        // a run with the user's full access answers to no read-only rule
        if !full && let Err(v) = guard(fact, scope, tools, sandboxed_shell) {
            self.stop = Some(r::RunStatus::Violated(v));
            return false;
        }
        self.calls.insert(fact.raw_id.clone());
        if budget
            .max_tools
            .is_some_and(|max| self.calls.len() > max as usize)
        {
            self.stop = Some(r::RunStatus::Budget(r::BudgetHit::Tools));
            return false;
        }
        if fact.phase == ToolPhase::Succeeded {
            let paths = if fact.category == ToolCategory::Read {
                fact.paths.clone()
            } else if fact.category == ToolCategory::Exec {
                match &fact.command {
                    Some(CommandFact::Shell(command)) => {
                        r::guard::shell_read(command).into_iter().collect()
                    }
                    Some(CommandFact::Argv(argv)) => argv
                        .first()
                        .filter(|program| {
                            matches!(
                                Path::new(program).file_name().and_then(|s| s.to_str()),
                                Some("cat" | "sed" | "head" | "tail" | "nl" | "less" | "bat")
                            )
                        })
                        .and_then(|_| argv.last().filter(|last| !last.starts_with('-')))
                        .cloned()
                        .into_iter()
                        .collect(),
                    None => vec![],
                }
            } else {
                vec![]
            };
            for path in paths {
                let path = steve_acp::normalize::shown(&path, &scope.cwd);
                if !self.files.contains(&path) {
                    self.files.push(path);
                }
            }
        }
        true
    }

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
        answer: String::new(),
        offer: None,
        citations: vec![],
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
                scope: &prepared.scope,
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
            open_message: None,
            pending_closing: vec![],
        }
    }
}

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
    scope: &'a r::Scope,
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
            scope,
            prompt,
            wrap,
        } = self;
        let full = spec.run.policy.permissions == PermissionMode::FullAccess;
        let allow = |fact: &ToolFact| {
            state.lock().unwrap().tool(
                fact,
                scope,
                budget,
                req.mcp.is_some(),
                node.0
                    .cfg
                    .acp
                    .adapter_properties(&req.harness)
                    .self_read_only,
                full,
            )
        };
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
                effort: req.effort.as_deref(),
                resume: plan.resume.as_deref(),
                head: None,
                tools_off: false,
                mode: spec.run.policy.permissions,
                prompt,
                mcp_servers: mcp,
                wrap: Some(wrap),
                timeout: budget.hard.unwrap_or(Duration::from_secs(86400)),
                prefix: exec.attempt().as_str(),
                allow_tool: &allow,
                on_session: Some(&session),
                on_activity: Some(&activity),
                max_output_bytes: spec.run.limits.max_output_bytes,
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
            Some(Err(e)) => r::RunStatus::Failed(e.to_string()),
            None => r::RunStatus::Stopped,
        }
    };
}

struct ResearchOutput {
    images: crate::inline_images::Images,
    scope: crate::inline_images::Scope,
    first_text_ms: Option<u64>,
    open_message: Option<String>,
    pending_closing: Vec<(Subject, EventBody)>,
}

impl ResearchOutput {
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
        match &body {
            EventBody::MessageStarted { message, .. } => self.open_message = Some(message.clone()),
            EventBody::MessageCompleted { .. } => self.open_message = None,
            EventBody::TextDelta { .. } if self.first_text_ms.is_none() => {
                self.first_text_ms = Some(started.elapsed().as_millis() as u64);
            }
            _ => {}
        }
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
        let mut tail = self.pending_closing;
        tail.extend(
            captured
                .into_iter()
                .map(|body| (Subject::Execution(exec.clone()), body)),
        );
        tail.extend(
            self.open_message
                .map(|message| {
                    (
                        Subject::Execution(exec.clone()),
                        EventBody::MessageCompleted { message },
                    )
                })
                .into_iter(),
        );
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_acp::normalize::Normalizer;

    fn fact(update: Value) -> ToolFact {
        let mut normalizer = Normalizer::new("/", "test", Epoch::new("test").unwrap());
        normalizer.update(&update);
        normalizer.fact("t1").unwrap().clone()
    }

    #[test]
    fn generic_acp_reads_keep_scope_secrets_and_actions_separate() {
        let dir = tempfile::tempdir().unwrap();
        let scope = r::Scope::new(dir.path(), &[], &dir.path().join("home"));
        let read = |path: &str| {
            fact(json!({
                "sessionUpdate": "tool_call", "toolCallId": "t1", "name": "any_reader",
                "kind": "read", "status": "in_progress", "locations": [{"path": path}],
            }))
        };
        assert!(guard(&read("src/lib.rs"), &scope, false, false).is_ok());
        assert!(matches!(
            guard(&read(".env.local"), &scope, false, false),
            Err(r::Violation::Sensitive { .. })
        ));
        assert!(matches!(
            guard(&read("/etc/hosts"), &scope, false, false),
            Err(r::Violation::OutOfScope { .. })
        ));
        let shell = fact(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "name": "any_shell",
            "kind": "execute", "rawInput": {"command": "cat src/lib.rs"}
        }));
        assert!(matches!(
            guard(&shell, &scope, false, false),
            Err(r::Violation::Tool { .. })
        ));
        assert!(guard(&shell, &scope, false, true).is_ok());
        let secret_shell = fact(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "name": "any_shell",
            "kind": "execute", "rawInput": {"command": ["/bin/sh", "-lc", "cat ~/.ssh/id_rsa"]}
        }));
        assert!(matches!(
            guard(&secret_shell, &scope, false, true),
            Err(r::Violation::Sensitive { .. })
        ));
        let secret_program = fact(json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "execute",
            "rawInput": {"command": [scope.home.join(".ssh/run").display().to_string()]}
        }));
        assert!(matches!(
            guard(&secret_program, &scope, false, true),
            Err(r::Violation::Sensitive { .. })
        ));
        let mcp = |name: &str| {
            fact(json!({
                "sessionUpdate": "tool_call", "toolCallId": "t1", "name": name, "kind": "other"
            }))
        };
        assert!(
            guard(
                &mcp("mcp__steve__steve_knowledge_search"),
                &scope,
                true,
                false
            )
            .is_ok()
        );
        assert!(
            guard(
                &mcp("mcp__steve__steve_knowledge_search"),
                &scope,
                false,
                false
            )
            .is_err()
        );
        assert!(guard(&mcp("mcp__steve__steve_deliver"), &scope, true, false).is_err());
    }

    #[test]
    fn a_full_access_run_answers_to_no_read_only_rule_but_keeps_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let scope = r::Scope::new(dir.path(), &[], &dir.path().join("home"));
        let call = |id: &str, update: Value| {
            let mut update = update;
            update["toolCallId"] = json!(id);
            let mut normalizer = Normalizer::new("/", "test", Epoch::new("test").unwrap());
            normalizer.update(&update);
            normalizer.fact(id).unwrap().clone()
        };
        let outside = json!({
            "sessionUpdate": "tool_call", "name": "any_reader", "kind": "read",
            "status": "in_progress", "locations": [{"path": "/etc/hosts"}],
        });
        let write = json!({
            "sessionUpdate": "tool_call", "name": "any_writer", "kind": "edit",
            "status": "in_progress", "locations": [{"path": "/etc/hosts"}],
        });
        let shell = json!({
            "sessionUpdate": "tool_call", "name": "any_shell", "kind": "execute",
            "rawInput": {"command": "touch notes.md"}
        });
        let budget = r::Budget::default();
        let run = |state: &mut State, id: &str, update: &Value, budget: r::Budget, full: bool| {
            let fact = call(id, update.clone());
            state.tool(&fact, &scope, budget, false, false, full)
        };

        // read-only: the first call out of scope ends the run, and nothing after it counts
        let mut limited = State::default();
        assert!(!run(&mut limited, "a", &outside, budget, false));
        assert!(matches!(
            limited.stop,
            Some(r::RunStatus::Violated(r::Violation::OutOfScope { .. }))
        ));
        assert!(!run(&mut limited, "b", &shell, budget, false));
        assert!(limited.calls.is_empty());

        // full access: the same calls go through
        let mut full = State::default();
        for (id, update) in [("a", &outside), ("b", &write), ("c", &shell)] {
            assert!(run(&mut full, id, update, budget, true));
        }
        assert!(full.stop.is_none());
        assert_eq!(full.calls.len(), 3);

        // and a limit the owner set on tool calls still ends it
        let two = r::Budget {
            max_tools: Some(2),
            ..budget
        };
        let mut capped = State::default();
        assert!(run(&mut capped, "a", &outside, two, true));
        assert!(run(&mut capped, "b", &write, two, true));
        assert!(!run(&mut capped, "c", &shell, two, true));
        assert!(matches!(
            capped.stop,
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
        assert_eq!(
            sandbox_mode(
                &SandboxRequirement::Required {
                    guarantees: vec!["no-network".into()]
                },
                &own
            )
            .unwrap_err()
            .code,
            ErrorCode::Unsupported,
        );
    }
}

//! Research's ACP policy and budgets. The transport is shared with think jobs; only this module
//! decides which reported reads belong to the research scope.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};
use steve_acp::normalize::{CommandFact, ToolFact};

use super::*;
use crate::acp_job;

pub(super) fn sandbox_mode(
    requirement: &SandboxRequirement,
    properties: &steve_acp::AdapterProperties,
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
            if r::sandbox::available() {
                Ok(r::SandboxMode::Required)
            } else {
                unsupported("no sandbox-exec on this node")
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
    last_event: Option<Instant>,
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
    profile: Option<PathBuf>,
}

async fn prepare(
    cfg: &r::ResearchConfig,
    req: &r::ResearchRequest,
    plan: &r::AttemptPlan,
    run_id: &str,
    adapter_state: Option<&Path>,
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
    let sandboxed = r::sandbox::decide(cfg.sandbox)
        .ok_or_else(|| std::io::Error::other("research requires an unavailable system sandbox"))?;
    let profile = if sandboxed {
        // The run's directory and the harness's own state. Project and attachment paths never
        // become writable simply because an adapter chose to run a command.
        let writable: Vec<PathBuf> = std::iter::once(dir.clone())
            .chain(adapter_state.map(r::guard::resolve))
            .collect();
        let sb = r::sandbox::profile(&writable)
            .ok_or_else(|| std::io::Error::other("unusable research sandbox path"))?;
        // and the control plane out of reach, after the profile's own allows ([`crate::confine`])
        let sb = format!("{sb}{}", confine.unwrap_or_default());
        let path = dir.join("sandbox.sb");
        tokio::fs::write(&path, sb).await?;
        Some(path)
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
        profile,
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
    ) -> (r::ResearchOutcome, Option<Stop>, Vec<(Subject, EventBody)>) {
        let started = Instant::now();
        let run_id = workspace::dir_name(exec.attempt().as_str());
        let mut out = outcome(run_id.clone(), plan.model.clone(), r::RunStatus::Done);
        // the harness's own state, where it declares one (its configuration, logins, history)
        let state_dir = self
            .0
            .cfg
            .acp
            .adapter_properties(&req.harness)
            .state
            .map(|st| {
                std::env::var_os(&st.env)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.0.cfg.acp.user_home.join(&st.default))
            });
        let prepared = match prepare(
            cfg,
            req,
            plan,
            &run_id,
            state_dir.as_deref(),
            self.0.confine.seatbelt_rules(),
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                out.status = r::RunStatus::Failed(format!("research ACP setup: {e}"));
                let _ = tokio::fs::remove_dir_all(cfg.files_root.join(&run_id)).await;
                return (out, None, vec![]);
            }
        };
        out.sandboxed = prepared.profile.is_some()
            || self
                .0
                .cfg
                .acp
                .adapter_properties(&req.harness)
                .self_read_only;
        let budget = cfg.budget;
        let full = spec.run.policy.permissions == PermissionMode::FullAccess;
        let state = Mutex::new(State {
            session: plan.resume.clone(),
            ..State::default()
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        let allow = |fact: &ToolFact| {
            state.lock().unwrap().tool(
                fact,
                &prepared.scope,
                budget,
                req.mcp.is_some(),
                self.0
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
            state.lock().unwrap().last_event = Some(Instant::now());
        };
        let mcp = req.mcp.as_ref().map_or_else(
            || json!([]),
            |m| {
                json!([{ "type": "http", "name": "steve", "url": m.url,
                "headers": [{ "name": "Authorization", "value": format!("Bearer {}", m.token) }] }])
            },
        );
        let properties = self.0.cfg.acp.adapter_properties(&req.harness);
        let wrap = acp_job::wrap(
            prepared.profile.as_deref(),
            properties.nested_sandbox.then_some(&self.0.confine),
            (!full).then_some(prepared.cwd.as_path()),
        );
        let mut run = Box::pin(acp_job::run(
            acp_job::Request {
                cfg: &self.0.cfg.acp,
                harness: &req.harness,
                cwd: &prepared.cwd,
                model: (!plan.model.is_empty()).then_some(plan.model.as_str()),
                effort: req.effort.as_deref(),
                resume: plan.resume.as_deref(),
                head: None,
                tools_off: false,
                mode: spec.run.policy.permissions,
                prompt: prepared.prompt,
                mcp_servers: mcp,
                wrap: Some(&wrap),
                timeout: budget.hard.unwrap_or(Duration::from_secs(86400)),
                prefix: exec.attempt().as_str(),
                allow_tool: &allow,
                on_session: Some(&session),
                on_activity: Some(&activity),
                max_output_bytes: spec.run.limits.max_output_bytes,
            },
            spawned,
            &event,
        ));
        let mut tick = tokio::time::interval(POLL);
        let mut why = None;
        let mut open_message = None;
        let mut receive = |body: EventBody| -> ApiResult<()> {
            let evs = vec![(Subject::Execution(exec.clone()), body.clone())];
            {
                let mut state = state.lock().unwrap();
                state.last_event = Some(Instant::now());
                state.output += output_bytes(&evs);
                state.event(&body, budget);
            }
            match &body {
                EventBody::MessageStarted { message, .. } => open_message = Some(message.clone()),
                EventBody::MessageCompleted { .. } => open_message = None,
                EventBody::TextDelta { .. } if out.first_text_ms.is_none() => {
                    out.first_text_ms = Some(started.elapsed().as_millis() as u64);
                }
                _ => {}
            }
            self.emit(evs)
        };
        let mut failure = None;
        let result = loop {
            tokio::select! {
                biased;
                Some(body) = rx.recv() => {
                    if let Err(e) = receive(body) {
                        failure = Some(r::RunStatus::Failed(e.to_string()));
                        break None;
                    }
                    if state.lock().unwrap().stop.is_some() {
                        break None;
                    }
                    if let Some(stop) = self.stop_now(op, &spec.run.limits, state.lock().unwrap().output) {
                        why = Some(stop);
                        break None;
                    }
                }
                result = &mut run => break Some(result),
                _ = tick.tick() => {
                    let state = state.lock().unwrap();
                    if let Some(stop) = self.stop_now(op, &spec.run.limits, state.output) {
                        why = Some(stop);
                        break None;
                    }
                    if state.last_event.unwrap_or(started).elapsed() >= budget.idle {
                        failure = Some(r::RunStatus::Budget(r::BudgetHit::Idle));
                        break None;
                    }
                }
            }
        };
        // All updates preceding the prompt response belong to this attempt.
        while let Ok(body) = rx.try_recv() {
            let _ = receive(body);
        }
        drop(run);
        if why.is_none() && state.lock().unwrap().output > spec.run.limits.max_output_bytes {
            why = Some(Stop::Output);
        }
        let state = state.into_inner().unwrap();
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
                        "max_tokens" | "max_turn_requests" => {
                            r::RunStatus::Budget(r::BudgetHit::Turns)
                        }
                        _ => r::RunStatus::Done,
                    }
                }
                Some(Err(acp_job::OneShotError::Timeout(_))) => {
                    r::RunStatus::Budget(r::BudgetHit::Time)
                }
                Some(Err(acp_job::OneShotError::OutputLimit)) => {
                    why = Some(Stop::Output);
                    r::RunStatus::Stopped
                }
                Some(Err(e)) => r::RunStatus::Failed(e.to_string()),
                None => r::RunStatus::Stopped,
            }
        };
        out.ms = started.elapsed().as_millis() as u64;
        let tail = open_message
            .map(|message| {
                (
                    Subject::Execution(exec.clone()),
                    EventBody::MessageCompleted { message },
                )
            })
            .into_iter()
            .collect();
        (out, why, tail)
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
    fn sandbox_policy_uses_adapter_capabilities_instead_of_names() {
        let own = steve_acp::AdapterProperties {
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
        let plain = steve_acp::AdapterProperties {
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

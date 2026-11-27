//! Claude Code (`@agentclientprotocol/claude-agent-acp`) runtime adapter.
//!
//! Verified empirically against `claude` 2.1.308 + `claude-agent-acp` 0.99.1:
//! - Negotiating `clientCapabilities.subagents: {}` (WITHOUT `_meta.jetbrains.air`) selects
//!   ACP RFD #1992 mode with `perTaskStopAffordance: true` and per-child `session/cancel`.
//! - Emits `subagent_update` (`capabilities.cancel: {}`, `state: {state, stopReason?, error?}`),
//!   `session_message` (linking parent `toolu_*` `messageId` to child `sessionId`), child
//!   `config_option_update` (model/agent), and child `tool_call`/`tool_call_update` carrying
//!   `_meta.claudeCode.parentToolUseId`.
//! - Fallback mode (when `subagents` is not negotiated): emits root `Agent`/`Task` tool calls
//!   with `_meta.claudeCode.toolResponse.{isAsync, status, agentId, resolvedModel, outputFile}`
//!   and child tool calls in root session with `_meta.claudeCode.parentToolUseId`.
//! - Background tasks: `Bash` with `rawInput.run_in_background == true` (returning
//!   `_meta.claudeCode.toolResponse.backgroundTaskId` and `.output` path), `Monitor`, `TaskStop`,
//!   plus forward-compatible `async_task_*`.
//! - Usage: `PromptResponse.usage` (`Main`) + `_meta.quota.model_usage` (`All`, covers subagents)
//!   + `usage_update` with `_meta["_claude/origin"]` (`Background`).

use serde_json::{Value, json};
use steve_node_api::{
    AdapterCapabilities, BackgroundTaskCapability, EventBody, ResumeCapability, SubagentCapability,
    SubtaskOrigin, SubtaskStatus, ToolPhase, ToolView, UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, HarnessAdapter, SUBTASK_TEXT_MAX, SUBTASK_TITLE_MAX,
    handle_async_task_update, handle_rfd_child_update, handle_rfd_session_message,
    handle_rfd_subagent_update, tool_text_output,
};
use crate::normalize::{Fact, ToolFact, bound, call_id};

pub struct ClaudeCodeAdapter;

impl HarnessAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::AcpRfd,
            subagent_cancel: true,
            background_tasks: BackgroundTaskCapability::ToolFallback,
            background_task_cancel: false,
            child_tool_nesting: true,
            usage_attribution: UsageAttribution::TurnAndModelAll,
            resume: ResumeCapability::SessionLoad,
            upgrade: steve_node_api::UpgradeCapability::ManagedNpm,
            effort_mode: steve_node_api::EffortMode::AcpOption,
            verified: true,
        }
    }

    fn configure_client_capabilities(&self, caps: &mut Value) {
        if let Some(obj) = caps.as_object_mut() {
            // Standard ACP RFD #1992 subagents. Deliberately omit `_meta.jetbrains.air` because
            // claude-agent-acp treats its presence as legacy AIR draft and disables per-child cancel.
            obj.insert("subagents".into(), json!({}));
        }
    }

    fn normalize_update(
        &self,
        ctx: &mut AdapterContext<'_>,
        session_id: Option<&str>,
        update: &Value,
    ) -> AdapterOutcome {
        if ctx.is_child_session(session_id) {
            return handle_rfd_child_update(ctx, session_id.unwrap_or_default(), update);
        }
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        match kind {
            "subagent_update" => match handle_rfd_subagent_update(ctx, session_id, update) {
                Some(facts) => AdapterOutcome::Handled(facts),
                None => AdapterOutcome::Handled(vec![Fact::Nothing]),
            },
            "session_message" => match handle_rfd_session_message(ctx, update) {
                Some(facts) => AdapterOutcome::Handled(facts),
                None => AdapterOutcome::Handled(vec![Fact::Nothing]),
            },
            "async_task_started" | "async_task_progress" | "async_task_completed" => {
                match handle_async_task_update(ctx, kind, update) {
                    Some(facts) => AdapterOutcome::Handled(facts),
                    None => AdapterOutcome::Handled(vec![Fact::Nothing]),
                }
            }
            _ => AdapterOutcome::PassThrough,
        }
    }

    fn observe_tool(
        &self,
        ctx: &mut AdapterContext<'_>,
        session_id: Option<&str>,
        raw_id: &str,
        fact: &ToolFact,
        view: &mut ToolView,
    ) -> Vec<Fact> {
        let parent_tool_use_id = fact
            .raw
            .pointer("/_meta/claudeCode/parentToolUseId")
            .and_then(Value::as_str);
        view.parent = ctx
            .state
            .resolve_parent(ctx.root_session_id, session_id, parent_tool_use_id);

        // Child tool calls belong inside their parent subtask, not as new top-level subtasks.
        if view.parent.is_some() {
            return vec![];
        }

        let input = fact.input();
        let tool_resp = fact.raw.pointer("/_meta/claudeCode/toolResponse");
        let name = fact.name.as_str();

        // 1) Fallback Agent/Task tool (when ACP RFD subagents was not negotiated or legacy fallback)
        if matches!(name, "Agent" | "Task") {
            let agent_id = tool_resp
                .and_then(|r| r.get("agentId"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            // If RFD already tracks this child session id, avoid duplicating it as a tool subtask.
            if let Some(aid) = agent_id
                && ctx.state.has_rfd_subagent(aid)
            {
                return vec![];
            }
            let key = format!("tool:{raw_id}");
            let default_title = input
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if !fact.title.is_empty() && fact.title != "Task" {
                        fact.title.as_str()
                    } else {
                        "Subagent"
                    }
                });
            let is_async_launched = tool_resp
                .and_then(|r| r.get("isAsync"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && tool_resp
                    .and_then(|r| r.get("status"))
                    .and_then(Value::as_str)
                    == Some("async_launched");
            let output_file = tool_resp
                .and_then(|r| r.get("outputFile"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from);
            let subtask = ctx.state.upsert(
                &key,
                fact.call.clone(),
                SubtaskOrigin::Subagent,
                default_title,
                |tracked| {
                    let v = &mut tracked.view;
                    if let Some(desc) = input
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        v.title = bound(desc, SUBTASK_TITLE_MAX);
                    }
                    if let Some(actor) = input
                        .get("subagent_type")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.actor = Some(bound(actor, SUBTASK_TITLE_MAX));
                    }
                    if let Some(model) = tool_resp
                        .and_then(|r| r.get("resolvedModel"))
                        .or_else(|| input.get("model"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.model = Some(model.to_string());
                    }
                    if let Some(aid) = agent_id {
                        v.external_id = Some(aid.to_string());
                    }
                    if let Some(prompt) = input
                        .get("prompt")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        && v.progress.is_none()
                    {
                        v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                    }
                    v.status = match fact.phase {
                        ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
                        ToolPhase::Succeeded if is_async_launched => SubtaskStatus::Running,
                        ToolPhase::Succeeded => {
                            if let Some(out) = tool_text_output(fact)
                                && !out.trim().starts_with("Async agent launched successfully")
                            {
                                v.summary = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                            }
                            SubtaskStatus::Done
                        }
                        ToolPhase::Failed => {
                            v.error = fact
                                .error
                                .as_deref()
                                .or_else(|| tool_text_output(fact).as_deref().map(|_| ""))
                                .filter(|s| !s.is_empty())
                                .map(|e| bound(e, SUBTASK_TEXT_MAX))
                                .or_else(|| {
                                    tool_text_output(fact).map(|s| bound(&s, SUBTASK_TEXT_MAX))
                                });
                            SubtaskStatus::Failed
                        }
                    };
                },
            );
            if let Some(path) = output_file {
                ctx.state.register_alias(path, &key);
            }
            return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
        }

        // 2) Background Bash / Monitor
        let bg_id = tool_resp
            .and_then(|r| r.get("backgroundTaskId"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| extract_bg_id_from_output(fact));
        let is_bg_bash = name == "Bash"
            && (input
                .get("run_in_background")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || bg_id.is_some());
        if is_bg_bash || name == "Monitor" {
            let key = format!("bg:{raw_id}");
            let title = input
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| input.get("command").and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if !fact.title.is_empty() {
                        fact.title.as_str()
                    } else {
                        name
                    }
                });
            let output_path = extract_output_path_from_output(fact);
            let subtask = ctx.state.upsert(
                &key,
                call_id(&key),
                SubtaskOrigin::BackgroundTask,
                title,
                |tracked| {
                    let v = &mut tracked.view;
                    v.actor = Some(name.to_string());
                    if !title.is_empty() && title != "Terminal" {
                        v.title = bound(title, SUBTASK_TITLE_MAX);
                    }
                    if let Some(cmd) = input
                        .get("command")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.progress = Some(bound(cmd, SUBTASK_TEXT_MAX));
                    }
                    if let Some(id) = &bg_id {
                        v.external_id = Some(id.clone());
                    }
                    v.status = match fact.phase {
                        ToolPhase::Pending | ToolPhase::Running => SubtaskStatus::Running,
                        // Launching a background Bash succeeds immediately while the background
                        // command keeps running until its output reports exit or TaskStop runs.
                        ToolPhase::Succeeded if is_bg_bash => SubtaskStatus::Running,
                        ToolPhase::Succeeded => {
                            if let Some(out) = tool_text_output(fact) {
                                v.summary = Some(bound(out.trim(), SUBTASK_TEXT_MAX));
                            }
                            SubtaskStatus::Done
                        }
                        ToolPhase::Failed => {
                            v.error = fact.error.as_deref().map(|e| bound(e, SUBTASK_TEXT_MAX));
                            SubtaskStatus::Failed
                        }
                    };
                },
            );
            if let Some(path) = output_path {
                ctx.state.register_alias(path, &key);
            }
            return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
        }

        // 3) Read of a tracked background task's output file settles its status when exit code is seen.
        if name == "Read" && fact.phase == ToolPhase::Succeeded {
            let file_path = input
                .get("file_path")
                .or_else(|| tool_resp.and_then(|r| r.pointer("/file/filePath")))
                .and_then(Value::as_str);
            if let Some(path) = file_path
                && let Some(key) = ctx.state.lookup_key_by_external(path).or_else(|| {
                    extract_task_id_from_path(path)
                        .and_then(|id| ctx.state.lookup_key_by_external(id))
                })
            {
                let content = tool_resp
                    .and_then(|r| r.pointer("/file/content"))
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or_else(|| tool_text_output(fact));
                if let Some(text) = content
                    && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
                {
                    let (status, summary) = parse_bg_output(&text);
                    if let Some(st) = status {
                        tracked.view.status = st;
                        if tracked.view.duration_ms.is_none() {
                            tracked.view.duration_ms =
                                Some(tracked.started_at.elapsed().as_millis() as u64);
                        }
                    }
                    if !summary.is_empty() {
                        if tracked.view.status == SubtaskStatus::Failed {
                            tracked.view.error = Some(bound(&summary, SUBTASK_TEXT_MAX));
                        } else {
                            tracked.view.summary = Some(bound(&summary, SUBTASK_TEXT_MAX));
                        }
                    }
                    return vec![Fact::Event(EventBody::SubtaskChanged {
                        subtask: tracked.view.clone(),
                    })];
                }
            }
        }

        // 4) TaskStop settles a tracked background task to Cancelled.
        if name == "TaskStop" && fact.phase == ToolPhase::Succeeded {
            let target = input
                .get("task_id")
                .or_else(|| input.get("shell_id"))
                .or_else(|| input.get("id"))
                .and_then(Value::as_str);
            if let Some(tid) = target
                && let Some(key) = ctx.state.lookup_key_by_external(tid)
                && let Some(tracked) = ctx.state.subtasks.get_mut(&key)
            {
                tracked.view.status = SubtaskStatus::Cancelled;
                tracked.view.can_cancel = false;
                if tracked.view.duration_ms.is_none() {
                    tracked.view.duration_ms =
                        Some(tracked.started_at.elapsed().as_millis() as u64);
                }
                return vec![Fact::Event(EventBody::SubtaskChanged {
                    subtask: tracked.view.clone(),
                })];
            }
        }

        vec![]
    }
}

fn extract_bg_id_from_output(fact: &ToolFact) -> Option<String> {
    let text = tool_text_output(fact)?;
    let rest = text.split("background with ID:").nth(1)?;
    let id = rest
        .trim()
        .split(|c: char| c.is_whitespace() || c == '.')
        .next()?
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

fn extract_output_path_from_output(fact: &ToolFact) -> Option<String> {
    let text = tool_text_output(fact)?;
    let rest = text.split("Output is being written to:").nth(1)?;
    let path = rest.trim().split_whitespace().next()?.trim_end_matches('.');
    (!path.is_empty()).then(|| path.to_string())
}

fn extract_task_id_from_path(path: &str) -> Option<&str> {
    let file = path.rsplit('/').next()?;
    file.strip_suffix(".output")
}

fn strip_read_line_number(line: &str) -> &str {
    let trimmed = line.trim_start();
    let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &trimmed[digits..];
        if let Some(after) = rest.strip_prefix('\t').or_else(|| rest.strip_prefix('→')) {
            return after.trim();
        }
    }
    trimmed.trim()
}

fn parse_bg_output(text: &str) -> (Option<SubtaskStatus>, String) {
    let mut status = None;
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = strip_read_line_number(line);
        if let Some(rest) = trimmed
            .strip_prefix("[exited with code ")
            .and_then(|s| s.strip_suffix(']'))
        {
            status = Some(if rest.trim() == "0" {
                SubtaskStatus::Done
            } else {
                SubtaskStatus::Failed
            });
            continue;
        }
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }
    (status, lines.join("\n"))
}

// ── Local transcript history & resume lookup ──

use std::path::{Path, PathBuf};

use crate::adapter::history::{
    HistoryAdapter, HistorySession, agent_chunk, first_lines, is_steve, js_string, mtime_ms,
    read_dir_sorted, real_user_text, slice, str_field, truthy,
};

/// Claude Code's project directory name for a working directory.
pub fn claude_project(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

impl HistoryAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn key_prefix(&self) -> &'static str {
        "cc"
    }

    fn default_state_rel(&self) -> &'static str {
        ".claude"
    }

    fn list_files(&self, user_home: &Path) -> Vec<PathBuf> {
        let root = user_home.join(".claude/projects");
        let Ok(dirs) = read_dir_sorted(&root) else {
            return vec![];
        };
        dirs.into_iter()
            .flat_map(|d| {
                let dir = root.join(&d);
                read_dir_sorted(&dir)
                    .map(|fs| {
                        fs.into_iter()
                            .filter(|f| f.ends_with(".jsonl"))
                            .map(|f| dir.join(f))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    fn peek(&self, steve_home: &Path, file: &Path) -> Option<HistorySession> {
        let (mut title, mut cwd) = (String::new(), String::new());
        for l in first_lines(file, 200_000).ok()? {
            if l.is_empty() {
                continue;
            }
            let Ok(m) = serde_json::from_str::<Value>(&l) else {
                continue;
            };
            if cwd.is_empty() && truthy(m.get("cwd")) {
                cwd = js_string(&m["cwd"]);
            }
            if m.get("type").and_then(Value::as_str) == Some("user")
                && let Some(Value::String(c)) = str_field(&m, &["message", "content"])
                && real_user_text(c)
            {
                title = c.clone();
                break;
            }
        }
        if title.is_empty() || is_steve(steve_home, &cwd) {
            return None;
        }
        Some(HistorySession {
            key: format!("{}:{}", self.key_prefix(), file.display()),
            harness_id: HistoryAdapter::id(self),
            harness: self.display_name().into(),
            title: slice(&title, 80),
            cwd,
            when: mtime_ms(file)?,
            size: std::fs::metadata(file).ok()?.len(),
        })
    }

    fn parse_rows(&self, text: &str) -> Vec<(String, Value)> {
        let read = |n: &str| ["Read", "Grep", "Glob"].iter().any(|x| n.contains(x));
        let edit = |n: &str| ["Edit", "Write"].iter().any(|x| n.contains(x));
        let mut out = vec![];
        for l in text.split('\n') {
            if l.is_empty() {
                continue;
            }
            let Ok(m) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            let ty = m.get("type").and_then(Value::as_str);
            if ty == Some("user")
                && let Some(Value::String(c)) = str_field(&m, &["message", "content"])
                && real_user_text(c)
            {
                out.push(("user".into(), json!({ "text": c, "via": "me" })));
            }
            if ty == Some("assistant")
                && let Some(Value::Array(cs)) = str_field(&m, &["message", "content"])
            {
                for c in cs {
                    let cty = c.get("type").and_then(Value::as_str);
                    if cty == Some("text") && truthy(c.get("text")) {
                        out.push(agent_chunk(&js_string(&c["text"])));
                    }
                    if cty == Some("tool_use") {
                        let name = c
                            .get("name")
                            .map(js_string)
                            .unwrap_or_else(|| "undefined".into());
                        let input = c.get("input");
                        let extra = if truthy(input.and_then(|i| i.get("command"))) {
                            format!(
                                " {}",
                                slice(
                                    &js_string(
                                        &input.map(|i| i["command"].clone()).unwrap_or_default()
                                    ),
                                    120
                                )
                            )
                        } else if truthy(input.and_then(|i| i.get("file_path"))) {
                            format!(
                                " {}",
                                js_string(
                                    &input.map(|i| i["file_path"].clone()).unwrap_or_default()
                                )
                            )
                        } else {
                            String::new()
                        };
                        let kind = if read(&name) {
                            "read"
                        } else if edit(&name) {
                            "edit"
                        } else {
                            "execute"
                        };
                        let mut call = serde_json::Map::new();
                        call.insert("sessionUpdate".into(), json!("tool_call"));
                        if let Some(id) = c.get("id") {
                            call.insert("toolCallId".into(), id.clone());
                        }
                        call.insert("title".into(), json!(format!("{name}{extra}")));
                        call.insert("kind".into(), json!(kind));
                        call.insert("status".into(), json!("completed"));
                        out.push(("tool_call".into(), Value::Object(call)));
                    }
                }
            }
        }
        out
    }

    fn find_transcript(&self, state_dir: &Path, id: &str, cwd: &str) -> Option<PathBuf> {
        let projects = state_dir.join("projects");
        let name = format!("{id}.jsonl");
        let direct = projects.join(claude_project(cwd)).join(&name);
        if direct.is_file() {
            return Some(direct);
        }
        std::fs::read_dir(&projects)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path().join(&name))
            .find(|p| p.is_file())
    }

    fn recorded(&self, file: &Path) -> (Option<String>, Option<String>) {
        use std::io::{BufRead, BufReader, Read};
        let Ok(f) = std::fs::File::open(file) else {
            return (None, None);
        };
        let (mut id, mut cwd) = (None, None);
        let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        for line in BufReader::new(f.take(8 << 20)).lines().take(50) {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            id = id.or_else(|| field(&v, "sessionId"));
            cwd = cwd.or_else(|| field(&v, "cwd"));
            if id.is_some() && cwd.is_some() {
                break;
            }
        }
        (id, cwd)
    }

    fn session_id_from_file(&self, file: &Path) -> String {
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    }
}

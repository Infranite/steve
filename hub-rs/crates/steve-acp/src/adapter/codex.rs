//! Codex (`@agentclientprotocol/codex-acp`) runtime adapter.
//!
//! Verified empirically against `codex-cli` 0.171.0 with BOTH:
//! - `@agentclientprotocol/codex-acp` 2.12.0 (Steve's builtin package):
//!   - Negotiating `clientCapabilities.subagents: {}` enables ACP RFD #1992 mode:
//!     incremental `subagent_update` frames (`title`, `state`, `capabilities: {cancel: {}}` /
//!     `{cancel: null}`, `cwd`), child session `tool_call`/`tool_call_update` carrying
//!     `_meta.terminal_output_delta.data`, child `usage_update` (child context meter only),
//!     and per-child `session/cancel(childSessionId)`.
//!   - Without `subagents` capability (fallback mode): emits root `tool_call` with
//!     `rawInput: {agentThreadId, agentPath, activityKind: "started" | "completed"}` and
//!     collab `wait`/`spawn_agent`/`send_input`/`close_agent` with
//!     `{senderThreadId, receiverThreadIds, agentsStates}`.
//!   - `PromptResponse` carries both `usage` (`Main`) and `_meta.quota.model_usage` (`All`).
//! - Legacy `@zed-industries/codex-acp` 0.16.0: emits no subagent ACP frames and uses rollout
//!   fallback for usage (configured via `builtins.toml`).

use serde_json::{Value, json};
use steve_node_api::{
    AdapterCapabilities, BackgroundTaskCapability, EventBody, ResumeCapability, SubagentCapability,
    SubtaskOrigin, SubtaskStatus, ToolPhase, ToolView, UsageAttribution,
};

use crate::adapter::{
    AdapterContext, AdapterOutcome, HarnessAdapter, SUBTASK_TEXT_MAX, SUBTASK_TITLE_MAX,
    handle_rfd_child_update, handle_rfd_session_message, handle_rfd_subagent_update,
};
use crate::normalize::{Fact, ToolFact, bound, call_id};

pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            subagents: SubagentCapability::AcpRfd,
            subagent_cancel: true,
            background_tasks: BackgroundTaskCapability::None,
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
        view.parent = ctx
            .state
            .resolve_parent(ctx.root_session_id, session_id, None);
        if view.parent.is_some() {
            return vec![];
        }

        let input = fact.input();

        // Fallback path 1: codex-acp 2.12 non-RFD subagent lifecycle tool calls:
        // rawInput = { agentThreadId, agentPath, activityKind: "started" | "completed" | ... }
        if let Some(thread_id) = input
            .get("agentThreadId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if is_tracked_rfd_thread(ctx, thread_id) {
                return vec![];
            }
            let key = format!("thread:{thread_id}");
            let agent_name = input
                .get("agentPath")
                .and_then(Value::as_str)
                .and_then(|p| p.rsplit('/').find(|seg| !seg.is_empty()))
                .filter(|s| !s.is_empty());
            let title = agent_name
                .map(|n| n.replace('_', " "))
                .or_else(|| (!fact.title.is_empty()).then(|| fact.title.clone()))
                .unwrap_or_else(|| "Subagent".into());
            let activity = input
                .get("activityKind")
                .and_then(Value::as_str)
                .unwrap_or("");
            let subtask = ctx.state.upsert(
                &key,
                call_id(thread_id),
                SubtaskOrigin::Subagent,
                &title,
                |tracked| {
                    let v = &mut tracked.view;
                    v.external_id = Some(thread_id.to_string());
                    if let Some(actor) = agent_name {
                        v.actor = Some(bound(actor, SUBTASK_TITLE_MAX));
                    }
                    if let Some(model) = input
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.model = Some(model.to_string());
                    }
                    v.status = match activity {
                        "started" | "running" | "in_progress" => SubtaskStatus::Running,
                        "completed" | "done" => SubtaskStatus::Done,
                        "failed" | "error" => SubtaskStatus::Failed,
                        "cancelled" | "canceled" | "closed" => SubtaskStatus::Cancelled,
                        _ => match fact.phase {
                            ToolPhase::Failed => SubtaskStatus::Failed,
                            ToolPhase::Succeeded => SubtaskStatus::Done,
                            _ => SubtaskStatus::Running,
                        },
                    };
                },
            );
            return vec![Fact::Event(EventBody::SubtaskChanged { subtask })];
        }

        // Fallback path 2: Codex collabAgentToolCall (`spawn_agent`, `wait`, `send_input`,
        // `resume_agent`, `close_agent`) carrying `agentsStates` or `receiverThreadIds`.
        let name = if !fact.name.is_empty() {
            fact.name.as_str()
        } else {
            fact.title.as_str()
        };
        let is_collab = matches!(
            name,
            "spawn_agent"
                | "send_input"
                | "send_message"
                | "wait"
                | "wait_agent"
                | "resume_agent"
                | "close_agent"
        ) || input.get("agentsStates").is_some()
            || input.get("receiverThreadIds").is_some();
        if !is_collab {
            return vec![];
        }

        let mut facts = Vec::new();
        if let Some(states) = input.get("agentsStates").and_then(Value::as_object) {
            for (tid, st_val) in states {
                if tid.is_empty() || is_tracked_rfd_thread(ctx, tid) {
                    continue;
                }
                let key = format!("thread:{tid}");
                let status_str = st_val
                    .get("status")
                    .and_then(Value::as_str)
                    .or_else(|| st_val.as_str())
                    .unwrap_or("");
                let msg = st_val
                    .get("message")
                    .or_else(|| st_val.get("summary"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let subtask = ctx.state.upsert(
                    &key,
                    call_id(tid),
                    SubtaskOrigin::Subagent,
                    "Subagent",
                    |tracked| {
                        let v = &mut tracked.view;
                        v.external_id = Some(tid.clone());
                        if let Some(model) = input
                            .get("model")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            v.model = Some(model.to_string());
                        }
                        if let Some(prompt) = input
                            .get("prompt")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            && v.progress.is_none()
                        {
                            v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                        }
                        v.status = match status_str {
                            "completed" | "done" => SubtaskStatus::Done,
                            "errored" | "failed" | "error" => SubtaskStatus::Failed,
                            "shutdown" | "closed" | "cancelled" => SubtaskStatus::Cancelled,
                            "pending_init" | "pendingInit" => SubtaskStatus::Queued,
                            _ => SubtaskStatus::Running,
                        };
                        if let Some(m) = msg {
                            if v.status == SubtaskStatus::Failed {
                                v.error = Some(bound(m, SUBTASK_TEXT_MAX));
                            } else {
                                v.summary = Some(bound(m, SUBTASK_TEXT_MAX));
                            }
                        }
                    },
                );
                facts.push(Fact::Event(EventBody::SubtaskChanged { subtask }));
            }
        } else if name == "spawn_agent" {
            let tid = input
                .get("receiverThreadIds")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let key = tid
                .map(|t| format!("thread:{t}"))
                .unwrap_or_else(|| format!("tool:{raw_id}"));
            let id = tid.map(call_id).unwrap_or_else(|| fact.call.clone());
            let title = input
                .get("nickname")
                .or_else(|| input.get("role"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("Subagent");
            let subtask = ctx
                .state
                .upsert(&key, id, SubtaskOrigin::Subagent, title, |tracked| {
                    let v = &mut tracked.view;
                    if let Some(t) = tid {
                        v.external_id = Some(t.to_string());
                    }
                    if let Some(prompt) = input
                        .get("prompt")
                        .or_else(|| input.get("message"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        v.progress = Some(bound(prompt, SUBTASK_TEXT_MAX));
                    }
                    v.status = match fact.phase {
                        ToolPhase::Failed => SubtaskStatus::Failed,
                        _ => SubtaskStatus::Running,
                    };
                });
            facts.push(Fact::Event(EventBody::SubtaskChanged { subtask }));
        }
        facts
    }
}

fn is_tracked_rfd_thread(ctx: &AdapterContext<'_>, thread_id: &str) -> bool {
    ctx.state.subtasks.keys().any(|k| {
        k.starts_with("rfd:") && (k.ends_with(thread_id) || k == &format!("rfd:{thread_id}"))
    })
}

// ── Local transcript history, resume lookup & model probing ──

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::adapter::history::{
    HistoryAdapter, HistorySession, agent_chunk, first_lines, is_steve, js_string, mtime_ms,
    read_dir_sorted, real_user_text, slice, str_field, tool_uid,
};

fn codex_text(p: &Value) -> String {
    match p.get("content") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|c| match c.get("text") {
                None | Some(Value::Null) => String::new(),
                Some(t) => js_string(t),
            })
            .collect(),
        _ => String::new(),
    }
}

/// The ids `codex debug models` lists (`visibility: list`), in Codex's own order.
pub fn parse_codex_models(json_text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(json_text) else {
        return vec![];
    };
    let Some(list) = v.get("models").and_then(Value::as_array).or(v.as_array()) else {
        return vec![];
    };
    let mut out: Vec<(i64, String)> = list
        .iter()
        .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|m| {
            let id = m.get("slug").and_then(Value::as_str)?;
            Some((
                m.get("priority").and_then(Value::as_i64).unwrap_or(99),
                id.to_string(),
            ))
        })
        .collect();
    out.sort();
    let mut seen = HashSet::new();
    out.into_iter()
        .map(|(_, id)| id)
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

impl HistoryAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn key_prefix(&self) -> &'static str {
        "cx"
    }

    fn default_state_rel(&self) -> &'static str {
        ".codex"
    }

    fn list_files(&self, user_home: &Path) -> Vec<PathBuf> {
        fn walk(d: &Path, depth: usize, out: &mut Vec<PathBuf>) {
            if depth > 4 || !d.exists() {
                return;
            }
            for f in read_dir_sorted(d).unwrap_or_default() {
                let p = d.join(&f);
                if f.ends_with(".jsonl") {
                    out.push(p);
                } else if depth < 3 && p.is_dir() {
                    walk(&p, depth + 1, out);
                }
            }
        }
        let mut out = vec![];
        walk(&user_home.join(".codex/sessions"), 0, &mut out);
        out
    }

    fn peek(&self, steve_home: &Path, file: &Path) -> Option<HistorySession> {
        let (mut title, mut cwd) = (String::new(), String::new());
        for l in first_lines(file, 400_000).ok()? {
            if l.is_empty() {
                continue;
            }
            let Ok(m) = serde_json::from_str::<Value>(&l) else {
                continue;
            };
            let ty = m.get("type").and_then(Value::as_str);
            if ty == Some("session_meta") {
                cwd = match str_field(&m, &["payload", "cwd"]) {
                    None | Some(Value::Null) => String::new(),
                    Some(c) => js_string(c),
                };
            }
            if ty == Some("response_item")
                && let Some(p) = m.get("payload")
                && p.get("type").and_then(Value::as_str) == Some("message")
                && p.get("role").and_then(Value::as_str) == Some("user")
            {
                let t = codex_text(p);
                if real_user_text(&t) {
                    title = t;
                    break;
                }
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
        let mut out = vec![];
        for l in text.split('\n') {
            if l.is_empty() {
                continue;
            }
            let Ok(m) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            if m.get("type").and_then(Value::as_str) != Some("response_item") {
                continue;
            }
            let Some(p) = m.get("payload") else { continue };
            let pty = p.get("type").and_then(Value::as_str);
            if pty == Some("message") {
                let t = codex_text(p);
                let role = p.get("role").and_then(Value::as_str);
                if role == Some("user") && real_user_text(&t) {
                    out.push(("user".into(), json!({ "text": t, "via": "me" })));
                }
                if role == Some("assistant") && !t.is_empty() {
                    out.push(agent_chunk(&t));
                }
            }
            if pty == Some("custom_tool_call") || pty == Some("function_call") {
                let id = match p.get("call_id") {
                    None | Some(Value::Null) => json!(tool_uid()),
                    Some(v) => v.clone(),
                };
                let arg = [p.get("input"), p.get("arguments")]
                    .into_iter()
                    .flatten()
                    .find(|v| !v.is_null())
                    .map(js_string)
                    .unwrap_or_default();
                let name = p
                    .get("name")
                    .map(js_string)
                    .unwrap_or_else(|| "undefined".into());
                out.push((
                    "tool_call".into(),
                    json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": format!("{name} {}", slice(&arg, 120)), "kind": "execute", "status": "completed" }),
                ));
            }
        }
        out
    }

    fn find_transcript(&self, state_dir: &Path, id: &str, _cwd: &str) -> Option<PathBuf> {
        crate::codex_rollout(state_dir, id)
    }

    fn recorded(&self, file: &Path) -> (Option<String>, Option<String>) {
        use std::io::{BufRead, BufReader, Read};
        let Ok(f) = std::fs::File::open(file) else {
            return (None, None);
        };
        let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        for line in BufReader::new(f.take(8 << 20)).lines().take(50) {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("type").and_then(Value::as_str) == Some("session_meta")
                && let Some(p) = v.get("payload")
            {
                return (field(p, "id"), field(p, "cwd"));
            }
        }
        (None, None)
    }

    fn session_id_from_file(&self, file: &Path) -> String {
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        stem.get(stem.len().saturating_sub(36)..)
            .unwrap_or("")
            .to_string()
    }

    fn model_probe_cmd(&self) -> Option<(&'static [&'static str], &'static str)> {
        Some((&["debug", "models"], "CODEX_HOME"))
    }

    fn parse_probed_models(&self, stdout: &str) -> Vec<String> {
        parse_codex_models(stdout)
    }
}

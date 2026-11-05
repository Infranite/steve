//! ACP → Steve's events (docs/node-protocol.md §6). The one place that reads ACP's `session/update`
//! payloads (`_meta`, `rawInput`, tool content) and says what they mean in Steve's terms; whatever
//! runs a harness over ACP reports through it, and nothing past it sees ACP.
//!
//! One [`Normalizer`] per attempt: it numbers the messages and blocks of the attempt and keeps each
//! tool's current view, so a `tool_call_update` comes out as the whole tool again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use steve_node_api::{
    Action, Digest, EventBody, Measurement, Money, NetworkScope, Part, PlanEntry, Role, Severity,
    Tokens, ToolCategory, ToolOrigin, ToolPhase, ToolView, UsageReport, UsageScope,
};

/// Text kept of a tool's output or input preview (characters).
pub const TEXT_MAX: usize = 4000;
/// Lines of a diff preview.
const DIFF_LINES: usize = 60;

/// What one `session/update` says. (`Event` is the common case; boxing it buys nothing.)
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Fact {
    /// A Steve event of the running attempt.
    Event(EventBody),
    /// The model the harness runs now.
    Model(String),
    /// The reasoning effort in force now.
    Effort(String),
    /// Nothing Steve keeps (mode and command lists, replayed history markers).
    Nothing,
}

#[derive(Debug)]
struct Open {
    role: Role,
    message: String,
    block: String,
    offset: u64,
}

pub struct Normalizer {
    /// The workspace root, to show paths inside it relative.
    root: PathBuf,
    /// Unique per attempt (its id): message ids and usage report ids derive from it.
    prefix: String,
    /// The harness process generation: a restarted adapter counts its cost from zero again.
    meter: steve_node_api::Epoch,
    messages: u64,
    reports: u64,
    open: Option<Open>,
    tools: HashMap<String, ToolView>,
}

impl Normalizer {
    pub fn new(root: impl Into<PathBuf>, prefix: &str, meter: steve_node_api::Epoch) -> Self {
        Normalizer {
            root: root.into(),
            prefix: prefix.to_string(),
            meter,
            messages: 0,
            reports: 0,
            open: None,
            tools: HashMap::new(),
        }
    }

    /// The events and facts of one `session/update` (the `update` object).
    pub fn update(&mut self, u: &Value) -> Vec<Fact> {
        let kind = u.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
        match kind {
            "agent_message_chunk" => self.text(u, Role::Assistant),
            "agent_thought_chunk" => self.text(u, Role::Thought),
            "user_message_chunk" => vec![Fact::Nothing],
            "tool_call" | "tool_call_update" => {
                let mut out = self.close();
                if let Some(t) = self.tool(u, kind == "tool_call") {
                    out.push(Fact::Event(EventBody::ToolChanged { tool: t }));
                }
                out
            }
            "plan" => {
                let entries = u
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|e| PlanEntry {
                                content: str_of(e.get("content")),
                                status: str_of(e.get("status")),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                vec![Fact::Event(EventBody::Plan { entries })]
            }
            "usage_update" => vec![Fact::Event(EventBody::Usage {
                report: self.usage(u),
            })],
            "config_option_update" => {
                let mut out = vec![];
                if let Some(m) = config_value(u, |o| o["id"] == "model") {
                    out.push(Fact::Model(m));
                }
                if let Some(e) = config_value(u, is_effort) {
                    out.push(Fact::Effort(e));
                }
                if out.is_empty() {
                    out.push(Fact::Nothing);
                }
                out
            }
            "available_commands_update" | "current_mode_update" | "session_info_update" | "" => {
                vec![Fact::Nothing]
            }
            other => vec![Fact::Event(EventBody::Diagnostic {
                severity: Severity::Info,
                code: "unsupported_update".into(),
                safe_text: bound(other, 80),
            })],
        }
    }

    /// The attempt is over: close the message still open.
    pub fn finish(&mut self) -> Vec<Fact> {
        self.close()
    }

    fn close(&mut self) -> Vec<Fact> {
        match self.open.take() {
            Some(o) => vec![Fact::Event(EventBody::MessageCompleted {
                message: o.message,
            })],
            None => vec![],
        }
    }

    fn text(&mut self, u: &Value, role: Role) -> Vec<Fact> {
        let t = u
            .pointer("/content/text")
            .and_then(Value::as_str)
            .unwrap_or("");
        if t.is_empty() {
            return vec![Fact::Nothing];
        }
        let mut out = vec![];
        if self.open.as_ref().is_some_and(|o| o.role != role) {
            out.extend(self.close());
        }
        if self.open.is_none() {
            self.messages += 1;
            let message = format!("{}.m{}", self.prefix, self.messages);
            let block = format!("{message}.b1");
            out.push(Fact::Event(EventBody::MessageStarted {
                message: message.clone(),
                block: block.clone(),
                role,
            }));
            self.open = Some(Open {
                role,
                message,
                block,
                offset: 0,
            });
        }
        let o = self.open.as_mut().expect("opened above");
        out.push(Fact::Event(EventBody::TextDelta {
            message: o.message.clone(),
            block: o.block.clone(),
            utf8_offset: o.offset,
            text: t.to_string(),
        }));
        o.offset += t.len() as u64;
        out
    }

    fn usage(&mut self, u: &Value) -> UsageReport {
        self.reports += 1;
        let int = |k: &str| {
            u.get(k)
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
        };
        let origin = u
            .pointer("/_meta/_claude~1origin")
            .and_then(Value::as_str)
            .is_some();
        UsageReport {
            report_id: format!("{}.u{}", self.prefix, self.reports),
            model: u
                .pointer("/_meta/_claude~1model")
                .and_then(Value::as_str)
                .map(String::from),
            meter_epoch: self.meter.clone(),
            // work the adapter started on its own is not this attempt's
            scope: if origin {
                UsageScope::Background
            } else {
                UsageScope::Main
            },
            // the adapter reports the process's running total
            measurement: Measurement::Cumulative,
            tokens: Tokens::default(),
            context_used: int("used"),
            context_limit: int("size"),
            cost: u
                .pointer("/cost/amount")
                .and_then(Value::as_f64)
                .map(|a| Money {
                    decimal: decimal(a),
                    currency: u
                        .pointer("/cost/currency")
                        .and_then(Value::as_str)
                        .unwrap_or("USD")
                        .to_string(),
                }),
        }
    }

    /// The tool's whole view after this update (`None`: an update of a tool never announced).
    fn tool(&mut self, u: &Value, new: bool) -> Option<ToolView> {
        let id = str_of(u.get("toolCallId"));
        if id.is_empty() {
            return None;
        }
        let call = steve_node_api::CallId::new(sanitize(&id)).ok()?;
        if new || !self.tools.contains_key(&id) {
            if !new {
                return None;
            }
            let name = tool_name(u);
            let origin = if name.starts_with("mcp__steve__") || steve_title(u) {
                ToolOrigin::Steve
            } else if name.starts_with("mcp__") {
                ToolOrigin::External
            } else {
                ToolOrigin::Harness
            };
            let category = if origin == ToolOrigin::Steve {
                ToolCategory::Steve
            } else {
                category(u.get("kind").and_then(Value::as_str).unwrap_or(""))
            };
            self.tools.insert(
                id.clone(),
                ToolView {
                    id: call,
                    parent: None,
                    origin,
                    name,
                    category,
                    title: String::new(),
                    target: None,
                    phase: ToolPhase::Pending,
                    action: None,
                    result: vec![],
                    error: None,
                    duration_ms: None,
                },
            );
        }
        let root = self.root.clone();
        let t = self.tools.get_mut(&id)?;
        if let Some(title) = u.get("title").and_then(Value::as_str) {
            t.title = bound(title, 200);
        }
        if let Some(s) = u.get("status").and_then(Value::as_str) {
            t.phase = match s {
                "in_progress" => ToolPhase::Running,
                "completed" => ToolPhase::Succeeded,
                "failed" => ToolPhase::Failed,
                _ => ToolPhase::Pending,
            };
        }
        if let Some(input) = u.get("rawInput").filter(|v| v.is_object()) {
            t.action = Some(action(&t.name, t.category, input, &root));
            let diffs = input_diffs(input, &root);
            if !diffs.is_empty() {
                t.result.retain(|p| !matches!(p, Part::Diff { .. }));
                t.result.extend(diffs);
            }
        }
        let target = target(u, &root);
        if target.is_some() {
            t.target = target;
        }
        let parts = content_parts(u, &root);
        if !parts.is_empty() {
            // the adapter's diff (with whole texts) replaces the one read from the input
            if parts.iter().any(|p| matches!(p, Part::Diff { .. })) {
                t.result.retain(|p| !matches!(p, Part::Diff { .. }));
            }
            t.result.retain(|p| !matches!(p, Part::Text { .. }));
            t.result.extend(parts);
        } else if let Some(out) = u.get("rawOutput") {
            let text = match out {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            if !text.is_empty() {
                t.result.retain(|p| !matches!(p, Part::Text { .. }));
                t.result.push(Part::Text {
                    text: bound(&text, TEXT_MAX),
                });
            }
        }
        if t.phase == ToolPhase::Failed && t.error.is_none() {
            t.error = t.result.iter().find_map(|p| match p {
                Part::Text { text } => Some(bound(text, 500)),
                _ => None,
            });
        }
        Some(t.clone())
    }
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_string()
}

/// At most `max` characters, with `…` when cut.
pub fn bound(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// A tool call id as Steve's id (harness ids may hold characters ids do not allow).
fn sanitize(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if s.is_empty() { "_".into() } else { s }
}

fn decimal(a: f64) -> String {
    let s = format!("{a:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.into() }
}

fn tool_name(u: &Value) -> String {
    u.pointer("/_meta/claudeCode/toolName")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            u.get("title")
                .and_then(Value::as_str)
                .and_then(|t| t.split_whitespace().next())
                .map(String::from)
        })
        .unwrap_or_else(|| "tool".into())
}

/// Steve's tools as some adapters title them (`steve_deliver`, `mcp__steve__steve_update`).
fn steve_title(u: &Value) -> bool {
    u.get("title")
        .and_then(Value::as_str)
        .is_some_and(|t| t.contains("steve_"))
}

fn category(kind: &str) -> ToolCategory {
    match kind {
        "read" => ToolCategory::Read,
        "edit" => ToolCategory::Edit,
        "delete" => ToolCategory::Delete,
        "move" => ToolCategory::Move,
        "search" => ToolCategory::Search,
        "execute" => ToolCategory::Exec,
        "think" => ToolCategory::Think,
        "fetch" => ToolCategory::Fetch,
        _ => ToolCategory::Other,
    }
}

fn is_effort(o: &Value) -> bool {
    o.get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| ["thought_level", "reasoning_effort", "effort"].contains(&id))
        || o.get("category").and_then(Value::as_str) == Some("thought_level")
}

fn config_value(u: &Value, which: impl Fn(&Value) -> bool) -> Option<String> {
    u.get("configOptions")?
        .as_array()?
        .iter()
        .find(|o| which(o))?
        .get("currentValue")?
        .as_str()
        .map(String::from)
}

/// `path` relative to the workspace root when it is inside, as is otherwise.
pub fn shown(path: &str, root: &Path) -> String {
    let p = Path::new(path);
    match p.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into(),
        Ok(_) => ".".into(),
        Err(_) => path.to_string(),
    }
}

fn command_of(input: &Value) -> Option<String> {
    match input.get("command")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

fn path_of(input: &Value) -> Option<String> {
    ["file_path", "path", "notebook_path"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(String::from)
}

fn target(u: &Value, root: &Path) -> Option<String> {
    if let Some(p) = u.pointer("/locations/0/path").and_then(Value::as_str) {
        return Some(shown(p, root));
    }
    let input = u.get("rawInput")?;
    if let Some(p) = path_of(input) {
        return Some(shown(&p, root));
    }
    if let Some(c) = command_of(input) {
        return Some(bound(&c, 200));
    }
    ["pattern", "query", "url"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(|s| bound(s, 200))
}

fn lines(s: &str) -> u32 {
    if s.is_empty() {
        0
    } else {
        s.lines().count() as u32
    }
}

fn preview_lines(s: &str) -> String {
    let mut out: Vec<&str> = s.lines().take(DIFF_LINES).collect();
    if s.lines().count() > DIFF_LINES {
        out.push("…");
    }
    bound(&out.join("\n"), TEXT_MAX)
}

fn diff(path: &str, root: &Path, old: Option<&str>, new: &str) -> Part {
    Part::Diff {
        path: shown(path, root),
        old: old.filter(|o| !o.is_empty()).map(preview_lines),
        new: preview_lines(new),
        added: lines(new),
        removed: old.map(lines).unwrap_or(0),
    }
}

/// An edit's change as Claude Code's tools put it in their input (`Edit`, `MultiEdit`, `Write`).
fn input_diffs(input: &Value, root: &Path) -> Vec<Part> {
    let Some(path) = path_of(input) else {
        return vec![];
    };
    if let Some(edits) = input.get("edits").and_then(Value::as_array) {
        let old: Vec<&str> = edits
            .iter()
            .filter_map(|e| e.get("old_string").and_then(Value::as_str))
            .collect();
        let new: Vec<&str> = edits
            .iter()
            .filter_map(|e| e.get("new_string").and_then(Value::as_str))
            .collect();
        return vec![diff(
            &path,
            root,
            Some(&old.join("\n⋯\n")),
            &new.join("\n⋯\n"),
        )];
    }
    match (
        input.get("old_string").and_then(Value::as_str),
        input.get("new_string").and_then(Value::as_str),
        input.get("content").and_then(Value::as_str),
    ) {
        (old, Some(new), _) => vec![diff(&path, root, old, new)],
        (None, None, Some(content)) => vec![diff(&path, root, None, content)],
        _ => vec![],
    }
}

/// The tool's output as the adapter reports it: text, and diffs with whole texts.
fn content_parts(u: &Value, root: &Path) -> Vec<Part> {
    let Some(items) = u.get("content").and_then(Value::as_array) else {
        return vec![];
    };
    let mut text = String::new();
    let mut out = vec![];
    for c in items {
        match c.get("type").and_then(Value::as_str) {
            Some("content") => {
                if let Some(t) = c.pointer("/content/text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
            }
            Some("diff") => out.push(diff(
                c.get("path").and_then(Value::as_str).unwrap_or(""),
                root,
                c.get("oldText").and_then(Value::as_str),
                c.get("newText").and_then(Value::as_str).unwrap_or(""),
            )),
            _ => {}
        }
    }
    if !text.is_empty() {
        out.insert(
            0,
            Part::Text {
                text: bound(&text, TEXT_MAX),
            },
        );
    }
    out
}

/// What the call would do, structured. Paths stay out of [`Action`]'s typed fields (they need the
/// Node's workspace reference): they are in the tool's `target` and its diffs.
fn action(name: &str, cat: ToolCategory, input: &Value, root: &Path) -> Action {
    if cat == ToolCategory::Steve {
        return Action::Tool {
            name: name
                .strip_prefix("mcp__steve__")
                .unwrap_or(name)
                .to_string(),
            args: input.clone(),
        };
    }
    if let Some(c) = command_of(input).filter(|_| cat == ToolCategory::Exec || name == "Bash") {
        return Action::Exec {
            command: c,
            cwd: None,
            network: NetworkScope::Unrestricted,
        };
    }
    match cat {
        ToolCategory::Edit => Action::Write {
            paths: vec![],
            change_summary: path_of(input)
                .map(|p| format!("编辑 {}", shown(&p, root)))
                .unwrap_or_else(|| "编辑".into()),
            change: steve_node_api::digest_of(input).ok(),
        },
        ToolCategory::Search => Action::Search {
            query: ["pattern", "query"]
                .iter()
                .find_map(|k| input.get(*k).and_then(Value::as_str))
                .unwrap_or("")
                .to_string(),
        },
        ToolCategory::Fetch => Action::Fetch {
            url: input
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        ToolCategory::Read => Action::Read { paths: vec![] },
        _ => Action::Unknown {
            summary: bound(name, 80),
        },
    }
}

/// Used by tests and drivers that need a digest of something ACP-specific without exposing it.
pub fn digest_json(v: &Value) -> Option<Digest> {
    steve_node_api::digest_of(v).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn n() -> Normalizer {
        Normalizer::new(
            "/work/proj",
            "a_1",
            steve_node_api::Epoch::new("g1").unwrap(),
        )
    }

    fn events(f: Vec<Fact>) -> Vec<EventBody> {
        f.into_iter()
            .filter_map(|f| match f {
                Fact::Event(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn chunks_become_one_message_with_offsets() {
        let mut n = n();
        let a = events(n.update(&json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "你好"}})));
        let b = events(n.update(&json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "!"}})));
        assert!(matches!(
            &a[0],
            EventBody::MessageStarted {
                role: Role::Assistant,
                ..
            }
        ));
        assert!(
            matches!(&a[1], EventBody::TextDelta { utf8_offset: 0, text, .. } if text == "你好")
        );
        assert!(matches!(&b[0], EventBody::TextDelta { utf8_offset: 6, .. }));
        // a thought closes the answer and opens its own message
        let c = events(n.update(&json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "嗯"}})));
        assert!(matches!(&c[0], EventBody::MessageCompleted { message } if message == "a_1.m1"));
        assert!(
            matches!(&c[1], EventBody::MessageStarted { role: Role::Thought, message, .. } if message == "a_1.m2")
        );
        assert!(matches!(
            &events(n.finish())[0],
            EventBody::MessageCompleted { .. }
        ));
    }

    #[test]
    fn a_shell_call_and_its_update() {
        let mut n = n();
        let start = events(n.update(&json!({"sessionUpdate": "tool_call", "toolCallId": "toolu_01", "kind": "execute",
            "status": "pending", "title": "ls -la", "rawInput": {"command": "ls -la", "description": "list"},
            "_meta": {"claudeCode": {"toolName": "Bash"}}})));
        let EventBody::ToolChanged { tool } = &start[0] else {
            panic!("{start:?}")
        };
        assert_eq!(
            (tool.name.as_str(), tool.category, tool.origin),
            ("Bash", ToolCategory::Exec, ToolOrigin::Harness)
        );
        assert_eq!(tool.target.as_deref(), Some("ls -la"));
        assert!(matches!(&tool.action, Some(Action::Exec { command, .. }) if command == "ls -la"));
        let done = events(n.update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "toolu_01", "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "a\nb"}}]})));
        let EventBody::ToolChanged { tool } = &done[0] else {
            panic!()
        };
        assert_eq!(tool.phase, ToolPhase::Succeeded);
        assert_eq!(tool.name, "Bash", "the update keeps what the call said");
        assert_eq!(
            tool.result,
            vec![Part::Text {
                text: "a\nb".into()
            }]
        );
    }

    #[test]
    fn an_edit_shows_its_diff_relative_to_the_workspace() {
        let mut n = n();
        let e = events(n.update(&json!({"sessionUpdate": "tool_call", "toolCallId": "t2", "kind": "edit", "status": "pending",
            "title": "Edit /work/proj/src/a.rs", "locations": [{"path": "/work/proj/src/a.rs"}],
            "rawInput": {"file_path": "/work/proj/src/a.rs", "old_string": "x\ny", "new_string": "x\ny\nz"},
            "_meta": {"claudeCode": {"toolName": "Edit"}}})));
        let EventBody::ToolChanged { tool } = &e[0] else {
            panic!()
        };
        assert_eq!(tool.target.as_deref(), Some("src/a.rs"));
        assert!(
            matches!(&tool.result[0], Part::Diff { path, added: 3, removed: 2, .. } if path == "src/a.rs")
        );
        // outside the workspace a path is shown as it is
        assert_eq!(shown("/etc/hosts", Path::new("/work/proj")), "/etc/hosts");
    }

    #[test]
    fn steve_tools_are_steves() {
        let mut n = n();
        let e = events(n.update(&json!({"sessionUpdate": "tool_call", "toolCallId": "t3", "kind": "other", "status": "pending",
            "title": "mcp__steve__steve_deliver", "rawInput": {"summary": "done"},
            "_meta": {"claudeCode": {"toolName": "mcp__steve__steve_deliver"}}})));
        let EventBody::ToolChanged { tool } = &e[0] else {
            panic!()
        };
        assert_eq!(
            (tool.origin, tool.category),
            (ToolOrigin::Steve, ToolCategory::Steve)
        );
        assert!(matches!(&tool.action, Some(Action::Tool { name, .. }) if name == "steve_deliver"));
    }

    #[test]
    fn usage_and_config() {
        let mut n = n();
        let u = events(n.update(&json!({"sessionUpdate": "usage_update", "used": 1200, "size": 200000,
            "cost": {"amount": 0.0125, "currency": "USD"}, "_meta": {"_claude/model": "claude-sonnet-5-5"}})));
        let EventBody::Usage { report } = &u[0] else {
            panic!()
        };
        assert_eq!(
            (report.context_used, report.context_limit),
            (Some(1200), Some(200000))
        );
        assert_eq!(report.cost.as_ref().unwrap().decimal, "0.0125");
        assert_eq!(
            (report.scope, report.measurement),
            (UsageScope::Main, Measurement::Cumulative)
        );
        assert_eq!(report.model.as_deref(), Some("claude-sonnet-5-5"));
        let bg = events(n.update(&json!({"sessionUpdate": "usage_update", "used": 1, "size": 2,
            "cost": {"amount": 0.5, "currency": "USD"}, "_meta": {"_claude/origin": "task-notification"}})));
        assert!(
            matches!(&bg[0], EventBody::Usage { report } if report.scope == UsageScope::Background && report.report_id == "a_1.u2")
        );
        let c = n.update(&json!({"sessionUpdate": "config_option_update", "configOptions": [
            {"id": "model", "currentValue": "opus"}, {"id": "reasoning_effort", "currentValue": "high"}]}));
        assert_eq!(
            c,
            vec![Fact::Model("opus".into()), Fact::Effort("high".into())]
        );
    }

    #[test]
    fn unknown_updates_are_named_not_passed_through() {
        let mut n = n();
        let d = events(n.update(&json!({"sessionUpdate": "brand_new_thing", "secret": "x"})));
        assert!(
            matches!(&d[0], EventBody::Diagnostic { code, safe_text, .. } if code == "unsupported_update" && safe_text == "brand_new_thing")
        );
        assert_eq!(
            n.update(&json!({"sessionUpdate": "current_mode_update"})),
            vec![Fact::Nothing]
        );
        // an update for a tool never announced says nothing
        assert!(events(n.update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "zz", "status": "completed"}))).is_empty());
    }

    #[test]
    fn long_output_is_bounded() {
        let mut n = n();
        n.update(&json!({"sessionUpdate": "tool_call", "toolCallId": "t4", "kind": "execute", "status": "pending", "title": "cat big"}));
        let e = events(n.update(
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": "t4", "status": "failed",
            "rawOutput": "x".repeat(10_000)}),
        ));
        let EventBody::ToolChanged { tool } = &e[0] else {
            panic!()
        };
        let Part::Text { text } = &tool.result[0] else {
            panic!()
        };
        assert_eq!(text.chars().count(), TEXT_MAX + 1);
        assert!(tool.error.as_ref().unwrap().chars().count() <= 501);
    }
}

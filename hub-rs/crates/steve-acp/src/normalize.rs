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
    Action, CallId, Digest, EventBody, Measurement, Money, NetworkScope, Part, PlanEntry, Role,
    Severity, Tokens, ToolCategory, ToolOrigin, ToolPhase, ToolView, UsageReport, UsageScope,
};

/// Text kept of a tool's output or input preview (characters).
pub const TEXT_MAX: usize = 4000;
/// Lines of a diff preview.
const DIFF_LINES: usize = 60;

/// A command before the Node binds its working directory and permission policy.
/// Never flatten argv into shell text: argument boundaries are part of the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandFact {
    Shell(String),
    Argv(Vec<String>),
}

/// Driver-local tool facts. Paths, cwd, command arguments and inputs are not display previews.
/// The Node must bind these to its workspace and resource versions before authorizing an action.
/// `result` is a bounded preview; `raw` retains the merged ACP fields for lossless driver use.
/// Neither `raw` nor this structure is a Hub wire payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolFact {
    pub raw_id: String,
    pub call: CallId,
    pub name: String,
    pub category: ToolCategory,
    pub origin: ToolOrigin,
    pub title: String,
    pub phase: ToolPhase,
    /// All known paths from current locations/input/diffs, in original form. An empty locations
    /// update clears that source, not independent paths still present in rawInput or content.
    pub paths: Vec<String>,
    pub cwd: Option<String>,
    pub command: Option<CommandFact>,
    pub query: Option<String>,
    pub url: Option<String>,
    /// Canonical digest of the complete edit input (or full content diffs), never its preview.
    pub change: Option<Digest>,
    /// Bounded previews; Diff added/removed count full supplied new/old text lines, not an LCS.
    pub result: Vec<Part>,
    pub error: Option<String>,
    pub raw: Value,
}

/// Stable call identity shared by tool updates and permission requests.
/// Valid ids are unchanged except the reserved `acp-h:` namespace. That namespace (including
/// valid raw ids beginning with it) and all other inputs use a full SHA-256 digest. The two
/// encoding spaces are disjoint; hashed inputs have SHA-256's collision resistance, not a
/// mathematical injectivity guarantee for arbitrarily long strings into bounded ids.
pub fn call_id(raw: &str) -> CallId {
    if !raw.starts_with("acp-h:")
        && let Ok(id) = CallId::new(raw)
    {
        return id;
    }
    let digest = Digest::sha256(raw.as_bytes());
    CallId::new(format!("acp-h:{}", &digest.as_str()[7..])).expect("bounded ASCII hash")
}

fn diagnostic(code: &str, context: &str) -> Fact {
    Fact::Event(EventBody::Diagnostic {
        severity: Severity::Info,
        code: code.into(),
        safe_text: bound(context, 200),
    })
}

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
    tools: HashMap<String, ToolFact>,
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
                if let Some((t, diagnostics)) = self.merge_tool(u) {
                    out.push(Fact::Event(EventBody::ToolChanged { tool: t }));
                    out.extend(diagnostics);
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
        if u.pointer("/content/type").and_then(Value::as_str) != Some("text") {
            return vec![diagnostic(
                "unsupported_content",
                "message content is not text",
            )];
        }
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

    /// The current lossless driver facts of a tool, including updates received in permissions.
    pub fn fact(&self, raw_id: &str) -> Option<&ToolFact> {
        self.tools.get(raw_id)
    }

    /// Merge an ACP ToolCall/ToolCallUpdate without touching message boundaries.
    /// Missing/null optional fields leave their value alone; explicit empty collections replace
    /// old content/locations. rawInput/rawOutput are opaque values and replace, never deep-merge.
    /// Returns a display projection and diagnostics, which a permission handler must also emit.
    pub fn merge_tool(&mut self, u: &Value) -> Option<(ToolView, Vec<Fact>)> {
        let raw_id = u.get("toolCallId")?.as_str()?.to_string();
        let mut merged = self
            .tools
            .get(&raw_id)
            .map(|t| t.raw.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let fields = merged.as_object_mut().expect("tool fields are an object");
        for (key, value) in u.as_object()? {
            if key != "sessionUpdate"
                && (!value.is_null() || matches!(key.as_str(), "rawInput" | "rawOutput"))
            {
                fields.insert(key.clone(), value.clone());
            }
        }
        let (fact, diagnostics) = tool_fact(&raw_id, merged, &self.root);
        let view = fact.view(&self.root);
        self.tools.insert(raw_id, fact);
        Some((view, diagnostics))
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

fn decimal(a: f64) -> String {
    let s = format!("{a:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.into() }
}

fn tool_name(u: &Value) -> String {
    u.get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            u.pointer("/_meta/claudeCode/toolName")
                .and_then(Value::as_str)
        })
        .or_else(|| u.pointer("/rawInput/tool_name").and_then(Value::as_str))
        .unwrap_or("tool")
        .to_string()
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

fn command_of(input: &Value) -> Option<CommandFact> {
    match input.get("command")? {
        Value::String(s) => Some(CommandFact::Shell(s.clone())),
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()
            .map(CommandFact::Argv),
        _ => None,
    }
}

fn path_of(input: &Value) -> Option<String> {
    ["file_path", "path", "notebook_path"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(String::from)
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

/// `added`/`removed` count the complete new/old supplied texts, not a minimal edit script.
/// Previews may be cut; these counts refer to their untruncated source texts.
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
fn content_parts(u: &Value, root: &Path, diagnostics: &mut Vec<Fact>) -> Vec<Part> {
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
                } else {
                    diagnostics.push(diagnostic(
                        "unsupported_content",
                        "tool content is not text",
                    ));
                }
            }
            Some("diff") => out.push(diff(
                c.get("path").and_then(Value::as_str).unwrap_or(""),
                root,
                c.get("oldText").and_then(Value::as_str),
                c.get("newText").and_then(Value::as_str).unwrap_or(""),
            )),
            _ => diagnostics.push(diagnostic(
                "unsupported_content",
                "unsupported tool content",
            )),
        }
    }
    if text.chars().count() > TEXT_MAX {
        diagnostics.push(diagnostic("truncated", "tool text preview"));
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

impl ToolFact {
    /// Display projection only. The Node builds the authoritative permission Action from these
    /// facts: it must bind paths/cwd, preserve argv, and capture/recheck resource preconditions.
    /// In particular an argv or explicit cwd cannot be represented as a root-relative shell Exec.
    pub fn view(&self, root: &Path) -> ToolView {
        let input = &self.raw["rawInput"];
        let action = if self.origin == ToolOrigin::Steve {
            Some(Action::Tool {
                name: self
                    .name
                    .strip_prefix("mcp__steve__")
                    .unwrap_or(&self.name)
                    .into(),
                args: input.clone(),
            })
        } else if self.category == ToolCategory::Exec || self.name == "Bash" {
            match (&self.command, &self.cwd) {
                (Some(CommandFact::Shell(command)), None) => Some(Action::Exec {
                    command: command.clone(),
                    cwd: None,
                    network: NetworkScope::Unrestricted,
                }),
                _ => Some(Action::Unknown {
                    summary: "command needs Node binding".into(),
                }),
            }
        } else {
            match self.category {
                ToolCategory::Edit => Some(Action::Write {
                    paths: vec![],
                    change_summary: bound(&self.title, 200),
                    change: self.change.clone(),
                }),
                ToolCategory::Read => Some(Action::Read { paths: vec![] }),
                ToolCategory::Search => self.query.as_ref().map(|query| Action::Search {
                    query: query.clone(),
                }),
                ToolCategory::Fetch => self
                    .url
                    .as_ref()
                    .map(|url| Action::Fetch { url: url.clone() }),
                _ => None,
            }
        };
        ToolView {
            id: self.call.clone(),
            parent: None,
            name: self.name.clone(),
            category: self.category,
            origin: self.origin,
            title: bound(&self.title, 200),
            target: self.target(root).map(|t| bound(&t, 200)),
            phase: self.phase,
            action,
            result: self.result.clone(),
            error: self.error.as_ref().map(|e| bound(e, 500)),
            duration_ms: None,
        }
    }

    fn target(&self, root: &Path) -> Option<String> {
        self.paths
            .first()
            .map(|p| shown(p, root))
            .or_else(|| {
                self.command.as_ref().map(|c| match c {
                    CommandFact::Shell(s) => s.clone(),
                    // JSON is a display representation, never a shell command.
                    CommandFact::Argv(a) => serde_json::to_string(a).expect("strings serialize"),
                })
            })
            .or_else(|| self.query.clone())
            .or_else(|| self.url.clone())
    }
}

fn tool_fact(raw_id: &str, raw: Value, root: &Path) -> (ToolFact, Vec<Fact>) {
    let name = tool_name(&raw);
    let input = &raw["rawInput"];
    // server_name is programmatic routing metadata, unlike a title or a command's text.
    let server = input.get("server_name").and_then(Value::as_str);
    let origin = if name.starts_with("mcp__steve__") || server == Some("steve") {
        ToolOrigin::Steve
    } else if name.starts_with("mcp__") || server.is_some() {
        ToolOrigin::External
    } else {
        ToolOrigin::Harness
    };
    let category = if origin == ToolOrigin::Steve {
        ToolCategory::Steve
    } else {
        category(raw.get("kind").and_then(Value::as_str).unwrap_or(""))
    };
    let title = str_of(raw.get("title"));
    let phase = match raw.get("status").and_then(Value::as_str) {
        Some("in_progress") => ToolPhase::Running,
        Some("completed") => ToolPhase::Succeeded,
        Some("failed") => ToolPhase::Failed,
        _ => ToolPhase::Pending,
    };
    let mut paths = vec![];
    let mut add_path = |p: &str| {
        if !paths.iter().any(|old| old == p) {
            paths.push(p.to_string());
        }
    };
    if let Some(locations) = raw.get("locations").and_then(Value::as_array) {
        for l in locations {
            if let Some(p) = l.get("path").and_then(Value::as_str) {
                add_path(p);
            }
        }
    }
    for key in [
        "file_path",
        "path",
        "notebook_path",
        "old_path",
        "new_path",
        "source",
        "destination",
    ] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            add_path(p);
        }
    }
    if let Some(ps) = input.get("paths").and_then(Value::as_array) {
        for p in ps.iter().filter_map(Value::as_str) {
            add_path(p);
        }
    }
    if let Some(content) = raw.get("content").and_then(Value::as_array) {
        for d in content
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("diff"))
        {
            if let Some(p) = d.get("path").and_then(Value::as_str) {
                add_path(p);
            }
        }
    }
    let mut diagnostics = vec![];
    // An explicitly supplied collection is authoritative, even when empty. Never resurrect an
    // old rawOutput/input diff when content was cleared by the adapter.
    let result = if raw.get("content").is_some_and(Value::is_array) {
        content_parts(&raw, root, &mut diagnostics)
    } else if let Some(output) = raw.get("rawOutput") {
        let text = match output {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            v => v.to_string(),
        };
        if text.chars().count() > TEXT_MAX {
            diagnostics.push(diagnostic("truncated", "tool output preview"));
        }
        if text.is_empty() {
            vec![]
        } else {
            vec![Part::Text {
                text: bound(&text, TEXT_MAX),
            }]
        }
    } else {
        input_diffs(input, root)
    };
    let diffs: Vec<&Value> = raw
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("type").and_then(Value::as_str) == Some("diff"))
        .collect();
    for d in &diffs {
        for key in ["oldText", "newText"] {
            if d.get(key)
                .and_then(Value::as_str)
                .is_some_and(diff_truncated)
            {
                diagnostics.push(diagnostic("truncated", "tool diff preview"));
            }
        }
    }
    if !raw.get("content").is_some_and(Value::is_array) && raw.get("rawOutput").is_none() {
        let truncated = if let Some(edits) = input.get("edits").and_then(Value::as_array) {
            ["old_string", "new_string"].iter().any(|key| {
                let texts: Vec<&str> = edits
                    .iter()
                    .filter_map(|e| e.get(*key).and_then(Value::as_str))
                    .collect();
                diff_truncated(&texts.join("\n⋯\n"))
            })
        } else {
            ["old_string", "new_string", "content"]
                .iter()
                .filter_map(|key| input.get(*key).and_then(Value::as_str))
                .any(diff_truncated)
        };
        if truncated {
            diagnostics.push(diagnostic("truncated", "input diff preview"));
        }
    }
    if title.chars().count() > 200 {
        diagnostics.push(diagnostic("truncated", "tool title preview"));
    }
    let change = if category == ToolCategory::Edit && !input.is_null() {
        digest_json(input)
    } else if !diffs.is_empty() {
        steve_node_api::digest_of(&diffs).ok()
    } else {
        None
    };
    let error = if phase == ToolPhase::Failed {
        if let Some(content) = raw.get("content").and_then(Value::as_array) {
            let texts: Vec<&str> = content
                .iter()
                .filter(|c| c["type"] == "content")
                .filter_map(|c| c.pointer("/content/text").and_then(Value::as_str))
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        } else {
            raw.get("rawOutput")
                .filter(|v| !v.is_null())
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    v => v.to_string(),
                })
        }
    } else {
        None
    };
    if error.as_ref().is_some_and(|e| e.chars().count() > 500) {
        diagnostics.push(diagnostic("truncated", "tool error preview"));
    }
    let fact = ToolFact {
        raw_id: raw_id.into(),
        call: call_id(raw_id),
        name,
        category,
        origin,
        title,
        phase,
        paths,
        cwd: input.get("cwd").and_then(Value::as_str).map(String::from),
        command: command_of(input),
        query: ["pattern", "query"]
            .iter()
            .find_map(|k| input.get(*k).and_then(Value::as_str))
            .map(String::from),
        url: input.get("url").and_then(Value::as_str).map(String::from),
        change,
        result,
        error,
        raw,
    };
    if fact.target(root).is_some_and(|t| t.chars().count() > 200) {
        diagnostics.push(diagnostic("truncated", "tool target preview"));
    }
    (fact, diagnostics)
}

fn diff_truncated(s: &str) -> bool {
    s.lines().count() > DIFF_LINES || s.chars().count() > TEXT_MAX
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
        // A permission or an update may be the first observation of a tool.
        assert!(
            matches!(&events(n.update(&json!({"sessionUpdate": "tool_call_update", "toolCallId": "zz", "status": "completed"})))[0], EventBody::ToolChanged { tool } if tool.phase == ToolPhase::Succeeded)
        );
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

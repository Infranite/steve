//! ACP → Steve's events. The one place that reads ACP's `session/update`
//! payloads (`_meta`, `rawInput`, tool content) and says what they mean in Steve's terms; whatever
//! runs a harness over ACP reports through it, and nothing past it sees ACP.
//!
//! One [`Normalizer`] per attempt: it numbers the messages and blocks of the attempt and keeps each
//! tool's current view, so a `tool_call_update` comes out as the whole tool again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use steve_node_api::{
    Action, CallId, Digest, EventBody, Measurement, Money, NetworkScope, Part, PlanEntry, Role,
    Severity, SubtaskView, Tokens, ToolCategory, ToolOrigin, ToolPhase, ToolView, UsageReport,
    UsageScope,
};

use crate::adapter::{AdapterContext, AdapterOutcome, HarnessAdapter, SubtaskState};

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
        details: None,
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
    /// The adapter's `messageId`, when it sends one: another id is another message.
    id: Option<String>,
    message: String,
    block: String,
    offset: u64,
}

#[derive(Debug, Default, Clone)]
struct TerminalOutput {
    text: String,
    chars: usize,
    truncated: bool,
}

impl TerminalOutput {
    /// Appends `chunk`, retaining at most [`TEXT_MAX`] characters. Returns `true` only on the
    /// update that first overflows [`TEXT_MAX`].
    fn push(&mut self, chunk: &str) -> bool {
        if chunk.is_empty() || self.truncated {
            return false;
        }
        let remaining = TEXT_MAX.saturating_sub(self.chars);
        let mut iter = chunk.chars();
        for _ in 0..remaining {
            let Some(ch) = iter.next() else {
                return false;
            };
            self.text.push(ch);
            self.chars += 1;
        }
        if iter.next().is_some() {
            self.truncated = true;
            metrics::counter!(
                "steve_acp_tool_output_truncated_total",
                "source" => "terminal_delta",
            )
            .increment(1);
            true
        } else {
            false
        }
    }

    fn rendered(&self) -> Option<String> {
        if self.text.is_empty() {
            return None;
        }
        if self.truncated {
            let mut out = self.text.clone();
            out.push('…');
            Some(out)
        } else {
            Some(self.text.clone())
        }
    }

    fn clear(&mut self) {
        self.text.clear();
        self.chars = 0;
        self.truncated = false;
    }
}

pub struct Normalizer {
    /// The workspace root, to show paths inside it relative.
    root: PathBuf,
    /// Unique per attempt (its id): message ids and usage report ids derive from it.
    prefix: String,
    /// The harness process generation: a restarted adapter counts its cost from zero again.
    meter: steve_node_api::Epoch,
    /// Per-harness runtime adapter responsible for harness-specific normalization & subtask state.
    adapter: Arc<dyn HarnessAdapter>,
    /// The root ACP session id once opened, used to distinguish child subagent session updates.
    root_session_id: Option<String>,
    /// Tracked subagents, background tasks, and per-tool monotonic timing.
    subtasks: SubtaskState,
    messages: u64,
    open: Option<Open>,
    tools: HashMap<String, ToolFact>,
    /// Bounded streaming output accumulated from `_meta.terminal_output_delta` /
    /// `_meta.terminal_output` / `_meta.mcp_output_delta` across updates of each `toolCallId`.
    terminal_outputs: HashMap<String, TerminalOutput>,
    /// See [`Normalizer::input_includes_cache`].
    input_includes_cache: bool,
    /// Cumulative session token baseline subtracted from `PromptResponse.usage` when
    /// `usage_prompt_cumulative` is set (`(raw_input, output, cache_read, cache_write, reasoning)`).
    prior_prompt_tokens: Option<(u64, u64, u64, u64, u64)>,
    /// Updated session cumulative token totals observed in `PromptResponse.usage`.
    last_prompt_tokens: Option<(u64, u64, u64, u64, u64)>,
    /// Per-turn streaming token deltas already emitted from `agent_*_chunk._meta.usage` during the
    /// current turn, subtracted from any repeating final `PromptResponse.usage` summary.
    streamed_turn_tokens: (u64, u64, u64, u64, u64),
}

impl Normalizer {
    pub fn new(root: impl Into<PathBuf>, prefix: &str, meter: steve_node_api::Epoch) -> Self {
        Normalizer {
            root: root.into(),
            prefix: prefix.to_string(),
            meter,
            adapter: crate::adapter::for_harness("generic"),
            root_session_id: None,
            subtasks: SubtaskState::default(),
            messages: 0,
            open: None,
            tools: HashMap::new(),
            terminal_outputs: HashMap::new(),
            input_includes_cache: false,
            prior_prompt_tokens: None,
            last_prompt_tokens: None,
            streamed_turn_tokens: (0, 0, 0, 0, 0),
        }
    }

    /// Select the per-harness runtime [`HarnessAdapter`] for this attempt.
    pub fn for_harness(mut self, harness: &str) -> Self {
        self.adapter = crate::adapter::for_harness(harness);
        self
    }

    /// Record the root ACP session id so updates arriving on child sessions are routed cleanly.
    pub fn root_session(mut self, session_id: impl Into<String>) -> Self {
        self.root_session_id = Some(session_id.into());
        self
    }

    /// Update the root ACP session id in place once `session/new` or `session/load` settles.
    pub fn set_root_session(&mut self, session_id: impl Into<String>) {
        self.root_session_id = Some(session_id.into());
    }

    /// The active per-harness runtime adapter.
    pub fn adapter(&self) -> &dyn HarnessAdapter {
        self.adapter.as_ref()
    }

    /// Whether `session_id` belongs to a tracked child subagent session spawned by this root session.
    pub fn is_child_session(&self, session_id: &str) -> bool {
        self.subtasks.has_rfd_subagent(session_id)
    }

    /// Current view of a tracked subtask by its Steve [`CallId`].
    pub fn subtask(&self, id: &CallId) -> Option<&SubtaskView> {
        self.subtasks.lookup_by_call_id(id)
    }

    /// Encode a native JSON-RPC notification to cancel one active subtask without cancelling the
    /// parent turn, when supported by the harness adapter and subtask.
    pub fn cancel_subtask_request(&self, id: &CallId) -> Option<(&'static str, Value)> {
        let subtask = self.subtasks.lookup_by_call_id(id)?;
        self.adapter.cancel_subtask_request(subtask)
    }

    /// The adapter counts cache reads inside `inputTokens` (OpenAI's convention, declared as
    /// `usage_input_includes_cache`): Steve's figures keep them apart, as Anthropic's do.
    pub fn input_includes_cache(mut self, yes: bool) -> Self {
        self.input_includes_cache = yes;
        self
    }

    /// The adapter reports cumulative session token totals in `PromptResponse.usage`
    /// (`usage_prompt_cumulative`): subtract `prior` (`(raw_input, output, cache_read, cache_write,
    /// reasoning)`) to emit per-turn deltas.
    pub fn prompt_cumulative_baseline(mut self, prior: (u64, u64, u64, u64, u64)) -> Self {
        self.set_prompt_cumulative_baseline(prior);
        self
    }

    /// Updates the cumulative session token baseline in place, preserving `root`, `prefix`,
    /// `meter`, and `input_includes_cache`.
    pub fn set_prompt_cumulative_baseline(&mut self, prior: (u64, u64, u64, u64, u64)) {
        self.prior_prompt_tokens = Some(prior);
        self.last_prompt_tokens = Some(prior);
    }

    /// The updated cumulative session token high-water mark after [`Self::prompt_response`].
    pub fn cumulative_prompt_tokens(&self) -> Option<(u64, u64, u64, u64, u64)> {
        self.last_prompt_tokens
    }

    /// Extracts cumulative session token counters from a `usage_update` notification (used when
    /// seeding a resumed session's baseline on `session/load`).
    pub fn cumulative_tokens_from_usage_update(u: &Value) -> Option<(u64, u64, u64, u64, u64)> {
        let meta = u.get("_meta")?;
        let prompt = meta
            .get("promptTokens")
            .or_else(|| meta.get("inputTokens"))
            .or_else(|| meta.get("prompt_tokens"))
            .or_else(|| meta.get("input_tokens"))
            .and_then(Value::as_u64);
        let completion = meta
            .get("completionTokens")
            .or_else(|| meta.get("outputTokens"))
            .or_else(|| meta.get("completion_tokens"))
            .or_else(|| meta.get("output_tokens"))
            .and_then(Value::as_u64);
        if prompt.is_none() && completion.is_none() {
            return None;
        }
        let cached = meta
            .get("cachedTokens")
            .or_else(|| meta.get("cachedReadTokens"))
            .or_else(|| meta.get("cachedInputTokens"))
            .or_else(|| meta.get("cached_tokens"))
            .or_else(|| meta.get("cached_read_tokens"))
            .or_else(|| meta.get("cache_read_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cached_write = meta
            .get("cachedWriteTokens")
            .or_else(|| meta.get("cached_write_tokens"))
            .or_else(|| meta.get("cache_creation_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reasoning = meta
            .get("reasoningTokens")
            .or_else(|| meta.get("thoughtTokens"))
            .or_else(|| meta.get("reasoning_tokens"))
            .or_else(|| meta.get("thought_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Some((
            prompt.unwrap_or(0),
            completion.unwrap_or(0),
            cached,
            cached_write,
            reasoning,
        ))
    }

    /// The events and facts of one `session/update` (the `update` object) on the root session.
    pub fn update(&mut self, u: &Value) -> Vec<Fact> {
        self.update_for_session(None, u)
    }

    /// The events and facts of one `session/update` carrying its ACP `sessionId` (which may be a
    /// registered child subagent session).
    pub fn update_for_session(&mut self, session_id: Option<&str>, u: &Value) -> Vec<Fact> {
        let adapter = self.adapter.clone();
        {
            let mut ctx = AdapterContext {
                state: &mut self.subtasks,
                root_session_id: self.root_session_id.as_deref(),
                prefix: &self.prefix,
                meter: &self.meter,
                input_includes_cache: self.input_includes_cache,
            };
            if let AdapterOutcome::Handled(facts) =
                adapter.normalize_update(&mut ctx, session_id, u)
            {
                return facts;
            }
        }
        let kind = u.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
        match kind {
            "agent_message_chunk" => self.text(u, Role::Assistant),
            "agent_thought_chunk" => self.text(u, Role::Thought),
            "user_message_chunk" => vec![Fact::Nothing],
            "tool_call" | "tool_call_update" => {
                let Some((t, diagnostics, adapter_facts)) =
                    self.merge_tool_for_session(session_id, u)
                else {
                    return vec![];
                };
                // Only root-session tool calls end an open root assistant/thought message; child
                // subagent tool calls interleave concurrently without splitting the parent reply.
                let mut out = if t.parent.is_none() {
                    self.close()
                } else {
                    vec![]
                };
                out.push(Fact::Event(EventBody::ToolChanged { tool: t }));
                out.extend(diagnostics);
                out.extend(adapter_facts);
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
                if let Some(m) = config_value(u, crate::session::is_model_option) {
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
                details: None,
            })],
        }
    }

    /// The attempt is over: close the message still open.
    pub fn finish(&mut self) -> Vec<Fact> {
        self.close()
    }

    /// Final token accounting from `session/prompt`, distinct from usage_update's context meter.
    pub fn prompt_response(&mut self, response: &Value) -> Vec<Fact> {
        let cached_inside = self.input_includes_cache;
        let mut rows = vec![];
        if let Some(u) = response.get("usage").filter(|u| u.is_object()).or_else(|| {
            response.get("_meta").filter(|m| {
                m.get("inputTokens").is_some()
                    || m.get("outputTokens").is_some()
                    || m.get("input_tokens").is_some()
                    || m.get("output_tokens").is_some()
            })
        }) {
            let t = if let Some(prior) = self.prior_prompt_tokens {
                self.prompt_delta(u, cached_inside, prior)
            } else {
                let mut t = prompt_tokens(u, cached_inside);
                let (si, so, scr, scw, sr) = self.streamed_turn_tokens;
                if (si | so | scr | scw | sr) != 0 {
                    t.input = t.input.map(|v| v.saturating_sub(si));
                    t.output = t.output.map(|v| v.saturating_sub(so));
                    t.cache_read = t.cache_read.map(|v| v.saturating_sub(scr));
                    t.cache_write = t.cache_write.map(|v| v.saturating_sub(scw));
                    t.reasoning = t.reasoning.map(|v| v.saturating_sub(sr));
                }
                t
            };
            self.streamed_turn_tokens = (0, 0, 0, 0, 0);
            if t.input.is_some_and(|v| v > 0)
                || t.output.is_some_and(|v| v > 0)
                || t.cache_read.is_some_and(|v| v > 0)
                || t.cache_write.is_some_and(|v| v > 0)
                || t.reasoning.is_some_and(|v| v > 0)
                || self.prior_prompt_tokens.is_none()
            {
                rows.push((None, UsageScope::Main, t));
            }
        }
        if let Some(models) = response
            .pointer("/_meta/quota/model_usage")
            .and_then(Value::as_array)
        {
            for model in models {
                if let Some(count) = model.get("token_count") {
                    rows.push((
                        model.get("model").and_then(Value::as_str).map(String::from),
                        UsageScope::All,
                        prompt_tokens(count, cached_inside),
                    ));
                }
            }
        }
        rows.into_iter()
            .map(|(model, scope, tokens)| {
                let mut report = UsageReport {
                    report_id: String::new(),
                    model,
                    meter_epoch: self.meter.clone(),
                    scope,
                    measurement: Measurement::Final,
                    tokens,
                    context_used: None,
                    context_limit: None,
                    cost: None,
                };
                crate::usage::identify(&self.prefix, &mut report);
                Fact::Event(EventBody::Usage { report })
            })
            .collect()
    }

    fn prompt_delta(
        &mut self,
        u: &Value,
        cached_inside: bool,
        (pi, po, pcr, pcw, pr): (u64, u64, u64, u64, u64),
    ) -> Tokens {
        let raw_in = u
            .get("inputTokens")
            .or_else(|| u.get("input_tokens"))
            .and_then(Value::as_u64);
        let raw_out = u
            .get("outputTokens")
            .or_else(|| u.get("output_tokens"))
            .and_then(Value::as_u64);
        let raw_cr = u
            .get("cachedReadTokens")
            .or_else(|| u.get("cachedInputTokens"))
            .or_else(|| u.get("cached_read_tokens"))
            .or_else(|| u.get("cache_read_input_tokens"))
            .and_then(Value::as_u64);
        let raw_cw = u
            .get("cachedWriteTokens")
            .or_else(|| u.get("cacheWriteInputTokens"))
            .or_else(|| u.get("cached_write_tokens"))
            .or_else(|| u.get("cache_creation_input_tokens"))
            .and_then(Value::as_u64);
        let raw_r = u
            .get("reasoningTokens")
            .or_else(|| u.get("thoughtTokens"))
            .or_else(|| u.get("reasoningOutputTokens"))
            .or_else(|| u.get("reasoning_tokens"))
            .or_else(|| u.get("thought_tokens"))
            .and_then(Value::as_u64);
        let next = (
            raw_in.unwrap_or(pi).max(pi),
            raw_out.unwrap_or(po).max(po),
            raw_cr.unwrap_or(pcr).max(pcr),
            raw_cw.unwrap_or(pcw).max(pcw),
            raw_r.unwrap_or(pr).max(pr),
        );
        self.prior_prompt_tokens = Some(next);
        self.last_prompt_tokens = Some(next);
        let d_cr = raw_cr.map(|v| v.saturating_sub(pcr));
        let d_in_raw = raw_in.map(|v| v.saturating_sub(pi));
        let d_in = d_in_raw.and_then(|i| {
            if cached_inside {
                d_cr.and_then(|c| i.checked_sub(c))
            } else {
                Some(i)
            }
        });
        let mut t = Tokens {
            input: d_in,
            output: raw_out.map(|v| v.saturating_sub(po)),
            cache_read: d_cr,
            cache_write: raw_cw.map(|v| v.saturating_sub(pcw)),
            reasoning: raw_r.map(|v| v.saturating_sub(pr)),
        };
        let (si, so, scr, scw, sr) = self.streamed_turn_tokens;
        if (si | so | scr | scw | sr) != 0 {
            t.input = t.input.map(|v| v.saturating_sub(si));
            t.output = t.output.map(|v| v.saturating_sub(so));
            t.cache_read = t.cache_read.map(|v| v.saturating_sub(scr));
            t.cache_write = t.cache_write.map(|v| v.saturating_sub(scw));
            t.reasoning = t.reasoning.map(|v| v.saturating_sub(sr));
        }
        t
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
        let mut meta_usage = vec![];
        if let Some(mu) = u.pointer("/_meta/usage").filter(|v| v.is_object()) {
            let saved_prior = self.prior_prompt_tokens.take();
            let saved_streamed = std::mem::replace(&mut self.streamed_turn_tokens, (0, 0, 0, 0, 0));
            let synthetic = serde_json::json!({"usage": mu});
            meta_usage = self.prompt_response(&synthetic);
            self.prior_prompt_tokens = saved_prior;
            let mut acc = saved_streamed;
            for f in &meta_usage {
                if let Fact::Event(EventBody::Usage { report }) = f {
                    acc.0 = acc.0.saturating_add(report.tokens.input.unwrap_or(0));
                    acc.1 = acc.1.saturating_add(report.tokens.output.unwrap_or(0));
                    acc.2 = acc.2.saturating_add(report.tokens.cache_read.unwrap_or(0));
                    acc.3 = acc.3.saturating_add(report.tokens.cache_write.unwrap_or(0));
                    acc.4 = acc.4.saturating_add(report.tokens.reasoning.unwrap_or(0));
                }
            }
            self.streamed_turn_tokens = acc;
        }
        let t = u
            .pointer("/content/text")
            .and_then(Value::as_str)
            .unwrap_or("");
        if t.is_empty() {
            return if meta_usage.is_empty() {
                vec![Fact::Nothing]
            } else {
                meta_usage
            };
        }
        let id = u.get("messageId").and_then(Value::as_str).map(String::from);
        let mut out = vec![];
        if self
            .open
            .as_ref()
            .is_some_and(|o| o.role != role || (id.is_some() && o.id.is_some() && o.id != id))
        {
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
                id,
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
        out.extend(meta_usage);
        out
    }

    fn usage(&mut self, u: &Value) -> UsageReport {
        let int = |k: &str| u.get(k).and_then(Value::as_u64);
        let origin = u
            .pointer("/_meta/_claude~1origin")
            .and_then(|o| o.as_str().or_else(|| o.get("kind").and_then(Value::as_str)))
            .is_some_and(|kind| !kind.is_empty());
        let mut report = UsageReport {
            report_id: String::new(),
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
            // Only cost is process cumulative. used/size describe context, never consumption.
            measurement: Measurement::Cumulative,
            tokens: Tokens::default(),
            context_used: int("used"),
            context_limit: int("size"),
            cost: u
                .pointer("/cost/amount")
                .and_then(Value::as_f64)
                .and_then(|a| {
                    let currency = match u.pointer("/cost") {
                        Some(c) => match c.get("currency") {
                            None => "USD",
                            Some(Value::String(s)) if s.trim().eq_ignore_ascii_case("USD") => "USD",
                            Some(_) => return None,
                        },
                        None => return None,
                    };
                    Some(Money {
                        decimal: decimal(a),
                        currency: currency.to_string(),
                    })
                }),
        };
        crate::usage::identify(&self.prefix, &mut report);
        report
    }

    /// The current lossless driver facts of a tool, including updates received in permissions.
    pub fn fact(&self, raw_id: &str) -> Option<&ToolFact> {
        self.tools.get(raw_id)
    }

    /// The tool's view as it stands.
    pub fn view_of(&self, raw_id: &str) -> Option<ToolView> {
        self.tools.get(raw_id).map(|f| f.view(&self.root))
    }

    /// Merge an ACP ToolCall/ToolCallUpdate without touching message boundaries.
    /// Missing/null optional fields leave their value alone; explicit empty collections replace
    /// old content/locations. rawInput/rawOutput are opaque values and replace, never deep-merge.
    /// Returns a display projection and diagnostics, which a permission handler must also emit.
    pub fn merge_tool(&mut self, u: &Value) -> Option<(ToolView, Vec<Fact>)> {
        let (view, mut diagnostics, adapter_facts) = self.merge_tool_for_session(None, u)?;
        diagnostics.extend(adapter_facts);
        Some((view, diagnostics))
    }

    fn merge_tool_for_session(
        &mut self,
        session_id: Option<&str>,
        u: &Value,
    ) -> Option<(ToolView, Vec<Fact>, Vec<Fact>)> {
        let raw_id = u.get("toolCallId")?.as_str()?.to_string();
        let initial_title = self
            .subtasks
            .record_initial_title(&raw_id, u.get("title").and_then(Value::as_str))
            .map(String::from);
        let mut merged = self
            .tools
            .get(&raw_id)
            .map(|t| t.raw.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let incoming_delta = incoming_meta_output_delta(u);
        let cleared_content = u
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        if cleared_content && incoming_delta.is_none() {
            if let Some(buf) = self.terminal_outputs.get_mut(&raw_id) {
                buf.clear();
            }
        }
        let terminal_newly_truncated = incoming_delta.is_some_and(|delta| {
            self.terminal_outputs
                .entry(raw_id.clone())
                .or_default()
                .push(delta)
        });
        let terminal_output = self
            .terminal_outputs
            .get(&raw_id)
            .and_then(TerminalOutput::rendered);
        let fields = merged.as_object_mut().expect("tool fields are an object");
        for (key, value) in u.as_object()? {
            if key == "sessionUpdate" {
                continue;
            }
            if key == "_meta"
                && let Some(incoming_meta) = value.as_object()
                && let Some(existing_meta) = fields.get_mut("_meta").and_then(Value::as_object_mut)
            {
                for (mk, mv) in incoming_meta {
                    if !mv.is_null() {
                        existing_meta.insert(mk.clone(), mv.clone());
                    }
                }
                continue;
            }
            if !value.is_null() || matches!(key.as_str(), "rawInput" | "rawOutput") {
                fields.insert(key.clone(), value.clone());
            }
        }
        let adapter = self.adapter.clone();
        let (fact, mut diagnostics) = tool_fact(
            &raw_id,
            merged,
            &self.root,
            adapter.as_ref(),
            initial_title.as_deref(),
            terminal_output.as_deref(),
        );
        if terminal_newly_truncated {
            diagnostics.push(diagnostic("truncated", "tool output preview"));
        }
        let mut view = fact.view(&self.root);
        let settled = matches!(fact.phase, ToolPhase::Succeeded | ToolPhase::Failed);
        view.duration_ms = self.subtasks.record_tool_timing(&raw_id, settled);
        let adapter_facts = {
            let mut ctx = AdapterContext {
                state: &mut self.subtasks,
                root_session_id: self.root_session_id.as_deref(),
                prefix: &self.prefix,
                meter: &self.meter,
                input_includes_cache: self.input_includes_cache,
            };
            adapter.observe_tool(&mut ctx, session_id, &raw_id, &fact, &mut view)
        };
        self.tools.insert(raw_id, fact);
        Some((view, diagnostics, adapter_facts))
    }
}

fn prompt_tokens(u: &Value, cached_inside: bool) -> Tokens {
    let input_raw = u
        .get("inputTokens")
        .or_else(|| u.get("input_tokens"))
        .and_then(Value::as_u64);
    let output = u
        .get("outputTokens")
        .or_else(|| u.get("output_tokens"))
        .and_then(Value::as_u64);
    let cache_read = u
        .get("cachedReadTokens")
        .or_else(|| u.get("cachedInputTokens"))
        .or_else(|| u.get("cached_read_tokens"))
        .or_else(|| u.get("cache_read_input_tokens"))
        .and_then(Value::as_u64);
    let cache_write = u
        .get("cachedWriteTokens")
        .or_else(|| u.get("cacheWriteInputTokens"))
        .or_else(|| u.get("cached_write_tokens"))
        .or_else(|| u.get("cache_creation_input_tokens"))
        .and_then(Value::as_u64);
    let reasoning = u
        .get("reasoningTokens")
        .or_else(|| u.get("thoughtTokens"))
        .or_else(|| u.get("reasoningOutputTokens"))
        .or_else(|| u.get("reasoning_tokens"))
        .or_else(|| u.get("thought_tokens"))
        .and_then(Value::as_u64)
        .filter(|r| output.is_none_or(|o| *r <= o));
    let input = input_raw.and_then(|i| {
        if cached_inside {
            cache_read.and_then(|cached| i.checked_sub(cached))
        } else {
            Some(i)
        }
    });
    Tokens {
        input,
        output,
        cache_read,
        cache_write,
        reasoning,
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
    let s = format!("{a:.10}");
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
        .or_else(|| u.pointer("/_meta/tool_name").and_then(Value::as_str))
        .or_else(|| u.pointer("/rawInput/tool_name").and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| {
            mcp_invocation(u).map(|(server, tool, _)| {
                if server == "steve" {
                    tool.to_owned()
                } else {
                    format!("mcp__{server}__{tool}")
                }
            })
        })
        // unknown: empty, never a made-up name
        .unwrap_or_default()
}

/// A structured MCP invocation as emitted by ACP adapters (including codex-acp 0.16).
/// Explicit tool names take precedence: an ordinary tool's argument keys must not rename it.
/// Display titles such as `Tool: server/tool` are deliberately never an identity source.
fn mcp_invocation(u: &Value) -> Option<(&str, &str, &Value)> {
    if u.get("name").and_then(Value::as_str).is_some()
        || u.pointer("/_meta/claudeCode/toolName")
            .and_then(Value::as_str)
            .is_some()
        || u.pointer("/_meta/tool_name")
            .and_then(Value::as_str)
            .is_some()
        || u.pointer("/rawInput/tool_name")
            .and_then(Value::as_str)
            .is_some()
    {
        return None;
    }
    let input = u.get("rawInput")?;
    let server = input.get("server")?.as_str().filter(|s| !s.is_empty())?;
    let tool = input.get("tool")?.as_str().filter(|s| !s.is_empty())?;
    Some((server, tool, input.get("arguments")?))
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
        .is_some_and(|id| ["thought_level", "reasoning_effort", "effort", "thinking"].contains(&id))
        || o.get("category")
            .and_then(Value::as_str)
            .is_some_and(|cat| ["thought_level", "thinking"].contains(&cat))
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

/// A command as one shell line: a shell string as it is, an argv with each word quoted.
fn command_line(c: &CommandFact) -> String {
    match c {
        CommandFact::Shell(s) => s.clone(),
        CommandFact::Argv(a) => a
            .iter()
            .map(|w| {
                let plain = !w.is_empty()
                    && w.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%^".contains(c));
                if plain {
                    w.clone()
                } else {
                    format!("'{}'", w.replace('\'', "'\\''"))
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn command_of(input: &Value) -> Option<CommandFact> {
    match input.get("command").or_else(|| input.get("CommandLine"))? {
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
    ["file_path", "path", "Path", "notebook_path"]
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
            // ACP terminal reference (`{type: "terminal", terminalId: ...}`): output streams via
            // `_meta.terminal_output_delta` / `_meta.terminal_output`, not inline content blocks.
            Some("terminal") => {}
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
    /// The tool's actual arguments, without an adapter's MCP routing envelope.
    pub fn input(&self) -> &Value {
        mcp_invocation(&self.raw)
            .map(|(_, _, arguments)| arguments)
            .unwrap_or(&self.raw["rawInput"])
    }

    /// Display projection only. The Node builds the authoritative permission Action from these
    /// facts: it must bind paths/cwd, preserve argv, and capture/recheck resource preconditions.
    /// In particular an argv or explicit cwd cannot be represented as a root-relative shell Exec.
    pub fn view(&self, root: &Path) -> ToolView {
        let input = self.input();
        let action = if self.origin == ToolOrigin::Steve {
            Some(Action::Tool {
                name: self.name.clone(),
                args: input.clone(),
            })
        } else if self.category == ToolCategory::Exec || self.name == "Bash" {
            match (&self.command, &self.cwd) {
                (Some(CommandFact::Shell(command)), None) => Some(Action::Exec {
                    command: command.clone(),
                    cwd: None,
                    network: NetworkScope::Unrestricted,
                }),
                // its command line as written (argv words quoted), for the timeline and the
                // tool facts; the directory it runs in is the harness's own
                (Some(c), _) => Some(Action::Unknown {
                    summary: command_line(c),
                }),
                (None, _) => Some(Action::Unknown {
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
            description: input
                .get("description")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map(|d| bound(d, 200)),
            target: self.target(root).map(|t| bound(&t, 200)),
            phase: self.phase,
            action,
            evidence: None,
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

fn tool_fact(
    raw_id: &str,
    raw: Value,
    root: &Path,
    adapter: &dyn HarnessAdapter,
    initial_title: Option<&str>,
    terminal_output: Option<&str>,
) -> (ToolFact, Vec<Fact>) {
    let mut name = tool_name(&raw);
    if name.is_empty()
        && let Some(inferred) = adapter.infer_tool_name(&raw, initial_title)
    {
        name = inferred;
    }
    let input = &raw["rawInput"];
    // server_name is programmatic routing metadata, unlike a title or a command's text.
    let server = input
        .get("server_name")
        .and_then(Value::as_str)
        .or_else(|| mcp_invocation(&raw).map(|(server, _, _)| server));
    let origin = match server {
        Some("steve") => ToolOrigin::Steve,
        Some(_) => ToolOrigin::External,
        None if name.starts_with("mcp__steve__") || name.starts_with("mcp_steve_") => {
            ToolOrigin::Steve
        }
        None if name.starts_with("mcp__") => ToolOrigin::External,
        None => ToolOrigin::Harness,
    };
    // Strip flat MCP transport prefixes (`mcp__steve__<tool>` from Claude Code, `mcp_steve_<tool>`
    // from Antigravity) only when the adapter did not supply structured server routing metadata.
    // A structured invocation with `server: "steve"` already carries the literal tool name, so a
    // nested prefix inside it must stay unstripped and fail strict name checks.
    if server.is_none()
        && origin == ToolOrigin::Steve
        && let Some(short) = name
            .strip_prefix("mcp__steve__")
            .or_else(|| name.strip_prefix("mcp_steve_"))
    {
        name = short.to_owned();
    }
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
    let paths = tool_paths(&raw, input);
    let mut diagnostics = vec![];
    let result = tool_result(&raw, input, root, terminal_output, &mut diagnostics);
    let diffs: Vec<&Value> = raw
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("type").and_then(Value::as_str) == Some("diff"))
        .collect();
    diagnose_tool_diffs(&raw, input, &diffs, &mut diagnostics);
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
    let error = tool_error(&raw, phase, terminal_output);
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

fn tool_paths(raw: &Value, input: &Value) -> Vec<String> {
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
        "Path",
        "notebook_path",
        "old_path",
        "new_path",
        "source",
        "destination",
        "dir_path",
        "directory",
        "root",
        "target_directory",
        "file",
    ] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            add_path(p);
        }
    }
    for key in ["paths", "files"] {
        if let Some(ps) = input.get(key).and_then(Value::as_array) {
            for p in ps.iter().filter_map(Value::as_str) {
                add_path(p);
            }
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
    paths
}

fn incoming_meta_output_delta(u: &Value) -> Option<&str> {
    u.pointer("/_meta/terminal_output_delta/data")
        .or_else(|| u.pointer("/_meta/terminal_output/data"))
        .or_else(|| u.pointer("/_meta/mcp_output_delta/data"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// True when `rawOutput` contains only command exit metadata (e.g. `{"exit_code": 0}` sent by
/// `@agentclientprotocol/codex-acp` on non-terminal read/search/list commands whose stdout arrives
/// in `_meta.terminal_output_delta`).
fn is_exit_metadata_only(output: &Value) -> bool {
    let Some(obj) = output.as_object() else {
        return false;
    };
    !obj.is_empty()
        && obj
            .keys()
            .all(|k| matches!(k.as_str(), "exit_code" | "exitCode" | "signal"))
}

fn tool_result(
    raw: &Value,
    input: &Value,
    root: &Path,
    terminal_output: Option<&str>,
    diagnostics: &mut Vec<Fact>,
) -> Vec<Part> {
    // An explicitly supplied collection is authoritative, even when empty. Never resurrect an
    // old rawOutput/input diff when content was cleared by the adapter.
    if raw.get("content").is_some_and(Value::is_array) {
        let mut out = content_parts(raw, root, diagnostics);
        if !out.iter().any(|p| matches!(p, Part::Text { .. }))
            && let Some(text) = terminal_output
        {
            out.insert(
                0,
                Part::Text {
                    text: text.to_string(),
                },
            );
        }
        out
    } else if let Some(output) = raw.get("rawOutput") {
        if let Some(text) = terminal_output
            && (output.is_null() || is_exit_metadata_only(output))
        {
            return vec![Part::Text {
                text: text.to_string(),
            }];
        }
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
    } else if let Some(text) = terminal_output {
        vec![Part::Text {
            text: text.to_string(),
        }]
    } else {
        input_diffs(input, root)
    }
}

fn tool_error(raw: &Value, phase: ToolPhase, terminal_output: Option<&str>) -> Option<String> {
    if phase != ToolPhase::Failed {
        return None;
    }
    if let Some(content) = raw.get("content").and_then(Value::as_array) {
        let texts: Vec<&str> = content
            .iter()
            .filter(|c| c["type"] == "content")
            .filter_map(|c| c.pointer("/content/text").and_then(Value::as_str))
            .collect();
        if !texts.is_empty() {
            return Some(texts.join("\n"));
        }
        if content.is_empty() && terminal_output.is_none() {
            return None;
        }
    }
    if let Some(text) = terminal_output {
        return Some(text.to_string());
    }
    if let Some(output) = raw.get("rawOutput").filter(|v| !v.is_null()) {
        return Some(match output {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        });
    }
    raw.pointer("/_meta/terminal_exit/exit_code")
        .and_then(Value::as_i64)
        .map(|code| format!("exit code {code}"))
}

fn diagnose_tool_diffs(raw: &Value, input: &Value, diffs: &[&Value], diagnostics: &mut Vec<Fact>) {
    for d in diffs {
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

    /// A harness that runs an argv (`["/bin/zsh", "-lc", script]`) in a directory of its own: the
    /// view carries the whole command line, as the timeline shows it.
    #[test]
    fn an_argv_command_keeps_its_command_line_in_the_view() {
        let mut n = n();
        let e = events(n.update(
            &json!({"sessionUpdate": "tool_call", "toolCallId": "c9", "kind": "execute",
            "status": "in_progress", "title": "git push",
            "rawInput": {"command": ["/bin/zsh", "-lc", "git push origin main"], "cwd": "/w/sub"}}),
        ));
        let EventBody::ToolChanged { tool } = &e[0] else {
            panic!("{e:?}")
        };
        assert!(
            matches!(&tool.action, Some(Action::Unknown { summary }) if summary == "/bin/zsh -lc 'git push origin main'"),
            "{:?}",
            tool.action
        );
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
    fn another_message_id_is_another_message() {
        let mut n = n();
        let chunk = |id: &str, t: &str| {
            json!({"sessionUpdate": "agent_message_chunk", "messageId": id,
                "content": {"type": "text", "text": t}})
        };
        let mut started = 0;
        for u in [chunk("a", "x"), chunk("a", "y"), chunk("b", "z")] {
            started += events(n.update(&u))
                .iter()
                .filter(|e| matches!(e, EventBody::MessageStarted { .. }))
                .count();
        }
        assert_eq!(started, 2);
        // without ids the chunks are one message
        let mut n = super::tests::n();
        let plain = json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "x"}});
        let mut started = 0;
        for u in [plain.clone(), plain] {
            started += events(n.update(&u))
                .iter()
                .filter(|e| matches!(e, EventBody::MessageStarted { .. }))
                .count();
        }
        assert_eq!(started, 1);
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
            matches!(&bg[0], EventBody::Usage { report } if report.scope == UsageScope::Background && report.report_id.starts_with("usage:"))
        );
        let object = events(
            n.update(&json!({"sessionUpdate":"usage_update","used":1,"size":2,
            "_meta":{"_claude/origin":{"kind":"task-notification"}}})),
        );
        assert!(
            matches!(&object[0], EventBody::Usage{report} if report.scope == UsageScope::Background)
        );
        let c = n.update(&json!({"sessionUpdate": "config_option_update", "configOptions": [
            {"id": "model", "currentValue": "opus"}, {"id": "reasoning_effort", "currentValue": "high"}]}));
        assert_eq!(
            c,
            vec![Fact::Model("opus".into()), Fact::Effort("high".into())]
        );
        // non-USD and empty-currency cost reports are stripped so USD-only session cost stays exact
        let cny = events(n.update(
            &json!({"sessionUpdate": "usage_update", "used": 100, "size": 1000,
            "cost": {"amount": 3.5, "currency": "CNY"}}),
        ));
        assert!(matches!(&cny[0], EventBody::Usage { report } if report.cost.is_none()));
        let blank = events(n.update(
            &json!({"sessionUpdate": "usage_update", "used": 100, "size": 1000,
            "cost": {"amount": 1.0, "currency": ""}}),
        ));
        assert!(matches!(&blank[0], EventBody::Usage { report } if report.cost.is_none()));
        // empty-text chunk carrying _meta.usage still emits Final usage
        let meta_chunk = events(n.update(&json!({"sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": ""},
            "_meta": {"usage": {"inputTokens": 40, "outputTokens": 12, "thoughtTokens": 5}}})));
        assert!(matches!(&meta_chunk[0], EventBody::Usage { report }
            if report.measurement == Measurement::Final
                && report.tokens.input == Some(40)
                && report.tokens.output == Some(12)
                && report.tokens.reasoning == Some(5)));
        // cumulative PromptResponse.usage emits exact per-turn deltas
        let mut cum = Normalizer::new(
            "/work/proj",
            "a_2",
            steve_node_api::Epoch::new("g2").unwrap(),
        )
        .input_includes_cache(true)
        .prompt_cumulative_baseline((100, 30, 60, 0, 0));
        let delta = events(cum.prompt_response(&json!({
            "usage": {"inputTokens": 250, "outputTokens": 75, "cachedReadTokens": 160}
        })));
        assert!(matches!(&delta[0], EventBody::Usage { report }
            if report.tokens.input == Some(50)
                && report.tokens.output == Some(45)
                && report.tokens.cache_read == Some(100)));
        assert_eq!(cum.cumulative_prompt_tokens(), Some((250, 75, 160, 0, 0)));
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

    #[test]
    fn pascal_case_command_line_and_path_normalize() {
        let mut n = n();
        let exec = events(n.update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "name": "run_command",
            "kind": "execute",
            "status": "pending",
            "title": "git status",
            "rawInput": {"CommandLine": "git status", "WaitMsBeforeAsync": 5000}
        })));
        let EventBody::ToolChanged { tool } = &exec[0] else {
            panic!("{exec:?}")
        };
        assert_eq!(tool.name, "run_command");
        assert!(
            matches!(&tool.action, Some(Action::Exec { command, .. }) if command == "git status")
        );
        let read = events(n.update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_2",
            "name": "read_file",
            "kind": "read",
            "status": "pending",
            "title": "Read file",
            "rawInput": {"Path": "/work/proj/Cargo.toml"}
        })));
        let EventBody::ToolChanged { tool } = &read[0] else {
            panic!("{read:?}")
        };
        assert_eq!(tool.target.as_deref(), Some("Cargo.toml"));
        assert_eq!(
            n.fact("call_2").unwrap().paths,
            vec!["/work/proj/Cargo.toml"]
        );
    }

    #[test]
    fn antigravity_and_claude_steve_mcp_prefixes_normalize_without_double_stripping() {
        let mut n = n();
        // Antigravity single-underscore prefix -> Steve origin + bare tool name.
        let agy = crate::permission::asked(
            &mut n,
            &json!(1),
            &json!({
                "sessionId": "s",
                "toolCall": {
                    "toolCallId": "p_agy",
                    "name": "mcp_steve_steve_bots_list",
                    "kind": "other",
                    "status": "pending",
                    "rawInput": {}
                },
                "options": [{"optionId": "allow", "kind": "allow_once"}]
            }),
        );
        assert_eq!(agy.fact.origin, ToolOrigin::Steve);
        assert_eq!(agy.fact.category, ToolCategory::Steve);
        assert_eq!(agy.fact.name, "steve_bots_list");

        // Claude Code double-underscore prefix -> Steve origin + bare tool name.
        let claude = crate::permission::asked(
            &mut n,
            &json!(2),
            &json!({
                "sessionId": "s",
                "toolCall": {
                    "toolCallId": "p_claude",
                    "_meta": {"claudeCode": {"toolName": "mcp__steve__steve_nodes_list"}},
                    "kind": "other",
                    "status": "pending",
                    "rawInput": {}
                },
                "options": [{"optionId": "allow", "kind": "allow_once"}]
            }),
        );
        assert_eq!(claude.fact.origin, ToolOrigin::Steve);
        assert_eq!(claude.fact.category, ToolCategory::Steve);
        assert_eq!(claude.fact.name, "steve_nodes_list");

        // Display title alone must NEVER grant Steve origin.
        let spoofed_title = crate::permission::asked(
            &mut n,
            &json!(3),
            &json!({
                "sessionId": "s",
                "toolCall": {
                    "toolCallId": "p_title",
                    "title": "mcp_steve_steve_bots_list",
                    "kind": "other",
                    "status": "pending",
                    "rawInput": {}
                },
                "options": [{"optionId": "allow", "kind": "allow_once"}]
            }),
        );
        assert_ne!(spoofed_title.fact.origin, ToolOrigin::Steve);

        // Structured server_name metadata must NOT strip a nested prefix inside tool name.
        let nested = crate::permission::asked(
            &mut n,
            &json!(4),
            &json!({
                "sessionId": "s",
                "toolCall": {
                    "toolCallId": "p_nested",
                    "name": "mcp_steve_steve_bots_list",
                    "kind": "other",
                    "status": "pending",
                    "rawInput": {"server_name": "steve"}
                },
                "options": [{"optionId": "allow", "kind": "allow_once"}]
            }),
        );
        assert_eq!(nested.fact.origin, ToolOrigin::Steve);
        assert_eq!(nested.fact.name, "mcp_steve_steve_bots_list");

        // External MCP server carrying the same suffix must stay External.
        let ext = crate::permission::asked(
            &mut n,
            &json!(5),
            &json!({
                "sessionId": "s",
                "toolCall": {
                    "toolCallId": "p_ext",
                    "name": "mcp__other__steve_bots_list",
                    "kind": "other",
                    "status": "pending",
                    "rawInput": {}
                },
                "options": [{"optionId": "allow", "kind": "allow_once"}]
            }),
        );
        assert_eq!(ext.fact.origin, ToolOrigin::External);
        assert_ne!(ext.fact.category, ToolCategory::Steve);
    }
}

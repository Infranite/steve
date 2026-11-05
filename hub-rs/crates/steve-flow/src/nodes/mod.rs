//! Node kinds as a registry: the built-in `trigger`, `filter`, `table`,
//! `flow`, `wait`, `approve`, `action`, `script`, `cmd`, `http` (S3) and `bot` (S4) here; the
//! vault-backed `http`, `cmd` with `STEVE_HOME`, the guarded `bot` and the channel actions are
//! registered through [`crate::FlowSetup`].

pub mod action;
pub mod approve;
pub mod bot;
pub mod cmd;
pub mod filter;
pub mod flow;
pub mod http;
pub mod script;
pub mod table;
pub mod trigger;
pub mod wait;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_store::NewDecision;

use crate::FlowCx;
use crate::expr::{self, Scope};
use crate::graph::{Graph, Node};
use crate::runs::RunRow;

/// The grant that lets an effectful node run without asking each time:
/// `grants(subject_type='flow', subject_id=<flow>, capability, resource_type, resource_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub capability: String,
    pub resource_type: String,
    pub resource_id: String,
}

impl Grant {
    pub fn new(capability: &str, resource_type: &str, resource_id: &str) -> Self {
        Self {
            capability: capability.into(),
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
        }
    }
}

/// How a waiting node is woken.
#[derive(Debug, Clone)]
pub struct WaitSpec {
    /// `delay | until | event | decision | flow`
    pub mode: &'static str,
    /// When the wait ends (`delay`, `until`) or times out (`event`).
    pub until: Option<i64>,
    /// `flow_waits.match`: `{source, type, when, timeout_label}` for events, `{child}` for flows.
    pub matcher: Value,
    /// A decision the engine opens in the same transaction (`subject_id` is filled in).
    pub decision: Option<NewDecision>,
    /// Step output while waiting.
    pub output: Value,
}

/// What a node did.
#[derive(Debug, Clone)]
pub enum NodeOutcome {
    /// Go on along the edge labelled `branch` (`None`: the unlabelled or first edge).
    Next {
        output: Value,
        branch: Option<String>,
    },
    Wait(Box<WaitSpec>),
    /// Worth one more try in 10 minutes (a model call failed).
    Retry(String),
    Fail(String),
}

impl NodeOutcome {
    pub fn next(output: Value) -> Self {
        Self::Next {
            output,
            branch: None,
        }
    }

    pub fn branch(output: Value, label: impl Into<String>) -> Self {
        Self::Next {
            output,
            branch: Some(label.into()),
        }
    }
}

/// What a node changes on its run besides its output; written with its step.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects {
    /// `flow_runs.outcome` (`need | item | linked | digest | ignored | report …`).
    pub outcome: Option<String>,
    pub label: Option<String>,
    pub item_id: Option<String>,
    /// `context.decision_id` (the event's `decision_id`).
    pub decision_id: Option<String>,
    /// `context.reason` (the event's triage reason).
    pub reason: Option<String>,
    /// `context.handled_by`.
    pub handled_by: Option<String>,
    pub cost: f64,
}

/// What a node runs with.
pub struct RunCx<'a> {
    pub flow: &'a FlowCx,
    /// The run as it was when the node started.
    pub run: RunRow,
    pub graph: &'a Graph,
    pub node: &'a Node,
    pub dry_run: bool,
    /// `{name, party_id, account_id}` of the event's sender, or `Null`.
    pub sender: Value,
    pub effects: Effects,
}

impl RunCx<'_> {
    pub fn ws(&self) -> &str {
        &self.run.workspace_id
    }

    /// The triggering event (`Null` for time and manual runs).
    pub fn event(&self) -> &Value {
        self.run.context.get("event").unwrap_or(&Value::Null)
    }

    pub fn event_id(&self) -> Option<&str> {
        self.run.event_id.as_deref()
    }

    pub fn scope<'s>(&'s self, input: &'s Value) -> Scope<'s> {
        Scope {
            event: self.run.context.get("event"),
            steps: self.run.context.get("steps"),
            sender: Some(&self.sender),
            input: Some(input),
        }
    }

    pub fn render(&self, tpl: &str, input: &Value) -> String {
        expr::render(tpl, &self.scope(input))
    }

    pub fn render_value(&self, v: &Value, input: &Value) -> Value {
        expr::render_value(v, &self.scope(input))
    }

    pub fn now(&self) -> i64 {
        self.flow.now()
    }
}

/// One kind of node.
#[async_trait]
pub trait NodeKind: Send + Sync {
    fn kind(&self) -> &'static str;

    /// Does running it reach outside Steve (send, pay, change another system)? Such a node runs only
    /// after an `approve` node on the run's path, a grant ([`NodeKind::grant`]) or an approval the
    /// engine asks for; restarts never replay it; dry runs skip it.
    fn effectful(&self, cfg: &Value) -> bool {
        let _ = cfg;
        false
    }

    /// The grant that lets an effectful node run without asking.
    fn grant(&self, cfg: &Value) -> Option<Grant> {
        let _ = cfg;
        None
    }

    /// Text appended to the question of the approval the engine asks for before this effectful
    /// node (e.g. the secrets an `http` step will release), so you know what you are approving.
    fn gate_note(&self, cx: &RunCx<'_>, cfg: &Value) -> Option<String> {
        let _ = (cx, cfg);
        None
    }

    /// Checks the node's `config`; the error is Chinese and goes into the `400` as it is.
    fn validate(&self, cfg: &Value) -> Result<(), String>;

    async fn run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome;

    /// The dry-run behaviour of a node that is not effectful. Default: the same as [`NodeKind::run`];
    /// nodes that write inside Steve (actions) override it to only describe what they would do.
    async fn dry_run(&self, cx: &mut RunCx<'_>, cfg: &Value, input: &Value) -> NodeOutcome {
        self.run(cx, cfg, input).await
    }
}

/// The node kinds by name.
#[derive(Clone, Default)]
pub struct NodeRegistry(HashMap<String, Arc<dyn NodeKind>>);

impl NodeRegistry {
    /// The built-in kinds, with the `action` node over `actions`.
    pub fn builtin(actions: action::Actions) -> Self {
        let mut r = Self::default();
        r.add(Arc::new(trigger::Trigger));
        r.add(Arc::new(filter::Filter));
        r.add(Arc::new(table::Table));
        r.add(Arc::new(flow::SubFlow));
        r.add(Arc::new(wait::Wait));
        r.add(Arc::new(approve::Approve));
        r.add(Arc::new(action::ActionNode::new(actions)));
        // without the vault's guard until the integrator registers `BotNode::new(guard)`
        r.add(Arc::new(bot::BotNode::default()));
        r.add(Arc::new(script::Script));
        // the integrator replaces these with `Cmd::new(steve_home)` and `Http::new(vault)`
        r.add(Arc::new(cmd::Cmd::default()));
        r.add(Arc::new(http::Http::default()));
        r
    }

    /// Adds or replaces a kind.
    pub fn add(&mut self, k: Arc<dyn NodeKind>) {
        self.0.insert(k.kind().to_string(), k);
    }

    pub fn get(&self, kind: &str) -> Option<&Arc<dyn NodeKind>> {
        self.0.get(kind)
    }

    pub fn kinds(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.0.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Is this node of `g` effectful?
    pub fn effectful(&self, n: &Node) -> bool {
        self.get(&n.kind).is_some_and(|k| k.effectful(&n.config))
    }
}

// ---------- small config helpers shared by the node kinds ----------

/// A string list field (absent = empty); `Err` names the field.
pub(crate) fn str_list(cfg: &Value, key: &str) -> Result<Vec<String>, String> {
    match cfg.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(String::from)
                    .ok_or_else(|| steve_i18n::t!("flow.node.text_list_only", key = key))
            })
            .collect(),
        Some(_) => Err(steve_i18n::t!("flow.node.text_list_only", key = key)),
    }
}

/// An optional string field.
pub(crate) fn opt_str<'v>(cfg: &'v Value, key: &str) -> Result<Option<&'v str>, String> {
    match cfg.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(steve_i18n::t!("flow.node.text_only", key = key)),
    }
}

/// A required text field of a node's config, for its two errors (each a whole sentence).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Required {
    /// `flow.flow_id`
    Flow,
    /// `approve.question`
    Question,
    /// `notify.text`
    Reminder,
    /// `script.code`
    Script,
    /// `http.url`
    Url,
    /// `cmd.command`
    Command,
    /// `cmd.project_id`
    Project,
    /// `bot.instruction`
    Instruction,
}

impl Required {
    fn missing(self) -> String {
        use steve_i18n::t;
        match self {
            Required::Flow => t!("flow.node.missing_flow"),
            Required::Question => t!("flow.node.missing_question"),
            Required::Reminder => t!("flow.node.missing_reminder"),
            Required::Script => t!("flow.node.missing_script"),
            Required::Url => t!("flow.node.missing_url"),
            Required::Command => t!("flow.node.missing_command"),
            Required::Project => t!("flow.node.missing_project"),
            Required::Instruction => t!("flow.node.missing_instruction"),
        }
    }

    fn too_long(self, max: usize) -> String {
        use steve_i18n::t;
        match self {
            Required::Flow => t!("flow.node.too_long_flow", max = max),
            Required::Question => t!("flow.node.too_long_question", max = max),
            Required::Reminder => t!("flow.node.too_long_reminder", max = max),
            Required::Script => t!("flow.node.too_long_script", max = max),
            Required::Url => t!("flow.node.too_long_url", max = max),
            Required::Command => t!("flow.node.too_long_command", max = max),
            Required::Project => t!("flow.node.too_long_project", max = max),
            Required::Instruction => t!("flow.node.too_long_instruction", max = max),
        }
    }
}

/// A required, non-empty string field of at most `max` characters.
pub(crate) fn req_str<'v>(
    cfg: &'v Value,
    key: &str,
    field: Required,
    max: usize,
) -> Result<&'v str, String> {
    let s = opt_str(cfg, key)?.map(str::trim).unwrap_or("");
    if s.is_empty() {
        return Err(field.missing());
    }
    if s.chars().count() > max {
        return Err(field.too_long(max));
    }
    Ok(s)
}

/// The output of a node that a dry run did not execute.
pub fn skipped(reason: &str) -> Value {
    json!({ "skipped": reason })
}

/// `skipped` of effectful nodes in dry runs.
pub const DRY_EFFECT: &str = "对外动作，试运行时不执行";
/// `skipped` of actions that would write inside Steve.
pub const DRY_WRITE: &str = "试运行时不执行";

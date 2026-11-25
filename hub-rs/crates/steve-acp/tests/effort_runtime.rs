//! Runtime ACP `session::open` integration tests covering all three effort strategies:
//! 1. `AcpOption`: ordinal mapping on a 6-level Codex-shaped adapter (`high -> xhigh`, `max -> ultra`)
//!    plus typed `EffortMismatch` when the adapter substitutes another level.
//! 2. `ModelVariant`: rule-based sibling model switching (`gemini-4-neon` + `max -> gemini-4-neon-xhigh`),
//!    including `agy`-style normalized model+thought_level responses.
//! 3. `Unsupported`: Antigravity `gemini-pro-orchestrator-neon` (omits `thought_level`) + `high`
//!    succeeds cleanly at model default and records `EffortStrategy::Unsupported` without stalling.

use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use serde_json::json;
use steve_acp::rpc::{Conn, Spawn};
use steve_acp::session::{OpenConfig, Reports, open};
use steve_harness_spec::{EffortMapping, EffortMode, EffortStrategy, ReasoningEffort};

#[derive(Default)]
struct Recorder {
    model: Mutex<Option<String>>,
    effort: Mutex<Option<ReasoningEffort>>,
    mapping: Mutex<Option<EffortMapping>>,
}

impl Reports for Recorder {
    fn model(&self, model: &str) {
        *self.model.lock().unwrap() = Some(model.to_owned());
    }
    fn effort(&self, effort: ReasoningEffort) {
        *self.effort.lock().unwrap() = Some(effort);
    }
    fn effort_mapping(&self, mapping: &EffortMapping) {
        *self.mapping.lock().unwrap() = Some(mapping.clone());
    }
}

fn spawn_script(dir: &std::path::Path, name: &str, body: &str) -> std::sync::Arc<Conn> {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    Conn::spawn(
        Spawn {
            prefix: None,
            program: path,
            args: vec![],
            cwd: dir.to_path_buf(),
            env: vec![],
            log_tag: format!("[test {name}]"),
            clean_env: false,
        },
        |_, _| {},
        || {},
    )
    .unwrap()
}

#[tokio::test]
async fn acp_option_maps_codex_six_levels_and_reports_typed_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let script = r#"#!/usr/bin/env node
const rl = require("readline").createInterface({ input: process.stdin });
const send = (o) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...o }) + "\n");
let model = "gpt-6.2-sol";
let effort = "medium";
const opts = () => [
  { id: "model", category: "model", type: "select", currentValue: model, options: [
    { value: "gpt-6.2-sol", name: "gpt-6.2-sol" },
    { value: "claude-haiku-4-5-20251001", name: "Haiku" }
  ]},
  { id: "reasoning_effort", category: "thought_level", type: "select", currentValue: effort, options: [
    { value: "low" }, { value: "medium" }, { value: "high" },
    { value: "xhigh" }, { value: "max" }, { value: "ultra" }
  ]}
];
rl.on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize") {
    send({ id: m.id, result: { protocolVersion: 1, agentCapabilities: {} } });
  } else if (m.method === "session/new") {
    send({ id: m.id, result: { sessionId: "codex-1", configOptions: opts() } });
  } else if (m.method === "session/set_config_option") {
    if (m.params.configId === "model") model = m.params.value;
    if (m.params.configId === "reasoning_effort") {
      effort = m.params.value === "ultra" ? "low" : m.params.value;
    }
    send({ id: m.id, result: { configOptions: opts() } });
  }
});
"#;
    // 1. High -> xhigh on 6-level Codex
    let conn = spawn_script(dir.path(), "codex-ok.js", script);
    let rec = Recorder::default();
    let opened = open(
        &conn,
        OpenConfig {
            harness: "codex",
            client_version: "test",
            cwd: dir.path().to_str().unwrap(),
            resume: None,
            mcp_servers: json!([]),
            mode: None,
            model: Some("gpt-6.2-sol"),
            effort: ReasoningEffort::High,
            effort_mode: EffortMode::AcpOption,
            system_prompt: None,
        },
        None,
        &rec,
    )
    .await
    .expect("high maps to xhigh");
    conn.kill();
    let mapping = opened.effort_mapping.expect("mapping recorded");
    assert_eq!(mapping.requested, ReasoningEffort::High);
    assert_eq!(mapping.strategy, EffortStrategy::AcpOption);
    assert_eq!(mapping.native_effort.as_deref(), Some("xhigh"));
    assert_eq!(mapping.effective, ReasoningEffort::High);
    assert!(mapping.supported);
    assert_eq!(*rec.effort.lock().unwrap(), Some(ReasoningEffort::High));
    assert_eq!(rec.mapping.lock().unwrap().as_ref(), Some(&mapping));

    // 2. Max -> ultra, script substitutes "low" -> typed effort_mismatch
    let conn = spawn_script(dir.path(), "codex-mismatch.js", script);
    let rec = Recorder::default();
    let err = open(
        &conn,
        OpenConfig {
            harness: "codex",
            client_version: "test",
            cwd: dir.path().to_str().unwrap(),
            resume: None,
            mcp_servers: json!([]),
            mode: None,
            model: Some("gpt-6.2-sol"),
            effort: ReasoningEffort::Max,
            effort_mode: EffortMode::AcpOption,
            system_prompt: None,
        },
        None,
        &rec,
    )
    .await
    .unwrap_err();
    conn.kill();
    let data = err.data.expect("structured error metadata");
    assert_eq!(data["kind"], "effort_mismatch");
    assert_eq!(data["requested"], "max");
    assert_eq!(data["expected"], "ultra");
    assert_eq!(data["actual"], "low");
}

#[tokio::test]
async fn antigravity_unsupported_model_falls_back_to_default_without_error() {
    let dir = tempfile::tempdir().unwrap();
    // Mirrors empirical `agy acp` 1.5.17 behavior: default model `gemini-4-neon` has
    // `thought_level`, while switching to `gemini-pro-orchestrator-neon` removes it.
    let script = r#"#!/usr/bin/env node
const rl = require("readline").createInterface({ input: process.stdin });
const send = (o) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...o }) + "\n");
let model = "gemini-4-neon";
const opts = () => {
  const base = [{ id: "model", category: "model", type: "select", currentValue: model, options: [
    { value: "gemini-4-neon" },
    { value: "gemini-pro-orchestrator-neon" }
  ]}];
  if (model === "gemini-4-neon") {
    base.push({ id: "thought_level", category: "thought_level", type: "select", currentValue: "high", options: [
      { value: "low" }, { value: "medium" }, { value: "high" }, { value: "xhigh" }
    ]});
  }
  return base;
};
rl.on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize") {
    send({ id: m.id, result: { protocolVersion: 1, agentCapabilities: {} } });
  } else if (m.method === "session/new") {
    send({ id: m.id, result: { sessionId: "agy-1", configOptions: opts() } });
  } else if (m.method === "session/set_config_option") {
    if (m.params.configId === "model") model = m.params.value;
    send({ id: m.id, result: { configOptions: opts() } });
  }
});
"#;
    let conn = spawn_script(dir.path(), "agy-orch.js", script);
    let rec = Recorder::default();
    let opened = open(
        &conn,
        OpenConfig {
            harness: "antigravity",
            client_version: "test",
            cwd: dir.path().to_str().unwrap(),
            resume: None,
            mcp_servers: json!([]),
            mode: None,
            model: Some("gemini-pro-orchestrator-neon"),
            effort: ReasoningEffort::High,
            effort_mode: EffortMode::AcpOption,
            system_prompt: None,
        },
        None,
        &rec,
    )
    .await
    .expect("unsupported model must never fail the turn");
    conn.kill();
    let mapping = opened.effort_mapping.expect("mapping recorded");
    assert_eq!(mapping.requested, ReasoningEffort::High);
    assert_eq!(mapping.strategy, EffortStrategy::Unsupported);
    assert!(!mapping.supported);
    assert_eq!(mapping.native_effort, None);
    assert_eq!(mapping.target_model, None);
    assert_eq!(mapping.effective, ReasoningEffort::Default);
    assert_eq!(
        *rec.model.lock().unwrap(),
        Some("gemini-pro-orchestrator-neon".to_owned())
    );
    assert_eq!(*rec.effort.lock().unwrap(), Some(ReasoningEffort::Default));
}

#[tokio::test]
async fn model_variant_strategy_switches_to_sibling_variant_and_accepts_agy_normalization() {
    let dir = tempfile::tempdir().unwrap();
    let script = r#"#!/usr/bin/env node
const rl = require("readline").createInterface({ input: process.stdin });
const send = (o) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...o }) + "\n");
let model = "gemini-4-neon";
let thought = "high";
rl.on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize") {
    send({ id: m.id, result: { protocolVersion: 1, agentCapabilities: {} } });
  } else if (m.method === "session/new") {
    send({ id: m.id, result: {
      sessionId: "var-1",
      models: {
        currentModelId: "gemini-4-neon-high",
        availableModels: [
          { modelId: "gemini-4-neon-low" },
          { modelId: "gemini-4-neon-medium" },
          { modelId: "gemini-4-neon-high" },
          { modelId: "gemini-4-neon-xhigh" },
          { modelId: "gpt-oss-120b-medium" }
        ]
      },
      configOptions: [{ id: "model", category: "model", type: "select", currentValue: model, options: [
        { value: "gemini-4-neon" },
        { value: "gpt-oss-120b-medium" }
      ]}]
    }});
  } else if (m.method === "session/set_config_option" && m.params.configId === "model") {
    if (m.params.value === "gemini-4-neon-xhigh") {
      model = "gemini-4-neon";
      thought = "xhigh";
      send({ id: m.id, result: { configOptions: [
        { id: "model", category: "model", currentValue: model },
        { id: "thought_level", category: "thought_level", currentValue: thought }
      ]}});
    } else {
      model = m.params.value;
      send({ id: m.id, result: { configOptions: [
        { id: "model", category: "model", currentValue: model }
      ]}});
    }
  }
});
"#;
    let conn = spawn_script(dir.path(), "variant.js", script);
    let rec = Recorder::default();
    let opened = open(
        &conn,
        OpenConfig {
            harness: "custom-variant",
            client_version: "test",
            cwd: dir.path().to_str().unwrap(),
            resume: None,
            mcp_servers: json!([]),
            mode: None,
            model: Some("gemini-4-neon"),
            effort: ReasoningEffort::Max,
            effort_mode: EffortMode::ModelVariant,
            system_prompt: None,
        },
        None,
        &rec,
    )
    .await
    .expect("variant switch should succeed");
    conn.kill();
    let mapping = opened.effort_mapping.expect("mapping recorded");
    assert_eq!(mapping.requested, ReasoningEffort::Max);
    assert_eq!(mapping.strategy, EffortStrategy::ModelVariant);
    assert_eq!(mapping.target_model.as_deref(), Some("gemini-4-neon-xhigh"));
    assert_eq!(mapping.effective, ReasoningEffort::Max);
    assert!(mapping.supported);
    assert_eq!(
        *rec.model.lock().unwrap(),
        Some("gemini-4-neon-xhigh".to_owned())
    );
    assert_eq!(*rec.effort.lock().unwrap(), Some(ReasoningEffort::Max));
}

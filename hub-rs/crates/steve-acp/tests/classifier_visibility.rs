//! End-to-end ACP adapter visibility tests for auto-mode classifier status and failures.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use steve_acp::StateDirRuntime;
use steve_acp::normalize::{Fact, Normalizer};
use steve_acp::rpc::{Conn, Incoming, Spawn};
use steve_acp::session::{OpenConfig, Reports};
use steve_node_api::{AUTO_MODE_CLASSIFIER_UNAVAILABLE, EffortMode, Epoch, EventBody, Severity};

struct NoopReports;
impl Reports for NoopReports {}

#[tokio::test]
async fn mock_claude_acp_classifier_unavailable_streams_typed_diagnostics_without_noise() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("mock-claude-acp.mjs");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env node
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin });
const send = (o) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...o }) + "\n");

rl.on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize") {
    return send({ id: m.id, result: { protocolVersion: 1, agentCapabilities: {} } });
  }
  if (m.method === "session/new") {
    return send({ id: m.id, result: { sessionId: "s-auto-1", modes: { currentModeId: "auto", availableModes: [{ id: "auto", name: "Auto" }] } } });
  }
  if (m.method === "session/set_mode") {
    return send({ id: m.id, result: {} });
  }
  if (m.method === "session/prompt") {
    // 1) claude-agent-acp 0.99.x stderr fallback for permission_check_status
    process.stderr.write(`Unexpected case: ${JSON.stringify({
      type: "system",
      subtype: "permission_check_status",
      tool_use_id: "toolu_1",
      status: "checking",
    })}\n`);
    process.stderr.write(`Unexpected case: ${JSON.stringify({
      type: "system",
      subtype: "permission_check_status",
      tool_use_id: "toolu_1",
      status: "done",
    })}\n`);
    // 2) forward-compatible ACP stdout permission_check_status
    send({
      method: "session/update",
      params: {
        sessionId: "s-auto-1",
        update: { sessionUpdate: "permission_check_status", tool_use_id: "toolu_2", status: "checking" },
      },
    });
    // 3) 3 consecutive Classifier unavailable denials + final Auto mode unavailable stop
    for (let i = 1; i <= 3; i++) {
      send({
        method: "session/update",
        params: {
          sessionId: "s-auto-1",
          update: {
            sessionUpdate: "tool_call_update",
            toolCallId: `toolu_${i}`,
            kind: "execute",
            title: "Bash",
            status: "failed",
            content: [{ type: "content", content: { type: "text", text: "Permission denied: Classifier unavailable" } }],
            _meta: {
              claudeCode: {
                toolName: "Bash",
                toolResponse: {
                  decisionReasonType: "classifier",
                  decisionReason: "Classifier unavailable",
                  message: "The server-side auto mode classifier gave no verdict",
                },
              },
            },
          },
        },
      });
    }
    send({
      method: "session/update",
      params: {
        sessionId: "s-auto-1",
        update: {
          sessionUpdate: "tool_call_update",
          toolCallId: "toolu_stop",
          kind: "execute",
          title: "Bash",
          status: "failed",
          content: [{ type: "content", content: { type: "text", text: "Permission denied: Auto mode unavailable — stopped after repeated responses with no safety verdict" } }],
          _meta: {
            claudeCode: {
              toolName: "Bash",
              toolResponse: {
                decisionReasonType: "classifier",
                decisionReason: "Auto mode unavailable — stopped after repeated responses with no safety verdict",
                hostCode: "auto_mode_stop_no_verdict",
              },
            },
          },
        },
      },
    });
    return send({ id: m.id, result: { stopReason: "end_turn" } });
  }
});
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

    let norm = Arc::new(Mutex::new(
        Normalizer::new(dir.path(), "att-mock", Epoch::new("e1").unwrap())
            .for_harness("claude-code"),
    ));
    let diagnostics = Arc::new(Mutex::new(Vec::<(Severity, String)>::new()));
    let norm_cb = norm.clone();
    let diag_cb = diagnostics.clone();

    let conn = Conn::spawn(
        Spawn {
            program: script,
            args: vec![],
            cwd: dir.path().to_path_buf(),
            env: vec![],
            log_tag: "[test-mock-claude]".into(),
            clean_env: false,
            prefix: None,
            harness: Some("claude-code".into()),
        },
        move |_, inc| {
            if let Incoming::Notification { method, params } = inc
                && method == "session/update"
                && let Some(u) = params.get("update")
            {
                let sid = params.get("sessionId").and_then(|v| v.as_str());
                let facts = norm_cb.lock().unwrap().update_for_session(sid, u);
                for f in facts {
                    if let Fact::Event(EventBody::Diagnostic { severity, code, .. }) = f {
                        diag_cb.lock().unwrap().push((severity, code));
                    }
                }
            }
        },
        || {},
    )
    .unwrap();

    let opened = steve_acp::session::open(
        &conn,
        OpenConfig {
            harness: "claude-code",
            client_version: "test",
            cwd: dir.path().to_str().unwrap(),
            resume: None,
            mcp_servers: json!([]),
            mode: Some(
                steve_acp::AcpConfig::from_env()
                    .mode_choice("claude-code", steve_node_api::PermissionMode::Auto),
            ),
            model: None,
            effort: steve_node_api::ReasoningEffort::Default,
            effort_mode: EffortMode::AcpOption,
            system_prompt: None,
        },
        None,
        &NoopReports,
    )
    .await
    .unwrap();
    norm.lock().unwrap().set_root_session(&opened.acp_id);

    let res = conn
        .request(
            "session/prompt",
            json!({"sessionId": opened.acp_id, "prompt": [{"type": "text", "text": "run test"}]}),
            Some(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(res["stopReason"], "end_turn");

    let got = diagnostics.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            (Severity::Warning, AUTO_MODE_CLASSIFIER_UNAVAILABLE.into()),
            (Severity::Error, AUTO_MODE_CLASSIFIER_UNAVAILABLE.into()),
        ],
        "expected exactly warning then error classifier diagnostics and zero unsupported_update noise"
    );
    conn.kill();
    conn.wait_exit().await;
}

/// Live end-to-end auto-mode classifier verification against the locally installed Claude Code
/// adapter in an isolated temporary `CLAUDE_CONFIG_DIR` using the declared `state.login` rules
/// (shared credential store via `CLAUDE_SECURESTORAGE_CONFIG_DIR` + synced `settings.json` login
/// and transport/classifier entries, read-only from the host's `~/.claude/settings.json`).
/// Ignored by default because it requires live model/gateway network access and a signed-in
/// Claude Code installation on the host.
#[tokio::test]
#[ignore = "requires live Claude Code installation and model/gateway network access"]
async fn live_claude_code_auto_mode_classifier_in_isolated_dir() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let cfg_dir = dir.path().join("claude-state");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&cfg_dir).unwrap();

    let cfg = steve_acp::AcpConfig::from_env();
    let spec = steve_acp::adapters::spec("claude-code", None);
    let sd = spec.properties.state.as_ref().expect("claude-code state");
    if let Some(host_dir) = sd.owner_source(&cfg.user_home) {
        let host_settings = host_dir.join(&sd.login.settings);
        if let Ok(raw) = std::fs::read(&host_settings)
            && let Ok(serde_json::Value::Object(src)) = serde_json::from_slice(&raw)
        {
            let mut dst = serde_json::Map::new();
            for entry in &sd.login.entries {
                match entry.split_once('.') {
                    None => {
                        if let Some(v) = src.get(entry) {
                            dst.insert(entry.clone(), v.clone());
                        }
                    }
                    Some((head, pat)) => {
                        if let Some(serde_json::Value::Object(sub)) = src.get(head) {
                            let prefix = pat.strip_suffix('*');
                            for (k, v) in sub {
                                let matched = match prefix {
                                    Some(p) => k.starts_with(p),
                                    None => k == pat,
                                };
                                if matched {
                                    let entry_obj = dst
                                        .entry(head.to_string())
                                        .or_insert_with(|| {
                                            serde_json::Value::Object(Default::default())
                                        })
                                        .as_object_mut()
                                        .unwrap();
                                    entry_obj.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
            if !dst.is_empty() {
                std::fs::write(
                    cfg_dir.join(&sd.login.settings),
                    serde_json::to_vec_pretty(&serde_json::Value::Object(dst)).unwrap(),
                )
                .unwrap();
            }
        }
    }

    let (program, args) = cfg
        .adapter_command_for_mode("claude-code", Some(steve_node_api::PermissionMode::Auto))
        .expect("claude-code adapter command must be installed");
    let mut env = cfg.adapter_env("claude-code");
    env.push(("CLAUDE_CONFIG_DIR".into(), cfg_dir.into_os_string()));
    if !sd.login.store_env.is_empty() {
        let store = std::env::var_os(&sd.login.store_env)
            .or_else(|| std::env::var_os(&sd.env))
            .unwrap_or_default();
        env.push((sd.login.store_env.clone().into(), store));
    }

    let norm = Arc::new(Mutex::new(
        Normalizer::new(&work, "att-live", Epoch::new("live").unwrap()).for_harness("claude-code"),
    ));
    let classifier_errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let norm_cb = norm.clone();
    let err_cb = classifier_errors.clone();

    let conn = Conn::spawn(
        Spawn {
            program,
            args,
            cwd: work.clone(),
            env,
            log_tag: "[live-claude-auto]".into(),
            clean_env: false,
            prefix: cfg.adapter_prefix("claude-code"),
            harness: Some("claude-code".into()),
        },
        move |_, inc| {
            if let Incoming::Notification { method, params } = inc
                && method == "session/update"
                && let Some(u) = params.get("update")
            {
                let sid = params.get("sessionId").and_then(|v| v.as_str());
                let facts = norm_cb.lock().unwrap().update_for_session(sid, u);
                for f in facts {
                    if let Fact::Event(EventBody::Diagnostic {
                        code, safe_text, ..
                    }) = f
                        && code == AUTO_MODE_CLASSIFIER_UNAVAILABLE
                    {
                        err_cb.lock().unwrap().push(safe_text);
                    }
                }
            }
        },
        || {},
    )
    .unwrap();

    let opened = steve_acp::session::open(
        &conn,
        OpenConfig {
            harness: "claude-code",
            client_version: "live-test",
            cwd: work.to_str().unwrap(),
            resume: None,
            mcp_servers: json!([]),
            mode: Some(cfg.mode_choice("claude-code", steve_node_api::PermissionMode::Auto)),
            model: None,
            effort: steve_node_api::ReasoningEffort::Default,
            effort_mode: EffortMode::AcpOption,
            system_prompt: None,
        },
        None,
        &NoopReports,
    )
    .await
    .unwrap();
    norm.lock().unwrap().set_root_session(&opened.acp_id);

    let res = conn
        .request(
            "session/prompt",
            json!({
                "sessionId": opened.acp_id,
                "prompt": [{"type": "text", "text": "Run `python3 -c 'print(1+1)'` using Bash and report the output."}]
            }),
            Some(Duration::from_secs(90)),
        )
        .await
        .unwrap();
    assert_eq!(res["stopReason"], "end_turn");
    assert!(
        classifier_errors.lock().unwrap().is_empty(),
        "classifier unavailable diagnostics: {:?}",
        classifier_errors.lock().unwrap()
    );
    conn.kill();
    conn.wait_exit().await;
}

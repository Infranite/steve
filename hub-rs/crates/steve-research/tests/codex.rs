//! Research on Codex against a scripted fake `codex`: the command line, an answer with a cited read,
//! the guard on shell commands, a failing model falling back, and no session to resume. No model is
//! called.

use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use steve_research::*;

struct Env {
    _t: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

fn env() -> Env {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let home = root.join("home");
    let proj = root.join("proj");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_rsa"), "PRIVATE").unwrap();
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("src/lib.rs"), "fn add() {}\n").unwrap();
    Env {
        _t: t,
        root,
        home,
        proj,
    }
}

/// Shell printing these lines (no here-documents).
fn emit(lines: &[serde_json::Value]) -> String {
    lines
        .iter()
        .map(|l| format!("printf '%s\\n' '{}'", l.to_string().replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A fake `codex`: logs its arguments and environment token to files, then runs `body`.
fn codex(e: &Env, body: &str) -> PathBuf {
    let p = e.root.join("codex");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{args}'\nprintf '@@END@@\\n' >> '{args}'\nprintf '%s' \"$STEVE_RESEARCH_MCP_TOKEN\" > '{tok}'\n{body}\n",
        args = e.root.join("args.txt").display(),
        tok = e.root.join("token.txt").display(),
    );
    std::fs::write(&p, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn cfg(e: &Env, codex: &Path) -> ResearchConfig {
    // the Claude Code executable must not run at all
    let mut c = ResearchConfig::new("/nonexistent/claude", e.root.join("scratch"));
    c.models = vec!["claude-sonnet-5-5".into()];
    c.home = e.home.clone();
    c.claude_dir = e.root.join("claude");
    c.sandbox = SandboxMode::Off;
    c.codex_executable = Some(codex.to_path_buf());
    c
}

fn req(e: &Env, models: &[&str]) -> ResearchRequest {
    let mut r = ResearchRequest::new("ws", "add 怎么工作？", "prompt");
    r.cwd = Some(e.proj.clone());
    r.harness = "codex".into();
    r.models = Some(models.iter().map(|m| m.to_string()).collect());
    r.effort = Some("low".into());
    r
}

fn cmd(id: &str, command: &str, done: bool) -> serde_json::Value {
    json!({"type": if done { "item.completed" } else { "item.started" }, "item": {"id": id, "type": "command_execution", "command": command, "aggregated_output": if done { "fn add() {}\n" } else { "" }, "exit_code": if done { json!(0) } else { json!(null) }, "status": if done { "completed" } else { "in_progress" }}})
}
fn msg(id: &str, t: &str) -> serde_json::Value {
    json!({"type": "item.completed", "item": {"id": id, "type": "agent_message", "text": t}})
}
fn done() -> serde_json::Value {
    json!({"type": "turn.completed", "usage": {"input_tokens": 1000, "cached_input_tokens": 400, "output_tokens": 50}})
}

fn runs(e: &Env) -> Vec<Vec<String>> {
    std::fs::read_to_string(e.root.join("args.txt"))
        .unwrap_or_default()
        .split("@@END@@\n")
        .filter(|r| !r.trim().is_empty())
        .map(|r| r.lines().map(String::from).collect())
        .collect()
}

#[tokio::test]
async fn a_codex_run_answers_with_citations_and_its_usage() {
    let e = env();
    let body = emit(&[
        json!({"type": "thread.started", "thread_id": "th-1"}),
        json!({"type": "turn.started"}),
        msg("i0", "我先看看代码"),
        cmd("i1", "/bin/zsh -lc 'cat src/lib.rs'", false),
        cmd("i1", "/bin/zsh -lc 'cat src/lib.rs'", true),
        msg("i2", "add 什么都不做（src/lib.rs）。"),
        done(),
    ]);
    let exe = codex(&e, &body);
    let r = Researcher::new(cfg(&e, &exe));
    let mut q = req(&e, &["gpt-6-luna"]);
    q.mcp = Some(McpAccess {
        url: "http://127.0.0.1:9/mcp".into(),
        token: "run-token-xyz".into(),
    });
    let out = r.start(q).unwrap().finish().await;
    assert_eq!(out.status, RunStatus::Done, "{out:?}");
    assert_eq!(out.answer, "add 什么都不做（src/lib.rs）。");
    assert_eq!(out.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(out.files_read, vec!["src/lib.rs".to_string()]);
    assert_eq!(out.tools.len(), 1);
    assert_eq!(
        (out.tools[0].name.as_str(), out.tools[0].ok),
        ("Shell", Some(true))
    );
    assert_eq!(out.tools[0].brief, "cat src/lib.rs");
    assert!(!out.sandboxed);
    // no harness session to resume: a follow-up starts fresh
    assert_eq!(out.claude_session, None);
    let res = out.result.unwrap();
    assert_eq!(res["harness"], "codex");
    assert_eq!(res["usage"]["input_tokens"], 1000);
    let a = &runs(&e)[0];
    assert_eq!(a[..2], ["exec".to_string(), "--json".to_string()]);
    let at = |f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
    assert_eq!(at("-s").as_deref(), Some("read-only"));
    assert_eq!(at("-m").as_deref(), Some("gpt-6-luna"));
    assert_eq!(at("-C").as_deref(), Some(&*e.proj.to_string_lossy()));
    assert_eq!(a.last().map(String::as_str), Some("prompt"));
    assert!(a.iter().any(|x| x == "model_reasoning_effort=\"low\""));
    // the run's MCP token reaches Codex through the environment, never the command line
    assert!(!a.iter().any(|x| x.contains("run-token-xyz")));
    assert_eq!(
        std::fs::read_to_string(e.root.join("token.txt")).unwrap(),
        "run-token-xyz"
    );
}

/// A shell command naming a secret ends the run at once as `violated`.
#[tokio::test]
async fn the_guard_stops_a_command_on_a_secret() {
    let e = env();
    let survived = e.root.join("survived");
    let body = format!(
        "{}\nsleep 5\ntouch '{}'",
        emit(&[
            msg("i0", "我看看"),
            cmd("i1", "/bin/zsh -lc 'cat ~/.ssh/id_rsa'", false),
        ]),
        survived.display()
    );
    let exe = codex(&e, &body);
    let r = Researcher::new(cfg(&e, &exe));
    let t0 = Instant::now();
    let out = r.start(req(&e, &["gpt-6-luna"])).unwrap().finish().await;
    assert!(t0.elapsed() < Duration::from_secs(4));
    assert!(
        matches!(&out.status, RunStatus::Violated(Violation::Sensitive { tool, .. }) if tool == "Shell"),
        "{:?}",
        out.status
    );
    tokio::time::sleep(Duration::from_millis(5200)).await;
    assert!(!survived.exists(), "the process group was killed");
}

/// A model Codex refuses falls back to the next; no model at all leaves `-m` out.
#[tokio::test]
async fn a_refused_model_falls_back_and_the_default_has_no_flag() {
    let e = env();
    let fail = emit(&[
        json!({"type": "error", "message": "requested model is not available"}),
        json!({"type": "turn.failed", "error": {"message": "requested model is not available"}}),
    ]);
    let ok = emit(&[msg("i0", "好的"), done()]);
    let body = format!(
        "for a; do case \"$p\" in -m) m=\"$a\";; esac; p=\"$a\"; done\nif [ \"$m\" = broken ]; then\n{fail}\nelse\n{ok}\nfi"
    );
    let exe = codex(&e, &body);
    let r = Researcher::new(cfg(&e, &exe));
    let out = r
        .start(req(&e, &["broken", "gpt-6-sol"]))
        .unwrap()
        .finish()
        .await;
    assert_eq!(out.status, RunStatus::Done, "{out:?}");
    assert_eq!(out.model.as_deref(), Some("gpt-6-sol"));
    assert_eq!(out.answer, "好的");
    let out = r.start(req(&e, &[])).unwrap().finish().await;
    assert_eq!(out.status, RunStatus::Done);
    let last = runs(&e).pop().unwrap();
    assert!(!last.iter().any(|x| x == "-m"), "{last:?}");
}

/// Without a codex executable the run fails clearly instead of running Claude Code.
#[tokio::test]
async fn no_codex_executable() {
    let e = env();
    let mut c = cfg(&e, Path::new("/x"));
    c.codex_executable = None;
    let out = Researcher::new(c)
        .start(req(&e, &["gpt-6-luna"]))
        .unwrap()
        .finish()
        .await;
    assert!(
        matches!(&out.status, RunStatus::Failed(m) if m.contains("codex")),
        "{:?}",
        out.status
    );
}

/// Attachments on Codex: copied into the run's files directory, named in the prompt, images given as
/// `-i <file>`; the guard lets the run read the copies.
#[tokio::test]
async fn a_codex_run_gets_the_attachments_and_images() {
    let e = env();
    let files_root = e.root.join("files");
    let png = e.root.join("blob1");
    std::fs::write(&png, b"\x89PNG").unwrap();
    let txt = e.root.join("blob2");
    std::fs::write(&txt, "需求").unwrap();
    let copy = files_root.join("tr_att/需求.txt").display().to_string();
    let body = format!(
        "for a; do case \"$p\" in -i) cat \"$a\" > '{img}';; esac; p=\"$a\"; done\n{}",
        emit(&[
            cmd("i1", &format!("cat '{copy}'"), false),
            cmd("i1", &format!("cat '{copy}'"), true),
            msg("i2", "看过了"),
            done(),
        ]),
        img = e.root.join("seen.png").display()
    );
    let exe = codex(&e, &body);
    let mut c = cfg(&e, &exe);
    c.files_root = files_root.clone();
    let mut q = req(&e, &["gpt-6-luna"]);
    q.run_id = Some("tr_att".into());
    q.attachments = vec![
        Attachment {
            path: png,
            name: "截图.png".into(),
            mime: "image/png".into(),
            size: 4,
            earlier: false,
        },
        Attachment {
            path: txt,
            name: "需求.txt".into(),
            mime: "text/plain".into(),
            size: 6,
            earlier: false,
        },
    ];
    let out = Researcher::new(c).start(q).unwrap().finish().await;
    assert_eq!(out.status, RunStatus::Done, "{out:?}");
    let a = &runs(&e)[0];
    let at = a.iter().position(|x| x == "-i").expect("-i");
    assert!(a[at + 1].ends_with("截图.png"), "{a:?}");
    let prompt = a.last().unwrap();
    assert!(prompt.contains("需求.txt"), "{prompt}");
    assert_eq!(std::fs::read(e.root.join("seen.png")).unwrap(), b"\x89PNG");
    assert_eq!(out.tools[0].ok, Some(true));
}

#[tokio::test]
async fn failed_mcp_diagnostics_are_classified_without_retaining_secrets() {
    let e = env();
    let body = emit(&[
        json!({"type":"item.completed","item":{
            "id":"i1","type":"mcp_tool_call","server":"steve","tool":"steve_item_read",
            "arguments":{"id":"ST-23"},"status":"failed",
            "error":{"message":"Permission denied: private-token-must-not-persist"}
        }}),
        msg("i2", "暂时无法查询。"),
        done(),
    ]);
    let exe = codex(&e, &body);
    let mut run = Researcher::new(cfg(&e, &exe))
        .start(req(&e, &["gpt-6-luna"]))
        .unwrap();
    let mut events = vec![];
    while let Some(event) = run.next_event().await {
        events.push(event);
    }
    let out = run.finish().await;
    assert_eq!(out.tools.len(), 1);
    assert_eq!(out.tools[0].ok, Some(false));
    assert_eq!(
        out.tools[0].error,
        Some(stream::ToolError::PermissionDenied)
    );
    let serialized = serde_json::to_string(&out.tools).unwrap();
    assert!(serialized.contains("permission_denied"));
    assert!(!serialized.contains("private-token-must-not-persist"));
    assert!(!format!("{events:?}").contains("private-token-must-not-persist"));
}

//! Print runs against stand-in `claude` and `codex` scripts.
#![cfg(unix)]

use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use steve_print::{Harness, Image, PrintError, PrintRun, Ran, ToolMode, UsageScope};

/// A stand-in CLI running `body`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn claude<'a>(exe: &'a Path, cwd: &'a Path) -> PrintRun<'a> {
    PrintRun {
        harness: Harness::Claude,
        executable: exe,
        cwd,
        model: "m1",
        effort: None,
        head: Some("rules"),
        message: "数数",
        tools: ToolMode::None,
        images: &[],
        timeout: Duration::from_secs(10),
    }
}

fn codex<'a>(exe: &'a Path, cwd: &'a Path) -> PrintRun<'a> {
    PrintRun {
        harness: Harness::Codex,
        model: "gpt-x",
        ..claude(exe, cwd)
    }
}

fn text(r: &Ran) -> &str {
    match &r.reply {
        Ok(r) => &r.text,
        Err(e) => panic!("{e:?}"),
    }
}

/// `echo` of one stream-json text delta.
fn delta(t: &str) -> String {
    format!(
        "echo '{}'",
        json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":t}}})
    )
}

/// Hears nothing; makes a Claude run stream.
const QUIET: steve_print::OnText<'static> = &|_| {};

const START: &str =
    r#"echo '{"type":"stream_event","event":{"type":"message_start","message":{}}}'"#;

#[tokio::test]
async fn claude_streams_text_before_it_exits() {
    let dir = tempfile::tempdir().unwrap();
    let body = [
        r#"case "$*" in *stream-json*--include-partial-messages*) ;; *) echo "not streaming: $*" >&2; exit 1;; esac"#.to_string(),
        delta("我先读"),
        START.into(),
        delta("一"),
        "sleep 0.3".into(),
        delta("二"),
        "sleep 0.3".into(),
        r#"echo '{"type":"result","is_error":false,"result":"一二","total_cost_usd":0.01,"usage":{"input_tokens":3,"output_tokens":2}}'"#.into(),
    ]
    .join("\n");
    let exe = script(dir.path(), "claude", &body);
    let t0 = Instant::now();
    let seen = Mutex::new(vec![]);
    let r = steve_print::run(
        &claude(&exe, dir.path()),
        Some(&|t: &str| seen.lock().unwrap().push((t.to_string(), t0.elapsed()))),
    )
    .await;
    assert_eq!(text(&r), "一二");
    let reply = r.reply.as_ref().unwrap();
    assert_eq!(
        reply.cost.as_ref().map(|m| m.decimal.as_str()),
        Some("0.01")
    );
    let seen = seen.into_inner().unwrap();
    assert_eq!(
        seen.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
        ["我先读", "一", "一二"],
        "a new message starts over"
    );
    assert!(
        t0.elapsed() - seen[1].1 >= Duration::from_millis(500),
        "the first part came before the run ended"
    );
    assert_eq!(r.usage.len(), 1);
    let u = &r.usage[0];
    assert_eq!(
        (u.model.as_deref(), u.scope, u.tokens.input, u.tokens.output),
        (Some("m1"), UsageScope::Main, Some(3), Some(2))
    );
    assert_eq!(u.cost.as_ref().map(|m| m.decimal.as_str()), Some("0.01"));
}

#[tokio::test]
async fn claude_json_gets_the_cache_friendly_argv() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("args.log");
    let exe = script(
        dir.path(),
        "claude",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\necho '{}'",
            log.display(),
            json!({"type":"result","is_error":false,"result":"ok","total_cost_usd":0.000829045,"modelUsage":{"claude-haiku-5-5":{"inputTokens":2,"outputTokens":4,"cacheReadInputTokens":11097,"cacheCreationInputTokens":5727,"costUSD":0.000829045}}})
        ),
    );
    let run = PrintRun {
        effort: Some("high"),
        ..claude(&exe, dir.path())
    };
    let r = steve_print::run(&run, None).await;
    assert_eq!(text(&r), "ok");
    let args = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "-p",
            "数数",
            "--output-format",
            "json",
            "--model",
            "m1",
            "--system-prompt",
            "rules",
            "--tools",
            "",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--effort",
            "high"
        ]
    );
    let u = &r.usage[0];
    assert_eq!(
        (
            u.model.as_deref(),
            u.scope,
            u.tokens.cache_read,
            u.tokens.cache_write
        ),
        (
            Some("claude-haiku-5-5"),
            UsageScope::All,
            Some(11_097),
            Some(5_727)
        )
    );
    assert_eq!(u.cost.as_ref().unwrap().decimal, "0.000829045");
}

#[tokio::test]
async fn claude_failures_are_typed() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    // a 503 that still says what it used
    let busy = script(
        cwd,
        "busy",
        r#"echo '{"type":"result","is_error":true,"result":"API Error: 503 upstream unavailable","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0}}'; exit 1"#,
    );
    let r = steve_print::run(&claude(&busy, cwd), None).await;
    let e = r.reply.unwrap_err();
    assert!(e.transient(), "{e:?}");
    assert_eq!(r.usage.len(), 1, "the failed try's usage is kept");
    let nope = script(cwd, "nope", "echo nope >&2; exit 2");
    for on_text in [None, Some(QUIET)] {
        let r = steve_print::run(&claude(&nope, cwd), on_text).await;
        assert!(
            matches!(&r.reply, Err(PrintError::Failed(e)) if e == "nope"),
            "{r:?}"
        );
    }
    let silent = script(cwd, "silent", "exit 0");
    let r = steve_print::run(&claude(&silent, cwd), None).await;
    assert!(matches!(r.reply, Err(PrintError::NoResult)), "{r:?}");
    let slow = script(cwd, "slow", "sleep 5");
    for on_text in [None, Some(QUIET)] {
        let run = PrintRun {
            timeout: Duration::from_millis(200),
            ..claude(&slow, cwd)
        };
        let t0 = Instant::now();
        let r = steve_print::run(&run, on_text).await;
        assert!(
            matches!(r.reply, Err(PrintError::Timeout(t)) if t == Duration::from_millis(200)),
            "{r:?}"
        );
        assert!(t0.elapsed() < Duration::from_secs(4));
    }
}

#[tokio::test]
async fn claude_gets_the_images_on_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let (log, stdin) = (dir.path().join("args.log"), dir.path().join("stdin"));
    let exe = script(
        dir.path(),
        "claude",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\ncat > '{}'\necho '{}'",
            log.display(),
            stdin.display(),
            json!({"type":"result","is_error":false,"result":"是一张图"})
        ),
    );
    let images = [Image {
        mime: "image/png".into(),
        bytes: b"image".to_vec(),
    }];
    let run = PrintRun {
        images: &images,
        ..claude(&exe, dir.path())
    };
    // no text wanted: still stream-json, the only layout that takes a message on stdin
    let r = steve_print::run(&run, None).await;
    assert_eq!(text(&r), "是一张图");
    let args = std::fs::read_to_string(&log).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[..3], ["-p", "--input-format", "stream-json"]);
    assert!(!args.contains(&"数数"), "the message is not an argument");
    assert_eq!(
        std::fs::read_to_string(&stdin).unwrap(),
        format!("{}\n", steve_print::user_message("数数", &images))
    );
}

/// A stand-in `codex`: logs its arguments, stdin and `-i` files, then prints `out.jsonl`.
fn fake_codex(dir: &Path) -> PathBuf {
    script(
        dir,
        "codex",
        &format!(
            "printf '%s\\n' \"$@\" > '{0}/codex.log'\ncat > '{0}/codex.stdin'\nfor a; do case \"$p\" in -i) cat \"$a\" >> '{0}/codex.images';; esac; p=\"$a\"; done\ncat '{0}/out.jsonl'",
            dir.display()
        ),
    )
}

fn after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::test]
async fn codex_runs_with_instructions_images_and_its_usage() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let exe = fake_codex(cwd);
    std::fs::write(
        cwd.join("out.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({"type": "item.completed", "item": {"type": "agent_message", "text": "先看看"}}),
            json!({"type": "item.completed", "item": {"type": "agent_message", "text": "是一张图"}}),
            json!({"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 40, "output_tokens": 5, "reasoning_output_tokens": 2}}),
        ),
    )
    .unwrap();
    let images = [Image {
        mime: "image/jpeg".into(),
        bytes: b"JPEGDATA".to_vec(),
    }];
    let run = PrintRun {
        images: &images,
        effort: Some("low"),
        ..codex(&exe, cwd)
    };
    let heard = Mutex::new(vec![]);
    let r = steve_print::run(
        &run,
        Some(&|t: &str| heard.lock().unwrap().push(t.to_string())),
    )
    .await;
    assert_eq!(text(&r), "是一张图");
    assert_eq!(*heard.lock().unwrap(), ["先看看", "是一张图"]);
    assert_eq!(r.reply.as_ref().unwrap().cost, None);
    let u = &r.usage[0];
    assert_eq!(
        (
            u.model.as_deref(),
            u.scope,
            u.tokens.input,
            u.tokens.cache_read,
            u.tokens.reasoning
        ),
        (Some("gpt-x"), UsageScope::Main, Some(60), Some(40), Some(2)),
        "input without the cached part"
    );
    let args: Vec<String> = std::fs::read_to_string(cwd.join("codex.log"))
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(after(&args, "-m").as_deref(), Some("gpt-x"));
    assert!(args.contains(&"model_reasoning_effort=\"low\"".to_string()));
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--disable" && w[1] == "shell_tool")
    );
    // the head is Codex's instructions, cached in a file; the message alone goes on stdin
    let instr = args
        .iter()
        .find_map(|x| x.strip_prefix("model_instructions_file="))
        .expect("instructions file");
    let instr: String = serde_json::from_str(instr).unwrap();
    assert!(
        instr.starts_with(
            &cwd.join("codex-instructions")
                .to_string_lossy()
                .into_owned()
        )
    );
    assert_eq!(std::fs::read_to_string(&instr).unwrap(), "rules");
    assert_eq!(
        std::fs::read_to_string(cwd.join("codex.stdin")).unwrap(),
        "数数"
    );
    // the image went in as an -i file, gone after the run
    let img = after(&args, "-i").expect("an -i argument");
    assert!(img.ends_with(".jpg"), "{img}");
    assert_eq!(
        std::fs::read_to_string(cwd.join("codex.images")).unwrap(),
        "JPEGDATA"
    );
    assert!(!Path::new(&img).exists(), "removed after the run");

    // with tools: the shell and Codex's instructions stay, the head goes before the message
    let run = PrintRun {
        tools: ToolMode::ReadOnly,
        ..codex(&exe, cwd)
    };
    steve_print::run(&run, None).await.reply.unwrap();
    let args = std::fs::read_to_string(cwd.join("codex.log")).unwrap();
    assert!(
        !args.contains("shell_tool") && !args.contains("model_instructions_file"),
        "{args}"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("codex.stdin")).unwrap(),
        "rules\n\n数数"
    );
}

#[tokio::test]
async fn codex_failures_are_typed() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();
    let exe = fake_codex(cwd);
    std::fs::write(
        cwd.join("out.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type": "error", "message": "requested model is not available"}),
            json!({"type": "turn.failed", "error": {"message": "requested model is not available"}}),
        ),
    )
    .unwrap();
    let r = steve_print::run(&codex(&exe, cwd), None).await;
    assert!(
        matches!(&r.reply, Err(PrintError::Harness { text, transient: false }) if text == "requested model is not available"),
        "{r:?}"
    );
    assert!(r.usage.is_empty());
    let dead = script(cwd, "dead", "cat > /dev/null; echo boom >&2; exit 3");
    let r = steve_print::run(&codex(&dead, cwd), None).await;
    assert!(
        matches!(&r.reply, Err(PrintError::Failed(e)) if e == "boom"),
        "{r:?}"
    );
    let slow = script(cwd, "slow", "sleep 5");
    let run = PrintRun {
        timeout: Duration::from_millis(200),
        ..codex(&slow, cwd)
    };
    assert!(matches!(
        steve_print::run(&run, None).await.reply,
        Err(PrintError::Timeout(_))
    ));
}

/// A supervised run leads its own process group, is told before it runs on, and numbers messages.
#[tokio::test]
async fn a_supervised_run_has_its_own_group_and_numbered_messages() {
    let dir = tempfile::tempdir().unwrap();
    let pgid = dir.path().join("pgid");
    let body = [
        format!("ps -o pgid= -p $$ | tr -d ' ' > '{}'", pgid.display()),
        START.into(),
        delta("先"),
        delta("看"),
        START.into(),
        delta("答案"),
        r#"echo '{"type":"result","is_error":false,"result":"答案"}'"#.into(),
    ]
    .join("\n");
    let exe = script(dir.path(), "claude", &body);
    let pid = Mutex::new(None);
    let seen = Mutex::new(vec![]);
    let r = steve_print::run_supervised(
        &claude(&exe, dir.path()),
        &|p| {
            *pid.lock().unwrap() = Some(p);
            true
        },
        &|m, t| seen.lock().unwrap().push((m, t.to_string())),
    )
    .await;
    assert_eq!(text(&r), "答案");
    let pid = pid.into_inner().unwrap().expect("told the pid");
    let group: u32 = std::fs::read_to_string(&pgid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(group, pid, "the harness leads its own group");
    assert_eq!(
        seen.into_inner().unwrap(),
        [(1, "先".into()), (1, "先看".into()), (2, "答案".into())]
    );

    // a process the host cannot record is killed and the run fails
    let exe = script(dir.path(), "slow", "sleep 30");
    let t0 = Instant::now();
    let r = steve_print::run_supervised(&claude(&exe, dir.path()), &|_| false, &|_, _| {}).await;
    assert!(matches!(r.reply, Err(PrintError::Io(_))), "{:?}", r.reply);
    assert!(t0.elapsed() < Duration::from_secs(5));
}

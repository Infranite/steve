//! The executor against a scripted fake harness: guard kills (layer 2), sandbox (layer 3, guard off),
//! budgets, stop, follow-up, fallback and concurrency. No model is called.

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
    std::fs::write(proj.join("keep.txt"), "orig").unwrap();
    Env {
        _t: t,
        root,
        home,
        proj,
    }
}

/// A fake harness: `body` is shell run with the harness's arguments; `$ARGS_FILE` gets them.
fn harness(e: &Env, body: &str) -> PathBuf {
    let p = e.root.join(format!("fake-{}.sh", uuid_like()));
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{args}'\n{body}\n",
        args = e.root.join("args.txt").display()
    );
    std::fs::write(&p, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

fn cfg(e: &Env, exe: &Path) -> ResearchConfig {
    let mut c = ResearchConfig::new(exe, e.root.join("scratch"));
    c.models = vec!["claude-sonnet-5-5".into()];
    c.home = e.home.clone();
    c.claude_dir = e.root.join("claude");
    c.sandbox = SandboxMode::Off;
    c
}

fn req(e: &Env, ws: &str) -> ResearchRequest {
    let mut r = ResearchRequest::new(ws, "add 怎么工作？", "prompt");
    r.cwd = Some(e.proj.clone());
    r
}

/// Shell that prints these stream-json lines (no here-documents: they need a writable temp dir).
fn emit(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| format!("printf '%s\\n' '{}'", l.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join("\n")
}

fn init() -> String {
    r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"claude-sonnet-5-5"}"#.into()
}
fn start(id: &str) -> String {
    format!(
        r#"{{"type":"stream_event","event":{{"type":"message_start","message":{{"id":"{id}"}}}}}}"#
    )
}
fn text(t: &str) -> String {
    serde_json::json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":t}}}).to_string()
}
fn tool(msg: &str, id: &str, name: &str, input: serde_json::Value) -> String {
    serde_json::json!({"type":"assistant","message":{"id":msg,"content":[{"type":"tool_use","id":id,"name":name,"input":input}]}}).to_string()
}
fn tool_result(id: &str) -> String {
    serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"1\tfn add() {}"}]}}).to_string()
}
fn result(t: &str) -> String {
    serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":t,"session_id":"sess-1","total_cost_usd":0.01}).to_string()
}

/// A tool call the guard must refuse: the run ends at once as `violated`, the process (and the
/// `sleep` it started) is gone, and the answer carries the violation mark.
async fn assert_violation(name: &str, input: serde_json::Value) {
    let e = env();
    let survived = e.root.join("survived");
    let body = format!(
        "{}\nsleep 5\ntouch '{}'",
        emit(&[
            init(),
            start("m1"),
            text("我先看看"),
            tool("m1", "t1", name, input.clone())
        ]),
        survived.display()
    );
    let exe = harness(&e, &body);
    let r = Researcher::new(cfg(&e, &exe));
    let t0 = Instant::now();
    let out = r.start(req(&e, "ws")).unwrap().finish().await;
    assert!(t0.elapsed() < Duration::from_secs(3), "killed promptly");
    assert!(
        matches!(out.status, RunStatus::Violated(_)),
        "{name}: {:?}",
        out.status
    );
    assert_eq!(out.status.as_str(), "violated");
    assert!(
        out.answer.ends_with("（我越界去读了不该读的东西，已停下）"),
        "{}",
        out.answer
    );
    assert!(out.answer.starts_with("我先看看"));
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(!survived.exists(), "harness process group was killed");
    assert_eq!(
        std::fs::read_to_string(e.proj.join("keep.txt")).unwrap(),
        "orig"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn guard_kills_bash_rm() {
    assert_violation("Bash", serde_json::json!({"command": "rm -rf keep.txt"})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn guard_kills_write_and_edit() {
    assert_violation(
        "Write",
        serde_json::json!({"file_path": "keep.txt", "content": "x"}),
    )
    .await;
    assert_violation(
        "Edit",
        serde_json::json!({"file_path": "keep.txt", "old_string": "orig", "new_string": "x"}),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn guard_kills_ssh_and_steve_reads() {
    assert_violation("Read", serde_json::json!({"file_path": "~/.ssh/id_rsa"})).await;
    assert_violation(
        "Read",
        serde_json::json!({"file_path": "~/.steve/data/steve.sqlite3"}),
    )
    .await;
    assert_violation("Grep", serde_json::json!({"pattern": "x", "path": "/etc"})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn done_streams_cites_and_offers() {
    let e = env();
    let lib = e.proj.join("src/lib.rs").display().to_string();
    let answer = "add 什么都不做，见 src/lib.rs:1。\nOFFER: {\"title\":\"实现 add\",\"goal\":\"让 add 返回和\"}";
    let body = emit(&[
        init(),
        start("m1"),
        tool("m1", "t1", "Read", serde_json::json!({"file_path": lib})),
        tool_result("t1"),
        start("m2"),
        text("add 什么都不做，"),
        text("见 src/lib.rs:1。"),
        result(answer),
    ]);
    let exe = harness(&e, &body);
    let r = Researcher::new(cfg(&e, &exe));
    let mut run = r.start(req(&e, "ws")).unwrap();
    let mut evs = Vec::new();
    while let Some(ev) = run.next_event().await {
        evs.push(ev);
    }
    let out = run.finish().await;
    assert_eq!(out.status, RunStatus::Done);
    assert_eq!(out.claude_session.as_deref(), Some("sess-1"));
    assert_eq!(out.model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(out.answer, "add 什么都不做，见 src/lib.rs:1。");
    assert_eq!(out.offer.unwrap().title, "实现 add");
    assert_eq!(out.files_read, vec!["src/lib.rs".to_string()]);
    assert_eq!(out.citations.len(), 1);
    assert!(out.citations[0].read);
    assert_eq!(out.tools.len(), 1);
    assert_eq!(out.tools[0].ok, Some(true));
    assert_eq!(out.turns, 2);
    assert_eq!(out.cost_usd, Some(0.01));
    let texts: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            ResearchEvent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["add 什么都不做，", "add 什么都不做，见 src/lib.rs:1。"]
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, ResearchEvent::ToolStarted(t) if t.brief == "src/lib.rs"))
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, ResearchEvent::ToolDone(t) if t.ok == Some(true)))
    );
    // the command line the fake received carries the allowlist
    let args = std::fs::read_to_string(e.root.join("args.txt")).unwrap();
    assert!(args.contains("--tools=Read,Grep,Glob"));
    assert!(args.contains("--strict-mcp-config"));
    assert!(args.contains("--permission-mode\ndontAsk"));
    // scratch run dir holds the settings, no MCP token left behind
    let run_dir = e.root.join("scratch").join(&out.run_id);
    assert!(run_dir.join("settings.json").exists());
    assert!(!run_dir.join("mcp.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn time_budget_keeps_partial_and_offers() {
    let e = env();
    let body = format!(
        "{}\nsleep 30",
        emit(&[init(), start("m1"), text("读到一半的结论")])
    );
    let exe = harness(&e, &body);
    let mut c = cfg(&e, &exe);
    // the fake prints its partial answer at once, but a busy machine may take a second or more just to
    // start it: the hard limit leaves room for that, the soft one still comes first
    c.budget.hard = Some(Duration::from_millis(3000));
    c.budget.soft = Duration::from_millis(300);
    let r = Researcher::new(c);
    let t0 = Instant::now();
    let mut run = r.start(req(&e, "ws")).unwrap();
    let mut still = false;
    while let Some(ev) = run.next_event().await {
        still |= ev == ResearchEvent::StillReading;
    }
    let out = run.finish().await;
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "the fake's `sleep 30` was cut short"
    );
    assert!(still, "soft limit reported");
    assert_eq!(out.status, RunStatus::Budget(BudgetHit::Time));
    assert_eq!(out.answer, "读到一半的结论\n\n（没读完就停了）");
    assert!(out.offer.unwrap().title.starts_with("继续深入："));
}

/// No limit by default: a run that keeps working goes on past any window (here several times the stall
/// window) and ends with its answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_keeps_working_is_never_cut_off() {
    let e = env();
    let mut body = emit(&[init(), start("m1")]);
    for i in 0..6 {
        body.push_str(&format!(
            "\nsleep 0.8\n{}",
            emit(&[text(&format!("第{i}段。"))])
        ));
    }
    body.push_str(&format!(
        "\n{}",
        emit(&[result("第0段。第1段。第2段。第3段。第4段。第5段。")])
    ));
    let exe = harness(&e, &body);
    let mut c = cfg(&e, &exe);
    assert_eq!(
        (c.budget.hard, c.budget.max_turns, c.budget.max_tools),
        (None, None, None)
    );
    c.budget.idle = Duration::from_secs(2);
    let t0 = Instant::now();
    let out = Researcher::new(c)
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert!(
        t0.elapsed() > Duration::from_secs(4),
        "longer than the stall window"
    );
    assert_eq!(out.status, RunStatus::Done, "{:?}", out.answer);
    assert!(
        out.answer.starts_with("第0段。") && out.answer.contains("第5段。"),
        "{}",
        out.answer
    );
}

/// A harness that says nothing at all for the stall window is stuck: ended, what it wrote is kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_silent_harness_is_ended_as_stuck() {
    let e = env();
    let exe = harness(
        &e,
        &format!(
            "{}\nsleep 30",
            emit(&[init(), start("m1"), text("写到一半")])
        ),
    );
    let mut c = cfg(&e, &exe);
    c.budget.idle = Duration::from_secs(2);
    let t0 = Instant::now();
    let out = Researcher::new(c)
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert!(
        t0.elapsed() < Duration::from_secs(15),
        "ended long before the 30 s sleep"
    );
    assert_eq!(out.status, RunStatus::Budget(BudgetHit::Idle));
    assert!(out.answer.starts_with("写到一半"), "{}", out.answer);
}

/// A harness that closed its output but does not exit is ended after the exit wait, not after the
/// (much longer) stall window.
#[tokio::test(flavor = "multi_thread")]
async fn a_harness_that_closed_its_output_but_hangs_is_ended_after_the_exit_wait() {
    let e = env();
    let exe = harness(
        &e,
        &format!(
            "{}\nexec >&-\nsleep 30",
            emit(&[init(), start("m1"), text("写到一半")])
        ),
    );
    let mut c = cfg(&e, &exe);
    assert_eq!(c.budget.idle, steve_research::DEFAULT_IDLE);
    c.budget.exit_wait = Duration::from_secs(2);
    let t0 = Instant::now();
    let out = Researcher::new(c)
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert!(
        t0.elapsed() < Duration::from_secs(15),
        "ended long before the 30 s sleep and the stall window"
    );
    assert_eq!(out.status, RunStatus::Budget(BudgetHit::Time));
    assert!(out.answer.starts_with("写到一半"), "{}", out.answer);
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_budget_stops_at_the_limit() {
    let e = env();
    let mut lines = vec![init(), start("m1"), text("先说一句")];
    for i in 0..6 {
        lines.push(tool(
            "m1",
            &format!("t{i}"),
            "Grep",
            serde_json::json!({"pattern": "add"}),
        ));
    }
    let exe = harness(&e, &format!("{}\nsleep 30", emit(&lines)));
    let mut c = cfg(&e, &exe);
    c.budget.max_tools = Some(4);
    let out = Researcher::new(c)
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert_eq!(out.status, RunStatus::Budget(BudgetHit::Tools));
    assert_eq!(out.tools.len(), 4);
    assert!(out.answer.ends_with("（没读完就停了）"));
    assert!(out.offer.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn turn_budget_stops_at_the_limit() {
    let e = env();
    let lines = vec![init(), start("m1"), text("一"), start("m2"), start("m3")];
    let exe = harness(&e, &format!("{}\nsleep 30", emit(&lines)));
    let mut c = cfg(&e, &exe);
    c.budget.max_turns = Some(2);
    let out = Researcher::new(c)
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert_eq!(out.status, RunStatus::Budget(BudgetHit::Turns));
    assert!(out.answer.starts_with("一"));
    // the harness's own --max-turns ending counts as the same budget
    let e2 = env();
    let body = emit(&[
        init(),
        r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#.to_string(),
    ]);
    let exe = harness(&e2, &body);
    let out = Researcher::new(cfg(&e2, &exe))
        .start(req(&e2, "ws"))
        .unwrap()
        .finish()
        .await;
    assert_eq!(out.status, RunStatus::Budget(BudgetHit::Turns));
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_keeps_the_partial_answer() {
    let e = env();
    let body = format!(
        "{}\nsleep 30",
        emit(&[init(), start("m1"), text("已经写了半句")])
    );
    let exe = harness(&e, &body);
    let r = Researcher::new(cfg(&e, &exe));
    let mut run = r.start(req(&e, "ws")).unwrap();
    assert_eq!(r.running("ws").as_deref(), Some(run.id.as_str()));
    while let Some(ev) = run.next_event().await {
        if matches!(ev, ResearchEvent::Text { .. }) {
            assert_eq!(r.stop_workspace("ws").as_deref(), Some(run.id.as_str()));
        }
    }
    let out = run.finish().await;
    assert_eq!(out.status, RunStatus::Stopped);
    assert_eq!(out.status.stopped(), Some("user"));
    assert_eq!(out.answer, "已经写了半句\n\n（已停下）");
    assert!(r.running("ws").is_none(), "slot freed");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_run_per_workspace_and_global_limit() {
    let e = env();
    let exe = harness(&e, &format!("{}\nsleep 30", emit(&[init()])));
    let mut c = cfg(&e, &exe);
    c.max_global = 2;
    let r = Researcher::new(c);
    let a = r.start(req(&e, "w1")).unwrap();
    assert_eq!(
        r.start(req(&e, "w1")).err(),
        Some(ResearchError::Busy {
            run_id: a.id.clone()
        })
    );
    let b = r.start(req(&e, "w2")).unwrap();
    assert_eq!(r.start(req(&e, "w3")).err(), Some(ResearchError::Full));
    a.stop();
    b.stopper().stop();
    assert_eq!(a.finish().await.status, RunStatus::Stopped);
    assert_eq!(b.finish().await.status, RunStatus::Stopped);
    assert!(r.start(req(&e, "w3")).is_ok());
    r.stop_workspace("w3");
}

#[tokio::test(flavor = "multi_thread")]
async fn follow_up_resumes_and_falls_back_fresh() {
    let e = env();
    // resumes when the model is there
    let exe = harness(
        &e,
        &emit(&[init(), start("m1"), text("续上"), result("续上")]),
    );
    let r = Researcher::new(cfg(&e, &exe));
    let mut q = req(&e, "ws");
    q.resume = Some(Resume {
        claude_session: "sess-0".into(),
        model: "claude-sonnet-5-5".into(),
        prior_answer: "上一轮".into(),
    });
    let out = r.start(q.clone()).unwrap().finish().await;
    assert_eq!(out.status, RunStatus::Done);
    let args = std::fs::read_to_string(e.root.join("args.txt")).unwrap();
    assert!(args.contains("--resume\nsess-0"), "{args}");

    // the resumed model fails before saying anything: a fresh run with the prior answer as context
    let body = format!(
        "case \"$*\" in *--resume*) echo boom >&2; exit 1;; esac\n{}",
        emit(&[init(), start("m1"), text("重新答"), result("重新答")])
    );
    let exe = harness(&e, &body);
    let out = Researcher::new(cfg(&e, &exe))
        .start(q)
        .unwrap()
        .finish()
        .await;
    assert_eq!(out.status, RunStatus::Done);
    assert_eq!(out.answer, "重新答");
    let args = std::fs::read_to_string(e.root.join("args.txt")).unwrap();
    assert!(!args.contains("--resume"));
    assert!(args.contains("上一轮你的回答：\n上一轮"));
}

#[tokio::test(flavor = "multi_thread")]
async fn model_fallback_and_failure() {
    let e = env();
    let body = format!(
        "case \"$*\" in *bad-model*) exit 1;; esac\n{}",
        emit(&[init(), result("好了")])
    );
    let exe = harness(&e, &body);
    let mut c = cfg(&e, &exe);
    c.models = vec!["bad-model".into(), "claude-opus-5-5".into()];
    let out = Researcher::new(c)
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert_eq!(out.status, RunStatus::Done);
    assert_eq!(out.model.as_deref(), Some("claude-opus-5-5"));

    let exe = harness(&e, "echo nope >&2; exit 3");
    let out = Researcher::new(cfg(&e, &exe))
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert!(
        matches!(&out.status, RunStatus::Failed(m) if m.contains("nope")),
        "{:?}",
        out.status
    );
}

/// Layer 3 alone: the guard is off and the fake harness ignores every flag and writes to the project.
/// Under `sandbox-exec` the writes fail; its own run directory stays writable.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_blocks_writes_with_allowlist_bypassed() {
    let e = env();
    let body = format!(
        "echo pwned > '{p}/pwned.txt'; echo x > '{p}/keep.txt'; rm -f '{p}/src/lib.rs'; echo ok > \"$TMPDIR/t\"\n{}",
        emit(&[init(), result("写完了")]),
        p = e.proj.display(),
    );
    let exe = harness(&e, &body);
    let mut c = cfg(&e, &exe);
    c.guard = false;
    c.sandbox = SandboxMode::Required;
    let out = Researcher::new(c.clone())
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert!(out.sandboxed);
    assert_eq!(out.status, RunStatus::Done);
    assert!(!e.proj.join("pwned.txt").exists());
    assert_eq!(
        std::fs::read_to_string(e.proj.join("keep.txt")).unwrap(),
        "orig"
    );
    assert!(e.proj.join("src/lib.rs").exists());
    let t = e.root.join("scratch").join(&out.run_id).join("tmp/t");
    assert_eq!(
        std::fs::read_to_string(t).unwrap(),
        "ok\n",
        "TMPDIR inside the run dir is writable"
    );

    // the same harness without the sandbox would have written: proves the test exercises layer 3
    c.sandbox = SandboxMode::Off;
    Researcher::new(c)
        .start(req(&e, "ws"))
        .unwrap()
        .finish()
        .await;
    assert!(e.proj.join("pwned.txt").exists());
}

/// The question's attachments: copied outside the run directory where the run may read them (the guard
/// lets a Read there through), named in the prompt, the image sent inline on stdin; the copies are gone
/// after the run.
#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_readable_and_images_go_inline() {
    let e = env();
    let blob = e.root.join("blob-png");
    std::fs::write(&blob, b"\x89PNG fake").unwrap();
    let doc = e.root.join("blob-txt");
    std::fs::write(&doc, "需求：按钮叫发送").unwrap();
    let files_root = e.root.join("files");
    let read = files_root.join("tr_att/需求.txt").display().to_string();
    let stdin = e.root.join("stdin.txt");
    let body = format!(
        "cat > '{}'\n{}",
        stdin.display(),
        emit(&[
            init(),
            start("m1"),
            tool("m1", "t1", "Read", serde_json::json!({"file_path": read})),
            tool_result("t1"),
            start("m2"),
            text("图上是红蓝两色。"),
            result("图上是红蓝两色。"),
        ])
    );
    let exe = harness(&e, &body);
    let mut c = cfg(&e, &exe);
    c.files_root = files_root.clone();
    let r = Researcher::new(c);
    let mut q = req(&e, "ws");
    q.run_id = Some("tr_att".into());
    q.attachments = vec![
        Attachment {
            path: blob,
            name: "截图.png".into(),
            mime: "image/png".into(),
            size: 10,
            earlier: false,
        },
        Attachment {
            path: doc,
            name: "需求.txt".into(),
            mime: "text/plain".into(),
            size: 24,
            earlier: false,
        },
    ];
    let mut run = r.start(q).unwrap();
    while run.next_event().await.is_some() {}
    let out = run.finish().await;
    assert_eq!(out.status, RunStatus::Done, "{out:?}");
    assert_eq!(out.answer, "图上是红蓝两色。");
    let args = std::fs::read_to_string(e.root.join("args.txt")).unwrap();
    assert!(args.contains("--input-format\nstream-json"));
    assert!(args.contains(&format!(
        "--add-dir={}",
        files_root.join("tr_att").display()
    )));
    let msg: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&stdin).unwrap().trim()).unwrap();
    let content = msg["message"]["content"].as_array().unwrap();
    let prompt = content[0]["text"].as_str().unwrap();
    assert!(prompt.starts_with("prompt"));
    assert!(prompt.contains("截图.png（image/png"), "{prompt}");
    assert!(prompt.contains("需求.txt（text/plain"), "{prompt}");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content.len(), 2, "only the image goes inline");
    assert!(!files_root.join("tr_att").exists(), "copies removed");
    /* reading anywhere under the secret dirs is still a violation */
    let s = e.root.join("scratch");
    assert!(
        research_guard(
            "Read",
            &serde_json::json!({"file_path": e.home.join(".ssh/id_rsa").display().to_string()}),
            &Scope::new(&e.proj, &[s], &e.home)
        )
        .is_err()
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_shutdown_reaps_its_harness_group_and_grandchild_only() {
    let e = env();
    let pids = e.root.join("research-pids");
    let exe = harness(
        &e,
        &format!(
            "trap '' TERM\nsleep 300 &\nprintf '%s %s' \"$$\" \"$!\" > '{}'\nwait",
            pids.display()
        ),
    );
    let r = Researcher::new(cfg(&e, &exe));
    let run = r.start(req(&e, "shutdown-test")).unwrap();
    let mut unrelated = tokio::process::Command::new("sleep")
        .arg("300")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !pids.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "fake harness did not start"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ids = std::fs::read_to_string(&pids)
        .unwrap()
        .split_whitespace()
        .map(|s| s.parse::<i32>().unwrap())
        .collect::<Vec<_>>();
    assert!(
        r.shutdown(Duration::from_secs(3)).await,
        "all owned groups must be confirmed gone"
    );
    for id in &ids {
        assert_ne!(
            unsafe { libc::kill(*id, 0) },
            0,
            "owned process {id} is still present"
        );
    }
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "shutdown must not kill unrelated processes"
    );
    assert_eq!(
        r.start(req(&e, "after-shutdown")).err(),
        Some(ResearchError::Closed)
    );
    tokio::time::timeout(Duration::from_secs(1), run.finish())
        .await
        .unwrap();
    unrelated.kill().await.unwrap();
    unrelated.wait().await.unwrap();
    assert!(
        r.shutdown(Duration::from_millis(20)).await,
        "shutdown is idempotent"
    );
}

#[tokio::test]
async fn shutdown_before_spawn_closes_pending_research_admission() {
    let e = env();
    let exe = harness(&e, "sleep 300");
    let r = Researcher::new(cfg(&e, &exe));
    let run = r.start(req(&e, "pending")).unwrap();
    assert!(r.shutdown(Duration::from_secs(1)).await);
    tokio::time::timeout(Duration::from_secs(1), run.finish())
        .await
        .unwrap();
    assert_eq!(r.start(req(&e, "late")).err(), Some(ResearchError::Closed));
}

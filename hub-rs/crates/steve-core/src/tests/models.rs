//! 设置 · 模型: the setting decides which `--model` (and `--effort`) the router's `claude -p` gets, in
//! fallback order, and which models a research run is asked to use. The harness is a stand-in script
//! that logs its arguments.

use crate::ClaudeCli;
use crate::repo::WS;
use crate::testkit::{FakeResearch, Kit, Step};
use serde_json::json;

/// A stand-in `claude`: appends its arguments (one per line, then `--`) to `args.log`; model `broken`
/// dies, any other answers a chat route.
fn fake_claude(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let reply = json!({"type": "result", "is_error": false, "result": json!({"kind": "chat", "confidence": 0.9}).to_string()});
    std::fs::write(dir.join("reply.json"), reply.to_string()).unwrap();
    let exe = dir.join("claude");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" -- >> '{0}/args.log'\nfor a; do case \"$p\" in --model) m=\"$a\";; esac; p=\"$a\"; done\n[ \"$m\" = broken ] && {{ echo boom >&2; exit 2; }}\ncat '{0}/reply.json'\n",
            dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    exe
}

/// `(model, effort)` of each run logged so far, then clears the log.
fn runs(dir: &std::path::Path) -> Vec<(String, Option<String>)> {
    let p = dir.join("args.log");
    let log = std::fs::read_to_string(&p).unwrap_or_default();
    let _ = std::fs::remove_file(&p);
    log.split("\n--\n")
        .filter(|r| !r.trim().is_empty())
        .map(|r| {
            let l: Vec<&str> = r.lines().collect();
            let after = |f: &str| l.iter().position(|x| *x == f).map(|i| l[i + 1].to_string());
            (after("--model").unwrap_or_default(), after("--effort"))
        })
        .collect()
}

#[tokio::test]
async fn the_setting_picks_the_routers_model_in_order() {
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let cli = ClaudeCli {
        codex: None,
        executable: fake_claude(dir.path()),
        fast: vec!["unused".into()],
        smart: vec!["unused".into()],
        cwd: dir.path().into(),
        models: Some(k.core.models().clone()),
    };
    let ctx = k.core.route_ctx(WS).unwrap();
    let route = || {
        crate::stats::tagged(
            "route",
            WS,
            crate::gateway::route(&cli, &ctx, "你好", &[], false),
        )
    };

    /* default: the fast list of the environment (STEVE_MODELS_FAST), no effort */
    let r = route().await;
    assert!(!r.fallback, "{r:?}");
    assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(runs(dir.path()), [("claude-haiku-5-5".into(), None)]);

    /* the user picks opus with high effort: the next call uses it, no restart */
    k.core
        .models()
        .apply(
            WS,
            &json!({"purposes": {"route": {"models": ["claude-opus-5-5"], "effort": "high"}}}),
            "me",
        )
        .unwrap();
    let r = route().await;
    assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
    // the effort does not turn the router into a call with Claude Code's tools: it keeps the
    // cache-friendly layout (stable head as the system prompt, no tools)
    let log = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
    let args: Vec<&str> = log.lines().collect();
    let at = |f: &str| args.iter().position(|x| *x == f);
    assert!(
        at("--system-prompt").is_some() && at("--append-system-prompt").is_none(),
        "{log}"
    );
    assert_eq!(at("--tools").map(|i| args[i + 1]), Some(""), "{log}");
    let system = log
        .split_once("\n--system-prompt\n")
        .and_then(|(_, r)| r.split_once("\n--tools\n"))
        .map(|(h, _)| h);
    assert!(
        system.is_some_and(|h| h.contains("Bots (id: role):")),
        "{log}"
    );
    assert_eq!(
        runs(dir.path()),
        [("claude-opus-5-5".into(), Some("high".into()))]
    );

    /* fallback order: the first model fails, the second answers */
    k.core
        .models()
        .apply(
            WS,
            &json!({"purposes": {"route": {"models": ["broken", "claude-sonnet-5-5"]}}}),
            "me",
        )
        .unwrap();
    let r = route().await;
    assert_eq!(r.model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(
        runs(dir.path()),
        [("broken".into(), None), ("claude-sonnet-5-5".into(), None)]
    );

    /* another purpose's setting does not touch routing; without its override routing follows Steve
    (the seeded Steve bot: Claude Code · claude-sonnet-5-5) */
    k.core
        .models()
        .apply(
            WS,
            &json!({"purposes": {"route": null, "chat": {"models": ["claude-opus-5-5"]}}}),
            "me",
        )
        .unwrap();
    route().await;
    assert_eq!(runs(dir.path()), [("claude-sonnet-5-5".into(), None)]);
}

#[tokio::test]
async fn the_setting_picks_the_research_models() {
    let k = Kit::new();
    let fr = FakeResearch::new(vec![Step::Text("答")]);
    assert!(k.core.set_researcher(fr.clone()));
    k.gw.set(|p, _| {
        if p.starts_with("You are the router") {
            Ok(json!({"kind": "research", "confidence": 0.9}).to_string())
        } else {
            Err(crate::error::msg("unexpected prompt"))
        }
    });
    let ask = |n: usize| {
        let fr = fr.clone();
        async move {
            for _ in 0..500 {
                if fr.seen.lock().unwrap().len() >= n {
                    return fr.seen.lock().unwrap()[n - 1].models.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("no research run {n}");
        }
    };
    k.core
        .say(WS, "现在steve的知识系统是怎么运作的？", &[])
        .unwrap();
    // research follows the Steve bot: its model, then the smart list as fallback
    let c = ask(1).await;
    assert_eq!(c.harness, "claude-code");
    assert_eq!(c.models, ["claude-sonnet-5-5", "claude-opus-5-5"]);
    assert_eq!(c.effort, None);

    k.core
        .models()
        .apply(
            WS,
            &json!({"purposes": {"research": {"models": ["claude-opus-5-5", "claude-sonnet-5-5"], "effort": "xhigh"}}}),
            "me",
        )
        .unwrap();
    /* wait until the first run has ended (one research run per workspace at a time) */
    for _ in 0..500 {
        if k.db().messages(WS).unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    k.core
        .say(WS, "那检索又是怎么做的？讲讲代码里的实现", &[])
        .unwrap();
    let c = ask(2).await;
    assert_eq!(c.models, ["claude-opus-5-5", "claude-sonnet-5-5"]);
    assert_eq!(c.effort.as_deref(), Some("xhigh"));
}

#[tokio::test]
async fn new_agent_sessions_take_the_workspace_default() {
    let k = Kit::new();
    let started = |k: &Kit| {
        let it = k.item("待开始", "排队", None);
        k.core.start_session(&it, None, None, &[]).unwrap().model
    };
    // the dev bot names no model: STEVE_DEFAULT_MODEL_CLAUDE
    assert_eq!(started(&k).as_deref(), Some("claude-opus-5-5"));
    k.core
        .models()
        .apply(
            WS,
            &json!({"agents": {"claude-code": "claude-sonnet-5-5"}}),
            "me",
        )
        .unwrap();
    assert_eq!(started(&k).as_deref(), Some("claude-sonnet-5-5"));
    // a bot's own model still wins
    k.db()
        .bot_patch("dev", &crate::patch![model: "claude-haiku-5-5"], "me")
        .unwrap();
    assert_eq!(started(&k).as_deref(), Some("claude-haiku-5-5"));
}

/// A stand-in `codex`: appends its arguments (one per line, then `--`) to `codex.log` and stdin to
/// `codex.stdin`; model `broken` fails its turn, any other answers with `reply.txt` as Codex's JSON
/// events (with usage).
fn fake_codex(dir: &std::path::Path, reply: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(dir.join("reply.txt"), reply).unwrap();
    let ok = |text: &str| {
        format!(
            "{}\n{}\n{}\n",
            json!({"type": "thread.started", "thread_id": "t1"}),
            json!({"type": "item.completed", "item": {"id": "item_0", "type": "agent_message", "text": text}}),
            json!({"type": "turn.completed", "usage": {"input_tokens": 6721, "cached_input_tokens": 6000, "cache_write_input_tokens": 0, "output_tokens": 9, "reasoning_output_tokens": 2}}),
        )
    };
    std::fs::write(dir.join("ok.jsonl"), ok(reply)).unwrap();
    std::fs::write(
        dir.join("broken.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type": "error", "message": "requested model is not available"}),
            json!({"type": "turn.failed", "error": {"message": "requested model is not available"}}),
        ),
    )
    .unwrap();
    let exe = dir.join("codex");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" -- >> '{0}/codex.log'\ncat > '{0}/codex.stdin'\nfor a; do case \"$p\" in -m) m=\"$a\";; -i) cat \"$a\" >> '{0}/codex.images';; esac; p=\"$a\"; done\n[ \"$m\" = broken ] && {{ cat '{0}/broken.jsonl'; exit 0; }}\ncat '{0}/ok.jsonl'\n",
            dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    exe
}

/// Each logged `codex` run's arguments, then clears the log.
fn codex_runs(dir: &std::path::Path) -> Vec<Vec<String>> {
    let p = dir.join("codex.log");
    let log = std::fs::read_to_string(&p).unwrap_or_default();
    let _ = std::fs::remove_file(&p);
    log.split("\n--\n")
        .map(|r| r.trim_end_matches("\n--").to_string())
        .filter(|r| !r.trim().is_empty())
        .map(|r| r.lines().map(String::from).collect())
        .collect()
}

fn after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Steve on Codex: routing and chat answers run through `codex exec` with Steve's model and effort,
/// and are booked under `codex-cli`; a route override keeps Claude Code.
#[tokio::test]
async fn a_codex_steve_routes_and_answers_through_codex_exec() {
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let route_reply = json!({"kind": "chat", "confidence": 0.9}).to_string();
    let codex = fake_codex(dir.path(), &route_reply);
    let gw = crate::stats::RecordingGateway::new(
        std::sync::Arc::new(ClaudeCli {
            codex: Some(codex),
            executable: fake_claude(dir.path()),
            fast: vec!["unused".into()],
            smart: vec!["unused".into()],
            cwd: dir.path().into(),
            models: Some(k.core.models().clone()),
        }),
        k.db().store().clone(),
    );
    // Steve's own harness, model and effort, set on its Bot page
    k.db()
        .bot_patch(
            "steve",
            &crate::patch![harness: "codex", model: "gpt-6-luna", reasoning_effort: "low"],
            "me",
        )
        .unwrap();
    k.core
        .models()
        .apply(WS, &json!({"purposes": {"route": null}}), "me")
        .unwrap();
    let ctx = k.core.route_ctx(WS).unwrap();
    let r = crate::stats::tagged(
        "route",
        WS,
        crate::gateway::route(&gw, &ctx, "你好", &[], false),
    )
    .await;
    assert!(!r.fallback, "{r:?}");
    assert_eq!(r.kind, "answer");
    assert_eq!(r.model.as_deref(), Some("gpt-6-luna"));
    let cx = codex_runs(dir.path());
    assert_eq!(cx.len(), 1);
    let a = &cx[0];
    assert_eq!(a[0], "exec");
    for f in ["--json", "--ephemeral", "--skip-git-repo-check", "-"] {
        assert!(a.contains(&f.to_string()), "{f}: {a:?}");
    }
    assert_eq!(after(a, "-s").as_deref(), Some("read-only"));
    assert_eq!(after(a, "-m").as_deref(), Some("gpt-6-luna"));
    assert!(
        a.contains(&"model_reasoning_effort=\"low\"".to_string()),
        "{a:?}"
    );
    assert!(a.contains(&"mcp_servers={}".to_string()));
    // no tools for routing: no shell, the stable head replaces Codex's instructions
    assert!(
        a.windows(2)
            .any(|w| w[0] == "--disable" && w[1] == "shell_tool"),
        "{a:?}"
    );
    let instr = a
        .iter()
        .find_map(|x| x.strip_prefix("model_instructions_file="))
        .expect("instructions file");
    let instr: String = serde_json::from_str(instr).unwrap();
    assert!(
        std::fs::read_to_string(&instr)
            .unwrap()
            .contains("Bots (id: role):")
    );
    // the message itself goes on stdin, without the head
    let stdin = std::fs::read_to_string(dir.path().join("codex.stdin")).unwrap();
    assert!(
        stdin.contains("你好") && !stdin.contains("Bots (id: role):"),
        "{stdin}"
    );
    assert!(!dir.path().join("args.log").exists(), "claude was not run");

    // a chat answer: Codex's message is the answer, heard once it is complete
    std::fs::write(
        dir.path().join("ok.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type": "item.completed", "item": {"id": "i", "type": "agent_message", "text": json!({"thought": "想了想", "answer": "四"}).to_string()}}),
            json!({"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 40, "output_tokens": 5}}),
        ),
    )
    .unwrap();
    let main = k.db().bot("steve").unwrap().unwrap();
    let heard = std::sync::Mutex::new(vec![]);
    let ans = crate::stats::tagged(
        "chat",
        WS,
        crate::gateway::answer(&gw, &main, "2+2?", &[], &Default::default(), "", &|t, a| {
            heard.lock().unwrap().push((t.to_string(), a.to_string()))
        }),
    )
    .await;
    assert!(!ans.failed, "{ans:?}");
    assert_eq!((ans.text.as_str(), ans.thought.as_str()), ("四", "想了想"));
    assert_eq!(ans.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(heard.lock().unwrap().last().unwrap().1, "四");
    assert_eq!(codex_runs(dir.path()).len(), 1);

    // a failing model falls back down the list; an override puts routing back on Claude Code
    k.core
        .models()
        .apply(
            WS,
            &json!({"purposes": {"chat": {"harness": "codex", "models": ["broken", "gpt-6-sol"]}, "route": {"harness": "claude-code", "models": ["claude-haiku-5-5"]}}}),
            "me",
        )
        .unwrap();
    let ans = crate::stats::tagged(
        "chat",
        WS,
        crate::gateway::answer(&gw, &main, "2+2?", &[], &Default::default(), "", &|_, _| {}),
    )
    .await;
    assert_eq!(ans.model.as_deref(), Some("gpt-6-sol"));
    let r = crate::stats::tagged(
        "route",
        WS,
        crate::gateway::route(&gw, &ctx, "你好", &[], false),
    )
    .await;
    assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(runs(dir.path()), [("claude-haiku-5-5".into(), None)]);

    // statistics: harness and model of each call, Codex input without the cached part
    type Booked = (String, String, String, Option<i64>, Option<i64>, bool);
    let rows: Vec<Booked> = k
        .db()
        .store()
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT purpose, harness, model, input, cache_read, ok FROM usage_records WHERE source = 'gateway' ORDER BY id",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "route".into(),
                "codex-cli".into(),
                "gpt-6-luna".into(),
                Some(721),
                Some(6000),
                true
            ),
            (
                "chat".into(),
                "codex-cli".into(),
                "gpt-6-luna".into(),
                Some(60),
                Some(40),
                true
            ),
            (
                "chat".into(),
                "codex-cli".into(),
                "gpt-6-sol".into(),
                Some(60),
                Some(40),
                true
            ),
            (
                "route".into(),
                "claude-cli".into(),
                "claude-haiku-5-5".into(),
                None,
                None,
                true
            ),
        ]
    );
    // and the settings page shows what each purpose ran on
    let v = k.core.models().view(WS);
    let chat = v["purposes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "chat")
        .unwrap();
    assert_eq!(chat["recent"]["used"][0]["harness"], json!("codex"));
}

/// A Codex judgment with no model named runs Codex's own default (`-m` left out); a Codex Steve
/// without a codex executable fails clearly.
#[tokio::test]
async fn codex_without_a_model_or_an_executable() {
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let codex = fake_codex(dir.path(), "标题");
    k.db()
        .bot_patch(
            "steve",
            &crate::patch![harness: "codex", model: serde_json::Value::Null],
            "me",
        )
        .unwrap();
    let cli = |codex: Option<std::path::PathBuf>| ClaudeCli {
        codex,
        executable: fake_claude(dir.path()),
        fast: vec![],
        smart: vec![],
        cwd: dir.path().into(),
        models: Some(k.core.models().clone()),
    };
    use crate::gateway::ModelGateway;
    let p = crate::stats::tagged(
        "chat",
        WS,
        cli(Some(codex)).call(
            "hi",
            crate::gateway::Tier::Smart,
            std::time::Duration::from_secs(10),
            &["--allowedTools".into(), "Read".into()],
        ),
    )
    .await
    .unwrap();
    assert_eq!(p.text, "标题");
    let a = &codex_runs(dir.path())[0];
    assert_eq!(after(a, "-m"), None);
    // a call with tools keeps Codex's shell and instructions
    assert!(!a.iter().any(|x| x == "shell_tool"), "{a:?}");
    assert!(!a.iter().any(|x| x.starts_with("model_instructions_file=")));
    let e = crate::stats::tagged(
        "chat",
        WS,
        cli(None).call(
            "hi",
            crate::gateway::Tier::Smart,
            std::time::Duration::from_secs(10),
            &[],
        ),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("codex"), "{e}");
}

/// A chat answer with an image on Codex: the image reaches `codex exec` as `-i <file>`, and the file is
/// gone after the call.
#[tokio::test]
async fn codex_gets_the_images_of_a_chat_answer() {
    use base64::Engine;
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let codex = fake_codex(
        dir.path(),
        &json!({"thought": "", "answer": "是一张图"}).to_string(),
    );
    k.db()
        .bot_patch(
            "steve",
            &crate::patch![harness: "codex", model: "gpt-6-luna"],
            "me",
        )
        .unwrap();
    let cli = ClaudeCli {
        codex: Some(codex),
        executable: fake_claude(dir.path()),
        fast: vec![],
        smart: vec![],
        cwd: dir.path().into(),
        models: Some(k.core.models().clone()),
    };
    let main = k.db().bot("steve").unwrap().unwrap();
    let files = crate::gateway::ChatFiles {
        count: 1,
        images: vec![
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(b"PNGDATA")}}),
        ],
        ..Default::default()
    };
    let ans = crate::stats::tagged(
        "chat",
        WS,
        crate::gateway::answer(&cli, &main, "这是什么？", &[], &files, "", &|_, _| {}),
    )
    .await;
    assert_eq!(ans.text, "是一张图", "{ans:?}");
    let a = &codex_runs(dir.path())[0];
    let img = after(a, "-i").expect("an -i argument");
    assert!(img.ends_with(".png"), "{img}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("codex.images")).unwrap(),
        "PNGDATA"
    );
    assert!(
        !std::path::Path::new(&img).exists(),
        "removed after the call"
    );
}

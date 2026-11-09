//! 设置 · 模型: the setting decides which model (and effort) the router's one-shot call gets, in
//! fallback order, on which harness adapter, and which models a research run is asked to use. The
//! adapter is a stand-in ([`FakeAcp`]) that logs each run.

use crate::InProcess;
use crate::repo::WS;
use crate::testkit::{FakeAcp, FakeResearch, Kit, Step};
use serde_json::json;

/// `(model, effort)` of each run logged so far (`""`: the adapter's default), then forgets them.
fn runs(acp: &FakeAcp) -> Vec<(String, Option<String>)> {
    acp.runs()
        .into_iter()
        .map(|r| {
            (
                r["model"].as_str().unwrap_or("").to_string(),
                r["effort"].as_str().map(String::from),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_setting_picks_the_routers_model_in_order() {
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let acp = FakeAcp::new(
        dir.path(),
        &json!({"kind": "chat", "confidence": 0.9}).to_string(),
    );
    let cli = InProcess {
        one: acp.one(),
        fast: vec!["unused".into()],
        smart: vec!["unused".into()],
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
    assert_eq!(runs(&acp), [("claude-haiku-5-5".into(), None)]);

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
    // the effort does not turn the router into a call with the harness's tools: the stable head
    // is the session's system prompt, in place of the harness's own
    let run = acp.runs().remove(0);
    assert_eq!(run["system"][0], "override");
    assert!(
        run["system"][1]
            .as_str()
            .is_some_and(|h| h.contains("Bots (id: role):")),
        "{run}"
    );
    assert_eq!(
        (run["model"].as_str(), run["effort"].as_str()),
        (Some("claude-opus-5-5"), Some("high"))
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
        runs(&acp),
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
    assert_eq!(runs(&acp), [("claude-sonnet-5-5".into(), None)]);
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

/// Steve on another harness (Codex): routing and chat answers run on its adapter with Steve's
/// model and effort, and are booked under its id; a route override goes back to Claude Code.
#[tokio::test]
async fn a_codex_steve_routes_and_answers_on_its_adapter() {
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let acp = FakeAcp::new(
        dir.path(),
        &json!({"kind": "chat", "confidence": 0.9}).to_string(),
    );
    let gw = crate::stats::RecordingGateway::new(
        std::sync::Arc::new(InProcess {
            one: acp.one(),
            fast: vec!["unused".into()],
            smart: vec!["unused".into()],
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
    let run = acp.runs().remove(0);
    assert_eq!(run["harness"], "codex");
    assert_eq!(
        (run["model"].as_str(), run["effort"].as_str()),
        (Some("gpt-6-luna"), Some("low"))
    );
    // the message alone is the prompt, the stable head the system prompt
    let prompt = run["prompt"][0]["text"].as_str().unwrap();
    assert!(
        prompt.contains("你好") && !prompt.contains("Bots (id: role):"),
        "{prompt}"
    );

    // a chat answer: the adapter's message is the answer
    std::fs::write(
        dir.path().join("reply.txt"),
        json!({"thought": "想了想", "answer": "四"}).to_string(),
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
    assert_eq!(acp.runs().len(), 1);

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
    std::fs::write(
        dir.path().join("reply.txt"),
        json!({"kind": "chat", "confidence": 0.9}).to_string(),
    )
    .unwrap();
    let r = crate::stats::tagged(
        "route",
        WS,
        crate::gateway::route(&gw, &ctx, "你好", &[], false),
    )
    .await;
    assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
    let last = acp.runs().pop().unwrap();
    assert_eq!(
        (last["harness"].as_str(), last["model"].as_str()),
        (Some("claude-code"), Some("claude-haiku-5-5"))
    );

    // statistics: the adapter and model of each call
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
    let booked = |purpose: &str, harness: &str, model: &str| -> Booked {
        (
            purpose.into(),
            harness.into(),
            model.into(),
            Some(2),
            Some(11097),
            true,
        )
    };
    assert_eq!(
        rows,
        vec![
            booked("route", "codex", "gpt-6-luna"),
            booked("chat", "codex", "gpt-6-luna"),
            booked("chat", "codex", "gpt-6-sol"),
            booked("route", "claude-code", "claude-haiku-5-5"),
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

/// A judgment with no model named runs the adapter's own default (no model is set); a Steve on
/// an adapter this host does not have fails clearly.
#[tokio::test]
async fn no_model_runs_the_adapters_default_and_a_missing_adapter_says_so() {
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let acp = FakeAcp::new(dir.path(), "标题");
    k.db()
        .bot_patch(
            "steve",
            &crate::patch![harness: "codex", model: serde_json::Value::Null],
            "me",
        )
        .unwrap();
    let cli = |acp: &FakeAcp| InProcess {
        one: acp.one(),
        fast: vec![],
        smart: vec![],
        models: Some(k.core.models().clone()),
    };
    use crate::gateway::ModelGateway;
    let p = crate::stats::tagged(
        "chat",
        WS,
        cli(&acp).call(
            "hi",
            crate::gateway::Tier::Smart,
            std::time::Duration::from_secs(10),
            &crate::gateway::ThinkTools::Read(vec![]),
        ),
    )
    .await
    .unwrap();
    assert_eq!(p.text, "标题");
    let run = acp.runs().remove(0);
    assert_eq!(run["model"], serde_json::Value::Null, "{run}");
    // a call with tools and no stable head keeps the harness's own system prompt as it is
    assert_eq!(run["system"], serde_json::Value::Null);
    let mut without = FakeAcp::new(dir.path(), "标题");
    without.acp.adapters.remove("codex");
    let e = crate::stats::tagged(
        "chat",
        WS,
        cli(&without).call(
            "hi",
            crate::gateway::Tier::Smart,
            std::time::Duration::from_secs(10),
            &crate::gateway::ThinkTools::None,
        ),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("codex"), "{e}");
}

/// A chat answer with an image: the image reaches the adapter as an ACP image block.
#[tokio::test]
async fn the_adapter_gets_the_images_of_a_chat_answer() {
    use base64::Engine;
    let k = Kit::new();
    let dir = tempfile::tempdir().unwrap();
    let acp = FakeAcp::new(
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
    let cli = InProcess {
        one: acp.one(),
        fast: vec![],
        smart: vec![],
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
    let run = acp.runs().remove(0);
    assert_eq!(
        run["prompt"][1],
        json!({"type": "image", "mimeType": "image/png",
            "data": base64::engine::general_purpose::STANDARD.encode(b"PNGDATA")})
    );
}

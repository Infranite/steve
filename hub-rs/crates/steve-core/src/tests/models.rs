//! 设置 · 模型: the setting chooses the router's one-shot model, effort and adapter, and the
//! separate list for research runs. The model double ([`ModelScript`]) records each attempt.

use crate::SingleModelGateway;
use crate::repo::WS;
use crate::testkit::{FakeResearch, Kit, ModelScript, Step};
use serde_json::json;

/// `(model, effort)` of each run logged so far (`""`: the adapter's default), then forgets them.
fn runs(acp: &ModelScript) -> Vec<(String, Option<String>)> {
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
    let acp = ModelScript::new(&json!({"kind": "answer", "confidence": 0.9}).to_string());
    let cli = SingleModelGateway {
        one: acp.clone(),
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
    let run = acp.runs().remove(0);
    assert_eq!(
        (run["model"].as_str(), run["effort"].as_str()),
        (Some("claude-opus-5-5"), Some("high"))
    );

    /* The first configured model fails; caller policy returns its deterministic route. */
    k.core
        .models()
        .apply(
            WS,
            &json!({"purposes": {"route": {"models": ["broken", "claude-sonnet-5-5"]}}}),
            "me",
        )
        .unwrap();
    let r = route().await;
    assert!(r.fallback);
    assert_eq!(r.model, None);
    assert_eq!(runs(&acp), [("broken".into(), None)]);

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
            steve_runtime::testkit::eventually_sync(
                format!("Research run {n} reaches the fake researcher."),
                || fr.seen.lock().unwrap().len() >= n,
            )
            .await;
            fr.seen.lock().unwrap()[n - 1].models.clone()
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
            &json!({"purposes": {"research": {"models": ["claude-opus-5-5", "claude-sonnet-5-5"], "effort": "max"}}}),
            "me",
        )
        .unwrap();
    /* wait until the first run has ended (one research run per workspace at a time) */
    steve_runtime::testkit::eventually_sync("The first research run stores its reply.", || {
        k.db().messages(WS).unwrap().len() >= 2
    })
    .await;
    k.core
        .say(WS, "那检索又是怎么做的？讲讲代码里的实现", &[])
        .unwrap();
    let c = ask(2).await;
    assert_eq!(c.models, ["claude-opus-5-5", "claude-sonnet-5-5"]);
    assert_eq!(c.effort, Some(steve_types::ReasoningEffort::Max));
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
    let acp = ModelScript::new(&json!({"kind": "answer", "confidence": 0.9}).to_string());
    let gw = crate::stats::RecordingGateway::new(
        std::sync::Arc::new(SingleModelGateway {
            one: acp.clone(),
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
    let (_, prompt) = crate::gateway::split_static(run["prompt"].as_str().unwrap());
    assert!(
        prompt.contains("你好") && !prompt.contains("Bots (id: role):"),
        "{prompt}"
    );

    // a chat answer: the adapter's message is the answer
    acp.set_reply(json!({"thought": "想了想", "answer": "四"}).to_string());
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

    // A chat failure never sends to the second model. A separate route still uses its own override.
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
    assert!(ans.failed, "{ans:?}");
    assert_eq!(ans.model, None);
    assert_eq!(runs(&acp), [("broken".into(), Some("low".into()))]);
    acp.set_reply(json!({"kind": "answer", "confidence": 0.9}).to_string());
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
    let rows: Vec<Booked> =
        steve_store::core_fixture::usage::usage(k.db().store(), Some("gateway"))
            .into_iter()
            .map(|r| {
                (
                    r.purpose,
                    r.harness,
                    r.model,
                    r.tokens.input,
                    r.tokens.cache_read,
                    r.ok,
                )
            })
            .collect();
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
            (
                "chat".into(),
                "codex".into(),
                "broken".into(),
                None,
                None,
                false
            ),
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
    let acp = ModelScript::new("标题");
    k.db()
        .bot_patch(
            "steve",
            &crate::patch![harness: "codex", model: serde_json::Value::Null],
            "me",
        )
        .unwrap();
    let cli = |acp: &ModelScript| SingleModelGateway {
        one: acp.clone(),
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
            &crate::gateway::ThinkTools::None,
        ),
    )
    .await
    .unwrap();
    assert_eq!(p.text, "标题");
    let run = acp.runs().remove(0);
    assert_eq!(run["model"], serde_json::Value::Null, "{run}");
    let mut without = ModelScript::new("标题");
    without.absent = Some("codex".into());
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

/// A chat answer passes its image to the selected model.
#[tokio::test]
async fn the_adapter_gets_the_images_of_a_chat_answer() {
    use base64::Engine;
    let k = Kit::new();
    let acp = ModelScript::new(&json!({"thought": "", "answer": "是一张图"}).to_string());
    k.db()
        .bot_patch(
            "steve",
            &crate::patch![harness: "codex", model: "gpt-6-luna"],
            "me",
        )
        .unwrap();
    let cli = SingleModelGateway {
        one: acp.clone(),
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
        run["images"][0],
        json!({"type": "image", "mimeType": "image/png",
            "data": base64::engine::general_purpose::STANDARD.encode(b"PNGDATA")})
    );
}

/// Model recovery must work while the old provider is down, and remove the fast-purpose overrides
/// which would otherwise keep routing on that provider after switching the main Bot.
#[tokio::test]
async fn explicit_model_selection_switches_judgments_without_a_model_call() {
    let k = Kit::new();
    let models = k.core.models();
    let before = models.view(WS);
    let choice = crate::models::SteveSelection {
        lightweight: None,
        harness: "codex".into(),
        model: Some("gpt-test".into()),
        expected_revision: before["bot_revision"].as_i64().unwrap(),
        follow_purposes: true,
    };
    let after = models.select_steve(WS, &choice, "me").unwrap();
    assert_eq!(after["steve"]["harness"], "codex");
    for p in after["purposes"].as_array().unwrap() {
        assert_eq!(p["follows"], true);
        assert_eq!(p["effective"]["harness"], "codex");
        assert_eq!(p["effective"]["models"][0], "gpt-test");
    }
    assert!(!models.migrate(WS).unwrap());
    assert!(models.custom(WS, "route").is_none());
    // A stale tab cannot overwrite the newer preference.
    assert!(matches!(
        models.select_steve(WS, &choice, "me"),
        Err(crate::CoreError::Status(409, _))
    ));
}

#[tokio::test]
async fn explicit_model_selection_can_keep_separate_purposes() {
    let k = Kit::new();
    let models = k.core.models();
    let route = models.custom(WS, "route");
    let choice = crate::models::SteveSelection {
        lightweight: None,
        harness: "codex".into(),
        model: None,
        expected_revision: models.view(WS)["bot_revision"].as_i64().unwrap(),
        follow_purposes: false,
    };
    models.select_steve(WS, &choice, "me").unwrap();
    assert_eq!(models.custom(WS, "route"), route);
}

#[tokio::test]
async fn failed_model_selection_rolls_back_the_bot_and_settings() {
    let k = Kit::new();
    let models = k.core.models();
    let before = models.view(WS);
    steve_store::core_fixture::usage::reject_model_settings(k.db().store());
    let choice = crate::models::SteveSelection {
        lightweight: None,
        harness: "codex".into(),
        model: Some("gpt-test".into()),
        expected_revision: before["bot_revision"].as_i64().unwrap(),
        follow_purposes: true,
    };
    assert!(models.select_steve(WS, &choice, "me").is_err());
    let after = models.view(WS);
    assert_eq!(after["steve"], before["steve"]);
    assert_eq!(after["bot_revision"], before["bot_revision"]);
    assert_eq!(after["purposes"], before["purposes"]);
}

#[tokio::test]
async fn model_selection_rejects_unknown_or_native_only_harnesses_before_writing() {
    let k = Kit::new();
    let models = k.core.models();
    let before = models.view(WS);
    for harness in ["does-not-exist", "pi"] {
        let choice = crate::models::SteveSelection {
            lightweight: None,
            harness: harness.into(),
            model: Some("a-model".into()),
            expected_revision: before["bot_revision"].as_i64().unwrap(),
            follow_purposes: true,
        };
        assert!(matches!(
            models.select_steve(WS, &choice, "me"),
            Err(crate::CoreError::Status(400, _))
        ));
        assert_eq!(models.view(WS)["steve"], before["steve"]);
    }
}

#[tokio::test]
async fn primary_and_lightweight_models_cover_all_purposes_and_respect_overrides() {
    let k = Kit::new();
    let models = k.core.models();
    let mut choice = crate::models::SteveSelection {
        harness: "codex".into(),
        model: Some("primary-model".into()),
        expected_revision: models.view(WS)["bot_revision"].as_i64().unwrap(),
        follow_purposes: true,
        lightweight: Some(crate::models::Choice {
            harness: "claude-code".into(),
            models: vec!["small-model".into()],
            effort: Some(steve_types::ReasoningEffort::Low),
        }),
    };
    let view = models.select_steve(WS, &choice, "me").unwrap();
    for p in view["purposes"].as_array().unwrap() {
        let light = p["role"] == "lightweight";
        assert_eq!(p["source"], if light { "lightweight" } else { "primary" });
        assert_eq!(
            p["effective"]["harness"],
            if light { "claude-code" } else { "codex" }
        );
        assert_eq!(
            p["effective"]["models"][0],
            if light {
                "small-model"
            } else {
                "primary-model"
            }
        );
        assert_eq!(p["follows"], true);
    }
    assert_eq!(
        models.for_call(WS, "intent", crate::Tier::Fast).models,
        ["small-model"]
    );
    assert_eq!(
        models.for_call(WS, "digest", crate::Tier::Fast).models,
        ["primary-model"]
    );
    models
        .apply(
            WS,
            &json!({"purposes":{"title":{"harness":"codex","models":["custom-title"]}}}),
            "me",
        )
        .unwrap();
    assert_eq!(models.for_purpose(WS, "title").models, ["custom-title"]);
    models
        .apply(WS, &json!({"purposes":{"title":null}}), "me")
        .unwrap();
    assert_eq!(models.for_purpose(WS, "title").models, ["small-model"]);
    choice.expected_revision = view["bot_revision"].as_i64().unwrap();
    choice.lightweight = None;
    models.select_steve(WS, &choice, "me").unwrap();
    for p in crate::models::PURPOSES {
        assert_eq!(models.for_purpose(WS, p.key).models, ["primary-model"]);
    }
}

#[tokio::test]
async fn lightweight_adapter_default_does_not_inherit_the_primary_model_or_effort() {
    let k = Kit::new();
    let models = k.core.models();
    let mut choice = crate::models::SteveSelection {
        harness: "codex".into(),
        model: Some("primary-model".into()),
        expected_revision: models.view(WS)["bot_revision"].as_i64().unwrap(),
        follow_purposes: true,
        lightweight: Some(crate::models::Choice {
            harness: "codex".into(),
            models: vec![],
            effort: None,
        }),
    };
    models.select_steve(WS, &choice, "me").unwrap();
    let route = models.for_purpose(WS, "route");
    assert!(!route.models.contains(&"primary-model".into()));
    assert_eq!(route.effort, None);
    choice.expected_revision = models.view(WS)["bot_revision"].as_i64().unwrap();
    choice.lightweight = None;
    models.select_steve(WS, &choice, "me").unwrap();
    assert_eq!(models.for_purpose(WS, "route").models[0], "primary-model");
}

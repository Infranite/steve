//! Runner behaviour against the scripted fake adapter (tests/fake-acp.js).
mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use steve_acp::{Acp, Prompted};

fn turn_ends(h: &MemHost) -> usize {
    h.rec(|r| r.turn_ends.len())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reasoning_effort_is_applied_after_model_and_read_back() {
    let f = Fake::new(&[("FAKE_ACP_SHAPE", "claude099"), ("FAKE_ACP_EFFORT", "1")]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.model = Some("kiro/claude-sonnet-5-5".into());
    s.reasoning_effort = Some("high".into());
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@say hi", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    let calls: Vec<_> = f
        .received()
        .into_iter()
        .filter(|m| m["method"] == "session/set_config_option")
        .collect();
    assert_eq!(calls[0]["params"]["configId"], "model");
    assert_eq!(calls[1]["params"]["configId"], "thought_level");
    assert_eq!(calls[1]["params"]["value"], "high");
    h.rec(|r| assert_eq!(r.sessions["s1"].reasoning_effort.as_deref(), Some("high")));
    assert_eq!(f.prompts(), ["@say hi"]);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn substituted_reasoning_is_reported_and_never_runs() {
    let f = Fake::new(&[
        ("FAKE_ACP_SHAPE", "claude099"),
        ("FAKE_ACP_EFFORT", "1"),
        ("FAKE_ACP_EFFORT_SUBSTITUTE", "low"),
    ]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.reasoning_effort = Some("high".into());
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "should not run", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert!(f.prompts().is_empty());
    h.rec(|r| {
        assert_eq!(r.sessions["s1"].reasoning_effort.as_deref(), Some("low"));
        assert!(r.turn_ends[0].error.is_some());
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_reasoning_effort_never_sends_the_prompt() {
    let f = Fake::new(&[("FAKE_ACP_SHAPE", "claude099"), ("FAKE_ACP_EFFORT", "1")]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.reasoning_effort = Some("xhigh".into());
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "should not run", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert!(f.prompts().is_empty());
    h.rec(|r| {
        assert!(
            r.turn_ends[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("xhigh")
        )
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_starts_the_adapter_and_ends() {
    let f = Fake::new(&[]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.perm_mode = Some("acceptEdits".into());
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    assert_eq!(
        acp.prompt("s1", "@say hi @usage 10 100 0.5", "me")
            .await
            .unwrap(),
        Prompted::Sent
    );
    assert!(acp.is_busy("s1"));
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert!(!acp.is_busy("s1"));

    assert_eq!(h.kinds(), ["user", "agent_message_chunk", "turn_end"]);
    h.rec(|r| {
        assert_eq!(
            r.events[0].1,
            json!({"text": "@say hi @usage 10 100 0.5", "via": "me"})
        );
        assert_eq!(r.events[2].1, json!({"stopReason": "end_turn"}));
        assert_eq!(r.statuses, ["starting", "running", "idle"]);
        assert_eq!(r.usage.unwrap().used, Some(10));
        assert_eq!(r.usage.unwrap().cost, Some(0.5));
        let s = &r.sessions["s1"];
        assert!(s.acp_session_id.as_deref().unwrap().starts_with("fake-"));
        assert_eq!(s.model.as_deref(), Some("fake-model"));
        assert_eq!(r.turn_ends[0].stop_reason, "end_turn");
    });
    let got = f.received();
    assert_eq!(got[0]["spawn"]["claude"], "/fake/claude");
    assert_eq!(
        f.methods(),
        [
            "initialize",
            "session/new",
            "session/set_mode",
            "session/prompt"
        ]
    );
    let init = &got[1];
    assert_eq!(
        init["params"]["clientInfo"],
        json!({"name": "steve", "version": "test"})
    );
    assert_eq!(init["params"]["protocolVersion"], 1);
    assert_eq!(
        got[2]["params"]["mcpServers"],
        json!([{"type": "http", "name": "steve", "url": "http://127.0.0.1:9/mcp", "headers": [{"name": "Authorization", "value": "Bearer tok-123"}]}])
    );
    assert_eq!(got[3]["params"]["modeId"], "acceptEdits");
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompts_queue_behind_a_turn_and_merge() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 600", "steve").await.unwrap();
    assert_eq!(acp.prompt("s1", "b", "me").await.unwrap(), Prompted::Queued);
    assert_eq!(acp.prompt("s1", "c", "me").await.unwrap(), Prompted::Queued);
    wait_for("both turns", 10, || turn_ends(&h) == 1).await;
    assert_eq!(f.prompts(), ["@sleep 600", "b\n\nc"]);
    // the first turn's end is replaced by the queued turn: one callback, two turn_end events
    assert_eq!(
        h.kinds(),
        [
            "user", "queued", "queued", "turn_end", "user", "user", "turn_end"
        ]
    );
    h.rec(|r| assert_eq!(r.events[1].1, json!({"text": "b", "via": "me"})));
    // the process is reused, not restarted
    assert_eq!(f.methods().iter().filter(|m| *m == "initialize").count(), 1);
    acp.shutdown(Duration::from_secs(2)).await;
}

/// The same message queued again while the first copy still waits is not added (no second `queued`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_message_is_queued_once() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 600", "me").await.unwrap();
    for via in ["steve", "me"] {
        assert_eq!(
            acp.prompt("s1", "验收通过", via).await.unwrap(),
            Prompted::Queued
        );
    }
    assert_eq!(
        acp.prompt("s1", "另一条", "me").await.unwrap(),
        Prompted::Queued
    );
    wait_for("both turns", 10, || turn_ends(&h) == 1).await;
    assert_eq!(f.prompts(), ["@sleep 600", "验收通过\n\n另一条"]);
    assert_eq!(h.kinds().iter().filter(|k| *k == "queued").count(), 2);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_ends_the_running_turn() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    assert!(!acp.cancel("s1"), "nothing to cancel");
    acp.prompt("s1", "@sleep 20000", "me").await.unwrap();
    wait_for("prompt sent", 10, || f.prompts().len() == 1).await;
    assert!(acp.cancel("s1"));
    wait_for("turn end", 5, || turn_ends(&h) == 1).await;
    assert_eq!(h.system(), ["你中断了当前回合"]);
    h.rec(|r| assert_eq!(r.turn_ends[0].stop_reason, "cancelled"));
    assert!(f.methods().contains(&"session/cancel".to_owned()));
    assert!(!acp.cancel("s1"));
    acp.shutdown(Duration::from_secs(2)).await;
}

/// A cancel while the adapter is still starting is not lost: it goes right after the prompt (the agent
/// has seen the message it interrupts), and the queue in order is the next turn (send-now, 插话).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_while_starting_goes_after_the_prompt_and_the_queue_runs_next() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 20000", "me").await.unwrap();
    assert_eq!(acp.prompt("s1", "b", "me").await.unwrap(), Prompted::Queued);
    assert_eq!(acp.prompt("s1", "c", "me").await.unwrap(), Prompted::Queued);
    assert!(f.prompts().is_empty(), "the adapter is still starting");
    assert!(acp.cancel("s1"));
    wait_for("the queued turn", 10, || turn_ends(&h) == 1).await;
    assert_eq!(f.prompts(), ["@sleep 20000", "b\n\nc"]);
    let m = f.methods();
    let at = |x: &str| m.iter().position(|y| y == x).unwrap();
    assert!(at("session/prompt") < at("session/cancel"), "{m:?}");
    assert_eq!(h.kinds().iter().filter(|k| *k == "turn_end").count(), 2);
    h.rec(|r| assert_eq!(r.turn_ends[0].stop_reason, "end_turn"));
    // a later cancel with nothing in flight is still false, and nothing is left pending
    assert!(!acp.cancel("s1"));
    acp.prompt("s1", "@say again", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 2).await;
    h.rec(|r| assert_eq!(r.turn_ends[1].stop_reason, "end_turn"));
    acp.shutdown(Duration::from_secs(2)).await;
}

/// A gateway error after the turn already ran tools: their effects happened, so the retry asks the agent
/// to check what is done and go on, instead of sending the same message to be acted on again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_after_tools_ran_goes_on_instead_of_starting_over() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    let first = "@tool edit Edit @fail 1 API Error: 529 overloaded @say ok";
    acp.prompt("s1", first, "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(
        f.prompts(),
        [first, steve_acp::RESUME_AFTER_TOOLS],
        "the retry continues, it does not repeat the message"
    );
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_gateway_errors_retry_then_succeed() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@fail 2 API Error: 503 overloaded @say ok", "me")
        .await
        .unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(
        h.system(),
        [
            "这一回合出错：API Error: 503 overloaded",
            "推理网关暂时不可用，0 秒后自动重试（第 1 次）",
            "这一回合出错：API Error: 503 overloaded",
            "推理网关暂时不可用，0 秒后自动重试（第 2 次）",
        ]
    );
    assert_eq!(f.prompts().len(), 3);
    assert_eq!(
        h.kinds().iter().filter(|k| *k == "user").count(),
        1,
        "retries do not repeat the user event"
    );
    h.rec(|r| {
        assert_eq!(r.turn_ends[0].stop_reason, "end_turn");
        assert_eq!(r.statuses.last().unwrap(), "idle");
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_are_bounded_and_other_errors_do_not_retry() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@fail 9 rate limit exceeded", "me")
        .await
        .unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(f.prompts().len(), 3);
    h.rec(|r| {
        assert_eq!(r.turn_ends[0].stop_reason, "error");
        assert_eq!(r.turn_ends[0].error.as_deref(), Some("rate limit exceeded"));
        assert_eq!(r.statuses.last().unwrap(), "error");
    });
    acp.prompt("s1", "@fail 1 invalid api key", "me")
        .await
        .unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 2).await;
    assert_eq!(f.prompts().len(), 4);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_retry_wait_drops_the_retry_and_runs_the_queue() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let mut cfg = f.config();
    cfg.retry_delays = vec![Duration::from_millis(1500)];
    let acp = Acp::new(cfg, h.clone());
    acp.prompt("s1", "@fail 1 503", "me").await.unwrap();
    wait_for("retry scheduled", 10, || h.system().len() == 2).await;
    assert!(acp.is_busy("s1"), "busy while waiting");
    assert_eq!(
        acp.prompt("s1", "next", "me").await.unwrap(),
        Prompted::Queued
    );
    assert!(acp.cancel("s1"));
    wait_for("queued turn", 10, || turn_ends(&h) == 1).await;
    tokio::time::sleep(Duration::from_millis(2000)).await; // the old retry timer must not fire
    assert_eq!(f.prompts(), ["@fail 1 503", "next"]);
    assert_eq!(h.system()[2], "你中断了当前回合，不再自动重试");
    h.rec(|r| {
        let ends: Vec<_> = r
            .events
            .iter()
            .filter(|(k, _)| k == "turn_end")
            .map(|(_, d)| d["stopReason"].clone())
            .collect();
        assert_eq!(ends, [json!("cancelled"), json!("end_turn")]);
        assert_eq!(r.turn_ends.len(), 1);
    });

    // without a queue the cancel reports the turn end itself
    acp.prompt("s1", "@fail 1 overloaded", "me").await.unwrap();
    wait_for("retry scheduled", 10, || h.system().len() == 5).await;
    assert!(acp.cancel("s1"));
    assert!(!acp.is_busy("s1"));
    h.rec(|r| assert_eq!(r.turn_ends[1].stop_reason, "cancelled"));
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(f.prompts().len(), 3);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adapter_crash_fails_the_turn_and_the_next_prompt_reloads() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@crash", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(h.system(), ["这一回合出错：adapter exited (3)"]);
    wait_for("entry dropped", 5, || acp.adapter_pid("s1").is_none()).await;
    let acp_id = h.rec(|r| r.sessions["s1"].acp_session_id.clone()).unwrap();
    acp.prompt("s1", "@say back", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 2).await;
    let load = f
        .received()
        .into_iter()
        .find(|m| m["method"] == "session/load")
        .expect("session/load");
    assert_eq!(load["params"]["sessionId"], acp_id.as_str());
    // history replayed during load is not stored
    assert_eq!(h.said(), "back");
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_failures_fail_the_turn_without_crashing() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let mut missing_cwd = f.session("s2", "claude-code");
    missing_cwd.cwd = "/nonexistent/steve-acp-test".into();
    h.with(missing_cwd);
    h.with(f.session("s3", "gemini"));
    let mut cfg = f.config();
    cfg.adapters
        .insert("claude-code".into(), "/nonexistent/adapter".into());
    let acp = Acp::new(cfg, h.clone());
    assert!(acp.prompt("nope", "x", "me").await.is_err());
    acp.prompt("s1", "x", "me").await.unwrap();
    wait_for("turn end", 5, || turn_ends(&h) == 1).await;
    assert!(
        h.system()[0].starts_with("无法启动 claude-code：spawn /nonexistent/adapter"),
        "{:?}",
        h.system()
    );
    h.rec(|r| {
        assert_eq!(
            (
                r.turn_ends[0].stop_reason.as_str(),
                r.statuses.last().unwrap().as_str()
            ),
            ("error", "error")
        )
    });
    assert!(!acp.is_busy("s1"));

    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s2", "x", "me").await.unwrap();
    wait_for("turn end", 5, || turn_ends(&h) == 2).await;
    acp.prompt("s3", "x", "me").await.unwrap();
    wait_for("turn end", 5, || turn_ends(&h) == 3).await;
    assert_eq!(
        h.system()[2],
        "无法启动 gemini：no adapter for harness gemini"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steve_tools_are_auto_allowed_others_become_decisions() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@perm mcp__steve__steve_deliver @perm Bash", "me")
        .await
        .unwrap();
    wait_for("decision", 10, || h.rec(|r| r.decisions.len()) == 1).await;
    assert_eq!(h.said(), "perm:selected:once");
    h.rec(|r| {
        let (id, d) = &r.decisions[0];
        assert_eq!(id, "d1");
        assert_eq!(d.question, "允许执行：Bash");
        assert_eq!(d.subject_id, "tc-1");
        assert_eq!(d.draft, "{\n \"tool\": \"Bash\"\n}");
        assert_eq!(
            d.options,
            [
                "allow|Always Allow|allow_always",
                "once|Allow|allow_once",
                "reject|Reject|reject_once"
            ]
        );
        assert_eq!(
            r.events.last().unwrap(),
            &(
                "permission".to_owned(),
                json!({"decision": "d1", "title": "Bash"})
            )
        );
    });
    assert!(!acp.answer_permission("d9", Some("once")));
    assert!(acp.answer_permission("d1", Some("reject")));
    assert!(
        !acp.answer_permission("d1", Some("reject")),
        "answered once"
    );
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(h.said(), "perm:selected:once|perm:selected:reject");

    acp.prompt("s1", "@perm mcp__steve__steve_other", "me")
        .await
        .unwrap();
    wait_for("decision", 10, || h.rec(|r| r.decisions.len()) == 2).await;
    assert!(acp.answer_permission("d2", None));
    wait_for("turn end", 10, || turn_ends(&h) == 2).await;
    assert!(h.said().ends_with("perm:cancelled:"));
    acp.shutdown(Duration::from_secs(2)).await;
}

/// A permission card lives as long as its turn: once the turn ends (here, cancelled while it waits) the card
/// is closed and an answer to it reaches nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_card_ends_with_its_turn() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@perm Bash", "me").await.unwrap();
    wait_for("decision", 10, || h.rec(|r| r.decisions.len()) == 1).await;
    assert!(h.rec(|r| r.expired.is_empty()));
    assert!(acp.cancel("s1"));
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(h.rec(|r| r.expired.clone()), ["d1"], "closed with its turn");
    assert!(
        !acp.answer_permission("d1", Some("once")),
        "nobody waits for it"
    );
    // the cancel was of that prompt only: the next one asks and is asked
    acp.prompt("s1", "@perm Bash", "me").await.unwrap();
    wait_for("second decision", 10, || h.rec(|r| r.decisions.len()) == 2).await;
    assert!(
        acp.answer_permission("d2", Some("once")),
        "the new card is live"
    );
    wait_for("second turn end", 10, || turn_ends(&h) == 2).await;
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_sessions_use_config_options_and_codex_modes() {
    let f = Fake::new(&[("FAKE_ACP_CODEX", "1")]);
    let h = host();
    let mut s = f.session("s1", "codex");
    s.model = Some("gpt-6-sol".into());
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@codexperm steve_update @fsread", "me")
        .await
        .unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(
        h.said(),
        "perm:selected:approved|fs:-32601:fs/read_text_file not supported by Steve"
    );
    let got = f.received();
    let set_mode = got
        .iter()
        .find(|m| m["method"] == "session/set_mode")
        .unwrap();
    assert_eq!(set_mode["params"]["modeId"], "read-only");
    let set = got
        .iter()
        .find(|m| m["method"] == "session/set_config_option")
        .unwrap();
    assert_eq!(set["params"]["configId"], "model");
    assert_eq!(set["params"]["value"], "gpt-6-sol");
    assert!(!f.methods().contains(&"session/set_model".to_owned()));
    // a model already chosen is not overwritten by the reported default
    assert_eq!(
        h.rec(|r| r.sessions["s1"].model.clone()).as_deref(),
        Some("gpt-6-sol")
    );
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_sessions_with_a_model_call_set_model() {
    let f = Fake::new(&[]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.model = Some("kiro/claude-haiku-5-5".into());
    s.perm_mode = Some("default".into());
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "x", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(
        f.methods(),
        [
            "initialize",
            "session/new",
            "session/set_model",
            "session/prompt"
        ]
    );
    let got = f.received();
    assert_eq!(
        got.iter()
            .find(|m| m["method"] == "session/set_model")
            .unwrap()["params"]["modelId"],
        "kiro/claude-haiku-5-5"
    );
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_adapters_are_released_and_resume_with_load() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let mut cfg = f.config();
    cfg.idle_release = Duration::from_millis(300);
    let acp = Acp::new(cfg, h.clone());
    acp.prompt("s1", "x", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    let pid = acp.adapter_pid("s1").unwrap();
    acp.release("s1"); // idle → killed now
    assert!(acp.adapter_pid("s1").is_none());
    wait_for("process gone", 5, || !alive(pid)).await;

    acp.prompt("s1", "@sleep 800", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 2).await;
    let pid = acp.adapter_pid("s1").unwrap();
    wait_for("idle release", 5, || acp.adapter_pid("s1").is_none()).await;
    wait_for("process gone", 5, || !alive(pid)).await;
    assert_eq!(
        f.methods().iter().filter(|m| *m == "session/load").count(),
        1
    );
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_spares_a_busy_adapter() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 500", "me").await.unwrap();
    wait_for("prompt sent", 10, || f.prompts().len() == 1).await;
    acp.release("s1");
    assert!(acp.adapter_pid("s1").is_some());
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(h.rec(|r| r.turn_ends[0].stop_reason.clone()), "end_turn");
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_leaves_no_orphans() {
    let f = Fake::new(&[]);
    let grandchild = f.dir.path().join("grandchild.pid");
    let f2 = Fake::new(&[("FAKE_ACP_GRANDCHILD", grandchild.to_str().unwrap())]);
    let h = host();
    h.with(f2.session("s1", "claude-code"));
    h.with(f2.session("s2", "claude-code"));
    let acp = Acp::new(f2.config(), h.clone());
    acp.prompt("s1", "@sleep 30000", "me").await.unwrap();
    wait_for("prompt sent", 10, || f2.prompts().len() == 1).await;
    acp.prompt("s2", "x", "me").await.unwrap();
    wait_for("idle second session", 10, || turn_ends(&h) == 1).await;
    let pids = [
        acp.adapter_pid("s1").unwrap(),
        acp.adapter_pid("s2").unwrap(),
    ];
    let gc: u32 = std::fs::read_to_string(&grandchild)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(gc));
    acp.shutdown(Duration::from_secs(3)).await;
    for p in pids.into_iter().chain([gc]) {
        wait_for("process gone", 5, || !alive(p)).await;
    }
    let ps = std::process::Command::new("ps")
        .args([
            "-o",
            "pid=",
            "-p",
            &format!("{},{},{}", pids[0], pids[1], gc),
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&ps.stdout).trim().is_empty(),
        "ps still lists: {}",
        String::from_utf8_lossy(&ps.stdout)
    );
    // the in-flight turn fails rather than hanging, and nothing new starts
    wait_for("turn end", 5, || turn_ends(&h) == 2).await;
    acp.prompt("s2", "x", "me").await.unwrap();
    wait_for("turn end", 5, || turn_ends(&h) == 3).await;
    assert!(h.system().last().unwrap().starts_with("无法启动"));
}

fn msg(text: &str, via: &str) -> steve_acp::Message {
    steve_acp::Message {
        text: text.into(),
        via: via.into(),
        files: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hold_queues_prompts_and_flush_runs_them_as_one_turn() {
    let f = Fake::new(&[]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.status = "starting".into();
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    acp.hold(true);
    assert!(acp.is_holding());
    assert_eq!(acp.prompt("s1", "a", "me").await.unwrap(), Prompted::Queued);
    // a session that never ran waits as idle instead of looking interrupted
    h.rec(|r| assert_eq!(r.sessions["s1"].status, "idle"));
    assert_eq!(acp.prompt("s1", "a", "me").await.unwrap(), Prompted::Queued);
    assert_eq!(acp.prompt("s1", "b", "me").await.unwrap(), Prompted::Queued);
    assert!(!acp.is_busy("s1"));
    assert_eq!(acp.running_turns(), 0);
    assert!(!acp.flush("s1", None), "no flush while holding");
    assert!(f.prompts().is_empty());

    acp.hold(false);
    assert!(acp.flush("s1", Some(msg("继续", "steve"))));
    assert_eq!(acp.running_turns(), 1);
    assert!(!acp.flush("s1", None), "a turn is running");
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(f.prompts(), ["继续\n\na\n\nb"]);
    h.rec(|r| {
        let users: Vec<_> = r
            .events
            .iter()
            .filter(|(k, _)| k == "user")
            .map(|(_, d)| d.clone())
            .collect();
        assert_eq!(
            users,
            [
                json!({"text": "继续", "via": "steve"}),
                json!({"text": "a", "via": "me"}),
                json!({"text": "b", "via": "me"})
            ]
        );
    });
    assert!(!acp.flush("s1", None), "nothing queued");
    assert_eq!(acp.running_turns(), 0);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_ending_under_hold_keeps_the_queue_and_skips_the_callback() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 500", "me").await.unwrap();
    wait_for("prompt sent", 10, || f.prompts().len() == 1).await;
    acp.hold(true);
    assert_eq!(
        acp.prompt("s1", "later", "me").await.unwrap(),
        Prompted::Queued
    );
    assert_eq!(acp.running_turns(), 1);
    wait_for("turn_end event", 10, || {
        h.kinds().contains(&"turn_end".to_owned())
    })
    .await;
    wait_for("turn out of flight", 5, || acp.running_turns() == 0).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        turn_ends(&h),
        0,
        "held messages remain: the turn is not over"
    );
    assert_eq!(f.prompts().len(), 1);
    assert_eq!(h.rec(|r| r.queue.get("s1").map_or(0, Vec::len)), 1);

    acp.hold(false);
    assert!(acp.flush("s1", None));
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    assert_eq!(f.prompts(), ["@sleep 500", "later"]);

    // holding with nothing queued: the callback runs as usual
    acp.prompt("s1", "x", "me").await.unwrap();
    acp.hold(true);
    wait_for("turn end", 10, || turn_ends(&h) == 2).await;
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_turns_count_permission_and_retry_waits() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    h.with(f.session("s2", "claude-code"));
    let mut cfg = f.config();
    cfg.retry_delays = vec![Duration::from_millis(1500)];
    let acp = Acp::new(cfg, h.clone());
    acp.prompt("s1", "@perm Bash", "me").await.unwrap();
    wait_for("decision", 10, || h.rec(|r| r.decisions.len()) == 1).await;
    acp.prompt("s2", "@fail 1 503", "me").await.unwrap();
    wait_for("retry scheduled", 10, || {
        h.system().iter().any(|t| t.starts_with("推理网关"))
    })
    .await;
    assert_eq!(acp.running_turns(), 2);

    // cancelling a retry under hold does not drain the queue, but still reports the turn end
    acp.hold(true);
    assert_eq!(
        acp.prompt("s2", "queued", "me").await.unwrap(),
        Prompted::Queued
    );
    assert!(acp.cancel("s2"));
    h.rec(|r| assert_eq!(r.turn_ends.last().unwrap().stop_reason, "cancelled"));
    assert_eq!(h.rec(|r| r.queue.get("s2").map_or(0, Vec::len)), 1);
    assert_eq!(acp.running_turns(), 1);

    assert!(acp.answer_permission("d1", Some("once")));
    wait_for("s1 done", 10, || acp.running_turns() == 0).await;
    acp.hold(false);
    assert!(acp.flush("s2", None));
    wait_for("flushed turn", 10, || turn_ends(&h) == 3).await;
    assert_eq!(f.prompts().last().unwrap(), "queued");
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_0_99_sets_the_model_by_config_option() {
    let f = Fake::new(&[
        ("FAKE_ACP_SHAPE", "claude099"),
        ("FAKE_ACP_NO_SET_MODEL", "1"),
    ]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.model = Some("kiro/claude-sonnet-5-5".into());
    h.with(s);
    h.with(f.session("s2", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "x", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    acp.release("s1");
    acp.prompt("s1", "y", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 2).await;
    let sets: Vec<_> = f
        .received()
        .into_iter()
        .filter(|m| m["method"] == "session/set_config_option")
        .map(|m| m["params"]["value"].clone())
        .collect();
    // set again after session/load, which resets the model
    assert_eq!(
        sets,
        [
            json!("kiro/claude-sonnet-5-5"),
            json!("kiro/claude-sonnet-5-5")
        ]
    );
    assert!(!f.methods().contains(&"session/set_model".to_owned()));

    // without a bot model the reported default is stored and nothing is set
    acp.prompt("s2", "x", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 3).await;
    assert_eq!(
        h.rec(|r| r.sessions["s2"].model.clone()).as_deref(),
        Some("kiro/claude-opus-5-5")
    );
    assert_eq!(
        f.methods()
            .iter()
            .filter(|m| *m == "session/set_config_option")
            .count(),
        2
    );
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_model_not_found_falls_back_to_the_config_option() {
    // `models` reported but set_model unsupported
    let f = Fake::new(&[("FAKE_ACP_NO_SET_MODEL", "1")]);
    let h = host();
    let mut s = f.session("s1", "claude-code");
    s.model = Some("claude-haiku-5-5".into());
    h.with(s);
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "x", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    let m = f.methods();
    let i = m.iter().position(|x| x == "session/set_model").unwrap();
    assert_eq!(m[i + 1], "session/set_config_option");
    acp.shutdown(Duration::from_secs(2)).await;
}

/// the content blocks of every session/prompt the fake received
fn prompt_blocks(f: &Fake) -> Vec<serde_json::Value> {
    f.received()
        .into_iter()
        .filter(|m| m["method"] == "session/prompt")
        .map(|m| m["params"]["prompt"].clone())
        .collect()
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nxx";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachments_go_by_path_and_images_inline_when_the_adapter_takes_them() {
    let f = Fake::new(&[("FAKE_ACP_IMAGE", "1")]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let png = h.add_file(
        f.dir.path(),
        "fil_00000000000000aa",
        "截图.png",
        "image/png",
        PNG,
    );
    let txt = h.add_file(
        f.dir.path(),
        "fil_00000000000000bb",
        "log.txt",
        "text/plain; charset=utf-8",
        b"hello",
    );
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt_files("s1", "看看", "me", vec![png, txt])
        .await
        .unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    let cwd = f.dir.path().display().to_string();
    let blocks = &prompt_blocks(&f)[0];
    assert_eq!(
        blocks[0],
        json!({"type": "text", "text": format!("看看\n\n用户附带了 2 个附件，已保存在工作目录里（绝对路径前缀 {cwd}/），需要时直接读取：\n- .steve/attachments/00000000000000aa-截图.png（image/png，1 KB，图片也直接附在这条消息里）\n- .steve/attachments/00000000000000bb-log.txt（text/plain，1 KB）")})
    );
    assert_eq!(
        blocks[1],
        json!({"type": "image", "data": "iVBORw0KGgp4eA==", "mimeType": "image/png"})
    );
    assert_eq!(blocks.as_array().unwrap().len(), 2);
    assert_eq!(
        std::fs::read(
            f.dir
                .path()
                .join(".steve/attachments/00000000000000aa-截图.png")
        )
        .unwrap(),
        PNG
    );
    // the user event keeps the original text and the refs, not the note or base64
    h.rec(|r| {
        assert_eq!(
            r.events[0].1,
            json!({"text": "看看", "via": "me", "files": [
                {"id": "fil_00000000000000aa", "name": "截图.png", "mime": "image/png", "size": 10},
                {"id": "fil_00000000000000bb", "name": "log.txt", "mime": "text/plain; charset=utf-8", "size": 5}]})
        )
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_image_blocks_for_adapters_without_the_capability() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "codex"));
    let png = h.add_file(
        f.dir.path(),
        "fil_00000000000000aa",
        "a.png",
        "image/png",
        PNG,
    );
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt_files("s1", "x", "me", vec![png]).await.unwrap();
    acp.prompt("s1", "plain", "me").await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    let blocks = prompt_blocks(&f);
    assert_eq!(blocks[0].as_array().unwrap().len(), 1);
    let text = blocks[0][0]["text"].as_str().unwrap();
    assert!(
        text.contains("- .steve/attachments/00000000000000aa-a.png（image/png，1 KB）"),
        "{text}"
    );
    assert_eq!(
        blocks[1],
        json!([{"type": "text", "text": "plain"}]),
        "no files, no note"
    );
    // a plain message has no files key
    h.rec(|r| {
        let q = r
            .events
            .iter()
            .find(|(k, d)| k == "queued" && d["text"] == "plain")
            .unwrap();
        assert_eq!(q.1, json!({"text": "plain", "via": "me"}));
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_attachments_survive_the_queue_and_flush_dedupes_on_text_and_files() {
    let f = Fake::new(&[("FAKE_ACP_IMAGE", "1")]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let png = h.add_file(
        f.dir.path(),
        "fil_00000000000000aa",
        "a.png",
        "image/png",
        PNG,
    );
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 400", "me").await.unwrap();
    assert_eq!(
        acp.prompt_files("s1", "pic", "me", vec![png.clone()])
            .await
            .unwrap(),
        Prompted::Queued
    );
    h.rec(|r| assert_eq!(r.events[1].1["files"][0]["id"], "fil_00000000000000aa"));
    wait_for("both turns", 10, || turn_ends(&h) == 1).await;
    let blocks = prompt_blocks(&f);
    assert!(
        blocks[1][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("pic\n\n用户附带了 1 个附件")
    );
    assert_eq!(blocks[1][1]["type"], "image");
    h.rec(|r| {
        assert_eq!(
            r.events
                .iter()
                .filter(|(k, d)| k == "user" && d["files"].is_array())
                .count(),
            1
        )
    });

    acp.hold(true);
    for files in [vec![], vec![], vec![png.clone()], vec![png.clone()]] {
        acp.prompt_files("s1", "same", "me", files).await.unwrap();
    }
    acp.hold(false);
    assert!(acp.flush("s1", None));
    wait_for("flushed", 10, || turn_ends(&h) == 2).await;
    let last = prompt_blocks(&f).pop().unwrap();
    let text = last[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("same\n\nsame\n\n用户附带了 1 个附件"),
        "{text}"
    );
    assert_eq!(text.matches("same").count(), 2);
    assert_eq!(last.as_array().unwrap().len(), 2, "one image");
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attachment_that_cannot_be_placed_becomes_a_note() {
    let f = Fake::new(&[("FAKE_ACP_IMAGE", "1")]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let png = h.add_file(
        f.dir.path(),
        "fil_00000000000000aa",
        "a.png",
        "image/png",
        PNG,
    );
    std::fs::remove_file(f.dir.path().join("blob-fil_00000000000000aa")).unwrap();
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt_files("s1", "x", "me", vec![png]).await.unwrap();
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    let blocks = prompt_blocks(&f);
    let text = blocks[0][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("x\n\n（用户附带了 1 个附件，但没能放进工作目录："),
        "{text}"
    );
    assert_eq!(blocks[0].as_array().unwrap().len(), 1);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn failed_queue_write_does_not_consume_an_older_message_or_acknowledge_new_input() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    {
        let mut r = h.0.lock().unwrap();
        r.queue.insert(
            "s1".into(),
            vec![steve_acp::Message {
                text: "old".into(),
                via: "me".into(),
                files: vec![],
            }],
        );
        r.reject_queue = true;
    }
    let acp = Acp::new(f.config(), h.clone());
    assert!(acp.prompt("s1", "new", "me").await.is_err());
    assert!(!acp.flush(
        "s1",
        Some(steve_acp::Message {
            text: "resume".into(),
            via: "steve".into(),
            files: vec![]
        })
    ));
    assert_eq!(h.rec(|r| r.queue["s1"].len()), 1);
    assert_eq!(acp.running_turns(), 0);
    assert!(f.prompts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receipt_write_failure_never_submits_a_prompt() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    h.0.lock().unwrap().reject_receipt = true;
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "retained", "me").await.unwrap();
    wait_for("receipt failure", 10, || {
        h.system().iter().any(|s| s.contains("无法保存发送收据"))
    })
    .await;
    assert!(f.prompts().is_empty());
    assert_eq!(acp.running_turns(), 0);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_quiesce_stops_adapter_groups_and_is_reversible() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@say before", "me").await.unwrap();
    wait_for("first turn", 10, || turn_ends(&h) == 1).await;
    let pid = acp.adapter_pid("s1").unwrap();
    assert!(acp.quiesce().await);
    // SAFETY: zero-signal probes the group of the test adapter.
    assert_ne!(unsafe { libc::kill(-(pid as i32), 0) }, 0);
    assert!(acp.is_holding());
    assert_eq!(
        acp.prompt("s1", "@say after", "me").await.unwrap(),
        Prompted::Queued
    );
    assert!(acp.reopen());
    assert!(acp.flush("s1", None));
    wait_for("second turn", 10, || turn_ends(&h) == 2).await;
    assert_eq!(f.prompts(), ["@say before", "@say after"]);
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_waiting_for_permission_does_not_hold_a_drain() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@perm Bash", "me").await.unwrap();
    wait_for("decision", 10, || h.rec(|r| r.decisions.len()) == 1).await;
    assert_eq!(
        (acp.running_turns(), acp.working_turns()),
        (1, 0),
        "it only waits for you"
    );
    let statuses = h.rec(|r| r.statuses.len());
    // the drain cuts it: nothing reported, the session stays as it was for the restart to resume
    assert!(acp.quiesce().await, "a waiting turn doesn't hold the drain");
    wait_for("cut", 10, || acp.running_turns() == 0).await;
    assert_eq!(turn_ends(&h), 0, "no turn end");
    assert_eq!(h.rec(|r| r.statuses.len()), statuses, "no status change");
    assert!(
        h.rec(|r| r.events.iter().any(|e| e.1["drain_cut"] == true)),
        "the cut is recorded"
    );
    // the drain is called off: the host continues it once maintenance is lifted (no restart will)
    let before = f.prompts().len();
    assert!(acp.reopen());
    let cut = acp.take_cut();
    assert_eq!(cut, ["s1"]);
    assert!(acp.continue_cut("s1"));
    wait_for("continued", 10, || f.prompts().len() > before).await;
    assert!(
        f.prompts().last().unwrap().contains("升级没有进行"),
        "{:?}",
        f.prompts().last()
    );
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_working_turn_still_holds_a_drain() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 2000", "me").await.unwrap();
    wait_for("running", 10, || acp.running_turns() == 1).await;
    assert_eq!(acp.working_turns(), 1);
    assert!(!acp.quiesce().await, "a working turn is waited for");
    assert!(acp.reopen());
    wait_for("turn end", 10, || turn_ends(&h) == 1).await;
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_turn_whose_prompt_returns_after_the_drain_ended_goes_on_by_itself() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@perm Bash", "me").await.unwrap();
    wait_for("decision", 10, || h.rec(|r| r.decisions.len()) == 1).await;
    let before = f.prompts().len();
    // the drain is called off right after it cut the turn: whichever comes first (its prompt coming
    // back, or the host collecting what was cut), the turn goes on and nothing reports a failure
    assert!(acp.quiesce().await);
    assert!(acp.reopen());
    for sid in acp.take_cut() {
        assert!(acp.continue_cut(&sid));
    }
    wait_for("continued", 10, || {
        for sid in acp.take_cut() {
            acp.continue_cut(&sid);
        }
        f.prompts().len() > before
    })
    .await;
    // the only turn end is the continued turn's own, and nothing failed
    assert!(
        h.rec(|r| r
            .turn_ends
            .iter()
            .all(|t| t.error.is_none() && t.stop_reason != "error")),
        "never reported as a failed turn"
    );
    assert!(
        !h.rec(|r| r.statuses.iter().any(|s| s == "error")),
        "never an error"
    );
    acp.shutdown(Duration::from_secs(2)).await;
}

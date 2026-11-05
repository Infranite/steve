//! What the runner reports for statistics, against the fake adapter.
mod common;

use std::time::Duration;

use common::*;
use serde_json::json;
use steve_acp::Acp;

fn finished(h: &MemHost) -> usize {
    h.rec(|r| r.finished.len())
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_on_two_models_gives_two_rows_with_tools_and_cost() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt(
        "s1",
        "@tool execute Bash @tool edit Edit failed @tool other mcp__steve__steve_deliver @usage 10 100 0.5 claude-opus-5-5 @tally 1 2 3 4 @quota claude-opus-5-5:10:20:30:40,claude-haiku-5-5:1:2:3:4",
        "me",
    )
    .await
    .unwrap();
    wait_for("turn end", 10, || h.rec(|r| r.turn_ends.len()) == 1).await;
    h.rec(|r| {
        assert_eq!(r.begun.len(), 1);
        assert_eq!(r.begun[0].attempt, 0);
        assert_eq!(r.begun[0].user_events, 1);
        let t = &r.finished[0];
        assert_eq!(t.turn_id, "tn1");
        assert_eq!(t.stop_reason, "end_turn");
        assert!(!t.transient);
        assert!(t.ended >= r.begun[0].started);
        assert_eq!((t.tools, t.tool_errors, t.permissions), (3, 1, 0));
        assert_eq!(t.tool_kinds["execute"], 1);
        assert_eq!(t.tool_kinds["edit"], 1);
        assert_eq!(t.tool_kinds["mcp:steve_deliver"], 1);
        assert_eq!(t.tool_kinds["tool:Bash"], 1);
        assert!(!t.tool_kinds.contains_key("other"));
        assert_eq!(t.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(t.usage.len(), 2);
        assert!(t.usage.iter().all(|u| u.scope == "all"));
        assert_eq!(t.usage[0].model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(t.usage[0].raw["cachedInputTokens"], 30);
        assert_eq!(t.usage[1].model.as_deref(), Some("claude-haiku-5-5"));
        assert!(close(t.cost.unwrap(), 0.5));
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_model_usage_one_main_row_under_the_last_model() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt(
        "s1",
        "@usage 5 100 0.1 claude-sonnet-5-5 @tally 1 2 3 4",
        "me",
    )
    .await
    .unwrap();
    wait_for("turn 1", 10, || finished(&h) == 1).await;
    // no usage_update with a model: the host falls back
    acp.prompt("s1", "@tally 7 8 0 0", "me").await.unwrap();
    wait_for("turn 2", 10, || finished(&h) == 2).await;
    // nothing reported at all
    acp.prompt("s1", "@say hi", "me").await.unwrap();
    wait_for("turn 3", 10, || finished(&h) == 3).await;
    h.rec(|r| {
        let u = &r.finished[0].usage;
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].scope, "main");
        assert_eq!(u[0].model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            u[0].raw,
            json!({"inputTokens": 1, "outputTokens": 2, "cachedReadTokens": 3, "cachedWriteTokens": 4, "totalTokens": 10})
        );
        assert_eq!(r.finished[1].usage[0].model, None);
        assert_eq!(r.finished[1].usage[0].raw["inputTokens"], 7);
        // the cost did not move since the first turn
        assert!(close(r.finished[1].cost.unwrap(), 0.0));
        assert!(r.finished[2].usage.is_empty());
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_follow_up_is_its_own_turn() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@sleep 500 @tally 1 1 1 1", "me")
        .await
        .unwrap();
    acp.prompt("s1", "b @tally 2 2 2 2", "me").await.unwrap();
    acp.prompt("s1", "c", "me").await.unwrap();
    wait_for("turn end", 10, || h.rec(|r| r.turn_ends.len()) == 1).await;
    h.rec(|r| {
        assert_eq!(r.begun.len(), 2);
        assert_eq!(r.begun[0].user_events, 1);
        assert_eq!(r.begun[1].user_events, 2);
        assert_eq!(r.begun[1].attempt, 0);
        assert_eq!(r.finished.len(), 2);
        assert_eq!(r.finished[0].usage[0].raw["inputTokens"], 1);
        assert_eq!(r.finished[1].usage[0].raw["inputTokens"], 2);
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_gateway_retry_is_a_turn_row() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt(
        "s1",
        "@fail 2 API Error: 503 overloaded @tally 1 2 3 4",
        "me",
    )
    .await
    .unwrap();
    wait_for("turn end", 10, || h.rec(|r| r.turn_ends.len()) == 1).await;
    h.rec(|r| {
        let att: Vec<u32> = r.begun.iter().map(|b| b.attempt).collect();
        assert_eq!(att, [0, 1, 2]);
        let users: Vec<usize> = r.begun.iter().map(|b| b.user_events).collect();
        assert_eq!(users, [1, 0, 0]);
        let tr: Vec<bool> = r.finished.iter().map(|t| t.transient).collect();
        assert_eq!(tr, [true, true, false]);
        assert_eq!(r.finished[0].stop_reason, "error");
        assert_eq!(
            r.finished[0].error.as_deref(),
            Some("API Error: 503 overloaded")
        );
        assert!(r.finished[0].usage.is_empty());
        assert_eq!(r.finished[2].stop_reason, "end_turn");
        assert_eq!(r.finished[2].usage.len(), 1);
    });
    // a non-transient error is a plain error row
    acp.prompt("s1", "@fail 1 invalid api key", "me")
        .await
        .unwrap();
    wait_for("turn 2", 10, || h.rec(|r| r.turn_ends.len()) == 2).await;
    h.rec(|r| {
        let t = r.finished.last().unwrap();
        assert_eq!((t.stop_reason.as_str(), t.transient), ("error", false));
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cost_increments_never_go_negative_across_adapter_restarts() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@usage 1 2 0.5", "me").await.unwrap();
    wait_for("turn 1", 10, || finished(&h) == 1).await;
    acp.prompt("s1", "@usage 1 2 0.8", "me").await.unwrap();
    wait_for("turn 2", 10, || finished(&h) == 2).await;
    // a new adapter process counts from zero
    acp.release("s1");
    acp.prompt("s1", "@usage 1 2 0.2", "me").await.unwrap();
    wait_for("turn 3", 10, || finished(&h) == 3).await;
    h.rec(|r| {
        let c: Vec<f64> = r.finished.iter().map(|t| t.cost.unwrap()).collect();
        assert!(
            close(c[0], 0.5) && close(c[1], 0.3) && close(c[2], 0.2),
            "{c:?}"
        );
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_turn_keeps_its_usage() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@tally 4 3 2 1 @sleep 5000", "me")
        .await
        .unwrap();
    wait_for("prompt sent", 10, || f.prompts().len() == 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(acp.cancel("s1"));
    wait_for("turn end", 10, || finished(&h) == 1).await;
    h.rec(|r| {
        let t = &r.finished[0];
        assert_eq!(t.stop_reason, "cancelled");
        assert_eq!(t.usage[0].raw["inputTokens"], 4);
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autonomous_cost_between_turns_is_reported_apart() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@usage 1 2 0.5 @later 300 0.9", "me")
        .await
        .unwrap();
    wait_for("autonomous", 10, || h.rec(|r| r.autonomous.len()) == 1).await;
    h.rec(|r| {
        let a = &r.autonomous[0];
        assert!(close(a.cost, 0.4));
        assert_eq!(a.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(a.origin, "task-notification");
        assert!(close(r.finished[0].cost.unwrap(), 0.5));
    });
    // already booked: the next turn only gets what is new
    acp.prompt("s1", "@usage 1 2 1.0", "me").await.unwrap();
    wait_for("turn 2", 10, || finished(&h) == 2).await;
    h.rec(|r| assert!(close(r.finished[1].cost.unwrap(), 0.1)));
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_permission_requests_for_the_user_count() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "claude-code"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "@perm mcp__steve__steve_update @perm Bash", "me")
        .await
        .unwrap();
    wait_for("decision", 10, || h.rec(|r| r.decisions.len()) == 1).await;
    assert!(acp.answer_permission("d1", Some("once")));
    wait_for("turn end", 10, || finished(&h) == 1).await;
    h.rec(|r| assert_eq!(r.finished[0].permissions, 1));
    acp.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adapter_that_cannot_start_is_an_error_turn() {
    let f = Fake::new(&[]);
    let h = host();
    h.with(f.session("s1", "nope"));
    let acp = Acp::new(f.config(), h.clone());
    acp.prompt("s1", "hi", "me").await.unwrap();
    wait_for("turn end", 10, || finished(&h) == 1).await;
    h.rec(|r| {
        let t = &r.finished[0];
        assert_eq!(t.stop_reason, "error");
        assert!(t.error.as_deref().unwrap().contains("no adapter"));
        assert_eq!(t.cost, None);
    });
    acp.shutdown(Duration::from_secs(2)).await;
}

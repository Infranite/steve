//! The MCP endpoint over HTTP, compared with what the TS server answers.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::connect_info::MockConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use steve_mcp::{END, PUBLISHED, router, testing::Recorder};
use tower::ServiceExt;

fn app(rec: Arc<Recorder>, peer: &str) -> Router {
    router(rec).layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()))
}

async fn post(app: &Router, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = Request::post(path)
        .header("host", "127.0.0.1:8440")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}})
}

#[tokio::test]
async fn refuses_without_token_or_from_outside() {
    let rec = Arc::new(Recorder::default());
    let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
    let (s, v) = post(
        &app(rec.clone(), "127.0.0.1:5000"),
        "/mcp",
        None,
        ping.clone(),
    )
    .await;
    assert_eq!((s, v), (StatusCode::UNAUTHORIZED, Value::Null));
    let (s, _) = post(
        &app(rec.clone(), "127.0.0.1:5000"),
        "/mcp",
        Some("wrong"),
        ping.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = post(
        &app(rec.clone(), "10.0.0.2:5000"),
        "/mcp",
        Some("tok"),
        ping.clone(),
    )
    .await;
    assert_eq!((s, v), (StatusCode::FORBIDDEN, Value::Null));
    let (s, _) = post(
        &app(rec.clone(), "[::ffff:127.0.0.1]:5000"),
        "/mcp",
        Some("tok"),
        ping.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post(&app(rec, "[::1]:5000"), "/mcpx", Some("tok"), ping).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn initialize_list_and_call() {
    let rec = Arc::new(Recorder::default());
    let app = app(rec.clone(), "127.0.0.1:5000");
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
    let (s, v) = post(&app, "/mcp", Some("tok"), init).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v["result"],
        json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"steve","version":"1.0.0"}})
    );

    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let mut want: Value = serde_json::from_str(include_str!("../src/tools.json")).unwrap();
    assert_eq!(v["result"]["tools"], want.take());

    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        call(
            3,
            "steve_ask",
            json!({"question":"哪个？","options":["a","b"]}),
        ),
    )
    .await;
    assert_eq!(
        v["result"],
        json!({"content":[{"type":"text","text":format!("Question sent to the user. {END}")}],"isError":false})
    );
    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        call(
            4,
            "steve_update",
            json!({"now":"写代码","plan":[{"step":"a","done":true,"x":1}],"extra":2}),
        ),
    )
    .await;
    assert_eq!(v["result"]["content"][0]["text"], "Steve updated the item.");
    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        call(
            5,
            "steve_request_approval",
            json!({"action":"推送","detail":"git push"}),
        ),
    )
    .await;
    assert_eq!(
        v["result"]["content"][0]["text"],
        format!("Approval requested. {END}")
    );
    let (_, v) = post(&app, "/mcp", Some("tok"), call(6, "steve_deliver", json!({"summary":"好了","outputs":[{"kind":"PR","title":"t"}],"checks":[{"name":"test","passed":true}],"unfinished":["b"]}))).await;
    assert_eq!(
        v["result"]["content"][0]["text"],
        format!("Delivered for acceptance. {END}")
    );
    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        call(
            7,
            "steve_publish",
            json!({"branch":"steve/st-12","summary":"新版本"}),
        ),
    )
    .await;
    assert_eq!(
        v["result"]["content"][0]["text"],
        format!("Release requested. {PUBLISHED} {END}")
    );
    let t = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("steve_lane_status"), "{t}");
    // read-only, no arguments: the text is the backend's, the turn goes on
    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        call(8, "steve_lane_status", json!({})),
    )
    .await;
    assert_eq!(
        v["result"],
        json!({"content":[{"type":"text","text":"在合并通道（github.com/x/y main）的 MQ-3 里"}],"isError":false})
    );
    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"steve_lane_status"}}),
    )
    .await;
    assert_eq!(v["result"]["isError"], false, "{v}");

    let calls = rec.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        [
            r#"s1 ask {"question":"哪个？","options":["a","b"]}"#,
            r#"s1 update {"now":"写代码","plan":[{"step":"a","done":true}]}"#,
            r#"s1 request_approval {"action":"推送","detail":"git push"}"#,
            r#"s1 deliver {"summary":"好了","outputs":[{"kind":"PR","title":"t"}],"checks":[{"name":"test","passed":true}],"unfinished":["b"]}"#,
            r#"s1 publish {"branch":"steve/st-12","summary":"新版本"}"#,
            "s1 lane_status",
            "s1 lane_status",
        ]
    );
}

#[tokio::test]
async fn errors_are_tool_results() {
    let rec = Arc::new(Recorder {
        fail: Some("boom 失败".into()),
        ..Default::default()
    });
    let app = app(rec.clone(), "127.0.0.1:5000");
    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        call(
            1,
            "steve_request_approval",
            json!({"action":"a","detail":"d"}),
        ),
    )
    .await;
    assert_eq!(
        v["result"],
        json!({"content":[{"type":"text","text":"boom 失败"}],"isError":true})
    );
    let (_, v) = post(&app, "/mcp", Some("tok"), call(2, "nope", json!({}))).await;
    assert_eq!(
        v["result"],
        json!({"content":[{"type":"text","text":"MCP error -32602: Tool nope not found"}],"isError":true})
    );
    let (_, v) = post(
        &app,
        "/mcp",
        Some("tok"),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"steve_update"}}),
    )
    .await;
    assert_eq!(
        v["result"]["content"][0]["text"],
        "MCP error -32602: Input validation error: Invalid arguments for tool steve_update: Invalid input: expected string, received undefined at now\nInvalid input: expected array, received undefined at plan"
    );
    assert_eq!(rec.calls.lock().unwrap().len(), 1);
}

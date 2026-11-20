//! Characterization of HTTP rendering, response shaping and early rejection before extraction.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use steve_flow::{
    FlowResult, engine, flows,
    graph::Graph,
    nodes::{
        NodeKind, NodeOutcome, RunCx,
        http::{BODY_CAP, Http},
    },
    runs,
    testkit::FlowEnv,
};
use steve_modules::testkit::{ME, WS};

fn run_row(e: &FlowEnv, cfg: &Value) -> (Graph, runs::RunRow) {
    let graph=Graph::from_value(&json!({"nodes":[{"id":"t","kind":"trigger","config":{"source":"manual"}},
        {"id":"h","kind":"http","title":"HTTP probe","config":cfg}],"edges":[{"from":"t","to":"h"}]})).unwrap();
    let row = e
        .env
        .store()
        .transaction(|tx| -> FlowResult<_> {
            let id = flows::create(
                tx,
                WS,
                ME,
                &flows::NewFlow {
                    kind: "flow",
                    name: "HTTP table",
                    description: "",
                    graph: &graph,
                    origin: "me",
                    origin_label: "",
                    enabled: false,
                },
                1,
            )?;
            let f = flows::own(tx, WS, &id)?;
            let id = engine::open_run(
                tx,
                &f,
                "manual",
                json!({"steps":{},"input":{}}),
                None,
                false,
                None,
                0,
                1,
            )?;
            Ok(runs::get(tx, &id)?.unwrap())
        })
        .unwrap();
    (graph, row)
}
async fn invoke(e: &FlowEnv, cfg: &Value, input: &Value, dry: bool) -> NodeOutcome {
    let (graph, run) = run_row(e, cfg);
    let mut cx = RunCx {
        flow: &e.flow,
        run,
        graph: &graph,
        node: graph.node("h").unwrap(),
        dry_run: dry,
        sender: Value::Null,
        effects: Default::default(),
    };
    if dry {
        Http::default().dry_run(&mut cx, cfg, input).await
    } else {
        Http::default().run(&mut cx, cfg, input).await
    }
}
#[derive(Clone)]
struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    got: Arc<Mutex<Vec<Value>>>,
}
async fn answer(
    State(reply): State<Reply>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    reply.got.lock().unwrap().push(json!({"method":method.as_str(),"probe":headers.get("x-probe").and_then(|v|v.to_str().ok()),
        "content_type":headers.get("content-type").and_then(|v|v.to_str().ok()),"body":String::from_utf8_lossy(&body)}));
    Response::builder()
        .status(reply.status)
        .header("content-type", reply.content_type)
        .body(Body::from(reply.body))
        .unwrap()
}

#[tokio::test]
async fn sg7_http_request_and_response_table() {
    // status, response bytes, request body, custom content type, expected error
    for (status, raw, body, custom, error) in [
        (
            200,
            br#"{"status":999,"content_type":"spoofed","body":"spoofed","claim":"C1"}"#.to_vec(),
            Value::Null,
            false,
            None,
        ),
        (
            201,
            br#"[1,2]"#.to_vec(),
            json!({"note":"{{input.note}}"}),
            false,
            None,
        ),
        (200, b"true".to_vec(), json!("{{input.note}}"), false, None),
        (
            200,
            b"plain response".to_vec(),
            json!({"note":"{{input.note}}"}),
            true,
            None,
        ),
        (200, vec![b'x'; BODY_CAP + 1], Value::Null, false, None),
        (200, vec![b'x'; BODY_CAP], Value::Null, false, None),
        (
            400,
            b"  refused  ".to_vec(),
            Value::Null,
            false,
            Some("请求失败：HTTP 400：refused".to_string()),
        ),
        (
            422,
            br#"{"reason":"no"}"#.to_vec(),
            Value::Null,
            false,
            Some("请求失败：HTTP 422".to_string()),
        ),
        (
            503,
            b" \n ".to_vec(),
            Value::Null,
            false,
            Some("请求失败：HTTP 503".to_string()),
        ),
        (
            500,
            "错".repeat(310).into_bytes(),
            Value::Null,
            false,
            Some(format!("请求失败：HTTP 500：{}", "错".repeat(300))),
        ),
    ] {
        let e = FlowEnv::new();
        e.env.store().with_conn(|c|{c.execute("INSERT OR REPLACE INTO settings(workspace_id,key,value,updated) VALUES (?,'http_allow_private','[\"127.0.0.1\"]',1)",[WS])?;Ok(())}).unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let reply = Reply {
            status,
            content_type: "application/json",
            body: raw.clone(),
            got: got.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, 8440);
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/probe", any(answer)).with_state(reply),
            )
            .await
            .unwrap()
        });
        let method = if body.is_null() { " get " } else { " post " };
        let mut cfg = json!({"url":format!("http://127.0.0.1:{port}/probe"),"method":method,"effect":!body.is_null(),"body":body,"headers":{"X-Probe":"{{input.name}}"}});
        if custom {
            cfg["headers"]["Content-Type"] = json!("text/custom");
        }
        let input = json!({"note":"Ada \"quote\"\nline","name":"Ada"});
        let out = invoke(&e, &cfg, &input, false).await;
        if let Some(error) = error {
            assert!(
                matches!(out,NodeOutcome::Fail(ref e) if e==&error),
                "{out:?}"
            );
        } else {
            let NodeOutcome::Next { output, branch } = out else {
                panic!("{out:?}")
            };
            assert!(branch.is_none());
            assert_eq!(output["status"], status);
            assert_eq!(output["content_type"], "application/json");
            if raw.len() > BODY_CAP {
                assert_eq!(output["truncated"], true);
                assert_eq!(output["body"].as_str().unwrap().len(), BODY_CAP);
            } else {
                assert!(output.get("truncated").is_none());
                match serde_json::from_slice::<Value>(&raw) {
                    Ok(value) => assert_eq!(output["body"], value),
                    Err(_) => assert_eq!(output["body"], String::from_utf8_lossy(&raw).as_ref()),
                }
            }
            if raw.starts_with(br#"{"status""#) {
                assert_eq!(output["claim"], "C1");
                assert_eq!(output["body"]["status"], 999);
            }
        }
        let requests = got.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request["method"],
            if body.is_null() { "GET" } else { "POST" }
        );
        assert_eq!(request["probe"], input["name"]);
        if body.is_object() {
            assert_eq!(
                serde_json::from_str::<Value>(request["body"].as_str().unwrap()).unwrap(),
                json!({"note":input["note"]})
            );
            assert_eq!(
                request["content_type"],
                if custom {
                    "text/custom"
                } else {
                    "application/json"
                }
            );
        } else if body.is_string() {
            assert_eq!(request["body"], input["note"]);
            assert!(request["content_type"].is_null());
        } else {
            assert_eq!(request["body"], "");
            assert!(request["content_type"].is_null());
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test]
async fn sg7_http_early_failure_order_and_secret_dry_run_table() {
    for (cfg, input, dry, expected) in [
        (
            json!({"method":"POST","url":"bad"}),
            json!({}),
            false,
            "没有副作用的请求只能用 GET",
        ),
        (
            json!({"url":"{{input.url}}"}),
            json!({"url":"not a url"}),
            false,
            "网址不对：not a url",
        ),
        (
            json!({"url":"{{input.url}}"}),
            json!({"url":"ftp://example.invalid/"}),
            false,
            "网址只能是 http 或 https",
        ),
        (
            json!({"url":"http://127.0.0.1:9/","headers":{"X-Key":"{{secrets.missing.token}}"}}),
            json!({}),
            false,
            "不能请求内网地址",
        ),
        (
            json!({"url":"https://example.invalid/","headers":{"X-Key":"{{secrets.missing.token}}"}}),
            json!({}),
            false,
            "保险箱还没有接上",
        ),
        (
            json!({"url":"https://example.invalid/","headers":{"X-Key":"{{secrets.missing.token}}"}}),
            json!({}),
            true,
            "用到了机密，试运行时不发送",
        ),
    ] {
        let e = FlowEnv::new();
        let out = invoke(&e, &cfg, &input, dry).await;
        if dry {
            assert!(
                matches!(out,NodeOutcome::Next{ref output,..} if output.to_string().contains(expected)),
                "{out:?}"
            );
        } else {
            assert!(
                matches!(out,NodeOutcome::Fail(ref error) if error.starts_with(expected)),
                "{out:?}"
            );
        }
        assert!(e.env.gateway.calls.lock().unwrap().is_empty());
    }
}

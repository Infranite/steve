//! SG32: header construction through real loopback requests, without external services.
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method},
    routing::any,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use steve_flow::{
    FlowResult, engine, flows,
    graph::Graph,
    nodes::{NodeKind, NodeOutcome, RunCx, http::Http},
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

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn capture(
    State(got): State<Arc<Mutex<Vec<Value>>>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> &'static str {
    got.lock().unwrap().push(json!({"method":method.as_str(),"content_type":headers.get("content-type").and_then(|v|v.to_str().ok()),"probe":headers.get_all("x-probe").iter().map(|v|v.to_str().unwrap()).collect::<Vec<_>>(),"body":String::from_utf8_lossy(&body)}));
    "ok"
}
#[tokio::test]
async fn sg32_http_header_append_json_default_and_rendered_rejection_table() {
    for mode in [
        "default-json",
        "custom-json",
        "duplicate",
        "newline",
        "bad-name",
    ] {
        let e = FlowEnv::new();
        e.env.store().with_conn(|c|{c.execute("INSERT OR REPLACE INTO settings(workspace_id,key,value,updated) VALUES (?,'http_allow_private','[\"127.0.0.1\"]',1)",[WS])?;Ok(())}).unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/", any(capture))
            .with_state(got.clone());
        let _server = Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        }));
        let mut cfg = json!({"url":format!("http://127.0.0.1:{port}/"),"method":"POST","effect":true,"body":{"note":"{{input.note}}"},"headers":{"X-Probe":"{{input.name}}"}});
        match mode {
            "custom-json" => cfg["headers"]["content-type"] = json!("text/custom"),
            "duplicate" => cfg["headers"]["x-probe"] = json!("second"),
            "bad-name" => cfg["headers"]["bad name"] = json!("value"),
            _ => {}
        }
        let input = json!({"name":if mode=="newline"{"Ada\nInjected: yes"}else{"Ada"},"note":"quote \" and newline\n"});
        let out = invoke(&e, &cfg, &input, false).await;
        if matches!(mode, "newline" | "bad-name") {
            assert!(
                matches!(out,NodeOutcome::Fail(ref e) if e.contains(if mode=="newline"{"的值不合法"}else{"请求头名字不对"})),
                "{out:?}"
            );
            assert!(got.lock().unwrap().is_empty());
        } else {
            assert!(matches!(out, NodeOutcome::Next { .. }), "{out:?}");
            let rows = got.lock().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["method"], "POST");
            assert_eq!(
                rows[0]["content_type"],
                if mode == "custom-json" {
                    "text/custom"
                } else {
                    "application/json"
                }
            );
            assert_eq!(
                rows[0]["probe"],
                if mode == "duplicate" {
                    json!(["Ada", "second"])
                } else {
                    json!(["Ada"])
                }
            );
            assert_eq!(
                serde_json::from_str::<Value>(rows[0]["body"].as_str().unwrap()).unwrap(),
                json!({"note":input["note"]})
            );
        }
    }
}

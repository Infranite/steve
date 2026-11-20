mod sg38;
use serde_json::{Value, json};
use sg38::{BOT, Mcp, TOKEN};
use steve_hub::wire::tool_service::Reach;

#[tokio::test]
async fn session_routing_preserves_peer_auth_method_and_initialize_boundaries() {
    let h = Mcp::new().await;
    let init = json!({"jsonrpc":"2.0","id":7,"method":"initialize"}).to_string();
    for (method, token, peer, expected) in [
        ("POST", Some(TOKEN), None, 403),
        ("POST", Some(TOKEN), Some("192.0.2.1:1234"), 403),
        ("POST", None, Some("127.0.0.1:1234"), 401),
        ("POST", Some("wrong-session"), Some("127.0.0.1:1234"), 401),
        ("GET", Some(TOKEN), Some("127.0.0.1:1234"), 405),
        ("POST", Some(TOKEN), Some("[::ffff:127.0.0.1]:1234"), 200),
    ] {
        assert_eq!(
            h.raw(method, token, peer, init.clone()).await.status(),
            expected
        );
    }
    let r = h.rpc("initialize", json!({})).await;
    assert_eq!(r["id"], 7);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "steve");
    let r = h
        .rpc("initialize", json!({"protocolVersion":"client-contract"}))
        .await;
    assert_eq!(r["result"]["protocolVersion"], "client-contract");
    for (method, body, expected) in [("GET", init, 405), ("POST", "{invalid".into(), 400)] {
        assert_eq!(
            h.raw(method, Some(&h.access), Some("192.0.2.1:1234"), body)
                .await
                .status(),
            expected
        );
    }
    let r = h
        .raw(
            "POST",
            None,
            None,
            json!({"method":"notifications/initialized"}).to_string(),
        )
        .await;
    assert_eq!(
        axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap(),
        "passed through"
    );
    h.stop().await;
}

#[tokio::test]
async fn module_dispatch_preserves_device_updates_and_reply_source_validation() {
    let h = Mcp::new().await;
    let list = h
        .service
        .call(h.session.clone(), "steve_devices", json!({"action":"list"}))
        .await
        .unwrap();
    let list: Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list["devices"][0]["node"], "local");
    assert_eq!(list["devices"][0]["online"], true);
    for (args, word) in [(json!({}), "undefined"), (json!({"text":7}), "other")] {
        let e = h
            .service
            .call(h.session.clone(), "steve_reply_source", args)
            .await
            .unwrap_err();
        assert!(e.contains(&format!("received {word} at text")), "{e}");
    }
    let out = h
        .service
        .call(
            h.session.clone(),
            "steve_reply_source",
            json!({"text":"result"}),
        )
        .await
        .unwrap();
    assert_eq!(out, steve_channels::outbound::SRC_NO_SOURCE);
    let r = h
        .rpc(
            "tools/call",
            json!({"name":"absent_contract_tool","arguments":{}}),
        )
        .await;
    assert_eq!(r["result"]["isError"], true);
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Tool absent_contract_tool not found")
    );
    let response = h
        .service
        .call(
            h.session.clone(),
            "steve_devices",
            json!({"action":"require"}),
        )
        .await
        .unwrap();
    assert!(response.contains("不再要求设备标签"));
    h.stop().await;
}

struct Remote {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Remote {
    async fn new(failed: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app=axum::Router::new().fallback(axum::routing::post(move |axum::Json(m):axum::Json<Value>| async move {
            let result=match m["method"].as_str().unwrap_or_default() {
                "initialize"=>json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"finite peer","version":"1"}}),
                "tools/list" if failed=>return axum::Json(json!({"jsonrpc":"2.0","id":m["id"],"error":{"code":-32603,"message":"fixture list refused"}})),
                "tools/list"=>json!({"tools":[{"name":"read","description":"Read contract evidence","inputSchema":{"type":"object","properties":{}},"annotations":{"readOnlyHint":true}}]}),
                _=>json!({}),
            };
            axum::Json(json!({"jsonrpc":"2.0","id":m["id"],"result":result}))
        }));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, task }
    }
}
impl Drop for Remote {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn registry_reach_combines_whole_server_and_tool_pins_without_losing_unreachable_servers() {
    let h = Mcp::new().await;
    let peers = [
        Remote::new(false).await,
        Remote::new(false).await,
        Remote::new(true).await,
    ];
    let mut ids = Vec::new();
    for (name, peer) in ["whole", "single", "broken"].into_iter().zip(&peers) {
        let v = h
            .service
            .extra
            .caps
            .mcp_create(
                "default",
                &h.user,
                serde_json::from_value(
                    json!({"name":name,"transport":"http","url":peer.url,"bot":BOT}),
                )
                .unwrap(),
            )
            .unwrap();
        ids.push(v["id"].as_str().unwrap().to_string());
    }
    for (kind, target) in [("mcp", ids[0].as_str()), ("tool", "single__read")] {
        h.service
            .extra
            .caps
            .pin_tool("default", &h.user, BOT, kind, target, true)
            .unwrap();
    }
    for (reach, remote_count, broken) in [
        (Reach::Listed, 2, false),
        (Reach::Tool("single__read"), 1, false),
        (Reach::All, 2, true),
    ] {
        let r = h.service.registry(&h.session, reach).await;
        let remotes: Vec<_> = r
            .entries
            .iter()
            .filter(|e| e.name.ends_with("__read"))
            .collect();
        assert_eq!(remotes.len(), remote_count);
        assert_eq!(r.unreachable.len(), usize::from(broken));
        if broken {
            assert_eq!(r.unreachable[0].0, "broken");
            assert!(r.unreachable[0].1.contains("fixture list refused"));
        }
        let names: Vec<_> = r.listed_entries().iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"single__read"));
        if reach != Reach::Tool("single__read") {
            assert!(names.contains(&"whole__read"));
        }
    }
    let r = h.rpc("tools/list", json!({})).await;
    let names: Vec<_> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"whole__read") && names.contains(&"single__read"));
    h.stop().await;
}

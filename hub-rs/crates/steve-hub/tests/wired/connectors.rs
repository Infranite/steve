//! The actual Hub wiring, session-authenticated MCP endpoint and websocket connector provider.
//! No direct calls to Channels or synthetic registry entries: discovery/pins/calls use public routes.

use super::*;
use steve_channels::connector::tool_name;
use steve_channels::fake::ext_ws::{FakeExtWs, TOKEN};

async fn rpc(e: &Env, token: &str, body: Value) -> Value {
    let r = e
        .http
        .post(format!("{}/mcp", e.base))
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .expect("MCP request");
    let status = r.status();
    let text = r.text().await.unwrap();
    assert!(status.is_success(), "{status}: {text}");
    serde_json::from_str(&text).expect("MCP JSON")
}

async fn call(e: &Env, name: &str, arguments: Value) -> Value {
    rpc(
        e,
        "connector-session-token",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":name,"arguments":arguments}}),
    )
    .await
}

fn answer(v: &Value) -> Value {
    assert_ne!(v["result"]["isError"], true, "{v}");
    serde_json::from_str(v["result"]["content"][0]["text"].as_str().expect("text"))
        .expect("connector answer JSON")
}

async fn catalog(e: &Env) -> Vec<Value> {
    rpc(
        e,
        "connector-session-token",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await["result"]["tools"]
        .as_array()
        .expect("catalog")
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn registry_pins_and_dispatch_use_current_session_account_and_references() {
    let fake = FakeExtWs::start().await.unwrap();
    fake.with(|w| {
        w.features = vec!["me".into(), "changes".into()];
        w.connector = Some(json!({"version":1,"id":"fixture","operations":[
            {"name":"search","description":"Search original documents","capability":"search",
             "inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false},
             "annotations":{"readOnlyHint":true,"idempotentHint":true,"steve":{"resources":["document"]}}},
            {"name":"read","description":"Read original document","capability":"read",
             "inputSchema":{"type":"object","properties":{"reference":{"type":"string"}},"required":["reference"],"additionalProperties":false},
             "bindings":{"reference":"reference"},"annotations":{"readOnlyHint":true,"steve":{"resources":["document"]}}},
            {"name":"update","description":"Update original document as you","capability":"write",
             "inputSchema":{"type":"object","properties":{"reference":{"type":"string"},"text":{"type":"string"}},"required":["reference","text"],"additionalProperties":false},
             "bindings":{"reference":"reference"},"annotations":{"readOnlyHint":false,"destructiveHint":true,"steve":{"resources":["document"],"asYou":true}}}
        ]}));
        w.connector_results.insert("search".into(), json!({"references":[{
            "kind":"reference","external_id":"document-7","original_type":"document","title":"Design"
        }]}));
        w.connector_results.insert("read".into(), json!({"content":[{"type":"text","text":"original"}]}));
    });
    let e = start().await;
    assert_eq!(e.call("POST", "/api/vault/init", None).await.0, 200);
    let (status, connection) = e
        .call(
            "POST",
            "/api/connections",
            Some(json!({
                "adapter":"ext_ws","fields":{"url":fake.url(),"token":TOKEN}
            })),
        )
        .await;
    assert_eq!(status, 200, "{connection}");
    let conn = connection["id"].as_str().unwrap();
    let me = store_fixture::default_workspace_member(&e.db).unwrap();
    let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
        b"connector-session-token",
    ));
    store_fixture::seed_connector_session(&e.db, &me, &hash).unwrap();
    let search = tool_name(conn, "search");
    let read = tool_name(conn, "read");
    let update = tool_name(conn, "update");
    assert!(!catalog(&e).await.iter().any(|t| t["name"] == search));
    let discovered = answer(
        &call(
            &e,
            "steve_tools_search",
            json!({"source":"connector","resource":"document","kind":"read"}),
        )
        .await,
    );
    let tools = discovered["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2, "{discovered}");
    let found = tools.iter().find(|t| t["name"] == search).unwrap();
    assert_eq!(
        found["annotations"]["steve"]["source"],
        format!("connector:fixture/{conn}")
    );
    assert_eq!(found["annotations"]["idempotentHint"], true);
    assert!(
        fake.calls("connector.call").is_empty(),
        "discovery never executes operations"
    );
    assert!(fake.calls("messages.history").is_empty());
    let writes = answer(
        &call(
            &e,
            "steve_tools_search",
            json!({"source":format!("connector:fixture/{conn}"),"kind":"write"}),
        )
        .await,
    );
    assert_eq!(writes["tools"][0]["name"], update);
    assert_eq!(writes["tools"][0]["annotations"]["steve"]["asYou"], true);
    let pin = format!("/api/bots/steve/tool-pins/tool/{search}");
    assert_eq!(e.call("PUT", &pin, None).await.0, 200);
    assert!(catalog(&e).await.iter().any(|t| t["name"] == search));
    assert_eq!(e.call("DELETE", &pin, None).await.0, 200);
    assert!(!catalog(&e).await.iter().any(|t| t["name"] == search));
    let bad = call(
        &e,
        "steve_tool_call",
        json!({"name":search,"arguments":{"query":"Design","account":"forged"}}),
    )
    .await;
    assert_eq!(bad["result"]["isError"], true, "{bad}");
    assert!(fake.calls("connector.call").is_empty());
    let result = answer(
        &call(
            &e,
            "steve_tool_call",
            json!({"name":search,"arguments":{"query":"Design"}}),
        )
        .await,
    );
    let reference = result["references"][0]["reference_id"].as_str().unwrap();
    assert!(reference.starts_with("ref_"));
    let n: i64 = store_fixture::connector_reference_count(&e.db).unwrap();
    assert_eq!(n, 1, "real Store boot applied migration 070");
    let n: i64 = store_fixture::channel_message_count(&e.db).unwrap();
    assert_eq!(n, 0, "search does not import history");
    let bad = call(
        &e,
        "steve_tool_call",
        json!({"name":read,"arguments":{"reference":"document-7"}}),
    )
    .await;
    assert_eq!(bad["result"]["isError"], true, "{bad}");
    let result = answer(
        &call(
            &e,
            "steve_tool_call",
            json!({"name":read,"arguments":{"reference":reference}}),
        )
        .await,
    );
    assert_eq!(result["content"][0]["text"], "original");
    assert_eq!(
        fake.calls("connector.call").last().unwrap()["arguments"]["reference"],
        "document-7"
    );
    fake.with(|w| w.lose_connector_response = true);
    let unknown = answer(
        &call(
            &e,
            "steve_tool_call",
            json!({"name":update,"arguments":{"reference":reference,"text":"change"}}),
        )
        .await,
    );
    assert_eq!(unknown["status"], "unknown");
    assert_eq!(unknown["retry"], false);
    assert_eq!(
        fake.with(|w| w
            .writes
            .iter()
            .filter(|(m, _, _)| m == "connector.call")
            .count()),
        1
    );
    let calls = fake.calls("connector.call").len();
    // Existing source-reply MCP now commits an intent and a real C6 draft atomically. No model
    // backend or outbound platform is required to exercise identity and durable submission.
    store_fixture::seed_connector_output_source(&e.db, conn).unwrap();
    let first = answer(
        &call(
            &e,
            "steve_reply_source",
            json!({"text":"Progress recorded","key":"wired-output"}),
        )
        .await,
    );
    assert_eq!(first["status"], "draft");
    let again = answer(
        &call(
            &e,
            "steve_reply_source",
            json!({"text":"Progress recorded","key":"wired-output"}),
        )
        .await,
    );
    assert_eq!(first["id"], again["id"]);
    let delivery = e
        .call(
            "GET",
            &format!("/api/deliveries/{}", first["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(delivery.0, 200);
    assert_eq!(delivery.1["message_id"], first["message_id"]);
    let intent = json!({
        "key":"wired-output", "identity":"as_you",
        "content":{"markdown":"Progress recorded"},
        "source_message_id":"output-source",
        "reason":"item:connector-item", "urgency":"normal"
    });
    let submitted = e
        .call("POST", "/api/deliveries", Some(intent.clone()))
        .await;
    assert_eq!(submitted.0, 200, "{submitted:?}");
    assert_eq!(submitted.1["id"], first["id"]);
    let mut changed = intent;
    changed["content"]["markdown"] = json!("A different update");
    assert_eq!(
        e.call("POST", "/api/deliveries", Some(changed)).await.0,
        409
    );
    let n: i64 = store_fixture::output_delivery_count(&e.db).unwrap();
    assert_eq!(n, 1);
    store_fixture::expire_connector_refs(&e.db).unwrap();
    assert_eq!(
        call(
            &e,
            "steve_tool_call",
            json!({"name":read,"arguments":{"reference":reference}})
        )
        .await["result"]["isError"],
        true
    );
    assert_eq!(fake.calls("connector.call").len(), calls);
    // Capability removal and pause are checked even with an existing pin/known name/reference.
    let path = format!("/api/connections/{conn}");
    assert_eq!(
        e.call("PATCH", &path, Some(json!({"caps":["msg.receive"]})))
            .await
            .0,
        200
    );
    let writes = answer(
        &call(
            &e,
            "steve_tools_search",
            json!({"source":"connector","kind":"write"}),
        )
        .await,
    );
    assert!(writes["tools"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &e,
            "steve_tool_call",
            json!({"name":update,"arguments":{"reference":reference,"text":"again"}})
        )
        .await["result"]["isError"],
        true
    );
    assert_eq!(
        e.call("PATCH", &path, Some(json!({"paused":true}))).await.0,
        200
    );
    assert_eq!(
        call(
            &e,
            "steve_tool_call",
            json!({"name":search,"arguments":{"query":"Design"}})
        )
        .await["result"]["isError"],
        true
    );
    assert_eq!(
        e.call("PATCH", &path, Some(json!({"paused":false})))
            .await
            .0,
        200
    );
    // The recorded requester wins over an owner audit; never borrow the owner identity.
    store_fixture::seed_connector_user(&e.db).unwrap();
    store_fixture::seed_connector_membership(&e.db).unwrap();
    store_fixture::set_connector_requester(&e.db, Some("connector-other")).unwrap();
    let hidden = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
    assert!(hidden["tools"].as_array().unwrap().is_empty(), "{hidden}");
    assert_eq!(
        call(
            &e,
            "steve_tool_call",
            json!({"name":search,"arguments":{"query":"Design"}})
        )
        .await["result"]["isError"],
        true
    );
    // A Node-bound session keeps its own principal even if the item later names the owner.
    store_fixture::set_connector_requester(&e.db, Some(&me)).unwrap();
    for principal in ["connector-other", "svc:default"] {
        store_fixture::bind_connector_principal(&e.db, principal).unwrap();
        let hidden = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
        assert!(
            hidden["tools"].as_array().unwrap().is_empty(),
            "{principal}: {hidden}"
        );
        assert_eq!(
            call(
                &e,
                "steve_tool_call",
                json!({"name":search,"arguments":{"query":"Design"}})
            )
            .await["result"]["isError"],
            true,
            "{principal} must not borrow the item's new requester"
        );
        let denied = call(
            &e,
            "steve_reply_source",
            json!({"text":"Must not borrow owner access","key":format!("denied-{principal}")}),
        )
        .await;
        assert_eq!(denied["result"]["isError"], true, "{principal}: {denied}");
        let n: i64 = store_fixture::output_delivery_count(&e.db).unwrap();
        assert_eq!(n, 1);
        store_fixture::unbind_connector_principal(&e.db).unwrap();
    }
    // Service work with an owner audit must not acquire the owner's connector either.
    store_fixture::set_connector_requester(&e.db, None).unwrap();
    let hidden = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
    assert!(hidden["tools"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &e,
            "steve_tool_call",
            json!({"name":search,"arguments":{"query":"Design"}})
        )
        .await["result"]["isError"],
        true
    );
    store_fixture::set_connector_session_item(&e.db, None).unwrap();
    let hidden = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
    assert!(hidden["tools"].as_array().unwrap().is_empty());
    // Conversely, a recorded personal principal is usable only while still a member.
    store_fixture::bind_connector_principal(&e.db, &me).unwrap();
    let visible = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
    assert_eq!(visible["tools"].as_array().unwrap().len(), 2);
    store_fixture::remove_connector_membership(&e.db, &me).unwrap();
    let hidden = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
    assert!(hidden["tools"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &e,
            "steve_tool_call",
            json!({"name":search,"arguments":{"query":"Design"}})
        )
        .await["result"]["isError"],
        true
    );
    store_fixture::set_connector_session_item(&e.db, Some("connector-item")).unwrap();
    let denied = call(
        &e,
        "steve_reply_source",
        json!({"text":"Departed requester","key":"departed"}),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let n: i64 = store_fixture::output_delivery_count(&e.db).unwrap();
    assert_eq!(n, 1);
    assert_eq!(fake.calls("connector.call").len(), calls);
    assert!(fake.calls("messages.history").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connector_declaring_calendar_v1_is_a_calendar_source() {
    let fake = FakeExtWs::start().await.unwrap();
    let now = steve_store::now();
    fake.with(|w| {
        w.features = vec!["me".into(), "changes".into()];
        // version 2: a profile and no operations of its own
        w.connector = Some(json!({"version":2,"id":"calfix","profiles":[
            {"id":"calendar.v1","caps":["cal.read"],"features":["instances"]}]}));
        w.connector_results.insert(
            "calendar.v1 cal.calendars.list".into(),
            json!({"calendars":[{"id":"primary","name":"Team","role":"owner"}]}),
        );
        w.connector_results.insert(
            "calendar.v1 cal.events.list".into(),
            json!({"events":[{"id":"e1","title":"Plan","start":{"ms":now + 3_600_000},"end":{"ms":now + 5_400_000}}]}),
        );
    });
    let e = start_with(&[("STEVE_CALENDAR_DEMO", "1".into())]).await;
    assert_eq!(e.call("POST", "/api/vault/init", None).await.0, 200);
    let (status, connection) = e
        .call(
            "POST",
            "/api/connections",
            Some(json!({"adapter":"ext_ws","fields":{"url":fake.url(),"token":TOKEN}})),
        )
        .await;
    assert_eq!(status, 200, "{connection}");
    let conn = connection["id"].as_str().unwrap();
    let (s, v) = e
        .call("POST", &format!("/api/calendar/sources/{conn}/sync"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["calendars"][0]["name"], "Team");
    assert_eq!(v["features"], json!(["instances"]));
    let (s, v) = e
        .call(
            "GET",
            &format!("/api/calendar/events?from={}&to={}", now, now + 86_400_000),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["events"][0]["title"], "Plan", "{v}");
    let methods: Vec<String> = fake.with(|w| {
        w.profile_calls
            .iter()
            .map(|p| p["method"].as_str().unwrap_or("").to_string())
            .collect()
    });
    assert_eq!(methods, ["cal.calendars.list", "cal.events.list"]);
    assert!(fake.calls("connector.call").is_empty());
    // the built-in sources are on the 接入 page (the demo only because it was asked for)
    let (_, x) = e.call("GET", "/api/connections/integrations", None).await;
    let providers: Vec<&str> = x["providers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert!(
        providers.contains(&"ics") && providers.contains(&"cal_demo"),
        "{providers:?}"
    );
}

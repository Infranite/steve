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
    let db = rusqlite::Connection::open(&e.db).unwrap();
    let me: String = db
        .query_row(
            "SELECT user_id FROM workspace_members WHERE workspace_id='default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    e.sql("INSERT INTO items (id,workspace_id,no,title,status,stage,bot_id,requested_by,created,updated) VALUES ('connector-item','default',9001,'connector fixture','active','working','steve',?1,1,1)", &[&me]);
    e.sql("INSERT INTO audit (workspace_id,ts,actor,action,target) VALUES ('default',1,?1,'item.create','connector-item')", &[&me]);
    let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
        b"connector-session-token",
    ));
    e.sql("INSERT INTO sessions (id,workspace_id,item_id,bot_id,harness,cwd,status,token_hash,created,updated) VALUES ('connector-session','default','connector-item','steve','claude-code','/nonexistent','idle',?1,1,1)", &[&hash]);
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
    let n: i64 = db
        .query_row("SELECT count(*) FROM connector_refs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "real Store boot applied migration 070");
    let n: i64 = db
        .query_row("SELECT count(*) FROM chan_messages", [], |r| r.get(0))
        .unwrap();
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
    e.sql("UPDATE connector_refs SET expires=1", &[]);
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
    e.sql("INSERT INTO users (id,workspace_id,subject,name,role,created) VALUES ('connector-other','default','connector-other','Other','member',1)", &[]);
    e.sql("INSERT INTO workspace_members (workspace_id,user_id,role,created) VALUES ('default','connector-other','member',1)", &[]);
    e.sql(
        "UPDATE items SET requested_by='connector-other' WHERE id='connector-item'",
        &[],
    );
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
    e.sql(
        "UPDATE items SET requested_by=?1 WHERE id='connector-item'",
        &[&me],
    );
    for principal in ["connector-other", "svc:default"] {
        e.sql("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated,principal) VALUES ('connector-session','node','{}','connector-session','r','ready',1,?1)", &[&principal]);
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
        e.sql(
            "DELETE FROM runtime_sessions WHERE session_id='connector-session'",
            &[],
        );
    }
    // Service work with an owner audit must not acquire the owner's connector either.
    e.sql(
        "UPDATE items SET requested_by=NULL WHERE id='connector-item'",
        &[],
    );
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
    e.sql(
        "UPDATE sessions SET item_id=NULL WHERE id='connector-session'",
        &[],
    );
    let hidden = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
    assert!(hidden["tools"].as_array().unwrap().is_empty());
    // Conversely, a recorded personal principal is usable only while still a member.
    e.sql("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated,principal) VALUES ('connector-session','node','{}','connector-session','r','ready',1,?1)", &[&me]);
    let visible = answer(&call(&e, "steve_tools_search", json!({"source":"connector"})).await);
    assert_eq!(visible["tools"].as_array().unwrap().len(), 2);
    e.sql(
        "DELETE FROM workspace_members WHERE workspace_id='default' AND user_id=?1",
        &[&me],
    );
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
    assert_eq!(fake.calls("connector.call").len(), calls);
    assert!(fake.calls("messages.history").is_empty());
}

use axum::{body::Body, http::Request};
use serde_json::{Value, json};
use steve_store::{NewDecision, NewSession, NewWorkspace};
use tower::ServiceExt;

use super::fixture::{Env, body};

fn error(value: &Value, code: i64, message: &str) {
    assert_eq!(
        value,
        &json!({"jsonrpc":"2.0","id":"sg20","error":{"code":code,"message":message}})
    );
}

#[tokio::test]
async fn sg20_a2a_authentication_precedes_body_parsing_and_enforces_workspace() {
    let e = Env::new().await;
    let r = e
        .unavailable
        .clone()
        .oneshot(Request::post("/a2a").body(Body::from("{")).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    for auth in [None, Some("Bearer stt_wrong"), Some("bearer stt_wrong")] {
        let r = e.raw("{", auth, None).await;
        assert_eq!(r.status(), 401);
        assert_eq!(r.headers()["www-authenticate"], "Bearer");
        assert_eq!(
            body(r).await,
            json!({"error":"a Steve access token is needed","code":"unauthenticated","retry":{"kind":"never"},"reason":null,"params":{}})
        );
    }
    let auth = format!("Bearer {}", e.token);
    assert_eq!(
        e.raw("{", Some(&auth), Some("not-my-workspace"))
            .await
            .status(),
        401
    );
    let r = e.raw("{", Some(&auth), None).await;
    assert_eq!(r.status(), 200);
    assert_eq!(
        body(r).await,
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
    );
    assert_eq!(
        e.raw("x".repeat(4 * 1024 * 1024 + 1), Some(&auth), None)
            .await
            .status(),
        413
    );
    let r = e.raw("{}", Some(&auth), None).await;
    assert_eq!(
        body(r).await,
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"Method not found: "}})
    );
    assert!(e.domain.script.lock().unwrap().calls.is_empty());
    e.stop().await;
}

#[tokio::test]
async fn sg20_a2a_method_errors_and_scoped_task_misses_are_exact() {
    let e = Env::new().await;
    for (method, code, message) in [
        (
            "message/stream",
            -32004,
            "Streaming is not supported: use message/send and tasks/get",
        ),
        (
            "tasks/resubscribe",
            -32004,
            "Streaming is not supported: use message/send and tasks/get",
        ),
        (
            "tasks/pushNotificationConfig/set",
            -32003,
            "Push notifications are not supported",
        ),
        ("unknown", -32601, "Method not found: unknown"),
        ("message/send", -32602, "the message has no text"),
        ("tasks/get", -32001, "Task not found: no task "),
        ("tasks/cancel", -32001, "Task not found: "),
    ] {
        error(&e.rpc(method, json!({})).await, code, message);
    }
    let ws = e
        .store
        .workspaces()
        .create(
            &NewWorkspace {
                name: "Other".into(),
                ..Default::default()
            },
            &e.user,
        )
        .unwrap();
    let foreign = e.item(&ws.id, "outside");
    for id in ["missing", foreign.id.as_str()] {
        error(
            &e.rpc("tasks/get", json!({"id":id})).await,
            -32001,
            &format!("Task not found: no task {id}"),
        );
        error(
            &e.rpc("tasks/cancel", json!({"id":id})).await,
            -32001,
            &format!("Task not found: {id}"),
        );
        error(
            &e.rpc(
                "message/send",
                json!({"message":{"taskId":id,"parts":[{"kind":"text","text":"extra"}]}}),
            )
            .await,
            -32001,
            &format!("Task not found: {id}"),
        );
    }
    assert!(e.domain.script.lock().unwrap().calls.is_empty());
    assert_eq!(
        e.store.items().get(&foreign.id).unwrap().unwrap().status,
        foreign.status
    );
    e.stop().await;
}

#[tokio::test]
async fn sg20_a2a_message_parts_creation_title_and_domain_arguments() {
    let e = Env::new().await;
    let text = "abcdefghijklmnopqrstuvwxyz";
    let r = e.rpc("message/send", json!({"message":{"taskId":"","parts":[
        {"kind":"text","text":format!("  {text}")},
        {"kind":"data","data":{"answer":42}}, {"kind":"file","file":{"uri":"file:///fixture"}},
        {"kind":"file","file":{}}, {"kind":"image","text":"ignored"}, {"kind":"text","text":"  "}
    ]}})).await;
    let id = r["result"]["id"].as_str().expect("created task");
    assert_eq!(r["result"]["status"]["state"], "submitted");
    let expected = format!("{text}\n{{\"answer\":42}}\n[file](file:///fixture)");
    {
        let s = e.domain.script.lock().unwrap();
        assert_eq!(
            s.calls,
            [json!(["route", "default", expected]), json!(["create"])]
        );
        let p = &s.made[0];
        assert_eq!(p.goal, expected);
        assert_eq!(p.title, "abcdefghijklmnopqrstuvwx");
        assert_eq!(p.ws, "default");
        assert_eq!(p.bot.as_deref(), Some("writer"));
        assert_eq!(p.team, None);
        assert!(p.deps.is_empty());
        assert_eq!(p.project, Some(None));
        assert_eq!(p.tag_ids, [super::fixture::kind(&e.store, "default")]);
        assert_eq!(p.source, ["A2A · SG20 caller", "a2a"]);
        assert_eq!(p.mode, "start");
        assert_eq!(p.actor, e.user);
        assert_eq!(p.opening, None);
    }
    assert_eq!(
        e.store.items().get(id).unwrap().unwrap().title,
        "abcdefghijklmnopqrstuvwx"
    );
    e.domain.script.lock().unwrap().title = "  Routed title  ".into();
    let r = e
        .rpc(
            "message/send",
            json!({"message":{"parts":[{"kind":"text","text":"new"}]}}),
        )
        .await;
    assert_eq!(r["result"]["metadata"]["title"], "  Routed title  ");
    let r = e
        .rpc(
            "message/send",
            json!({"message":{"taskId":id,"parts":[{"kind":"text","text":" extra "}]}}),
        )
        .await;
    assert_eq!(r["result"]["id"], id);
    assert_eq!(
        e.domain.script.lock().unwrap().calls.last().unwrap(),
        &json!(["supplement", id, "extra", "auto", []])
    );
    e.stop().await;
}

#[tokio::test]
async fn sg20_a2a_domain_failures_keep_codes_and_prevent_later_calls() {
    let e = Env::new().await;
    let item = e.item("default", "existing");
    for (fail, method, params, code) in [
        (
            "route",
            "message/send",
            json!({"message":{"parts":[{"kind":"text","text":"hello"}]}}),
            -32603,
        ),
        (
            "create",
            "message/send",
            json!({"message":{"parts":[{"kind":"text","text":"hello"}]}}),
            -32603,
        ),
        (
            "supplement",
            "message/send",
            json!({"message":{"taskId":item.id,"parts":[{"kind":"text","text":"hello"}]}}),
            -32603,
        ),
        ("cancel", "tasks/cancel", json!({"id":item.id}), -32002),
    ] {
        e.domain.script.lock().unwrap().fail = fail;
        error(
            &e.rpc(method, params).await,
            code,
            &format!("{fail} refused"),
        );
        assert_eq!(e.store.items().all("default").unwrap().len(), 1);
    }
    e.domain.script.lock().unwrap().fail = "missing-created-id";
    error(
        &e.rpc(
            "message/send",
            json!({"message":{"parts":[{"kind":"text","text":"hello"}]}}),
        )
        .await,
        -32603,
        "no task ",
    );
    e.domain.script.lock().unwrap().fail = "";
    let r = e.rpc("tasks/cancel", json!({"id":item.id})).await;
    assert_eq!(r["result"]["status"]["state"], "canceled");
    assert_eq!(
        e.domain.script.lock().unwrap().calls.last().unwrap(),
        &json!(["action", item.id, "drop", e.user])
    );
    e.stop().await;
}

#[tokio::test]
async fn sg20_a2a_task_state_precedence_and_delivery_artifacts() {
    let e = Env::new().await;
    for (status, outcome, open, started, expected) in [
        ("run", "", false, false, "submitted"),
        ("run", "", false, true, "working"),
        ("run", "", true, false, "input-required"),
        ("run", "failed", true, true, "failed"),
        ("drop", "failed", true, true, "canceled"),
        ("done", "failed", true, true, "completed"),
    ] {
        let item = e.item("default", expected);
        e.store
            .transaction(|tx| -> steve_store::Result<_> {
                tx.execute(
                    "UPDATE items SET status=?1,outcome=?2 WHERE id=?3",
                    rusqlite::params![status, outcome, item.id],
                )?;
                Ok(())
            })
            .unwrap();
        if started {
            e.store
                .sessions()
                .create(&NewSession {
                    workspace_id: "default".into(),
                    item_id: Some(item.id.clone()),
                    harness: "claude-code".into(),
                    cwd: e.st.cfg.home.display().to_string(),
                    token: format!("token-{}", item.id),
                    ..Default::default()
                })
                .unwrap();
        }
        if open {
            e.store
                .decisions()
                .create(&NewDecision {
                    workspace_id: "default".into(),
                    kind: "ask".into(),
                    subject_type: "item".into(),
                    subject_id: item.id.clone(),
                    item_id: Some(item.id.clone()),
                    question: "Question?".into(),
                    ..Default::default()
                })
                .unwrap()
                .unwrap();
        }
        let r = e.rpc("tasks/get", json!({"id":item.id})).await;
        assert_eq!(r["result"]["status"]["state"], expected, "{r}");
        assert_eq!(r["result"]["kind"], "task");
        assert_eq!(r["result"]["contextId"], item.id);
        assert_eq!(r["result"]["status"]["message"]["role"], "agent");
        assert!(r["result"]["artifacts"].is_null());
    }
    let item = e.item("default", "delivery");
    let mut ids = vec![];
    for draft in ["old delivery", "new delivery", "  "] {
        let d = e
            .store
            .decisions()
            .create(&NewDecision {
                workspace_id: "default".into(),
                kind: "accept".into(),
                subject_type: "item".into(),
                subject_id: item.id.clone(),
                item_id: Some(item.id.clone()),
                draft: Some(draft.into()),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        ids.push(d.id);
        e.store
            .transaction(|tx| -> steve_store::Result<_> {
                tx.execute(
                    "UPDATE decisions SET status='closed',created=? WHERE id=?",
                    rusqlite::params![ids.len() as i64, ids.last().unwrap()],
                )?;
                Ok(())
            })
            .unwrap();
    }
    e.store
        .transaction(|tx| -> steve_store::Result<_> {
            tx.execute("UPDATE items SET status='done' WHERE id=?", [&item.id])?;
            Ok(())
        })
        .unwrap();
    let r = e.rpc("tasks/get", json!({"id":item.id})).await;
    assert_eq!(
        r["result"]["artifacts"],
        json!([{"artifactId":ids[1],"name":"delivery","parts":[{"kind":"text","text":"new delivery"}]}])
    );
    e.stop().await;
}

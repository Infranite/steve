//! A Bot's skills through `/bots/:id/skills`: validation, unique names, the switch, the limits on
//! what a session is given, deletion, the audit, and Bots the caller does not see.

use axum::Router;
use serde_json::{Value, json};
use steve_modules::bot_skills::{BotSkills, MAX_CONTENT, routes};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};

struct H {
    env: TestEnv,
    api: Router,
}

fn setup() -> H {
    let env = TestEnv::new(|_| {});
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('dev', 'default', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let api = routes(BotSkills::new(env.store().clone())).with_state(());
    H { env, api }
}

async fn req(h: &H, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call(&h.api, Some(&caller(WS, ME)), method, path, body.as_ref()).await
}

fn q<T: rusqlite::types::FromSql>(h: &H, sql: &str) -> T {
    h.env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

#[tokio::test]
async fn a_skill_is_added_changed_switched_and_removed() {
    let h = setup();
    let (s, v) = req(
        &h,
        "POST",
        "/bots/dev/skills",
        Some(
            json!({"name": "code-review", "description": "  Review a change before merging.  ",
                    "content": "# Review\n1. Read the diff."}),
        ),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("bskill_"));
    assert_eq!(v["description"], "Review a change before merging.");
    assert_eq!(v["enabled"], true);

    // the same name again: a conflict
    let (s, _) = req(
        &h,
        "POST",
        "/bots/dev/skills",
        Some(json!({"name": "code-review", "description": "x"})),
    )
    .await;
    assert_eq!(s, 409);
    // names a directory takes, a description of one line
    for bad in [
        json!({"name": "Code Review", "description": "x"}),
        json!({"name": "a--b", "description": "x"}),
        json!({"name": "ok", "description": ""}),
        json!({"name": "ok", "description": "two\nlines"}),
        json!({"name": "ok", "description": "x", "content": "a".repeat(MAX_CONTENT + 1)}),
        json!({"description": "no name"}),
    ] {
        let (s, v) = req(&h, "POST", "/bots/dev/skills", Some(bad.clone())).await;
        assert_eq!(s, 400, "{bad} → {v}");
    }

    let (s, v) = req(
        &h,
        "PATCH",
        &format!("/bots/dev/skills/{id}"),
        Some(json!({"content": "Changed."})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["content"], "Changed.");
    assert_eq!(v["name"], "code-review");
    let (s, v) = req(
        &h,
        "PATCH",
        &format!("/bots/dev/skills/{id}"),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!((s, &v["enabled"]), (200, &json!(false)));
    let (_, list) = req(&h, "GET", "/bots/dev/skills", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["content"], "Changed.");

    let verbs: String = q(
        &h,
        "SELECT group_concat(action, ',') FROM (SELECT action FROM audit WHERE action LIKE 'bot_skill.%' ORDER BY id)",
    );
    assert_eq!(verbs, "bot_skill.create,bot_skill.update,bot_skill.disable");

    let (s, _) = req(&h, "DELETE", &format!("/bots/dev/skills/{id}"), None).await;
    assert_eq!(s, 200);
    let (s, _) = req(&h, "DELETE", &format!("/bots/dev/skills/{id}"), None).await;
    assert_eq!(s, 404);
    let n: i64 = q(&h, "SELECT count(*) FROM bot_skills");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn what_a_session_is_given_stays_within_one_message() {
    let h = setup();
    let big = "a".repeat(MAX_CONTENT);
    for i in 0..4 {
        let (s, v) = req(
            &h,
            "POST",
            "/bots/dev/skills",
            Some(json!({"name": format!("s{i}"), "description": "x", "content": big})),
        )
        .await;
        assert_eq!(s, 200, "{v}");
    }
    // a fifth enabled one would be too much; added switched off it is kept
    let (s, _) = req(
        &h,
        "POST",
        "/bots/dev/skills",
        Some(json!({"name": "s4", "description": "x", "content": big})),
    )
    .await;
    assert_eq!(s, 400);
    let (s, v) = req(
        &h,
        "POST",
        "/bots/dev/skills",
        Some(json!({"name": "s4", "description": "x", "content": big, "enabled": false})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    // and cannot be switched on until another is switched off
    let id = v["id"].as_str().unwrap().to_string();
    let path = format!("/bots/dev/skills/{id}");
    let (s, _) = req(&h, "PATCH", &path, Some(json!({"enabled": true}))).await;
    assert_eq!(s, 400);
    let (_, list) = req(&h, "GET", "/bots/dev/skills", None).await;
    let first = list[0]["id"].as_str().unwrap().to_string();
    let (s, _) = req(
        &h,
        "PATCH",
        &format!("/bots/dev/skills/{first}"),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(s, 200);
    let (s, _) = req(&h, "PATCH", &path, Some(json!({"enabled": true}))).await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn a_bot_the_caller_does_not_see_has_no_skills_to_them() {
    let h = setup();
    let (s, _) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "GET",
        "/bots/dev/skills",
        None,
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = req(&h, "GET", "/bots/nobody/skills", None).await;
    assert_eq!(s, 404);
}

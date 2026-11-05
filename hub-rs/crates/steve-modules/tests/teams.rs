//! Slice P2 acceptance: virtual teams (fewer than two members → 400), member changes
//! write revisions, org-chart teams refuse a new name with 400, `headcount` includes sub-teams, and
//! `team:<id>` visibility reaches members — plus the other branches of the `/teams` routes.

use std::sync::{Arc, Mutex};

use axum::Router;
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_modules::contacts::{self, Ports, SpacePort, TeamSync};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_modules::{ApiResult, Caller};
use steve_store::ChangeEvent;
use steve_store::base::Viewer;

const READONLY: &str = "组织架构团队跟随同步，不能改这一项";

fn env() -> TestEnv {
    TestEnv::new(|_| {})
}

fn app(t: &TestEnv, ports: Ports) -> Router {
    contacts::routes(ports).with_state(t.modules.clone())
}

fn me() -> Caller {
    caller(WS, ME)
}

fn other() -> Caller {
    caller(WS, OTHER)
}

async fn req(
    r: &Router,
    who: &Caller,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call(r, Some(who), method, path, body.as_ref()).await
}

fn err(msg: &str) -> Value {
    json!({ "error": msg })
}

fn count(t: &TestEnv, sql: &str) -> i64 {
    t.store()
        .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .expect("count")
}

fn text(t: &TestEnv, sql: &str) -> String {
    t.store()
        .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .expect("text")
}

fn exec(t: &TestEnv, sql: &str) {
    t.store()
        .with_conn(|c| Ok(c.execute_batch(sql)?))
        .expect("exec");
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_string()
}

async fn contact(r: &Router, who: &Caller, name: &str) -> String {
    let (s, b) = req(r, who, "POST", "/contacts", Some(json!({ "name": name }))).await;
    assert_eq!(s, 200, "{b}");
    id(&b)
}

async fn me_id(r: &Router, who: &Caller) -> String {
    let (_, b) = req(r, who, "GET", "/me/contact", None).await;
    id(&b)
}

async fn new_team(r: &Router, body: Value) -> Value {
    let (s, b) = req(r, &me(), "POST", "/teams", Some(body)).await;
    assert_eq!(s, 200, "{b}");
    b
}

fn party_events(t: &TestEnv) -> Vec<String> {
    t.take_events()
        .into_iter()
        .filter_map(|e| match e {
            ChangeEvent::Party { id, .. } => Some(id),
            _ => None,
        })
        .collect()
}

/// An org-chart team as the directory sync (P6) would write it.
fn chart_team(t: &TestEnv, id: &str, name: &str, parent: Option<&str>, members: &[&str]) {
    let parent = parent.map_or("NULL".to_string(), |p| format!("'{p}'"));
    let mut sql = format!(
        "INSERT INTO parties (id, workspace_id, type, name, source, created_by, created, updated) VALUES ('{id}', 'default', 'team', '{name}', 'sync', 'usr_dev', 1, 1);
         INSERT INTO team_details (party_id, workspace_id, kind, parent_id, sync_connection, external_id, synced) VALUES ('{id}', 'default', 'chart', {parent}, 'con_fs', 'dep_{id}', 1700000000000);"
    );
    for m in members {
        sql.push_str(&format!(
            "INSERT INTO team_members (workspace_id, team_id, contact_id, role, source, joined) VALUES ('default', '{id}', '{m}', 'member', 'sync', 1);"
        ));
    }
    exec(t, &sql);
}

// ---------------------------------------------------------------- virtual teams

#[tokio::test]
async fn create_virtual_team_and_validation() {
    let t = env();
    let r = app(&t, Ports::default());
    let mine = me_id(&r, &me()).await;
    let lilei = contact(&r, &me(), "李雷").await;
    let wang = contact(&r, &me(), "王芳").await;
    t.take_events();

    let tm = new_team(
        &r,
        json!({"name": " Acme 交付小组 ", "purpose": "交付", "members": [
            {"contact_id": lilei}, {"contact_id": mine}, {"contact_id": wang, "role": "lead"}]}),
    )
    .await;
    assert!(id(&tm).starts_with("tm_"));
    assert_eq!(tm["name"], "Acme 交付小组");
    assert_eq!(tm["kind"], "virtual");
    assert_eq!(tm["status"], "active");
    assert_eq!(tm["purpose"], "交付");
    assert_eq!(tm["visibility"], "workspace");
    assert_eq!(tm["parent_id"], Value::Null);
    assert_eq!(tm["space_id"], Value::Null);
    assert_eq!(tm["headcount"], 3);
    assert_eq!(tm["children"], json!([]));
    assert!(
        tm["source_label"]
            .as_str()
            .unwrap()
            .starts_with("你建的 · "),
        "{tm}"
    );
    let roles: Vec<(String, String)> = tm["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().into(),
                m["role"].as_str().unwrap().into(),
            )
        })
        .collect();
    // leads first; “我” became lead without asking, an explicit role is kept
    assert_eq!(
        roles,
        vec![
            ("Dev".into(), "lead".into()),
            ("王芳".into(), "lead".into()),
            ("李雷".into(), "member".into())
        ]
    );
    assert_eq!(tm["members"][0]["source"], "manual");
    assert_eq!(tm["members"][0]["external"], false);
    assert_eq!(party_events(&t), vec![id(&tm)], "one event");
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM audit WHERE action = 'team.create'"
        ),
        1
    );
    // the contact page lists the team
    let (_, c) = req(&r, &me(), "GET", &format!("/contacts/{lilei}"), None).await;
    assert_eq!(
        c["teams"],
        json!([{"id": id(&tm), "name": "Acme 交付小组", "kind": "virtual", "role": "member"}])
    );

    // fewer than two members
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "一个人", "members": [{"contact_id": lilei}]})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：members")));
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "没人", "members": []})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：members")));
    let (s, b) = req(&r, &me(), "POST", "/teams", Some(json!({"name": "x"}))).await;
    assert_eq!((s, b), (400, err("参数不对：members")));
    // name 1..40, purpose ≤400
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "  ", "purpose": "长".repeat(401),
            "members": [{"contact_id": lilei}, {"contact_id": wang}]})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：name、purpose")));
    let (s, _) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "名".repeat(41), "members": [{"contact_id": lilei}, {"contact_id": wang}]})),
    )
    .await;
    assert_eq!(s, 400);
    // duplicate members, bad roles
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "x", "members": [{"contact_id": lilei}, {"contact_id": lilei, "role": "boss"}]})),
    )
    .await;
    assert_eq!(
        (s, b),
        (400, err("参数不对：members.1.role、members.1.contact_id"))
    );
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "x", "members": [{"contact_id": lilei}, {"role": "lead"}]})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：members.1.contact_id")));
    // unknown, foreign-workspace and invisible contacts
    let w2 = contact(&r, &caller(WS2, ME), "别处的人").await;
    let hidden = {
        let (_, b) = req(
            &r,
            &other(),
            "POST",
            "/contacts",
            Some(json!({"name": "别人的私人联系人", "visibility": "private"})),
        )
        .await;
        id(&b)
    };
    for bad in ["ct_nope", w2.as_str(), hidden.as_str(), id(&tm).as_str()] {
        let (s, b) = req(
            &r,
            &me(),
            "POST",
            "/teams",
            Some(json!({"name": "x", "members": [{"contact_id": lilei}, {"contact_id": bad}]})),
        )
        .await;
        assert_eq!((s, b), (404, err("没有这个联系人")), "{bad}");
    }
    // visibility must name a team of the workspace
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "x", "visibility": "team:tm_nope",
            "members": [{"contact_id": lilei}, {"contact_id": wang}]})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个团队")));
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "x", "visibility": "everyone",
            "members": [{"contact_id": lilei}, {"contact_id": wang}]})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：visibility")));
    // nothing was written by the refused requests
    assert_eq!(
        count(&t, "SELECT COUNT(*) FROM parties WHERE type = 'team'"),
        1
    );
    assert_eq!(count(&t, "SELECT COUNT(*) FROM team_members"), 3);
}

#[tokio::test]
async fn members_external_follows_the_primary_job() {
    let t = env();
    let r = app(&t, Ports::default());
    let (_, orgs) = req(&r, &me(), "GET", "/orgs", None).await;
    let own = id(&orgs[0]);
    let (_, acme) = req(&r, &me(), "POST", "/orgs", Some(json!({"name": "Acme"}))).await;
    let (_, sarah) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "Sarah", "org_id": id(&acme)})),
    )
    .await;
    let (_, lilei) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "李雷", "org_id": own})),
    )
    .await;
    let solo = contact(&r, &me(), "没有组织").await;
    let tm = new_team(
        &r,
        json!({"name": "交付", "members": [{"contact_id": "me"}, {"contact_id": id(&sarah)},
            {"contact_id": id(&lilei)}, {"contact_id": solo}]}),
    )
    .await;
    let ext: Vec<(String, bool)> = tm["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().into(),
                m["external"].as_bool().unwrap(),
            )
        })
        .collect();
    assert!(ext.contains(&("Sarah".into(), true)), "{ext:?}");
    assert!(ext.contains(&("李雷".into(), false)));
    assert!(ext.contains(&("没有组织".into(), false)));
    assert!(ext.contains(&("Dev".into(), false)));
    assert_eq!(tm["members"][0]["name"], "Dev", "me resolved and lead");
}

#[tokio::test]
async fn replacing_members_writes_a_revision_and_one_event() {
    let t = env();
    let r = app(&t, Ports::default());
    let mine = me_id(&r, &me()).await;
    let a = contact(&r, &me(), "甲").await;
    let b = contact(&r, &me(), "乙").await;
    let c = contact(&r, &me(), "丙").await;
    let tm = new_team(
        &r,
        json!({"name": "小组", "members": [{"contact_id": mine}, {"contact_id": a}, {"contact_id": b}]}),
    )
    .await;
    let tid = id(&tm);
    assert_eq!(tm["revision"], 1);
    t.take_events();

    // remove 乙, promote 甲, add 丙
    let (s, got) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([{"contact_id": mine, "role": "lead"}, {"contact_id": a, "role": "lead"}, {"contact_id": c, "role": "member"}])),
    )
    .await;
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["revision"], 2);
    assert_eq!(got["headcount"], 3);
    let names: Vec<&str> = got["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Dev", "甲", "丙"]);
    assert_eq!(
        party_events(&t),
        vec![tid.clone()],
        "one event for the whole list"
    );
    let patch: Value = serde_json::from_str(&text(
        &t,
        &format!("SELECT patch FROM revisions WHERE object_id = '{tid}' ORDER BY id DESC LIMIT 1"),
    ))
    .unwrap();
    assert_eq!(
        patch,
        json!({
            "before": {"members": [{"contact_id": a, "role": "member"}, {"contact_id": b, "role": "member"}]},
            "after": {"members": [{"contact_id": a, "role": "lead"}, {"contact_id": c, "role": "member"}]}
        })
    );
    assert_eq!(
        count(
            &t,
            &format!(
                "SELECT version FROM revisions WHERE object_id = '{tid}' ORDER BY id DESC LIMIT 1"
            )
        ),
        2
    );
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM audit WHERE action = 'team.members'"
        ),
        1
    );
    // the same list again changes nothing
    let (s, same) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([{"contact_id": mine, "role": "lead"}, {"contact_id": a, "role": "lead"}, {"contact_id": c}])),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(same["revision"], 2);
    assert!(party_events(&t).is_empty());
    // validation and ownership
    let (s, e) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([{"contact_id": a, "role": "chief"}])),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：members.0.role")));
    let (s, e) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!({"contact_id": a})),
    )
    .await;
    assert_eq!(s, 400, "{e}");
    let many: Vec<Value> = (0..501)
        .map(|i| json!({"contact_id": format!("ct_{i}")}))
        .collect();
    let (s, e) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(Value::Array(many)),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：members")));
    let (s, e) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([{"contact_id": "ct_nope"}])),
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个联系人")));
    let (s, e) = req(
        &r,
        &caller(WS2, ME),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([])),
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个团队")));
    // emptying a team is allowed
    let (s, empty) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([])),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(empty["headcount"], 0);
    assert_eq!(empty["members"], json!([]));
}

#[tokio::test]
async fn patch_virtual_team() {
    let t = env();
    let r = app(&t, Ports::default());
    exec(
        &t,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1), ('tag_w2', 'w2', 'B', '客户', '', 'usr_dev', 1);",
    );
    let a = contact(&r, &me(), "甲").await;
    let tm = new_team(
        &r,
        json!({"name": "小组", "members": [{"contact_id": "me"}, {"contact_id": a}]}),
    )
    .await;
    let tid = id(&tm);
    t.take_events();
    let (s, got) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"name": "新名字", "purpose": "做事", "tags": ["tag_a", "tag_w2"], "visibility": "private", "space_id": null})),
    )
    .await;
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["name"], "新名字");
    assert_eq!(got["purpose"], "做事");
    assert_eq!(got["tags"], json!(["tag_a"]));
    assert_eq!(got["visibility"], "private");
    assert_eq!(got["revision"], 2);
    assert_eq!(party_events(&t), vec![tid.clone()]);
    let patch: Value = serde_json::from_str(&text(
        &t,
        &format!("SELECT patch FROM revisions WHERE object_id = '{tid}' ORDER BY id DESC LIMIT 1"),
    ))
    .unwrap();
    assert_eq!(
        patch,
        json!({
            "before": {"name": "小组", "visibility": "workspace", "purpose": "", "tags": []},
            "after": {"name": "新名字", "visibility": "private", "purpose": "做事", "tags": ["tag_a"]}
        })
    );
    // nothing changed → no revision, no event
    let (_, same) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"name": "新名字"})),
    )
    .await;
    assert_eq!(same["revision"], 2);
    assert!(party_events(&t).is_empty());
    // private now: the other user cannot see or change it
    let (s, e) = req(&r, &other(), "GET", &format!("/teams/{tid}"), None).await;
    assert_eq!((s, e), (404, err("没有这个团队")));
    let (s, _) = req(
        &r,
        &other(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"purpose": "x"})),
    )
    .await;
    assert_eq!(s, 404);
    let (_, list) = req(&r, &other(), "GET", "/teams", None).await;
    assert_eq!(list["virtual"], json!([]));
    // validation
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"name": "", "purpose": "长".repeat(401)})),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：name、purpose")));
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"visibility": "team:tm_nope"})),
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个团队")));
    // a team can be visible to its own members
    let (s, own) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"visibility": format!("team:{tid}")})),
    )
    .await;
    assert_eq!(s, 200, "{own}");
    // ids of other kinds or workspaces
    let (s, e) = req(&r, &me(), "PATCH", &format!("/teams/{a}"), Some(json!({}))).await;
    assert_eq!((s, e), (404, err("没有这个团队")));
    let (s, e) = req(&r, &caller(WS2, ME), "GET", &format!("/teams/{tid}"), None).await;
    assert_eq!((s, e), (404, err("没有这个团队")));
    let (s, e) = req(&r, &me(), "GET", "/teams/tm_nope", None).await;
    assert_eq!((s, e), (404, err("没有这个团队")));
}

#[tokio::test]
async fn space_binding_goes_through_the_space_port() {
    let t = env();
    let a = {
        let r = app(&t, Ports::default());
        contact(&r, &me(), "甲").await
    };
    // without the channels module only null
    let r = app(&t, Ports::default());
    let tm = new_team(
        &r,
        json!({"name": "小组", "members": [{"contact_id": "me"}, {"contact_id": a}]}),
    )
    .await;
    let tid = id(&tm);
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"space_id": "spc_1"})),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：space_id")));
    let (s, _) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"space_id": null})),
    )
    .await;
    assert_eq!(s, 200);
    // with it
    let r = app(
        &t,
        Ports {
            spaces: Some(Arc::new(FakeSpaces)),
            ..Default::default()
        },
    );
    let (s, got) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"space_id": "spc_1"})),
    )
    .await;
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["space_id"], "spc_1");
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"space_id": "spc_other"})),
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个群")));
    let (_, got) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"space_id": null})),
    )
    .await;
    assert_eq!(got["space_id"], Value::Null);
    // absent leaves it alone
    req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"space_id": "spc_1"})),
    )
    .await;
    let (_, got) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"purpose": "p"})),
    )
    .await;
    assert_eq!(got["space_id"], "spc_1");
}

#[tokio::test]
async fn dissolving_a_virtual_team() {
    let t = env();
    let r = app(&t, Ports::default());
    let a = contact(&r, &me(), "甲").await;
    let tm = new_team(
        &r,
        json!({"name": "小组", "members": [{"contact_id": "me"}, {"contact_id": a}]}),
    )
    .await;
    let tid = id(&tm);
    let (s, e) = req(
        &r,
        &caller(WS2, ME),
        "DELETE",
        &format!("/teams/{tid}"),
        None,
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个团队")));
    t.take_events();
    let (s, ok) = req(&r, &me(), "DELETE", &format!("/teams/{tid}"), None).await;
    assert_eq!((s, ok), (200, json!({"ok": true})));
    assert_eq!(party_events(&t), vec![tid.clone()]);
    let (_, list) = req(&r, &me(), "GET", "/teams", None).await;
    assert_eq!(list["virtual"], json!([]));
    // still readable for references
    let (s, got) = req(&r, &me(), "GET", &format!("/teams/{tid}"), None).await;
    assert_eq!(s, 200);
    assert_eq!(got["status"], "dissolved");
    let (_, c) = req(&r, &me(), "GET", &format!("/contacts/{a}"), None).await;
    assert_eq!(c["teams"], json!([]));
    // but not writable
    let (s, _) = req(&r, &me(), "DELETE", &format!("/teams/{tid}"), None).await;
    assert_eq!(s, 404);
    let (s, _) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/teams/{tid}"),
        Some(json!({"purpose": "x"})),
    )
    .await;
    assert_eq!(s, 404);
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM audit WHERE action = 'team.dissolve'"
        ),
        1
    );
}

// ---------------------------------------------------------------- org-chart teams

#[tokio::test]
async fn org_chart_teams_follow_the_sync() {
    let t = env();
    let r = app(&t, Ports::default());
    let mine = me_id(&r, &me()).await;
    let a = contact(&r, &me(), "甲").await;
    let b = contact(&r, &me(), "乙").await;
    chart_team(&t, "tm_root", "易人实验室", None, &[&mine]);
    chart_team(&t, "tm_rd", "研发", Some("tm_root"), &[&mine, &a]);
    chart_team(&t, "tm_steve", "Steve 组", Some("tm_rd"), &[&a, &b]);
    chart_team(&t, "tm_infra", "基础设施组", Some("tm_rd"), &[]);

    for (field, value) in [("name", json!("新名字")), ("visibility", json!("private"))] {
        let (s, e) = req(
            &r,
            &me(),
            "PATCH",
            "/teams/tm_rd",
            Some(json!({ field: value })),
        )
        .await;
        assert_eq!((s, e), (400, err(READONLY)), "{field}");
    }
    let (s, e) = req(
        &r,
        &me(),
        "PUT",
        "/teams/tm_rd/members",
        Some(json!([{"contact_id": a}])),
    )
    .await;
    assert_eq!((s, e), (400, err(READONLY)));
    let (s, e) = req(&r, &me(), "DELETE", "/teams/tm_rd", None).await;
    assert_eq!((s, e), (400, err(READONLY)));
    // the same name is not a change
    let (s, got) = req(
        &r,
        &me(),
        "PATCH",
        "/teams/tm_rd",
        Some(json!({"name": "研发", "purpose": "做研发"})),
    )
    .await;
    assert_eq!(s, 200, "{got}");
    assert_eq!(got["purpose"], "做研发");
    assert_eq!(got["kind"], "chart");
    assert_eq!(got["parent_id"], "tm_root");
    // siblings by name (byte order: Latin before CJK)
    assert_eq!(got["children"], json!(["tm_steve", "tm_infra"]));
    assert!(
        got["source_label"]
            .as_str()
            .unwrap()
            .starts_with("通讯录 · "),
        "{got}"
    );
    assert!(got["source_label"].as_str().unwrap().ends_with(" 同步"));
    assert!(
        got["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["source"] == "sync")
    );
    // headcounts include everyone below, once
    let (_, list) = req(&r, &me(), "GET", "/teams", None).await;
    let rows: Vec<(String, i64, i64)> = list["chart"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                id(t),
                t["depth"].as_i64().unwrap(),
                t["headcount"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("tm_root".into(), 0, 3),
            ("tm_rd".into(), 1, 3),
            ("tm_steve".into(), 2, 2),
            ("tm_infra".into(), 2, 0),
        ]
    );
    assert!(
        list["chart"][0].get("members").is_none(),
        "list without members"
    );
    assert_eq!(list["virtual"], json!([]));
    assert_eq!(
        list["sync"],
        json!({"connection_id": "con_fs", "label": "通讯录", "at": 1700000000000i64})
    );
    // a deleted contact or dissolved sub-team no longer counts
    exec(
        &t,
        "UPDATE parties SET status = 'dissolved' WHERE id = 'tm_steve'",
    );
    let (_, rd) = req(&r, &me(), "GET", "/teams/tm_rd", None).await;
    assert_eq!(rd["headcount"], 2);
    assert_eq!(rd["children"], json!(["tm_infra"]));
    // a virtual team counts only direct members
    let tm = new_team(
        &r,
        json!({"name": "虚拟", "members": [{"contact_id": a}, {"contact_id": b}]}),
    )
    .await;
    assert_eq!(tm["headcount"], 2);
    // a sub-team whose parent is gone starts its own tree
    exec(
        &t,
        "UPDATE parties SET status = 'active' WHERE id = 'tm_steve'; UPDATE parties SET status = 'dissolved' WHERE id = 'tm_rd';",
    );
    let (_, list) = req(&r, &me(), "GET", "/teams", None).await;
    let rows: Vec<(String, i64)> = list["chart"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (id(t), t["depth"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("tm_steve".into(), 0),
            ("tm_infra".into(), 0),
            ("tm_root".into(), 0),
        ]
    );
}

#[tokio::test]
async fn sync_label_and_scheduling_go_through_the_sync_port() {
    let t = env();
    let r = app(&t, Ports::default());
    let (s, e) = req(&r, &me(), "POST", "/teams/sync", None).await;
    assert_eq!((s, e), (400, err("还没有接入能读通讯录的账号")));
    let (_, list) = req(&r, &me(), "GET", "/teams", None).await;
    assert_eq!(list["sync"], Value::Null);

    let sync = Arc::new(FakeSync::default());
    let r = app(
        &t,
        Ports {
            sync: Some(sync.clone()),
            ..Default::default()
        },
    );
    let (s, e) = req(&r, &me(), "POST", "/teams/sync", Some(json!({}))).await;
    assert_eq!((s, e), (400, err("还没有接入能读通讯录的账号")));
    *sync.connected.lock().unwrap() = true;
    let (s, b) = req(&r, &me(), "POST", "/teams/sync", None).await;
    assert_eq!((s, b), (202, json!({"job_id": "job_1"})));
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/teams/sync",
        Some(json!({"connection_id": "con_fs"})),
    )
    .await;
    assert_eq!((s, b), (202, json!({"job_id": "job_2"})));
    assert_eq!(
        *sync.calls.lock().unwrap(),
        vec![
            (WS.to_string(), ME.to_string(), None),
            (WS.to_string(), ME.to_string(), None),
            (WS.to_string(), ME.to_string(), Some("con_fs".to_string())),
        ]
    );
    let (s, e) = req(
        &r,
        &me(),
        "POST",
        "/teams/sync",
        Some(json!({"connection_id": 3})),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：connection_id")));

    chart_team(&t, "tm_root", "易人实验室", None, &[]);
    let (_, list) = req(&r, &me(), "GET", "/teams", None).await;
    assert_eq!(list["sync"]["label"], "飞书通讯录");
    assert!(
        list["chart"][0]["source_label"]
            .as_str()
            .unwrap()
            .starts_with("飞书通讯录 · "),
        "{list}"
    );
}

// ---------------------------------------------------------------- team:<id> visibility

/// Can the other user see contact `sid`, in its detail and in the list alike?
async fn sees(r: &Router, sid: &str) -> bool {
    let (s, _) = req(r, &other(), "GET", &format!("/contacts/{sid}"), None).await;
    let (_, list) = req(r, &other(), "GET", "/contacts", None).await;
    let listed = list.as_array().unwrap().iter().any(|c| c["id"] == sid);
    assert_eq!(s == 200, listed, "detail and list agree");
    listed
}

#[tokio::test]
async fn team_visibility_reaches_members_and_parent_teams() {
    let t = env();
    let r = app(&t, Ports::default());
    let other_me = me_id(&r, &other()).await;
    let a = contact(&r, &me(), "甲").await;
    let tm = new_team(
        &r,
        json!({"name": "小组", "members": [{"contact_id": "me"}, {"contact_id": a}]}),
    )
    .await;
    let tid = id(&tm);
    let (_, secret) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "只对小组可见", "visibility": format!("team:{tid}")})),
    )
    .await;
    let sid = id(&secret);
    assert!(!sees(&r, &sid).await, "not a member yet");
    // adding the other user's “我” to the team opens it
    let (s, _) = req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([{"contact_id": "me"}, {"contact_id": a}, {"contact_id": other_me}])),
    )
    .await;
    assert_eq!(s, 200);
    assert!(sees(&r, &sid).await, "a member");
    let (_, list) = req(&r, &other(), "GET", "/teams", None).await;
    assert_eq!(list["virtual"][0]["id"], tid.as_str());
    req(
        &r,
        &me(),
        "PUT",
        &format!("/teams/{tid}/members"),
        Some(json!([{"contact_id": "me"}, {"contact_id": a}])),
    )
    .await;
    assert!(!sees(&r, &sid).await, "removed again");
    // the creator always sees it
    let (s, _) = req(&r, &me(), "GET", &format!("/contacts/{sid}"), None).await;
    assert_eq!(s, 200);

    // a member of a sub-team counts as a member of the parent
    chart_team(&t, "tm_root", "公司", None, &[]);
    chart_team(&t, "tm_sub", "研发", Some("tm_root"), &[&other_me]);
    let (_, rooted) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "全公司可见", "visibility": "team:tm_root"})),
    )
    .await;
    let (s, _) = req(
        &r,
        &other(),
        "GET",
        &format!("/contacts/{}", id(&rooted)),
        None,
    )
    .await;
    assert_eq!(s, 200, "member of a sub-team");
    let viewer = t
        .store()
        .reader(|c| Viewer::load(c, WS, OTHER))
        .expect("viewer");
    assert_eq!(
        viewer.scopes,
        vec!["team:tm_root".to_string(), "team:tm_sub".to_string()]
    );
}

// ---------------------------------------------------------------- fakes

/// The channels module with one space, `spc_1`.
struct FakeSpaces;

impl SpacePort for FakeSpaces {
    fn visible(&self, _: &Connection, viewer: &Viewer, space_id: &str) -> ApiResult<bool> {
        Ok(viewer.ws == WS && space_id == "spc_1")
    }
}

/// The directory sync of P6: one Feishu connection once `connected`.
#[derive(Default)]
struct FakeSync {
    connected: Mutex<bool>,
    calls: Mutex<Vec<(String, String, Option<String>)>>,
}

impl TeamSync for FakeSync {
    fn label(&self, _: &Connection, _: &str, connection_id: &str) -> ApiResult<Option<String>> {
        Ok((connection_id == "con_fs").then(|| "飞书通讯录".to_string()))
    }

    fn schedule(
        &self,
        _: &Connection,
        ws: &str,
        actor: &str,
        connection_id: Option<&str>,
    ) -> ApiResult<Option<String>> {
        let mut calls = self.calls.lock().unwrap();
        calls.push((ws.into(), actor.into(), connection_id.map(String::from)));
        if !*self.connected.lock().unwrap() {
            return Ok(None);
        }
        Ok(Some(format!("job_{}", calls.len() - 1)))
    }
}

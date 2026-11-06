//! Slice P1 acceptance: contacts, organizations, attributes and jobs — every success
//! and 4xx branch, “我” created once, foreign-workspace ids 404, delete keeps links but drops attributes
//! and memories, domain conflicts 409 — plus ownership and visibility.

use std::sync::{Arc, Mutex};

use axum::Router;
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_modules::contacts::{self, HandleSearch, PartyHooks, Ports};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, call_raw, caller};
use steve_modules::{ApiResult, Caller};
use steve_store::ChangeEvent;
use steve_store::Tx;
use steve_store::base::mems::{self, NewMem};
use steve_store::base::{ObjRef, Visibility};

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

fn exec(t: &TestEnv, sql: &str) {
    t.store()
        .with_conn(|c| Ok(c.execute_batch(sql)?))
        .expect("exec");
}

async fn new_contact(r: &Router, who: &Caller, body: Value) -> Value {
    let (s, b) = req(r, who, "POST", "/contacts", Some(body)).await;
    assert_eq!(s, 200, "{b}");
    b
}

async fn new_org(r: &Router, who: &Caller, body: Value) -> Value {
    let (s, b) = req(r, who, "POST", "/orgs", Some(body)).await;
    assert_eq!(s, 200, "{b}");
    b
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_string()
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array().expect("array").iter().map(id).collect()
}

// ---------------------------------------------------------------- “我”

#[tokio::test]
async fn me_is_created_once_per_user_and_workspace() {
    let t = env();
    let r = app(&t, Ports::default());
    let (s, a) = req(&r, &me(), "GET", "/me/contact", None).await;
    assert_eq!(s, 200, "{a}");
    assert_eq!(a["is_me"], true);
    assert_eq!(a["name"], "Dev");
    assert_eq!(a["rels"], json!(["me"]));
    assert_eq!(a["kind"], "real");
    assert_eq!(a["status"], "active");
    let (_, b) = req(&r, &me(), "GET", "/contacts/me", None).await;
    assert_eq!(a["id"], b["id"]);
    let (_, c) = req(&r, &me(), "GET", &format!("/contacts/{}", id(&a)), None).await;
    assert_eq!(c["id"], a["id"]);

    // concurrent first calls of another user still make one row
    let mut hs = Vec::new();
    for _ in 0..8 {
        let r = r.clone();
        hs.push(tokio::spawn(async move {
            req(&r, &other(), "GET", "/contacts", None).await.0
        }));
    }
    for h in hs {
        assert_eq!(h.await.unwrap(), 200);
    }
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM parties WHERE user_id = 'usr_dev' AND workspace_id = 'default'"
        ),
        1
    );
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM parties WHERE user_id = 'usr_other'"
        ),
        1
    );
    // one own organization per workspace, named after it
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM parties WHERE workspace_id = 'default' AND org_kind = 'self'"
        ),
        1
    );
    let (_, orgs) = req(&r, &me(), "GET", "/orgs", None).await;
    assert_eq!(orgs[0]["kind"], "self");

    // another workspace has its own “我”
    let (_, w2) = req(&r, &caller(WS2, ME), "GET", "/me/contact", None).await;
    assert_ne!(w2["id"], a["id"]);
    // “我” of the first workspace is not reachable from the second
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "GET",
        &format!("/contacts/{}", id(&a)),
        None,
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));
    // and the list contains “我”
    let (_, list) = req(&r, &me(), "GET", "/contacts", None).await;
    assert!(ids(&list).contains(&id(&a)));
}

#[tokio::test]
async fn me_cannot_be_deleted_or_lose_its_relation() {
    let t = env();
    let r = app(&t, Ports::default());
    let (s, b) = req(&r, &me(), "DELETE", "/contacts/me", None).await;
    assert_eq!((s, b), (400, err("不能删除你自己")));
    let (_, mine) = req(&r, &me(), "GET", "/me/contact", None).await;
    let (s, b) = req(
        &r,
        &me(),
        "DELETE",
        &format!("/contacts/{}", id(&mine)),
        None,
    )
    .await;
    assert_eq!((s, b), (400, err("不能删除你自己")));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        "/contacts/me",
        Some(json!({"rels": ["friend"]})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：rels")));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        "/contacts/me",
        Some(json!({"kind": "virtual"})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：kind")));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        "/contacts/me",
        Some(json!({"rels": ["me", "colleague"], "name": "李"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["rels"], json!(["me", "colleague"]));
    assert_eq!(b["name"], "李");
    // a call preference on “我”
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/parties/me/attrs",
        Some(json!({"key": "call", "value": "老李"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["attrs"][0]["level"], "public");
}

// ---------------------------------------------------------------- contacts

#[tokio::test]
async fn create_contact_with_job_and_validation() {
    let t = env();
    let r = app(&t, Ports::default());
    let acme = new_org(&r, &me(), json!({"name": "Acme", "kind": "customer"})).await;
    t.take_events();
    let c = new_contact(
        &r,
        &me(),
        json!({"name": " Sarah Chen ", "aliases": ["Sarah", "Sarah"], "rels": ["customer"], "org_id": id(&acme), "title": "产品经理", "note": "n"}),
    )
    .await;
    assert!(id(&c).starts_with("ct_"));
    assert_eq!(c["name"], "Sarah Chen");
    assert_eq!(c["aliases"], json!(["Sarah"]));
    assert_eq!(c["kind"], "real");
    assert_eq!(c["important"], false);
    assert_eq!(c["visibility"], "workspace");
    assert_eq!(c["is_me"], false);
    assert_eq!(c["revision"], 1);
    assert_eq!(c["source"], "manual");
    assert_eq!(
        c["org"],
        json!({"id": id(&acme), "name": "Acme", "title": "产品经理", "dept": null})
    );
    assert_eq!(
        c["jobs"],
        json!([{"org_id": id(&acme), "org_name": "Acme", "title": "产品经理", "dept": null, "from": null, "to": null, "source": "manual"}])
    );
    for k in ["attrs", "teams", "reports_to", "merges", "tags"] {
        assert_eq!(c[k], json!([]), "{k}");
    }
    assert!(c.get("accounts").is_none(), "counts only on the brief");
    let ev = t.take_events();
    assert_eq!(
        ev,
        vec![ChangeEvent::Party {
            id: id(&c),
            ws: WS.into()
        }]
    );
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM audit WHERE action = 'party.create' AND detail LIKE '%contact%' AND detail NOT LIKE '%me%'"
        ),
        1
    );

    let bad = [
        (json!({"name": ""}), "参数不对：name"),
        (json!({}), "参数不对：name"),
        (json!({"name": "x".repeat(81)}), "参数不对：name"),
        (
            json!({"name": "a", "aliases": (0..11).map(|i| i.to_string()).collect::<Vec<_>>()}),
            "参数不对：aliases",
        ),
        (json!({"name": "a", "aliases": [""]}), "参数不对：aliases.0"),
        (json!({"name": "a", "rels": ["me"]}), "参数不对：rels"),
        (
            json!({"name": "a", "rels": ["a", "b", "c", "d", "e", "f"]}),
            "参数不对：rels",
        ),
        (
            json!({"name": "a", "kind": "unconfirmed"}),
            "参数不对：kind",
        ),
        (
            json!({"name": "a", "title": "t".repeat(81)}),
            "参数不对：title",
        ),
        (
            json!({"name": "a", "note": "n".repeat(2001)}),
            "参数不对：note",
        ),
        (
            json!({"name": "a", "visibility": "everyone"}),
            "参数不对：visibility",
        ),
        (
            json!({"name": "a", "visibility": "space:spc_1"}),
            "参数不对：visibility",
        ),
        (json!({"name": "a", "important": "yes"}), ""),
    ];
    for (body, msg) in bad {
        let (s, b) = req(&r, &me(), "POST", "/contacts", Some(body.clone())).await;
        if msg.is_empty() {
            assert_eq!(s, 200, "unknown fields are ignored: {body}");
        } else {
            assert_eq!((s, b), (400, err(msg)), "{body}");
        }
    }
    let (s, b) = call_raw(&r, &me(), "POST", "/contacts", "{nope").await;
    assert_eq!((s, b), (400, err("参数不对：body")));
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "a", "visibility": "team:tm_nope"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个团队")));
    // an organization of another workspace
    let w2org = new_org(&r, &caller(WS2, ME), json!({"name": "W2"})).await;
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "a", "org_id": id(&w2org)})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
    // a contact id is not an organization
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "a", "org_id": id(&c)})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
    let (s, _) = call(&r, None, "GET", "/contacts", None).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn list_filters_and_order() {
    let t = env();
    let accounts = Arc::new(FakeHandles::default());
    let r = app(
        &t,
        Ports {
            hooks: vec![],
            handles: Some(accounts.clone()),
            ..Default::default()
        },
    );
    let acme = new_org(&r, &me(), json!({"name": "Acme"})).await;
    let globex = new_org(&r, &me(), json!({"name": "Globex"})).await;
    let sarah = new_contact(&r, &me(), json!({"name": "Sarah Chen", "aliases": ["小陈"], "rels": ["customer"], "org_id": id(&acme), "title": "设计负责人"})).await;
    let li = new_contact(
        &r,
        &me(),
        json!({"name": "李雷", "rels": ["colleague"], "org_id": id(&globex)}),
    )
    .await;
    let bot = new_contact(&r, &me(), json!({"name": "dependabot", "kind": "virtual"})).await;
    let zed = new_contact(&r, &me(), json!({"name": "Zed"})).await;
    req(
        &r,
        &me(),
        "PATCH",
        &format!("/contacts/{}", id(&zed)),
        Some(json!({"important": true})),
    )
    .await;
    exec(
        &t,
        "INSERT INTO parties (id, workspace_id, type, name, contact_kind, created_by, created, updated) VALUES ('ct_unc', 'default', 'contact', 'sarah.c.design', 'unconfirmed', 'usr_dev', 1, 1);",
    );
    exec(
        &t,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_vip', 'default', 'VIP', '客户', '', 'usr_dev', 1);",
    );
    req(
        &r,
        &me(),
        "PATCH",
        &format!("/contacts/{}", id(&li)),
        Some(json!({"tags": ["tag_vip"]})),
    )
    .await;
    accounts
        .0
        .lock()
        .unwrap()
        .push(("sarah@acme.com".into(), id(&sarah)));

    let list = |q: &str| {
        let r = r.clone();
        let q = q.to_string();
        async move {
            let (s, b) = req(&r, &me(), "GET", &format!("/contacts{q}"), None).await;
            assert_eq!(s, 200, "{q}: {b}");
            b
        }
    };
    let all = list("").await;
    let all_ids = ids(&all);
    assert_eq!(all_ids[0], id(&zed), "important first");
    assert!(
        !all_ids.contains(&"ct_unc".to_string()),
        "unconfirmed hidden by default"
    );
    assert_eq!(all_ids.len(), 5, "me + 4");
    let s = all
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == sarah["id"])
        .unwrap();
    assert_eq!(s["accounts"], 1);
    assert_eq!(s["items"], 0);
    assert_eq!(s["org"]["name"], "Acme");
    assert!(s.get("jobs").is_none());

    assert_eq!(ids(&list("?q=sarah").await), vec![id(&sarah)]);
    assert_eq!(
        ids(&list("?q=%E5%B0%8F%E9%99%88").await),
        vec![id(&sarah)],
        "alias 小陈"
    );
    assert_eq!(
        ids(&list("?q=globex").await),
        vec![id(&li)],
        "organization name"
    );
    assert_eq!(
        ids(&list("?q=%E8%AE%BE%E8%AE%A1").await),
        vec![id(&sarah)],
        "title 设计"
    );
    assert_eq!(
        ids(&list("?q=acme.com").await),
        vec![id(&sarah)],
        "account handle"
    );
    assert_eq!(
        ids(&list("?q=%25").await),
        Vec::<String>::new(),
        "LIKE wildcards are literal"
    );
    assert_eq!(ids(&list("?rel=colleague").await), vec![id(&li)]);
    assert_eq!(ids(&list("?kind=virtual").await), vec![id(&bot)]);
    assert_eq!(
        ids(&list("?kind=unconfirmed").await),
        vec!["ct_unc".to_string()]
    );
    assert_eq!(
        ids(&list(&format!("?org={}", id(&acme))).await),
        vec![id(&sarah)]
    );
    assert_eq!(ids(&list("?tag=tag_vip").await), vec![id(&li)]);
    assert_eq!(list("?limit=2").await.as_array().unwrap().len(), 2);
    for bad in ["?limit=0", "?limit=501", "?limit=x"] {
        let (s, b) = req(&r, &me(), "GET", &format!("/contacts{bad}"), None).await;
        assert_eq!((s, b), (400, err("参数不对：limit")), "{bad}");
    }
    let (s, b) = req(&r, &me(), "GET", "/contacts?kind=robot", None).await;
    assert_eq!((s, b), (400, err("参数不对：kind")));
    // contacts of another workspace never appear
    new_contact(&r, &caller(WS2, ME), json!({"name": "Sarah W2"})).await;
    assert_eq!(ids(&list("?q=sarah").await), vec![id(&sarah)]);
}

#[tokio::test]
async fn patch_contact_writes_revision_tags_and_one_event() {
    let t = env();
    let r = app(&t, Ports::default());
    let c = new_contact(&r, &me(), json!({"name": "Sarah"})).await;
    exec(
        &t,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1), ('tag_w2', 'w2', 'B', '客户', '', 'usr_dev', 1);",
    );
    t.take_events();
    let path = format!("/contacts/{}", id(&c));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &path,
        Some(json!({"name": "Sarah Chen", "aliases": ["S"], "rels": ["partner"], "important": true, "note": "x", "kind": "virtual", "visibility": "private", "tags": ["tag_a", "tag_w2", "tag_nope"]})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["name"], "Sarah Chen");
    assert_eq!(b["important"], true);
    assert_eq!(b["kind"], "virtual");
    assert_eq!(b["visibility"], "private");
    assert_eq!(b["tags"], json!(["tag_a"]), "only tags of this workspace");
    assert_eq!(b["revision"], 2);
    assert_eq!(t.take_events().len(), 1);
    let patch: String = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT patch FROM revisions WHERE object_type = 'party' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let patch: Value = serde_json::from_str(&patch).unwrap();
    assert_eq!(patch["before"]["name"], "Sarah");
    assert_eq!(patch["after"]["name"], "Sarah Chen");
    assert_eq!(patch["after"]["tags"], json!(["tag_a"]));
    // no change → no revision, no event
    let (_, b) = req(
        &r,
        &me(),
        "PATCH",
        &path,
        Some(json!({"name": "Sarah Chen"})),
    )
    .await;
    assert_eq!(b["revision"], 2);
    assert!(t.take_events().is_empty());

    for (body, msg) in [
        (json!({"name": ""}), "参数不对：name"),
        (json!({"important": "yes"}), "参数不对：important"),
        (json!({"kind": "unconfirmed"}), "参数不对：kind"),
        (json!({"rels": ["me"]}), "参数不对：rels"),
        (json!({"visibility": "nobody"}), "参数不对：visibility"),
    ] {
        let (s, b) = req(&r, &me(), "PATCH", &path, Some(body.clone())).await;
        assert_eq!((s, b), (400, err(msg)), "{body}");
    }
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "PATCH",
        &path,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        "/contacts/ct_nope",
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));
}

#[tokio::test]
async fn confirm_unconfirmed_contact() {
    let t = env();
    let r = app(&t, Ports::default());
    req(&r, &me(), "GET", "/contacts", None).await;
    exec(
        &t,
        "INSERT INTO parties (id, workspace_id, type, name, contact_kind, source, created_by, created, updated) VALUES ('ct_unc', 'default', 'contact', 'sarah.c', 'unconfirmed', 'identify', 'usr_dev', 1, 1);",
    );
    let (s, b) = req(&r, &me(), "POST", "/contacts/ct_unc/confirm", None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["kind"], "real");
    let (s, b) = req(&r, &me(), "POST", "/contacts/ct_unc/confirm", None).await;
    assert_eq!((s, b), (409, err("这个联系人已经确认过了")));
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "POST",
        "/contacts/ct_unc/confirm",
        None,
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));
}

#[derive(Default)]
struct DeleteSpy(Mutex<Vec<String>>);

impl PartyHooks for DeleteSpy {
    fn name(&self) -> &str {
        "spy"
    }
    fn on_delete(&self, tx: &Tx, _ws: &str, party_id: &str) -> ApiResult<()> {
        // runs inside the same transaction: the contact is still active here
        let st: String =
            tx.query_row("SELECT status FROM parties WHERE id = ?", [party_id], |r| {
                r.get(0)
            })?;
        self.0.lock().unwrap().push(format!("{party_id}:{st}"));
        Ok(())
    }
}

#[tokio::test]
async fn delete_contact_drops_attrs_and_memories_keeps_links() {
    let t = env();
    let spy = Arc::new(DeleteSpy::default());
    let r = app(
        &t,
        Ports {
            hooks: vec![spy.clone()],
            handles: None,
            ..Default::default()
        },
    );
    let acme = new_org(&r, &me(), json!({"name": "Acme"})).await;
    let c = new_contact(&r, &me(), json!({"name": "Sarah", "org_id": id(&acme)})).await;
    let cid = id(&c);
    req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{cid}/attrs"),
        Some(json!({"key": "phone", "value": "+1 555"})),
    )
    .await;
    exec(
        &t,
        &format!(
            "INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated) VALUES ('ST-1', 'default', 1, 't', 'open', 'new', 1, 1);
             INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES ('default', 'item', 'ST-1', 'related', 'party', '{cid}', 'manual', 1);
             INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('tm_1', 'default', 'team', 'T', 'usr_dev', 1, 1);
             INSERT INTO team_details (party_id, workspace_id, kind) VALUES ('tm_1', 'default', 'virtual');
             INSERT INTO team_members (workspace_id, team_id, contact_id, joined) VALUES ('default', 'tm_1', '{cid}', 1);"
        ),
    );
    let (only, shared) = t
        .store()
        .transaction(|tx| {
            let a = mems::create(
                tx,
                WS,
                &NewMem {
                    text: "只关于她".into(),
                    created_by: ME.into(),
                    about: vec![ObjRef::new("party", &cid)],
                    ..Default::default()
                },
            )?;
            let b = mems::create(
                tx,
                WS,
                &NewMem {
                    text: "共享".into(),
                    created_by: ME.into(),
                    visibility: Visibility::Workspace,
                    about: vec![ObjRef::new("party", &cid), ObjRef::new("item", "ST-1")],
                    ..Default::default()
                },
            )?;
            Ok::<_, steve_store::StoreError>((a.id, b.id))
        })
        .unwrap();
    let (_, got) = req(&r, &me(), "GET", &format!("/contacts/{cid}"), None).await;
    assert_eq!(
        got["teams"],
        json!([{"id": "tm_1", "name": "T", "kind": "virtual", "role": "member"}])
    );
    let (_, list) = req(&r, &me(), "GET", "/contacts?q=Sarah", None).await;
    assert_eq!(list[0]["items"], 1);

    let (s, b) = req(&r, &me(), "DELETE", &format!("/contacts/{cid}"), None).await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    assert_eq!(*spy.0.lock().unwrap(), vec![format!("{cid}:active")]);
    assert_eq!(
        count(
            &t,
            &format!("SELECT COUNT(*) FROM attr_values WHERE party_id = '{cid}'")
        ),
        0
    );
    assert_eq!(
        count(
            &t,
            &format!("SELECT COUNT(*) FROM mems WHERE id = '{only}'")
        ),
        0
    );
    assert_eq!(
        count(
            &t,
            &format!("SELECT COUNT(*) FROM mems WHERE id = '{shared}'")
        ),
        1,
        "shared memory stays"
    );
    assert_eq!(
        count(
            &t,
            &format!("SELECT COUNT(*) FROM team_members WHERE contact_id = '{cid}'")
        ),
        0
    );
    assert_eq!(
        count(
            &t,
            &format!(
                "SELECT COUNT(*) FROM links WHERE subject_id = '{cid}' OR object_id = '{cid}'"
            )
        ),
        2,
        "job and item links stay"
    );
    assert_eq!(
        count(
            &t,
            &format!("SELECT COUNT(*) FROM parties WHERE id = '{cid}' AND status = 'deleted'")
        ),
        1,
        "row kept"
    );
    let detail: String = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT detail FROM audit WHERE action = 'party.delete'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let detail = serde_json::from_str::<Value>(&detail).unwrap();
    assert_eq!(
        (
            &detail["type"],
            &detail["attrs"],
            &detail["mems"],
            &detail["teams"]
        ),
        (&json!("contact"), &json!(1), &json!(1), &json!(1))
    );
    // the rows deleted for good, as they were: enough to put them back by hand
    let d = &detail["deleted"];
    assert_eq!(d["attr_values"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d["attr_values"][0]["party_id"], cid.as_str());
    assert!(d["attr_values"][0]["value"].is_string(), "{d}");
    assert_eq!(d["mems"].as_array().unwrap().len(), 1, "{d}");
    assert!(d["mems"][0]["text"].is_string(), "{d}");
    assert_eq!(d["mem_about"][0]["mem_id"], d["mems"][0]["id"]);
    assert_eq!(d["team_members"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d["team_members"][0]["contact_id"], cid.as_str());
    let (s, b) = req(&r, &me(), "GET", &format!("/contacts/{cid}"), None).await;
    assert_eq!((s, b), (404, err("没有这个联系人")));
    let (s, _) = req(&r, &me(), "DELETE", &format!("/contacts/{cid}"), None).await;
    assert_eq!(s, 404);
    let (_, list) = req(&r, &me(), "GET", "/contacts?q=Sarah", None).await;
    assert_eq!(list, json!([]));
    let (_, o) = req(&r, &me(), "GET", &format!("/orgs/{}", id(&acme)), None).await;
    assert_eq!(o["people"], json!([]));
}

#[tokio::test]
async fn delete_contact_rolls_back_when_a_hook_fails() {
    struct Fails;
    impl PartyHooks for Fails {
        fn name(&self) -> &str {
            "fails"
        }
        fn on_delete(&self, _: &Tx, _: &str, _: &str) -> ApiResult<()> {
            Err(steve_modules::ApiError::rule("账号模块拒绝"))
        }
    }
    let t = env();
    let r = app(
        &t,
        Ports {
            hooks: vec![Arc::new(Fails)],
            handles: None,
            ..Default::default()
        },
    );
    let c = new_contact(&r, &me(), json!({"name": "Sarah"})).await;
    req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{}/attrs", id(&c)),
        Some(json!({"key": "phone", "value": "1"})),
    )
    .await;
    let (s, b) = req(&r, &me(), "DELETE", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!((s, b), (400, err("账号模块拒绝")));
    let (_, got) = req(&r, &me(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!(got["status"], "active");
    assert_eq!(got["attrs"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------- jobs

#[tokio::test]
async fn jobs_open_close_reopen_and_remove() {
    let t = env();
    let r = app(&t, Ports::default());
    let acme = new_org(&r, &me(), json!({"name": "Acme"})).await;
    let globex = new_org(&r, &me(), json!({"name": "Globex"})).await;
    let c = new_contact(
        &r,
        &me(),
        json!({"name": "Sarah", "org_id": id(&acme), "title": "PM"}),
    )
    .await;
    let jobs = format!("/contacts/{}/jobs", id(&c));

    let (s, b) = req(&r, &me(), "POST", &jobs, Some(json!({"org_id": id(&acme)}))).await;
    assert_eq!((s, b), (409, err("已经在这个组织任职")));
    for (body, msg) in [
        (
            json!({"org_id": id(&globex), "from": "2024-13"}),
            "参数不对：from",
        ),
        (
            json!({"org_id": id(&globex), "from": "2024-1"}),
            "参数不对：from",
        ),
        (
            json!({"org_id": id(&globex), "title": "t".repeat(81)}),
            "参数不对：title",
        ),
        (
            json!({"org_id": id(&globex), "dept": "d".repeat(81)}),
            "参数不对：dept",
        ),
        (json!({}), "参数不对：org_id"),
    ] {
        let (s, b) = req(&r, &me(), "POST", &jobs, Some(body.clone())).await;
        assert_eq!((s, b), (400, err(msg)), "{body}");
    }
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &jobs,
        Some(json!({"org_id": "org_nope"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "POST",
        &jobs,
        Some(json!({"org_id": id(&globex)})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));

    // a second, newer job is the primary one
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &jobs,
        Some(json!({"org_id": id(&globex), "title": "顾问", "dept": "设计", "from": "2026-09"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["jobs"].as_array().unwrap().len(), 2);
    assert_eq!(b["org"]["name"], "Globex");
    assert_eq!(b["org"]["dept"], "设计");

    // leaving Acme
    let acme_job = format!("{jobs}/{}", id(&acme));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &acme_job,
        Some(json!({"to": "2026-10", "from": "2020-01"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let j = b["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["org_id"] == acme["id"])
        .unwrap()
        .clone();
    assert_eq!(
        (j["from"].as_str(), j["to"].as_str(), j["title"].as_str()),
        (Some("2020-01"), Some("2026-10"), Some("PM"))
    );
    assert_eq!(
        b["jobs"][1]["org_id"], acme["id"],
        "closed jobs after current ones"
    );
    let (_, list) = req(
        &r,
        &me(),
        "GET",
        &format!("/contacts?org={}", id(&acme)),
        None,
    )
    .await;
    assert_eq!(list, json!([]), "no longer a current job");
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &acme_job,
        Some(json!({"to": "10/2026"})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：to")));

    // back at Acme: same link, old interval in history
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &jobs,
        Some(json!({"org_id": id(&acme), "title": "总监", "from": "2027-01"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let props: String = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                &format!(
                    "SELECT props FROM links WHERE predicate = 'works_at' AND object_id = '{}'",
                    id(&acme)
                ),
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let props: Value = serde_json::from_str(&props).unwrap();
    assert_eq!(props["to"], Value::Null);
    assert_eq!(props["title"], "总监");
    assert_eq!(
        props["history"],
        json!([{"title": "PM", "dept": null, "from": "2020-01", "to": "2026-10"}])
    );
    assert_eq!(b["org"]["name"], "Acme", "most recent from");

    // clearing a field with null, reopening with to: null
    let (_, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("{jobs}/{}", id(&globex)),
        Some(json!({"dept": null, "to": "2027-02"})),
    )
    .await;
    let g = b["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["org_id"] == globex["id"])
        .unwrap()
        .clone();
    assert_eq!(
        (g["dept"].clone(), g["to"].clone()),
        (Value::Null, json!("2027-02"))
    );
    let (_, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("{jobs}/{}", id(&globex)),
        Some(json!({"to": null})),
    )
    .await;
    let g = b["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["org_id"] == globex["id"])
        .unwrap()
        .clone();
    assert_eq!(g["to"], Value::Null);

    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("{jobs}/org_nope"),
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个任职")));
    let (s, b) = req(
        &r,
        &me(),
        "DELETE",
        &format!("{jobs}/{}", id(&globex)),
        None,
    )
    .await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    let (s, b) = req(
        &r,
        &me(),
        "DELETE",
        &format!("{jobs}/{}", id(&globex)),
        None,
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个任职")));
    let (_, b) = req(&r, &me(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!(b["jobs"].as_array().unwrap().len(), 1);
    assert!(
        b["revision"].as_i64().unwrap() > 1,
        "jobs revise the contact"
    );
}

// ---------------------------------------------------------------- attributes

#[tokio::test]
async fn attributes_levels_stale_and_validation() {
    let t = env();
    let r = app(&t, Ports::default());
    let c = new_contact(&r, &me(), json!({"name": "Sarah"})).await;
    let attrs = format!("/parties/{}/attrs", id(&c));

    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &attrs,
        Some(json!({"key": "phone", "value": " +1 555 "})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let first = b["attrs"][0].clone();
    assert_eq!(first["label"], "电话");
    assert_eq!(first["value"], "+1 555");
    assert_eq!(
        first["level"], "reasoning",
        "default level of the definition"
    );
    assert_eq!(first["status"], "confirmed");
    assert_eq!(first["source_label"], "你填的");
    assert!(id(&first).starts_with("av_"));

    // a newer phone with the same label replaces it; another label does not
    let (_, b) = req(
        &r,
        &me(),
        "POST",
        &attrs,
        Some(json!({"key": "phone", "value": "+1 666", "level": "public"})),
    )
    .await;
    let a = b["attrs"].as_array().unwrap();
    assert_eq!(
        (
            a[0]["status"].as_str(),
            a[1]["status"].as_str(),
            a[1]["level"].as_str()
        ),
        (Some("stale"), Some("confirmed"), Some("public"))
    );
    let (_, b) = req(
        &r,
        &me(),
        "POST",
        &attrs,
        Some(json!({"key": "phone", "label": "工作电话", "value": "+1 777"})),
    )
    .await;
    let a = b["attrs"].as_array().unwrap();
    assert_eq!(a.iter().filter(|x| x["status"] == "confirmed").count(), 2);

    // a new key gets a definition of yours
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &attrs,
        Some(json!({"key": "shoe_size", "label": "鞋码", "value": "42"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let (_, defs) = req(&r, &me(), "GET", "/attr-defs", None).await;
    let d = defs
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"] == "shoe_size")
        .unwrap()
        .clone();
    assert_eq!(
        (
            d["source"].as_str(),
            d["type"].as_str(),
            d["applies"].as_str(),
            d["builtin"].as_bool()
        ),
        (Some("me"), Some("text"), Some("contact"), Some(false))
    );

    for (body, msg) in [
        (json!({"key": "Phone", "value": "1"}), "参数不对：key"),
        (json!({"key": "1phone", "value": "1"}), "参数不对：key"),
        (
            json!({"key": "a".repeat(33), "value": "1"}),
            "参数不对：key",
        ),
        (
            json!({"key": "website", "value": "acme.com"}),
            "参数不对：key",
        ),
        (json!({"key": "phone", "value": ""}), "参数不对：value"),
        (
            json!({"key": "phone", "value": "v".repeat(501)}),
            "参数不对：value",
        ),
        (
            json!({"key": "phone", "label": "", "value": "1"}),
            "参数不对：label",
        ),
        (
            json!({"key": "phone", "label": "l".repeat(21), "value": "1"}),
            "参数不对：label",
        ),
        (
            json!({"key": "phone", "value": "1", "level": "secret"}),
            "参数不对：level",
        ),
        (json!({"value": "1"}), "参数不对：key"),
    ] {
        let (s, b) = req(&r, &me(), "POST", &attrs, Some(body.clone())).await;
        assert_eq!((s, b), (400, err(msg)), "{body}");
    }
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/parties/ct_nope/attrs",
        Some(json!({"key": "phone", "value": "1"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "POST",
        &attrs,
        Some(json!({"key": "phone", "value": "1"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));

    // edits
    let fid = id(&first);
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/attrs/{fid}"),
        Some(json!({"status": "confirmed", "level": "never", "value": "+1 000"})),
    )
    .await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    let (_, b) = req(&r, &me(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    let a = b["attrs"].as_array().unwrap();
    let f = a.iter().find(|x| x["id"] == first["id"]).unwrap();
    assert_eq!(
        (
            f["status"].as_str(),
            f["level"].as_str(),
            f["value"].as_str()
        ),
        (Some("confirmed"), Some("never"), Some("+1 000"))
    );
    assert_eq!(
        a[1]["status"], "stale",
        "re-confirming the old value makes the other one with the same label stale"
    );
    for (body, msg) in [
        (json!({"status": "candidate"}), "参数不对：status"),
        (json!({"value": ""}), "参数不对：value"),
        (json!({"level": "x"}), "参数不对：level"),
    ] {
        let (s, b) = req(
            &r,
            &me(),
            "PATCH",
            &format!("/attrs/{fid}"),
            Some(body.clone()),
        )
        .await;
        assert_eq!((s, b), (400, err(msg)), "{body}");
    }
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "PATCH",
        &format!("/attrs/{fid}"),
        Some(json!({"value": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个属性")));
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "DELETE",
        &format!("/attrs/{fid}"),
        None,
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个属性")));
    let (s, b) = req(&r, &me(), "DELETE", &format!("/attrs/{fid}"), None).await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    let (s, b) = req(&r, &me(), "DELETE", &format!("/attrs/{fid}"), None).await;
    assert_eq!((s, b), (404, err("没有这个属性")));
    let (s, _) = req(
        &r,
        &me(),
        "PATCH",
        "/attrs/av_nope",
        Some(json!({"value": "x"})),
    )
    .await;
    assert_eq!(s, 404);
}

#[tokio::test]
async fn attributes_on_organizations_answer_with_the_org() {
    let t = env();
    let r = app(&t, Ports::default());
    let o = new_org(&r, &me(), json!({"name": "Acme"})).await;
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{}/attrs", id(&o)),
        Some(json!({"key": "website", "value": "https://acme.com"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["id"], o["id"]);
    assert!(b.get("people").is_some(), "Org shape");
    assert_eq!(b["attrs"][0]["level"], "public");
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{}/attrs", id(&o)),
        Some(json!({"key": "birthday", "value": "1990-01-01"})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：key")), "contact-only key");
    // new keys on an organization apply to organizations
    req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{}/attrs", id(&o)),
        Some(json!({"key": "tax_id", "value": "123"})),
    )
    .await;
    let c = new_contact(&r, &me(), json!({"name": "S"})).await;
    let (s, _) = req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{}/attrs", id(&c)),
        Some(json!({"key": "tax_id", "value": "1"})),
    )
    .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn attribute_definitions() {
    let t = env();
    let r = app(&t, Ports::default());
    let (s, defs) = req(&r, &me(), "GET", "/attr-defs", None).await;
    assert_eq!(s, 200);
    let keys: Vec<&str> = defs
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "phone", "address", "birthday", "tz", "lang", "call", "website", "contract"
        ]
    );
    assert_eq!(
        defs[0],
        json!({"id": "ad_phone", "applies": "both", "key": "phone", "label": "电话", "type": "phone", "options": [], "default_level": "reasoning", "source": "builtin", "status": "active", "builtin": true, "created": 0})
    );

    let (s, b) = req(&r, &me(), "POST", "/attr-defs", Some(json!({"applies": "contact", "key": "phone", "label": "手机", "type": "phone", "default_level": "public"}))).await;
    assert_eq!((s, b), (409, err("已有这个属性")));
    let (s, d) = req(&r, &me(), "POST", "/attr-defs", Some(json!({"applies": "contact", "key": "tier", "label": "等级", "type": "enum", "options": ["A", "B"], "default_level": "conclusion"}))).await;
    assert_eq!(s, 200, "{d}");
    assert_eq!(
        (d["source"].as_str(), d["builtin"].as_bool()),
        (Some("me"), Some(false))
    );
    let (s, b) = req(&r, &me(), "POST", "/attr-defs", Some(json!({"applies": "contact", "key": "tier", "label": "x", "type": "text", "default_level": "public"}))).await;
    assert_eq!((s, b), (409, err("已有这个属性")));
    for (body, msg) in [
        (
            json!({"applies": "team", "key": "k", "label": "l", "type": "text", "default_level": "public"}),
            "参数不对：applies",
        ),
        (
            json!({"applies": "org", "key": "K", "label": "l", "type": "text", "default_level": "public"}),
            "参数不对：key",
        ),
        (
            json!({"applies": "org", "key": "k", "label": "", "type": "text", "default_level": "public"}),
            "参数不对：label",
        ),
        (
            json!({"applies": "org", "key": "k", "label": "l", "type": "money", "default_level": "public"}),
            "参数不对：type",
        ),
        (
            json!({"applies": "org", "key": "k", "label": "l", "type": "enum", "default_level": "public"}),
            "参数不对：options",
        ),
        (
            json!({"applies": "org", "key": "k", "label": "l", "type": "text", "options": (0..31).map(|i| i.to_string()).collect::<Vec<_>>(), "default_level": "public"}),
            "参数不对：options",
        ),
        (
            json!({"applies": "org", "key": "k", "label": "l", "type": "text"}),
            "参数不对：default_level",
        ),
    ] {
        let (s, b) = req(&r, &me(), "POST", "/attr-defs", Some(body.clone())).await;
        assert_eq!((s, b), (400, err(msg)), "{body}");
    }

    // enum values must be one of the options
    let c = new_contact(&r, &me(), json!({"name": "S"})).await;
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{}/attrs", id(&c)),
        Some(json!({"key": "tier", "value": "C"})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：value")));
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{}/attrs", id(&c)),
        Some(json!({"key": "tier", "value": "A"})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(b["attrs"][0]["level"], "conclusion");
    let av = id(&b["attrs"][0]);

    let did = id(&d);
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/attr-defs/{did}"),
        Some(json!({"label": "客户等级", "options": ["A", "B", "C"], "default_level": "public"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        (
            b["label"].as_str(),
            b["options"].clone(),
            b["default_level"].as_str()
        ),
        (Some("客户等级"), json!(["A", "B", "C"]), Some("public"))
    );
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/attr-defs/{did}"),
        Some(json!({"options": []})),
    )
    .await;
    assert_eq!((s, b), (400, err("参数不对：options")));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        "/attr-defs/ad_phone",
        Some(json!({"label": "手机"})),
    )
    .await;
    assert_eq!((s, b), (400, err("内置属性不能修改")));
    let (s, b) = req(&r, &me(), "DELETE", "/attr-defs/ad_phone", None).await;
    assert_eq!((s, b), (400, err("内置属性不能修改")));
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "PATCH",
        &format!("/attr-defs/{did}"),
        Some(json!({"label": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个属性")));
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "DELETE",
        &format!("/attr-defs/{did}"),
        None,
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个属性")));

    let (s, b) = req(&r, &me(), "DELETE", &format!("/attr-defs/{did}"), None).await;
    assert_eq!((s, b), (409, err("还有 1 个联系人在用这个属性")));
    req(&r, &me(), "DELETE", &format!("/attrs/{av}"), None).await;
    let (s, b) = req(&r, &me(), "DELETE", &format!("/attr-defs/{did}"), None).await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    let (s, _) = req(&r, &me(), "DELETE", &format!("/attr-defs/{did}"), None).await;
    assert_eq!(s, 404);
    // another workspace sees only the built-ins
    let (_, w2) = req(&r, &caller(WS2, ME), "GET", "/attr-defs", None).await;
    assert_eq!(w2.as_array().unwrap().len(), 8);
}

// ---------------------------------------------------------------- organizations

#[tokio::test]
async fn organizations_domains_and_people() {
    let t = env();
    let r = app(&t, Ports::default());
    let acme = new_org(&r, &me(), json!({"name": "Acme", "kind": "customer", "domains": ["@Acme.COM", "www.acme.io", "acme.com"], "aliases": ["ACME"], "note": "大客户"})).await;
    assert!(id(&acme).starts_with("org_"));
    assert_eq!(acme["domains"], json!(["acme.com", "acme.io"]));
    assert_eq!(acme["kind"], "customer");
    assert_eq!(acme["people"], json!([]));
    assert_eq!(acme["revision"], 1);
    let other_org = new_org(&r, &me(), json!({"name": "Initech"})).await;
    assert_eq!(other_org["kind"], "other", "default kind");

    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/orgs",
        Some(json!({"name": "Fake", "domains": ["sales@acme.com"]})),
    )
    .await;
    assert_eq!((s, b), (409, err("这个域名已经属于「Acme」")));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/orgs/{}", id(&other_org)),
        Some(json!({"domains": ["acme.io"]})),
    )
    .await;
    assert_eq!((s, b), (409, err("这个域名已经属于「Acme」")));
    // the same domain in another workspace is fine
    new_org(
        &r,
        &caller(WS2, ME),
        json!({"name": "Acme 2", "domains": ["acme.com"]}),
    )
    .await;
    for (body, msg) in [
        (json!({"name": ""}), "参数不对：name"),
        (json!({"name": "x", "kind": "self"}), "参数不对：kind"),
        (
            json!({"name": "x", "domains": ["not a domain"]}),
            "参数不对：domains.0",
        ),
        (
            json!({"name": "x", "domains": ["localhost"]}),
            "参数不对：domains.0",
        ),
        (
            json!({"name": "x", "domains": (0..21).map(|i| format!("d{i}.com")).collect::<Vec<_>>()}),
            "参数不对：domains",
        ),
        (
            json!({"name": "x", "note": "n".repeat(2001)}),
            "参数不对：note",
        ),
    ] {
        let (s, b) = req(&r, &me(), "POST", "/orgs", Some(body.clone())).await;
        assert_eq!((s, b), (400, err(msg)), "{body}");
    }

    let sarah = new_contact(
        &r,
        &me(),
        json!({"name": "Sarah", "org_id": id(&acme), "title": "PM"}),
    )
    .await;
    let (_, o) = req(&r, &me(), "GET", &format!("/orgs/{}", id(&acme)), None).await;
    assert_eq!(
        o["people"],
        json!([{"id": id(&sarah), "name": "Sarah", "title": "PM"}])
    );
    let (_, list) = req(&r, &me(), "GET", "/orgs", None).await;
    assert_eq!(list[0]["kind"], "self");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|o| o.get("people").is_none())
    );
    assert_eq!(list.as_array().unwrap().len(), 3);

    exec(
        &t,
        "INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1);",
    );
    t.take_events();
    let (s, b) = req(&r, &me(), "PATCH", &format!("/orgs/{}", id(&acme)), Some(json!({"name": "Acme Inc", "kind": "partner", "domains": ["acme.com"], "tags": ["tag_a"], "note": "改"}))).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        (
            b["name"].as_str(),
            b["kind"].as_str(),
            b["revision"].as_i64()
        ),
        (Some("Acme Inc"), Some("partner"), Some(2))
    );
    assert_eq!(b["domains"], json!(["acme.com"]));
    assert_eq!(b["tags"], json!(["tag_a"]));
    assert_eq!(
        t.take_events(),
        vec![ChangeEvent::Party {
            id: id(&acme),
            ws: WS.into()
        }]
    );
    // acme.io is free again
    let (s, _) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/orgs/{}", id(&other_org)),
        Some(json!({"domains": ["acme.io"]})),
    )
    .await;
    assert_eq!(s, 200);

    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "GET",
        &format!("/orgs/{}", id(&acme)),
        None,
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
    let (s, b) = req(&r, &me(), "GET", &format!("/orgs/{}", id(&sarah)), None).await;
    assert_eq!((s, b), (404, err("没有这个组织")), "a contact id");
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "PATCH",
        &format!("/orgs/{}", id(&acme)),
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "DELETE",
        &format!("/orgs/{}", id(&acme)),
        None,
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
}

#[tokio::test]
async fn own_organization_rules() {
    let t = env();
    let r = app(&t, Ports::default());
    let (_, list) = req(&r, &me(), "GET", "/orgs", None).await;
    let own = list[0].clone();
    assert_eq!(own["kind"], "self");
    let ws_name: String = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT name FROM workspaces WHERE id = 'default'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(own["name"], ws_name);
    let path = format!("/orgs/{}", id(&own));
    let (s, b) = req(&r, &me(), "PATCH", &path, Some(json!({"kind": "customer"}))).await;
    assert_eq!((s, b), (400, err("本公司不能改类型")));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &path,
        Some(json!({"name": "我们公司", "domains": ["us.com"]})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let (s, b) = req(&r, &me(), "DELETE", &path, None).await;
    assert_eq!((s, b), (400, err("本公司不能删除")));
}

#[tokio::test]
async fn delete_org_closes_jobs_and_frees_domains() {
    let t = env();
    let r = app(&t, Ports::default());
    let acme = new_org(
        &r,
        &me(),
        json!({"name": "Acme", "domains": ["acme.com"], "note": "n", "aliases": ["A"]}),
    )
    .await;
    let c = new_contact(
        &r,
        &me(),
        json!({"name": "Sarah", "org_id": id(&acme), "title": "PM"}),
    )
    .await;
    let (s, b) = req(&r, &me(), "DELETE", &format!("/orgs/{}", id(&acme)), None).await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    let (_, got) = req(&r, &me(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!(got["org"], Value::Null);
    assert_eq!(got["jobs"][0]["to"], contacts::service::this_month());
    assert_eq!(
        got["jobs"][0]["org_name"], "Acme",
        "the name stays for display"
    );
    let (s, b) = req(&r, &me(), "GET", &format!("/orgs/{}", id(&acme)), None).await;
    assert_eq!((s, b), (404, err("没有这个组织")));
    let (_, list) = req(&r, &me(), "GET", "/orgs", None).await;
    assert!(!ids(&list).contains(&id(&acme)));
    assert_eq!(
        count(
            &t,
            &format!(
                "SELECT COUNT(*) FROM parties WHERE id = '{}' AND status = 'deleted' AND note = '' AND aliases = '[]'",
                id(&acme)
            )
        ),
        1
    );
    new_org(
        &r,
        &me(),
        json!({"name": "Acme 2", "domains": ["acme.com"]}),
    )
    .await;
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &format!("/contacts/{}/jobs", id(&c)),
        Some(json!({"org_id": id(&acme)})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
}

// ---------------------------------------------------------------- visibility

#[tokio::test]
async fn private_contacts_of_another_user_are_invisible() {
    let t = env();
    let r = app(&t, Ports::default());
    let secret = new_contact(
        &r,
        &other(),
        json!({"name": "Secret Sarah", "visibility": "private"}),
    )
    .await;
    let secret_org = {
        let o = new_org(&r, &other(), json!({"name": "Hidden"})).await;
        exec(
            &t,
            &format!(
                "UPDATE parties SET visibility = 'private' WHERE id = '{}'",
                id(&o)
            ),
        );
        o
    };
    let sid = id(&secret);
    // the creator sees it
    let (s, _) = req(&r, &other(), "GET", &format!("/contacts/{sid}"), None).await;
    assert_eq!(s, 200);
    let (_, l) = req(&r, &other(), "GET", "/contacts?q=Secret", None).await;
    assert_eq!(ids(&l), vec![sid.clone()]);
    // nobody else does, anywhere
    let (_, l) = req(&r, &me(), "GET", "/contacts?q=Secret", None).await;
    assert_eq!(l, json!([]));
    let (_, l) = req(&r, &me(), "GET", "/contacts", None).await;
    assert!(!ids(&l).contains(&sid));
    for (method, path, body) in [
        ("GET", format!("/contacts/{sid}"), None),
        (
            "PATCH",
            format!("/contacts/{sid}"),
            Some(json!({"name": "x"})),
        ),
        ("DELETE", format!("/contacts/{sid}"), None),
        ("POST", format!("/contacts/{sid}/confirm"), None),
        (
            "POST",
            format!("/contacts/{sid}/jobs"),
            Some(json!({"org_id": "org_x"})),
        ),
        (
            "POST",
            format!("/parties/{sid}/attrs"),
            Some(json!({"key": "phone", "value": "1"})),
        ),
    ] {
        let (s, b) = req(&r, &me(), method, &path, body).await;
        assert_eq!((s, b), (404, err("没有这个联系人")), "{method} {path}");
    }
    let (_, b) = req(
        &r,
        &other(),
        "POST",
        &format!("/parties/{sid}/attrs"),
        Some(json!({"key": "phone", "value": "1"})),
    )
    .await;
    let av = id(&b["attrs"][0]);
    let (s, _) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/attrs/{av}"),
        Some(json!({"value": "2"})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = req(&r, &me(), "DELETE", &format!("/attrs/{av}"), None).await;
    assert_eq!(s, 404);
    // a private organization can be neither seen nor referenced
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/contacts",
        Some(json!({"name": "a", "org_id": id(&secret_org)})),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个组织")));
    let (_, l) = req(&r, &me(), "GET", "/orgs", None).await;
    assert!(!ids(&l).contains(&id(&secret_org)));
    // its jobs do not leak the organization name either
    let (_, c) = req(
        &r,
        &other(),
        "POST",
        "/contacts",
        Some(json!({"name": "Pub", "org_id": id(&secret_org)})),
    )
    .await;
    let (_, seen) = req(&r, &me(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!(
        (seen["jobs"].clone(), seen["org"].clone()),
        (json!([]), Value::Null)
    );
    // and the private contact is not among an organization's people
    let acme = new_org(&r, &me(), json!({"name": "Acme"})).await;
    req(
        &r,
        &other(),
        "POST",
        &format!("/contacts/{sid}/jobs"),
        Some(json!({"org_id": id(&acme)})),
    )
    .await;
    let (_, o) = req(&r, &me(), "GET", &format!("/orgs/{}", id(&acme)), None).await;
    assert_eq!(o["people"], json!([]));
    let (_, o) = req(&r, &other(), "GET", &format!("/orgs/{}", id(&acme)), None).await;
    assert_eq!(ids(&o["people"]), vec![sid]);
}

#[tokio::test]
async fn team_visibility_reaches_team_members() {
    let t = env();
    let r = app(&t, Ports::default());
    let (_, other_me) = req(&r, &other(), "GET", "/me/contact", None).await;
    exec(
        &t,
        &format!(
            "INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated) VALUES ('tm_a', 'default', 'team', 'A 组', 'usr_dev', 1, 1);
             INSERT INTO team_details (party_id, workspace_id, kind) VALUES ('tm_a', 'default', 'virtual');
             INSERT INTO team_members (workspace_id, team_id, contact_id, joined) VALUES ('default', 'tm_a', '{}', 1);",
            id(&other_me)
        ),
    );
    let c = new_contact(
        &r,
        &me(),
        json!({"name": "团队可见", "visibility": "team:tm_a"}),
    )
    .await;
    assert_eq!(c["visibility"], "team:tm_a");
    let (s, _) = req(&r, &other(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!(s, 200, "a member of the team");
    exec(
        &t,
        &format!(
            "DELETE FROM team_members WHERE contact_id = '{}'",
            id(&other_me)
        ),
    );
    let (s, _) = req(&r, &other(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!(s, 404, "no longer a member");
    let (s, _) = req(&r, &me(), "GET", &format!("/contacts/{}", id(&c)), None).await;
    assert_eq!(s, 200, "the creator");
}

// ---------------------------------------------------------------- fakes

/// Account handles as the accounts module would answer.
#[derive(Default)]
struct FakeHandles(Mutex<Vec<(String, String)>>);

impl HandleSearch for FakeHandles {
    fn parties(&self, _: &Connection, _: &str, q: &str) -> ApiResult<Vec<String>> {
        let q = q.to_lowercase();
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(h, _)| h.contains(&q))
            .map(|(_, p)| p.clone())
            .collect())
    }
    fn count(&self, _: &Connection, _: &str, party_id: &str) -> ApiResult<i64> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p == party_id)
            .count() as i64)
    }
}

#[test]
fn domains_are_normalized() {
    use contacts::service::normalize_domain;
    assert_eq!(normalize_domain(" @WWW.Acme.com "), "acme.com");
    assert_eq!(normalize_domain("sarah@acme.com"), "acme.com");
    assert_eq!(normalize_domain("www.acme.co.uk"), "acme.co.uk");
}

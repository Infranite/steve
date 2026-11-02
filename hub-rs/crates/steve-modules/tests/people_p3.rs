//! Slice P3 acceptance (contacts.md §9): mentions link `related`, names are inferred at 0.6,
//! `steve_contacts_find` never shows `never` attributes or other users' private contacts — plus the
//! memory routes, item people, the `@` menu, ⌘K, a party's items and timeline, `steve_contacts_remember`
//! and contact cards.

use axum::Router;
use serde_json::{Value, json};
use steve_modules::contacts::mcp::{self, McpCx};
use steve_modules::contacts::{self, Ports};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_modules::{Caller, Modules};
use steve_store::base::LEVELS_NOTE;

fn env() -> TestEnv {
    let t = TestEnv::new(|_| {});
    exec(
        &t,
        "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('bot_dev', 'default', 'Dev Bot', 'claude', 1), ('bot_off', 'default', 'Off Bot', 'claude', 1);
         UPDATE bots SET enabled = 0 WHERE id = 'bot_off';
         INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('bot_dev', 'default', 1), ('bot_off', 'default', 1);
         INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES
           ('ST-1', 'default', 1, 'task', '报价单', 'run', '定义', 1, 1),
           ('ST-2', 'default', 2, 'task', '交付', 'run', '定义', 2, 2),
           ('ST-3', 'default', 3, 'task', '旧事', 'done', '完成', 3, 3),
           ('W2-1', 'w2', 1, 'task', '别处', 'run', '定义', 4, 4);
         UPDATE items SET archived = 1 WHERE id = 'ST-3';",
    );
    t
}

fn app(t: &TestEnv) -> Router {
    contacts::routes(Ports::default()).with_state(t.modules.clone())
}

fn me() -> Caller {
    caller(WS, ME)
}

fn other() -> Caller {
    caller(WS, OTHER)
}

fn m(t: &TestEnv) -> &Modules {
    &t.modules
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

fn exec(t: &TestEnv, sql: &str) {
    t.store()
        .with_conn(|c| Ok(c.execute_batch(sql)?))
        .expect("exec");
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

async fn contact(r: &Router, who: &Caller, body: Value) -> String {
    let (s, b) = req(r, who, "POST", "/contacts", Some(body)).await;
    assert_eq!(s, 200, "{b}");
    b["id"].as_str().unwrap().to_string()
}

async fn mem(r: &Router, who: &Caller, party: &str, body: Value) -> Value {
    let (s, b) = req(
        r,
        who,
        "POST",
        &format!("/parties/{party}/memories"),
        Some(body),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    b
}

fn cx(user: &str) -> McpCx {
    McpCx {
        ws: WS.into(),
        user: user.into(),
        bot_id: "bot_dev".into(),
        item_id: Some("ST-1".into()),
        session_id: Some("ses_1".into()),
    }
}

fn link(t: &TestEnv, item: &str, party: &str) -> Option<(String, f64)> {
    t.store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT source, confidence FROM links WHERE subject_type = 'item' AND subject_id = ?1 AND predicate = 'related' AND object_id = ?2",
                [item, party],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok())
        })
        .unwrap()
}

// ---------------------------------------------------------------- acceptance

#[tokio::test]
async fn mentions_link_related() {
    let t = env();
    let r = app(&t);
    let lilei = contact(&r, &me(), json!({"name": "李雷"})).await;
    let ms = vec![
        contacts::Mention {
            kind: "party".into(),
            id: lilei.clone(),
        },
        contacts::Mention {
            kind: "party".into(),
            id: lilei.clone(),
        },
        contacts::Mention {
            kind: "bot".into(),
            id: "bot_dev".into(),
        },
        contacts::Mention {
            kind: "party".into(),
            id: "ct_gone".into(),
        },
    ];
    contacts::check_mentions(&ms).unwrap();
    let names = contacts::link_mentions(m(&t), &me(), "ST-1", &ms).unwrap();
    assert_eq!(names, ["李雷"]);
    assert_eq!(
        contacts::mention_note(&names).as_deref(),
        Some("关联了 李雷")
    );
    assert_eq!(contacts::mention_note(&[]), None);
    assert_eq!(link(&t, "ST-1", &lilei), Some(("mention".into(), 1.0)));
    let (s, people) = req(&r, &me(), "GET", "/items/ST-1/people", None).await;
    assert_eq!(s, 200);
    assert_eq!(
        people,
        json!([{"party_id": lilei, "name": "李雷", "type": "contact", "role": "related", "source": "mention", "confidence": 1.0}])
    );

    // validation
    let bad = vec![contacts::Mention {
        kind: "user".into(),
        id: "x".into(),
    }];
    assert_eq!(
        contacts::check_mentions(&bad).unwrap_err().to_string(),
        "参数不对：mentions.0.kind"
    );
    let many: Vec<_> = (0..21)
        .map(|i| contacts::Mention {
            kind: "party".into(),
            id: format!("ct_{i}"),
        })
        .collect();
    assert!(contacts::check_mentions(&many).is_err());
    // another workspace's item
    assert_eq!(
        contacts::link_mentions(m(&t), &me(), "W2-1", &ms)
            .unwrap_err()
            .to_string(),
        "没有这个事项"
    );
}

#[tokio::test]
async fn names_are_inferred_without_mentions() {
    let t = env();
    let r = app(&t);
    let lilei = contact(&r, &me(), json!({"name": "李雷", "aliases": ["Lei"]})).await;
    let lileilei = contact(&r, &me(), json!({"name": "李雷雷"})).await;
    let wang1 = contact(&r, &me(), json!({"name": "王芳"})).await;
    let wang2 = contact(&r, &me(), json!({"name": "王芳"})).await;
    let secret = contact(
        &r,
        &other(),
        json!({"name": "韩梅梅", "visibility": "private"}),
    )
    .await;
    let x = contact(&r, &me(), json!({"name": "X"})).await;
    let team_body =
        json!({"name": "交付小组", "members": [{"contact_id": lilei}, {"contact_id": wang1}]});
    let (s, team) = req(&r, &me(), "POST", "/teams", Some(team_body)).await;
    assert_eq!(s, 200, "{team}");
    let team = team["id"].as_str().unwrap().to_string();

    let names = contacts::infer_item_people(
        m(&t),
        &me(),
        "ST-1",
        "请李雷和王芳、韩梅梅跟交付小组一起看 X 的报价",
    )
    .unwrap();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"李雷".to_string()) && names.contains(&"交付小组".to_string()));
    assert_eq!(link(&t, "ST-1", &lilei), Some(("infer".into(), 0.6)));
    assert_eq!(link(&t, "ST-1", &team), Some(("infer".into(), 0.6)));
    // 重名不猜，别人的私有联系人看不到，单字名不算
    for id in [&wang1, &wang2, &secret, &x, &lileilei] {
        assert_eq!(link(&t, "ST-1", id), None, "{id}");
    }

    // a longer name does not also name the shorter one inside it; aliases count
    let names = contacts::infer_item_people(m(&t), &me(), "ST-2", "李雷雷说过").unwrap();
    assert_eq!(names, ["李雷雷"]);
    assert_eq!(link(&t, "ST-2", &lilei), None);
    let names = contacts::infer_item_people(m(&t), &me(), "ST-2", "李雷雷 and Lei").unwrap();
    assert_eq!(names, ["李雷"]);
    // “我” is never inferred, even by name
    let names = contacts::infer_item_people(m(&t), &me(), "ST-2", "Dev 自己来").unwrap();
    assert!(names.is_empty());
    // an existing link is not touched
    exec(
        &t,
        "UPDATE links SET source = 'mention', confidence = 1 WHERE subject_id = 'ST-2'",
    );
    assert!(
        contacts::infer_item_people(m(&t), &me(), "ST-2", "李雷雷")
            .unwrap()
            .is_empty()
    );
    assert_eq!(link(&t, "ST-2", &lileilei), Some(("mention".into(), 1.0)));
}

#[tokio::test]
async fn find_hides_never_and_private() {
    let t = env();
    let r = app(&t);
    let (_, acme) = req(
        &r,
        &me(),
        "POST",
        "/orgs",
        Some(json!({"name": "Acme", "kind": "customer", "domains": ["acme.com"]})),
    )
    .await;
    let acme = acme["id"].as_str().unwrap().to_string();
    let sarah = contact(&r, &me(), json!({"name": "Sarah Chen", "aliases": ["Sarah"], "rels": ["customer"], "org_id": acme, "title": "产品经理"})).await;
    for (key, label, value, level) in [
        ("phone", "电话", "+1 555", "reasoning"),
        ("secret", "暗号", "天王盖地虎", "never"),
        ("call", "偏好的称呼", "Sarah", "public"),
    ] {
        let (s, b) = req(
            &r,
            &me(),
            "POST",
            &format!("/parties/{sarah}/attrs"),
            Some(json!({"key": key, "label": label, "value": value, "level": level})),
        )
        .await;
        assert_eq!(s, 200, "{b}");
    }
    mem(
        &r,
        &me(),
        &sarah,
        json!({"text": "喜欢简短的邮件，先给结论", "kind": "preference", "level": "public"}),
    )
    .await;
    mem(
        &r,
        &me(),
        &sarah,
        json!({"text": "绝对不能提的事", "level": "never"}),
    )
    .await;
    // the other user's private contact with the same name, and their private memory about Sarah
    contact(
        &r,
        &other(),
        json!({"name": "Sarah Lee", "visibility": "private"}),
    )
    .await;
    mem(&r, &other(), &sarah, json!({"text": "别人的私事"})).await;

    let out = mcp::call_sync(
        m(&t),
        &Ports::default(),
        &cx(ME),
        mcp::FIND,
        &json!({"query": "sarah"}),
    )
    .unwrap();
    assert!(
        out.starts_with(&format!("Sarah Chen (contact, id {sarah})")),
        "{out}"
    );
    assert!(out.contains("Works at: Acme, 产品经理"), "{out}");
    assert!(out.contains("Relationship to the user: customer"));
    assert!(out.contains("- 电话: +1 555 (level: reasoning)"));
    assert!(out.contains("- 偏好的称呼: Sarah (level: public)"));
    assert!(out.contains("- [preference] 喜欢简短的邮件，先给结论 (level: public)"));
    assert!(out.ends_with(LEVELS_NOTE), "{out}");
    for hidden in [
        "天王盖地虎",
        "暗号",
        "绝对不能提的事",
        "Sarah Lee",
        "别人的私事",
    ] {
        assert!(!out.contains(hidden), "{hidden} in {out}");
    }
    // the other user sees their own contact, not my private memory
    let theirs = mcp::call_sync(
        m(&t),
        &Ports::default(),
        &cx(OTHER),
        mcp::FIND,
        &json!({"query": "Sarah"}),
    )
    .unwrap();
    assert!(theirs.contains("Sarah Lee") && theirs.contains("别人的私事"));
    assert!(!theirs.contains("喜欢简短的邮件"));

    // organizations by name or by an email's domain, with their people
    let org = mcp::call_sync(
        m(&t),
        &Ports::default(),
        &cx(ME),
        mcp::FIND,
        &json!({"query": "bob@acme.com"}),
    )
    .unwrap();
    assert!(
        org.starts_with(&format!("Acme (organization, id {acme})")),
        "{org}"
    );
    assert!(org.contains("Domains: acme.com") && org.contains("People: Sarah Chen (产品经理)"));
    let none = mcp::call_sync(
        m(&t),
        &Ports::default(),
        &cx(ME),
        mcp::FIND,
        &json!({"query": "nobody"}),
    )
    .unwrap();
    assert_eq!(
        none,
        format!("No people, organizations or teams match \"nobody\".\n\n{LEVELS_NOTE}")
    );

    // argument errors keep steve-mcp's wording
    assert_eq!(
        mcp::call_sync(
            m(&t),
            &Ports::default(),
            &cx(ME),
            mcp::FIND,
            &json!({"query": 3})
        )
        .unwrap_err(),
        "MCP error -32602: Input validation error: Invalid arguments for tool steve_contacts_find: Invalid input: expected string, received number at query"
    );
    assert_eq!(
        mcp::call_sync(m(&t), &Ports::default(), &cx(ME), "steve_nope", &json!({})).unwrap_err(),
        "MCP error -32602: Tool steve_nope not found"
    );
}

#[tokio::test]
async fn remember_writes_a_candidate_with_a_decision() {
    let t = env();
    let r = app(&t);
    let sarah = contact(
        &r,
        &me(),
        json!({"name": "Sarah Chen", "aliases": ["Sarah"]}),
    )
    .await;
    contact(&r, &me(), json!({"name": "王芳"})).await;
    contact(&r, &me(), json!({"name": "王芳"})).await;
    let p = Ports::default();
    let out = mcp::call_sync(
        m(&t),
        &p,
        &cx(ME),
        mcp::REMEMBER,
        &json!({"who": "sarah", "text": " 周五不开会 ", "kind": "fact"}),
    )
    .unwrap();
    assert_eq!(out, "Saved as a candidate; the user will confirm it.");
    let mm = text(&t, "SELECT id FROM mems WHERE text = '周五不开会'");
    assert_eq!(
        text(
            &t,
            &format!(
                "SELECT status || '|' || writer || '|' || visibility || '|' || created_by || '|' || source_type || '|' || source_id FROM mems WHERE id = '{mm}'"
            )
        ),
        "candidate|bot_dev|private|usr_dev|item|ST-1"
    );
    assert_eq!(
        text(
            &t,
            &format!("SELECT object_id FROM mem_about WHERE mem_id = '{mm}'")
        ),
        sarah
    );
    assert_eq!(
        text(
            &t,
            &format!(
                "SELECT origin || '|' || origin_id || '|' || origin_label FROM mem_candidates WHERE mem_id = '{mm}'"
            )
        ),
        "tool|ST-1|ST-1 的会话"
    );
    assert_eq!(
        text(
            &t,
            &format!(
                "SELECT d.kind || '|' || d.subject_type || '|' || d.question || '|' || d.requested_by || '|' || d.options FROM decisions d JOIN mems m ON m.decision_id = d.id WHERE m.id = '{mm}'"
            )
        ),
        "confirm|mem|记住这条吗？周五不开会|bot_dev|[\"yes\",\"no\"]"
    );
    // a candidate is not confirmed knowledge: find does not show it
    let found = mcp::call_sync(m(&t), &p, &cx(ME), mcp::FIND, &json!({"query": "Sarah"})).unwrap();
    assert!(!found.contains("周五不开会"));
    // by id; never a guess
    assert!(
        mcp::call_sync(
            m(&t),
            &p,
            &cx(ME),
            mcp::REMEMBER,
            &json!({"who": sarah, "text": "a", "kind": "event"})
        )
        .is_ok()
    );
    let e = mcp::call_sync(
        m(&t),
        &p,
        &cx(ME),
        mcp::REMEMBER,
        &json!({"who": "王芳", "text": "a", "kind": "fact"}),
    )
    .unwrap_err();
    assert!(e.starts_with("Several are called \"王芳\""), "{e}");
    let e = mcp::call_sync(
        m(&t),
        &p,
        &cx(ME),
        mcp::REMEMBER,
        &json!({"who": "Nobody", "text": "a", "kind": "fact"}),
    )
    .unwrap_err();
    assert!(
        e.starts_with("No person, organization or team is called \"Nobody\""),
        "{e}"
    );
    let e = mcp::call_sync(
        m(&t),
        &p,
        &cx(ME),
        mcp::REMEMBER,
        &json!({"who": "Sarah", "text": "a", "kind": "rule"}),
    )
    .unwrap_err();
    assert!(
        e.ends_with("Invalid option: expected one of \"fact\"|\"preference\"|\"event\" at kind"),
        "{e}"
    );
    // the other user can't target my private contact
    let secret = contact(&r, &me(), json!({"name": "秘密", "visibility": "private"})).await;
    assert!(
        mcp::call_sync(
            m(&t),
            &p,
            &cx(OTHER),
            mcp::REMEMBER,
            &json!({"who": secret, "text": "a", "kind": "fact"})
        )
        .is_err()
    );

    // at most 20 open memory decisions; the rest wait without one
    for i in 0..25 {
        mcp::call_sync(
            m(&t),
            &p,
            &cx(ME),
            mcp::REMEMBER,
            &json!({"who": "Sarah", "text": format!("事 {i}"), "kind": "fact"}),
        )
        .unwrap();
    }
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM decisions WHERE subject_type = 'mem' AND status = 'open'"
        ),
        20
    );
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM mems WHERE status = 'candidate' AND decision_id IS NULL"
        ),
        7
    );

    // deleting the contact deletes its candidates with their rows and closes their decisions
    let (s, b) = req(&r, &me(), "DELETE", &format!("/contacts/{sarah}"), None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(count(&t, "SELECT COUNT(*) FROM mems"), 0);
    assert_eq!(count(&t, "SELECT COUNT(*) FROM mem_candidates"), 0);
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM decisions WHERE subject_type = 'mem' AND status = 'open'"
        ),
        0
    );
}

// ---------------------------------------------------------------- memories

#[tokio::test]
async fn memory_routes() {
    let t = env();
    let r = app(&t);
    let sarah = contact(&r, &me(), json!({"name": "Sarah"})).await;
    let a = mem(
        &r,
        &me(),
        &sarah,
        json!({"text": "  喜欢简短的邮件 ", "kind": "preference"}),
    )
    .await;
    let mm = a["id"].as_str().unwrap().to_string();
    assert!(mm.starts_with("mm_"));
    assert_eq!(a["text"], "喜欢简短的邮件");
    assert_eq!(a["kind"], "preference");
    assert_eq!(a["status"], "confirmed");
    assert_eq!(a["level"], "conclusion");
    assert_eq!(a["visibility"], "private");
    assert_eq!(a["source_label"], "你写的");
    assert_eq!(a["about"], json!([{"type": "party", "id": sarah}]));
    assert!(
        t.take_events()
            .iter()
            .any(|e| matches!(e, steve_store::ChangeEvent::Mm { id, ws } if *id == mm && ws == WS))
    );

    // “我” and teams and organizations have memories too
    mem(
        &r,
        &me(),
        "me",
        json!({"text": "早上不开会", "kind": "rule"}),
    )
    .await;
    let (s, _) = req(&r, &me(), "GET", "/parties/me/memories", None).await;
    assert_eq!(s, 200);

    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{sarah}/memories"),
        Some(json!({"text": "", "kind": "summary"})),
    )
    .await;
    assert_eq!((s, b), (400, json!({"error": "参数不对：text、kind"})));
    let (s, _) = req(
        &r,
        &me(),
        "POST",
        "/parties/ct_none/memories",
        Some(json!({"text": "x"})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "GET",
        &format!("/parties/{sarah}/memories"),
        None,
    )
    .await;
    assert_eq!((s, b), (404, json!({"error": "没有这个联系人"})));

    // private: the other user sees the contact but not the memory
    let (s, list) = req(
        &r,
        &other(),
        "GET",
        &format!("/parties/{sarah}/memories"),
        None,
    )
    .await;
    assert_eq!((s, list), (200, json!([])));
    let (s, b) = req(
        &r,
        &other(),
        "PATCH",
        &format!("/mems/{mm}"),
        Some(json!({"text": "x"})),
    )
    .await;
    assert_eq!((s, b), (404, json!({"error": "没有这个记忆"})));

    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/mems/{mm}"),
        Some(json!({"text": "喜欢短邮件", "level": "public", "visibility": "workspace"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        (
            b["text"].clone(),
            b["level"].clone(),
            b["visibility"].clone()
        ),
        (json!("喜欢短邮件"), json!("public"), json!("workspace"))
    );
    let (_, list) = req(
        &r,
        &other(),
        "GET",
        &format!("/parties/{sarah}/memories"),
        None,
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/mems/{mm}"),
        Some(json!({"status": "candidate"})),
    )
    .await;
    assert_eq!((s, b), (400, json!({"error": "参数不对：status"})));
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/mems/{mm}"),
        Some(json!({"visibility": "team:tm_none"})),
    )
    .await;
    assert_eq!((s, b), (404, json!({"error": "没有这个团队"})));

    // never-level memories show on the page, stale ones too
    mem(
        &r,
        &me(),
        &sarah,
        json!({"text": "不可用", "level": "never"}),
    )
    .await;
    let (_, list) = req(
        &r,
        &me(),
        "GET",
        &format!("/parties/{sarah}/memories"),
        None,
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    let (s, b) = req(&r, &me(), "DELETE", &format!("/mems/{mm}"), None).await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    let (s, _) = req(&r, &me(), "DELETE", &format!("/mems/{mm}"), None).await;
    assert_eq!(s, 404);
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM audit WHERE action IN ('mm.create', 'mm.patch', 'mm.delete')"
        ),
        5
    );
}

#[tokio::test]
async fn confirming_a_candidate_by_hand_settles_its_decision() {
    let t = env();
    let r = app(&t);
    let sarah = contact(&r, &me(), json!({"name": "Sarah"})).await;
    mcp::call_sync(
        m(&t),
        &Ports::default(),
        &cx(ME),
        mcp::REMEMBER,
        &json!({"who": sarah, "text": "周五不开会", "kind": "fact"}),
    )
    .unwrap();
    let mm = text(&t, "SELECT id FROM mems");
    let (s, b) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/mems/{mm}"),
        Some(json!({"status": "confirmed"})),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        text(&t, "SELECT status || '|' || result FROM decisions"),
        "resolved|{\"choice\":\"yes\",\"by\":\"edit\"}"
    );
    assert_eq!(count(&t, "SELECT COUNT(*) FROM mem_candidates"), 0);
}

// ---------------------------------------------------------------- item people

#[tokio::test]
async fn item_people_routes() {
    let t = env();
    let r = app(&t);
    let sarah = contact(&r, &me(), json!({"name": "Sarah"})).await;
    let (s, list) = req(
        &r,
        &me(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": sarah, "role": "waiting_on"})),
    )
    .await;
    assert_eq!(s, 200, "{list}");
    assert_eq!(
        list,
        json!([{"party_id": sarah, "name": "Sarah", "type": "contact", "role": "waiting_on", "source": "manual", "confidence": 1.0}])
    );
    // again: unchanged, no second timeline line
    let (s, again) = req(
        &r,
        &me(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": sarah, "role": "waiting_on"})),
    )
    .await;
    assert_eq!((s, &again), (200, &list));
    req(
        &r,
        &me(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": sarah, "role": "about"})),
    )
    .await;
    assert_eq!(
        text(
            &t,
            "SELECT group_concat(actor || ' ' || kind || ' ' || text, ' / ') FROM timeline WHERE item_id = 'ST-1'"
        ),
        "usr_dev sys 关联了 Sarah（在等） / usr_dev sys 关联了 Sarah（关于）"
    );

    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": sarah, "role": "boss"})),
    )
    .await;
    assert_eq!((s, b), (400, json!({"error": "参数不对：role"})));
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/items/W2-1/people",
        Some(json!({"party_id": sarah, "role": "related"})),
    )
    .await;
    assert_eq!((s, b), (404, json!({"error": "没有这个事项"})));
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": "ct_none", "role": "related"})),
    )
    .await;
    assert_eq!((s, b), (404, json!({"error": "没有这个联系人"})));
    let secret = contact(&r, &other(), json!({"name": "私", "visibility": "private"})).await;
    let (s, _) = req(
        &r,
        &me(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": secret, "role": "related"})),
    )
    .await;
    assert_eq!(s, 404);

    let (s, b) = req(
        &r,
        &me(),
        "DELETE",
        &format!("/items/ST-1/people/{sarah}?role=about"),
        None,
    )
    .await;
    assert_eq!((s, b), (200, json!({"ok": true})));
    let (_, list) = req(&r, &me(), "GET", "/items/ST-1/people", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    req(
        &r,
        &me(),
        "DELETE",
        &format!("/items/ST-1/people/{sarah}"),
        None,
    )
    .await;
    let (_, list) = req(&r, &me(), "GET", "/items/ST-1/people", None).await;
    assert_eq!(list, json!([]));
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM timeline WHERE text = '移除了 Sarah'"
        ),
        2
    );
    // nothing left to remove: no line
    req(
        &r,
        &me(),
        "DELETE",
        &format!("/items/ST-1/people/{sarah}"),
        None,
    )
    .await;
    assert_eq!(
        count(
            &t,
            "SELECT COUNT(*) FROM timeline WHERE text = '移除了 Sarah'"
        ),
        2
    );

    // a deleted contact keeps its link and is named as such
    req(
        &r,
        &me(),
        "POST",
        "/items/ST-2/people",
        Some(json!({"party_id": sarah, "role": "raised_by"})),
    )
    .await;
    req(&r, &me(), "DELETE", &format!("/contacts/{sarah}"), None).await;
    let (_, list) = req(&r, &me(), "GET", "/items/ST-2/people", None).await;
    assert_eq!(list[0]["name"], "已删除的联系人");
}

#[tokio::test]
async fn party_items_and_timeline() {
    let t = env();
    let r = app(&t);
    let a = contact(&r, &me(), json!({"name": "A"})).await;
    let b = contact(&r, &me(), json!({"name": "B"})).await;
    let (_, team) = req(
        &r,
        &me(),
        "POST",
        "/teams",
        Some(json!({"name": "T", "members": [{"contact_id": a}, {"contact_id": b}]})),
    )
    .await;
    let team = team["id"].as_str().unwrap().to_string();
    for (item, party, role) in [
        ("ST-1", &a, "raised_by"),
        ("ST-3", &b, "related"),
        ("ST-2", &team, "related"),
        ("ST-1", &b, "related"),
    ] {
        let (s, x) = req(
            &r,
            &me(),
            "POST",
            &format!("/items/{item}/people"),
            Some(json!({"party_id": party, "role": role})),
        )
        .await;
        assert_eq!(s, 200, "{x}");
    }
    let (s, items) = req(&r, &me(), "GET", &format!("/parties/{b}/items"), None).await;
    assert_eq!(s, 200);
    let got: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item_id"].as_str().unwrap())
        .collect();
    assert_eq!(got.len(), 2, "{items}"); // archived items stay on a contact
    // a team: its own items ∪ its members' unarchived items, one per (item, role)
    let (_, items) = req(&r, &me(), "GET", &format!("/parties/{team}/items"), None).await;
    let mut got: Vec<String> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            format!(
                "{}:{}",
                i["item_id"].as_str().unwrap(),
                i["role"].as_str().unwrap()
            )
        })
        .collect();
    got.sort();
    assert_eq!(got, ["ST-1:raised_by", "ST-1:related", "ST-2:related"]);

    let (s, tl) = req(&r, &me(), "GET", &format!("/parties/{a}/timeline"), None).await;
    assert_eq!(s, 200);
    assert_eq!(tl.as_array().unwrap().len(), 1);
    assert_eq!(tl[0]["kind"], "item");
    assert_eq!(tl[0]["text"], "ST-1 报价单（提出人）");
    assert_eq!(tl[0]["href"], "#/items/ST-1");
    let (s, b2) = req(
        &r,
        &me(),
        "GET",
        &format!("/parties/{a}/timeline?limit=101"),
        None,
    )
    .await;
    assert_eq!((s, b2), (400, json!({"error": "参数不对：limit"})));
    let ts = tl[0]["ts"].as_i64().unwrap();
    let (_, tl) = req(
        &r,
        &me(),
        "GET",
        &format!("/parties/{a}/timeline?before={ts}"),
        None,
    )
    .await;
    assert_eq!(tl, json!([]));
    let (s, _) = req(
        &r,
        &caller(WS2, ME),
        "GET",
        &format!("/parties/{a}/items"),
        None,
    )
    .await;
    assert_eq!(s, 404);
}

// ---------------------------------------------------------------- @ menu and ⌘K

#[tokio::test]
async fn mention_options_and_search() {
    let t = env();
    let r = app(&t);
    let (_, acme) = req(&r, &me(), "POST", "/orgs", Some(json!({"name": "Acme"}))).await;
    contact(
        &r,
        &me(),
        json!({"name": "Sarah Chen", "org_id": acme["id"], "title": "PM"}),
    )
    .await;
    contact(&r, &me(), json!({"name": "Lisa", "aliases": ["小 sa"]})).await;
    contact(
        &r,
        &other(),
        json!({"name": "Sam", "visibility": "private"}),
    )
    .await;
    let (s, o) = req(&r, &me(), "GET", "/mention-options?q=SA", None).await;
    assert_eq!(s, 200);
    let names: Vec<&str> = o["parties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Sarah Chen", "Lisa"]); // prefix first, private of others hidden
    assert_eq!(o["parties"][0]["sub"], "Acme · PM");
    assert_eq!(o["parties"][0]["type"], "contact");
    assert_eq!(o["bots"], json!([]));
    let (_, o) = req(&r, &me(), "GET", "/mention-options?q=dev", None).await;
    assert_eq!(o["bots"], json!([{"id": "bot_dev", "name": "Dev Bot"}])); // enabled only
    assert_eq!(o["parties"], json!([])); // never yourself
    let (_, o) = req(&r, &me(), "GET", "/mention-options", None).await;
    assert!(o["parties"].as_array().unwrap().len() <= 8);

    let (s, f) = req(&r, &me(), "GET", "/parties/search?q=a", None).await;
    assert_eq!(s, 200);
    assert_eq!(f["orgs"][0]["name"], "Acme");
    assert_eq!(f["contacts"].as_array().unwrap().len(), 2);
    let (_, f) = req(&r, &me(), "GET", "/parties/search?q=", None).await;
    assert_eq!(f, json!({"contacts": [], "orgs": [], "teams": []}));
}

// ---------------------------------------------------------------- contact cards

#[tokio::test]
async fn contact_cards_for_prompts() {
    let t = env();
    let r = app(&t);
    let (_, acme) = req(
        &r,
        &me(),
        "POST",
        "/orgs",
        Some(json!({"name": "Acme", "kind": "customer"})),
    )
    .await;
    let sarah = contact(&r, &me(), json!({"name": "Sarah Chen", "rels": ["customer"], "org_id": acme["id"], "title": "产品经理"})).await;
    req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{sarah}/attrs"),
        Some(json!({"key": "phone", "value": "+1 555"})),
    )
    .await;
    req(
        &r,
        &me(),
        "POST",
        &format!("/parties/{sarah}/attrs"),
        Some(json!({"key": "secret", "label": "暗号", "value": "虎", "level": "never"})),
    )
    .await;
    mem(
        &r,
        &me(),
        &sarah,
        json!({"text": "喜欢简短的邮件", "kind": "preference", "level": "public"}),
    )
    .await;
    mem(
        &r,
        &me(),
        &sarah,
        json!({"text": "不能说", "level": "never"}),
    )
    .await;
    let hidden = contact(&r, &other(), json!({"name": "私", "visibility": "private"})).await;
    req(
        &r,
        &me(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": sarah, "role": "raised_by"})),
    )
    .await;
    req(
        &r,
        &other(),
        "POST",
        "/items/ST-1/people",
        Some(json!({"party_id": hidden, "role": "related"})),
    )
    .await;

    let viewer = t.modules.viewer(&me()).unwrap();
    let cards = t
        .store()
        .reader(|c| contacts::item_contact_cards(c, &viewer, "ST-1"))
        .unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0].text,
        "Sarah Chen（联系人 · 提出人；Acme · 产品经理；和用户的关系：客户）\n- 电话：+1 555（仅用于推理）\n- 记得（偏好）：喜欢简短的邮件（可以引用）"
    );
    assert_eq!(cards[0].roles, ["raised_by"]);
    let cards = t
        .store()
        .reader(|c| {
            contacts::contact_cards(c, &viewer, &[sarah.clone(), hidden.clone(), sarah.clone()])
        })
        .unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(contacts::cards::render(&[]), "");
}

//! The knowledge page's provenance, subjects and the memory upkeep tools: every memory says where it
//! came from and how often it was used; a subject gathers its memories, documents and uses; and
//! `steve_memory_tidy` merges (through the 待确认 flow), resolves, flags, marks stale and fills in
//! subjects, each change listed per run and undoable.

use axum::Router;
use serde_json::{Value, json};
use steve_knowledge::Knowledge;
use steve_knowledge::candidates::{self, MemDecisions};
use steve_knowledge::steward::{self, LIST, TIDY};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_store::base::mems::{self, Mem};

struct Env {
    env: TestEnv,
    app: Router,
}

fn setup() -> Env {
    let env = TestEnv::new(|r| {
        r.decision(candidates::SUBJECT, Box::new(MemDecisions));
    });
    let kb = Knowledge::new(|_| {});
    let app = steve_knowledge::routes(kb).with_state(env.modules.clone());
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, aliases, created_by, created, updated) VALUES
                   ('ct_sarah', 'default', 'contact', 'Sarah Chen', '[\"Sarah\"]', 'usr_dev', 1, 1),
                   ('org_acme', 'default', 'org', 'Acme', '[]', 'usr_dev', 1, 1);
                 INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, created, updated)
                   VALUES ('ST-12', 'default', 12, '整理知识', 'running', '进行中', NULL, 1, 100);",
            )?;
            Ok(())
        })
        .expect("seed");
    Env { env, app }
}

fn cx() -> McpCx {
    McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "bot_steward".into(),
        item_id: Some("ST-12".into()),
        session_id: Some("ses_1".into()),
    }
}

#[test]
fn memory_listing_only_attaches_a_live_members_personal_profile() {
    let e = setup();
    let listed: Value =
        serde_json::from_str(&steward::call(&e.env.modules, &cx(), LIST, &json!({})).unwrap())
            .unwrap();
    assert_eq!(listed["profile"]["is_me"], true);
    let mut service = cx();
    service.user = format!("svc:{WS}");
    let listed: Value =
        serde_json::from_str(&steward::call(&e.env.modules, &service, LIST, &json!({})).unwrap())
            .unwrap();
    assert!(listed["profile"].is_null());
    assert!(
        e.env
            .store()
            .reader(|c| { steve_modules::contacts::repo::me_of(c, WS, &service.user) })
            .unwrap()
            .is_none()
    );
}

impl Env {
    async fn me(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        call(
            &self.app,
            Some(&caller(WS, ME)),
            method,
            path,
            body.as_ref(),
        )
        .await
    }
    fn mem(&self, id: &str) -> Option<Mem> {
        self.env.store().reader(|c| mems::get(c, id)).unwrap()
    }
    fn status(&self, id: &str) -> String {
        self.mem(id)
            .map(|m| m.status)
            .unwrap_or_else(|| "gone".into())
    }
    async fn add(&self, text: &str, about: Option<(&str, &str)>) -> String {
        let about = about
            .map(|(t, i)| json!([{"type": t, "id": i}]))
            .unwrap_or(json!([]));
        let (s, v) = self
            .me(
                "POST",
                "/kb/mems",
                Some(json!({"text": text, "kind": "preference", "about": about})),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        v["id"].as_str().unwrap().to_string()
    }
    fn tidy(&self, args: Value) -> Result<String, String> {
        steward::call(&self.env.modules, &cx(), TIDY, &args)
    }
    async fn changes(&self) -> Value {
        let (s, v) = self.me("GET", "/kb/changes", None).await;
        assert_eq!(s, 200, "{v}");
        v
    }
}

#[tokio::test]
async fn memories_say_where_they_came_from_and_how_often_they_were_used() {
    let e = setup();
    let a = e
        .add("Sarah 喜欢简短的邮件", Some(("party", "ct_sarah")))
        .await;
    e.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO kb_uses (id, workspace_id, consumer_type, consumer_id, query, hits, created) VALUES ('ku_1', 'default', 'session', 'ses_1', '邮件', ?, 50)",
                [json!([{"n": 1, "obj_type": "mem", "obj_id": a, "level": "conclusion", "label": "偏好"}]).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let r = candidates::remember(
        &e.env.modules,
        &cx(),
        &json!({"text": "Acme 每年三月续签", "kind": "fact", "about": "Acme"}),
    );
    assert!(r.is_ok(), "{r:?}");

    let (_, list) = e.me("GET", "/kb/mems", None).await;
    let mine = list
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == a.as_str())
        .unwrap();
    assert_eq!(mine["origin"]["kind"], "you");
    assert_eq!(mine["origin"]["href"], "");
    assert_eq!(mine["used"], json!({"count": 1, "last": 50}));
    let cand = list
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["status"] == "candidate")
        .unwrap();
    assert_eq!(cand["origin"]["kind"], "item");
    assert_eq!(cand["origin"]["href"], "#/items/ST-12");
    assert_eq!(cand["origin"]["writer"], "bot_steward");
    assert_eq!(cand["used"]["count"], 0);

    // the 来源 tab: who writes the memories
    let (s, p) = e.me("GET", "/kb/providers", None).await;
    assert_eq!(s, 200, "{p}");
    let kinds: Vec<&str> = p["memory"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"you") && kinds.contains(&"item"), "{p}");
    assert_eq!(p["store"], "sqlite");

    // a subject: its memories and where they were used
    let (_, subs) = e.me("GET", "/kb/subjects", None).await;
    let sarah = subs
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == "ct_sarah")
        .unwrap();
    assert_eq!(
        (
            sarah["name"].as_str(),
            sarah["mems"].as_i64(),
            sarah["href"].as_str()
        ),
        (Some("Sarah Chen"), Some(1), Some("#/contacts/ct_sarah"))
    );
    let (s, one) = e.me("GET", "/kb/subjects/party/ct_sarah", None).await;
    assert_eq!(s, 200, "{one}");
    assert_eq!(one["mems"][0]["id"], a.as_str());
    assert_eq!(one["uses"][0]["consumer_type"], "session");
    assert_eq!(one["uses"][0]["href"], "#/sessions/ses_1");
    let (s, _) = e.me("GET", "/kb/subjects/party/ct_nobody", None).await;
    assert_eq!(s, 404);
}

#[tokio::test]
async fn a_merge_waits_for_you_and_replaces_all_it_names() {
    let e = setup();
    let a = e.add("提交信息用中文", None).await;
    let b = e.add("commit message 写中文", None).await;
    let c = e.add("先说结论", None).await;

    let listed = steward::call(&e.env.modules, &cx(), LIST, &json!({})).unwrap();
    assert!(
        listed.contains(&a) && listed.contains("written by the user"),
        "{listed}"
    );

    let out = e
        .tidy(json!({"op": "merge", "ids": [a, b], "text": "提交信息用中文写", "reason": "两条说的是同一件事"}))
        .unwrap();
    assert!(out.contains("candidate"), "{out}");
    // nothing changed yet: the merge is a candidate in 待确认
    assert_eq!(
        (e.status(&a), e.status(&b)),
        ("confirmed".into(), "confirmed".into())
    );
    let ch = e.changes().await;
    assert_eq!(ch[0]["run_id"], "ST-12");
    assert_eq!(ch[0]["href"], "#/items/ST-12");
    let merge = &ch[0]["changes"][0];
    assert_eq!(
        (merge["op"].as_str(), merge["state"].as_str()),
        (Some("merge"), Some("pending"))
    );
    assert_eq!(merge["targets"].as_array().unwrap().len(), 2);
    let new = merge["mem"]["id"].as_str().unwrap().to_string();

    let (s, v) = e
        .me(
            "POST",
            "/kb/candidates/resolve",
            Some(json!({"ids": [new], "choice": "yes"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (e.status(&a), e.status(&b), e.status(&new)),
        ("stale".into(), "stale".into(), "confirmed".into())
    );
    assert_eq!(e.changes().await[0]["changes"][0]["state"], "confirmed");

    // undone: both come back, the merge goes
    let id = merge["id"].as_str().unwrap();
    let (s, v) = e.me("POST", &format!("/kb/changes/{id}/undo"), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (e.status(&a), e.status(&b), e.status(&new)),
        ("confirmed".into(), "confirmed".into(), "gone".into())
    );
    assert_eq!(e.changes().await[0]["changes"][0]["state"], "undone");
    let (s, _) = e.me("POST", &format!("/kb/changes/{id}/undo"), None).await;
    assert_eq!(s, 409);

    // a merge still waiting, undone: its candidate is ignored
    e.tidy(json!({"op": "merge", "ids": [a, c], "text": "x", "reason": "r"}))
        .unwrap();
    let ch = e.changes().await;
    let m2 = ch[0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["state"] == "pending")
        .unwrap()
        .clone();
    let (s, _) = e
        .me(
            "POST",
            &format!("/kb/changes/{}/undo", m2["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(e.status(m2["mem"]["id"].as_str().unwrap()), "gone");
    let (_, pending) = e.me("GET", "/kb/candidates", None).await;
    assert_eq!(pending, json!([]));

    // only confirmed memories merge, and the reason is required
    assert!(
        e.tidy(json!({"op": "merge", "ids": [a], "text": "x", "reason": "r"}))
            .is_err()
    );
    assert!(
        e.tidy(json!({"op": "stale", "ids": [a], "reason": ""}))
            .is_err()
    );
    assert!(
        e.tidy(json!({"op": "stale", "ids": ["mm_nope"], "reason": "r"}))
            .unwrap_err()
            .contains("mm_nope")
    );
}

#[tokio::test]
async fn resolve_flag_stale_and_about_are_listed_and_undoable() {
    let e = setup();
    let old = e.add("周报周五发", None).await;
    let new = e.add("周报改成周一发", None).await;
    let x = e.add("用 Jenkins 构建", None).await;
    let y = e.add("Sarah 负责 Acme", None).await;

    // the newer explicit correction holds
    e.tidy(json!({"op": "resolve", "ids": [old, new], "keep": new, "reason": "后来改成了周一"}))
        .unwrap();
    assert_eq!(
        (e.status(&old), e.status(&new)),
        ("stale".into(), "confirmed".into())
    );
    e.tidy(json!({"op": "stale", "ids": [x], "reason": "早就不用 Jenkins 了"}))
        .unwrap();
    e.tidy(json!({"op": "about", "ids": [y], "about": "Sarah Chen", "reason": "说的是 Sarah"}))
        .unwrap();
    assert_eq!(e.mem(&y).unwrap().about.len(), 1);
    e.tidy(json!({"op": "flag", "ids": [new, x], "reason": "看不出哪条对"}))
        .unwrap();

    let ch = e.changes().await;
    let list = ch[0]["changes"].as_array().unwrap();
    let ops: Vec<(&str, &str)> = list
        .iter()
        .map(|c| (c["op"].as_str().unwrap(), c["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        ops,
        vec![
            ("resolve", "applied"),
            ("stale", "applied"),
            ("about", "applied"),
            ("flag", "open")
        ]
    );
    let id = |n: usize| list[n]["id"].as_str().unwrap().to_string();

    for n in 0..3 {
        let (s, v) = e
            .me("POST", &format!("/kb/changes/{}/undo", id(n)), None)
            .await;
        assert_eq!(s, 200, "{v}");
    }
    assert_eq!(
        (e.status(&old), e.status(&x)),
        ("confirmed".into(), "confirmed".into())
    );
    assert!(e.mem(&y).unwrap().about.is_empty());

    // a flag is answered on the page: keep one, the others go stale; undo opens it again
    let (s, _) = e
        .me(
            "POST",
            &format!("/kb/changes/{}/keep", id(3)),
            Some(json!({"keep": new})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        (e.status(&new), e.status(&x)),
        ("confirmed".into(), "stale".into())
    );
    let (s, _) = e
        .me(
            "POST",
            &format!("/kb/changes/{}/keep", id(3)),
            Some(json!({"keep": null})),
        )
        .await;
    assert_eq!(s, 409);
    let (s, _) = e
        .me("POST", &format!("/kb/changes/{}/undo", id(3)), None)
        .await;
    assert_eq!(s, 200);
    assert_eq!(e.status(&x), "confirmed");
    let (s, _) = e
        .me(
            "POST",
            &format!("/kb/changes/{}/keep", id(3)),
            Some(json!({"keep": null})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(e.changes().await[0]["changes"][3]["state"], "dismissed");
}

#[tokio::test]
async fn the_page_remembers_which_schedule_is_the_steward() {
    let e = setup();
    let (_, v) = e.me("GET", "/kb/steward", None).await;
    assert_eq!(v, json!({"schedule_id": null}));
    let (s, _) = e
        .me("PUT", "/kb/steward", Some(json!({"schedule_id": "fl_1"})))
        .await;
    assert_eq!(s, 200);
    let (_, v) = e.me("GET", "/kb/steward", None).await;
    assert_eq!(v["schedule_id"], "fl_1");
}

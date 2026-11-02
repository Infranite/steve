//! Slice P4 acceptance (contacts.md §9): after a merge, attributes, links, memories and team memberships
//! are on `into` with one audit row; a split restores all of it; a later merge makes the split 409;
//! after `not_same` the pair is never proposed again — plus the REST branches, the proposal decision with
//! undo, accounts through the hooks, and the `attr_value` / `attr_def` handlers.

use axum::Router;
use serde_json::{Value, json};
use steve_modules::accounts::{self, Hints, identify};
use steve_modules::contacts::{self, merge};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_modules::{Caller, Registry, decisions};
use steve_store::base::decisions as bd;
use steve_store::{Decision, NewDecision};

fn env() -> TestEnv {
    let t = TestEnv::new(|r: &mut Registry| {
        accounts::register(r);
        merge::register(r, &accounts::ports());
    });
    exec(
        &t,
        "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES
           ('ST-1', 'default', 1, 'task', '报价单', 'run', '定义', 1, 1),
           ('ST-2', 'default', 2, 'task', '交付', 'run', '定义', 2, 2);
         INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES
           ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1), ('tag_b', 'default', 'B', '客户', '', 'usr_dev', 1);",
    );
    t
}

fn app(t: &TestEnv) -> Router {
    contacts::routes(accounts::ports())
        .merge(accounts::routes())
        .with_state(t.modules.clone())
}

fn me() -> Caller {
    caller(WS, ME)
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

async fn ok(r: &Router, method: &str, path: &str, body: Value) -> Value {
    let (s, b) = req(r, &me(), method, path, Some(body)).await;
    assert_eq!(s, 200, "{method} {path}: {b}");
    b
}

fn err(msg: &str) -> Value {
    json!({ "error": msg })
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_string()
}

fn exec(t: &TestEnv, sql: &str) {
    t.store()
        .with_conn(|c| Ok(c.execute_batch(sql)?))
        .expect("exec");
}

fn scalar<T: rusqlite::types::FromSql>(t: &TestEnv, sql: &str) -> T {
    t.store()
        .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .expect("query")
}

fn strings(t: &TestEnv, sql: &str) -> Vec<String> {
    t.store()
        .with_conn(|c| {
            let mut st = c.prepare(sql)?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(v)
        })
        .expect("query")
}

fn decision(t: &TestEnv, id: &str) -> Decision {
    t.store()
        .with_conn(|c| bd::get(c, id))
        .expect("get")
        .expect("decision")
}

/// What one contact has, in a comparable form.
fn snapshot(t: &TestEnv, p: &str) -> Value {
    let q = |sql: &str| strings(t, &sql.replace("$P", p));
    json!({
        "attrs": q("SELECT key || '=' || value FROM attr_values WHERE party_id = '$P' ORDER BY 1"),
        "out": q("SELECT predicate || '>' || object_id FROM links WHERE subject_type = 'party' AND subject_id = '$P' ORDER BY 1"),
        "in": q("SELECT subject_id || '>' || predicate FROM links WHERE object_type = 'party' AND object_id = '$P' ORDER BY 1"),
        "mems": q("SELECT mem_id FROM mem_about WHERE object_type = 'party' AND object_id = '$P' ORDER BY 1"),
        "teams": q("SELECT team_id || ':' || role FROM team_members WHERE contact_id = '$P' ORDER BY 1"),
        "tags": q("SELECT tag_id FROM entity_tags WHERE object_type = 'party' AND object_id = '$P' ORDER BY 1"),
        "accounts": q("SELECT id FROM accounts WHERE party_id = '$P' ORDER BY 1"),
    })
}

struct World {
    org: String,
    sarah: String,
    dup: String,
    lei: String,
    team: String,
    mem_dup: String,
    mem_shared: String,
}

/// Sarah (Acme) and her duplicate “sarah.c” with an attribute, a job, an item link, a memory, a tag, a
/// team and an account each — some of them overlapping with Sarah's.
async fn world(r: &Router) -> World {
    let org = id(&ok(
        r,
        "POST",
        "/orgs",
        json!({ "name": "Acme", "domains": ["acme.com"] }),
    )
    .await);
    let globex = id(&ok(r, "POST", "/orgs", json!({ "name": "Globex" })).await);
    let sarah = id(&ok(
        r,
        "POST",
        "/contacts",
        json!({ "name": "Sarah Chen", "org_id": org, "title": "产品经理" }),
    )
    .await);
    let dup = id(&ok(
        r,
        "POST",
        "/contacts",
        json!({ "name": "sarah.c", "org_id": org }),
    )
    .await);
    let lei = id(&ok(r, "POST", "/contacts", json!({ "name": "李雷" })).await);
    ok(
        r,
        "POST",
        &format!("/contacts/{dup}/jobs"),
        json!({ "org_id": globex, "title": "顾问" }),
    )
    .await;
    ok(
        r,
        "POST",
        &format!("/parties/{sarah}/attrs"),
        json!({ "key": "phone", "value": "+1 111" }),
    )
    .await;
    ok(
        r,
        "POST",
        &format!("/parties/{dup}/attrs"),
        json!({ "key": "phone", "value": "+1 111" }),
    )
    .await;
    ok(
        r,
        "POST",
        &format!("/parties/{dup}/attrs"),
        json!({ "key": "tz", "value": "PST" }),
    )
    .await;
    ok(
        r,
        "POST",
        "/items/ST-1/people",
        json!({ "party_id": dup, "role": "related" }),
    )
    .await;
    ok(
        r,
        "POST",
        "/items/ST-2/people",
        json!({ "party_id": dup, "role": "waiting_on" }),
    )
    .await;
    ok(
        r,
        "POST",
        "/items/ST-2/people",
        json!({ "party_id": sarah, "role": "waiting_on" }),
    )
    .await;
    let mem_dup = id(&ok(
        r,
        "POST",
        &format!("/parties/{dup}/memories"),
        json!({ "text": "喜欢简短的邮件" }),
    )
    .await);
    let mem_shared = id(&ok(
        r,
        "POST",
        &format!("/parties/{dup}/memories"),
        json!({ "text": "两人一起开会" }),
    )
    .await);
    ok(
        r,
        "PATCH",
        &format!("/contacts/{sarah}"),
        json!({ "tags": ["tag_a"] }),
    )
    .await;
    ok(
        r,
        "PATCH",
        &format!("/contacts/{dup}"),
        json!({ "tags": ["tag_a", "tag_b"] }),
    )
    .await;
    let team = id(&ok(r, "POST", "/teams", json!({ "name": "交付小组", "members": [{ "contact_id": dup, "role": "lead" }, { "contact_id": lei }] })).await);
    ok(
        r,
        "POST",
        "/teams",
        json!({ "name": "两个都在", "members": [{ "contact_id": dup }, { "contact_id": sarah }] }),
    )
    .await;
    ok(r, "POST", "/accounts", json!({ "system_id": "mail", "party_id": dup, "kind": "work_email", "handle": "sarah.c@gmail.com" })).await;
    ok(r, "POST", "/accounts", json!({ "system_id": "mail", "party_id": sarah, "kind": "work_email", "handle": "sarah@acme.com" })).await;
    World {
        org,
        sarah,
        dup,
        lei,
        team,
        mem_dup,
        mem_shared,
    }
}

fn share_memory(t: &TestEnv, w: &World) {
    exec(
        t,
        &format!(
            "INSERT INTO mem_about (mem_id, workspace_id, object_type, object_id) VALUES ('{}', 'default', 'party', '{}')",
            w.mem_shared, w.sarah
        ),
    );
}

#[tokio::test]
async fn merge_moves_everything_and_split_restores_it() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    share_memory(&t, &w);
    let before_dup = snapshot(&t, &w.dup);
    let before_sarah = snapshot(&t, &w.sarah);

    let b = ok(
        &r,
        "POST",
        &format!("/contacts/{}/merge", w.dup),
        json!({ "into": w.sarah }),
    )
    .await;
    let mg = b["merge_id"].as_str().unwrap().to_string();
    assert_eq!(b["into"]["id"], json!(w.sarah));
    assert_eq!(
        b["into"]["merges"],
        json!([{ "id": mg, "from_name": "sarah.c", "created": b["into"]["merges"][0]["created"], "splittable": true }])
    );

    let s = snapshot(&t, &w.sarah);
    assert_eq!(
        s["attrs"],
        json!(["phone=+1 111", "tz=PST"]),
        "duplicate phone not doubled"
    );
    let out = s["out"].as_array().unwrap();
    assert!(out.contains(&json!(format!("works_at>{}", w.org))));
    assert_eq!(out.len(), 2, "Acme once, Globex moved: {out:?}");
    assert_eq!(s["in"], json!(["ST-1>related", "ST-2>waiting_on"]));
    let mut mems = vec![w.mem_dup.clone(), w.mem_shared.clone()];
    mems.sort();
    assert_eq!(s["mems"], json!(mems));
    assert_eq!(s["tags"], json!(["tag_a", "tag_b"]));
    assert_eq!(s["teams"].as_array().unwrap().len(), 2);
    assert!(
        s["teams"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("{}:lead", w.team)))
    );
    assert_eq!(s["accounts"].as_array().unwrap().len(), 2);

    let d = snapshot(&t, &w.dup);
    assert_eq!(
        d["attrs"],
        json!(["phone=+1 111"]),
        "exact duplicate stays behind"
    );
    for k in ["out", "in", "mems", "teams", "tags", "accounts"] {
        assert_eq!(d[k], json!([]), "{k}");
    }
    let (status, into): (String, String) = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status, merged_into FROM parties WHERE id = ?",
                [&w.dup],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (status.as_str(), into.as_str()),
        ("merged", w.sarah.as_str())
    );
    assert_eq!(
        scalar::<i64>(
            &t,
            "SELECT COUNT(*) FROM audit WHERE action = 'party.merge'"
        ),
        1
    );
    let moved: Value =
        serde_json::from_str(&scalar::<String>(&t, "SELECT moved FROM merges")).unwrap();
    for k in ["attrs", "links", "mems", "teams", "hooks"] {
        assert!(moved.get(k).is_some(), "moved.{k}");
    }
    assert_eq!(
        moved["hooks"]["vault"]["accounts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // the merged contact is gone from lists, and a second merge of it 409s
    let (_, list) = req(&r, &me(), "GET", "/contacts", None).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"] != json!(w.dup))
    );
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        &format!("/contacts/{}/merge", w.dup),
        Some(json!({ "into": w.lei })),
    )
    .await;
    assert_eq!((s, b), (409, err("这个联系人已经合并过了")));

    let (s, b) = req(&r, &me(), "POST", &format!("/merges/{mg}/split"), None).await;
    assert_eq!((s, b), (200, json!({ "ok": true })));
    assert_eq!(snapshot(&t, &w.dup), before_dup);
    assert_eq!(snapshot(&t, &w.sarah), before_sarah);
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM parties WHERE id = '{}'", w.dup)
        ),
        "active"
    );
    assert_eq!(
        scalar::<Option<String>>(
            &t,
            &format!("SELECT merged_into FROM parties WHERE id = '{}'", w.dup)
        ),
        None
    );
    assert_eq!(
        scalar::<i64>(
            &t,
            "SELECT COUNT(*) FROM audit WHERE action = 'party.split'"
        ),
        1
    );
    let (s, b) = req(&r, &me(), "POST", &format!("/merges/{mg}/split"), None).await;
    assert_eq!((s, b), (409, err("这次合并已经拆开了")));
    let (_, c) = req(&r, &me(), "GET", &format!("/contacts/{}", w.sarah), None).await;
    assert_eq!(c["merges"], json!([]));
}

#[tokio::test]
async fn split_leaves_later_manual_changes_alone() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let b = ok(
        &r,
        "POST",
        &format!("/contacts/{}/merge", w.dup),
        json!({ "into": w.sarah }),
    )
    .await;
    let mg = b["merge_id"].as_str().unwrap().to_string();
    // after the merge: remove the moved tz attribute, take the moved team lead role away, add a new phone
    let tz: String = scalar(&t, "SELECT id FROM attr_values WHERE key = 'tz'");
    let (s, _) = req(&r, &me(), "DELETE", &format!("/attrs/{tz}"), None).await;
    assert_eq!(s, 200);
    ok(&r, "PUT", &format!("/teams/{}/members", w.team), json!([{ "contact_id": w.lei, "role": "lead" }, { "contact_id": w.sarah, "role": "member" }])).await;
    ok(
        &r,
        "POST",
        &format!("/parties/{}/attrs", w.sarah),
        json!({ "key": "address", "value": "北京" }),
    )
    .await;

    ok(&r, "POST", &format!("/merges/{mg}/split"), json!(null)).await;
    let d = snapshot(&t, &w.dup);
    assert_eq!(
        d["attrs"],
        json!(["phone=+1 111"]),
        "deleted tz not resurrected"
    );
    assert_eq!(
        d["teams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x.as_str().unwrap().starts_with(&w.team))
            .count(),
        1
    );
    assert!(
        d["teams"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("{}:member", w.team))),
        "current role kept: {d}"
    );
    assert_eq!(
        snapshot(&t, &w.sarah)["attrs"],
        json!(["address=北京", "phone=+1 111"]),
        "new attribute stays"
    );
}

#[tokio::test]
async fn only_the_latest_merge_splits() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let first = ok(
        &r,
        "POST",
        &format!("/contacts/{}/merge", w.dup),
        json!({ "into": w.sarah }),
    )
    .await["merge_id"]
        .as_str()
        .unwrap()
        .to_string();
    let second = ok(
        &r,
        "POST",
        &format!("/contacts/{}/merge", w.lei),
        json!({ "into": w.sarah }),
    )
    .await["merge_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, c) = req(&r, &me(), "GET", &format!("/contacts/{}", w.sarah), None).await;
    let ms = c["merges"].as_array().unwrap();
    assert_eq!(
        (ms[0]["id"].as_str(), ms[0]["splittable"].as_bool()),
        (Some(second.as_str()), Some(true))
    );
    assert_eq!(
        (ms[1]["id"].as_str(), ms[1]["splittable"].as_bool()),
        (Some(first.as_str()), Some(false))
    );

    let (s, b) = req(&r, &me(), "POST", &format!("/merges/{first}/split"), None).await;
    assert_eq!((s, b), (409, err("先拆开之后的合并")));
    ok(&r, "POST", &format!("/merges/{second}/split"), json!(null)).await;
    ok(&r, "POST", &format!("/merges/{first}/split"), json!(null)).await;

    // a chain: into merged elsewhere → split that first
    let a = ok(
        &r,
        "POST",
        &format!("/contacts/{}/merge", w.dup),
        json!({ "into": w.lei }),
    )
    .await["merge_id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(
        &r,
        "POST",
        &format!("/contacts/{}/merge", w.lei),
        json!({ "into": w.sarah }),
    )
    .await;
    let (s, b) = req(&r, &me(), "POST", &format!("/merges/{a}/split"), None).await;
    assert_eq!((s, b), (409, err("先拆开之后的合并")));
}

#[tokio::test]
async fn merge_rejections() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let post = |from: String, into: Value| {
        let r = r.clone();
        async move {
            req(
                &r,
                &me(),
                "POST",
                &format!("/contacts/{from}/merge"),
                Some(json!({ "into": into })),
            )
            .await
        }
    };
    assert_eq!(
        post(w.sarah.clone(), json!(w.sarah)).await,
        (400, err("不能合并到自己"))
    );
    assert_eq!(
        post("me".into(), json!(w.sarah)).await,
        (400, err("不能把你自己合并到别人"))
    );
    assert_eq!(
        post(w.sarah.clone(), json!("ct_nope")).await,
        (404, err("没有这个联系人"))
    );
    assert_eq!(
        post(w.sarah.clone(), json!(w.org)).await,
        (404, err("没有这个联系人"))
    );
    assert_eq!(post(w.sarah.clone(), json!(3)).await.0, 400);
    // into “me” is fine
    let (s, b) = post(w.lei.clone(), json!("me")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["into"]["is_me"], json!(true));
    // another workspace
    let (s, b) = req(
        &r,
        &caller(WS2, ME),
        "POST",
        &format!("/contacts/{}/merge", w.dup),
        Some(json!({ "into": w.sarah })),
    )
    .await;
    assert_eq!((s, b), (404, err("没有这个联系人")));
    let (s, b) = req(&r, &me(), "POST", "/merges/mg_nope/split", None).await;
    assert_eq!((s, b), (404, err("没有这个合并记录")));
    // someone else's private contact
    let private = id(&ok(
        &r,
        "POST",
        "/contacts",
        json!({ "name": "私人", "visibility": "private" }),
    )
    .await);
    let (s, _) = req(
        &r,
        &caller(WS, OTHER),
        "POST",
        &format!("/contacts/{}/merge", w.dup),
        Some(json!({ "into": private })),
    )
    .await;
    assert_eq!(s, 404);
}

fn propose(t: &TestEnv) -> (String, String, String) {
    let got = t
        .store()
        .transaction(|tx| {
            identify(
                tx,
                WS,
                ME,
                "mail",
                "sarah.c.design@gmail.com",
                &Hints {
                    display_name: Some("Sarah Chen".into()),
                    signature_org: Some("Acme".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    let mp = got.proposal.expect("proposal");
    let d: String = scalar(
        t,
        &format!("SELECT decision_id FROM merge_proposals WHERE id = '{mp}'"),
    );
    (mp, d, got.party_id)
}

fn resolve(t: &TestEnv, d: &str, choice: &str) -> Result<Decision, steve_modules::ApiError> {
    decisions::dispatch(&t.modules, &me(), d, choice, "").expect("ours")
}

/// What steve-core's `undoDecision` runs for a module decision.
fn undo(t: &TestEnv, d: &str) -> Result<(), steve_modules::ApiError> {
    decisions::undo(&t.modules, d, ME, |d| {
        d.status == "resolved" && !d.undo.is_null()
    })
    .expect("ours")
    .map(drop)
}

#[tokio::test]
async fn proposal_merge_through_the_decision_and_undo() {
    let t = env();
    let r = app(&t);
    let sarah = id(&ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Acme", "domains": ["acme.com"] }),
    )
    .await);
    let sarah = id(&ok(
        &r,
        "POST",
        "/contacts",
        json!({ "name": "Sarah Chen", "org_id": sarah }),
    )
    .await);
    let (mp, d, from) = propose(&t);

    let (_, pending) = req(&r, &me(), "GET", "/merges", None).await;
    let ps = pending["proposals"].as_array().unwrap();
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0]["id"], json!(mp));
    assert_eq!(ps[0]["from"]["id"], json!(from));
    assert_eq!(ps[0]["into"]["id"], json!(sarah));
    assert_eq!(
        ps[0]["account"]["handle"],
        json!("sarah.c.design@gmail.com")
    );
    assert_eq!(ps[0]["account"]["system"], json!("mail"));
    assert_eq!(ps[0]["decision_id"], json!(d));
    assert_eq!(
        pending["unconfirmed"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["id"] == json!(from))
            .count(),
        1
    );

    let got = resolve(&t, &d, "merge").unwrap();
    assert_eq!(got.status, "resolved");
    assert!(got.result["merge_id"].as_str().unwrap().starts_with("mg_"));
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM merge_proposals WHERE id = '{mp}'")
        ),
        "merged"
    );
    assert_eq!(
        scalar::<String>(
            &t,
            "SELECT party_id FROM accounts WHERE handle = 'sarah.c.design@gmail.com'"
        ),
        sarah
    );
    let (_, pending) = req(&r, &me(), "GET", "/merges", None).await;
    assert_eq!(pending["proposals"], json!([]));
    assert!(resolve(&t, &d, "merge").is_err(), "already handled");

    undo(&t, &d).unwrap();
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM merge_proposals WHERE id = '{mp}'")
        ),
        "open"
    );
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM parties WHERE id = '{from}'")
        ),
        "active"
    );
    assert_eq!(
        scalar::<String>(
            &t,
            "SELECT party_id FROM accounts WHERE handle = 'sarah.c.design@gmail.com'"
        ),
        from
    );
    assert_eq!(decision(&t, &d).status, "open");
}

#[tokio::test]
async fn not_same_is_never_proposed_again() {
    let t = env();
    let r = app(&t);
    let org = id(&ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Acme", "domains": ["acme.com"] }),
    )
    .await);
    ok(
        &r,
        "POST",
        "/contacts",
        json!({ "name": "Sarah Chen", "org_id": org }),
    )
    .await;
    let (mp, d, _) = propose(&t);
    let got = resolve(&t, &d, "not_same").unwrap();
    assert_eq!(got.result, json!({ "choice": "not_same" }));
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM merge_proposals WHERE id = '{mp}'")
        ),
        "rejected"
    );
    assert!(resolve(&t, &d, "bogus").is_err());

    // the same pair again (a second handle of the same unconfirmed contact) → no new proposal
    let n = scalar::<i64>(&t, "SELECT COUNT(*) FROM merge_proposals");
    let again = t
        .store()
        .transaction(|tx| {
            identify(
                tx,
                WS,
                ME,
                "mail",
                "sarah.c.design@gmail.com",
                &Hints {
                    display_name: Some("Sarah Chen".into()),
                    signature_org: Some("Acme".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert!(again.proposal.is_none());
    assert_eq!(scalar::<i64>(&t, "SELECT COUNT(*) FROM merge_proposals"), n);
    assert_eq!(
        scalar::<i64>(
            &t,
            "SELECT COUNT(*) FROM merge_proposals WHERE status = 'open'"
        ),
        0
    );
}

#[tokio::test]
async fn manual_merge_settles_the_open_proposal() {
    let t = env();
    let r = app(&t);
    let org = id(&ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Acme", "domains": ["acme.com"] }),
    )
    .await);
    let sarah = id(&ok(
        &r,
        "POST",
        "/contacts",
        json!({ "name": "Sarah Chen", "org_id": org }),
    )
    .await);
    let (mp, d, from) = propose(&t);
    ok(
        &r,
        "POST",
        &format!("/contacts/{from}/merge"),
        json!({ "into": sarah }),
    )
    .await;
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM merge_proposals WHERE id = '{mp}'")
        ),
        "merged"
    );
    let d = decision(&t, &d);
    assert_eq!(
        (d.status.as_str(), d.result.clone()),
        ("resolved", json!({ "choice": "merge" }))
    );
}

fn open_confirm(t: &TestEnv, subject_type: &str, subject_id: &str) -> String {
    let n: NewDecision = decisions::confirm(WS, subject_type, subject_id, "确认？", "steve");
    t.store()
        .transaction(|tx| Ok::<_, steve_store::StoreError>(bd::open(tx, &n)?.id))
        .unwrap()
}

#[tokio::test]
async fn attr_value_candidates() {
    let t = env();
    let r = app(&t);
    let sarah = id(&ok(&r, "POST", "/contacts", json!({ "name": "Sarah" })).await);
    ok(
        &r,
        "POST",
        &format!("/parties/{sarah}/attrs"),
        json!({ "key": "phone", "value": "+1 old" }),
    )
    .await;
    let old: String = scalar(&t, "SELECT id FROM attr_values");
    let cand = |v: &str, id: &str| {
        exec(
            &t,
            &format!(
                "INSERT INTO attr_values (id, workspace_id, party_id, key, label, value, source, source_label, level, status, created, updated) VALUES ('{id}', 'default', '{sarah}', 'phone', '电话', '{v}', 'steve', '邮件签名', 'reasoning', 'candidate', 2, 2)"
            ),
        );
    };
    cand("+1 new", "av_c1");
    let d = open_confirm(&t, "attr_value", "av_c1");
    resolve(&t, &d, "yes").unwrap();
    assert_eq!(
        scalar::<String>(&t, "SELECT status FROM attr_values WHERE id = 'av_c1'"),
        "confirmed"
    );
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM attr_values WHERE id = '{old}'")
        ),
        "stale"
    );
    undo(&t, &d).unwrap();
    assert_eq!(
        scalar::<String>(&t, "SELECT status FROM attr_values WHERE id = 'av_c1'"),
        "candidate"
    );
    assert_eq!(
        scalar::<String>(
            &t,
            &format!("SELECT status FROM attr_values WHERE id = '{old}'")
        ),
        "confirmed"
    );

    resolve(&t, &d, "no").unwrap();
    assert_eq!(
        scalar::<i64>(&t, "SELECT COUNT(*) FROM attr_values WHERE id = 'av_c1'"),
        0
    );
    undo(&t, &d).unwrap();
    assert_eq!(
        scalar::<String>(
            &t,
            "SELECT value || '/' || status || '/' || source_label FROM attr_values WHERE id = 'av_c1'"
        ),
        "+1 new/candidate/邮件签名"
    );

    cand("+1 x", "av_c2");
    let d2 = open_confirm(&t, "attr_value", "av_c2");
    assert!(decisions::expire(&t.modules, &d2).unwrap());
    assert_eq!(
        scalar::<i64>(&t, "SELECT COUNT(*) FROM attr_values WHERE id = 'av_c2'"),
        0
    );
}

#[tokio::test]
async fn attr_def_proposals() {
    let t = env();
    exec(
        &t,
        "INSERT INTO attr_defs (id, workspace_id, applies, key, label, type, default_level, source, status, created) VALUES ('ad_p', 'default', 'contact', 'wechat', '微信', 'text', 'reasoning', 'steve', 'proposed', 1)",
    );
    let d = open_confirm(&t, "attr_def", "ad_p");
    resolve(&t, &d, "yes").unwrap();
    assert_eq!(
        scalar::<String>(&t, "SELECT status FROM attr_defs WHERE id = 'ad_p'"),
        "active"
    );
    undo(&t, &d).unwrap();
    assert_eq!(
        scalar::<String>(&t, "SELECT status FROM attr_defs WHERE id = 'ad_p'"),
        "proposed"
    );
    resolve(&t, &d, "no").unwrap();
    assert_eq!(
        scalar::<i64>(&t, "SELECT COUNT(*) FROM attr_defs WHERE id = 'ad_p'"),
        0
    );
    undo(&t, &d).unwrap();
    assert_eq!(
        scalar::<String>(
            &t,
            "SELECT key || '/' || label || '/' || status FROM attr_defs WHERE id = 'ad_p'"
        ),
        "wechat/微信/proposed"
    );
}

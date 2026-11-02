//! Ownership (steve-rs-plan/modules/ownership.md): the defaults when “我” is created, the REST of an
//! object's stewards with undo through the change records, a person's holdings and the unowned list,
//! merges and deletes moving the rows, bot parties from identification, the sender card, and the MCP
//! tools with a chat proposal confirmed and undone through the decision.

use axum::Router;
use serde_json::{Value, json};
use steve_modules::accounts::{self, Hints, identify};
use steve_modules::changes as records;
use steve_modules::contacts::mcp::{self, McpCx};
use steve_modules::contacts::{self, changes, merge};
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::{Caller, Registry, decisions};
use steve_store::base::Viewer;

fn env() -> TestEnv {
    let t = TestEnv::new(|r: &mut Registry| {
        accounts::register(r);
        changes::register(r, &accounts::ports());
        merge::register(r, &accounts::ports());
    });
    exec(
        &t,
        "INSERT INTO bots (id, workspace_id, name, main, harness, updated) VALUES ('steve', 'default', 'Steve', 1, 'claude-code', 1), ('old', 'default', '旧的', 0, 'claude-code', 1);
         UPDATE bots SET deleted = 1 WHERE id = 'old';
         INSERT INTO personas (id, workspace_id, name, bot_id, created, updated) VALUES ('per_s', 'default', 'Steve', 'steve', 1, 1);
         INSERT OR IGNORE INTO devices (id, name, roles, seen, created) VALUES ('hub', 'Mac mini', '[\"Hub\"]', 1, 1);
         INSERT OR IGNORE INTO devices (id, name, roles, seen, created) VALUES ('dev_laptop', '笔记本', '[]', 1, 1);",
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

async fn req(r: &Router, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call(r, Some(&me()), method, path, body.as_ref()).await
}

async fn ok(r: &Router, method: &str, path: &str, body: Option<Value>) -> Value {
    let (s, b) = req(r, method, path, body).await;
    assert_eq!(s, 200, "{method} {path}: {b}");
    b
}

fn exec(t: &TestEnv, sql: &str) {
    t.store()
        .with_conn(|c| Ok(c.execute_batch(sql)?))
        .expect("exec");
}

fn rows(t: &TestEnv) -> Vec<String> {
    t.store()
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT object_type || ' ' || object_id || ' ' || person_id || ' ' || role || ' ' || source || ' ' || status FROM stewards ORDER BY 1",
            )?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(v)
        })
        .expect("rows")
}

fn owners(v: &Value) -> Vec<String> {
    v["stewards"]
        .as_array()
        .expect("stewards")
        .iter()
        .map(|s| s["person_name"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn cx() -> McpCx {
    McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "steve".into(),
        item_id: Some("ST-1".into()),
        session_id: None,
    }
}

fn tool(t: &TestEnv, name: &str, args: Value) -> Result<String, String> {
    mcp::call_sync(&t.modules, &accounts::ports(), &cx(), name, &args)
}

async fn setup(r: &Router) -> (String, String) {
    let me = ok(r, "GET", "/me/contact", None).await["id"]
        .as_str()
        .expect("me")
        .to_string();
    let zhang = ok(r, "POST", "/contacts", Some(json!({ "name": "张三" }))).await["id"]
        .as_str()
        .expect("id")
        .to_string();
    (me, zhang)
}

#[tokio::test]
async fn me_gets_the_bots_personas_and_hub_device_and_edits_undo() {
    let t = env();
    let r = app(&t);
    let (me, zhang) = setup(&r).await;
    assert_eq!(
        rows(&t),
        vec![
            format!("bot steve {me} owner seed confirmed"),
            format!("device hub {me} owner seed confirmed"),
            format!("persona per_s {me} owner seed confirmed"),
        ]
    );

    // the chip: set 张三 as the 负责人, replacing “我”
    let v = ok(
        &r,
        "PUT",
        "/stewards/bot/steve",
        Some(json!({ "person_id": zhang })),
    )
    .await;
    assert_eq!(owners(&v), vec!["张三"]);
    assert_eq!(v["object"]["name"], "Steve");
    let undo = v["undo"].as_str().expect("undo").to_string();
    let cs: (String, String) = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status, changes FROM changesets WHERE id = ?",
                [&undo],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .expect("changeset");
    assert_eq!(cs.0, "applied");
    assert!(
        cs.1.contains("「Steve」的负责人") && cs.1.contains("张三"),
        "{}",
        cs.1
    );
    // setting the same again records nothing
    let v = ok(
        &r,
        "PUT",
        "/stewards/bot/steve",
        Some(json!({ "person_id": zhang })),
    )
    .await;
    assert!(v["undo"].is_null(), "{v}");

    // 张三's holdings
    let h = ok(&r, "GET", &format!("/people/{zhang}/holdings"), None).await;
    assert_eq!(h["responsible"][0]["object"]["object_type"], "bot");
    assert_eq!(h["responsible"][0]["role_label"], "负责人");

    // undo through the change record puts “我” back
    records::undo(&t.modules, &caller(WS, ME), &undo)
        .expect("ours")
        .expect("undone");
    let v = ok(&r, "GET", "/stewards/bot/steve", None).await;
    assert_eq!(owners(&v), vec!["Dev"]);

    // remove: the bot shows up as unowned; undo brings the owner back
    let v = ok(&r, "DELETE", "/stewards/bot/steve", None).await;
    assert!(owners(&v).is_empty());
    let u = ok(&r, "GET", "/stewards/unowned", None).await;
    let unowned: Vec<String> = u["objects"]
        .as_array()
        .expect("objects")
        .iter()
        .map(|o| {
            format!(
                "{} {}",
                o["object_type"].as_str().unwrap(),
                o["object_id"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(unowned, vec!["bot steve", "device dev_laptop"]);
    records::undo(
        &t.modules,
        &caller(WS, ME),
        v["undo"].as_str().expect("undo"),
    )
    .expect("ours")
    .expect("undone");
    assert!(rows(&t).contains(&format!("bot steve {me} owner seed confirmed")));

    // a second role keeps the owner
    let v = ok(
        &r,
        "PUT",
        "/stewards/device/hub",
        Some(json!({ "person_id": zhang, "role": "maintainer" })),
    )
    .await;
    assert_eq!(owners(&v), vec!["Dev", "张三"]);
}

#[tokio::test]
async fn rest_errors() {
    let t = env();
    let r = app(&t);
    let (_, zhang) = setup(&r).await;
    let (s, b) = req(&r, "GET", "/stewards/bot/nope", None).await;
    assert_eq!((s, b), (404, json!({ "error": "没有这个 Bot" })));
    let (s, _) = req(&r, "GET", "/stewards/old/steve", None).await;
    assert_eq!(s, 400);
    // a deleted bot is gone
    let (s, _) = req(&r, "GET", "/stewards/bot/old", None).await;
    assert_eq!(s, 404);
    let (s, _) = req(
        &r,
        "PUT",
        "/stewards/bot/steve",
        Some(json!({ "person_id": "ct_nobody" })),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = req(
        &r,
        "PUT",
        "/stewards/bot/steve",
        Some(json!({ "person_id": zhang, "role": "boss" })),
    )
    .await;
    assert_eq!(s, 400);
    // a bot cannot be responsible
    let bot = ok(
        &r,
        "POST",
        "/contacts",
        Some(json!({ "name": "告警小助手", "kind": "bot" })),
    )
    .await;
    let (s, b) = req(
        &r,
        "PUT",
        "/stewards/bot/steve",
        Some(json!({ "person_id": bot["id"] })),
    )
    .await;
    assert_eq!((s, b), (400, json!({ "error": "机器人不能做负责人" })));
}

#[tokio::test]
async fn merge_moves_rows_and_split_restores_them_delete_forgets() {
    let t = env();
    let r = app(&t);
    let (_, zhang) = setup(&r).await;
    let zhang2 = ok(
        &r,
        "POST",
        "/contacts",
        Some(json!({ "name": "张三（旧）" })),
    )
    .await["id"]
        .as_str()
        .expect("id")
        .to_string();
    ok(
        &r,
        "PUT",
        "/stewards/persona/per_s",
        Some(json!({ "person_id": zhang2, "replace": false })),
    )
    .await;
    ok(
        &r,
        "PUT",
        "/stewards/device/hub",
        Some(json!({ "person_id": zhang2, "replace": false })),
    )
    .await;
    ok(
        &r,
        "PUT",
        "/stewards/device/hub",
        Some(json!({ "person_id": zhang, "replace": false })),
    )
    .await;

    let m = ok(
        &r,
        "POST",
        &format!("/contacts/{zhang2}/merge"),
        Some(json!({ "into": zhang })),
    )
    .await;
    let rs = rows(&t);
    assert!(
        rs.contains(&format!("persona per_s {zhang} owner manual confirmed")),
        "{rs:?}"
    );
    assert!(!rs.iter().any(|x| x.contains(&zhang2)), "{rs:?}");
    let mg = m["merge_id"]
        .as_str()
        .or(m["id"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            t.store()
                .with_conn(|c| Ok(c.query_row("SELECT id FROM merges", [], |r| r.get(0))?))
                .expect("merge")
        });
    ok(&r, "POST", &format!("/merges/{mg}/split"), None).await;
    let rs = rows(&t);
    assert!(
        rs.contains(&format!("persona per_s {zhang2} owner manual confirmed")),
        "{rs:?}"
    );
    assert!(
        rs.contains(&format!("device hub {zhang2} owner manual confirmed")),
        "{rs:?}"
    );
    assert!(
        rs.contains(&format!("device hub {zhang} owner manual confirmed")),
        "{rs:?}"
    );

    ok(&r, "DELETE", &format!("/contacts/{zhang2}"), None).await;
    assert!(!rows(&t).iter().any(|x| x.contains(&zhang2)));
}

#[tokio::test]
async fn bots_seen_in_chat_cards_tools_and_chat_proposals() {
    let t = env();
    let r = app(&t);
    let (_, zhang) = setup(&r).await;
    exec(
        &t,
        "INSERT OR IGNORE INTO systems (id, workspace_id, name, kind, family, created) VALUES ('feishu', '*', '飞书', 'comm', 'feishu_user', 1);",
    );
    // another bot in a group: its own bot party, nobody's
    let got = t
        .store()
        .transaction(|tx| {
            identify(
                tx,
                WS,
                ME,
                "feishu",
                "cli_alarm",
                &Hints {
                    display_name: Some("告警小助手".into()),
                    kind: Some("bot".into()),
                    ..Hints::default()
                },
            )
        })
        .expect("identify");
    assert!(got.created && !got.is_ours && got.proposal.is_none());
    let bot = got.party_id.clone();
    let kind: String = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT contact_kind FROM parties WHERE id = ?",
                [&bot],
                |r| r.get(0),
            )?)
        })
        .expect("kind");
    assert_eq!(kind, "bot");
    let u = ok(&r, "GET", "/stewards/unowned", None).await;
    assert!(u.to_string().contains("告警小助手"), "{u}");

    let card = |t: &TestEnv| {
        t.store()
            .with_conn(|c| {
                let v = Viewer::load(c, WS, ME)?;
                Ok(contacts::contact_cards(c, &v, std::slice::from_ref(&bot)).expect("cards"))
            })
            .expect("card")[0]
            .text
            .clone()
    };
    assert!(
        card(&t).starts_with("告警小助手（机器人；负责人未知"),
        "{}",
        card(&t)
    );

    // chat: “这是张三的机器人” → a proposal and its card
    let out = tool(
        &t,
        "steve_steward_propose",
        json!({ "object": "告警小助手", "person": "张三", "basis": "群里说这是张三的机器人" }),
    )
    .expect("propose");
    assert_eq!(out, "Saved as a proposal; the user will confirm it.");
    assert!(rows(&t).contains(&format!("party {bot} {zhang} owner chat proposed")));
    let again = tool(
        &t,
        "steve_steward_propose",
        json!({ "object": bot, "person": zhang }),
    )
    .expect("again");
    assert!(again.starts_with("Already proposed"), "{again}");
    let d: String = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT id FROM decisions WHERE subject_type = 'steward' AND status = 'open'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("decision");
    decisions::dispatch(&t.modules, &me(), &d, "yes", "")
        .expect("ours")
        .expect("resolved");
    assert!(rows(&t).contains(&format!("party {bot} {zhang} owner chat confirmed")));
    assert!(card(&t).contains("负责人 张三"), "{}", card(&t));

    // whois: account → bot party → 负责人
    let w = tool(&t, "steve_whois", json!({ "query": "cli_alarm" })).expect("whois");
    assert!(
        w.contains("告警小助手")
            && w.contains("Identity of: 告警小助手 (a bot")
            && w.contains("负责人 (owner): 张三"),
        "{w}"
    );
    let w = tool(&t, "steve_whois", json!({ "query": "Mac mini" })).expect("whois");
    assert!(
        w.contains("设备「Mac mini」") && w.contains("负责人 (owner): Dev"),
        "{w}"
    );
    let w = tool(&t, "steve_whois", json!({ "query": "nothing-like-this" })).expect("whois");
    assert!(w.starts_with("No account"), "{w}");

    // holdings: 张三 is responsible for the bot; “我” for Steve's things
    let h = tool(&t, "steve_person_holdings", json!({ "person": "张三" })).expect("holdings");
    assert!(
        h.contains("Responsible for:") && h.contains("机器人「告警小助手」"),
        "{h}"
    );
    let h = tool(&t, "steve_person_holdings", json!({ "person": "我" })).expect("holdings");
    assert!(
        h.contains("Bot「Steve」") && h.contains("设备「Mac mini」"),
        "{h}"
    );
    let h = tool(
        &t,
        "steve_person_holdings",
        json!({ "person": "告警小助手" }),
    )
    .expect("holdings");
    assert!(h.contains("负责人: 张三"), "{h}");
    // contacts_find mentions it too
    let f = tool(&t, "steve_contacts_find", json!({ "query": "张三" })).expect("find");
    assert!(
        f.contains("Responsible for: 机器人「告警小助手」(负责人)"),
        "{f}"
    );

    // undoing the answer makes it a proposal again; “no” drops it, undo puts it back
    decisions::undo(&t.modules, &d, ME, |d| {
        d.status == "resolved" && !d.undo.is_null()
    })
    .expect("ours")
    .expect("undone");
    assert!(rows(&t).contains(&format!("party {bot} {zhang} owner chat proposed")));
    decisions::dispatch(&t.modules, &me(), &d, "no", "")
        .expect("ours")
        .expect("resolved");
    assert!(!rows(&t).iter().any(|x| x.contains(&bot)));
    decisions::undo(&t.modules, &d, ME, |d| {
        d.status == "resolved" && !d.undo.is_null()
    })
    .expect("ours")
    .expect("undone");
    assert!(rows(&t).contains(&format!("party {bot} {zhang} owner chat proposed")));

    // bad arguments
    let e = tool(
        &t,
        "steve_steward_propose",
        json!({ "object": "nothing", "person": "张三" }),
    )
    .unwrap_err();
    assert!(e.contains("Nothing matches"), "{e}");
    let e = tool(
        &t,
        "steve_steward_propose",
        json!({ "object": "Steve", "person": "告警小助手" }),
    )
    .unwrap_err();
    assert!(e.contains("Several match") || e.contains("is a bot"), "{e}");
    let e = tool(
        &t,
        "steve_steward_propose",
        json!({ "object": "steve", "person": "张三", "role": "boss" }),
    )
    .unwrap_err();
    assert!(e.contains("Invalid option"), "{e}");
}

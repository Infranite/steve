//! Slice A1 acceptance: the four identification paths of §2.3 (Feishu
//! union_id/open_id, the prefix rule excluding `noreply`, same name + same organization giving a
//! proposal), handle conflicts 409, accounts following a merge and coming back on split — plus the REST
//! branches, the contacts ports and the change operations. Tables from migration 5 (steve-store).

use axum::Router;
use serde_json::{Value, json};
use steve_modules::accounts::{self, AccountHooks, ExtraHandle, Hints, Identified, identify};
use steve_modules::changes::{self, Origin, Plan, Prepared};
use steve_modules::contacts::{self, PartyHooks};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_modules::{ApiError, Caller};
use steve_store::ChangeEvent;

fn env() -> TestEnv {
    TestEnv::new(accounts::register)
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

fn scalar<T: rusqlite::types::FromSql>(t: &TestEnv, sql: &str) -> T {
    t.store()
        .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .expect("query")
}

fn exec(t: &TestEnv, sql: &str) {
    t.store()
        .with_conn(|c| Ok(c.execute_batch(sql)?))
        .expect("exec");
}

fn ident(t: &TestEnv, system: &str, handle: &str, hints: &Hints) -> Result<Identified, ApiError> {
    t.store()
        .transaction(|tx| identify(tx, WS, ME, system, handle, hints))
}

fn named(n: &str) -> Hints {
    Hints {
        display_name: Some(n.into()),
        ..Default::default()
    }
}

/// Acme (acme.com, acme.io) with Sarah Chen working there and owning sarah@acme.com.
struct Acme {
    org: String,
    sarah: String,
    sarah_mail: String,
}

async fn acme(r: &Router) -> Acme {
    let org = id(&ok(r, "POST", "/orgs", json!({ "name": "Acme", "kind": "customer", "domains": ["acme.com", "acme.io"], "aliases": ["Acme Inc."] })).await);
    let sarah = id(&ok(
        r,
        "POST",
        "/contacts",
        json!({ "name": "Sarah Chen", "aliases": ["Sarah"], "org_id": org, "title": "产品经理" }),
    )
    .await);
    let a = ok(r, "POST", "/accounts", json!({ "system_id": "mail", "party_id": sarah, "kind": "work_email", "handle": "sarah@acme.com" })).await;
    Acme {
        org,
        sarah,
        sarah_mail: id(&a),
    }
}

// ---------------------------------------------------------------- §2.3 step 1: exact handles

#[tokio::test]
async fn exact_handle_finds_the_owner_and_follows_merges() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let got = ident(&t, "mail", "  Sarah@ACME.com ", &named("whoever")).unwrap();
    assert_eq!(
        got,
        Identified {
            account_id: a.sarah_mail.clone(),
            party_id: a.sarah.clone(),
            is_ours: false,
            created: false,
            proposal: None
        }
    );
    // a merged owner resolves to the contact it was merged into
    let tom = id(&ok(&r, "POST", "/contacts", json!({ "name": "Tom" })).await);
    exec(
        &t,
        &format!(
            "UPDATE parties SET status = 'merged', merged_into = '{tom}' WHERE id = '{}'",
            a.sarah
        ),
    );
    let got = ident(&t, "gmail", "sarah@acme.com", &Hints::default()).unwrap();
    assert_eq!((got.party_id.as_str(), got.created), (tom.as_str(), false));
}

#[tokio::test]
async fn own_organization_accounts_are_ours() {
    let t = env();
    let r = app(&t);
    ok(&r, "GET", "/me/contact", Value::Null).await;
    let org: String = scalar(&t, "SELECT id FROM parties WHERE org_kind = 'self'");
    ok(&r, "POST", "/accounts", json!({ "system_id": "feishu_app", "party_id": org, "kind": "app_bot", "handle": "cli_a5f", "display_name": "值班小运" })).await;
    let got = ident(&t, "feishu_app", "cli_a5f", &Hints::default()).unwrap();
    assert!(got.is_ours && !got.created);
    assert_eq!(got.party_id, org);
}

#[tokio::test]
async fn feishu_union_id_and_open_ids() {
    let t = env();
    let hints = Hints {
        display_name: Some("李雷".into()),
        extra: vec![ExtraHandle {
            handle: "ou_app1".into(),
            connection_id: Some("con_app1".into()),
        }],
        source_label: Some("Steve 开发群".into()),
        ..Default::default()
    };
    let first = ident(&t, "feishu", "on_union1", &hints).unwrap();
    assert!(first.created && !first.is_ours);
    let rows: i64 = scalar(
        &t,
        &format!(
            "SELECT COUNT(*) FROM account_handles WHERE account_id = '{}' AND family = 'feishu_user'",
            first.account_id
        ),
    );
    assert_eq!(rows, 2);
    let conn: String = scalar(
        &t,
        "SELECT connection_id FROM account_handles WHERE handle = 'ou_app1'",
    );
    assert_eq!(conn, "con_app1");
    let primary: String = scalar(
        &t,
        &format!(
            "SELECT handle FROM accounts WHERE id = '{}'",
            first.account_id
        ),
    );
    assert_eq!(primary, "on_union1");

    // a message under that app carries only the open_id
    let again = ident(&t, "feishu", "ou_app1", &Hints::default()).unwrap();
    assert_eq!(
        (again.account_id.as_str(), again.created),
        (first.account_id.as_str(), false)
    );

    // another app of the same developer: same union_id, a new open_id is remembered with it
    let other_app = Hints {
        extra: vec![ExtraHandle {
            handle: "ou_app2".into(),
            connection_id: Some("con_app2".into()),
        }],
        ..Default::default()
    };
    let third = ident(&t, "feishu", "on_union1", &other_app).unwrap();
    assert_eq!(third.account_id, first.account_id);
    let via2 = ident(&t, "feishu", "ou_app2", &Hints::default()).unwrap();
    assert_eq!(via2.account_id, first.account_id);
    // only open_id known and it was seen before through the union_id → same account; nothing new
    let n: i64 = scalar(&t, "SELECT COUNT(*) FROM accounts");
    assert_eq!(n, 1);
    let kind: String = scalar(
        &t,
        &format!(
            "SELECT contact_kind FROM parties WHERE id = '{}'",
            first.party_id
        ),
    );
    assert_eq!(kind, "unconfirmed");
    let label: String = scalar(&t, "SELECT source_label FROM accounts");
    assert_eq!(label, "Steve 开发群");
}

// ---------------------------------------------------------------- §2.3 step 2: the prefix rule

#[tokio::test]
async fn prefix_rule_attaches_same_local_part_in_the_same_org() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let got = ident(&t, "mail", "sarah@acme.io", &named("S. Chen")).unwrap();
    assert!(got.created && got.proposal.is_none());
    assert_eq!(got.party_id, a.sarah);
    let acc = ok(
        &r,
        "GET",
        &format!("/accounts/{}", got.account_id),
        Value::Null,
    )
    .await;
    assert_eq!(acc["party"]["id"], json!(a.sarah));
    assert_eq!(acc["source"], "rule:email_prefix");
    assert_eq!(acc["source_label"], "邮箱前缀规则");
    assert_eq!(acc["confidence"], json!(0.9));
    assert_eq!(acc["verified"], true);
    assert_eq!(acc["kind"], "work_email");

    // same prefix under a domain of no organization: same_org_only keeps it apart
    let gmail = ident(&t, "mail", "sarah@gmail.com", &Hints::default()).unwrap();
    assert_ne!(gmail.party_id, a.sarah);
}

#[tokio::test]
async fn prefix_rule_excludes_public_prefixes_and_short_ones() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    ok(&r, "POST", "/accounts", json!({ "system_id": "mail", "party_id": a.sarah, "kind": "shared_mailbox", "handle": "noreply@acme.com" })).await;
    ok(&r, "POST", "/accounts", json!({ "system_id": "mail", "party_id": a.sarah, "kind": "work_email", "handle": "sc@acme.com" })).await;
    let n = ident(&t, "mail", "noreply@acme.io", &Hints::default()).unwrap();
    assert_ne!(n.party_id, a.sarah, "noreply is excluded");
    let s = ident(&t, "mail", "sc@acme.io", &Hints::default()).unwrap();
    assert_ne!(s.party_id, a.sarah, "shorter than min_len 3");
    // new unconfirmed contacts at Acme (the domain's organization)
    let works: i64 = scalar(
        &t,
        &format!(
            "SELECT COUNT(*) FROM links WHERE subject_id = '{}' AND predicate = 'works_at' AND object_id = '{}' AND source = 'infer'",
            n.party_id, a.org
        ),
    );
    assert_eq!(works, 1);
}

#[tokio::test]
async fn prefix_rule_follows_its_settings() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let rules = ok(&r, "GET", "/match-rules", Value::Null).await;
    assert_eq!(rules.as_array().unwrap().len(), 1);
    let rule = &rules[0];
    assert_eq!(rule["kind"], "email_prefix");
    assert_eq!(rule["family"], "email");
    assert_eq!(rule["enabled"], true);
    assert_eq!(
        rule["config"],
        json!({ "same_org_only": true, "exclude": ["admin", "info", "noreply", "support", "hello"], "min_len": 3 })
    );
    let rid = id(rule);

    // off: nothing is attached by prefix
    let b = ok(
        &r,
        "PATCH",
        &format!("/match-rules/{rid}"),
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(b["enabled"], false);
    let off = ident(&t, "mail", "sarah@acme.io", &Hints::default()).unwrap();
    assert_ne!(off.party_id, a.sarah);

    // on again, across organizations, and “sarah” excluded
    let b = ok(
        &r,
        "PATCH",
        &format!("/match-rules/{rid}"),
        json!({ "enabled": true, "config": { "same_org_only": false, "exclude": [" Sarah "] } }),
    )
    .await;
    assert_eq!(b["config"]["exclude"], json!(["sarah"]));
    assert_eq!(b["config"]["min_len"], 3);
    let ex = ident(&t, "mail", "sarah@gmail.com", &Hints::default()).unwrap();
    assert_ne!(ex.party_id, a.sarah);
    ok(
        &r,
        "PATCH",
        &format!("/match-rules/{rid}"),
        json!({ "config": { "exclude": [] } }),
    )
    .await;
    let cross = ident(&t, "mail", "sarah@outlook.com", &Hints::default()).unwrap();
    // three owners have a “sarah@” mailbox by now (Sarah and the contacts behind sarah@acme.io, sarah@gmail.com): ambiguous
    assert_ne!(cross.party_id, a.sarah);

    // validation and ownership
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/match-rules/{rid}"),
        Some(json!({ "config": { "min_len": 11 } })),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：config.min_len")));
    let many: Vec<String> = (0..51).map(|i| format!("p{i}")).collect();
    let (s, _) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/match-rules/{rid}"),
        Some(json!({ "config": { "exclude": many } })),
    )
    .await;
    assert_eq!(s, 400);
    let (s, e) = req(
        &r,
        &caller(WS2, ME),
        "PATCH",
        &format!("/match-rules/{rid}"),
        Some(json!({ "enabled": false })),
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个规则")));
}

#[tokio::test]
async fn prefix_rule_with_unique_owner_across_two_domains() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    // another person with the same local part at another organization does not interfere
    let globex = id(&ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Globex", "domains": ["globex.io"] }),
    )
    .await);
    let other = id(&ok(
        &r,
        "POST",
        "/contacts",
        json!({ "name": "Sarah Lee", "org_id": globex }),
    )
    .await);
    ok(&r, "POST", "/accounts", json!({ "system_id": "mail", "party_id": other, "kind": "work_email", "handle": "sarah@globex.io" })).await;
    let got = ident(&t, "outlook", "SARAH@acme.io", &Hints::default()).unwrap();
    assert_eq!(got.party_id, a.sarah);
}

// ---------------------------------------------------------------- §2.3 step 3: proposals

#[tokio::test]
async fn same_name_and_signature_org_gives_a_proposal() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let hints = Hints {
        display_name: Some("Sarah Chen".into()),
        signature_org: Some("Acme".into()),
        source_label: Some("邮件 · 10-29".into()),
        ..Default::default()
    };
    let got = ident(&t, "mail", "sarah.c.design@gmail.com", &hints).unwrap();
    assert!(got.created);
    assert_ne!(got.party_id, a.sarah, "never merged silently");
    let mp = got.proposal.clone().expect("proposal");

    let (from, into, acc, reason, conf, status, dec): (String, String, String, String, f64, String, String) = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT from_id, into_id, account_id, reason, confidence, status, decision_id FROM merge_proposals WHERE id = ?",
                [&mp],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (from.as_str(), into.as_str(), acc.as_str()),
        (
            got.party_id.as_str(),
            a.sarah.as_str(),
            got.account_id.as_str()
        )
    );
    assert_eq!(
        reason,
        "邮件签名写着 Sarah Chen · Acme，和 Sarah Chen 同名同组织"
    );
    assert_eq!((conf, status.as_str()), (0.6, "open"));

    let d = t
        .store()
        .with_conn(|c| steve_store::base::decisions::get(c, &dec))
        .unwrap()
        .unwrap();
    assert_eq!(d.kind, "propose");
    assert_eq!(d.subject_type, "party_merge");
    assert_eq!(d.subject_id, mp);
    assert_eq!(d.question, "sarah.c.design@gmail.com 可能是 Sarah Chen");
    assert_eq!(d.options, vec!["merge".to_string(), "not_same".to_string()]);
    assert_eq!(d.status, "open");
    assert!(d.item_id.is_none());

    // the new contact is unconfirmed; the account is not verified yet
    let p: String = scalar(
        &t,
        &format!(
            "SELECT contact_kind FROM parties WHERE id = '{}'",
            got.party_id
        ),
    );
    assert_eq!(p, "unconfirmed");
    let acc = ok(
        &r,
        "GET",
        &format!("/accounts/{}", got.account_id),
        Value::Null,
    )
    .await;
    assert_eq!(acc["verified"], false);
    assert_eq!(acc["confidence"], json!(0.6));
    assert_eq!(acc["source_label"], "邮件 · 10-29");
    assert_eq!(acc["kind"], "personal");

    // seen again: the same account, no second proposal
    let again = ident(&t, "mail", "sarah.c.design@gmail.com", &hints).unwrap();
    assert_eq!((again.created, again.proposal), (false, None));
    let n: i64 = scalar(&t, "SELECT COUNT(*) FROM merge_proposals");
    assert_eq!(n, 1);
}

#[tokio::test]
async fn same_name_by_alias_and_domain_org() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    // the prefix differs, the domain is Acme's, the display name is an alias
    let got = ident(&t, "mail", "s.chen@acme.io", &named("sarah")).unwrap();
    assert!(got.proposal.is_some());
    let reason: String = scalar(&t, "SELECT reason FROM merge_proposals");
    assert_eq!(
        reason,
        "sarah 的邮箱域名属于 Acme，和 Sarah Chen 同名同组织"
    );
    let into: String = scalar(&t, "SELECT into_id FROM merge_proposals");
    assert_eq!(into, a.sarah);
}

#[tokio::test]
async fn same_name_without_same_org_is_no_clue() {
    let t = env();
    let r = app(&t);
    acme(&r).await;
    // same name, signature names another organization
    let hints = Hints {
        display_name: Some("Sarah Chen".into()),
        signature_org: Some("Initech".into()),
        ..Default::default()
    };
    let got = ident(&t, "mail", "sc.design@gmail.com", &hints).unwrap();
    assert!(got.proposal.is_none());
    // a one-character name is no clue either
    let one = ident(&t, "mail", "x@acme.io", &named("S")).unwrap();
    assert!(one.proposal.is_none());
    let n: i64 = scalar(&t, "SELECT COUNT(*) FROM merge_proposals");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn private_contacts_of_others_are_not_proposed() {
    let t = env();
    let r = app(&t);
    let other = caller(WS, OTHER);
    let org = id(&ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Acme", "domains": ["acme.com"] }),
    )
    .await);
    let (s, p) = req(
        &r,
        &other,
        "POST",
        "/contacts",
        Some(json!({ "name": "Sarah Chen", "org_id": org, "visibility": "private" })),
    )
    .await;
    assert_eq!(s, 200, "{p}");
    let got = ident(&t, "mail", "s.chen@acme.com", &named("Sarah Chen")).unwrap();
    assert!(got.proposal.is_none());
}

// ---------------------------------------------------------------- §2.3 step 4: new contacts

#[tokio::test]
async fn unknown_handle_creates_an_unconfirmed_contact() {
    let t = env();
    let r = app(&t);
    let globex = id(&ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Globex", "domains": ["globex.io"] }),
    )
    .await);
    t.take_events();
    let got = ident(&t, "mail", "Hank@Globex.io", &named("Hank Scorpio")).unwrap();
    assert!(got.created && got.proposal.is_none() && !got.is_ours);
    let (name, kind, vis): (String, String, String) = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT name, contact_kind, visibility FROM parties WHERE id = ?",
                [&got.party_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (name.as_str(), kind.as_str(), vis.as_str()),
        ("Hank Scorpio", "unconfirmed", "workspace")
    );
    let works: i64 = scalar(
        &t,
        &format!(
            "SELECT COUNT(*) FROM links WHERE subject_id = '{}' AND object_id = '{globex}' AND predicate = 'works_at' AND source = 'infer'",
            got.party_id
        ),
    );
    assert_eq!(works, 1);
    let acc = ok(
        &r,
        "GET",
        &format!("/accounts/{}", got.account_id),
        Value::Null,
    )
    .await;
    assert_eq!(acc["handle"], "hank@globex.io");
    assert_eq!(acc["display_name"], "Hank Scorpio");
    assert_eq!(acc["kind"], "work_email");
    assert_eq!(acc["source"], "identify");
    assert_eq!(acc["verified"], true);
    // unconfirmed contacts stay out of the default list, show up under kind=unconfirmed
    let list = ok(&r, "GET", "/contacts?kind=unconfirmed", Value::Null).await;
    assert_eq!(list[0]["id"], json!(got.party_id));
    assert_eq!(list[0]["accounts"], 1);
    let evs = t.take_events();
    assert!(evs.contains(&ChangeEvent::Account {
        id: got.account_id.clone(),
        ws: WS.into()
    }));
    assert!(evs.contains(&ChangeEvent::Party {
        id: got.party_id.clone(),
        ws: WS.into()
    }));

    // no display name, no organization: named after the handle, no job
    let bare = ident(&t, "github", "Octocat", &Hints::default()).unwrap();
    let name: String = scalar(
        &t,
        &format!("SELECT name FROM parties WHERE id = '{}'", bare.party_id),
    );
    assert_eq!(name, "octocat");
    let jobs: i64 = scalar(
        &t,
        &format!(
            "SELECT COUNT(*) FROM links WHERE subject_id = '{}'",
            bare.party_id
        ),
    );
    assert_eq!(jobs, 0);
}

#[tokio::test]
async fn identify_rejects_unknown_systems_and_empty_handles() {
    let t = env();
    assert_eq!(
        ident(&t, "nope", "x", &Hints::default()),
        Err(ApiError::not_found("系统"))
    );
    assert_eq!(
        ident(&t, "mail", "not-an-email", &Hints::default()),
        Err(ApiError::invalid("handle"))
    );
    assert_eq!(
        ident(&t, "phone", "abc", &Hints::default()),
        Err(ApiError::invalid("handle"))
    );
}

// ---------------------------------------------------------------- REST: accounts

#[tokio::test]
async fn record_list_and_get_accounts() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    t.take_events();
    let mine = ok(&r, "POST", "/accounts", json!({ "system_id": "aws", "kind": "root", "handle": " 123456789012 ", "display_name": "AWS 主账号" })).await;
    assert_eq!(
        mine["system"],
        json!({ "id": "aws", "name": "AWS", "kind": "cloud" })
    );
    assert_eq!(mine["party"]["is_me"], true);
    assert_eq!(mine["party"]["type"], "contact");
    assert_eq!(mine["handle"], "123456789012");
    assert_eq!(mine["display_name"], "AWS 主账号");
    assert_eq!(mine["source_label"], "你填的");
    assert_eq!(mine["verified"], true);
    assert_eq!(mine["connection"], Value::Null);
    assert_eq!(mine["vault_items"], json!([]));
    assert_eq!(mine["revision"], 1);
    let evs = t.take_events();
    assert!(evs.contains(&ChangeEvent::Account {
        id: id(&mine),
        ws: WS.into()
    }));

    let org: String = scalar(&t, "SELECT id FROM parties WHERE org_kind = 'self'");
    let bot = ok(&r, "POST", "/accounts", json!({ "system_id": "feishu_app", "party_id": org, "kind": "app_bot", "handle": "cli_b81" })).await;
    assert_eq!(bot["display_name"], "cli_b81", "falls back to the handle");

    let ids = |v: &Value| v.as_array().unwrap().iter().map(id).collect::<Vec<_>>();
    assert_eq!(
        ids(&ok(&r, "GET", "/accounts?scope=mine", Value::Null).await),
        vec![id(&mine)]
    );
    assert_eq!(
        ids(&ok(&r, "GET", "/accounts?scope=company", Value::Null).await),
        vec![id(&bot)]
    );
    assert_eq!(
        ids(&ok(&r, "GET", "/accounts?scope=others", Value::Null).await),
        vec![a.sarah_mail.clone()]
    );
    assert_eq!(
        ids(&ok(
            &r,
            "GET",
            &format!("/accounts?party={}", a.sarah),
            Value::Null
        )
        .await),
        vec![a.sarah_mail.clone()]
    );
    assert_eq!(
        ids(&ok(&r, "GET", "/accounts?party=me&system=aws", Value::Null).await),
        vec![id(&mine)]
    );
    assert_eq!(
        ids(&ok(&r, "GET", "/accounts?system=mail", Value::Null).await),
        vec![a.sarah_mail.clone()]
    );
    assert_eq!(
        ok(&r, "GET", "/accounts", Value::Null)
            .await
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let (s, e) = req(&r, &me(), "GET", "/accounts?scope=all", None).await;
    assert_eq!((s, e), (400, err("参数不对：scope")));
    let (s, e) = req(&r, &me(), "GET", "/accounts?party=ct_nope", None).await;
    assert_eq!((s, e), (404, err("没有这个联系人")));
    let (s, e) = req(
        &r,
        &me(),
        "GET",
        &format!("/accounts?party={}", a.org),
        None,
    )
    .await;
    assert_eq!(s, 200, "{e}");

    // another workspace sees none of it
    let w2 = caller(WS2, ME);
    let (s, e) = req(&r, &w2, "GET", &format!("/accounts/{}", a.sarah_mail), None).await;
    assert_eq!((s, e), (404, err("没有这个账号")));
    let (s, b) = req(&r, &w2, "GET", "/accounts", None).await;
    assert_eq!((s, b), (200, json!([])));
    let (s, e) = req(
        &r,
        &w2,
        "GET",
        &format!("/accounts?party={}", a.sarah),
        None,
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个联系人")));
}

#[tokio::test]
async fn accounts_of_invisible_contacts_are_hidden() {
    let t = env();
    let r = app(&t);
    let other = caller(WS, OTHER);
    let (_, p) = req(
        &r,
        &other,
        "POST",
        "/contacts",
        Some(json!({ "name": "秘密", "visibility": "private" })),
    )
    .await;
    let (s, a) = req(&r, &other, "POST", "/accounts", Some(json!({ "system_id": "phone", "party_id": id(&p), "kind": "phone", "handle": "+86 138 0000 0000" }))).await;
    assert_eq!(s, 200, "{a}");
    assert_eq!(a["handle"], "+86 138 0000 0000");
    let (s, e) = req(&r, &me(), "GET", &format!("/accounts/{}", id(&a)), None).await;
    assert_eq!((s, e), (404, err("没有这个账号")));
    assert_eq!(ok(&r, "GET", "/accounts", Value::Null).await, json!([]));
    // the conflict does not reveal the private owner's name
    let (s, e) = req(
        &r,
        &me(),
        "POST",
        "/accounts",
        Some(json!({ "system_id": "phone", "kind": "phone", "handle": "+8613800000000" })),
    )
    .await;
    assert_eq!((s, e), (409, err("这个账号已经记在「其他人」名下")));
}

#[tokio::test]
async fn handle_conflicts_are_409() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    for h in ["sarah@acme.com", " SARAH@acme.COM", "sa rah@acme.com"] {
        let (s, e) = req(
            &r,
            &me(),
            "POST",
            "/accounts",
            Some(json!({ "system_id": "mail", "kind": "personal", "handle": h })),
        )
        .await;
        assert_eq!(
            (s, e),
            (409, err("这个账号已经记在「Sarah Chen」名下")),
            "{h}"
        );
    }
    // the family is shared: Gmail and the generic mail system are the same mailbox
    let (s, _) = req(
        &r,
        &me(),
        "POST",
        "/accounts",
        Some(json!({ "system_id": "gmail", "kind": "personal", "handle": "sarah@acme.com" })),
    )
    .await;
    assert_eq!(s, 409);
    // another family is another handle
    let (s, b) = req(
        &r,
        &me(),
        "POST",
        "/accounts",
        Some(json!({ "system_id": "github", "kind": "personal", "handle": "sarah@acme.com" })),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let n: i64 = scalar(
        &t,
        &format!(
            "SELECT COUNT(*) FROM accounts WHERE party_id = '{}'",
            a.sarah
        ),
    );
    assert_eq!(n, 1);
}

#[tokio::test]
async fn record_validation() {
    let t = env();
    let r = app(&t);
    let cases = [
        (
            json!({ "system_id": "mail", "kind": "x", "handle": "a@b.c" }),
            400,
            err("参数不对：kind"),
        ),
        (
            json!({ "system_id": "mail", "kind": "personal", "handle": "  " }),
            400,
            err("参数不对：handle"),
        ),
        (
            json!({ "system_id": "mail", "kind": "personal", "handle": "x".repeat(201) }),
            400,
            err("参数不对：handle"),
        ),
        (
            json!({ "system_id": "mail", "kind": "personal", "handle": "no-at" }),
            400,
            err("参数不对：handle"),
        ),
        (
            json!({ "system_id": "nope", "kind": "personal", "handle": "a" }),
            404,
            err("没有这个系统"),
        ),
        (
            json!({ "system_id": "mail", "party_id": "ct_nope", "kind": "personal", "handle": "a@b.c" }),
            404,
            err("没有这个联系人"),
        ),
        (
            json!({ "kind": "personal", "handle": "a@b.c" }),
            400,
            err("参数不对：system_id"),
        ),
    ];
    for (body, status, e) in cases {
        let (s, b) = req(&r, &me(), "POST", "/accounts", Some(body.clone())).await;
        assert_eq!((s, b), (status, e), "{body}");
    }
    let (s, _) = call(&r, None, "GET", "/accounts", None).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn cards_keep_only_the_last_four_digits() {
    let t = env();
    let r = app(&t);
    let card = ok(&r, "POST", "/accounts", json!({ "system_id": "bank", "kind": "bank_card", "handle": "6225 8812 3456 4821", "display_name": "招商银行信用卡" })).await;
    assert_eq!(card["handle"], "•••• 4821");
    let (h, d): (String, String) = t
        .store()
        .with_conn(|c| {
            Ok(
                c.query_row("SELECT handle, handle_display FROM accounts", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .unwrap();
    assert_eq!((h.as_str(), d.as_str()), ("card:4821:bank", "•••• 4821"));
    // the full number is nowhere in the database
    let dump: String = t
        .store()
        .with_conn(|c| {
            let mut out = String::new();
            for table in ["accounts", "account_handles", "audit", "revisions"] {
                let mut st = c.prepare(&format!("SELECT * FROM {table}"))?;
                let n = st.column_count();
                let mut rows = st.query([])?;
                while let Some(row) = rows.next()? {
                    for i in 0..n {
                        if let Ok(s) = row.get::<_, String>(i) {
                            out.push_str(&s);
                        }
                    }
                }
            }
            Ok(out)
        })
        .unwrap();
    assert!(!dump.contains("6225") && !dump.contains("88123456"));
    let (s, _) = req(
        &r,
        &me(),
        "POST",
        "/accounts",
        Some(json!({ "system_id": "bank", "kind": "bank_card", "handle": "123" })),
    )
    .await;
    assert_eq!(s, 400);
    let (s, e) = req(
        &r,
        &me(),
        "POST",
        "/accounts",
        Some(json!({ "system_id": "bank", "kind": "bank_card", "handle": "0000 4821" })),
    )
    .await;
    assert_eq!((s, e), (409, err("这个账号已经记在「Dev」名下")));
}

#[tokio::test]
async fn systems_are_seeded() {
    let t = env();
    let r = app(&t);
    let s = ok(&r, "GET", "/systems", Value::Null).await;
    let ids: Vec<&str> = s
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "steve",
            "mail",
            "gmail",
            "outlook",
            "icloud",
            "feishu",
            "feishu_app",
            "slack",
            "gcal",
            "github",
            "aws",
            "phone",
            "bank",
            "feishu_tenant"
        ]
    );
    let gcal = &s[8];
    assert_eq!(gcal["kind"], "calendar");
    assert_eq!(gcal["family"], "email");
    assert_eq!(gcal["plugin"], "builtin:gcal");
    assert_eq!(gcal["builtin"], true);
    // a workspace's own system shows after the built-in ones
    exec(
        &t,
        "INSERT INTO systems (id, workspace_id, name, kind, family, handle_rule, created) VALUES ('jira', 'default', 'Jira', 'code', 'other', '{\"normalize\":\"lower\"}', 1)",
    );
    let s = ok(&r, "GET", "/systems", Value::Null).await;
    assert_eq!(s.as_array().unwrap().last().unwrap()["id"], "jira");
    let (_, b) = req(&r, &caller(WS2, ME), "GET", "/systems", None).await;
    assert_eq!(b.as_array().unwrap().len(), 14);
}

#[tokio::test]
async fn patch_moves_and_verifies() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let got = ident(
        &t,
        "mail",
        "sarah.c.design@gmail.com",
        &Hints {
            display_name: Some("Sarah Chen".into()),
            signature_org: Some("Acme".into()),
            ..Default::default()
        },
    )
    .unwrap();
    t.take_events();
    let b = ok(
        &r,
        "PATCH",
        &format!("/accounts/{}", got.account_id),
        json!({ "party_id": a.sarah }),
    )
    .await;
    assert_eq!(b["party"]["id"], json!(a.sarah));
    assert_eq!(b["verified"], true);
    assert_eq!(b["confidence"], json!(1.0));
    assert_eq!(b["revision"], 2);
    let (action, detail): (String, String) = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT action, detail FROM audit WHERE target = ? ORDER BY id DESC LIMIT 1",
                [&got.account_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(action, "account.move");
    assert_eq!(
        serde_json::from_str::<Value>(&detail).unwrap(),
        json!({ "from": got.party_id, "to": a.sarah })
    );
    let patch: String = scalar(
        &t,
        &format!(
            "SELECT patch FROM revisions WHERE object_type = 'account' AND object_id = '{}'",
            got.account_id
        ),
    );
    let patch: Value = serde_json::from_str(&patch).unwrap();
    assert_eq!(patch["before"]["party_id"], json!(got.party_id));
    assert_eq!(patch["after"]["party_id"], json!(a.sarah));
    let evs = t.take_events();
    assert!(evs.contains(&ChangeEvent::Party {
        id: a.sarah.clone(),
        ws: WS.into()
    }));
    assert!(evs.contains(&ChangeEvent::Party {
        id: got.party_id.clone(),
        ws: WS.into()
    }));

    let b = ok(
        &r,
        "PATCH",
        &format!("/accounts/{}", got.account_id),
        json!({ "display_name": "Sarah 的私人邮箱", "kind": "personal" }),
    )
    .await;
    assert_eq!(
        (b["display_name"].clone(), b["revision"].clone()),
        (json!("Sarah 的私人邮箱"), json!(3))
    );
    let action: String = scalar(&t, "SELECT action FROM audit ORDER BY id DESC LIMIT 1");
    assert_eq!(action, "account.update");
    // nothing changed: no new revision
    let b = ok(
        &r,
        "PATCH",
        &format!("/accounts/{}", got.account_id),
        json!({ "kind": "personal" }),
    )
    .await;
    assert_eq!(b["revision"], 3);

    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/accounts/{}", got.account_id),
        Some(json!({ "verified": false })),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：verified")));
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/accounts/{}", got.account_id),
        Some(json!({ "kind": "boss" })),
    )
    .await;
    assert_eq!((s, e), (400, err("参数不对：kind")));
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/accounts/{}", got.account_id),
        Some(json!({ "party_id": "org_nope" })),
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个联系人")));
    let (s, e) = req(
        &r,
        &caller(WS2, ME),
        "PATCH",
        &format!("/accounts/{}", got.account_id),
        Some(json!({ "kind": "other" })),
    )
    .await;
    assert_eq!((s, e), (404, err("没有这个账号")));
}

#[tokio::test]
async fn connected_accounts_stay_with_us() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let mine = ok(
        &r,
        "POST",
        "/accounts",
        json!({ "system_id": "gmail", "kind": "personal", "handle": "dev@example.test" }),
    )
    .await;
    exec(
        &t,
        &format!(
            "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated) VALUES ('con_1', 'default', '{}', 'gmail', 'oauth', '[\"mail.read\"]', 'usr_dev', 1, 1)",
            id(&mine)
        ),
    );
    let got = ok(&r, "GET", &format!("/accounts/{}", id(&mine)), Value::Null).await;
    assert_eq!(
        got["connection"],
        json!({ "id": "con_1", "status": "ok", "caps": ["mail.read"], "last_sync": null })
    );
    let (s, e) = req(
        &r,
        &me(),
        "PATCH",
        &format!("/accounts/{}", id(&mine)),
        Some(json!({ "party_id": a.sarah })),
    )
    .await;
    assert_eq!(
        (s, e),
        (400, err("已接入 Steve 的账号只能属于你、本公司或团队"))
    );
    let (s, e) = req(
        &r,
        &me(),
        "DELETE",
        &format!("/accounts/{}", id(&mine)),
        None,
    )
    .await;
    assert_eq!((s, e), (409, err("先断开 Steve 的接入")));
    let n: i64 = scalar(&t, "SELECT COUNT(*) FROM accounts");
    assert_eq!(n, 2);
}

/// A shared and a personal vault with an item each linked to `account`, plus the system vault.
fn vault_items(t: &TestEnv, account: &str) {
    exec(t, &format!(
        "INSERT INTO vaults (id, workspace_id, name, scope, owner_user, created) VALUES ('vlt_s', 'default', '共享', 'shared', NULL, 1), ('vlt_p', 'default', '个人', 'personal', 'usr_other', 1), ('vlt_x', 'default', '系统', 'system', NULL, 1);
         INSERT INTO vault_items (id, workspace_id, vault_id, template, title, account_id, sensitivity, created_by, created, updated) VALUES
           ('vi_s', 'default', 'vlt_s', 'login', 'AWS 登录', '{account}', 'sensitive', 'usr_dev', 1, 1),
           ('vi_p', 'default', 'vlt_p', 'login', '别人的', '{account}', 'sensitive', 'usr_other', 2, 2),
           ('vi_x', 'default', 'vlt_x', 'connection_credential', '凭据', '{account}', 'sensitive', 'usr_dev', 3, 3);"
    ));
}

#[tokio::test]
async fn delete_unlinks_vault_items() {
    let t = env();
    let r = app(&t);
    let acc = ok(
        &r,
        "POST",
        "/accounts",
        json!({ "system_id": "aws", "kind": "root", "handle": "123456789012" }),
    )
    .await;
    vault_items(&t, &id(&acc));
    let got = ok(&r, "GET", &format!("/accounts/{}", id(&acc)), Value::Null).await;
    assert_eq!(
        got["vault_items"],
        json!([{ "id": "vi_s", "title": "AWS 登录" }]),
        "only items the caller may see"
    );
    let (s, b) = req(
        &r,
        &me(),
        "DELETE",
        &format!("/accounts/{}", id(&acc)),
        None,
    )
    .await;
    assert_eq!((s, b), (200, json!({ "ok": true })));
    let linked: i64 = scalar(
        &t,
        "SELECT COUNT(*) FROM vault_items WHERE account_id IS NOT NULL",
    );
    let items: i64 = scalar(&t, "SELECT COUNT(*) FROM vault_items");
    let handles: i64 = scalar(&t, "SELECT COUNT(*) FROM account_handles");
    assert_eq!((linked, items, handles), (0, 3, 0));
    let (s, e) = req(&r, &me(), "GET", &format!("/accounts/{}", id(&acc)), None).await;
    assert_eq!((s, e), (404, err("没有这个账号")));
    let action: String = scalar(&t, "SELECT action FROM audit ORDER BY id DESC LIMIT 1");
    assert_eq!(action, "account.delete");
}

// ---------------------------------------------------------------- contacts ports

#[tokio::test]
async fn contacts_count_and_search_accounts() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    ok(&r, "POST", "/accounts", json!({ "system_id": "github", "party_id": a.sarah, "kind": "personal", "handle": "SarahC" })).await;
    let list = ok(&r, "GET", "/contacts", Value::Null).await;
    let sarah = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(a.sarah))
        .unwrap();
    assert_eq!(sarah["accounts"], 2);
    let found = ok(&r, "GET", "/contacts?q=ACME.COM", Value::Null).await;
    assert_eq!(
        found.as_array().unwrap().iter().map(id).collect::<Vec<_>>(),
        vec![a.sarah.clone()]
    );
    let found = ok(&r, "GET", "/contacts?q=sarahc", Value::Null).await;
    assert_eq!(found[0]["id"], json!(a.sarah));
    // `_` in a query is literal
    assert_eq!(
        ok(&r, "GET", "/contacts?q=sarah_acme", Value::Null).await,
        json!([])
    );
}

#[tokio::test]
async fn deleting_a_contact_deletes_its_accounts() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    vault_items(&t, &a.sarah_mail);
    t.take_events();
    let (s, b) = req(&r, &me(), "DELETE", &format!("/contacts/{}", a.sarah), None).await;
    assert_eq!((s, b), (200, json!({ "ok": true })));
    let n: i64 = scalar(&t, "SELECT COUNT(*) FROM accounts");
    let h: i64 = scalar(&t, "SELECT COUNT(*) FROM account_handles");
    let linked: i64 = scalar(
        &t,
        "SELECT COUNT(*) FROM vault_items WHERE account_id IS NOT NULL",
    );
    assert_eq!((n, h, linked), (0, 0, 0));
    assert!(t.take_events().contains(&ChangeEvent::Account {
        id: a.sarah_mail.clone(),
        ws: WS.into()
    }));
    // the handle is free again
    ok(
        &r,
        "POST",
        "/accounts",
        json!({ "system_id": "mail", "kind": "personal", "handle": "sarah@acme.com" }),
    )
    .await;
}

#[tokio::test]
async fn deleting_a_contact_with_a_connected_account_rolls_back() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    exec(
        &t,
        &format!(
            "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated) VALUES ('con_1', 'default', '{}', 'gmail', 'oauth', 'usr_dev', 1, 1)",
            a.sarah_mail
        ),
    );
    let (s, e) = req(&r, &me(), "DELETE", &format!("/contacts/{}", a.sarah), None).await;
    assert_eq!((s, e), (409, err("先断开 Steve 的接入")));
    let st: String = scalar(
        &t,
        &format!("SELECT status FROM parties WHERE id = '{}'", a.sarah),
    );
    assert_eq!(st, "active");
}

#[tokio::test]
async fn accounts_follow_a_merge_and_come_back_on_split() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let got = ident(
        &t,
        "mail",
        "sarah.c.design@gmail.com",
        &Hints {
            display_name: Some("Sarah Chen".into()),
            signature_org: Some("Acme".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let extra = ok(&r, "POST", "/accounts", json!({ "system_id": "phone", "party_id": got.party_id, "kind": "phone", "handle": "+1 415 555 0123" })).await;
    let hooks = AccountHooks;
    assert_eq!(hooks.name(), "vault");
    t.take_events();

    let moved = t
        .store()
        .transaction(|tx| hooks.on_merge(tx, WS, &got.party_id, &a.sarah))
        .unwrap();
    assert_eq!(moved, json!({ "accounts": [got.account_id, id(&extra)] }));
    let on_sarah = ok(
        &r,
        "GET",
        &format!("/accounts?party={}", a.sarah),
        Value::Null,
    )
    .await;
    assert_eq!(on_sarah.as_array().unwrap().len(), 3);
    assert!(t.take_events().contains(&ChangeEvent::Account {
        id: got.account_id.clone(),
        ws: WS.into()
    }));
    // what the merge record will hold under moved.hooks.vault
    exec(
        &t,
        &format!(
            "UPDATE parties SET status = 'merged', merged_into = '{}' WHERE id = '{}'",
            a.sarah, got.party_id
        ),
    );
    let again = ident(&t, "mail", "sarah.c.design@gmail.com", &Hints::default()).unwrap();
    assert_eq!(again.party_id, a.sarah);

    // one account moved by hand since the merge stays where you put it
    let tom = id(&ok(&r, "POST", "/contacts", json!({ "name": "Tom" })).await);
    ok(
        &r,
        "PATCH",
        &format!("/accounts/{}", id(&extra)),
        json!({ "party_id": tom }),
    )
    .await;
    exec(
        &t,
        &format!(
            "UPDATE parties SET status = 'active', merged_into = NULL WHERE id = '{}'",
            got.party_id
        ),
    );
    t.store()
        .transaction(|tx| hooks.on_split(tx, WS, &got.party_id, &a.sarah, &moved))
        .unwrap();
    let back = ok(
        &r,
        "GET",
        &format!("/accounts?party={}", got.party_id),
        Value::Null,
    )
    .await;
    assert_eq!(
        back.as_array().unwrap().iter().map(id).collect::<Vec<_>>(),
        vec![got.account_id.clone()]
    );
    let on_sarah = ok(
        &r,
        "GET",
        &format!("/accounts?party={}", a.sarah),
        Value::Null,
    )
    .await;
    assert_eq!(
        on_sarah
            .as_array()
            .unwrap()
            .iter()
            .map(id)
            .collect::<Vec<_>>(),
        vec![a.sarah_mail.clone()]
    );
    let on_tom = ok(&r, "GET", &format!("/accounts?party={tom}"), Value::Null).await;
    assert_eq!(on_tom[0]["id"], json!(id(&extra)));
    // an empty or foreign `moved` does nothing
    t.store()
        .transaction(|tx| hooks.on_split(tx, WS, &got.party_id, &a.sarah, &Value::Null))
        .unwrap();
}

// ---------------------------------------------------------------- change operations

fn prepared() -> Prepared {
    Prepared {
        target_id: None,
        revision: None,
        title: "测试".into(),
        href: "#/contacts".into(),
        subject: "test".into(),
        fields: Vec::new(),
        ops: Vec::new(),
        free_text: None,
        context: Value::Null,
    }
}

fn plan(t: &TestEnv, ops: Vec<Value>) -> String {
    changes::store_plan(
        &t.modules,
        &me(),
        "contact",
        &prepared(),
        Plan {
            ops,
            changes: Vec::new(),
        },
        Origin::default(),
    )
    .unwrap()
    .id
}

#[tokio::test]
async fn account_add_applies_and_undoes() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let cs = plan(
        &t,
        vec![
            json!({ "op": "account.add", "party": a.sarah, "system": "phone", "handle": "+1 (415) 555-0123" }),
        ],
    );
    let res = changes::apply(&t.modules, &me(), &cs).unwrap().unwrap();
    assert_eq!(res["ok"], true);
    let (handle, kind, source, label): (String, String, String, String) = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT handle, kind, source, source_label FROM accounts WHERE system_id = 'phone'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (
            handle.as_str(),
            kind.as_str(),
            source.as_str(),
            label.as_str()
        ),
        ("+14155550123", "phone", "chat", "你说的")
    );
    changes::undo(&t.modules, &me(), &cs).unwrap().unwrap();
    let n: i64 = scalar(
        &t,
        "SELECT COUNT(*) FROM accounts WHERE system_id = 'phone'",
    );
    assert_eq!(n, 0);

    // a taken handle refuses the whole changeset
    let cs = plan(
        &t,
        vec![
            json!({ "op": "account.add", "party": a.sarah, "system": "mail", "handle": "sarah@acme.com" }),
        ],
    );
    let e = changes::apply(&t.modules, &me(), &cs).unwrap().unwrap_err();
    assert_eq!(e, ApiError::conflict("这个账号已经记在「Sarah Chen」名下"));
    let cs = plan(
        &t,
        vec![
            json!({ "op": "account.add", "party": "ct_nope", "system": "mail", "handle": "x@y.z" }),
        ],
    );
    assert_eq!(
        changes::apply(&t.modules, &me(), &cs).unwrap().unwrap_err(),
        ApiError::not_found("联系人")
    );
}

#[tokio::test]
async fn account_patch_applies_and_undoes() {
    let t = env();
    let r = app(&t);
    let a = acme(&r).await;
    let tom = id(&ok(&r, "POST", "/contacts", json!({ "name": "Tom" })).await);
    let cs = plan(
        &t,
        vec![
            json!({ "op": "account.patch", "id": a.sarah_mail, "owner": tom, "display_name": "共用邮箱" }),
        ],
    );
    changes::apply(&t.modules, &me(), &cs).unwrap().unwrap();
    let acc = ok(
        &r,
        "GET",
        &format!("/accounts/{}", a.sarah_mail),
        Value::Null,
    )
    .await;
    assert_eq!(
        (acc["party"]["id"].clone(), acc["display_name"].clone()),
        (json!(tom), json!("共用邮箱"))
    );
    changes::undo(&t.modules, &me(), &cs).unwrap().unwrap();
    let acc = ok(
        &r,
        "GET",
        &format!("/accounts/{}", a.sarah_mail),
        Value::Null,
    )
    .await;
    assert_eq!(acc["party"]["id"], json!(a.sarah));
    assert_eq!(acc["display_name"], "sarah@acme.com");
    assert_eq!(acc["revision"], 3);
    let cs = plan(
        &t,
        vec![json!({ "op": "account.patch", "id": "acc_nope", "kind": "other" })],
    );
    assert_eq!(
        changes::apply(&t.modules, &me(), &cs).unwrap().unwrap_err(),
        ApiError::not_found("账号")
    );
}

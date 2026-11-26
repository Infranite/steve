//! Slice P4 acceptance: after a merge, attributes, links, memories and team memberships
//! are on `into` with one audit row; a split restores all of it; a later merge makes the split 409;
//! after `not_same` the pair is never proposed again — plus the REST branches, the proposal decision with
//! undo, accounts through the hooks, and the `attr_value` / `attr_def` handlers.

use axum::Router;
use serde_json::{Value, json};
use steve_modules::accounts::{self, Hints, identify};
use steve_modules::contacts::{self, merge};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::{Caller, Registry, decisions};
use steve_store::testkit::modules::merge as fixture;
use steve_store::{Decision, NewDecision};

fn env() -> TestEnv {
    let t = TestEnv::new(|r: &mut Registry| {
        accounts::register(r);
        merge::register(r, &accounts::ports());
    });
    fixture::seed_items(t.store()).unwrap();
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

fn decision(t: &TestEnv, id: &str) -> Decision {
    t.store()
        .read(|r| steve_store::DecisionRows(r).get(id))
        .expect("get")
        .expect("decision")
}

/// What one contact has, in a comparable form.

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
    fixture::link_shared_memory(t.store(), &w.mem_shared, &w.sarah).unwrap();
}

#[tokio::test]
async fn merge_moves_everything_and_split_restores_it() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    share_memory(&t, &w);
    let before_dup = fixture::snapshot(t.store(), &w.dup);
    let before_sarah = fixture::snapshot(t.store(), &w.sarah);

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

    let s = fixture::snapshot(t.store(), &w.sarah);
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

    let d = fixture::snapshot(t.store(), &w.dup);
    assert_eq!(
        d["attrs"],
        json!(["phone=+1 111"]),
        "exact duplicate stays behind"
    );
    for k in ["out", "in", "mems", "teams", "tags", "accounts"] {
        assert_eq!(d[k], json!([]), "{k}");
    }
    let (status, into): (String, String) =
        fixture::status_and_merge_target(t.store(), &w.dup).unwrap();
    assert_eq!(
        (status.as_str(), into.as_str()),
        ("merged", w.sarah.as_str())
    );
    assert_eq!(fixture::merge_audits(t.store()).unwrap(), 1);
    let moved: Value = serde_json::from_str(&fixture::merge_payload(t.store()).unwrap()).unwrap();
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
    assert_eq!(fixture::snapshot(t.store(), &w.dup), before_dup);
    assert_eq!(fixture::snapshot(t.store(), &w.sarah), before_sarah);
    assert_eq!(fixture::party_status(t.store(), &w.dup).unwrap(), "active");
    assert_eq!(fixture::merge_target(t.store(), &w.dup).unwrap(), None);
    assert_eq!(fixture::split_audits(t.store()).unwrap(), 1);
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
    let tz: String = fixture::timezone_attr(t.store()).unwrap();
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
    let d = fixture::snapshot(t.store(), &w.dup);
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
        fixture::snapshot(t.store(), &w.sarah)["attrs"],
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
}

fn propose(t: &TestEnv) -> (String, String, String) {
    let got = t
        .store()
        .write(|tx| {
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
    let d: String = fixture::proposal_decision(t.store(), &mp).unwrap();
    (mp, d, got.party_id)
}

fn resolve(t: &TestEnv, d: &str, choice: &str) -> Result<Decision, steve_modules::ApiError> {
    decisions::dispatch(&t.modules, &me(), d, choice, "").expect("ours")
}

/// What steve-core's `undoDecision` runs for a module decision.
fn undo(t: &TestEnv, d: &str) -> Result<(), steve_modules::ApiError> {
    decisions::undo(&t.modules, d, ME, |d| {
        d.status == steve_types::DecisionStatus::Resolved && d.undo.is_some()
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
    assert_eq!(got.status, steve_types::DecisionStatus::Resolved);
    assert!(got.result["merge_id"].as_str().unwrap().starts_with("mg_"));
    assert_eq!(fixture::proposal_status(t.store(), &mp).unwrap(), "merged");
    assert_eq!(fixture::sender_party(t.store()).unwrap(), sarah);
    let (_, pending) = req(&r, &me(), "GET", "/merges", None).await;
    assert_eq!(pending["proposals"], json!([]));
    assert_eq!(resolve(&t, &d, "merge").unwrap(), got);

    undo(&t, &d).unwrap();
    assert_eq!(fixture::proposal_status(t.store(), &mp).unwrap(), "open");
    assert_eq!(fixture::party_status(t.store(), &from).unwrap(), "active");
    assert_eq!(fixture::sender_party(t.store()).unwrap(), from);
    assert_eq!(decision(&t, &d).status, steve_types::DecisionStatus::Open);
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
        fixture::proposal_status(t.store(), &mp).unwrap(),
        "rejected"
    );
    assert!(resolve(&t, &d, "bogus").is_err());

    // the same pair again (a second handle of the same unconfirmed contact) → no new proposal
    let n = fixture::proposal_count(t.store()).unwrap();
    let again = t
        .store()
        .write(|tx| {
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
    assert_eq!(fixture::proposal_count(t.store()).unwrap(), n);
    assert_eq!(fixture::open_proposal_count(t.store()).unwrap(), 0);
}

#[tokio::test]
async fn me_match_proposes_without_auto_merge_and_folds_out_of_decide_tier() {
    let t = env();
    let r = app(&t);
    let me_party = ok(&r, "GET", "/me/contact", Value::Null).await;
    let me_id = id(&me_party);
    ok(
        &r,
        "PATCH",
        &format!("/contacts/{me_id}"),
        json!({ "aliases": ["owner@corp.example"] }),
    )
    .await;
    let seen = t
        .store()
        .write(|tx| {
            identify(
                tx,
                WS,
                ME,
                "mail",
                "owner@corp.example",
                &Hints {
                    display_name: Some("Owner Alt".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_ne!(seen.party_id, me_id);
    let mp = t
        .store()
        .write(|tx| {
            steve_modules::accounts::identify::propose_if_me(
                tx,
                WS,
                &me_id,
                &seen.party_id,
                &seen.account_id,
                Some("Owner Alt"),
                &["owner@corp.example"],
            )
        })
        .unwrap()
        .expect("merge proposal id");
    assert!(mp.starts_with("mp_"), "{mp}");
    // Safety rule: an unconfirmed inbound contact is never silently merged into “我”.
    assert_eq!(
        fixture::party_status(t.store(), &seen.party_id).unwrap(),
        "active"
    );
    let d = fixture::proposal_decision(t.store(), &mp).unwrap();
    // Noise-reduction rule: the party_merge decision folds out of "待我决定" (zero Decide cards).
    let open_views = t
        .store()
        .read(|r| steve_store::views::Views::decision_views(r).open())
        .unwrap();
    let view = open_views
        .iter()
        .find(|v| v.id.as_str() == d)
        .expect("party_merge view");
    assert_eq!(view.tier, steve_types::DecisionTier::Folded);
    assert_eq!(view.folded, Some(steve_types::FoldReason::LowPriority));
    assert_eq!(
        open_views
            .iter()
            .filter(|v| v.tier == steve_types::DecisionTier::Decide)
            .count(),
        0
    );

    resolve(&t, &d, "merge").unwrap();
    assert_eq!(
        fixture::party_status(t.store(), &seen.party_id).unwrap(),
        "merged"
    );
    undo(&t, &d).unwrap();
    assert_eq!(
        fixture::party_status(t.store(), &seen.party_id).unwrap(),
        "active"
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
    assert_eq!(fixture::proposal_status(t.store(), &mp).unwrap(), "merged");
    let d = decision(&t, &d);
    assert_eq!(
        (d.status.as_str(), d.result.clone()),
        ("resolved", json!({ "choice": "merge" }))
    );
}

fn open_confirm(t: &TestEnv, subject_type: &str, subject_id: &str) -> String {
    let n: NewDecision = decisions::confirm(WS, subject_type, subject_id, "确认？", "steve");
    t.store()
        .write(|tx| Ok::<_, steve_store::StoreError>(steve_store::DecisionRows(tx).open(&n)?.id))
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
    let old: String = fixture::only_attribute(t.store()).unwrap();
    let cand = |v: &str, id: &str| {
        fixture::seed_candidate(t.store(), id, &sarah, v).unwrap();
    };
    cand("+1 new", "av_c1");
    let d = open_confirm(&t, "attr_value", "av_c1");
    resolve(&t, &d, "yes").unwrap();
    assert_eq!(fixture::candidate_status(t.store()).unwrap(), "confirmed");
    assert_eq!(fixture::attr_status(t.store(), &old).unwrap(), "stale");
    undo(&t, &d).unwrap();
    assert_eq!(fixture::candidate_status(t.store()).unwrap(), "candidate");
    assert_eq!(fixture::attr_status(t.store(), &old).unwrap(), "confirmed");

    resolve(&t, &d, "no").unwrap();
    assert_eq!(fixture::first_candidate_count(t.store()).unwrap(), 0);
    undo(&t, &d).unwrap();
    assert_eq!(
        fixture::candidate_details(t.store()).unwrap(),
        "+1 new/candidate/邮件签名"
    );

    cand("+1 x", "av_c2");
    let d2 = open_confirm(&t, "attr_value", "av_c2");
    assert!(decisions::expire(&t.modules, &d2).unwrap());
    assert_eq!(fixture::second_candidate_count(t.store()).unwrap(), 0);
}

#[tokio::test]
async fn attr_def_proposals() {
    let t = env();
    fixture::seed_attr_definition(t.store()).unwrap();
    let d = open_confirm(&t, "attr_def", "ad_p");
    resolve(&t, &d, "yes").unwrap();
    assert_eq!(fixture::attr_def_status(t.store()).unwrap(), "active");
    undo(&t, &d).unwrap();
    assert_eq!(fixture::attr_def_status(t.store()).unwrap(), "proposed");
    resolve(&t, &d, "no").unwrap();
    assert_eq!(fixture::attr_def_count(t.store()).unwrap(), 0);
    undo(&t, &d).unwrap();
    assert_eq!(
        fixture::attr_def_details(t.store()).unwrap(),
        "wechat/微信/proposed"
    );
}

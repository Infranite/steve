//! Slice P5 acceptance: with a scripted model gateway, the two examples give
//! the expected operations and preview lines, apply writes the data and undo puts it back exactly;
//! names that are not recognized show “（没认出：X）” instead of being guessed. Plus the other fields and
//! operations of the three targets, staleness (409), org-chart limits and the `global` candidates.

use axum::Router;
use serde_json::{Value, json};
use steve_modules::changes::{self, Origin, Proposal};
use steve_modules::contacts::{self, Ports};
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::{ApiError, Caller, accounts};

fn env_with(ports: Ports) -> TestEnv {
    TestEnv::new(move |reg| {
        accounts::register(reg);
        contacts::changes::register(reg, &ports);
    })
}

fn env() -> TestEnv {
    env_with(Ports::default())
}

fn app(t: &TestEnv) -> Router {
    contacts::routes(Ports::default()).with_state(t.modules.clone())
}

fn me() -> Caller {
    caller(WS, ME)
}

async fn ok(r: &Router, method: &str, path: &str, body: Value) -> Value {
    let b = if body.is_null() { None } else { Some(body) };
    let (s, v) = call(r, Some(&me()), method, path, b.as_ref()).await;
    assert_eq!(s, 200, "{method} {path}: {v}");
    v
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

/// Drafts with the model answering `ops`; returns the proposal.
async fn draft(
    t: &TestEnv,
    kind: &str,
    target: Option<&str>,
    request: &str,
    ops: Value,
) -> Proposal {
    t.gateway.reply(json!({ "ops": ops }).to_string());
    changes::draft(&t.modules, &me(), kind, target, request, Origin::default())
        .await
        .expect("draft")
}

fn lines(p: &Proposal) -> Vec<(String, String, String)> {
    p.changes
        .iter()
        .map(|c| (c.label.clone(), c.from.clone(), c.to.clone()))
        .collect()
}

fn line(l: &str, f: &str, to: &str) -> (String, String, String) {
    (l.into(), f.into(), to.into())
}

fn stored_ops(t: &TestEnv, cs: &str) -> Vec<Value> {
    let s: String = scalar(t, &format!("SELECT ops FROM changesets WHERE id = '{cs}'"));
    serde_json::from_str(&s).expect("ops json")
}

fn apply(t: &TestEnv, cs: &str) -> Result<Value, ApiError> {
    changes::apply(&t.modules, &me(), cs).expect("stored changeset")
}

fn undo(t: &TestEnv, cs: &str) {
    changes::undo(&t.modules, &me(), cs)
        .expect("stored changeset")
        .expect("undo");
}

struct World {
    acme: String,
    sarah: String,
    lilei: String,
    wang: String,
}

async fn world(r: &Router) -> World {
    let acme = id(&ok(
        r,
        "POST",
        "/orgs",
        json!({ "name": "Acme", "kind": "customer" }),
    )
    .await);
    let sarah = id(&ok(
        r,
        "POST",
        "/contacts",
        json!({ "name": "Sarah Chen", "aliases": ["Sarah"], "rels": ["customer"], "org_id": acme, "title": "产品经理" }),
    )
    .await);
    let lilei = id(&ok(r, "POST", "/contacts", json!({ "name": "李雷" })).await);
    let wang = id(&ok(r, "POST", "/contacts", json!({ "name": "王芳" })).await);
    World {
        acme,
        sarah,
        lilei,
        wang,
    }
}

/// Everything about the data the examples touch, to compare before/after an apply + undo.
fn state(t: &TestEnv) -> Value {
    let rows = |sql: &str| -> Value {
        t.store()
            .with_conn(|c| {
                let mut st = c.prepare(sql)?;
                let n = st.column_count();
                let rows = st
                    .query_map([], |r| {
                        let mut row = Vec::new();
                        for i in 0..n {
                            let v: rusqlite::types::Value = r.get(i)?;
                            row.push(match v {
                                rusqlite::types::Value::Null => Value::Null,
                                rusqlite::types::Value::Integer(i) => json!(i),
                                rusqlite::types::Value::Real(f) => json!(f),
                                rusqlite::types::Value::Text(s) => json!(s),
                                rusqlite::types::Value::Blob(_) => json!("blob"),
                            });
                        }
                        Ok(Value::Array(row))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(Value::Array(rows))
            })
            .expect("rows")
    };
    json!({
        "parties": rows("SELECT id, type, name, aliases, rels, important, note, org_kind, status FROM parties ORDER BY id"),
        "links": rows("SELECT subject_id, predicate, object_id, props FROM links ORDER BY subject_id, object_id"),
        "mems": rows("SELECT id, kind, text FROM mems ORDER BY id"),
        "attrs": rows("SELECT id, key, label, value, status FROM attr_values ORDER BY id"),
        "defs": rows("SELECT key FROM attr_defs WHERE workspace_id != '*' ORDER BY key"),
        "teams": rows("SELECT party_id, kind, purpose FROM team_details ORDER BY party_id"),
        "members": rows("SELECT team_id, contact_id, role, source FROM team_members ORDER BY team_id, contact_id"),
        "domains": rows("SELECT domain, org_id FROM org_domains ORDER BY domain"),
    })
}

// ---------------------------------------------------------------- Example 1

#[tokio::test]
async fn sarah_moves_to_globex() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let before = state(&t);
    let p = draft(
        &t,
        "contact",
        Some(&w.sarah),
        "Sarah 换到 Globex 做 PM 了",
        json!([
            { "op": "job.close", "org": "Acme", "to": "2026-10" },
            { "op": "job.open", "org": "Globex", "title": "PM" },
            { "op": "mem.add", "kind": "event", "text": "Sarah 换到 Globex 做 PM" }
        ]),
    )
    .await;

    // the prompt described the target, its fields and operations
    let prompt = t.gateway.last_prompt().unwrap();
    assert!(
        prompt.contains("the contact \"Sarah Chen\" (currently at: Acme · 产品经理)"),
        "{prompt}"
    );
    assert!(
        prompt.contains(
            "rels · 和你的关系 · list · 同事|上级|客户|合作方|朋友|家人|供应商 · [\"客户\"]"
        ),
        "{prompt}"
    );
    assert!(prompt.contains("job.close · "), "{prompt}");
    assert!(
        prompt.contains("Request: \"Sarah 换到 Globex 做 PM 了\""),
        "{prompt}"
    );
    assert!(
        !prompt.contains("account.add"),
        "accounts are not plugged in"
    );

    assert_eq!(p.title, "Sarah Chen 的资料");
    assert_eq!(p.href, format!("#/contacts/{}", w.sarah));
    assert_eq!(p.target, json!({ "kind": "contact", "id": w.sarah }));
    assert_eq!(
        lines(&p),
        vec![
            line("任职", "Acme · 产品经理", "已离开（到 2026-10）"),
            line("任职", "—", "Globex · PM（新建组织）"),
            line("Steve 记得的", "—", "Sarah 换到 Globex 做 PM（事件）"),
        ]
    );
    let ops: Vec<String> = stored_ops(&t, &p.id)
        .iter()
        .map(|o| o["op"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ops, ["job.close", "org.create", "job.open", "mem.add"]);
    assert_eq!(stored_ops(&t, &p.id)[1]["kind"], "partner");
    assert_eq!(stored_ops(&t, &p.id)[2]["org"], "$org1");

    assert_eq!(
        apply(&t, &p.id).unwrap(),
        json!({ "ok": true, "undo": p.id })
    );
    let c = ok(&r, "GET", &format!("/contacts/{}", w.sarah), Value::Null).await;
    assert_eq!(c["org"]["name"], "Globex");
    assert_eq!(c["org"]["title"], "PM");
    let acme_job = c["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["org_name"] == "Acme")
        .unwrap();
    assert_eq!(acme_job["to"], "2026-10");
    let globex: String = scalar(&t, "SELECT org_kind FROM parties WHERE name = 'Globex'");
    assert_eq!(globex, "partner");
    let (text, status, st): (String, String, String) = t
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT m.text, m.status, m.source_type FROM mems m JOIN mem_about a ON a.mem_id = m.id WHERE a.object_id = ?",
                [&w.sarah],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (text.as_str(), status.as_str(), st.as_str()),
        ("Sarah 换到 Globex 做 PM", "confirmed", "me")
    );

    undo(&t, &p.id);
    assert_eq!(state(&t), before, "undo puts everything back");
}

// ---------------------------------------------------------------- Example 2

#[tokio::test]
async fn new_delivery_team_from_names() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let mine = id(&ok(&r, "GET", "/me/contact", Value::Null).await);
    let before = state(&t);
    let p = draft(
        &t,
        "team",
        Some("new"),
        "把李雷、王芳和 Sarah 拉一个 Acme 交付小组",
        json!([{ "op": "team.create", "name": "Acme 交付小组", "members": ["李雷", "王芳", "Sarah"] }]),
    )
    .await;
    assert_eq!(p.title, "新虚拟团队");
    assert_eq!(p.target, json!({ "kind": "team" }));
    assert_eq!(
        lines(&p),
        vec![
            line("新团队", "—", "Acme 交付小组 · 虚拟团队"),
            line("成员", "—", "我、李雷、王芳、Sarah Chen"),
        ]
    );
    let ops = stored_ops(&t, &p.id);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["members"], json!([w.lilei, w.wang, w.sarah]));

    apply(&t, &p.id).unwrap();
    let tm: String = scalar(&t, "SELECT id FROM parties WHERE type = 'team'");
    let team = ok(&r, "GET", &format!("/teams/{tm}"), Value::Null).await;
    assert_eq!(team["kind"], "virtual");
    assert_eq!(team["headcount"], 4);
    let members = team["members"].as_array().unwrap();
    assert_eq!(members[0]["contact_id"], mine);
    assert_eq!(members[0]["role"], "lead");
    assert!(members.iter().all(|m| m["source"] == "chat"));

    undo(&t, &p.id);
    assert_eq!(state(&t), before, "the team is gone again");
}

// ---------------------------------------------------------------- not guessing

#[tokio::test]
async fn unknown_and_ambiguous_names_are_not_guessed() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    ok(&r, "POST", "/contacts", json!({ "name": "韩梅" })).await;
    ok(&r, "POST", "/contacts", json!({ "name": "张伟" })).await;
    ok(&r, "POST", "/contacts", json!({ "name": "张伟" })).await;
    let p = draft(
        &t,
        "team",
        None,
        "拉一个读书会：李雷、韩梅梅、张伟",
        json!([{ "op": "team.create", "name": "读书会", "members": ["李雷", "韩梅梅", "张伟"] }]),
    )
    .await;
    assert_eq!(
        lines(&p),
        vec![
            line("（没认出：韩梅梅）", "—", "没有改动"),
            line("（没认出：张伟）", "—", "有 2 个同名的，没有自动选"),
            line("新团队", "—", "读书会 · 虚拟团队"),
            line("成员", "—", "我、李雷"),
        ]
    );
    assert_eq!(stored_ops(&t, &p.id)[0]["members"], json!([w.lilei]));

    // nobody recognized → no operation at all
    let p = draft(
        &t,
        "team",
        None,
        "拉一个小组：韩梅梅",
        json!([{ "op": "team.create", "name": "小组", "members": ["韩梅梅"] }]),
    )
    .await;
    assert_eq!(
        lines(&p),
        vec![
            line("（没认出：韩梅梅）", "—", "没有改动"),
            line("（成员不够）", "—", "虚拟团队至少要两个人"),
        ]
    );
    assert!(stored_ops(&t, &p.id).is_empty());

    // an organization known under two names is not picked
    ok(&r, "POST", "/orgs", json!({ "name": "Initech" })).await;
    ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Initech Ltd", "aliases": ["Initech"] }),
    )
    .await;
    let p = draft(
        &t,
        "contact",
        Some(&w.lilei),
        "李雷去 Initech 了",
        json!([{ "op": "job.open", "org": "Initech" }, { "op": "job.close", "org": "Acme" }]),
    )
    .await;
    assert_eq!(
        lines(&p),
        vec![
            line("（没认出：Initech）", "—", "有 2 个同名的，没有自动选"),
            line("（没认出：Acme）", "—", "没有改动"),
        ]
    );
    assert!(stored_ops(&t, &p.id).is_empty());
}

// ---------------------------------------------------------------- contact fields, attributes, accounts

#[tokio::test]
async fn contact_fields_attributes_and_append() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    ok(
        &r,
        "POST",
        &format!("/parties/{}/attrs", w.sarah),
        json!({ "key": "phone", "value": "+1 111" }),
    )
    .await;
    ok(
        &r,
        "PATCH",
        &format!("/contacts/{}", w.sarah),
        json!({ "note": "老客户" }),
    )
    .await;
    let before = state(&t);
    let p = draft(
        &t,
        "contact",
        Some(&w.sarah),
        "她是我的朋友也是客户，很重要，电话 +1 222，叫她 Sas，喜欢周五开会",
        json!([
            { "op": "set", "key": "rels", "value": ["朋友", "客户"] },
            { "op": "set", "key": "important", "value": "是" },
            { "op": "set", "key": "note", "value": "喜欢周五开会" },
            { "op": "set", "key": "unknown_field", "value": "x" },
            { "op": "attr.set", "key": "phone", "value": "+1 222" },
            { "op": "attr.set", "key": "call", "value": "Sas" },
            { "op": "attr.set", "key": "hobby", "label": "爱好", "value": "攀岩", "level": "public" },
            { "op": "attr.set", "key": "Bad Key", "value": "x" },
            { "op": "account.add", "system": "mail", "handle": "s@x.com" },
            { "op": "nonsense" },
            "not an object"
        ]),
    )
    .await;
    assert_eq!(
        lines(&p),
        vec![
            line("和你的关系", "客户", "朋友、客户"),
            line("重要", "否", "是"),
            line("备注（追加）", "老客户", "喜欢周五开会"),
            line("电话", "+1 111", "+1 222"),
            line("偏好的称呼", "（空）", "Sas"),
            line("爱好", "（空）", "攀岩"),
        ]
    );
    let ops = stored_ops(&t, &p.id);
    assert_eq!(
        ops[0],
        json!({ "op": "party.patch", "id": w.sarah, "rels": ["friend", "customer"], "important": true, "note": "老客户\n喜欢周五开会" })
    );

    apply(&t, &p.id).unwrap();
    let c = ok(&r, "GET", &format!("/contacts/{}", w.sarah), Value::Null).await;
    assert_eq!(c["rels"], json!(["friend", "customer"]));
    assert_eq!(c["important"], true);
    assert_eq!(c["note"], "老客户\n喜欢周五开会");
    let attrs = c["attrs"].as_array().unwrap();
    let get = |k: &str, s: &str| {
        attrs
            .iter()
            .filter(|a| a["key"] == k && a["status"] == s)
            .count()
    };
    assert_eq!((get("phone", "confirmed"), get("phone", "stale")), (1, 1));
    let call_attr = attrs.iter().find(|a| a["key"] == "call").unwrap();
    assert_eq!(
        (
            call_attr["level"].as_str(),
            call_attr["source_label"].as_str()
        ),
        (Some("public"), Some("你说的"))
    );
    let hobby = attrs.iter().find(|a| a["key"] == "hobby").unwrap();
    assert_eq!(
        (hobby["label"].as_str(), hobby["level"].as_str()),
        (Some("爱好"), Some("public"))
    );

    undo(&t, &p.id);
    assert_eq!(
        state(&t),
        before,
        "old phone confirmed again, new definition gone"
    );
}

#[tokio::test]
async fn me_keeps_its_relation_and_accounts_are_offered_with_the_module() {
    let mut ports = Ports::default();
    accounts::plug(&mut ports);
    let t = env_with(ports);
    let r = app(&t);
    let w = world(&r).await;
    // account.add's validation needs the built-in systems, which the accounts routes seed on first use
    let acc = accounts::routes().with_state(t.modules.clone());
    ok(&acc, "GET", "/systems", Value::Null).await;
    let mine = id(&ok(&r, "GET", "/me/contact", Value::Null).await);
    let p = draft(
        &t,
        "contact",
        Some("me"),
        "我是 Sarah 的朋友",
        json!([{ "op": "set", "key": "rels", "value": ["朋友"] }, { "op": "set", "key": "aliases", "value": "小李、Lee" }]),
    )
    .await;
    assert!(!t.gateway.last_prompt().unwrap().contains("和你的关系"));
    assert_eq!(p.href, "#/contacts/me");
    assert_eq!(lines(&p), vec![line("别名", "（空）", "小李、Lee")]);
    apply(&t, &p.id).unwrap();
    let c = ok(&r, "GET", "/me/contact", Value::Null).await;
    assert_eq!(
        (c["id"].as_str(), c["rels"].clone()),
        (Some(mine.as_str()), json!(["me"]))
    );

    let p = draft(
        &t,
        "contact",
        Some(&w.sarah),
        "Sarah 的邮箱是 sarah@acme.com，银行卡 6222…",
        json!([
            { "op": "account.add", "system": "mail", "handle": "sarah@acme.com" },
            { "op": "account.add", "system": "bank", "handle": "6222000011112222" }
        ]),
    )
    .await;
    assert!(t.gateway.last_prompt().unwrap().contains("account.add · "));
    assert_eq!(lines(&p), vec![line("新账号", "—", "邮箱 sarah@acme.com")]);
    apply(&t, &p.id).unwrap();
    let n: i64 = scalar(
        &t,
        &format!(
            "SELECT COUNT(*) FROM accounts WHERE party_id = '{}'",
            w.sarah
        ),
    );
    assert_eq!(n, 1);
    undo(&t, &p.id);
    let n: i64 = scalar(
        &t,
        &format!(
            "SELECT COUNT(*) FROM accounts WHERE party_id = '{}'",
            w.sarah
        ),
    );
    assert_eq!(n, 0);
}

// ---------------------------------------------------------------- staleness, reuse, references

#[tokio::test]
async fn edited_target_makes_the_draft_stale() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let p = draft(
        &t,
        "contact",
        Some(&w.lilei),
        "李雷是同事",
        json!([{ "op": "set", "key": "rels", "value": ["同事"] }]),
    )
    .await;
    ok(
        &r,
        "PATCH",
        &format!("/contacts/{}", w.lilei),
        json!({ "important": true }),
    )
    .await;
    assert_eq!(
        apply(&t, &p.id).unwrap_err(),
        ApiError::conflict(changes::STALE)
    );

    // another workspace's contact is not a target
    let other = changes::draft(
        &t.modules,
        &caller("w2", ME),
        "contact",
        Some(&w.lilei),
        "x",
        Origin::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(other, ApiError::not_found("联系人"));
}

#[tokio::test]
async fn org_created_meanwhile_is_reused_and_kept_on_undo_when_referenced() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let p = draft(
        &t,
        "contact",
        Some(&w.lilei),
        "李雷在 Hooli 做工程师",
        json!([{ "op": "job.open", "org": "Hooli", "title": "工程师" }]),
    )
    .await;
    assert_eq!(
        lines(&p),
        vec![line("任职", "—", "Hooli · 工程师（新建组织）")]
    );
    // someone creates Hooli by hand before the draft is applied: it is reused, not duplicated
    let hooli = id(&ok(&r, "POST", "/orgs", json!({ "name": "hooli" })).await);
    apply(&t, &p.id).unwrap();
    let n: i64 = scalar(
        &t,
        "SELECT COUNT(*) FROM parties WHERE lower(name) = 'hooli'",
    );
    assert_eq!(n, 1);
    let c = ok(&r, "GET", &format!("/contacts/{}", w.lilei), Value::Null).await;
    assert_eq!(c["org"]["id"], hooli);
    undo(&t, &p.id);
    let status: String = scalar(
        &t,
        &format!("SELECT status FROM parties WHERE id = '{hooli}'"),
    );
    assert_eq!(status, "active", "a reused organization stays");

    // a created organization that got referenced meanwhile is retired, not purged
    let p = draft(
        &t,
        "contact",
        Some(&w.wang),
        "王芳去了 Pied Piper",
        json!([{ "op": "job.open", "org": "Pied Piper" }]),
    )
    .await;
    apply(&t, &p.id).unwrap();
    let pp: String = scalar(&t, "SELECT id FROM parties WHERE name = 'Pied Piper'");
    ok(
        &r,
        "POST",
        &format!("/contacts/{}/jobs", w.lilei),
        json!({ "org_id": pp }),
    )
    .await;
    undo(&t, &p.id);
    let status: String = scalar(&t, &format!("SELECT status FROM parties WHERE id = '{pp}'"));
    assert_eq!(status, "deleted");
    let _ = w.acme;
}

// ---------------------------------------------------------------- org and team targets

#[tokio::test]
async fn org_fields_and_domains() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    ok(
        &r,
        "POST",
        "/orgs",
        json!({ "name": "Globex", "domains": ["globex.com"] }),
    )
    .await;
    let before = state(&t);
    let p = draft(
        &t,
        "org",
        Some(&w.acme),
        "Acme 现在是合作方，域名 acme.com 和 www.Acme.io，也叫 ACME Corp，他们年底换 CEO",
        json!([
            { "op": "set", "key": "kind", "value": "合作方" },
            { "op": "set", "key": "domains", "value": ["acme.com", "www.Acme.io"] },
            { "op": "set", "key": "aliases", "value": ["ACME Corp"] },
            { "op": "set", "key": "note", "value": "年底换 CEO" }
        ]),
    )
    .await;
    assert_eq!(p.title, "Acme 的资料");
    assert_eq!(p.href, format!("#/orgs/{}", w.acme));
    assert_eq!(
        lines(&p),
        vec![
            line("类型", "客户", "合作方"),
            line("域名", "（空）", "acme.com、acme.io"),
            line("别名", "（空）", "ACME Corp"),
            line("备注（追加）", "（空）", "年底换 CEO"),
        ]
    );
    apply(&t, &p.id).unwrap();
    let o = ok(&r, "GET", &format!("/orgs/{}", w.acme), Value::Null).await;
    assert_eq!(
        (o["kind"].clone(), o["domains"].clone()),
        (json!("partner"), json!(["acme.com", "acme.io"]))
    );
    undo(&t, &p.id);
    assert_eq!(state(&t), before);

    // a domain of another organization is shown, not planned
    let p = draft(
        &t,
        "org",
        Some(&w.acme),
        "acme 也用 globex.com",
        json!([{ "op": "set", "key": "domains", "value": ["globex.com"] }]),
    )
    .await;
    assert_eq!(
        lines(&p),
        vec![line(
            "（域名 globex.com）",
            "—",
            "这个域名已经属于「Globex」"
        )]
    );
    assert!(stored_ops(&t, &p.id).is_empty());
}

#[tokio::test]
async fn team_members_purpose_and_org_chart_limits() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let tm = id(&ok(
        &r,
        "POST",
        "/teams",
        json!({ "name": "前端小组", "purpose": "做 web", "members": [{ "contact_id": "me" }, { "contact_id": w.lilei }] }),
    )
    .await);
    let before = state(&t);
    let p = draft(
        &t,
        "team",
        Some(&tm),
        "把王芳和 Sarah 加进来，李雷移出，改名叫 Web 组，也负责手机端",
        json!([
            { "op": "members.add", "names": ["王芳", "Sarah", "我"] },
            { "op": "members.remove", "names": ["李雷"] },
            { "op": "set", "key": "name", "value": "Web 组" },
            { "op": "set", "key": "purpose", "value": "也负责手机端" }
        ]),
    )
    .await;
    assert_eq!(p.title, "团队「前端小组」");
    assert_eq!(
        lines(&p),
        vec![
            line("名字", "前端小组", "Web 组"),
            line("用途（追加）", "做 web", "也负责手机端"),
            line("成员", "2 人", "3 人（加入 王芳、Sarah Chen；移出 李雷）"),
        ]
    );
    let ops: Vec<String> = stored_ops(&t, &p.id)
        .iter()
        .map(|o| o["op"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ops, ["team.patch", "team.members"]);
    apply(&t, &p.id).unwrap();
    let team = ok(&r, "GET", &format!("/teams/{tm}"), Value::Null).await;
    assert_eq!(
        (team["name"].as_str(), team["purpose"].as_str()),
        (Some("Web 组"), Some("做 web\n也负责手机端"))
    );
    assert_eq!(team["headcount"], 3);
    undo(&t, &p.id);
    assert_eq!(state(&t), before);

    // org-chart team: only purpose
    exec(
        &t,
        "INSERT INTO parties (id, workspace_id, type, name, source, created_by, created, updated) VALUES ('tm_rd', 'default', 'team', '研发', 'sync', 'usr_dev', 1, 1);
         INSERT INTO team_details (party_id, workspace_id, kind, sync_connection, external_id, synced) VALUES ('tm_rd', 'default', 'chart', 'con_fs', 'dep_rd', 1);",
    );
    let p = draft(
        &t,
        "team",
        Some("tm_rd"),
        "研发改名叫 R&D，加上李雷，负责 Steve",
        json!([
            { "op": "set", "key": "name", "value": "R&D" },
            { "op": "members.add", "names": ["李雷"] },
            { "op": "set", "key": "purpose", "value": "负责 Steve" }
        ]),
    )
    .await;
    let prompt = t.gateway.last_prompt().unwrap();
    assert!(!prompt.contains("members.add"), "{prompt}");
    assert_eq!(
        lines(&p),
        vec![
            line(
                "（不能改：名字）",
                "—",
                "组织架构团队跟随同步，不能改这一项"
            ),
            line(
                "（不能改：成员 李雷）",
                "—",
                "组织架构团队跟随同步，不能改这一项"
            ),
            line("用途（追加）", "（空）", "负责 Steve"),
        ]
    );
    assert_eq!(
        stored_ops(&t, &p.id),
        vec![json!({ "op": "team.patch", "id": "tm_rd", "purpose": "负责 Steve" })]
    );
    apply(&t, &p.id).unwrap();
    let purpose: String = scalar(
        &t,
        "SELECT purpose FROM team_details WHERE party_id = 'tm_rd'",
    );
    assert_eq!(purpose, "负责 Steve");
}

#[tokio::test]
async fn global_candidates() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let tm = id(&ok(
        &r,
        "POST",
        "/teams",
        json!({ "name": "前端小组", "members": [{ "contact_id": "me" }, { "contact_id": w.lilei }] }),
    )
    .await);
    let lines: Vec<String> = changes::candidates(&t.modules, &me())
        .unwrap()
        .iter()
        .map(|c| c.line())
        .collect();
    assert!(
        lines.contains(&format!("contact:{} — Sarah Chen（Acme）", w.sarah)),
        "{lines:?}"
    );
    assert!(
        lines.contains(&format!("contact:{} — 李雷", w.lilei)),
        "{lines:?}"
    );
    assert!(
        lines.contains(&format!("team:{tm} — 前端小组")),
        "{lines:?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("team:new — 新建虚拟团队")
    );
}

// ---------------------------------------------------------------- language

/// A draft is stored for every member to read: its card is zh-CN whatever the asker's language, and an error
/// still comes back in the asker's language.
#[tokio::test]
async fn a_draft_is_stored_in_zh_cn_from_an_english_request() {
    let t = env();
    let r = app(&t);
    let w = world(&r).await;
    let p = steve_i18n::scope(
        steve_i18n::EN,
        draft(
            &t,
            "contact",
            Some(&w.sarah),
            "Sarah is important",
            json!([{ "op": "set", "key": "important", "value": "是" }]),
        ),
    )
    .await;
    assert_eq!(p.title, "Sarah Chen 的资料");
    assert_eq!(lines(&p), vec![line("重要", "否", "是")]);
    let title: String = scalar(
        &t,
        &format!("SELECT title FROM changesets WHERE id = '{}'", p.id),
    );
    assert_eq!(title, "Sarah Chen 的资料");
    let e = steve_i18n::scope(
        steve_i18n::EN,
        changes::draft(
            &t.modules,
            &me(),
            "contact",
            Some("ct_nope"),
            "x",
            Origin::default(),
        ),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ApiError::NotFound(_)), "{e:?}");
}

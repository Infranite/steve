//! C2 acceptance: the default persona is seeded; a space's own binding
//! overrides the account default; deleting a persona moves accounts to the default persona and
//! spaces back to following; your own account cannot get a persona (400). Plus the `/personas`
//! routes (validation, ownership, revisions, `intro_preview`, `used_by`) and `on_bot_deleted`.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_channels::bindings::{self, SELF_NO_PERSONA};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::personas;
use steve_channels::{Channels, SpaceKind, api};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::ChangeEvent;
use steve_store::base::Viewer;
use tempfile::TempDir;

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    api: Router,
    con: String,
}

fn setup() -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '沉稳、简洁，先说结论；不客套。', 1),
                    ('ops', 'default', '运维', 0, 'claude', '', 1),
                    ('far', 'w2', '别处', 0, 'claude', '', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES
                    ('steve', 'default', 1), ('ops', 'default', 2), ('far', 'w2', 1);",
            )?;
            Ok(())
        })
        .expect("bots");
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    let fake = Arc::new(FakeChannel::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(fake.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
        .adapter(fake.clone())
        .build();
    let api = api::routes(ch.clone()).with_state(env.modules.clone());
    let (con, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("connection");
    fake.space(
        "G1",
        "运维群",
        SpaceKind::Group,
        false,
        &[("ualice", "王小丽")],
    );
    fake.space("D1", "", SpaceKind::Dm, false, &[("ualice", "王小丽")]);
    H {
        env,
        _keys: keys,
        ch,
        api,
        con,
    }
}

fn me() -> steve_modules::Caller {
    caller(WS, ME)
}

async fn space_of(h: &H, conn: &str, ext: &str, msg: &str) -> String {
    h.ch.on_inbound(WS, conn, FakeChannel::message(ext, msg, "ualice", "你好"))
        .await
        .expect("inbound")
        .space_id
}

async fn get(h: &H, path: &str) -> Value {
    let (s, v) = call(&h.api, Some(&me()), "GET", path, None).await;
    assert_eq!(s, 200, "{path} → {v}");
    v
}

async fn new_persona(h: &H, name: &str, bot: &str) -> String {
    let (s, v) = call(
        &h.api,
        Some(&me()),
        "POST",
        "/personas",
        Some(&json!({"name": name, "bot_id": bot, "intro": format!("我是{name}，负责运维群的告警排查。")})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    v["id"].as_str().expect("id").to_string()
}

fn set_default(h: &H, conn: &str, persona: &str) -> Result<(), steve_channels::ChannelError> {
    let v = Viewer::bare(WS, ME);
    let now = h.ch.now();
    h.env
        .store()
        .transaction(|tx| bindings::set_account_default(tx, &v, conn, persona, now))
}

#[tokio::test(flavor = "multi_thread")]
async fn default_persona_is_seeded_on_first_use() {
    let h = setup();
    assert!(
        h.env
            .store()
            .reader(|c| personas::default_persona(c, WS))
            .expect("read")
            .is_none(),
        "nothing before channels are used"
    );
    // first inbound message on the account: channel settings and the default persona
    let space = space_of(&h, &h.con, "G1", "m1").await;
    let s = get(&h, &format!("/spaces/{space}")).await;
    let list = get(&h, "/personas").await;
    let list = list.as_array().expect("array");
    assert_eq!(list.len(), 1);
    let p = &list[0];
    assert_eq!(p["name"], "Steve");
    assert_eq!(p["is_default"], true);
    assert_eq!(p["bot_id"], "steve");
    assert_eq!(p["avatar"], json!({"seed": "Steve"}));
    assert_eq!(p["intro"], "我是 Steve，负责接收和分派事情。");
    assert_eq!(p["soul"], "沉稳、简洁，先说结论；不客套。");
    assert_eq!(
        p["reveal"],
        json!({"name": true, "role": true, "ai": true, "steve": true, "owner": true})
    );
    assert_eq!(
        p["intro_preview"],
        "我是Steve，负责接收和分派事情，我是 AI 助手，由 Steve 驱动，归 Dev 管理。"
    );
    // the bot account and its space speak as Steve (nothing bound: inherited)
    assert_eq!(
        s["persona"],
        json!({"id": p["id"], "name": "Steve", "inherited": true})
    );
    assert_eq!(p["used_by"]["accounts"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        p["used_by"]["spaces"],
        json!([{"id": space, "name": "运维群", "own": false}])
    );
    // seeded once
    get(&h, "/personas").await;
    let n: i64 = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT count(*) FROM personas WHERE workspace_id = ? AND is_default = 1",
                [WS],
                |r| r.get(0),
            )?)
        })
        .expect("count");
    assert_eq!(n, 1);
    // a workspace without channels gets it from GET /personas, with its own Bot
    let (s, v) = call(&h.api, Some(&caller(WS2, ME)), "GET", "/personas", None).await;
    assert_eq!(s, 200);
    assert_eq!(v[0]["bot_id"], "far");
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_default_intro_uses_current_scoped_address_without_rewriting_custom_text() {
    use steve_modules::contacts::{repo, service};

    let h = setup();
    let space = space_of(&h, &h.con, "G1", "legacy").await;
    let default = h
        .env
        .store()
        .reader(|c| personas::default_persona(c, WS))
        .unwrap()
        .unwrap();
    h.env
        .store()
        .transaction(|tx| -> steve_modules::ApiResult<()> {
            let me = service::ensure_base(tx, &caller(WS, ME))?;
            for (key, value) in [
                ("call", "Private nickname".to_string()),
                ("call_external", "Current public name".to_string()),
                (
                    "call_ext_scope",
                    json!({"kind": "persona", "id": default.id, "name": "Persona name"})
                        .to_string(),
                ),
                (
                    "call_ext_scope",
                    json!({"kind": "space", "id": space, "name": "Space name"}).to_string(),
                ),
            ] {
                repo::value_insert(
                    tx,
                    WS,
                    &repo::NewValue {
                        party_id: me.id.clone(),
                        key: key.into(),
                        label: key.into(),
                        value,
                        source: "me".into(),
                        source_label: "Manual".into(),
                        source_ref: None,
                        level: steve_store::base::Level::Public,
                        status: "confirmed".into(),
                        decision_id: None,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    let custom_id = new_persona(&h, "Custom", "ops").await;
    for legacy in [
        "我是 Steve，Old legal name 的 AI 助手，帮他接收和分派事情。",
        "I'm Steve, Old legal name's AI assistant; I take in and hand out their work.",
    ] {
        h.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "UPDATE personas SET intro = ?1 WHERE id IN (?2, ?3)",
                    rusqlite::params![legacy, default.id, custom_id],
                )?;
                Ok(())
            })
            .unwrap();
        h.env
            .store()
            .reader(|c| -> steve_modules::ApiResult<()> {
                // Every reader used by bindings, previews and explicit persona drafts upgrades the seed.
                let p = personas::get(c, &default.id)?.unwrap();
                assert_eq!(p.intro, steve_store::seeded::persona_intro());
                assert_eq!(personas::default_persona(c, WS)?.unwrap().intro, p.intro);
                assert_eq!(personas::list(c, WS)?[0].intro, p.intro);
                let preview = personas::out(c, &Viewer::bare(WS, ME), &p)?.intro_preview;
                assert!(preview.contains("归 Persona name 管理"), "{preview}");
                let owner = personas::owner_name_in(c, WS, Some(&p.id), Some(&space))?;
                let layer = steve_channels::compose::persona_layer(&p, &owner);
                assert!(layer.contains("归 Space name 管理"), "{layer}");
                for text in [&preview, &layer] {
                    assert!(
                        !text.contains("Old legal name") && !text.contains("Private nickname"),
                        "{text}"
                    );
                }
                assert_eq!(
                    personas::get(c, &custom_id)?.unwrap().intro,
                    legacy,
                    "non-default personas stay user-owned"
                );
                let stored: String = c.query_row(
                    "SELECT intro FROM personas WHERE id = ?",
                    [&default.id],
                    |r| r.get(0),
                )?;
                assert_eq!(
                    stored, legacy,
                    "read-time upgrade does not rewrite stored text"
                );
                Ok(())
            })
            .unwrap();
        let ensured = h
            .env
            .store()
            .transaction(|tx| personas::ensure_default(tx, WS, h.ch.now()))
            .unwrap()
            .unwrap();
        assert_eq!(ensured.intro, steve_store::seeded::persona_intro());
    }
    // A custom default intro is left alone, including one deliberately written in the old seed shape.
    for custom in [
        "我是 Steve，Old legal name 的 AI 助手，帮他接收和分派事情。另有自定义职责。",
        "我是 Steve，Custom name 的 AI 助手，帮他接收和分派事情。",
    ] {
        h.env
            .store()
            .transaction(|tx| {
                personas::patch(
                    tx,
                    &Viewer::bare(WS, ME),
                    &default.id,
                    &personas::PersonaPatch {
                        intro: Some(custom.into()),
                        ..Default::default()
                    },
                    h.ch.now(),
                )
            })
            .unwrap();
        let p = h
            .env
            .store()
            .reader(|c| personas::default_persona(c, WS))
            .unwrap()
            .unwrap();
        assert_eq!(p.intro, custom);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn space_binding_overrides_the_account_default() {
    let h = setup();
    let g1 = space_of(&h, &h.con, "G1", "m1").await;
    let d1 = space_of(&h, &h.con, "D1", "m2").await;
    let oncall = new_persona(&h, "值班小运", "ops").await;
    let pm = new_persona(&h, "需求官", "steve").await;

    set_default(&h, &h.con, &oncall).expect("bind");
    let acc = h
        .env
        .store()
        .reader(|c| bindings::account_persona(c, WS, &h.con))
        .expect("account persona")
        .expect("bot");
    assert_eq!((acc.id.as_str(), acc.inherited), (oncall.as_str(), None));
    for s in [&g1, &d1] {
        assert_eq!(
            get(&h, &format!("/spaces/{s}")).await["persona"],
            json!({"id": oncall, "name": "值班小运", "inherited": true})
        );
    }

    // the group gets its own persona; the DM keeps following the account
    let (s, v) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{g1}"),
        Some(&json!({"persona_id": pm})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v["persona"],
        json!({"id": pm, "name": "需求官", "inherited": false})
    );
    assert_eq!(v["configured"], true);
    assert_eq!(
        get(&h, &format!("/spaces/{d1}")).await["persona"]["id"],
        json!(oncall)
    );
    let used = get(&h, &format!("/personas/{pm}")).await["used_by"].clone();
    assert_eq!(
        used,
        json!({"accounts": [], "spaces": [{"id": g1, "name": "运维群", "own": true}]})
    );
    // the revision records the binding
    let patch: String = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT patch FROM revisions WHERE object_type = 'space' AND object_id = ? ORDER BY id DESC LIMIT 1",
                [&g1],
                |r| r.get(0),
            )?)
        })
        .expect("revision");
    let patch: Value = serde_json::from_str(&patch).expect("json");
    assert_eq!(patch["before"]["persona_id"], Value::Null);
    assert_eq!(patch["after"]["persona_id"], json!(pm));

    // null = follow the account again; absent = unchanged
    let (s, v) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{g1}"),
        Some(&json!({"persona_id": null})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(
        v["persona"],
        json!({"id": oncall, "name": "值班小运", "inherited": true})
    );
    let (_, v) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{g1}"),
        Some(&json!({"duty": "答疑"})),
    )
    .await;
    assert_eq!(v["persona"]["id"], json!(oncall));

    // a persona that is not there, or of another workspace → 404
    let (s, v) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{g1}"),
        Some(&json!({"persona_id": "per_nope"})),
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("没有这个对外身份")));
    let (_, far) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "POST",
        "/personas",
        Some(&json!({"name": "别处", "bot_id": "far"})),
    )
    .await;
    let (s, _) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{g1}"),
        Some(&json!({"persona_id": far["id"]})),
    )
    .await;
    assert_eq!(s, 404);
    assert_eq!(
        set_default(&h, &h.con, far["id"].as_str().unwrap()).map_err(|e| e.status()),
        Err(404)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_persona_moves_accounts_to_default_and_spaces_to_following() {
    let h = setup();
    let g1 = space_of(&h, &h.con, "G1", "m1").await;
    let d1 = space_of(&h, &h.con, "D1", "m2").await;
    let oncall = new_persona(&h, "值班小运", "ops").await;
    let pm = new_persona(&h, "需求官", "steve").await;
    set_default(&h, &h.con, &oncall).expect("bind");
    for (s, p) in [(&g1, &pm), (&d1, &oncall)] {
        let (st, _) = call(
            &h.api,
            Some(&me()),
            "PATCH",
            &format!("/spaces/{s}"),
            Some(&json!({"persona_id": p})),
        )
        .await;
        assert_eq!(st, 200);
    }
    let steve = get(&h, "/personas").await[0]["id"].clone();
    h.env.take_events();

    let (s, v) = call(
        &h.api,
        Some(&me()),
        "DELETE",
        &format!("/personas/{oncall}"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v,
        json!({"ok": true, "moved": {"accounts": 1, "spaces": 1}})
    );
    let ev = h.env.take_events();
    assert!(ev.contains(&ChangeEvent::Persona {
        id: oncall.clone(),
        ws: WS.into()
    }));
    assert!(ev.contains(&ChangeEvent::Channel {
        id: h.con.clone(),
        ws: WS.into()
    }));
    assert!(ev.contains(&ChangeEvent::Space {
        id: d1.clone(),
        ws: WS.into()
    }));

    // the account speaks as Steve; the DM follows it; the group keeps its own persona
    assert_eq!(
        get(&h, &format!("/spaces/{d1}")).await["persona"],
        json!({"id": steve, "name": "Steve", "inherited": true})
    );
    assert_eq!(
        get(&h, &format!("/spaces/{g1}")).await["persona"]["id"],
        json!(pm)
    );
    let acc = h
        .env
        .store()
        .reader(|c| bindings::account_persona(c, WS, &h.con))
        .expect("read")
        .expect("bot");
    assert_eq!(json!(acc.id), steve);
    // gone from lists and lookups
    let (s, _) = call(
        &h.api,
        Some(&me()),
        "GET",
        &format!("/personas/{oncall}"),
        None,
    )
    .await;
    assert_eq!(s, 404);
    let names: Vec<Value> = get(&h, "/personas")
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("Steve"), json!("需求官")]);
    let (s, _) = call(
        &h.api,
        Some(&me()),
        "DELETE",
        &format!("/personas/{oncall}"),
        None,
    )
    .await;
    assert_eq!(s, 404);

    // the default persona cannot be deleted
    let (s, v) = call(
        &h.api,
        Some(&me()),
        "DELETE",
        &format!("/personas/{}", steve.as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("默认身份不能删除")));
}

#[tokio::test(flavor = "multi_thread")]
async fn own_account_cannot_get_a_persona() {
    let h = setup();
    let (mine, _) =
        FakeChannel::seed_connection(h.env.store(), &me(), Some("ume")).expect("personal");
    let space = space_of(&h, &mine, "G1", "p1").await;
    let pm = new_persona(&h, "需求官", "steve").await;

    let s = get(&h, &format!("/spaces/{space}")).await;
    assert_eq!(s["persona"], Value::Null, "you speak as yourself");
    let (st, v) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"persona_id": pm})),
    )
    .await;
    assert_eq!((st, v["error"].as_str()), (400, Some(SELF_NO_PERSONA)));
    // nothing written: still not configured
    assert_eq!(
        get(&h, &format!("/spaces/{space}")).await["configured"],
        false
    );
    // clearing is harmless
    let (st, _) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"persona_id": null})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(
        set_default(&h, &mine, &pm).map_err(|e| (e.status(), e.to_string())),
        Err((400, SELF_NO_PERSONA.to_string()))
    );
    assert!(
        h.env
            .store()
            .reader(|c| bindings::account_persona(c, WS, &mine))
            .expect("read")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn persona_routes_validate_and_keep_revisions() {
    let h = setup();
    let who = me();
    let post = |b: Value| {
        let api = h.api.clone();
        let who = who.clone();
        async move { call(&api, Some(&who), "POST", "/personas", Some(&b)).await }
    };
    // validation
    let (s, v) = post(json!({"name": "", "bot_id": "ops"})).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：name")));
    let (s, _) =
        post(json!({"name": "一二三四五六七八九十一二三四五六七八九十一", "bot_id": "ops"})).await;
    assert_eq!(s, 400);
    let (s, v) = post(json!({"name": "x", "bot_id": "ops", "signature": "长".repeat(61)})).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：signature")));
    let (s, v) = post(json!({"name": "x"})).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：bot_id")));
    let (s, _) = post(json!({"name": "x", "bot_id": "ops", "colour": 1})).await;
    assert_eq!(s, 400);
    // ownBot: another workspace's Bot, or none at all
    for bot in ["far", "nope"] {
        let (s, v) = post(json!({"name": "x", "bot_id": bot})).await;
        assert_eq!((s, v["error"].as_str()), (404, Some("没有这个 Bot")));
    }

    // created with the new-persona reveal defaults; the default persona is seeded alongside
    let (s, p) = post(json!({"name": " 值班小运 ", "bot_id": "ops",
        "intro": "我是值班小运，负责运维群的告警排查和部署答疑。", "signature": "— 小运（AI 值班）"}))
    .await;
    assert_eq!(s, 200, "{p}");
    let id = p["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("per_"));
    assert_eq!(p["name"], "值班小运");
    assert_eq!(p["avatar"], json!({"seed": "值班小运"}));
    assert_eq!(
        p["reveal"],
        json!({"name": true, "role": true, "ai": true, "steve": false, "owner": false})
    );
    assert_eq!(
        p["intro_preview"],
        "我是值班小运，负责运维群的告警排查和部署答疑，我是 AI 助手。"
    );
    assert_eq!(p["is_default"], false);
    assert_eq!(p["revision"], 1);
    assert_eq!(get(&h, "/personas").await.as_array().map(Vec::len), Some(2));

    // patch: partial reveal, another Bot; a revision each time
    let (s, v) = call(
        &h.api,
        Some(&who),
        "PATCH",
        &format!("/personas/{id}"),
        Some(&json!({"reveal": {"ai": false, "owner": true}, "bot_id": "steve"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["bot_id"], "steve");
    assert_eq!(v["revision"], 2);
    assert_eq!(
        v["intro_preview"],
        "我是值班小运，负责运维群的告警排查和部署答疑，归 Dev 管理。"
    );
    let (s, _) = call(
        &h.api,
        Some(&who),
        "PATCH",
        &format!("/personas/{id}"),
        Some(&json!({"bot_id": "far"})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(
        &h.api,
        Some(&who),
        "PATCH",
        &format!("/personas/{id}"),
        Some(&json!({"reveal": {"ai": "no"}})),
    )
    .await;
    assert_eq!(s, 400);
    let revs: Vec<i64> = h
        .env
        .store()
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT version FROM revisions WHERE object_type = 'persona' AND object_id = ? ORDER BY id",
            )?;
            Ok::<_, steve_store::StoreError>(
                st.query_map([&id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            )
        })
        .expect("revisions");
    assert_eq!(revs, vec![1, 2]);

    // another workspace, another user of this one
    for (who, code) in [(caller(WS2, ME), 404), (caller(WS, OTHER), 200)] {
        let (s, _) = call(&h.api, Some(&who), "GET", &format!("/personas/{id}"), None).await;
        assert_eq!(s, code);
    }
    let (s, _) = call(&h.api, None, "GET", "/personas", None).await;
    assert_eq!(s, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn deleted_bot_hands_its_personas_to_the_heir() {
    let h = setup();
    let id = new_persona(&h, "值班小运", "ops").await;
    let now = h.ch.now();
    let n = h
        .env
        .store()
        .transaction(|tx| personas::on_bot_deleted(tx, WS, "ops", "steve", now))
        .expect("moved");
    assert_eq!(n, 1);
    let p = get(&h, &format!("/personas/{id}")).await;
    assert_eq!(
        (p["bot_id"].as_str(), p["revision"].as_i64()),
        (Some("steve"), Some(2))
    );
}

//! C8 acceptance: the two examples of §7 with a fixed model output —
//! change rows, apply, undo — plus `409` on a stale draft, your own account refusing a persona (one
//! line with the reason, no operation) and `reply`, the channel target (persona, rules, duty,
//! trigger words / where), names that are not recognized, and the `global` candidates. The model is
//! always the scripted gateway; nothing real is called.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_channels::bindings::{self, SELF_NO_PERSONA};
use steve_channels::drafts::{self, DISCLOSURE_LABEL, TRIGGER_SELF_ONLY};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::rules::SELF_NO_REPLY;
use steve_channels::{Channels, SpaceKind, api, personas, spaces};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::changes::{self, Origin, Proposal, STALE};
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{ApiError, Caller, SystemClock};
use steve_store::base::changesets;
use steve_store::base::{Change, Viewer};
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
        drafts::register(r, Arc::new(SystemClock));
    });
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '沉稳、简洁。', 1),
                    ('ops', 'default', '运维', 0, 'claude', '', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES
                    ('steve', 'default', 1), ('ops', 'default', 2);
                 INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_ops', 'default', '运维', '领域', '', 'usr_dev', 1);",
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
    fake.space(
        "G2",
        "Acme 交付群",
        SpaceKind::Group,
        true,
        &[("ualice", "王小丽")],
    );
    H {
        env,
        _keys: keys,
        ch,
        api,
        con,
    }
}

fn me() -> Caller {
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

async fn new_persona(h: &H, body: Value) -> String {
    let (s, v) = call(&h.api, Some(&me()), "POST", "/personas", Some(&body)).await;
    assert_eq!(s, 200, "{v}");
    v["id"].as_str().expect("id").to_string()
}

/// One draft with a fixed model reply.
async fn draft(h: &H, kind: &str, id: &str, request: &str, reply: Value) -> Proposal {
    h.env.gateway.reply(reply.to_string());
    changes::draft(
        &h.env.modules,
        &me(),
        kind,
        Some(id),
        request,
        Origin::default(),
    )
    .await
    .expect("draft")
}

fn ops_of(h: &H, id: &str) -> Vec<Value> {
    h.env
        .store()
        .reader(|c| changesets::get(c, id))
        .expect("read")
        .expect("changeset")
        .ops
}

fn apply(h: &H, id: &str) -> Result<Value, ApiError> {
    changes::apply(&h.env.modules, &me(), id).expect("stored")
}

fn undo(h: &H, id: &str) -> Result<Value, ApiError> {
    changes::undo(&h.env.modules, &me(), id).expect("stored")
}

fn set(key: &str, value: Value) -> Value {
    json!({"op": "set", "key": key, "value": value})
}

fn line(label: &str, from: &str, to: &str) -> Change {
    Change::new(label, from, to)
}

#[tokio::test(flavor = "multi_thread")]
async fn space_example_rules_disclosure_and_duty() {
    let h = setup();
    let g1 = space_of(&h, &h.con, "G1", "m1").await;
    let before = get(&h, &format!("/spaces/{g1}")).await;
    assert_eq!(before["disclosure"], "conclusion");
    assert_eq!(before["configured"], false);

    let p = draft(
        &h,
        "space",
        &g1,
        "运维群只在被 @ 时回复，这里有客户，不要透露其他群的细节",
        json!({"ops": [
            set("rules", json!([
                {"when": {"kind": "mention_or_dm"}, "act": "reply"},
                {"when": {"kind": "all"}, "act": "record"}
            ])),
            set("disclosure", json!("仅用于推理")),
            set("duty", json!("回复时不引用其他群的细节，只说结论")),
        ]}),
    )
    .await;
    // the prompt carries the target's fields
    let prompt = h.env.gateway.last_prompt().expect("prompt");
    assert!(prompt.contains("the chat space \"运维群\""), "{prompt}");
    assert!(prompt.contains(DISCLOSURE_LABEL));
    assert!(
        prompt.contains("persona ·"),
        "a bot account's space has a persona field"
    );

    assert_eq!(p.title, "运维群 的设置");
    assert_eq!(p.href, format!("#/channels/space/{g1}"));
    assert_eq!(p.target, json!({"kind": "space", "id": g1}));
    assert_eq!(
        p.changes,
        vec![
            line(
                "响应规则",
                "跟随账号（被 @ 或私聊时直接回复）",
                "被 @ 或私聊时直接回复；其他只记录"
            ),
            line(DISCLOSURE_LABEL, "可转述结论", "仅用于推理"),
            line(
                "职责（agents）",
                "（空）",
                "回复时不引用其他群的细节，只说结论（追加）"
            ),
        ]
    );
    let ops = ops_of(&h, &p.id);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["op"], "chan.space.patch");

    // apply
    h.env.take_events();
    assert_eq!(
        apply(&h, &p.id).expect("apply"),
        json!({"ok": true, "undo": p.id})
    );
    let after = get(&h, &format!("/spaces/{g1}")).await;
    assert_eq!(
        after["rules"],
        json!([
            {"when": {"kind": "mention_or_dm"}, "act": "reply"},
            {"when": {"kind": "all"}, "act": "record"}
        ])
    );
    assert_eq!(after["disclosure"], "reasoning");
    assert_eq!(after["duty"], "回复时不引用其他群的细节，只说结论");
    assert_eq!(after["configured"], true);
    assert_eq!(after["revision"], before["revision"].as_i64().unwrap() + 1);
    assert!(
        h.env
            .take_events()
            .iter()
            .any(|e| matches!(e, steve_store::ChangeEvent::Space { id, .. } if *id == g1)),
        "SSE space"
    );
    // applying twice is a conflict
    assert_eq!(apply(&h, &p.id), Err(ApiError::conflict(STALE)));

    // undo puts every patched key back, including “not configured yet”
    assert_eq!(undo(&h, &p.id).expect("undo"), json!({"ok": true}));
    let back = get(&h, &format!("/spaces/{g1}")).await;
    for k in [
        "rules",
        "disclosure",
        "duty",
        "configured",
        "persona",
        "muted",
    ] {
        assert_eq!(back[k], before[k], "{k}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_draft_is_409() {
    let h = setup();
    let g1 = space_of(&h, &h.con, "G1", "m1").await;
    let p = draft(
        &h,
        "space",
        &g1,
        "静默",
        json!({"ops": [set("muted", json!("静默"))]}),
    )
    .await;
    assert_eq!(p.changes, vec![line("工作状态", "工作中", "静默")]);
    // the space changes after drafting
    let (s, _) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{g1}"),
        Some(&json!({"collab": true})),
    )
    .await;
    assert_eq!(s, 200);
    let e = apply(&h, &p.id).expect_err("stale");
    assert_eq!((e.status(), e.to_string()), (409, STALE.to_string()));
    assert_eq!(get(&h, &format!("/spaces/{g1}")).await["muted"], false);

    // same for personas and channels
    let per = new_persona(&h, json!({"name": "值班小运", "bot_id": "ops"})).await;
    let p = draft(
        &h,
        "persona",
        &per,
        "签名改成 — 小运",
        json!({"ops": [set("signature", json!("— 小运"))]}),
    )
    .await;
    let (s, _) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/personas/{per}"),
        Some(&json!({"intro": "我是值班小运。"})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(apply(&h, &p.id).map_err(|e| e.status()), Err(409));

    let p = draft(
        &h,
        "channel",
        &h.con,
        "职责",
        json!({"ops": [set("duty", json!("只管告警"))]}),
    )
    .await;
    let now = h.ch.now();
    h.env
        .store()
        .transaction(|tx| {
            spaces::set_account_rules(
                tx,
                &Viewer::bare(WS, ME),
                &h.con,
                &json!([{"when": {"kind": "all"}, "act": "record"}]),
                now,
            )
        })
        .expect("rules");
    assert_eq!(apply(&h, &p.id).map_err(|e| e.status()), Err(409));
}

#[tokio::test(flavor = "multi_thread")]
async fn persona_example_reveal_and_signature() {
    let h = setup();
    let per = new_persona(
        &h,
        json!({"name": "值班小运", "bot_id": "ops", "intro": "我是值班小运，负责运维群的告警排查。",
               "reveal": {"steve": true}}),
    )
    .await;
    let p = draft(
        &h,
        "persona",
        &per,
        "值班小运不要透露由 Steve 驱动，签名改成 — 运维值班",
        json!({"ops": [set("reveal.steve", json!("隐藏")), set("signature", json!("— 运维值班"))]}),
    )
    .await;
    assert_eq!(p.title, "对外身份「值班小运」");
    assert_eq!(p.href, format!("#/channels/personas/{per}"));
    assert_eq!(
        p.changes,
        vec![
            line("透露：由 Steve 驱动", "透露", "隐藏"),
            line("签名", "无", "— 运维值班"),
        ]
    );
    assert_eq!(
        ops_of(&h, &p.id),
        vec![json!({"op": "chan.persona.patch", "persona_id": per,
                    "patch": {"signature": "— 运维值班", "reveal": {"steve": false}}})]
    );
    let before = get(&h, &format!("/personas/{per}")).await;
    apply(&h, &p.id).expect("apply");
    let after = get(&h, &format!("/personas/{per}")).await;
    assert_eq!(
        after["reveal"],
        json!({"name": true, "role": true, "ai": true, "steve": false, "owner": false})
    );
    assert_eq!(after["signature"], "— 运维值班");
    assert!(
        !after["intro_preview"]
            .as_str()
            .unwrap_or("")
            .contains("Steve 驱动")
    );
    undo(&h, &p.id).expect("undo");
    let back = get(&h, &format!("/personas/{per}")).await;
    for k in ["reveal", "signature", "name", "intro", "soul", "bot_id"] {
        assert_eq!(back[k], before[k], "{k}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn persona_name_bot_and_soul() {
    let h = setup();
    let per = new_persona(
        &h,
        json!({"name": "值班小运", "bot_id": "ops", "soul": "耐心。"}),
    )
    .await;
    let p = draft(
        &h,
        "persona",
        &per,
        "改名叫小运，交给 Steve，说话更简洁",
        json!({"ops": [
            set("name", json!("小运")),
            set("bot", json!("Steve")),
            set("soul", json!("耐心。\n说话更简洁。")),
            set("reveal.ai", json!("透露")),
            {"op": "persona.delete"},
        ]}),
    )
    .await;
    assert_eq!(
        p.changes,
        vec![
            line("名字", "值班小运", "小运"),
            line("背后的 Bot", "运维", "Steve"),
            line("口吻（soul）", "耐心。", "说话更简洁。（追加）"),
            line("（没认出）", "—", "{\"op\":\"persona.delete\"}"),
        ],
        "reveal.ai is already revealed: no line"
    );
    apply(&h, &p.id).expect("apply");
    let v = get(&h, &format!("/personas/{per}")).await;
    assert_eq!(
        (v["name"].as_str(), v["bot_id"].as_str(), v["soul"].as_str()),
        (Some("小运"), Some("steve"), Some("耐心。\n说话更简洁。"))
    );
    undo(&h, &p.id).expect("undo");
    let v = get(&h, &format!("/personas/{per}")).await;
    assert_eq!(
        (v["name"].as_str(), v["bot_id"].as_str(), v["soul"].as_str()),
        (Some("值班小运"), Some("ops"), Some("耐心。"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn your_own_account_refuses_a_persona_and_reply() {
    let h = setup();
    let (mine, _) =
        FakeChannel::seed_connection(h.env.store(), &me(), Some("ume")).expect("personal");
    let space = space_of(&h, &mine, "G1", "p1").await;
    new_persona(&h, json!({"name": "需求官", "bot_id": "steve"})).await;

    // a space of your own account
    let p = draft(
        &h,
        "space",
        &space,
        "这里用需求官说话",
        json!({"ops": [set("persona", json!("需求官"))]}),
    )
    .await;
    assert!(
        !h.env.gateway.last_prompt().unwrap().contains("persona ·"),
        "no persona field for your own account"
    );
    assert_eq!(
        p.changes,
        vec![line(
            "对外身份",
            "你本人",
            &format!("（不能改：{SELF_NO_PERSONA}）")
        )]
    );
    assert!(ops_of(&h, &p.id).is_empty(), "no operation");
    apply(&h, &p.id).expect("apply of nothing");
    assert_eq!(
        get(&h, &format!("/spaces/{space}")).await["configured"],
        false,
        "nothing written"
    );

    // your own account itself: persona refused; `reply` refused like PATCH does
    let p = draft(
        &h,
        "channel",
        &mine,
        "默认用需求官，被 @ 就直接回复",
        json!({"ops": [
            set("persona", json!("需求官")),
            set("rules", json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}])),
        ]}),
    )
    .await;
    assert_eq!(
        p.changes,
        vec![
            line(
                "默认对外身份",
                "你本人",
                &format!("（不能改：{SELF_NO_PERSONA}）")
            ),
            line(
                "默认响应规则",
                "私聊你时起草，等你批准；@你时起草，等你批准；其他只记录",
                &format!("（不能改：{SELF_NO_REPLY}）")
            ),
        ]
    );
    assert!(ops_of(&h, &p.id).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn bot_channel_persona_rules_and_duty() {
    let h = setup();
    let g1 = space_of(&h, &h.con, "G1", "m1").await;
    let oncall = new_persona(&h, json!({"name": "值班小运", "bot_id": "ops"})).await;
    let p = draft(
        &h,
        "channel",
        &h.con,
        "这个机器人默认用值班小运，只在被 @ 时回复，提到告警也回复，负责告警排查",
        json!({"ops": [
            set("persona", json!("值班小运")),
            set("rules", json!([
                {"when": {"kind": "mention_or_dm"}, "act": "reply"},
                {"when": {"kind": "keyword", "words": ["告警"]}, "act": "reply"},
                {"when": {"kind": "other_group"}, "act": "record"}
            ])),
            set("duty", json!("负责告警排查")),
            set("trigger.words", json!(["/bot"])),
        ]}),
    )
    .await;
    assert_eq!(p.title, "账号「测试机器人」");
    assert_eq!(p.href, format!("#/channels/acc/{}", h.con));
    assert_eq!(
        p.changes,
        vec![
            line("默认对外身份", "Steve", "值班小运"),
            line(
                "默认响应规则",
                "被 @ 或私聊时直接回复",
                "被 @ 或私聊时直接回复；消息包含「告警」时直接回复；其他群消息只记录"
            ),
            line("账号默认职责", "（空）", "负责告警排查（追加）"),
            line("触发语词", "—", &format!("（不能改：{TRIGGER_SELF_ONLY}）")),
        ]
    );
    let rev0 = h
        .env
        .store()
        .reader(|c| spaces::channel_settings(c, WS, &h.con))
        .expect("read")
        .expect("settings")
        .revision;
    apply(&h, &p.id).expect("apply");
    let (s, per) = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, ApiError>((
                spaces::channel_settings(c, WS, &h.con)?.expect("settings"),
                bindings::account_persona(c, WS, &h.con)?,
            ))
        })
        .expect("read");
    assert_eq!(s.duty, "负责告警排查");
    assert_eq!(s.rules.as_array().map(Vec::len), Some(3));
    assert_eq!(per.map(|p| p.id), Some(oncall.clone()));
    assert!(s.revision > rev0);
    // the space follows the account: it now speaks as 值班小运
    assert_eq!(
        get(&h, &format!("/spaces/{g1}")).await["persona"]["name"],
        "值班小运"
    );

    undo(&h, &p.id).expect("undo");
    let (s, per) = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, ApiError>((
                spaces::channel_settings(c, WS, &h.con)?.expect("settings"),
                bindings::account_persona(c, WS, &h.con)?,
            ))
        })
        .expect("read");
    assert_eq!(s.duty, "");
    assert_eq!(
        s.rules,
        json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}])
    );
    assert_eq!(per.map(|p| p.name), Some("Steve".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn self_channel_trigger_words_and_where() {
    let h = setup();
    let (mine, _) =
        FakeChannel::seed_connection(h.env.store(), &me(), Some("ume")).expect("personal");
    let g1 = space_of(&h, &mine, "G1", "p1").await;
    let p = draft(
        &h,
        "channel",
        &mine,
        "打开触发语，加上 /steve，在运维群和所有私聊里生效",
        json!({"ops": [
            set("trigger.on", json!("开")),
            set("trigger.words", json!(["/bot", "/steve"])),
            set("trigger.where", json!(["运维群", "所有私聊"])),
        ]}),
    )
    .await;
    assert_eq!(p.title, "账号「Dev」");
    assert_eq!(
        p.changes,
        vec![
            line("触发语", "关", "开"),
            line("触发语词", "/bot", "/bot /steve"),
            line("触发语生效范围", "所有群和私聊", "运维群、所有私聊"),
        ]
    );
    apply(&h, &p.id).expect("apply");
    let t = h
        .env
        .store()
        .reader(|c| spaces::channel_settings(c, WS, &mine))
        .expect("read")
        .expect("settings")
        .trigger;
    assert_eq!(
        t,
        json!({"on": true, "words": ["/bot", "/steve"], "where": [g1, "dm:*"], "mark": true})
    );
    undo(&h, &p.id).expect("undo");
    let t = h
        .env
        .store()
        .reader(|c| spaces::channel_settings(c, WS, &mine))
        .expect("read")
        .expect("settings")
        .trigger;
    assert_eq!(t["on"], false);
    // nothing chosen before reads (and comes back) as everywhere
    assert_eq!(t["where"], json!(["*"]));

    // a bad word and an unknown group
    let p = draft(
        &h,
        "channel",
        &mine,
        "触发语改成 bot，加上财务群",
        json!({"ops": [
            set("trigger.words", json!(["bot"])),
            set("trigger.where", json!(["财务群"])),
        ]}),
    )
    .await;
    assert_eq!(
        p.changes,
        vec![
            line(
                "触发语词",
                "/bot",
                "（不能改：触发语要以 / 开头、只用英文字母数字和下划线：bot）"
            ),
            line("触发语生效范围", "—", "（没认出：财务群）"),
        ]
    );
    assert!(ops_of(&h, &p.id).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn names_resolve_exactly_or_are_not_recognized() {
    let h = setup();
    let g2 = space_of(&h, &h.con, "G2", "m1").await;
    let alice: String = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, ApiError>(c.query_row(
                "SELECT id FROM parties WHERE workspace_id = ? AND type = 'contact' AND name = '王小丽'",
                [WS],
                |r| r.get(0),
            )?)
        })
        .expect("alice");
    let p = draft(
        &h,
        "space",
        &g2,
        "这里用小运说话，王小丽说话时转给我，李雷也算，标签打运维和财务，记忆与身份共享",
        json!({"ops": [
            set("persona", json!("小运")),
            set("rules", json!([{"when": {"kind": "member", "parties": ["李雷"]}, "act": "handoff"}])),
            set("item_defaults.tags", json!(["运维", "财务"])),
            set("memory_scope", json!("与身份共享")),
        ]}),
    )
    .await;
    assert_eq!(
        p.changes,
        vec![
            line("对外身份", "—", "（没认出：小运）"),
            line("响应规则", "—", "（没认出：李雷）"),
            line("事项默认标签", "—", "（没认出：财务）"),
            line("记忆", "仅本群", "与身份共享"),
        ]
    );
    let p = draft(
        &h,
        "space",
        &g2,
        "王小丽说话时转给我，标签打运维",
        json!({"ops": [
            set("rules", json!([{"when": {"kind": "member", "parties": ["王小丽"]}, "act": "handoff"}])),
            set("item_defaults.tags", json!("运维")),
        ]}),
    )
    .await;
    assert_eq!(
        p.changes,
        vec![
            line(
                "响应规则",
                "跟随账号（被 @ 或私聊时直接回复）",
                "王小丽 发言时转给你"
            ),
            line("事项默认标签", "无", "运维"),
        ]
    );
    apply(&h, &p.id).expect("apply");
    let s = get(&h, &format!("/spaces/{g2}")).await;
    assert_eq!(s["rules"][0]["when"]["parties"], json!([alice]));
    assert_eq!(s["item_defaults"]["tags"], json!(["tag_ops"]));
    // a whole rules value that is not rules
    let p = draft(
        &h,
        "space",
        &g2,
        "?",
        json!({"ops": [set("rules", json!([{"when": {"kind": "sometimes"}, "act": "reply"}]))]}),
    )
    .await;
    assert_eq!(
        p.changes,
        vec![line(
            "响应规则",
            "王小丽 发言时转给你",
            "（不能改：参数不对：rules.0.when.kind）"
        )]
    );
    // a reply without JSON: no changes
    let p = draft(&h, "space", &g2, "?", json!("我不明白")).await;
    assert!(p.changes.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ownership_and_candidates() {
    let h = setup();
    let g1 = space_of(&h, &h.con, "G1", "m1").await;
    let g2 = space_of(&h, &h.con, "G2", "m2").await;
    let per = new_persona(&h, json!({"name": "值班小运", "bot_id": "ops"})).await;
    // another workspace sees none of it
    let other = caller("w2", ME);
    for (kind, id) in [
        ("space", g1.as_str()),
        ("persona", per.as_str()),
        ("channel", h.con.as_str()),
    ] {
        let e = changes::draft(
            &h.env.modules,
            &other,
            kind,
            Some(id),
            "x",
            Origin::default(),
        )
        .await
        .expect_err("404");
        assert_eq!(e.status(), 404, "{kind}");
    }
    let mut cands: Vec<String> = changes::candidates(&h.env.modules, &me())
        .expect("candidates")
        .into_iter()
        .filter(|c| {
            ["space:", "persona:", "channel:"]
                .iter()
                .any(|k| c.code.starts_with(k))
        })
        .map(|c| c.line())
        .collect();
    cands.sort();
    let mut want = vec![
        format!("space:{g1} — 运维群（测试机器人）"),
        format!("space:{g2} — Acme 交付群（测试机器人）"),
        format!("persona:{per} — 对外身份 值班小运"),
        format!("channel:{} — 接入账号 测试机器人", h.con),
    ];
    let default = h
        .env
        .store()
        .reader(|c| personas::default_persona(c, WS))
        .expect("read")
        .expect("seeded");
    want.push(format!("persona:{} — 对外身份 Steve", default.id));
    want.sort();
    assert_eq!(cands, want);
    // bestSpace: the longest shared piece of at least 2 characters
    let hints = h
        .env
        .store()
        .reader(|c| drafts::space_hints(c, &Viewer::bare(WS, ME), "Acme 交付那边别回复太快"))
        .expect("hints");
    assert_eq!(
        hints.first().map(|c| c.code.clone()),
        Some(format!("space:{g2}"))
    );
    let hints = h
        .env
        .store()
        .reader(|c| drafts::space_hints(c, &Viewer::bare(WS, ME), "运维那边静默"))
        .expect("hints");
    assert_eq!(
        hints.iter().map(|c| c.code.clone()).collect::<Vec<_>>(),
        vec![format!("space:{g1}")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnected_space_can_only_be_viewed() {
    let h = setup();
    let g1 = space_of(&h, &h.con, "G1", "m1").await;
    // drafted while connected, applied after the account was disconnected
    let p = draft(
        &h,
        "space",
        &g1,
        "运维群的职责改成值班",
        json!({"ops": [set("duty", json!("值班"))]}),
    )
    .await;
    let refused = |e: ApiError| {
        assert_eq!(e.status(), 409, "{e:?}");
        assert_eq!(e.to_string(), spaces::DISCONNECTED);
    };
    // the connection gone (ON DELETE SET NULL), settings still there: the op itself refuses
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE spaces SET status = 'left', account_id = (SELECT account_id FROM connections WHERE id = ?1), connection_id = NULL WHERE connection_id = ?1",
                [&h.con],
            )?;
            // what ChannelUsage does to the identities
            c.execute(
                "UPDATE space_identities SET left_at = 1 WHERE connection_id = ?1",
                [&h.con],
            )?;
            Ok(())
        })
        .expect("disconnect");
    refused(
        changes::apply(&h.env.modules, &me(), &p.id)
            .expect("stored")
            .expect_err("apply refused"),
    );
    // and what ChannelUsage leaves: no settings
    h.env
        .store()
        .with_conn(|c| {
            c.execute("DELETE FROM space_settings WHERE space_id = ?", [&g1])?;
            Ok(())
        })
        .expect("settings");
    let shown = get(&h, &format!("/spaces/{g1}")).await;
    assert_eq!(shown["status"], "left");
    assert_eq!(shown["connection_id"], "");

    h.env.gateway.reply(json!({"ops": []}).to_string());
    refused(
        changes::draft(
            &h.env.modules,
            &me(),
            "space",
            Some(&g1),
            "改职责",
            Origin::default(),
        )
        .await
        .expect_err("draft refused"),
    );
    let (st, v) = call(
        &h.api,
        Some(&me()),
        "PATCH",
        &format!("/spaces/{g1}"),
        Some(&json!({"duty": "x"})),
    )
    .await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"], spaces::DISCONNECTED);
}
